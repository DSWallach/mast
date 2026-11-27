//! Repos — this device's git repositories, branches, worktrees, and the folder
//! browser (feature-inventory §3.5; port of zeron's `repos.ts` + `folder-lister.ts`).
//!
//! Repos are device-local (paths differ per machine), so the known set is a plain
//! JSON list (`{data_dir}/repos.json`) — no sync. Existing repos can live anywhere
//! the user points us; cloned/created ones land in `{data_dir}/repos`. Worktrees are
//! created under `~/.zeron/worktrees/<repoName>/<worktreeName>` (NOT the data
//! dir — worktrees are user-facing working checkouts), with an auto-generated name +
//! matching `zeron/<name>` branch. `MAST_WORKTREES_DIR` overrides the root.
//!
//! All git access is via subprocess (`tokio::process`) — never libgit2.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::{StreamExt, stream};
use sha2::{Digest, Sha256};

use mast_proto::{
    DriveEntry, FileSearchMatch, FolderEntry, FolderListing, GitHistoryCommit,
    GitHistoryComparison, GitHistoryPage, GitHistoryRef, GitHistoryRefKind, Repo, RepoRef,
    Worktree,
};

use crate::EngineError;

mod branch_naming;
mod checkout_layout;
mod git_history;
pub use git_history::git_history_matches;
mod workspace_access_policy;
use branch_naming::*;
use checkout_layout::*;
use workspace_access_policy::*;
pub use branch_naming::worktree_branch_from_title;
pub(crate) use branch_naming::{BRANCH_PREFIX, is_owned_branch, is_owned_branch_of};
pub use checkout_layout::CheckoutIdentity;

/// Read a checkout's symbolic branch without spawning git.
pub(crate) fn checkout_branch_from_fs(path: &Path) -> Option<String> {
    let (_, git_dir) = checkout_layout::checkout_layout_from_fs(path)?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    head.trim().strip_prefix("ref: refs/heads/")
        .filter(|branch| !branch.is_empty()).map(str::to_owned)
}
pub use workspace_access_policy::is_automatic_access_blocked;
pub(crate) use workspace_access_policy::{home_dir, is_broad_workspace_root, is_broad_workspace_root_resolved};

/// Existence probe timeout for user-chosen / remembered paths, which can point at
/// dead network mounts where a bare `stat` hangs for minutes.
const PATH_EXISTS_TIMEOUT: Duration = Duration::from_secs(2);
/// Hard wall-clock ceiling for a folder listing (the walk runs in a disposable
/// blocking task; on expiry the caller unblocks and the task is abandoned).
const FOLDER_LIST_TIMEOUT: Duration = Duration::from_secs(6);
/// Cap on returned folder entries (bounds response size).
const FOLDER_LIST_MAX_ENTRIES: usize = 500;
/// Cap on returned drives (a machine with more mounts than this is a server
/// farm, not a laptop picking a project folder).
const DRIVE_LIST_MAX_ENTRIES: usize = 50;
/// File mentions should remain responsive even in very large checkouts.
const FILE_SEARCH_MAX_RESULTS: usize = 8;
/// A dead network mount must not leave the composer search spinning forever.
const FILE_SEARCH_TIMEOUT: Duration = Duration::from_secs(6);
const GITHUB_AVATAR_TIMEOUT: Duration = Duration::from_secs(6);
const FILE_INDEX_TTL: Duration = Duration::from_secs(10);
const FILE_INDEX_MAX_ENTRIES: usize = 250_000;
const RANK_BUFFER: usize = 1_024;
pub const GIT_HISTORY_DEFAULT_LIMIT: usize = 100;
pub const GIT_HISTORY_MAX_LIMIT: usize = 200;

/// Where new worktrees live. Deliberately NOT under the backend data dir —
/// worktrees are user-facing working checkouts. `MAST_WORKTREES_DIR` overrides
/// (test isolation); empty reads as unset.
fn default_worktrees_root() -> PathBuf {
    mast_proto::env::var_os("MAST_WORKTREES_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = home_dir();
            mast_proto::env::assert_test_home_isolated(&home);
            mast_proto::env::home_storage_dir(&home, false).0.join("worktrees")
        })
}

struct ReposInner {
    data_dir: PathBuf,
    device_id: String,
    worktrees_root: PathBuf,
    file_searches: std::sync::Mutex<HashMap<PathBuf, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    http: reqwest::Client,
    github_avatars: std::sync::Mutex<HashMap<String, String>>,
    github_avatar_pages: std::sync::Mutex<HashSet<String>>,
    file_index: FileIndexCache,
    checkout_sets: std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<CheckoutSetSlot>>>,
    checkout_set_refreshes: std::sync::atomic::AtomicU64,
}

/// Canonical repo-root + linked-worktree paths of one Space, refreshed under
/// the slot's async mutex so concurrent callers share a single refresh.
#[derive(Default)]
struct CheckoutSetSlot {
    state: tokio::sync::Mutex<Option<CheckoutSet>>,
}

struct CheckoutSet {
    paths: HashSet<PathBuf>,
    refreshed: std::time::Instant,
}

/// A cached checkout set is trusted this long when the lookup hits.
const CHECKOUT_SET_TTL: Duration = Duration::from_secs(60);
/// A lookup miss re-reads `git worktree list` only if the set is older than this.
const CHECKOUT_SET_MISS_REFRESH: Duration = Duration::from_secs(3);

struct IndexedPath {
    path: String,
    haystack: nucleo_matcher::Utf32String,
    is_dir: bool,
}

struct FileIndex {
    entries: Vec<IndexedPath>,
    built: std::time::Instant,
}

type FileIndexCache = std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<FileIndex>>>;

#[derive(Clone)]
pub struct Repos {
    inner: std::sync::Arc<ReposInner>,
}

impl Repos {
    pub(crate) fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    /// `data_dir` holds `repos.json` + cloned/created repos; the worktree root
    /// comes from `$MAST_WORKTREES_DIR` or `~/.zeron/worktrees`.
    pub fn new(data_dir: &Path, device_id: &str) -> Self {
        Self::with_worktrees_root(data_dir, device_id, default_worktrees_root())
    }

    /// Explicit worktree root (tests).
    pub fn with_worktrees_root(data_dir: &Path, device_id: &str, worktrees_root: PathBuf) -> Self {
        Self {
            inner: std::sync::Arc::new(ReposInner {
                data_dir: data_dir.to_path_buf(),
                device_id: device_id.to_string(),
                worktrees_root,
                file_searches: std::sync::Mutex::new(HashMap::new()),
                http: reqwest::Client::builder()
                    .timeout(GITHUB_AVATAR_TIMEOUT)
                    .user_agent("Comet-Git-History")
                    .build()
                    .unwrap_or_else(|_| reqwest::Client::new()),
                github_avatars: std::sync::Mutex::new(HashMap::new()),
                github_avatar_pages: std::sync::Mutex::new(HashSet::new()),
                file_index: std::sync::Mutex::new(HashMap::new()),
                checkout_sets: std::sync::Mutex::new(HashMap::new()),
                checkout_set_refreshes: std::sync::atomic::AtomicU64::new(0),
            }),
        }
    }

    // ── registry (repos.json) ───────────────────────────────────────────────

    fn registry_path(&self) -> PathBuf {
        self.inner.data_dir.join("repos.json")
    }

    fn load_paths(&self) -> Vec<String> {
        std::fs::read_to_string(self.registry_path())
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default()
    }

    fn save_paths(&self, paths: &[String]) -> Result<(), EngineError> {
        let mut seen = HashSet::new();
        let deduped: Vec<&String> = paths.iter().filter(|p| seen.insert(p.as_str())).collect();
        let json = serde_json::to_string_pretty(&deduped)
            .map_err(|e| EngineError::Other(format!("repos registry serialize: {e}")))?;
        std::fs::create_dir_all(&self.inner.data_dir)?;
        std::fs::write(self.registry_path(), json)?;
        Ok(())
    }

    fn register(&self, path: &str) -> Result<(), EngineError> {
        let mut paths = self.load_paths();
        paths.push(path.to_string());
        self.save_paths(&paths)
    }

    // ── git plumbing ────────────────────────────────────────────────────────

    /// Run `git <args>` (optionally under `cwd`), returning trimmed stdout.
    pub(crate) async fn git(
        &self,
        args: &[&str],
        cwd: Option<&Path>,
    ) -> Result<String, EngineError> {
        let mut cmd = mast_harness::git_command_async();
        cmd.args(args);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(std::process::Stdio::null());
        // Network operations can run for minutes; they must not hold a slot
        // that quick local lookups are queued behind.
        let network = args
            .iter()
            .find(|arg| !arg.starts_with('-'))
            .is_some_and(|arg| matches!(*arg, "fetch" | "clone" | "push" | "pull"));
        let output = {
            let _permit = if network {
                None
            } else {
                Some(crate::git_gate::acquire(crate::git_gate::default_tag("repos", args)).await)
            };
            cmd.output().await
        }
        .map_err(|e| EngineError::Other(format!("git spawn failed: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = stderr.trim();
            return Err(EngineError::Other(if message.is_empty() {
                format!(
                    "git {} failed ({})",
                    args.first().unwrap_or(&"?"),
                    output.status
                )
            } else {
                format!("git: {message}")
            }));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Async existence probe with a timeout: a wedged network mount just reads
    /// as "gone" instead of hanging every caller.
    pub(crate) async fn path_exists(path: &Path) -> bool {
        let path = path.to_path_buf();
        matches!(
            tokio::time::timeout(PATH_EXISTS_TIMEOUT, tokio::fs::metadata(path)).await,
            Ok(Ok(_))
        )
    }

    /// Is `path` inside a git work tree? (Also the SpacesSync git-presence probe.)
    pub async fn is_repo(&self, path: &Path) -> bool {
        if checkout_layout(path).await.is_some() {
            return true;
        }
        matches!(
            self.git(&["rev-parse", "--is-inside-work-tree"], Some(path)).await,
            Ok(out) if out == "true"
        )
    }

    /// The branch currently checked out at a repo/worktree path (`"HEAD"` when detached).
    pub async fn current_branch(&self, path: &Path) -> Result<String, EngineError> {
        // Spawn-free when HEAD is a plain branch ref or a detached sha.
        if let Some((_, git_dir)) = checkout_layout(path).await
            && let Ok(head) = tokio::fs::read_to_string(git_dir.join("HEAD")).await
        {
            let head = head.trim();
            if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
                return Ok(branch.to_owned());
            }
            if !head.starts_with("ref:") && head.len() >= 40 && head.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Ok("HEAD".to_string());
            }
        }
        let branch = self.git(&["branch", "--show-current"], Some(path)).await?;
        Ok(if branch.is_empty() {
            "HEAD".to_string()
        } else {
            branch
        })
    }

    /// Commit checked out at `path`, or `None` for a repository without an
    /// initial commit.
    pub async fn head_sha(&self, path: &Path) -> Result<Option<String>, EngineError> {
        match self
            .git(&["rev-parse", "--verify", "HEAD^{commit}"], Some(path))
            .await
        {
            Ok(sha) if !sha.is_empty() => Ok(Some(sha)),
            Ok(_) => Ok(None),
            Err(_) if self.is_repo(path).await => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Fetch every configured remote without pruning or integrating anything
    /// into the active branch. This intentionally updates refs only.
    pub async fn fetch_all(&self, repo_path: &Path) -> Result<(), EngineError> {
        self.git(&["fetch", "--all", "--quiet"], Some(repo_path))
            .await
            .map(drop)
    }

    /// The absolute Git `HEAD` file for event-driven external branch reconciliation.
    pub async fn git_head_path(&self, path: &Path) -> Result<PathBuf, EngineError> {
        let git_dir = self
            .git(&["rev-parse", "--absolute-git-dir"], Some(path))
            .await?;
        Ok(PathBuf::from(git_dir).join("HEAD"))
    }

    /// Canonical identity shared by every chat operating in this exact worktree:
    /// `sha256(deviceId ‖ NUL ‖ canonical git dir)`.
    pub async fn checkout_identity(&self, path: &Path) -> Result<CheckoutIdentity, EngineError> {
        // Fast path: read the checkout layout from disk, no `git` spawn. Any
        // doubt (bare repo, odd layout, env overrides) falls through to git.
        if let Some((root, git_dir)) = checkout_layout(path).await {
            return Ok(self.identity_from(root, git_dir));
        }
        let root = self
            .git(&["rev-parse", "--show-toplevel"], Some(path))
            .await?;
        let git_dir = self
            .git(
                &["rev-parse", "--path-format=absolute", "--git-dir"],
                Some(path),
            )
            .await?;
        let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| PathBuf::from(&root));
        let canonical_git_dir =
            std::fs::canonicalize(&git_dir).unwrap_or_else(|_| PathBuf::from(&git_dir));
        Ok(self.identity_from(canonical_root, canonical_git_dir))
    }

    fn identity_from(&self, root: PathBuf, git_dir: PathBuf) -> CheckoutIdentity {
        let mut hasher = Sha256::new();
        hasher.update(self.inner.device_id.as_bytes());
        hasher.update([0u8]);
        hasher.update(git_dir.to_string_lossy().as_bytes());
        CheckoutIdentity {
            id: hex(&hasher.finalize()),
            root,
            git_dir,
        }
    }

    async fn to_repo(&self, path: &Path) -> Result<Repo, EngineError> {
        let branch = self.current_branch(path).await.ok();
        Ok(Repo {
            path: path.to_string_lossy().to_string(),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string_lossy().to_string()),
            default_branch: branch,
        })
    }

    // ── ListRepos / AddRepo / CloneRepo / CreateRepo ────────────────────────

    /// Known repos that still exist, each with its current branch. Never fails:
    /// vanished paths and non-repos are silently dropped.
    pub async fn list(&self) -> Vec<Repo> {
        let mut repos = Vec::new();
        for path in self.load_paths() {
            let path = PathBuf::from(path);
            if !Self::path_exists(&path).await || !self.is_repo(&path).await {
                continue;
            }
            match self.to_repo(&path).await {
                Ok(repo) => repos.push(repo),
                Err(err) => {
                    tracing::debug!(path = %path.display(), error = %err, "repo listing skip")
                }
            }
        }
        repos
    }

    /// Remember an existing repository the user pointed us at.
    pub async fn add(&self, path: &str) -> Result<Repo, EngineError> {
        let abs = absolutize(Path::new(path));
        if !Self::path_exists(&abs).await {
            return Err(EngineError::Other(format!(
                "No such folder: {}",
                abs.display()
            )));
        }
        if !self.is_repo(&abs).await {
            return Err(EngineError::Other(format!(
                "Not a git repository: {}",
                abs.display()
            )));
        }
        self.register(&abs.to_string_lossy())?;
        self.to_repo(&abs).await
    }

    /// `git clone <url>` under `{data_dir}/repos`. (Named `clone_repo` to keep
    /// `Clone::clone` unambiguous on the service handle.)
    pub async fn clone_repo(&self, url: &str) -> Result<Repo, EngineError> {
        let trimmed = url.trim().trim_end_matches('/');
        let name = trimmed
            .trim_end_matches(".git")
            .rsplit(['/', ':'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("repo")
            .to_string();
        let repos_dir = self.inner.data_dir.join("repos");
        let target = repos_dir.join(&name);
        if target.exists() {
            return Err(EngineError::Other(format!(
                "Already exists: {}",
                target.display()
            )));
        }
        std::fs::create_dir_all(&repos_dir)?;
        self.git(&["clone", trimmed, &target.to_string_lossy()], None)
            .await?;
        self.register(&target.to_string_lossy())?;
        self.to_repo(&target).await
    }

    /// `git init -b main` a fresh repository under `{data_dir}/repos`.
    pub async fn create(&self, name: &str) -> Result<Repo, EngineError> {
        let clean: String = name
            .trim()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        if clean.is_empty() || clean.chars().all(|c| c == '-' || c == '.') {
            return Err(EngineError::Other("Invalid repository name".into()));
        }
        let target = self.inner.data_dir.join("repos").join(&clean);
        if target.exists() {
            return Err(EngineError::Other(format!(
                "Already exists: {}",
                target.display()
            )));
        }
        std::fs::create_dir_all(&target)?;
        self.git(&["init", "-b", "main"], Some(&target)).await?;
        self.register(&target.to_string_lossy())?;
        self.to_repo(&target).await
    }

    // ── branches ────────────────────────────────────────────────────────────

    /// All branches (`git branch -a`), local first, deduped against their remote
    /// counterparts, with the repo's default branch first.
    pub async fn branches(&self, repo_path: &Path) -> Result<Vec<String>, EngineError> {
        let out = self
            .git(&["branch", "-a", "--format=%(refname)"], Some(repo_path))
            .await?;
        let mut names: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut push = |name: &str| {
            if !name.is_empty() && name != "HEAD" && seen.insert(name.to_string()) {
                names.push(name.to_string());
            }
        };
        // Locals first, then remote-only branches (stripped of their remote prefix).
        for line in out.lines().map(str::trim) {
            if let Some(local) = line.strip_prefix("refs/heads/") {
                push(local);
            }
        }
        for line in out.lines().map(str::trim) {
            if let Some(remote) = line.strip_prefix("refs/remotes/")
                && let Some((_, name)) = remote.split_once('/')
            {
                push(name);
            }
        }
        // Default branch first: origin/HEAD's target, else the checked-out branch.
        let default = match self
            .git(
                &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
                Some(repo_path),
            )
            .await
        {
            Ok(short) => short.split_once('/').map(|(_, b)| b.to_string()),
            Err(_) => None,
        };
        let default = match default {
            Some(branch) => Some(branch),
            None => self
                .current_branch(repo_path)
                .await
                .ok()
                .filter(|b| b != "HEAD"),
        };
        if let Some(default) = default
            && let Some(pos) = names.iter().position(|n| *n == default)
        {
            let head = names.remove(pos);
            names.insert(0, head);
        }
        Ok(names)
    }

    /// [`Self::branches`] enriched with checkout state: which branch the MAIN
    /// folder has checked out (`current`) and which branches are materialized
    /// as linked worktrees (`worktree_path`). Feeds the composer's ref picker
    /// and its checkout-kind selector.
    pub async fn refs(&self, repo_path: &Path) -> Result<Vec<RepoRef>, EngineError> {
        let names = self.branches(repo_path).await?;
        let current = self.current_branch(repo_path).await.ok();
        // `git worktree list --porcelain`: stanzas of `worktree <path>` /
        // `HEAD <sha>` / `branch refs/heads/<name>`. The first stanza is the
        // main checkout — excluded (it's `current`, not a linked worktree).
        let mut worktrees: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        if let Ok(out) = self
            .git(&["worktree", "list", "--porcelain"], Some(repo_path))
            .await
        {
            let mut stanza = 0usize;
            let mut path: Option<String> = None;
            for line in out.lines().map(str::trim) {
                if let Some(p) = line.strip_prefix("worktree ") {
                    stanza += 1;
                    // The first stanza is the main checkout, not a linked tree.
                    path = (stanza > 1).then(|| p.to_string());
                } else if let Some(branch) = line.strip_prefix("branch refs/heads/")
                    && let Some(path) = path.take()
                {
                    worktrees.insert(branch.to_string(), path);
                }
            }
        }
        Ok(names
            .into_iter()
            .map(|name| RepoRef {
                current: current.as_deref() == Some(name.as_str()),
                worktree_path: worktrees.get(&name).cloned(),
                name,
            })
            .collect())
    }

    /// Whether `candidate` is the repository root or one of its linked
    /// worktrees. Filesystem resolution happens on a disposable thread because
    /// user-selected paths may be dead mounts.
    pub async fn workspace_checkout(&self, repo_path: &Path, candidate: &Path) -> Option<PathBuf> {
        let candidate = self.canonical_path(candidate).await?;
        self.workspace_checkout_canonical(repo_path, candidate).await
    }

    /// Canonicalize on a disposable thread (dead mounts must not wedge callers).
    pub(crate) async fn canonical_path(&self, path: &Path) -> Option<PathBuf> {
        let path = path.to_path_buf();
        disposable_worker("checkout-auth", move || std::fs::canonicalize(path).ok())
            .await
            .flatten()
    }

    /// Like [`Self::workspace_checkout`] for an already-canonical candidate.
    /// Membership comes from a per-Space cache of `git worktree list` (one git
    /// spawn per refresh, never branches or refs). A hit is served from the
    /// cache; a miss re-reads the list only when the cache is a few seconds
    /// old, so a stale path asked for repeatedly costs no further spawns.
    pub(crate) async fn workspace_checkout_canonical(
        &self,
        repo_path: &Path,
        candidate: PathBuf,
    ) -> Option<PathBuf> {
        let slot = {
            let mut sets = self
                .inner
                .checkout_sets
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            sets.entry(repo_path.to_path_buf()).or_default().clone()
        };
        let mut state = slot.state.lock().await;
        if let Some(set) = state.as_ref()
            && set.refreshed.elapsed() < CHECKOUT_SET_TTL
        {
            if set.paths.contains(&candidate) {
                return Some(candidate);
            }
            if set.refreshed.elapsed() < CHECKOUT_SET_MISS_REFRESH {
                return None;
            }
        }
        let paths = self.read_checkout_set(repo_path).await;
        let found = paths.contains(&candidate);
        *state = Some(CheckoutSet {
            paths,
            refreshed: std::time::Instant::now(),
        });
        found.then_some(candidate)
    }

    /// Canonical repo root plus every `worktree <path>` from one
    /// `git worktree list --porcelain`.
    async fn read_checkout_set(&self, repo_path: &Path) -> HashSet<PathBuf> {
        self.inner
            .checkout_set_refreshes
            .fetch_add(1, Ordering::Relaxed);
        let listed = self
            .git(&["worktree", "list", "--porcelain"], Some(repo_path))
            .await
            .unwrap_or_default();
        let mut raw: Vec<PathBuf> = vec![repo_path.to_path_buf()];
        raw.extend(
            listed
                .lines()
                .filter_map(|line| line.trim().strip_prefix("worktree "))
                .map(PathBuf::from),
        );
        disposable_worker("checkout-auth", move || {
            raw.into_iter()
                .filter_map(|path| std::fs::canonicalize(path).ok())
                .collect::<HashSet<_>>()
        })
        .await
        .unwrap_or_default()
    }

    pub(crate) async fn same_folder(&self, folder: &Path, candidate: &Path) -> Option<PathBuf> {
        let folder = folder.to_path_buf();
        let candidate = candidate.to_path_buf();
        disposable_worker("folder-auth", move || {
            let candidate = std::fs::canonicalize(candidate).ok()?;
            (std::fs::canonicalize(folder).ok()? == candidate).then_some(candidate)
        })
        .await
        .flatten()
    }

    /// Switch the checkout at `cwd` (a main folder OR a linked worktree) to
    /// `ref_name` — the t3code `switchRef` port: an existing local branch is
    /// checked out directly; a remote-only branch gets a local tracking
    /// branch (`checkout --track origin/<ref>`). A dirty tree or a branch
    /// already checked out in another worktree fails with git's own message.
    /// Returns the resulting current branch.
    pub async fn switch_ref(&self, cwd: &Path, ref_name: &str) -> Result<String, EngineError> {
        let local = self
            .git(
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{ref_name}"),
                ],
                Some(cwd),
            )
            .await
            .is_ok();
        if local {
            self.git(&["checkout", ref_name], Some(cwd)).await?;
        } else {
            let remote = format!("origin/{ref_name}");
            let has_remote = self
                .git(
                    &[
                        "show-ref",
                        "--verify",
                        "--quiet",
                        &format!("refs/remotes/{remote}"),
                    ],
                    Some(cwd),
                )
                .await
                .is_ok();
            if has_remote {
                self.git(&["checkout", "--track", &remote], Some(cwd))
                    .await?;
            } else {
                // Unknown ref: let git produce the authoritative error.
                self.git(&["checkout", ref_name], Some(cwd)).await?;
            }
        }
        let out = self.git(&["branch", "--show-current"], Some(cwd)).await?;
        Ok(out.trim().to_string())
    }

    // ── worktrees ───────────────────────────────────────────────────────────

    /// `git worktree add` an isolated checkout under
    /// `{worktrees_root}/<repoName>/<generatedName>`, on a fresh `zeron/<name>`
    /// branch off `branch`.
    pub async fn create_worktree(
        &self,
        repo_path: &Path,
        branch: &str,
    ) -> Result<Worktree, EngineError> {
        let repo_name = repo_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".to_string());
        let base = self.inner.worktrees_root.join(&repo_name);
        std::fs::create_dir_all(&base)?;
        // Auto-generate a name colliding with neither an existing dir nor branch.
        let existing: HashSet<String> = self
            .branches(repo_path)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut name = None;
        for attempt in 0..50u64 {
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(attempt)
                .wrapping_add(attempt.wrapping_mul(0x9E37_79B9));
            let candidate = format!(
                "{}-{}",
                ADJECTIVES[(seed % ADJECTIVES.len() as u64) as usize],
                NOUNS[((seed / 31) % NOUNS.len() as u64) as usize]
            );
            if !base.join(&candidate).exists()
                && !existing.contains(&format!("{BRANCH_PREFIX}{candidate}"))
            {
                name = Some(candidate);
                break;
            }
        }
        let name =
            name.ok_or_else(|| EngineError::Other("Could not allocate a worktree name".into()))?;
        let path = base.join(&name);
        let branch_name = format!("{BRANCH_PREFIX}{name}");
        self.git(
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                &path.to_string_lossy(),
                branch,
            ],
            Some(repo_path),
        )
        .await?;
        let checkout = self.checkout_identity(&path).await?;
        Ok(Worktree {
            repo_path: repo_path.to_string_lossy().to_string(),
            path: path.to_string_lossy().to_string(),
            branch: branch_name,
            name,
            checkout_id: Some(checkout.id),
        })
    }

    async fn branch_exists(&self, path: &Path, branch: &str) -> bool {
        self.git(
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
            Some(path),
        )
        .await
        .is_ok()
    }

    /// Rename a zeron-created worktree branch after its chat's generated title
    /// (port of zeron's `renameWorktreeBranch`). Guards:
    /// - respect an external checkout/rename: only act while the worktree is still
    ///   on `expected_branch` AND that branch is the original `zeron/<folderName>`;
    /// - a title-slug collision gets a stable 6-hex suffix (hash of the worktree
    ///   path); a collision on THAT too fails.
    ///
    /// Returns the branch the worktree ends up on (re-read after the rename so a
    /// concurrent external checkout always wins the metadata race).
    pub async fn rename_worktree_branch(
        &self,
        worktree_path: &Path,
        expected_branch: &str,
        title: &str,
    ) -> Result<String, EngineError> {
        let current = self.current_branch(worktree_path).await?;
        let folder = worktree_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if current != expected_branch || !is_owned_branch_of(expected_branch, &folder) {
            return Ok(current);
        }
        let preferred = worktree_branch_from_title(title);
        if preferred == current {
            return Ok(current);
        }
        let mut hasher = Sha256::new();
        hasher.update(worktree_path.to_string_lossy().as_bytes());
        let suffix = &hex(&hasher.finalize())[..6];
        let target = if self.branch_exists(worktree_path, &preferred).await {
            format!("{preferred}-{suffix}")
        } else {
            preferred
        };
        if self.branch_exists(worktree_path, &target).await {
            return Err(EngineError::Other(format!(
                "Branch already exists: {target}"
            )));
        }
        self.git(
            &["branch", "-m", "--", &current, &target],
            Some(worktree_path),
        )
        .await?;
        self.current_branch(worktree_path).await
    }

    /// Best-effort worktree removal (if it still exists), then prune stale refs.
    /// Deletes the worktree's branch ONLY when zeron created it (`zeron/…`) — the
    /// user may have checked out their own branch inside the worktree.
    pub async fn delete_worktree(
        &self,
        repo_path: &Path,
        worktree_path: &Path,
    ) -> Result<(), EngineError> {
        let branch = if worktree_path.exists() {
            self.current_branch(worktree_path).await.unwrap_or_default()
        } else {
            String::new()
        };
        if worktree_path.exists() {
            let removed = self
                .git(
                    &[
                        "worktree",
                        "remove",
                        "--force",
                        &worktree_path.to_string_lossy(),
                    ],
                    Some(repo_path),
                )
                .await;
            if removed.is_err() {
                // git refused (or the dir is half-gone) — delete the folder directly.
                let _ = std::fs::remove_dir_all(worktree_path);
            }
        }
        let _ = self.git(&["worktree", "prune"], Some(repo_path)).await;
        if is_owned_branch(&branch) {
            let _ = self.git(&["branch", "-D", &branch], Some(repo_path)).await;
        }
        Ok(())
    }

    // ── ListFolders ─────────────────────────────────────────────────────────

    /// One directory level (home by default): dotfiles hidden, directories first,
    /// capped at [`FOLDER_LIST_MAX_ENTRIES`] with a `truncated` flag. The walk runs
    /// in a spawned blocking task under a 6s wall-clock ceiling — a wedged path
    /// (dead mount, permission-gated folder) fails this listing without blocking
    /// anything else; the abandoned task unwinds on its own thread.
    pub async fn list_folders(&self, path: Option<String>) -> Result<FolderListing, EngineError> {
        self.list_folders_with(path, FOLDER_LIST_TIMEOUT, false)
            .await
    }

    /// Search a checkout's files and directories by fuzzy relative path. The
    /// `ignore` walker honors `.gitignore`, `.ignore`, and global git excludes.
    /// Dotfiles remain searchable; only repository metadata is always pruned.
    pub async fn search_files(
        &self,
        root: PathBuf,
        query: String,
        featured_paths: Vec<String>,
    ) -> Result<Vec<FileSearchMatch>, EngineError> {
        // Defense in depth: the authenticated workspace-file resolver already
        // rejects broad roots. Keep the lower-level indexer safe too so a new
        // caller cannot accidentally recursively walk HOME (and its protected
        // Music/Pictures/Documents descendants).
        if is_broad_workspace_root(&root) {
            return Err(EngineError::Other(
                "file search requires a specific project folder".into(),
            ));
        }
        let deadline = tokio::time::Instant::now() + FILE_SEARCH_TIMEOUT;
        let gate = {
            let mut searches = self
                .inner
                .file_searches
                .lock()
                .map_err(|_| EngineError::Other("file search registry poisoned".into()))?;
            if let Some(gate) = searches.get(&root).and_then(std::sync::Weak::upgrade) {
                gate
            } else {
                let gate = std::sync::Arc::new(tokio::sync::Mutex::new(()));
                searches.insert(root.clone(), std::sync::Arc::downgrade(&gate));
                gate
            }
        };
        let gate = tokio::time::timeout_at(deadline, gate.lock_owned())
            .await
            .map_err(|_| EngineError::Other("file search timed out".into()))?;
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(cancelled.clone());
        let worker_cancelled = cancelled.clone();
        let cache = self.inner.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("file-search".into())
            .spawn(move || {
                let _gate = gate;
                let _ = tx.send(search_files_cached(
                    &cache.file_index,
                    &root,
                    &query,
                    &featured_paths,
                    || worker_cancelled.load(Ordering::Relaxed),
                ));
            })
            .map_err(|e| EngineError::Other(format!("file search failed: {e}")))?;
        match tokio::time::timeout_at(deadline, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(EngineError::Other("file search worker exited".into())),
            Err(_) => Err(EngineError::Other("file search timed out".into())),
        }
    }

    /// `hang_for_test` makes the worker never respond — exercises the timeout path.
    ///
    /// The walk runs on a DETACHED OS thread (not the tokio blocking pool): a
    /// readdir wedged in the kernel can't be cancelled, and a poisoned blocking
    /// pool — or a runtime shutdown waiting on it — must never be possible. On
    /// timeout the thread is simply abandoned (the zeron backend's disposable
    /// worker, minus the terminate()).
    #[doc(hidden)]
    pub async fn list_folders_with(
        &self,
        path: Option<String>,
        timeout: Duration,
        hang_for_test: bool,
    ) -> Result<FolderListing, EngineError> {
        let target = match path.filter(|p| !p.trim().is_empty()) {
            Some(p) => absolutize(Path::new(&p)),
            None => home_dir(),
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("folder-list".into())
            .spawn(move || {
                if hang_for_test {
                    // Hold the sender without responding (detached thread; process
                    // exit reclaims it) — the caller must hit its timeout.
                    std::thread::sleep(Duration::from_secs(3600));
                }
                let _ = tx.send(list_folders_blocking(&target));
            });
        if let Err(err) = spawned {
            return Err(EngineError::Other(format!("folder listing failed: {err}")));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(EngineError::Other("folder listing worker exited".into())),
            Err(_) => Err(EngineError::Other(
                "folder listing timed out on the device".into(),
            )),
        }
    }

    // ── ListDrives ──────────────────────────────────────────────────────────

    /// Mounted drives/volumes — the browse roots beyond home. Same disposable
    /// worker + wall-clock ceiling as `ListFolders`: a dead network mount can
    /// wedge the probe (macOS `canonicalize` stats each volume), and that must
    /// fail this listing, not the runtime.
    pub async fn list_drives(&self) -> Result<Vec<DriveEntry>, EngineError> {
        let worker = disposable_worker("drive-list", list_drives_blocking);
        match tokio::time::timeout(FOLDER_LIST_TIMEOUT, worker).await {
            Ok(Some(drives)) => Ok(drives),
            Ok(None) => Err(EngineError::Other("drive listing worker exited".into())),
            Err(_) => Err(EngineError::Other(
                "drive listing timed out on the device".into(),
            )),
        }
    }
}

struct CancelOnDrop(std::sync::Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

async fn disposable_worker<T: Send + 'static>(
    name: &'static str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .ok()?;
    rx.await.ok()
}

/// The blocking walk: ONE readdir of the target; `is_repo` is a cheap `.git`
/// existence probe per directory entry.
fn list_folders_blocking(target: &Path) -> Result<FolderListing, EngineError> {
    let read = std::fs::read_dir(target).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            EngineError::Other("Mast doesn't have access to this folder on the device.".into())
        }
        _ => EngineError::Other(format!("could not read that folder: {e}")),
    })?;
    let mut entries: Vec<FolderEntry> = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        // Merely opening the project browser at HOME must not probe inside
        // macOS-protected folders. Importantly this decision happens BEFORE
        // `DirEntry::file_type`: that metadata call itself can trigger TCC,
        // even if we avoid the later `Music/.git` / `Pictures/.git` probe.
        // These well-known entries are directories on macOS; if the user
        // explicitly navigates into one, that later listing is intentional.
        let protected_home_child = is_protected_home_child_with_home(target, &name, &home_dir());
        let is_dir = protected_home_child || entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let is_repo = is_dir && !protected_home_child && entry.path().join(".git").exists();
        entries.push(FolderEntry {
            name,
            is_dir,
            is_repo,
        });
    }
    // Directories first, each group name-sorted (case-insensitive).
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let truncated = entries.len() > FOLDER_LIST_MAX_ENTRIES;
    entries.truncate(FOLDER_LIST_MAX_ENTRIES);
    Ok(FolderListing {
        path: target.to_string_lossy().to_string(),
        entries,
        truncated,
    })
}

/// The blocking drive walk. Best-effort everywhere: the list is navigation
/// sugar for the folder browser, so an unreadable source means fewer rows,
/// never an error (home always remains reachable without it).
fn list_drives_blocking() -> Vec<DriveEntry> {
    #[cfg(target_os = "macos")]
    let drives = macos_drives();
    #[cfg(target_os = "linux")]
    let drives = linux_drives(&std::fs::read_to_string("/proc/mounts").unwrap_or_default());
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let drives = Vec::new();
    finish_drives(drives)
}

/// `/Volumes` holds every mounted volume; the boot volume is a symlink to `/`,
/// which `canonicalize` resolves — so the system drive arrives under its real
/// name ("Macintosh HD") and the fallback "System" row never shows on macOS.
#[cfg(target_os = "macos")]
fn macos_drives() -> Vec<DriveEntry> {
    let mut drives: Vec<DriveEntry> = Vec::new();
    if let Ok(read) = std::fs::read_dir("/Volumes") {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            // Follows the symlink — a dangling one (mid-eject) drops out here.
            if !path.is_dir() {
                continue;
            }
            let resolved = std::fs::canonicalize(&path).unwrap_or(path);
            drives.push(DriveEntry {
                name,
                path: resolved.to_string_lossy().to_string(),
            });
        }
    }
    drives
}

/// Top-level directories the FHS (or its de-facto extensions) owns. A
/// depth-one mount point OUTSIDE this set is a user-created drive root
/// (`/disk2`, `/data`, `/tank`) — the system's own split partitions
/// (`/boot`, a separate `/var` or `/home`) are plumbing, not drives.
#[cfg(any(target_os = "linux", test))]
const FHS_TOP_LEVEL: &[&str] = &[
    "bin",
    "boot",
    "dev",
    "efi",
    "etc",
    "home",
    "lib",
    "lib32",
    "lib64",
    "libx32",
    "lost+found",
    "media",
    "mnt",
    "nix",
    "opt",
    "proc",
    "root",
    "run",
    "sbin",
    "snap",
    "srv",
    "sys",
    "tmp",
    "usr",
    "var",
];

/// Mounted drives from `/proc/mounts`: the conventional removable locations
/// (`/media`, `/run/media`, `/mnt` — udisks, WSL drive letters, manual
/// mounts, whatever the filesystem) plus block-device partitions mounted at
/// custom top-level paths like `/disk2` (PR #144 feedback) — `/dev/*`
/// sources at depth-one non-FHS points, minus squashfs/erofs images (snaps).
/// Plus "System" for `/`: anything mounted deeper stays reachable by
/// browsing from the root, and the palette accepts typed paths besides.
#[cfg(any(target_os = "linux", test))]
fn linux_drives(mounts: &str) -> Vec<DriveEntry> {
    let mut drives: Vec<DriveEntry> = Vec::new();
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(source), Some(point), fstype) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let point = unescape_mount_point(point);
        let conventional = point.starts_with("/media/")
            || point.starts_with("/run/media/")
            || point == "/mnt"
            || point.starts_with("/mnt/");
        let custom_block = source.starts_with("/dev/")
            && !matches!(fstype, Some("squashfs") | Some("erofs"))
            && point
                .strip_prefix('/')
                .is_some_and(|p| !p.is_empty() && !p.contains('/') && !FHS_TOP_LEVEL.contains(&p));
        if !conventional && !custom_block {
            continue;
        }
        let name = point
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("Drive")
            .to_string();
        drives.push(DriveEntry { name, path: point });
    }
    drives.push(DriveEntry {
        name: "System".into(),
        path: "/".into(),
    });
    drives
}

/// Dedupe by mount point (first mention wins — the named row beats a
/// late generic duplicate), system root first, then name order, capped.
fn finish_drives(mut drives: Vec<DriveEntry>) -> Vec<DriveEntry> {
    let mut seen = HashSet::new();
    drives.retain(|d| seen.insert(d.path.clone()));
    drives.sort_by(|a, b| {
        (a.path != "/")
            .cmp(&(b.path != "/"))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    drives.truncate(DRIVE_LIST_MAX_ENTRIES);
    drives
}

/// `/proc/mounts` octal-escapes whitespace in mount points (`\040` = space).
#[cfg(any(target_os = "linux", test))]
fn unescape_mount_point(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let digits: String = chars.clone().take(3).collect();
            if digits.len() == 3
                && let Ok(code) = u8::from_str_radix(&digits, 8)
            {
                out.push(code as char);
                chars.nth(2);
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Case-insensitive subsequence score. Lower is better: adjacent and earlier
/// characters win, while still allowing `cmp rs` to find `composer.rs`.
fn fuzzy_score(query: &str, candidate: &str) -> Option<usize> {
    let candidate = candidate.to_lowercase();
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .try_fold(0usize, |total, term| {
            let mut at = 0;
            let mut score = 0usize;
            let mut previous_end = None;
            for needle in term.chars() {
                let found = candidate[at..].find(needle)? + at;
                score += found;
                if previous_end == Some(found) {
                    score = score.saturating_sub(2);
                }
                at = found + needle.len_utf8();
                previous_end = Some(at);
            }
            Some(total.saturating_add(score))
        })
}

type RankedFileMatch = (Option<usize>, u32, String, bool);

fn compare_file_matches(
    query: &str,
    (featured_a, score_a, path_a, dir_a): &RankedFileMatch,
    (featured_b, score_b, path_b, dir_b): &RankedFileMatch,
) -> std::cmp::Ordering {
    let empty_query = query.trim().is_empty();
    featured_a
        .is_none()
        .cmp(&featured_b.is_none())
        .then_with(|| featured_a.cmp(featured_b))
        .then_with(|| score_b.cmp(score_a))
        .then_with(|| {
            if empty_query { path_a.split('/').count().cmp(&path_b.split('/').count()) } else { std::cmp::Ordering::Equal }
        })
        .then_with(|| {
            if empty_query { dir_a.cmp(dir_b) } else { dir_b.cmp(dir_a) }
        })
        .then_with(|| path_a.len().cmp(&path_b.len()))
        .then_with(|| path_a.cmp(path_b))
}

#[cfg(test)]
fn search_files_blocking(
    root: &Path,
    query: &str,
    featured_paths: &[String],
) -> Result<Vec<FileSearchMatch>, EngineError> {
    search_files_blocking_with_cancel(root, query, featured_paths, || false)
}

#[cfg(test)]
fn search_files_blocking_with_cancel<F: Fn() -> bool + Sync>(
    root: &Path,
    query: &str,
    featured_paths: &[String],
    cancelled: F,
) -> Result<Vec<FileSearchMatch>, EngineError> {
    let root = canonical_search_root(root)?;
    let index = walk_file_index(&root, &cancelled)?;
    Ok(rank_file_matches(&index, &root, query, featured_paths))
}

fn search_files_cached<F: Fn() -> bool + Sync>(
    cache: &FileIndexCache,
    root: &Path,
    query: &str,
    featured_paths: &[String],
    cancelled: F,
) -> Result<Vec<FileSearchMatch>, EngineError> {
    let root = canonical_search_root(root)?;
    let fresh = cache
        .lock()
        .ok()
        .and_then(|indexes| indexes.get(&root).cloned())
        .filter(|index| index.built.elapsed() < FILE_INDEX_TTL);
    let index = match fresh {
        Some(index) => index,
        None => {
            let index = std::sync::Arc::new(FileIndex {
                entries: walk_file_index(&root, &cancelled)?,
                built: std::time::Instant::now(),
            });
            if let Ok(mut indexes) = cache.lock() {
                indexes.retain(|_, index| index.built.elapsed() < FILE_INDEX_TTL);
                indexes.insert(root.clone(), index.clone());
            }
            index
        }
    };
    if cancelled() {
        return Err(EngineError::Other("file search cancelled".into()));
    }
    Ok(rank_file_matches(
        &index.entries,
        &root,
        query,
        featured_paths,
    ))
}

fn canonical_search_root(root: &Path) -> Result<PathBuf, EngineError> {
    std::fs::canonicalize(root)
        .map_err(|e| EngineError::Other(format!("could not search workspace: {e}")))
}

fn walk_file_index<F: Fn() -> bool + Sync>(
    root: &Path,
    cancelled: &F,
) -> Result<Vec<IndexedPath>, EngineError> {
    if cancelled() {
        return Err(EngineError::Other("file search cancelled".into()));
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().min(12))
        .unwrap_or(4);
    let collected: std::sync::Mutex<Vec<IndexedPath>> = std::sync::Mutex::new(Vec::new());
    let was_cancelled = AtomicBool::new(false);
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .threads(threads)
        .filter_entry(|entry| entry.depth() == 0 || entry.file_name() != ".git")
        .build_parallel()
        .run(|| {
            const BATCH: usize = 512;
            struct Batch<'a> {
                items: Vec<IndexedPath>,
                sink: &'a std::sync::Mutex<Vec<IndexedPath>>,
            }
            impl Drop for Batch<'_> {
                fn drop(&mut self) {
                    if self.items.is_empty() {
                        return;
                    }
                    if let Ok(mut all) = self.sink.lock() {
                        all.append(&mut self.items);
                    }
                }
            }
            let mut batch = Batch {
                items: Vec::with_capacity(BATCH),
                sink: &collected,
            };
            let was_cancelled = &was_cancelled;
            Box::new(move |entry| {
                if was_cancelled.load(Ordering::Relaxed) {
                    return ignore::WalkState::Quit;
                }
                if cancelled() {
                    was_cancelled.store(true, Ordering::Relaxed);
                    return ignore::WalkState::Quit;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        tracing::debug!(%err, "file mention index skipped entry");
                        return ignore::WalkState::Continue;
                    }
                };
                let path = entry.path();
                if path == root {
                    return ignore::WalkState::Continue;
                }
                let Ok(relative) = path.strip_prefix(root) else {
                    return ignore::WalkState::Continue;
                };
                let relative = relative.to_string_lossy().replace('\\', "/");
                if relative == ".git" || relative.starts_with(".git/") {
                    return ignore::WalkState::Continue;
                }
                batch.items.push(IndexedPath {
                    haystack: nucleo_matcher::Utf32String::from(relative.as_str()),
                    path: relative,
                    is_dir: entry.file_type().is_some_and(|kind| kind.is_dir()),
                });
                if batch.items.len() >= BATCH
                    && let Ok(mut all) = batch.sink.lock()
                {
                    all.append(&mut batch.items);
                    if all.len() >= FILE_INDEX_MAX_ENTRIES {
                        return ignore::WalkState::Quit;
                    }
                }
                ignore::WalkState::Continue
            })
        });
    if was_cancelled.load(Ordering::Relaxed) || cancelled() {
        return Err(EngineError::Other("file search cancelled".into()));
    }
    let mut entries = collected
        .into_inner()
        .map_err(|_| EngineError::Other("file index poisoned".into()))?;
    entries.truncate(FILE_INDEX_MAX_ENTRIES);
    Ok(entries)
}

fn rank_file_matches(
    entries: &[IndexedPath],
    root: &Path,
    query: &str,
    featured_paths: &[String],
) -> Vec<FileSearchMatch> {
    let featured: HashMap<String, usize> = featured_paths
        .iter()
        .filter_map(|path| {
            let path = Path::new(path);
            let full = if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            };
            let canonical = std::fs::canonicalize(full).ok()?;
            let relative = canonical.strip_prefix(root).ok()?;
            Some(relative.to_string_lossy().replace('\\', "/"))
        })
        .enumerate()
        .fold(HashMap::new(), |mut paths, (rank, path)| {
            paths.entry(path).or_insert(rank);
            paths
        });
    let mut matcher = nucleo_matcher::Matcher::new({
        let mut config = nucleo_matcher::Config::DEFAULT;
        config.set_match_paths();
        config
    });
    let pattern = nucleo_matcher::pattern::Pattern::parse(
        query,
        nucleo_matcher::pattern::CaseMatching::Smart,
        nucleo_matcher::pattern::Normalization::Smart,
    );
    let mut matches: Vec<RankedFileMatch> = Vec::new();
    for entry in entries {
        let Some(score) = pattern.score(entry.haystack.slice(..), &mut matcher) else {
            continue;
        };
        matches.push((
            featured.get(&entry.path).copied(),
            score,
            entry.path.clone(),
            entry.is_dir,
        ));
        if matches.len() >= RANK_BUFFER {
            matches.sort_by(|a, b| compare_file_matches(query, a, b));
            matches.truncate(FILE_SEARCH_MAX_RESULTS);
        }
    }
    matches.sort_by(|a, b| compare_file_matches(query, a, b));
    matches.truncate(FILE_SEARCH_MAX_RESULTS);
    matches
        .into_iter()
        .map(|(_, _, path, is_dir)| FileSearchMatch { path, is_dir })
        .collect()
}

/// Absolute form of a possibly-relative path (no filesystem access).
fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[allow(clippy::disallowed_methods)]
#[cfg(test)]
mod tests;
