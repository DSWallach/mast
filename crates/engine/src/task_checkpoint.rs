//! Task checkpoints: a worker's worktree state saved as a git commit, so a
//! successor on another account or harness can pick the work up.
//!
//! A checkpoint is one commit under `refs/mast/wip/<item>`, parented on the
//! worker's HEAD. Its tree is the working tree as `git add -A` sees it: staged
//! and unstaged changes, deletions and untracked files that are not ignored.
//! It is built in a throwaway copy of the index, so the worker's HEAD, index
//! and files are never touched. The copy starts from the real index, so a
//! tracked file that matches an ignore rule stays in.
//!
//! The metadata is one JSON line in the commit message, so the ref is the whole
//! record. There is one ref per work item and the newest checkpoint wins;
//! saving the same tree on the same HEAD again changes nothing.
//!
//! Not kept: a version that exists only in the index (staged, then edited
//! again), the files of a nested repository or submodule (only its commit), and
//! a merge or rebase in progress (conflicted files are saved with their
//! markers, `MERGE_HEAD` and the rebase state are not).
//!
//! Saving is bounded by [`Limits`]: past the deadline the running git is
//! killed, and a checkout with too much untracked data is refused.
//!
//! Every function here blocks on git; call it from `spawn_blocking` in async
//! code.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use mast_proto::artifacts::git_refs::REFS_MAST_WIP;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::EngineError;

/// Longest progress note kept, in characters.
pub const MAX_NOTE_CHARS: usize = 500;
/// Longest item id used verbatim as a ref component.
const MAX_REF_COMPONENT: usize = 80;

/// How much work one [`save`] may do.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// For the whole save; the git running when it passes is killed.
    pub timeout: Duration,
    /// Untracked, unignored files, and their total size.
    pub max_untracked_files: usize,
    pub max_untracked_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(60), max_untracked_files: 5_000, max_untracked_bytes: 256 * 1024 * 1024 }
    }
}

/// What the caller knows about the work being saved.
#[derive(Debug, Clone, Default)]
pub struct CheckpointRequest {
    pub item_id: String,
    pub chat_id: String,
    /// Managed account the chat ran on; `None` is the Default login.
    pub account_id: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    /// Why the checkpoint was taken (a limit code, a drain).
    pub reason: String,
    /// Short progress note; cut at [`MAX_NOTE_CHARS`].
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointMeta {
    pub item_id: String,
    /// The worker's HEAD when saved: the checkpoint commit's parent.
    pub base_sha: String,
    /// The worker's branch; `None` when HEAD was detached.
    pub branch: Option<String>,
    pub chat_id: String,
    pub account_id: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub reason: String,
    /// Unix milliseconds.
    pub created_at: i64,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCheckpoint {
    pub ref_name: String,
    pub commit: String,
    pub tree: String,
    pub meta: CheckpointMeta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub checkpoint: TaskCheckpoint,
    /// False when the ref already held this tree on this HEAD.
    pub created: bool,
}

/// The ref a work item's checkpoint lives under. An id that is not a plain ref
/// component is cleaned and given a hash of the original, so two odd ids never
/// share a ref.
pub fn ref_name(item_id: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_');
    if !item_id.is_empty() && item_id.len() <= MAX_REF_COMPONENT && item_id.chars().all(safe) {
        return format!("{REFS_MAST_WIP}/{item_id}");
    }
    let cleaned: String = item_id.chars().take(MAX_REF_COMPONENT).map(|c| if safe(c) { c } else { '-' }).collect();
    let digest = Sha256::digest(item_id.as_bytes());
    let hash: String = digest.iter().take(6).map(|byte| format!("{byte:02x}")).collect();
    format!("{REFS_MAST_WIP}/{cleaned}-{hash}")
}

type Envs<'a> = [(&'a str, &'a std::ffi::OsStr)];

/// Run git in `dir`, with `envs` added, and return its trimmed stdout.
fn git(dir: &Path, envs: &Envs<'_>, args: &[&str]) -> Result<String, EngineError> {
    git_until(dir, envs, args, None)
}

/// [`git`], killed with everything it started if still running at `deadline`.
fn git_until(dir: &Path, envs: &Envs<'_>, args: &[&str], deadline: Option<Instant>) -> Result<String, EngineError> {
    let mut command = mast_harness::git_command();
    // Checkpoints never run repository hooks.
    command.args(["-c", "core.hooksPath=/dev/null"]).args(args).current_dir(dir).stdin(Stdio::null());
    // An inherited override would point git at some other repository or index.
    for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
        command.env_remove(name);
    }
    command.envs(envs.iter().copied()).env("GIT_TERMINAL_PROMPT", "0");
    let subcommand = args.first().copied().unwrap_or_default();
    // Output goes to files: a helper that outlives git cannot hold a pipe open.
    let (mut stdout, mut stderr) = (tempfile::tempfile()?, tempfile::tempfile()?);
    command.stdout(stdout.try_clone()?).stderr(stderr.try_clone()?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so a kill reaches filters and other helpers too.
        command.process_group(0);
    }
    let _permit = crate::git_gate::acquire_blocking("task_checkpoint");
    let mut child = command.spawn().map_err(|e| EngineError::Other(format!("git {subcommand} could not run: {e}")))?;
    let mut pause = Duration::from_millis(1);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            #[cfg(unix)]
            if let Ok(pid) = i32::try_from(child.id()) {
                // SAFETY: plain signal to the process group created for this child above.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(EngineError::Other(format!("git {subcommand} did not finish in time and was stopped")));
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(25));
    };
    let read = |file: &mut std::fs::File| -> std::io::Result<String> {
        let mut bytes = Vec::new();
        file.seek(SeekFrom::Start(0))?;
        file.read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    if !status.success() {
        return Err(EngineError::Other(format!("git {subcommand} failed: {}", read(&mut stderr)?.trim())));
    }
    // NUL is not whitespace, so NUL-separated output keeps its separators.
    Ok(read(&mut stdout)?.trim().to_owned())
}

/// Refuse a checkout whose untracked files are more than `limits` allows.
fn check_untracked(root: &Path, envs: &Envs<'_>, limits: &Limits, deadline: Instant) -> Result<(), EngineError> {
    let listed = git_until(root, envs, &["ls-files", "--others", "--exclude-standard", "-z"], Some(deadline))?;
    let (mut files, mut bytes) = (0usize, 0u64);
    for path in listed.split('\0').filter(|path| !path.is_empty()) {
        files += 1;
        if files > limits.max_untracked_files {
            return Err(EngineError::Other(format!("More than {} untracked files", limits.max_untracked_files)));
        }
        bytes = bytes.saturating_add(std::fs::symlink_metadata(root.join(path)).map_or(0, |meta| meta.len()));
        if bytes > limits.max_untracked_bytes {
            return Err(EngineError::Other(format!("More than {} bytes of untracked files", limits.max_untracked_bytes)));
        }
    }
    Ok(())
}

/// The working tree of `root` written to the object database.
fn write_worktree_tree(root: &Path, limits: &Limits, deadline: Instant) -> Result<String, EngineError> {
    let run = |envs: &Envs<'_>, args: &[&str]| git_until(root, envs, args, Some(deadline));
    let scratch = tempfile::tempdir()?;
    let index = scratch.path().join("index");
    let real = PathBuf::from(run(&[], &["rev-parse", "--path-format=absolute", "--git-path", "index"])?);
    if real.is_file() {
        // Git re-reads files changed in the same instant the index was written;
        // it needs the original's time to see which ones those are. Read before
        // the copy: an index rewritten meanwhile must not make the copy newer.
        let modified = std::fs::metadata(&real)?.modified()?;
        std::fs::copy(&real, &index)?;
        std::fs::File::options().write(true).open(&index)?.set_modified(modified)?;
    }
    let envs = [("GIT_INDEX_FILE", index.as_os_str())];
    check_untracked(root, &envs, limits, deadline)?;
    run(&envs, &["add", "-A", "--", "."])?;
    run(&envs, &["write-tree"])
}

/// Save the state of the checkout containing `worktree` as the checkpoint of
/// `request.item_id`. A repository without a commit cannot be checkpointed.
pub fn save(worktree: &Path, request: &CheckpointRequest) -> Result<Saved, EngineError> {
    save_within(worktree, request, &Limits::default())
}

/// [`save`] under other limits.
pub fn save_within(worktree: &Path, request: &CheckpointRequest, limits: &Limits) -> Result<Saved, EngineError> {
    if request.item_id.is_empty() {
        return Err(EngineError::Other("A checkpoint needs a work item id".into()));
    }
    let deadline = Instant::now() + limits.timeout;
    let run = |dir: &Path, args: &[&str]| git_until(dir, &[], args, Some(deadline));
    let root = PathBuf::from(run(worktree, &["rev-parse", "--show-toplevel"])?);
    let head = run(&root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let branch = run(&root, &["branch", "--show-current"])?;
    let tree = write_worktree_tree(&root, limits, deadline)?;
    let name = ref_name(&request.item_id);
    let existing = read_ref(&root, &name)?;
    if let Some(checkpoint) = existing.as_ref().filter(|c| c.tree == tree && c.meta.base_sha == head) {
        return Ok(Saved { checkpoint: checkpoint.clone(), created: false });
    }
    let meta = CheckpointMeta {
        item_id: request.item_id.clone(),
        base_sha: head.clone(),
        branch: Some(branch).filter(|branch| !branch.is_empty()),
        chat_id: request.chat_id.clone(),
        account_id: request.account_id.clone().filter(|id| !id.is_empty()),
        harness: request.harness.clone(),
        model: request.model.clone(),
        reason: request.reason.clone(),
        created_at: crate::now_ms(),
        note: request.note.chars().take(MAX_NOTE_CHARS).collect(),
    };
    let json = serde_json::to_string(&meta).map_err(|e| EngineError::Other(e.to_string()))?;
    let subject = "Mast task checkpoint";
    let who = std::ffi::OsStr::new("Mast");
    let email = std::ffi::OsStr::new("checkpoint@mast.invalid");
    let identity = [
        ("GIT_AUTHOR_NAME", who), ("GIT_AUTHOR_EMAIL", email),
        ("GIT_COMMITTER_NAME", who), ("GIT_COMMITTER_EMAIL", email),
    ];
    let commit = git_until(
        &root, &identity, &["commit-tree", &tree, "-p", &head, "-m", subject, "-m", &json], Some(deadline),
    )?;
    // Compare-and-swap: a checkpoint written meanwhile is not silently replaced.
    // An empty old value means the ref must not exist yet. No deadline: a git
    // killed here would leave the ref locked.
    let old = existing.map(|c| c.commit).unwrap_or_default();
    git(&root, &[], &["update-ref", &name, &commit, &old])?;
    Ok(Saved { checkpoint: TaskCheckpoint { ref_name: name, commit, tree, meta }, created: true })
}

/// Every readable checkpoint under `pattern`. A ref without Mast's metadata is
/// skipped.
fn read_refs(repo: &Path, pattern: &str) -> Result<Vec<TaskCheckpoint>, EngineError> {
    let out = git(repo, &[], &[
        "for-each-ref", "--format=%(refname)%00%(objectname)%00%(tree)%00%(contents)%00", pattern,
    ])?;
    let fields: Vec<&str> = out.split('\0').collect();
    let (records, _) = fields.as_chunks::<4>();
    Ok(records.iter().filter_map(|record| {
        let meta = record[3].lines().rev().find_map(|line| serde_json::from_str::<CheckpointMeta>(line).ok())?;
        Some(TaskCheckpoint {
            ref_name: record[0].trim().to_owned(),
            commit: record[1].to_owned(),
            tree: record[2].to_owned(),
            meta,
        })
    }).collect())
}

fn read_ref(repo: &Path, name: &str) -> Result<Option<TaskCheckpoint>, EngineError> {
    Ok(read_refs(repo, name)?.into_iter().find(|c| c.ref_name == name))
}

/// The checkpoint of a work item, if the repository holds one.
pub fn load(repo: &Path, item_id: &str) -> Result<Option<TaskCheckpoint>, EngineError> {
    read_ref(repo, &ref_name(item_id))
}

/// All checkpoints in the repository (every worktree shares them), newest first.
pub fn list(repo: &Path) -> Result<Vec<TaskCheckpoint>, EngineError> {
    let mut all = read_refs(repo, REFS_MAST_WIP)?;
    all.sort_by(|a, b| b.meta.created_at.cmp(&a.meta.created_at).then_with(|| a.ref_name.cmp(&b.ref_name)));
    Ok(all)
}

/// Delete the checkpoints `stale` picks and return them. One that changed after
/// it was listed is an error, not a deletion.
pub fn prune(repo: &Path, stale: impl Fn(&TaskCheckpoint) -> bool) -> Result<Vec<TaskCheckpoint>, EngineError> {
    let mut removed = Vec::new();
    for checkpoint in list(repo)?.into_iter().filter(|c| stale(c)) {
        git(repo, &[], &["update-ref", "-d", &checkpoint.ref_name, &checkpoint.commit])?;
        removed.push(checkpoint);
    }
    Ok(removed)
}

/// Create worktree `dest` on the new branch `branch` for a successor. Its files
/// equal the checkpoint's, HEAD is the worker's base commit and every saved
/// change is staged. `dest` must be absolute. An existing branch is refused.
pub fn restore(repo: &Path, item_id: &str, dest: &Path, branch: &str) -> Result<TaskCheckpoint, EngineError> {
    // `worktree add` and the reset would resolve a relative path differently.
    if !dest.is_absolute() {
        return Err(EngineError::Other("The restore folder must be an absolute path".into()));
    }
    let checkpoint = load(repo, item_id)?
        .ok_or_else(|| EngineError::Other(format!("No checkpoint for work item {item_id}")))?;
    git(repo, &[], &["check-ref-format", "--branch", branch])?;
    let dest_text = dest.to_str().ok_or_else(|| EngineError::Other("The worktree path is not UTF-8".into()))?;
    git(repo, &[], &["worktree", "add", "--no-track", "-b", branch, "--end-of-options", dest_text, &checkpoint.commit])?;
    // Soft: the index keeps the checkpoint's tree, ignored-but-tracked files too.
    // The target is the commit's real parent, not what its message claims.
    let parent = format!("{}^", checkpoint.commit);
    if let Err(error) = git(dest, &[], &["reset", "--soft", &parent]) {
        let _ = git(repo, &[], &["worktree", "remove", "--force", dest_text]);
        let _ = git(repo, &[], &["branch", "-D", branch]);
        return Err(error);
    }
    Ok(checkpoint)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
        let mut full = vec!["-c", "user.name=Test", "-c", "user.email=test@mast.invalid", "-c", "commit.gpgsign=false"];
        full.extend_from_slice(args);
        git(dir, &[], &full).unwrap_or_else(|error| panic!("git {args:?}: {error}"))
    }

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Every file of a worktree outside `.git`.
    fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.file_name().is_some_and(|name| name == ".git") { continue; }
                if path.is_dir() {
                    walk(root, &path, out);
                } else {
                    out.insert(path.strip_prefix(root).unwrap().to_owned(), std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    /// A repository with one commit and a worker's mix of changes on top.
    fn dirty_repo(root: &Path) -> PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "-q", "-b", "main"]);
        write(&repo, ".gitignore", "ignored.log\n*.keep\n");
        write(&repo, "staged.txt", "base\n");
        write(&repo, "unstaged.txt", "base\n");
        write(&repo, "deleted.txt", "base\n");
        write(&repo, "forced.keep", "base\n");
        run(&repo, &["add", ".gitignore", "staged.txt", "unstaged.txt", "deleted.txt"]);
        run(&repo, &["add", "-f", "forced.keep"]);
        run(&repo, &["commit", "-q", "-m", "base"]);
        write(&repo, "staged.txt", "staged\n");
        run(&repo, &["add", "staged.txt"]);
        write(&repo, "unstaged.txt", "unstaged\n");
        write(&repo, "forced.keep", "changed\n");
        std::fs::remove_file(repo.join("deleted.txt")).unwrap();
        write(&repo, "new/untracked.txt", "untracked\n");
        write(&repo, "ignored.log", "ignored\n");
        repo
    }

    fn request(item: &str) -> CheckpointRequest {
        CheckpointRequest {
            item_id: item.into(),
            chat_id: "chat-1".into(),
            account_id: Some("account-1".into()),
            harness: "codex".into(),
            model: Some("gpt-5".into()),
            reason: "usage_5h".into(),
            note: "halfway".into(),
        }
    }

    /// HEAD, branch, staged listing and status: what must not change.
    fn state(repo: &Path) -> (String, String, String, String) {
        (
            run(repo, &["rev-parse", "HEAD"]),
            run(repo, &["branch", "--show-current"]),
            run(repo, &["ls-files", "--stage"]),
            run(repo, &["status", "--porcelain", "--untracked-files=all"]),
        )
    }

    #[test]
    fn a_checkpoint_captures_every_change_and_leaves_the_worker_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_repo(tmp.path());
        let (before, files_before) = (state(&repo), files(&repo));
        let index_before = std::fs::read(repo.join(".git/index")).unwrap();

        let saved = save(&repo.join("new"), &request("item-1")).unwrap();

        assert!(saved.created);
        assert_eq!(std::fs::read(repo.join(".git/index")).unwrap(), index_before);
        assert_eq!(files(&repo), files_before);
        assert_eq!(state(&repo), before);
        let commit = &saved.checkpoint.commit;
        assert_eq!(saved.checkpoint.ref_name, "refs/mast/wip/item-1");
        assert_eq!(run(&repo, &["rev-parse", "refs/mast/wip/item-1"]), *commit);
        assert_eq!(run(&repo, &["rev-parse", &format!("{commit}^")]), before.0);
        for (path, text) in [("staged.txt", "staged"), ("unstaged.txt", "unstaged"),
            ("new/untracked.txt", "untracked"), ("forced.keep", "changed")]
        {
            assert_eq!(run(&repo, &["show", &format!("{commit}:{path}")]), text, "{path}");
        }
        let names = run(&repo, &["ls-tree", "-r", "--name-only", commit]);
        assert!(!names.contains("deleted.txt") && !names.contains("ignored.log"), "{names}");
        let meta = &saved.checkpoint.meta;
        assert_eq!((meta.base_sha.as_str(), meta.branch.as_deref()), (before.0.as_str(), Some("main")));
        assert_eq!((meta.chat_id.as_str(), meta.account_id.as_deref()), ("chat-1", Some("account-1")));
        assert_eq!((meta.harness.as_str(), meta.model.as_deref()), ("codex", Some("gpt-5")));
        assert_eq!((meta.reason.as_str(), meta.note.as_str()), ("usage_5h", "halfway"));
        assert!(meta.created_at > 0);
        assert_eq!(load(&repo, "item-1").unwrap(), Some(saved.checkpoint));
    }

    #[test]
    fn restore_reproduces_the_files_in_a_new_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_repo(tmp.path());
        let saved = save(&repo, &request("item-1")).unwrap().checkpoint;
        let before = state(&repo);
        let dest = tmp.path().join("successor");

        restore(&repo, "item-1", &dest, "work/successor").unwrap();

        let mut expected = files(&repo);
        expected.remove(Path::new("ignored.log"));
        assert_eq!(files(&dest), expected);
        assert_eq!(run(&dest, &["rev-parse", "HEAD"]), saved.meta.base_sha);
        assert_eq!(run(&dest, &["branch", "--show-current"]), "work/successor");
        assert_eq!(run(&dest, &["write-tree"]), saved.tree);
        assert_eq!(state(&repo), before);
        // An existing branch is never moved.
        assert!(restore(&repo, "item-1", &tmp.path().join("again"), "work/successor").is_err());
        assert!(restore(&repo, "missing", &tmp.path().join("none"), "work/none").is_err());
    }

    #[test]
    fn the_same_tree_on_the_same_head_is_saved_once() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_repo(tmp.path());
        let first = save(&repo, &request("item-1")).unwrap();
        let again = save(&repo, &CheckpointRequest { reason: "drain".into(), ..request("item-1") }).unwrap();
        assert!(!again.created);
        assert_eq!(again.checkpoint, first.checkpoint);

        write(&repo, "more.txt", "more\n");
        let changed = save(&repo, &request("item-1")).unwrap();
        assert!(changed.created);
        assert_ne!(changed.checkpoint.tree, first.checkpoint.tree);

        // The worker committing everything moves the base: same files, new record.
        run(&repo, &["add", "-A"]);
        run(&repo, &["commit", "-q", "-m", "progress"]);
        let committed = save(&repo, &request("item-1")).unwrap();
        assert!(committed.created);
        assert_eq!(committed.checkpoint.tree, changed.checkpoint.tree);
        assert_eq!(committed.checkpoint.meta.base_sha, run(&repo, &["rev-parse", "HEAD"]));
    }

    #[test]
    fn odd_item_ids_become_valid_distinct_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_repo(tmp.path());
        assert_eq!(ref_name("abc-DEF_1"), "refs/mast/wip/abc-DEF_1");
        let long = "x".repeat(300);
        let ids = ["a b", "a/b", "a:b", "a..b", ".lock", "x.lock", "-lead", "@{", "~^?*[\\", "émoji 🚀", &long, ""];
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            let name = ref_name(id);
            git(&repo, &[], &["check-ref-format", &name]).unwrap_or_else(|e| panic!("{id:?} -> {name}: {e}"));
            assert_eq!(name.matches('/').count(), 3, "one component: {name}");
            assert!(seen.insert(name), "{id:?} collides");
        }
        let saved = save(&repo, &request("../../heads/main")).unwrap().checkpoint;
        assert!(saved.ref_name.starts_with("refs/mast/wip/"));
        assert_eq!(load(&repo, "../../heads/main").unwrap(), Some(saved));
        assert!(save(&repo, &request("")).is_err());
    }

    #[test]
    fn list_and_prune_cover_only_checkpoints() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_repo(tmp.path());
        assert_eq!(list(&repo).unwrap(), Vec::new());
        save(&repo, &request("item-1")).unwrap();
        save(&repo, &request("item-10")).unwrap();
        run(&repo, &["update-ref", "refs/mast/wip/foreign", "HEAD"]);
        let mut items: Vec<String> = list(&repo).unwrap().into_iter().map(|c| c.meta.item_id).collect();
        items.sort();
        assert_eq!(items, ["item-1", "item-10"]);

        let removed = prune(&repo, |c| c.meta.item_id == "item-1").unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(load(&repo, "item-1").unwrap(), None);
        assert!(load(&repo, "item-10").unwrap().is_some());
        assert!(!run(&repo, &["rev-parse", "refs/mast/wip/foreign"]).is_empty());
    }

    #[test]
    fn too_much_untracked_data_is_refused_and_nothing_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_repo(tmp.path());
        let before = state(&repo);
        // One untracked file of 10 bytes: the ignored one does not count.
        for limits in [
            Limits { max_untracked_files: 0, ..Limits::default() },
            Limits { max_untracked_bytes: 9, ..Limits::default() },
        ] {
            assert!(save_within(&repo, &request("item-1"), &limits).is_err());
        }
        assert_eq!(load(&repo, "item-1").unwrap(), None);
        assert_eq!(state(&repo), before);
        let enough = Limits { max_untracked_files: 1, max_untracked_bytes: 10, ..Limits::default() };
        assert!(save_within(&repo, &request("item-1"), &enough).unwrap().created);
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_filter_is_killed_at_the_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_repo(tmp.path());
        write(&repo, ".gitattributes", "*.slow filter=slow\n");
        write(&repo, "data.slow", "data\n");
        run(&repo, &["config", "filter.slow.clean", "sleep 600"]);
        let limits = Limits { timeout: Duration::from_secs(2), ..Limits::default() };
        let started = Instant::now();
        let error = save_within(&repo, &request("item-1"), &limits).unwrap_err();
        assert!(error.to_string().contains("did not finish in time"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(60));
        assert_eq!(load(&repo, "item-1").unwrap(), None);
    }

    #[test]
    fn restore_needs_an_absolute_folder_and_ignores_a_forged_base() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_repo(tmp.path());
        let saved = save(&repo, &request("item-1")).unwrap().checkpoint;
        assert!(restore(&repo, "item-1", Path::new("relative"), "work/relative").is_err());
        assert!(!repo.join("relative").exists());

        // Rewrite the message so it names some other commit as the base.
        let other = run(&repo, &["commit-tree", &format!("{}^{{tree}}", saved.meta.base_sha), "-m", "other"]);
        let forged = CheckpointMeta { base_sha: other, ..saved.meta.clone() };
        let json = serde_json::to_string(&forged).unwrap();
        let commit = run(&repo, &["commit-tree", &saved.tree, "-p", &saved.meta.base_sha, "-m", &json]);
        run(&repo, &["update-ref", &saved.ref_name, &commit]);
        let dest = tmp.path().join("successor");
        restore(&repo, "item-1", &dest, "work/successor").unwrap();
        assert_eq!(run(&dest, &["rev-parse", "HEAD"]), saved.meta.base_sha);
    }

    #[test]
    fn a_long_note_is_cut_and_a_detached_head_has_no_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_repo(tmp.path());
        run(&repo, &["checkout", "-q", "--detach"]);
        let long = CheckpointRequest { note: "é\n".repeat(MAX_NOTE_CHARS), account_id: Some(String::new()), ..request("item-1") };
        let meta = save(&repo, &long).unwrap().checkpoint.meta;
        assert_eq!(meta.note.chars().count(), MAX_NOTE_CHARS);
        assert_eq!((meta.branch, meta.account_id), (None, None));
        assert_eq!(load(&repo, "item-1").unwrap().unwrap().meta.note, meta.note);
    }
}
