//! Friction reports recorded by agents and automatic capture.

use crate::RequestLink;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrictionKind {
    Bug,
    ToolFailure,
    Permission,
    Environment,
    Workflow,
    Docs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrictionSeverity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrictionStatus {
    Open,
    Dismissed,
    Promoted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrictionSource {
    Agent,
    Auto,
    Import,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrictionApproval {
    pub command: String,
    pub why: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrictionProvenance {
    pub chat_id: String,
    pub harness: String,
    pub model: Option<String>,
    pub space_id: String,
    pub work_item_id: Option<String>,
    pub branch: Option<String>,
    pub created_at: i64,
    pub source: FrictionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrictionOccurrence {
    pub at: i64,
    pub chat_id: String,
    pub space_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrictionReport {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_name: Option<String>,
    pub kind: FrictionKind,
    pub summary: String,
    pub what_happened: String,
    pub workaround: Option<String>,
    pub suggestion: Option<String>,
    pub approval: Option<FrictionApproval>,
    pub severity: Option<FrictionSeverity>,
    #[serde(default)]
    pub links: Vec<RequestLink>,
    pub provenance: FrictionProvenance,
    pub fingerprint: String,
    pub occurrences: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub occurrence_history: Vec<FrictionOccurrence>,
    pub last_seen: i64,
    pub status: FrictionStatus,
    #[serde(default)]
    pub promoted_item_id: Option<String>,
    #[serde(default)]
    pub promoted_at: Option<i64>,
}

/// Whether a report is sufficiently specific to Mast to share across Spaces.
pub fn friction_is_about_mast(report: &FrictionReport) -> bool {
    if !matches!(report.kind, FrictionKind::Bug | FrictionKind::ToolFailure) {
        return false;
    }
    let names_tool = |text: &str| {
        text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|word| crate::agent::agent_tools::USAGE.iter().any(|usage| usage.tool == word)
                || crate::agent::agent_tools::tools::MCP_ONLY.contains(&word))
    };
    names_tool(&report.summary) || names_tool(&report.what_happened)
        || report.links.iter().any(|link| match &link.target {
            crate::RequestLinkTarget::Url { url } | crate::RequestLinkTarget::Pr { url } => {
                url.starts_with("mast://") || url.starts_with("mast-artifact:")
                    || url.starts_with("https://github.com/Pluto8195/mast/issues/")
                    || url.starts_with("https://github.com/Pluto8195/mast/pull/")
            }
            _ => false,
        })
}

#[cfg(test)]
mod mast_filter_tests {
    use super::*;

    fn report(kind: &str, summary: &str, detail: &str, source: &str) -> FrictionReport {
        serde_json::from_value(serde_json::json!({
            "id": "report", "kind": kind, "summary": summary, "whatHappened": detail,
            "workaround": null, "suggestion": null, "approval": null, "severity": null,
            "provenance": {"chatId": "chat", "harness": "test", "model": null,
                "spaceId": "space", "workItemId": null, "branch": null,
                "createdAt": 1, "source": source},
            "fingerprint": "report", "occurrences": 1, "lastSeen": 1, "status": "open"
        })).unwrap()
    }

    #[test]
    fn recognizes_registered_tools_and_mast_links() {
        assert!(friction_is_about_mast(&report("tool_failure", "review_publish refused: failed", "error", "auto")));
        assert!(friction_is_about_mast(&report("bug", "Failed", "create_chat returned an error", "agent")));
        assert!(friction_is_about_mast(&report("bug", "Failed", "work_outline_read_current failed", "agent")));
        assert!(friction_is_about_mast(&report("tool_failure", "list_chats failed", "error", "agent")));
        let mut linked = report("bug", "Failed", "Details", "agent");
        linked.links.push(crate::RequestLink { label: "issue".into(), target: crate::RequestLinkTarget::Url { url: "https://github.com/Pluto8195/mast/issues/42".into() } });
        assert!(friction_is_about_mast(&linked));
    }

    #[test]
    fn excludes_other_kinds_and_non_mast_tools() {
        for kind in ["permission", "environment", "workflow", "docs"] {
            assert!(!friction_is_about_mast(&report(kind, "review_publish failed", "Details", "auto")));
        }
        for name in ["gh", "cargo test", "Linear", "shell"] {
            assert!(!friction_is_about_mast(&report("tool_failure", name, "failed", "auto")));
        }
        assert!(!friction_is_about_mast(&report("bug", "review_publish_extra", "Details", "agent")));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFrictionReportsParams {
    pub space_id: String,
    #[serde(default)]
    pub all_spaces: bool,
    pub kind: Option<FrictionKind>,
    pub status: Option<FrictionStatus>,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub since: Option<i64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFrictionStatusParams {
    pub space_id: String,
    pub fingerprint: String,
    pub status: FrictionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromoteFrictionToItemParams {
    pub space_id: String,
    pub fingerprint: String,
    pub parent_item_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposeFrictionInstructionParams {
    pub space_id: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrictionSettings {
    pub auto_capture: bool,
    /// Space IDs explicitly allowed to read Mast friction across Spaces.
    #[serde(default)]
    pub all_spaces_read_grants: std::collections::BTreeSet<String>,
}

impl Default for FrictionSettings {
    fn default() -> Self {
        Self { auto_capture: true, all_spaces_read_grants: Default::default() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFrictionParams {
    pub space_id: String,
    pub file_name: String,
    pub markdown: String,
    #[serde(default)]
    pub preview: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrictionImportEntry {
    pub kind: FrictionKind,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrictionImportSkip {
    pub line: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFrictionResult {
    pub added: usize,
    pub merged: usize,
    pub skipped: Vec<FrictionImportSkip>,
    pub entries: Vec<FrictionImportEntry>,
}
