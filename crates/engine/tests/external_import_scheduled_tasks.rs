//! Imported Claude scheduled-task runs are stamped with their task name, both
//! at import and by the boot-time backfill for chats imported earlier.

use std::sync::Arc;

use mast_engine::{EngineCore, EngineProfile, HarnessId, default_registry};

fn assemble(profile: EngineProfile) -> EngineCore {
    EngineCore::assemble_with_profile(profile, Arc::new(default_registry()), HarnessId::Mock, None)
        .expect("assemble profile")
}

fn user_line(session_id: &str, cwd: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "parentUuid": null, "isSidechain": false, "type": "user",
        "message": {"role": "user", "content": text},
        "uuid": "u1", "timestamp": "2026-01-01T00:00:01.000Z",
        "cwd": cwd, "sessionId": session_id,
    })
}

fn assistant_line(session_id: &str, cwd: &str) -> serde_json::Value {
    serde_json::json!({
        "parentUuid": "u1", "isSidechain": false, "type": "assistant",
        "message": {"model": "claude-x", "content": [{"type": "text", "text": "ack"}]},
        "uuid": "a1", "timestamp": "2026-01-01T00:00:02.000Z",
        "cwd": cwd, "sessionId": session_id,
    })
}

fn custom_title_line(session_id: &str, title: &str) -> serde_json::Value {
    serde_json::json!({"type": "custom-title", "customTitle": title, "sessionId": session_id})
}

fn write_transcript(path: &std::path::Path, lines: &[serde_json::Value]) {
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(path, body).expect("write transcript");
}

fn task_prompt(name: &str) -> String {
    format!(
        "<system-reminder>\nInjected context.\n</system-reminder>\n\
         <scheduled-task name=\"{name}\" file=\"<home>/.claude/scheduled-tasks/{name}/SKILL.md\">\n\
         Do the daily thing.\n</scheduled-task>"
    )
}

#[tokio::test]
async fn scheduled_runs_are_tagged_at_import_and_by_backfill() {
    let fake_home = tempfile::tempdir().expect("fake home tempdir");
    let previous_home = std::env::var_os("HOME");
    // SAFETY: test-only, restored at the end — same as the other importer tests.
    unsafe {
        std::env::set_var("HOME", fake_home.path());
    }
    let data_dir = tempfile::tempdir().expect("data dir");
    let core = assemble(EngineProfile::development(
        data_dir.path(),
        "dev-org",
        "dev-user",
    ));
    let transcripts = tempfile::tempdir().expect("transcripts dir");
    let chat = |id: &str| core.workspace.chat(id).expect("read").expect("chat exists");

    let titled = transcripts.path().join("titled.jsonl");
    write_transcript(
        &titled,
        &[
            user_line("sess-titled", "/work/a", &task_prompt("github-summary")),
            assistant_line("sess-titled", "/work/a"),
            custom_title_line("sess-titled", "GitHub priority briefing"),
        ],
    );
    core.external_import
        .import("chat-titled", "sess-titled", &titled)
        .expect("import titled");
    let imported = chat("chat-titled");
    assert_eq!(imported.scheduled_task.as_deref(), Some("github-summary"));
    assert_eq!(imported.title.as_deref(), Some("GitHub priority briefing"));

    let untitled = transcripts.path().join("untitled.jsonl");
    write_transcript(
        &untitled,
        &[
            user_line("sess-untitled", "/work/a", &task_prompt("daily-summary")),
            assistant_line("sess-untitled", "/work/a"),
        ],
    );
    core.external_import
        .import("chat-untitled", "sess-untitled", &untitled)
        .expect("import untitled");
    let imported = chat("chat-untitled");
    assert_eq!(imported.scheduled_task.as_deref(), Some("daily-summary"));
    assert_eq!(
        imported.title.as_deref(),
        Some("daily-summary"),
        "the task prompt is never used as a title"
    );

    let mention = transcripts.path().join("mention.jsonl");
    write_transcript(
        &mention,
        &[
            user_line(
                "sess-mention",
                "/work/a",
                "why does <scheduled-task name=\"x\"> show up in my chat list?",
            ),
            assistant_line("sess-mention", "/work/a"),
        ],
    );
    core.external_import
        .import("chat-mention", "sess-mention", &mention)
        .expect("import mention");
    assert!(chat("chat-mention").scheduled_task.is_none());

    // Imported before the field existed: its title is the prompt's first line.
    let legacy = transcripts.path().join("legacy.jsonl");
    write_transcript(
        &legacy,
        &[
            user_line("sess-legacy", "/work/a", "<system-reminder>\nstale"),
            assistant_line("sess-legacy", "/work/a"),
        ],
    );
    core.external_import
        .import("chat-legacy", "sess-legacy", &legacy)
        .expect("import legacy");
    assert_eq!(
        chat("chat-legacy").title.as_deref(),
        Some("<system-reminder>")
    );
    write_transcript(
        &legacy,
        &[
            user_line("sess-legacy", "/work/a", &task_prompt("codespace-manager")),
            assistant_line("sess-legacy", "/work/a"),
            custom_title_line("sess-legacy", "Codespace manager"),
        ],
    );

    let repaired = core
        .external_import
        .repair_scheduled_tasks()
        .expect("backfill");
    assert_eq!(repaired, 1, "only the legacy chat needed tagging");
    let backfilled = chat("chat-legacy");
    assert_eq!(
        backfilled.scheduled_task.as_deref(),
        Some("codespace-manager")
    );
    assert_eq!(backfilled.title.as_deref(), Some("Codespace manager"));
    assert!(chat("chat-mention").scheduled_task.is_none());

    assert_eq!(
        core.external_import
            .repair_scheduled_tasks()
            .expect("second backfill"),
        0
    );

    unsafe {
        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}
