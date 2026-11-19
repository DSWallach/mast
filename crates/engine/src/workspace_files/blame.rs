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
/// Commits per blame that may cost a `git branch --contains` spawn.
const MAX_FALLBACK_SPAWNS: usize = 12;
const MAX_CONTAINING_CACHE: usize = 512;
const MAX_TURN_CACHE: usize = 512;
const DEFAULT_BRANCHES: [&str; 5] = ["main", "master", "develop", "trunk", "HEAD"];

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
        let key = (workspace.checkout_id.clone(), revision.clone(), path.clone(), stamp);
        let cache = &self.inner.file_history;
        // Bind first: a guard in the scrutinee would live through the arms.
        let hit = cache.blames.lock().await.get(&key).cloned();
        let raw = match hit {
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
                        let blame = Arc::new(parse_porcelain(&revision, &output));
                        let mut blames = cache.blames.lock().await;
                        evict_if_full(&mut blames, MAX_BLAME_CACHE_ENTRIES);
                        blames.insert(key, blame.clone());
                        blame
                    }
                }
            }
        };
        let mut blame = (*raw).clone();
        self.resolve_chats(&workspace, &path, &mut blame).await;
        Ok(Arc::new(blame))
    }

    /// Attach the chat behind each commit: its registry record, else the
    /// branch-level guess ("probably").
    async fn resolve_chats(&self, workspace: &ResolvedWorkspace, path: &str, blame: &mut Blame) {
        let host = &self.inner.workspace;
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
            let turn_id = self.turn_for(&commit.sha, path, &record).await;
            commit.chat = Some(BlameChat {
                chat_id: record.chat_id,
                title,
                confidence: BlameChatConfidence::Recorded,
                turn_id,
            });
        }
        if unrecorded.is_empty() {
            return;
        }
        let chats = host.watch_chats().borrow().clone();
        if !chats.iter().any(|chat| chat.branch.is_some()) {
            return;
        }
        // Newest commits first: they are the ones a bound chat most plausibly wrote.
        unrecorded.sort_by_key(|&i| std::cmp::Reverse(blame.commits[i].author_time));
        let mut spawns = 0;
        for index in unrecorded {
            let sha = blame.commits[index].sha.clone();
            let key = (workspace.checkout_id.clone(), sha.clone());
            let cached = lock(&self.inner.file_history.containing).get(&key).cloned();
            let branches = match cached {
                Some(branches) => branches,
                None if spawns >= MAX_FALLBACK_SPAWNS => continue,
                None => {
                    spawns += 1;
                    let output = run_git(
                        &workspace.root,
                        &["branch", "--contains", &sha, "--format=%(refname:short)"],
                    )
                    .await
                    .unwrap_or_default();
                    let branches = Arc::new(
                        output
                            .lines()
                            .map(str::trim)
                            .filter(|b| !b.is_empty() && !DEFAULT_BRANCHES.contains(b))
                            .map(str::to_owned)
                            .collect::<Vec<_>>(),
                    );
                    let mut containing = lock(&self.inner.file_history.containing);
                    evict_if_full(&mut containing, MAX_CONTAINING_CACHE);
                    containing.insert(key, branches.clone());
                    branches
                }
            };
            let guess = chats
                .iter()
                .filter(|chat| chat.branch.as_deref().is_some_and(|b| branches.iter().any(|x| x == b)))
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

    /// The turn in the recorded range whose typed edit calls touched `path`.
    async fn turn_for(&self, sha: &str, path: &str, record: &mast_proto::CommitProvenance) -> Option<String> {
        let key = (sha.to_owned(), path.to_owned(), record.chat_id.clone());
        if let Some(hit) = lock(&self.inner.file_history.turns).get(&key) {
            return hit.clone();
        }
        let reader = self.inner.transcripts.get()?.clone();
        let (chat_id, start, end, path_owned) = (
            record.chat_id.clone(),
            record.turn_start.clone(),
            record.turn_end.clone(),
            path.to_owned(),
        );
        let turn = tokio::task::spawn_blocking(move || {
            let entries = reader(&chat_id)?;
            crate::provenance::turn_for_path(&entries, start.as_deref(), end.as_deref(), &path_owned)
        })
        .await
        .ok()
        .flatten();
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
