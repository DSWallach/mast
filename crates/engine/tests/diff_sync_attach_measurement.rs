//! Manual measurement of the 50-checkout watch attachment path.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mast_engine::{EngineCore, HarnessRegistry};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;

#[derive(Default)]
struct Sample {
    stage: String,
    elapsed_us: Option<u64>,
    successful: u64,
    failed: u64,
    skipped: bool,
    root_watched: bool,
}

impl Visit for Sample {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "stage" { self.stage = value.to_owned(); }
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "elapsed_us" => self.elapsed_us = Some(value),
            "successful" => self.successful = value,
            "failed" => self.failed = value,
            _ => {}
        }
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        match field.name() {
            "skipped" => self.skipped = value,
            "root_watched" => self.root_watched = value,
            _ => {}
        }
    }
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

struct Measurement(Arc<Mutex<Vec<Sample>>>);
impl<S: Subscriber> Layer<S> for Measurement {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        if event.metadata().target() == "diff_sync::attach" {
            let mut sample = Sample::default();
            event.record(&mut sample);
            self.0.lock().unwrap().push(sample);
        }
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let output = mast_harness::git_command().args(args).current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "test").env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test").env("GIT_COMMITTER_EMAIL", "test@test")
        .output().unwrap();
    assert!(output.status.success(), "git {args:?} failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual timing measurement"]
async fn measure_attach_for_fifty_checkouts() {
    let samples = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Measurement(samples.clone()))).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let core = EngineCore::assemble(&data, Arc::new(HarnessRegistry::new()), mast_proto::HarnessId::Mock, None).unwrap();
    let mut repos = Vec::new();
    for i in 0..50 {
        let repo = tmp.path().join(format!("repo{i}"));
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "initial"]);
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        repos.push(repo);
    }
    let start = Instant::now();
    for (i, repo) in repos.iter().enumerate() {
        core.workspace.create_space(&format!("space-{i}"), &core.device_id, &repo.to_string_lossy(), None, true).unwrap();
        core.workspace.create_chat(&format!("chat-{i}"), Some(&format!("space-{i}")), None, None, None).unwrap();
    }
    core.diff_sync.reconcile_now().await;
    let deadline = Instant::now() + Duration::from_secs(300);
    while core.diff_sync.watch_diffs().borrow().len() != 50 || !core.diff_sync.is_settled() {
        assert!(Instant::now() < deadline, "attachment did not settle");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let total = start.elapsed();
    core.shutdown().await;
    let samples = samples.lock().unwrap();
    let mut stages: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for sample in samples.iter() {
        if let Some(elapsed) = sample.elapsed_us { stages.entry(&sample.stage).or_default().push(elapsed); }
    }
    for (stage, values) in &mut stages {
        values.sort_unstable();
        let p50 = values[(values.len() - 1) * 50 / 100];
        let p95 = values[(values.len() - 1) * 95 / 100];
        println!("attach {stage}: count={} p50_us={p50} p95_us={p95} sum_us={}", values.len(), values.iter().sum::<u64>());
    }
    let coverage: Vec<_> = samples.iter().filter(|s| s.stage == "coverage").collect();
    println!("attach total_ms={} checkouts={} root_watched={} successful={} failed={} budget_skips={}", total.as_millis(), coverage.len(), coverage.iter().filter(|s| s.root_watched).count(), coverage.iter().map(|s| s.successful).sum::<u64>(), coverage.iter().map(|s| s.failed).sum::<u64>(), samples.iter().filter(|s| s.stage == "budget" && s.skipped).count());
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
