//! Bounded checkout file history and immutable commit-diff cache.

use super::*;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

const MAX_COMMITS: usize = 100;
const MAX_HISTORY_CACHE_ENTRIES: usize = 32;
const MAX_DIFF_CACHE_ENTRIES: usize = 16;
const MAX_GIT_OUTPUT: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistoryRequest {
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistoryDiffRequest {
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    pub commit: String,
    pub path: String,
    pub previous_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistoryEntry {
    pub commit: mast_proto::GitHistoryCommit,
    pub path: String,
    pub previous_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistory {
    pub revision: String,
    pub commits: Vec<FileHistoryEntry>,
    pub truncated: bool,
}

#[derive(Default)]
pub struct FileHistoryCache {
    histories: tokio::sync::Mutex<HashMap<(String, String, String), Arc<FileHistory>>>,
    diffs: tokio::sync::Mutex<HashMap<(String, String, String, Option<String>), Arc<String>>>,
}

impl WorkspaceFiles {
    pub async fn file_history(
        &self,
        request: FileHistoryRequest,
    ) -> Result<Arc<FileHistory>, WorkspaceFilesError> {
        let path = WorkspaceRelativePath::file(&request.path)?.wire_path();
        let workspace = self.resolve_target(&request.target).await?;
        let revision =
            run_git(&workspace.root, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
        let revision = revision.trim().to_string();
        if !valid_sha(&revision) {
            return Err(WorkspaceFilesError::Io("invalid checkout revision".into()));
        }
        let key = (
            workspace.checkout_id.clone(),
            revision.clone(),
            path.clone(),
        );
        let mut cache = self.inner.file_history.histories.lock().await;
        if let Some(history) = cache.get(&key) {
            return Ok(history.clone());
        }
        let output = run_git(
            &workspace.root,
            &[
                "log",
                "--follow",
                "--max-count=101",
                "--no-color",
                "--no-decorate",
                "--no-show-signature",
                "--name-status",
                "-z",
                "--format=%x00%H%x00%P%x00%<(200,trunc)%s%x00%<(120,trunc)%an%x00%aI%x00",
                &revision,
                "--",
                &path,
            ],
        )
        .await?;
        let mut commits = parse_log(&output);
        let truncated = commits.len() > MAX_COMMITS;
        commits.truncate(MAX_COMMITS);
        let history = Arc::new(FileHistory {
            revision,
            commits,
            truncated,
        });
        evict_if_full(&mut cache, MAX_HISTORY_CACHE_ENTRIES);
        cache.insert(key, history.clone());
        Ok(history)
    }

    pub async fn file_history_diff(
        &self,
        request: FileHistoryDiffRequest,
    ) -> Result<Arc<String>, WorkspaceFilesError> {
        if !valid_sha(&request.commit) {
            return Err(WorkspaceFilesError::BadParams(
                "commit must be a full hexadecimal object id".into(),
            ));
        }
        let path = WorkspaceRelativePath::file(&request.path)?.wire_path();
        let previous = request
            .previous_path
            .as_deref()
            .map(WorkspaceRelativePath::file)
            .transpose()?
            .map(|path| path.wire_path());
        let workspace = self.resolve_target(&request.target).await?;
        let key = (
            workspace.checkout_id,
            request.commit.clone(),
            path.clone(),
            previous.clone(),
        );
        let mut cache = self.inner.file_history.diffs.lock().await;
        if let Some(diff) = cache.get(&key) {
            return Ok(diff.clone());
        }
        let mut args = vec![
            "show",
            "--format=",
            "--no-ext-diff",
            "--no-color",
            "--find-renames",
            "--root",
            request.commit.as_str(),
            "--",
            path.as_str(),
        ];
        if let Some(previous) = &previous {
            args.push(previous);
        }
        let diff = Arc::new(run_git(&workspace.root, &args).await?);
        evict_if_full(&mut cache, MAX_DIFF_CACHE_ENTRIES);
        cache.insert(key, diff.clone());
        Ok(diff)
    }
}

fn evict_if_full<K: Eq + std::hash::Hash + Clone, V>(cache: &mut HashMap<K, V>, limit: usize) {
    if cache.len() >= limit
        && let Some(key) = cache.keys().next().cloned()
    {
        cache.remove(&key);
    }
}

fn valid_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn run_git(root: &Path, args: &[&str]) -> Result<String, WorkspaceFilesError> {
    let mut child = mast_harness::git_command_async()
        .current_dir(root)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| WorkspaceFilesError::Io(error.to_string()))?;
    let output = tokio::time::timeout(Duration::from_secs(20), async {
        let mut bytes = Vec::new();
        let mut stdout = child
            .stdout
            .take()
            .expect("piped git output")
            .take(MAX_GIT_OUTPUT as u64 + 1);
        stdout
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| WorkspaceFilesError::Io(error.to_string()))?;
        if bytes.len() > MAX_GIT_OUTPUT {
            let _ = child.kill().await;
            return Err(WorkspaceFilesError::Unsupported(
                "file history output exceeds the limit".into(),
            ));
        }
        let status = child
            .wait()
            .await
            .map_err(|error| WorkspaceFilesError::Io(error.to_string()))?;
        if !status.success() {
            return Err(WorkspaceFilesError::Io("git file history failed".into()));
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    })
    .await
    .map_err(|_| WorkspaceFilesError::Io("git file history timed out".into()))??;
    Ok(output)
}

fn parse_log(output: &str) -> Vec<FileHistoryEntry> {
    let fields: Vec<_> = output.split('\0').collect();
    let mut index = 0;
    let mut commits = Vec::new();
    while index + 6 < fields.len() {
        if !fields[index].is_empty() || !valid_sha(fields[index + 1]) {
            index += 1;
            continue;
        }
        let sha = fields[index + 1].to_owned();
        let parents = fields[index + 2]
            .split_ascii_whitespace()
            .filter(|parent| valid_sha(parent))
            .map(str::to_owned)
            .collect();
        let subject = fields[index + 3].trim_end().chars().take(200).collect();
        let author = fields[index + 4].trim_end().chars().take(120).collect();
        let date = fields[index + 5].chars().take(80).collect();
        index += 6;
        while index < fields.len() && fields[index].is_empty() {
            index += 1;
        }
        if index + 1 >= fields.len() {
            break;
        }
        let status = fields[index].trim_start_matches(['\r', '\n']);
        index += 1;
        let first = fields[index];
        index += 1;
        let (path, previous_path) = if status.starts_with('R') && index < fields.len() {
            let second = fields[index];
            index += 1;
            (second, Some(first))
        } else {
            (first, None)
        };
        if WorkspaceRelativePath::file(path).is_err()
            || previous_path.is_some_and(|path| WorkspaceRelativePath::file(path).is_err())
        {
            continue;
        }
        commits.push(FileHistoryEntry {
            commit: mast_proto::GitHistoryCommit {
                sha,
                parent_shas: parents,
                subject,
                author_name: author,
                author_email: String::new(),
                authored_at: date,
                refs: Vec::new(),
            },
            path: path.into(),
            previous_path: previous_path.map(str::to_owned),
        });
    }
    commits
}
