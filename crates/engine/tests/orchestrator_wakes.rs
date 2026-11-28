//! Worker-event wakes for orchestrator chats, against a fake harness and a
//! fake clock: batching, idle-only delivery, the user-first holds, the
//! interrupt of an event-started turn, settings, dedupe and non-children.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use mast_engine::orchestrator_wakes::{Clock, OrchestratorWakes, USER_QUIET_MS, WAKE_WINDOW_MS};
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::{Harness, HarnessError, RunControls};
use mast_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, Session, SessionStatus,
    SteeringMode, WakeEventKind, WakeSettings, WorkOutlineSnapshot,
};

const ORCH: &str = "orch";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_limit_holds_orchestrator_wakes_until_reset() {
    let rig = Rig::new().await;
    let journals = rig.store.parent().unwrap().join("data/orgs/dev-org/dev-user/journals");
    let journal = mast_engine::RunJournal::open(journals).unwrap();
    journal.append(ORCH, &AgentEvent::Error {
        message: "limit".into(), code: Some("usage_5h".into()), resets_at: Some(2_000_000),
    }).unwrap();
    rig.wakes.report_to_parent("w1", "held report").await.unwrap();
    rig.wakes.evaluate(ORCH, true).await;
    assert!(rig.wake_prompts().is_empty());
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);
    rig.clock.store(2_000_000, Ordering::SeqCst);
    rig.wakes.evaluate(ORCH, true).await;
    rig.wait_for(|| rig.wake_prompts().iter().any(|p| p.contains("held report")), "wake after reset").await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discarded_wake_row_is_reclaimed_again_after_crash_and_unarchive() {
    let rig = Rig::new().await;
    // The fake-clock service can still arm timers, despite having no watchers.
    // Keep its report held throughout the simulated crashes.
    rig.wakes.note_user_activity(ORCH, Some(true)).await;
    rig.wakes.report_to_parent("w1", "discard this queued report").await.unwrap();
    let mut saved: serde_json::Value = serde_json::from_slice(&std::fs::read(rig.store.with_file_name("orchestrator-wake-events.json")).unwrap()).unwrap();
    // The core watcher shares the doc host's in-memory wake persistence. Use
    // a separate store for the crash fixture so an archive notification cannot
    // overwrite the simulated on-disk state before recovery reads it.
    rig.core.orchestrator_wakes.shutdown();
    let restart_dir = tempfile::tempdir_in(rig.store.parent().unwrap()).unwrap();
    let restart_store = restart_dir.path().join("wakes.json");
    let path = restart_store.with_file_name("orchestrator-wake-events.json");
    saved["orchestrators"][ORCH][0]["message_id"] = "discarded-row".into();
    let handle = rig.core.doc_host.open(ORCH).unwrap();
    let row = mast_doc::QueuedMessage { id: "discarded-row".into(), text: "Worker updates".into(), attachments: vec![],
        hold_for_turn_end: false, issued_by: rig.core.device_id.clone(), issued_at: 1, edited_at: None, delivery_gate: None };
    handle.doc().push_queued(&row).unwrap();
    rig.core.workspace.set_chat_archived(ORCH, true).unwrap();
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let tick = rig.clock.clone();
    let restarted = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(restart_store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    assert!(handle.doc().read_queue().unwrap().is_empty());
    assert!(restarted.status(ORCH).held.is_empty());
    // Simulate a crash before the removal reached the chat snapshot.
    // Restore the persisted discard receipt from that crash boundary. The
    // live test host may already have snapshotted and pruned the real one.
    saved["orchestrators"][ORCH][0]["discarded"] = true.into();
    saved["orchestrators"][ORCH][0]["message_id"] = serde_json::Value::Null;
    saved["orchestrators"][ORCH][0]["retired_messages"] = serde_json::json!([{"chat_id": ORCH, "message_id": "discarded-row"}]);
    handle.doc().push_queued(&row).unwrap();
    rig.core.workspace.set_chat_archived(ORCH, false).unwrap();
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let tick = rig.clock.clone();
    let restarted_again = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(restart_store), Arc::new(move || tick.load(Ordering::SeqCst)));
    assert!(handle.doc().read_queue().unwrap().is_empty());
    assert!(restarted_again.status(ORCH).held.is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_coordinator_drops_durable_events_on_restore() {
    let rig = Rig::new().await;
    rig.wakes.report_to_parent("w1", "discard disabled report").await.unwrap();
    let path = rig.store.with_file_name("orchestrator-wake-events.json");
    let saved = std::fs::read(&path).unwrap();
    let mut settings = rig.wakes.settings(ORCH);
    settings.enabled = false;
    rig.wakes.set_settings(ORCH, settings).await;
    // The event store may still reflect the state before settings were saved.
    std::fs::write(&path, saved).unwrap();
    let tick = rig.clock.clone();
    let restarted = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    assert!(restarted.status(ORCH).held.is_empty());
    rig.advance(2 * USER_QUIET_MS);
    restarted.evaluate(ORCH, true).await;
    assert!(rig.wake_prompts().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_peer_tells_the_parent_once_per_outage() {
    let rig = Rig::new().await;
    let parents = vec![ORCH.to_string(), "not-a-chat".to_string()];
    let told = rig.wakes.note_peer_unreachable("fedora", "outage-1", &parents, "connection refused").await;
    assert_eq!(told, 1, "only the parent chat on this device is told");
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);
    let again = rig.wakes.note_peer_unreachable("fedora", "outage-1", &parents, "connection refused").await;
    assert_eq!(again, 0, "the same outage is not announced twice");
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);
    rig.advance(2 * USER_QUIET_MS);
    rig.wakes.evaluate(ORCH, true).await;
    rig.wait_for(|| rig.wake_prompts().iter().any(|p| p.contains("fedora") && p.contains("is down")), "the unreachable notice reaching the parent").await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_poller_tells_a_remembered_parent_when_the_peer_stays_unreachable() {
    let rig = Rig::new().await;
    let dir = rig.store.parent().unwrap().join("links-notice");
    std::fs::create_dir_all(&dir).unwrap();
    let url = "ws://127.0.0.1:1".to_string();
    let links = mast_engine::peer_outbound::Links { peers: vec![mast_engine::peer_outbound::SavedPeer { name: "fedora".into(), url: url.clone(), token: "t".into() }] };
    mast_engine::peer_outbound::save_links(&dir, &links).unwrap();
    // What an earlier successful poll (before an engine restart) left behind.
    let remembered = mast_engine::peer_outbound::PeerLinkStatus { url, state: "ok".into(), parents: vec![ORCH.to_string()], ..Default::default() };
    std::fs::write(mast_engine::peer_outbound::status_path(&dir), serde_json::to_vec(&std::collections::HashMap::from([("fedora".to_string(), remembered)])).unwrap()).unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = mast_engine::peer_outbound::spawn_with(dir.clone(), rig.wakes.clone(), cancel.clone(), Duration::from_millis(20), Duration::ZERO);
    rig.wait_for(|| rig.wakes.status(ORCH).held.len() == 1, "the parent being told").await;
    let status = mast_engine::peer_outbound::load_status(&dir).remove("fedora").unwrap();
    assert_eq!(status.state, "unreachable");
    assert_eq!(status.parents, [ORCH]);
    cancel.cancel();
    let _ = task.await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_poller_records_an_unreachable_peer_with_its_stored_url() {
    let rig = Rig::new().await;
    let dir = rig.store.parent().unwrap().join("links");
    std::fs::create_dir_all(&dir).unwrap();
    // A loopback port nothing listens on, so every poll fails.
    let url = "ws://127.0.0.1:1".to_string();
    let links = mast_engine::peer_outbound::Links { peers: vec![mast_engine::peer_outbound::SavedPeer { name: "fedora".into(), url: url.clone(), token: "t".into() }] };
    mast_engine::peer_outbound::save_links(&dir, &links).unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = mast_engine::peer_outbound::spawn_with(dir.clone(), rig.wakes.clone(), cancel.clone(), Duration::from_millis(20), Duration::from_secs(60));
    rig.wait_for(|| mast_engine::peer_outbound::load_status(&dir).get("fedora").is_some_and(|s| s.state == "unreachable"), "the unreachable status").await;
    let status = mast_engine::peer_outbound::load_status(&dir).remove("fedora").unwrap();
    assert_eq!(status.url, url);
    assert!(status.unreachable_since_ms.is_some());
    cancel.cancel();
    let _ = task.await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_coordinator_does_not_receive_reports() {
    let rig = Rig::new().await;
    let mut settings = rig.wakes.settings(ORCH);
    settings.enabled = false;
    rig.wakes.set_settings(ORCH, settings).await;
    let result = rig.wakes.report_to_parent("w1", "disabled report").await.unwrap();
    assert_eq!(result.delivery, "parentReportsOff");
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.advance(2 * USER_QUIET_MS);
    rig.wakes.evaluate(ORCH, true).await;
    assert!(rig.wake_prompts().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archived_coordinator_drops_durable_report_on_restore() {
    let rig = Rig::new().await;
    rig.wakes.report_to_parent("w1", "discarded report").await.unwrap();
    rig.core.workspace.set_chat_archived(ORCH, true).unwrap();
    let tick = rig.clock.clone();
    let restarted = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    assert!(restarted.status(ORCH).held.is_empty());
    rig.core.workspace.set_chat_archived(ORCH, false).unwrap();
    rig.advance(2 * USER_QUIET_MS);
    restarted.evaluate(ORCH, true).await;
    assert!(rig.wake_prompts().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_report_survives_service_restart_and_respects_draft() {
    let rig = Rig::new().await;
    rig.wakes.note_user_activity(ORCH, Some(true)).await;
    rig.wakes.report_to_parent("w1", "restart report").await.unwrap();
    let tick = rig.clock.clone();
    let restarted = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    restarted.note_user_activity(ORCH, Some(true)).await;
    rig.advance(2 * USER_QUIET_MS);
    restarted.evaluate(ORCH, false).await;
    assert!(rig.wake_prompts().is_empty());
    assert_eq!(restarted.status(ORCH).held.len(), 1);
    restarted.note_user_activity(ORCH, Some(false)).await;
    rig.advance(2 * USER_QUIET_MS);
    restarted.evaluate(ORCH, false).await;
    rig.wait_for(|| rig.wake_prompts().iter().any(|prompt| prompt.contains("restart report")), "restored report").await;
    assert_eq!(rig.wake_prompts().iter().filter(|prompt| prompt.contains("restart report")).count(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_reclaims_only_wake_rows() {
    let rig = Rig::new().await;
    rig.wakes.report_to_parent("w1", "queued report").await.unwrap();
    let path = rig.store.with_file_name("orchestrator-wake-events.json");
    let mut saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    saved["orchestrators"][ORCH][0]["message_id"] = "wake-row".into();
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let handle = rig.core.doc_host.open(ORCH).unwrap();
    for (id, text) in [("user-row", "user message"), ("wake-row", "Worker updates")] {
        handle.doc().push_queued(&mast_doc::QueuedMessage { id: id.into(), text: text.into(), attachments: vec![],
            hold_for_turn_end: false, issued_by: rig.core.device_id.clone(), issued_at: 1, edited_at: None,
            delivery_gate: (id == "user-row").then(|| mast_doc::QueueDeliveryGate::ReviewRequired {
                previous_lease_id: "lease".into(), owner_device_id: rig.core.device_id.clone(), since_ms: 1, base_text_hash: "hash".into(),
            }) }).unwrap();
    }
    let tick = rig.clock.clone();
    let restarted = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    let queue = handle.doc().read_queue().unwrap();
    assert_eq!(queue.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(), vec!["user-row"]);
    assert_eq!(restarted.status(ORCH).held.len(), 1);
    // The first removal might not have reached the session snapshot before a
    // second crash; recovery must reclaim the same old row again.
    handle.doc().push_queued(&mast_doc::QueuedMessage { id: "wake-row".into(), text: "Worker updates".into(), attachments: vec![],
        hold_for_turn_end: false, issued_by: rig.core.device_id.clone(), issued_at: 1, edited_at: None, delivery_gate: None }).unwrap();
    let tick = rig.clock.clone();
    let restarted_again = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    assert_eq!(handle.doc().read_queue().unwrap().iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(), vec!["user-row"]);
    assert_eq!(restarted_again.status(ORCH).held.len(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_with_reports_off_drops_a_pulled_back_ordinary_report() {
    let rig = Rig::new().await;
    rig.wakes.report_to_parent("w1", "pulled back report").await.unwrap();
    let path = rig.store.with_file_name("orchestrator-wake-events.json");
    // Crash boundary: the Manager pullback retired the row ID, but the queue
    // removal never reached the snapshot, so the old row is still persisted.
    let mut saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    saved["orchestrators"][ORCH][0]["message_id"] = serde_json::Value::Null;
    saved["orchestrators"][ORCH][0]["retired_messages"] = serde_json::json!([{"chat_id": ORCH, "message_id": "wake-row"}]);
    let handle = rig.core.doc_host.open(ORCH).unwrap();
    handle.doc().push_queued(&mast_doc::QueuedMessage { id: "wake-row".into(), text: "Worker updates".into(), attachments: vec![],
        hold_for_turn_end: false, issued_by: rig.core.device_id.clone(), issued_at: 1, edited_at: None, delivery_gate: None }).unwrap();
    let mut settings = rig.wakes.settings(ORCH);
    settings.worker_reports = false;
    rig.wakes.set_settings(ORCH, settings).await;
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let tick = rig.clock.clone();
    let restarted = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    assert!(handle.doc().read_queue().unwrap().iter().all(|entry| entry.id != "wake-row"));
    assert!(restarted.status(ORCH).held.is_empty());
    rig.advance(2 * USER_QUIET_MS);
    restarted.evaluate(ORCH, true).await;
    assert!(rig.wake_prompts().iter().all(|prompt| !prompt.contains("pulled back report")));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_replays_a_dequeued_unreceipted_report() {
    let rig = Rig::new().await;
    rig.wakes.report_to_parent("w1", "dequeued report").await.unwrap();
    let path = rig.store.with_file_name("orchestrator-wake-events.json");
    let mut saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    saved["orchestrators"][ORCH][0]["message_id"] = "dequeued-row".into();
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let tick = rig.clock.clone();
    let restarted = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    assert_eq!(restarted.status(ORCH).held.len(), 1);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["orchestrators"][ORCH][0]["retired_messages"][0]["message_id"], "dequeued-row");
    rig.advance(2 * USER_QUIET_MS);
    restarted.evaluate(ORCH, false).await;
    rig.wait_for(|| rig.wake_prompts().iter().any(|prompt| prompt.contains("dequeued report")), "replayed report").await;
    assert_eq!(rig.wake_prompts().iter().filter(|prompt| prompt.contains("dequeued report")).count(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_restart_does_not_replay_a_delivered_report() {
    let rig = Rig::new().await;
    rig.wakes.report_to_parent("w1", "delivered report").await.unwrap();
    rig.advance(2 * USER_QUIET_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(|| rig.wake_prompts().iter().any(|prompt| prompt.contains("delivered report")), "delivered report").await;
    let handle = rig.core.doc_host.open(ORCH).unwrap();
    handle.doc().push_message(&mast_doc::SessionMessageEntry {
        id: "large-assistant-message".into(), role: mast_doc::MessageRole::Assistant,
        parts: (0..60).map(|number| mast_doc::MessagePart::Text { id: format!("part-{number}"), text: "text".into() }).collect(),
        created_at: 1, device_id: rig.core.device_id.clone(), status: None, continuation_of: None,
    }).unwrap();
    let tick = rig.clock.clone();
    let restarted = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    assert!(restarted.status(ORCH).held.is_empty());
    rig.advance(2 * USER_QUIET_MS);
    restarted.evaluate(ORCH, false).await;
    assert_eq!(rig.wake_prompts().iter().filter(|prompt| prompt.contains("delivered report")).count(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_moves_report_restored_after_restart() {
    let rig = Rig::new().await;
    rig.core.workspace.create_space("restore-handoff-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for (id, parent) in [("restore-source", None), ("restore-destination", None), ("restore-worker", Some("restore-source"))] {
        rig.core.workspace.create_chat_with_parent(id, Some("restore-handoff-space"), Some(&rig.core.device_id), None, None, parent.map(str::to_owned)).unwrap();
    }
    rig.wakes.report_to_parent("restore-worker", "restored hand-off").await.unwrap();
    let tick = rig.clock.clone();
    let restarted = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    restarted.hand_off_orchestrator("restore-source", "restore-destination", "restore-source").await.unwrap();
    assert!(restarted.status("restore-source").held.is_empty());
    assert_eq!(restarted.status("restore-destination").held.len(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_reclaims_a_source_row_that_returns_after_restart() {
    let rig = Rig::new().await;
    rig.core.workspace.create_space("retired-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for (id, parent) in [("retired-source", None), ("retired-destination", None), ("retired-worker", Some("retired-source"))] {
        rig.core.workspace.create_chat_with_parent(id, Some("retired-space"), Some(&rig.core.device_id), None, None, parent.map(str::to_owned)).unwrap();
    }
    rig.wakes.report_to_parent("retired-worker", "retired report").await.unwrap();
    let path = rig.store.with_file_name("orchestrator-wake-events.json");
    let mut saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    saved["orchestrators"]["retired-source"][0]["message_id"] = "retired-row".into();
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let handle = rig.core.doc_host.open("retired-source").unwrap();
    handle.doc().push_queued(&mast_doc::QueuedMessage { id: "user-row".into(), text: "user message".into(), attachments: vec![],
        hold_for_turn_end: false, issued_by: rig.core.device_id.clone(), issued_at: 1, edited_at: None,
        delivery_gate: Some(mast_doc::QueueDeliveryGate::ReviewRequired { previous_lease_id: "lease".into(),
            owner_device_id: rig.core.device_id.clone(), since_ms: 1, base_text_hash: "hash".into() }) }).unwrap();
    let queued = mast_doc::QueuedMessage { id: "retired-row".into(), text: "Worker updates".into(), attachments: vec![],
        hold_for_turn_end: false, issued_by: rig.core.device_id.clone(), issued_at: 1, edited_at: None, delivery_gate: None };
    handle.doc().push_queued(&queued).unwrap();
    let tick = rig.clock.clone();
    let restarted = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(), rig.core.workspace.clone(),
        Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    restarted.hand_off_orchestrator("retired-source", "retired-destination", "retired-source").await.unwrap();
    // Simulate a crash before the source queue removal was snapshotted.
    let mut saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    saved["orchestrators"]["retired-destination"][0]["retired_messages"] = serde_json::json!([{"chat_id": "retired-source", "message_id": "retired-row"}]);
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    handle.doc().push_queued(&queued).unwrap();
    let tick = rig.clock.clone();
    let restarted_again = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(), rig.core.workspace.clone(),
        Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    assert_eq!(handle.doc().read_queue().unwrap().iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(), vec!["user-row"]);
    assert_eq!(restarted_again.status("retired-destination").held.len(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_reparents_workers_and_moves_held_reports() {
    let rig = Rig::new().await;
    rig.core.workspace.create_space("handoff-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for (id, parent) in [("source", None), ("successor", None), ("worker", Some("source"))] {
        rig.core.workspace.create_chat_with_parent(id, Some("handoff-space"), Some(&rig.core.device_id), None, None, parent.map(str::to_owned)).unwrap();
    }
    let settings = WakeSettings { auto_continue: true, ..WakeSettings::default() };
    rig.wakes.set_settings("source", settings).await;
    rig.wakes.report_to_parent("worker", "before hand-off").await.unwrap();
    assert_eq!(rig.wakes.hand_off_orchestrator("source", "successor", "source").await.unwrap(), vec!["worker"]);
    assert_eq!(rig.wakes.hand_off_orchestrator("source", "successor", "source").await.unwrap(), vec!["worker"]);
    assert_eq!(rig.core.workspace.chat("worker").unwrap().unwrap().parent_chat_id.as_deref(), Some("successor"));
    assert_eq!(rig.wakes.settings("successor"), settings);
    assert!(rig.wakes.status("source").held.is_empty());
    assert_eq!(rig.wakes.status("successor").held.len(), 1);
    let result = rig.wakes.report_to_parent("worker", "after hand-off").await.unwrap();
    assert_eq!(result.parent_chat_id, "successor");
    assert_eq!(rig.wakes.status("successor").held.len(), 2);
    assert!(rig.wakes.status("source").held.is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_reclaims_a_queued_worker_report() {
    let rig = Rig::new().await;
    rig.core.workspace.create_space("queued-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for (id, parent) in [("queued-source", None), ("queued-successor", None), ("queued-worker", Some("queued-source"))] {
        rig.core.workspace.create_chat_with_parent(id, Some("queued-space"), Some(&rig.core.device_id), None, None, parent.map(str::to_owned)).unwrap();
    }
    rig.wakes.note_user_activity("queued-successor", Some(true)).await;
    rig.core.doc_host.queue_message("queued-source", "opening", Vec::new()).unwrap();
    rig.wait_for(|| rig.core.sessions.turn_in_flight("queued-source"), "source turn").await;
    let handle = rig.core.doc_host.open("queued-source").unwrap();
    handle.doc().push_queued(&mast_doc::QueuedMessage {
        id: "queued-wake".into(), text: "Worker updates: queued report".into(), attachments: vec![],
        hold_for_turn_end: true, issued_by: rig.core.device_id.clone(), issued_at: 1,
        edited_at: None, delivery_gate: None,
    }).unwrap();
    rig.core.doc_host.event_turns().register("queued-source", "queued-wake".into(), vec![
        mast_engine::orchestrator_wakes::WakeEvent {
            id: "event-1".into(), key: "report-1".into(), worker_chat_id: "queued-worker".into(),
            worker_title: None, kind: WakeEventKind::WorkerReport, summary: "queued report".into(),
            report: Some("report body".into()), final_handback: None, at_ms: 1, repeats: 0, urgent: false,
        }
    ]);
    rig.wakes.hand_off_orchestrator("queued-source", "queued-successor", "queued-source").await.unwrap();
    assert!(handle.doc().read_queue().unwrap().is_empty());
    assert!(rig.wakes.status("queued-source").held.is_empty());
    assert_eq!(rig.wakes.status("queued-successor").held.len(), 1);
    rig.wakes.note_user_activity("queued-successor", Some(false)).await;
    rig.advance(2 * USER_QUIET_MS);
    rig.wakes.evaluate("queued-successor", false).await;
    rig.wait_for(|| rig.wake_prompts().iter().any(|p| p.contains("report body")), "transferred report").await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_retry_finishes_after_registry_write() {
    let rig = Rig::new().await;
    rig.core.workspace.create_space("retry-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for (id, parent) in [("retry-source", None), ("retry-successor", None), ("retry-worker", Some("retry-source"))] {
        rig.core.workspace.create_chat_with_parent(id, Some("retry-space"), Some(&rig.core.device_id), None, None, parent.map(str::to_owned)).unwrap();
    }
    rig.wakes.report_to_parent("retry-worker", "pending report").await.unwrap();
    rig.core.workspace.hand_off_chat_children("retry-source", "retry-successor", "retry-source", 1).unwrap();
    rig.wakes.hand_off_orchestrator("retry-source", "retry-successor", "retry-source").await.unwrap();
    assert!(rig.wakes.status("retry-source").held.is_empty());
    assert_eq!(rig.wakes.status("retry-successor").held.len(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_retry_after_restart_copies_settings_once() {
    let rig = Rig::new().await;
    rig.core.workspace.create_space("restart-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for (id, parent) in [("restart-source", None), ("restart-successor", None), ("restart-worker", Some("restart-source"))] {
        rig.core.workspace.create_chat_with_parent(id, Some("restart-space"), Some(&rig.core.device_id), None, None, parent.map(str::to_owned)).unwrap();
    }
    let settings = WakeSettings { auto_continue: true, turn_finished: true, ..WakeSettings::default() };
    rig.wakes.set_settings("restart-source", settings).await;
    rig.core.workspace.hand_off_chat_children("restart-source", "restart-successor", "restart-source", 1).unwrap();
    let tick = rig.clock.clone();
    let restarted = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    assert_eq!(restarted.hand_off_orchestrator("restart-source", "restart-successor", "restart-source").await.unwrap(), vec!["restart-worker"]);
    assert_eq!(restarted.settings("restart-successor"), settings);
    let changed = WakeSettings { auto_continue: false, ..settings };
    restarted.set_settings("restart-successor", changed).await;
    restarted.hand_off_orchestrator("restart-source", "restart-successor", "restart-source").await.unwrap();
    assert_eq!(restarted.settings("restart-successor"), changed);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_retry_after_settings_write_failure() {
    let rig = Rig::new().await;
    rig.core.workspace.create_space("write-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for id in ["write-source", "write-successor"] {
        rig.core.workspace.create_chat_with_parent(id, Some("write-space"), Some(&rig.core.device_id), None, None, None).unwrap();
    }
    let directory = rig.store.parent().unwrap().join("missing-settings-directory");
    let path = directory.join("wakes.json");
    let tick = rig.clock.clone();
    let wakes = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(path.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    let settings = WakeSettings { auto_continue: true, ..WakeSettings::default() };
    wakes.set_settings("write-source", settings).await;
    assert!(wakes.hand_off_orchestrator("write-source", "write-successor", "write-source").await.is_err());
    std::fs::create_dir(&directory).unwrap();
    wakes.hand_off_orchestrator("write-source", "write-successor", "write-source").await.unwrap();
    assert_eq!(wakes.settings("write-successor"), settings);
    assert!(path.exists());
    rig.core.shutdown().await;
}

/// A turn that ends only when the test says so.
struct HeldHarness {
    finish: tokio::sync::broadcast::Sender<()>,
    prompts: Arc<Mutex<Vec<String>>>,
    /// Answers to the approvals runs asked for (`ask:<command>` prompts).
    answers: Arc<Mutex<Vec<Vec<mast_proto::UserInputAnswer>>>>,
}

#[async_trait]
impl Harness for HeldHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Held"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        // Subscribe before the prompt is visible: a test that sees the prompt may send `finish` at once,
        // and a broadcast sent before this subscription is lost (the turn would never end).
        let mut finish = self.finish.subscribe();
        self.prompts.lock().unwrap().push(request.prompt.clone());
        // `ask:<command>` asks for a Yes/No approval of the command first.
        let ask = request.prompt.strip_prefix("ask:").map(str::to_owned);
        let answers = self.answers.clone();
        // Other prompts emit nothing here.
        let asked = futures::stream::once(async move {
            let command = ask?;
            let reply = (controls.request_input)(vec![approval_question(&command)]).await.unwrap_or_default();
            answers.lock().unwrap().push(reply);
            Some(Ok(AgentEvent::TextDelta { text: "after approval".into() }))
        })
        .filter_map(futures::future::ready);
        // `measured` records provider usage and one tool call.
        let measured = futures::stream::iter((request.prompt == "measured").then(|| vec![
            Ok(AgentEvent::ToolCall { id: "m-tool".into(), call: mast_proto::ToolCall::Exec { command: "ls".into() } }),
            Ok(AgentEvent::ToolResult { id: "m-tool".into(), is_error: false, exit_code: Some(0), output: None, diff: None }),
            Ok(AgentEvent::UsageMeasurement { measurement: mast_proto::token_usage::UsageMeasurement {
                id: "provider:mock-result:measured".into(), model: Some("mock-1".into()), scope: "turn".into(),
                counts: mast_proto::token_usage::TokenCounts { input: 1200, output: 50, cache_read: Some(1000), cache_write: Some(0), reasoning: None },
            } }),
        ]).into_iter().flatten());
        let limited = request.prompt == "hit-limit";
        let started = futures::stream::iter(vec![Ok(AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: "mock-1".into(),
            tools: vec![],
            cwd: request.cwd.clone(),
            session_id: "sess-wake".into(),
            assistant_message_id: format!("a-{}", request.prompt.len()),
            chat_id: None,
            space_id: None,
        })]);
        let error = futures::stream::iter(limited.then(|| Ok(AgentEvent::Error {
            message: "limit".into(), code: Some("usage_weekly".into()), resets_at: Some(1_000_500),
        })));
        let done = futures::stream::once(async move {
            let _ = finish.recv().await;
            Ok(AgentEvent::Done {
                status: if limited { DoneStatus::Errored } else { DoneStatus::Completed },
                result: None,
                error: None,
                session_id: Some("sess-wake".into()),
            })
        });
        Ok(started.chain(asked).chain(measured).chain(error).chain(done).boxed())
    }
}

struct Rig {
    core: EngineCore,
    wakes: OrchestratorWakes,
    clock: Arc<AtomicI64>,
    finish: tokio::sync::broadcast::Sender<()>,
    prompts: Arc<Mutex<Vec<String>>>,
    answers: Arc<Mutex<Vec<Vec<mast_proto::UserInputAnswer>>>>,
    store: std::path::PathBuf,
}

impl Rig {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.keep();
        let (finish, _) = tokio::sync::broadcast::channel(16);
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let answers = Arc::new(Mutex::new(Vec::new()));
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(HeldHarness {
            finish: finish.clone(),
            prompts: prompts.clone(),
            answers: answers.clone(),
        }));
        let core = EngineCore::assemble(
            &path.join("data"),
            Arc::new(registry),
            HarnessId::Mock,
            None,
        )
        .expect("engine core assembles");
        let store = path.join("wakes.json");
        let clock = Arc::new(AtomicI64::new(1_000_000));
        let tick = clock.clone();
        let clock_fn: Clock = Arc::new(move || tick.load(Ordering::SeqCst));
        // A separate, inert service on the fake clock; the core's own one
        // keeps watching on the real clock and never reaches its window.
        let wakes = OrchestratorWakes::new(
            core.sessions.clone(),
            core.doc_host.clone(),
            core.workspace.clone(),
            Some(store.clone()),
            clock_fn,
        );
        let rig = Self {
            core,
            wakes,
            clock,
            finish,
            prompts,
            answers,
            store,
        };
        rig.chat(ORCH, None);
        rig.chat("w1", Some(ORCH));
        rig.chat("w2", Some(ORCH));
        rig.chat("stranger", None);
        rig.wakes
            .ingest_chats(&rig.core.workspace.read_chats().unwrap())
            .await;
        rig.wakes
            .ingest_sessions(&[
                session("w1", SessionStatus::Working),
                session("w2", SessionStatus::Working),
                session("stranger", SessionStatus::Working),
            ])
            .await;
        rig
    }

    fn chat(&self, id: &str, parent: Option<&str>) {
        self.core
            .workspace
            .create_chat_with_parent(
                id,
                None,
                Some(&self.core.device_id),
                None,
                None,
                parent.map(str::to_owned),
            )
            .unwrap();
        self.core
            .workspace
            .rename_chat(id, &format!("Title {id}"))
            .unwrap();
    }

    fn advance(&self, ms: i64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
    }

    async fn worker_errors(&self, id: &str) {
        self.wakes
            .ingest_sessions(&[session(id, SessionStatus::Errored)])
            .await;
    }

    fn wake_prompts(&self) -> Vec<String> {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.contains("Worker updates:"))
            .cloned()
            .collect()
    }

    async fn wait_for<F: FnMut() -> bool>(&self, mut f: F, what: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }

    async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn session(chat: &str, status: SessionStatus) -> Session {
    Session {
        last_completed_turn: None,
        awaiting_input: None,
        last_turn_tool_failures: None, todo_progress: None,
        chat_id: chat.into(),
        device_id: "d".into(),
        status,
        started_at: None,
        updated_at: chrono::Utc::now(),
    }
}

fn auto_continue_worker(rig: &Rig, chat: &str, status: &str) -> String {
    let space = "space-auto";
    rig.core
        .workspace
        .create_space(space, &rig.core.device_id, "/tmp", None, false)
        .unwrap();
    rig.core
        .workspace
        .create_chat_with_parent(chat, Some(space), None, None, None, Some(ORCH.into()))
        .unwrap();
    rig.core.workspace.rename_chat(chat, chat).unwrap();
    let actor = mast_proto::WorkOutlineActor {
        kind: mast_proto::WorkOutlineActorKind::User,
        device_id: rig.core.device_id.clone(),
        ..Default::default()
    };
    let operation = format!("create-{chat}");
    let item = mast_proto::derive_id(&operation, mast_proto::roles::ITEM);
    let create: mast_proto::WorkOutlineMutation = serde_json::from_value(serde_json::json!({
        "op": "createWorkItem", "spaceId": space, "title": "Worker item", "status": status, "operationId": operation
    })).unwrap();
    rig.core
        .workspace
        .work_outline_mutate(&create, &actor, &[])
        .unwrap();
    let assign: mast_proto::WorkOutlineMutation = serde_json::from_value(serde_json::json!({
        "op": "assignChatToWorkItem", "chatId": chat, "workItemId": item, "operationId": format!("assign-{chat}")
    })).unwrap();
    rig.core
        .workspace
        .work_outline_mutate(&assign, &actor, &[])
        .unwrap();
    item
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waiting_period_raises_one_notice_without_starting_a_worker() {
    let rig = Rig::new().await;
    let item = auto_continue_worker(&rig, "waiting-worker", "active");
    let actor = mast_proto::WorkOutlineActor { kind: mast_proto::WorkOutlineActorKind::User,
        device_id: rig.core.device_id.clone(), ..Default::default() };
    let until = chrono::Utc::now().timestamp_millis() + 86_400_000;
    let mutation: mast_proto::WorkOutlineMutation = serde_json::from_value(serde_json::json!({
        "op": "updateWorkItem", "workItemId": item, "changes": {"waitingUntil": until, "waitingReason": "Observation"}, "operationId": "wait"
    })).unwrap();
    rig.core.workspace.work_outline_mutate(&mutation, &actor, &[]).unwrap();
    rig.clock.store(until - 1, Ordering::SeqCst);
    rig.wakes.notify_due_waiting_items();
    let snapshot = || mast_doc::materialize_work_outline("space-auto", &rig.core.workspace.work_outline_inputs("space-auto"));
    assert!(snapshot().user_action_requests.is_empty());
    rig.clock.store(until, Ordering::SeqCst);
    rig.wakes.notify_due_waiting_items();
    rig.wakes.notify_due_waiting_items();
    let requests = snapshot().user_action_requests;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].text, "Waiting period ended: Observation");
    assert!(rig.wake_prompts().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_limit_defers_worker_continuation_until_reset() {
    let rig = Rig::new().await;
    auto_continue_worker(&rig, "limited-worker", "active");
    rig.wakes.set_settings(ORCH, WakeSettings { auto_continue: true, ..Default::default() }).await;
    rig.core.doc_host.queue_message("limited-worker", "hit-limit", Vec::new()).unwrap();
    rig.wait_for(|| rig.core.sessions.turn_in_flight("limited-worker"), "worker start").await;
    let started = rig.core.sessions.session_status("limited-worker").unwrap();
    rig.wakes.ingest_sessions(&[started]).await;
    let _ = rig.finish.send(());
    rig.wait_for(|| !rig.core.sessions.turn_in_flight("limited-worker"), "worker finish").await;
    let done = rig.core.sessions.session_status("limited-worker").unwrap();
    rig.wakes.ingest_sessions(&[done]).await;
    assert!(!rig.prompts.lock().unwrap().iter().any(|p| p.contains("auto-continue")));
    rig.clock.store(1_000_500, Ordering::SeqCst);
    rig.wait_for(|| rig.prompts.lock().unwrap().iter().any(|p| p.contains("auto-continue")), "continuation after reset").await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_continued_by_a_credit_fallback_successor_is_not_resumed_at_reset() {
    let rig = Rig::new().await;
    auto_continue_worker(&rig, "limited-worker", "active");
    rig.wakes.set_settings(ORCH, WakeSettings { auto_continue: true, ..Default::default() }).await;
    rig.core.doc_host.queue_message("limited-worker", "hit-limit", Vec::new()).unwrap();
    rig.wait_for(|| rig.core.sessions.turn_in_flight("limited-worker"), "worker start").await;
    let started = rig.core.sessions.session_status("limited-worker").unwrap();
    rig.wakes.ingest_sessions(&[started]).await;
    let _ = rig.finish.send(());
    rig.wait_for(|| !rig.core.sessions.turn_in_flight("limited-worker"), "worker finish").await;
    // Its work moved to a successor, which now owns the work item.
    rig.core.workspace.ledger_mark("work-outline:credit-fallback-succ:limited-worker:successor").unwrap();
    let done = rig.core.sessions.session_status("limited-worker").unwrap();
    rig.wakes.ingest_sessions(&[done]).await;
    rig.clock.store(1_000_500, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert!(!rig.prompts.lock().unwrap().iter().any(|p| p.contains("auto-continue")));
    rig.core.shutdown().await;
}

async fn auto_continue_finish(rig: &Rig, chat: &str) {
    rig.wakes
        .ingest_sessions(&[session(chat, SessionStatus::Working)])
        .await;
    let mut done = session(chat, SessionStatus::Idle);
    done.last_completed_turn = Some(format!("completed-{chat}"));
    rig.wakes.ingest_sessions(&[done]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_waiting_on_a_running_command_is_not_nudged_but_a_stopped_one_is() {
    let rig = Rig::new().await;
    auto_continue_worker(&rig, "cmd-worker", "active");
    rig.wakes.set_settings(ORCH, WakeSettings { auto_continue: true, ..Default::default() }).await;
    let nudged = || rig.prompts.lock().unwrap().iter().any(|p| p.contains("auto-continue"));
    {
        let _run = mast_engine::agent_tools::track_running_command_for_test("cmd-worker", "run-1");
        for n in 0..4 {
            let mut done = session("cmd-worker", SessionStatus::Idle);
            done.last_completed_turn = Some(format!("waiting-{n}"));
            rig.wakes.ingest_sessions(&[session("cmd-worker", SessionStatus::Working)]).await;
            rig.wakes.ingest_sessions(&[done]).await;
        }
        rig.settle().await;
        assert!(!nudged(), "a worker waiting on a command gets no nudge");
    }
    auto_continue_finish(&rig, "cmd-worker").await;
    rig.wait_for(nudged, "nudge once nothing is pending").await;
    rig.core.shutdown().await;
}

/// A session whose current turn began at `started_ms`.
fn turn_started(chat: &str, started_ms: i64) -> Session {
    let mut working = session(chat, SessionStatus::Working);
    working.started_at = chrono::DateTime::from_timestamp_millis(started_ms);
    working
}

async fn finish_turn(rig: &Rig, chat: &str, turn: &str) {
    let mut done = session(chat, SessionStatus::Idle);
    done.last_completed_turn = Some(turn.to_owned());
    rig.wakes.ingest_sessions(&[done]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_waiting_on_a_declared_background_wait_is_nudged_only_after_its_next_turn() {
    let rig = Rig::new().await;
    auto_continue_worker(&rig, "bg-worker", "active");
    rig.wakes.set_settings(ORCH, WakeSettings { auto_continue: true, ..Default::default() }).await;
    let nudged = || rig.prompts.lock().unwrap().iter().any(|p| p.contains("auto-continue"));
    let now = chrono::Utc::now().timestamp_millis();
    // The wake pipeline is driven with session transitions; the declaration
    // goes through the same function the tool calls (the tool itself is
    // covered in agent_tools.rs), and a second turn start stands in for the
    // wake that ends the wait.
    // Turn one declares the wait after the engine saw it begin, then ends.
    rig.wakes.ingest_sessions(&[turn_started("bg-worker", now)]).await;
    mast_engine::agent_tools::declare_awaiting_background("bg-worker", now + 5, 30);
    finish_turn(&rig, "bg-worker", "bg-1").await;
    rig.settle().await;
    assert!(!nudged(), "a declared wait gets no nudge");
    assert!(mast_engine::agent_tools::is_awaiting_background("bg-worker", now + 10));
    // The wake that ends the wait starts the next turn, which clears it; that
    // turn stopping with the item open is nudged.
    rig.wakes.ingest_sessions(&[turn_started("bg-worker", now + 1_000)]).await;
    assert!(!mast_engine::agent_tools::is_awaiting_background("bg-worker", now + 1_001));
    finish_turn(&rig, "bg-worker", "bg-2").await;
    rig.wait_for(nudged, "nudge after the declared wait ended").await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_background_wait_declared_before_the_engine_saw_the_turn_begin_still_mutes_it() {
    let rig = Rig::new().await;
    auto_continue_worker(&rig, "early-worker", "active");
    rig.wakes.set_settings(ORCH, WakeSettings { auto_continue: true, ..Default::default() }).await;
    let nudged = || rig.prompts.lock().unwrap().iter().any(|p| p.contains("auto-continue"));
    let now = chrono::Utc::now().timestamp_millis();
    // The worker declares during its turn, before the engine notices the turn
    // started.
    mast_engine::agent_tools::declare_awaiting_background("early-worker", now + 5, 30);
    rig.wakes.ingest_sessions(&[turn_started("early-worker", now)]).await;
    assert!(mast_engine::agent_tools::is_awaiting_background("early-worker", now + 10));
    finish_turn(&rig, "early-worker", "early-1").await;
    rig.settle().await;
    assert!(!nudged(), "the early declaration survives the late turn-start clear");
    // A declaration older than the turn that starts is cleared.
    rig.wakes.ingest_sessions(&[turn_started("early-worker", now + 1_000)]).await;
    assert!(!mast_engine::agent_tools::is_awaiting_background("early-worker", now + 1_001));
    finish_turn(&rig, "early-worker", "early-2").await;
    rig.wait_for(nudged, "nudge once the stale declaration is gone").await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_running_for_over_an_hour_no_longer_mutes_its_worker() {
    let rig = Rig::new().await;
    auto_continue_worker(&rig, "long-worker", "active");
    rig.wakes.set_settings(ORCH, WakeSettings { auto_continue: true, ..Default::default() }).await;
    let nudged = || rig.prompts.lock().unwrap().iter().any(|p| p.contains("auto-continue"));
    let now = chrono::Utc::now().timestamp_millis();
    {
        let _within_the_hour = mast_engine::agent_tools::track_running_command_since_for_test("long-worker", "run-fresh", now - 59 * 60_000);
        rig.wakes.ingest_sessions(&[turn_started("long-worker", now)]).await;
        finish_turn(&rig, "long-worker", "long-1").await;
        rig.settle().await;
        assert!(!nudged(), "a command within the hour mutes");
    }
    let _past_the_hour = mast_engine::agent_tools::track_running_command_since_for_test("long-worker", "run-old", now - 61 * 60_000);
    rig.wakes.ingest_sessions(&[turn_started("long-worker", now + 1_000)]).await;
    finish_turn(&rig, "long-worker", "long-2").await;
    rig.wait_for(nudged, "nudge once the command has run past the cap").await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_coordinator_does_not_auto_continue_workers() {
    let rig = Rig::new().await;
    auto_continue_worker(&rig, "disabled-auto-worker", "active");
    rig.wakes.set_settings(ORCH, WakeSettings {
        enabled: false, auto_continue: true, ..WakeSettings::default()
    }).await;
    auto_continue_finish(&rig, "disabled-auto-worker").await;
    let handle = rig.core.doc_host.open("disabled-auto-worker").unwrap();
    assert!(handle.doc().read_queue().unwrap().is_empty());
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_continue_only_queues_for_an_open_unblocked_item() {
    let rig = Rig::new().await;
    auto_continue_worker(&rig, "off-worker", "active");
    auto_continue_finish(&rig, "off-worker").await;
    rig.wakes
        .set_settings(
            ORCH,
            WakeSettings {
                auto_continue: true,
                ..Default::default()
            },
        )
        .await;
    auto_continue_worker(&rig, "completed-worker", "completed");
    auto_continue_finish(&rig, "completed-worker").await;
    let item = auto_continue_worker(&rig, "blocked-worker", "active");
    let actor = mast_proto::WorkOutlineActor {
        kind: mast_proto::WorkOutlineActorKind::Agent,
        device_id: rig.core.device_id.clone(),
        chat_id: Some("blocked-worker".into()),
        ..Default::default()
    };
    let action: mast_proto::WorkOutlineMutation = serde_json::from_value(serde_json::json!({
        "op": "createUserActionRequest", "workItemId": item, "origin": {"chatId":"blocked-worker"},
        "text": "Need input", "operationId": "blocked-action"
    }))
    .unwrap();
    rig.core
        .workspace
        .work_outline_mutate(&action, &actor, &[])
        .unwrap();
    auto_continue_finish(&rig, "blocked-worker").await;
    auto_continue_worker(&rig, "user-worker", "active");
    rig.wakes
        .ingest_sessions(&[session("user-worker", SessionStatus::Working)])
        .await;
    rig.core
        .doc_host
        .open("user-worker")
        .unwrap()
        .write_user_message(
            "user-during-turn",
            "Please wait",
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    let mut done = session("user-worker", SessionStatus::Idle);
    done.last_completed_turn = Some("user-worker-turn".into());
    rig.wakes.ingest_sessions(&[done]).await;
    auto_continue_worker(&rig, "failed-worker", "active");
    rig.wakes
        .ingest_sessions(&[session("failed-worker", SessionStatus::Working)])
        .await;
    let mut failed = session("failed-worker", SessionStatus::Idle);
    failed.last_completed_turn = Some("failed-worker-turn".into());
    failed.last_turn_tool_failures = Some(mast_proto::ToolFailures {
        count: 1,
        tools: vec!["tool".into()],
    });
    rig.wakes.ingest_sessions(&[failed]).await;
    auto_continue_worker(&rig, "awaiting-worker", "active");
    rig.wakes
        .ingest_sessions(&[session("awaiting-worker", SessionStatus::Working)])
        .await;
    rig.wakes
        .ingest_sessions(&[session("awaiting-worker", SessionStatus::AwaitingInput)])
        .await;
    rig.settle().await;
    assert!(
        !rig.prompts
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("auto-continue"))
    );
    auto_continue_worker(&rig, "open-worker", "active");
    auto_continue_finish(&rig, "open-worker").await;
    rig.wait_for(
        || {
            rig.prompts
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.contains("auto-continue 1/3]"))
        },
        "worker continuation",
    )
    .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_continue_stops_after_three_and_resets_on_a_user_message() {
    let rig = Rig::new().await;
    let worker = "capped-worker";
    auto_continue_worker(&rig, worker, "active");
    rig.wakes
        .set_settings(
            ORCH,
            WakeSettings {
                auto_continue: true,
                ..Default::default()
            },
        )
        .await;
    auto_continue_finish(&rig, worker).await;
    let sessions = rig.core.sessions.watch_sessions();
    for number in 1..=3 {
        rig.wait_for(
            || {
                rig.prompts
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|prompt| prompt.contains(&format!("auto-continue {number}/3]")))
            },
            "continuation prompt",
        )
        .await;
        rig.wait_for(
            || {
                sessions
                    .borrow()
                    .iter()
                    .any(|s| s.chat_id == worker && s.status == SessionStatus::Working)
            },
            "working worker",
        )
        .await;
        let running = sessions
            .borrow()
            .iter()
            .find(|s| s.chat_id == worker)
            .unwrap()
            .clone();
        rig.wakes.ingest_sessions(&[running]).await;
        let _ = rig.finish.send(());
        rig.wait_for(
            || {
                sessions.borrow().iter().any(|s| {
                    s.chat_id == worker
                        && s.status == SessionStatus::Idle
                        && s.last_completed_turn.is_some()
                })
            },
            "completed worker",
        )
        .await;
        let done = sessions
            .borrow()
            .iter()
            .find(|s| s.chat_id == worker)
            .unwrap()
            .clone();
        rig.wakes.ingest_sessions(&[done]).await;
    }
    let stopped = rig
        .wakes
        .status(ORCH)
        .held
        .into_iter()
        .filter(|event| event.kind == WakeEventKind::AutoContinueStopped)
        .count();
    assert_eq!(stopped, 1);
    let mut running = session(worker, SessionStatus::Working);
    running.last_completed_turn = sessions
        .borrow()
        .iter()
        .find(|s| s.chat_id == worker)
        .unwrap()
        .last_completed_turn
        .clone();
    rig.wakes.ingest_sessions(&[running]).await;
    let mut done = session(worker, SessionStatus::Idle);
    done.last_completed_turn = Some("another-completed-turn".into());
    rig.wakes.ingest_sessions(&[done]).await;
    assert_eq!(
        rig.wakes
            .status(ORCH)
            .held
            .iter()
            .filter(|event| event.kind == WakeEventKind::AutoContinueStopped)
            .count(),
        1
    );
    rig.core
        .doc_host
        .open(worker)
        .unwrap()
        .write_user_message(
            "new-user-message",
            "Continue",
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    let last = sessions
        .borrow()
        .iter()
        .find(|s| s.chat_id == worker)
        .unwrap()
        .last_completed_turn
        .clone();
    let mut running = session(worker, SessionStatus::Working);
    running.last_completed_turn = last;
    rig.wakes.ingest_sessions(&[running]).await;
    let mut done = session(worker, SessionStatus::Idle);
    done.last_completed_turn = Some("after-user-message".into());
    rig.wakes.ingest_sessions(&[done]).await;
    rig.wait_for(
        || {
            rig.prompts
                .lock()
                .unwrap()
                .iter()
                .filter(|prompt| prompt.contains("auto-continue 1/3]"))
                .count()
                == 2
        },
        "reset continuation",
    )
    .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_continue_orchestrator_override_disables_global_default() {
    let mut rig = Rig::new().await;
    std::fs::write(
        &rig.store,
        r#"{"autoContinue":true,"orchestrators":{"orch":{"autoContinue":false}}}"#,
    )
    .unwrap();
    rig.wakes = OrchestratorWakes::new(
        rig.core.sessions.clone(),
        rig.core.doc_host.clone(),
        rig.core.workspace.clone(),
        Some(rig.store.clone()),
        Arc::new(|| 1_000_000),
    );
    rig.wakes.ingest_sessions(&[]).await;
    auto_continue_worker(&rig, "override-worker", "active");
    auto_continue_finish(&rig, "override-worker").await;
    rig.settle().await;
    assert!(
        !rig.prompts
            .lock()
            .unwrap()
            .iter()
            .any(|prompt| prompt.contains("auto-continue"))
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_errors_batch_into_one_system_message_after_the_window() {
    let rig = Rig::new().await;
    rig.worker_errors("w1").await;
    rig.advance(10_000);
    rig.worker_errors("w2").await;
    // Not before the window ends.
    rig.advance(10_000);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    assert_eq!(rig.wakes.status(ORCH).held.len(), 2);

    rig.advance(WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "the wake message")
        .await;
    let prompts = rig.wake_prompts();
    assert_eq!(prompts.len(), 1, "ONE message for both workers");
    assert!(prompts[0].contains("Title w1") && prompts[0].contains("Title w2"));
    assert!(prompts[0].contains("not from the user"));
    assert!(rig.wakes.status(ORCH).held.is_empty());
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_children_wake_their_orchestrator() {
    let rig = Rig::new().await;
    rig.worker_errors("stranger").await;
    rig.advance(10 * WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    let status = rig.wakes.status(ORCH);
    assert!(status.is_orchestrator);
    assert!(status.held.is_empty() && status.recent.is_empty());
    assert!(!rig.wakes.status("w1").is_orchestrator);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_turn_holds_the_batch_until_it_ends() {
    let rig = Rig::new().await;
    rig.core
        .doc_host
        .queue_message(ORCH, "opening", Vec::new())
        .unwrap();
    rig.wait_for(
        || rig.core.sessions.turn_in_flight(ORCH),
        "the orchestrator's turn",
    )
    .await;
    rig.worker_errors("w1").await;
    rig.advance(10 * WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty(), "never delivered mid-turn");
    // Even Deliver now cannot interrupt the turn.
    rig.wakes.deliver_now(ORCH).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);

    let _ = rig.finish.send(());
    rig.wait_for(
        || !rig.core.sessions.turn_in_flight(ORCH),
        "the turn to end",
    )
    .await;
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "delivery after the turn")
        .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_draft_and_recent_activity_hold_delivery() {
    let rig = Rig::new().await;
    rig.worker_errors("w1").await;
    rig.wakes.note_user_activity(ORCH, Some(true)).await;
    rig.advance(10 * USER_QUIET_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(
        rig.wake_prompts().is_empty(),
        "a draft holds however long it sits"
    );
    assert_eq!(
        rig.wakes.status(ORCH).hold_reason,
        Some(mast_proto::WakeHoldReason::Draft)
    );

    rig.advance(1_000);
    rig.wakes.note_user_activity(ORCH, Some(false)).await;
    rig.advance(USER_QUIET_MS / 2);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(
        rig.wake_prompts().is_empty(),
        "the user was here a minute ago"
    );

    rig.advance(USER_QUIET_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(
        || !rig.wake_prompts().is_empty(),
        "delivery once the user is gone",
    )
    .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deliver_now_skips_the_window_and_the_draft() {
    let rig = Rig::new().await;
    rig.worker_errors("w1").await;
    rig.wakes.note_user_activity(ORCH, Some(true)).await;
    rig.wakes.deliver_now(ORCH).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "the forced delivery")
        .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_message_interrupts_an_event_started_turn_and_runs_next() {
    let rig = Rig::new().await;
    rig.worker_errors("w1").await;
    rig.advance(WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "the event turn to start")
        .await;
    rig.wait_for(
        || rig.core.sessions.turn_in_flight(ORCH),
        "the event turn in flight",
    )
    .await;
    // The wake service sees the turn running (the session watch does this).
    rig.wakes
        .ingest_sessions(&[session(ORCH, SessionStatus::Working)])
        .await;

    // Mikey types: the event turn is interrupted (it never finished — the
    // test never sent `finish`), and his message is the very next turn.
    rig.core
        .doc_host
        .queue_message(ORCH, "hello from the user", Vec::new())
        .unwrap();
    rig.wait_for(
        || {
            rig.prompts
                .lock()
                .unwrap()
                .iter()
                .any(|p| p == "hello from the user")
        },
        "the user's message to run next",
    )
    .await;
    let prompts = rig.prompts.lock().unwrap().clone();
    let wake = prompts
        .iter()
        .position(|p| p.contains("Worker updates:"))
        .unwrap();
    let user = prompts
        .iter()
        .position(|p| p == "hello from the user")
        .unwrap();
    assert!(wake < user);
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_default_and_gate_event_kinds() {
    let rig = Rig::new().await;
    assert_eq!(rig.wakes.settings(ORCH), WakeSettings::default());
    // Turn-finished is on by default: it is held and logged.
    rig.wakes
        .ingest_sessions(&[session("w1", SessionStatus::Idle)])
        .await;
    let status = rig.wakes.status(ORCH);
    assert_eq!(status.held.len(), 1);
    assert_eq!(status.recent.len(), 1);
    assert_eq!(status.recent[0].kind, WakeEventKind::TurnFinished);
    assert!(status.recent[0].unseen);

    // Turning turn_finished off logs new turn-finished events without holding them.
    rig.wakes
        .set_settings(
            ORCH,
            WakeSettings {
                turn_finished: false,
                ..WakeSettings::default()
            },
        )
        .await;
    rig.wakes
        .ingest_sessions(&[session("w2", SessionStatus::Idle)])
        .await;
    let status = rig.wakes.status(ORCH);
    assert!(status.held.is_empty());
    assert_eq!(status.recent.len(), 2);
    assert_eq!(status.recent[0].kind, WakeEventKind::TurnFinished);

    // Master switch off clears what was held and holds nothing new.
    rig.wakes
        .set_settings(
            ORCH,
            WakeSettings {
                enabled: false,
                ..WakeSettings::default()
            },
        )
        .await;
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.worker_errors("w1").await;
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.advance(10 * WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn awaiting_input_and_failed_tool_calls_wake_the_parent() {
    use mast_engine::sessions::WorkerSignal;
    let rig = Rig::new().await;
    rig.wakes
        .ingest_worker_signal(WorkerSignal::Input {
            chat_id: "w1".into(),
            input: mast_proto::AwaitingInput {
                request_id: "request-1".into(),
                questions: vec![mast_proto::UserInputQuestion { permission: None,
                    command_approval: None,
                    approval_scope: None,
                    allow_custom: false,
                    id: "q1".into(),
                    header: "Approval".into(),
                    question: "Run cargo clean?".into(),
                    options: vec![],
                    multi_select: false,
                }],
            },
        })
        .await;
    rig.wakes
        .ingest_worker_signal(WorkerSignal::ToolFailures {
            chat_id: "w2".into(),
            failures: mast_proto::ToolFailures {
                count: 3,
                tools: vec!["bind_chat_worktree".into()],
            },
        })
        .await;
    let held = rig.wakes.status(ORCH).held;
    assert_eq!(held.len(), 2);
    assert_eq!(held[0].kind, WakeEventKind::AwaitingInput);
    assert!(held[0].summary.contains("Run cargo clean?"));
    assert_eq!(held[1].kind, WakeEventKind::ToolCallsFailed);
    assert!(
        held[1]
            .summary
            .contains("3 failed Mast tool calls (bind_chat_worktree)")
    );
    rig.core.shutdown().await;
}

/// Advances the fake clock by `ms` in 30 second steps with the long-tool-call
/// check after each, as the real tick runs: one long jump would look like the
/// host waking from sleep.
async fn run_ticks(rig: &Rig, ms: i64) {
    let mut left = ms;
    while left > 0 {
        let by = left.min(30_000);
        rig.advance(by);
        rig.wakes.flag_long_tool_calls().await;
        left -= by;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_call_open_too_long_in_a_working_worker_is_told_to_the_manager_once() {
    use mast_engine::tool_watch::{LONG_TOOL_CALL_MS, call_emitted, call_finished};
    let rig = Rig::new().await;
    // The core's own tick reads the same registry at the real time: open the
    // calls at about that time so it neither flags nor forgets them.
    rig.clock.store(chrono::Utc::now().timestamp_millis(), Ordering::SeqCst);
    rig.chat(ORCH, None);
    rig.chat("ltc-hung", Some(ORCH));
    rig.chat("ltc-quick", Some(ORCH));
    rig.chat("ltc-idle", Some(ORCH));
    rig.chat("ltc-stranger", None);
    rig.wakes.ingest_chats(&rig.core.workspace.read_chats().unwrap()).await;
    rig.wakes
        .ingest_sessions(&[
            session("ltc-hung", SessionStatus::Working),
            session("ltc-quick", SessionStatus::Working),
            session("ltc-idle", SessionStatus::Working),
            session("ltc-stranger", SessionStatus::Working),
        ])
        .await;
    let started = rig.clock.load(Ordering::SeqCst);
    let wait = mast_proto::ToolCall::Exec { command: "scripts/remote-cargo.sh wait david@laptop job 30".into() };
    for chat in ["ltc-hung", "ltc-quick", "ltc-idle", "ltc-stranger"] {
        call_emitted(chat, "call-1", &wait, started);
    }
    let long = |rig: &Rig| rig.wakes.status(ORCH).held.into_iter().filter(|e| e.kind == WakeEventKind::LongToolCall).collect::<Vec<_>>();

    rig.wakes.flag_long_tool_calls().await;
    run_ticks(&rig, LONG_TOOL_CALL_MS - 30_000).await;
    assert!(long(&rig).is_empty(), "not before the threshold");

    // One returned, one worker went idle (its turn ended), one has no Manager.
    call_finished("ltc-quick", "call-1");
    rig.wakes.ingest_sessions(&[session("ltc-idle", SessionStatus::Idle)]).await;
    run_ticks(&rig, 90_000).await;
    // The event is held for the batching window, or already delivered.
    let told = |rig: &Rig| -> String {
        let held: Vec<String> = long(rig).into_iter().map(|e| format!("{} {}", e.worker_chat_id, e.summary)).collect();
        format!("{}\n{}", held.join("\n"), rig.wake_prompts().join("\n"))
    };
    rig.wait_for(|| told(&rig).contains("no result for 20 min"), "the hung call told to the Manager").await;
    let text = told(&rig);
    assert!(text.contains("ltc-hung") || text.contains("Title ltc-hung"), "{text}");
    assert!(text.contains("remote-cargo.sh wait") && text.contains("still shows as working"), "{text}");
    for quiet in ["ltc-quick", "ltc-idle", "ltc-stranger"] {
        assert!(!text.lines().any(|line| line.contains(quiet) && line.contains("no result for")), "{quiet} was flagged: {text}");
    }

    // Told once: later ticks add nothing, even much later.
    run_ticks(&rig, LONG_TOOL_CALL_MS).await;
    assert_eq!(told(&rig).matches("no result for 20 min").count(), 1, "{}", told(&rig));
    assert!(rig.wakes.status(ORCH).settings.wakes_for(WakeEventKind::LongToolCall));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_persist_across_service_restarts() {
    let rig = Rig::new().await;
    let custom = WakeSettings {
        prs: true,
        errors: false,
        ..WakeSettings::default()
    };
    rig.wakes.set_settings(ORCH, custom).await;
    let again = OrchestratorWakes::new(
        rig.core.sessions.clone(),
        rig.core.doc_host.clone(),
        rig.core.workspace.clone(),
        Some(rig.store.clone()),
        Arc::new(|| 0),
    );
    assert_eq!(again.settings(ORCH), custom);
    assert_eq!(again.settings("someone-else"), WakeSettings::default());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_continue_defaults_and_orchestrator_overrides_persist() {
    let rig = Rig::new().await;
    assert!(!rig.wakes.settings(ORCH).auto_continue);
    std::fs::write(
        &rig.store,
        r#"{"autoContinue":true,"orchestrators":{"orch":{"autoContinue":false}}}"#,
    )
    .unwrap();
    let reload = || {
        OrchestratorWakes::new(
            rig.core.sessions.clone(),
            rig.core.doc_host.clone(),
            rig.core.workspace.clone(),
            Some(rig.store.clone()),
            Arc::new(|| 0),
        )
    };
    let wakes = reload();
    assert!(!wakes.settings(ORCH).auto_continue);
    assert!(wakes.settings("another").auto_continue);
    wakes.set_settings(ORCH, wakes.settings(ORCH)).await;
    let again = reload();
    assert!(!again.status(ORCH).settings.auto_continue);
    assert!(again.status("another").settings.auto_continue);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_continue_loads_legacy_wake_settings() {
    let rig = Rig::new().await;
    std::fs::write(&rig.store, r#"{"orch":{"errors":false,"prs":true}}"#).unwrap();
    let wakes = OrchestratorWakes::new(
        rig.core.sessions.clone(),
        rig.core.doc_host.clone(),
        rig.core.workspace.clone(),
        Some(rig.store.clone()),
        Arc::new(|| 0),
    );
    let settings = wakes.settings(ORCH);
    assert!(!settings.auto_continue);
    assert!(!settings.errors);
    assert!(settings.prs);
    rig.core.shutdown().await;
}

fn project_snapshot(requests: serde_json::Value) -> WorkOutlineSnapshot {
    serde_json::from_value(serde_json::json!({
        "spaceId": "project-space", "available": true, "synced": true,
        "items": [
            {"id": "project", "spaceId": "project-space", "isProject": true,
             "workerDestinationId": "workers", "chatIds": [ORCH, "coordinator"]},
            {"id": "workers", "spaceId": "project-space", "parentWorkItemId": "project"},
            {"id": "worker-item", "spaceId": "project-space", "parentWorkItemId": "workers",
             "chatIds": ["stranger", "w1"]}
        ],
        "userActionRequests": requests
    })).unwrap()
}

async fn seed_project_routing(rig: &Rig, archive_coordinator: bool) {
    rig.chat("coordinator", None);
    if archive_coordinator {
        rig.core.workspace.set_chat_archived("coordinator", true).unwrap();
    }
    let mut chats = rig.core.workspace.read_chats().unwrap();
    for chat in &mut chats {
        if chat.id == ORCH { chat.last_message_at = Some(chrono::DateTime::from_timestamp(1, 0).unwrap()); }
        if chat.id == "coordinator" { chat.last_message_at = Some(chrono::DateTime::from_timestamp(2, 0).unwrap()); }
    }
    rig.wakes.ingest_chats(&chats).await;
    rig.wakes.ingest_snapshot(&project_snapshot(serde_json::json!([]))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_destination_wakes_only_the_most_recent_coordinator() {
    let rig = Rig::new().await;
    seed_project_routing(&rig, false).await;
    rig.wakes.ingest_snapshot(&project_snapshot(serde_json::json!([request("r-project", "stranger", "ready", "open")]))).await;
    assert!(rig.wakes.status(ORCH).held.is_empty());
    assert_eq!(rig.wakes.status("coordinator").held.len(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_destination_skips_archived_coordinator() {
    let rig = Rig::new().await;
    seed_project_routing(&rig, true).await;
    rig.wakes.ingest_snapshot(&project_snapshot(serde_json::json!([request("r-archive", "stranger", "ready", "open")]))).await;
    assert!(rig.wakes.status("coordinator").held.is_empty());
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_parent_receives_wake_instead_of_project_coordinator() {
    let rig = Rig::new().await;
    seed_project_routing(&rig, false).await;
    rig.wakes.ingest_snapshot(&project_snapshot(serde_json::json!([request("r-parent", "w1", "ready", "open")]))).await;
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);
    assert!(rig.wakes.status("coordinator").held.is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn immediate_unarchive_does_not_replay_held_wakes() {
    let rig = Rig::new().await;
    let wakes = &rig.core.orchestrator_wakes;
    wakes.note_user_activity(ORCH, Some(true)).await;
    wakes.report_to_parent("w1", "discard before unarchive").await.unwrap();
    assert_eq!(wakes.status(ORCH).held.len(), 1);
    rig.core.workspace.set_chat_archived(ORCH, true).unwrap();
    rig.core.workspace.set_chat_archived(ORCH, false).unwrap();
    assert!(wakes.status(ORCH).held.is_empty());
    wakes.evaluate(ORCH, true).await;
    assert!(rig.wake_prompts().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_wake_is_discarded_if_coordinator_is_archived_before_dispatch() {
    let rig = Rig::new().await;
    let handle = rig.core.doc_host.open(ORCH).unwrap();
    handle.doc().push_queued(&mast_doc::QueuedMessage {
        id: "archive-queued-wake".into(), text: "Worker updates: queued report".into(), attachments: vec![],
        hold_for_turn_end: false, issued_by: rig.core.device_id.clone(), issued_at: 1,
        edited_at: None, delivery_gate: None,
    }).unwrap();
    rig.core.doc_host.event_turns().register(ORCH, "archive-queued-wake".into(), vec![]);
    rig.core.workspace.set_chat_archived(ORCH, true).unwrap();
    rig.core.doc_host.drain_queue(&handle).await;
    assert!(handle.doc().read_queue().unwrap().is_empty());
    assert!(rig.wake_prompts().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archived_parent_does_not_receive_worker_wakes() {
    let rig = Rig::new().await;
    rig.core.workspace.set_chat_archived(ORCH, true).unwrap();
    rig.wakes.ingest_chats(&rig.core.workspace.read_chats().unwrap()).await;
    rig.worker_errors("w1").await;
    assert!(rig.wakes.status(ORCH).held.is_empty());
    assert!(!rig.wakes.status(ORCH).is_orchestrator);
    rig.advance(2 * USER_QUIET_MS);
    rig.wakes.evaluate(ORCH, true).await;
    assert!(rig.wake_prompts().is_empty());
    rig.core.shutdown().await;
}

fn snapshot(requests: serde_json::Value) -> WorkOutlineSnapshot {
    serde_json::from_value(serde_json::json!({
        "spaceId": "s1", "available": true, "synced": true,
        "userActionRequests": requests,
    }))
    .unwrap()
}

fn request(id: &str, chat: &str, text: &str, state: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "spaceId": "s1", "workItemId": "item1", "chatId": chat,
        "text": text, "dedupeKey": id, "createdAt": 1, "state": state,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn required_actions_raised_wake_once_and_dedupe() {
    let rig = Rig::new().await;
    // The first snapshot only seeds what already exists.
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!([request(
            "r0", "w1", "old", "open"
        )])))
        .await;
    assert!(rig.wakes.status(ORCH).held.is_empty());

    let raised = snapshot(serde_json::json!([
        request("r0", "w1", "old", "open"),
        request("r123456789abcdef", "w1", "Ready to merge: parser-fix", "open"),
        request("r2", "stranger", "not mine", "open"),
    ]));
    rig.wakes.ingest_snapshot(&raised).await;
    rig.wakes.ingest_snapshot(&raised).await; // a re-read of the same state
    let held = rig.wakes.status(ORCH).held;
    assert_eq!(
        held.len(),
        1,
        "one event; strangers and repeats are dropped"
    );
    assert!(held[0].summary.contains("Ready to merge: parser-fix"));
    assert!(held[0].summary.contains("request r123456789abcdef"));

    rig.advance(WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(
        || !rig.wake_prompts().is_empty(),
        "the Required Action wake",
    )
    .await;
    assert!(rig.wake_prompts()[0].contains("Ready to merge: parser-fix"));

    // Re-raised after delivery: the same action is not news again.
    let resolved = snapshot(serde_json::json!([request(
        "r123456789abcdef",
        "w1",
        "Ready to merge: parser-fix",
        "resolved"
    )]));
    rig.wakes.ingest_snapshot(&resolved).await;
    let held = rig.wakes.status(ORCH).held;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].kind, WakeEventKind::ActionResolved);
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolution_wakes_only_other_actors() {
    let rig = Rig::new().await;
    let mut requests = vec![
        request("r1", "w1", "One", "open"),
        request("r2", "w1", "Two", "open"),
        request("r3", "w1", "Three", "open"),
    ];
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!(requests)))
        .await;
    for (request, actor) in requests.iter_mut().zip([
        serde_json::json!({"kind":"agent", "chatId":ORCH}),
        serde_json::json!({"kind":"agent", "chatId":"w2"}),
        serde_json::json!({"kind":"user", "chatId":ORCH}),
    ]) {
        request["state"] = "resolved".into();
        request["resolution"] = serde_json::json!({"by":actor, "at":1});
    }
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!(requests)))
        .await;
    let held = rig.wakes.status(ORCH).held;
    assert_eq!(held.len(), 2);
    assert!(held.iter().any(|event| event.summary.contains("Two")));
    assert!(held.iter().any(|event| event.summary.contains("Three")));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raised_and_resolved_inside_one_window_say_nothing() {
    let rig = Rig::new().await;
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!([])))
        .await;
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!([request(
            "r1",
            "w1",
            "Approve plan",
            "open"
        )])))
        .await;
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!([request(
            "r1",
            "w1",
            "Approve plan",
            "resolved"
        )])))
        .await;
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_reports_batch_with_other_events_and_say_what_the_worker_said() {
    let rig = Rig::new().await;
    let sent = rig
        .wakes
        .report_to_parent("w1", "Done: fixed the bug\nPR is up.")
        .await
        .unwrap();
    assert_eq!(sent.parent_chat_id, ORCH);
    assert_eq!(sent.delivery, "queuedForParent");
    assert_eq!(sent.held_reason, Some(mast_proto::WakeHoldReason::Window));
    assert!(!sent.truncated);
    rig.worker_errors("w2").await;
    rig.advance(WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "the wake message")
        .await;
    let prompts = rig.wake_prompts();
    assert_eq!(prompts.len(), 1, "ONE message for the report and the error");
    assert!(prompts[0].contains("worker report: Done: fixed the bug"));
    assert!(
        prompts[0].contains("| PR is up."),
        "full text follows the line"
    );
    assert!(prompts[0].contains("Title w2"));
    assert!(rig.wakes.status(ORCH).held.is_empty());
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_evaluations_queue_one_report() {
    let rig = Rig::new().await;
    rig.wakes.report_to_parent("w1", "finished").await.unwrap();
    rig.advance(WAKE_WINDOW_MS);
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let wakes = rig.wakes.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            wakes.evaluate(ORCH, false).await;
        }));
    }
    barrier.wait().await;
    for task in tasks {
        task.await.unwrap();
    }
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "report delivery")
        .await;
    rig.settle().await;
    assert_eq!(rig.wake_prompts().len(), 1);
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_reports_follow_the_user_first_holds() {
    let rig = Rig::new().await;
    rig.wakes.note_user_activity(ORCH, Some(true)).await;
    rig.advance(10 * USER_QUIET_MS);
    let sent = rig.wakes.report_to_parent("w1", "all done").await.unwrap();
    assert_eq!(sent.held_reason, Some(mast_proto::WakeHoldReason::Draft));
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty(), "held while the user drafts");
    rig.wakes.note_user_activity(ORCH, Some(false)).await;
    rig.advance(2 * USER_QUIET_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(
        || !rig.wake_prompts().is_empty(),
        "delivery after the draft",
    )
    .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_reports_can_be_turned_off() {
    let rig = Rig::new().await;
    rig.wakes
        .set_settings(
            ORCH,
            WakeSettings {
                worker_reports: false,
                ..WakeSettings::default()
            },
        )
        .await;
    let sent = rig.wakes.report_to_parent("w1", "all done").await.unwrap();
    assert_eq!(sent.delivery, "parentReportsOff");
    rig.advance(10 * WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_reports_are_cut_and_marked_and_orphans_cannot_report() {
    let rig = Rig::new().await;
    let long = "x".repeat(10_000);
    let sent = rig.wakes.report_to_parent("w1", &long).await.unwrap();
    assert!(sent.truncated);
    let status = rig.wakes.status(ORCH);
    let report = status.held[0].report.as_deref().unwrap();
    assert!(report.len() < 4_300 && report.contains("[truncated: first 4096 of 10000 bytes"));

    let err = rig
        .wakes
        .report_to_parent("stranger", "hi")
        .await
        .unwrap_err();
    assert!(err.contains("no parent chat"), "{err}");
    assert!(rig.wakes.report_to_parent("w1", "  ").await.is_err());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_moves_a_standing_role_and_leaves_plain_chats_alone() {
    use mast_proto::StandingRoleKind;
    let rig = Rig::new().await;
    let workspace = &rig.core.workspace;
    workspace.create_space("role-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for id in ["role-source", "role-successor", "plain-source", "plain-successor"] {
        workspace.create_chat_with_parent(id, Some("role-space"), Some(&rig.core.device_id), None, None, None).unwrap();
    }
    let item = workspace.ensure_chat_work_item("role-source").unwrap();
    workspace.adopt_standing_role(StandingRoleKind::Manager, &item, "role-source", &[]).unwrap();

    assert_eq!(workspace.rotate_standing_role_for_chat("plain-source", "plain-successor").unwrap(), None);
    assert!(!workspace.chat("plain-source").unwrap().unwrap().archived);

    let role = workspace.rotate_standing_role_for_chat("role-source", "role-successor").unwrap().unwrap();
    assert_eq!(role.current_chat_id.as_deref(), Some("role-successor"));
    // A retried launch finds the role already moved.
    assert_eq!(workspace.rotate_standing_role_for_chat("role-source", "role-successor").unwrap(), Some(role));
    // A source a role already replaced cannot hand off to a different chat.
    assert!(workspace.rotate_standing_role_for_chat("role-source", "plain-successor").is_err());
    assert_eq!(workspace.standing_role_chat(StandingRoleKind::Manager, &item).as_deref(), Some("role-successor"));
    let source = workspace.chat("role-source").unwrap().unwrap();
    assert!(source.archived && source.is_standing_role_predecessor());
    assert_eq!(workspace.standing_role_chat(StandingRoleKind::Manager, &item).as_deref(), Some("role-successor"));
    rig.core.shutdown().await;
}

// ── autonomous child worker rails (W2-T5, Conflict C1) ─────────────────────

const AUTO_SPACE: &str = "space-autonomous";
const MGR: &str = "auto-manager";

fn approval_question(command: &str) -> mast_proto::UserInputQuestion {
    mast_proto::UserInputQuestion {
        id: "q1".into(),
        header: "Approve command".into(),
        question: format!("Run `{command}`?"),
        options: vec!["Yes".into(), "No".into()],
        multi_select: false,
        allow_custom: false,
        approval_scope: None,
        command_approval: Some(mast_proto::CommandApproval {
            command: command.into(),
            cwd: None,
            reason: None,
            sandbox_permissions: None,
        }),
        permission: None,
    }
}

fn approval_input(request: &str, command: &str) -> mast_proto::AwaitingInput {
    mast_proto::AwaitingInput { request_id: request.into(), questions: vec![approval_question(command)] }
}

/// `chat` in a Space shared with the Manager chat [`MGR`], with `parent` as
/// its parent chat, assigned to a work item carrying `spec` as its task spec.
async fn space_worker(rig: &Rig, chat: &str, parent: Option<&str>, spec: serde_json::Value, cwd: Option<String>, config: Option<mast_proto::ChatConfig>) {
    let workspace = &rig.core.workspace;
    let _ = workspace.create_space(AUTO_SPACE, &rig.core.device_id, "/tmp", None, false);
    if workspace.chat(MGR).unwrap().is_none() {
        workspace.create_chat_with_parent(MGR, Some(AUTO_SPACE), Some(&rig.core.device_id), None, None, None).unwrap();
        workspace.rename_chat(MGR, "Manager").unwrap();
    }
    workspace.create_chat_with_parent(chat, Some(AUTO_SPACE), Some(&rig.core.device_id), config, cwd, parent.map(str::to_owned)).unwrap();
    workspace.rename_chat(chat, chat).unwrap();
    let actor = mast_proto::WorkOutlineActor {
        kind: mast_proto::WorkOutlineActorKind::User,
        device_id: rig.core.device_id.clone(),
        ..Default::default()
    };
    let operation = format!("create-{chat}");
    let item = mast_proto::derive_id(&operation, mast_proto::roles::ITEM);
    for mutation in [
        serde_json::json!({"op": "createWorkItem", "spaceId": AUTO_SPACE, "title": format!("Item {chat}"), "status": "active", "operationId": operation}),
        serde_json::json!({"op": "assignChatToWorkItem", "chatId": chat, "workItemId": item, "operationId": format!("assign-{chat}")}),
        serde_json::json!({"op": "updateWorkItem", "workItemId": item, "changes": {"taskSpec": spec}, "operationId": format!("spec-{chat}")}),
    ] {
        let mutation: mast_proto::WorkOutlineMutation = serde_json::from_value(mutation).unwrap();
        workspace.work_outline_mutate(&mutation, &actor, &[]).unwrap();
    }
    rig.wakes.ingest_chats(&workspace.read_chats().unwrap()).await;
}

fn autonomous_spec() -> serde_json::Value {
    serde_json::json!({"requiredReport": true})
}

/// Start `prompt` in `chat` and wait for the approval it asks for.
async fn ask(rig: &Rig, chat: &str, command: &str) -> mast_proto::AwaitingInput {
    rig.core.doc_host.queue_message(chat, &format!("ask:{command}"), Vec::new()).unwrap();
    let mut found = None;
    rig.wait_for(
        || {
            found = rig.core.sessions.session_status(chat).and_then(|session| session.awaiting_input);
            found.is_some()
        },
        "the approval prompt",
    )
    .await;
    found.unwrap()
}

fn pending(rig: &Rig, chat: &str) -> bool {
    rig.core.sessions.session_status(chat).is_some_and(|session| session.awaiting_input.is_some())
}

fn answered(rig: &Rig) -> Vec<Vec<String>> {
    rig.answers.lock().unwrap().iter().map(|reply| reply.iter().flat_map(|a| a.labels.clone()).collect()).collect()
}

fn recent(rig: &Rig, orch: &str, worker: &str) -> Vec<mast_proto::WakeEventView> {
    rig.wakes.status(orch).recent.into_iter().filter(|event| event.worker_chat_id == worker).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autonomous_approval_wakes_the_manager_at_once_and_plain_workers_keep_their_holds() {
    use mast_engine::sessions::WorkerSignal;
    let rig = Rig::new().await;
    space_worker(&rig, "auto-w", Some(MGR), autonomous_spec(), None, None).await;
    space_worker(&rig, "plain-w", Some(MGR), serde_json::json!({}), None, None).await;
    // The user just acted in the Manager chat: ordinary events wait.
    rig.wakes.note_user_activity(MGR, None).await;
    rig.wakes.ingest_worker_signal(WorkerSignal::Input { chat_id: "plain-w".into(), input: approval_input("plain-req", "cargo clean") }).await;
    let status = rig.wakes.status(MGR);
    assert_eq!(status.held.len(), 1);
    assert_eq!(status.held[0].kind, WakeEventKind::AwaitingInput);
    assert!(status.held[0].summary.contains("(answer in Needs You)"), "{}", status.held[0].summary);
    assert_eq!(status.hold_reason, Some(mast_proto::WakeHoldReason::RecentActivity));
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());

    let started = std::time::Instant::now();
    rig.wakes.ingest_worker_signal(WorkerSignal::Input { chat_id: "auto-w".into(), input: approval_input("auto-req", "cargo clean") }).await;
    rig.wait_for(|| rig.wake_prompts().iter().any(|p| p.contains("auto-declined in 10 min")), "the urgent approval wake").await;
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(1), "the Manager was woken after {elapsed:?}");
    let prompt = rig.wake_prompts().into_iter().find(|p| p.contains("auto-declined")).unwrap();
    assert!(prompt.contains("auto-w (chat auto-w) — is waiting for approval: Run `cargo clean`?"), "{prompt}");
    assert!(prompt.contains("respond_to_input"), "{prompt}");
    // Launched without autoApprove (the Ask default): the Manager is told.
    assert!(prompt.contains("runs in Ask mode") && prompt.contains("autoApprove: true"), "{prompt}");
    // The plain worker's held approval rode along, worded as before.
    assert!(prompt.contains("plain-w (chat plain-w) — is waiting for approval: Run `cargo clean`? (answer in Needs You)"), "{prompt}");
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_auto_mode_autonomous_worker_is_not_told_to_relaunch() {
    use mast_engine::sessions::WorkerSignal;
    let rig = Rig::new().await;
    let config: mast_proto::ChatConfig = serde_json::from_value(serde_json::json!({
        "harness": "mock", "model": null, "reasoning": null, "sandbox": "workspace-write", "autoApprove": true
    })).unwrap();
    space_worker(&rig, "auto-mode-w", Some(MGR), autonomous_spec(), None, Some(config)).await;
    rig.wakes.ingest_worker_signal(WorkerSignal::Input { chat_id: "auto-mode-w".into(), input: approval_input("r", "cargo clean") }).await;
    let events = recent(&rig, MGR, "auto-mode-w");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, WakeEventKind::WorkerAwaitingApproval);
    assert!(!events[0].summary.contains("Ask mode"), "{}", events[0].summary);
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blocked_manager_gets_the_approval_when_its_turn_ends_and_duplicates_are_told_once() {
    use mast_engine::sessions::WorkerSignal;
    let rig = Rig::new().await;
    space_worker(&rig, "dup-w", Some(MGR), autonomous_spec(), None, None).await;
    rig.core.doc_host.queue_message(MGR, "busy manager", Vec::new()).unwrap();
    rig.wait_for(|| rig.core.sessions.turn_in_flight(MGR), "the Manager's turn").await;
    // The local signal and the session-row watcher can both report one request.
    for _ in 0..2 {
        rig.wakes.ingest_worker_signal(WorkerSignal::Input { chat_id: "dup-w".into(), input: approval_input("dup-req", "cargo clean") }).await;
    }
    let status = rig.wakes.status(MGR);
    assert_eq!(status.held.iter().filter(|e| e.kind == WakeEventKind::WorkerAwaitingApproval).count(), 1);
    assert_eq!(status.hold_reason, Some(mast_proto::WakeHoldReason::Busy));
    assert!(rig.wake_prompts().is_empty());
    // The turn ends: delivered at once, no batch window.
    let _ = rig.finish.send(());
    rig.wait_for(|| !rig.core.sessions.turn_in_flight(MGR), "the Manager going idle").await;
    rig.wakes.ingest_sessions(&[rig.core.sessions.session_status(MGR).unwrap()]).await;
    rig.wait_for(|| rig.wake_prompts().iter().any(|p| p.contains("dup-w (chat dup-w) — is waiting for approval")), "the held approval").await;
    rig.wakes.ingest_worker_signal(WorkerSignal::Input { chat_id: "dup-w".into(), input: approval_input("dup-req", "cargo clean") }).await;
    rig.settle().await;
    let told = rig.wake_prompts().iter().filter(|p| p.contains("dup-w (chat dup-w) — is waiting for approval")).count();
    assert_eq!(told, 1, "{:?}", rig.wake_prompts());
    assert!(rig.wakes.status(MGR).held.is_empty());
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handed_off_manager_is_not_woken_its_successor_is() {
    use mast_engine::sessions::WorkerSignal;
    let rig = Rig::new().await;
    space_worker(&rig, "handoff-w", Some(MGR), autonomous_spec(), None, None).await;
    rig.core.workspace.create_chat_with_parent("auto-manager-2", Some(AUTO_SPACE), Some(&rig.core.device_id), None, None, None).unwrap();
    rig.wakes.ingest_chats(&rig.core.workspace.read_chats().unwrap()).await;
    rig.wakes.hand_off_orchestrator(MGR, "auto-manager-2", MGR).await.unwrap();
    rig.wakes.ingest_worker_signal(WorkerSignal::Input { chat_id: "handoff-w".into(), input: approval_input("h-req", "cargo clean") }).await;
    rig.wait_for(|| rig.wake_prompts().iter().any(|p| p.contains("handoff-w (chat handoff-w) — is waiting for approval")), "the successor's wake").await;
    assert!(recent(&rig, MGR, "handoff-w").is_empty(), "the stale holder is not told");
    assert_eq!(recent(&rig, "auto-manager-2", "handoff-w").len(), 1);
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autonomous_approval_is_declined_at_the_deadline_unless_answered_or_human_only() {
    use mast_engine::sessions::WorkerSignal;
    let rig = Rig::new().await;
    space_worker(&rig, "decline-w", Some(MGR), autonomous_spec(), Some("/tmp".into()), None).await;
    let input = ask(&rig, "decline-w", "cargo clean").await;
    rig.wakes.ingest_worker_signal(WorkerSignal::Input { chat_id: "decline-w".into(), input: input.clone() }).await;
    // A replay of the same request keeps the first deadline.
    rig.advance(1_000);
    rig.wakes.ingest_worker_signal(WorkerSignal::Input { chat_id: "decline-w".into(), input }).await;
    rig.advance(600_000 - 1_001);
    rig.wakes.expire_approval_deadlines().await;
    rig.settle().await;
    assert!(pending(&rig, "decline-w"), "not declined before 600 s");
    assert!(answered(&rig).is_empty());
    rig.advance(1);
    rig.wakes.expire_approval_deadlines().await;
    rig.wait_for(|| answered(&rig) == [vec!["No".to_owned()]], "the auto-decline").await;
    rig.wait_for(|| !pending(&rig, "decline-w"), "the prompt retired").await;
    assert!(recent(&rig, MGR, "decline-w").iter().any(|e| e.summary.contains("approval auto-declined")));

    // A host-admin prompt waits for the user whatever the time.
    space_worker(&rig, "admin-w", Some(MGR), autonomous_spec(), Some("/tmp".into()), None).await;
    let input = ask(&rig, "admin-w", "sudo dnf install -y ripgrep").await;
    rig.wakes.ingest_worker_signal(WorkerSignal::Input { chat_id: "admin-w".into(), input }).await;
    assert!(recent(&rig, MGR, "admin-w").iter().any(|e| e.summary.contains("host-admin prompt") && e.summary.contains("never auto-declined")));
    rig.advance(4 * 3_600_000);
    rig.wakes.expire_approval_deadlines().await;
    rig.settle().await;
    assert!(pending(&rig, "admin-w"));

    // Answered before the deadline: the expired deadline leaves it alone.
    space_worker(&rig, "answered-w", Some(MGR), serde_json::json!({"requiredReport": true, "approvalTimeoutSecs": 90}), Some("/tmp".into()), None).await;
    let input = ask(&rig, "answered-w", "cargo fmt").await;
    rig.wakes.ingest_worker_signal(WorkerSignal::Input { chat_id: "answered-w".into(), input: input.clone() }).await;
    assert!(recent(&rig, MGR, "answered-w").iter().any(|e| e.summary.contains("auto-declined in 1 min")));
    let answer = mast_proto::UserInputAnswer { question_id: "q1".into(), labels: vec!["Yes".into()] };
    assert!(rig.core.sessions.respond_input("answered-w", &input.request_id, vec![answer]).unwrap());
    rig.wait_for(|| answered(&rig).len() == 2, "the user's answer").await;
    rig.advance(90_000);
    rig.wakes.expire_approval_deadlines().await;
    rig.settle().await;
    assert_eq!(answered(&rig), [vec!["No".to_owned()], vec!["Yes".to_owned()]]);
    assert!(!recent(&rig, MGR, "answered-w").iter().any(|e| e.summary.contains("approval auto-declined")));
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_and_non_autonomous_chats_have_no_approval_deadline() {
    use mast_engine::sessions::WorkerSignal;
    let rig = Rig::new().await;
    // A parentless chat stays interactive even with a task spec.
    space_worker(&rig, "solo", None, autonomous_spec(), Some("/tmp".into()), None).await;
    // A child chat whose task does not require a report is not autonomous.
    space_worker(&rig, "plain-child", Some(MGR), serde_json::json!({"approvalTimeoutSecs": 60}), Some("/tmp".into()), None).await;
    for chat in ["solo", "plain-child"] {
        let input = ask(&rig, chat, "cargo clean").await;
        rig.wakes.ingest_worker_signal(WorkerSignal::Input { chat_id: chat.into(), input }).await;
    }
    let held = rig.wakes.status(MGR).held;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].kind, WakeEventKind::AwaitingInput);
    assert_eq!(rig.wakes.status(MGR).hold_reason, Some(mast_proto::WakeHoldReason::Window));
    rig.advance(24 * 3_600_000);
    rig.wakes.expire_approval_deadlines().await;
    rig.settle().await;
    assert!(pending(&rig, "solo") && pending(&rig, "plain-child"));
    assert!(answered(&rig).is_empty());
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_completion_report_is_stamped_with_the_workers_token_totals() {
    let rig = Rig::new().await;
    space_worker(&rig, "stamp-w", Some(MGR), autonomous_spec(), Some("/tmp".into()), None).await;
    space_worker(&rig, "unmeasured-w", Some(MGR), autonomous_spec(), Some("/tmp".into()), None).await;
    // An open item gets an auto-continue nudge when the worker's turn ends, and that second turn would
    // never be released here. A completed item is not nudged.
    let item = mast_proto::derive_id("create-stamp-w", mast_proto::roles::ITEM);
    let actor = mast_proto::WorkOutlineActor { kind: mast_proto::WorkOutlineActorKind::User, device_id: rig.core.device_id.clone(), ..Default::default() };
    let complete: mast_proto::WorkOutlineMutation = serde_json::from_value(serde_json::json!({
        "op": "updateWorkItem", "workItemId": item, "changes": {"status": "completed"}, "operationId": "complete-stamp-w"
    })).unwrap();
    rig.core.workspace.work_outline_mutate(&complete, &actor, &[]).unwrap();
    rig.core.doc_host.queue_message("stamp-w", "measured", Vec::new()).unwrap();
    rig.wait_for(|| rig.core.sessions.turn_in_flight("stamp-w") && rig.prompts.lock().unwrap().iter().any(|p| p == "measured"), "the measured turn").await;
    let _ = rig.finish.send(());
    rig.wait_for(|| !rig.core.sessions.turn_in_flight("stamp-w"), "the measured turn's end").await;
    let completion = mast_proto::ReportCompletion { tested_sha: "abcdef1".into(), evidence: "tests pass".into() };
    rig.wakes.report_to_parent_with("stamp-w", "progress: half done", None).await.unwrap();
    rig.wakes.report_to_parent_with("stamp-w", "W2-T5 done", Some(&completion)).await.unwrap();
    rig.wakes.report_to_parent_with("unmeasured-w", "nothing measured", Some(&completion)).await.unwrap();
    let report = |worker: &str, first: &str| {
        recent(&rig, MGR, worker).into_iter().find_map(|e| e.report.filter(|r| r.starts_with(first))).unwrap()
    };
    let done = report("stamp-w", "W2-T5 done");
    assert!(
        done.ends_with(r#"Tokens: {"inputTokens":1200,"cachedInputTokens":1000,"outputTokens":50,"turns":1,"toolCalls":1}"#),
        "{done}"
    );
    assert!(!report("stamp-w", "progress").contains("Tokens:"), "only a completion is stamped");
    assert!(!report("unmeasured-w", "nothing measured").contains("Tokens:"), "no recorded turn, no stamp");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edits_outside_owned_paths_are_flagged_in_the_report_and_never_reverted() {
    let rig = Rig::new().await;
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let git = |args: &[&str]| {
        assert!(std::process::Command::new("git").args(args).current_dir(root)
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output().unwrap().status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::create_dir_all(root.join("owned")).unwrap();
    std::fs::write(root.join("owned/a.rs"), "a").unwrap();
    std::fs::write(root.join("shared.rs"), "s").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let spec = serde_json::json!({"requiredReport": true, "ownedPaths": ["owned/**"]});
    space_worker(&rig, "scope-w", Some(MGR), spec, Some(root.display().to_string()), None).await;
    // Its turn begins, then it edits inside and outside its owned paths.
    rig.wakes.ingest_sessions(&[session("scope-w", SessionStatus::Working)]).await;
    std::fs::write(root.join("owned/new.rs"), "n").unwrap();
    std::fs::write(root.join("shared.rs"), "edited").unwrap();
    rig.wakes.report_to_parent_with("scope-w", "progress", None).await.unwrap();
    let report = recent(&rig, MGR, "scope-w").into_iter().find_map(|e| e.report).unwrap();
    assert!(report.ends_with("Scope: edited outside the task's ownedPaths (flagged, not reverted): shared.rs"), "{report}");
    assert_eq!(std::fs::read_to_string(root.join("shared.rs")).unwrap(), "edited");
    // A later report lists only paths no earlier report did.
    rig.wakes.report_to_parent_with("scope-w", "second progress", None).await.unwrap();
    let report_for = |first: &str| recent(&rig, MGR, "scope-w").into_iter().find_map(|e| e.report.filter(|r| r.starts_with(first))).unwrap();
    assert!(!report_for("second progress").contains("Scope:"), "{}", report_for("second progress"));
    std::fs::write(root.join("stray.txt"), "x").unwrap();
    rig.wakes.report_to_parent_with("scope-w", "third progress", None).await.unwrap();
    let third = report_for("third progress");
    assert!(third.ends_with("(flagged, not reverted): stray.txt"), "{third}");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_autonomous_worker_is_reported_stalled_once() {
    use mast_engine::orchestrator_wakes::autonomy::{STALL_AFTER_MS, note_event};
    let rig = Rig::new().await;
    // The core's own tick reads the same registries at the real time.
    rig.clock.store(chrono::Utc::now().timestamp_millis(), Ordering::SeqCst);
    space_worker(&rig, "stall-w", Some(MGR), autonomous_spec(), None, None).await;
    space_worker(&rig, "tool-w", Some(MGR), autonomous_spec(), None, None).await;
    space_worker(&rig, "plain-stall-w", Some(MGR), serde_json::json!({}), None, None).await;
    let chats = ["stall-w", "tool-w", "plain-stall-w"];
    rig.wakes.ingest_sessions(&chats.map(|chat| session(chat, SessionStatus::Working))).await;
    let start = rig.clock.load(Ordering::SeqCst);
    for chat in chats {
        note_event(chat, &AgentEvent::TextDelta { text: "working".into() }, start);
    }
    mast_engine::tool_watch::call_emitted("tool-w", "t1", &mast_proto::ToolCall::Exec { command: "sleep 600".into() }, start);
    let stalled = |rig: &Rig| rig.wakes.status(MGR).recent.into_iter().filter(|e| e.kind == WakeEventKind::WorkerStalled).collect::<Vec<_>>();
    rig.advance(STALL_AFTER_MS - 1);
    rig.wakes.flag_stalled_workers().await;
    assert!(stalled(&rig).is_empty());
    rig.advance(1);
    rig.wakes.flag_stalled_workers().await;
    rig.wakes.flag_stalled_workers().await;
    let events = stalled(&rig);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].worker_chat_id, "stall-w");
    assert!(events[0].summary.contains("streamed nothing for 5 min"), "{}", events[0].summary);
    // New output starts a new quiet stretch.
    note_event("stall-w", &AgentEvent::TextDelta { text: "more".into() }, start + STALL_AFTER_MS);
    rig.advance(STALL_AFTER_MS);
    rig.wakes.flag_stalled_workers().await;
    assert_eq!(stalled(&rig).len(), 2);
    mast_engine::tool_watch::chat_turn_boundary("tool-w");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_autonomous_worker_auto_continues_by_default_and_a_plain_child_does_not() {
    let rig = Rig::new().await;
    space_worker(&rig, "ac-auto-w", Some(MGR), autonomous_spec(), None, None).await;
    space_worker(&rig, "ac-plain-w", Some(MGR), serde_json::json!({}), None, None).await;
    assert!(!rig.wakes.settings(MGR).auto_continue, "the Manager's own toggle stays off");
    auto_continue_finish(&rig, "ac-plain-w").await;
    auto_continue_finish(&rig, "ac-auto-w").await;
    let nudged = |chat: &str| rig.core.doc_host.open(chat).unwrap().doc().read_entries().unwrap().iter()
        .any(|entry| entry.parts.iter().any(|part| matches!(part, mast_doc::MessagePart::Text { text, .. } if text.contains("auto-continue 1/3"))));
    rig.wait_for(|| nudged("ac-auto-w"), "the autonomous worker's continuation").await;
    assert!(!nudged("ac-plain-w"));
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}
