use super::*;
use futures::StreamExt;
use std::sync::Arc;

#[tokio::test]
async fn replay_cutoff_travels_with_coalesced_backfill_and_live_content() {
    use crate::doc_host::{DocHost, DocHostConfig};
    use mast_sync::chat_client::ChatDocSink;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(mast_sync::DocsStore::open(dir.path()).unwrap());
    let host = DocHost::new(
        store.clone(),
        DocHostConfig {
            device_id: "viewer".into(),
            default_harness: mast_proto::HarnessId::Mock,
            edge: None,
        },
    );
    let handle = host.open("replay-chat").unwrap();
    let sink = crate::chat2_host::EngineChatSink::new(&handle.doc_arc(), store, "replay-chat")
        .with_handle(Arc::downgrade(&handle));
    let mut stream = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
    let first: mast_doc::TranscriptUpdate =
        serde_json::from_value(stream.next().await.unwrap()).unwrap();
    assert!(first.replay_baseline.unwrap().entries.is_empty());

    let source = mast_doc::SessionDoc::init("replay-chat").unwrap();
    let append = |id: &str| {
        source
            .push_message(&mast_doc::SessionMessageEntry {
                id: id.into(),
                role: mast_doc::MessageRole::Assistant,
                parts: vec![mast_doc::MessagePart::Text {
                    id: "text".into(),
                    text: id.into(),
                }],
                created_at: 0,
                device_id: "writer".into(),
                status: Some(mast_doc::MessageStatus::Streaming),
                continuation_of: None,
            })
            .unwrap()
    };
    append("cached");
    sink.apply_checkpoint(&source.export_snapshot().unwrap(), 0)
        .unwrap();
    let checkpoint: mast_doc::TranscriptUpdate = serde_json::from_value(
        tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        checkpoint
            .replay_baseline
            .unwrap()
            .entries
            .contains_key("cached")
    );

    let version = source.doc().oplog_vv();
    append("away");
    sink.apply_replay_row(
        &source
            .doc()
            .export(loro::ExportMode::updates(&version))
            .unwrap(),
        1,
    );
    let version = source.doc().oplog_vv();
    append("live");
    sink.apply_row(
        &source
            .doc()
            .export(loro::ExportMode::updates(&version))
            .unwrap(),
        2,
    );
    // Neither the doc worker nor the RPC consumer ran between these
    // imports. They must not flatten their different presentation origins.
    let update: mast_doc::TranscriptUpdate = serde_json::from_value(
        tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let cutoff = update.replay_baseline.unwrap();
    assert!(cutoff.entries.contains_key("away"));
    assert!(!cutoff.entries.contains_key("live"));
    let mut entries = vec![];
    mast_doc::apply_transcript_frame(&mut entries, checkpoint.frame).unwrap();
    mast_doc::apply_transcript_frame(&mut entries, update.frame).unwrap();
    assert_eq!(entries.len(), 3);

    let version = source.doc().oplog_vv();
    append("next-live");
    sink.apply_row(
        &source
            .doc()
            .export(loro::ExportMode::updates(&version))
            .unwrap(),
        3,
    );
    let update: mast_doc::TranscriptUpdate = serde_json::from_value(
        tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        update.replay_baseline.is_none(),
        "live updates must not resend the history watermark"
    );
    let mut reopened = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
    let opening: mast_doc::TranscriptUpdate =
        serde_json::from_value(reopened.next().await.unwrap()).unwrap();
    assert_eq!(
        opening.replay_baseline.unwrap().entries.len(),
        4,
        "reopening includes all existing content as history"
    );
    host.shutdown_workers().await;
}

#[tokio::test]
async fn replay_metadata_and_backfill_leave_interleaved_local_content_live() {
    use crate::doc_host::{DocHost, DocHostConfig};
    use mast_sync::chat_client::ChatDocSink;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(mast_sync::DocsStore::open(dir.path()).unwrap());
    let host = DocHost::new(
        store.clone(),
        DocHostConfig {
            device_id: "host".into(),
            default_harness: mast_proto::HarnessId::Mock,
            edge: None,
        },
    );
    let handle = host.open("interleaved").unwrap();
    let sink = crate::chat2_host::EngineChatSink::new(&handle.doc_arc(), store, "interleaved")
        .with_handle(Arc::downgrade(&handle));
    let mut stream = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
    stream.next().await.unwrap();
    let source = mast_doc::SessionDoc::init("interleaved").unwrap();
    sink.apply_checkpoint(&source.export_snapshot().unwrap(), 0)
        .unwrap();
    let entry = |id: &str| mast_doc::SessionMessageEntry {
        id: id.into(),
        role: mast_doc::MessageRole::Assistant,
        parts: vec![mast_doc::MessagePart::Text {
            id: "text".into(),
            text: id.into(),
        }],
        created_at: 0,
        device_id: "host".into(),
        status: Some(mast_doc::MessageStatus::Streaming),
        continuation_of: None,
    };
    handle.doc().push_message(&entry("local-before")).unwrap();
    source.update_context_usage(Some(10), Some(100)).unwrap();
    sink.apply_replay_row(&source.export_snapshot().unwrap(), 1);
    let update: mast_doc::TranscriptUpdate = serde_json::from_value(
        tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        update.replay_baseline.is_none(),
        "metadata must not reset ongoing live animations"
    );
    let mut entries = vec![];
    mast_doc::apply_transcript_frame(&mut entries, update.frame).unwrap();
    assert_eq!(entries[0].id, "local-before");
    let version = source.doc().oplog_vv();
    source.push_message(&entry("historical")).unwrap();
    handle.doc().push_message(&entry("local-between")).unwrap();
    sink.apply_replay_row(
        &source
            .doc()
            .export(loro::ExportMode::updates(&version))
            .unwrap(),
        2,
    );
    handle.doc().push_message(&entry("local-after")).unwrap();
    let update: mast_doc::TranscriptUpdate = serde_json::from_value(
        tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let baseline = update.replay_baseline.unwrap();
    assert_eq!(baseline.entries.len(), 1);
    assert!(baseline.entries.contains_key("historical"));
    mast_doc::apply_transcript_frame(&mut entries, update.frame).unwrap();
    assert_eq!(entries.len(), 4);
    host.shutdown_workers().await;
}

#[tokio::test]
async fn replay_preserves_each_watchers_opening_cutoff_without_consuming_live_text() {
    use crate::doc_host::{DocHost, DocHostConfig};
    use mast_sync::chat_client::ChatDocSink;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(mast_sync::DocsStore::open(dir.path()).unwrap());
    let host = DocHost::new(
        store.clone(),
        DocHostConfig {
            device_id: "viewer".into(),
            default_harness: mast_proto::HarnessId::Mock,
            edge: None,
        },
    );
    let handle = host.open("cached-replay").unwrap();
    let sink =
        crate::chat2_host::EngineChatSink::new(&handle.doc_arc(), store, "cached-replay")
            .with_handle(Arc::downgrade(&handle));
    let source = mast_doc::SessionDoc::init("cached-replay").unwrap();
    let mut writer = mast_doc::SegmentWriter::begin(&source, "reply", "host", 0).unwrap();
    let text = |id: &str, value: &str| mast_doc::MessagePart::Text {
        id: id.into(),
        text: value.into(),
    };
    let cached = text("body", "café histórico");
    writer.sync(std::slice::from_ref(&cached)).unwrap();
    sink.apply_checkpoint(&source.export_snapshot().unwrap(), 0)
        .unwrap();
    // Cached content exists before the first watcher and never enters
    // the changed-parts tracker. It may not have been painted yet.
    let mut first = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
    let opening: mast_doc::TranscriptUpdate =
        serde_json::from_value(first.next().await.unwrap()).unwrap();
    assert_eq!(
        opening.replay_baseline.unwrap().entries["reply"]["body"],
        "café histórico".len()
    );

    let live = text("body", "café histórico y nuevo");
    writer.sync(std::slice::from_ref(&live)).unwrap();
    sink.apply_row(&source.export_snapshot().unwrap(), 1);
    let update: mast_doc::TranscriptUpdate = serde_json::from_value(
        tokio::time::timeout(std::time::Duration::from_secs(2), first.next())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(update.replay_baseline.is_none());
    // A later subscriber sees a longer historical prefix, but must not
    // change the first subscriber's ongoing live animation.
    let mut second = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
    let opening: mast_doc::TranscriptUpdate =
        serde_json::from_value(second.next().await.unwrap()).unwrap();
    assert_eq!(
        opening.replay_baseline.unwrap().entries["reply"]["body"],
        "café histórico y nuevo".len()
    );

    let mut parts = vec![live];
    for ix in 0..2 {
        let id = format!("recovered-{ix}");
        parts.push(text(&id, "otro bloque histórico"));
        writer.sync(&parts).unwrap();
        sink.apply_replay_row(&source.export_snapshot().unwrap(), 2 + ix);
        for (stream, expected) in [
            (&mut first, "café histórico".len()),
            (&mut second, "café histórico y nuevo".len()),
        ] {
            let update: mast_doc::TranscriptUpdate = serde_json::from_value(
                tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                    .await
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            let baseline = update.replay_baseline.unwrap();
            assert_eq!(
                baseline.entries["reply"].get("body"),
                Some(&expected),
                "replay must retain this watcher's opening cutoff, excluding later live bytes"
            );
            assert_eq!(
                baseline.entries["reply"][&id],
                "otro bloque histórico".len()
            );
            assert_eq!(baseline.entries["reply"].len(), 2 + ix as usize);
        }
    }
    host.shutdown_workers().await;
}

#[tokio::test]
async fn reopening_rearms_history_for_previously_live_text() {
    use crate::doc_host::{DocHost, DocHostConfig};
    use mast_sync::chat_client::ChatDocSink;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(mast_sync::DocsStore::open(dir.path()).unwrap());
    let host = DocHost::new(
        store.clone(),
        DocHostConfig {
            device_id: "viewer".into(),
            default_harness: mast_proto::HarnessId::Mock,
            edge: None,
        },
    );
    let handle = host.open("reopen").unwrap();
    let sink = crate::chat2_host::EngineChatSink::new(&handle.doc_arc(), store, "reopen")
        .with_handle(Arc::downgrade(&handle));
    let source = mast_doc::SessionDoc::init("reopen").unwrap();
    let mut writer = mast_doc::SegmentWriter::begin(&source, "reply", "host", 0).unwrap();
    let part = |text: &str| mast_doc::MessagePart::Text {
        id: "body".into(),
        text: text.into(),
    };
    let mut stream = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
    stream.next().await.unwrap();
    writer.sync(&[part("live")]).unwrap();
    sink.apply_row(&source.export_snapshot().unwrap(), 1);
    let _: serde_json::Value =
        tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap();
    drop(stream);
    // No unwatched commit clears provenance before the new attach.
    let mut stream = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
    stream.next().await.unwrap();
    writer.sync(&[part("live plus recovered")]).unwrap();
    sink.apply_replay_row(&source.export_snapshot().unwrap(), 2);
    let update: mast_doc::TranscriptUpdate = serde_json::from_value(
        tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        update.replay_baseline.unwrap().entries["reply"]["body"],
        "live plus recovered".len()
    );
    host.shutdown_workers().await;
}

#[tokio::test]
async fn context_only_commits_reach_remote_watch_and_reconnect() {
    let host = mast_doc::SessionDoc::init("context-chat").unwrap();
    host.update_context_usage(Some(42000), Some(200000))
        .unwrap();
    // The viewing engine reads a replicated document, with no harness process.
    let remote = Arc::new(mast_doc::SessionDoc::from_doc(loro::LoroDoc::new()));
    remote
        .doc()
        .import(&host.export_snapshot().unwrap())
        .unwrap();
    let (tx, rx) = watch::channel(crate::doc_host::TranscriptSnapshot::default());
    let mut stream = doc_messages_stream(rx, remote.clone());
    let first = stream.next().await.unwrap();
    assert_eq!(first["contextUsage"]["tokens"], 42000);
    assert!(first.get("reset").is_some());
    let version = host.doc().oplog_vv();
    host.update_context_usage(Some(0), None).unwrap();
    remote
        .doc()
        .import(
            &host
                .doc()
                .export(loro::ExportMode::updates(&version))
                .unwrap(),
        )
        .unwrap();
    tx.send_replace(crate::doc_host::TranscriptSnapshot::default());
    let update = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(update["contextUsage"]["tokens"], 0);
    assert_eq!(update["contextUsage"]["window"], 200000);
    let mut reconnect = doc_messages_stream(tx.subscribe(), remote.clone());
    assert_eq!(
        reconnect.next().await.unwrap()["contextUsage"],
        update["contextUsage"]
    );
    remote.clear_context_usage().unwrap();
    tx.send_replace(crate::doc_host::TranscriptSnapshot::default());
    assert!(stream.next().await.unwrap()["contextUsage"].is_null());
}
