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

/// A chat that finished a turn this recently still counts as active (a commit
/// often lands just after the last assistant message).
const ACTIVE_GRACE_MS: i64 = 60_000;
/// Output cap for the `git log -p` feeding `git patch-id`.
const MAX_PATCH_BYTES: usize = 8 * 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(20);
/// Most turns a record spans.
const MAX_TURN_SPAN: usize = 20;

/// One HEAD move of a checkout some chat has bound as a diff target.
#[derive(Debug, Clone)]
pub struct HeadMove {
    pub root: PathBuf,
    pub old: String,
    pub new: String,
    /// Chats that bound this checkout.
    pub chats: Vec<String>,
    /// Epoch ms diff-sync last saw `old` as HEAD.
    pub since_ms: i64,
}

pub type HeadMoveListener = Arc<dyn Fn(HeadMove) + Send + Sync>;
/// Reads one chat's transcript entries (best effort).
pub type TranscriptReader = Arc<dyn Fn(&str) -> Option<Vec<SessionMessageEntry>> + Send + Sync>;

#[derive(Debug, Clone, Copy, Default)]
struct Activity {
    working: bool,
    /// Epoch ms of the last transition into / out of working.
    changed_ms: i64,
}

pub struct ProvenanceRecorder {
    workspace: WorkspaceHost,
    transcript: TranscriptReader,
    activity: Mutex<HashMap<String, Activity>>,
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
        })
    }

    /// Feed the sessions watch: remembers when each chat started or stopped
    /// working. Cheap: a map compare per emission, no I/O.
    pub fn observe_sessions(&self, sessions: &[Session], now_ms: i64) {
        let mut activity = lock(&self.activity);
        for session in sessions {
            let working = matches!(
                session.status,
                SessionStatus::Working | SessionStatus::AwaitingInput
            );
            let entry = activity.entry(session.chat_id.clone()).or_default();
            if entry.working != working {
                *entry = Activity { working, changed_ms: now_ms };
            }
        }
    }

    #[doc(hidden)]
    pub fn note_activity_for_test(&self, chat_id: &str, working: bool, changed_ms: i64) {
        lock(&self.activity).insert(chat_id.to_owned(), Activity { working, changed_ms });
    }

    /// The bound chat that is working now or finished within the grace window;
    /// the most recently active wins.
    fn active_chat(&self, chats: &[String], now_ms: i64) -> Option<String> {
        let activity = lock(&self.activity);
        chats
            .iter()
            .filter_map(|chat| {
                let a = activity.get(chat)?;
                let recent = a.working || now_ms - a.changed_ms <= ACTIVE_GRACE_MS;
                recent.then(|| (if a.working { i64::MAX } else { a.changed_ms }, chat.clone()))
            })
            .max()
            .map(|(_, chat)| chat)
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

    /// Record and remap for one HEAD move. Returns how many records changed.
    pub async fn handle(&self, head_move: HeadMove) -> Result<usize, String> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let root = head_move.root.as_path();
        let added = rev_list(root, &[&head_move.new, "--not", &head_move.old]).await?;
        let removed = rev_list(root, &[&head_move.old, "--not", &head_move.new]).await?;
        if added.is_empty() {
            return Ok(0);
        }
        // Remap: removed commits that carry a record, matched by patch-id.
        let mut by_patch: HashMap<String, CommitProvenance> = HashMap::new();
        let removed_records: Vec<CommitProvenance> = removed
            .iter()
            .filter_map(|sha| self.workspace.commit_provenance(sha))
            .collect();
        if !removed_records.is_empty() {
            let ids = patch_ids(root, &["--no-walk=unsorted"], &removed).await;
            for record in removed_records {
                let id = ids.get(&record.sha).cloned().unwrap_or(record.patch_id.clone());
                if !id.is_empty() {
                    by_patch.insert(id, record);
                }
            }
        }
        let added_ids = patch_ids(root, &["--reverse", "--no-walk=unsorted"], &added).await;
        let active = self.active_chat(&head_move.chats, now_ms);
        let range = active
            .as_deref()
            .map(|chat| self.turn_range(chat, head_move.since_ms));
        let mut changed = 0;
        for sha in &added {
            let patch_id = added_ids.get(sha).cloned().unwrap_or_default();
            let record = if let Some(source) = (!patch_id.is_empty())
                .then(|| by_patch.get(&patch_id))
                .flatten()
            {
                CommitProvenance {
                    sha: sha.clone(),
                    patch_id,
                    chat_id: source.chat_id.clone(),
                    turn_start: source.turn_start.clone(),
                    turn_end: source.turn_end.clone(),
                    recorded_at: now_ms,
                    source: ProvenanceSource::Remapped,
                    remapped_from: Some(source.sha.clone()),
                }
            } else if let (Some(chat), Some((start, end))) = (&active, &range) {
                CommitProvenance {
                    sha: sha.clone(),
                    patch_id,
                    chat_id: chat.clone(),
                    turn_start: start.clone(),
                    turn_end: end.clone(),
                    recorded_at: now_ms,
                    source: ProvenanceSource::Recorded,
                    remapped_from: None,
                }
            } else {
                continue;
            };
            if self.workspace.put_commit_provenance(&record).map_err(|e| e.to_string())? {
                changed += 1;
            }
        }
        if changed > 0 {
            self.workspace.prune_commit_provenance();
        }
        Ok(changed)
    }

    fn turn_range(&self, chat_id: &str, since_ms: i64) -> (Option<String>, Option<String>) {
        match (self.transcript)(chat_id) {
            Some(entries) => turn_range(&entries, since_ms),
            None => (None, None),
        }
    }
}

/// First and last user-message ids (turns) spanning `since_ms` to now: the
/// turn running at `since_ms` (or the first after it) through the latest,
/// capped at [`MAX_TURN_SPAN`] turns.
pub(crate) fn turn_range(
    entries: &[SessionMessageEntry],
    since_ms: i64,
) -> (Option<String>, Option<String>) {
    let users: Vec<&SessionMessageEntry> = entries
        .iter()
        .filter(|entry| entry.role == MessageRole::User)
        .collect();
    let Some(last) = users.last() else { return (None, None) };
    let end = users.len() - 1;
    let start = users
        .iter()
        .rposition(|entry| entry.created_at <= since_ms)
        .or_else(|| users.iter().position(|entry| entry.created_at > since_ms))
        .unwrap_or(end)
        .max(end.saturating_sub(MAX_TURN_SPAN - 1));
    (Some(users[start].id.clone()), Some(last.id.clone()))
}

/// The latest turn in `[turn_start, turn_end]` whose typed edit calls touch
/// `path` (workspace-relative). `None` when no edit call matches or the range
/// ids are not in `entries`. Turns are user-message ids.
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
    for entry in &entries[start..] {
        if entry.role == MessageRole::User {
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

/// sha → `git patch-id --stable` for `shas`. Commits without a diff (merges,
/// empty) are absent; an over-large patch stream yields an empty map.
async fn patch_ids(root: &Path, log_flags: &[&str], shas: &[String]) -> HashMap<String, String> {
    let mut args = vec!["log", "-p", "--no-color", "--no-ext-diff", "--no-show-signature"];
    args.extend_from_slice(log_flags);
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

    fn mv(f: &Fixture, old: &str) -> HeadMove {
        HeadMove {
            root: f.repo.clone(),
            old: old.into(),
            new: head(&f.repo),
            chats: vec!["chat-1".into()],
            since_ms: 0,
        }
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    #[tokio::test]
    async fn records_commit_for_active_bound_chat() {
        let f = fixture();
        let old = head(&f.repo);
        f.recorder.note_activity_for_test("chat-1", true, now());
        let sha = commit_file(&f.repo, "b.txt", "b\n", "add b");
        assert_eq!(f.recorder.handle(mv(&f, &old)).await.unwrap(), 1);
        let record = f.workspace.commit_provenance(&sha).unwrap();
        assert_eq!(record.chat_id, "chat-1");
        assert_eq!(record.source, ProvenanceSource::Recorded);
        assert!(!record.patch_id.is_empty());
        // The same move again changes nothing.
        assert_eq!(f.recorder.handle(mv(&f, &old)).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn nothing_recorded_for_idle_or_unbound_chat() {
        let f = fixture();
        let old = head(&f.repo);
        let sha = commit_file(&f.repo, "b.txt", "b\n", "add b");
        // Never seen working.
        assert_eq!(f.recorder.handle(mv(&f, &old)).await.unwrap(), 0);
        // Finished long ago.
        f.recorder.note_activity_for_test("chat-1", false, now() - 10 * ACTIVE_GRACE_MS);
        assert_eq!(f.recorder.handle(mv(&f, &old)).await.unwrap(), 0);
        // Unbound worktree: diff-sync passes no chats.
        f.recorder.note_activity_for_test("chat-1", true, now());
        let mut unbound = mv(&f, &old);
        unbound.chats.clear();
        assert_eq!(f.recorder.handle(unbound).await.unwrap(), 0);
        assert!(f.workspace.commit_provenance(&sha).is_none());
    }

    #[tokio::test]
    async fn amend_remaps_by_patch_id() {
        let f = fixture();
        let base = head(&f.repo);
        f.recorder.note_activity_for_test("chat-1", true, now());
        let first = commit_file(&f.repo, "b.txt", "b\n", "add b");
        f.recorder.handle(mv(&f, &base)).await.unwrap();
        // The chat goes quiet; a message-only amend rewrites the sha.
        f.recorder.note_activity_for_test("chat-1", false, now() - 10 * ACTIVE_GRACE_MS);
        git(&f.repo, &["commit", "-q", "--amend", "-m", "add b (reworded)"]);
        let amended = head(&f.repo);
        assert_ne!(first, amended);
        let mut amend_move = mv(&f, &first);
        amend_move.old = first.clone();
        assert_eq!(f.recorder.handle(amend_move).await.unwrap(), 1);
        let record = f.workspace.commit_provenance(&amended).unwrap();
        assert_eq!(record.source, ProvenanceSource::Remapped);
        assert_eq!(record.remapped_from.as_deref(), Some(first.as_str()));
        assert_eq!(record.chat_id, "chat-1");
    }

    #[tokio::test]
    async fn rebase_and_reset_to_rewritten_history_remap() {
        let f = fixture();
        let base = head(&f.repo);
        git(&f.repo, &["checkout", "-q", "-b", "feature"]);
        f.recorder.note_activity_for_test("chat-1", true, now());
        let c1 = commit_file(&f.repo, "b.txt", "b\n", "add b");
        let c2 = commit_file(&f.repo, "c.txt", "c\n", "add c");
        f.recorder.handle(mv(&f, &base)).await.unwrap();
        f.recorder.note_activity_for_test("chat-1", false, now() - 10 * ACTIVE_GRACE_MS);
        // main moves on; feature is rebased onto it.
        git(&f.repo, &["checkout", "-q", "main"]);
        commit_file(&f.repo, "m.txt", "m\n", "main moves");
        git(&f.repo, &["checkout", "-q", "feature"]);
        git(&f.repo, &["rebase", "-q", "main"]);
        let new = head(&f.repo);
        assert_ne!(new, c2);
        assert_eq!(f.recorder.handle(mv(&f, &c2)).await.unwrap(), 2);
        let rebased_tip = f.workspace.commit_provenance(&new).unwrap();
        assert_eq!(rebased_tip.source, ProvenanceSource::Remapped);
        assert_eq!(rebased_tip.remapped_from.as_deref(), Some(c2.as_str()));
        assert!(f.workspace.commit_provenance(&c1).is_some());
        // Force-push shape: the branch is reset back and forth between the
        // histories; records already exist for both, so nothing is rewritten.
        git(&f.repo, &["reset", "-q", "--hard", &c2]);
        assert_eq!(f.recorder.handle(mv(&f, &new)).await.unwrap(), 0);
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
            entry("u1", MessageRole::User, 100, serde_json::json!([])),
            entry("a1", MessageRole::Assistant, 110, edit("/w/repo/src/old.rs")),
            entry("u2", MessageRole::User, 200, serde_json::json!([])),
            entry("a2", MessageRole::Assistant, 210, edit("/w/repo/src/a.rs")),
            entry("u3", MessageRole::User, 300, serde_json::json!([])),
            entry("a3", MessageRole::Assistant, 310, serde_json::json!([])),
        ];
        // `since` inside turn 2: it starts there and runs to the latest turn.
        assert_eq!(turn_range(&entries, 250), (Some("u2".into()), Some("u3".into())));
        assert_eq!(turn_range(&entries, 50), (Some("u1".into()), Some("u3".into())));
        assert_eq!(turn_range(&[], 0), (None, None));
        assert_eq!(turn_for_path(&entries, Some("u2"), Some("u3"), "src/a.rs"), Some("u2".into()));
        assert_eq!(turn_for_path(&entries, Some("u2"), Some("u3"), "src/old.rs"), None);
        assert_eq!(turn_for_path(&entries, Some("u1"), Some("u3"), "src/old.rs"), Some("u1".into()));
        assert_eq!(turn_for_path(&entries, Some("gone"), Some("u3"), "src/a.rs"), None);
    }
}
