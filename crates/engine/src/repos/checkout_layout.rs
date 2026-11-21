use super::*;

/// Canonical (worktree root, git dir) of the checkout containing `path`, read
/// from the `.git` entry nearest `path` without spawning git. `None` means
/// "ask git": nothing found, `GIT_DIR`/`GIT_WORK_TREE` set, `path` inside a git
/// dir, a `.git` that is not a valid git dir, or an unreadable gitdir file.
pub(super) fn checkout_layout_from_fs(path: &Path) -> Option<(PathBuf, PathBuf)> {
    if std::env::var_os("GIT_DIR").is_some() || std::env::var_os("GIT_WORK_TREE").is_some() {
        return None;
    }
    let start = std::fs::canonicalize(path).ok()?;
    if start.components().any(|c| c.as_os_str() == ".git") {
        return None;
    }
    let start = if start.is_dir() { start } else { start.parent()?.to_path_buf() };
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
            continue;
        };
        let git_dir = if meta.is_dir() {
            dot_git
        } else if meta.is_file() {
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
            if target.is_empty() {
                return None;
            }
            let target = Path::new(target);
            if target.is_absolute() { target.to_path_buf() } else { dir.join(target) }
        } else {
            return None;
        };
        let git_dir = std::fs::canonicalize(git_dir).ok()?;
        // Same test git applies before accepting a directory as a git dir.
        if !git_dir.join("HEAD").is_file()
            || (!git_dir.join("objects").is_dir() && !git_dir.join("commondir").is_file()) {
            return None;
        }
        return Some((dir.to_path_buf(), git_dir));
    }
    None
}

/// [`checkout_layout_from_fs`] on the blocking pool, so a wedged mount cannot
/// stall a runtime worker.
pub(super) async fn checkout_layout(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || checkout_layout_from_fs(&path))
        .await
        .ok()
        .flatten()
}

/// Canonical identity shared by every chat operating in this exact worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutIdentity {
    /// `sha256(deviceId ‖ NUL ‖ canonical git dir)` — device-scoped, path-stable.
    pub id: String,
    /// Canonical worktree root (`rev-parse --show-toplevel`, symlinks resolved).
    pub root: PathBuf,
    /// Canonical git dir (worktree-specific for linked worktrees).
    pub git_dir: PathBuf,
}
