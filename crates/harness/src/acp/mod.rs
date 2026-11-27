//! ACP harness: spawns an Agent Client Protocol agent (JSON-RPC 2.0 over
//! stdio, protocol v1) and maps its session updates onto [`AgentEvent`]s.
//!
//! KEPT ONLY for agents built ground-up on ACP: Grok ([`AcpHarness::grok`],
//! `grok agent stdio`), Devin ([`AcpHarness::devin`], `devin acp`) and Hermes
//! ([`AcpHarness::hermes`], `hermes acp`) and Antigravity
//! ([`AcpHarness::antigravity`], Google's `agy_acp_server`, installed from its
//! pinned release archive) — plus pi ([`AcpHarness::pi`]) via the community
//! `pi-acp` adapter until a native driver exists. Claude, Codex and Cursor moved to native drivers
//! ([`crate::ClaudeHarness`], [`crate::CodexHarness`], [`crate::CursorHarness`])
//! after adapter-mediated ACP kept manufacturing done-status bugs the native
//! wires don't have (turn-hold bookkeeping vs the CLI's own eager result).
//!
//! - `initialize` (protocolVersion 1, fs/terminal capabilities declined) →
//!   `session/new`, or `session/load` with a fresh-session fallback when
//!   resuming; replayed history during a load is dropped (the doc already
//!   holds it).
//! - `session/prompt` owns the turn: its response's `stopReason` ends the
//!   turn (`cancelled` → Interrupted, `refusal` → Errored, else Completed).
//! - `session/update` notifications normalize per [`normalize::map_update`].
//! - Permission requests auto-accept with the agent's preferred allow option
//!   (zeron sessions run unattended); question-shaped requests block on the
//!   engine's input bridge.
//! - Steering: agents advertising `_session/steering` get mid-turn injection;
//!   others queue steers and deliver them as the next `session/prompt` at the
//!   turn boundary. The session stays parked between turns while the
//!   steering mailbox lives.
//! - Interrupt: `session/cancel`, escalating SIGTERM → SIGKILL; the stream
//!   always ends with `Done { status: Interrupted }`.

mod agent_specs;
mod antigravity_authentication;
mod antigravity_context;
mod antigravity_paths;
mod devin_models;
mod normalize;
mod session;
mod subagent;
mod subagent_devin;
mod system_message;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

use mast_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ModelOption, ModelOptionChoice, ReasoningLevel,
    RunRequest, SlashCommand, UserInputAnswer, UserInputQuestion,
};

use crate::jsonrpc::{Incoming, RpcClient};
use crate::process::{Command, Stdio};
use crate::scratch::ScratchDir;
use child::Child;
use session::run_session;
mod child;
use crate::{Harness, HarnessError, RunControls, Signal, send_signal, shutdown_child};
use agent_specs::*;
pub(crate) use agent_specs::{agy_install_paths, find_on_paths};
pub use agent_specs::{antigravity_home, can_install, install_harness};
pub use antigravity_authentication::SignInProgress;
use antigravity_authentication::*;
use normalize::{map_update, parse_commands, preferred_allow_option};
use subagent::SubagentTracker;
use subagent_devin::DevinTracker;

const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(120);
/// Largest Work Outline envelope the first-turn channel writes into a prompt.
const FIRST_TURN_MAX_BYTES: usize = 256 * 1024;

/// ACP agents that take the Work Outline envelope as text ahead of their first
/// prompt. Antigravity is not here: its envelope rides the system prompt.
fn first_turn_instruction_agent(harness: HarnessId) -> bool {
    matches!(
        harness,
        HarnessId::Devin | HarnessId::Grok | HarnessId::Hermes | HarnessId::Pi
    )
}
const DEFAULT_MODEL_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
// The one-file server unpacks on launch. A local cold probe took 1.903s, but
// slower disks need substantially more headroom than the generic 10s budget.
const ANTIGRAVITY_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(90);
/// Background-install managed npm adapters for agents whose CLI is present
/// on this device, so a first chat never pays (or trips over) an npm run.
/// Skips agents whose adapter is already resolvable; failures are logged and
/// retried on the next daemon start or blocking launch. A no-op outside a
/// tokio runtime. Archive-distributed servers require explicit installation.
pub fn prewarm_managed_adapters() {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    for spec in [grok_spec(), pi_spec()] {
        let Some(pkg) = spec.npm_package else {
            continue;
        };
        let pin = crate::adapter_install::NpmPin::parse(pkg);
        if find_on_paths(spec.executable, (spec.extra_paths)()).is_some()
            || crate::adapter_install::installed_entry(&pin, spec.executable).is_some()
            || find_on_paths(spec.cli_executable, (spec.cli_extra_paths)()).is_none()
            || crate::adapter_install::find_npm().is_none()
        {
            continue;
        }
        let (bin_name, display_name) = (spec.executable, spec.display_name);
        handle.spawn(async move {
            match crate::adapter_install::ensure_installed(pin, bin_name, display_name).await {
                Ok(entry) => tracing::info!(
                    target: "mast_harness::adapter_install",
                    adapter = %entry.display(),
                    "prewarmed {display_name} ACP adapter"
                ),
                Err(e) => tracing::warn!(
                    target: "mast_harness::adapter_install",
                    "prewarm of the {display_name} ACP adapter failed: {e}"
                ),
            }
        });
    }
}

/// A resolved launch: a concrete program, or a managed npm adapter that may
/// still need installing (see [`AcpHarness::resolve_program`]).
enum Launch {
    Program(PathBuf, Vec<String>),
    Managed {
        pin: crate::adapter_install::NpmPin,
        bin_name: &'static str,
        args: Vec<String>,
    },
    Archive {
        pin: crate::archive_install::ArchivePin,
        args: Vec<String>,
    },
}

/// The ACP harness. Construct with [`AcpHarness::grok`]; tests point it at a
/// fake agent with [`AcpHarness::with_executable`].
pub struct AcpHarness {
    spec: AcpAgentSpec,
    executable: Option<PathBuf>,
    /// Mast-owned cwd for automatic catalog probes. Never a user project or HOME.
    model_discovery_cwd: Option<PathBuf>,
    /// Override of the agent's on-disk sessions root (grok's
    /// `~/.grok/sessions`), where subagent transcripts are tailed from.
    sessions_root: Option<PathBuf>,
    /// Grace between `session/cancel` and SIGTERM.
    interrupt_grace: Duration,
    /// Grace between SIGTERM and SIGKILL.
    kill_grace: Duration,
    /// Bound on the initialize → session handshake; a hang past it errors the
    /// run instead of spinning "Working" forever.
    handshake_timeout: Duration,
    /// Bound on the initialize → session/new probe used to populate the model
    /// picker. OpenCode shares this with its real startup budget because both
    /// paths wait for the same plugin-heavy boot.
    model_discovery_timeout: Duration,
    /// Discovery result cache: the advertised commands survive across calls.
    commands: tokio::sync::OnceCell<Vec<SlashCommand>>,
    /// Retain successful catalogs per credential/binary context through outages.
    models_cache: crate::catalog::Catalog,
    devin_models: devin_models::Catalog,
}

impl AcpHarness {
    fn with_spec(spec: AcpAgentSpec) -> Self {
        Self {
            spec,
            executable: None,
            model_discovery_cwd: None,
            sessions_root: None,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
            // Generous: the handshake is local work for every agent
            // (session/load replays from disk), so a hang past this is a
            // wedged agent, not a slow one.
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            model_discovery_timeout: DEFAULT_MODEL_DISCOVERY_TIMEOUT,
            commands: tokio::sync::OnceCell::new(),
            models_cache: crate::catalog::Catalog::default(),
            devin_models: devin_models::Catalog::default(),
        }
    }

    /// Devin (`devin acp`) — Cognition's native ACP server.
    pub fn devin() -> Self {
        Self::with_spec(devin_spec())
    }

    /// Grok Build (`grok agent stdio`) — xAI's native ACP agent.
    pub fn grok() -> Self {
        Self::with_spec(grok_spec())
    }

    /// Hermes Agent (`hermes acp`) — Nous Research's native ACP server.
    pub fn hermes() -> Self {
        Self::with_spec(hermes_spec())
    }

    /// The pi coding agent over ACP — the community `pi-acp` adapter wrapping
    /// pi's RPC mode.
    pub fn pi() -> Self {
        Self::with_spec(pi_spec()).with_model_discovery_timeout(Duration::from_secs(60))
    }

    /// google antigravity over its acp server (`agy_acp_server`).
    pub fn antigravity() -> Self {
        Self::with_spec(antigravity_spec())
            .with_model_discovery_timeout(ANTIGRAVITY_DISCOVERY_TIMEOUT)
    }

    /// Use a fixed agent binary instead of PATH/known-location resolution.
    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    /// Run automatic model probes and their `session/new` in this directory.
    pub fn with_model_discovery_cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.model_discovery_cwd = Some(path.into());
        self
    }

    /// Test seam: tail subagent transcripts from this sessions root instead
    /// of the agent's real one (`~/.grok/sessions`).
    #[doc(hidden)]
    pub fn with_sessions_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.sessions_root = Some(root.into());
        self
    }

    /// Tune the interrupt→SIGTERM→SIGKILL escalation timing.
    pub fn with_graces(mut self, interrupt_grace: Duration, kill_grace: Duration) -> Self {
        self.interrupt_grace = interrupt_grace;
        self.kill_grace = kill_grace;
        self
    }

    /// Tune the handshake bound (tests shrink it; default 120s).
    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Tune the model-probe bound (tests shrink it; OpenCode defaults to the
    /// same five-minute, environment-overridable budget as its real startup).
    #[doc(hidden)]
    pub fn with_model_discovery_timeout(mut self, timeout: Duration) -> Self {
        self.model_discovery_timeout = timeout;
        self
    }

    /// Test seam: the program `run` would spawn (the adapter binary, or —
    /// for a managed npm adapter — its installed entry, else npm as the
    /// installer that would run first).
    #[doc(hidden)]
    pub fn launch_program(&self) -> Result<PathBuf, HarnessError> {
        match self.resolve_launch()? {
            Launch::Program(program, _) => Ok(program),
            Launch::Managed { pin, bin_name, .. } => {
                match crate::adapter_install::installed_entry(&pin, bin_name) {
                    Some(entry) => Ok(entry),
                    None => crate::adapter_install::find_npm()
                        .ok_or_else(|| HarnessError::NotInstalled(self.spec.install_hint.into())),
                }
            }
            Launch::Archive { pin, .. } => crate::archive_install::entry_path(&pin)
                .ok_or_else(|| HarnessError::NotInstalled(self.spec.install_hint.into())),
        }
    }

    fn env_override_path(&self) -> Option<PathBuf> {
        if let Some(p) = std::env::var_os(self.spec.env_override)
            && !p.is_empty()
        {
            return Some(PathBuf::from(p));
        }
        if self.spec.id == HarnessId::Antigravity
            && let Some(p) = std::env::var_os("ANTIGRAVITY_ACP_EXECUTABLE")
            && !p.is_empty()
        {
            return Some(PathBuf::from(p));
        }
        None
    }

    fn launch_args_for(&self, program: &Path) -> Vec<String> {
        if self.spec.id == HarnessId::Antigravity
            && program
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("agy_acp_server"))
        {
            return if cfg!(target_os = "linux") {
                ANTIGRAVITY_LINUX_ARGS
                    .iter()
                    .map(|a| (*a).to_string())
                    .collect()
            } else {
                Vec::new()
            };
        }
        self.spec.args.iter().map(|a| (*a).to_string()).collect()
    }

    /// Resolve what to spawn: an explicit/installed adapter binary, or the
    /// managed install of the spec's pinned npm package. `NotInstalled` only
    /// when neither the binary nor the machinery to install it (npm) exists.
    fn find_server(&self) -> Option<PathBuf> {
        find_on_paths(self.spec.executable, (self.spec.extra_paths)()).or_else(|| {
            if self.spec.id == HarnessId::Antigravity {
                ["agy_acp_server", "agy_acp_server.par"]
                    .into_iter()
                    .find_map(|name| find_on_paths(name, Vec::new()))
            } else {
                None
            }
        })
    }

    fn resolve_launch(&self) -> Result<Launch, HarnessError> {
        let spec_args: Vec<String> = self.spec.args.iter().map(|a| a.to_string()).collect();
        if let Some(p) = &self.executable {
            return crate::executable::validate_native_override(p).map(|program| {
                let args = self.launch_args_for(&program);
                Launch::Program(program, args)
            });
        }
        if let Some(p) = self.env_override_path() {
            return crate::executable::validate_native_override(&p).map(|program| {
                let args = self.launch_args_for(&program);
                Launch::Program(program, args)
            });
        }
        if let Some(found) = self.find_server() {
            let args = self.launch_args_for(&found);
            return Ok(Launch::Program(found, args));
        }
        if let Some(pkg) = self.spec.npm_package {
            let pin = crate::adapter_install::NpmPin::parse(pkg);
            if crate::adapter_install::installed_entry(&pin, self.spec.executable).is_some()
                || crate::adapter_install::find_npm().is_some()
            {
                return Ok(Launch::Managed {
                    pin,
                    bin_name: self.spec.executable,
                    args: spec_args,
                });
            }
        }
        if let Some(pin) = self.spec.archive {
            if crate::archive_install::installed_entry(&pin).is_none() {
                return Err(HarnessError::NotInstalled(self.spec.install_hint.into()));
            }
            return Ok(Launch::Archive {
                pin,
                args: spec_args,
            });
        }
        Err(HarnessError::NotInstalled(self.spec.install_hint.into()))
    }

    /// Resolve to a concrete (program, args), running the managed install if
    /// it hasn't completed yet. `block_on_install: false` (discovery paths)
    /// never waits on npm: it kicks the install in the background and errors
    /// out, so a picker open falls back to the static catalog instead of
    /// stalling for however long a 500MB dependency tree takes to land.
    /// Resolve the server, optionally waiting for its managed installation.
    #[doc(hidden)]
    pub async fn resolve_program(
        &self,
        block_on_install: bool,
    ) -> Result<(PathBuf, Vec<String>), HarnessError> {
        match self.resolve_launch()? {
            Launch::Program(program, args) => Ok((program, args)),
            Launch::Managed {
                pin,
                bin_name,
                args,
            } => {
                let entry = match crate::adapter_install::installed_entry(&pin, bin_name) {
                    Some(entry) => entry,
                    None if block_on_install => {
                        crate::adapter_install::ensure_installed(
                            pin,
                            bin_name,
                            self.spec.display_name,
                        )
                        .await?
                    }
                    None => {
                        let display_name = self.spec.display_name;
                        tokio::spawn(async move {
                            if let Err(e) = crate::adapter_install::ensure_installed(
                                pin,
                                bin_name,
                                display_name,
                            )
                            .await
                            {
                                tracing::warn!(
                                    target: "mast_harness::adapter_install",
                                    "background adapter install failed: {e}"
                                );
                            }
                        });
                        return Err(HarnessError::Protocol(format!(
                            "{} adapter is installing in the background",
                            self.spec.display_name
                        )));
                    }
                };
                let (program, mut node_args) = crate::adapter_install::launch_for_entry(&entry)?;
                node_args.extend(args);
                Ok((program, node_args))
            }
            Launch::Archive { pin, args } => {
                let entry = crate::archive_install::installed_entry(&pin)
                    .ok_or_else(|| HarnessError::NotInstalled(self.spec.install_hint.into()))?;
                Ok((entry, args))
            }
        }
    }

    fn configure_adapter_environment(&self, cmd: &mut Command, executable: &Path) {
        if self.spec.id == HarnessId::Antigravity
            && let Some(parent) = executable.parent()
        {
            let sibling = parent.join("localharness_external");
            if sibling.is_file() {
                cmd.env("ANTIGRAVITY_HARNESS_PATH", sibling);
                cmd.env("PYTHONUNBUFFERED", "1");
            }
        }
    }

    fn adapter_scratch(&self) -> Result<Option<ScratchDir>, HarnessError> {
        Ok(matches!(self.resolve_launch()?, Launch::Archive { .. })
            .then(|| ScratchDir::new(self.spec.executable))
            .transpose()?)
    }

    async fn spawn_agent(
        &self,
        cwd: Option<&str>,
        block_on_install: bool,
        extra_args: &[String],
        scheduled: Option<crate::ScheduledEnv<'_>>,
    ) -> Result<(Option<ScratchDir>, Child, crate::StderrTail), HarnessError> {
        let (exe, args) = self.resolve_program(block_on_install).await?;
        let mut cmd = Command::new(&exe);
        cmd.args(args);
        cmd.args(extra_args);
        child::configure(&mut cmd);
        crate::compose_child_path(&mut cmd, &exe);
        self.configure_adapter_environment(&mut cmd, &exe);
        crate::export_scheduled_task(&mut cmd, scheduled);
        if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
            cmd.current_dir(cwd);
        }
        if self.spec.id == HarnessId::Antigravity {
            cmd.env("GEMINI_HOME", antigravity_paths::home()?);
            // Python webbrowser accepts an executable template; never launch a browser here.
            #[cfg(unix)]
            cmd.env("BROWSER", "/usr/bin/true %s");
        }
        let scratch = self.adapter_scratch()?;
        if let Some(dir) = &scratch {
            dir.apply(&mut cmd);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(crate::executable::binary_hint(&exe))
            } else {
                HarnessError::Io(e)
            }
        })?;
        let mut child = Child::new(child);
        let stderr_tail = crate::StderrTail::default();
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "mast_harness::acp", "stderr: {line}");
                    tail.push(&line);
                }
                tail.close();
            });
        }
        Ok((scratch, child, stderr_tail))
    }

    /// Short-lived discovery run for [`Harness::commands`]: initialize, scan
    /// the response, then try one unauthenticated `session/new` and wait
    /// briefly for `available_commands_update`. Best-effort — an agent that
    /// refuses sessions before login still surfaces whatever the handshake
    /// advertised.
    async fn discover_commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        let (_scratch, mut child, _stderr) = self.spawn_agent(None, false, &[], None).await?;
        let (client, mut incoming) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => RpcClient::new(stdin, stdout),
            _ => {
                child.shutdown(self.kill_grace).await;
                return Err(HarnessError::Protocol("agent child has no stdio".into()));
            }
        };
        let discovery = async {
            let init = client
                .request("initialize", initialize_params(self.spec.id))
                .await?;
            let mut commands = scan_available_commands(&init);
            if commands.is_empty() {
                let cwd = crate::executable::home_or_current_dir();
                let session = client
                    .request("session/new", json!({ "cwd": cwd, "mcpServers": [] }))
                    .await;
                if session.is_ok() {
                    // The update usually arrives within milliseconds of the
                    // session response; 2s bounds a quiet agent.
                    let deadline = tokio::time::sleep(Duration::from_secs(2));
                    tokio::pin!(deadline);
                    loop {
                        tokio::select! {
                            inc = incoming.recv() => match inc {
                                Some(Incoming::Notification { method, params })
                                    if method == "session/update" =>
                                {
                                    let update = params.get("update").cloned().unwrap_or(Value::Null);
                                    if update.get("sessionUpdate").and_then(Value::as_str)
                                        == Some("available_commands_update")
                                    {
                                        commands = parse_commands(update.get("availableCommands"));
                                        break;
                                    }
                                }
                                Some(Incoming::Request { id, .. }) => {
                                    client.respond_error(&id, -32601, "unsupported during discovery");
                                }
                                Some(_) => {}
                                None => break,
                            },
                            _ = &mut deadline => break,
                        }
                    }
                }
            }
            Ok::<Vec<SlashCommand>, HarnessError>(commands)
        };
        let result = tokio::time::timeout(self.model_discovery_timeout, discovery).await;
        child.shutdown(self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol("command discovery timed out".into())),
        }
    }

    /// One short-lived probe for the agent's real model list: initialize →
    /// `session/new`, then read the response's first-class `models`
    /// (SessionModelState) with the `model` config option as fallback. The
    /// wire is the source of truth — the spec's static catalog only enriches
    /// matching entries and names the pick when the agent advertises nothing.
    async fn discover_models(&self) -> Result<Vec<Model>, HarnessError> {
        let discovery_cwd =
            crate::executable::model_discovery_cwd(self.model_discovery_cwd.as_deref())?;
        let discovery_cwd_string = discovery_cwd.to_string_lossy().into_owned();
        let (_scratch, mut child, stderr_tail) = self
            .spawn_agent(Some(&discovery_cwd_string), false, &[], None)
            .await?;
        let (client, _incoming) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => RpcClient::new(stdin, stdout),
            _ => {
                child.shutdown(self.kill_grace).await;
                return Err(HarnessError::Protocol("agent child has no stdio".into()));
            }
        };
        let discovery = async {
            client
                .request("initialize", initialize_params(self.spec.id))
                .await?;
            let session = client
                .request(
                    "session/new",
                    json!({ "cwd": discovery_cwd, "mcpServers": [] }),
                )
                .await?;
            let mut models = models_from_session(&session, &(self.spec.models)());
            // Prompt-convention modes (Claude Ultrathink) extend any real
            // ladder — never an effort-less model's empty one.
            for model in &mut models {
                if !model.reasoning_levels.is_empty() {
                    for extra in self.spec.ladder_extras {
                        if !model.reasoning_levels.contains(extra) {
                            model.reasoning_levels.push(*extra);
                        }
                    }
                }
            }
            if self.spec.effort_in_model_id {
                models = group_effort_variants(models);
            }
            if self.spec.id == HarnessId::Antigravity {
                antigravity_context::enrich_antigravity_models(&mut models);
            }
            Ok::<Vec<Model>, HarnessError>(models)
        };
        let result = tokio::time::timeout(self.model_discovery_timeout, discovery).await;
        child.shutdown(self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => {
                let mut error = format!(
                    "{} model discovery did not complete within {}s",
                    self.spec.display_name,
                    self.model_discovery_timeout.as_secs()
                );
                if let Some(stderr) = stderr_tail.snapshot() {
                    error.push_str("; stderr: ");
                    error.push_str(&stderr);
                }
                Err(HarnessError::Protocol(error))
            }
        }
    }
}

/// Map an advertised `thought_level` value id onto zeron's ladder.
fn reasoning_from_value(value: &str) -> Option<ReasoningLevel> {
    match norm_id(value).as_str() {
        "minimal" => Some(ReasoningLevel::Minimal),
        "low" => Some(ReasoningLevel::Low),
        "medium" => Some(ReasoningLevel::Medium),
        "high" => Some(ReasoningLevel::High),
        "xhigh" => Some(ReasoningLevel::XHigh),
        "max" => Some(ReasoningLevel::Max),
        "ultra" => Some(ReasoningLevel::Ultra),
        "ultracode" => Some(ReasoningLevel::Ultracode),
        "ultrathink" => Some(ReasoningLevel::Ultrathink),
        _ => None,
    }
}

/// Derive the model list a `session/new` response advertises. The `model`
/// config option's choices come FIRST, the legacy first-class `models` state
/// is only a fallback: the org adapters enumerate one `availableModels` entry
/// per model × effort combination on that deprecated surface (Zed dropped it
/// entirely), while their `configOptions` carry base model ids with effort as
/// a separate `thought_level` option. `[1m]`-suffixed long-context variants
/// collapse into the base model's Context Window trait, matching the static
/// catalogs. Traits come off the wire too — every select/boolean config
/// option outside mode/model/thought_level becomes a `ModelOption` — so
/// unmatched models keep fast mode etc.; the catalog only enriches matched
/// ids with label/description/per-model ladders.
fn models_from_session(session_response: &Value, catalog: &[Model]) -> Vec<Model> {
    let config_options = session_response
        .get("configOptions")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default();

    let ladder: Vec<ReasoningLevel> = config_options
        .iter()
        .find(|o| o.get("category").and_then(Value::as_str) == Some("thought_level"))
        .and_then(|o| o.get("options").and_then(Value::as_array))
        .map(|opts| {
            opts.iter()
                .filter_map(|o| o.get("value").and_then(Value::as_str))
                .filter_map(reasoning_from_value)
                .collect()
        })
        .unwrap_or_default();
    let wire_options: Vec<ModelOption> = config_options
        .iter()
        .filter_map(trait_from_config_option)
        .collect();

    let exact = |id: &str| catalog.iter().find(|m| norm_id(&m.id) == norm_id(id));
    // Family-alias catalog row: the claude adapter advertises bare aliases
    // (`opus`, `sonnet`, `haiku`) meaning "the current generation" — match
    // them to the first (flagship-ordered) catalog row of that family so
    // the picker shows the curated label/ladder ("Opus 5") instead of the
    // terse alias. Alphabetic-only ids ONLY: versioned ids
    // (`gpt-5.2-codex`) must never fuzzy-match a foreign row.
    let alias = |id: &str| {
        let norm = norm_id(id);
        (!norm.is_empty() && norm.chars().all(|c| c.is_ascii_alphabetic()))
            .then(|| catalog.iter().find(|m| norm_id(&m.id).contains(&norm)))
            .flatten()
    };
    let build = |id: &str,
                 name: Option<&str>,
                 description: Option<&str>,
                 options: Vec<ModelOption>|
     -> Model {
        let exact = exact(id);
        let aliased = if exact.is_none() { alias(id) } else { None };
        let known = exact.or(aliased);
        // The wire name wins for real ids; an ALIAS row's terse wire name
        // ("Opus") loses to the curated family label/description.
        Model {
            id: id.to_owned(),
            label: aliased
                .map(|m| m.label.clone())
                .or_else(|| name.map(str::to_owned))
                .or_else(|| known.map(|m| m.label.clone()))
                .unwrap_or_else(|| id.to_owned()),
            description: aliased
                .and_then(|m| m.description.clone())
                .or_else(|| description.map(str::to_owned))
                .or_else(|| known.and_then(|m| m.description.clone())),
            reasoning_levels: match known.filter(|m| !m.reasoning_levels.is_empty()) {
                Some(m) => m.reasoning_levels.clone(),
                None => ladder.clone(),
            },
            options,
        }
    };

    let model_select: Vec<&Value> = config_options
        .iter()
        .find(|o| o.get("category").and_then(Value::as_str) == Some("model"))
        .and_then(|o| o.get("options").and_then(Value::as_array))
        .map(|opts| opts.iter().collect())
        .unwrap_or_default();
    if !model_select.is_empty() {
        let raw_ids: Vec<&str> = model_select
            .iter()
            .filter_map(|o| o.get("value").and_then(Value::as_str))
            .collect();
        // `default` is an ALIAS row (Claude Code's "Default (recommended)"),
        // duplicating whichever real model the CLI resolves it to — dropped
        // whenever a real row exists (it read as clutter in the picker, user
        // request). Send-side, a chat that saved `default` still matches the
        // advertised value exactly.
        let has_real = raw_ids.iter().any(|id| norm_id(id) != "default");
        return model_select
            .iter()
            .filter_map(|o| {
                let id = o.get("value").and_then(Value::as_str)?;
                if has_real && norm_id(id) == "default" {
                    return None;
                }
                let name = o.get("name").and_then(Value::as_str);
                let description = o.get("description").and_then(Value::as_str);
                let mut options = wire_options.clone();
                if let Some(base) = strip_context_hint(id) {
                    // A 1M variant with its bare base advertised too folds
                    // into THAT row's Context Window trait (added below).
                    if raw_ids.contains(&base) {
                        return None;
                    }
                    // Orphan 1M variant (`opus[1m]` with no bare `opus` —
                    // the CLI pins the 1M window): present it AS the base
                    // model with the trait defaulting to 1M, instead of a
                    // one-off "Opus (1M context)" row (user request). The
                    // send path recomposes the advertised id via
                    // `pick_model_value`'s compose/family fallback.
                    let mut window = crate::claude::catalog::context_window();
                    window.default_choice = "1m".into();
                    options.push(window);
                    return Some(build(
                        base,
                        name.map(strip_trailing_parenthetical)
                            .filter(|n| !n.is_empty()),
                        description,
                        options,
                    ));
                }
                if raw_ids
                    .iter()
                    .any(|raw| strip_context_hint(raw) == Some(id))
                {
                    options.push(crate::claude::catalog::context_window());
                }
                Some(build(id, name, description, options))
            })
            .collect();
    }

    // Legacy fallback for agents predating session config options. The
    // catalog's own option sets apply here — nothing arrives on the wire.
    session_response
        .get("models")
        .and_then(|m| m.get("availableModels"))
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let id = m.get("modelId").and_then(Value::as_str)?;
            Some(build(
                id,
                m.get("name").and_then(Value::as_str),
                m.get("description").and_then(Value::as_str),
                exact(id).map(|k| k.options.clone()).unwrap_or_default(),
            ))
        })
        .collect()
}

/// A session config option surfaced as a Traits-dropdown section. Mode is
/// zeron's own (forced to the no-prompts choice), model rides the model rows,
/// and thought_level is the Reasoning ladder — everything else the agent
/// advertises (fast mode, collaboration mode, agent persona, …) passes
/// through. `currentValue` doubles as the default: it is the state the
/// session opens in. Booleans render as an off/on select, mirroring the
/// catalogs (zeron never declares the boolean config capability, so adapters
/// send selects, but handle the shape defensively).
fn trait_from_config_option(option: &Value) -> Option<ModelOption> {
    if matches!(
        option.get("category").and_then(Value::as_str),
        Some("mode" | "model" | "thought_level")
    ) {
        return None;
    }
    let id = option.get("id").and_then(Value::as_str)?;
    let label = option.get("name").and_then(Value::as_str).unwrap_or(id);
    match option.get("type").and_then(Value::as_str)? {
        "select" => {
            let choices: Vec<ModelOptionChoice> = option
                .get("options")
                .and_then(Value::as_array)?
                .iter()
                .filter_map(|c| {
                    let id = c.get("value").and_then(Value::as_str)?;
                    Some(ModelOptionChoice {
                        id: id.to_owned(),
                        label: c
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or(id)
                            .to_owned(),
                    })
                })
                .collect();
            let default_choice = option
                .get("currentValue")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| choices.first().map(|c| c.id.clone()))?;
            (choices.len() > 1).then(|| ModelOption {
                id: id.to_owned(),
                label: label.to_owned(),
                choices,
                default_choice,
            })
        }
        "boolean" => Some(ModelOption {
            id: id.to_owned(),
            label: label.to_owned(),
            choices: vec![
                ModelOptionChoice {
                    id: "off".into(),
                    label: "Off".into(),
                },
                ModelOptionChoice {
                    id: "on".into(),
                    label: "On".into(),
                },
            ],
            default_choice: if option.get("currentValue") == Some(&Value::Bool(true)) {
                "on".into()
            } else {
                "off".into()
            },
        }),
        _ => None,
    }
}

#[async_trait]
impl Harness for AcpHarness {
    fn id(&self) -> HarnessId {
        self.spec.id
    }
    fn display_name(&self) -> &str {
        self.spec.display_name
    }
    fn capabilities(&self) -> crate::HarnessCapabilities {
        crate::capabilities(self.id())
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        self.spec.reasoning_levels
    }

    /// The agent's own CLI, not the adapter: `claude` counts as installed even
    /// when `claude-agent-acp` would arrive via npx, and an npx-reachable
    /// adapter does NOT count when the CLI itself is missing. Explicit
    /// executables (tests, `*_EXECUTABLE` overrides) always count.
    fn installed(&self) -> bool {
        // Overrides go through the same validation as `resolve_launch`, so an
        // override that points at nothing reports not-installed instead of an
        // agent that shows up in the composer and then fails to launch.
        if let Some(p) = &self.executable {
            return crate::executable::validate_native_override(p).is_ok();
        }
        if let Some(p) = self.env_override_path() {
            return crate::executable::validate_native_override(&p).is_ok();
        }
        if self
            .spec
            .archive
            .as_ref()
            .is_some_and(|pin| crate::archive_install::installed_entry(pin).is_some())
        {
            return true;
        }
        if self.spec.id == HarnessId::Antigravity {
            self.find_server().is_some()
        } else {
            find_on_paths(self.spec.cli_executable, (self.spec.cli_extra_paths)()).is_some()
        }
    }

    /// Devin refreshes through its native catalog command on each request.
    /// Other ACP agents use a fresh session probe, with the spec's static
    /// catalog as fallback when they advertise nothing or probing fails.
    fn model_context(&self) -> Result<Option<crate::ModelContext>, HarnessError> {
        let binary = match self.resolve_launch()? {
            Launch::Program(path, _) => path,
            Launch::Managed { pin, bin_name, .. } => {
                crate::adapter_install::installed_entry(&pin, bin_name)
                    .unwrap_or_else(|| PathBuf::from(format!("{}@{}", pin.name, pin.version)))
            }
            Launch::Archive { pin, .. } => crate::archive_install::installed_entry(&pin)
                .unwrap_or_else(|| PathBuf::from(format!("{}@{}", pin.name, pin.version))),
        };
        let extra = if self.id() == HarnessId::Antigravity {
            let home = antigravity_paths::home()?;
            let mut files = Vec::new();
            for dir in ["antigravity-cli", "antigravity-acp"] {
                let root = home.join(dir);
                for name in [
                    "settings.json",
                    "acp_auth_state.json",
                    "oauth_creds.json",
                    "google_accounts.json",
                    "credentials.json",
                    "auth.json",
                ] {
                    files.push(root.join(name));
                }
            }
            files
        } else {
            vec![]
        };
        crate::model_context::context(self.id(), &binary, &extra).map(Some)
    }
    fn fallback_models(&self) -> Vec<Model> {
        (self.spec.models)()
    }
    async fn model_catalog(&self, force: bool) -> Result<crate::ModelCatalog, HarnessError> {
        self.model_context()?.unwrap().log();
        self.models_cache
            .get_with_timeout(
                force,
                self.model_discovery_timeout * 3 + Duration::from_secs(1),
                || self.model_context().map(|c| c.unwrap().key()),
                || async {
                    if self.id() == HarnessId::Devin {
                        let (exe, _) = self.resolve_program(false).await?;
                        let discovery_cwd = crate::executable::model_discovery_cwd(
                            self.model_discovery_cwd.as_deref(),
                        )?;
                        self.devin_models
                            .refresh(&exe, &discovery_cwd, self.model_discovery_timeout)
                            .await
                    } else {
                        self.discover_models().await
                    }
                },
            )
            .await
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.resolve_launch()?;
        let mut models = match self.model_catalog(true).await {
            Ok(catalog) => catalog.models,
            Err(error)
                if self.id() == HarnessId::Devin
                    || !crate::CatalogFailure::classify(&error).allows_stale() =>
            {
                return Err(error);
            }
            Err(error) => {
                tracing::warn!(%error, source = "static", "Model discovery failed");
                self.fallback_models()
            }
        };
        if self.spec.id == HarnessId::Antigravity {
            antigravity_context::enrich_antigravity_models(&mut models);
        }
        Ok(models)
    }

    /// the agent's advertised commands minus the spec's hidden ones, then its
    /// skills. Skills are read fresh on every call so a newly added one shows
    /// up, and they still list when discovery fails (a signed-out agent).
    /// `spec.skill_dirs` is a fixed, global set of folders (see e.g.
    /// `antigravity_skill_dirs`) — the ACP agents here don't have a chat cwd
    /// to discover project-scoped skills from, unchanged here.
    async fn commands(&self, _cwd: &str) -> Result<Vec<SlashCommand>, HarnessError> {
        let discovered = self
            .commands
            .get_or_try_init(|| self.discover_commands())
            .await
            .cloned();
        let skills = skill_commands(&(self.spec.skill_dirs)());
        let mut commands = match discovered {
            Ok(commands) => commands,
            Err(_) if !skills.is_empty() => Vec::new(),
            Err(error) => return Err(error),
        };
        commands.retain(|command| !self.spec.hidden_commands.contains(&command.name.as_str()));
        for skill in skills {
            if !commands.iter().any(|command| command.name == skill.name) {
                commands.push(skill);
            }
        }
        Ok(commands)
    }

    fn instruction_support(&self) -> crate::InstructionSupport {
        if self.spec.id == HarnessId::Antigravity {
            if self.installed() {
                crate::InstructionSupport::Supported {
                    channel: crate::InstructionChannel::SystemPromptAppend,
                    replace_strategy: crate::InstructionReplaceStrategy::RespawnResume,
                    max_bytes: 256 * 1024,
                }
            } else {
                crate::InstructionSupport::Unsupported {
                    reason: self.spec.install_hint.into(),
                    upgrade_hint: Some("Install Antigravity (agy CLI)".into()),
                }
            }
        } else if first_turn_instruction_agent(self.spec.id) {
            // The envelope rides the first prompt of a new session (see
            // `first_turn_envelope`); the ACP session has no system-prompt field.
            if self.installed() {
                crate::InstructionSupport::Supported {
                    channel: crate::InstructionChannel::FirstTurnPrompt,
                    replace_strategy: crate::InstructionReplaceStrategy::FreshSession,
                    max_bytes: FIRST_TURN_MAX_BYTES,
                }
            } else {
                crate::InstructionSupport::Unsupported {
                    reason: self.spec.install_hint.into(),
                    upgrade_hint: Some(format!("Install {}", self.spec.display_name)),
                }
            }
        } else {
            crate::InstructionSupport::no_channel()
        }
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.run_with_tools(request, controls, None).await
    }

    async fn run_with_agent_tools(
        &self,
        request: RunRequest,
        controls: RunControls,
        tools: crate::AgentToolsInjection,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.run_with_tools(request, controls, Some(tools)).await
    }
}

impl AcpHarness {
    /// Antigravity's system-prompt append for a run that names an agent: the
    /// definition's instructions, then the Work Outline envelope. `None` when
    /// the run names no agent. Antigravity has no `--agent`, so Mast resolves
    /// the definition itself (see [`crate::agent_definition`]).
    async fn agent_instructions(&self, request: &RunRequest) -> Result<Option<String>, HarnessError> {
        if self.spec.id != HarnessId::Antigravity
            || mast_proto::agent_option(&request.model_options).is_none()
        {
            return Ok(None);
        }
        let Some(agent) =
            crate::agent_definition::resolve_for_request(self.id(), request, None).await?
        else {
            return Ok(None);
        };
        let composed = crate::agent_definition::compose(
            &agent,
            request
                .instructions
                .as_ref()
                .map(|delivery| delivery.envelope.rendered.as_str()),
        );
        crate::agent_definition::require_channel_fits(
            self.id(),
            &agent,
            &self.instruction_support(),
            &composed,
        )?;
        Ok(Some(composed))
    }

    async fn run_with_tools(
        &self,
        request: RunRequest,
        controls: RunControls,
        agent_tools: Option<crate::AgentToolsInjection>,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        crate::require_read_only_support(self.id(), &request)?;
        crate::require_instruction_support(self.id(), &self.instruction_support(), &request)?;
        crate::require_named_agent_support(self.id(), self.capabilities(), &request)?;
        if let Some(delivery) = &request.instructions {
            let digest = mast_proto::envelope_sha256(&delivery.envelope.rendered);
            if digest != delivery.envelope.envelope_sha256 {
                return Err(HarnessError::Capability {
                    harness: self.id(),
                    detail: "the rendered instructions do not match their envelope hash".into(),
                    upgrade_hint: None,
                });
            }
        }
        let agent_instructions = self.agent_instructions(&request).await?;
        let first_turn_envelope = if first_turn_instruction_agent(self.spec.id) {
            crate::first_turn_envelope(self.id(), &request)?.map(str::to_owned)
        } else {
            None
        };
        // A `/command` first turn cannot carry the envelope as text, the same
        // rule opencode applies: refuse before spawning rather than drop it.
        if first_turn_envelope.is_some() && request.prompt.starts_with('/') {
            return Err(HarnessError::Capability {
                harness: self.id(),
                detail: "a slash command cannot carry Work Outline instructions on its first \
                         turn; send the instructions with a plain message"
                    .into(),
                upgrade_hint: None,
            });
        }
        let (scratch, mut child, stderr_tail) =
            self.spawn_agent(Some(&request.cwd), true, &[], crate::ScheduledEnv::of(&request)).await?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Protocol("agent child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("agent child has no stdout".into()))?;
        let (client, incoming) = RpcClient::new(stdin, stdout);
        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        tokio::spawn(run_session(Session {
            child,
            scratch,
            client,
            incoming,
            event_tx,
            controls,
            request,
            agent_tools,
            agent_instructions,
            first_turn_envelope,
            harness: self.spec.id,
            agent_name: self.spec.display_name,
            permission_title_is_command: self.spec.permission_title_is_command,
            prompt_transform: self.spec.prompt_transform,
            effort_values: self.spec.effort_values,
            prompt_complete_extension: self.spec.prompt_complete_extension,
            prompt_stall: self.spec.prompt_stall,
            stall_hint: self.spec.stall_hint,
            effort_in_model_id: self.spec.effort_in_model_id,
            auth_method: self.spec.auth_method,
            sessions_root: self.sessions_root.clone(),
            interrupt_grace: self.interrupt_grace,
            kill_grace: self.kill_grace,
            handshake_timeout: self.handshake_timeout,
            stderr_tail,
        }));

        let events = futures::stream::unfold(event_rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        })
        .boxed();
        Ok(if self.spec.id == HarnessId::Antigravity {
            system_message::strip_system_message_echoes(events)
        } else {
            events
        })
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

struct Session {
    child: Child,
    scratch: Option<ScratchDir>,
    client: RpcClient,
    incoming: mpsc::Receiver<Incoming>,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    controls: RunControls,
    request: RunRequest,
    agent_tools: Option<crate::AgentToolsInjection>,
    /// Antigravity: a named agent's definition and the envelope, composed.
    agent_instructions: Option<String>,
    /// The verified envelope written ahead of the first prompt (a fresh
    /// session, or a revision change). `None` when the session already carries it.
    first_turn_envelope: Option<String>,
    harness: HarnessId,
    agent_name: &'static str,
    permission_title_is_command: bool,
    prompt_complete_extension: bool,
    prompt_stall: Option<Duration>,
    stall_hint: &'static str,
    effort_in_model_id: bool,
    auth_method: Option<&'static str>,
    /// Sessions-root override for the subagent transcript tail (tests).
    sessions_root: Option<PathBuf>,
    prompt_transform: fn(Option<ReasoningLevel>, &str) -> String,
    effort_values: fn(Option<ReasoningLevel>, Option<&str>) -> Vec<&'static str>,
    interrupt_grace: Duration,
    kill_grace: Duration,
    handshake_timeout: Duration,
    stderr_tail: crate::StderrTail,
}

fn initialize_params(harness: HarnessId) -> Value {
    let mut capabilities = json!({
        "fs": { "readTextFile": false, "writeTextFile": false },
        "terminal": false,
    });
    if harness == HarnessId::Devin {
        // Devin otherwise exposes only the parent's run_subagent call. This
        // unlocks lifecycle tags plus every nested message, thought, and tool
        // update, all of which DevinTracker can route. Do not advertise the
        // separate subagentControl extension: Mast has no matching UI yet.
        capabilities["_meta"] = json!({ "cognition.ai/subagentSupport": true });
    }
    json!({
        "protocolVersion": 1,
        "clientInfo": {
            "name": "zeron",
            "title": "Mast",
            "version": env!("CARGO_PKG_VERSION"),
        },
        // Declined: agents fall back to their own fs/terminal access, which
        // is what zeron wants — the working tree is the source of truth for
        // the diff pane, and commands belong to the agent's own sandbox.
        "clientCapabilities": capabilities,
    })
}

/// `initialize._meta.steering.supported` — the `_session/steering` extension
/// both org-maintained adapters advertise (not part of the v1 spec).
fn steering_supported(init: &Value) -> bool {
    init.get("_meta")
        .and_then(|m| m.get("steering"))
        .and_then(|s| s.get("supported"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Depth-limited scan for an `availableCommands` array anywhere in a response
/// (agents differ on where the handshake advertises them: top level, inside
/// `agentCapabilities`, or `_meta`).
fn scan_available_commands(value: &Value) -> Vec<SlashCommand> {
    fn scan(value: &Value, depth: u8) -> Option<&Value> {
        if depth == 0 {
            return None;
        }
        let obj = value.as_object()?;
        if let Some(cmds) = obj.get("availableCommands").filter(|c| c.is_array()) {
            return Some(cmds);
        }
        obj.values().find_map(|v| scan(v, depth - 1))
    }
    parse_commands(scan(value, 4))
}

fn new_message_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Rotate the assistant message id; returns (previous, next).
fn rotate(id: &mut String) -> (String, String) {
    let prev = std::mem::replace(id, new_message_id());
    (prev, id.clone())
}

async fn send(tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>, ev: AgentEvent) -> bool {
    tx.send(Ok(ev)).await.is_ok()
}

/// Normalize an option/model-option id for matching across naming styles
/// (`fastMode` == `fast_mode` == `fast-mode`).
fn norm_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Whether a model id carries the 1M-context hint, in either spelling: the
/// display form `opus[1m]` or the SDK-id form `claude-opus-4-6-1m`.
fn context_hint_1m(id: &str) -> bool {
    id.contains("[1m]") || id.ends_with("-1m")
}

/// The id with a trailing long-context hint removed; `None` when it carries
/// none.
fn strip_context_hint(id: &str) -> Option<&str> {
    id.strip_suffix("[1m]").or_else(|| id.strip_suffix("-1m"))
}

/// A wire display name with its trailing parenthetical removed
/// ("Opus (1M context)" → "Opus") — a folded base row must not keep the
/// variant tag.
fn strip_trailing_parenthetical(name: &str) -> &str {
    match name.rfind(" (") {
        Some(at) if name.ends_with(')') => name[..at].trim_end(),
        _ => name,
    }
}

/// Pick the advertised model value for a requested model id. Agents differ in
/// what they advertise: full ids (`claude-opus-5`), SDK aliases
/// (`opus`, `sonnet`, `haiku` — the claude adapter), and long-context
/// variants in either hint spelling. Exact match first (with the 1M compose
/// when the run selects the 1M window), then a family-token fallback that
/// prefers a variant matching the requested context window.
fn pick_model_value(requested: &str, available: &[&str], context_1m: bool) -> Option<String> {
    if context_1m {
        for composed in [format!("{requested}[1m]"), format!("{requested}-1m")] {
            if available.contains(&composed.as_str()) {
                return Some(composed);
            }
        }
    }
    if available.contains(&requested) {
        return Some(requested.to_owned());
    }
    // Family fallback: "claude-opus-5" → "opus" matches "opus[1m]".
    let family = ["fable", "opus", "sonnet", "haiku", "gpt"]
        .into_iter()
        .find(|f| norm_id(requested).contains(f))?;
    let candidates: Vec<&&str> = available
        .iter()
        .filter(|v| norm_id(v).contains(family))
        .collect();
    candidates
        .iter()
        .find(|v| context_hint_1m(v) == context_1m)
        .or_else(|| candidates.first())
        .map(|v| (**v).to_owned())
}

/// Pick a first-class ACP model switch when the session uses the legacy
/// `models` state instead of a category=model config option. Grok Build 1.0.5
/// has exactly this shape and accepts the selected id through
/// `session/set_model`; treating its advertised models as config options makes
/// the picker look functional while every selection is silently ignored.
///
/// Config options remain preferred when present: org adapters can expose a
/// legacy `models` matrix alongside the canonical base-model config option.
fn first_class_model_change(
    session_response: &Value,
    requested: Option<&str>,
) -> Result<Option<String>, HarnessError> {
    let Some(requested) = requested else {
        return Ok(None);
    };
    let has_model_config = session_response
        .get("configOptions")
        .and_then(Value::as_array)
        .is_some_and(|options| {
            options.iter().any(|option| {
                option.get("type").and_then(Value::as_str) == Some("select")
                    && option.get("category").and_then(Value::as_str) == Some("model")
            })
        });
    if has_model_config {
        return Ok(None);
    }

    let Some(models) = session_response.get("models") else {
        return Ok(None);
    };
    let available: Vec<&str> = models
        .get("availableModels")
        .and_then(Value::as_array)
        .map(|models| models.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|model| model.get("modelId").and_then(Value::as_str))
        .collect();
    if available.is_empty() {
        return Ok(None);
    }
    if !available.contains(&requested) {
        return Err(HarnessError::Protocol(format!(
            "agent does not advertise requested model {requested}; available models: {}",
            available.join(", ")
        )));
    }
    if models.get("currentModelId").and_then(Value::as_str) == Some(requested) {
        return Ok(None);
    }
    Ok(Some(requested.to_owned()))
}

fn validate_config_model_selection(
    session_response: &Value,
    requested: Option<&str>,
    model_options: &serde_json::Map<String, Value>,
) -> Result<(), HarnessError> {
    let Some(requested) = requested else {
        return Ok(());
    };
    let Some(option) = session_response
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|options| {
            options.iter().find(|option| {
                option.get("type").and_then(Value::as_str) == Some("select")
                    && option.get("category").and_then(Value::as_str) == Some("model")
            })
        })
    else {
        return Ok(());
    };
    let available: Vec<&str> = option
        .get("options")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|choice| choice.get("value").and_then(Value::as_str))
        .collect();
    let context_1m = model_options
        .get("contextWindow")
        .and_then(Value::as_str)
        .is_some_and(|window| window.eq_ignore_ascii_case("1m") || window == "1000000");
    if pick_model_value(requested, &available, context_1m).is_some() {
        return Ok(());
    }
    Err(HarnessError::Protocol(format!(
        "agent does not advertise requested model {requested}; available models: {}",
        available.join(", ")
    )))
}

fn is_model_config_option(session_response: &Value, config_id: &str) -> bool {
    session_response
        .get("configOptions")
        .and_then(Value::as_array)
        .is_some_and(|options| {
            options.iter().any(|option| {
                option.get("id").and_then(Value::as_str) == Some(config_id)
                    && option.get("category").and_then(Value::as_str) == Some("model")
            })
        })
}

/// The `session/set_config_option` calls a session response's `configOptions`
/// warrant for this run:
/// - the requested model (category `model`; a `contextWindow: "1m"` model
///   option composes the `<model>[1m]` id first, the CLI's own convention),
/// - the effort (category `thought_level`, first advertised value from the
///   spec's preference list),
/// - any remaining `model_options` matched by normalized id — selects take
///   the choice id, booleans take `on`/`true` truthiness (fastMode, thinking).
///
/// Matched against advertised values and skipped when already current. Pure
/// so it's testable; the returned value is the request's flattened `value`
/// payload (select: `{"value": id}`, boolean: `{"type":"boolean","value": b}`).
fn config_option_sets(
    session_response: &Value,
    model: Option<&str>,
    efforts: &[&'static str],
    model_options: &serde_json::Map<String, Value>,
    rules_mode: bool,
) -> Vec<(String, Value)> {
    let Some(options) = session_response
        .get("configOptions")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let context_1m = model_options
        .get("contextWindow")
        .and_then(Value::as_str)
        .is_some_and(|w| w.eq_ignore_ascii_case("1m") || w == "1000000");
    let mut sets = Vec::new();
    for option in options {
        let Some(config_id) = option.get("id").and_then(Value::as_str) else {
            continue;
        };
        let kind = option.get("type").and_then(Value::as_str).unwrap_or("");
        let category = option.get("category").and_then(Value::as_str);
        let current = option.get("currentValue");
        let available: Vec<&str> = option
            .get("options")
            .and_then(Value::as_array)
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|o| o.get("value").and_then(Value::as_str))
            .collect();

        let wanted: Option<Value> = match (kind, category) {
            ("select", Some("model")) => model
                .and_then(|m| pick_model_value(m, &available, context_1m))
                .map(Value::String),
            // Unattended parity with the retired custom adapters (claude
            // bypassPermissions / codex approvalPolicy never): pick the
            // no-prompts mode when the agent offers one. claude-agent-acp
            // calls it `bypassPermissions`, codex-acp `agent-full-access`
            // (approvalPolicy "never" + danger-full-access sandbox), Devin
            // `bypass`. Cursor instead exposes agent/plan/ask — those arrive
            // as a Traits "Mode" option and win when the run selected one.
            ("select", Some("mode")) => model_options
                .get("mode")
                .and_then(Value::as_str)
                .filter(|c| available.contains(c))
                .map(|c| Value::String(c.to_owned()))
                // Rules mode needs the agent to keep asking.
                .or_else(|| {
                    (!rules_mode).then_some(()).and_then(|()| {
                        [
                            "bypassPermissions",
                            "bypass_permissions",
                            "bypass",
                            "yolo",
                            "agent-full-access",
                            "danger-full-access",
                            "full-access",
                        ]
                        .into_iter()
                        .find(|v| available.contains(v))
                        .map(|v| Value::String(v.to_owned()))
                    })
                }),
            ("select", Some("thought_level")) => efforts
                .iter()
                .find(|c| available.contains(*c))
                .map(|c| Value::String((*c).to_owned())),
            // Everything else: best-effort match against the run's
            // model-option selections by normalized id.
            _ => model_options.iter().find_map(|(opt_id, choice)| {
                if norm_id(opt_id) != norm_id(config_id) || opt_id == "contextWindow" {
                    return None;
                }
                match kind {
                    "select" => choice
                        .as_str()
                        .filter(|c| available.contains(c))
                        .map(|c| Value::String(c.to_owned())),
                    "boolean" => {
                        let on = choice == &Value::Bool(true)
                            || choice
                                .as_str()
                                .is_some_and(|c| c.eq_ignore_ascii_case("on"));
                        Some(Value::Bool(on))
                    }
                    _ => None,
                }
            }),
        };
        if let Some(value) = wanted
            && current != Some(&value)
        {
            let payload = match value {
                Value::Bool(b) => serde_json::json!({ "type": "boolean", "value": b }),
                other => serde_json::json!({ "value": other }),
            };
            sets.push((config_id.to_owned(), payload));
        }
    }
    sets
}

/// Per-agent subagent correlation: Devin maps tagged ACP updates inline
/// (`cognition.ai/subagent_*` lifecycle on ordinary `session/update`s),
/// Grok tails child transcripts from disk (inert for agents that never
/// emit its `subagent_*` extension). Both produce the same
/// [`AgentEvent::Subagent`] contract.
enum SubagentObserver {
    Devin(DevinTracker),
    Grok(SubagentTracker),
}

impl SubagentObserver {
    fn observe(&mut self, update: &Value) {
        match self {
            SubagentObserver::Devin(_) => {}
            SubagentObserver::Grok(tracker) => tracker.observe(update),
        }
    }

    fn finish_open(&mut self, status: DoneStatus) -> Vec<AgentEvent> {
        match self {
            SubagentObserver::Devin(tracker) => tracker.finish_open(status),
            SubagentObserver::Grok(_) => Vec::new(),
        }
    }
}

/// The events of one notification, session-filtered. `session/update` maps
/// per [`map_update`]; `_x.ai/session_notification` is grok's extension
/// channel — same `{sessionId, update}` envelope, but its updates (the
/// `subagent_*` lifecycle) render nothing directly. The subagent tracker sees
/// both first (spawn/finished correlation + transcript tails); its tagged
/// events flow from its own tasks, not this return value.
fn session_update_events(
    method: &str,
    params: &Value,
    session_id: &str,
    subagents: &mut SubagentObserver,
) -> Vec<AgentEvent> {
    if params.get("sessionId").and_then(Value::as_str) != Some(session_id) {
        return Vec::new();
    }
    let update = params.get("update").unwrap_or(&Value::Null);
    match method {
        "session/update" => match subagents {
            SubagentObserver::Devin(tracker) => tracker.map(update),
            _ => {
                subagents.observe(update);
                map_update(update)
            }
        },
        "_x.ai/session_notification" => {
            subagents.observe(update);
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Per-turn token usage from a settled `session/prompt` response, when the
/// adapter attaches it (tolerant of both field spellings; absent → nothing).
fn usage_from_response(res: &Result<Value, HarnessError>) -> Option<AgentEvent> {
    let resp = res.as_ref().ok()?;
    // Grok settles usage on the response `_meta` (inputTokens/outputTokens —
    // verified live, 1.0.4); adapters used a first-class `usage` object.
    let usage = resp.get("usage").or_else(|| resp.get("_meta"))?;
    let count = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| usage.get(*k))
            .and_then(Value::as_u64)
    };
    let input = count(&["inputTokens", "input_tokens"]);
    let output = count(&["outputTokens", "output_tokens"]);
    (input.is_some() || output.is_some()).then(|| AgentEvent::Usage {
        input_tokens: input.unwrap_or(0),
        output_tokens: output.unwrap_or(0),
    })
}

/// Settled context usage snapshot from a finished `session/prompt` response,
/// when the response carries explicit context-occupancy (`used`) or capacity
/// fields. Turn-cumulative `totalTokens` and `inputTokens` are billing totals
/// across all model steps in the turn (emitted via [`usage_from_response`]),
/// never context window occupancy; session start already seeds the fallback
/// window from `effective_context_window`.
fn context_usage_from_response(res: &Result<Value, HarnessError>) -> Option<AgentEvent> {
    let resp = res.as_ref().ok()?;
    let usage = resp.get("usage").or_else(|| resp.get("_meta"))?;
    let count = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| usage.get(*k))
            .and_then(Value::as_u64)
    };
    let tokens = count(&["used"]);
    let window = count(&[
        "max",
        "limit",
        "size",
        "contextWindow",
        "context_window",
        "modelContextWindow",
        "model_context_window",
    ])
    .filter(|n| *n > 0);
    (tokens.is_some() || window.is_some()).then_some(AgentEvent::ContextUsage { tokens, window })
}

/// Map a finished `session/prompt` result to the run's terminal status.
fn stop_outcome(
    res: &Result<Value, HarnessError>,
    interrupted: bool,
) -> (DoneStatus, Option<String>) {
    if interrupted {
        return (DoneStatus::Interrupted, None);
    }
    match res {
        Ok(resp) => match resp.get("stopReason").and_then(Value::as_str) {
            Some("cancelled") => (DoneStatus::Interrupted, None),
            Some("error") => (
                DoneStatus::Errored,
                Some("The agent failed to complete the turn.".to_owned()),
            ),
            Some("refusal") => (
                DoneStatus::Errored,
                Some("The agent refused to continue.".to_owned()),
            ),
            // end_turn / max_tokens / max_turn_requests: the turn ended;
            // partial output is already in the doc.
            _ => (DoneStatus::Completed, None),
        },
        Err(e) => (DoneStatus::Errored, Some(e.to_string())),
    }
}

/// One turn: `session/prompt` whose response (the `stopReason`) ends it.
/// `prompt_id` (agents with the prompt-complete extension) rides `_meta` so
/// the `_x.ai/session/prompt_complete` notification can be matched exactly —
/// grok echoes it back (verified live, 1.0.4).
fn prompt_turn(
    client: RpcClient,
    session_id: String,
    text: String,
    prompt_id: Option<String>,
) -> BoxFuture<'static, Result<Value, HarnessError>> {
    Box::pin(async move {
        let mut params = json!({
            "sessionId": session_id,
            "prompt": [{ "type": "text", "text": text }],
        });
        if let Some(id) = prompt_id {
            params["_meta"] = json!({ "promptId": id, "requestId": id });
        }
        client.request("session/prompt", params).await
    })
}

/// Answer a server→client request. Permission requests are auto-accepted with
/// the agent's preferred allow option — parity with the claude harness's
/// bypassPermissions and the codex harness's approvalPolicy "never" (zeron
/// sessions run unattended). Everything else (fs, terminal, elicitation) was
/// declined at initialize, so a stray request gets method-not-found rather
/// than wedging the agent.
fn handle_server_request(
    client: &RpcClient,
    id: Value,
    method: &str,
    params: &Value,
) -> Vec<AgentEvent> {
    match method {
        "session/request_permission" => {
            let options: Vec<Value> = params
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            match preferred_allow_option(&options) {
                Some(option_id) => client.respond(
                    &id,
                    json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
                ),
                None => client.respond(&id, json!({ "outcome": { "outcome": "cancelled" } })),
            }
            Vec::new()
        }
        _ => {
            tracing::debug!(target: "mast_harness::acp", "unhandled server request: {method}");
            client.respond_error(&id, -32601, &format!("unsupported method: {method}"));
            Vec::new()
        }
    }
}

type RequestInputFn = Box<
    dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
        + Send
        + Sync,
>;

/// A permission request is a QUESTION (not a tool permission) when any of
/// its options lacks an allow/reject kind — that's how the agent relays
/// user-facing choices (Claude's AskUserQuestion arrives this way through
/// the adapter). Every option carrying an allow/reject kind means a real
/// tool permission, which auto-accepts (unattended parity); kinds may
/// legitimately repeat — codex sends two `allow_always` options ("Allow for
/// Session" and a prefix-rule amendment) on every exec approval.
fn is_user_question(options: &[Value]) -> bool {
    options.iter().any(|option| {
        !matches!(
            option.get("kind").and_then(Value::as_str),
            Some("allow_once" | "allow_always" | "reject_once" | "reject_always")
        )
    })
}

/// The live-run request handler: tool permissions auto-accept like
/// [`handle_server_request`], but question-shaped requests block on the
/// engine's input bridge (in a subtask so the message loop keeps flowing)
/// and answer with the option whose name matches the chosen label. A dropped
/// resolver degrades to `cancelled` — never a silent allow.
fn handle_server_request_live(
    client: &RpcClient,
    id: Value,
    method: &str,
    params: &Value,
    request_input: &std::sync::Arc<RequestInputFn>,
    session_id: &str,
    rules: Option<bool>,
) -> Vec<AgentEvent> {
    if params
        .get("sessionId")
        .and_then(Value::as_str)
        .is_some_and(|id| id != session_id)
    {
        client.respond(&id, json!({"outcome": {"outcome": "cancelled"}}));
        return Vec::new();
    }
    if method != "session/request_permission" {
        return handle_server_request(client, id, method, params);
    }
    let options: Vec<Value> = params
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(title_is_command) = rules.filter(|_| !is_user_question(&options)) {
        return ask_tool_permission(client, id, params, options, request_input, title_is_command);
    }
    if !is_user_question(&options) {
        return handle_server_request(client, id, method, params);
    }
    let names: Vec<String> = options
        .iter()
        .map(|o| {
            o.get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    let question = UserInputQuestion {
        permission: None,
        command_approval: None,
        approval_scope: None,
        allow_custom: false,
        id: new_message_id(),
        header: "Agent question".into(),
        question: params
            .get("toolCall")
            .and_then(|t| t.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("The agent needs your input.")
            .to_owned(),
        options: names.clone(),
        multi_select: false,
    };
    let client = client.clone();
    let request_input = std::sync::Arc::clone(request_input);
    tokio::spawn(async move {
        let answers = (request_input)(vec![question.clone()])
            .await
            .unwrap_or_default();
        let picked = answers
            .iter()
            .find(|a| a.question_id == question.id)
            .and_then(|a| a.labels.first())
            .and_then(|label| {
                options
                    .iter()
                    .find(|o| o.get("name").and_then(Value::as_str) == Some(label.as_str()))
            })
            .and_then(|o| o.get("optionId").and_then(Value::as_str));
        match picked {
            Some(option_id) => client.respond(
                &id,
                json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
            ),
            None => client.respond(&id, json!({ "outcome": { "outcome": "cancelled" } })),
        }
    });
    Vec::new()
}

/// What an ACP tool permission request asks for. Gemini CLI names MCP tools
/// in `toolCall._meta["gemini-cli"].mcp`; shell commands come from
/// `rawInput.command`, or from the title for agents that put it there.
fn acp_permission(
    tool_call: &Value,
    title_is_command: bool,
) -> Option<mast_proto::PermissionRequest> {
    let meta = tool_call.get("_meta");
    let mcp = meta
        .and_then(|m| m.get("gemini-cli"))
        .and_then(|g| g.get("mcp"))
        .or_else(|| meta.and_then(|m| m.get("mcp")));
    if let Some(mcp) = mcp {
        let field = |k: &str| {
            mcp.get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        return Some(mast_proto::PermissionRequest::McpTool {
            server: field("server_name")?,
            tool: field("tool_name")?,
        });
    }
    if tool_call.get("kind").and_then(Value::as_str) != Some("execute") {
        return None;
    }
    let command = tool_call
        .get("rawInput")
        .and_then(|input| input.get("command"))
        .filter(|c| c.is_array() || c.as_str().is_some_and(|s| !s.trim().is_empty()))
        .cloned()
        .or_else(|| {
            title_is_command
                .then(|| tool_call.get("title").and_then(Value::as_str))
                .flatten()
                .filter(|t| !t.trim().is_empty())
                .map(|t| Value::String(t.to_owned()))
        })?;
    Some(mast_proto::PermissionRequest::Command { command })
}

/// Rules mode: ask the engine (which answers from the user's rules or asks
/// the user) and reply with the agent's allow-once or reject option. Never
/// picks an allow-always option, which would store a grant inside the agent.
fn ask_tool_permission(
    client: &RpcClient,
    id: Value,
    params: &Value,
    options: Vec<Value>,
    request_input: &std::sync::Arc<RequestInputFn>,
    title_is_command: bool,
) -> Vec<AgentEvent> {
    let tool_call = params.get("toolCall").cloned().unwrap_or(Value::Null);
    let title = tool_call
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("a tool");
    let question = UserInputQuestion {
        permission: acp_permission(&tool_call, title_is_command),
        command_approval: None,
        approval_scope: Some(mast_proto::ApprovalScope::Summary),
        allow_custom: false,
        id: new_message_id(),
        header: "Approve tool".into(),
        question: format!("The agent wants to run `{title}`. Allow it?"),
        options: vec!["Yes".into(), "No".into()],
        multi_select: false,
    };
    let client = client.clone();
    let request_input = std::sync::Arc::clone(request_input);
    tokio::spawn(async move {
        let answers = (request_input)(vec![question.clone()])
            .await
            .unwrap_or_default();
        let allowed = answers
            .iter()
            .find(|a| a.question_id == question.id)
            .and_then(|a| a.labels.first())
            .is_some_and(|l| l.eq_ignore_ascii_case("yes"));
        let by_kind = |kinds: &[&str]| {
            kinds.iter().find_map(|kind| {
                options
                    .iter()
                    .find(|o| o.get("kind").and_then(Value::as_str) == Some(*kind))
                    .and_then(|o| o.get("optionId").and_then(Value::as_str))
                    .map(str::to_owned)
            })
        };
        let picked = if allowed {
            // No allow-once option: cancel rather than store a grant.
            by_kind(&["allow_once"])
        } else {
            by_kind(&["reject_once", "reject_always"])
        };
        match picked {
            Some(option_id) => client.respond(
                &id,
                json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
            ),
            None => client.respond(&id, json!({ "outcome": { "outcome": "cancelled" } })),
        }
    });
    Vec::new()
}

/// `session/new`. Agents that sign in from Settings never start a browser
/// sign-in mid-chat; an auth_required answer points the user there instead.
async fn new_session(
    client: &RpcClient,
    incoming: &mut mpsc::Receiver<Incoming>,
    params: Value,
    agent_name: &str,
    signs_in_from_settings: bool,
) -> Result<Value, HarnessError> {
    match request_draining(client, incoming, "session/new", params).await {
        Err(error) if signs_in_from_settings && is_auth_required(&error) => {
            Err(HarnessError::Protocol(format!(
                "{agent_name} isn't signed in. Use Settings → Agents → Sign in."
            )))
        }
        other => other,
    }
}

/// acp reserves -32000 for auth_required.
fn is_auth_required(error: &HarnessError) -> bool {
    matches!(error, HarnessError::Protocol(message) if message.contains("(code -32000)"))
}

const EFFORT_SUFFIXES: [(&str, &str, ReasoningLevel); 3] = [
    ("-low", " (Low)", ReasoningLevel::Low),
    ("-medium", " (Medium)", ReasoningLevel::Medium),
    ("-high", " (High)", ReasoningLevel::High),
];

/// one picker row: either a single advertised model, or the effort variants
/// of one model with the id the agent advertises for each level.
struct EffortGroup<'a> {
    id: String,
    label: &'a str,
    variants: Vec<(ReasoningLevel, &'a str)>,
}

/// group advertised `(id, name)` choices by their display name. antigravity
/// names every effort variant "<model> (Low|Medium|High)", but its ids don't
/// always follow (a signed-in account pairs `gemini-3.1-pro-low` with
/// `gemini-pro-agent` for High), so the name is the only reliable key. A
/// group's id is a variant id minus its level suffix, falling back to the
/// first variant's id; ids come from the wire alone, so discovery and run
/// time derive the same one.
fn effort_groups<'a>(
    choices: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<EffortGroup<'a>> {
    let mut groups: Vec<EffortGroup<'a>> = Vec::new();
    for (id, name) in choices {
        let variant = EFFORT_SUFFIXES.iter().find_map(|(_, label_suffix, level)| {
            name.strip_suffix(label_suffix).map(|base| (base, *level))
        });
        let Some((base, level)) = variant else {
            groups.push(EffortGroup {
                id: id.to_owned(),
                label: name,
                variants: Vec::new(),
            });
            continue;
        };
        match groups
            .iter_mut()
            .find(|group| !group.variants.is_empty() && group.label == base)
        {
            Some(group) => group.variants.push((level, id)),
            None => groups.push(EffortGroup {
                id: String::new(),
                label: base,
                variants: vec![(level, id)],
            }),
        }
    }
    for group in groups.iter_mut().filter(|group| !group.variants.is_empty()) {
        group.id = group
            .variants
            .iter()
            .find_map(|(_, id)| {
                EFFORT_SUFFIXES
                    .iter()
                    .find_map(|(id_suffix, _, _)| id.strip_suffix(id_suffix))
            })
            .unwrap_or(group.variants[0].1)
            .to_owned();
        group.variants.sort_by_key(|(level, _)| *level);
    }
    groups
}

/// fold effort variants into one row whose ladder lists the levels offered;
/// models without a level in their name pass through.
fn group_effort_variants(models: Vec<Model>) -> Vec<Model> {
    let groups = effort_groups(models.iter().map(|m| (m.id.as_str(), m.label.as_str())));
    groups
        .iter()
        .filter_map(|group| {
            let Some((_, first_id)) = group.variants.first() else {
                return models.iter().find(|m| m.id == group.id).cloned();
            };
            let first = models.iter().find(|m| m.id == *first_id)?;
            Some(Model {
                id: group.id.clone(),
                label: group.label.to_owned(),
                // variant descriptions only restate their thinking level
                description: None,
                reasoning_levels: group.variants.iter().map(|(level, _)| *level).collect(),
                options: first.options.clone(),
            })
        })
        .collect()
}

/// the advertised variant id for a grouped model: the picked level when the
/// model offers it, else its strongest level. Ids the agent already
/// advertises (chats saved with a full variant id) pass through untouched.
fn effort_variant_id(
    session_response: &Value,
    model: &str,
    reasoning: Option<ReasoningLevel>,
) -> String {
    let choices: Vec<(&str, &str)> = session_response
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|options| {
            options
                .iter()
                .find(|o| o.get("category").and_then(Value::as_str) == Some("model"))
        })
        .and_then(|o| o.get("options").and_then(Value::as_array))
        .map(|choices| {
            choices
                .iter()
                .filter_map(|c| {
                    let id = c.get("value").and_then(Value::as_str)?;
                    Some((id, c.get("name").and_then(Value::as_str).unwrap_or(id)))
                })
                .collect()
        })
        .unwrap_or_default();
    if choices.iter().any(|(id, _)| *id == model) {
        return model.to_owned();
    }
    if let Some(base) = EFFORT_SUFFIXES
        .iter()
        .find_map(|(id_suffix, _, _)| model.strip_suffix(id_suffix))
        && choices.iter().any(|(id, _)| *id == base)
    {
        return base.to_owned();
    }
    let groups = effort_groups(choices);
    let Some(group) = groups.iter().find(|group| group.id == model) else {
        return model.to_owned();
    };
    let offered = |level: ReasoningLevel| {
        group
            .variants
            .iter()
            .find(|(variant_level, _)| *variant_level == level)
            .map(|(_, id)| (*id).to_owned())
    };
    reasoning
        .and_then(offered)
        .or_else(|| {
            [
                ReasoningLevel::High,
                ReasoningLevel::Medium,
                ReasoningLevel::Low,
            ]
            .into_iter()
            .find_map(offered)
        })
        .unwrap_or_else(|| model.to_owned())
}

/// Await a setup request while draining incoming messages, so a `session/load`
/// whose replay outruns the incoming channel's capacity can't deadlock the
/// reader. Replayed `session/update`s are dropped (the doc already holds the
/// history); server requests are answered.
async fn request_draining(
    client: &RpcClient,
    incoming: &mut mpsc::Receiver<Incoming>,
    method: &'static str,
    params: Value,
) -> Result<Value, HarnessError> {
    let loading_session = matches!(method, "session/new" | "session/load");
    let requested_session = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut metadata = VecDeque::new();
    let mut handle_incoming = |inc| match inc {
        Incoming::Request { id, method, params } => {
            if method == "session/request_permission"
                && params.get("sessionId").and_then(Value::as_str) != requested_session.as_deref()
            {
                client.respond(&id, json!({"outcome": {"outcome": "cancelled"}}));
            } else {
                handle_server_request(client, id, &method, &params);
            }
        }
        Incoming::Notification { method, params }
            if loading_session
                && method == "session/update"
                && matches!(
                    params["update"]["sessionUpdate"].as_str(),
                    Some(
                        "config_option_update"
                            | "available_commands_update"
                            | "current_mode_update"
                    )
                ) =>
        {
            if metadata.len() == 32 {
                metadata.pop_front();
            }
            metadata.push_back(params);
        }
        _ => {}
    };
    let mut fut = prompt_like_request(client.clone(), method, params);
    let res = loop {
        tokio::select! {
            res = &mut fut => break res,
            inc = incoming.recv() => match inc {
                Some(inc) => handle_incoming(inc),
                None => {
                    return Err(HarnessError::Protocol(format!(
                        "{method}: agent exited during setup"
                    )));
                }
            },
        }
    };
    // Responses resolve through the pending map, not the incoming queue, so
    // replay updates the reader forwarded BEFORE the response line may still
    // sit in the buffer — flush them now or they'd leak into the live turn.
    while let Ok(inc) = incoming.try_recv() {
        handle_incoming(inc);
    }
    // Keep config refreshes even when they race the session response. They
    // are session state, not replayed transcript, and dropping them can lose
    // the only notification advertising a newly released Devin model.
    res.map(|mut response| {
        let id = response
            .get("sessionId")
            .and_then(Value::as_str)
            .or(requested_session.as_deref());
        let id = id.map(str::to_owned);
        for params in metadata {
            if params["sessionId"].as_str() != id.as_deref() {
                continue;
            }
            let update = &params["update"];
            match update["sessionUpdate"].as_str() {
                Some("config_option_update") if update["configOptions"].is_array() => {
                    response["configOptions"] = update["configOptions"].clone();
                }
                Some("available_commands_update") if update["availableCommands"].is_array() => {
                    response["availableCommands"] = update["availableCommands"].clone();
                }
                Some("current_mode_update") if update["currentModeId"].is_string() => {
                    if !response["modes"].is_object() {
                        response["modes"] = json!({});
                    }
                    response["modes"]["currentModeId"] = update["currentModeId"].clone();
                }
                _ => {}
            }
        }
        response
    })
}

fn prompt_like_request(
    client: RpcClient,
    method: &'static str,
    params: Value,
) -> BoxFuture<'static, Result<Value, HarnessError>> {
    Box::pin(async move { client.request(method, params).await })
}

/// Track tools to avoid prompting into an unowned self-continued turn.
fn track_open_tools(ev: &AgentEvent, open_tools: &mut std::collections::HashSet<String>) {
    match ev {
        AgentEvent::ToolCall { id, .. } => {
            open_tools.insert(id.clone());
        }
        AgentEvent::ToolResult { id, .. } => {
            open_tools.remove(id);
        }
        _ => {}
    }
}

/// A mid-turn `_session/steering` call. `idleBehavior: promptRequired`
/// covers the turn-ended race: the agent hands the text back instead of
/// firing an untracked turn.
fn steering_call_future(
    client: &RpcClient,
    session_id: &str,
    text: &str,
) -> BoxFuture<'static, Result<Value, HarnessError>> {
    let params = json!({
        "sessionId": session_id,
        "prompt": [{ "type": "text", "text": text }],
        "_meta": { "steering": { "idleBehavior": "promptRequired" } },
    });
    prompt_like_request(client.clone(), "_session/steering", params)
}

const ANTIGRAVITY_BACKGROUND_TASK_GUIDANCE: &str = "\
<mast_acp_constraints>\n\
Do not start an in-process background command or background subagent and end your turn saying you will report back when finished: this ACP transport cannot wake you when an in-process background task completes. Wait for long-running commands in the foreground, or delegate background work to a Mast worker chat with `create_chat`.\n\
</mast_acp_constraints>\n";

/// `agent_instructions` is a named agent's composed text (definition, then
/// the envelope), which replaces the envelope alone.
fn antigravity_system_prompt_append(request: &RunRequest, agent_instructions: Option<&str>) -> String {
    let instructions = agent_instructions.or_else(|| {
        request
            .instructions
            .as_ref()
            .map(|delivery| delivery.envelope.rendered.as_str())
    });
    match instructions {
        Some(text) if !text.is_empty() => {
            if text.ends_with('\n') {
                format!("{text}{ANTIGRAVITY_BACKGROUND_TASK_GUIDANCE}")
            } else {
                format!("{text}\n{ANTIGRAVITY_BACKGROUND_TASK_GUIDANCE}")
            }
        }
        _ => ANTIGRAVITY_BACKGROUND_TASK_GUIDANCE.to_owned(),
    }
}

fn build_session_params(
    harness: HarnessId,
    request: &RunRequest,
    agent_tools: Option<&crate::AgentToolsInjection>,
    agent_instructions: Option<&str>,
) -> Value {
    let mcp_servers = match agent_tools {
        Some(tools) => json!([tools.acp_mcp_server()]),
        None => json!([]),
    };
    let mut params = json!({
        "cwd": request.cwd,
        "mcpServers": mcp_servers,
    });
    if harness == HarnessId::Antigravity {
        params["_meta"] = json!({
            "antigravity": {
                "systemPrompt": {
                    "append": antigravity_system_prompt_append(request, agent_instructions),
                }
            }
        });
    }
    params
}

#[cfg(all(test, unix))]
mod session_lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn scratch_survives_until_the_child_exits() {
        let directory = tempfile::tempdir().unwrap();
        let scratch = ScratchDir::new("acp-test").unwrap();
        let scratch_path = scratch.path().to_path_buf();
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-robust-acp.py");
        let mut command = Command::new(fixture);
        child::configure(&mut command);
        scratch.apply(&mut command);
        command.current_dir(directory.path());
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        let mut child = Child::new(command.spawn().unwrap());
        let pid = child.id().unwrap() as libc::pid_t;
        let (client, incoming) =
            RpcClient::new(child.stdin.take().unwrap(), child.stdout.take().unwrap());
        let (event_tx, mut event_rx) = mpsc::channel(256);
        let (_steer_tx, steering) = mpsc::channel(8);
        let controls = RunControls {
            request_input: Box::new(|_| {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = tx.send(Vec::new());
                rx
            }),
            steering,
            interrupt: crate::CancellationToken::new(),
            approval: None,
        };
        let request = RunRequest {
            prompt: "scratch-lifecycle".into(),
            harness: None,
            model: None,
            reasoning: None,
            model_options: serde_json::Map::new(),
            cwd: directory.path().display().to_string(),
            sandbox: mast_proto::SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            approval_rules: false,
            scheduled_task: None,
            scheduled_notion_writes: None,
            fork_from: None,
            attachments: Vec::new(),
            worktree: None,
            resume: None,
            instructions: None,
        };
        let spec = grok_spec();
        let task = tokio::spawn(run_session(Session {
            child,
            scratch: Some(scratch),
            client,
            incoming,
            event_tx,
            controls,
            request,
            agent_tools: None,
            agent_instructions: None,
            first_turn_envelope: None,
            permission_title_is_command: spec.permission_title_is_command,
            harness: spec.id,
            agent_name: spec.display_name,
            prompt_complete_extension: spec.prompt_complete_extension,
            prompt_stall: spec.prompt_stall,
            stall_hint: spec.stall_hint,
            effort_in_model_id: spec.effort_in_model_id,
            auth_method: spec.auth_method,
            sessions_root: None,
            prompt_transform: spec.prompt_transform,
            effort_values: spec.effort_values,
            interrupt_grace: Duration::from_millis(50),
            kill_grace: Duration::from_millis(500),
            handshake_timeout: Duration::from_secs(5),
            stderr_tail: crate::StderrTail::default(),
        }));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match event_rx.recv().await.expect("event stream closed").unwrap() {
                    AgentEvent::TextDelta { text } if text == "ready" => break,
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(scratch_path.is_dir());
        drop(event_rx);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.path().join("scratch-at-exit")).unwrap(),
            "True"
        );
        assert!(!scratch_path.exists());
        // Signal zero only queries whether the child has been reaped.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}

#[cfg(test)]
mod tests;
