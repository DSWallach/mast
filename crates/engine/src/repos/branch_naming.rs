/// Branch prefix of worktrees Mast creates (`mast/<name>`). Worktrees made
/// before the rename carry `zeron/<name>` and are still recognised as ours:
/// they are renamed from the chat title and deleted with their worktree.
pub(crate) const BRANCH_PREFIX: &str = "mast/";
pub(crate) const LEGACY_BRANCH_PREFIX: &str = "zeron/";

/// A branch this app created for a worktree, under either spelling.
pub(crate) fn is_owned_branch(branch: &str) -> bool {
    branch.starts_with(BRANCH_PREFIX) || branch.starts_with(LEGACY_BRANCH_PREFIX)
}

/// Whether `branch` is the original auto-created branch of the worktree
/// folder `folder` (either prefix).
pub(crate) fn is_owned_branch_of(branch: &str, folder: &str) -> bool {
    branch.strip_prefix(BRANCH_PREFIX) == Some(folder)
        || branch.strip_prefix(LEGACY_BRANCH_PREFIX) == Some(folder)
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
