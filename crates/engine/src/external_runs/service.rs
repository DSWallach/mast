//! Runs the poller on the Space's owning device: reads the registry, drives
//! the state machine, and turns reserved transitions into orchestrator wakes.
//! It reads the registry and never writes it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use mast_proto::{ExternalRunsObserved, ExternalRunSettings, WorkOutlineSnapshot};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use super::gh::GhRunner;
use super::poller::{
    Counters, PollerCore, RunInfo, SpaceInput, TickInput, WakeCandidate, WakeReason,
};
use crate::orchestrator_wakes::OrchestratorWakes;
use crate::workspace_host::WorkspaceHost;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// What the wake text needs about a run, read with the tick's input.
#[derive(Debug, Clone, Default)]
struct RunMeta {
    title: String,
    item_title: String,
    owner_chat_id: Option<String>,
    item_chats: Vec<String>,
}

struct Gathered {
    input: TickInput,
    meta: HashMap<String, RunMeta>,
    signature: u64,
}

pub(crate) fn gather_space(
    snapshot: &WorkOutlineSnapshot,
    space_id: &str,
    settings: ExternalRunSettings,
    now_ms: i64,
) -> (SpaceInput, HashMap<String, RunMeta>) {
    let stale_ms = i64::from(settings.stale_days) * 24 * 3600 * 1000;
    let mut meta = HashMap::new();
    let mut runs = Vec::new();
    for run in &snapshot.external_runs {
        let item = snapshot.item(&run.work_item_id);
        let live = run.expected_alive
            && run.ended_at.is_none()
            && item.is_some_and(|item| item.archived_at.is_none())
            && now_ms - run.updated_at <= stale_ms;
        let open_actions = snapshot
            .user_action_requests
            .iter()
            .filter(|request| request.work_item_id == run.work_item_id && request.is_unresolved())
            .count();
        meta.insert(
            run.id.clone(),
            RunMeta {
                title: run
                    .display_name
                    .clone()
                    .or_else(|| run.codespace_name.clone())
                    .unwrap_or_else(|| run.run_tag.clone()),
                item_title: item.map(|item| item.title.clone()).unwrap_or_default(),
                owner_chat_id: run.owner_chat_id.clone(),
                item_chats: item.map(|item| item.chat_ids.clone()).unwrap_or_default(),
            },
        );
        runs.push(RunInfo {
            id: run.id.clone(),
            work_item_id: run.work_item_id.clone(),
            run_tag: run.run_tag.clone(),
            codespace_name: run.codespace_name.clone(),
            owner_chat_id: run.owner_chat_id.clone(),
            live,
            ended_at: run.ended_at.map(|ms| ms.div_euclid(1000)),
            open_actions,
        });
    }
    (SpaceInput { space_id: space_id.to_owned(), settings, runs }, meta)
}

fn signature(input: &TickInput) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for space in &input.spaces {
        space.space_id.hash(&mut hasher);
        space.settings.capacity_cap.hash(&mut hasher);
        space.settings.run_dir.hash(&mut hasher);
        space.settings.exit_file.hash(&mut hasher);
        space.settings.output_file.hash(&mut hasher);
        space.settings.tmux_session.hash(&mut hasher);
        for run in space.runs.iter().filter(|run| run.live) {
            (&run.id, &run.codespace_name, run.open_actions).hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn wake_text(candidate: &WakeCandidate, meta: &RunMeta) -> String {
    let run = if meta.item_title.is_empty() {
        format!("External run {}", meta.title)
    } else {
        format!("External run {} (item \"{}\")", meta.title, meta.item_title)
    };
    match &candidate.reason {
        WakeReason::Done => format!("{run} finished: exit 0."),
        WakeReason::Failed(code) => format!("{run} failed: exit {code}."),
        WakeReason::Orphaned => format!(
            "{run} is orphaned: no exit was seen and the box or its tmux session is gone. \
             The poller does not start boxes; check it by hand."
        ),
        WakeReason::UnknownHeld => format!(
            "{run} could not be observed for 15 minutes (see work_item_external_run_list)."
        ),
        WakeReason::CapacityBreach { running, cap } => {
            format!("Codespace capacity reached: {running}/{cap} running ({run} is one of the live runs).")
        }
    }
}

struct Inner {
    core: Mutex<PollerCore>,
    meta: Mutex<HashMap<String, RunMeta>>,
    last_input: Mutex<TickInput>,
    workspace: WorkspaceHost,
    wakes: OrchestratorWakes,
    nudge: Notify,
    revision: watch::Sender<u64>,
}

#[derive(Clone)]
pub struct ExternalRuns {
    inner: Arc<Inner>,
}

impl ExternalRuns {
    pub fn new(
        runner: Arc<dyn GhRunner>,
        path: PathBuf,
        workspace: WorkspaceHost,
        wakes: OrchestratorWakes,
    ) -> Self {
        let jitter: Box<dyn FnMut() -> f64 + Send> =
            Box::new(|| uuid::Uuid::new_v4().as_u128() as f64 / u128::MAX as f64 * 2.0 - 1.0);
        let clock: super::poller::Clock = Arc::new(now_ms);
        Self::with_core(PollerCore::new(runner, clock, path, jitter), workspace, wakes)
    }

    fn with_core(core: PollerCore, workspace: WorkspaceHost, wakes: OrchestratorWakes) -> Self {
        Self {
            inner: Arc::new(Inner {
                core: Mutex::new(core),
                meta: Mutex::new(HashMap::new()),
                last_input: Mutex::new(TickInput::default()),
                workspace,
                wakes,
                nudge: Notify::new(),
                revision: watch::channel(0).0,
            }),
        }
    }

    pub fn watch_revision(&self) -> watch::Receiver<u64> {
        self.inner.revision.subscribe()
    }

    pub fn counters(&self) -> Counters {
        lock(&self.inner.core).counters.clone()
    }

    fn bump(&self) {
        self.inner.revision.send_modify(|revision| *revision += 1);
    }

    fn gather(&self, now_ms: i64) -> Gathered {
        let mut input = TickInput::default();
        let mut meta = HashMap::new();
        let Ok(spaces) = self.inner.workspace.read_spaces() else {
            return Gathered { input, meta, signature: 0 };
        };
        for space in spaces {
            if space.device_id != self.inner.workspace.device_id() {
                continue;
            }
            let inputs = self.inner.workspace.work_outline_inputs(&space.id);
            if inputs.runs.is_empty() {
                continue;
            }
            let snapshot = mast_doc::materialize_work_outline(&space.id, &inputs);
            let settings = snapshot.space_settings.external_runs.clone();
            let (space_input, space_meta) = gather_space(&snapshot, &space.id, settings, now_ms);
            meta.extend(space_meta);
            input.spaces.push(space_input);
        }
        let signature = signature(&input);
        Gathered { input, meta, signature }
    }

    /// The observed view of one Space. A device that does not own the Space has
    /// the durable record only, so it answers `hosted: false`.
    pub fn observed(&self, space_id: &str) -> ExternalRunsObserved {
        let hosted = self
            .inner
            .workspace
            .space(space_id)
            .ok()
            .flatten()
            .is_some_and(|space| space.device_id == self.inner.workspace.device_id());
        if !hosted {
            return ExternalRunsObserved::default();
        }
        let now = now_ms();
        let space_input = {
            let input = lock(&self.inner.last_input);
            input.spaces.iter().find(|space| space.space_id == space_id).cloned()
        };
        let space_input = space_input.unwrap_or_else(|| {
            let inputs = self.inner.workspace.work_outline_inputs(space_id);
            let snapshot = mast_doc::materialize_work_outline(space_id, &inputs);
            let settings = snapshot.space_settings.external_runs.clone();
            gather_space(&snapshot, space_id, settings, now).0
        });
        let (view, stale) = {
            let core = lock(&self.inner.core);
            (core.view(&space_input), core.capacity_is_stale(now))
        };
        if stale {
            self.inner.nudge.notify_one();
        }
        view
    }

    async fn deliver(&self, wakes: Vec<WakeCandidate>) {
        for candidate in wakes {
            let meta = lock(&self.inner.meta).get(&candidate.run_id).cloned().unwrap_or_default();
            let Some(target) = self
                .inner
                .wakes
                .external_run_target(meta.owner_chat_id.as_deref(), &meta.item_chats)
            else {
                continue;
            };
            let text = wake_text(&candidate, &meta);
            self.inner.wakes.note_external_run_changed(&target, &candidate.key, &text).await;
        }
    }

    fn markers(&self) -> Vec<(String, (u64, u64, bool))> {
        let device = self.inner.workspace.device_id().to_owned();
        let Ok(spaces) = self.inner.workspace.read_spaces() else { return Vec::new() };
        spaces
            .into_iter()
            .filter(|space| space.device_id == device)
            .map(|space| {
                let marker = self.inner.workspace.work_outline_inputs_marker(&space.id);
                (space.id, marker)
            })
            .collect()
    }

    /// One pass: when the registry moved or a tick is due, refresh the input,
    /// tick if due, and deliver what was reserved.
    async fn step(
        &self,
        last_signature: &mut u64,
        last_markers: &mut Vec<(String, (u64, u64, bool))>,
        force_capacity: bool,
    ) {
        let now = now_ms();
        let markers = self.markers();
        let due = lock(&self.inner.core).due(now);
        if markers == *last_markers && !due && !force_capacity {
            return;
        }
        *last_markers = markers;
        let this = self.clone();
        let Ok(gathered) = tokio::task::spawn_blocking(move || this.gather(now)).await else {
            return;
        };
        *lock(&self.inner.meta) = gathered.meta;
        *lock(&self.inner.last_input) = gathered.input.clone();
        let has_live = gathered.input.has_live();
        let changed = gathered.signature != *last_signature;
        *last_signature = gathered.signature;
        let this = self.clone();
        let input = gathered.input;
        let (wakes, observed) = tokio::task::spawn_blocking(move || {
            let mut core = lock(&this.inner.core);
            if changed {
                core.registry_changed(now, has_live);
            }
            if core.due(now) {
                (core.tick(now, &input), true)
            } else if force_capacity && !has_live {
                core.refresh_capacity(now);
                (Vec::new(), true)
            } else {
                (Vec::new(), false)
            }
        })
        .await
        .unwrap_or_default();
        if observed {
            self.bump();
        }
        self.deliver(wakes).await;
    }

    fn wait_for(&self, now: i64) -> Option<Duration> {
        lock(&self.inner.core)
            .next_due_ms()
            .map(|due| Duration::from_millis((due - now).max(0) as u64))
    }

    pub fn spawn(&self, cancel: CancellationToken) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut changes = this.inner.workspace.watch_changes();
            let mut last_signature = u64::MAX;
            let mut last_markers = Vec::new();
            let mut force_capacity = false;
            loop {
                this.step(&mut last_signature, &mut last_markers, force_capacity).await;
                force_capacity = false;
                let sleep = async {
                    match this.wait_for(now_ms()) {
                        Some(wait) => tokio::time::sleep(wait).await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    changed = changes.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        changes.borrow_and_update();
                    }
                    _ = this.inner.nudge.notified() => force_capacity = true,
                    _ = sleep => {}
                }
            }
        });
    }

    #[cfg(test)]
    pub(crate) fn tick_now(&self, now_ms: i64) -> Vec<WakeCandidate> {
        let gathered = self.gather(now_ms);
        *lock(&self.inner.meta) = gathered.meta;
        let mut core = lock(&self.inner.core);
        core.registry_changed(now_ms, gathered.input.has_live());
        core.tick(now_ms, &gathered.input)
    }

    #[cfg(test)]
    pub(crate) async fn deliver_for_test(&self, wakes: Vec<WakeCandidate>) {
        self.deliver(wakes).await;
    }
}
