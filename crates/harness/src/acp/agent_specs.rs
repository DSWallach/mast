use super::*;

/// Per-agent configuration: which binary to spawn and what to tell the picker.
pub(super) struct AcpAgentSpec {
    pub(super) id: HarnessId,
    pub(super) display_name: &'static str,
    /// Binary name searched on PATH (and platform install dirs).
    pub(super) executable: &'static str,
    /// Env var overriding executable resolution (tests, custom installs).
    pub(super) env_override: &'static str,
    /// Arguments that put the binary in ACP-serving mode.
    pub(super) args: &'static [&'static str],
    /// Pinned npm package (`name@version`) installed ONCE into the managed
    /// adapters dir when the binary isn't already present — the launch then
    /// spawns `node <entry>` directly, keeping npm (and every way a user's
    /// npm state can break) out of chat turns. See [`crate::adapter_install`].
    pub(super) npm_package: Option<&'static str>,
    /// pinned release archive for this platform, installed once into the
    /// managed adapters dir when the binary isn't already present. See
    /// [`crate::archive_install`].
    pub(super) archive: Option<crate::archive_install::ArchivePin>,
    /// Extra install locations to probe after PATH.
    pub(super) extra_paths: fn() -> Vec<PathBuf>,
    /// The agent's own CLI binary (`claude`, `codex`, …) — what "installed"
    /// means to the user. Distinct from `executable` where the spawned adapter
    /// wraps the CLI (`claude-agent-acp`, `codex-acp`, `pi-acp`), and the npx
    /// fallback deliberately doesn't count: npx can fetch an adapter on
    /// demand, but an absent CLI still means no logins/config to drive.
    pub(super) cli_executable: &'static str,
    /// Extra install locations probed for [`Self::cli_executable`].
    pub(super) cli_extra_paths: fn() -> Vec<PathBuf>,
    /// Search summary + install hint for the NotInstalled error.
    pub(super) install_hint: &'static str,
    pub(super) models: fn() -> Vec<Model>,
    /// The agent puts the exact shell command in a permission request's
    /// `toolCall.title` and sends no `rawInput` (Gemini CLI). Rules mode only
    /// reads a command from the title when this is set.
    pub(super) permission_title_is_command: bool,
    /// Effort ladder surfaced in the picker; applied per session via the
    /// `thought_level` config option (must mirror the registry descriptor).
    pub(super) reasoning_levels: &'static [ReasoningLevel],
    /// Transform applied to the initial prompt and every steer — Claude's
    /// Ultrathink is a prompt-prefix convention, not an effort flag.
    pub(super) prompt_transform: fn(Option<ReasoningLevel>, &str) -> String,
    /// Preference-ordered `thought_level` value ids for the run's reasoning
    /// (per-agent clamping, e.g. Claude xhigh→max off the xhigh family). The
    /// first value the agent actually advertises wins.
    pub(super) effort_values: fn(Option<ReasoningLevel>, Option<&str>) -> Vec<&'static str>,
    /// Levels appended to a DISCOVERED model's non-empty ladder: modes the
    /// wire can't advertise because they aren't `thought_level` values.
    pub(super) ladder_extras: &'static [ReasoningLevel],
    /// The agent emits `_x.ai/session/prompt_complete` (Grok): treat it as
    /// the AUTHORITATIVE turn end — grok's `session/prompt` RPC can hang
    /// silently after the turn really finished (field reports: total silent
    /// non-response on some machines). The prompt response stays as the
    /// fallback; whichever lands first settles the turn, exactly once.
    pub(super) prompt_complete_extension: bool,
    /// Bound on prompt-send → FIRST sign of life on the wire. `None`
    /// disables. Grok's healthy runs acknowledge a prompt within
    /// milliseconds (queue bookkeeping precedes any model work), so total
    /// silence past this window is a wedged agent (a stale shared leader,
    /// a dead update check) — surface a visible error chip instead of
    /// indefinite Working.
    pub(super) prompt_stall: Option<Duration>,
    /// Agent-specific advice appended to the stall error chip: what a wedge
    /// usually means for THIS agent and what the user can check.
    pub(super) stall_hint: &'static str,
    /// the agent advertises every effort as its own model id (`…-low`,
    /// `…-high`) instead of a `thought_level` option: discovered variants fold
    /// into one row with a ladder, and a run sends the variant for its level.
    pub(super) effort_in_model_id: bool,
    /// auth method to sign in with when `session/new` answers auth_required —
    /// for agents that expect the client to pick one before the first session.
    pub(super) auth_method: Option<&'static str>,
    /// folders of `<skill>/SKILL.md` the agent loads itself but never
    /// advertises, listed as slash commands alongside its own.
    pub(super) skill_dirs: fn() -> Vec<PathBuf>,
    /// advertised commands the picker leaves out.
    pub(super) hidden_commands: &'static [&'static str],
}

pub(super) fn identity_transform(_reasoning: Option<ReasoningLevel>, text: &str) -> String {
    text.to_owned()
}

/// PATH + login-shell + extra dirs + node-version-manager scan for a binary.
pub(crate) fn find_on_paths(exe: &str, extra: Vec<PathBuf>) -> Option<PathBuf> {
    crate::executable::find_on_paths(exe, extra)
}

/// Generic effort ladder for agents without their own clamping rules.
pub(super) fn default_effort_values(
    reasoning: Option<ReasoningLevel>,
    _model: Option<&str>,
) -> Vec<&'static str> {
    let Some(level) = reasoning else {
        return Vec::new();
    };
    match level {
        ReasoningLevel::Minimal => vec!["minimal", "low"],
        ReasoningLevel::Low => vec!["low", "minimal"],
        ReasoningLevel::Medium => vec!["medium"],
        ReasoningLevel::High => vec!["high"],
        ReasoningLevel::XHigh => vec!["xhigh", "x-high", "high"],
        ReasoningLevel::Max => vec!["max", "xhigh", "high"],
        ReasoningLevel::Ultra | ReasoningLevel::Ultracode | ReasoningLevel::Ultrathink => {
            vec!["ultra", "max", "high"]
        }
    }
}

/// npm-global bin dirs for an adapter binary (`npm i -g` installs).
pub(super) fn npm_global_paths(exe: &'static str) -> fn() -> Vec<PathBuf> {
    // fn pointers can't capture; probe the fixed npm-global locations and
    // append the exe at call time via a small per-exe shim table.
    match exe {
        "pi-acp" => || npm_global_bins("pi-acp"),
        _ => || Vec::new(),
    }
}

pub(super) fn npm_global_bins(exe: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = crate::executable::home_dir() {
        dirs.push(home.join(".local").join("bin").join(exe));
        dirs.push(home.join(".npm-global").join("bin").join(exe));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin").join(exe));
    dirs.push(PathBuf::from("/usr/local/bin").join(exe));
    dirs
}

pub(super) fn grok_install_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = crate::executable::home_dir() {
        dirs.push(home.join(".local").join("bin").join("grok"));
        dirs.push(home.join(".grok").join("bin").join("grok"));
        dirs.push(home.join(".npm-global").join("bin").join("grok"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin/grok"));
    dirs.push(PathBuf::from("/usr/local/bin/grok"));
    dirs
}

pub(super) fn grok_spec() -> AcpAgentSpec {
    AcpAgentSpec {
        id: HarnessId::Grok,
        display_name: "Grok",
        executable: "grok",
        env_override: "GROK_EXECUTABLE",
        // Flag placement verified against grok 1.0.4: `--no-auto-update` is
        // TOP-LEVEL (before the subcommand) and kills the launch-time update
        // check (a silent multi-second network stall); `--no-leader` lives on
        // the `agent` subcommand and starts a fresh agent even when
        // `[cli] use_leader` is set — leader mode ATTACHES `agent stdio` to a
        // shared process via ~/.grok/leader.sock, so a wedged/stale leader
        // (the user's TUI) reads as total silent non-response in zeron.
        args: &["--no-auto-update", "agent", "--no-leader", "stdio"],
        npm_package: Some("@xai-official/grok@1.0.4"),
        archive: None,
        extra_paths: grok_install_paths,
        cli_executable: "grok",
        cli_extra_paths: grok_install_paths,
        install_hint: "grok (searched PATH, the login shell's PATH, ~/.local/bin, \
             ~/.grok/bin, ~/.npm-global/bin, /opt/homebrew/bin, /usr/local/bin, and \
             fnm/nvm/volta/pnpm/bun install dirs; install with \
             `curl -fsSL https://x.ai/cli/install.sh | bash` or \
             `npm install -g @xai-official/grok`; set GROK_EXECUTABLE to override)",
        models: || {
            vec![Model {
                id: "grok-4.5".into(),
                label: "Grok 4.5".into(),
                description: Some("xAI's coding model — 500k context".into()),
                reasoning_levels: vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High,
                ],
                options: Vec::new(),
            }]
        },
        // No `_session/steering` extension: steers deliver at turn boundaries.
        permission_title_is_command: false,
        // Grok Build's advertised efforts (default high); applied through the
        // session's `thought_level` config option.
        reasoning_levels: &[
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
        ],
        prompt_transform: identity_transform,
        effort_values: default_effort_values,
        ladder_extras: &[],
        // Verified live (1.0.4): the notification fires with the echoed
        // `_meta.promptId`, just ahead of the RPC response.
        prompt_complete_extension: true,
        prompt_stall: Some(Duration::from_secs(30)),
        stall_hint: "The agent process is likely wedged — a stale shared leader \
             process or a hung startup check; zeron launches it with --no-leader \
             and --no-auto-update to avoid both.",
        effort_in_model_id: false,
        auth_method: None,
        skill_dirs: Vec::new,
        hidden_commands: &[],
    }
}

pub(super) fn devin_install_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = crate::executable::home_dir() {
        // The official installer's launcher symlink (the binary lives below
        // ~/.local/share/devin/cli/_versions).
        dirs.push(home.join(".local").join("bin").join("devin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin/devin"));
    dirs.push(PathBuf::from("/usr/local/bin/devin"));
    dirs
}

pub(super) fn devin_spec() -> AcpAgentSpec {
    AcpAgentSpec {
        id: HarnessId::Devin,
        display_name: "Devin",
        executable: "devin",
        env_override: "DEVIN_EXECUTABLE",
        args: &["acp"],
        // Native ACP server — no adapter package in between.
        npm_package: None,
        archive: None,
        extra_paths: devin_install_paths,
        cli_executable: "devin",
        cli_extra_paths: devin_install_paths,
        install_hint: "devin (searched PATH, the login shell's PATH, ~/.local/bin, \
             /opt/homebrew/bin, and /usr/local/bin; install with \
             `curl -fsSL https://cli.devin.ai/install.sh | bash` or \
             `brew install --cask devin-cli`, then `devin auth login`; set \
             DEVIN_EXECUTABLE to override)",
        // Legacy metadata only: discovery uses `devin models list` because
        // session/new starts with a stale catalog. Effort is baked into ids.
        // These ids were live-verified with CLI 3000.6.14; `swe-1-7-medium`
        // is the session default the server reports.
        models: || {
            vec![
                Model {
                    id: "swe-1-7-medium".into(),
                    label: "SWE-1.7 Medium".into(),
                    description: Some("Devin's default coding model".into()),
                    reasoning_levels: Vec::new(),
                    options: Vec::new(),
                },
                Model {
                    id: "claude-fable-5-1-high".into(),
                    label: "Claude Fable 5.1 High".into(),
                    description: Some("Anthropic's frontier model through Devin".into()),
                    reasoning_levels: Vec::new(),
                    options: Vec::new(),
                },
                Model {
                    id: "adaptive".into(),
                    label: "Adaptive".into(),
                    description: Some("Devin picks the model per request".into()),
                    reasoning_levels: Vec::new(),
                    options: Vec::new(),
                },
            ]
        },
        permission_title_is_command: false,
        // Effort is encoded in Devin's advertised model ids, not a separate
        // thought_level config option.
        reasoning_levels: &[],
        prompt_transform: identity_transform,
        effort_values: default_effort_values,
        ladder_extras: &[],
        prompt_complete_extension: false,
        prompt_stall: None,
        stall_hint: "The agent process is likely wedged.",
        effort_in_model_id: false,
        auth_method: None,
        skill_dirs: Vec::new,
        hidden_commands: &[],
    }
}

pub(super) fn hermes_install_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = crate::executable::home_dir() {
        dirs.push(home.join(".local").join("bin").join("hermes"));
        dirs.push(home.join(".hermes").join("bin").join("hermes"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin/hermes"));
    dirs.push(PathBuf::from("/usr/local/bin/hermes"));
    dirs
}

pub(super) fn hermes_spec() -> AcpAgentSpec {
    AcpAgentSpec {
        id: HarnessId::Hermes,
        display_name: "Hermes",
        executable: "hermes",
        env_override: "HERMES_EXECUTABLE",
        args: &["acp"],
        // Python/uv install — no npm fallback exists.
        npm_package: None,
        archive: None,
        extra_paths: hermes_install_paths,
        cli_executable: "hermes",
        cli_extra_paths: hermes_install_paths,
        install_hint: "hermes (searched PATH, the login shell's PATH, ~/.local/bin, \
             ~/.hermes/bin, /opt/homebrew/bin, /usr/local/bin, and fnm/nvm/volta/pnpm/bun \
             install dirs; install with \
             `curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash`, then \
             `cd ~/.hermes/hermes-agent && uv pip install -e '.[acp]'` for the ACP \
             server; set HERMES_EXECUTABLE to override)",
        // Hermes derives its model list from the providers the user has
        // authenticated (`hermes model`); these are the Nous flagships every
        // portal account gets. Ids the agent doesn't advertise are skipped by
        // the config-option set, falling back to the agent's own default.
        models: || {
            vec![
                Model {
                    id: "hermes-4-405b".into(),
                    label: "Hermes 4 405B".into(),
                    description: Some("Nous Research's hybrid-reasoning flagship".into()),
                    reasoning_levels: Vec::new(),
                    options: Vec::new(),
                },
                Model {
                    id: "hermes-4-70b".into(),
                    label: "Hermes 4 70B".into(),
                    description: Some("Faster Hermes 4 — same post-training, 70B".into()),
                    reasoning_levels: Vec::new(),
                    options: Vec::new(),
                },
            ]
        },
        // No `_session/steering` extension: steers deliver at turn boundaries.
        permission_title_is_command: false,
        // Hermes exposes no effort config over ACP today (hybrid reasoning is
        // model-internal); revisit when the adapter advertises a ladder.
        reasoning_levels: &[],
        prompt_transform: identity_transform,
        effort_values: default_effort_values,
        ladder_extras: &[],
        prompt_complete_extension: false,
        prompt_stall: None,
        stall_hint: "The agent process is likely wedged.",
        effort_in_model_id: false,
        auth_method: None,
        skill_dirs: Vec::new,
        hidden_commands: &[],
    }
}

pub(super) fn pi_spec() -> AcpAgentSpec {
    AcpAgentSpec {
        id: HarnessId::Pi,
        display_name: "Pi",
        executable: "pi-acp",
        env_override: "PI_ACP_EXECUTABLE",
        args: &[],
        npm_package: Some("pi-acp@0.0.33"),
        archive: None,
        extra_paths: npm_global_paths("pi-acp"),
        cli_executable: "pi",
        cli_extra_paths: || npm_global_bins("pi"),
        install_hint: "pi-acp (searched PATH, the login shell's PATH, npm global bins, \
             and fnm/nvm/volta/pnpm/bun install dirs; zeron installs the pinned \
             pi-acp automatically when npm is available — the pi CLI itself is \
             still required, `npm install -g --ignore-scripts \
             @earendil-works/pi-coding-agent`; set PI_ACP_EXECUTABLE to override)",
        // pi routes models through its own provider config (~/.pi); the picker
        // advertises the pass-through entry and pi keeps whatever the user set
        // up. Unknown ids are skipped by the config-option set.
        models: || {
            vec![Model {
                id: "default".into(),
                label: "pi default".into(),
                description: Some("Runs the model configured in pi (`pi` settings)".into()),
                reasoning_levels: vec![
                    ReasoningLevel::Minimal,
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High,
                    ReasoningLevel::XHigh,
                    ReasoningLevel::Max,
                ],
                options: Vec::new(),
            }]
        },
        // The adapter has no `_session/steering` extension: turn boundaries.
        permission_title_is_command: false,
        // pi's thinking ladder (minimal→max; its extra "off" tier has no zeron
        // equivalent and is left to the agent default).
        reasoning_levels: &[
            ReasoningLevel::Minimal,
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::XHigh,
            ReasoningLevel::Max,
        ],
        prompt_transform: identity_transform,
        effort_values: default_effort_values,
        ladder_extras: &[],
        prompt_complete_extension: false,
        prompt_stall: None,
        stall_hint: "The agent process is likely wedged.",
        effort_in_model_id: false,
        auth_method: None,
        skill_dirs: Vec::new,
        hidden_commands: &[],
    }
}

/// google's builds as the acp registry lists them (`antigravity-acp`); `None`
/// on platforms without one, where only an explicit override can launch.
pub(super) fn antigravity_archive() -> Option<crate::archive_install::ArchivePin> {
    let (url, entry, sha512) = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        (
            "https://dl.google.com/agy-extensions/releases/macos/agy-acp-server-agy_acp_server_1.1.1-darwin-arm64.zip",
            "agy_acp_server.par",
            "82576ba00164331daeba798db43f9e7c9097b76f0cd536cc07956f976e0132e3500f51b28288f07bdb5a3f90f9702dabc20ca0627edb25aa7e8e50f9fac29b8a",
        )
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        (
            "https://dl.google.com/agy-extensions/releases/linux/agy-acp-server-agy_acp_server_1.1.1-linux-x86_64.zip",
            "agy_acp_server.par",
            "bccda188b2903d3ff7a56691064fc8698f76d7bc2bb0f24e99bb5f8cda02c77f77629edc5dba640d6e713edf8e6576dfa38e88fb532752a53e26710934b5e4f0",
        )
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        (
            "https://dl.google.com/agy-extensions/releases/linux/agy-acp-server-agy_acp_server_1.1.1-linux-arm64.zip",
            "agy_acp_server.par",
            "f895b4ade624e25f9765c90df66f3a864b19b2d1f2bde2fc485de6b6782ce30be458abed2da587d7cdd2272ea0812a8452639682c6318127e4fe0b4b8fc62dcf",
        )
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        (
            "https://dl.google.com/agy-extensions/releases/windows/agy-acp-server-agy_acp_server_1.1.1-windows-x86_64.zip",
            "agy_acp_server.exe",
            "5f1c7ad17a3f3a877552bcb9c75da7c53d05417916728a3436962d81a136bb136e9d9e35a880c9c8ca4db0fcbab9c76e959b26965c5ce6e16ebdea51e4ea28b5",
        )
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        (
            "https://dl.google.com/agy-extensions/releases/windows/agy-acp-server-agy_acp_server_1.1.1-windows-arm64.zip",
            "agy_acp_server.exe",
            "5ac47faa3d74a447f8c2b0aa6d174360c2fd08d0009368c2eff97ed8f24d0fedc6b798e5709e91d18ab756af9b06657dd9d90103cd37c67392afb94229fcc536",
        )
    } else {
        return None;
    };
    Some(crate::archive_install::ArchivePin {
        name: "antigravity-acp",
        version: "1.1.1",
        url,
        entry,
        sha512,
    })
}

/// Whether the listing device has a pinned, explicitly installable archive.
pub fn can_install(harness: HarnessId) -> bool {
    harness == HarnessId::Antigravity && antigravity_spec().archive.is_some()
}

/// Only an explicit Settings action may call this installer.
pub async fn install_harness(harness: HarnessId) -> Result<(), HarnessError> {
    let pin = (harness == HarnessId::Antigravity)
        .then(antigravity_archive)
        .flatten()
        .ok_or_else(|| {
            HarnessError::NotInstalled(
                "Set AGY_EXECUTABLE to an installed agy CLI (or ANTIGRAVITY_ACP_EXECUTABLE)".into(),
            )
        })?;
    crate::archive_install::ensure_installed(pin, "Antigravity").await?;
    Ok(())
}

/// the registry launches linux builds with an empty `--uid`, which keeps a
/// root-started server running as the invoking user.
pub(super) const ANTIGRAVITY_LINUX_ARGS: &[&str] = &["--uid="];

pub(super) fn antigravity_spec() -> AcpAgentSpec {
    AcpAgentSpec {
        id: HarnessId::Antigravity,
        display_name: "Antigravity",
        executable: "agy",
        env_override: "AGY_EXECUTABLE",
        args: &["acp", "--dangerously-skip-permissions"],
        npm_package: None,
        archive: None,
        extra_paths: agy_install_paths,
        cli_executable: "agy",
        cli_extra_paths: agy_install_paths,
        install_hint: "agy (searched PATH, the login shell's PATH, ~/.local/bin, \
             /opt/homebrew/bin, and /usr/local/bin; set AGY_EXECUTABLE or \
             ANTIGRAVITY_ACP_EXECUTABLE to override)",
        models: || {
            use ReasoningLevel::{High, Low, Medium};
            let mut models = vec![
                Model {
                    id: "gemini-4-argon".into(),
                    label: "Gemini 4 Argon".into(),
                    description: None,
                    reasoning_levels: vec![Low, Medium, High],
                    options: Vec::new(),
                },
                Model {
                    id: "gemini-3.8-flash".into(),
                    label: "Gemini 3.8 Flash".into(),
                    description: None,
                    reasoning_levels: vec![Low, Medium, High],
                    options: Vec::new(),
                },
                Model {
                    id: "gemini-3.7-flash".into(),
                    label: "Gemini 3.7 Flash".into(),
                    description: None,
                    reasoning_levels: vec![Low, Medium, High],
                    options: Vec::new(),
                },
                Model {
                    id: "gemini-3.1-pro".into(),
                    label: "Gemini 3.1 Pro".into(),
                    description: None,
                    reasoning_levels: vec![Low, High],
                    options: Vec::new(),
                },
            ];
            antigravity_context::enrich_antigravity_models(&mut models);
            models
        },
        permission_title_is_command: false,
        reasoning_levels: &[
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
        ],
        prompt_transform: identity_transform,
        effort_values: default_effort_values,
        ladder_extras: &[],
        prompt_complete_extension: false,
        prompt_stall: None,
        stall_hint: "The agent process is likely wedged.",
        effort_in_model_id: true,
        // the personal oauth method is only the first-run default; sign-in
        // preserves any method already selected in antigravity's settings.
        auth_method: Some("oauth-personal"),
        skill_dirs: antigravity_skill_dirs,
        hidden_commands: &[],
    }
}

pub fn antigravity_home() -> Result<PathBuf, HarnessError> {
    antigravity_paths::home()
}

pub(crate) fn agy_install_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = crate::executable::home_dir() {
        dirs.push(home.join(".local").join("bin").join("agy"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin/agy"));
    dirs.push(PathBuf::from("/usr/local/bin/agy"));
    dirs
}
