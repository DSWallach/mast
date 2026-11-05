# Test home isolation audit

Initial inventory at integration head f566310ac. Matches include production paths, comments, and isolated fixtures; inclusion does not establish an unsafe access. Further classification is required.

Confirmed exposures:

- `crates/harness/src/acp/mod.rs:4270`: test creates a directory in the passwd home.
- `crates/engine/tests/external_import_real_smoke.rs:52` and `:65`: ignored tests explicitly read live Claude transcripts.
- `crates/engine/tests/subagent_scan_harness_session_real_smoke.rs:27`: ignored test explicitly reads a live Claude transcript.
- `crates/engine/src/external_import.rs:1002`: background scan derives the Claude projects directory from HOME, including when an engine has an isolated data directory.
- `crates/engine/src/repos.rs:175` and `:215`: default repository worktree root derives from HOME even when the engine data directory is explicit.

## Search inventory

Rust source matches for home resolution, home-relative application paths, data directories, relevant configuration variables, and temporary directory placement. This deliberately includes production references to expose constructor fallback paths. Line numbers refer to f566310ac.

```text
crates/engine/src/rpc.rs:2584:                let models = crate::model_catalogs::list(self.repos.data_dir(), harness, p.force)
crates/engine/src/rpc.rs:3882:                        .with_cache_file(self.repos.data_dir().join("cache").join("pr-dash.json"))
crates/engine/src/rpc.rs:5531:    /// machine's `~/.claude/projects` on-disk state — device-local, same
crates/engine/src/local_import.rs:38:/// Marker recording completed imports, at `{data_dir}/local-import.json`.
crates/engine/src/local_import.rs:120:    data_dir: PathBuf,
crates/engine/src/local_import.rs:133:        data_dir: &Path,
crates/engine/src/local_import.rs:144:                data_dir: data_dir.to_path_buf(),
crates/engine/src/local_import.rs:157:        self.inner.data_dir.join("profiles").join("local")
crates/engine/src/local_import.rs:165:        self.inner.data_dir.join(MARKER_FILE)
crates/engine/src/local_import.rs:498:pub fn marker_grants_read_root(data_dir: &Path, org_id: &str, user_id: &str) -> Option<PathBuf> {
crates/engine/src/local_import.rs:499:    let marker: Marker = std::fs::read_to_string(data_dir.join(MARKER_FILE))
crates/engine/src/local_import.rs:506:    let uploads = data_dir.join("profiles").join("local").join("uploads");
crates/engine/src/execution_scope.rs:300:    let home = crate::repos::home_dir();
crates/engine/src/repos.rs:5://! JSON list (`{data_dir}/repos.json`) — no sync. Existing repos can live anywhere
crates/engine/src/repos.rs:6://! the user points us; cloned/created ones land in `{data_dir}/repos`. Worktrees are
crates/engine/src/repos.rs:7://! created under `~/.zeron/worktrees/<repoName>/<worktreeName>` (NOT the data
crates/engine/src/repos.rs:90:pub(crate) fn home_dir() -> PathBuf {
crates/engine/src/repos.rs:109:    is_broad_workspace_root_with_home(path, &home_dir())
crates/engine/src/repos.rs:123:    let home = home_dir();
crates/engine/src/repos.rs:137:    is_automatic_access_blocked_with_home(path, &home_dir())
crates/engine/src/repos.rs:175:        .unwrap_or_else(|| home_dir().join(".zeron").join("worktrees"))
crates/engine/src/repos.rs:179:    data_dir: PathBuf,
crates/engine/src/repos.rs:208:    pub(crate) fn data_dir(&self) -> &Path {
crates/engine/src/repos.rs:209:        &self.inner.data_dir
crates/engine/src/repos.rs:212:    /// `data_dir` holds `repos.json` + cloned/created repos; the worktree root
crates/engine/src/repos.rs:213:    /// comes from `$MAST_WORKTREES_DIR` or `~/.zeron/worktrees`.
crates/engine/src/repos.rs:214:    pub fn new(data_dir: &Path, device_id: &str) -> Self {
crates/engine/src/repos.rs:215:        Self::with_worktrees_root(data_dir, device_id, default_worktrees_root())
crates/engine/src/repos.rs:219:    pub fn with_worktrees_root(data_dir: &Path, device_id: &str, worktrees_root: PathBuf) -> Self {
crates/engine/src/repos.rs:222:                data_dir: data_dir.to_path_buf(),
crates/engine/src/repos.rs:241:        self.inner.data_dir.join("repos.json")
crates/engine/src/repos.rs:256:        std::fs::create_dir_all(&self.inner.data_dir)?;
crates/engine/src/repos.rs:443:    /// `git clone <url>` under `{data_dir}/repos`. (Named `clone_repo` to keep
crates/engine/src/repos.rs:454:        let repos_dir = self.inner.data_dir.join("repos");
crates/engine/src/repos.rs:469:    /// `git init -b main` a fresh repository under `{data_dir}/repos`.
crates/engine/src/repos.rs:485:        let target = self.inner.data_dir.join("repos").join(&clean);
crates/engine/src/repos.rs:1031:        let cache_dir = self.inner.data_dir.join("cache").join("git-avatars");
crates/engine/src/repos.rs:1397:            None => home_dir(),
crates/engine/src/repos.rs:1483:        let protected_home_child = is_protected_home_child_with_home(target, &name, &home_dir());
crates/engine/src/repos.rs:2321:        let home = home_dir();
crates/engine/src/pr_dash.rs:777:        || in_dir(&["doc", "docs", ".claude"])
crates/engine/src/workspace_files.rs:2229:        let home = crate::repos::home_dir();
crates/engine/src/artifacts.rs:664:    /// The user's home directory (deny-list anchor); `$HOME` by default.
crates/engine/src/external_import.rs:5://! The on-disk transcript (`~/.claude/projects/<project>/<uuid>.jsonl`) is a
crates/engine/src/external_import.rs:257:        // only reads `~/.claude/projects`) — stamp that harness on the config
crates/engine/src/external_import.rs:685:    /// `~/.claude/projects/*/*.jsonl`), which is exactly the source this
crates/engine/src/external_import.rs:995:    /// Claude Code sessions found on disk under `~/.claude/projects/*/*.jsonl`
crates/engine/src/external_import.rs:1002:        let root = home_dir().join(".claude").join("projects");
crates/engine/src/external_import.rs:1004:            return Ok(Vec::new()); // no ~/.claude/projects at all
crates/engine/src/external_import.rs:1560:/// under `~/.claude/projects/<scratch-cwd>/`, which the importer then picked
crates/engine/src/external_import.rs:1600:/// `~/.claude/projects` the same as any real session), and without this
crates/engine/src/external_import.rs:1697:    let path = home_dir()
crates/engine/src/external_import.rs:1713:    home_dir()
crates/engine/src/external_import.rs:1738:/// synthetic temp dir without mutating `$HOME` (see `title_registry_tests`
crates/engine/src/external_import.rs:2397:    /// synthetic temp dir — no `$HOME` mutation needed since
crates/engine/src/external_import.rs:2581:        // under ~/.claude/projects/<scratch-tmpdir>/).
crates/engine/src/auth.rs:160:    pub data_dir: PathBuf,
crates/engine/src/auth.rs:172:    pub fn new(edge_url: impl Into<String>, data_dir: impl Into<PathBuf>) -> Self {
crates/engine/src/auth.rs:175:            data_dir: data_dir.into(),
crates/engine/src/auth.rs:283:        let session_file = config.data_dir.join("session.json");
crates/engine/src/auth.rs:961:        self.inner.config.data_dir.join("session.json")
crates/engine/src/uploads.rs:115:    pub fn new(data_dir: &Path) -> Self {
crates/engine/src/uploads.rs:116:        Self::from_root(&data_dir.join("uploads"))
crates/engine/src/context_usage.rs:28://!    (`~/.claude/projects/<project>/<session_id>.jsonl`) for the most
crates/engine/src/context_usage.rs:93:    /// once created) — avoids re-globbing `~/.claude/projects` on every
crates/engine/src/context_usage.rs:101:    /// transcripts under that home, not `~/.claude` / `~/.codex`.
crates/engine/src/context_usage.rs:235:    /// everything else keeps the default `~/.claude/projects`.
crates/engine/src/context_usage.rs:245:    /// `~/.codex/sessions/2026/10/01/rollout-...-<session-id>.jsonl`, unlike
crates/engine/src/context_usage.rs:293:    /// depending on `$HOME`. Globally-unique filename convention: the
crates/engine/src/context_usage.rs:337:    crate::repos::home_dir().join(".claude").join("projects")
crates/engine/src/context_usage.rs:341:    std::env::var_os("CODEX_HOME")
crates/engine/src/context_usage.rs:343:        .unwrap_or_else(|| crate::repos::home_dir().join(".codex"))
crates/engine/src/context_usage.rs:728:        // transcript, so the lookup goes to ~/.claude/projects (absent here).
crates/engine/src/sessions.rs:2487:        return Ok(crate::repos::home_dir()
crates/engine/src/sessions.rs:2505:    let home = crate::repos::home_dir();
crates/engine/src/sessions.rs:3874:        let home = crate::repos::home_dir();
crates/engine/src/sessions.rs:3895:        let home = crate::repos::home_dir();
crates/engine/src/execution_scope/tests.rs:512:    let home = crate::repos::home_dir();
crates/engine/src/agent_accounts.rs:6://! - **Claude Code** — credentials in `~/.claude/.credentials.json`
crates/engine/src/agent_accounts.rs:7://!   (`$CLAUDE_CONFIG_DIR` relocates the dir) or, on macOS, the Keychain item
crates/engine/src/agent_accounts.rs:9://!   lives in `~/.claude.json`.
crates/engine/src/agent_accounts.rs:10://! - **Codex** — `$CODEX_HOME/auth.json` (default `~/.codex`): a ChatGPT OAuth
crates/engine/src/agent_accounts.rs:20://!    under `{data_dir}/agent-accounts/{harness}/{slotId}.json` — the current
crates/engine/src/agent_accounts.rs:24://!    Claude, merge the identity back into `~/.claude.json`) with a saved slot.
crates/engine/src/agent_accounts.rs:31://!    Codex spawns `codex login` against a throwaway `CODEX_HOME` and polls
crates/engine/src/agent_accounts.rs:102:    /// Engine data dir; slots live under `{data_dir}/agent-accounts/`.
crates/engine/src/agent_accounts.rs:103:    pub data_dir: PathBuf,
crates/engine/src/agent_accounts.rs:104:    /// Claude config dir (`$CLAUDE_CONFIG_DIR` or `~/.claude`) — holds `.credentials.json`.
crates/engine/src/agent_accounts.rs:106:    /// Claude identity file (`~/.claude.json`, or `$CLAUDE_CONFIG_DIR/.claude.json`).
crates/engine/src/agent_accounts.rs:108:    /// Codex home (`$CODEX_HOME` or `~/.codex`) — holds `auth.json`.
crates/engine/src/agent_accounts.rs:117:    /// Production resolution: `CLAUDE_CONFIG_DIR` relocates both the Claude config
crates/engine/src/agent_accounts.rs:118:    /// json and the credentials file; `CODEX_HOME` relocates the Codex auth file.
crates/engine/src/agent_accounts.rs:119:    pub fn detect(data_dir: &Path) -> Self {
crates/engine/src/agent_accounts.rs:125:        let claude_dir = env_dir("CLAUDE_CONFIG_DIR");
crates/engine/src/agent_accounts.rs:127:            Some(dir) => dir.join(".claude.json"),
crates/engine/src/agent_accounts.rs:128:            None => home_dir().join(".claude.json"),
crates/engine/src/agent_accounts.rs:131:            data_dir: data_dir.to_path_buf(),
crates/engine/src/agent_accounts.rs:132:            claude_config_dir: claude_dir.unwrap_or_else(|| home_dir().join(".claude")),
crates/engine/src/agent_accounts.rs:134:            codex_home: env_dir("CODEX_HOME").unwrap_or_else(|| home_dir().join(".codex")),
crates/engine/src/agent_accounts.rs:135:            cursor_sdk_auth_file: home_dir().join(".cursor").join("sdk").join("auth.json"),
crates/engine/src/agent_accounts.rs:140:        self.claude_config_dir.join(".credentials.json")
crates/engine/src/agent_accounts.rs:144:        self.codex_home.join("auth.json")
crates/engine/src/agent_accounts.rs:148:        self.data_dir.join("agent-accounts")
crates/engine/src/agent_accounts.rs:178:    /// Claude only: `{oauthAccount, userID}` merged into `~/.claude.json` on swap.
crates/engine/src/agent_accounts.rs:227:    /// throwaway `CODEX_HOME`, or the cursor shim's login mode minting into a
crates/engine/src/agent_accounts.rs:314:        // `.login-<uuid>` throwaway CODEX_HOME dirs — each may hold live OAuth
crates/engine/src/agent_accounts.rs:495:        // Merge the identity back into ~/.claude.json — everything else (caches,
crates/engine/src/agent_accounts.rs:502:        let file = &self.inner.config.claude_config_file;
crates/engine/src/agent_accounts.rs:506:                "~/.claude.json exists but could not be parsed — not switching to avoid wiping \
crates/engine/src/agent_accounts.rs:513:            EngineError::Other("~/.claude.json is not a JSON object — not switching.".into())
crates/engine/src/agent_accounts.rs:515:        let (oauth_account, user_id) = match &slot.claude_config {
crates/engine/src/agent_accounts.rs:544:        std::fs::create_dir_all(&self.inner.config.codex_home)?;
crates/engine/src/agent_accounts.rs:547:        write_file_atomic(&self.inner.config.codex_auth_file(), json.as_bytes(), true)
crates/engine/src/agent_accounts.rs:620:            claude_config: slot.claude_config,
crates/engine/src/agent_accounts.rs:747:        // A throwaway CODEX_HOME isolates the new login completely — the live
crates/engine/src/agent_accounts.rs:748:        // ~/.codex session is never touched until the user explicitly switches.
crates/engine/src/agent_accounts.rs:876:    /// poll — mirroring codex's throwaway `CODEX_HOME`.
crates/engine/src/agent_accounts.rs:1256:        let cfg = read_json(&self.inner.config.claude_config_file);
crates/engine/src/agent_accounts.rs:1292:        read_json(&self.inner.config.codex_auth_file()).and_then(parse_codex_auth)
crates/engine/src/agent_accounts.rs:1327:            claude_config: d.claude_config.clone(),
crates/engine/src/agent_accounts.rs:1339:        if let Some(creds) = read_json(&self.inner.config.claude_creds_file()) {
crates/engine/src/agent_accounts.rs:1359:            if !self.inner.config.claude_creds_file().exists() {
crates/engine/src/agent_accounts.rs:1363:        std::fs::create_dir_all(&self.inner.config.claude_config_dir)?;
crates/engine/src/agent_accounts.rs:1366:            &self.inner.config.claude_creds_file(),
crates/engine/src/agent_accounts.rs:1465:            HarnessId::ClaudeCode => self.claude_usage(slot, is_active).await,
crates/engine/src/agent_accounts.rs:1466:            HarnessId::Codex => self.codex_usage(&slot.credentials).await,
crates/engine/src/agent_accounts.rs:1505:                    Some(token) => match self.claude_usage_request(&token).await {
crates/engine/src/agent_accounts.rs:1513:            HarnessId::Codex => self.codex_probe(credentials).await,
crates/engine/src/agent_accounts.rs:1534:        match self.claude_usage_request(&access_token).await {
crates/engine/src/agent_accounts.rs:1543:                self.claude_usage_request(&fresh).await.ok().flatten()
crates/engine/src/agent_accounts.rs:1581:        self.codex_probe(credentials).await.ok()
crates/engine/src/spaces.rs:210:        let home = crate::repos::home_dir();
crates/engine/src/pr_dash/tests.rs:394:        (".claude/skills/a.py", Docs),
crates/engine/src/run_journal.rs:3://! One append-only JSONL file per chat under `{data_dir}/journals/{chat_id}.jsonl`; each
crates/engine/src/registry.rs:6://! this device's composer offers, persisted in `{data_dir}/harness-prefs.json`.
crates/engine/src/registry.rs:197:    pub fn load_prefs(&self, data_dir: &Path) {
crates/engine/src/registry.rs:198:        let path = data_dir.join("harness-prefs.json");
crates/engine/src/change_requests.rs:545:        let home = crate::repos::home_dir();
crates/engine/src/subagent_scan.rs:404:            "/Users/mikey/.claude/projects/-Users-mikey-Projects-agent-mode-tools/29fd48ab-0258-4406-b725-26265045e05f.jsonl",
crates/engine/src/managed_accounts.rs:4://! (`<data>/managed-accounts/<harness>/<id>/`, never under `~/.claude`). Chats
crates/engine/src/managed_accounts.rs:5://! pinned to the account run their CLI with `CLAUDE_CONFIG_DIR` / `CODEX_HOME`
crates/engine/src/managed_accounts.rs:11://! `~/.claude` / `~/.codex`, untouched, exactly as before this module existed.
crates/engine/src/managed_accounts.rs:149:    pub fn new(data_dir: &Path, claude_dir: PathBuf, codex_home: PathBuf) -> Self {
crates/engine/src/managed_accounts.rs:151:            root: data_dir.join("managed-accounts"),
crates/engine/src/managed_accounts.rs:414:    /// The CLI home of an account. Always under `root`, never `~/.claude`.
crates/engine/src/managed_accounts.rs:451:    /// home's `.claude.json`; Codex → `auth.json`. After this the CLI is the
crates/engine/src/managed_accounts.rs:551:                if let Some(saved) = &slot.claude_config {
crates/engine/src/managed_accounts.rs:558:                write_file_atomic(&home.join(".claude.json"), config.to_string().as_bytes(), true)
crates/engine/src/managed_accounts.rs:1038:            let config = read_json(&home.join(".claude.json"))?;
crates/engine/src/chat_workspace_plan.rs:352:    // config (e.g. `.claude/skills/*`) is missing from the new worktree and
crates/engine/src/chat_workspace_plan.rs:361:            "create_chat_worktree: some .claude entries could not be copied into the new worktree"
crates/engine/src/chat_workspace_plan.rs:371:/// Merge-copy `<src_root>/.claude` into `<dst_root>/.claude`, returning how
crates/engine/src/chat_workspace_plan.rs:372:/// many entries failed to copy (0 = clean; a missing source `.claude` is a
crates/engine/src/chat_workspace_plan.rs:386:    let src = src_root.join(".claude");
crates/engine/src/chat_workspace_plan.rs:391:    merge_claude_dir(&src, &dst_root.join(".claude"), &mut failures);
crates/engine/src/chat_workspace_plan.rs:397:        tracing::warn!(path = %dst.display(), error = %err, "seed .claude: cannot create directory");
crates/engine/src/chat_workspace_plan.rs:404:            tracing::warn!(path = %src.display(), error = %err, "seed .claude: cannot read directory");
crates/engine/src/chat_workspace_plan.rs:435:                tracing::warn!(path = %from.display(), error = %err, "seed .claude: copy failed");
crates/engine/src/chat_workspace_plan.rs:741:        // Untracked `.claude/` content is (mostly) what `seed_claude_config`
crates/engine/src/chat_workspace_plan.rs:744:        .filter(|path| !path.starts_with(".claude/"))
crates/engine/src/chat_workspace_plan.rs:1509:    /// Plain repo (as `init_repo` builds) plus: tracked `.claude/agents/x.md`,
crates/engine/src/chat_workspace_plan.rs:1510:    /// tracked `.claude/skills/tracked/SKILL.md`, then untracked
crates/engine/src/chat_workspace_plan.rs:1511:    /// `.claude/skills/y/SKILL.md`, an untracked sibling inside the tracked
crates/engine/src/chat_workspace_plan.rs:1515:        let claude = root.join(".claude");
crates/engine/src/chat_workspace_plan.rs:1520:        run_git(root, &["add", ".claude"]).unwrap();
crates/engine/src/chat_workspace_plan.rs:1547:        let claude = wt.join(".claude");
crates/engine/src/chat_workspace_plan.rs:1596:                .join(".claude")
crates/engine/src/profile.rs:34:    pub fn local(data_dir: &Path) -> Result<Self, EngineError> {
crates/engine/src/profile.rs:35:        let profile_id = load_or_create_local_profile_id(data_dir)?;
crates/engine/src/profile.rs:36:        let store_root = data_dir.join("profiles").join("local");
crates/engine/src/profile.rs:39:            device_root: data_dir.to_path_buf(),
crates/engine/src/profile.rs:48:    pub fn synced(data_dir: &Path, org_id: &str, user_id: &str) -> Self {
crates/engine/src/profile.rs:49:        Self::account_scoped(data_dir, org_id, user_id, WorkspaceScope::Synced)
crates/engine/src/profile.rs:53:    pub fn development(data_dir: &Path, org_id: &str, user_id: &str) -> Self {
crates/engine/src/profile.rs:54:        Self::account_scoped(data_dir, org_id, user_id, WorkspaceScope::Development)
crates/engine/src/profile.rs:57:    fn account_scoped(data_dir: &Path, org_id: &str, user_id: &str, scope: WorkspaceScope) -> Self {
crates/engine/src/profile.rs:58:        let store_root = data_dir
crates/engine/src/profile.rs:64:            device_root: data_dir.to_path_buf(),
crates/engine/src/profile.rs:98:    /// Legacy transcript entries persist absolute paths under `{data_dir}/uploads`,
crates/engine/src/profile.rs:152:fn load_or_create_local_profile_id(data_dir: &Path) -> Result<String, EngineError> {
crates/engine/src/profile.rs:153:    std::fs::create_dir_all(data_dir)?;
crates/engine/src/profile.rs:154:    let path = data_dir.join(LOCAL_PROFILE_FILE);
crates/engine/src/profile.rs:168:    let temp_path = data_dir.join(format!(
crates/engine/src/profile.rs:320:    fn local_profiles_in_different_data_dirs_have_different_ids() {
crates/update/src/lib.rs:12://! - **Managed** (`~/.zeron/app/<ver>` + `current` symlink — the curl|sh
crates/update/src/lib.rs:244:    /// `~/.zeron/app/<ver>/zeron` behind the `current` symlink
crates/update/src/lib.rs:270:        data_dir: &Path,
crates/update/src/lib.rs:273:            Self::MacApp { .. } => stage_mac_app(edge_url, manifest, data_dir).await,
crates/update/src/lib.rs:511:/// Download + unpack the app tarball into `{data_dir}/updates/<ver>/Zeron.app`
crates/update/src/lib.rs:516:    data_dir: &Path,
crates/update/src/lib.rs:521:    let dir = data_dir.join("updates").join(version);
crates/update/src/lib.rs:1047:        let data_dir = tmp.path().join("data");
crates/update/src/lib.rs:1055:        let mac_err = stage_mac_app("http://127.0.0.1:1", &manifest, &data_dir)
crates/update/src/lib.rs:1059:        assert!(!data_dir.exists(), "macOS staging must not touch disk");
crates/update/src/lib.rs:1068:            apply_mac_app(&data_dir.join("Zeron.app"), &data_dir.join("Installed.app"))
crates/engine/src/instance_lock.rs:1://! Single-instance lock — an exclusive OS lock on `{data_dir}/engine.lock`
crates/engine/src/instance_lock.rs:29:    pub fn acquire(data_dir: &Path) -> Result<Self, EngineError> {
crates/engine/src/instance_lock.rs:30:        let path = data_dir.join("engine.lock");
crates/engine/src/instance_lock.rs:65:                             stop it or use a different data dir (MAST_DATA_DIR)",
crates/engine/src/instance_lock.rs:66:                            data_dir.display(),
crates/engine/src/instance_lock.rs:82:                    let holder = windows_holder_pid(data_dir);
crates/engine/src/instance_lock.rs:83:                    return Err(already_running_error(data_dir, &holder));
crates/engine/src/instance_lock.rs:102:            let _ = std::fs::write(windows_pid_path(data_dir), std::process::id().to_string());
crates/engine/src/instance_lock.rs:113:    pub fn holder(data_dir: &Path) -> Option<String> {
crates/engine/src/instance_lock.rs:114:        let path = data_dir.join("engine.lock");
crates/engine/src/instance_lock.rs:154:                Err(std::fs::TryLockError::WouldBlock) => Some(windows_holder_pid(data_dir)),
crates/engine/src/instance_lock.rs:167:fn already_running_error(data_dir: &Path, holder: &str) -> EngineError {
crates/engine/src/instance_lock.rs:170:         stop it or use a different data dir (MAST_DATA_DIR)",
crates/engine/src/instance_lock.rs:171:        data_dir.display(),
crates/engine/src/instance_lock.rs:177:fn windows_pid_path(data_dir: &Path) -> std::path::PathBuf {
crates/engine/src/instance_lock.rs:178:    data_dir.join("engine.lock.pid")
crates/engine/src/instance_lock.rs:182:fn windows_holder_pid(data_dir: &Path) -> String {
crates/engine/src/instance_lock.rs:183:    std::fs::read_to_string(windows_pid_path(data_dir))
crates/mcp/src/tools.rs:1225:        // own default (e.g. `~/.claude/settings.json`), not the composer's.
crates/engine/tests/message_queue.rs:367:    let data_dir = tmp.path().join("data");
crates/engine/tests/message_queue.rs:369:    let before = assemble_at(&data_dir, before_harness);
crates/engine/tests/message_queue.rs:401:    let after = assemble_at(&data_dir, after_harness);
crates/ui/src/pickers.rs:642:    /// Where [`Self::defaults`] persists (`{data_dir}/composer-defaults.json`);
crates/ui/src/pickers.rs:644:    data_dir: Option<PathBuf>,
crates/ui/src/pickers.rs:713:    /// External-session candidates (`~/.claude/projects` sessions Mast
crates/ui/src/pickers.rs:761:        let data_dir = state.read(cx).data_dir.clone();
crates/ui/src/pickers.rs:762:        let defaults = data_dir
crates/ui/src/pickers.rs:768:        Self::build(state, None, defaults, data_dir, cx)
crates/ui/src/pickers.rs:783:        let data_dir = seed.data_dir.clone();
crates/ui/src/pickers.rs:809:            data_dir,
crates/ui/src/pickers.rs:824:        data_dir: Option<PathBuf>,
crates/ui/src/pickers.rs:976:            data_dir,
crates/ui/src/pickers.rs:1040:        if let Some(dir) = self.data_dir.as_deref()
crates/ui/src/pickers.rs:3074:        if let Some(dir) = &self.data_dir
crates/engine/tests/subagent_scan_harness_session_real_smoke.rs:10://! `subagent_scan_real_smoke.rs` — depends on a real `~/.claude/projects`
crates/engine/tests/subagent_scan_harness_session_real_smoke.rs:22:#[ignore = "depends on real ~/.claude/projects data on this machine"]
crates/engine/tests/subagent_scan_harness_session_real_smoke.rs:27:        "/Users/mikey/.claude/projects/-Users-mikey-Projects-agent-mode-tools/8de11ea8-2a2a-42a2-9784-81b4142b404d.jsonl",
crates/proto/src/agent.rs:38:    /// `~/.claude/settings.json`). `None` for harnesses without a built-in
crates/engine/tests/local_import.rs:20:async fn seed_local(data_dir: &std::path::Path) -> (String, String, String) {
crates/engine/tests/local_import.rs:21:    let local = assemble(EngineProfile::local(data_dir).expect("local profile"));
crates/engine/tests/local_import.rs:63:    let store = mast_sync::DocsStore::open(data_dir.join("profiles").join("local"))
crates/engine/tests/local_import.rs:71:    let journals = data_dir.join("profiles").join("local").join("journals");
crates/engine/tests/local_import.rs:78:    let uploads = data_dir.join("profiles").join("local").join("uploads");
crates/theme/src/library.rs:104:    pub fn path(data_dir: &Path) -> PathBuf {
crates/theme/src/library.rs:105:        data_dir.join(LIBRARY_FILE)
crates/theme/src/library.rs:108:    pub fn load(data_dir: &Path) -> Result<Self> {
crates/theme/src/library.rs:109:        let path = Self::path(data_dir);
crates/theme/src/library.rs:125:    pub fn save(&self, data_dir: &Path) -> Result<()> {
crates/theme/src/library.rs:127:        fs::create_dir_all(data_dir)
crates/theme/src/library.rs:128:            .with_context(|| format!("could not create {}", data_dir.display()))?;
crates/theme/src/library.rs:129:        let path = Self::path(data_dir);
crates/theme/src/library.rs:384:    pub fn duplicate_as_editable(&mut self, id: &str, data_dir: &Path) -> Result<String> {
crates/theme/src/library.rs:400:        let editable_dir = data_dir.join("custom-themes");
crates/engine/tests/perf_bench.rs:182:/// with `sqlite3 -readonly ~/.zeron/profiles/local/docs.sqlite3 ".backup
crates/engine/tests/perf_bench.rs:183:/// $DIR/profiles/local/docs.sqlite3"` plus `cp ~/.zeron/local-profile.json
crates/engine/tests/perf_bench.rs:339:/// `cp -c -R ~/.zeron/profiles/local/journals ...`, read-only on the source).
crates/update/src/windows.rs:74:        .tempdir_in(directory)?;
crates/engine/src/pr_ticket_cache.rs:1548:        let home = crate::repos::home_dir();
crates/engine/src/pr_ticket_cache.rs:1571:        let cwd = crate::repos::home_dir()
crates/proto/src/entities.rs:1237:/// under Mast's data dir (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`), never in the
crates/proto/src/entities.rs:1238:/// live `~/.claude` / `~/.codex`. Device-local, like the homes themselves.
crates/engine/src/lib.rs:150:    /// Data directory (default `~/.zeron`, dev `~/.zeron-dev`).
crates/engine/src/lib.rs:151:    pub data_dir: PathBuf,
crates/engine/src/lib.rs:231:    /// Open stores under `data_dir`, wire sessions ⇄ doc host ⇄ workspace host, and
crates/engine/src/lib.rs:236:        data_dir: &Path,
crates/engine/src/lib.rs:243:        let profile = EngineProfile::development(data_dir, &org_id, &user_id);
crates/engine/src/lib.rs:248:        data_dir: &Path,
crates/engine/src/lib.rs:255:        let profile = EngineProfile::synced(data_dir, org_id, user_id);
crates/engine/src/lib.rs:266:        let data_dir = profile.device_root();
crates/engine/src/lib.rs:267:        std::fs::create_dir_all(data_dir)?;
crates/engine/src/lib.rs:271:        let lock = InstanceLock::acquire(data_dir)?;
crates/engine/src/lib.rs:285:        let data_dir = profile.device_root();
crates/engine/src/lib.rs:286:        std::fs::create_dir_all(data_dir)?;
crates/engine/src/lib.rs:288:        let device_id = load_or_create_device_id(data_dir)?;
crates/engine/src/lib.rs:291:        registry.load_prefs(data_dir);
crates/engine/src/lib.rs:327:        let repos = Repos::new(data_dir, &device_id);
crates/engine/src/lib.rs:331:            Some(data_dir.join("orchestrator-wakes.json")),
crates/engine/src/lib.rs:363:                local_import::marker_grants_read_root(data_dir, profile.org_id(), profile.user_id())
crates/engine/src/lib.rs:370:        let agent_accounts_config = AgentAccountsConfig::detect(data_dir);
crates/engine/src/lib.rs:373:            agent_accounts_config.codex_home.join("generated_images"),
crates/engine/src/lib.rs:377:                data_dir,
crates/engine/src/lib.rs:487:        linear.set_cache_dir(&data_dir.join("cache"));
crates/engine/src/lib.rs:576:            data_dir,
crates/engine/src/lib.rs:577:            agent_accounts_config.claude_config_dir.clone(),
crates/engine/src/lib.rs:578:            agent_accounts_config.codex_home.clone(),
crates/engine/src/lib.rs:613:            .set_store(data_dir.join("admin-grants.json"));
crates/engine/src/lib.rs:616:            data_dir.join("cache/pr-merged-wakes.json"), workspace.clone(), orchestrator_wakes.clone(),
crates/engine/src/lib.rs:940:        let mut auth_config = AuthConfig::new(config.edge_url.clone(), config.data_dir.clone());
crates/engine/src/lib.rs:979:            WorkspaceScope::Local => EngineProfile::local(&config.data_dir).map(Some),
crates/engine/src/lib.rs:994:                    &config.data_dir,
crates/engine/src/lib.rs:1010:                    &config.data_dir,
crates/engine/src/lib.rs:1023:        std::fs::create_dir_all(&config.data_dir)?;
crates/engine/src/lib.rs:1025:            device_id: load_or_create_device_id(&config.data_dir)?,
crates/engine/src/lib.rs:1196:        tracing::info!(data_dir = %config.data_dir.display(), "engine starting");
crates/engine/src/lib.rs:1198:        std::fs::create_dir_all(&config.data_dir)?;
crates/engine/src/lib.rs:1260:        let home = repos::home_dir();
crates/engine/src/lib.rs:1622:/// Stable per-installation device id, persisted at `{data_dir}/device-id`.
crates/engine/src/lib.rs:1623:fn load_or_create_device_id(data_dir: &Path) -> Result<String, EngineError> {
crates/engine/src/lib.rs:1624:    std::fs::create_dir_all(data_dir)?;
crates/engine/src/lib.rs:1629:    let _identity_lock = DeviceIdentityLock::acquire(data_dir)?;
crates/engine/src/lib.rs:1630:    let path = data_dir.join("device-id");
crates/engine/src/lib.rs:1641:    let temp_path = data_dir.join(format!(
crates/engine/src/lib.rs:1701:    fn acquire(data_dir: &Path) -> Result<Self, EngineError> {
crates/engine/src/lib.rs:1702:        let path = data_dir.join("device-id.lock");
crates/ui/src/overview.rs:1523:    /// (lazily, once `AppState.data_dir` exists).
crates/ui/src/overview.rs:2101:    /// Read the persisted acknowledged/hidden-repo sets once `data_dir`
crates/ui/src/overview.rs:2108:        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
crates/ui/src/overview.rs:2113:            overview_positions::load_string_set(&data_dir, overview_positions::UI_FLAGS_FILE);
crates/ui/src/overview.rs:2118:                &data_dir,
crates/ui/src/overview.rs:2123:                &data_dir,
crates/ui/src/overview.rs:2127:            .extend(work_item_filter::load_filter(&data_dir));
crates/ui/src/overview.rs:2136:        // A toggle made before `data_dir` existed wins over the older file.
crates/ui/src/overview.rs:2164:        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
crates/ui/src/overview.rs:2168:            overview_positions::load_string_set(&data_dir, overview_positions::UI_FLAGS_FILE);
crates/ui/src/overview.rs:2174:        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
crates/ui/src/overview.rs:2177:        if let Err(err) = overview_positions::save_string_set(&data_dir, file_name, set) {
crates/ui/src/overview.rs:2185:        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
crates/ui/src/overview.rs:2189:            overview_positions::load_string_set(&data_dir, overview_positions::UI_FLAGS_FILE);
crates/ui/src/overview.rs:8080:            state.data_dir = Some(dir);
crates/ui/src/overview.rs:8103:            assert!(o.prefs_loaded, "data_dir was available at first render");
crates/ui/src/overview.rs:9152:            state.data_dir = Some(dir);
crates/harness/src/opencode/mod.rs:130:    if let Some(home) = crate::executable::home_dir() {
crates/engine/tests/local_first.rs:18:    data_dir: &std::path::Path,
crates/engine/tests/local_first.rs:24:        data_dir: data_dir.to_path_buf(),
crates/engine/tests/managed_usage.rs:94:    let claude = tmp.path().join("live/.claude");
crates/engine/tests/managed_usage.rs:95:    let codex = tmp.path().join("live/.codex");
crates/engine/tests/managed_usage.rs:99:        data_dir: data.clone(),
crates/engine/tests/managed_usage.rs:101:        claude_config_file: claude.join(".claude.json"),
crates/engine/tests/auth.rs:251:fn workos_config(edge_url: &str, data_dir: &std::path::Path) -> AuthConfig {
crates/engine/tests/auth.rs:252:    let mut config = AuthConfig::new(edge_url, data_dir);
crates/engine/tests/external_import_bulk_session_canvas.rs:3://! and the session_canvas archive reader both resolve `~/.claude/projects`
crates/engine/tests/external_import_bulk_session_canvas.rs:4://! and `~/.config/session-canvas` off `$HOME`, so this test redirects `HOME`
crates/engine/tests/external_import_bulk_session_canvas.rs:42:    // A fake $HOME holding the on-disk transcripts and session_canvas's
crates/engine/tests/external_import_bulk_session_canvas.rs:55:        .join(".claude")
crates/engine/tests/external_import_bulk_session_canvas.rs:81:    // Mast's own data dir — unrelated to $HOME, must not collide with it.
crates/engine/tests/external_import_bulk_session_canvas.rs:82:    let data_dir = tempfile::tempdir().expect("data dir");
crates/engine/tests/external_import_bulk_session_canvas.rs:84:        data_dir.path(),
crates/ui/src/perf/overview_bench.rs:132:        state.data_dir = Some(dir.path().to_path_buf());
crates/engine/tests/external_import_real_smoke.rs:48:#[ignore = "depends on a real ~/.claude/projects transcript on this machine"]
crates/engine/tests/external_import_real_smoke.rs:52:            "/Users/mikey/.claude/projects/-Users-mikey-Projects-agent-mode-tools/572aa472-53ef-435f-8596-190f70bbf034.jsonl",
crates/engine/tests/external_import_real_smoke.rs:61:#[ignore = "depends on a real ~/.claude/projects transcript on this machine"]
crates/engine/tests/external_import_real_smoke.rs:65:            "/Users/mikey/.claude/projects/-Users-mikey-Projects-cofactr-workspace/258f3c46-1d77-46e8-b4bc-170ecd3dd0bc.jsonl",
crates/engine/tests/external_import_title_repair.rs:86:    let data_dir = tempfile::tempdir().expect("data dir");
crates/engine/tests/external_import_title_repair.rs:88:        data_dir.path(),
crates/engine/tests/external_import_title_repair.rs:341:// process-global `$HOME` env var and this file already has one such test
crates/engine/tests/codex_login_resolution.rs:16:        data_dir: root.join("data"),
crates/engine/tests/external_import_title_repair_no_transcript.rs:3://! `~/.claude/projects/*/*.jsonl` glob just never matches) must be
crates/engine/tests/external_import_title_repair_no_transcript.rs:6://! the process-global `$HOME` env var — same "at most one `$HOME`-mutating
crates/engine/tests/external_import_title_repair_no_transcript.rs:27:    // Deliberately no `~/.claude/projects` tree at all — the harness-session
crates/engine/tests/external_import_title_repair_no_transcript.rs:30:    let data_dir = tempfile::tempdir().expect("data dir");
crates/engine/tests/external_import_title_repair_no_transcript.rs:32:        data_dir.path(),
crates/engine/tests/promote_saved_accounts.rs:3://! `~/.claude` / `~/.codex` and the default Keychain item are never touched.
crates/engine/tests/promote_saved_accounts.rs:51:    let claude = tmp.path().join("live/.claude");
crates/engine/tests/promote_saved_accounts.rs:52:    let codex = tmp.path().join("live/.codex");
crates/engine/tests/promote_saved_accounts.rs:56:        data_dir: data.clone(),
crates/engine/tests/promote_saved_accounts.rs:58:        claude_config_file: claude.join(".claude.json"),
crates/engine/tests/promote_saved_accounts.rs:104:    let live = rig.tmp.path().join("live/.claude");
crates/engine/tests/promote_saved_accounts.rs:106:        live.join(".claude.json"),
crates/engine/tests/promote_saved_accounts.rs:224:        std::fs::read_dir(rig.tmp.path().join("live/.codex")).unwrap().count(),
crates/ui/src/settings.rs:256:    data_dir: PathBuf,
crates/ui/src/settings.rs:293:pub fn init(settings: UiSettings, data_dir: impl Into<PathBuf>, cx: &mut App) {
crates/ui/src/settings.rs:296:        data_dir: data_dir.into(),
crates/ui/src/settings.rs:321:    let data_dir = cx
crates/ui/src/settings.rs:323:        .map(|store| store.data_dir.clone())
crates/ui/src/settings.rs:325:    let backgrounds_dir = data_dir.join(NEW_THREAD_BACKGROUND_DIR);
crates/ui/src/settings.rs:361:    if next.save(&data_dir).is_err() {
crates/ui/src/settings.rs:374:    let data_dir = cx
crates/ui/src/settings.rs:376:        .map(|store| store.data_dir.clone())
crates/ui/src/settings.rs:383:    if next.save(&data_dir).is_err() {
crates/ui/src/settings.rs:391:        &data_dir.join(NEW_THREAD_BACKGROUND_DIR),
crates/ui/src/settings.rs:506:    let data_dir = store.data_dir.clone();
crates/ui/src/settings.rs:507:    match settings.save(&data_dir) {
crates/ui/src/settings.rs:1816:    pub fn load_for_startup(data_dir: &Path) -> Self {
crates/ui/src/settings.rs:1817:        let profile = if Self::path(data_dir).is_file() || has_prior_workspace(data_dir) {
crates/ui/src/settings.rs:1822:        let stored_settings = std::fs::read_to_string(Self::path(data_dir))
crates/ui/src/settings.rs:1831:        let mut settings = Self::load(data_dir);
crates/ui/src/settings.rs:1871:        if migrated && let Err(error) = settings.save(data_dir) {
crates/ui/src/settings.rs:1882:    /// Load from `{data_dir}/ui-settings.json`; defaults on any failure.
crates/ui/src/settings.rs:1883:    pub fn load(data_dir: &Path) -> Self {
crates/ui/src/settings.rs:1884:        match std::fs::read_to_string(Self::path(data_dir)) {
crates/ui/src/settings.rs:2019:    pub fn save(&self, data_dir: &Path) -> io::Result<()> {
crates/ui/src/settings.rs:2020:        std::fs::create_dir_all(data_dir)?;
crates/ui/src/settings.rs:2021:        let path = Self::path(data_dir);
crates/ui/src/settings.rs:2039:    pub fn path(data_dir: &Path) -> PathBuf {
crates/ui/src/settings.rs:2040:        data_dir.join(FILE_NAME)
crates/ui/src/settings.rs:2044:fn has_prior_workspace(data_dir: &Path) -> bool {
crates/ui/src/settings.rs:2067:        data_dir.join("docs.sqlite3"),
crates/ui/src/settings.rs:2068:        data_dir.join("profiles/local/docs.sqlite3"),
crates/ui/src/settings.rs:2070:    if let Ok(orgs) = std::fs::read_dir(data_dir.join("orgs")) {
crates/ui/src/settings.rs:2237:            let data_dir = parent.path().join("profile");
crates/ui/src/settings.rs:2239:                std::fs::create_dir(&data_dir).unwrap();
crates/ui/src/settings.rs:2240:                std::fs::write(UiSettings::path(&data_dir), "{}").unwrap();
crates/ui/src/settings.rs:2247:            let mut settings = UiSettings::load_for_startup(&data_dir);
crates/ui/src/settings.rs:2250:            settings.save(&data_dir).unwrap();
crates/ui/src/settings.rs:2251:            let reloaded = UiSettings::load_for_startup(&data_dir);
crates/ui/src/settings.rs:2945:            data_dir: dir.path().to_path_buf(),
crates/ui/src/settings.rs:3003:            data_dir: dir.path().to_path_buf(),
crates/engine/tests/subagent_scan_harness_session.rs:8://! then find whatever's on disk next to it. Same `$HOME`-redirection
crates/engine/tests/subagent_scan_harness_session.rs:33:        .join(".claude")
crates/engine/tests/subagent_scan_harness_session.rs:54:    let data_dir = tempfile::tempdir().expect("data dir");
crates/engine/tests/subagent_scan_harness_session.rs:56:        data_dir.path(),
crates/harness/src/acp/mod.rs:188:    if let Some(home) = crate::executable::home_dir() {
crates/harness/src/acp/mod.rs:199:    if let Some(home) = crate::executable::home_dir() {
crates/harness/src/acp/mod.rs:274:    if let Some(home) = crate::executable::home_dir() {
crates/harness/src/acp/mod.rs:350:    if let Some(home) = crate::executable::home_dir() {
crates/harness/src/acp/mod.rs:4270:        let directory = tempfile::tempdir_in(&real_home).unwrap();
crates/ui/src/overview/details_width.rs:148:        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
crates/ui/src/overview/details_width.rs:152:        let mut flags = crate::overview_positions::load_string_set(&data_dir, file);
crates/engine/tests/external_import.rs:13:/// A synthetic on-disk transcript shaped like a real `~/.claude/projects/
crates/engine/tests/external_import_config_repair.rs:2://! sessions (the scan only reads `~/.claude/projects`), so `import()` must
crates/engine/tests/external_import_config_repair.rs:21:/// Minimal on-disk transcript shaped like a real `~/.claude/projects/
crates/engine/tests/external_import_config_repair.rs:96:            "transcriptPath": "/home/user/.claude/projects/-work-project/some-external-session-id.jsonl",
crates/ui/src/overview/linear_host.rs:627:        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
crates/ui/src/overview/linear_host.rs:631:        let mut flags = crate::overview_positions::load_string_set(&data_dir, file);
crates/engine/tests/m5c_accounts_uploads_titles.rs:6://! the real `~/.claude` / `~/.codex`), so they are hermetic and parallel-safe.
crates/engine/tests/m5c_accounts_uploads_titles.rs:34:        data_dir: root.join("data"),
crates/engine/tests/m5c_accounts_uploads_titles.rs:44:    std::fs::create_dir_all(&config.claude_config_dir).expect("claude dir");
crates/engine/tests/m5c_accounts_uploads_titles.rs:46:        &config.claude_config_file,
crates/engine/tests/m5c_accounts_uploads_titles.rs:63:        config.claude_config_dir.join(".credentials.json"),
crates/engine/tests/m5c_accounts_uploads_titles.rs:95:    std::fs::create_dir_all(&config.codex_home).expect("codex home");
crates/engine/tests/m5c_accounts_uploads_titles.rs:97:        config.codex_home.join("auth.json"),
crates/engine/tests/m5c_accounts_uploads_titles.rs:227:        &std::fs::read_to_string(config.claude_config_dir.join(".credentials.json"))
crates/engine/tests/m5c_accounts_uploads_titles.rs:233:        serde_json::from_str(&std::fs::read_to_string(&config.claude_config_file).expect("cfg"))
crates/engine/tests/m5c_accounts_uploads_titles.rs:244:    let slots_dir = config.data_dir.join("agent-accounts").join("claude-code");
crates/engine/tests/m5c_accounts_uploads_titles.rs:253:    std::fs::write(&config.claude_config_file, "{ definitely not json").expect("corrupt");
crates/engine/tests/m5c_accounts_uploads_titles.rs:264:        std::fs::read_to_string(&config.claude_config_file).expect("still there"),
crates/engine/tests/m5c_accounts_uploads_titles.rs:274:    let creds_file = config.claude_config_dir.join(".credentials.json");
crates/engine/tests/m5c_accounts_uploads_titles.rs:349:    let creds_file = config.claude_config_dir.join(".credentials.json");
crates/engine/tests/m5c_accounts_uploads_titles.rs:418:        &std::fs::read_to_string(config.codex_home.join("auth.json")).expect("auth"),
crates/engine/tests/m5c_accounts_uploads_titles.rs:425:        config.codex_home.join("auth.json"),
crates/engine/tests/external_import_junk_filter.rs:4://! one-shot transcript under `~/.claude/projects` as a side effect, the same
crates/engine/tests/external_import_junk_filter.rs:14://! Same `$HOME`-redirection convention as `external_import_bulk_session_canvas.rs`
crates/engine/tests/external_import_junk_filter.rs:68:        .join(".claude")
crates/engine/tests/external_import_junk_filter.rs:96:    let data_dir = tempfile::tempdir().expect("data dir");
crates/engine/tests/external_import_junk_filter.rs:98:        data_dir.path(),
crates/harness/src/adapter_install.rs:9://! ONCE into a zeron-owned prefix (`~/.zeron/adapters/<pkg>/<version>` on
crates/harness/src/adapter_install.rs:66:/// `$MAST_DATA_DIR/adapters`. Windows defaults to
crates/harness/src/adapter_install.rs:68:/// Unix keeps `~/.zeron/adapters`.
crates/harness/src/adapter_install.rs:88:    if let Some(dir) = value("MAST_DATA_DIR") {
crates/harness/src/adapter_install.rs:704:                    ("MAST_DATA_DIR", data.clone().into_os_string()),
crates/harness/src/adapter_install.rs:713:                &env(&[("MAST_DATA_DIR", data.clone().into_os_string())]),
crates/engine/tests/external_import_auto_adopt.rs:2://! function because it redirects the process-global `$HOME` and sets
crates/engine/tests/external_import_auto_adopt.rs:65:        .join(".claude")
crates/engine/tests/external_import_auto_adopt.rs:116:    let data_dir = tempfile::tempdir().expect("data dir");
crates/engine/tests/external_import_auto_adopt.rs:118:        data_dir.path(),
crates/engine/tests/external_import_title_repair_harness_session.rs:7://! other cases) since it mutates the process-global `$HOME` env var — same
crates/engine/tests/external_import_title_repair_harness_session.rs:8://! "at most one `$HOME`-mutating test per binary" convention every other
crates/engine/tests/external_import_title_repair_harness_session.rs:61:        .join(".claude")
crates/engine/tests/external_import_title_repair_harness_session.rs:95:    let data_dir = tempfile::tempdir().expect("data dir");
crates/engine/tests/external_import_title_repair_harness_session.rs:97:        data_dir.path(),
crates/proto/src/env.rs:2://! spelling still honoured so existing shells, launchd/systemd units, `~/.zeron/env`
crates/proto/src/env.rs:5://! Callers pass the new name (`MAST_DATA_DIR`). The new name wins when both are
crates/proto/src/env.rs:70:            legacy_name("MAST_DATA_DIR").as_deref(),
crates/proto/src/env.rs:71:            Some("ZERON_DATA_DIR")
crates/harness/src/account_home.rs:2://! CLI with `CLAUDE_CONFIG_DIR` / `CODEX_HOME` pointing at that account's
crates/harness/src/account_home.rs:5://! sees exactly the environment Mast itself has, and the live `~/.claude` /
crates/harness/src/account_home.rs:6://! `~/.codex` login is untouched.
crates/harness/src/account_home.rs:23:        HarnessId::ClaudeCode => Some("CLAUDE_CONFIG_DIR"),
crates/harness/src/account_home.rs:24:        HarnessId::Codex => Some("CODEX_HOME"),
crates/engine/tests/m5_repos_diffs_terminals.rs:70:fn test_repos(data_dir: &Path) -> Repos {
crates/engine/tests/m5_repos_diffs_terminals.rs:71:    Repos::with_worktrees_root(data_dir, "device-test", data_dir.join("worktrees"))
crates/engine/tests/m5_repos_diffs_terminals.rs:1930:    // worktrees out of $HOME. (Process-global — this is the only test that sets it.)
crates/ui/src/appshots.rs:612:pub fn request_running_appshot(data_dir: &Path) -> Result<(), CaptureError> {
crates/ui/src/appshots.rs:613:    linux::request_running_appshot(data_dir)
crates/ui/src/appshots.rs:617:pub fn request_running_appshot(_data_dir: &Path) -> Result<(), CaptureError> {
crates/engine/tests/workspace_files.rs:42:fn assemble(data_dir: &Path, device_id: &str) -> EngineCore {
crates/engine/tests/workspace_files.rs:43:    std::fs::create_dir_all(data_dir).expect("data dir");
crates/engine/tests/workspace_files.rs:44:    std::fs::write(data_dir.join("device-id"), device_id).expect("device id");
crates/engine/tests/workspace_files.rs:46:        data_dir,
crates/engine/tests/external_import_titles.rs:13://! Same `$HOME`-redirection convention as `external_import_bulk_session_canvas.rs`
crates/engine/tests/external_import_titles.rs:14://! (the registry lives under `$HOME`) — kept as one test function so a
crates/engine/tests/external_import_titles.rs:15://! sibling test in this crate can never observe a mid-flight `$HOME` swap
crates/engine/tests/external_import_titles.rs:85:    let data_dir = tempfile::tempdir().expect("data dir");
crates/engine/tests/external_import_titles.rs:87:        data_dir.path(),
crates/harness/src/executable.rs:22:pub(crate) fn home_dir() -> Option<PathBuf> {
crates/engine/tests/local_profiles.rs:14:    data_dir: &Path,
crates/engine/tests/local_profiles.rs:20:        data_dir: data_dir.to_path_buf(),
crates/rpc/src/lib.rs:380:    /// Claude Code sessions found on disk (`~/.claude/projects`) that this
crates/rpc/src/lib.rs:383:    /// scan the wrong machine's `~/.claude`).
crates/rpc/src/lib.rs:482:    /// `~/.claude/projects` transcripts — neither makes sense forwarded to
crates/rpc/src/lib.rs:530:    /// own `~/.zeron/worktrees` layout — see `chat_workspace_plan` module docs):
crates/engine/tests/codex_login_resolver.rs:23:        "#!/bin/sh\n[ \"$2\" = login ] || exit 1\n[ -d \"$CODEX_HOME\" ] || exit 2\nprintf '%s\\n' 'https://auth.openai.com/authorize?test=resolver'\n",
crates/harness/src/agent_tools.rs:16://!   layer), so `~/.codex/config.toml` is untouched.
crates/harness/src/agent_tools.rs:144:        "mcpServers": { injection.server_name.clone(): injection.claude_server_config() }
crates/harness/src/agent_tools.rs:169:/// [`write_claude_mcp_config`]). Never the user's checkout or `~/.claude`.
crates/harness/src/agent_tools.rs:280:        let overrides = injection().codex_config_overrides();
crates/engine/tests/managed_accounts.rs:3://! path lives under a tempdir (never the real `~/.claude` / `~/.codex`), and
crates/engine/tests/managed_accounts.rs:23:    let claude = root.join("live/.claude");
crates/engine/tests/managed_accounts.rs:24:    let codex = root.join("live/.codex");
crates/engine/tests/managed_accounts.rs:32:fn homes_live_under_the_data_dir_and_are_private() {
crates/engine/tests/managed_accounts.rs:242:    let claude_live = tmp.path().join("live/.claude");
crates/engine/tests/managed_accounts.rs:243:    let codex_live = tmp.path().join("live/.codex");
crates/engine/tests/managed_accounts.rs:252:             printf '%s|%s\\n' \"$*\" \"${{CLAUDE_CONFIG_DIR-<unset>}}\" >> {rec}\n\
crates/engine/tests/managed_accounts.rs:254:             printf '{{\"oauthAccount\":{{\"emailAddress\":\"me@work.test\"}}}}' > \"$CLAUDE_CONFIG_DIR/.claude.json\"\n\
crates/engine/tests/managed_accounts.rs:572:    std::fs::write(home.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"work@example.test"}}"#).unwrap();
crates/engine/tests/managed_accounts.rs:597:  printf '{"oauthAccount":{"emailAddress":"me@work.test"}}' > "$CLAUDE_CONFIG_DIR/.claude.json"
crates/engine/tests/managed_accounts.rs:722:printf '{"tokens":{}}' > "$CODEX_HOME/auth.json"
crates/ui/src/shell/start_chat_ui.rs:1650:        let data_dir = dir.to_path_buf();
crates/ui/src/shell/start_chat_ui.rs:1661:                    data_dir: data_dir.clone(),
crates/ui/src/state.rs:94:    /// Data directory for the embedded engine (`~/.zeron`).
crates/ui/src/state.rs:95:    pub data_dir: PathBuf,
crates/ui/src/state.rs:291:        tracing::info!(data_dir = %config.data_dir.display(), "no daemon on port; embedding engine");
crates/ui/src/state.rs:293:            data_dir: config.data_dir,
crates/ui/src/state.rs:307:        std::fs::create_dir_all(&engine_config.data_dir)?;
crates/ui/src/state.rs:310:            match InstanceLock::acquire(&engine_config.data_dir) {
crates/ui/src/state.rs:758:    pub data_dir: Option<PathBuf>,
crates/ui/src/state.rs:884:            data_dir: None,
crates/ui/src/state.rs:2046:        self.data_dir.hash(&mut h);
crates/ui/src/state.rs:2964:        let data_dir = config.data_dir.clone();
crates/ui/src/state.rs:2969:            s.data_dir = Some(data_dir);
crates/ui/src/state.rs:4253:            data_dir: dir.path().to_path_buf(),
crates/ui/src/state.rs:4283:            data_dir: dir.path().to_path_buf(),
crates/ui/src/state.rs:4321:            data_dir: dir.path().to_path_buf(),
crates/ui/src/state.rs:4378:            data_dir: dir.path().to_path_buf(),
crates/ui/src/state.rs:4416:            data_dir: dir.path().to_path_buf(),
crates/ui/src/state.rs:4458:            data_dir: dir.path().to_path_buf(),
crates/ui/src/state.rs:4518:            data_dir: dir.path().to_path_buf(),
crates/ui/src/state.rs:4545:            data_dir: dir.path().to_path_buf(),
crates/ui/src/state.rs:4596:            data_dir: dir.path().to_path_buf(),
crates/ui/src/state.rs:4654:            data_dir: ui_dir.path().to_path_buf(),
crates/ui/src/shell/required_actions_ui.rs:151:        let data_dir = dir.to_path_buf();
crates/ui/src/shell/required_actions_ui.rs:162:                    data_dir: data_dir.clone(),
crates/ui/src/shell/workspace_search.rs:986:                    data_dir: dir.path().into(),
crates/ui/src/shell/workspace_search.rs:1133:                    data_dir: dir.path().into(),
crates/ui/src/shell/workspace_search.rs:1264:                    data_dir: dir.path().into(),
crates/ui/src/shell/workspace_search.rs:1572:                    data_dir: dir.path().into(),
crates/ui/src/github/prefs.rs:74:pub(crate) fn load_state(data_dir: &Path) -> GithubPanelState {
crates/ui/src/github/prefs.rs:76:        data_dir,
crates/ui/src/github/prefs.rs:81:pub(crate) fn save_state(data_dir: &Path, state: &GithubPanelState) {
crates/ui/src/github/prefs.rs:83:        overview_positions::save_string_set(data_dir, GITHUB_PANEL_FILE, &state_to_set(state))
crates/ui/src/shell/perf_tests.rs:91:        let data_dir = dir.path().to_path_buf();
crates/ui/src/shell/perf_tests.rs:96:                    data_dir,
crates/ui/src/shell/perf_tests.rs:112:        state.data_dir = Some(dir.path().to_path_buf());
crates/ui/src/shell/perf_tests.rs:271:        state.data_dir = Some(dir.path().to_path_buf());
crates/ui/src/shell/perf_tests.rs:274:        data_dir: dir.path().to_path_buf(),
crates/ui/src/shell/perf_tests.rs:416:        data_dir: dir.path().to_path_buf(),
crates/ui/src/github/panel.rs:151:            .data_dir
crates/ui/src/github/panel.rs:287:        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
crates/ui/src/github/panel.rs:291:            &data_dir,
crates/harness/src/cursor/mod.rs:19://!   (`~/.zeron/adapters/…`, [`crate::adapter_install::ensure_installed_shim`])
crates/harness/src/cursor/mod.rs:68:    if let Some(home) = crate::executable::home_dir() {
crates/ui/src/shell/pr_review_ui.rs:123:        let data_dir = dir.to_path_buf();
crates/ui/src/shell/pr_review_ui.rs:134:                    data_dir: data_dir.clone(),
crates/ui/src/shell/instruction_skill_tests.rs:20:/// never reads `.claude/skills`), like the live engine does. A folder named
crates/ui/src/shell/instruction_skill_tests.rs:270:    let path = dir.join(".claude").join("skills").join(folder);
crates/ui/src/shell/instruction_skill_tests.rs:280:/// `SKILL.md` files (up to the submodule's git root) plus `~/.claude/skills`,
crates/harness/src/archive_install.rs:6://! archive lands ONCE in `~/.zeron/adapters/<name>/<version>`, extraction runs
crates/ui/src/settings/composer.rs:75:    /// Load from `{data_dir}/composer-defaults.json`; defaults on any failure.
crates/ui/src/settings/composer.rs:76:    pub fn load(data_dir: &Path) -> Self {
crates/ui/src/settings/composer.rs:77:        match std::fs::read_to_string(Self::path(data_dir)) {
crates/ui/src/settings/composer.rs:90:    pub fn save(&self, data_dir: &Path) -> io::Result<()> {
crates/ui/src/settings/composer.rs:91:        std::fs::create_dir_all(data_dir)?;
crates/ui/src/settings/composer.rs:92:        let path = Self::path(data_dir);
crates/ui/src/settings/composer.rs:107:            std::fs::File::open(data_dir)?.sync_all()?;
crates/ui/src/settings/composer.rs:116:    pub fn path(data_dir: &Path) -> PathBuf {
crates/ui/src/settings/composer.rs:117:        data_dir.join(FILE_NAME)
crates/ui/src/shell/work_outline_ui.rs:1652:        let data_dir = dir.to_path_buf();
crates/ui/src/shell/work_outline_ui.rs:1664:                    data_dir: data_dir.clone(),
crates/ui/src/work_outline/filter.rs:263:pub(crate) fn load_filter(data_dir: &Path) -> BTreeSet<String> {
crates/ui/src/work_outline/filter.rs:264:    overview_positions::load_string_set(data_dir, overview_positions::WORK_ITEM_FILTER_FILE)
crates/ui/src/work_outline/filter.rs:267:pub(crate) fn save_filter(data_dir: &Path, filter: &BTreeSet<String>) -> std::io::Result<()> {
crates/ui/src/work_outline/filter.rs:268:    overview_positions::save_string_set(data_dir, overview_positions::WORK_ITEM_FILTER_FILE, filter)
crates/ui/src/shell/tabs.rs:181:        let defaults = crate::settings::composer::ComposerDefaults::load(&self.data_dir);
crates/ui/examples/appshots-fixture.rs:77:        data_dir: data.clone(),
crates/ui/src/shell/menu_bar_ui.rs:203:        let data_dir = dir.to_path_buf();
crates/ui/src/shell/menu_bar_ui.rs:214:                    data_dir: data_dir.clone(),
crates/ui/examples/command-palette-fixture.rs:51:        let boot = EngineBootConfig { data_dir: data, ipc_port: 0, edge_url: String::new(), edge_token: None, org_id: None, workos_client_id: None, default_harness: HarnessId::ClaudeCode };
crates/ui/src/shell/files_panel_workspace_tests.rs:193:                data_dir: data,
crates/sync/src/store.rs:69:/// SQLite-backed store under a data directory (`{data_dir}/docs.sqlite3`).
crates/sync/src/store.rs:79:    data_dir: std::path::PathBuf,
crates/sync/src/store.rs:84:    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
crates/sync/src/store.rs:85:        let data_dir = data_dir.as_ref();
crates/sync/src/store.rs:86:        std::fs::create_dir_all(data_dir)?;
crates/sync/src/store.rs:87:        let mut conn = Connection::open(data_dir.join("docs.sqlite3"))?;
crates/sync/src/store.rs:96:            data_dir: data_dir.to_path_buf(),
crates/sync/src/store.rs:102:    /// `data_dir` string) place their own sidecar files alongside it without
crates/sync/src/store.rs:105:        &self.data_dir
crates/harness/tests/model_catalog.rs:141:        ".codex/auth.json",
crates/harness/tests/model_catalog.rs:164:        (&claude as &dyn Harness, ".claude/settings.json"),
crates/harness/tests/model_catalog.rs:189:        "CLAUDE_CONFIG_DIR",
crates/harness/tests/model_catalog.rs:192:        "CODEX_HOME",
crates/ui/examples/outline-fixture.rs:55:            s.data_dir = Some(data.clone());
crates/harness/src/codex/mod.rs:114:    if let Some(home) = crate::executable::home_dir() {
crates/harness/src/codex/mod.rs:116:        extra.push(home.join(".codex").join("bin").join("codex"));
crates/harness/src/codex/mod.rs:129:/// the harness itself would run. `CODEX_HOME` isolates the login from the live
crates/harness/src/codex/mod.rs:130:/// `~/.codex` session; the caller owns stdio wiring and cancellation.
crates/harness/src/codex/mod.rs:135:    cmd.arg("login").env("CODEX_HOME", codex_home);
crates/harness/src/codex/mod.rs:152:    /// Managed account home (`CODEX_HOME`); `None` = the Default login.
crates/harness/src/codex/mod.rs:217:                 ~/.codex/bin, ~/.npm-global/bin, /opt/homebrew/bin, /usr/local/bin, \
crates/harness/src/codex/mod.rs:228:    /// (custom `~/.codex/prompts` are NOT exposed; the TUI-only built-ins
crates/harness/src/codex/mod.rs:1024:/// `~/.codex/config.toml` servers untouched. A no-op without an injection.
crates/harness/src/codex/mod.rs:1039:        config.extend(tools.codex_config_overrides());
crates/ui/src/shell/running_ui.rs:1251:        let data_dir = dir.to_path_buf();
crates/ui/src/shell/running_ui.rs:1262:                    data_dir: data_dir.clone(),
crates/ui/examples/macos-resource-profile.rs:111:            let boot = EngineBootConfig { data_dir:data, ipc_port:0,
crates/ui/src/composer.rs:4175:/// `.claude/commands` and skills) — two chats on the same harness but
crates/ui/src/composer.rs:4242:/// `<cwd>/.claude/skills` (walking up to the cwd's git root, stopping at a
crates/harness/tests/account_home.rs:2://! `CLAUDE_CONFIG_DIR` / `CODEX_HOME`; Default spawns carry neither. Fake
crates/harness/tests/account_home.rs:50:/// A fake claude that records `CLAUDE_CONFIG_DIR` (or `<unset>`) and finishes.
crates/harness/tests/account_home.rs:57:             printf '%s\\n' \"${{CLAUDE_CONFIG_DIR-<unset>}}\" >> {}\n\
crates/harness/tests/account_home.rs:84:    unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };
crates/harness/tests/account_home.rs:124:            "printf '%s\\n' \"${{CODEX_HOME-<unset>}}\" >> {}\nexit 1\n",
crates/harness/tests/account_home.rs:128:    unsafe { std::env::remove_var("CODEX_HOME") };
crates/ui/src/shell/chat_item_tests.rs:133:                    data_dir: dir.path().into(),
crates/ui/src/overview_positions.rs:39:/// Load a JSON string array from `{data_dir}/{file_name}`. Absent or corrupt
crates/ui/src/overview_positions.rs:41:pub fn load_string_set(data_dir: &Path, file_name: &str) -> BTreeSet<String> {
crates/ui/src/overview_positions.rs:42:    match std::fs::read_to_string(data_dir.join(file_name)) {
crates/ui/src/overview_positions.rs:55:pub fn save_string_set(data_dir: &Path, file_name: &str, set: &BTreeSet<String>) -> io::Result<()> {
crates/ui/src/overview_positions.rs:56:    write_json_atomic(data_dir, &data_dir.join(file_name), set)
crates/ui/src/overview_positions.rs:59:/// Load an ordered JSON string array from `{data_dir}/{file_name}`.
crates/ui/src/overview_positions.rs:61:pub fn load_string_list(data_dir: &Path, file_name: &str) -> Vec<String> {
crates/ui/src/overview_positions.rs:62:    match std::fs::read_to_string(data_dir.join(file_name)) {
crates/ui/src/overview_positions.rs:75:pub fn save_string_list(data_dir: &Path, file_name: &str, list: &[String]) -> io::Result<()> {
crates/ui/src/overview_positions.rs:76:    write_json_atomic(data_dir, &data_dir.join(file_name), &list)
crates/ui/src/overview_positions.rs:82:    data_dir: &Path,
crates/ui/src/overview_positions.rs:86:    std::fs::create_dir_all(data_dir)?;
crates/sync/examples/doc_surgery.rs:3://!   dump-workspace    <data_dir>
crates/sync/examples/doc_surgery.rs:4://!   inspect-workspace <data_dir> <chat_id>
crates/sync/examples/doc_surgery.rs:5://!   inspect-chat      <data_dir> <chat_id>
crates/sync/examples/doc_surgery.rs:6://!   cut-chat          <data_dir> <chat_id> <from_index>
crates/sync/examples/doc_surgery.rs:24:    let data_dir = args.get(2).expect("data dir");
crates/sync/examples/doc_surgery.rs:25:    let store = DocsStore::open(data_dir).expect("open store");
crates/ui/examples/preview-fixture.rs:143:        data_dir: data.clone(),
crates/ui/src/work_outline/details_linear.rs:89:        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
crates/ui/src/work_outline/details_linear.rs:94:            crate::overview_positions::load_string_set(&data_dir, PASTE_HINT_SEEN_FILE);
crates/ui/src/work_outline/details_linear.rs:99:            crate::overview_positions::save_string_set(&data_dir, PASTE_HINT_SEEN_FILE, &seen)
crates/harness/src/lib.rs:178:    /// project-scoped (Claude Code's `.claude/commands` and skills) should
crates/harness/src/lib.rs:186:    /// catalog probes) uses `home` as its CLI home (`CLAUDE_CONFIG_DIR` /
crates/harness/src/lib.rs:187:    /// `CODEX_HOME`), with its own caches. `None` for harnesses without
crates/ui/src/shell.rs:1406:    data_dir: &std::path::Path,
crates/ui/src/shell.rs:1419:        if port_closed && InstanceLock::holder(data_dir).is_none() {
crates/ui/src/shell.rs:1437:    data_dir: &std::path::Path,
crates/ui/src/shell.rs:1450:    match wait_for_remote_engine_shutdown(ipc_port, data_dir, RUNTIME_CHANGE_TIMEOUT).await {
crates/ui/src/shell.rs:1897:    data_dir: PathBuf,
crates/ui/src/shell.rs:2113:        let data_dir = boot.data_dir.clone();
crates/ui/src/shell.rs:2339:            data_dir,
crates/ui/src/shell.rs:5760:        let data_dir = self.data_dir.clone();
crates/ui/src/shell.rs:5761:        let shutdown_dir = data_dir.clone();
crates/ui/src/shell.rs:5901:        let data_dir = self.data_dir.clone();
crates/ui/src/shell.rs:5903:            stop_synced_runtime(engine, ipc_port, &data_dir).await
crates/ui/src/shell.rs:6104:        let data_dir = self.data_dir.clone();
crates/ui/src/shell.rs:6111:            wait_for_remote_engine_shutdown(ipc_port, &data_dir, RUNTIME_CHANGE_TIMEOUT).await
crates/ui/src/shell.rs:8523:        let data_dir = self.data_dir.clone();
crates/ui/src/shell.rs:8528:            install.stage_desktop(&edge_url, &manifest, &data_dir).await
crates/ui/src/shell.rs:13202:            data_dir: dir.path().to_path_buf(),
crates/ui/src/shell.rs:13900:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:13987:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:14059:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:14163:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:14269:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:14354:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:14428:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:14513:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:14585:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:14650:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:14786:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:15062:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:15158:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:15236:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:15300:                    data_dir: dir.path().into(),
crates/ui/src/shell.rs:15450:        _data_dir: tempfile::TempDir,
crates/ui/src/shell.rs:15479:                        data_dir: dir.path().into(),
crates/ui/src/shell.rs:15497:                _data_dir: dir,
crates/ui/src/shell.rs:15798:        _data_dir: tempfile::TempDir,
crates/ui/src/shell.rs:15833:                        data_dir: dir.path().into(),
crates/ui/src/shell.rs:15883:                _data_dir: dir,
crates/ui/src/shell/files_panel.rs:443:                    data_dir: dir.path().into(),
crates/ui/src/shell/files_panel.rs:509:                    data_dir: dir.path().into(),
crates/harness/src/model_context.rs:36:                .unwrap_or_else(|| root("CODEX_HOME", home.join(".codex")));
crates/harness/src/model_context.rs:42:                .unwrap_or_else(|| root("CLAUDE_CONFIG_DIR", home.join(".claude")));
crates/ui/examples/project-selector-fixture.rs:48:        let boot = EngineBootConfig { data_dir: data, ipc_port: 0, edge_url: String::new(),
crates/harness/src/claude/mod.rs:75:    if let Some(home) = crate::executable::home_dir() {
crates/harness/src/claude/mod.rs:76:        extra.push(home.join(".claude").join("local").join("claude"));
crates/harness/src/claude/mod.rs:93:/// harness's executable resolution and child PATH, and `CLAUDE_CONFIG_DIR`
crates/harness/src/claude/mod.rs:119:    /// (project-scoped `.claude/commands` and skills) — kept separate from
crates/harness/src/claude/mod.rs:130:    /// Managed account home (`CLAUDE_CONFIG_DIR`); `None` = the Default login.
crates/harness/src/claude/mod.rs:261:                "claude (searched PATH, the login shell's PATH, ~/.claude/local, \
crates/harness/src/claude/mod.rs:303:        // Mast's default so `~/.claude/settings.json` cannot pick it silently.
crates/harness/src/claude/mod.rs:391:    /// from the directory it's spawned in (`<cwd>/.claude` alongside the
crates/harness/src/claude/mod.rs:392:    /// global `~/.claude`), so this must run in that chat's cwd rather than
crates/engine/tests/subagent_scan_real_smoke.rs:8:        "/Users/mikey/.claude/projects/-Users-mikey-Projects-cofactr-workspace/13371e6c-e8b4-4c61-a772-da80b5909352.jsonl",
crates/ui/src/shell/instructions_ui.rs:1397:        let data_dir = dir.to_path_buf();
crates/ui/src/shell/instructions_ui.rs:1408:                    data_dir: data_dir.clone(),
crates/ui/src/work_outline/space_roots.rs:91:    pub(super) fn load_space_prefs(&mut self, data_dir: &std::path::Path) {
crates/ui/src/work_outline/space_roots.rs:93:            data_dir,
crates/ui/src/work_outline/space_roots.rs:97:            data_dir,
crates/ui/src/work_outline/space_roots.rs:173:        if let Some(data_dir) = self.state.read(cx).data_dir.clone()
crates/ui/src/work_outline/space_roots.rs:175:                &data_dir,
crates/ui/src/work_outline/space_roots.rs:218:        if let Some(data_dir) = self.state.read(cx).data_dir.clone()
crates/ui/src/work_outline/space_roots.rs:220:                &data_dir,
crates/harness/src/claude/catalog.rs:155:            std::env::var_os("CLAUDE_CONFIG_DIR")
crates/harness/src/claude/catalog.rs:158:                .unwrap_or_else(|| crate::executable::home_or_current_dir().join(".claude"))
crates/ui/examples/browser-fixture.rs:189:        let boot = EngineBootConfig { data_dir: data, ipc_port: 0, edge_url: String::new(), edge_token: None, org_id: None, workos_client_id: None, default_harness: HarnessId::ClaudeCode };
crates/ui/src/shell/spaces.rs:235:                    data_dir: path.into(),
crates/ui/src/shell/spaces.rs:1018:                        data_dir: dir.path().into(),
crates/ui/src/shell/spaces.rs:1129:                        data_dir: dir.path().into(),
crates/ui/src/shell/spaces.rs:1223:                        data_dir: dir.path().into(),
crates/ui/src/shell/spaces.rs:6865:                    data_dir: data.path().into(),
crates/ui/src/shell/spaces.rs:6943:                    data_dir: data.path().into(),
crates/ui/src/shell/spaces.rs:7015:                    data_dir: data.path().into(),
crates/ui/src/shell/work_outline_menu_ui.rs:774:        let data_dir = dir.to_path_buf();
crates/ui/src/shell/work_outline_menu_ui.rs:785:                    data_dir: data_dir.clone(),
crates/ui/src/shell/artifacts_ui.rs:382:        let data_dir = dir.to_path_buf();
crates/ui/src/shell/artifacts_ui.rs:393:                    data_dir: data_dir.clone(),
crates/ui/src/shell/sidebar_sections.rs:604:                    data_dir: path.into(),
crates/harness/src/claude/discovery.rs:65:/// Slash commands depend on the CLI's project cwd (`.claude/commands`,
crates/ui/examples/new-project-fixture.rs:58:        let boot = EngineBootConfig { data_dir: data, ipc_port: 0, edge_url: String::new(), edge_token: None, org_id: None, workos_client_id: None, default_harness: HarnessId::ClaudeCode };
crates/ui/src/appshots/linux/mod.rs:108:pub fn activation_socket_path(data_dir: &Path) -> PathBuf {
crates/ui/src/appshots/linux/mod.rs:109:    data_dir.join(ACTIVATION_SOCKET)
crates/ui/src/appshots/linux/mod.rs:112:fn start_activation_socket(data_dir: PathBuf, tx: mpsc::UnboundedSender<()>) {
crates/ui/src/appshots/linux/mod.rs:116:            if fs::create_dir_all(&data_dir).is_err() {
crates/ui/src/appshots/linux/mod.rs:119:            let path = activation_socket_path(&data_dir);
crates/ui/src/appshots/linux/mod.rs:137:pub fn request_running_appshot(data_dir: &Path) -> Result<(), CaptureError> {
crates/ui/src/appshots/linux/mod.rs:144:        .connect(activation_socket_path(data_dir))
crates/ui/src/appshots/linux/mod.rs:159:    fn activation_path_stays_inside_data_directory() {
crates/ui/src/work_outline/outline_view.rs:783:        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
crates/ui/src/work_outline/outline_view.rs:788:            &data_dir,
crates/ui/src/work_outline/outline_view.rs:791:        self.load_space_prefs(&data_dir);
crates/ui/src/work_outline/outline_view.rs:855:        if let Some(data_dir) = self.state.read(cx).data_dir.clone() {
crates/ui/src/work_outline/outline_view.rs:858:                &data_dir,
crates/ui/examples/sidebar-fixture.rs:82:        let boot = EngineBootConfig { data_dir: data, ipc_port: 0, edge_url: String::new(), edge_token: None, org_id: None, workos_client_id: None, default_harness: HarnessId::ClaudeCode };
crates/ui/src/linear/prefs.rs:119:pub(crate) fn load_panel_state(data_dir: &Path) -> PanelState {
crates/ui/src/linear/prefs.rs:121:        data_dir,
crates/ui/src/linear/prefs.rs:126:pub(crate) fn save_panel_state(data_dir: &Path, state: &PanelState) {
crates/ui/src/linear/prefs.rs:128:        overview_positions::save_string_set(data_dir, LINEAR_PANEL_FILE, &panel_state_to_set(state))
crates/ui/src/theme_library.rs:16:    pub data_dir: PathBuf,
crates/ui/src/theme_library.rs:23:pub fn init(data_dir: impl Into<PathBuf>, cx: &mut App) {
crates/ui/src/theme_library.rs:24:    let data_dir = data_dir.into();
crates/ui/src/theme_library.rs:25:    let (library, load_warning) = match CustomThemeLibrary::load(&data_dir) {
crates/ui/src/theme_library.rs:34:        data_dir,
crates/ui/src/theme_library.rs:108:    let duplicate = next.duplicate_as_editable(id, &state.data_dir)?;
crates/ui/src/theme_library.rs:149:    next.save(&state.data_dir)
crates/ui/src/theme_library.rs:187:        let data_dir = tempfile::tempdir().unwrap();
crates/ui/src/theme_library.rs:188:        let blocking_file = data_dir.path().join("not-a-directory");
crates/ui/src/theme_library.rs:192:            data_dir: blocking_file,
crates/harness/tests/claude.rs:659:/// `.claude/commands`/skills live there), not the engine's own inherited
crates/ui/src/linear/panel.rs:388:            .data_dir
crates/ui/src/linear/panel.rs:714:        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
crates/ui/src/linear/panel.rs:718:            &data_dir,
crates/ui/src/composer/skill_files.rs:2://! `.claude/skills/<dir>/SKILL.md` from a folder up to its nearest git root
crates/ui/src/composer/skill_files.rs:3://! (project skills, nearest folder first), plus `~/.claude/skills` (user
crates/ui/src/composer/skill_files.rs:18:    /// `.claude/skills` from the folder up to its git root, nearest first.
crates/ui/src/composer/skill_files.rs:20:    /// `~/.claude/skills`.
crates/ui/src/composer/skill_files.rs:29:        .flat_map(|dir| read_skills_dir(&dir.join(".claude").join("skills")))
crates/ui/src/composer/skill_files.rs:32:        .map(|home| read_skills_dir(&home.join(".claude").join("skills")))
crates/ui/src/composer/skill_files.rs:41:/// `.claude/skills` are the user skills).
crates/ui/src/composer/skill_files.rs:165:        let path = dir.join(".claude").join("skills").join(folder);
crates/ui/src/composer/skill_files.rs:207:    /// `~/.claude/skills`.
crates/ui/src/composer/skill_completion.rs:26://! git root, then `~/.claude/skills`) when the root is on this device,
crates/ui/src/composer/skill_completion.rs:259:    /// Whose `~/.claude/skills` the instruction editors read.
crates/ui/src/composer/skill_completion.rs:335:    /// Read user skills from `home` instead of `$HOME` (tests).
crates/ui/src/lib.rs:119:    pub data_dir: PathBuf,
crates/ui/src/lib.rs:140:            data_dir: self.data_dir.clone(),
crates/ui/src/lib.rs:164:    let data_dir = config.boot().data_dir.clone();
crates/ui/src/lib.rs:166:        .in_scope(|| settings::UiSettings::load(&data_dir));
crates/ui/src/lib.rs:196:        settings::init(ui_settings.clone(), data_dir.clone(), cx);
crates/ui/src/lib.rs:212:            .in_scope(|| theme_library::init(data_dir.clone(), cx));
crates/ui/src/lib.rs:290:        start_appshot_service(config.boot().data_dir, cx);
crates/ui/src/composer/instruction_reset.rs:791:        let data_dir = dir.path().to_path_buf();
crates/ui/src/composer/instruction_reset.rs:803:            crate::settings::init(crate::settings::UiSettings::default(), &data_dir, cx);
```
