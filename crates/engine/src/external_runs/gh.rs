//! The only code that talks to `gh`. It can run exactly three read-only
//! commands: list the user's codespaces, read one, and run a fixed read-only
//! script over `gh codespace ssh`. Nothing here starts, stops, creates or
//! deletes a box, and nothing logs in.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use mast_proto::{BoxFact, ProbeFact};

pub const LIST_TIMEOUT: Duration = Duration::from_secs(30);
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
pub const SSH_CONNECT_TIMEOUT_SECS: u32 = 10;

/// Why a `gh` call produced nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhFailure {
    Missing,
    NotSignedIn,
    /// A 429 or a rate-limit 403, with the server's `Retry-After` when it
    /// sent one.
    RateLimited {
        retry_after_s: Option<u64>,
    },
    /// Any other 403: the login lacks the codespace scope.
    Forbidden,
    Unreachable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedBoxes {
    /// `ETag` of the response, when `gh` showed headers.
    pub etag: Option<String>,
    /// Hash of the body, for the "nothing changed" check.
    pub body_hash: u64,
    pub boxes: Vec<(String, BoxFact)>,
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListOutcome {
    Boxes(ListedBoxes),
    /// A conditional request matched: nothing changed.
    NotModified,
    Failed(GhFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    Box(BoxFact),
    Gone,
    Failed(GhFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    Output(String),
    /// The connection failed with a message that says the box is not running.
    /// A hint to re-read the box, not a fact about it.
    BoxStopped,
    Failed(GhFailure),
}

pub trait GhRunner: Send + Sync {
    /// `gh api user/codespaces`.
    fn list_codespaces(&self, etag: Option<&str>) -> ListOutcome;
    /// `gh api user/codespaces/<name>`.
    fn read_codespace(&self, name: &str) -> ReadOutcome;
    /// `gh codespace ssh -c <name> -- … sh -s` with `script` on stdin.
    fn probe(&self, name: &str, script: &str) -> ProbeOutcome;
}

/// Argument lists for the three commands, apart from the program.
pub mod args {
    use super::SSH_CONNECT_TIMEOUT_SECS;

    pub fn list() -> Vec<String> {
        ["api", "--include", "user/codespaces"]
            .map(String::from)
            .to_vec()
    }

    pub fn read(name: &str) -> Vec<String> {
        vec![
            "api".into(),
            "--include".into(),
            format!("user/codespaces/{name}"),
        ]
    }

    pub fn probe(name: &str) -> Vec<String> {
        vec![
            "codespace".into(),
            "ssh".into(),
            "-c".into(),
            name.into(),
            "--".into(),
            "-o".into(),
            format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"),
            "-o".into(),
            "BatchMode=yes".into(),
            "sh".into(),
            "-s".into(),
        ]
    }
}

/// A codespace name is a path segment of a GitHub API call and an argument to
/// ssh; anything outside this set is refused rather than escaped.
pub fn safe_box_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

struct Finished {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(
    program: &Path,
    arguments: &[String],
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<Finished, GhFailure> {
    let mut command = mast_harness::command(program);
    command
        .args(arguments)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => GhFailure::Missing,
        _ => GhFailure::Unreachable(format!("could not run gh: {error}")),
    })?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let text = text.to_owned();
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    fn drain<T: Read + Send + 'static>(stream: Option<T>) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GhFailure::Unreachable("timed out".into()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => return Err(GhFailure::Unreachable(error.to_string())),
        }
    };
    Ok(Finished {
        code: status.code(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

fn short(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    line.chars().take(80).collect()
}

/// Headers and body of a `gh api --include` response.
fn split_response(stdout: &str) -> (&str, &str) {
    for separator in ["\r\n\r\n", "\n\n"] {
        if let Some((head, body)) = stdout.split_once(separator)
            && head.starts_with("HTTP/")
        {
            return (head, body);
        }
    }
    ("", stdout)
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn status_code(head: &str) -> Option<u16> {
    head.lines().next()?.split_whitespace().nth(1)?.parse().ok()
}

fn classify(finished: &Finished) -> GhFailure {
    let (head, _) = split_response(&finished.stdout);
    let text = format!("{} {}", finished.stderr, head).to_lowercase();
    let rate_limited = text.contains("rate limit") || text.contains("too many requests");
    match status_code(head) {
        Some(401) => return GhFailure::NotSignedIn,
        Some(429) => {
            return GhFailure::RateLimited {
                retry_after_s: header(head, "retry-after").and_then(|v| v.parse().ok()),
            };
        }
        Some(403) if rate_limited => {
            return GhFailure::RateLimited {
                retry_after_s: header(head, "retry-after").and_then(|v| v.parse().ok()),
            };
        }
        Some(403) => return GhFailure::Forbidden,
        _ => {}
    }
    if finished.code == Some(4)
        || text.contains("gh auth login")
        || text.contains("http 401")
        || text.contains("not logged in")
        || text.contains("authentication required")
    {
        return GhFailure::NotSignedIn;
    }
    if text.contains("http 429") || (text.contains("http 403") && rate_limited) {
        return GhFailure::RateLimited {
            retry_after_s: None,
        };
    }
    if text.contains("http 403") {
        return GhFailure::Forbidden;
    }
    let detail = short(&finished.stderr);
    GhFailure::Unreachable(if detail.is_empty() {
        "gh failed".into()
    } else {
        detail
    })
}

pub fn parse_box(value: &serde_json::Value, at: i64) -> Option<(String, BoxFact)> {
    let name = value.get("name")?.as_str()?.to_owned();
    let state = value.get("state")?.as_str()?.to_owned();
    let last_used_at = value
        .get("last_used_at")
        .and_then(|v| v.as_str())
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.timestamp());
    let idle_timeout_min = value
        .get("idle_timeout_minutes")
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok());
    Some((
        name,
        BoxFact {
            state,
            last_used_at,
            idle_timeout_min,
            at,
        },
    ))
}

fn hash_text(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

pub fn parse_list(stdout: &str, now_s: i64) -> Result<ListedBoxes, GhFailure> {
    let (head, body) = split_response(stdout);
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| GhFailure::Unreachable("unreadable codespace list".into()))?;
    let items = value
        .get("codespaces")
        .and_then(|v| v.as_array())
        .ok_or_else(|| GhFailure::Unreachable("unexpected codespace list".into()))?;
    let boxes: Vec<_> = items
        .iter()
        .filter_map(|item| parse_box(item, now_s))
        .collect();
    let total = value
        .get("total_count")
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(boxes.len() as u32);
    Ok(ListedBoxes {
        etag: header(head, "etag").map(str::to_owned),
        body_hash: hash_text(body),
        boxes,
        total,
    })
}

/// Runs the real `gh`. Under `cfg(test)` there is no way to construct one, so
/// a test that forgets to inject a fake fails to compile instead of calling
/// GitHub.
#[cfg_attr(test, allow(dead_code))]
pub struct SystemGh {
    program: std::path::PathBuf,
}

#[cfg(not(test))]
impl SystemGh {
    /// `None` when `gh` is not installed, and always `None` in a test
    /// process (the isolation marker every test binary sets), so a test engine
    /// can never poll.
    pub fn new() -> Option<Self> {
        if std::env::var_os("MAST_TEST_ORIGINAL_HOME").is_some() {
            return None;
        }
        let program = mast_harness::gh_command()?.get_program().into();
        Some(Self { program })
    }
}

fn now_s() -> i64 {
    chrono::Utc::now().timestamp()
}

fn list_with(program: &Path) -> ListOutcome {
    // The conditional request is left out: `gh api` reports a 304 as a failed
    // call, which `gh api --help` does not describe. A body hash does the job.
    match run(program, &args::list(), None, LIST_TIMEOUT) {
        Err(failure) => ListOutcome::Failed(failure),
        Ok(finished) if finished.code == Some(0) => match parse_list(&finished.stdout, now_s()) {
            Ok(list) => ListOutcome::Boxes(list),
            Err(failure) => ListOutcome::Failed(failure),
        },
        Ok(finished) => ListOutcome::Failed(classify(&finished)),
    }
}

fn read_with(program: &Path, name: &str) -> ReadOutcome {
    if !safe_box_name(name) {
        return ReadOutcome::Failed(GhFailure::Unreachable("unusable codespace name".into()));
    }
    match run(program, &args::read(name), None, LIST_TIMEOUT) {
        Err(failure) => ReadOutcome::Failed(failure),
        Ok(finished) if finished.code == Some(0) => {
            let (_, body) = split_response(&finished.stdout);
            match serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|value| parse_box(&value, now_s()))
            {
                Some((_, fact)) => ReadOutcome::Box(fact),
                None => ReadOutcome::Failed(GhFailure::Unreachable("unreadable codespace".into())),
            }
        }
        Ok(finished)
            if finished.stdout.starts_with("HTTP/")
                && status_code(split_response(&finished.stdout).0) == Some(404) =>
        {
            ReadOutcome::Gone
        }
        Ok(finished) => ReadOutcome::Failed(classify(&finished)),
    }
}

/// Only a hint: the caller re-reads the box before believing it.
fn looks_stopped(stderr: &str) -> bool {
    let lowered = stderr.to_lowercase();
    [
        "codespace is shutdown",
        "codespace is not running",
        "codespace is stopped",
    ]
    .iter()
    .any(|message| lowered.contains(message))
}

fn probe_with(program: &Path, name: &str, script: &str) -> ProbeOutcome {
    if !safe_box_name(name) {
        return ProbeOutcome::Failed(GhFailure::Unreachable("unusable codespace name".into()));
    }
    match run(program, &args::probe(name), Some(script), PROBE_TIMEOUT) {
        Err(failure) => ProbeOutcome::Failed(failure),
        Ok(finished) if finished.code == Some(0) => ProbeOutcome::Output(finished.stdout),
        Ok(finished) if looks_stopped(&finished.stderr) => ProbeOutcome::BoxStopped,
        Ok(finished) => ProbeOutcome::Failed(classify(&finished)),
    }
}

impl GhRunner for SystemGh {
    fn list_codespaces(&self, _etag: Option<&str>) -> ListOutcome {
        list_with(&self.program)
    }

    fn read_codespace(&self, name: &str) -> ReadOutcome {
        read_with(&self.program, name)
    }

    fn probe(&self, name: &str, script: &str) -> ProbeOutcome {
        probe_with(&self.program, name, script)
    }
}

/// A run id is spliced into the probe script, so only the hex of a row id gets in.
pub fn safe_run_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// One run to look at on a box.
pub struct ProbeTarget<'a> {
    pub run_id: &'a str,
    pub dir: &'a str,
    pub exit_file: &'a str,
    pub output_file: Option<&'a str>,
    pub session: &'a str,
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The fixed script sent on stdin. It reads: `test`, `cat`, `stat`,
/// `tmux has-session` and `echo`. No verb writes, kills or starts anything.
pub fn probe_script(targets: &[ProbeTarget<'_>]) -> String {
    let mut script = String::new();
    for target in targets.iter().filter(|target| safe_run_id(target.run_id)) {
        let dir = target.dir.trim_end_matches('/');
        script.push_str(&format!("echo {}\n", quote(&format!("run={}", target.run_id))));
        let exit = quote(&format!("{dir}/{}", target.exit_file));
        script.push_str(&format!(
            "if test -f {exit}; then echo \"exit=$(cat {exit} 2>/dev/null)\"; fi\n"
        ));
        script.push_str(&format!(
            "if tmux has-session -t {} 2>/dev/null; then echo tmux=1; else echo tmux=0; fi\n",
            quote(&format!("={}", target.session))
        ));
        if let Some(output) = target.output_file {
            let output = quote(&format!("{dir}/{output}"));
            script.push_str(&format!(
                "if test -f {output}; then echo \"mtime=$(stat -c %Y {output} 2>/dev/null)\"; fi\n"
            ));
        }
    }
    script.push_str("echo done=1\n");
    script
}

/// Per-run facts from a probe's output. A run missing from the output, or an
/// output that never reached `done=1`, yields nothing for that run.
pub fn parse_probe(output: &str, now_s: i64) -> HashMap<String, ProbeFact> {
    let mut facts = HashMap::new();
    let mut complete = false;
    let mut current: Option<(String, ProbeFact)> = None;
    for line in output.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key {
            "run" => {
                if let Some((id, fact)) = current.take() {
                    facts.insert(id, fact);
                }
                current = Some((value.to_owned(), ProbeFact::default()));
            }
            "exit" => {
                if let Some((_, fact)) = current.as_mut() {
                    fact.exit_code = value.split_whitespace().next().and_then(|v| v.parse().ok());
                }
            }
            "tmux" => {
                if let Some((_, fact)) = current.as_mut() {
                    fact.tmux_up = value == "1";
                }
            }
            "mtime" => {
                if let (Some((_, fact)), Ok(mtime)) = (current.as_mut(), value.parse::<i64>()) {
                    fact.output_age_s = Some((now_s - mtime).max(0) as u64);
                }
            }
            "done" => complete = value == "1",
            _ => {}
        }
    }
    if let Some((id, fact)) = current.take() {
        facts.insert(id, fact);
    }
    if complete { facts } else { HashMap::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "HTTP/2.0 200 OK\r\nEtag: W/\"abc\"\r\nContent-Type: application/json\r\n\r\n{\"total_count\":2,\"codespaces\":[{\"name\":\"box-a\",\"state\":\"Available\",\"last_used_at\":\"2026-11-24T10:00:00Z\",\"idle_timeout_minutes\":30},{\"name\":\"box-b\",\"state\":\"Shutdown\"}]}";

    #[test]
    fn the_list_parses_boxes_etag_and_total() {
        let list = parse_list(LIST, 5).unwrap();
        assert_eq!(list.etag.as_deref(), Some("W/\"abc\""));
        assert_eq!(list.total, 2);
        assert_eq!(list.boxes.len(), 2);
        let (name, fact) = &list.boxes[0];
        assert_eq!(name, "box-a");
        assert!(fact.is_available());
        assert_eq!(fact.idle_timeout_min, Some(30));
        assert_eq!(fact.last_used_at, Some(1_795_514_400));
        assert_eq!(fact.at, 5);
        assert!(list.boxes[1].1.is_shutdown());
    }

    #[test]
    fn a_body_without_headers_still_parses_and_garbage_does_not() {
        let body = LIST.split_once("\r\n\r\n").unwrap().1;
        assert_eq!(parse_list(body, 0).unwrap().boxes.len(), 2);
        assert_eq!(
            parse_list(body, 0).unwrap().body_hash,
            parse_list(LIST, 9).unwrap().body_hash
        );
        assert!(parse_list("not json", 0).is_err());
        assert!(parse_list("{}", 0).is_err());
    }

    #[test]
    fn failures_are_classified() {
        let fail = |code, stdout: &str, stderr: &str| {
            classify(&Finished {
                code: Some(code),
                stdout: stdout.into(),
                stderr: stderr.into(),
            })
        };
        assert_eq!(
            fail(
                4,
                "",
                "To get started with GitHub CLI, please run:  gh auth login"
            ),
            GhFailure::NotSignedIn
        );
        assert_eq!(
            fail(
                1,
                "HTTP/2.0 401 Unauthorized\r\n\r\n{}",
                "gh: Bad credentials (HTTP 401)"
            ),
            GhFailure::NotSignedIn
        );
        assert_eq!(
            fail(
                1,
                "HTTP/2.0 429 Too Many Requests\r\nRetry-After: 90\r\n\r\n{}",
                "gh: HTTP 429"
            ),
            GhFailure::RateLimited {
                retry_after_s: Some(90)
            }
        );
        assert_eq!(
            fail(1, "", "gh: API rate limit exceeded (HTTP 403)"),
            GhFailure::RateLimited {
                retry_after_s: None
            }
        );
        assert_eq!(
            fail(
                1,
                "HTTP/2.0 403 Forbidden\r\n\r\n{}",
                "gh: Resource not accessible by personal access token (HTTP 403)"
            ),
            GhFailure::Forbidden
        );
        assert_eq!(
            fail(
                1,
                "HTTP/2.0 403 Forbidden\r\nRetry-After: 30\r\n\r\n{}",
                "gh: You have exceeded a secondary rate limit (HTTP 403)"
            ),
            GhFailure::RateLimited {
                retry_after_s: Some(30)
            }
        );
        assert_eq!(
            fail(1, "", "gh: Must have admin rights (HTTP 403)"),
            GhFailure::Forbidden
        );
        assert_eq!(
            fail(1, "", "error connecting to api.github.com\nmore"),
            GhFailure::Unreachable("error connecting to api.github.com".into())
        );
    }

    #[test]
    fn box_names_are_checked_before_they_reach_an_argument() {
        assert!(safe_box_name("user-fuzzy-garbanzo-abc123"));
        for bad in ["", "a b", "a/b", "a;b", "$(x)", "-rf", "a\nb"] {
            assert!(!safe_box_name(bad), "{bad:?}");
        }
    }

    fn targets() -> Vec<ProbeTarget<'static>> {
        vec![
            ProbeTarget {
                run_id: "a1",
                dir: "/work/runs/one/",
                exit_file: "exit_code",
                output_file: Some("out.log"),
                session: "run-one",
            },
            ProbeTarget {
                run_id: "a2",
                dir: "/work/runs/two",
                exit_file: "exit_code",
                output_file: None,
                session: "run-two",
            },
        ]
    }

    #[test]
    fn the_script_has_only_read_verbs() {
        let script = probe_script(&targets());
        assert!(script.contains("tmux has-session -t '=run-one'"));
        assert!(script.contains("stat -c %Y '/work/runs/one/out.log'"));
        assert!(!script.contains("//"));
        for line in script.lines() {
            let allowed = ["echo ", "if test -f ", "if tmux has-session "];
            assert!(allowed.iter().any(|p| line.starts_with(p)), "{line}");
        }
        for banned in [
            "rm ",
            "kill",
            "mv ",
            "cp ",
            "tee",
            "touch",
            "mkdir",
            "chmod",
            "new-session",
            "send-keys",
            "sudo",
            "curl",
            "start",
            "stop",
            "delete",
            "dd ",
        ] {
            assert!(!script.contains(banned), "{banned}");
        }
        assert_eq!(
            script.matches('>').count(),
            script.matches("2>/dev/null").count()
        );
    }

    #[test]
    fn only_a_specific_stopped_message_is_a_stopped_hint() {
        assert!(looks_stopped("error: Codespace is not running"));
        assert!(looks_stopped("Codespace is shutdown\n"));
        for other in [
            "ssh: connect to host: Connection refused",
            "service unavailable",
            "operation stopped by user",
            "shutdown: timed out",
        ] {
            assert!(!looks_stopped(other), "{other}");
        }
    }

    #[test]
    fn a_run_id_that_is_not_hex_never_reaches_the_script() {
        let script = probe_script(&[
            ProbeTarget {
                run_id: "ab12",
                dir: "/d",
                exit_file: "e",
                output_file: None,
                session: "s",
            },
            ProbeTarget {
                run_id: "x'; rm -rf /; '",
                dir: "/d",
                exit_file: "e",
                output_file: None,
                session: "s",
            },
        ]);
        assert!(script.contains("echo 'run=ab12'"));
        assert_eq!(script.matches("run=").count(), 1);
        assert!(!script.contains("rm -rf"));
        assert!(safe_run_id("0f9a"));
        for bad in ["", "r1", "a b", "a'b"] {
            assert!(!safe_run_id(bad), "{bad:?}");
        }
        assert!(!safe_run_id(&"a".repeat(65)));
    }

    #[test]
    fn script_values_are_single_quoted() {
        let script = probe_script(&[ProbeTarget {
            run_id: "a",
            dir: "/a/it's",
            exit_file: "e",
            output_file: None,
            session: "s",
        }]);
        assert!(script.contains("'/a/it'\\''s/e'"));
    }

    #[test]
    fn probe_output_parses_per_run_and_needs_the_trailer() {
        let output = "run=r1\nexit=0\ntmux=0\nmtime=900\nrun=r2\ntmux=1\ndone=1\n";
        let facts = parse_probe(output, 1_000);
        assert_eq!(
            facts["r1"],
            ProbeFact {
                exit_code: Some(0),
                tmux_up: false,
                output_age_s: Some(100)
            }
        );
        assert_eq!(
            facts["r2"],
            ProbeFact {
                exit_code: None,
                tmux_up: true,
                output_age_s: None
            }
        );
        assert!(parse_probe("run=r1\ntmux=1\n", 0).is_empty());
        let odd = parse_probe("run=r1\nexit=oops\ntmux=1\nmtime=2000\ndone=1\n", 1_000);
        assert_eq!(odd["r1"].exit_code, None);
        assert_eq!(odd["r1"].output_age_s, Some(0));
    }

    #[test]
    fn the_commands_are_the_three_read_only_ones() {
        assert_eq!(args::list(), ["api", "--include", "user/codespaces"]);
        assert_eq!(
            args::read("box-1"),
            ["api", "--include", "user/codespaces/box-1"]
        );
        let probe = args::probe("box-1");
        assert_eq!(&probe[..4], ["codespace", "ssh", "-c", "box-1"]);
        assert_eq!(&probe[probe.len() - 2..], ["sh", "-s"]);
    }

    fn fake_gh(directory: &Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join("gh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn the_process_runner_passes_exact_arguments_and_stdin_to_a_fake_gh() {
        let directory = tempfile::tempdir().unwrap();
        let record = directory.path().join("argv");
        let program = fake_gh(
            directory.path(),
            &format!(
                "printf '%s\\n' \"$@\" >> {record:?}\nif [ \"$1\" = codespace ]; then cat; else printf 'HTTP/2.0 200 OK\\r\\n\\r\\n{{\"codespaces\":[]}}'; fi",
            ),
        );
        assert!(matches!(list_with(&program), ListOutcome::Boxes(list) if list.boxes.is_empty()));
        assert!(matches!(
            read_with(&program, "bad name"),
            ReadOutcome::Failed(_)
        ));
        assert!(matches!(
            probe_with(&program, "box-1", "echo hi\n"),
            ProbeOutcome::Output(text) if text == "echo hi\n"
        ));
        let argv = std::fs::read_to_string(&record).unwrap();
        let lines: Vec<&str> = argv.lines().collect();
        assert_eq!(&lines[..3], ["api", "--include", "user/codespaces"]);
        assert_eq!(&lines[3..7], ["codespace", "ssh", "-c", "box-1"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_program_and_a_timeout_are_reported() {
        assert_eq!(
            run(
                Path::new("/nonexistent/gh"),
                &[],
                None,
                Duration::from_secs(1)
            )
            .err(),
            Some(GhFailure::Missing)
        );
        let directory = tempfile::tempdir().unwrap();
        let program = fake_gh(directory.path(), "sleep 5");
        assert!(matches!(
            run(&program, &[], None, Duration::from_millis(100)),
            Err(GhFailure::Unreachable(_))
        ));
    }
}
