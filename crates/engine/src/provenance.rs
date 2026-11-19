//! Code → chat provenance recording (area E).
//!
//! When a checkout bound as a chat diff target moves HEAD, diff-sync hands a
//! [`HeadMove`] to [`ProvenanceRecorder`], which records (commit, patch-id,
//! chat, turn range) in the profile registry. No polling, no git hooks, no
//! trailers: it runs only on HEAD moves of bound worktrees, walks at most
//! [`MAX_PROVENANCE_WALK`] commits, and remaps records across rebases and
//! amends with `git patch-id --stable`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use mast_doc::{MessagePart, MessageRole, SessionMessageEntry};
use mast_proto::{
    CommitProvenance, MAX_PROVENANCE_WALK, ProvenanceSource, Session, SessionStatus, ToolCall,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::WorkspaceHost;

/// A commit made this soon after a turn ends (Working → idle) still belongs to
/// it: the agent commits just before the status flips. Not granted when the
/// chat parked on a question (Working → AwaitingInput): what follows is a human.
const ACTIVE_GRACE_MS: i64 = 15_000;
/// Status updates trail the turn start a little.
const START_SLACK_MS: i64 = 2_000;
/// Output cap for the `git log -p` feeding `git patch-id`.
const MAX_PATCH_BYTES: usize = 8 * 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(20);
/// Most turns a record spans.
const MAX_TURN_SPAN: usize = 20;
/// Working intervals remembered per chat.
const MAX_INTERVALS: usize = 16;

/// One HEAD move of a checkout some chat has bound as a diff target.
#[derive(Debug, Clone)]
pub struct HeadMove {
    pub root: PathBuf,
    /// The checkout's git dir (worktree-specific for linked worktrees).
    pub git_dir: PathBuf,
    pub old: String,
    pub new: String,
    /// Chats that bound this checkout.
    pub chats: Vec<String>,
    /// Epoch ms diff-sync last saw `old` as HEAD.
    pub since_ms: i64,
    /// Branch and remote-tracking ref tips as of diff-sync's capture BEFORE the
    /// move. Commits reachable from these are not the chat's. Empty when no
    /// snapshot exists yet (the recorder then reads the refs itself).
    pub prior_tips: Vec<String>,
}

pub type HeadMoveListener = Arc<dyn Fn(HeadMove) + Send + Sync>;
/// Reads one chat's transcript entries from local state, read-only (best effort).
pub type TranscriptReader = Arc<dyn Fn(&str) -> Option<Vec<SessionMessageEntry>> + Send + Sync>;

/// Working periods of one chat. Only `Working` counts: a chat parked on a
/// question (`AwaitingInput`) or idle is not writing code.
#[derive(Debug, Clone, Default)]
struct Activity {
    open_since: Option<i64>,
    /// Closed `(start, end, grace_ms)` intervals, oldest first. `grace_ms` is
    /// [`ACTIVE_GRACE_MS`] after a turn ended idle, 0 after it parked on a
    /// question.
    closed: std::collections::VecDeque<(i64, i64, i64)>,
}

impl Activity {
    /// The Working interval containing `t_ms` (with the grace after its end),
    /// as `(start, end)`; an open interval ends at `now_ms`.
    fn interval_at(&self, t_ms: i64, now_ms: i64) -> Option<(i64, i64)> {
        if let Some(start) = self.open_since
            && t_ms >= start - START_SLACK_MS
        {
            return Some((start, now_ms));
        }
        self.closed
            .iter()
            .rev()
            .find(|(start, end, grace)| t_ms >= start - START_SLACK_MS && t_ms <= end + grace)
            .map(|(start, end, _)| (*start, *end))
    }

    fn overlaps(&self, from_ms: i64, to_ms: i64) -> bool {
        self.open_since.is_some()
            || self
                .closed
                .iter()
                .any(|(start, end, grace)| *start <= to_ms && end + grace >= from_ms)
    }
}

pub struct ProvenanceRecorder {
    workspace: WorkspaceHost,
    transcript: TranscriptReader,
    activity: Mutex<HashMap<String, Activity>>,
    /// Test-visible work counters: commit walks and patch-id runs.
    walks: std::sync::atomic::AtomicU64,
    patch_id_runs: std::sync::atomic::AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ProvenanceRecorder {
    pub fn new(workspace: WorkspaceHost, transcript: TranscriptReader) -> Arc<Self> {
        Arc::new(Self {
            workspace,
            transcript,
            activity: Mutex::new(HashMap::new()),
            walks: Default::default(),
            patch_id_runs: Default::default(),
        })
    }

    /// Feed the sessions watch: remembers when each chat started or stopped
    /// Working. Cheap: a map compare per emission, no I/O.
    pub fn observe_sessions(&self, sessions: &[Session], now_ms: i64) {
        let mut activity = lock(&self.activity);
        for session in sessions {
            let working = session.status == SessionStatus::Working;
            let entry = activity.entry(session.chat_id.clone()).or_default();
            match (working, entry.open_since) {
                (true, None) => entry.open_since = Some(now_ms),
                (false, Some(start)) => {
                    entry.open_since = None;
                    let grace = if session.status == SessionStatus::AwaitingInput { 0 } else { ACTIVE_GRACE_MS };
                    entry.closed.push_back((start, now_ms, grace));
                    while entry.closed.len() > MAX_INTERVALS {
                        entry.closed.pop_front();
                    }
                }
                _ => {}
            }
        }
    }

    #[doc(hidden)]
    pub fn work_counters_for_test(&self) -> (u64, u64) {
        (
            self.walks.load(std::sync::atomic::Ordering::Relaxed),
            self.patch_id_runs.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    #[doc(hidden)]
    pub fn note_working_for_test(&self, chat_id: &str, start_ms: i64, end_ms: Option<i64>) {
        let mut activity = lock(&self.activity);
        let entry = activity.entry(chat_id.to_owned()).or_default();
        match end_ms {
            Some(end) => entry.closed.push_back((start_ms, end, ACTIVE_GRACE_MS)),
            None => entry.open_since = Some(start_ms),
        }
    }

    /// Spawn-and-forget entry used by the diff-sync listener.
    pub fn handle_in_background(self: &Arc<Self>, head_move: HeadMove) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(error) = this.handle(head_move).await {
                tracing::debug!(%error, "provenance: head move not recorded");
            }
        });
    }

    /// Record and remap for one HEAD move. Returns how many records were new.
    ///
    /// A commit is attributed to a chat only when all hold: it is first-parent
    /// on the new HEAD's own line; it is not reachable from any remote-tracking
    /// ref or other local branch (pulls, merges of main and rebases onto main
    /// bring nothing in); its committer time falls in one Working interval of
    /// exactly one bound chat (two chats working at once is "ambiguous" and
    /// attributes nothing). Rewritten commits are remapped by patch-id.
    pub async fn handle(&self, head_move: HeadMove) -> Result<usize, String> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let root = head_move.root.as_path();
        let removed = rev_list(root, &[&head_move.old, "--not", &head_move.new]).await?;
        let removed_records: Vec<CommitProvenance> = removed
            .iter()
            .filter_map(|sha| self.workspace.commit_provenance(sha))
            .collect();
        let active = self.any_active(&head_move.chats, head_move.since_ms, now_ms);
        // Nothing to attribute and nothing to remap: no further git.
        if !active && removed_records.is_empty() {
            return Ok(0);
        }
        self.walks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tips = if head_move.prior_tips.is_empty() {
            // No snapshot yet (first move since the checkout was bound): read
            // the refs now, minus the tip HEAD itself just moved to.
            let mut tips = read_ref_tips(&head_move.git_dir);
            tips.retain(|tip| tip != &head_move.new);
            tips
        } else {
            head_move.prior_tips.clone()
        };
        let added = own_commits(root, &head_move.old, &head_move.new, &tips).await?;
        if added.is_empty() {
            return Ok(0);
        }
        // Patch-ids only when commits were removed (rebase, amend, reset): an
        // added commit matching ANY removed commit is a rewrite, never fresh
        // work. It inherits the removed commit's record, or stays unattributed.
        let mut by_patch: HashMap<String, Option<CommitProvenance>> = HashMap::new();
        let mut added_ids: HashMap<String, String> = HashMap::new();
        if !removed.is_empty() {
            self.patch_id_runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let removed_ids = patch_ids(root, &removed).await;
            for sha in &removed {
                let record = removed_records.iter().find(|r| &r.sha == sha).cloned();
                let id = removed_ids
                    .get(sha)
                    .cloned()
                    .or_else(|| record.as_ref().map(|r| r.patch_id.clone()))
                    .unwrap_or_default();
                if !id.is_empty() {
                    by_patch.insert(id, record);
                }
            }
            let shas: Vec<String> = added.iter().map(|c| c.sha.clone()).collect();
            added_ids = patch_ids(root, &shas).await;
        }
        let mut entries_by_chat: HashMap<String, Option<Arc<Vec<SessionMessageEntry>>>> = HashMap::new();
        let mut records = Vec::new();
        let repo_id = repo_id(root).await.unwrap_or_default();
        for commit in &added {
            let patch_id = added_ids.get(&commit.sha).cloned().unwrap_or_default();
            if let Some(source) = (!patch_id.is_empty()).then(|| by_patch.get(&patch_id)).flatten() {
                let Some(source) = source else { continue }; // rewrite of an unrecorded commit
                records.push(CommitProvenance {
                    sha: commit.sha.clone(),
                    repo_id: repo_id.clone(),
                    patch_id,
                    chat_id: source.chat_id.clone(),
                    turn_start: source.turn_start.clone(),
                    turn_end: source.turn_end.clone(),
                    recorded_at: now_ms,
                    source: ProvenanceSource::Remapped,
                    remapped_from: Some(source.sha.clone()),
                });
                continue;
            }
            let Some((chat, start_ms, end_ms)) = self.sole_working_chat(&head_move.chats, commit.committed_ms, now_ms)
            else {
                continue;
            };
            if !entries_by_chat.contains_key(&chat) {
                let reader = self.transcript.clone();
                let chat_id = chat.clone();
                let entries = tokio::task::spawn_blocking(move || reader(&chat_id))
                    .await
                    .ok()
                    .flatten()
                    .map(Arc::new);
                entries_by_chat.insert(chat.clone(), entries);
            }
            let (turn_start, turn_end) = match entries_by_chat.get(&chat).and_then(Option::as_ref) {
                Some(entries) => turn_range(entries, start_ms, end_ms),
                None => (None, None),
            };
            records.push(CommitProvenance {
                sha: commit.sha.clone(),
                repo_id: repo_id.clone(),
                patch_id,
                chat_id: chat,
                turn_start,
                turn_end,
                recorded_at: now_ms,
                source: ProvenanceSource::Recorded,
                remapped_from: None,
            });
        }
        if records.is_empty() {
            return Ok(0);
        }
        self.workspace
            .put_commit_provenance_batch(&records, &repo_id)
            .map_err(|e| e.to_string())
    }

    /// Whether any bound chat was (or just stopped) Working since `since_ms`.
    fn any_active(&self, chats: &[String], since_ms: i64, now_ms: i64) -> bool {
        let activity = lock(&self.activity);
        chats
            .iter()
            .any(|chat| activity.get(chat).is_some_and(|a| a.overlaps(since_ms, now_ms)))
    }

    /// The single bound chat Working at `t_ms`, with its interval. `None` when
    /// no chat was, or when several were (ambiguous).
    fn sole_working_chat(&self, chats: &[String], t_ms: i64, now_ms: i64) -> Option<(String, i64, i64)> {
        let activity = lock(&self.activity);
        let mut found = chats
            .iter()
            .filter_map(|chat| {
                let (start, end) = activity.get(chat)?.interval_at(t_ms, now_ms)?;
                Some((chat.clone(), start, end))
            })
            .collect::<Vec<_>>();
        (found.len() == 1).then(|| found.remove(0))
    }
}

/// First and last user-message ids (turns) of a Working interval
/// `[from_ms, to_ms]`: the turn running at `from_ms` (or the first after it)
/// through the last turn started by `to_ms`, capped at [`MAX_TURN_SPAN`] turns.
pub(crate) fn turn_range(
    entries: &[SessionMessageEntry],
    from_ms: i64,
    to_ms: i64,
) -> (Option<String>, Option<String>) {
    let users: Vec<&SessionMessageEntry> = entries
        .iter()
        .filter(|entry| entry.role == MessageRole::User)
        .collect();
    let Some(end) = users.iter().rposition(|entry| entry.created_at <= to_ms) else {
        return (None, None);
    };
    let start = users[..=end]
        .iter()
        .rposition(|entry| entry.created_at <= from_ms + START_SLACK_MS)
        .unwrap_or(0)
        .max(end.saturating_sub(MAX_TURN_SPAN - 1));
    (Some(users[start].id.clone()), Some(users[end].id.clone()))
}

/// The latest turn in `[turn_start, turn_end]` whose typed edit calls touch
/// `path` (workspace-relative). Turns are user-message ids; the scan ends with
/// the `turn_end` turn. `None` when no edit call matches or the range ids are
/// not in `entries`.
pub(crate) fn turn_for_path(
    entries: &[SessionMessageEntry],
    turn_start: Option<&str>,
    turn_end: Option<&str>,
    path: &str,
) -> Option<String> {
    let start = entries.iter().position(|e| Some(e.id.as_str()) == turn_start)?;
    let end_user = entries.iter().position(|e| Some(e.id.as_str()) == turn_end)?;
    if end_user < start {
        return None;
    }
    let mut turn: Option<&str> = None;
    let mut found = None;
    for (offset, entry) in entries[start..].iter().enumerate() {
        if entry.role == MessageRole::User {
            if start + offset > end_user {
                break; // the next turn after the range
            }
            turn = Some(entry.id.as_str());
        }
        for part in &entry.parts {
            let MessagePart::Tool { call, .. } = part else { continue };
            let edited = match call {
                ToolCall::EditFile { path, .. } | ToolCall::WriteFile { path, .. } => Some(path),
                ToolCall::ApplyPatch { path } => path.as_ref(),
                _ => None,
            };
            if edited.is_some_and(|edited| path_matches(edited, path)) {
                found = turn.map(str::to_owned);
            }
        }
    }
    found
}

fn path_matches(edited: &str, rel: &str) -> bool {
    edited == rel || edited.strip_suffix(rel).is_some_and(|rest| rest.ends_with('/'))
}

// ── git ─────────────────────────────────────────────────────────────────────

async fn git_bytes(root: &Path, args: &[&str], max: usize) -> Result<Vec<u8>, String> {
    let mut child = mast_harness::git_command_async()
        .current_dir(root)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let result = tokio::time::timeout(GIT_TIMEOUT, async {
        let mut bytes = Vec::new();
        let mut stdout = child.stdout.take().ok_or("no stdout")?.take(max as u64 + 1);
        stdout.read_to_end(&mut bytes).await.map_err(|e| e.to_string())?;
        if bytes.len() > max {
            let _ = child.kill().await;
            return Err("git output exceeds the limit".to_string());
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        status.success().then_some(bytes).ok_or_else(|| "git failed".to_string())
    })
    .await
    .map_err(|_| "git timed out".to_string())?;
    result
}

/// Commits of `rev-list --max-count=200 <rev_args>`, oldest first.
async fn rev_list(root: &Path, rev_args: &[&str]) -> Result<Vec<String>, String> {
    let max = format!("--max-count={MAX_PROVENANCE_WALK}");
    let mut args = vec!["rev-list", "--reverse", max.as_str()];
    args.extend_from_slice(rev_args);
    let out = git_bytes(root, &args, 1024 * 1024).await?;
    // `--reverse` applies after `--max-count`, so the 200 newest, oldest first.
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter(|l| matches!(l.len(), 40 | 64))
        .map(str::to_owned)
        .collect())
}

/// A commit on the new HEAD's own line with its committer time.
#[derive(Debug, Clone)]
struct OwnCommit {
    sha: String,
    committed_ms: i64,
}

/// Commits new in `old..new`, first-parent only, not reachable from `tips`
/// (the branch and remote-tracking ref tips before the move), oldest first, at
/// most [`MAX_PROVENANCE_WALK`]. The tips go in on stdin, so any number is fine.
async fn own_commits(root: &Path, old: &str, new: &str, tips: &[String]) -> Result<Vec<OwnCommit>, String> {
    let max = format!("--max-count={MAX_PROVENANCE_WALK}");
    let mut stdin = format!("{new}\n^{old}\n");
    for tip in tips.iter().take(MAX_TIPS).filter(|t| matches!(t.len(), 40 | 64)) {
        stdin.push('^');
        stdin.push_str(tip);
        stdin.push('\n');
    }
    let args = ["log", "--first-parent", "--reverse", "--no-show-signature", "--format=%H %ct %P", max.as_str(), "--stdin"];
    let out = git_with_stdin(root, &args, stdin.as_bytes(), 1024 * 1024).await?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let sha = fields.next()?;
            matches!(sha.len(), 40 | 64).then_some(())?;
            let time: i64 = fields.next()?.parse().ok()?;
            // A merge commit carries no authored change: never attributed.
            (fields.count() <= 1).then_some(())?;
            Some(OwnCommit { sha: sha.to_owned(), committed_ms: time * 1000 })
        })
        .collect())
}

/// Most ref tips handed to one walk.
const MAX_TIPS: usize = 10_000;
/// Most ref files read when snapshotting tips.
const MAX_TIP_FILES: usize = 10_000;

/// Resolve a worktree git dir to the repository's common dir.
pub(crate) fn common_dir_of(git_dir: &Path) -> PathBuf {
    std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|rel| git_dir.join(rel.trim()))
        .and_then(|path| path.canonicalize().ok())
        .unwrap_or_else(|| git_dir.to_path_buf())
}

/// Directories walked by [`refs_stamp`].
const MAX_REF_DIRS: usize = 512;

/// Cheap fingerprint of the branch and remote-tracking refs from file metadata
/// (no spawn, no ref contents): `packed-refs` mtime and size plus the mtime of
/// every directory under `refs/heads` and `refs/remotes` (nested branches like
/// `feature/x` included) and of each loose ref file, bounded to
/// [`MAX_REF_DIRS`] directories. Creating, moving or deleting a ref rewrites
/// its file in that directory, so the stamp changes. `common_dir` is the
/// repository's common git dir.
pub(crate) fn refs_stamp(common_dir: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mtime = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos())
    };
    let packed = common_dir.join("packed-refs");
    mtime(&packed).hash(&mut hasher);
    std::fs::metadata(&packed).map_or(0, |m| m.len()).hash(&mut hasher);
    let mut stack = vec![common_dir.join("refs/heads"), common_dir.join("refs/remotes")];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        seen += 1;
        if seen > MAX_REF_DIRS {
            break;
        }
        mtime(&dir).hash(&mut hasher);
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        let mut children: Vec<_> = read.flatten().collect();
        children.sort_by_key(|e| e.file_name());
        for child in children {
            if child.file_type().is_ok_and(|kind| kind.is_dir()) {
                stack.push(child.path());
            } else {
                child.file_name().hash(&mut hasher);
                mtime(&child.path()).hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Ref tips cached under [`refs_stamp`]: re-read only when the stamp changes,
/// so same-HEAD captures (every agent save) cost a handful of stats.
pub(crate) struct TipsCache {
    stamp: u64,
    tips: Arc<Vec<String>>,
    /// How many times the refs were actually read (tests).
    pub(crate) reads: u64,
}

impl TipsCache {
    /// The tips in `slot` if still current, else re-read and cache them.
    pub(crate) fn refresh(slot: &mut Option<TipsCache>, git_dir: &Path) -> Arc<Vec<String>> {
        let stamp = refs_stamp(&common_dir_of(git_dir));
        if let Some(cache) = slot.as_ref()
            && cache.stamp == stamp
        {
            return cache.tips.clone();
        }
        let reads = slot.as_ref().map_or(0, |cache| cache.reads) + 1;
        let tips = Arc::new(read_ref_tips(git_dir));
        *slot = Some(TipsCache { stamp, tips: tips.clone(), reads });
        tips
    }

    pub(crate) fn current(slot: &Option<TipsCache>) -> Option<Arc<Vec<String>>> {
        slot.as_ref().map(|cache| cache.tips.clone())
    }
}

/// Tips of every branch and remote-tracking ref of the repository whose git
/// dir is `git_dir`, read from the files (packed-refs plus loose refs): no
/// spawn. Bounded; symbolic refs and unreadable files are skipped.
pub(crate) fn read_ref_tips(git_dir: &Path) -> Vec<String> {
    let common = common_dir_of(git_dir);
    let mut tips: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(packed) = std::fs::read_to_string(common.join("packed-refs")) {
        for line in packed.lines().take(MAX_TIP_FILES) {
            let mut parts = line.split(' ');
            if let (Some(sha), Some(name)) = (parts.next(), parts.next())
                && (name.starts_with("refs/heads/") || name.starts_with("refs/remotes/"))
                && matches!(sha.len(), 40 | 64)
            {
                tips.insert(sha.to_owned());
            }
        }
    }
    let mut stack = vec![(common.join("refs/heads"), 0usize), (common.join("refs/remotes"), 0usize)];
    let mut files = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        for entry in read.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() && depth < 8 => stack.push((path, depth + 1)),
                Ok(kind) if kind.is_file() => {
                    files += 1;
                    if files > MAX_TIP_FILES {
                        return tips.into_iter().collect();
                    }
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        let sha = text.trim();
                        if matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                            tips.insert(sha.to_owned());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    tips.into_iter().collect()
}

async fn git_with_stdin(root: &Path, args: &[&str], input: &[u8], max: usize) -> Result<Vec<u8>, String> {
    let mut child = mast_harness::git_command_async()
        .current_dir(root)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let input = input.to_vec();
    let write = tokio::spawn(async move {
        let _ = stdin.write_all(&input).await;
    });
    let result = tokio::time::timeout(GIT_TIMEOUT, child.wait_with_output()).await;
    let _ = write.await;
    let out = result.map_err(|_| "git timed out".to_string())?.map_err(|e| e.to_string())?;
    if !out.status.success() || out.stdout.len() > max {
        return Err("git failed or output too large".into());
    }
    Ok(out.stdout)
}

/// Stable id of the repository (hash of its git common dir).
async fn repo_id(root: &Path) -> Option<String> {
    let out = git_bytes(root, &["rev-parse", "--path-format=absolute", "--git-common-dir"], 4096).await.ok()?;
    let dir = String::from_utf8_lossy(&out).trim().to_owned();
    (!dir.is_empty()).then(|| mast_proto::derive_id(&dir, "provenance-repo"))
}

/// sha → `git patch-id --stable` for `shas`. Commits without a diff (merges,
/// empty) are absent; an over-large patch stream yields an empty map.
async fn patch_ids(root: &Path, shas: &[String]) -> HashMap<String, String> {
    let mut args = vec!["log", "-p", "--no-color", "--no-ext-diff", "--no-show-signature", "--no-walk=unsorted"];
    args.extend(shas.iter().map(String::as_str));
    let Ok(patch) = git_bytes(root, &args, MAX_PATCH_BYTES).await else {
        return HashMap::new();
    };
    let Ok(mut child) = mast_harness::git_command_async()
        .current_dir(root)
        .args(["patch-id", "--stable"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
    else {
        return HashMap::new();
    };
    let Some(mut stdin) = child.stdin.take() else { return HashMap::new() };
    let write = tokio::spawn(async move {
        let _ = stdin.write_all(&patch).await;
    });
    let out = tokio::time::timeout(GIT_TIMEOUT, child.wait_with_output()).await;
    let _ = write.await;
    let Ok(Ok(out)) = out else { return HashMap::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (patch, sha) = line.split_once(' ')?;
            Some((sha.trim().to_owned(), patch.to_owned()))
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // test repos are built with plain `git`
mod tests {
    use super::*;
    use crate::workspace_host::WorkspaceHostConfig;
    use mast_sync::DocsStore;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@t.invalid", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        repo: PathBuf,
        recorder: Arc<ProvenanceRecorder>,
        workspace: WorkspaceHost,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(&dir.path().join("store")).unwrap());
        let workspace = WorkspaceHost::open(
            store,
            WorkspaceHostConfig {
                device_id: "device".into(),
                device_name: "test".into(),
                platform: "test".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
            },
        )
        .unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        let recorder = ProvenanceRecorder::new(workspace.clone(), Arc::new(|_| None));
        Fixture { _dir: dir, repo, recorder, workspace }
    }

    fn head(repo: &Path) -> String {
        git(repo, &["rev-parse", "HEAD"])
    }

    fn commit_file(repo: &Path, file: &str, text: &str, message: &str) -> String {
        std::fs::write(repo.join(file), text).unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", message]);
        head(repo)
    }

    /// What diff-sync holds just before a move: HEAD and every ref tip.
    struct Before {
        old: String,
        tips: Vec<String>,
    }

    fn before(f: &Fixture) -> Before {
        let tips = git(&f.repo, &["for-each-ref", "--format=%(objectname)", "refs/heads", "refs/remotes"])
            .lines()
            .map(str::to_owned)
            .collect();
        Before { old: head(&f.repo), tips }
    }

    fn mv(f: &Fixture, before: &Before, chats: &[&str]) -> HeadMove {
        HeadMove {
            root: f.repo.clone(),
            git_dir: f.repo.join(".git"),
            old: before.old.clone(),
            new: head(&f.repo),
            chats: chats.iter().map(|c| (*c).to_owned()).collect(),
            since_ms: now() - 3_600_000,
            prior_tips: before.tips.clone(),
        }
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    /// `chat` has been Working for the last 100 seconds.
    fn working(f: &Fixture, chat: &str) {
        f.recorder.note_working_for_test(chat, now() - 100_000, None);
    }

    /// `chat` worked in a closed interval that ended long ago.
    fn worked_long_ago(f: &Fixture, chat: &str) {
        f.recorder.note_working_for_test(chat, now() - 7_200_000, Some(now() - 3_600_000));
    }

    fn fresh_recorder(f: Fixture) -> Fixture {
        Fixture { recorder: ProvenanceRecorder::new(f.workspace.clone(), Arc::new(|_| None)), ..f }
    }

    #[tokio::test]
    async fn records_a_chats_own_commit() {
        let f = fixture();
        let b = before(&f);
        working(&f, "chat-1");
        let sha = commit_file(&f.repo, "b.txt", "b\n", "add b");
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 1);
        let record = f.workspace.commit_provenance(&sha).unwrap();
        assert_eq!(record.chat_id, "chat-1");
        assert_eq!(record.source, ProvenanceSource::Recorded);
        assert!(!record.repo_id.is_empty());
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_push_or_new_branch_right_after_the_commit_cannot_hide_it() {
        let f = fixture();
        let b = before(&f);
        working(&f, "chat-1");
        let sha = commit_file(&f.repo, "b.txt", "b\n", "add b");
        // Pushed (remote-tracking ref) and branched off before the recorder runs.
        git(&f.repo, &["update-ref", "refs/remotes/origin/main", &sha]);
        git(&f.repo, &["branch", "coordinator-branch", &sha]);
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 1);
        assert_eq!(f.workspace.commit_provenance(&sha).unwrap().chat_id, "chat-1");
    }

    #[tokio::test]
    async fn nothing_for_idle_unbound_ambiguous_or_awaiting_input() {
        let f = fixture();
        let b = before(&f);
        let sha = commit_file(&f.repo, "b.txt", "b\n", "add b");
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 0);
        worked_long_ago(&f, "chat-1");
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 0);
        working(&f, "chat-1");
        assert_eq!(f.recorder.handle(mv(&f, &b, &[])).await.unwrap(), 0);
        working(&f, "chat-2");
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1", "chat-2"])).await.unwrap(), 0);
        assert!(f.workspace.commit_provenance(&sha).is_none());
    }

    #[tokio::test]
    async fn fast_forward_pull_and_merge_of_origin_main_attribute_nothing() {
        let f = fixture();
        git(&f.repo, &["checkout", "-q", "-b", "feature"]);
        git(&f.repo, &["checkout", "-q", "-b", "upstream", "main"]);
        commit_file(&f.repo, "u1.txt", "1\n", "upstream 1");
        let upstream = commit_file(&f.repo, "u2.txt", "2\n", "upstream 2");
        git(&f.repo, &["checkout", "-q", "feature"]);
        git(&f.repo, &["branch", "-q", "-D", "upstream"]);
        git(&f.repo, &["update-ref", "refs/remotes/origin/main", &upstream]);
        working(&f, "chat-1");
        let b = before(&f);
        git(&f.repo, &["merge", "-q", "--ff-only", "origin/main"]);
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 0);
        git(&f.repo, &["reset", "-q", "--hard", "main"]);
        let own = commit_file(&f.repo, "own.txt", "x\n", "own");
        let b = before(&f);
        git(&f.repo, &["merge", "-q", "--no-edit", "origin/main"]);
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 0);
        assert!(f.workspace.commit_provenance(&own).is_none());
    }

    #[tokio::test]
    async fn rebase_onto_main_brings_nothing_in_and_remaps_own_commits() {
        let f = fixture();
        git(&f.repo, &["checkout", "-q", "-b", "feature"]);
        let b = before(&f);
        working(&f, "chat-1");
        let c1 = commit_file(&f.repo, "b.txt", "b\n", "add b");
        let c2 = commit_file(&f.repo, "c.txt", "c\n", "add c");
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 2);
        let f2 = fresh_recorder(f);
        worked_long_ago(&f2, "chat-1");
        git(&f2.repo, &["checkout", "-q", "main"]);
        let main_commit = commit_file(&f2.repo, "m.txt", "m\n", "main moves");
        git(&f2.repo, &["checkout", "-q", "feature"]);
        let b = before(&f2);
        git(&f2.repo, &["rebase", "-q", "main"]);
        let new = head(&f2.repo);
        assert_ne!(new, c2);
        assert_eq!(f2.recorder.handle(mv(&f2, &b, &["chat-1"])).await.unwrap(), 2);
        let tip = f2.workspace.commit_provenance(&new).unwrap();
        assert_eq!(tip.source, ProvenanceSource::Remapped);
        assert_eq!(tip.remapped_from.as_deref(), Some(c2.as_str()));
        assert!(f2.workspace.commit_provenance(&main_commit).is_none(), "main's commit is not attributed");
        assert!(f2.workspace.commit_provenance(&c1).is_some());
        let b = before(&f2);
        git(&f2.repo, &["reset", "-q", "--hard", &c2]);
        assert_eq!(f2.recorder.handle(mv(&f2, &b, &["chat-1"])).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_human_commit_rebased_while_a_chat_works_is_not_the_chats() {
        let f = fixture();
        git(&f.repo, &["checkout", "-q", "-b", "feature"]);
        let human = commit_file(&f.repo, "h.txt", "h\n", "human commit");
        git(&f.repo, &["checkout", "-q", "main"]);
        commit_file(&f.repo, "m.txt", "m\n", "main moves");
        git(&f.repo, &["checkout", "-q", "feature"]);
        working(&f, "chat-1");
        let b = before(&f);
        git(&f.repo, &["rebase", "-q", "main"]);
        let rebased = head(&f.repo);
        assert_ne!(rebased, human);
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 0);
        assert!(f.workspace.commit_provenance(&rebased).is_none());
    }

    #[tokio::test]
    async fn rebase_with_no_chat_working_and_no_records_attributes_nothing() {
        let f = fixture();
        git(&f.repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&f.repo, "b.txt", "b\n", "add b");
        git(&f.repo, &["checkout", "-q", "main"]);
        commit_file(&f.repo, "m.txt", "m\n", "main moves");
        git(&f.repo, &["checkout", "-q", "feature"]);
        let b = before(&f);
        git(&f.repo, &["rebase", "-q", "main"]);
        worked_long_ago(&f, "chat-1");
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn amend_remaps_by_patch_id() {
        let f = fixture();
        let b = before(&f);
        working(&f, "chat-1");
        let first = commit_file(&f.repo, "b.txt", "b\n", "add b");
        f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap();
        let f = fresh_recorder(f);
        worked_long_ago(&f, "chat-1");
        let b = before(&f);
        git(&f.repo, &["commit", "-q", "--amend", "-m", "add b (reworded)"]);
        let amended = head(&f.repo);
        assert_ne!(first, amended);
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 1);
        let record = f.workspace.commit_provenance(&amended).unwrap();
        assert_eq!(record.source, ProvenanceSource::Remapped);
        assert_eq!(record.remapped_from.as_deref(), Some(first.as_str()));
    }

    #[tokio::test]
    async fn a_coordinator_merging_worker_branches_keeps_their_attribution() {
        let f = fixture();
        let base = head(&f.repo);
        git(&f.repo, &["checkout", "-q", "-b", "worker-1"]);
        let w = commit_file(&f.repo, "w.txt", "w\n", "worker change");
        f.workspace
            .put_commit_provenance(&mast_proto::CommitProvenance {
                sha: w.clone(), repo_id: "r".into(), patch_id: String::new(), chat_id: "worker-chat".into(),
                turn_start: None, turn_end: None, recorded_at: 1,
                source: ProvenanceSource::Recorded, remapped_from: None,
            })
            .unwrap();
        git(&f.repo, &["checkout", "-q", "-b", "coordinator", &base]);
        working(&f, "coordinator-chat");
        let b = before(&f);
        git(&f.repo, &["merge", "-q", "--ff-only", "worker-1"]);
        assert_eq!(f.recorder.handle(mv(&f, &b, &["coordinator-chat"])).await.unwrap(), 0);
        assert_eq!(f.workspace.commit_provenance(&w).unwrap().chat_id, "worker-chat");
    }

    fn entry(id: &str, role: MessageRole, at: i64, parts: serde_json::Value) -> SessionMessageEntry {
        serde_json::from_value(serde_json::json!({
            "id": id, "role": match role { MessageRole::User => "user", _ => "assistant" },
            "parts": parts, "createdAt": at, "deviceId": "d"
        }))
        .unwrap()
    }

    fn edit(path: &str) -> serde_json::Value {
        serde_json::json!([{"kind": "tool", "id": "p", "call": {"kind": "editFile", "path": path}, "resolved": true}])
    }

    #[test]
    fn turn_range_and_edit_turn_lookup() {
        let entries = vec![
            entry("u1", MessageRole::User, 1_000_000, serde_json::json!([])),
            entry("a1", MessageRole::Assistant, 1_100_000, edit("/w/repo/src/old.rs")),
            entry("u2", MessageRole::User, 2_000_000, serde_json::json!([])),
            entry("a2", MessageRole::Assistant, 2_100_000, edit("/w/repo/src/a.rs")),
            entry("u3", MessageRole::User, 3_000_000, serde_json::json!([])),
            entry("a3", MessageRole::Assistant, 3_100_000, edit("/w/repo/src/later.rs")),
        ];
        assert_eq!(turn_range(&entries, 2_500_000, 4_000_000), (Some("u2".into()), Some("u3".into())));
        assert_eq!(turn_range(&entries, 500_000, 4_000_000), (Some("u1".into()), Some("u3".into())));
        assert_eq!(turn_range(&entries, 500_000, 2_500_000), (Some("u1".into()), Some("u2".into())));
        assert_eq!(turn_range(&[], 0, 10), (None, None));
        assert_eq!(turn_for_path(&entries, Some("u2"), Some("u3"), "src/a.rs"), Some("u2".into()));
        assert_eq!(turn_for_path(&entries, Some("u2"), Some("u3"), "src/old.rs"), None);
        assert_eq!(turn_for_path(&entries, Some("u1"), Some("u3"), "src/old.rs"), Some("u1".into()));
        assert_eq!(turn_for_path(&entries, Some("gone"), Some("u3"), "src/a.rs"), None);
        assert_eq!(turn_for_path(&entries, Some("u1"), Some("u2"), "src/later.rs"), None);
        assert_eq!(turn_for_path(&entries, Some("u1"), Some("u3"), "src/later.rs"), Some("u3".into()));
    }

    #[test]
    fn activity_intervals_follow_working_only() {
        let mut a = Activity { open_since: Some(1_000), ..Default::default() };
        assert_eq!(a.interval_at(1_500, 2_000), Some((1_000, 2_000)));
        assert_eq!(a.interval_at(-5_000, 2_000), None);
        a.open_since = None;
        a.closed.push_back((1_000, 2_000, ACTIVE_GRACE_MS));
        assert!(a.interval_at(2_000 + ACTIVE_GRACE_MS, 9_999_999).is_some());
        assert!(a.interval_at(2_000 + ACTIVE_GRACE_MS + 1, 9_999_999).is_none());
    }

    fn session(chat: &str, status: &str) -> Session {
        serde_json::from_value(serde_json::json!({
            "chatId": chat, "deviceId": "d", "status": status,
            "startedAt": null, "updatedAt": "2026-01-01T00:00:00Z"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn grace_after_a_turn_ends_idle_but_not_after_it_parks_on_a_question() {
        let f = fixture();
        f.recorder.observe_sessions(&[session("idle-chat", "working"), session("asking-chat", "working")], 1_000);
        f.recorder.observe_sessions(&[session("idle-chat", "idle"), session("asking-chat", "awaitingInput")], 10_000);
        // 5 s after the turn ended idle: still the chat's. 5 s after it parked
        // on a question: a human's.
        assert!(f.recorder.sole_working_chat(&["idle-chat".into()], 15_000, 20_000).is_some());
        assert!(f.recorder.sole_working_chat(&["asking-chat".into()], 15_000, 20_000).is_none());
        // Past the 15 s grace even the idle chat no longer claims it.
        assert!(f.recorder.sole_working_chat(&["idle-chat".into()], 10_000 + ACTIVE_GRACE_MS + 1, 99_000).is_none());
        // While it was Working, both did.
        assert!(f.recorder.sole_working_chat(&["asking-chat".into()], 5_000, 20_000).is_some());
    }

    #[tokio::test]
    async fn idle_moves_do_no_walk_and_patch_ids_only_run_when_commits_were_removed() {
        let f = fixture();
        let b = before(&f);
        commit_file(&f.repo, "b.txt", "b\n", "human commit");
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 0);
        assert_eq!(f.recorder.work_counters_for_test(), (0, 0));
        let f = fixture();
        let b = before(&f);
        working(&f, "chat-1");
        commit_file(&f.repo, "b.txt", "b\n", "chat commit");
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 1);
        assert_eq!(f.recorder.work_counters_for_test(), (1, 0));
        let recorded = head(&f.repo);
        let b = before(&f);
        git(&f.repo, &["commit", "-q", "--amend", "-m", "reworded"]);
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), 1);
        assert_eq!(f.recorder.work_counters_for_test(), (2, 1));
        assert_ne!(recorded, head(&f.repo));
    }

    #[tokio::test]
    async fn one_move_records_at_most_the_newest_200_commits() {
        let f = fixture();
        let b = before(&f);
        working(&f, "chat-1");
        let mut shas = Vec::new();
        for i in 0..205 {
            shas.push(commit_file(&f.repo, "many.txt", &format!("{i}\n"), &format!("c{i}")));
        }
        assert_eq!(f.recorder.handle(mv(&f, &b, &["chat-1"])).await.unwrap(), MAX_PROVENANCE_WALK);
        assert!(f.workspace.commit_provenance(&shas[0]).is_none(), "oldest are beyond the cap");
        assert!(f.workspace.commit_provenance(&shas[204]).is_some());
    }

    #[tokio::test]
    async fn ref_tips_are_read_from_files_including_packed_and_nested_refs() {
        let f = fixture();
        git(&f.repo, &["branch", "feature/nested", "HEAD"]);
        let main = head(&f.repo);
        git(&f.repo, &["update-ref", "refs/remotes/origin/main", &main]);
        let second = commit_file(&f.repo, "x.txt", "x\n", "second");
        git(&f.repo, &["branch", "packed-branch", &second]);
        git(&f.repo, &["pack-refs", "--all"]);
        git(&f.repo, &["branch", "loose-branch", &second]);
        let tips = read_ref_tips(&f.repo.join(".git"));
        assert!(tips.contains(&main), "nested/remote tip");
        assert!(tips.contains(&second), "packed and loose tip");
    }

    #[tokio::test]
    async fn same_head_captures_with_unchanged_refs_do_not_reread_them() {
        let f = fixture();
        let git_dir = f.repo.join(".git");
        let mut slot: Option<TipsCache> = None;
        let first = TipsCache::refresh(&mut slot, &git_dir);
        for _ in 0..50 {
            // Fifty agent saves: files change, refs do not.
            std::fs::write(f.repo.join("work.txt"), "edit\n").unwrap();
            let again = TipsCache::refresh(&mut slot, &git_dir);
            assert!(Arc::ptr_eq(&first, &again));
        }
        assert_eq!(slot.as_ref().unwrap().reads, 1, "refs read once");
        // A ref change re-reads exactly once.
        std::thread::sleep(Duration::from_millis(20));
        git(&f.repo, &["branch", "new-branch", "HEAD"]);
        let after = TipsCache::refresh(&mut slot, &git_dir);
        assert_eq!(slot.as_ref().unwrap().reads, 2);
        assert!(after.contains(&head(&f.repo)));
        TipsCache::refresh(&mut slot, &git_dir);
        assert_eq!(slot.as_ref().unwrap().reads, 2);
        assert!(TipsCache::current(&slot).is_some());
    }

    #[test]
    fn the_refs_stamp_changes_for_nested_remote_and_packed_refs() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path();
        std::fs::create_dir_all(git.join("refs/heads/feature")).unwrap();
        std::fs::create_dir_all(git.join("refs/remotes/origin")).unwrap();
        let sha = "a".repeat(40);
        let base = refs_stamp(git);
        assert_eq!(base, refs_stamp(git), "stable while nothing moves");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(git.join("refs/heads/feature/x"), &sha).unwrap();
        let nested = refs_stamp(git);
        assert_ne!(base, nested, "nested branch ref");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(git.join("refs/remotes/origin/main"), &sha).unwrap();
        let remote = refs_stamp(git);
        assert_ne!(nested, remote, "remote-tracking ref");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(git.join("packed-refs"), format!("{sha} refs/heads/p\n")).unwrap();
        assert_ne!(remote, refs_stamp(git), "packed-refs");
    }
}
