use mast_engine::{EngineCore, EngineProfile, HarnessId, default_registry};
use mast_proto::*;
use std::sync::Arc;

fn draft(root: &std::path::Path) -> ScheduledTaskDraft {
    ScheduledTaskDraft {
        name: "Scheduled integration".into(),
        prompt: "Hello".into(),
        cwd: root.to_string_lossy().into(),
        space_id: None,
        config: ChatConfig {
            harness: HarnessId::Mock,
            model: Some("mock-1".into()),
            reasoning: Some(ReasoningLevel::Medium),
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: false,
        },
        schedule: TaskSchedule::Cron {
            expression: "0 8 * * 1-5".into(),
            timezone: "America/New_York".into(),
        },
        enabled: false,
        grace_minutes: 360,
        cutover_confirmed: false,
    }
}
#[tokio::test]
async fn scheduled_launch_is_durable_idempotent_and_tagged() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir.path(), "org", "user"),
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let save = ScheduledTasksRequest::Save {
        id: "task".into(),
        expected_revision: 0,
        draft: draft(dir.path()),
        operation_id: "save-task".into(),
    };
    core.scheduled_tasks.request(save.clone()).await.unwrap();
    core.scheduled_tasks.request(save).await.unwrap();
    let run = ScheduledTasksRequest::RunNow {
        id: "task".into(),
        operation_id: "manual-run".into(),
    };
    core.scheduled_tasks.request(run.clone()).await.unwrap();
    core.scheduled_tasks.request(run).await.unwrap();
    let snapshot = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::List)
        .await
        .unwrap();
    assert_eq!(snapshot.runs.len(), 1);
    assert!(!snapshot.tasks[0].draft.enabled);
    core.scheduled_tasks
        .tick(chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let chat_id = snapshot.runs[0].chat_id.as_ref().unwrap();
    let chat = core.workspace.chat(chat_id).unwrap().unwrap();
    assert_eq!(chat.scheduled_task.as_ref().unwrap().task_id, "task");
    assert_eq!(chat.config.unwrap().reasoning, Some(ReasoningLevel::Medium));
    let doc = core.doc_host.open(chat_id).unwrap();
    assert_eq!(doc.doc().read_commands().unwrap().len(), 1);
    core.scheduled_tasks
        .tick(chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    assert_eq!(doc.doc().read_commands().unwrap().len(), 1);
    let delete = ScheduledTasksRequest::Delete {
        id: "task".into(),
        expected_revision: 1,
        operation_id: "delete-task".into(),
    };
    core.scheduled_tasks.request(delete.clone()).await.unwrap();
    core.scheduled_tasks.request(delete).await.unwrap();
    assert!(
        core.scheduled_tasks
            .request(ScheduledTasksRequest::List)
            .await
            .unwrap()
            .tasks
            .is_empty()
    );
    core.shutdown().await;
}

#[tokio::test]
async fn rejects_unavailable_model_options_and_foreign_space() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir.path(), "org", "user"),
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let mut d = draft(dir.path());
    d.enabled = true;
    d.config
        .model_options
        .insert("contextWindow".into(), serde_json::json!("1m"));
    assert!(
        core.scheduled_tasks
            .request(ScheduledTasksRequest::Save {
                id: "bad".into(),
                expected_revision: 0,
                draft: d,
                operation_id: "save-bad".into()
            })
            .await
            .is_err()
    );
    core.workspace
        .create_space(
            "foreign",
            "other-device",
            dir.path().to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    let mut d = draft(dir.path());
    d.space_id = Some("foreign".into());
    assert!(
        core.scheduled_tasks
            .request(ScheduledTasksRequest::Save {
                id: "foreign-task".into(),
                expected_revision: 0,
                draft: d,
                operation_id: "save-foreign".into()
            })
            .await
            .is_err()
    );
    core.shutdown().await;
}

#[tokio::test]
async fn claude_import_is_previewed_paused_and_deduplicated() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir.path(), "org", "user"),
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let prompt = dir.path().join("SKILL.md");
    std::fs::write(&prompt, "Read the current procedure. Do not invent it.").unwrap();
    let registry = dir.path().join("scheduled-tasks.json");
    std::fs::write(&registry,serde_json::to_vec(&serde_json::json!({"scheduledTasks":[{"id":"briefing","enabled":true,"cronExpression":"40 4 * * 1-5","cwd":dir.path(),"filePath":prompt,"permissionMode":"auto"}]})).unwrap()).unwrap();
    let original = std::fs::read(&registry).unwrap();
    let preview = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::ImportPreview {
            registry_path: Some(registry.to_string_lossy().into()),
            timezone: "America/New_York".into(),
            space_id: None,
        })
        .await
        .unwrap();
    assert_eq!(preview.candidates.len(), 1);
    assert!(!preview.candidates[0].draft.enabled);
    assert!(!preview.candidates[0].draft.config.auto_approve);
    let import = ScheduledTasksRequest::Import {
        candidate: preview.candidates[0].clone(),
        operation_id: "import-one".into(),
    };
    core.scheduled_tasks.request(import.clone()).await.unwrap();
    core.scheduled_tasks.request(import).await.unwrap();
    let result = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::List)
        .await
        .unwrap();
    assert_eq!(result.tasks.len(), 1);
    assert!(!result.tasks[0].draft.enabled);
    assert!(result.runs.is_empty());
    assert_eq!(std::fs::read(registry).unwrap(), original);
    core.shutdown().await;
}

#[tokio::test]
async fn imported_sessions_keep_original_edition_order() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir.path(), "org", "user"),
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    for (id, date) in [
        ("newer", "2026-11-11T08:00:00Z"),
        ("older", "2026-11-10T08:00:00Z"),
    ] {
        let path = dir.path().join(format!("{id}.jsonl"));
        let prompt = format!(
            "<scheduled-task name=\"daily\" file=\"{}/SKILL.md\">\nDo the task\n</scheduled-task>",
            dir.path().display()
        );
        let line = serde_json::json!({"type":"user","uuid":id,"timestamp":date,"cwd":dir.path(),"sessionId":id,"message":{"role":"user","content":prompt}});
        std::fs::write(&path, format!("{line}\n")).unwrap();
        core.external_import.import(id, id, &path).unwrap();
    }
    let chats = core.workspace.read_chats().unwrap();
    assert!(chats.iter().all(|c| c.scheduled_task.is_some()));
    let ids = latest_task_chat_ids(&chats);
    assert!(ids.contains("newer"));
    assert!(!ids.contains("older"));
    core.shutdown().await;
}

#[tokio::test]
async fn recovers_a_claim_after_restart_without_another_occurrence() {
    let dir = tempfile::tempdir().unwrap();
    let assemble = || {
        EngineCore::assemble_with_profile(
            EngineProfile::development(dir.path(), "org", "user"),
            Arc::new(default_registry()),
            HarnessId::Mock,
            None,
        )
        .unwrap()
    };
    let core = assemble();
    core.scheduled_tasks
        .request(ScheduledTasksRequest::Save {
            id: "recover".into(),
            expected_revision: 0,
            draft: draft(dir.path()),
            operation_id: "save-recover".into(),
        })
        .await
        .unwrap();
    core.scheduled_tasks
        .request(ScheduledTasksRequest::RunNow {
            id: "recover".into(),
            operation_id: "recover-run".into(),
        })
        .await
        .unwrap();
    let before = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::List)
        .await
        .unwrap();
    let chat = before.runs[0].chat_id.clone().unwrap();
    core.shutdown().await;
    drop(core);
    let core = assemble();
    core.scheduled_tasks
        .tick(chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let after = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::List)
        .await
        .unwrap();
    assert_eq!(after.runs.len(), 1);
    assert_eq!(after.runs[0].chat_id.as_deref(), Some(chat.as_str()));
    assert_eq!(
        core.doc_host
            .open(&chat)
            .unwrap()
            .doc()
            .read_commands()
            .unwrap()
            .len(),
        1
    );
    core.shutdown().await;
}

#[tokio::test]
async fn coalesces_missed_minutes_and_does_not_overlap_manual_runs() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir.path(), "org", "user"),
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let mut d = draft(dir.path());
    d.enabled = true;
    d.schedule = TaskSchedule::Cron {
        expression: "* * * * *".into(),
        timezone: "UTC".into(),
    };
    let saved = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::Save {
            id: "coalesce".into(),
            expected_revision: 0,
            draft: d,
            operation_id: "save-coalesce".into(),
        })
        .await
        .unwrap();
    let wake = saved.tasks[0].next_due.unwrap() + 130 * 60_000;
    core.scheduled_tasks.tick(wake).await.unwrap();
    let after = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::List)
        .await
        .unwrap();
    assert_eq!(after.runs.len(), 1);
    assert_eq!(after.runs[0].scheduled_for, wake);
    core.scheduled_tasks
        .request(ScheduledTasksRequest::RunNow {
            id: "coalesce".into(),
            operation_id: "overlap-manual".into(),
        })
        .await
        .unwrap();
    let after = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::List)
        .await
        .unwrap();
    assert_eq!(after.runs.len(), 2);
    assert!(
        after
            .runs
            .iter()
            .any(|r| r.state == ScheduledRunState::Skipped
                && r.detail.as_deref() == Some("Previous edition is still active"))
    );
    core.shutdown().await;
}

#[tokio::test]
async fn verifies_source_pause_and_keeps_handoff_after_source_retirement() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir.path(), "org", "user"),
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let registry = dir.path().join("source.json");
    std::fs::write(
        &registry,
        r#"{"scheduledTasks":[{"id":"daily","enabled":true}]}"#,
    )
    .unwrap();
    let candidate = ScheduledImportCandidate {
        source: format!("{}#daily", registry.display()),
        source_enabled: true,
        draft: draft(dir.path()),
        warnings: vec![],
    };
    let imported = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::Import {
            candidate,
            operation_id: "import-handoff".into(),
        })
        .await
        .unwrap();
    let task = &imported.tasks[0];
    let mut d = task.draft.clone();
    d.enabled = true;
    d.cutover_confirmed = true;
    let save = ScheduledTasksRequest::Save {
        id: task.id.clone(),
        expected_revision: 1,
        draft: d,
        operation_id: "enable-handoff".into(),
    };
    assert!(core.scheduled_tasks.request(save.clone()).await.is_err());
    std::fs::write(
        &registry,
        r#"{"scheduledTasks":[{"id":"daily","enabled":false}]}"#,
    )
    .unwrap();
    let saved = core.scheduled_tasks.request(save).await.unwrap();
    assert!(saved.tasks[0].source_handoff_at.is_some());
    std::fs::remove_file(registry).unwrap();
    core.scheduled_tasks
        .request(ScheduledTasksRequest::RunNow {
            id: task.id.clone(),
            operation_id: "retired-source-run".into(),
        })
        .await
        .unwrap();
    core.scheduled_tasks
        .tick(chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let snapshot = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::List)
        .await
        .unwrap();
    assert_eq!(snapshot.runs.len(), 1);
    assert_ne!(snapshot.runs[0].state, ScheduledRunState::Failed);
    core.shutdown().await;
}

#[tokio::test]
async fn identical_claude_sources_in_different_profiles_get_distinct_task_ids() {
    let dir = tempfile::tempdir().unwrap();
    let source = format!("{}#same-task", dir.path().join("registry.json").display());
    let mut ids = vec![];
    for org in ["org-a", "org-b"] {
        let core = EngineCore::assemble_with_profile(
            EngineProfile::development(dir.path(), org, "user"),
            Arc::new(default_registry()),
            HarnessId::Mock,
            None,
        )
        .unwrap();
        let candidate = ScheduledImportCandidate {
            source: source.clone(),
            source_enabled: false,
            draft: draft(dir.path()),
            warnings: vec![],
        };
        let response = core
            .scheduled_tasks
            .request(ScheduledTasksRequest::Import {
                candidate,
                operation_id: "same-import-key".into(),
            })
            .await
            .unwrap();
        ids.push(response.tasks[0].id.clone());
        core.shutdown().await;
        drop(core);
    }
    assert_ne!(ids[0], ids[1]);
}
