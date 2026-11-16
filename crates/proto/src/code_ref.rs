//! Structured locations attached to a user message.

use serde::{Deserialize, Serialize};

/// Versioned code location. The referenced text travels separately from this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRef {
    pub version: u32,
    #[serde rename = "where"]
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
    pub path: String,
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
    Commit { commit: String },
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
            location: CodeRefLocation { repo_id: "repo".into(), worktree_id: None, path: "src/main.rs".into() },
            kind: CodeRefKind::Lines,
            range: Some(CodeRefRange { start_line: 2, end_line: 4 }),
            symbol: None,
            at: CodeRefRevision::WorkingTree { working_tree: true, content_hash: "hash".into() },
        };
        let value = serde_json::to_value(&reference).unwrap();
        assert_eq!(value["where"]["repoId"], "repo");
        assert_eq!(value["range"]["startLine"], 2);
        assert_eq!(value["at"]["workingTree"], true);
        assert_eq!(serde_json::from_value::<CodeRef>(value).unwrap(), reference);
    }
}
