//! Main-thread freeze watchdog (post-mortem 2026-11-16, P2).
//!
//! A background thread pings the main thread every [`Config::poll`]. The main
//! thread answers from a task parked on [`Heartbeat::serve`], which stores a
//! tick. When the tick is older than [`Config::threshold`] the watchdog records
//! one freeze: a `warn`, one `/usr/bin/sample` capture of this process written
//! to `freeze-<timestamp>.txt` in the logs dir (newest [`Config::max_files`]
//! kept), and a second log line with the total stall once the thread recovers.
//!
//! The ping is sent from the watchdog thread, so the main thread runs no timer
//! and does no work beyond one wake, one atomic swap and one atomic store per
//! poll interval. Nothing here allocates on the main thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::Poll;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures::task::AtomicWaker;

#[derive(Clone, Debug)]
pub struct Config {
    /// Main thread silent for at least this long is a freeze.
    pub threshold: Duration,
    /// Watchdog wake interval.
    pub poll: Duration,
    /// At most one capture per this window.
    pub min_capture_gap: Duration,
    /// Capture files kept in the logs dir.
    pub max_files: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold: Duration::from_secs(2),
            poll: Duration::from_millis(500),
            min_capture_gap: Duration::from_secs(60),
            max_files: 10,
        }
    }
}

/// Shared between the main thread (answers pings) and the watchdog.
pub struct Heartbeat {
    epoch: Instant,
    tick_ms: AtomicU64,
    ping: AtomicBool,
    waker: AtomicWaker,
}

impl Heartbeat {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: Instant::now(),
            tick_ms: AtomicU64::new(0),
            ping: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        })
    }

    /// Milliseconds since this heartbeat was created (monotonic).
    pub fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Main thread: record liveness. One clock read and one atomic store.
    pub fn beat(&self) {
        self.tick_ms.store(self.now_ms(), Ordering::Release);
    }

    pub fn last_tick_ms(&self) -> u64 {
        self.tick_ms.load(Ordering::Acquire)
    }

    /// Watchdog: ask the main thread to tick.
    pub fn ping(&self) {
        self.ping.store(true, Ordering::Release);
        self.waker.wake();
    }

    /// Main thread: consume a pending ping and tick.
    pub fn answer_ping(&self) {
        if self.ping.swap(false, Ordering::AcqRel) {
            self.beat();
        }
    }

    /// Never completes. Spawn on the main-thread executor: each ping wakes it,
    /// it ticks once and parks again.
    pub async fn serve(self: Arc<Self>) {
        self.beat();
        std::future::poll_fn(|cx| {
            self.waker.register(cx.waker());
            self.answer_ping();
            Poll::<()>::Pending
        })
        .await
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Idle,
    /// Main thread just crossed the threshold. `capture` is false when the
    /// 60 s rate limit suppressed it.
    Frozen { stalled_ms: u64, capture: bool },
    /// Main thread ticked again; `total_ms` is the whole stall.
    Recovered { total_ms: u64 },
}

/// Pure detector: clock and tick values in, decisions out.
pub struct Detector {
    threshold_ms: u64,
    gap_ms: u64,
    /// Observation gaps this long mean the watchdog itself was suspended
    /// (process stopped, debugger), so stale ticks prove nothing.
    suspend_ms: u64,
    /// Tick value at which the current freeze began.
    frozen_at_tick: Option<u64>,
    last_capture_ms: Option<u64>,
    last_observe_ms: Option<u64>,
}

impl Detector {
    pub fn new(config: &Config) -> Self {
        let poll_ms = (config.poll.as_millis() as u64).max(1);
        Self {
            threshold_ms: config.threshold.as_millis() as u64,
            gap_ms: config.min_capture_gap.as_millis() as u64,
            suspend_ms: poll_ms.saturating_mul(4).max(config.threshold.as_millis() as u64 + poll_ms),
            frozen_at_tick: None,
            last_capture_ms: None,
            last_observe_ms: None,
        }
    }

    pub fn observe(&mut self, now_ms: u64, last_tick_ms: u64) -> Decision {
        let suspended = self
            .last_observe_ms
            .is_some_and(|prev| now_ms.saturating_sub(prev) > self.suspend_ms);
        self.last_observe_ms = Some(now_ms);
        if let Some(frozen_tick) = self.frozen_at_tick {
            if last_tick_ms != frozen_tick {
                self.frozen_at_tick = None;
                return Decision::Recovered { total_ms: last_tick_ms.saturating_sub(frozen_tick) };
            }
            return Decision::Idle;
        }
        let stalled = now_ms.saturating_sub(last_tick_ms);
        if suspended || stalled < self.threshold_ms {
            return Decision::Idle;
        }
        self.frozen_at_tick = Some(last_tick_ms);
        let capture = self
            .last_capture_ms
            .is_none_or(|prev| now_ms.saturating_sub(prev) >= self.gap_ms);
        if capture {
            self.last_capture_ms = Some(now_ms);
        }
        Decision::Frozen { stalled_ms: stalled, capture }
    }
}

/// Captures the main thread's stack to `path`. Injectable for tests.
pub trait Capturer: Send {
    fn capture(&mut self, pid: u32, path: &Path);
}

/// `/usr/bin/sample <pid> 1 -file <path>` (needs no permission for our own
/// process). Blocks the watchdog thread for about a second; never the main one.
pub struct SampleCapturer;

impl Capturer for SampleCapturer {
    fn capture(&mut self, pid: u32, path: &Path) {
        let result = mast_harness::command("/usr/bin/sample")
            .arg(pid.to_string())
            .arg("1")
            .arg("-file")
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match result {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::warn!(%status, "freeze watchdog: sample exited unsuccessfully"),
            Err(error) => tracing::warn!(%error, "freeze watchdog: could not run sample"),
        }
    }
}

const FILE_PREFIX: &str = "freeze-";
const FILE_SUFFIX: &str = ".txt";

/// `freeze-YYYYMMDD-HHMMSS.txt` (local time; sorts chronologically).
pub fn capture_path(dir: &Path) -> PathBuf {
    dir.join(format!("{FILE_PREFIX}{}{FILE_SUFFIX}", chrono::Local::now().format("%Y%m%d-%H%M%S")))
}

/// Delete all but the newest `keep` capture files (by name).
pub fn prune_captures(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut names: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(FILE_PREFIX) && name.ends_with(FILE_SUFFIX))
        .collect();
    names.sort();
    let excess = names.len().saturating_sub(keep);
    for name in &names[..excess] {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

pub struct WatchdogHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WatchdogHandle {
    /// Stop the thread and wait for it (tests; the app just exits).
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

/// Start the watchdog thread. `heartbeat` must have a main-thread consumer
/// ([`Heartbeat::serve`]) or the first poll will report a freeze.
pub fn start(
    config: Config,
    heartbeat: Arc<Heartbeat>,
    dir: PathBuf,
    mut capturer: Box<dyn Capturer>,
) -> std::io::Result<WatchdogHandle> {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread = std::thread::Builder::new().name("freeze-watchdog".into()).spawn(move || {
        let pid = std::process::id();
        let mut detector = Detector::new(&config);
        while !thread_stop.load(Ordering::Acquire) {
            heartbeat.ping();
            std::thread::park_timeout(config.poll);
            if thread_stop.load(Ordering::Acquire) {
                break;
            }
            match detector.observe(heartbeat.now_ms(), heartbeat.last_tick_ms()) {
                Decision::Idle => {}
                Decision::Frozen { stalled_ms, capture } => {
                    tracing::warn!(stalled_ms, capture, "main thread frozen");
                    if capture {
                        let _ = std::fs::create_dir_all(&dir);
                        let path = capture_path(&dir);
                        capturer.capture(pid, &path);
                        prune_captures(&dir, config.max_files);
                        tracing::warn!(path = %path.display(), "freeze stack captured");
                    }
                }
                Decision::Recovered { total_ms } => {
                    tracing::warn!(total_ms, "main thread recovered from freeze");
                }
            }
        }
    })?;
    Ok(WatchdogHandle { stop, thread: Some(thread) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn detector() -> Detector {
        Detector::new(&Config::default())
    }

    #[test]
    fn healthy_ticks_stay_idle() {
        let mut d = detector();
        for i in 1..=20u64 {
            assert_eq!(d.observe(i * 500, i * 500 - 100), Decision::Idle);
        }
    }

    #[test]
    fn detects_at_threshold_once() {
        let mut d = detector();
        assert_eq!(d.observe(500, 100), Decision::Idle);
        assert_eq!(d.observe(1_000, 100), Decision::Idle);
        assert_eq!(d.observe(1_500, 100), Decision::Idle);
        assert_eq!(d.observe(2_000, 100), Decision::Idle);
        assert_eq!(d.observe(2_500, 100), Decision::Frozen { stalled_ms: 2_400, capture: true });
        // Still frozen: no second event.
        assert_eq!(d.observe(3_000, 100), Decision::Idle);
        assert_eq!(d.observe(3_500, 100), Decision::Idle);
    }

    #[test]
    fn recovery_reports_total_stall() {
        let mut d = detector();
        d.observe(500, 100);
        d.observe(1_000, 100);
        d.observe(1_500, 100);
        d.observe(2_000, 100);
        assert!(matches!(d.observe(2_500, 100), Decision::Frozen { .. }));
        assert_eq!(d.observe(3_000, 100), Decision::Idle);
        assert_eq!(d.observe(3_500, 3_400), Decision::Recovered { total_ms: 3_300 });
        assert_eq!(d.observe(4_000, 3_900), Decision::Idle);
    }

    fn freeze_at(d: &mut Detector, start: u64) -> Decision {
        // Ticks stop at `start`; walk the clock until a freeze is reported.
        let mut now = start + 500;
        loop {
            match d.observe(now, start) {
                Decision::Idle => now += 500,
                other => return other,
            }
        }
    }

    #[test]
    fn rate_limits_captures_to_one_per_minute() {
        let mut d = detector();
        d.observe(500, 400);
        assert_eq!(freeze_at(&mut d, 400), Decision::Frozen { stalled_ms: 2_000, capture: true });
        // Recover, then a second freeze 10 s later: logged but not captured.
        assert!(matches!(d.observe(5_000, 4_900), Decision::Recovered { .. }));
        d.observe(5_500, 5_400);
        assert_eq!(freeze_at(&mut d, 5_400), Decision::Frozen { stalled_ms: 2_000, capture: false });
        // A freeze after the window captures again.
        assert!(matches!(d.observe(9_000, 8_900), Decision::Recovered { .. }));
        d.observe(70_000, 69_900);
        assert_eq!(freeze_at(&mut d, 69_900), Decision::Frozen { stalled_ms: 2_000, capture: true });
    }

    #[test]
    fn watchdog_suspension_is_not_a_freeze() {
        let mut d = detector();
        d.observe(500, 400);
        // Next observation 30 s later with a stale tick: we were suspended.
        assert_eq!(d.observe(30_500, 400), Decision::Idle);
        // A genuine stall after that is still detected.
        assert_eq!(d.observe(31_000, 400), Decision::Frozen { stalled_ms: 30_600, capture: true });
    }

    #[test]
    fn prune_keeps_newest_files_only() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..14 {
            std::fs::write(dir.path().join(format!("freeze-20261117-1200{i:02}.txt")), b"x").unwrap();
        }
        std::fs::write(dir.path().join("mast-headed.log"), b"keep").unwrap();
        prune_captures(dir.path(), 10);
        let mut left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left.len(), 11);
        assert!(left.contains(&"mast-headed.log".to_string()));
        assert!(!left.contains(&"freeze-20261117-120003.txt".to_string()));
        assert!(left.contains(&"freeze-20261117-120004.txt".to_string()));
        assert!(left.contains(&"freeze-20261117-120013.txt".to_string()));
    }

    #[test]
    fn capture_path_shape_sorts_by_time() {
        let path = capture_path(Path::new("/logs"));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("freeze-") && name.ends_with(".txt"));
        assert_eq!(name.len(), "freeze-20261117-120000.txt".len());
    }

    struct Recording(Arc<Mutex<Vec<PathBuf>>>);
    impl Capturer for Recording {
        fn capture(&mut self, _pid: u32, path: &Path) {
            std::fs::write(path, b"fake sample").unwrap();
            self.0.lock().unwrap().push(path.to_path_buf());
        }
    }

    /// A fake main thread that answers pings, then stalls, then answers again.
    #[test]
    fn stalled_fake_main_thread_triggers_exactly_one_capture() {
        let config = Config {
            threshold: Duration::from_millis(200),
            poll: Duration::from_millis(25),
            min_capture_gap: Duration::from_secs(60),
            max_files: 10,
        };
        let heartbeat = Heartbeat::new();
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));

        let main_hb = heartbeat.clone();
        let main_stop = Arc::new(AtomicBool::new(false));
        let fake_stop = main_stop.clone();
        let fake_main = std::thread::spawn(move || {
            let run_for = |ms: u64, hb: &Heartbeat| {
                let until = Instant::now() + Duration::from_millis(ms);
                while Instant::now() < until && !fake_stop.load(Ordering::Acquire) {
                    hb.answer_ping();
                    std::thread::sleep(Duration::from_millis(2));
                }
            };
            main_hb.beat();
            run_for(300, &main_hb);
            std::thread::sleep(Duration::from_millis(700)); // the freeze
            run_for(600, &main_hb);
        });

        let handle = start(config, heartbeat.clone(), dir.path().to_path_buf(), Box::new(Recording(calls.clone()))).unwrap();
        fake_main.join().unwrap();
        handle.stop();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "one capture per freeze: {calls:?}");
        assert!(calls[0].starts_with(dir.path()));
        assert!(calls[0].exists());
    }

    #[test]
    fn healthy_fake_main_thread_never_captures() {
        let config = Config {
            threshold: Duration::from_millis(200),
            poll: Duration::from_millis(25),
            ..Config::default()
        };
        let heartbeat = Heartbeat::new();
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let main_hb = heartbeat.clone();
        let done = Arc::new(AtomicBool::new(false));
        let fake_done = done.clone();
        let fake_main = std::thread::spawn(move || {
            main_hb.beat();
            while !fake_done.load(Ordering::Acquire) {
                main_hb.answer_ping();
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let handle = start(config, heartbeat, dir.path().to_path_buf(), Box::new(Recording(calls.clone()))).unwrap();
        std::thread::sleep(Duration::from_millis(700));
        done.store(true, Ordering::Release);
        fake_main.join().unwrap();
        handle.stop();
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn serve_ticks_on_each_ping_without_polling_timers() {
        let heartbeat = Heartbeat::new();
        let mut serve = Box::pin(heartbeat.clone().serve());
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        assert!(serve.as_mut().poll(&mut cx).is_pending());
        std::thread::sleep(Duration::from_millis(5));
        let before = heartbeat.last_tick_ms();
        heartbeat.ping();
        assert!(serve.as_mut().poll(&mut cx).is_pending());
        assert!(heartbeat.last_tick_ms() > before);
    }
}
