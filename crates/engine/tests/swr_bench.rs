//! SWR lock-hold benchmark (`#[ignore]`). Its own test target so it builds under
//! the `installed` profile, which the `engine` target's debug-only seams exclude:
//! `cargo test -p mast-engine --profile installed --test swr_bench -- --ignored --nocapture`

use std::sync::Arc;
use std::time::Instant;

use mast_engine::swr::{Entry, SwrStore};
use mast_proto::LinearIssueSummary;

fn issue(i: usize) -> LinearIssueSummary {
    LinearIssueSummary {
        id: format!("uuid-{i}"),
        identifier: format!("ENG-{i}"),
        title: format!("Issue {i}: something is slow"),
        description: Some("Description paragraph. ".repeat(60)),
        url: format!("https://linear.app/acme/issue/ENG-{i}"),
        updated_at: "2026-10-28T12:00:00Z".into(),
        ..Default::default()
    }
}

/// How long a writer's `get(force)` keeps the store's lock from readers: a
/// reader thread polls `peek` while the writer refreshes one key of a cache
/// holding 1, 8 or 32 keys (300 issues each). The reader's worst stall is the
/// lock hold time (`finish` serializes the whole map under the lock). One
/// `SAMPLE swr | <keys> | <metric> | ms=` line per metric.
#[test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_swr_lock_hold() {
    for keys in [1usize, 8, 32] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let entries: std::collections::HashMap<String, Entry<Vec<LinearIssueSummary>>> = (0..keys)
            .map(|k| {
                (
                    format!("key{k}"),
                    Entry {
                        fetched_at: chrono::Utc::now(),
                        value: (0..300).map(|i| issue(k * 1000 + i)).collect(),
                    },
                )
            })
            .collect();
        std::fs::write(&path, serde_json::to_vec(&entries).unwrap()).unwrap();
        let store: SwrStore<Vec<LinearIssueSummary>, String> =
            SwrStore::new(std::time::Duration::from_secs(60));
        store.attach_file(path.clone());
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (store, stop) = (store.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut worst = 0f64;
                let mut total = 0f64;
                let mut count = 0u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let t = Instant::now();
                    std::hint::black_box(store.peek("key0").is_some());
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    worst = worst.max(ms);
                    total += ms;
                    count += 1;
                }
                (worst, total / count.max(1) as f64)
            })
        };
        let t = Instant::now();
        for _ in 0..20 {
            rt.block_on(store.get("key0", true, || {
                Box::pin(async { Ok((0..300).map(issue).collect()) })
            }))
            .unwrap();
        }
        let per_get = t.elapsed().as_secs_f64() * 1000.0 / 20.0;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let (worst, mean) = reader.join().unwrap();
        eprintln!("SAMPLE swr | {keys} | get(force) | ms={per_get:.3}");
        eprintln!("SAMPLE swr | {keys} | lock hold (reader max stall) | ms={worst:.3}");
        eprintln!("SAMPLE swr | {keys} | reader mean peek | ms={mean:.4}");
    }
}

