use super::*;

/// Best-effort home directory (the `ListFolders` default and worktree root base).
pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Whether a path is too broad for automatic recursive filesystem work. A
/// saved Space may legitimately retain HOME as historical chat context, but
/// background indexing/watching it would reach macOS-protected descendants
/// (Music, Pictures/Photos Library, Documents, and so on). Ancestors of HOME
/// are broader still. Relative paths are rejected because their scope depends
/// on the process cwd.
pub(crate) fn is_broad_workspace_root(path: &Path) -> bool {
    is_broad_workspace_root_with_home(path, &home_dir())
}

/// The same policy after resolving an existing path. Use this only on an
/// explicit filesystem operation (not a background scan): canonicalization
/// catches a saved symlink/`..` alias of HOME before a recursive consumer is
/// allowed to proceed.
pub(crate) fn is_broad_workspace_root_resolved(path: &Path) -> bool {
    if is_broad_workspace_root(path) {
        return true;
    }
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return false;
    };
    let home = home_dir();
    let resolved_home = std::fs::canonicalize(&home).unwrap_or(home);
    is_broad_workspace_root_with_home(&resolved, &resolved_home)
}

pub(super) fn is_broad_workspace_root_with_home(path: &Path, home: &Path) -> bool {
    !path.is_absolute() || home.starts_with(path)
}

/// Whether unattended/background work must leave this root alone. In addition
/// to HOME and its ancestors, this covers macOS privacy-managed HOME subtrees.
/// A person may still explicitly open a project there; this guard is for boot
/// reconciliation, imported-history discovery, and other automatic activity.
pub fn is_automatic_access_blocked(path: &Path) -> bool {
    is_automatic_access_blocked_with_home(path, &home_dir())
}

pub(super) fn is_automatic_access_blocked_with_home(path: &Path, home: &Path) -> bool {
    if is_broad_workspace_root_with_home(path, home) {
        return true;
    }
    let Ok(relative) = path.strip_prefix(home) else {
        return false;
    };
    let Some(std::path::Component::Normal(first)) = relative.components().next() else {
        return false;
    };
    matches!(
        first.to_str(),
        Some("Desktop" | "Documents" | "Downloads" | "Movies" | "Music" | "Pictures" | "Library")
    )
}

/// A first-level HOME entry whose metadata must not be probed merely to render
/// the project picker. On macOS even a `stat`/`.git` existence check against
/// these privacy-managed folders can raise a TCC prompt. Entering the folder is
/// still allowed when the person explicitly selects it in the picker.
pub(super) fn is_protected_home_child_with_home(target: &Path, name: &str, home: &Path) -> bool {
    target == home
        && matches!(
            name,
            "Desktop" | "Documents" | "Downloads" | "Movies" | "Music" | "Pictures" | "Library"
        )
}
