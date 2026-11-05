//! Long-chat transcript: ~2000 messages with large tool outputs. First
//! paint, idle redraw and scrolling through the whole list.

use super::*;
use crate::perf::{data, measure, measure_n};
use mast_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};

fn text(id: &str, t: String) -> MessagePart {
    MessagePart::Text {
        id: id.into(),
        text: t,
    }
}

fn tool(i: usize) -> MessagePart {
    MessagePart::Tool {
        id: format!("tool{i}"),
        call: ToolCall::Exec {
            command: format!("cargo test -p crate-{i} -- --nocapture"),
        },
        is_error: false,
        resolved: true,
        // Pre-strip entries carry up to 4 KB inline; newer ones a ref + size.
        output: Some(data::tool_output(4096)),
        diff: None,
        output_ref: Some(format!("chat/tool{i}")),
        output_bytes: Some(200_000),
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
    }
}

pub(crate) fn long_chat(messages: usize) -> Vec<SessionMessageEntry> {
    let body = data::markdown_plan(1800);
    (0..messages)
        .map(|i| {
            let user = i % 2 == 0;
            let parts = if user {
                vec![text("t", format!("Please look at step {i}: the build fails with the new registry ops and I do not know why."))]
            } else {
                vec![
                    MessagePart::Reasoning { id: "r".into(), text: "Thinking about the failure. ".repeat(20) },
                    tool(i),
                    tool(i + 100_000),
                    text("t", body.clone()),
                ]
            };
            SessionMessageEntry {
                id: format!("m{i}"),
                role: if user { MessageRole::User } else { MessageRole::Assistant },
                parts,
                created_at: i as i64,
                device_id: "dev".into(),
                status: Some(MessageStatus::Complete),
                continuation_of: None,
            }
        })
        .collect()
}

struct Cached(Entity<Transcript>);

impl Render for Cached {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.0
            .clone()
            .cached(gpui::StyleRefinement::default().size_full())
    }
}

fn draw(vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
}

#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_transcript_long_chat(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        Cached(transcript)
    });
    let transcript = view.read_with(vcx, |v, _| v.0.clone());
    let entries = measure("transcript", "build 2000 synthetic entries", || {
        long_chat(2000)
    });
    let payload: usize = entries
        .iter()
        .flat_map(|e| &e.parts)
        .map(|p| match p {
            MessagePart::Text { text, .. } | MessagePart::Reasoning { text, .. } => text.len(),
            MessagePart::Tool { output, .. } => output.as_ref().map_or(0, String::len),
            _ => 0,
        })
        .sum();
    eprintln!("BENCH transcript | payload bytes={payload}");
    measure("transcript", "sync (entries -> rows)", || {
        transcript.update(vcx, |this, cx| {
            this.state.update(cx, |state, _| {
                state.selected_chat = Some("chat".into());
                state.transcript_replayed = true;
                state.transcript = entries;
                state.transcript_revision += 1;
            });
            this.sync(cx);
        })
    });
    let rows = transcript.read_with(vcx, |t, _| t.rows.len());
    eprintln!("BENCH transcript | rows={rows}");
    measure("transcript", "first draw", || draw(vcx));
    // BENCH_ONLY=stream skips the redraw/scroll loops (to profile the tick).
    let only_stream = std::env::var("BENCH_ONLY").as_deref() == Ok("stream");
    if !only_stream {
        measure_n("transcript", "idle redraw", 30, |_| draw(vcx));
        measure_n("transcript", "scroll 700px + draw", 100, |_| {
            transcript.update(vcx, |t, _| t.list.scroll_by(px(700.0)));
            draw(vcx);
        });
        measure_n("transcript", "scroll back -700px + draw", 100, |_| {
            transcript.update(vcx, |t, _| t.list.scroll_by(px(-700.0)));
            draw(vcx);
        });
    }
    // A streaming tick: the last message grows, the whole list is re-synced.
    measure_n(
        "transcript",
        "stream tick (revision bump + sync + draw)",
        30,
        |i| {
            transcript.update(vcx, |this, cx| {
                this.state.update(cx, |state, _| {
                    if let Some(last) = state.transcript.last_mut() {
                        last.parts
                            .push(text(&format!("s{i}"), format!("token {i}")));
                    }
                    state.transcript_revision += 1;
                });
                this.sync(cx);
            });
            draw(vcx);
        },
    );
}

#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_prepared_transcript_open(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let entries = long_chat(2000);
    let update = mast_doc::TranscriptUpdate {
        frame: mast_doc::TranscriptFrame::reset(&entries),
        context_usage: None,
        replay_baseline: Some(mast_doc::TranscriptBaseline::capture(&entries)),
    };
    let prepared = measure("transcript", "prepare 2000 entries", || {
        crate::transcript::TranscriptPreparation::default()
            .prepare(&update)
            .unwrap()
    });
    let state = cx.new(|_| AppState::new());
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        Cached(transcript)
    });
    let transcript = view.read_with(vcx, |v, _| v.0.clone());
    measure("transcript", "open prepared 2000 entries and draw", || {
        transcript.update(vcx, |this, cx| {
            this.state.update(cx, |state, _| {
                state.selected_chat = Some("chat".into());
                state.transcript_replayed = true;
                state.transcript = entries;
                state.transcript_revision += 1;
                state.prepared_transcripts.insert("chat".into(), prepared);
            });
            this.sync(cx);
        });
        draw(vcx);
    });
}

/// A wire frame traverses the real RPC subscription, background preparation,
/// AppState notification, transcript sync and headless GPUI draw.
#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_subscribed_transcript_stream(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = runtime.enter();
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let (out, mut requests) = tokio::sync::mpsc::channel::<String>(64);
    let (replies, inbound) = tokio::sync::mpsc::channel::<String>(64);
    let state = cx.new(|_| AppState::new());
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        Cached(transcript)
    });
    let transcript = view.read_with(vcx, |view, _| view.0.clone());
    let mut entries = long_chat(2000);
    entries.last_mut().unwrap().status = Some(MessageStatus::Streaming);
    let opening = mast_doc::TranscriptUpdate {
        frame: mast_doc::TranscriptFrame::reset(&entries),
        context_usage: None,
        replay_baseline: Some(mast_doc::TranscriptBaseline::capture(&entries)),
    };
    let started = std::time::Instant::now();
    state.update(vcx, |state, cx| {
        state.set_test_engine(crate::state::EngineHandle::from_test_client(
            mast_rpc::RpcClient::new(out, inbound),
        ));
        state.select_chat(Some("chat".into()), cx);
    });
    vcx.run_until_parked();
    runtime.block_on(tokio::task::yield_now());
    let request_id = loop {
        let request: serde_json::Value =
            serde_json::from_str(&requests.try_recv().expect("transcript watch request")).unwrap();
        if request["method"] == mast_rpc::methods::WATCH_DOC_MESSAGES {
            break request["id"].clone();
        }
    };
    let frame = |update: &mast_doc::TranscriptUpdate| {
        serde_json::json!({"id": request_id, "item": update}).to_string()
    };
    let wait_for_revision = |vcx: &mut gpui::VisualTestContext, previous: u64| {
        let started = std::time::Instant::now();
        loop {
            runtime.block_on(tokio::task::yield_now());
            vcx.run_until_parked();
            if state.read_with(vcx, |state, _| {
                state.transcript_revision > previous && state.transcript_replayed
            }) {
                break;
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(30),
                "transcript watch did not deliver the frame"
            );
            std::thread::yield_now();
        }
    };
    let previous = state.read_with(vcx, |state, _| state.transcript_revision);
    replies.try_send(frame(&opening)).unwrap();
    wait_for_revision(vcx, previous);
    draw(vcx);
    eprintln!(
        "BENCH transcript | selection through subscription replay and draw | ms={:.2}",
        started.elapsed().as_secs_f64() * 1000.0
    );
    state.update(vcx, |state, cx| state.select_chat(None, cx));
    let revisit = std::time::Instant::now();
    state.update(vcx, |state, cx| state.select_chat(Some("chat".into()), cx));
    assert!(
        state.read_with(vcx, |state, _| state.transcript_replayed),
        "revisit should restore cached transcript"
    );
    draw(vcx);
    eprintln!(
        "BENCH transcript | cached selection and draw | ms={:.2}",
        revisit.elapsed().as_secs_f64() * 1000.0
    );
    // The selection starts a fresh watch. Reconnect the synthetic peer before
    // measuring streaming, since the preceding watch was cancelled.
    vcx.run_until_parked();
    runtime.block_on(tokio::task::yield_now());
    let request_id = loop {
        let request: serde_json::Value =
            serde_json::from_str(&requests.try_recv().expect("revisit watch request")).unwrap();
        if request["method"] == mast_rpc::methods::WATCH_DOC_MESSAGES {
            break request["id"].clone();
        }
    };
    let frame = |update: &mast_doc::TranscriptUpdate| {
        serde_json::json!({"id": request_id, "item": update}).to_string()
    };
    let previous = state.read_with(vcx, |state, _| state.transcript_revision);
    replies.try_send(frame(&opening)).unwrap();
    wait_for_revision(vcx, previous);
    let retained = state.read_with(vcx, |state, _| {
        state.prepared_transcripts["chat"].rows["m0"].clone()
    });
    let mut updates = Vec::new();
    for _ in 0..30 {
        let before = entries.clone();
        if let Some(MessagePart::Text { text, .. }) = entries.last_mut().unwrap().parts.last_mut() {
            text.push_str(" chunk");
        } else {
            panic!("last part must be text");
        }
        let update = mast_doc::TranscriptUpdate {
            frame: mast_doc::diff_transcript(&before, &entries),
            context_usage: None,
            replay_baseline: None,
        };
        updates.push(frame(&update));
    }
    RENDERED_ROW_COUNT.with(|count| count.set(0));
    measure_n(
        "transcript",
        "subscription chunk through preparation and draw",
        updates.len(),
        |index| {
            let previous = state.read_with(vcx, |state, _| state.transcript_revision);
            replies.try_send(updates[index].clone()).unwrap();
            wait_for_revision(vcx, previous);
            draw(vcx);
        },
    );
    let rendered = RENDERED_ROW_COUNT.with(|count| count.get());
    let rows = transcript.read_with(vcx, |transcript, _| transcript.rows.len());
    let reused = state.read_with(vcx, |state, _| {
        std::sync::Arc::ptr_eq(&retained, &state.prepared_transcripts["chat"].rows["m0"])
    });
    eprintln!(
        "BENCH transcript | rows={rows} rendered_row_calls={rendered} chunks={} unchanged_entry_reused={reused}",
        updates.len()
    );
    assert!(reused, "unchanged entry rows remain cached");
    assert!(
        rendered < rows * updates.len(),
        "streaming must not render every transcript row per chunk"
    );
}

/// Selection through the embedded engine's local opening-tail and full replay.
#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_engine_chat_selection(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = runtime.enter();
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let core = mast_engine::EngineCore::assemble(
        dir.path(),
        std::sync::Arc::new(mast_engine::HarnessRegistry::new()),
        mast_proto::HarnessId::Mock,
        None,
    )
    .unwrap();
    let doc = core.doc_host.open("selection-benchmark").unwrap();
    for entry in long_chat(2000) {
        doc.doc().push_message(&entry).unwrap();
    }
    let client = mast_rpc::memory_client(core.rpc_service());
    let state = cx.new(|_| AppState::new());
    let (_view, vcx) = cx.add_window_view(|_, cx| {
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        Cached(transcript)
    });
    state.update(vcx, |state, _| {
        state.set_test_engine(crate::state::EngineHandle::from_test_client(client));
    });
    let start = std::time::Instant::now();
    state.update(vcx, |state, cx| {
        state.select_chat(Some("selection-benchmark".into()), cx)
    });
    loop {
        runtime.block_on(tokio::task::yield_now());
        vcx.run_until_parked();
        if state.read_with(vcx, |state, _| !state.transcript.is_empty()) {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(60),
            "opening frame did not arrive"
        );
        std::thread::yield_now();
    }
    draw(vcx);
    eprintln!(
        "BENCH transcript | engine selection to opening draw | ms={:.2}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    loop {
        runtime.block_on(tokio::task::yield_now());
        vcx.run_until_parked();
        if state.read_with(vcx, |state, _| state.transcript_replayed) {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(60),
            "full replay did not arrive"
        );
        std::thread::yield_now();
    }
    draw(vcx);
    eprintln!(
        "BENCH transcript | engine selection to full replay draw | ms={:.2}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    state.update(vcx, |state, cx| state.select_chat(None, cx));
    let revisit = std::time::Instant::now();
    state.update(vcx, |state, cx| {
        state.select_chat(Some("selection-benchmark".into()), cx)
    });
    let previous_revision = state.read_with(vcx, |state, _| state.transcript_revision);
    let hit = state.read_with(vcx, |state, _| state.transcript_replayed);
    draw(vcx);
    eprintln!(
        "BENCH transcript | engine cached revisit draw | ms={:.2} | cache_hit={hit}",
        revisit.elapsed().as_secs_f64() * 1000.0
    );
    loop {
        runtime.block_on(tokio::task::yield_now());
        vcx.run_until_parked();
        if state.read_with(vcx, |state, _| {
            state.transcript_revision > previous_revision
        }) {
            break;
        }
        assert!(
            revisit.elapsed() < std::time::Duration::from_secs(60),
            "revisit replay did not arrive"
        );
        std::thread::yield_now();
    }
    draw(vcx);
    eprintln!(
        "BENCH transcript | engine revisit to authoritative replay draw | ms={:.2}",
        revisit.elapsed().as_secs_f64() * 1000.0
    );
    state.update(vcx, |state, cx| state.select_chat(None, cx));
    vcx.run_until_parked();
}
