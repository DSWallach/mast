use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::FutureExt;

use super::*;

type Store = SwrStore<Vec<u32>, String>;

fn counting(
    calls: &Arc<AtomicUsize>,
    value: Vec<u32>,
) -> impl FnOnce() -> BoxFuture<'static, Result<Vec<u32>, String>> {
    let calls = calls.clone();
    move || {
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            for _ in 0..5 {
                tokio::task::yield_now().await;
            }
            Ok(value)
        }
        .boxed()
    }
}

#[tokio::test]
async fn fresh_value_makes_no_request() {
    let store = Store::new(Duration::from_secs(60));
    let calls = Arc::new(AtomicUsize::new(0));
    store
        .get("k", false, counting(&calls, vec![1]))
        .await
        .unwrap();
    let again = store
        .get("k", false, counting(&calls, vec![2]))
        .await
        .unwrap();
    assert_eq!(again.value, vec![1]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn force_bypasses_the_window() {
    let store = Store::new(Duration::from_secs(60));
    let calls = Arc::new(AtomicUsize::new(0));
    store
        .get("k", false, counting(&calls, vec![1]))
        .await
        .unwrap();
    let forced = store
        .get("k", true, counting(&calls, vec![2]))
        .await
        .unwrap();
    assert_eq!(forced.value, vec![2]);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn concurrent_gets_share_one_request() {
    let store = Store::new(Duration::from_secs(60));
    let calls = Arc::new(AtomicUsize::new(0));
    let (a, b, c) = tokio::join!(
        store.get("k", false, counting(&calls, vec![1])),
        store.get("k", false, counting(&calls, vec![1])),
        store.get("k", true, counting(&calls, vec![1])),
    );
    assert!(a.is_ok() && b.is_ok() && c.is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failure_keeps_the_stale_value() {
    let store = Store::new(Duration::ZERO);
    let calls = Arc::new(AtomicUsize::new(0));
    store
        .get("k", false, counting(&calls, vec![1]))
        .await
        .unwrap();
    let failed = store
        .get("k", false, || async { Err("boom".to_string()) }.boxed())
        .await;
    assert_eq!(failed.unwrap_err(), "boom");
    assert_eq!(store.peek("k").unwrap().value, vec![1]);
}

#[tokio::test]
async fn a_cold_start_loads_the_persisted_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.json");
    let first = Store::new(Duration::from_secs(60));
    first.attach_file(path.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    first
        .get("k", false, counting(&calls, vec![7]))
        .await
        .unwrap();
    // The write runs on a blocking worker.
    for _ in 0..100 {
        if path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let second = Store::new(Duration::from_secs(60));
    second.attach_file(path);
    assert_eq!(second.peek("k").unwrap().value, vec![7]);
    // Still inside the window: no request.
    let hit = second
        .get("k", false, counting(&calls, vec![9]))
        .await
        .unwrap();
    assert_eq!(hit.value, vec![7]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stale_persisted_value_is_peekable_then_refetched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.json");
    let old = Utc::now() - chrono::Duration::minutes(10);
    std::fs::write(
        &path,
        serde_json::json!({ "k": { "fetchedAt": old, "value": [1] } }).to_string(),
    )
    .unwrap();
    let store = Store::new(Duration::from_secs(60));
    store.attach_file(path);
    assert_eq!(store.peek("k").unwrap().value, vec![1]);
    let calls = Arc::new(AtomicUsize::new(0));
    let fresh = store
        .get("k", false, counting(&calls, vec![2]))
        .await
        .unwrap();
    assert_eq!(fresh.value, vec![2]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn clearing_cache_discards_inflight_results() {
    let store = Store::new(Duration::from_secs(60));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let reader = store.clone();
    let old = tokio::spawn(async move {
        reader.get("k", false, || async move {
            let _ = started_tx.send(());
            let _ = release_rx.await;
            Ok(vec![1])
        }.boxed()).await.unwrap()
    });
    started_rx.await.unwrap();
    store.clear();
    let fresh = store.get("k", false, || async { Ok(vec![2]) }.boxed()).await.unwrap();
    release_tx.send(()).unwrap();
    assert_eq!(old.await.unwrap().value, vec![1]);
    assert_eq!(fresh.value, vec![2]);
    assert_eq!(store.peek("k").unwrap().value, vec![2]);
}

#[test]
fn older_disk_writes_cannot_restore_a_cleared_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.json");
    let store = Store::new(Duration::from_secs(60));
    store.attach_file(path.clone());
    let (old, cleared) = {
        let mut state = store.lock();
        state.entries.insert("k".into(), Arc::new(Entry { fetched_at: Utc::now(), value: vec![1] }));
        let old = persist_snapshot(&state);
        state.entries.clear();
        let cleared = persist_snapshot(&state);
        (old, cleared)
    };
    write_snapshot(cleared);
    write_snapshot(old);
    let disk: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(disk, serde_json::json!({}));
}
