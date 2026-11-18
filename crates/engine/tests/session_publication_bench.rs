//! Run with `cargo test -p mast-engine --profile installed --lib session_publication_benchmark -- --ignored --nocapture --test-threads=1`.
//! Included in the sessions unit tests to exercise the private production update paths.

macro_rules! session_publication_benchmark {
    () => {
        #[tokio::test(flavor = "current_thread")]
        #[ignore = "benchmark: run with --ignored --nocapture"]
        async fn session_publication_benchmark() {
            use std::time::{Duration, Instant};
            use crate::{EngineCore, default_registry};

            fn cpu_seconds() -> f64 {
                let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
                assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) }, 0);
                let usage = unsafe { usage.assume_init() };
                (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64
                    + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1_000_000.0
            }

            for active in [10, 50] {
                for watchers in [1, 4] {
                    let dir = tempfile::tempdir().unwrap();
                    let core = EngineCore::assemble(dir.path(), Arc::new(default_registry()), HarnessId::Mock, None).unwrap();
                    let engine = &core.sessions;
                    for i in 0..360 {
                        engine.inner.set_status(&format!("chat-{i:03}"), SessionStatus::Working, true);
                    }
                    let mut receivers: Vec<_> = (0..watchers)
                        .map(|_| core.workspace.merged_sessions_watch(engine.watch_sessions()))
                        .collect();
                    let cpu_start = cpu_seconds();
                    let start = Instant::now();
                    let mut emissions = 0;
                    let mut updates = 0;
                    // Each chat produces one heartbeat, one todo change and one status
                    // change per second. Wait for every subscriber so watch coalescing
                    // does not hide publications or merges.
                    for cycle in 0..2 {
                        let deadline = Instant::now() + Duration::from_secs(1);
                        for i in 0..active {
                            let chat = format!("chat-{i:03}");
                            for kind in 0..3 {
                                match kind {
                                    0 => {
                                        lock(&engine.inner.statuses).get_mut(&chat).unwrap().updated_at = Utc::now() - chrono::Duration::seconds(11);
                                        engine.inner.touch_session(&chat);
                                    }
                                    1 => engine.inner.set_todo_progress(&chat, mast_proto::TodoProgress { done: cycle + 1, total: 3 }),
                                    _ => engine.inner.set_status(&chat, if cycle == 0 { SessionStatus::Idle } else { SessionStatus::Working }, false),
                                }
                                updates += 1;
                                for receiver in &mut receivers {
                                    tokio::time::timeout(Duration::from_secs(5), receiver.changed()).await.unwrap().unwrap();
                                    emissions += 1;
                                }
                            }
                        }
                        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                    }
                    let elapsed = start.elapsed().as_secs_f64();
                    let cpu = cpu_seconds() - cpu_start;
                    // The current implementation performs one full local clone/sort
                    // per update and one merge per independently subscribed watch.
                    eprintln!("BENCH session publication | active={active} watchers={watchers} elapsed_s={elapsed:.3} cpu_s={cpu:.4} core_pct={:.2} updates_per_s={:.1} local_table_clones_per_s={:.1} local_sorts_per_s={:.1} inferred_merges_per_s={:.1} emissions_per_s={:.1}", 100.0 * cpu / elapsed, updates as f64 / elapsed, updates as f64 / elapsed, updates as f64 / elapsed, emissions as f64 / elapsed, emissions as f64 / elapsed);
                }
            }
        }
    };
}
