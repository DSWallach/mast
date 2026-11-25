//! Device-local dispatch for a Space. The selected adapter owns CLI-specific
//! discovery, reconciliation and authentication. The engine owns approval,
//! capacity, lifecycle gates and registration. See docs/internal/codespace-launchers.md.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use mast_proto::{ExternalRunOutcome, ExternalRunSettings, UpsertExternalRunParams};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

const MAX_DOCUMENT: u64 = 512 * 1024;
const ADAPTER_TIMEOUT: Duration = Duration::from_secs(600);
const REVIEW_LOOP: &str = "\n\n## Review and handoff\n\nFor work that touches a pull request, run the complete configured review loop on every pushed head. Wait for CI and review synchronously. After a change, push and repeat the loop until a full pass produces no changes. Follow the configured bot triage and resolution instructions. Handle clear human review feedback in the code without replying in-thread. Bring ambiguous or contested human review feedback to the owner. Verify the current head and attach the required evidence before reporting completion.";
pub(super) type Result<T> = std::result::Result<T, &'static str>;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Action {
    Launch,
    Resume,
    Inspect,
    Stop,
    Cleanup,
}

impl Action {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Resume => "resume",
            Self::Inspect => "inspect",
            Self::Stop => "stop",
            Self::Cleanup => "cleanup",
        }
    }

    fn starts_worker(self) -> bool {
        matches!(self, Self::Launch | Self::Resume)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    executable: PathBuf,
    working_directory: PathBuf,
    worker_sources: Vec<Source>,
    #[serde(default)]
    launcher_sources: Vec<Source>,
    #[serde(default)]
    cleanup_enabled: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Source {
    path: PathBuf,
    /// Optional exact surrounding lines for a worker template in a larger file.
    start_after: Option<String>,
    end_before: Option<String>,
    /// Remove one Markdown blockquote level from the selected region.
    #[serde(default)]
    unquote: bool,
}

impl Source {
    fn read(&self) -> Result<String> {
        if !self.path.is_absolute() {
            return Err("Playbook sources must be absolute files selected on the host.");
        }
        let contents =
            read_text(&self.path).map_err(|_| "A configured playbook source could not be read.")?;
        let mut lines = contents.split_inclusive('\n');
        if let Some(start) = &self.start_after
            && !lines.any(|line| line.trim_end_matches(['\r', '\n']) == start)
        {
            return Err("A configured playbook start line was not found.");
        }
        let mut output = String::new();
        let mut ended = self.end_before.is_none();
        for line in lines {
            if self.end_before.as_deref() == Some(line.trim_end_matches(['\r', '\n'])) {
                ended = true;
                break;
            }
            let line = if self.unquote && !line.trim().is_empty() {
                line.strip_prefix("> ")
                    .or_else(|| line.strip_prefix('>'))
                    .ok_or("The configured worker block contains an unquoted line.")?
            } else {
                line
            };
            output.push_str(line);
        }
        if !ended || output.trim().is_empty() {
            return Err("The configured playbook region is empty or its end line is missing.");
        }
        Ok(output)
    }
}

fn read_text(path: &Path) -> io::Result<String> {
    let mut output = String::new();
    fs::File::open(path)?
        .take(MAX_DOCUMENT + 1)
        .read_to_string(&mut output)?;
    if output.len() as u64 > MAX_DOCUMENT {
        return Err(io::Error::other("document too large"));
    }
    Ok(output)
}

pub(crate) fn fingerprint(value: impl Serialize) -> String {
    let bytes = serde_json::to_vec(&value).expect("serializable launcher data");
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Identity {
    codespace_name: String,
    run_tag: String,
    provider: String,
    session_id: String,
}

impl Identity {
    fn valid(&self) -> bool {
        use mast_proto::external_runs::limits::MAX_RUN_SESSION_ID_CHARS;
        super::gh::safe_box_name(&self.codespace_name)
            && super::gh::safe_box_name(&self.run_tag)
            && matches!(self.provider.as_str(), "codex" | "claude")
            && !self.session_id.trim().is_empty()
            && self.session_id.chars().count() <= MAX_RUN_SESSION_ID_CHARS
            && !self.session_id.chars().any(char::is_control)
    }
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum WorkerState {
    Running,
    Stalled,
    Done,
    Failed,
    Orphaned,
    NeedsHuman,
    Unknown,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum PullRequestState {
    Open,
    Merged,
    ClosedUnmerged,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Review {
    state: PullRequestState,
    url: String,
    head_sha: String,
    checked_head_sha: String,
    review_head_sha: String,
    checks_passed: u32,
    checks_pending_or_failed: u32,
    scope_matches: bool,
    evidence_present: bool,
    review_complete: bool,
    final_report_complete: bool,
}

impl Review {
    fn verified(&self) -> bool {
        self.state != PullRequestState::ClosedUnmerged
            && self.url.starts_with("https://github.com/")
            && self.url.contains("/pull/")
            && self.head_sha.len() == 40
            && self.head_sha.bytes().all(|byte| byte.is_ascii_hexdigit())
            && self.checked_head_sha == self.head_sha
            && self.review_head_sha == self.head_sha
            && self.checks_passed > 0
            && self.checks_pending_or_failed == 0
            && self.scope_matches
            && self.evidence_present
            && self.review_complete
            && self.final_report_complete
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    worker: WorkerState,
    box_running: bool,
    review: Option<Review>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Admission {
    ready: bool,
    running_or_starting: u32,
    total_boxes: u32,
    /// The configured launcher's account limit, including any policy limit.
    capacity_cap: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Archive {
    /// Opaque receipt for a durable, retrievable transcript outside the box.
    transcript_receipt: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Request<'a> {
    protocol_version: u8,
    phase: &'a str,
    operation_id: &'a str,
    space_id: &'a str,
    work_item_id: &'a str,
    owner_chat_id: &'a str,
    action: Action,
    run_key: &'a str,
    identity: Option<&'a Identity>,
    worker_prompt: Option<&'a str>,
    launcher_sources: &'a [String],
    scope: &'a str,
    review: Option<&'a Review>,
    transcript_receipt: Option<&'a str>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Journal {
    identity: Option<Identity>,
    pending: Option<String>,
    #[serde(default)]
    pending_intent: Option<String>,
    #[serde(default)]
    remote_result: Option<serde_json::Value>,
    results: std::collections::BTreeMap<String, serde_json::Value>,
    launcher_binding: Option<String>,
    deleted: bool,
}

impl Journal {
    fn load(path: &Path) -> Result<Self> {
        match read_text(path) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|_| "The dispatch journal is unreadable.")
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(_) => Err("The dispatch journal could not be read."),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        fn atomic_write(journal: &Journal, path: &Path) -> io::Result<()> {
            let parent = path.parent().expect("journal has a directory");
            fs::create_dir_all(parent)?;
            let mut output = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer(output.as_file_mut(), journal)?;
            output.as_file().sync_all()?;
            output.persist(path)?;
            #[cfg(unix)]
            fs::File::open(parent)?.sync_all()?;
            Ok(())
        }
        atomic_write(self, path).map_err(|_| "Could not durably save the dispatch journal.")
    }
}

pub(crate) struct Dispatch {
    config: Config,
    launcher_sources: Vec<String>,
    worker_prompt: String,
    binding: String,
    executable_hash: String,
    pub(crate) digest: String,
    intent: String,
    journal: PathBuf,
    run_key: String,
    space: String,
    item: String,
    owner: String,
    action: Action,
    scope: String,
}

impl Dispatch {
    pub(crate) fn load(
        store: &Path,
        space: &str,
        item: &str,
        owner: &str,
        action: Action,
        scope: String,
        attempt: &str,
    ) -> Result<Self> {
        if !super::gh::safe_box_name(space) {
            return Err("The Space id cannot select a launcher configuration.");
        }
        let directory = store.join("codespace-launchers");
        let config: Config = serde_json::from_str(
            &read_text(&directory.join(format!("{space}.json")))
                .map_err(|_| "This Space has no readable launcher configuration on its host.")?,
        )
        .map_err(|_| "This Space's launcher configuration is invalid.")?;
        if !config.executable.is_absolute() || !config.working_directory.is_absolute() {
            return Err("The configured launcher and working directory must be absolute.");
        }
        if action == Action::Cleanup && !config.cleanup_enabled {
            return Err("Cleanup is disabled for this Space.");
        }
        let launcher_sources = config
            .launcher_sources
            .iter()
            .map(Source::read)
            .collect::<Result<Vec<_>>>()?;
        let worker_sources = config
            .worker_sources
            .iter()
            .map(Source::read)
            .collect::<Result<Vec<_>>>()?;
        if worker_sources.is_empty() {
            return Err("Select the full worker playbook for this Space before dispatch.");
        }
        let mut worker_prompt = worker_sources.join("\n\n");
        worker_prompt.push_str(REVIEW_LOOP);
        worker_prompt.push_str("\n\n## Work for this turn\n\n");
        worker_prompt.push_str(&scope);
        if action == Action::Resume {
            worker_prompt.push_str("\n\nContinue the existing session. Recheck any outstanding wait in this turn and verify the current PR head before claiming completion.");
        }
        let executable_hash = executable_hash(&config.executable)?;
        let binding = fingerprint((&config.executable, &config.working_directory));
        let run_key = fingerprint((space, item));
        let intent = fingerprint((&run_key, owner, action, attempt, &scope));
        let digest = fingerprint((
            &run_key,
            owner,
            action,
            attempt,
            &config,
            &executable_hash,
            &launcher_sources,
            &worker_prompt,
        ));
        Ok(Self {
            config,
            launcher_sources,
            worker_prompt,
            binding,
            executable_hash,
            digest,
            intent,
            journal: directory.join("runs").join(format!("{run_key}.json")),
            run_key,
            space: space.into(),
            item: item.into(),
            owner: owner.into(),
            action,
            scope,
        })
    }

    fn request<'a>(&'a self, phase: &'a str, identity: Option<&'a Identity>) -> Request<'a> {
        Request {
            protocol_version: 1,
            phase,
            operation_id: &self.digest,
            space_id: &self.space,
            work_item_id: &self.item,
            owner_chat_id: &self.owner,
            action: self.action,
            run_key: &self.run_key,
            identity,
            worker_prompt: self.action.starts_worker().then_some(&self.worker_prompt),
            launcher_sources: &self.launcher_sources,
            scope: &self.scope,
            review: None,
            transcript_receipt: None,
        }
    }

    pub(crate) fn execute(
        &self,
        settings: &ExternalRunSettings,
        authorize: impl Fn() -> Result<()>,
        register: impl Fn(UpsertExternalRunParams) -> Result<()>,
    ) -> Result<serde_json::Value> {
        self.execute_with(settings, authorize, register, |request| self.call(request))
    }

    fn execute_with(
        &self,
        settings: &ExternalRunSettings,
        authorize: impl Fn() -> Result<()>,
        register: impl Fn(UpsertExternalRunParams) -> Result<()>,
        call: impl Fn(Request<'_>) -> Result<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let mut journal = Journal::load(&self.journal)?;
        if journal
            .launcher_binding
            .as_ref()
            .is_some_and(|binding| binding != &self.binding)
        {
            return Err("Use the launcher that owns this run.");
        }
        if let Some(result) = journal.results.get(&self.digest) {
            return Ok(result.clone());
        }
        authorize()?;
        if journal.deleted {
            return Err("This run's codespace was deleted.");
        }
        let identity = journal.identity.clone();
        if self.action == Action::Inspect {
            let identity = identity.as_ref().ok_or("This item has no managed run.")?;
            let observation: Observation = decode(call(self.request("inspect", Some(identity)))?)?;
            let verified = observation.review.as_ref().is_some_and(Review::verified);
            return Ok(serde_json::json!({
                "identity": identity, "observation": observation, "verified": verified
            }));
        }
        // A refreshed approval may change the digest while a prior reply is
        // outstanding. Only the same intent may recover that operation.
        if journal
            .pending
            .as_ref()
            .is_some_and(|pending| pending != &self.digest)
            && journal.pending_intent.as_ref() != Some(&self.intent)
        {
            return Err(
                "Retry the pending operation with the same inputs before changing this run.",
            );
        }
        let pending = journal.pending.clone();
        let mut remote_id = pending.as_deref().unwrap_or(&self.digest);
        let retry = journal.pending.is_some();
        if !retry {
            if (self.action == Action::Launch) == identity.is_some() {
                return Err(
                    "Launch requires a fresh item. Other operations require a managed run.",
                );
            }
            journal.launcher_binding = Some(self.binding.clone());
        }
        let mut result = journal.remote_result.clone();
        if retry && result.is_none() {
            // A lost reply is ambiguous. Only the configured adapter may
            // reconcile it, without creating, resuming, stopping or deleting.
            result = decode::<Option<serde_json::Value>>(call(Request {
                operation_id: remote_id,
                ..self.request("reconcile", identity.as_ref())
            })?)?;
        }
        if result.is_none() {
            let observation: Option<Observation> = identity
                .as_ref()
                .map(|id| call(self.request("inspect", Some(id))).and_then(decode))
                .transpose()?;
            if self.action.starts_worker() {
                let admission: Admission =
                    decode(call(self.request("preflight", identity.as_ref()))?)?;
                check_admission(self.action, settings, &admission, observation.as_ref())?;
            }
            let review = check_lifecycle(self.action, observation.as_ref())?;
            authorize()?;
            journal.pending = Some(self.digest.clone());
            journal.pending_intent = Some(self.intent.clone());
            journal.save(&self.journal)?;
            remote_id = &self.digest;
            let mut request = self.request(self.action.name(), identity.as_ref());
            request.review = review;
            let archive: Archive;
            if self.action == Action::Cleanup {
                archive = decode(call(Request {
                    phase: "archive",
                    ..request
                })?)?;
                if archive.transcript_receipt.trim().is_empty() {
                    return Err("Cleanup requires a preserved transcript.");
                }
                request.transcript_receipt = Some(&archive.transcript_receipt);
                authorize()?;
            }
            result = Some(call(request)?);
        }
        let raw_result = result.expect("operation produced a reply");
        match self.action {
            Action::Launch | Action::Resume => {
                let launched: Identity = decode(raw_result.clone())?;
                if !launched.valid() || identity.as_ref().is_some_and(|prior| prior != &launched) {
                    return Err("The launcher returned an invalid or changed run identity.");
                }
            }
            _ if raw_result.as_bool() != Some(true) => {
                return Err("The adapter could not confirm the lifecycle operation.");
            }
            _ => {}
        }
        journal.remote_result = Some(raw_result.clone());
        journal.save(&self.journal)?;
        let reply = match self.action {
            Action::Launch | Action::Resume => {
                let launched: Identity = decode(raw_result)?;
                register(self.registration(&launched, true, remote_id))?;
                journal.identity = Some(launched.clone());
                serde_json::json!({
                    "runId": mast_proto::external_run_id(&self.item, &launched.run_tag),
                    "workItemId": self.item, "ownerChatId": self.owner, "identity": launched,
                })
            }
            Action::Stop | Action::Cleanup => {
                register(self.registration(
                    identity.as_ref().expect("managed run"),
                    false,
                    remote_id,
                ))?;
                journal.deleted = self.action == Action::Cleanup;
                serde_json::json!({ "state": if journal.deleted { "deleted" } else { "stopped" } })
            }
            Action::Inspect => unreachable!("inspect is read-only"),
        };
        journal.pending = None;
        journal.pending_intent = None;
        journal.remote_result = None;
        if remote_id != self.digest {
            journal.results.insert(remote_id.to_owned(), reply.clone());
        }
        journal.results.insert(self.digest.clone(), reply.clone());
        journal.save(&self.journal)?;
        Ok(reply)
    }

    fn registration(
        &self,
        identity: &Identity,
        alive: bool,
        remote_id: &str,
    ) -> UpsertExternalRunParams {
        UpsertExternalRunParams {
            work_item_id: self.item.clone(),
            run_tag: identity.run_tag.clone(),
            codespace_name: Some(identity.codespace_name.clone()),
            provider: Some(identity.provider.clone()),
            session_id: Some(identity.session_id.clone()),
            owner_chat_id: Some(self.owner.clone()),
            expected_alive: Some(alive),
            outcome: (!alive).then_some(ExternalRunOutcome::Done),
            reopen: self.action == Action::Resume,
            operation_id: mast_proto::derive_id(remote_id, "codespace-external-run"),
            ..Default::default()
        }
    }

    fn call(&self, request: Request<'_>) -> Result<serde_json::Value> {
        if executable_hash(&self.config.executable)? != self.executable_hash {
            return Err("The configured launcher changed. Reconcile with the original launcher.");
        }
        let input = serde_json::to_vec(&request).map_err(|_| "Could not encode launcher input.")?;
        let mut child = mast_harness::command(&self.config.executable)
            .current_dir(&self.config.working_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "Could not start the configured launcher.")?;
        let mut stdin = child.stdin.take().expect("piped adapter input");
        std::thread::spawn(move || stdin.write_all(&input));
        let stdout = child.stdout.take().expect("piped adapter output");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut output = String::new();
            let read = stdout.take(MAX_DOCUMENT + 1).read_to_string(&mut output);
            let _ = tx.send(read.map(|_| output));
        });
        let deadline = Instant::now() + ADAPTER_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(
                        "The launcher did not finish. Retry with the same operation inputs to reconcile.",
                    );
                }
            }
        };
        if !status.success() {
            return Err("The launcher failed. Retry with the same operation inputs to reconcile.");
        }
        let output = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "The launcher did not close its output.")?
            .map_err(|_| "Could not read launcher output.")?;
        if output.len() as u64 > MAX_DOCUMENT {
            return Err("Launcher output is too large.");
        }
        serde_json::from_str(&output).map_err(|_| "The launcher returned invalid JSON.")
    }
}

fn executable_hash(path: &Path) -> Result<String> {
    let mut executable =
        fs::File::open(path).map_err(|_| "The configured launcher could not be read.")?;
    let mut digest = Sha256::new();
    io::copy(&mut executable, &mut digest)
        .map_err(|_| "The configured launcher could not be read.")?;
    Ok(format!("{:x}", digest.finalize()))
}

fn decode<T: DeserializeOwned>(value: serde_json::Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| "Unexpected response from the launcher.")
}

fn check_admission(
    action: Action,
    settings: &ExternalRunSettings,
    admission: &Admission,
    observation: Option<&Observation>,
) -> Result<()> {
    if !admission.ready {
        return Err("The configured readiness or placement check blocked dispatch.");
    }
    if !observation.is_some_and(|observation| observation.box_running)
        && admission.running_or_starting >= admission.capacity_cap.min(settings.capacity_cap)
    {
        return Err("No running Codespace capacity is available.");
    }
    if action == Action::Launch
        && settings
            .box_limit
            .is_some_and(|limit| admission.total_boxes >= limit)
    {
        return Err("The configured total Codespace limit has been reached.");
    }
    Ok(())
}

fn check_lifecycle(action: Action, observation: Option<&Observation>) -> Result<Option<&Review>> {
    let Some(observation) = observation else {
        return Ok(None);
    };
    let review = observation.review.as_ref();
    if review.is_some_and(|review| review.state == PullRequestState::ClosedUnmerged) {
        return Err("The PR was closed without merging. Retain the box for a human decision.");
    }
    match action {
        Action::Resume if review.is_some_and(|review| review.state == PullRequestState::Merged) => {
            Err("The PR already merged. New work needs a new work item.")
        }
        Action::Resume
            if matches!(
                observation.worker,
                WorkerState::Running | WorkerState::Unknown
            ) =>
        {
            Err("Resume needs a confirmed stalled or ended worker.")
        }
        Action::Stop
            if observation.worker == WorkerState::Done && review.is_some_and(Review::verified) =>
        {
            Ok(review)
        }
        Action::Stop => {
            Err("Stop requires an ended worker and verified PR, CI, evidence and review.")
        }
        Action::Cleanup
            if !observation.box_running
                && review.is_some_and(|review| {
                    review.state == PullRequestState::Merged && review.scope_matches
                }) =>
        {
            Ok(review)
        }
        Action::Cleanup => Err("Cleanup requires a stopped box and a confirmed merged PR."),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use serde_json::{Value, json};

    use super::*;

    struct Fixture {
        root: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let local = root.path().join("codespace-launchers");
            fs::create_dir(&local).unwrap();
            let source = root.path().join("source");
            fs::write(
                &source,
                "Host instructions\nPrompt starts\n> First step\n>\n> Review again after each push\nPrompt ends\nHost notes\n",
            ).unwrap();
            let executable = root.path().join("fixture");
            fs::write(&executable, "fixture binary").unwrap();
            let config = json!({
                "executable": executable,
                "workingDirectory": root.path(),
                "workerSources": [{
                    "path": source, "startAfter": "Prompt starts", "endBefore": "Prompt ends", "unquote": true,
                }],
                "launcherSources": [{ "path": source }],
                "cleanupEnabled": true,
            });
            fs::write(local.join("space.json"), config.to_string()).unwrap();
            Self { root }
        }

        fn action(&self, action: Action) -> Dispatch {
            self.action_for(action, "item")
        }

        fn action_for(&self, action: Action, item: &str) -> Dispatch {
            Dispatch::load(
                self.root.path(),
                "space",
                item,
                "owner",
                action,
                "resolved decision".into(),
                "first",
            )
            .unwrap()
        }
    }

    fn identity() -> Value {
        json!({
            "codespaceName": "gh-box-88xx",
            "runTag": "box-run",
            "provider": "codex",
            "sessionId": "first-session",
        })
    }

    fn admission() -> Value {
        json!({ "ready": true, "runningOrStarting": 2, "totalBoxes": 7, "capacityCap": 5 })
    }

    fn inspection(worker: &str, box_running: bool, pr: &str) -> Value {
        let sha = "0123456789012345678901234567890123456789";
        json!({
            "worker": worker, "boxRunning": box_running,
            "review": {
                "state": pr, "url": "https://github.com/example/fixture/pull/1",
                "headSha": sha, "checkedHeadSha": sha, "reviewHeadSha": sha,
                "checksPassed": 2, "checksPendingOrFailed": 0,
                "scopeMatches": true, "evidencePresent": true, "reviewComplete": true,
                "finalReportComplete": true,
            }
        })
    }

    fn launch(fixture: &Fixture) {
        fixture
            .action(Action::Launch)
            .execute_with(
                &ExternalRunSettings::default(),
                || Ok(()),
                |_| Ok(()),
                |request| {
                    Ok(match request.phase {
                        "preflight" => admission(),
                        "launch" => identity(),
                        _ => panic!("unexpected phase {}", request.phase),
                    })
                },
            )
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn host_adapter_receives_the_playbook_through_stdin() {
        use std::os::unix::fs::PermissionsExt as _;

        let fixture = Fixture::new();
        let adapter = fixture.root.path().join("fixture");
        fs::write(&adapter, r#"#!/bin/sh
set -eu
IFS= read -r request || test -n "$request"
case "$request" in
    *'"phase":"preflight"'*) printf '%s' '{"ready":true,"runningOrStarting":0,"totalBoxes":0,"capacityCap":5}' ;;
    *'"phase":"launch"'*)
        case "$request" in
            *'Review again after each push'*'resolved decision'*) ;;
            *) exit 1 ;;
        esac
        printf '%s' '{"codespaceName":"gh-box-88xx","runTag":"box-run","provider":"codex","sessionId":"first-session"}'
        ;;
    *) exit 1 ;;
esac
"#).unwrap();
        fs::set_permissions(&adapter, fs::Permissions::from_mode(0o700)).unwrap();
        let receipt = fixture
            .action(Action::Launch)
            .execute(&ExternalRunSettings::default(), || Ok(()), |_| Ok(()))
            .unwrap();
        assert_eq!(receipt["identity"], identity());
    }

    #[test]
    fn launch_preserves_sources_and_records_the_concrete_identity() {
        let fixture = Fixture::new();
        let action = fixture.action(Action::Launch);
        let phases = RefCell::new(Vec::new());
        let recorded = RefCell::new(None);
        let receipt = action
            .execute_with(
                &ExternalRunSettings::default(),
                || Ok(()),
                |params| {
                    recorded.replace(Some(params));
                    Ok(())
                },
                |request| {
                    phases.borrow_mut().push(request.phase.to_owned());
                    assert_eq!(request.owner_chat_id, "owner");
                    let prompt = request.worker_prompt.unwrap();
                    assert!(prompt.starts_with("First step\n\nReview again after each push\n"));
                    assert!(prompt.contains(REVIEW_LOOP));
                    assert!(prompt.ends_with("## Work for this turn\n\nresolved decision"));
                    assert!(request.launcher_sources[0].ends_with("Host notes\n"));
                    Ok(if request.phase == "preflight" {
                        admission()
                    } else {
                        identity()
                    })
                },
            )
            .unwrap();
        assert_eq!(*phases.borrow(), ["preflight", "launch"]);
        assert_eq!(receipt["identity"], identity());
        let recorded = recorded.into_inner().unwrap();
        assert_eq!(recorded.work_item_id, "item");
        assert_eq!(recorded.owner_chat_id.as_deref(), Some("owner"));
        assert_eq!(recorded.run_tag, "box-run");
        assert_eq!(recorded.session_id.as_deref(), Some("first-session"));
        assert_eq!(recorded.expected_alive, Some(true));
        assert_eq!(
            action
                .execute_with(
                    &ExternalRunSettings::default(),
                    || Ok(()),
                    |_| panic!("already registered"),
                    |_| panic!("already launched")
                )
                .unwrap(),
            receipt
        );
    }

    #[test]
    fn a_lost_launch_reply_reconciles_and_a_failed_registration_replays_only_the_write() {
        let fixture = Fixture::new();
        let settings = ExternalRunSettings::default();
        let action = fixture.action(Action::Launch);
        assert_eq!(
            action.execute_with(
                &settings,
                || Ok(()),
                |_| panic!("no reply"),
                |request| {
                    match request.phase {
                        "preflight" => Ok(admission()),
                        "launch" => Err("lost reply"),
                        _ => panic!("unexpected phase"),
                    }
                }
            ),
            Err("lost reply")
        );
        let action = fixture.action(Action::Launch);
        assert_eq!(
            action.execute_with(
                &settings,
                || Ok(()),
                |_| Err("storage failed"),
                |request| {
                    assert_eq!(request.phase, "reconcile");
                    assert_eq!(request.operation_id, action.digest);
                    Ok(identity())
                }
            ),
            Err("storage failed")
        );
        let action = fixture.action(Action::Launch);
        assert!(
            action
                .execute_with(
                    &settings,
                    || Ok(()),
                    |_| Ok(()),
                    |_| panic!("use durable reply")
                )
                .is_ok()
        );
    }

    #[tokio::test]
    async fn launch_writes_the_real_tracker_registry() {
        use crate::work_outline::WorkOutlineService;
        use crate::workspace_host::{WorkspaceHost, WorkspaceHostConfig};
        use mast_proto::{AssignChatToWorkItemParams, CreateWorkItemParams, WorkOutlineMutation};
        use mast_sync::DocsStore;
        use std::sync::Arc;

        let fixture = Fixture::new();
        let store =
            Arc::new(DocsStore::open(fixture.root.path().join("registry").as_path()).unwrap());
        let host = WorkspaceHost::open(
            store,
            WorkspaceHostConfig {
                device_id: "device".into(),
                device_name: "fixture".into(),
                platform: "test".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
            },
        )
        .unwrap();
        host.create_space("space", "device", "/tmp/fixture", None, false)
            .unwrap();
        host.create_chat_with_parent("owner", Some("space"), Some("device"), None, None, None)
            .unwrap();
        let outline = WorkOutlineService::new(host);
        let item = outline
            .mutate(&WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
                space_id: "space".into(),
                parent_work_item_id: None,
                placement: Default::default(),
                title: "Fixture task".into(),
                description: None,
                status: None,
                operation_id: "item".into(),
            }))
            .unwrap()
            .work_item_id
            .unwrap();
        outline
            .mutate(&WorkOutlineMutation::AssignChatToWorkItem(
                AssignChatToWorkItemParams {
                    chat_id: "owner".into(),
                    work_item_id: item.clone(),
                    operation_id: "assignment".into(),
                },
            ))
            .unwrap();
        fixture
            .action_for(Action::Launch, &item)
            .execute_with(
                &ExternalRunSettings::default(),
                || Ok(()),
                |params| {
                    let mutation = WorkOutlineMutation::UpsertExternalRun(params);
                    outline
                        .mutate_as(&mutation, &outline.agent_actor("owner", None))
                        .unwrap();
                    Ok(())
                },
                |request| {
                    Ok(match request.phase {
                        "preflight" => admission(),
                        "launch" => identity(),
                        _ => panic!("unexpected phase {}", request.phase),
                    })
                },
            )
            .unwrap();
        let snapshot = outline.snapshot("space");
        let [run] = snapshot.external_runs.as_slice() else {
            panic!("expected one run")
        };
        assert_eq!(run.work_item_id, item);
        assert_eq!(run.run_tag, "box-run");
        assert_eq!(run.codespace_name.as_deref(), Some("gh-box-88xx"));
        assert_eq!(run.provider.as_deref(), Some("codex"));
        assert_eq!(run.session_id.as_deref(), Some("first-session"));
        assert_eq!(run.owner_chat_id.as_deref(), Some("owner"));
    }

    #[test]
    fn retry_after_playbook_edit_reconciles_the_original_operation() {
        let fixture = Fixture::new();
        let settings = ExternalRunSettings::default();
        let original = fixture.action(Action::Launch);
        assert_eq!(
            original.execute_with(
                &settings,
                || Ok(()),
                |_| panic!("no result"),
                |request| {
                    match request.phase {
                        "preflight" => Ok(admission()),
                        "launch" => Err("lost reply"),
                        _ => panic!("unexpected phase"),
                    }
                }
            ),
            Err("lost reply")
        );
        let source = fixture.root.path().join("source");
        let updated = fs::read_to_string(&source)
            .unwrap()
            .replace("First step", "Updated first step");
        fs::write(source, updated).unwrap();
        let retry = fixture.action(Action::Launch);
        assert_ne!(retry.digest, original.digest);
        assert_eq!(
            retry.execute_with(
                &settings,
                || Err("fresh approval missing"),
                |_| panic!("unauthorized"),
                |_| panic!("unauthorized")
            ),
            Err("fresh approval missing")
        );
        let result = retry
            .execute_with(
                &settings,
                || Ok(()),
                |params| {
                    assert_eq!(
                        params.operation_id,
                        mast_proto::derive_id(&original.digest, "codespace-external-run")
                    );
                    Ok(())
                },
                |request| {
                    assert_eq!(request.phase, "reconcile");
                    assert_eq!(request.operation_id, original.digest);
                    Ok(identity())
                },
            )
            .unwrap();
        assert_eq!(result["identity"], identity());
    }

    #[test]
    fn launch_requires_approval_readiness_and_capacity_before_mutation() {
        let fixture = Fixture::new();
        let action = fixture.action(Action::Launch);
        let settings = ExternalRunSettings::default();
        assert_eq!(
            action.execute_with(
                &settings,
                || Err("approval missing"),
                |_| panic!("recorded"),
                |_| panic!("contacted adapter")
            ),
            Err("approval missing")
        );
        for (ready, running, boxes, expected) in [
            (
                false,
                0,
                0,
                "The configured readiness or placement check blocked dispatch.",
            ),
            (true, 5, 6, "No running Codespace capacity is available."),
            (
                true,
                4,
                8,
                "The configured total Codespace limit has been reached.",
            ),
        ] {
            let settings = ExternalRunSettings {
                box_limit: Some(8),
                ..settings.clone()
            };
            assert_eq!(action.execute_with(&settings, || Ok(()), |_| panic!("recorded"), |request| {
                assert_eq!(request.phase, "preflight");
                Ok(json!({ "ready": ready, "runningOrStarting": running, "totalBoxes": boxes, "capacityCap": 5 }))
            }), Err(expected));
        }
    }

    #[test]
    fn resume_uses_the_same_session_and_full_playbook() {
        let fixture = Fixture::new();
        launch(&fixture);
        let action = fixture.action(Action::Resume);
        let settings = ExternalRunSettings::default();
        assert!(
            action
                .execute_with(
                    &settings,
                    || Ok(()),
                    |params| {
                        assert!(params.reopen);
                        assert_eq!(params.session_id.as_deref(), Some("first-session"));
                        Ok(())
                    },
                    |request| {
                        assert_eq!(serde_json::to_value(request.identity).unwrap(), identity());
                        Ok(match request.phase {
                            "inspect" => inspection("stalled", true, "open"),
                            "preflight" => admission(),
                            "resume" => {
                                let prompt = request.worker_prompt.unwrap();
                                assert!(prompt.contains("Review again after each push"));
                                assert!(prompt.contains("Recheck any outstanding wait"));
                                identity()
                            }
                            _ => panic!("unexpected phase"),
                        })
                    }
                )
                .is_ok()
        );
    }

    #[test]
    fn stop_and_cleanup_require_real_verification_and_preserved_transcript() {
        let fixture = Fixture::new();
        launch(&fixture);
        let settings = ExternalRunSettings::default();
        let stop = fixture.action(Action::Stop);
        for (field, value) in [
            ("checksPassed", json!(0)),
            ("reviewHeadSha", json!("old-head")),
            ("evidencePresent", json!(false)),
            ("finalReportComplete", json!(false)),
        ] {
            assert!(
                stop.execute_with(
                    &settings,
                    || Ok(()),
                    |_| panic!("not verified"),
                    |request| {
                        assert_eq!(request.phase, "inspect");
                        let mut observation = inspection("done", true, "open");
                        observation["review"][field] = value.clone();
                        Ok(observation)
                    }
                )
                .is_err()
            );
        }
        assert!(
            stop.execute_with(
                &settings,
                || Ok(()),
                |params| {
                    assert_eq!(params.outcome, Some(ExternalRunOutcome::Done));
                    Ok(())
                },
                |request| Ok(match request.phase {
                    "inspect" => inspection("done", true, "open"),
                    "stop" => {
                        assert!(request.review.unwrap().verified());
                        json!(true)
                    }
                    _ => panic!("unexpected phase"),
                })
            )
            .is_ok()
        );
        let cleanup = fixture.action(Action::Cleanup);
        assert!(
            cleanup
                .execute_with(
                    &settings,
                    || Ok(()),
                    |_| panic!("already ended"),
                    |request| {
                        assert_eq!(request.phase, "inspect");
                        Ok(inspection("done", false, "closedUnmerged"))
                    }
                )
                .unwrap_err()
                .contains("human decision")
        );
        let archived = Cell::new(false);
        assert!(
            cleanup
                .execute_with(
                    &settings,
                    || Ok(()),
                    |params| {
                        assert_eq!(params.expected_alive, Some(false));
                        Ok(())
                    },
                    |request| {
                        Ok(match request.phase {
                            "inspect" => inspection("done", false, "merged"),
                            "archive" => {
                                archived.set(true);
                                json!({ "transcriptReceipt": "durable archive" })
                            }
                            "cleanup" => {
                                assert!(archived.get());
                                assert_eq!(request.transcript_receipt, Some("durable archive"));
                                json!(true)
                            }
                            _ => panic!("unexpected phase"),
                        })
                    }
                )
                .is_ok()
        );
    }
}
