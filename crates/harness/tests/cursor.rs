//! CursorHarness integration tests against the fake shim in
//! `tests/fixtures/fake-cursor-shim.sh` (no node/@cursor/sdk involved).

#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use mast_harness::{CancellationToken, CursorHarness, Harness, RunControls, SteerMessage};
use mast_proto::{AgentEvent, DoneStatus, HarnessId, RunRequest, SandboxLevel, ToolCall};

fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-cursor-shim.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    path
}

fn harness() -> CursorHarness {
    CursorHarness::new().with_executable(fixture_path())
}

fn request(prompt: &str) -> RunRequest {
    RunRequest { scheduled_task: None, scheduled_notion_writes: None, approval_rules: false,
        prompt: prompt.into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: String::new(),
        sandbox: SandboxLevel::DangerFullAccess,
        auto_approve: true,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
        fork_from: None,
        instructions: None,
    }
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        request_input: Box::new(move |_| {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Vec::new());
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
    };
    (controls, steer_tx, token)
}

/// Collect events until the first Done (the session parks afterwards).
async fn run_to_first_done(
    harness: &CursorHarness,
    req: RunRequest,
    controls: RunControls,
) -> Vec<AgentEvent> {
    let mut stream = harness.run(req, controls).await.expect("run starts");
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            let done = matches!(ev, AgentEvent::Done { .. });
            events.push(ev);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("run finished in time")
}

#[tokio::test]
async fn happy_path_maps_shim_frames_and_tags_subagents() {
    let (controls, _steer, _token) = controls();
    let events = run_to_first_done(&harness(), request("scenario:happy"), controls).await;

    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::SessionStarted { harness, model, session_id, .. }
            if *harness == HarnessId::Cursor && model == "composer-2.5" && session_id == "agent-1"
    )));
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "planning".into()
    }));
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Hello from cursor".into()
    }));
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "c1".into(),
        call: ToolCall::Exec {
            command: "ls -la".into()
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "c1".into(),
        is_error: false,
        output: None,
        exit_code: None,
        diff: None,
    }));

    // The task spawn is a parent chip; its interior arrives tagged, never bare.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCall { id, call: ToolCall::Unknown { name, .. } }
            if id == "task1" && name == "Agent: scan repo"
    )));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "task1".into(),
        event: Box::new(AgentEvent::TextDelta {
            text: "sub scanning".into()
        }),
    }));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "task1".into(),
        event: Box::new(AgentEvent::ToolCall {
            id: "s1".into(),
            call: ToolCall::Search {
                pattern: "todo".into(),
                path: None,
            },
        }),
    }));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "s1")),
        "subagent tool leaked into the parent feed: {events:?}"
    );
    // The task tool's end doubles as the subagent's tagged terminal — the
    // SDK has no separate frame for it, and without this the chip stays
    // "running" forever.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == "task1"
                && matches!(event.as_ref(), AgentEvent::Done { status: DoneStatus::Completed, .. })
    )));

    assert!(events.contains(&AgentEvent::Usage {
        input_tokens: 11,
        output_tokens: 5
    }));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            session_id: Some(id),
            ..
        }) if id == "agent-1"
    ));
}

#[tokio::test]
async fn steer_after_done_becomes_the_next_turn() {
    let (controls, steer, _token) = controls();
    let harness = harness();
    let mut stream = harness
        .run(request("scenario:happy"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = Vec::new();
        let mut dones = 0;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::Done { .. }) {
                dones += 1;
                if dones == 1 {
                    steer
                        .send(SteerMessage {
                            prompt: "follow up".into(),
                            message_id: None,
                        })
                        .await
                        .expect("steer sent");
                }
            }
            let done = dones >= 2;
            events.push(ev);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("both turns finished in time");

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Steered { .. })),
        "{events:?}"
    );
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "second turn".into()
    }));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Done { .. }))
            .count(),
        2
    );
}

#[tokio::test]
async fn interrupt_maps_to_interrupted_done() {
    let harness = CursorHarness::new()
        .with_executable(fixture_path())
        .with_graces(Duration::from_millis(200), Duration::from_millis(500));
    let (controls, _steer, token) = controls();
    let mut stream = harness
        .run(request("scenario:interrupt"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { .. }) {
                token.cancel();
            }
            let done = matches!(ev, AgentEvent::Done { .. });
            events.push(ev);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("interrupt completed in time");

    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Interrupted,
            ..
        })
    ));
}

#[tokio::test]
async fn fatal_frame_surfaces_the_auth_fix_as_an_errored_done() {
    let (controls, _steer, _token) = controls();
    let events = run_to_first_done(&harness(), request("scenario:fatal"), controls).await;
    match events.last() {
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(message),
            ..
        }) => {
            assert!(message.contains("CURSOR_API_KEY"), "{message}");
        }
        other => panic!("expected errored done, got {other:?}"),
    }
}

#[tokio::test]
async fn shim_crash_mid_run_reports_stderr_tail() {
    let (controls, _steer, _token) = controls();
    let events = run_to_first_done(&harness(), request("scenario:crash"), controls).await;
    match events.last() {
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(message),
            ..
        }) => {
            assert!(
                message.contains("shim exploded") || message.contains("exit code 3"),
                "{message}"
            );
        }
        other => panic!("expected errored done, got {other:?}"),
    }
}

#[tokio::test]
async fn model_discovery_maps_the_live_catalog() {
    let models = harness().models().await.expect("models");
    // Parameterized Auto first; its bare `default` alias twin skipped.
    assert_eq!(models.len(), 2, "{models:?}");
    assert_eq!(models[0].id, "auto-smart");
    assert_eq!(models[0].label, "Auto");
    let optimize = &models[0].options[0];
    assert_eq!(optimize.id, "optimize_for");
    assert_eq!(optimize.label, "Optimize For");
    assert_eq!(
        optimize
            .choices
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["intelligence", "balanced", "cost"]
    );
    // The default comes from the isDefault variant, not the first value.
    assert_eq!(optimize.default_choice, "balanced");
    assert_eq!(models[1].id, "claude-fable-5");
    assert_eq!(models[1].description.as_deref(), Some("Anthropic frontier"));
    // A parameter without displayName labels by id; default = first value.
    assert_eq!(models[1].options[0].id, "thinking");
    assert_eq!(models[1].options[0].default_choice, "enabled");
}

#[tokio::test]
async fn model_discovery_process_uses_configured_private_cwd() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let discovery = dir.path().join("profile-model-discovery");
    let observed = dir.path().join("observed-cwd");
    let wrapper = dir.path().join("cursor-wrapper");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\npwd > \"{}\"\nexec \"{}\" \"$@\"\n",
            observed.display(),
            fixture_path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();

    CursorHarness::new()
        .with_executable(wrapper)
        .with_model_discovery_cwd(&discovery)
        .models()
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(observed).unwrap().trim(),
        discovery.canonicalize().unwrap().to_string_lossy()
    );
}

#[tokio::test]
async fn followup_crash_is_not_hidden_by_a_previous_completed_turn() {
    let (controls, steer, _token) = controls();
    let mut stream = harness()
        .run(request("scenario:followup-crash"), controls)
        .await
        .unwrap();
    let dones = tokio::time::timeout(Duration::from_secs(10), async {
        let mut dones = Vec::new();
        while let Some(event) = stream.next().await {
            if let AgentEvent::Done { status, error, .. } = event.unwrap() {
                dones.push((status, error));
                if dones.len() == 1 {
                    steer
                        .send(SteerMessage {
                            prompt: "follow up".into(),
                            message_id: None,
                        })
                        .await
                        .unwrap();
                } else {
                    break;
                }
            }
        }
        dones
    })
    .await
    .unwrap();
    assert_eq!(dones.len(), 2);
    assert_eq!(dones[0].0, DoneStatus::Completed);
    assert_eq!(dones[1].0, DoneStatus::Errored);
    assert!(dones[1].1.as_deref().unwrap().contains("followup exploded"));
}

#[tokio::test]
async fn cancellation_racing_a_fatal_error_emits_exactly_one_terminal_event() {
    let (controls, _steer, token) = controls();
    let mut stream = harness()
        .run(request("scenario:interrupt-fatal"), controls)
        .await
        .unwrap();
    let statuses = tokio::time::timeout(Duration::from_secs(5), async {
        let mut statuses = Vec::new();
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                AgentEvent::TextDelta { .. } => token.cancel(),
                AgentEvent::Done { status, .. } => statuses.push(status),
                _ => {}
            }
        }
        statuses
    })
    .await
    .unwrap();
    assert_eq!(statuses, vec![DoneStatus::Interrupted]);
}

#[tokio::test]
async fn steering_spam_preserves_every_turn_in_order_and_closes_cleanly() {
    for _ in 0..10 {
        let (controls, steer, _) = controls();
        let mut stream = harness()
            .run(request("scenario:burst"), controls)
            .await
            .unwrap();
        let producer = tokio::spawn(async move {
            for n in 0..200 {
                steer
                    .send(SteerMessage {
                        prompt: format!("ITEM-{n}"),
                        message_id: None,
                    })
                    .await
                    .unwrap();
            }
        });
        let mut texts = Vec::new();
        let mut dones = 0;
        let mut transitions = 0;
        let mut ids = std::collections::HashSet::new();
        let mut current = None;
        tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(event) = stream.next().await {
                match event.unwrap() {
                    AgentEvent::SessionStarted {
                        assistant_message_id,
                        ..
                    } => {
                        ids.insert(assistant_message_id.clone());
                        current = Some(assistant_message_id);
                    }
                    AgentEvent::Steered {
                        assistant_message_id,
                        next_assistant_message_id,
                    } => {
                        assert_eq!(assistant_message_id, current);
                        let next = next_assistant_message_id.unwrap();
                        assert!(ids.insert(next.clone()));
                        current = Some(next);
                        transitions += 1;
                    }
                    AgentEvent::TextDelta { text } => texts.push(text),
                    AgentEvent::Done { status, .. } => {
                        assert_eq!(status, DoneStatus::Completed);
                        dones += 1;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        producer.await.unwrap();
        assert_eq!(
            texts,
            std::iter::once("INITIAL".into())
                .chain((0..200).map(|n| format!("ITEM-{n}")))
                .collect::<Vec<_>>()
        );
        assert_eq!(dones, 201);
        assert_eq!(transitions, 200);
        assert_eq!(ids.len(), 201);
    }
}

#[tokio::test]
async fn cancelling_a_saturated_steering_queue_never_starts_queued_turns() {
    for _ in 0..20 {
        let (controls, steer, token) = controls();
        let mut stream = harness()
            .run(request("scenario:burst-cancel"), controls)
            .await
            .unwrap();
        for n in 0..100 {
            steer
                .send(SteerMessage {
                    prompt: format!("MUST-NOT-RUN-{n}"),
                    message_id: None,
                })
                .await
                .unwrap();
        }
        token.cancel();
        drop(steer);
        let mut dones = 0;
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(event) = stream.next().await {
                match event.unwrap() {
                    AgentEvent::Steered { .. } | AgentEvent::TextDelta { .. } => {
                        panic!("cancelled queue executed")
                    }
                    AgentEvent::Done { status, .. } => {
                        assert_eq!(status, DoneStatus::Interrupted);
                        dones += 1;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(dones, 1);
    }
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;

fn instruction_request(
    prompt: &str,
    resume: Option<&str>,
    transition: mast_proto::InstructionTransition,
) -> RunRequest {
    let mut req = request(prompt);
    req.resume = resume.map(str::to_owned);
    req.instructions = Some(mast_proto::InstructionDelivery {
        envelope: mast_proto::InstructionEnvelope {
            revision_id: "rev-1".into(),
            revision_hash: "hash-1".into(),
            envelope_version: 1,
            envelope_sha256: mast_proto::envelope_sha256("RULES-MARK"),
            rendered: "RULES-MARK".into(),
        },
        transition,
    });
    req
}

fn tools_injection() -> mast_harness::AgentToolsInjection {
    mast_harness::AgentToolsInjection {
        server_name: "mast-agent".into(),
        command: PathBuf::from("/opt/zeron/bin/zeron"),
        args: vec!["mcp".into()],
        env: [("MAST_AGENT_TOKEN".to_owned(), "secret".to_owned())].into(),
    }
}

fn texts(events: &[AgentEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn cursor_reports_agent_tools_and_instruction_support() {
    let harness = harness();
    assert!(harness.supports_agent_tools());
    assert!(harness.instruction_support().is_supported());
}

#[tokio::test]
async fn agent_tools_reach_the_shim_as_mcp_servers() {
    let (c1, _steer, _token) = controls();
    let mut stream = harness()
        .run_with_agent_tools(request("scenario:echo"), c1, tools_injection())
        .await
        .expect("run starts");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event.expect("event");
        let done = matches!(event, AgentEvent::Done { .. });
        events.push(event);
        if done {
            break;
        }
    }
    assert!(texts(&events).contains(&"mcp:yes"), "{events:?}");

    // A resumed run gets the server too.
    let mut resumed = request("scenario:echo");
    resumed.resume = Some("agent-echo".into());
    let (c2, _steer, _token) = controls();
    let mut stream = harness()
        .run_with_agent_tools(resumed, c2, tools_injection())
        .await
        .expect("resume starts");
    let mut resumed_events = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event.expect("event");
        let done = matches!(event, AgentEvent::Done { .. });
        resumed_events.push(event);
        if done {
            break;
        }
    }
    assert!(texts(&resumed_events).contains(&"mcp:yes"), "{resumed_events:?}");

    let (c3, _steer, _token) = controls();
    let plain = run_to_first_done(&harness(), request("scenario:echo"), c3).await;
    assert!(texts(&plain).contains(&"mcp:no"), "{plain:?}");
}

#[tokio::test]
async fn a_fresh_instruction_bearing_run_delivers_the_envelope_and_a_continue_does_not() {
    let fresh = instruction_request(
        "scenario:echo",
        None,
        mast_proto::InstructionTransition::Fresh,
    );
    let (fresh_controls, _steer, _token) = controls();
    let events = run_to_first_done(&harness(), fresh, fresh_controls).await;
    assert!(texts(&events).contains(&"instructions:yes"), "{events:?}");

    let resumed = instruction_request(
        "scenario:echo",
        Some("agent-echo"),
        mast_proto::InstructionTransition::Continue,
    );
    let (resumed_controls, _steer, _token) = controls();
    let events = run_to_first_done(&harness(), resumed, resumed_controls).await;
    assert!(texts(&events).contains(&"instructions:no"), "{events:?}");
}

#[tokio::test]
async fn a_replace_aimed_at_an_existing_session_is_refused_before_spawning() {
    let replace = instruction_request(
        "scenario:echo",
        Some("agent-echo"),
        mast_proto::InstructionTransition::Replace {
            previous_revision_id: "rev-0".into(),
        },
    );
    let (controls, _steer, _token) = controls();
    let error = harness().run(replace, controls).await.err();
    assert!(
        matches!(error, Some(mast_harness::HarnessError::Capability { .. })),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_usage_limit_turn_error_is_classified_before_the_errored_done() {
    let (controls, _steer, _token) = controls();
    let events = run_to_first_done(&harness(), request("scenario:usage-limit"), controls).await;
    let n = events.len();
    match (&events[n - 2], &events[n - 1]) {
        (
            AgentEvent::Error {
                code: Some(code),
                message,
                resets_at: None,
            },
            AgentEvent::Done {
                status: DoneStatus::Errored,
                ..
            },
        ) => {
            assert_eq!(code, "usage_limit");
            assert!(message.contains("usage limit"), "{message}");
            assert!(message.contains("PRO_USER_USAGE_LIMIT"), "{message}");
        }
        other => panic!("expected classified error then errored done, got {other:?}"),
    }
}

#[tokio::test]
async fn a_rate_limit_fatal_is_classified_as_a_rate_limit() {
    let (controls, _steer, _token) = controls();
    let events = run_to_first_done(&harness(), request("scenario:rate-limit"), controls).await;
    let message = events.iter().find_map(|event| match event {
        AgentEvent::Error { code: Some(code), message, .. } if code == "rate_limit" => {
            Some(message.to_ascii_lowercase())
        }
        _ => None,
    });
    let message = message.unwrap_or_else(|| panic!("no rate_limit error in {events:?}"));
    // Transient: nothing here may read as out-of-credits to the quota guard.
    for needle in ["usage limit", "rate limit", "rate_limit", "quota", "limit reached"] {
        assert!(!message.contains(needle), "{needle}: {message}");
    }
}

#[tokio::test]
async fn an_unclassified_failure_emits_no_limit_error() {
    let (controls, _steer, _token) = controls();
    let events = run_to_first_done(&harness(), request("scenario:fatal"), controls).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Error { .. })),
        "{events:?}"
    );
}
