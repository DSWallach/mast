//! Branch transfer for a Manager driving a paired device: the sending side.
//!
//! `peer_push_branch` bundles a local branch with `git bundle`, sends it over
//! the peer link in chunks and has the device file it as a branch of its own
//! (`peer/<peer>/<branch>`) with a worktree to work in. `peer_fetch_branch`
//! asks the device to bundle what that branch gained and fetches it into a
//! local branch. Neither touches a branch somebody has checked out: git itself
//! refuses to fetch into one, and the device only ever writes under its own
//! `peer/` namespace.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use base64::Engine as _;
use mast_proto::peer_link as wire;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::mast::Mast;

/// Attempts at one chunk. Every transfer call is safe to repeat.
const CHUNK_ATTEMPTS: usize = 3;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushArgs {
    /// Absolute path of the repository on this machine.
    pub repo: String,
    /// Local branch to send (a name, not a revision).
    pub branch: String,
    /// The Space on the device whose repository receives it (id, path or name).
    pub project: String,
    /// A revision the device already has (for example the branch both sides
    /// track). Only commits after it are sent. Omit it to send the whole history.
    #[serde(default)]
    pub base: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchArgs {
    pub repo: String,
    /// The branch name used when sending it.
    pub branch: String,
    pub project: String,
    /// The `head` `peer_push_branch` returned, or any commit you already have
    /// on the branch: only newer commits come back.
    #[serde(default)]
    pub base: Option<String>,
    /// Local branch that receives the commits (default `peer/<branch>`).
    #[serde(default)]
    pub into: Option<String>,
    /// Allow `into` to move to commits that do not descend from where it is.
    #[serde(default)]
    pub force: bool,
}

/// A bundle on disk for one transfer. It lives in a folder only this user can
/// open (a bundle holds source code, and the shared temp folder is world
/// readable), removed with it.
struct Scratch {
    dir: PathBuf,
    file: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("mast-peer-{}", uuid::Uuid::new_v4().simple()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        // Not recursive: an existing path is an error, never somebody else's folder.
        builder.create(&dir)?;
        let file = dir.join(format!("{label}.bundle"));
        Ok(Self { dir, file })
    }

    /// The bundle file, created empty and readable by its owner only.
    fn create_file(&self) -> std::io::Result<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&self.file)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Fill `buffer` from `reader` unless it ends first; returns the bytes read.
fn read_full(reader: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

async fn git(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = mast_harness::git_command_async()
        .args(args)
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .context("could not run git")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.first().copied().unwrap_or(""), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn local_repo(path: &str) -> anyhow::Result<PathBuf> {
    let path = PathBuf::from(path.trim());
    if !path.is_absolute() || !path.is_dir() {
        bail!("repo must be the absolute path of a repository on this machine");
    }
    Ok(path)
}

/// Branch names with nothing a git argument or ref path could misread.
fn plain_branch(name: &str) -> anyhow::Result<&str> {
    let ok = !name.is_empty()
        && name.len() <= 100
        && !name.starts_with(['-', '/', '.'])
        && !name.ends_with(['/', '.'])
        && !name.contains("..")
        && !name.contains("//")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    if !ok {
        bail!("branch {name:?} must be a plain branch name (letters, digits, . _ - and /)");
    }
    Ok(name)
}

async fn resolve_commit(repo: &Path, revision: &str) -> anyhow::Result<String> {
    if revision.starts_with('-') {
        bail!("{revision:?} is not a revision");
    }
    git(repo, &["rev-parse", "--verify", &format!("{revision}^{{commit}}")])
        .await
        .map_err(|_| anyhow!("{revision:?} is not a commit in {}", repo.display()))
}

async fn call_with_retry(mast: &Mast, method: &str, params: Value) -> anyhow::Result<Value> {
    let mut last = None;
    for _ in 0..CHUNK_ATTEMPTS {
        match mast.call(method, params.clone()).await {
            Ok(value) => return Ok(value),
            // A refusal is final; only a failed connection is worth another try.
            Err(error) if error.to_string().contains("expected offset") || error.to_string().contains("not available to a paired peer") => return Err(error),
            Err(error) => {
                last = Some(error);
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("{method} failed")))
}

/// Send `args.branch` to the device and return where it landed.
pub async fn push_branch(mast: &Mast, args: PushArgs) -> anyhow::Result<Value> {
    let repo = local_repo(&args.repo)?;
    let branch = plain_branch(args.branch.trim())?.to_string();
    let head = resolve_commit(&repo, &format!("refs/heads/{branch}")).await?;
    let base = match args.base.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(base) => Some(resolve_commit(&repo, base).await?),
        None => None,
    };
    let space = mast.resolve_space(&args.project).await?;

    let scratch = Scratch::new("out").context("could not prepare a private folder for the bundle")?;
    let target = scratch.file.to_string_lossy().to_string();
    let wanted = format!("refs/heads/{branch}");
    match &base {
        Some(base) => git(&repo, &["bundle", "create", "--quiet", &target, &wanted, &format!("^{base}")]).await?,
        None => git(&repo, &["bundle", "create", "--quiet", &target, &wanted]).await?,
    };
    let on_disk = std::fs::metadata(&scratch.file).context("could not read the bundle back")?.len();
    if on_disk > wire::PEER_GIT_MAX_BUNDLE_BYTES {
        bail!("the bundle is {} MiB, over the {} MiB limit; send a newer base", on_disk / 1024 / 1024, wire::PEER_GIT_MAX_BUNDLE_BYTES / 1024 / 1024);
    }

    // One chunk is in memory at a time, however large the bundle is.
    let mut bundle = std::fs::File::open(&scratch.file).context("could not read the bundle back")?;
    let mut chunk = vec![0u8; wire::PEER_GIT_CHUNK_BYTES];
    let transfer_id = uuid::Uuid::new_v4().to_string();
    let mut offset = 0u64;
    loop {
        let read = read_full(&mut bundle, &mut chunk).context("could not read the bundle back")?;
        if read == 0 {
            break;
        }
        let params = json!({
            "transferId": transfer_id,
            "offset": offset,
            "data": base64::engine::general_purpose::STANDARD.encode(&chunk[..read]),
        });
        let reply = call_with_retry(mast, wire::PEER_GIT_PUT, params).await.context("sending the bundle")?;
        let received: wire::PeerGitPutReply = serde_json::from_value(reply).context("unexpected reply to a bundle chunk")?;
        offset += read as u64;
        if received.received != offset {
            bail!("the device holds {} bytes after a chunk that should end at {offset}", received.received);
        }
        if offset > wire::PEER_GIT_MAX_BUNDLE_BYTES {
            bail!("the bundle grew past the {} MiB limit while it was sent", wire::PEER_GIT_MAX_BUNDLE_BYTES / 1024 / 1024);
        }
    }
    if offset != on_disk {
        bail!("the bundle file changed while it was sent ({offset} of {on_disk} bytes)");
    }
    let reply = call_with_retry(
        mast,
        wire::PEER_GIT_APPLY,
        json!({ "transferId": transfer_id, "spaceId": space.id, "branch": branch, "size": offset }),
    )
    .await
    .context("the device could not use the bundle")?;
    let applied: wire::PeerGitApplyReply = serde_json::from_value(reply).context("unexpected reply to applying the bundle")?;
    Ok(json!({
        "projectId": applied.space_id,
        "branch": branch,
        "peerBranch": applied.peer_branch,
        "head": applied.head,
        "worktreePath": applied.worktree_path,
        "sentHead": head,
        "bytes": offset,
        "next": "Start the worker with create_chat on this peer: project = projectId, cwd = worktreePath, branch = peerBranch, parent = your own chat id (whoami on your own Mast server). Its commits come back with peer_fetch_branch (base = head).",
    }))
}

/// Fetch what the device's copy of the branch gained into a local branch.
pub async fn fetch_branch(mast: &Mast, args: FetchArgs) -> anyhow::Result<Value> {
    let repo = local_repo(&args.repo)?;
    let branch = plain_branch(args.branch.trim())?.to_string();
    let into = match args.into.as_deref().map(str::trim).filter(|i| !i.is_empty()) {
        Some(into) => plain_branch(into)?.to_string(),
        None => format!("peer/{branch}"),
    };
    git(&repo, &["check-ref-format", "--branch", &into]).await.map_err(|_| anyhow!("{into:?} is not a valid branch name"))?;
    let base = match args.base.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(base) => Some(resolve_commit(&repo, base).await?),
        None => None,
    };
    let space = mast.resolve_space(&args.project).await?;

    let reply = mast
        .call(wire::PEER_GIT_BUNDLE, json!({ "spaceId": space.id, "branch": branch, "base": base }))
        .await
        .context("the device could not bundle the branch")?;
    let bundle: wire::PeerGitBundleReply = serde_json::from_value(reply).context("unexpected reply to bundling the branch")?;
    // The device is trusted with its own branches, not with what we do locally: check what it says.
    if bundle.size > wire::PEER_GIT_MAX_BUNDLE_BYTES {
        bail!("the device announced a bundle over the size limit");
    }
    let sent_ref = bundle
        .ref_name
        .strip_prefix("refs/heads/peer/")
        .and_then(|rest| rest.split_once('/'))
        .filter(|(slug, name)| !slug.is_empty() && slug.chars().all(|c| c.is_ascii_alphanumeric()) && *name == branch);
    if sent_ref.is_none() {
        bail!("the device named a branch other than the one asked for");
    }

    // Straight to the private file, chunk by chunk: the bundle is never held in memory.
    let scratch = Scratch::new("in").context("could not prepare a private folder for the bundle")?;
    let mut stored = scratch.create_file().context("could not store the bundle")?;
    let mut received = 0u64;
    loop {
        let reply = call_with_retry(mast, wire::PEER_GIT_READ, json!({ "transferId": bundle.transfer_id, "offset": received }))
            .await
            .context("receiving the bundle")?;
        let part: wire::PeerGitReadReply = serde_json::from_value(reply).context("unexpected reply to a bundle read")?;
        let data = base64::engine::general_purpose::STANDARD.decode(part.data.as_bytes()).context("the device sent a chunk that is not base64")?;
        received += data.len() as u64;
        if received > wire::PEER_GIT_MAX_BUNDLE_BYTES || received > bundle.size {
            bail!("the device's bundle is over the size it announced or the size limit");
        }
        stored.write_all(&data).context("could not store the bundle")?;
        if part.eof || data.is_empty() {
            break;
        }
    }
    stored.flush().context("could not store the bundle")?;
    drop(stored);
    if received != bundle.size {
        bail!("received {received} bytes of a {} byte bundle", bundle.size);
    }
    let file = scratch.file.to_string_lossy().to_string();
    git(&repo, &["bundle", "verify", &file]).await.context("the bundle does not apply to this repository (is the base commit here?)")?;
    let force = if args.force { "+" } else { "" };
    let refspec = format!("{force}{}:refs/heads/{into}", bundle.ref_name);
    git(&repo, &["-c", "transfer.fsckObjects=true", "fetch", "--no-tags", "--quiet", &file, &refspec])
        .await
        .map_err(|error| anyhow!("{error}. {into} is checked out, or has commits the device's branch does not (pass force to move it)"))?;
    let head = resolve_commit(&repo, &format!("refs/heads/{into}")).await?;
    let commits = match &base {
        Some(base) => git(&repo, &["rev-list", "--count", &format!("{base}..{head}")]).await.ok().and_then(|n| n.parse::<u64>().ok()),
        None => None,
    };
    Ok(json!({
        "branch": into,
        "head": head,
        "deviceHead": bundle.head,
        "newCommits": commits,
        "bytes": received,
        "next": format!("Review and merge {into} yourself; nothing was merged or pushed."),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names_with_options_or_revision_syntax_are_refused() {
        for ok in ["work", "feature/x", "a.b_c-d"] {
            assert!(plain_branch(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-delete", "--upload-pack=x", "/abs", "a..b", "a b", "HEAD~1", "x^", "a:b", ".hidden", "a//b", "end/"] {
            assert!(plain_branch(bad).is_err(), "{bad:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_scratch_bundle_sits_in_a_folder_only_its_owner_can_open() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new("in").unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&scratch.dir), 0o700);
        let mut file = scratch.create_file().unwrap();
        file.write_all(b"bundle").unwrap();
        assert_eq!(mode(&scratch.file), 0o600);
        assert!(scratch.create_file().is_err(), "an existing file is never reused");
        assert_ne!(Scratch::new("in").unwrap().dir, scratch.dir, "each transfer gets its own folder");
        let dir = scratch.dir.clone();
        drop(scratch);
        assert!(!dir.exists(), "the folder goes with the transfer");
    }

    #[test]
    fn a_bundle_is_read_in_chunk_sized_pieces_even_when_the_reader_returns_short_reads() {
        // A reader that hands out at most 3 bytes at a time, like a pipe or a slow disk.
        struct Short<'a>(&'a [u8]);
        impl Read for Short<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.0.len().min(buf.len()).min(3);
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let mut reader = Short(b"0123456789");
        let mut chunk = [0u8; 4];
        let mut seen = Vec::new();
        loop {
            let read = read_full(&mut reader, &mut chunk).unwrap();
            if read == 0 {
                break;
            }
            seen.push(chunk[..read].to_vec());
        }
        assert_eq!(seen, [b"0123".to_vec(), b"4567".to_vec(), b"89".to_vec()]);
    }

    #[test]
    fn the_repository_must_be_an_absolute_existing_folder() {
        assert!(local_repo("relative/path").is_err());
        assert!(local_repo("/definitely/not/here").is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(local_repo(dir.path().to_str().unwrap()).is_ok());
    }
}
