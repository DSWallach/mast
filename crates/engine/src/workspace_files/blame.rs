//! `git blame` for the code viewer's provenance gutter, sharing the git
//! plumbing and caches of [`file_history`](super::file_history).
//!
//! One blame per (checkout, HEAD, path, working-file stamp); computed only when
//! the UI asks (gutter on), off the UI thread, bounded by a file-size limit, an
//! output cap and a timeout. The cached result is chat-free; the chat behind
//! each commit (registry record, else a branch-level guess) is resolved per
//! request from cheap lookups so a record written after the cache entry still
//! shows up.

use super::file_history::{evict_if_full, flight, run_git, run_git_limited, valid_sha};
use super::*;
use mast_proto::{Blame, BlameChat, BlameChatConfidence, BlameCommit, BlameRun};

/// Largest working file blamed (bytes).
pub const MAX_BLAME_FILE_BYTES: u64 = 512 * 1024;
const MAX_BLAME_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_BLAME_CACHE_ENTRIES: usize = 16;
const MAX_TURN_CACHE: usize = 512;
/// Candidate branches considered for the guess (bound chats' branches).
const MAX_CANDIDATE_BRANCHES: usize = 16;
/// Commits per candidate branch (`default..branch`) examined.
const MAX_BRANCH_COMMITS: usize = 2_000;
const DEFAULT_BRANCH_NAMES: [&str; 4] = ["main", "master", "develop", "trunk"];

/// One cached blame: the chat-free result plus what the "probably" guess needs.
/// The guess is scoped: a commit is a candidate for branch `B` only when it
/// lies in `default..B` (never main's own history), and chats count only when
/// their cwd is in this repository.
#[derive(Debug, Clone)]
pub struct BlameEntry {
    pub blame: Blame,
    /// sha → candidate branches (those whose `default..branch` holds it).
    candidates: HashMap<String, Vec<String>>,
    /// Worktree roots of this repository.
    repo_roots: Vec<PathBuf>,
}

impl WorkspaceFiles {
    pub async fn file_blame(
        &self,
        request: super::file_history::FileHistoryRequest,
    ) -> Result<Arc<Blame>, WorkspaceFilesError> {
        let path = WorkspaceRelativePath::file(&request.path)?.wire_path();
        let workspace = self.resolve_target(&request.target).await?;
        let metadata = std::fs::metadata(workspace.root.join(&path))
            .map_err(|error| WorkspaceFilesError::NotFound(error.to_string()))?;
        if !metadata.is_file() {
            return Err(WorkspaceFilesError::BadParams("blame needs a file".into()));
        }
        if metadata.len() > MAX_BLAME_FILE_BYTES {
            return Err(WorkspaceFilesError::Unsupported(
                "file is too large to blame".into(),
            ));
        }
        let stamp = (
            metadata.len(),
            metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos()),
        );
        let revision = run_git(&workspace.root, &["rev-parse", "--verify", "HEAD^{commit}"])
            .await?
            .trim()
            .to_string();
        if !valid_sha(&revision) {
            return Err(WorkspaceFilesError::Io("invalid checkout revision".into()));
        }
        let cache = &self.inner.file_history;
        let common_dir = self.common_dir(&workspace).await;
        let refs = common_dir.as_deref().map_or(0, refs_stamp);
        let key = (workspace.checkout_id.clone(), revision.clone(), path.clone(), stamp, refs);
        // Bind first: a guard in the scrutinee would live through the arms.
        let hit = cache.blames.lock().await.get(&key).cloned();
        let entry = match hit {
            Some(hit) => hit,
            None => {
                let flight = flight(&cache.blame_flights, &key);
                let _guard = flight.lock().await;
                let cached = cache.blames.lock().await.get(&key).cloned();
                match cached {
                    Some(hit) => hit,
                    None => {
                        let output = run_git_limited(
                            &workspace.root,
                            &["blame", "--porcelain", "--no-progress", "--", &path],
                            MAX_BLAME_OUTPUT,
                        )
                        .await?;
                        let blame = parse_porcelain(&revision, &output);
                        let (candidates, repo_roots) = self.branch_candidates(&workspace.root, &blame).await;
                        let entry = Arc::new(BlameEntry { blame, candidates, repo_roots });
                        let mut blames = cache.blames.lock().await;
                        evict_if_full(&mut blames, MAX_BLAME_CACHE_ENTRIES);
                        blames.insert(key, entry.clone());
                        entry
                    }
                }
            }
        };
        let mut blame = entry.blame.clone();
        self.resolve_chats(&entry, &path, &mut blame).await;
        Ok(Arc::new(blame))
    }

    async fn common_dir(&self, workspace: &ResolvedWorkspace) -> Option<PathBuf> {
        if let Some(dir) = lock(&self.inner.file_history.common_dirs).get(&workspace.checkout_id) {
            return Some(dir.clone());
        }
        let out = run_git(&workspace.root, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await.ok()?;
        let dir = PathBuf::from(out.trim());
        lock(&self.inner.file_history.common_dirs).insert(workspace.checkout_id.clone(), dir.clone());
        Some(dir)
    }

    /// For the branches bound chats work on (this repository only), the blamed
    /// commits lying in `default..branch`. A handful of spawns per blame
    /// computation (default branch, worktree list, one `rev-list` per branch),
    /// none on a cache hit.
    async fn branch_candidates(&self, root: &Path, blame: &Blame) -> (HashMap<String, Vec<String>>, Vec<PathBuf>) {
        let roots: Vec<PathBuf> = run_git(root, &["worktree", "list", "--porcelain"])
            .await
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.strip_prefix("worktree ").map(PathBuf::from))
            .collect();
        let chats = self.inner.workspace.watch_chats().borrow().clone();
        let mut branches: Vec<String> = Vec::new();
        for chat in &chats {
            let (Some(branch), Some(cwd)) = (chat.branch.as_deref(), chat.cwd.as_deref()) else { continue };
            if roots.iter().any(|r| Path::new(cwd).starts_with(r)) && !branches.iter().any(|b| b == branch) {
                branches.push(branch.to_owned());
            }
        }
        branches.truncate(MAX_CANDIDATE_BRANCHES);
        let mut candidates: HashMap<String, Vec<String>> = HashMap::new();
        if branches.is_empty() || blame.commits.is_empty() {
            return (candidates, roots);
        }
        let Some(default) = default_branch(root).await else { return (candidates, roots) };
        let wanted: HashSet<&str> = blame.commits.iter().map(|c| c.sha.as_str()).collect();
        for branch in branches {
            if branch == default || default.strip_prefix("origin/") == Some(branch.as_str()) {
                continue; // the default branch's own history is never guessed
            }
            let limit = format!("--max-count={MAX_BRANCH_COMMITS}");
            let Ok(out) = run_git(root, &["rev-list", &limit, &format!("refs/heads/{branch}"), "--not", &default]).await else {
                continue;
            };
            for sha in out.lines().map(str::trim).filter(|sha| wanted.contains(sha)) {
                candidates.entry(sha.to_owned()).or_default().push(branch.clone());
            }
        }
        (candidates, roots)
    }

    /// Attach the chat behind each commit: its registry record, else the
    /// branch-level guess ("probably"). One transcript read per chat per call.
    async fn resolve_chats(&self, entry: &BlameEntry, path: &str, blame: &mut Blame) {
        let host = &self.inner.workspace;
        let mut transcripts: HashMap<String, Option<Arc<Vec<mast_doc::SessionMessageEntry>>>> = HashMap::new();
        let mut unrecorded: Vec<usize> = Vec::new();
        for (index, commit) in blame.commits.iter_mut().enumerate() {
            let Some(record) = host.commit_provenance(&commit.sha) else {
                unrecorded.push(index);
                continue;
            };
            let title = match host.chat(&record.chat_id) {
                Ok(Some(chat)) => chat.title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "Untitled chat".into()),
                _ => "Deleted chat".into(),
            };
            let turn_id = self.turn_for(&mut transcripts, path, &record).await;
            commit.chat = Some(BlameChat {
                chat_id: record.chat_id,
                title,
                confidence: BlameChatConfidence::Recorded,
                turn_id,
            });
        }
        if unrecorded.is_empty() || entry.candidates.is_empty() {
            return;
        }
        let chats = host.watch_chats().borrow().clone();
        for index in unrecorded {
            let Some(branches) = entry.candidates.get(&blame.commits[index].sha) else { continue };
            let guess = chats
                .iter()
                .filter(|chat| {
                    chat.branch.as_deref().is_some_and(|b| branches.iter().any(|x| x == b))
                        && chat.cwd.as_deref().is_some_and(|cwd| entry.repo_roots.iter().any(|r| Path::new(cwd).starts_with(r)))
                })
                .max_by_key(|chat| (!chat.archived, chat.last_message_at));
            if let Some(chat) = guess {
                blame.commits[index].chat = Some(BlameChat {
                    chat_id: chat.id.clone(),
                    title: chat.title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "Untitled chat".into()),
                    confidence: BlameChatConfidence::Probably,
                    turn_id: None,
                });
            }
        }
    }

    /// The turn in the recorded range whose typed edit calls touched `path`,
    /// cached per (chat, range, path). `transcripts` holds this call's reads:
    /// one read-only transcript load per chat, off the async threads; a chat
    /// with no local transcript gives no turn link.
    async fn turn_for(
        &self,
        transcripts: &mut HashMap<String, Option<Arc<Vec<mast_doc::SessionMessageEntry>>>>,
        path: &str,
        record: &mast_proto::CommitProvenance,
    ) -> Option<String> {
        let key = (
            record.chat_id.clone(),
            record.turn_start.clone(),
            record.turn_end.clone(),
            path.to_owned(),
        );
        if let Some(hit) = lock(&self.inner.file_history.turns).get(&key) {
            return hit.clone();
        }
        record.turn_start.as_ref()?;
        let reader = self.inner.transcripts.get()?.clone();
        if !transcripts.contains_key(&record.chat_id) {
            let chat_id = record.chat_id.clone();
            let loaded = tokio::task::spawn_blocking(move || reader(&chat_id)).await.ok().flatten().map(Arc::new);
            transcripts.insert(record.chat_id.clone(), loaded);
        }
        let Some(Some(entries)) = transcripts.get(&record.chat_id) else {
            return None; // no local transcript: nothing cached, it may arrive later
        };
        let turn = crate::provenance::turn_for_path(entries, record.turn_start.as_deref(), record.turn_end.as_deref(), path);
        let mut turns = lock(&self.inner.file_history.turns);
        evict_if_full(&mut turns, MAX_TURN_CACHE);
        turns.insert(key, turn.clone());
        turn
    }

    /// Reads chat transcripts for turn precision (wired by engine assembly).
    pub fn set_transcript_reader(&self, reader: crate::provenance::TranscriptReader) {
        let _ = self.inner.transcripts.set(reader);
    }
}

/// The repository's default branch: `origin/HEAD` when set, else the first of
/// main / master / develop / trunk that exists locally.
async fn default_branch(root: &Path) -> Option<String> {
    if let Ok(out) = run_git(root, &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"]).await {
        let name = out.trim();
        if !name.is_empty() {
            return Some(name.to_owned());
        }
    }
    for name in DEFAULT_BRANCH_NAMES {
        if run_git(root, &["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")]).await.is_ok() {
            return Some(name.to_owned());
        }
    }
    None
}

/// Cheap fingerprint of the branch refs from file metadata (no spawn): mtimes
/// of `packed-refs`, `refs/heads` and its first-level subdirectories. A moved
/// ref rewrites its file or directory entry, so the stamp changes.
pub(super) fn refs_stamp(common_dir: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mtime = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos())
    };
    mtime(&common_dir.join("packed-refs")).hash(&mut hasher);
    let heads = common_dir.join("refs/heads");
    mtime(&heads).hash(&mut hasher);
    if let Ok(read) = std::fs::read_dir(&heads) {
        let mut entries: Vec<_> = read.flatten().take(256).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            entry.file_name().hash(&mut hasher);
            mtime(&entry.path()).hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Parse `git blame --porcelain`: header lines `<sha> <orig> <final> [<count>]`
/// (the count only opens a group), per-commit metadata on first sight, and a
/// tab-prefixed content line per source line.
pub(super) fn parse_porcelain(revision: &str, output: &str) -> Blame {
    let mut commits: Vec<BlameCommit> = Vec::new();
    let mut index_of: HashMap<String, Option<u32>> = HashMap::new();
    let mut runs: Vec<BlameRun> = Vec::new();
    let mut current: Option<String> = None;
    for line in output.lines() {
        if line.starts_with('\t') {
            continue;
        }
        let mut fields = line.split(' ');
        let first = fields.next().unwrap_or("");
        if valid_sha(first) {
            let (Some(_orig), Some(final_line)) = (fields.next(), fields.next().and_then(|v| v.parse::<u32>().ok())) else {
                continue;
            };
            let count = fields.next().and_then(|v| v.parse::<u32>().ok());
            let uncommitted = first.bytes().all(|b| b == b'0');
            let entry = index_of.entry(first.to_owned()).or_insert_with(|| {
                if uncommitted {
                    None
                } else {
                    commits.push(BlameCommit {
                        sha: first.to_owned(),
                        author: String::new(),
                        author_time: 0,
                        subject: String::new(),
                        path: String::new(),
                        previous_path: None,
                        chat: None,
                    });
                    Some(commits.len() as u32 - 1)
                }
            });
            current = Some(first.to_owned());
            if let Some(count) = count {
                let commit = *entry;
                match runs.last_mut() {
                    Some(last) if last.commit == commit && last.start + last.len == final_line => last.len += count,
                    _ => runs.push(BlameRun { start: final_line, len: count, commit }),
                }
            }
            continue;
        }
        let Some(Some(index)) = current.as_ref().and_then(|sha| index_of.get(sha)).copied() else { continue };
        let commit = &mut commits[index as usize];
        match line.split_once(' ') {
            Some(("author", name)) if commit.author.is_empty() => commit.author = name.chars().take(120).collect(),
            Some(("author-time", time)) => commit.author_time = time.parse().unwrap_or(0),
            Some(("summary", subject)) => commit.subject = subject.chars().take(200).collect(),
            Some(("filename", path)) => commit.path = path.to_owned(),
            Some(("previous", rest)) => {
                commit.previous_path = rest.split_once(' ').map(|(_, path)| path.to_owned());
            }
            _ => {}
        }
    }
    for commit in &mut commits {
        if commit.previous_path.as_deref() == Some(commit.path.as_str()) {
            commit.previous_path = None;
        }
    }
    Blame { revision: revision.to_owned(), commits, runs }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_runs_commits_and_uncommitted_lines() {
        let a = "a".repeat(40);
        let z = "0".repeat(40);
        let output = format!(
            "{a} 1 1 2\nauthor Ann\nauthor-mail <a@x>\nauthor-time 1700000000\nsummary first\nprevious {a} old.rs\nfilename new.rs\n\tl1\n{a} 2 2\n\tl2\n{z} 3 3 1\nauthor Not Committed Yet\nsummary Version of new.rs from new.rs\nfilename new.rs\n\tl3\n{a} 3 4 1\n\tl4\n"
        );
        let blame = parse_porcelain("rev", &output);
        assert_eq!(blame.commits.len(), 1);
        assert_eq!(blame.commits[0].author, "Ann");
        assert_eq!(blame.commits[0].subject, "first");
        assert_eq!(blame.commits[0].path, "new.rs");
        assert_eq!(blame.commits[0].previous_path.as_deref(), Some("old.rs"));
        assert_eq!(
            blame.runs,
            vec![
                BlameRun { start: 1, len: 2, commit: Some(0) },
                BlameRun { start: 3, len: 1, commit: None },
                BlameRun { start: 4, len: 1, commit: Some(0) },
            ]
        );
    }
}
