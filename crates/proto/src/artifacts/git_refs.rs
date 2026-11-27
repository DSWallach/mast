//! The unified `refs/mast/*` Git namespace. Everything Mast writes into a
//! repository outside `refs/heads/` lives under one of four sub-namespaces,
//! so it never collides with user branches or Mast-owned worktree branches.

/// Root of every Mast-owned ref.
pub const REFS_MAST: &str = "refs/mast";
/// Pull-request heads fetched for diffing: `refs/mast/pr/<n>/head`.
pub const REFS_MAST_PR: &str = "refs/mast/pr";
/// Work-in-progress snapshots: `refs/mast/wip/*`.
pub const REFS_MAST_WIP: &str = "refs/mast/wip";
/// Integration (merge-queue) refs: `refs/mast/int/*`.
pub const REFS_MAST_INT: &str = "refs/mast/int";
/// Build refs pushed to build hosts: `refs/mast/build/*`.
pub const REFS_MAST_BUILD: &str = "refs/mast/build";

/// The ref a fetched pull-request head is stored under.
pub fn pr_head_ref(number: u64) -> String {
    format!("{REFS_MAST_PR}/{number}/head")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_is_unified_under_refs_mast() {
        for ns in [REFS_MAST_PR, REFS_MAST_WIP, REFS_MAST_INT, REFS_MAST_BUILD] {
            assert!(ns.starts_with("refs/mast/"), "{ns}");
        }
        assert_eq!(pr_head_ref(9), "refs/mast/pr/9/head");
    }
}
