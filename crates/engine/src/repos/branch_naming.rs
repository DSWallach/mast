/// Branch prefix of worktrees Mast creates (`mast/<name>`). Release, `work/*`,
/// `int/*` branches and everything under `refs/mast/*` are never ours.
pub(crate) const BRANCH_PREFIX: &str = "mast/";

/// A branch this app created for a worktree.
pub(crate) fn is_owned_branch(branch: &str) -> bool {
    branch.starts_with(BRANCH_PREFIX)
}

/// Whether `branch` is the original auto-created branch of the worktree
/// folder `folder`.
pub(crate) fn is_owned_branch_of(branch: &str, folder: &str) -> bool {
    branch.strip_prefix(BRANCH_PREFIX) == Some(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_mast_prefixed_branches_are_owned() {
        assert!(is_owned_branch("mast/swift-otter"));
        for b in [
            "v0.3.0",
            "int/v0.3.0/x",
            "work/v030-x",
            "zeron/swift-otter",
            "refs/mast/pr/9/head",
            "refs/mast/int/x",
        ] {
            assert!(!is_owned_branch(b), "{b}");
        }
        assert!(!is_owned_branch_of("zeron/swift-otter", "swift-otter"));
    }
}

pub(super) const ADJECTIVES: &[&str] = &[
    "swift", "calm", "bright", "bold", "keen", "brave", "clever", "lucky", "quiet", "warm", "cool",
    "sharp", "gentle", "vivid", "amber", "cobalt",
];
pub(super) const NOUNS: &[&str] = &[
    "otter", "harbor", "falcon", "cedar", "meadow", "zeron", "delta", "ember", "lynx", "maple",
    "onyx", "quartz", "raven", "summit", "willow", "aspen",
];

/// Turn a generated chat title into the semantic portion of a Mast branch
/// (port of zeron's `worktreeBranchFromTitle`). Mast NFKD-normalizes accented
/// letters first; native keeps it ASCII-only (generated titles are Title Case
/// English), so non-ASCII characters collapse into the `-` separator.
pub fn worktree_branch_from_title(title: &str) -> String {
    let mut slug = String::new();
    for c in title.trim().chars() {
        if matches!(c, '\'' | '"' | '`') {
            continue; // dropped entirely (cafe's → cafes), not a separator
        }
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(48);
    let slug = slug.trim_matches('-');
    format!(
        "{BRANCH_PREFIX}{}",
        if slug.is_empty() { "update" } else { slug }
    )
}
