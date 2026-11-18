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
        output: Default::default(),
        after_success: Default::default(),
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
    std::fs::write(&registry,serde_json::to_vec(&serde_json::json!({"scheduledTasks":[{"id":"briefing","enabled":true,"cronExpression":"40 4 * * 1-5","cwd":dir.path(),"filePath":prompt,"permissionMode":"auto"},{"id":"interval","enabled":true,"cronExpression":"*/30 9-17 * * 1-5","cwd":dir.path(),"filePath":prompt},{"id":"each-minute","enabled":true,"cronExpression":"* * * * *","cwd":dir.path(),"filePath":prompt}]})).unwrap()).unwrap();
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
    let each_minute = preview
        .candidates
        .iter()
        .position(|c| c.source.ends_with("#each-minute"))
        .unwrap();
    assert_eq!(preview.candidates.remove(each_minute).draft.grace_minutes, 30);
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

fn item_profile(root: &std::path::Path) -> EngineProfile {
    EngineProfile::development(&root.join("data"), "test-org", "test-user")
}

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
    let core = EngineCore::assemble_with_profile(
        item_profile(root), Arc::new(harnesses), HarnessId::Mock, None
    ).unwrap();
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
async fn background_tasks_require_an_item_and_launch_a_hidden_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, client, item) = item_core(&root).await;
    let service = &core.scheduled_tasks;
    let mut draft = item_draft(&root, None);
    draft.output = ScheduledTaskOutput::Background;
    let save = |draft| ScheduledTasksRequest::Save {
        id: "background".into(),
        expected_revision: 0,
        draft,
        operation_id: "save-background".into(),
    };
    let error = service.request(save(draft.clone())).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Background tasks require a work item")
    );

    draft.work_item_id = Some(item.clone());
    draft.after_success = ScheduledTaskAfterSuccess::Keep;
    service.request(save(draft.clone())).await.unwrap();
    draft.output = ScheduledTaskOutput::Interactive;
    draft.after_success = ScheduledTaskAfterSuccess::Archive;
    let update = |revision| {
        serde_json::to_value(ScheduledTasksRequest::Save {
            id: "background".into(),
            expected_revision: revision,
            draft: draft.clone(),
            operation_id: format!("edit-background-{revision}"),
        })
        .unwrap()
    };
    let legacy = update(1);
    assert!(legacy.pointer("/draft/output").is_none());
    assert!(legacy.pointer("/draft/afterSuccess").is_none());
    let edited: ScheduledTasksSnapshot = client
        .call_as(mast_rpc::methods::SCHEDULED_TASKS, legacy)
        .await
        .unwrap();
    assert_eq!(edited.tasks[0].draft.output, ScheduledTaskOutput::Background);
    assert_eq!(edited.tasks[0].draft.after_success, ScheduledTaskAfterSuccess::Keep);

    let queued = service
        .request(ScheduledTasksRequest::RunNow {
            id: "background".into(),
            operation_id: "run-background".into(),
        })
        .await
        .unwrap();
    let mut explicit = update(2);
    explicit["draft"]["output"] = serde_json::json!("interactive");
    let edited: ScheduledTasksSnapshot = client
        .call_as(mast_rpc::methods::SCHEDULED_TASKS, explicit)
        .await
        .unwrap();
    assert_eq!(edited.tasks[0].draft.output, ScheduledTaskOutput::Interactive);

    service
        .tick(chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();

    let chat_id = queued.runs[0].chat_id.as_ref().unwrap();
    let chat = core.workspace.chat(chat_id).unwrap().unwrap();
    assert!(chat.is_background_task_run());
    let outline = mast_doc::materialize_work_outline(
        ITEM_SPACE,
        &core.workspace.work_outline_inputs(ITEM_SPACE),
    );
    assert_eq!(outline.item(&item).unwrap().chat_ids, [chat_id.clone()]);
    assert_eq!(outline.hidden_run_chat_ids, [chat_id.clone()]);
    let history = service
        .request(ScheduledTasksRequest::History {
            id: "background".into(),
            before: None,
        })
        .await
        .unwrap();
    assert_eq!(history.runs[0].chat_id.as_ref(), Some(chat_id));
    assert_eq!(
        history.runs[0].snapshot.output,
        ScheduledTaskOutput::Background
    );
    assert_eq!(history.runs[0].snapshot.after_success, ScheduledTaskAfterSuccess::Keep);

    let error = client
        .call(
            mast_rpc::methods::SET_PERSONAL_ASSISTANT,
            serde_json::json!({ "chatId": chat_id }),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("background task runs cannot be the personal assistant")
    );
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
    // An old imported run can lose its transcript before backfill finds its file.
    let transcript = root.join("missing.jsonl");
    let wrapper = format!(
        "<scheduled-task name=\"routine-a\" file=\"{}\">\nDo the task\n</scheduled-task>",
        prompt.display(),
    );
    let line = serde_json::json!({
        "type": "user", "uuid": "first", "timestamp": "2026-11-10T08:00:00Z",
        "cwd": root, "sessionId": "missing", "message": {"role": "user", "content": wrapper}
    });
    std::fs::write(&transcript, format!("{line}\n")).unwrap();
    core.external_import
        .import("missing", "missing", &transcript)
        .unwrap();
    let mut missing = info("missing");
    missing.source_file = None;
    core.workspace
        .set_chat_scheduled_task_info("missing", &missing)
        .unwrap();
    std::fs::remove_file(transcript).unwrap();

    // Within the recheck interval nothing runs; after it, a full pass
    // rewrites only the run that arrived since.
    legacy_run("late", "legacy-id-d", "routine-a", &prompt, 5);
    assert_eq!(core.scheduled_tasks.reconcile_run_identities(now + 1).unwrap(), 0);
    assert_eq!(info("late").task_id, "legacy-id-d");
    let recheck = now + 10 * 60_000;
    assert_eq!(core.scheduled_tasks.reconcile_run_identities(recheck).unwrap(), 1);
    assert_eq!(info("late").task_id, task);
    assert_eq!(info("missing"), missing, "the file has not been checked");

    assert_eq!(core.external_import.backfill_scheduled_source_files().unwrap(), 0);
    let recheck = recheck + 10 * 60_000;
    assert_eq!(core.scheduled_tasks.reconcile_run_identities(recheck).unwrap(), 1);
    assert!(info("missing").retired);
    assert_eq!(info("missing").task_id, missing.task_id);
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

async fn save_lifecycle_task(
    core: &EngineCore,
    id: &str,
    revision: u64,
    draft: ScheduledTaskDraft,
) {
    core.scheduled_tasks
        .request(ScheduledTasksRequest::Save {
            id: id.into(),
            expected_revision: revision,
            draft,
            operation_id: format!("save-{id}-{revision}"),
        })
        .await
        .unwrap();
}

/// A well-behaved run: its Background results are published before it ends.
async fn claim_lifecycle_run(core: &EngineCore, task: &str, operation: &str) -> ScheduledRun {
    let run = claim_run_without_results(core, task, operation).await;
    if run.results.is_some() {
        let chat = run.chat_id.clone().unwrap();
        publish_results(core, &run, &chat, &format!("Run: {}\nDone.", run.id));
    }
    run
}

/// Publish a run's results artifact as `chat`; returns the reply.
fn publish_results(
    core: &EngineCore,
    run: &ScheduledRun,
    chat: &str,
    body: &str,
) -> mast_proto::ArtifactPublishReply {
    let results = run.results.as_ref().expect("a Background run with an item");
    let dir = std::env::temp_dir().join(format!("mast-results-{}", &run.id[..8]));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join(format!("results-{chat}.md"));
    std::fs::write(&source, body).unwrap();
    core.artifacts
        .record(mast_engine::artifacts::PublishRequest {
            space_id: ITEM_SPACE.into(),
            work_item_id: run.snapshot.work_item_id.clone(),
            chat_id: Some(chat.into()),
            source,
            title: results.title.clone(),
            kind: None,
            role: Some(results.role.clone()),
            note: "results".into(),
            pin_as_plan: false,
            operation_id: Some(format!("test-{chat}-{}", body.len())),
            by: WorkOutlineActor {
                kind: WorkOutlineActorKind::Agent,
                device_id: ITEM_DEVICE.into(),
                chat_id: Some(chat.into()),
                message_id: None,
            },
        })
        .unwrap()
}

async fn claim_run_without_results(core: &EngineCore, task: &str, operation: &str) -> ScheduledRun {
    let snapshot = core
        .scheduled_tasks
        .request(ScheduledTasksRequest::RunNow {
            id: task.into(),
            operation_id: operation.into(),
        })
        .await
        .unwrap();
    let id = mast_proto::work_outline::derive_id(
        &format!("{}\0{task}", core.workspace.device_id()),
        &format!("manual:{operation}"),
    );
    snapshot.runs.into_iter().find(|run| run.id == id).unwrap()
}

async fn wait_for_run_state(core: &EngineCore, run: &ScheduledRun, state: ScheduledRunState) {
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
        let current = snapshot.runs.iter().find(|r| r.id == run.id).unwrap();
        if current.state == state {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected {state:?}, got {current:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn only_archive_policy_background_runs_are_auto_archived() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _, item) = item_core(&root).await;
    for (id, output, policy, archived) in [
        (
            "archive",
            ScheduledTaskOutput::Background,
            ScheduledTaskAfterSuccess::Archive,
            true,
        ),
        (
            "keep",
            ScheduledTaskOutput::Background,
            ScheduledTaskAfterSuccess::Keep,
            false,
        ),
        (
            "interactive",
            ScheduledTaskOutput::Interactive,
            ScheduledTaskAfterSuccess::Archive,
            false,
        ),
    ] {
        let mut draft = item_draft(&root, Some(item.clone()));
        draft.output = output;
        draft.after_success = policy;
        save_lifecycle_task(&core, id, 0, draft).await;
        let run = claim_lifecycle_run(&core, id, &format!("run-{id}")).await;
        wait_for_run_state(&core, &run, ScheduledRunState::Completed).await;
        let chat = run.chat_id.as_deref().unwrap();
        assert_eq!(
            core.workspace.chat(chat).unwrap().unwrap().archived,
            archived
        );

        // An additional tick writes no lifecycle changes.
        let changes = core.workspace.watch_changes();
        core.scheduled_tasks
            .tick(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();
        assert!(!changes.has_changed().unwrap());
        if archived {
            core.workspace.set_chat_archived(chat, false).unwrap();
            // Replay what would remain if the registry snapshot was saved
            // but the scheduler had not yet acknowledged the archive.
            let profile = item_profile(&root);
            let ledger =
                rusqlite::Connection::open(profile.store_root().join("scheduled-tasks.sqlite3"))
                    .unwrap();
            assert_eq!(ledger.execute(
                "INSERT INTO run_followups SELECT 'archive', id, body, created_at FROM runs WHERE id=?1",
                [&run.id],
            ).unwrap(), 1);
            core.scheduled_tasks
                .tick(chrono::Utc::now().timestamp_millis() + 60_000)
                .await
                .unwrap();
            assert!(!core.workspace.chat(chat).unwrap().unwrap().archived);
        }
    }
    core.shutdown().await;
}

#[tokio::test]
async fn background_failures_update_one_action_and_success_resolves_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _, item) = item_core(&root).await;
    let mut draft = item_draft(&root, Some(item));
    draft.output = ScheduledTaskOutput::Background;
    draft.prompt = "[crash]".into();
    save_lifecycle_task(&core, "routine", 0, draft.clone()).await;
    let actions = || {
        mast_doc::materialize_work_outline(
            ITEM_SPACE,
            &core.workspace.work_outline_inputs(ITEM_SPACE),
        )
        .user_action_requests
        .into_iter()
        .filter(|request| request.dedupe_key.as_deref() == Some("scheduled-run-failed:routine"))
        .collect::<Vec<_>>()
    };
    let mut request_id = None;
    for operation in ["first-failure", "second-failure"] {
        let run = claim_lifecycle_run(&core, "routine", operation).await;
        wait_for_run_state(&core, &run, ScheduledRunState::Failed).await;
        let id = run.chat_id.unwrap();
        assert!(!core.workspace.chat(&id).unwrap().unwrap().archived);
        let requests = actions();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].is_open());
        assert!(requests[0].text.contains("Scheduled integration"));
        assert!(requests[0].text.contains("failed"));
        assert_eq!(requests[0].links[0].target, RequestLinkTarget::Chat { id });
        assert_eq!(
            request_id.get_or_insert_with(|| requests[0].id.clone()),
            &requests[0].id
        );
    }
    draft.prompt = "This run should succeed".into();
    save_lifecycle_task(&core, "routine", 1, draft).await;
    let run = claim_lifecycle_run(&core, "routine", "recovery").await;
    wait_for_run_state(&core, &run, ScheduledRunState::Completed).await;
    let requests = actions();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].is_open());
    assert!(
        requests[0]
            .resolution
            .as_ref()
            .unwrap()
            .note
            .as_deref()
            .unwrap()
            .contains("completed successfully")
    );
    core.shutdown().await;
}

#[tokio::test]
async fn pending_archive_survives_restart_and_respects_open_actions_and_pins() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let harness = mast_harness::mock::InstructionMockHarness::default();
    let (core, _, item) = item_core_with(&root, harness.clone()).await;
    let mut draft = item_draft(&root, Some(item.clone()));
    draft.output = ScheduledTaskOutput::Background;
    draft.prompt = "[hold]".into();
    let mut protected = Vec::new();
    for task_id in ["pinned", "assistant", "action"] {
        save_lifecycle_task(&core, task_id, 0, draft.clone()).await;
        let run = claim_lifecycle_run(&core, task_id, &format!("run-{task_id}")).await;
        wait_for_run_state(&core, &run, ScheduledRunState::Running).await;
        let chat_id = run.chat_id.as_deref().unwrap();
        match task_id {
            "pinned" => core
                .workspace
                .change_sidebar_pin(&SidebarPinChange::Pin {
                    session_id: chat_id.into(),
                    after: None,
                    before: None,
                })
                .unwrap(),
            "assistant" => {
                let chat = core.workspace.chat(chat_id).unwrap().unwrap();
                core.sessions.agent_tokens().personal_assistant().set(&chat);
            }
            _ => {
                core.workspace
                    .work_outline_mutate(
                        &WorkOutlineMutation::CreateUserActionRequest(
                            CreateUserActionRequestParams {
                                work_item_id: item.clone(),
                                origin: UserActionRequestOrigin {
                                    kind: UserActionOriginKind::System,
                                    ..Default::default()
                                },
                                text: "Review the run".into(),
                                info: None,
                                dedupe_key: None,
                                operation_id: "action-protects-run".into(),
                                options: vec![],
                                links: vec![RequestLink {
                                    label: "Open run".into(),
                                    target: RequestLinkTarget::Chat { id: chat_id.into() },
                                }],
                                facts: vec![],
                                kind: Some(UserActionKind::Ask),
                                priority: None,
                            },
                        ),
                        &WorkOutlineActor {
                            kind: WorkOutlineActorKind::System,
                            device_id: core.workspace.device_id().into(),
                            chat_id: None,
                            message_id: None,
                        },
                        &[],
                    )
                    .unwrap();
            }
        }
        harness.release();
        wait_for_run_state(&core, &run, ScheduledRunState::Completed).await;
        assert!(!core.workspace.chat(chat_id).unwrap().unwrap().archived);
        protected.push(chat_id.to_owned());
    }
    core.shutdown().await;
    drop(core);

    let (core, client, _) = item_core(&root).await;
    let request_id = mast_proto::work_outline::derive_id(
        "action-protects-run",
        mast_proto::work_outline::roles::REQUEST,
    );
    client
        .call(
            mast_rpc::methods::WORK_OUTLINE_MUTATE,
            serde_json::json!({
                "op": "resolveUserActionRequest", "userActionRequestId": request_id,
                "operationId": "resolve-archive-blocker",
            }),
        )
        .await
        .unwrap();
    core.scheduled_tasks
        .tick(chrono::Utc::now().timestamp_millis() + 60_000)
        .await
        .unwrap();
    assert!(
        !core
            .workspace
            .chat(&protected[0])
            .unwrap()
            .unwrap()
            .archived
    );
    assert!(
        !core
            .workspace
            .chat(&protected[1])
            .unwrap()
            .unwrap()
            .archived
    );
    assert!(
        core.workspace
            .chat(&protected[2])
            .unwrap()
            .unwrap()
            .archived
    );
    core.shutdown().await;
}

#[tokio::test]
async fn queued_turn_defers_archiving_a_completed_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let harness = mast_harness::mock::InstructionMockHarness::default();
    let (core, _, item) = item_core_with(&root, harness.clone()).await;
    let mut draft = item_draft(&root, Some(item));
    draft.output = ScheduledTaskOutput::Background;
    draft.prompt = "[hold]".into();
    save_lifecycle_task(&core, "routine", 0, draft).await;
    let run = claim_lifecycle_run(&core, "routine", "run-held").await;
    wait_for_run_state(&core, &run, ScheduledRunState::Running).await;
    let chat = run.chat_id.as_deref().unwrap();
    core.doc_host.pause_all_queues();
    let queued = core.doc_host.queue_message(chat, "Later", vec![]).unwrap();
    harness.release();
    wait_for_run_state(&core, &run, ScheduledRunState::Completed).await;
    assert!(!core.workspace.chat(chat).unwrap().unwrap().archived);
    core.doc_host
        .remove_queued_message(chat, &queued)
        .await
        .unwrap();
    core.scheduled_tasks
        .tick(chrono::Utc::now().timestamp_millis() + 60_000)
        .await
        .unwrap();
    assert!(core.workspace.chat(chat).unwrap().unwrap().archived);
    core.shutdown().await;
}

#[tokio::test]
async fn successful_turn_is_reconciled_from_the_journal_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let harness = mast_harness::mock::InstructionMockHarness::default();
    let (core, _, item) = item_core_with(&root, harness.clone()).await;
    let mut draft = item_draft(&root, Some(item));
    draft.output = ScheduledTaskOutput::Background;
    draft.prompt = "[hold]".into();
    save_lifecycle_task(&core, "routine", 0, draft).await;
    let run = claim_lifecycle_run(&core, "routine", "run-before-restart").await;
    wait_for_run_state(&core, &run, ScheduledRunState::Running).await;
    core.scheduled_tasks.shutdown().await;
    harness.release();
    let chat = run.chat_id.as_deref().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !core.sessions.session_status(chat).is_some_and(|status| {
            status.status == SessionStatus::Idle && status.last_completed_turn.is_some()
        }) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(!core.workspace.chat(chat).unwrap().unwrap().archived);
    core.shutdown().await;
    drop(core);

    let (reopened, _, _) = item_core(&root).await;
    wait_for_run_state(&reopened, &run, ScheduledRunState::Completed).await;
    assert!(reopened.workspace.chat(chat).unwrap().unwrap().archived);
    reopened.shutdown().await;
}

#[tokio::test]
async fn deleting_a_task_before_dispatch_does_not_raise_an_action() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _, item) = item_core(&root).await;
    let mut draft = item_draft(&root, Some(item));
    draft.output = ScheduledTaskOutput::Background;
    save_lifecycle_task(&core, "routine", 0, draft).await;
    let run = claim_lifecycle_run(&core, "routine", "run-before-delete").await;
    core.scheduled_tasks
        .request(ScheduledTasksRequest::Delete {
            id: "routine".into(),
            expected_revision: 1,
            operation_id: "delete-before-dispatch".into(),
        })
        .await
        .unwrap();
    wait_for_run_state(&core, &run, ScheduledRunState::Cancelled).await;
    assert!(
        mast_doc::materialize_work_outline(
            ITEM_SPACE,
            &core.workspace.work_outline_inputs(ITEM_SPACE)
        )
        .user_action_requests
        .is_empty()
    );
    core.shutdown().await;
}

#[tokio::test]
async fn cleanup_previews_and_archives_only_confirmed_idle_runs_on_the_host() {
    use mast_rpc::methods;
    use serde_json::json;
    use std::collections::BTreeSet;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (core, client, item) = item_core(&root).await;
    let add_run = |id: &str, task: &str, at, space, device: &str| {
        core.workspace.create_chat(id, space, Some(device), None, None).unwrap();
        core.workspace.set_chat_scheduled_task_info(id, &ScheduledChat {
            task_id: task.into(),
            task_name: task.into(),
            run_id: id.into(),
            session_created_at: Some(at),
            ..Default::default()
        }).unwrap();
    };
    for (id, at) in [
        ("run-a", 100),
        ("run-z", 100),
        ("pinned", 50),
        ("required-action", 49),
        ("queued", 48),
        ("already-archived", 47),
        ("linked-action", 46),
    ] {
        add_run(id, "briefing", at, Some(ITEM_SPACE), ITEM_DEVICE);
    }
    add_run("other-space", "briefing", 200, None, ITEM_DEVICE);
    add_run("peer-old", "briefing", 50, None, "peer");
    add_run("peer-new", "briefing", 150, None, "peer");
    add_run("assistant-old", "assistant-task", 1, Some(ITEM_SPACE), ITEM_DEVICE);
    add_run("assistant-new", "assistant-task", 2, Some(ITEM_SPACE), ITEM_DEVICE);
    let retired = core.scheduled_tasks.request(ScheduledTasksRequest::Import {
        candidate: ScheduledImportCandidate {
            source: format!("{}#retired-source", root.join("scheduled-tasks.json").display()),
            source_enabled: false,
            draft: item_draft(&root, None),
            warnings: vec![],
        },
        operation_id: "import-retired-source".into(),
    }).await.unwrap().tasks.remove(0);
    core.scheduled_tasks.request(ScheduledTasksRequest::Delete {
        id: retired.id.clone(),
        expected_revision: retired.revision,
        operation_id: "delete-retired-source".into(),
    }).await.unwrap();
    add_run("retired-desktop", &retired.id, 1, Some(ITEM_SPACE), ITEM_DEVICE);
    let mut desktop = core.workspace.chat("retired-desktop").unwrap().unwrap().scheduled_task_info.unwrap();
    desktop.source_file = Some(root.join("retired.md").to_string_lossy().into_owned());
    core.workspace.set_chat_scheduled_task_info("retired-desktop", &desktop).unwrap();
    core.workspace.set_chat_archived("already-archived", true).unwrap();
    core.workspace.create_chat("ordinary", Some(ITEM_SPACE), None, None, None).unwrap();
    core.workspace.change_sidebar_pin(&SidebarPinChange::Pin {
        session_id: "pinned".into(), before: None, after: None,
    }).unwrap();
    core.doc_host.open("queued").unwrap().doc().push_queued(&mast_doc::QueuedMessage {
        id: "queued-message".into(),
        text: "Follow up".into(),
        attachments: vec![],
        hold_for_turn_end: true,
        issued_by: ITEM_DEVICE.into(),
        issued_at: 0,
        edited_at: None,
        delivery_gate: Some(mast_doc::QueueDeliveryGate::ReviewRequired {
            previous_lease_id: "lease".into(), owner_device_id: ITEM_DEVICE.into(),
            since_ms: 0, base_text_hash: "test".into(),
        }),
    }).unwrap();
    client.call(methods::SET_PERSONAL_ASSISTANT, json!({"chatId": "assistant-old"})).await.unwrap();
    client.call(methods::WORK_OUTLINE_MUTATE, json!({
        "op":"createUserActionRequest", "workItemId":item,
        "origin":{"chatId":"required-action"}, "text":"Review this run",
        "links":[{"label":"Related run", "target":{"type":"chat", "id":"linked-action"}}],
        "operationId":"cleanup-open-action",
    })).await.unwrap();

    let cleanup = |mode: serde_json::Value| {
        let client = &client;
        async move {
            client.call_as::<ScheduledTasksSnapshot>(methods::SCHEDULED_TASKS, mode)
                .await.unwrap().cleanup.unwrap()
        }
    };
    let ids = |result: &ScheduledRunCleanup| result.chat_ids().cloned().collect::<BTreeSet<_>>();
    let preview = cleanup(json!({"action":"cleanup", "mode":"preview"})).await;
    assert_eq!(ids(&preview), BTreeSet::from(["run-a".into(), "retired-desktop".into()]));
    assert_eq!(preview.tasks.iter().map(|row| row.count).collect::<Vec<_>>(), [1, 1]);
    assert!(!preview.applied);
    for id in preview.chat_ids() {
        assert!(!core.workspace.chat(id).unwrap().unwrap().archived);
    }
    let confirm = json!({"action":"cleanup", "mode":"apply", "expectedChatIds":ids(&preview)});
    core.workspace.change_sidebar_pin(&SidebarPinChange::Pin {
        session_id: "run-a".into(), before: None, after: None,
    }).unwrap();
    let stale = cleanup(confirm.clone()).await;
    assert_eq!(ids(&stale), BTreeSet::from(["retired-desktop".into()]));
    assert!(!stale.applied, "a new pin requires another confirmation");
    assert!(!core.workspace.chat("retired-desktop").unwrap().unwrap().archived);
    core.workspace.change_sidebar_pin(&SidebarPinChange::Unpin {
        session_id: "run-a".into(),
    }).unwrap();
    let applied = cleanup(confirm.clone()).await;
    assert!(applied.applied);
    assert_eq!(applied.tasks, preview.tasks);
    let second = cleanup(confirm).await;
    assert!(second.tasks.is_empty());
    let archived: BTreeSet<_> = core.workspace.read_chats().unwrap().into_iter()
        .filter(|chat| chat.archived).map(|chat| chat.id).collect();
    assert_eq!(archived, BTreeSet::from(["already-archived".into(), "run-a".into(), "retired-desktop".into()]));
    core.workspace.set_chat_archived("run-a", false).unwrap();
    assert!(!core.workspace.chat("run-a").unwrap().unwrap().archived);
    core.shutdown().await;
}

#[tokio::test]
async fn cleanup_leaves_running_turns_and_pending_input_alone() {
    for (asks, status) in [(false, SessionStatus::Working), (true, SessionStatus::AwaitingInput)] {
        let dir = tempfile::tempdir().unwrap();
        let registry = mast_engine::HarnessRegistry::new();
        registry.register(Arc::new(HeldOpenFixture { asks }));
        let core = EngineCore::assemble_with_profile(
            EngineProfile::development(dir.path(), "org", "user"),
            Arc::new(registry), HarnessId::ClaudeCode, None,
        ).unwrap();
        // No scheduled ledger rows. These are follow-up turns on old chats.
        for (id, at) in [("old", 1), ("latest", 2)] {
            core.workspace.create_chat(id, None, Some(&core.device_id), None, None).unwrap();
            core.workspace.set_chat_scheduled_task_info(id, &ScheduledChat {
                task_id: "task".into(), task_name: "Task".into(), run_id: id.into(),
                session_created_at: Some(at), ..Default::default()
            }).unwrap();
        }
        let request = serde_json::from_value(serde_json::json!({
            "prompt":"Follow up", "cwd":dir.path(), "model":"fixture-model",
            "harness":"claude-code", "sandbox":"workspace-write",
        })).unwrap();
        core.sessions.dispatch("old", HarnessId::ClaudeCode, request, None).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !core.sessions.session_status("old").is_some_and(|s| s.status == status) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        let preview = core.scheduled_tasks.request(ScheduledTasksRequest::Cleanup {
            mode: ScheduledRunCleanupMode::Preview,
        }).await.unwrap().cleanup.unwrap();
        assert!(preview.tasks.is_empty(), "{status:?} must keep the old run eligible for interaction");
        core.shutdown().await;
    }
}

fn failure_actions(core: &EngineCore, task: &str) -> Vec<UserActionRequestView> {
    let key = format!("scheduled-run-failed:{task}");
    mast_doc::materialize_work_outline(
        ITEM_SPACE,
        &core.workspace.work_outline_inputs(ITEM_SPACE),
    )
    .user_action_requests
    .into_iter()
    .filter(|request| request.dedupe_key.as_deref() == Some(key.as_str()))
    .collect()
}

async fn finished_run(core: &EngineCore, run: &ScheduledRun) -> ScheduledRun {
    core.scheduled_tasks
        .request(ScheduledTasksRequest::History {
            id: run.task_id.clone(),
            before: None,
        })
        .await
        .unwrap()
        .runs
        .into_iter()
        .find(|r| r.id == run.id)
        .unwrap()
}

#[tokio::test]
async fn a_background_run_that_skips_publishing_fails_and_is_not_archived() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _, item) = item_core(&root).await;
    let mut draft = item_draft(&root, Some(item.clone()));
    draft.output = ScheduledTaskOutput::Background;
    save_lifecycle_task(&core, "routine", 0, draft).await;
    let run = claim_run_without_results(&core, "routine", "silent").await;
    let results = run.results.clone().unwrap();
    assert_eq!(results.title, "Results: Scheduled integration");
    assert_eq!(results.role, ScheduledRunResults::role_for("routine"));
    assert!(results.revision.is_none());
    wait_for_run_state(&core, &run, ScheduledRunState::Failed).await;

    let run = finished_run(&core, &run).await;
    assert!(run.detail.as_deref().unwrap().contains("without publishing"));
    assert!(run.results.unwrap().revision.is_none());
    assert!(!core.workspace.chat(run.chat_id.as_deref().unwrap()).unwrap().unwrap().archived);
    let actions = failure_actions(&core, "routine");
    assert_eq!(actions.len(), 1);
    assert!(actions[0].is_open());
    assert!(actions[0].info.as_deref().unwrap().contains("without publishing"));
    core.shutdown().await;
}

#[tokio::test]
async fn only_this_runs_own_new_revision_counts_as_published() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _, item) = item_core(&root).await;
    let mut draft = item_draft(&root, Some(item.clone()));
    draft.output = ScheduledTaskOutput::Background;
    save_lifecycle_task(&core, "routine", 0, draft).await;

    // First run publishes: its revision is recorded on the run.
    let first = claim_run_without_results(&core, "routine", "first").await;
    let chat = first.chat_id.clone().unwrap();
    let reply = publish_results(&core, &first, &chat, "Run: first\nAll quiet.");
    wait_for_run_state(&core, &first, ScheduledRunState::Completed).await;
    let first = finished_run(&core, &first).await;
    assert_eq!(first.results.unwrap().revision, Some(reply.revision));

    // Identical content records no revision, so the run did not publish.
    let second = claim_run_without_results(&core, "routine", "second").await;
    let chat = second.chat_id.clone().unwrap();
    let repeat = publish_results(&core, &second, &chat, "Run: first\nAll quiet.");
    assert!(!repeat.changed);
    wait_for_run_state(&core, &second, ScheduledRunState::Failed).await;
    let second = finished_run(&core, &second).await;
    assert!(second.detail.as_deref().unwrap().contains("not published by this run"));

    // Another chat publishing the same artifact (an interactive chat on the
    // item) is not this run's publication.
    let third = claim_run_without_results(&core, "routine", "third").await;
    publish_results(&core, &third, "someone-elses-chat", "Run: other\nManual edit.");
    wait_for_run_state(&core, &third, ScheduledRunState::Failed).await;
    assert!(finished_run(&core, &third).await.detail.unwrap().contains("not published by this run"));

    // Changed content from the run's own chat succeeds again.
    let fourth = claim_run_without_results(&core, "routine", "fourth").await;
    let chat = fourth.chat_id.clone().unwrap();
    let reply = publish_results(&core, &fourth, &chat, "Run: fourth\nNew finding.");
    wait_for_run_state(&core, &fourth, ScheduledRunState::Completed).await;
    assert_eq!(finished_run(&core, &fourth).await.results.unwrap().revision, Some(reply.revision));
    assert!(failure_actions(&core, "routine").iter().all(|a| !a.is_open()));
    core.shutdown().await;
}

#[tokio::test]
async fn tasks_sharing_an_item_publish_to_their_own_results_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _, item) = item_core(&root).await;
    for (id, name) in [("slack", "Slack"), ("github", "GitHub")] {
        let mut draft = item_draft(&root, Some(item.clone()));
        draft.name = name.into();
        draft.output = ScheduledTaskOutput::Background;
        save_lifecycle_task(&core, id, 0, draft).await;
    }
    let slack = claim_run_without_results(&core, "slack", "s").await;
    let github = claim_run_without_results(&core, "github", "g").await;
    assert_ne!(
        slack.results.as_ref().unwrap().artifact_id,
        github.results.as_ref().unwrap().artifact_id
    );
    // Only the Slack run publishes; the GitHub run cannot lean on it.
    let chat = slack.chat_id.clone().unwrap();
    publish_results(&core, &slack, &chat, "Run: s");
    wait_for_run_state(&core, &slack, ScheduledRunState::Completed).await;
    wait_for_run_state(&core, &github, ScheduledRunState::Failed).await;
    core.shutdown().await;
}

#[tokio::test]
async fn interactive_runs_carry_no_results_contract() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _, item) = item_core(&root).await;
    save_lifecycle_task(&core, "chatty", 0, item_draft(&root, Some(item))).await;
    let run = claim_run_without_results(&core, "chatty", "interactive").await;
    assert!(run.results.is_none());
    wait_for_run_state(&core, &run, ScheduledRunState::Completed).await;
    assert!(failure_actions(&core, "chatty").is_empty());
    core.shutdown().await;
}

#[tokio::test]
async fn a_background_run_may_publish_its_results_once_and_never_as_the_plan() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _, item) = item_core(&root).await;
    let mut draft = item_draft(&root, Some(item.clone()));
    draft.output = ScheduledTaskOutput::Background;
    save_lifecycle_task(&core, "routine", 0, draft).await;
    let run = claim_run_without_results(&core, "routine", "guarded").await;
    core.scheduled_tasks
        .tick(chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let results = run.results.clone().unwrap();
    let chat = run.chat_id.clone().unwrap();
    let caller = mast_engine::artifacts::AgentCaller {
        chat_id: chat.clone(),
        run_id: "agent-run".into(),
        space_id: ITEM_SPACE.into(),
        device_id: ITEM_DEVICE.into(),
        work_item_id: Some(item),
    };
    std::fs::write(root.join("results.md"), format!("Run: {}\nFirst.", run.id)).unwrap();
    let publish = |title: &str, role: &str, key: &str, pin: bool| {
        core.artifacts.agent_publish(
            &caller,
            &serde_json::json!({
                "path": "results.md", "title": title, "role": role, "note": "n",
                "pinAsPlan": pin, "idempotencyKey": key,
            }),
        )
    };
    let err = |outcome: Result<_, mast_proto::artifacts::ArtifactError>| {
        outcome.map(|_: serde_json::Value| ()).unwrap_err().to_string()
    };
    assert!(err(publish("Other title", &results.role, "k1", false)).contains("use both exactly"));
    assert!(err(publish(&results.title, "plan", "k1", false)).contains("use both exactly"));
    assert!(err(publish("Anything", "scheduled-results:other", "k1", false)).contains("another task"));
    assert!(err(publish(&results.title, &results.role, "k1", true)).contains("not the item's plan"));

    let ok = publish(&results.title, &results.role, "k1", false).unwrap();
    assert_eq!(ok["artifactId"], results.artifact_id);
    assert!(core.workspace.artifact(&results.artifact_id).unwrap().pinned_as_plan == false);
    // A retry with the same key replays; a new key is a second publish.
    publish(&results.title, &results.role, "k1", false).unwrap();
    std::fs::write(root.join("results.md"), "Run: again").unwrap();
    assert!(err(publish(&results.title, &results.role, "k2", false)).contains("publishes once"));
    // Unrelated artifacts from the run are not restricted.
    publish("Scratch notes", "doc", "k3", false).unwrap();
    core.shutdown().await;
}

#[tokio::test]
async fn renaming_a_task_keeps_one_results_artifact_and_its_history() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _, item) = item_core(&root).await;
    let mut draft = item_draft(&root, Some(item.clone()));
    draft.output = ScheduledTaskOutput::Background;
    save_lifecycle_task(&core, "routine", 0, draft.clone()).await;
    let first = claim_lifecycle_run(&core, "routine", "before-rename").await;
    wait_for_run_state(&core, &first, ScheduledRunState::Completed).await;
    let first = finished_run(&core, &first).await;
    let original = first.results.clone().unwrap();

    draft.name = "Renamed integration".into();
    save_lifecycle_task(&core, "routine", 1, draft).await;
    let second = claim_run_without_results(&core, "routine", "after-rename").await;
    // The claim names the new title; launch resolves to the existing artifact.
    assert_ne!(second.results.as_ref().unwrap().artifact_id, original.artifact_id);
    let mut as_first = second.clone();
    as_first.results = Some(original.clone());
    let chat = second.chat_id.clone().unwrap();
    let reply = publish_results(&core, &as_first, &chat, "Run: second\nAfter the rename.");
    assert_eq!(reply.artifact_id, original.artifact_id);
    wait_for_run_state(&core, &second, ScheduledRunState::Completed).await;
    let second = finished_run(&core, &second).await;
    let results = second.results.unwrap();
    assert_eq!(results.artifact_id, original.artifact_id);
    assert_eq!(results.title, original.title);
    assert_eq!(results.revision, Some(reply.revision));
    assert_eq!(reply.revision, 2);
    let artifacts = core
        .workspace
        .artifacts_of_space(ITEM_SPACE)
        .into_iter()
        .filter(|a| a.role.as_deref() == Some(original.role.as_str()))
        .count();
    assert_eq!(artifacts, 1);
    core.shutdown().await;
}

#[tokio::test]
async fn simultaneous_publishes_from_one_run_let_exactly_one_through() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (core, _, item) = item_core(&root).await;
    for round in 0..4 {
        let task = format!("racer-{round}");
        let mut draft = item_draft(&root, Some(item.clone()));
        draft.name = format!("Racer {round}");
        draft.output = ScheduledTaskOutput::Background;
        save_lifecycle_task(&core, &task, 0, draft).await;
        let run = claim_run_without_results(&core, &task, &format!("race-{round}")).await;
        core.scheduled_tasks
            .tick(chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();
        let results = run.results.clone().unwrap();
        let caller = mast_engine::artifacts::AgentCaller {
            chat_id: run.chat_id.clone().unwrap(),
            run_id: "agent-run".into(),
            space_id: ITEM_SPACE.into(),
            device_id: ITEM_DEVICE.into(),
            work_item_id: Some(item.clone()),
        };
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|n| {
                std::fs::write(root.join(format!("race-{round}-{n}.md")), format!("Run: {n}")).unwrap();
                let (artifacts, caller, barrier, results) =
                    (core.artifacts.clone(), caller.clone(), barrier.clone(), results.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    artifacts
                        .agent_publish(
                            &caller,
                            &serde_json::json!({
                                "path": format!("race-{round}-{n}.md"),
                                "title": results.title, "role": results.role,
                                "note": "n", "idempotencyKey": format!("key-{n}"),
                            }),
                        )
                        .is_ok()
                })
            })
            .collect();
        let accepted = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count();
        assert_eq!(accepted, 1, "round {round}");
        let artifact = core.workspace.artifact(&results.artifact_id).unwrap();
        assert_eq!(artifact.revisions.len(), 1, "round {round}");
        wait_for_run_state(&core, &run, ScheduledRunState::Completed).await;
    }
    core.shutdown().await;
}
