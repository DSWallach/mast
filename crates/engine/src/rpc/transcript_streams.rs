use super::*;

/// The transcript watch as delta frames (`mast_doc::transcript_delta`): a
/// full `reset` first, then only changed entries per commit — the whole-Vec
/// serialization here was the per-tick cost that scaled with transcript size.
pub(super) fn doc_messages_stream(
    rx: watch::Receiver<crate::doc_host::TranscriptSnapshot>,
    doc: std::sync::Arc<mast_doc::SessionDoc>,
) -> BoxStream<'static, serde_json::Value> {
    use mast_doc::transcript_delta::{TranscriptFrame, diff_transcript};
    futures::stream::unfold(
        (
            rx,
            None::<crate::doc_host::TranscriptSnapshot>,
            doc,
            None,
            mast_doc::TranscriptBaseline::default(),
        ),
        |(mut rx, mut prev, doc, mut previous_usage, mut opening_baseline)| async move {
            loop {
                if prev.is_some() {
                    rx.changed().await.ok()?;
                }
                // Watchers retain the immutable published snapshot. Each
                // connection used to deep-copy the entire transcript here.
                let current = rx.borrow_and_update().clone();
                let frame = match prev.as_ref() {
                    None => TranscriptFrame::reset(&current.entries),
                    Some(prev) => diff_transcript(&prev.entries, &current.entries),
                };
                let replay_baseline = match prev.as_ref() {
                    None => {
                        opening_baseline = mast_doc::TranscriptBaseline::capture(&current.entries);
                        Some(opening_baseline.clone())
                    }
                    Some(prev)
                        if !std::sync::Arc::ptr_eq(
                            &prev.replay_baseline,
                            &current.replay_baseline,
                        ) =>
                    {
                        // The tracker only observes changes after attach; its
                        // baseline omits unchanged cached parts. Preserve this
                        // subscription's opening cutoff without capturing live
                        // appends or sharing another viewer's later cutoff.
                        // Ordinary live updates never rebuild this metadata.
                        let mut baseline = (*current.replay_baseline).clone();
                        for (entry, parts) in &opening_baseline.entries {
                            let merged = baseline.entries.entry(entry.clone()).or_default();
                            for (part, &len) in parts {
                                let cutoff = merged.entry(part.clone()).or_default();
                                *cutoff = (*cutoff).max(len);
                            }
                        }
                        Some(baseline)
                    }
                    _ => None,
                };
                prev = Some(current);
                // No-op commits (a second watcher attaching, command-only
                // changes) produce empty deltas — skip the frame entirely.
                let usage = doc.context_usage();
                if frame.is_empty_delta() && usage == previous_usage && replay_baseline.is_none() {
                    continue;
                }
                previous_usage = usage;
                let value = serde_json::to_value(mast_doc::TranscriptUpdate {
                    frame,
                    context_usage: usage,
                    replay_baseline,
                })
                .ok()?;
                return Some((value, (rx, prev, doc, previous_usage, opening_baseline)));
            }
        },
    )
    .boxed()
}

/// First paint reads only the local tail; full history is deferred until the
/// next stream poll. No network dependency or persisted truncation.
pub(super) async fn opening_doc_messages_stream(
    host: crate::doc_host::DocHost,
    chat_id: String,
) -> Result<BoxStream<'static, serde_json::Value>, RpcError> {
    let (handle, preview) = tokio::task::spawn_blocking(move || {
        let handle = host.open(&chat_id)?;
        let entries = handle.doc().read_opening_tail(128)?;
        let mut preview = serde_json::to_value(mast_doc::TranscriptUpdate {
            frame: mast_doc::TranscriptFrame::reset(&entries),
            context_usage: handle.doc().context_usage(),
            replay_baseline: Some(mast_doc::TranscriptBaseline::capture(&entries)),
        })
        .map_err(|e| crate::EngineError::Other(e.to_string()))?;
        preview["historyPending"] = serde_json::Value::Bool(true);
        Ok::<_, crate::EngineError>((handle, preview))
    })
    .await
    .map_err(|e| RpcError::Failed(e.to_string()))?
    .map_err(|e| RpcError::Failed(e.to_string()))?;
    // Start the authoritative attachment now, independently of the consumer
    // polling the second stream item.  Deferring the spawn itself to the next
    // poll left a restored viewport with only its provisional frame when the
    // RPC/GPUI pumps went quiet after first paint; the next user mutation woke
    // the pipeline and made the old transcript appear, which looked as if the
    // send had recovered it.  The blocking work still runs off-thread and we
    // still yield `preview` first, but history hydration no longer depends on
    // another UI event.
    let full_attach =
        tokio::task::spawn_blocking(move || (handle.watch_messages(), handle.doc_arc()));
    let full = futures::stream::once(async move {
        match full_attach.await {
            Ok((rx, doc)) => doc_messages_stream(rx, doc),
            Err(error) => {
                tracing::warn!(%error, "transcript opening failed");
                futures::stream::empty().boxed()
            }
        }
    })
    .flatten();
    Ok(futures::stream::once(async move { preview })
        .chain(full)
        .boxed())
}

