use mast_engine::{EngineCore, EngineProfile, HarnessId, default_registry};
use mast_proto::*;
use std::sync::Arc;

fn draft(root: &std::path::Path) -> ScheduledTaskDraft {
    ScheduledTaskDraft {
        name: "Scheduled integration".into(),
        prompt: "Hello".into(),
        cwd: root.to_string_lossy().into(),
        space_id: None,
        config: ChatConfig { approval_rules: false,
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
        work_item_id: None,
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
    assert_eq!(chat.scheduled_task_info.as_ref().unwrap().task_id, "task");
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
    std::fs::write(&registry,serde_json::to_vec(&serde_json::json!({"scheduledTasks":[{"id":"briefing","enabled":true,"cronExpression":"40 4 * * 1-5","cwd":dir.path(),"filePath":prompt,"permissionMode":"auto"},{"id":"interval","enabled":true,"cronExpression":"*/30 9-17 * * 1-5","cwd":dir.path(),"filePath":prompt}]})).unwrap()).unwrap();
    let original = std::fs::read(&registry).unwrap();
    let mut preview = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::ImportPreview {
            registry_path: Some(registry.to_string_lossy().into()),
            timezone: "America/New_York".into(),
            space_id: None,
        })
        .await
        .unwrap();
    // A task that repeats within the hour catches up only briefly.
    let interval = preview
        .candidates
        .iter()
        .position(|c| c.source.ends_with("#interval"))
        .unwrap();
    assert_eq!(preview.candidates.remove(interval).draft.grace_minutes, 30);
    assert_eq!(preview.candidates.len(), 1);
    assert_eq!(preview.candidates[0].draft.grace_minutes, 360);
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
    let original_task = result.tasks[0].clone();
    core.scheduled_tasks
        .request(ScheduledTasksRequest::Delete {
            id: original_task.id.clone(),
            expected_revision: original_task.revision,
            operation_id: "delete-import".into(),
        })
        .await
        .unwrap();
    let reimport = ScheduledTasksRequest::Import {
        candidate: preview.candidates[0].clone(),
        operation_id: "reimport-deleted-task".into(),
    };
    let restored = core
        .scheduled_tasks
        .request(reimport.clone())
        .await
        .unwrap();
    assert_eq!(restored.tasks.len(), 1);
    assert_eq!(restored.tasks[0].id, original_task.id);
    assert_eq!(restored.tasks[0].revision, original_task.revision + 2);
    assert!(!restored.tasks[0].draft.enabled);
    assert!(!restored.tasks[0].draft.config.auto_approve);
    core.scheduled_tasks.request(reimport).await.unwrap();
    assert!(
        core.scheduled_tasks
            .request(ScheduledTasksRequest::Import {
                candidate: preview.candidates[0].clone(),
                operation_id: "duplicate-active-import".into(),
            })
            .await
            .is_err()
    );
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
    assert!(chats.iter().all(|c| c.scheduled_task_info.is_some()));
    let ids = latest_task_chat_ids(&chats);
    assert!(ids.contains("newer"));
    assert!(!ids.contains("older"));
    let skill = format!("{}/SKILL.md", dir.path().display());
    let source_file = |id: &str| {
        core.workspace.chat(id).unwrap().unwrap().scheduled_task_info.unwrap().source_file
    };
    assert_eq!(source_file("newer").as_deref(), Some(skill.as_str()));
    // A run imported before the file was recorded gets it from its transcript, once.
    let mut info = core.workspace.chat("older").unwrap().unwrap().scheduled_task_info.unwrap();
    info.source_file = None;
    core.workspace.set_chat_scheduled_task_info("older", &info).unwrap();
    assert_eq!(core.external_import.backfill_scheduled_source_files().unwrap(), 1);
    assert_eq!(source_file("older").as_deref(), Some(skill.as_str()));
    assert_eq!(core.external_import.backfill_scheduled_source_files().unwrap(), 0);
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

/// Two deterministic adapters exercise provider changes without contacting a CLI.
struct ProviderFixture(HarnessId);
#[async_trait::async_trait]
impl mast_harness::Harness for ProviderFixture {
    fn id(&self) -> HarnessId {
        self.0
    }
    fn display_name(&self) -> &str {
        "Provider fixture"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, mast_harness::HarnessError> {
        Ok(vec![Model {
            id: "fixture-model".into(),
            label: "Fixture model".into(),
            description: None,
            reasoning_levels: vec![ReasoningLevel::Medium],
            options: vec![],
        }])
    }
    async fn run(
        &self,
        request: RunRequest,
        _: mast_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<AgentEvent, mast_harness::HarnessError>>,
        mast_harness::HarnessError,
    > {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: self.0,
                model: "fixture-model".into(),
                tools: vec![],
                cwd: request.cwd,
                session_id: format!("fixture-{:?}", self.0),
                assistant_message_id: "reply".into(),
                chat_id: None,
                space_id: None,
            }),
            Ok(AgentEvent::TextDelta {
                text: "Completed fixture task".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            }),
        ])))
    }
}

/// Holds its run open until interrupted, so the run keeps a dispatch slot.
/// With `asks`, it parks on an approval nobody answers.
struct HeldOpenFixture {
    asks: bool,
}
#[async_trait::async_trait]
impl mast_harness::Harness for HeldOpenFixture {
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCode
    }
    fn display_name(&self) -> &str {
        "Held-open fixture"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, mast_harness::HarnessError> {
        mast_harness::Harness::models(&ProviderFixture(HarnessId::ClaudeCode)).await
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: mast_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<AgentEvent, mast_harness::HarnessError>>,
        mast_harness::HarnessError,
    > {
        use futures::StreamExt;
        let mast_harness::RunControls {
            request_input,
            interrupt,
            ..
        } = controls;
        let asks = self.asks;
        Ok(futures::stream::iter(vec![Ok(AgentEvent::SessionStarted {
            harness: HarnessId::ClaudeCode,
            model: "fixture-model".into(),
            tools: vec![],
            cwd: request.cwd,
            session_id: "held-open".into(),
            assistant_message_id: "reply".into(),
            chat_id: None,
            space_id: None,
        })])
        .chain(futures::stream::once(async move {
            let _answer = asks.then(|| {
                request_input(vec![UserInputQuestion {
                    id: "approve".into(),
                    header: "Notion".into(),
                    question: "Allow notion-update-page?".into(),
                    options: vec!["Yes".into(), "No".into()],
                    multi_select: false,
                    allow_custom: false,
                    command_approval: None,
                    permission: None,
                }])
            });
            interrupt.cancelled().await;
            Ok(AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: None,
                session_id: None,
            })
        }))
        .boxed())
    }
}

#[tokio::test]
async fn waiting_runs_expire_on_schedule_while_both_slots_are_held() {
    let dir = tempfile::tempdir().unwrap();
    let registry = mast_engine::HarnessRegistry::new();
    registry.register(Arc::new(HeldOpenFixture { asks: false }));
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir.path(), "org", "user"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
    )
    .unwrap();
    let run_now = |id: &'static str, grace_minutes: u32| {
        let mut d = draft(dir.path());
        d.config.harness = HarnessId::ClaudeCode;
        d.config.model = Some("fixture-model".into());
        d.grace_minutes = grace_minutes;
        let tasks = core.scheduled_tasks.clone();
        async move {
            tasks
                .request(ScheduledTasksRequest::Save {
                    id: id.into(),
                    expected_revision: 0,
                    draft: d,
                    operation_id: format!("save-{id}"),
                })
                .await
                .unwrap();
            tasks
                .request(ScheduledTasksRequest::RunNow {
                    id: id.into(),
                    operation_id: format!("run-{id}"),
                })
                .await
                .unwrap()
        }
    };
    let state = |snapshot: &ScheduledTasksSnapshot, id: &str| {
        snapshot
            .runs
            .iter()
            .find(|r| r.task_id == id)
            .map(|r| (r.state, r.detail.clone()))
            .unwrap()
    };
    run_now("first", 360).await;
    run_now("second", 360).await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        core.scheduled_tasks
            .tick(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();
        let snapshot = core
            .scheduled_tasks
            .request(ScheduledTasksRequest::List)
            .await
            .unwrap();
        if ["first", "second"]
            .iter()
            .all(|id| state(&snapshot, id).0 == ScheduledRunState::Running)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "held runs did not start: {:?}",
            snapshot.runs
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    run_now("expired", 1).await;
    run_now("waiting", 360).await;
    core.scheduled_tasks
        .tick(chrono::Utc::now().timestamp_millis() + 2 * 60_000)
        .await
        .unwrap();
    let snapshot = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::List)
        .await
        .unwrap();
    assert_eq!(state(&snapshot, "first").0, ScheduledRunState::Running);
    assert_eq!(state(&snapshot, "second").0, ScheduledRunState::Running);
    assert_eq!(
        state(&snapshot, "expired"),
        (
            ScheduledRunState::Skipped,
            Some("Dispatch queue grace expired".into())
        )
    );
    assert_eq!(
        state(&snapshot, "waiting"),
        (ScheduledRunState::Launching, None)
    );
    core.shutdown().await;
}

#[tokio::test]
async fn a_run_left_awaiting_input_is_stopped_after_thirty_minutes() {
    let dir = tempfile::tempdir().unwrap();
    let registry = mast_engine::HarnessRegistry::new();
    registry.register(Arc::new(HeldOpenFixture { asks: true }));
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir.path(), "org", "user"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
    )
    .unwrap();
    let mut d = draft(dir.path());
    d.config.harness = HarnessId::ClaudeCode;
    d.config.model = Some("fixture-model".into());
    core.scheduled_tasks
        .request(ScheduledTasksRequest::Save {
            id: "unattended".into(),
            expected_revision: 0,
            draft: d,
            operation_id: "save-unattended".into(),
        })
        .await
        .unwrap();
    core.scheduled_tasks
        .request(ScheduledTasksRequest::RunNow {
            id: "unattended".into(),
            operation_id: "run-unattended".into(),
        })
        .await
        .unwrap();
    let run = || async {
        core.scheduled_tasks
            .request(ScheduledTasksRequest::List)
            .await
            .unwrap()
            .runs
            .remove(0)
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let since = loop {
        core.scheduled_tasks
            .tick(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();
        let r = run().await;
        if r.state == ScheduledRunState::AwaitingInput {
            break r.awaiting_since.unwrap();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "run never awaited input: {r:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    core.scheduled_tasks
        .tick(since + 29 * 60_000)
        .await
        .unwrap();
    let r = run().await;
    assert_eq!(r.state, ScheduledRunState::AwaitingInput);
    assert_eq!(r.awaiting_since, Some(since));
    core.scheduled_tasks
        .tick(since + 30 * 60_000)
        .await
        .unwrap();
    let r = run().await;
    assert_eq!(r.state, ScheduledRunState::Interrupted);
    assert_eq!(
        r.detail.as_deref(),
        Some("Stopped after 30 min waiting for input: Notion")
    );
    assert_eq!(r.finished_at, Some(since + 30 * 60_000));
    core.shutdown().await;
}

#[tokio::test]
async fn an_existing_task_can_switch_from_claude_to_codex_without_rewriting_past_runs() {
    let dir = tempfile::tempdir().unwrap();
    let registry = mast_engine::HarnessRegistry::new();
    registry.register(Arc::new(ProviderFixture(HarnessId::ClaudeCode)));
    registry.register(Arc::new(ProviderFixture(HarnessId::Codex)));
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir.path(), "org", "user"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
    )
    .unwrap();
    let mut definition = draft(dir.path());
    definition.config.harness = HarnessId::ClaudeCode;
    definition.config.model = Some("fixture-model".into());
    for (revision, provider) in [(0, HarnessId::ClaudeCode), (1, HarnessId::Codex)] {
        definition.config.harness = provider;
        core.scheduled_tasks
            .request(ScheduledTasksRequest::Save {
                id: "switch-provider".into(),
                expected_revision: revision,
                draft: definition.clone(),
                operation_id: format!("save-provider-{revision}"),
            })
            .await
            .unwrap();
        let result = core
            .scheduled_tasks
            .request(ScheduledTasksRequest::RunNow {
                id: "switch-provider".into(),
                operation_id: format!("run-provider-{revision}"),
            })
            .await
            .unwrap();
        let run_id = result
            .runs
            .iter()
            .find(|r| r.task_revision == revision + 1)
            .unwrap()
            .id
            .clone();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            core.scheduled_tasks
                .tick(chrono::Utc::now().timestamp_millis())
                .await
                .unwrap();
            let snapshot = core
                .scheduled_tasks
                .request(ScheduledTasksRequest::List)
                .await
                .unwrap();
            let run = snapshot.runs.iter().find(|r| r.id == run_id).unwrap();
            if run.state == ScheduledRunState::Completed {
                assert_eq!(run.snapshot.config.harness, provider);
                let chat = core
                    .workspace
                    .chat(run.chat_id.as_ref().unwrap())
                    .unwrap()
                    .unwrap();
                assert_eq!(chat.config.unwrap().harness, provider);
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "provider run did not complete: {run:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
    let history = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::History {
            id: "switch-provider".into(),
            before: None,
        })
        .await
        .unwrap();
    assert_eq!(history.runs.len(), 2);
    let old = history.runs.iter().find(|r| r.task_revision == 1).unwrap();
    assert_eq!(old.snapshot.config.harness, HarnessId::ClaudeCode);
    let old_chat = core
        .workspace
        .chat(old.chat_id.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(old_chat.config.unwrap().harness, HarnessId::ClaudeCode);
    assert_eq!(
        history
            .runs
            .iter()
            .find(|r| r.task_revision == 2)
            .unwrap()
            .snapshot
            .config
            .harness,
        HarnessId::Codex
    );
    core.shutdown().await;
}

const ITEM_DEVICE: &str = "item-device";
const ITEM_SPACE: &str = "item-space";

async fn item_core(
    root: &std::path::Path,
) -> (EngineCore, mast_rpc::RpcClient, String) {
    item_core_with(root, mast_harness::mock::InstructionMockHarness::default()).await
}

async fn item_core_with(
    root: &std::path::Path,
    harness: mast_harness::mock::InstructionMockHarness,
) -> (EngineCore, mast_rpc::RpcClient, String) {
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), ITEM_DEVICE).unwrap();
    let harnesses = mast_engine::HarnessRegistry::new();
    harnesses.register(Arc::new(harness));
    let core =
        EngineCore::assemble(&data, Arc::new(harnesses), HarnessId::Mock, None).unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    client
        .call(
            mast_rpc::methods::MUTATE,
            serde_json::json!({ "op": "createSpace", "spaceId": ITEM_SPACE,
                "deviceId": ITEM_DEVICE, "path": root.to_string_lossy() }),
        )
        .await
        .unwrap();
    for request in [
        serde_json::json!({ "op": "createWorkItem", "spaceId": ITEM_SPACE,
            "title": "Routines", "operationId": "op-item" }),
        serde_json::json!({ "op": "createWorkItemInstruction", "spaceId": ITEM_SPACE,
            "scope": { "type": "spaceRoot" }, "text": "Follow the team checklist.",
            "operationId": "op-instruction" }),
    ] {
        client
            .call(mast_rpc::methods::WORK_OUTLINE_MUTATE, request)
            .await
            .unwrap();
    }
    let item = mast_proto::work_outline::derive_id("op-item", mast_proto::work_outline::roles::ITEM);
    (core, client, item)
}

fn item_draft(root: &std::path::Path, item: Option<String>) -> ScheduledTaskDraft {
    let mut d = draft(root);
    d.config.model = None;
    d.config.reasoning = None;
    d.space_id = Some(ITEM_SPACE.into());
    d.work_item_id = item;
    d
}

#[tokio::test]
async fn a_task_with_a_work_item_attaches_each_run_and_pins_its_instructions() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _client, item) = item_core(&root).await;
    core.scheduled_tasks
        .request(ScheduledTasksRequest::Save {
            id: "linked".into(),
            expected_revision: 0,
            draft: item_draft(&root, Some(item.clone())),
            operation_id: "save-linked".into(),
        })
        .await
        .unwrap();
    let snapshot = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::RunNow {
            id: "linked".into(),
            operation_id: "run-linked".into(),
        })
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    core.scheduled_tasks.tick(now).await.unwrap();
    core.scheduled_tasks.tick(now).await.unwrap();
    let chat_id = snapshot.runs[0].chat_id.clone().unwrap();
    let outline = core
        .workspace
        .work_outline_inputs(ITEM_SPACE);
    let outline = mast_doc::materialize_work_outline(ITEM_SPACE, &outline);
    assert_eq!(outline.item(&item).unwrap().chat_ids, vec![chat_id.clone()]);
    assert!(!outline.unassociated_chat_ids.contains(&chat_id));
    let commands = core.doc_host.open(&chat_id).unwrap().doc().read_commands().unwrap();
    assert_eq!(commands.len(), 1, "a second tick never queues another turn");
    assert!(matches!(
        commands[0].payload,
        mast_doc::SessionCommandPayload::RunWithInstructions { .. }
    ));
    core.shutdown().await;
}

#[tokio::test]
async fn a_work_item_outside_the_task_space_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _client, item) = item_core(&root).await;
    let mut wrong_space = item_draft(&root, Some(item));
    wrong_space.space_id = None;
    let missing = item_draft(&root, Some("no-such-item".into()));
    for (n, d) in [wrong_space, missing].into_iter().enumerate() {
        let saved = core
            .scheduled_tasks
            .request(ScheduledTasksRequest::Save {
                id: format!("bad-{n}"),
                expected_revision: 0,
                draft: d,
                operation_id: format!("save-bad-{n}"),
            })
            .await;
        assert!(saved.is_err(), "draft {n} must be rejected");
    }
    core.shutdown().await;
}

#[tokio::test]
async fn imported_runs_join_their_native_task_and_retired_runs_hide() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _client, _item) = item_core(&root).await;
    // The source registry row names the prompt file each imported run's tag carries.
    let prompt = root.join("routine-a.md");
    let other_account = root.join("other-account/routine-a.md");
    let registry = root.join("scheduled-tasks.json");
    std::fs::write(
        &registry,
        serde_json::to_vec(&serde_json::json!({"scheduledTasks": [
            {"id": "routine-a", "cronExpression": "0 8 * * *", "filePath": prompt}
        ]}))
        .unwrap(),
    )
    .unwrap();
    let candidate = ScheduledImportCandidate {
        source: format!("{}#routine-a", registry.display()),
        source_enabled: true,
        draft: item_draft(&root, None),
        warnings: vec![],
    };
    let task = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::Import {
            candidate,
            operation_id: "import-routine".into(),
        })
        .await
        .unwrap()
        .tasks[0]
        .id
        .clone();
    let legacy_run = |id: &str, task_id: &str, name: &str, file: &std::path::Path, at: i64| {
        core.workspace
            .create_chat(id, Some(ITEM_SPACE), None, None, None)
            .unwrap();
        core.workspace
            .set_chat_scheduled_task_info(
                id,
                &ScheduledChat {
                    task_id: task_id.into(),
                    task_name: name.into(),
                    run_id: id.into(),
                    session_created_at: Some(at),
                    source_file: Some(file.to_string_lossy().into()),
                    ..Default::default()
                },
            )
            .unwrap();
    };
    legacy_run("run-a", "legacy-id-a", "routine-a", &prompt, 1);
    legacy_run("run-b", "legacy-id-b", "routine-a", &prompt, 2);
    legacy_run("other", "legacy-id-c", "routine-a", &other_account, 4);
    legacy_run("retired", "old-routine", "retired-routine", &prompt, 3);
    let now = chrono::Utc::now().timestamp_millis();
    let written = core.scheduled_tasks.reconcile_run_identities(now).unwrap();
    assert_eq!(written, 4);
    let info = |id: &str| core.workspace.chat(id).unwrap().unwrap().scheduled_task_info.unwrap();
    assert_eq!(info("run-a").task_id, task);
    assert_eq!(info("run-b").task_id, task);
    assert_eq!(info("run-b").legacy_task_id.as_deref(), Some("legacy-id-b"));
    // Same task name, another account's prompt file: never joins this task.
    assert_eq!(info("other").task_id, "legacy-id-c");
    assert!(info("other").retired);
    assert!(info("retired").retired);
    let chats = core.workspace.read_chats().unwrap();
    assert_eq!(
        mast_proto::latest_task_chat_ids(&chats),
        std::collections::HashSet::from(["run-b"])
    );
    let outline = mast_doc::materialize_work_outline(
        ITEM_SPACE,
        &core.workspace.work_outline_inputs(ITEM_SPACE),
    );
    assert_eq!(outline.hidden_run_chat_ids, ["other", "retired", "run-a"]);
    // Within the recheck interval nothing runs; after it, a full pass
    // rewrites only the run that arrived since.
    legacy_run("late", "legacy-id-d", "routine-a", &prompt, 5);
    assert_eq!(core.scheduled_tasks.reconcile_run_identities(now + 1).unwrap(), 0);
    assert_eq!(info("late").task_id, "legacy-id-d");
    let recheck = now + 10 * 60_000;
    assert_eq!(core.scheduled_tasks.reconcile_run_identities(recheck).unwrap(), 1);
    assert_eq!(info("late").task_id, task);
    assert_eq!(
        core.scheduled_tasks
            .reconcile_run_identities(recheck + 10 * 60_000)
            .unwrap(),
        0,
        "an applied pass writes nothing"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn an_unsynced_registry_skips_the_identity_pass_and_keeps_it_due() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir.path(), "org", "user"),
        Arc::new(default_registry()),
        HarnessId::Mock,
        Some(mast_engine::EdgeConfig::with_static_token(
            "http://127.0.0.1:1",
            "test",
        )),
    )
    .unwrap();
    assert!(!core.workspace.registry_synced());
    let device = core.workspace.device_id().to_owned();
    core.workspace
        .create_chat("orphan", None, Some(&device), None, None)
        .unwrap();
    let orphan = ScheduledChat {
        task_id: "no-such-task".into(),
        task_name: "routine-a".into(),
        run_id: "orphan".into(),
        ..Default::default()
    };
    core.workspace
        .set_chat_scheduled_task_info("orphan", &orphan)
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    assert_eq!(core.scheduled_tasks.reconcile_run_identities(now).unwrap(), 0);
    let info = core.workspace.chat("orphan").unwrap().unwrap().scheduled_task_info;
    assert_eq!(info, Some(orphan), "a stale replica writes nothing");
    assert_eq!(
        core.scheduled_tasks.identities_checked_at(),
        None,
        "the pass stays due for the first synced tick"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn a_run_its_harness_cannot_start_leaves_no_chat_behind() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let harness = mast_harness::mock::InstructionMockHarness::new(
        mast_harness::InstructionSupport::no_channel(),
    );
    let (core, _client, item) = item_core_with(&root, harness).await;
    core.scheduled_tasks
        .request(ScheduledTasksRequest::Save {
            id: "linked".into(),
            expected_revision: 0,
            draft: item_draft(&root, Some(item)),
            operation_id: "save-linked".into(),
        })
        .await
        .unwrap();
    let snapshot = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::RunNow {
            id: "linked".into(),
            operation_id: "run-linked".into(),
        })
        .await
        .unwrap();
    let chat_id = snapshot.runs[0].chat_id.clone().unwrap();
    core.scheduled_tasks
        .tick(chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let runs = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::List)
        .await
        .unwrap()
        .runs;
    assert_eq!(runs[0].state, ScheduledRunState::Failed);
    assert!(runs[0].detail.as_deref().is_some_and(|d| d.contains("instructions")));
    assert!(core.workspace.chat(&chat_id).unwrap().is_none());
    core.shutdown().await;
}
