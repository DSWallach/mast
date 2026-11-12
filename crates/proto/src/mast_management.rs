//! Assistant management uses the desktop RPC contracts, with an explicit catalog.
use serde_json::{Value, json};

pub const TOOL: &str = "mast_manage";

/// Examples are real wire shapes. An empty tool call returns this guide before
/// making changes; the same catalog drives MCP discovery and engine dispatch.
pub fn operations() -> Value {
    let mutations = json!({"section":"sessions/spaces/sidebar/devices", "examples":[
        {"op":"renameChat", "chatId":"CHAT", "title":"New title"},
        {"op":"setChatArchived", "chatId":"CHAT", "archived":true},
        {"op":"deleteChat", "chatId":"CHAT"},
        {"op":"setChatAutoApprove", "chatId":"CHAT", "autoApprove":false},
        {"op":"setChatBranch", "chatId":"CHAT", "branch":"main"},
        {"op":"setChatCwd", "chatId":"CHAT", "cwd":"/path/to/checkout"},
        {"op":"setChatConfig", "chatId":"CHAT", "config":{"harness":"codex", "model":"MODEL", "sandbox":"workspace-write", "autoApprove":false}},
        {"op":"changeSidebarPin", "change":{"action":"section", "change":{"action":"create", "id":"GROUP", "name":"Group name"}}},
        {"op":"changeSidebarPin", "change":{"action":"section", "change":{"action":"assign", "sessionId":"CHAT", "sectionId":"GROUP"}}},
        {"op":"changeSidebarPin", "change":{"action":"section", "change":{"action":"assign", "sessionId":"CHAT", "sectionId":null}}},
        {"op":"changeSidebarPin", "change":{"action":"section", "change":{"action":"rename", "id":"GROUP", "name":"Renamed"}}},
        {"op":"changeSidebarPin", "change":{"action":"section", "change":{"action":"collapse", "id":"GROUP", "collapsed":true}}},
        {"op":"changeSidebarPin", "change":{"action":"section", "change":{"action":"delete", "id":"GROUP"}}},
        {"op":"changeSidebarPin", "change":{"action":"pin", "sessionId":"CHAT", "before":null, "after":null}},
        {"op":"changeSidebarPin", "change":{"action":"move", "sessionId":"CHAT", "before":"OTHER_CHAT", "after":null}},
        {"op":"changeSidebarPin", "change":{"action":"unpin", "sessionId":"CHAT"}},
        {"op":"createSpace", "spaceId":"SPACE", "deviceId":"DEVICE", "path":"/path", "name":"Project"},
        {"op":"renameSpace", "spaceId":"SPACE", "name":"Project"},
        {"op":"deleteSpace", "spaceId":"SPACE"},
        {"op":"renameDevice", "deviceId":"DEVICE", "name":"Laptop"}
    ]});
    let scheduled_tasks = json!({"section":"scheduled tasks", "notes":"List returns complete task drafts and revisions. Save replaces the entire draft; preserve fields you are not changing. Use expectedRevision from List (0 for a new task). Give every save/delete/run a unique operationId; reuse it only when retrying the same operation. RunNow triggers a fresh manual run even for a disabled task. History returns run snapshots and chat IDs.", "examples":[
        {"action":"list"},
        {"action":"history", "id":"TASK", "before":null},
        {"action":"save", "id":"TASK", "expectedRevision":1, "operationId":"UNIQUE", "draft":{"name":"Briefing", "prompt":"Summarize my day", "cwd":"/path", "spaceId":null, "config":{"harness":"codex", "model":"MODEL", "sandbox":"workspace-write", "autoApprove":false}, "schedule":{"type":"cron", "expression":"0 8 * * 1-5", "timezone":"America/New_York"}, "enabled":true, "graceMinutes":60, "cutoverConfirmed":false}},
        {"action":"runNow", "id":"TASK", "operationId":"UNIQUE"},
        {"action":"delete", "id":"TASK", "expectedRevision":1, "operationId":"UNIQUE"},
        {"action":"preview", "schedule":{"type":"once", "at":1794488400000_i64}}
    ]});
    let mut catalog = json!({
        "WatchChats": {"section":"sessions", "params":{}},
        "WatchSessions": {"section":"sessions", "params":{}},
        "WatchDevices": {"section":"devices", "params":{}},
        "WatchSpaces": {"section":"spaces", "params":{}},
        "WatchSidebarPreferences": {"section":"sidebar", "params":{}},
        "Mutate": mutations,
        "ScheduledTasks": scheduled_tasks,
        "WatchWorkOutline": {"section":"work outline", "params":{"spaceId":"SPACE"}},
        "WorkOutlineMutate": {"section":"work outline", "notes":"Uses desktop WorkOutlineMutation operations, across all Spaces. Every mutation needs operationId. Supports items, movement, descriptions, instructions, execution settings, templates, references and requests.", "examples":[
            {"op":"createWorkItem", "spaceId":"SPACE", "title":"Title", "operationId":"UNIQUE"},
            {"op":"updateWorkItem", "workItemId":"ITEM", "changes":{"title":"New title", "description":"Details"}, "operationId":"UNIQUE"},
            {"op":"moveWorkItem", "workItemId":"ITEM", "destinationParentWorkItemId":null, "placement":{"type":"last"}, "operationId":"UNIQUE"}
        ]},
        "WorkOutlineQuery": {"section":"work outline", "params":{"query":"resolveEffectiveInstructions", "workItemId":"ITEM"}},
        "ListProjectActions": {"section":"project actions", "params":{"spaceId":"SPACE"}},
        "UpsertProjectAction": {"section":"project actions", "params":{"spaceId":"SPACE", "actionId":null, "action":{"name":"Test", "command":"cargo test", "icon":"test"}}},
        "DeleteProjectAction": {"section":"project actions", "params":{"spaceId":"SPACE", "actionId":"ACTION"}},
        "GetWakeStatus": {"section":"session wake settings", "params":{"chatId":"CHAT"}},
        "SetWakeSettings": {"section":"session wake settings", "params":{"chatId":"CHAT", "settings":{}}},
        "WakeDeliverNow": {"section":"session wake settings", "params":{"chatId":"CHAT"}},
    });
    catalog.as_object_mut().unwrap().extend(json!({
        "RunProjectAction": {"section":"project actions", "params":{"spaceId":"SPACE", "chatId":"CHAT", "actionId":"ACTION", "cols":80, "rows":24}},
        "WatchArtifacts": {"section":"artifacts", "params":{"spaceId":"SPACE"}},
        "ArtifactContent": {"section":"artifacts", "params":{"artifactId":"ARTIFACT"}},
        "ArtifactPin": {"section":"artifacts", "params":{"artifactId":"ARTIFACT", "pinned":true}},
        "ArtifactRegister": {"section":"artifacts", "params":{"spaceId":"SPACE", "path":"/path/to/file", "title":"Title"}},
        "ArtifactAddComment": {"section":"artifacts", "params":{"artifactId":"ARTIFACT", "anchor":{"exact":"Quoted text"}, "text":"Comment"}},
        "ArtifactResolveComment": {"section":"artifacts", "params":{"artifactId":"ARTIFACT", "commentId":"COMMENT", "resolved":true}},
        "PrMyOpen": {"section":"pull requests", "params":{"spaceId":"SPACE"}},
        "PrReviewRequested": {"section":"pull requests", "params":{"spaceId":"SPACE"}},
        "PrReviewSyncNow": {"section":"pull requests", "params":{"spaceId":"SPACE"}},
        "LinearMyIssues": {"section":"Linear", "params":{}},
        "LinearMyProjects": {"section":"Linear", "params":{}},
        "LinearProjectIssues": {"section":"Linear", "params":{"projectId":"PROJECT"}},
        "GetPersonalAssistant": {"section":"personal assistant", "params":{}},
        "ListAgentAccounts": {"section":"agent accounts", "params":{}},
    }).as_object().unwrap().clone());
    catalog.as_object_mut().unwrap().extend(json!({
        "ListHarnesses": {"section":"agents", "params":{}},
        "ListModels": {"section":"agents", "params":{"harness":"codex"}},
        "SetHarnessEnabled": {"section":"agents", "params":{"harness":"codex", "enabled":true}},
        "GetTitleSettings": {"section":"title settings", "params":{}},
        "SetTitleSettings": {"section":"title settings", "notes":"Use the complete shape returned by GetTitleSettings."},
        "GetFrictionSettings": {"section":"friction", "params":{}},
        "SetFrictionSettings": {"section":"friction", "notes":"Use the complete shape returned by GetFrictionSettings."},
        "ListFrictionReports": {"section":"friction", "params":{"spaceId":"SPACE"}},
        "ChatLinkStatus": {"section":"session links", "params":{"chatId":"CHAT"}},
        "SetChatLink": {"section":"session links", "params":{"chatId":"CHAT", "kind":"pr", "value":"https://github.com/OWNER/REPO/pull/1"}}
    }).as_object().unwrap().clone());
    catalog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_examples_match_the_desktop_contracts() {
        let guide = operations();
        for example in guide["ScheduledTasks"]["examples"].as_array().unwrap() {
            serde_json::from_value::<crate::ScheduledTasksRequest>(example.clone()).unwrap();
        }
        for example in guide["WorkOutlineMutate"]["examples"].as_array().unwrap() {
            serde_json::from_value::<crate::WorkOutlineMutation>(example.clone()).unwrap();
        }
        serde_json::from_value::<crate::ProjectActionDraft>(
            guide["UpsertProjectAction"]["params"]["action"].clone(),
        )
        .unwrap();
        serde_json::from_value::<crate::ArtifactAddCommentParams>(
            guide["ArtifactAddComment"]["params"].clone(),
        )
        .unwrap();
        serde_json::from_value::<crate::ArtifactRegisterParams>(
            guide["ArtifactRegister"]["params"].clone(),
        )
        .unwrap();
        serde_json::from_value::<crate::SetWakeSettingsParams>(
            guide["SetWakeSettings"]["params"].clone(),
        )
        .unwrap();
    }
}
