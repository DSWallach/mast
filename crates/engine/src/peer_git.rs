//! Branch transfer for a paired peer: the receiving side.
//!
//! A peer sends a branch as a `git bundle`, in chunks over the peer link, and
//! this device files it under the peer's own namespace
//! (`refs/heads/peer/<peer>/<branch>`) in the repository of a Space here, with a
//! worktree Mast creates for it. Nothing outside that namespace is read or
//! written: a peer never pushes to a branch someone has checked out, and it can
//! only bundle back the branches it sent. Bundles are verified, size-capped and
//! fetched with object checks on.
//!
//! Every function here blocks (it shells out to `git` and reads files); the
//! peer gate calls them from `spawn_blocking`.
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use mast_proto::peer_link::{
    PEER_GIT_CHUNK_BYTES, PEER_GIT_MAX_BUNDLE_BYTES, PeerGitApplyReply, PeerGitBundleReply,
};

/// Bundles are scratch files: anything older than this is deleted on the next transfer.
const TRANSFER_TTL: Duration = Duration::from_secs(60 * 60);

pub fn valid_transfer_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The part of a peer id used in ref and path names.
pub fn peer_slug(peer_id: &str) -> String {
    let slug: String = peer_id.chars().filter(char::is_ascii_alphanumeric).take(8).collect();
    if slug.is_empty() { "peer".to_string() } else { slug }
}

/// Plain branch names only: no option-looking names, no revision syntax, no
/// path tricks. `git check-ref-format` has the final say.
pub fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with(['-', '/', '.'])
        && !name.ends_with(['/', '.'])
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("/.")
        && !name.contains(".lock/")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

/// `peer/<peer>/<branch>`: the branch name on this device.
pub fn peer_branch(peer_id: &str, branch: &str) -> String {
    format!("peer/{}/{}", peer_slug(peer_id), branch)
}

fn transfers_dir(root: &Path, peer_id: &str) -> PathBuf {
    root.join(peer_slug(peer_id))
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn sweep_old(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > TRANSFER_TTL);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Forget a peer's scratch bundles (it was revoked).
pub fn remove_peer_transfers(root: &Path, peer_id: &str) {
    let _ = std::fs::remove_dir_all(transfers_dir(root, peer_id));
}

fn incoming_path(root: &Path, peer_id: &str, transfer_id: &str) -> Result<PathBuf, String> {
    if !valid_transfer_id(transfer_id) {
        return Err("transfer id must be 8 to 64 letters, digits or dashes".into());
    }
    Ok(transfers_dir(root, peer_id).join(format!("{transfer_id}.in.bundle")))
}

fn outgoing_path(root: &Path, peer_id: &str, transfer_id: &str) -> Result<PathBuf, String> {
    if !valid_transfer_id(transfer_id) {
        return Err("transfer id must be 8 to 64 letters, digits or dashes".into());
    }
    Ok(transfers_dir(root, peer_id).join(format!("{transfer_id}.out.bundle")))
}

/// Append a chunk to an incoming bundle. A chunk the file already holds is
/// accepted again (a retry after a lost reply); a gap is refused with the
/// length held, so the sender can resume. Returns the bytes held.
pub fn put_chunk(root: &Path, peer_id: &str, transfer_id: &str, offset: u64, bytes: &[u8]) -> Result<u64, String> {
    if bytes.len() > PEER_GIT_CHUNK_BYTES {
        return Err(format!("a chunk is at most {PEER_GIT_CHUNK_BYTES} bytes"));
    }
    let path = incoming_path(root, peer_id, transfer_id)?;
    let dir = transfers_dir(root, peer_id);
    create_private_dir(root).map_err(|e| format!("could not prepare the transfer area: {e}"))?;
    create_private_dir(&dir).map_err(|e| format!("could not prepare the transfer area: {e}"))?;
    sweep_old(&dir);
    let held = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if offset < held {
        return if offset + bytes.len() as u64 <= held { Ok(held) } else { Err(format!("chunk overlaps what is held; resume at offset {held}")) };
    }
    if offset > held {
        return Err(format!("expected offset {held}"));
    }
    if held + bytes.len() as u64 > PEER_GIT_MAX_BUNDLE_BYTES {
        return Err(format!("bundle is larger than {} MiB", PEER_GIT_MAX_BUNDLE_BYTES / 1024 / 1024));
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|e| format!("could not store the chunk: {e}"))?;
    file.write_all(bytes).map_err(|e| format!("could not store the chunk: {e}"))?;
    Ok(held + bytes.len() as u64)
}

/// Read up to one chunk of an outgoing bundle from `offset`; the flag says it was the last.
pub fn read_chunk(root: &Path, peer_id: &str, transfer_id: &str, offset: u64) -> Result<(Vec<u8>, bool), String> {
    let path = outgoing_path(root, peer_id, transfer_id)?;
    let mut file = std::fs::File::open(&path).map_err(|_| "unknown transfer (it may have expired)".to_string())?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if offset > len {
        return Err("offset is past the end of the bundle".into());
    }
    file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut buffer = vec![0u8; PEER_GIT_CHUNK_BYTES.min((len - offset) as usize)];
    file.read_exact(&mut buffer).map_err(|e| e.to_string())?;
    let end = offset + buffer.len() as u64;
    Ok((buffer, end >= len))
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = mast_harness::git_command()
        .args(args)
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        return Err(format!("git {} failed: {}", args.first().copied().unwrap_or(""), if message.is_empty() { "no output" } else { message }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_ok(repo: &Path, args: &[&str]) -> bool {
    mast_harness::git_command().args(args).current_dir(repo).env("GIT_TERMINAL_PROMPT", "0").output().is_ok_and(|o| o.status.success())
}

fn is_commit_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// Worktrees of `repo` as `(path, checked-out branch ref)`.
fn worktrees(repo: &Path) -> Result<Vec<(PathBuf, Option<String>)>, String> {
    let listing = git(repo, &["worktree", "list", "--porcelain"])?;
    let mut out = Vec::new();
    let mut current: Option<(PathBuf, Option<String>)> = None;
    for line in listing.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(done) = current.take() {
                out.push(done);
            }
            current = Some((PathBuf::from(path), None));
        } else if let Some(branch) = line.strip_prefix("branch ")
            && let Some((_, slot)) = current.as_mut()
        {
            *slot = Some(branch.to_string());
        }
    }
    out.extend(current);
    Ok(out)
}

/// Turn a fully received bundle into the peer's branch and worktree in `repo`.
///
/// Idempotent: applying the same bundle again returns the same worktree.
/// Refused when the branch already exists with commits the bundle does not
/// contain (the device's copy is ahead: fetch it first), or when its worktree
/// has uncommitted changes to tracked files.
pub fn apply_bundle(
    root: &Path,
    peer_id: &str,
    repo: &Path,
    transfer_id: &str,
    branch: &str,
    size: u64,
) -> Result<PeerGitApplyReply, String> {
    if !valid_branch(branch) {
        return Err("branch name has characters or a shape a peer branch cannot have".into());
    }
    let file = incoming_path(root, peer_id, transfer_id)?;
    let held = std::fs::metadata(&file).map(|m| m.len()).map_err(|_| "unknown transfer (it may have expired)".to_string())?;
    if held != size {
        return Err(format!("expected {size} bytes, have {held}"));
    }
    if size > PEER_GIT_MAX_BUNDLE_BYTES {
        return Err("bundle is too large".into());
    }
    let repo = repo.to_path_buf();
    let toplevel = git(&repo, &["rev-parse", "--show-toplevel"]).map_err(|_| "the Space is not a git repository".to_string())?;
    let repo = PathBuf::from(toplevel);
    let bundle = file.to_string_lossy().to_string();
    git(&repo, &["bundle", "verify", &bundle]).map_err(|e| {
        format!("{e} (the device may lack the base commit the bundle builds on; send a full bundle or an older base)")
    })?;
    let heads = git(&repo, &["bundle", "list-heads", &bundle])?;
    let wanted = format!("refs/heads/{branch}");
    let refs: Vec<&str> = heads.lines().filter_map(|l| l.split_whitespace().nth(1)).collect();
    if refs != [wanted.as_str()] {
        return Err(format!("the bundle must hold exactly {wanted}; it holds {}", if refs.is_empty() { "nothing".to_string() } else { refs.join(", ") }));
    }

    let slug = peer_slug(peer_id);
    let peer_ref_name = peer_branch(peer_id, branch);
    if !git_ok(&repo, &["check-ref-format", "--branch", &peer_ref_name]) {
        return Err("branch name is not a valid git branch name".into());
    }
    let incoming = format!("refs/peer-incoming/{slug}/{branch}");
    let fetch = format!("+{wanted}:{incoming}");
    git(&repo, &["-c", "transfer.fsckObjects=true", "fetch", "--no-tags", "--quiet", &bundle, &fetch])?;
    let result = place_branch(&repo, &slug, &peer_ref_name, &incoming, branch);
    let _ = git(&repo, &["update-ref", "-d", &incoming]);
    let (head, worktree) = result?;
    Ok(PeerGitApplyReply {
        space_id: String::new(),
        peer_branch: peer_ref_name,
        head,
        worktree_path: worktree.to_string_lossy().to_string(),
    })
}

fn place_branch(repo: &Path, slug: &str, peer_ref_name: &str, incoming: &str, branch: &str) -> Result<(String, PathBuf), String> {
    let head = git(repo, &["rev-parse", "--verify", &format!("{incoming}^{{commit}}")])?;
    let full_ref = format!("refs/heads/{peer_ref_name}");
    let all = worktrees(repo)?;
    let checked_out = all.iter().find(|(_, b)| b.as_deref() == Some(full_ref.as_str())).map(|(p, _)| p.clone());
    let exists = git_ok(repo, &["show-ref", "--verify", "--quiet", &full_ref]);
    if !exists {
        let target = worktree_target(repo, slug, branch)?;
        if all.iter().any(|(p, _)| p == &target) || target.exists() {
            return Err(format!("{} is already in use by another branch", target.display()));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("could not create the worktree folder: {e}"))?;
        }
        git(repo, &["worktree", "add", "-b", peer_ref_name, &target.to_string_lossy(), &head])?;
        return Ok((head, target));
    }
    let current = git(repo, &["rev-parse", "--verify", &format!("{full_ref}^{{commit}}")])?;
    if current != head && !git_ok(repo, &["merge-base", "--is-ancestor", &current, &head]) {
        return Err(format!(
            "{peer_ref_name} on this device has commits the bundle does not contain; fetch them first, or send under a new branch name"
        ));
    }
    if let Some(worktree) = checked_out {
        if current != head {
            if !git(&worktree, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
                return Err(format!("the worktree {} has uncommitted changes; commit or discard them first", worktree.display()));
            }
            git(&worktree, &["merge", "--ff-only", "--quiet", &head])?;
        }
        return Ok((head, worktree));
    }
    if current != head {
        git(repo, &["update-ref", &full_ref, &head, &current])?;
    }
    let target = worktree_target(repo, slug, branch)?;
    if all.iter().any(|(p, _)| p == &target) || target.exists() {
        return Err(format!("{} is already in use by another branch", target.display()));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create the worktree folder: {e}"))?;
    }
    git(repo, &["worktree", "add", &target.to_string_lossy(), peer_ref_name])?;
    Ok((head, target))
}

fn worktree_target(repo: &Path, slug: &str, branch: &str) -> Result<PathBuf, String> {
    let flat: String = branch.chars().map(|c| if c == '/' { '-' } else { c }).collect();
    crate::chat_workspace_plan::chat_worktree_target_for_slug(repo, &format!("peer-{slug}-{flat}"))
        .map(|(path, _)| path)
        .map_err(|e| e.to_string())
}

/// Bundle the commits the peer's branch gained since `base` (or all of it),
/// for the peer to fetch. Only a branch the peer sent can be bundled.
pub fn make_bundle(
    root: &Path,
    peer_id: &str,
    repo: &Path,
    branch: &str,
    base: Option<&str>,
) -> Result<PeerGitBundleReply, String> {
    if !valid_branch(branch) {
        return Err("branch name has characters or a shape a peer branch cannot have".into());
    }
    let full_ref = format!("refs/heads/{}", peer_branch(peer_id, branch));
    let toplevel = git(repo, &["rev-parse", "--show-toplevel"]).map_err(|_| "the Space is not a git repository".to_string())?;
    let repo = PathBuf::from(toplevel);
    if !git_ok(&repo, &["show-ref", "--verify", "--quiet", &full_ref]) {
        return Err(format!("this device has no branch {branch} sent by this peer"));
    }
    let head = git(&repo, &["rev-parse", "--verify", &format!("{full_ref}^{{commit}}")])?;
    let transfer_id = uuid::Uuid::new_v4().to_string();
    let dir = transfers_dir(root, peer_id);
    create_private_dir(root).map_err(|e| format!("could not prepare the transfer area: {e}"))?;
    create_private_dir(&dir).map_err(|e| format!("could not prepare the transfer area: {e}"))?;
    sweep_old(&dir);
    let out = outgoing_path(root, peer_id, &transfer_id)?;
    let out_arg = out.to_string_lossy().to_string();
    match base {
        Some(base) => {
            if !is_commit_id(base) {
                return Err("base must be a full commit id".into());
            }
            if !git_ok(&repo, &["cat-file", "-e", &format!("{base}^{{commit}}")]) {
                return Err("base is not a commit on this device".into());
            }
            if base == head {
                return Err("no new commits since base".into());
            }
            let exclude = format!("^{base}");
            git(&repo, &["bundle", "create", "--quiet", &out_arg, &full_ref, &exclude])?;
        }
        None => {
            git(&repo, &["bundle", "create", "--quiet", &out_arg, &full_ref])?;
        }
    }
    let size = std::fs::metadata(&out).map(|m| m.len()).map_err(|e| e.to_string())?;
    if size > PEER_GIT_MAX_BUNDLE_BYTES {
        let _ = std::fs::remove_file(&out);
        return Err("bundle is too large to send; send fewer commits".into());
    }
    Ok(PeerGitBundleReply { transfer_id, size, head, ref_name: full_ref })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
        let output = mast_harness::git_command()
            .args(["-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn repo_with_commit(dir: &Path) {
        run(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        run(dir, &["add", "."]);
        run(dir, &["commit", "-q", "-m", "one"]);
    }

    fn upload(root: &Path, peer: &str, transfer: &str, bundle: &Path) -> u64 {
        let bytes = std::fs::read(bundle).unwrap();
        let mut offset = 0u64;
        for chunk in bytes.chunks(PEER_GIT_CHUNK_BYTES) {
            offset = put_chunk(root, peer, transfer, offset, chunk).unwrap();
        }
        bytes.len() as u64
    }

    #[test]
    fn branch_names_are_plain() {
        for ok in ["feature", "feature/x", "a.b-c_d", "release/1.2"] {
            assert!(valid_branch(ok), "{ok}");
        }
        for bad in ["", "-x", "/x", "x/", ".x", "x.", "a..b", "a//b", "a b", "a~b", "a^b", "a:b", "a@{1}", "x.lock", "a/.b", "refs/../x", "a\\b"] {
            assert!(!valid_branch(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_chunk_is_appended_once_replayed_safely_and_a_gap_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("transfers");
        assert_eq!(put_chunk(&root, "peer-one", "transfer-1", 0, b"abcd").unwrap(), 4);
        assert_eq!(put_chunk(&root, "peer-one", "transfer-1", 0, b"abcd").unwrap(), 4, "a retried chunk changes nothing");
        assert_eq!(put_chunk(&root, "peer-one", "transfer-1", 4, b"ef").unwrap(), 6);
        assert!(put_chunk(&root, "peer-one", "transfer-1", 9, b"x").unwrap_err().contains("expected offset 6"));
        assert!(put_chunk(&root, "peer-one", "../escape-1", 0, b"x").is_err(), "a transfer id cannot name a path");
        assert_eq!(put_chunk(&root, "peer-two", "transfer-1", 0, b"zz").unwrap(), 2, "peers do not share transfers");
    }

    #[test]
    fn a_bundle_becomes_a_peer_branch_and_worktree_and_applying_twice_is_the_same() {
        let sender = tempfile::tempdir().unwrap();
        repo_with_commit(sender.path());
        run(sender.path(), &["checkout", "-q", "-b", "feature/x"]);
        std::fs::write(sender.path().join("b.txt"), "two\n").unwrap();
        run(sender.path(), &["add", "."]);
        run(sender.path(), &["commit", "-q", "-m", "two"]);
        let bundle = sender.path().join("out.bundle");
        run(sender.path(), &["bundle", "create", "-q", bundle.to_str().unwrap(), "refs/heads/feature/x"]);

        let receiver = tempfile::tempdir().unwrap();
        let repo = receiver.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        repo_with_commit(&repo);
        let root = receiver.path().join("transfers");
        let size = upload(&root, "peer-one", "transfer-1", &bundle);

        let first = apply_bundle(&root, "peer-one", &repo, "transfer-1", "feature/x", size).unwrap();
        assert_eq!(first.peer_branch, "peer/peerone/feature/x");
        assert_eq!(first.head, run(sender.path(), &["rev-parse", "HEAD"]));
        assert!(Path::new(&first.worktree_path).join("b.txt").exists(), "the worktree holds the sent commit");
        assert_eq!(run(Path::new(&first.worktree_path), &["rev-parse", "--abbrev-ref", "HEAD"]), "peer/peerone/feature/x");
        assert_eq!(run(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "main", "the Space's own checkout is untouched");
        assert!(run(&repo, &["for-each-ref", "refs/peer-incoming"]).is_empty(), "the staging ref is gone");

        let again = apply_bundle(&root, "peer-one", &repo, "transfer-1", "feature/x", size).unwrap();
        assert_eq!((again.head, again.worktree_path), (first.head, first.worktree_path));
    }

    #[test]
    fn a_resend_fast_forwards_a_clean_worktree_and_refuses_a_dirty_or_diverged_one() {
        let sender = tempfile::tempdir().unwrap();
        repo_with_commit(sender.path());
        run(sender.path(), &["checkout", "-q", "-b", "work"]);
        let receiver = tempfile::tempdir().unwrap();
        let repo = receiver.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        repo_with_commit(&repo);
        let root = receiver.path().join("transfers");
        let send = |transfer: &str| {
            let bundle = sender.path().join(format!("{transfer}.bundle"));
            run(sender.path(), &["bundle", "create", "-q", bundle.to_str().unwrap(), "refs/heads/work"]);
            let size = upload(&root, "peer-one", transfer, &bundle);
            apply_bundle(&root, "peer-one", &repo, transfer, "work", size)
        };
        let first = send("transfer-1").unwrap();

        std::fs::write(sender.path().join("c.txt"), "three\n").unwrap();
        run(sender.path(), &["add", "."]);
        run(sender.path(), &["commit", "-q", "-m", "three"]);
        let second = send("transfer-2").unwrap();
        assert_eq!(second.worktree_path, first.worktree_path);
        assert!(Path::new(&second.worktree_path).join("c.txt").exists());

        // The worker commits in the worktree; the sender amends its own history.
        let worktree = PathBuf::from(&second.worktree_path);
        std::fs::write(worktree.join("w.txt"), "worker\n").unwrap();
        run(&worktree, &["add", "."]);
        run(&worktree, &["commit", "-q", "-m", "worker"]);
        std::fs::write(sender.path().join("d.txt"), "four\n").unwrap();
        run(sender.path(), &["add", "."]);
        run(sender.path(), &["commit", "-q", "-m", "four"]);
        let diverged = send("transfer-3").unwrap_err();
        assert!(diverged.contains("has commits the bundle does not contain"), "{diverged}");
        assert!(worktree.join("w.txt").exists(), "the worker's commit is not lost");
    }

    #[test]
    fn a_bundle_with_any_other_ref_or_a_wrong_size_is_refused_and_writes_nothing() {
        let sender = tempfile::tempdir().unwrap();
        repo_with_commit(sender.path());
        run(sender.path(), &["branch", "other"]);
        let receiver = tempfile::tempdir().unwrap();
        let repo = receiver.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        repo_with_commit(&repo);
        let root = receiver.path().join("transfers");

        let two = sender.path().join("two.bundle");
        run(sender.path(), &["bundle", "create", "-q", two.to_str().unwrap(), "refs/heads/main", "refs/heads/other"]);
        let size = upload(&root, "peer-one", "transfer-1", &two);
        assert!(apply_bundle(&root, "peer-one", &repo, "transfer-1", "main", size).unwrap_err().contains("exactly"));

        let one = sender.path().join("one.bundle");
        run(sender.path(), &["bundle", "create", "-q", one.to_str().unwrap(), "refs/heads/main"]);
        let size = upload(&root, "peer-one", "transfer-2", &one);
        assert!(apply_bundle(&root, "peer-one", &repo, "transfer-2", "other", size).unwrap_err().contains("exactly"), "a ref under another name");
        assert!(apply_bundle(&root, "peer-one", &repo, "transfer-2", "main", size + 1).unwrap_err().contains("expected"));
        assert!(apply_bundle(&root, "peer-one", &repo, "transfer-2", "../main", size).is_err());
        assert!(apply_bundle(&root, "peer-two", &repo, "transfer-2", "main", size).is_err(), "another peer cannot apply this peer's upload");

        assert_eq!(run(&repo, &["branch", "--list", "peer/*"]), "", "no peer branch was written");
        assert_eq!(run(&repo, &["worktree", "list", "--porcelain"]).matches("worktree ").count(), 1);
    }

    #[test]
    fn only_a_branch_the_peer_sent_can_be_bundled_back_from_its_base() {
        let sender = tempfile::tempdir().unwrap();
        repo_with_commit(sender.path());
        run(sender.path(), &["checkout", "-q", "-b", "work"]);
        let receiver = tempfile::tempdir().unwrap();
        let repo = receiver.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        repo_with_commit(&repo);
        let root = receiver.path().join("transfers");
        let bundle = sender.path().join("out.bundle");
        run(sender.path(), &["bundle", "create", "-q", bundle.to_str().unwrap(), "refs/heads/work"]);
        let size = upload(&root, "peer-one", "transfer-1", &bundle);
        let applied = apply_bundle(&root, "peer-one", &repo, "transfer-1", "work", size).unwrap();

        assert!(make_bundle(&root, "peer-one", &repo, "main", None).unwrap_err().contains("no branch main"), "the Space's own branches are not reachable");
        assert!(make_bundle(&root, "peer-two", &repo, "work", None).unwrap_err().contains("no branch work"), "another peer's branch is not reachable");
        assert!(make_bundle(&root, "peer-one", &repo, "work", Some("main")).unwrap_err().contains("full commit id"));
        assert!(make_bundle(&root, "peer-one", &repo, "work", Some(&applied.head)).unwrap_err().contains("no new commits"));

        let worktree = PathBuf::from(&applied.worktree_path);
        std::fs::write(worktree.join("w.txt"), "worker\n").unwrap();
        run(&worktree, &["add", "."]);
        run(&worktree, &["commit", "-q", "-m", "worker"]);
        let reply = make_bundle(&root, "peer-one", &repo, "work", Some(&applied.head)).unwrap();
        assert_eq!(reply.ref_name, "refs/heads/peer/peerone/work");
        assert_eq!(reply.head, run(&worktree, &["rev-parse", "HEAD"]));
        let mut offset = 0;
        let mut bytes = Vec::new();
        loop {
            let (chunk, eof) = read_chunk(&root, "peer-one", &reply.transfer_id, offset).unwrap();
            offset += chunk.len() as u64;
            bytes.extend(chunk);
            if eof {
                break;
            }
        }
        assert_eq!(bytes.len() as u64, reply.size);
        assert!(read_chunk(&root, "peer-two", &reply.transfer_id, 0).is_err(), "another peer cannot read this bundle");
    }
}
