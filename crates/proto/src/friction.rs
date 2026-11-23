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
    /// The command is the provider's structured command rendered as shell
    /// text, stored whole and unredacted. Rows recorded before this field
    /// existed took the text between the first two backticks of the prompt,
    /// which is lossy, so they never count as exact.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exact: bool,
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
    static TOOLS: std::sync::OnceLock<std::collections::HashSet<&'static str>> = std::sync::OnceLock::new();
    let tools = TOOLS.get_or_init(|| crate::agent::agent_tools::USAGE.iter().map(|usage| usage.tool)
        .chain(crate::agent::agent_tools::tools::MCP_ONLY.iter().copied()).collect());
    let names_tool = |text: &str| {
        ["mcp__mast-agent__", "mast-agent.", "mast-agent/"].iter().any(|prefix| {
            text.match_indices(*prefix).any(|(index, _)| {
                if text[..index].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') { return false; }
                let name = text[index + prefix.len()..].split(|c: char| !c.is_ascii_alphanumeric() && c != '_').next().unwrap_or("");
                tools.contains(name)
            })
        })
    };
    let captured_refusal = report.provenance.source == FrictionSource::Auto
        && report.summary.split_once(" refused: ").is_some_and(|(tool, code)| !code.is_empty() && tools.contains(tool));
    captured_refusal || names_tool(&report.summary) || names_tool(&report.what_happened)
        || report.links.iter().any(|link| match &link.target {
            crate::RequestLinkTarget::Url { url } | crate::RequestLinkTarget::Pr { url } => {
                let url = url.to_ascii_lowercase();
                url.starts_with("https://github.com/pluto8195/mast/issues/")
                    || url.starts_with("https://github.com/pluto8195/mast/pull/")
            }
            _ => false,
        })
}

/// The fields that may cross a Space boundary.
pub fn foreign_friction_view(report: &FrictionReport, space_name: &str) -> serde_json::Value {
    serde_json::json!({"id":report.id, "kind":report.kind, "summary":report.summary,
        "whatHappened":report.what_happened, "severity":report.severity,
        "occurrences":report.occurrences, "lastSeen":report.last_seen, "status":report.status,
        "spaceId":report.provenance.space_id, "spaceName":space_name})
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
        assert!(friction_is_about_mast(&report("bug", "Failed", "mast-agent.create_chat returned an error", "agent")));
        assert!(friction_is_about_mast(&report("bug", "Failed", "mast-agent/work_outline_read_current failed", "agent")));
        assert!(friction_is_about_mast(&report("tool_failure", "mcp__mast-agent__review_publish failed", "error", "agent")));
        assert!(friction_is_about_mast(&report("tool_failure", "whoami refused: failed", "error", "auto")));
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
        assert!(!friction_is_about_mast(&report("tool_failure", "server rejected server request: unknown", "Unsupported server request", "auto")));
        assert!(!friction_is_about_mast(&report("tool_failure", "whoami failed", "shell command failed", "agent")));
        assert!(!friction_is_about_mast(&report("tool_failure", "Linear list_projects returned 401", "failed", "agent")));
        assert!(!friction_is_about_mast(&report("tool_failure", "our API's send_message endpoint 500s", "failed", "agent")));
        assert!(!friction_is_about_mast(&report("bug", "Build failed", "Project test failed", "import")));
        assert!(!friction_is_about_mast(&report("bug", "review_publish_extra", "Details", "agent")));
        let mut unrelated = report("bug", "Failure", "Details", "agent");
        unrelated.links.push(crate::RequestLink { label: "chat".into(), target: crate::RequestLinkTarget::Url { url: "mast://open/chat/other".into() } });
        assert!(!friction_is_about_mast(&unrelated));
    }

    #[test]
    fn foreign_view_contains_only_the_allowed_fields() {
        let report = report("bug", "mast-agent.create_chat failed", "error", "agent");
        let value = foreign_friction_view(&report, "Other Space");
        let fields: std::collections::BTreeSet<_> = value.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(fields, ["id", "kind", "summary", "whatHappened", "severity", "occurrences", "lastSeen", "status", "spaceId", "spaceName"].into_iter().collect());
    }
}

#[cfg(test)]
mod approval_tests {
    use super::*;

    #[test]
    fn rows_without_the_exact_marker_read_as_not_exact_and_stay_unmarked() {
        let legacy: FrictionApproval = serde_json::from_value(serde_json::json!({"command": "cargo test", "why": "sandbox", "answer": true})).unwrap();
        assert!(!legacy.exact);
        assert!(serde_json::to_value(&legacy).unwrap().get("exact").is_none(), "older readers see the row they always saw");
        let exact = FrictionApproval { exact: true, ..legacy };
        let value = serde_json::to_value(&exact).unwrap();
        assert_eq!(value["exact"], true);
        assert_eq!(serde_json::from_value::<FrictionApproval>(value).unwrap(), exact);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFrictionReportsParams {
    pub space_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
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
