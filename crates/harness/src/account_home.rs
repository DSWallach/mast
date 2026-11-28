//! Per-account CLI homes. A chat pinned to a Mast-managed account runs its
//! CLI with `CLAUDE_CONFIG_DIR` / `CODEX_HOME` pointing at that account's
//! home, so each account has its own credentials, settings and session
//! history. Chats on the Default account get no override at all: the CLI
//! sees exactly the environment Mast itself has, and the live `~/.claude` /
//! `~/.codex` login is untouched.
//!
//! Claude Code keys its macOS Keychain item to the config dir
//! (`Claude Code-credentials-<first 8 hex of sha256(dir)>`, verified from the
//! 2.1.285 binary), so two homes never share credentials. [`CLAUDE_KEYCHAIN_ISOLATED`]
//! is the switch the engine reads when it prepares a managed Claude home: when
//! false it forces file credentials (`.credentials.json`, mode 0600) instead.
use mast_proto::HarnessId;
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// Phase 0 finding, see the module docs. Flip to `false` to make the engine
/// force file credentials in managed Claude homes.
pub const CLAUDE_KEYCHAIN_ISOLATED: bool = true;

/// The env vars that relocate this harness's home, if it has any. Antigravity
/// gets both Gemini names. `GEMINI_HOME` is the one Mast's own Antigravity
/// launch and path resolution use (`acp::antigravity_paths`);
/// `GEMINI_CLI_HOME` is the name the v0.3.0 plan specifies. Tested on
/// 2026-11-27 against the installed `agy` CLI: it ignores BOTH and keeps its
/// state in `$HOME/.gemini`, so these variables alone do not isolate `agy`.
pub fn home_envs(id: HarnessId) -> &'static [&'static str] {
    match id {
        HarnessId::ClaudeCode => &["CLAUDE_CONFIG_DIR"],
        HarnessId::Codex => &["CODEX_HOME"],
        HarnessId::Antigravity => &["GEMINI_HOME", "GEMINI_CLI_HOME"],
        _ => &[],
    }
}

/// The env var that relocates this harness's home, if it has one (the first of
/// [`home_envs`]).
pub fn home_env(id: HarnessId) -> Option<&'static str> {
    home_envs(id).first().copied()
}

/// Point `cmd` at a managed account home. `None` (Default) leaves the env alone.
pub fn apply(cmd: &mut crate::process::Command, id: HarnessId, home: Option<&Path>) {
    if let Some(home) = home {
        for var in home_envs(id) {
            cmd.as_std_mut().env(var, home);
        }
    }
}

// ── Worker environment scrubbing (P12) ──────────────────────────────────────
//
// A sandboxed worker (a `WorkspaceWrite` run that carries a scoped
// `MAST_AGENT_TOKEN`) must see exactly one account: the one it was assigned.
// The child's environment is therefore rebuilt from an allowlist rather than
// inherited and pruned, so a credential variable nobody thought of is dropped
// by default. Anything the engine set on the command itself (the scoped agent
// token, scheduled-task markers, `PATH` composition, the assigned home) is
// Mast's own and is kept as is.
//
// This hides env-carried secrets only. A worker can still read files its OS
// user can read; filesystem confinement is the sandbox's job, not this one's.

/// Env var listing extra names (comma or whitespace separated) a worker may
/// inherit. A deliberate, user-owned escape hatch for tools that need a
/// variable the allowlist drops.
pub const WORKER_ENV_PASSTHROUGH: &str = "MAST_WORKER_ENV_PASSTHROUGH";

/// Variables every worker needs to behave like a process in a normal session.
const BASE_NAMES: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PWD",
    "LANG",
    "LANGUAGE",
    "TERM",
    "COLORTERM",
    "NO_COLOR",
    "TMPDIR",
    "TMP",
    "TEMP",
    "TZ",
    "HOSTNAME",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "SSH_AUTH_SOCK",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "JAVA_HOME",
    "VIRTUAL_ENV",
    "NVM_DIR",
    "VOLTA_HOME",
    "PNPM_HOME",
    "BUN_INSTALL",
    "GOPATH",
    "GOROOT",
    "GOFLAGS",
    "GOPROXY",
    "GOCACHE",
    "GOMODCACHE",
    "GOTOOLCHAIN",
    "CC",
    "CXX",
    "CFLAGS",
    "CXXFLAGS",
    "LDFLAGS",
    "PKG_CONFIG_PATH",
    "SCCACHE_DIR",
];

/// Prefix classes a worker's toolchain needs. A name that looks secret is
/// still dropped (see [`looks_secret`]).
const BASE_PREFIXES: &[&str] = &[
    "LC_",
    "XDG_",
    "CARGO_",
    "RUSTUP_",
    "RUST",
    "NODE_",
    "NPM_CONFIG_",
    "PYTHON",
    "MAST_",
];

/// The assigned harness's own configuration and credential namespaces.
fn own_prefixes(id: HarnessId) -> &'static [&'static str] {
    match id {
        HarnessId::ClaudeCode => &["ANTHROPIC_", "CLAUDE_"],
        HarnessId::Codex => &["OPENAI_", "CODEX_"],
        HarnessId::Antigravity => &["GEMINI_", "GOOGLE_GENAI_", "ANTIGRAVITY_", "AGY_"],
        HarnessId::Cursor => &["CURSOR_"],
        HarnessId::Devin => &["DEVIN_"],
        HarnessId::Grok => &["XAI_", "GROK_"],
        HarnessId::Hermes => &["HERMES_"],
        HarnessId::Pi => &["PI_"],
        HarnessId::Opencode => &["OPENCODE_"],
        HarnessId::Mock => &["MAST_MOCK_"],
    }
}

/// Exact-name credentials of the assigned harness (not covered by a prefix).
fn own_names(id: HarnessId) -> &'static [&'static str] {
    match id {
        HarnessId::Antigravity => &["GOOGLE_API_KEY", "GOOGLE_CLOUD_PROJECT"],
        _ => &[],
    }
}

/// Credentials that would override a managed account's own login if they
/// leaked in from the user's shell. Dropped for a managed run even though they
/// belong to the assigned harness.
const OWN_LOGIN_OVERRIDES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
];

/// Every account-home variable of every harness.
fn is_any_home_var(name: &str) -> bool {
    HarnessId::ALL
        .iter()
        .any(|id| home_envs(*id).contains(&name))
}

/// Names that read as credentials. Applied inside prefix classes so a
/// toolchain or harness namespace cannot smuggle one through.
fn looks_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "API_KEY",
        "APIKEY",
        "PRIVATE_KEY",
        "_AUTH",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

/// Raw OAuth refresh tokens are never inherited, not even by their own harness.
fn is_refresh_token(name: &str) -> bool {
    name.to_ascii_uppercase().contains("REFRESH_TOKEN")
}

/// Whether a worker running `id` may inherit `name`. `managed` is true when
/// the run is pinned to a Mast-managed account home.
pub fn worker_env_allows(name: &str, id: HarnessId, managed: bool) -> bool {
    // The scoped token is injected by Mast on the command; an inherited one
    // belongs to another scope.
    if name == mast_proto::agent_tools::TOKEN_ENV || is_refresh_token(name) {
        return false;
    }
    if is_any_home_var(name) {
        // Only the assigned harness's Default home survives, and only when no
        // managed home replaces it.
        return !managed && home_envs(id).contains(&name);
    }
    if managed && OWN_LOGIN_OVERRIDES.contains(&name) {
        return false;
    }
    if BASE_NAMES.contains(&name) {
        return true;
    }
    if own_names(id).contains(&name) {
        return true;
    }
    // The assigned harness's own namespace legitimately holds its API key and
    // access token; refresh tokens were rejected above.
    if own_prefixes(id).iter().any(|p| name.starts_with(p)) {
        return true;
    }
    BASE_PREFIXES.iter().any(|p| name.starts_with(p)) && !looks_secret(name)
}

/// The variables of `inherited` a worker may keep, in input order.
pub fn filter_worker_env(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    id: HarnessId,
    managed: bool,
    passthrough: &[String],
) -> Vec<(OsString, OsString)> {
    inherited
        .into_iter()
        .filter(|(name, _)| {
            let Some(name) = name.to_str() else {
                return false;
            };
            worker_env_allows(name, id, managed)
                || (passthrough.iter().any(|p| p == name)
                    && name != mast_proto::agent_tools::TOKEN_ENV
                    && !is_refresh_token(name))
        })
        .collect()
}

/// Parse [`WORKER_ENV_PASSTHROUGH`].
pub fn parse_passthrough(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Rebuild `cmd`'s environment from `inherited` through the allowlist. Values
/// already set on `cmd` (the scoped token, scheduled-task markers, `PATH`) are
/// kept; `home` becomes the only account home the child can see.
pub fn scrub_worker_env_from(
    cmd: &mut std::process::Command,
    id: HarnessId,
    home: Option<&Path>,
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    passthrough: &[String],
) {
    let explicit: Vec<(OsString, Option<OsString>)> = cmd
        .get_envs()
        .map(|(k, v)| (k.to_owned(), v.map(OsStr::to_owned)))
        .collect();
    let kept = filter_worker_env(inherited, id, home.is_some(), passthrough);
    cmd.env_clear();
    for (name, value) in kept {
        cmd.env(name, value);
    }
    for (name, value) in explicit {
        match value {
            Some(value) => cmd.env(name, value),
            None => cmd.env_remove(name),
        };
    }
    if let Some(home) = home {
        for var in home_envs(id) {
            cmd.env(var, home);
        }
    }
}

/// Key in `AgentToolsInjection::env` the engine sets to `1` for a task worker
/// (a child chat with an Implementer, TestAuthor or DocWriter task and no
/// standing role, admin grant or assistant designation). It is the only signal
/// the harness has for "this run is a worker": a run without it, whatever its
/// token, keeps its full environment.
pub const WORKER_SCRUB_ENV: &str = "MAST_AGENT_SCRUB";

/// True for a run whose child must be scrubbed: a `WorkspaceWrite` run whose
/// agent tools carry the engine's worker flag ([`WORKER_SCRUB_ENV`]). Managers,
/// Leads, Verifiers, admin and assistant chats, interactive chats, read-only
/// and full-access runs are never flagged.
pub fn is_sandboxed_worker(
    sandbox: mast_proto::SandboxLevel,
    tools: Option<&crate::AgentToolsInjection>,
) -> bool {
    sandbox == mast_proto::SandboxLevel::WorkspaceWrite
        && tools.is_some_and(|t| t.env.get(WORKER_SCRUB_ENV).is_some_and(|v| v == "1"))
}

/// Scrub `cmd` from the live process environment when this run is a sandboxed
/// worker. Call after the command's own env is set; a no-op otherwise.
pub fn scrub_worker_env(
    cmd: &mut crate::process::Command,
    id: HarnessId,
    home: Option<&Path>,
    sandbox: mast_proto::SandboxLevel,
    tools: Option<&crate::AgentToolsInjection>,
) {
    if is_sandboxed_worker(sandbox, tools) {
        scrub_worker_env_now(cmd, id, home);
    }
}

/// [`scrub_worker_env`] without the run-shape gate, for a caller that already
/// decided (see [`is_sandboxed_worker`]).
pub fn scrub_worker_env_now(cmd: &mut crate::process::Command, id: HarnessId, home: Option<&Path>) {
    let passthrough = mast_proto::env::var(WORKER_ENV_PASSTHROUGH)
        .map(|raw| parse_passthrough(&raw))
        .unwrap_or_default();
    scrub_worker_env_from(
        cmd.as_std_mut(),
        id,
        home,
        std::env::vars_os(),
        &passthrough,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::SandboxLevel;
    use std::path::PathBuf;

    /// Synthetic values only: nothing here is a real credential.
    fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    fn names(vars: &[(OsString, OsString)]) -> Vec<String> {
        vars.iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn antigravity_homes_use_both_gemini_names() {
        assert_eq!(
            home_envs(HarnessId::Antigravity),
            ["GEMINI_HOME", "GEMINI_CLI_HOME"]
        );
        let mut cmd = crate::process::Command::new("agy");
        apply(
            &mut cmd,
            HarnessId::Antigravity,
            Some(Path::new("/m/gemini/a1")),
        );
        let set: Vec<_> = cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(set.contains(&("GEMINI_HOME".into(), Some("/m/gemini/a1".into()))));
        assert!(set.contains(&("GEMINI_CLI_HOME".into(), Some("/m/gemini/a1".into()))));
        // The Default account sets nothing, and Claude/Codex are unchanged.
        let mut none = crate::process::Command::new("agy");
        apply(&mut none, HarnessId::Antigravity, None);
        assert_eq!(none.as_std().get_envs().count(), 0);
        assert_eq!(home_env(HarnessId::ClaudeCode), Some("CLAUDE_CONFIG_DIR"));
        assert_eq!(home_env(HarnessId::Codex), Some("CODEX_HOME"));
        assert_eq!(home_env(HarnessId::Opencode), None);
    }

    #[test]
    fn a_managed_claude_worker_sees_only_its_own_home() {
        let inherited = env(&[
            ("PATH", "/usr/bin"),
            ("HOME", "/home/u"),
            // Other accounts' homes, in every spelling.
            ("CLAUDE_CONFIG_DIR", "/home/u/.claude-other"),
            ("CODEX_HOME", "/m/codex/b2"),
            ("GEMINI_HOME", "/m/gemini/c3"),
            ("GEMINI_CLI_HOME", "/m/gemini/c3"),
        ]);
        let mut cmd = std::process::Command::new("claude");
        scrub_worker_env_from(
            &mut cmd,
            HarnessId::ClaudeCode,
            Some(Path::new("/m/claude/a1")),
            inherited,
            &[],
        );
        let got: std::collections::BTreeMap<_, _> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(got["CLAUDE_CONFIG_DIR"].as_deref(), Some("/m/claude/a1"));
        for gone in ["CODEX_HOME", "GEMINI_HOME", "GEMINI_CLI_HOME"] {
            assert!(!got.contains_key(gone), "{gone} leaked");
        }
        assert_eq!(got["PATH"].as_deref(), Some("/usr/bin"));
    }

    #[test]
    fn each_removed_variable_class_is_dropped() {
        let cases: &[(&str, &str, HarnessId, bool)] = &[
            // class: another provider's API key / credentials
            (
                "other-provider key",
                "OPENAI_API_KEY",
                HarnessId::ClaudeCode,
                false,
            ),
            (
                "other-provider key",
                "ANTHROPIC_API_KEY",
                HarnessId::Codex,
                false,
            ),
            (
                "other-provider key",
                "GEMINI_API_KEY",
                HarnessId::Codex,
                false,
            ),
            (
                "other-provider key",
                "XAI_API_KEY",
                HarnessId::ClaudeCode,
                false,
            ),
            // class: raw OAuth refresh tokens, even for the assigned harness
            (
                "refresh token",
                "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
                HarnessId::ClaudeCode,
                false,
            ),
            (
                "refresh token",
                "CODEX_REFRESH_TOKEN",
                HarnessId::Codex,
                false,
            ),
            (
                "refresh token",
                "GOOGLE_OAUTH_REFRESH_TOKEN",
                HarnessId::Antigravity,
                false,
            ),
            // class: other accounts' homes
            ("other home", "CODEX_HOME", HarnessId::ClaudeCode, false),
            ("other home", "CLAUDE_CONFIG_DIR", HarnessId::Codex, true),
            (
                "other home",
                "GEMINI_CLI_HOME",
                HarnessId::ClaudeCode,
                false,
            ),
            ("other home", "GEMINI_HOME", HarnessId::Codex, true),
            // class: an inherited agent token belongs to another scope
            (
                "agent token",
                "MAST_AGENT_TOKEN",
                HarnessId::ClaudeCode,
                false,
            ),
            // class: generic ambient secrets
            ("ambient secret", "GH_TOKEN", HarnessId::ClaudeCode, false),
            ("ambient secret", "GITHUB_TOKEN", HarnessId::Codex, false),
            (
                "ambient secret",
                "AWS_SECRET_ACCESS_KEY",
                HarnessId::ClaudeCode,
                false,
            ),
            (
                "ambient secret",
                "AWS_ACCESS_KEY_ID",
                HarnessId::ClaudeCode,
                false,
            ),
            ("ambient secret", "NPM_TOKEN", HarnessId::ClaudeCode, false),
            (
                "ambient secret",
                "CARGO_REGISTRY_TOKEN",
                HarnessId::ClaudeCode,
                false,
            ),
            (
                "ambient secret",
                "CARGO_REGISTRIES_PRIVATE_TOKEN",
                HarnessId::Codex,
                false,
            ),
            (
                "ambient secret",
                "NPM_CONFIG__AUTH",
                HarnessId::Codex,
                false,
            ),
            (
                "ambient secret",
                "MAST_SIGNING_SECRET",
                HarnessId::Codex,
                false,
            ),
            (
                "ambient secret",
                "DBUS_SESSION_BUS_ADDRESS",
                HarnessId::Codex,
                false,
            ),
            (
                "unknown variable",
                "SOME_VENDOR_KEY",
                HarnessId::Codex,
                false,
            ),
            // class: a managed account's own login must not be overridden from the shell
            (
                "login override",
                "ANTHROPIC_API_KEY",
                HarnessId::ClaudeCode,
                true,
            ),
            (
                "login override",
                "ANTHROPIC_AUTH_TOKEN",
                HarnessId::ClaudeCode,
                true,
            ),
            (
                "login override",
                "CLAUDE_CODE_OAUTH_TOKEN",
                HarnessId::ClaudeCode,
                true,
            ),
            ("login override", "OPENAI_API_KEY", HarnessId::Codex, true),
            ("login override", "CODEX_API_KEY", HarnessId::Codex, true),
            (
                "login override",
                "GEMINI_API_KEY",
                HarnessId::Antigravity,
                true,
            ),
            (
                "login override",
                "GOOGLE_API_KEY",
                HarnessId::Antigravity,
                true,
            ),
        ];
        for (class, name, harness, managed) in cases {
            assert!(
                !worker_env_allows(name, *harness, *managed),
                "{class}: {name} must be dropped for {harness:?} (managed={managed})"
            );
        }
    }

    #[test]
    fn what_a_worker_needs_is_kept() {
        for (name, harness, managed) in [
            ("PATH", HarnessId::ClaudeCode, true),
            ("HOME", HarnessId::Codex, true),
            ("LC_ALL", HarnessId::Codex, false),
            ("XDG_CONFIG_HOME", HarnessId::ClaudeCode, true),
            ("CARGO_TARGET_DIR", HarnessId::Codex, false),
            ("RUSTFLAGS", HarnessId::Codex, false),
            ("HTTPS_PROXY", HarnessId::ClaudeCode, true),
            ("MAST_IPC_PORT", HarnessId::ClaudeCode, true),
            ("SSH_AUTH_SOCK", HarnessId::Codex, false),
            // The Default account keeps its own env-configured login.
            ("ANTHROPIC_API_KEY", HarnessId::ClaudeCode, false),
            ("OPENAI_API_KEY", HarnessId::Codex, false),
            ("CLAUDE_CODE_USE_BEDROCK", HarnessId::ClaudeCode, true),
            // …and its own relocated home.
            ("CLAUDE_CONFIG_DIR", HarnessId::ClaudeCode, false),
            ("CODEX_HOME", HarnessId::Codex, false),
            ("GEMINI_CLI_HOME", HarnessId::Antigravity, false),
        ] {
            assert!(
                worker_env_allows(name, harness, managed),
                "{name} for {harness:?} managed={managed}"
            );
        }
    }

    #[test]
    fn the_escape_hatch_admits_a_named_variable_but_never_a_token_or_refresh_token() {
        let pass = parse_passthrough("GH_TOKEN, AWS_PROFILE\nMAST_AGENT_TOKEN CODEX_REFRESH_TOKEN");
        assert_eq!(pass.len(), 4);
        let kept = filter_worker_env(
            env(&[
                ("GH_TOKEN", "synthetic-gh"),
                ("AWS_PROFILE", "dev"),
                ("MAST_AGENT_TOKEN", "synthetic-other-scope"),
                ("CODEX_REFRESH_TOKEN", "synthetic-refresh"),
                ("NPM_TOKEN", "synthetic-npm"),
            ]),
            HarnessId::ClaudeCode,
            false,
            &pass,
        );
        assert_eq!(names(&kept), ["GH_TOKEN", "AWS_PROFILE"]);
    }

    #[test]
    fn the_scrub_applies_only_to_workspace_write_runs_with_an_agent_token() {
        let tools = |flag: Option<&str>| crate::AgentToolsInjection {
            server_name: "mast-agent".into(),
            command: PathBuf::from("/bin/true"),
            args: Vec::new(),
            env: [("MAST_AGENT_TOKEN".to_owned(), "zat_synthetic".to_owned())]
                .into_iter()
                .chain(flag.map(|v| (WORKER_SCRUB_ENV.to_owned(), v.to_owned())))
                .collect(),
        };
        let flagged = tools(Some("1"));
        assert!(is_sandboxed_worker(
            SandboxLevel::WorkspaceWrite,
            Some(&flagged)
        ));
        // A token alone is not enough: Manager, Lead, Verifier, admin and
        // interactive runs carry one but are never flagged.
        let unflagged = tools(None);
        assert!(!is_sandboxed_worker(
            SandboxLevel::WorkspaceWrite,
            Some(&unflagged)
        ));
        assert!(!is_sandboxed_worker(
            SandboxLevel::WorkspaceWrite,
            Some(&tools(Some("0")))
        ));
        assert!(!is_sandboxed_worker(SandboxLevel::WorkspaceWrite, None));
        assert!(!is_sandboxed_worker(SandboxLevel::ReadOnly, Some(&flagged)));
        assert!(!is_sandboxed_worker(
            SandboxLevel::DangerFullAccess,
            Some(&flagged)
        ));
        // A run that is not a sandboxed worker keeps the command untouched.
        let mut cmd = crate::process::Command::new("claude");
        cmd.env("KEEP", "1");
        scrub_worker_env(
            &mut cmd,
            HarnessId::ClaudeCode,
            None,
            SandboxLevel::DangerFullAccess,
            Some(&flagged),
        );
        assert_eq!(cmd.as_std().get_envs().count(), 1);
        // …and so does an unflagged WorkspaceWrite run, token or not.
        scrub_worker_env(
            &mut cmd,
            HarnessId::ClaudeCode,
            None,
            SandboxLevel::WorkspaceWrite,
            Some(&unflagged),
        );
        assert_eq!(cmd.as_std().get_envs().count(), 1);
    }

    /// The real child, not just the filter: spawn `env` with a scrubbed command
    /// and read what the process actually received.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_spawned_worker_child_env_excludes_unassigned_homes_and_refresh_tokens() {
        let assigned = PathBuf::from("/m/codex/assigned");
        let inherited = env(&[
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/u"),
            ("CODEX_HOME", "/m/codex/other-account"),
            ("CLAUDE_CONFIG_DIR", "/m/claude/other-account"),
            ("GEMINI_HOME", "/m/gemini/other-account"),
            ("GEMINI_CLI_HOME", "/m/gemini/other-account"),
            ("CODEX_REFRESH_TOKEN", "rt-synthetic-1"),
            ("CLAUDE_CODE_OAUTH_REFRESH_TOKEN", "rt-synthetic-2"),
            ("ANTHROPIC_API_KEY", "key-synthetic-1"),
            ("OPENAI_API_KEY", "key-synthetic-2"),
            ("GH_TOKEN", "gh-synthetic"),
            ("MAST_AGENT_TOKEN", "zat_inherited_other_scope"),
        ]);
        let mut cmd = crate::process::Command::new("/usr/bin/env");
        cmd.env("MAST_AGENT_TOKEN", "zat_scoped_to_this_run");
        scrub_worker_env_from(
            cmd.as_std_mut(),
            HarnessId::Codex,
            Some(&assigned),
            inherited,
            &[],
        );
        let out = cmd.output().await.unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        let lines: std::collections::BTreeSet<&str> = text.lines().collect();
        assert!(lines.contains("CODEX_HOME=/m/codex/assigned"));
        assert!(lines.contains("MAST_AGENT_TOKEN=zat_scoped_to_this_run"));
        assert!(lines.contains("HOME=/home/u"));
        for forbidden in [
            "other-account",
            "rt-synthetic",
            "key-synthetic",
            "gh-synthetic",
            "zat_inherited_other_scope",
            "CLAUDE_CONFIG_DIR",
            "GEMINI_",
        ] {
            assert!(
                !text.contains(forbidden),
                "{forbidden} reached the worker child:\n{text}"
            );
        }
    }
}
