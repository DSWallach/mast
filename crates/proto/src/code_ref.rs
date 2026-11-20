//! Structured locations attached to a user message.

use serde::{Deserialize, Serialize};

/// Versioned code location. The referenced text travels separately from this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRef {
    pub version: u32,
    #[serde(rename = "where")]
    pub location: CodeRefLocation,
    pub kind: CodeRefKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<CodeRefRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<CodeRefSymbol>,
    pub at: CodeRefRevision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRefLocation {
    pub repo_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    /// Checkout-relative root of the nested (submodule) repository that `path`
    /// and the revision belong to. Absent for the checkout's own repository and
    /// for references written before nested support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    /// Repository-relative when `repo_root` is set, else checkout-relative.
    pub path: String,
}

impl CodeRefLocation {
    /// Checkout-relative path, for workspace reads and navigation.
    pub fn workspace_path(&self) -> String {
        match self.repo_root.as_deref().filter(|root| !root.is_empty()) {
            Some(root) => format!("{}/{}", root.trim_end_matches('/'), self.path),
            None => self.path.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CodeRefKind {
    Lines,
    Symbol,
    Hunk,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRefRange {
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRefSymbol {
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, rename_all = "camelCase")]
pub enum CodeRefRevision {
    Commit {
        commit: String,
    },
    WorkingTree {
        #[serde(rename = "workingTree")]
        working_tree: bool,
        #[serde(rename = "contentHash")]
        content_hash: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_round_trips_with_camel_case_fields() {
        let reference = CodeRef {
            version: 1,
            location: CodeRefLocation {
                repo_id: "repo".into(),
                worktree_id: None,
                repo_root: None,
                path: "src/main.rs".into(),
            },
            kind: CodeRefKind::Lines,
            range: Some(CodeRefRange {
                start_line: 2,
                end_line: 4,
            }),
            symbol: None,
            at: CodeRefRevision::WorkingTree {
                working_tree: true,
                content_hash: "hash".into(),
            },
        };
        let value = serde_json::to_value(&reference).unwrap();
        assert_eq!(value["where"]["repoId"], "repo");
        assert_eq!(value["range"]["startLine"], 2);
        assert_eq!(value["at"]["workingTree"], true);
        assert_eq!(serde_json::from_value::<CodeRef>(value).unwrap(), reference);
    }

    #[test]
    fn reference_without_repo_root_still_deserializes() {
        let old = serde_json::json!({
            "version": 1,
            "where": {"repoId": "repo", "path": "a.rs"},
            "kind": "hunk",
            "at": {"commit": "abc"}
        });
        let reference: CodeRef = serde_json::from_value(old).unwrap();
        assert_eq!(reference.location.repo_root, None);
        assert_eq!(reference.location.workspace_path(), "a.rs");
        let serialized = serde_json::to_value(&reference).unwrap();
        assert!(serialized["where"].get("repoRoot").is_none());
    }

    #[test]
    fn nested_location_joins_root_and_path() {
        let location = CodeRefLocation {
            repo_id: "r".into(),
            worktree_id: None,
            repo_root: Some("ui/".into()),
            path: "src/a.rs".into(),
        };
        assert_eq!(location.workspace_path(), "ui/src/a.rs");
        let value = serde_json::to_value(&location).unwrap();
        assert_eq!(value["repoRoot"], "ui/");
    }
}
