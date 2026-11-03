use super::*;

pub(super) fn comment_state_key(
    comments: &[ReviewComment],
    draft: Option<&(String, CommentSide, u32)>,
) -> u64 {
    let mut parts: Vec<String> = comments
        .iter()
        .flat_map(|comment| [comment.id.clone(), comment.body.clone()])
        .collect();
    if let Some((path, side, line)) = draft {
        parts.push(format!("draft:{path}:{}:{line}", side.tag()));
    }
    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    hash64(&refs)
}

pub(super) fn hash64(parts: &[&str]) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    for p in parts {
        p.hash(&mut hasher);
    }
    hasher.finish()
}

/// Stable identity for the changed lines of a hunk. Context and line numbers
/// are excluded so an unrelated edit above the hunk does not invalidate it.
pub(super) fn hunk_identity(path: &str, hunk: &Hunk) -> String {
    use sha2::{Digest, Sha256};

    let mut hash = Sha256::new();
    hash.update((path.len() as u64).to_be_bytes());
    hash.update(path.as_bytes());
    for line in &hunk.lines {
        let tag = match line.kind {
            LineKind::Add => b'+',
            LineKind::Del => b'-',
            LineKind::Context | LineKind::Meta => continue,
        };
        hash.update([tag]);
        hash.update((line.text.len() as u64).to_be_bytes());
        hash.update(line.text.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_identity_survives_line_and_context_changes() {
        let first = parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n context\n-old\n+new\n");
        let moved = parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -20,2 +20,2 @@\n different context\n-old\n+new\n");
        let changed = parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -20 +20 @@\n-old\n+other\n");
        let id = hunk_identity("a", &first[0].hunks[0]);
        assert_eq!(id, hunk_identity("a", &moved[0].hunks[0]));
        assert_ne!(id, hunk_identity("a", &changed[0].hunks[0]));
        assert_ne!(id, hunk_identity("b", &first[0].hunks[0]));
    }
}
