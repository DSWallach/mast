use super::*;
use mast_proto::{CheckoutGitStatus, GitFileState, GitFileStatus};

fn state(code: u8) -> Option<GitFileState> {
    Some(match code {
        b' ' => GitFileState::Unchanged,
        b'A' => GitFileState::Added,
        b'M' => GitFileState::Modified,
        b'D' => GitFileState::Deleted,
        b'R' => GitFileState::Renamed,
        b'C' => GitFileState::Copied,
        b'U' => GitFileState::Unmerged,
        b'?' => GitFileState::Untracked,
        b'T' => GitFileState::TypeChanged,
        _ => return None,
    })
}

/// Porcelain -z paths are literal (including spaces/newlines), destination first
/// for renames. Never turn a truncated record or invalid UTF-8 into a clean row.
pub(super) fn parse(bytes: &[u8], truncated: bool) -> (Vec<GitFileStatus>, bool) {
    let mut complete = !truncated && (bytes.is_empty() || bytes.ends_with(&[0]));
    let mut records = bytes.split(|b| *b == 0).peekable();
    let mut files = Vec::new();
    while let Some(record) = records.next() {
        if record.is_empty() && records.peek().is_none() {
            break;
        }
        if record.len() < 4 || record[2] != b' ' {
            complete = false;
            break;
        }
        let renamed = record[..2].iter().any(|code| matches!(code, b'R' | b'C'));
        let old = if renamed { records.next() } else { None };
        let (Some(index), Some(worktree)) = (state(record[0]), state(record[1])) else {
            complete = false;
            continue;
        };
        let Ok(path) = std::str::from_utf8(&record[3..]) else {
            complete = false;
            continue;
        };
        let old_path = match old.map(std::str::from_utf8) {
            Some(Ok(path)) if !path.is_empty() => Some(path.to_owned()),
            None if !renamed => None,
            _ => {
                complete = false;
                continue;
            }
        };
        files.push(GitFileStatus {
            path: path.into(),
            old_path,
            index,
            worktree,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    // A staged deletion followed by an untracked replacement has two records
    // for one path. Preserve both sides instead of letting the last one win.
    let mut merged: Vec<GitFileStatus> = Vec::with_capacity(files.len());
    for file in files {
        if let Some(previous) = merged.last_mut().filter(|p| p.path == file.path) {
            if !matches!(
                file.index,
                GitFileState::Unchanged | GitFileState::Untracked
            ) {
                previous.index = file.index;
            }
            if file.worktree != GitFileState::Unchanged {
                previous.worktree = file.worktree;
            }
            previous.old_path = previous.old_path.take().or(file.old_path);
        } else {
            merged.push(file);
        }
    }
    (merged, complete)
}

pub(super) fn publish(
    inner: &DiffSyncInner,
    entry: &CheckoutEntry,
    files: Vec<GitFileStatus>,
    complete: bool,
) {
    // Hold the entries lock through publication so a removed checkout cannot
    // be resurrected by an in-flight capture.
    let entries = lock(&inner.entries);
    if !entries.contains_key(&entry.identity.id) {
        return;
    }
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(&files).expect("Git status serialization"));
    hasher.update([u8::from(complete)]);
    let next = CheckoutGitStatus {
        checkout_id: entry.identity.id.clone(),
        device_id: inner.device_id.clone(),
        revision: crate::repos::hex(&hasher.finalize()),
        complete,
        files,
    };
    inner.statuses_tx.send_if_modified(|statuses| {
        if let Some(previous) = statuses
            .iter_mut()
            .find(|s| s.checkout_id == next.checkout_id)
        {
            if *previous == next {
                return false;
            }
            *previous = next;
        } else {
            statuses.push(next);
            statuses.sort_by(|a, b| a.checkout_id.cmp(&b.checkout_id));
        }
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_preserves_both_columns_and_literal_rename_paths() {
        let (files, complete) = parse(
            b"MM src/main.rs\0 R new\n name\0 old name\0?? new/child.rs\0UU conflict\0",
            false,
        );
        assert!(complete);
        let modified = files.iter().find(|f| f.path == "src/main.rs").unwrap();
        assert_eq!(modified.index, GitFileState::Modified);
        assert_eq!(modified.worktree, GitFileState::Modified);
        let rename = files.iter().find(|f| f.path == "new\n name").unwrap();
        assert_eq!(rename.old_path.as_deref(), Some(" old name"));
        assert_eq!(rename.worktree, GitFileState::Renamed);
        assert!(
            files
                .iter()
                .any(|f| f.path == "new/child.rs" && f.index == GitFileState::Untracked)
        );
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // fixture git commands are tests
    fn v2_status_converts_to_exactly_what_v1_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let git = |args: &[&str]| -> Vec<u8> {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            out.stdout
        };
        git(&["init", "-q", "-b", "main"]);
        for name in ["keep.txt", "mod.txt", "gone.txt", "old name.txt", "both.txt"] {
            std::fs::write(dir.join(name), format!("{name}\n".repeat(20))).unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "one"]);
        std::fs::write(dir.join("mod.txt"), "changed\n").unwrap();
        std::fs::write(dir.join("both.txt"), "staged\n").unwrap();
        git(&["add", "both.txt"]);
        std::fs::write(dir.join("both.txt"), "staged and more\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        git(&["mv", "old name.txt", "new name.txt"]);
        std::fs::write(dir.join("added.txt"), "a\n").unwrap();
        git(&["add", "added.txt"]);
        std::fs::create_dir_all(dir.join("sub dir")).unwrap();
        std::fs::write(dir.join("sub dir/untracked file.rs"), "u\n").unwrap();
        let v1 = git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
        let v2 = git(&["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"]);
        assert_eq!(v2_to_v1(&v2).expect("convertible"), v1);
        assert_eq!(v2_to_v1(b"# branch.oid x\0"), Some(Vec::new()));
        assert_eq!(v2_to_v1(b"bogus record\0"), None);
    }

    #[test]
    fn incomplete_status_is_never_reported_as_clean() {
        for bytes in [b" M truncated".as_slice(), b"R  new\0", b" M \xff\0"] {
            assert!(!parse(bytes, false).1);
        }
        assert!(!parse(b"", true).1);
        assert_eq!(parse(b"", false), (vec![], true));
    }

    #[test]
    fn staged_deletion_and_untracked_replacement_share_one_status() {
        let (files, complete) = parse(b"D  replaced\0?? replaced\0", false);
        assert!(complete);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].index, GitFileState::Deleted);
        assert_eq!(files[0].worktree, GitFileState::Untracked);
    }
}

/// Rewrite `status --porcelain=v2 --branch -z` output as the `--porcelain=v1 -z`
/// bytes the capture parses, so one `git status` can serve both. `None` for
/// anything unexpected (the caller then runs its own v1 status).
pub(super) fn v2_to_v1(v2: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(v2.len());
    let mut records = v2.split(|b| *b == 0).peekable();
    while let Some(record) = records.next() {
        if record.is_empty() {
            if records.peek().is_none() {
                break;
            }
            return None;
        }
        let record = std::str::from_utf8(record).ok()?;
        let (fields, with_origin) = match record.as_bytes()[0] {
            b'#' => continue,
            b'?' => {
                out.extend_from_slice(b"?? ");
                out.extend_from_slice(record.get(2..)?.as_bytes());
                out.push(0);
                continue;
            }
            b'1' => (9, false),
            b'2' => (10, true),
            b'u' => (11, false),
            _ => return None,
        };
        let xy = record.get(2..4)?;
        let path = record.splitn(fields, ' ').last()?;
        out.extend(xy.bytes().map(|c| if c == b'.' { b' ' } else { c }));
        out.push(b' ');
        out.extend_from_slice(path.as_bytes());
        out.push(0);
        if with_origin {
            out.extend_from_slice(records.next()?);
            out.push(0);
        }
    }
    Some(out)
}
