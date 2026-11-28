//! Folds one land request's changelog fragment into the merge commit.
//!
//! A repository with a root `CHANGELOG.md` gets the fragment appended to its
//! `Unreleased` section, and the item's `docs/changelog.d/<workItemId>.md` is
//! removed. A repository without one keeps the fragment convention: the entry is
//! written to `docs/changelog.d/<workItemId>.md`. Lands on one target are
//! serialized and each folds on top of the previous merge, so two fragments
//! never conflict.
//!
//! The merged tree is worker-controlled, so nothing is read or written through a
//! symbolic link.

use super::MergeQueueError;
use std::path::{Path, PathBuf};

/// Longest changelog fragment accepted, in bytes.
pub const MAX_FRAGMENT_BYTES: usize = 4096;

const CHANGELOG: &str = "CHANGELOG.md";
const FRAGMENT_DIR: &str = "docs/changelog.d";

/// `fragment` as a Markdown list entry: a bullet, with continuation lines
/// indented so none of them can start a heading. Empty when there is no text.
pub fn normalize_fragment(fragment: &str) -> String {
    let mut out = String::new();
    for line in fragment.trim().lines().map(str::trim_end) {
        if line.is_empty() {
            continue;
        }
        if out.is_empty() {
            if !line.starts_with("- ") && !line.starts_with("* ") {
                out.push_str("- ");
            }
        } else {
            out.push('\n');
            // A further top-level bullet stays a bullet.
            if !line.starts_with("- ") && !line.starts_with("* ") && !line.starts_with("  ") {
                out.push_str("  ");
            }
        }
        out.push_str(line);
    }
    out
}

fn is_unreleased_heading(line: &str) -> bool {
    line.strip_prefix("## ").is_some_and(|title| {
        let title = title.trim().trim_start_matches('[');
        title.to_ascii_lowercase().starts_with("unreleased")
    })
}

/// `changelog` with `entry` at the end of its `Unreleased` section, creating the
/// section above the newest release when there is none.
pub fn fold_into_changelog(changelog: &str, entry: &str) -> String {
    let lines: Vec<&str> = changelog.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 4);
    if let Some(start) = lines.iter().position(|line| is_unreleased_heading(line)) {
        let end = lines[start + 1..]
            .iter()
            .position(|line| line.starts_with("## "))
            .map_or(lines.len(), |offset| start + 1 + offset);
        let mut last = end;
        while last > start + 1 && lines[last - 1].trim().is_empty() {
            last -= 1;
        }
        out.extend(lines[..last].iter().map(|l| (*l).to_owned()));
        if last == start + 1 {
            out.push(String::new());
        }
        out.push(entry.to_owned());
        if end < lines.len() {
            out.push(String::new());
            out.extend(lines[end..].iter().map(|l| (*l).to_owned()));
        }
    } else {
        let at = lines
            .iter()
            .position(|line| line.starts_with("## "))
            .unwrap_or(lines.len());
        out.extend(lines[..at].iter().map(|l| (*l).to_owned()));
        if out.last().is_some_and(|line| !line.trim().is_empty()) {
            out.push(String::new());
        }
        out.extend(["## Unreleased".to_owned(), String::new(), entry.to_owned()]);
        if at < lines.len() {
            out.push(String::new());
            out.extend(lines[at..].iter().map(|l| (*l).to_owned()));
        }
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

fn refused(path: &Path, what: &str) -> MergeQueueError {
    MergeQueueError::Invalid(format!(
        "changelog folding refused: {} {what}",
        path.display()
    ))
}

/// Whether `relative` is a regular file under `root`. Errors when it, or a
/// directory on the way to it, is a symbolic link or the wrong kind of file.
fn regular_file(root: &Path, relative: &str) -> Result<bool, MergeQueueError> {
    let mut path = root.to_path_buf();
    let mut parts = relative.split('/').peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(refused(&path, &error.to_string())),
        };
        if meta.file_type().is_symlink() {
            return Err(refused(&path, "is a symbolic link"));
        }
        let is_last = parts.peek().is_none();
        if is_last && !meta.is_file() {
            return Err(refused(&path, "is not a regular file"));
        }
        if !is_last && !meta.is_dir() {
            return Err(refused(&path, "is not a directory"));
        }
    }
    Ok(true)
}

fn read(path: &Path) -> Result<String, MergeQueueError> {
    std::fs::read_to_string(path).map_err(|e| refused(path, &e.to_string()))
}

fn write(path: &Path, text: &str) -> Result<(), MergeQueueError> {
    std::fs::write(path, text).map_err(|e| refused(path, &e.to_string()))
}

/// Apply the fragment of `work_item_id` to the merged tree at `worktree`.
/// `fragment` is the one sent with the land request; without it the branch's own
/// `docs/changelog.d/<workItemId>.md` is used. Returns the paths changed, for
/// staging. `work_item_id` must already be a safe file name.
pub(super) fn fold(
    worktree: &Path,
    work_item_id: &str,
    fragment: Option<&str>,
) -> Result<Vec<PathBuf>, MergeQueueError> {
    let fragment_path = format!("{FRAGMENT_DIR}/{work_item_id}.md");
    let has_changelog = regular_file(worktree, CHANGELOG)?;
    let has_fragment_file = regular_file(worktree, &fragment_path)?;
    let sent = fragment.map(normalize_fragment).filter(|e| !e.is_empty());
    let entry = match &sent {
        Some(entry) => entry.clone(),
        None if has_fragment_file => {
            let text = read(&worktree.join(&fragment_path))?;
            if text.len() > MAX_FRAGMENT_BYTES {
                return Err(refused(Path::new(&fragment_path), "is too large"));
            }
            normalize_fragment(&text)
        }
        None => return Ok(Vec::new()),
    };
    if entry.is_empty() {
        return Ok(Vec::new());
    }
    let mut changed = Vec::new();
    if has_changelog {
        let path = worktree.join(CHANGELOG);
        write(&path, &fold_into_changelog(&read(&path)?, &entry))?;
        changed.push(PathBuf::from(CHANGELOG));
        if has_fragment_file {
            let path = worktree.join(&fragment_path);
            std::fs::remove_file(&path).map_err(|e| refused(&path, &e.to_string()))?;
            changed.push(PathBuf::from(&fragment_path));
        }
    } else if sent.is_some() {
        // `regular_file` has already refused a linked `docs` or fragment folder.
        let directory = worktree.join(FRAGMENT_DIR);
        std::fs::create_dir_all(&directory).map_err(|e| refused(&directory, &e.to_string()))?;
        write(&worktree.join(&fragment_path), &format!("{entry}\n"))?;
        changed.push(PathBuf::from(&fragment_path));
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_become_list_entries_without_headings() {
        assert_eq!(normalize_fragment("  Add a thing.\n"), "- Add a thing.");
        assert_eq!(
            normalize_fragment("- One\n\n## Not a heading\n- Two"),
            "- One\n  ## Not a heading\n- Two"
        );
        assert_eq!(normalize_fragment(" \n "), "");
    }

    #[test]
    fn entries_append_to_the_unreleased_section_in_land_order() {
        let start = "# Changelog\n\n## [Unreleased]\n\n## 0.2.0\n\n- Old\n";
        let one = fold_into_changelog(start, "- A");
        let two = fold_into_changelog(&one, "- B");
        assert_eq!(
            two,
            "# Changelog\n\n## [Unreleased]\n\n- A\n- B\n\n## 0.2.0\n\n- Old\n"
        );
    }

    #[test]
    fn a_missing_unreleased_section_goes_above_the_newest_release() {
        assert_eq!(
            fold_into_changelog("# Changelog\n## 0.2.0\n- Old\n", "- A"),
            "# Changelog\n\n## Unreleased\n\n- A\n\n## 0.2.0\n- Old\n"
        );
        assert_eq!(fold_into_changelog("", "- A"), "## Unreleased\n\n- A\n");
    }

    #[test]
    fn a_linked_changelog_is_never_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.md");
        std::fs::write(&outside, "keep").unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join(CHANGELOG)).unwrap();
        assert!(fold(&tree, "item", Some("A")).is_err());
        std::fs::remove_file(tree.join(CHANGELOG)).unwrap();
        std::os::unix::fs::symlink(dir.path(), tree.join("docs")).unwrap();
        assert!(fold(&tree, "item", Some("A")).is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
        assert!(!dir.path().join("changelog.d").exists());
    }

    #[test]
    fn without_a_changelog_the_fragment_file_is_kept_or_written() {
        let dir = tempfile::tempdir().unwrap();
        assert!(fold(dir.path(), "item", None).unwrap().is_empty());
        let changed = fold(dir.path(), "item", Some("A")).unwrap();
        assert_eq!(changed, vec![PathBuf::from("docs/changelog.d/item.md")]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(&changed[0])).unwrap(),
            "- A\n"
        );
        // The branch's own file stands when nothing was sent.
        assert!(fold(dir.path(), "item", None).unwrap().is_empty());
    }
}
