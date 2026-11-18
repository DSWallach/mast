//! Native scheduled-task management. All mutations go through the owning engine.
use crate::{composer::ComposerInput, popover, state::AppState, theme::Theme};
use futures::StreamExt as _;
use gpui::{
    AnyElement, Context, Entity, EventEmitter, Render, SharedString, Subscription, Task, Window,
    div, prelude::*, px,
};
use mast_engine::registry::HarnessDescriptor;
use mast_proto::*;
use mast_rpc::methods;
use std::collections::HashMap;

pub enum TaskPageEvent {
    OpenChat(String),
}
pub struct TaskEditor {
    id: String,
    revision: u64,
    draft: ScheduledTaskDraft,
    name: Entity<ComposerInput>,
    prompt: Entity<ComposerInput>,
    cwd: Entity<ComposerInput>,
    cron: Entity<ComposerInput>,
    timezone: Entity<ComposerInput>,
    grace: Entity<ComposerInput>,
    once: Entity<ComposerInput>,
    subscriptions: Vec<Subscription>,
}
pub struct ScheduledTasksPage {
    state: Entity<AppState>,
    snapshot: ScheduledTasksSnapshot,
    cleanup: Option<ScheduledRunCleanup>,
    target: Option<String>,
    connection: Option<crate::state::EngineHandle>,
    models: Vec<Model>,
    harnesses: Vec<HarnessDescriptor>,
    catalogs: HashMap<HarnessId, Vec<Model>>,
    catalog_harness: HarnessId,
    catalog_task: Option<Task<()>>,
    catalog_generation: u64,
    catalog_loading: bool,
    catalog_error: Option<String>,
    editor: Option<TaskEditor>,
    requested_task: Option<String>,
    error: Option<String>,
    message: Option<String>,
    busy: bool,
    active: bool,
    choices: Option<&'static str>,
    task: Option<Task<()>>,
    poll: Option<Task<()>>,
    _observe: Subscription,
}
impl EventEmitter<TaskPageEvent> for ScheduledTasksPage {}
impl ScheduledTasksPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let target = state.read(cx).effective_device_id();
        let connection = state.read(cx).engine().cloned();
        let observe = cx.observe(&state, |this: &mut Self, state, cx| {
            let next = state.read(cx).engine().cloned();
            let changed = match (&this.connection, &next) {
                (Some(a), Some(b)) => !a.same_connection(b),
                (None, None) => false,
                _ => true,
            };
            if changed {
                this.task = None;
                this.catalog_task = None;
                this.catalog_generation += 1;
                this.catalog_loading = false;
                this.catalog_error = None;
                this.busy = false;
                this.snapshot = Default::default();
                this.cleanup = None;
                this.editor = None;
                this.requested_task = None;
                this.models.clear();
                this.catalogs.clear();
                this.harnesses.clear();
                this.target = state.read(cx).effective_device_id();
                this.connection = next;
                this.refresh(cx);
            }
            cx.notify();
        });
        let mut page = Self {
            state,
            snapshot: Default::default(),
            cleanup: None,
            target,
            connection,
            models: vec![],
            harnesses: vec![],
            catalogs: HashMap::new(),
            catalog_harness: HarnessId::ClaudeCode,
            catalog_task: None,
            catalog_generation: 0,
            catalog_loading: false,
            catalog_error: None,
            editor: None,
            requested_task: None,
            error: None,
            message: None,
            busy: false,
            active: true,
            choices: None,
            task: None,
            poll: None,
            _observe: observe,
        };
        page.refresh(cx);
        page.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(10))
                    .await;
                if this
                    .update(cx, |p, cx| {
                        if p.active
                            && !p.busy
                            && p.editor.is_none()
                            && p.snapshot.candidates.is_empty()
                        {
                            p.load(cx)
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        page
    }
    /// Offline new-task editor for the preview renderer.
    pub(crate) fn offline_editor_fixture(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut page = Self::new(state, cx);
        page.target = Some("local".into());
        page.edit(None, cx);
        page.error = None;
        page
    }
    pub fn show_task(&mut self, id: String, device: Option<String>, cx: &mut Context<Self>) {
        if let Some(device) = device {
            if self.target.as_ref() != Some(&device) {
                self.task = None;
                self.busy = false;
                self.target = Some(device);
                self.snapshot = Default::default();
                self.models.clear();
                self.catalogs.clear();
                self.harnesses.clear();
            }
        }
        self.cleanup = None;
        self.requested_task = Some(id);
        self.editor = None;
        self.open_requested_task(cx);
        self.refresh(cx);
        cx.notify();
    }
    pub fn show_tasks(&mut self, cx: &mut Context<Self>) {
        self.cleanup = None;
        self.editor = None;
        self.requested_task = None;
        self.choices = None;
        self.snapshot.preview.clear();
        self.refresh(cx);
        cx.notify();
    }

    fn requested_task_definition(&self) -> Option<&ScheduledTask> {
        let id = self.requested_task.as_ref()?;
        self.snapshot
            .tasks
            .iter()
            .find(|t| &t.id == id && Some(&t.device_id) == self.target.as_ref() && !t.deleted)
    }

    fn open_requested_task(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self.requested_task_definition().cloned() {
            self.edit(Some(task), cx);
        }
    }
    pub(crate) fn set_active(&mut self, active: bool) {
        self.active = active;
        if !active {
            self.cleanup = None;
        }
    }
    fn load(&mut self, cx: &mut Context<Self>) {
        self.call(ScheduledTasksRequest::List, cx);
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
        self.load_catalog(cx);
    }
    fn output_supported(&self, cx: &gpui::App) -> bool {
        self.target.as_deref().is_some_and(|device| {
            self.state
                .read(cx)
                .device_supports(device, capabilities::SCHEDULED_TASK_OUTPUT_V1)
        })
    }

    fn after_success_supported(&self, cx: &gpui::App) -> bool {
        self.target.as_deref().is_some_and(|device| {
            self.state
                .read(cx)
                .device_supports(device, capabilities::SCHEDULED_TASK_AFTER_SUCCESS_V1)
        })
    }

    fn notion_writes_supported(&self, cx: &gpui::App) -> bool {
        self.target.as_deref().is_some_and(|device| {
            self.state
                .read(cx)
                .device_supports(device, capabilities::SCHEDULED_TASK_NOTION_WRITES_V1)
        })
    }

    fn call(&mut self, request: ScheduledTasksRequest, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let ScheduledTasksRequest::Save { draft, .. } = &request
            && draft.output == ScheduledTaskOutput::Background
        {
            let error = if !self.output_supported(cx) {
                Some("Update the execution device to save a background task")
            } else if draft.work_item_id.is_none() {
                Some("Choose a work item for background output")
            } else {
                None
            };
            if let Some(error) = error {
                self.error = Some(error.into());
                cx.notify();
                return;
            }
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.error = Some("Engine disconnected".into());
            return;
        };
        self.busy = true;
        self.error = None;
        let target = self.target.clone();
        let is_import = matches!(request, ScheduledTasksRequest::Import { .. });
        let saved_id = match &request {
            ScheduledTasksRequest::Save { id, .. } => Some(id.clone()),
            _ => None,
        };
        let is_preview = matches!(request, ScheduledTasksRequest::Preview { .. });
        let is_import_preview = matches!(request, ScheduledTasksRequest::ImportPreview { .. });
        let is_cleanup = matches!(request, ScheduledTasksRequest::Cleanup { .. });
        let is_cleanup_apply = matches!(
            request,
            ScheduledTasksRequest::Cleanup { mode: ScheduledRunCleanupMode::Apply { .. } }
        );
        if is_cleanup {
            self.message = None;
        }
        let mut params = serde_json::to_value(request).unwrap();
        if saved_id.is_some() && let Some(draft) = params["draft"].as_object_mut() {
            // Send explicit defaults to hosts that understand them. Omission
            // means preserve the stored policy on an older client's save.
            if self.output_supported(cx) {
                draft.entry("output").or_insert(serde_json::json!(ScheduledTaskOutput::Interactive));
            }
            if self.after_success_supported(cx)
                && draft.get("output").and_then(|mode| mode.as_str()) == Some("background")
            {
                draft.entry("afterSuccess").or_insert(serde_json::json!(ScheduledTaskAfterSuccess::Archive));
            } else {
                draft.remove("afterSuccess");
            }
            // An older host would drop the field, so it is never sent to one.
            if !self.notion_writes_supported(cx) {
                draft.remove("writeToNotion");
            }
        }
        params["targetDeviceId"] = serde_json::json!(target);
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<ScheduledTasksSnapshot>(methods::SCHEDULED_TASKS, params)
                .await
                .map_err(|e| e.to_string());
            let _ = this.update(cx, |p, cx| {
                if p.target != target {
                    return;
                }
                if !p
                    .state
                    .read(cx)
                    .engine()
                    .is_some_and(|current| current.same_connection(&engine))
                {
                    return;
                }
                p.busy = false;
                match result {
                    Ok(snapshot) => {
                        if is_cleanup {
                            if let Some(cleanup) = snapshot.cleanup {
                                p.show_cleanup_result(cleanup, is_cleanup_apply);
                            }
                        } else if is_preview {
                            p.snapshot.preview = snapshot.preview;
                        } else if is_import_preview {
                            p.snapshot.candidates = snapshot.candidates;
                        } else {
                            let candidates = if is_import {
                                p.snapshot
                                    .candidates
                                    .iter()
                                    .filter(|c| {
                                        !snapshot.tasks.iter().any(|t| {
                                            t.import_source.as_deref() == Some(c.source.as_str())
                                        })
                                    })
                                    .cloned()
                                    .collect()
                            } else {
                                vec![]
                            };
                            p.snapshot = snapshot;
                            p.snapshot.candidates = candidates;
                            p.finish_task_load(saved_id.as_deref(), cx);
                        }
                    }
                    Err(e) => p.error = Some(e),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn show_cleanup_result(&mut self, cleanup: ScheduledRunCleanup, attempted_apply: bool) {
        if cleanup.applied {
            self.cleanup = None;
            let count = cleanup.chat_ids().count();
            self.message = Some(format!("Archived {count} {}.", run_label(count)));
        } else if self.active && self.editor.is_none() && self.requested_task.is_none() {
            self.cleanup = Some(cleanup);
            if attempted_apply {
                self.message = Some("Eligible runs changed. Review the updated preview.".into());
            }
        }
    }

    fn finish_task_load(&mut self, saved_id: Option<&str>, cx: &mut Context<Self>) {
        if let Some(id) = saved_id {
            if self.editor.as_ref().is_some_and(|e| e.id == id) {
                if let Some(task) = self.snapshot.tasks.iter().find(|t| t.id == id).cloned() {
                    self.edit(Some(task), cx);
                }
            }
            self.message = Some("Task saved. Model changes apply to future runs.".into());
        } else {
            self.open_requested_task(cx);
        }
    }
    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.catalog_generation += 1;
        let generation = self.catalog_generation;
        let target = self.target.clone();
        self.catalog_loading = true;
        self.catalog_error = None;
        self.catalog_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<Vec<HarnessDescriptor>>(
                    methods::LIST_HARNESSES,
                    serde_json::json!({"targetDeviceId":target}),
                )
                .await;
            let offered = match result {
                Ok(list) => crate::pickers::offered_harnesses(&list),
                Err(error) => {
                    let _ = this.update(cx, |p, cx| {
                        if p.catalog_generation == generation {
                            p.catalog_loading = false;
                            p.catalog_error = Some(format!("Could not load agents: {error}"));
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            let targets = this
                .update(cx, |p, cx| {
                    if p.catalog_generation != generation
                        || p.target != target
                        || !p
                            .state
                            .read(cx)
                            .engine()
                            .is_some_and(|e| e.same_connection(&engine))
                    {
                        return Vec::new();
                    }
                    p.harnesses = offered;
                    if p.editor.is_none() {
                        let session_harness = p
                            .state
                            .read(cx)
                            .selected_chat_row()
                            .filter(|c| Some(&c.device_id) == p.target.as_ref())
                            .and_then(|c| c.config.as_ref())
                            .map(|c| c.harness);
                        p.catalog_harness = session_harness
                            .filter(|h| p.harnesses.iter().any(|d| d.id == *h))
                            .or_else(|| p.harnesses.first().map(|d| d.id))
                            .unwrap_or(p.catalog_harness);
                        p.models = if p.harnesses.is_empty() {
                            Vec::new()
                        } else {
                            p.catalogs
                                .get(&p.catalog_harness)
                                .cloned()
                                .unwrap_or_default()
                        };
                    }
                    let mut ids: Vec<_> = p.harnesses.iter().map(|d| d.id).collect();
                    if let Some(e) = &p.editor {
                        if !ids.contains(&e.draft.config.harness) {
                            ids.push(e.draft.config.harness);
                        }
                    }
                    if ids.is_empty() {
                        p.catalog_error = Some(
                            "Enable an installed agent in Settings → Agents to choose its models."
                                .into(),
                        );
                    }
                    cx.notify();
                    ids
                })
                .unwrap_or_default();
            let mut probes = futures::stream::FuturesUnordered::new();
            for harness in targets {
                let engine = engine.clone();
                let target = target.clone();
                probes.push(async move {
                    let result = engine
                        .client()
                        .call_as::<Vec<Model>>(
                            methods::LIST_MODELS,
                            serde_json::json!({"harness":harness,"targetDeviceId":target}),
                        )
                        .await;
                    (harness, result)
                });
            }
            while let Some((harness, result)) = probes.next().await {
                let _ = this.update(cx, |p, cx| {
                    if p.catalog_generation != generation
                        || p.target != target
                        || !p
                            .state
                            .read(cx)
                            .engine()
                            .is_some_and(|e| e.same_connection(&engine))
                    {
                        return;
                    }
                    match result {
                        Ok(models) => {
                            let models = crate::pickers::normalize_model_rows(harness, models);
                            p.catalogs.insert(harness, models.clone());
                            if p.catalog_harness == harness {
                                p.models = models;
                            }
                        }
                        Err(error) => {
                            p.catalog_error =
                                Some(format!("Could not load {harness:?} models: {error}"))
                        }
                    }
                    cx.notify();
                });
            }
            let _ = this.update(cx, |p, cx| {
                if p.catalog_generation == generation {
                    p.catalog_loading = false;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn edit(&mut self, task: Option<ScheduledTask>, cx: &mut Context<Self>) {
        let (id, revision, draft) = if let Some(t) = task {
            (t.id, t.revision, t.draft)
        } else {
            let state = self.state.read(cx);
            let space = state
                .selected_space_row()
                .filter(|s| Some(&s.device_id) == self.target.as_ref());
            let cwd = space
                .map(|s| s.path.clone())
                .unwrap_or_else(|| std::env::var("HOME").unwrap_or_default());
            (
                uuid::Uuid::new_v4().to_string(),
                0,
                ScheduledTaskDraft {
                    name: String::new(),
                    prompt: String::new(),
                    cwd,
                    space_id: space.map(|s| s.id.clone()),
                    config: ChatConfig { approval_rules: true,
                        harness: self.catalog_harness,
                        model: self.models.first().map(|m| m.id.clone()),
                        reasoning: None,
                        model_options: Default::default(),
                        sandbox: SandboxLevel::WorkspaceWrite,
                        auto_approve: false,
                    },
                    schedule: TaskSchedule::Cron {
                        expression: "0 8 * * 1-5".into(),
                        timezone: "America/New_York".into(),
                    },
                    enabled: false,
                    grace_minutes: 360,
                    cutover_confirmed: false,
                    work_item_id: None,
                    output: if self.output_supported(cx) {
                        ScheduledTaskOutput::Background
                    } else {
                        ScheduledTaskOutput::Interactive
                    },
                    after_success: Default::default(),
                    write_to_notion: self.notion_writes_supported(cx).then_some(false),
                },
            )
        };
        if self.catalog_harness != draft.config.harness {
            self.catalog_harness = draft.config.harness;
            self.models = self
                .catalogs
                .get(&self.catalog_harness)
                .cloned()
                .unwrap_or_default();
        }
        let mut subscriptions = vec![];
        let mut input = |label: &str, text: String, cx: &mut Context<Self>| {
            let e = cx.new(|cx| {
                let input = ComposerInput::new(label, cx).with_text_metrics(13.0, 20.0);
                if label == "Instructions" {
                    input.with_auto_grow()
                } else {
                    input.with_single_line()
                }
            });
            e.update(cx, |i, cx| i.set_text(text, cx));
            subscriptions.push(cx.subscribe(&e, |_: &mut Self, _, _, cx| cx.notify()));
            e
        };
        let (expression, timezone, once) = match &draft.schedule {
            TaskSchedule::Cron {
                expression,
                timezone,
            } => (expression.clone(), timezone.clone(), String::new()),
            TaskSchedule::Once { at } => (
                String::new(),
                "America/New_York".into(),
                chrono::DateTime::from_timestamp_millis(*at)
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_default(),
            ),
        };
        self.editor = Some(TaskEditor {
            id,
            revision,
            name: input("Task name", draft.name.clone(), cx),
            prompt: input("Instructions", draft.prompt.clone(), cx),
            cwd: input("Absolute working directory", draft.cwd.clone(), cx),
            cron: input("Five-field cron", expression, cx),
            timezone: input("IANA timezone", timezone, cx),
            grace: input("Grace in minutes", draft.grace_minutes.to_string(), cx),
            once: input("One-time date: RFC 3339 with timezone", once, cx),
            draft,
            subscriptions,
        });
        self.requested_task = None;
        self.message = None;
        self.snapshot.preview.clear();
        self.choices = None;
        cx.notify();
    }
    fn editor_draft(&self, cx: &gpui::App) -> Result<ScheduledTaskDraft, String> {
        let e = self.editor.as_ref().ok_or("No task selected")?;
        let mut d = e.draft.clone();
        d.name = e.name.read(cx).text().trim().into();
        d.prompt = e.prompt.read(cx).text().into();
        d.cwd = e.cwd.read(cx).text().trim().into();
        d.grace_minutes = e
            .grace
            .read(cx)
            .text()
            .trim()
            .parse()
            .map_err(|_| "Grace must be a number of minutes")?;
        let once = e.once.read(cx).text().trim();
        d.schedule = if !once.is_empty() {
            TaskSchedule::Once {
                at: chrono::DateTime::parse_from_rfc3339(once)
                    .map_err(
                        |_| "Enter a one-time date with timezone, e.g. 2026-12-01T08:00:00-05:00",
                    )?
                    .timestamp_millis(),
            }
        } else {
            TaskSchedule::Cron {
                expression: e.cron.read(cx).text().trim().into(),
                timezone: e.timezone.read(cx).text().trim().into(),
            }
        };
        Ok(d)
    }
    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match self.editor_draft(cx) {
            Ok(draft) => {
                let e = self.editor.as_ref().unwrap();
                let req = ScheduledTasksRequest::Save {
                    id: e.id.clone(),
                    expected_revision: e.revision,
                    draft,
                    operation_id: uuid::Uuid::new_v4().to_string(),
                };
                self.call(req, cx);
            }
            Err(e) => {
                self.error = Some(e);
                cx.notify();
            }
        }
    }
    fn choice(&mut self, key: &'static str, value: String, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(e) = self.editor.as_mut() {
            match key {
                "harness" => {
                    let Ok(harness) = serde_json::from_value::<HarnessId>(serde_json::json!(value))
                    else {
                        return;
                    };
                    if e.draft.config.harness != harness {
                        self.catalog_harness = harness;
                        self.models = self.catalogs.get(&harness).cloned().unwrap_or_default();
                        e.draft.config.harness = harness;
                        e.draft.config.model = self.models.first().map(|m| m.id.clone());
                        e.draft.config.reasoning = None;
                        e.draft.config.model_options.clear();
                    }
                }
                "space" => {
                    if e.draft.space_id.as_deref() != Some(value.as_str()) {
                        e.draft.work_item_id = None;
                    }
                    e.draft.space_id = (!value.is_empty()).then_some(value.clone());
                    if let Some(space) = self.state.read(cx).spaces.iter().find(|s| s.id == value) {
                        let path = space.path.clone();
                        e.cwd.update(cx, |i, cx| i.set_text(path, cx));
                    }
                }
                "model" => {
                    e.draft.config.model = Some(value.clone());
                    if let Some(m) = self.models.iter().find(|m| m.id == value) {
                        if e.draft
                            .config
                            .reasoning
                            .is_some_and(|r| !m.reasoning_levels.contains(&r))
                        {
                            e.draft.config.reasoning = None;
                        }
                        e.draft.config.model_options.retain(|id, v| {
                            m.options.iter().any(|o| {
                                &o.id == id && o.choices.iter().any(|c| v.as_str() == Some(&c.id))
                            })
                        });
                    }
                }
                "thinking" => {
                    let toggle = self
                        .models
                        .iter()
                        .find(|m| Some(&m.id) == e.draft.config.model.as_ref())
                        .is_some_and(|m| {
                            m.reasoning_levels.is_empty()
                                && m.options.iter().any(|o| o.id == "thinking")
                        });
                    if toggle {
                        e.draft.config.reasoning = None;
                        if value == "null" {
                            e.draft.config.model_options.remove("thinking");
                        } else {
                            e.draft
                                .config
                                .model_options
                                .insert("thinking".into(), serde_json::json!(value));
                        }
                    } else {
                        e.draft.config.reasoning =
                            serde_json::from_value(serde_json::json!(value)).ok();
                    }
                }
                "output" => {
                    e.draft.output = match value.as_str() {
                        "background" => ScheduledTaskOutput::Background,
                        _ => ScheduledTaskOutput::Interactive,
                    };
                }
                "afterSuccess" => {
                    e.draft.after_success = match value.as_str() {
                        "keep" => ScheduledTaskAfterSuccess::Keep,
                        _ => ScheduledTaskAfterSuccess::Archive,
                    };
                }
                "workItem" => e.draft.work_item_id = (!value.is_empty()).then_some(value.clone()),
                "contextWindow" => {
                    e.draft
                        .config
                        .model_options
                        .insert(key.into(), serde_json::json!(value));
                }
                _ => {}
            }
        }
        self.choices = None;
        cx.notify();
    }
    fn render_cleanup(&self, cleanup: &ScheduledRunCleanup, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let count = cleanup.chat_ids().count();
        let mut panel = crate::components::scheduled_task_parts::cleanup_panel(theme)
            .id("scheduled-run-cleanup-preview")
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(
                if count == 0 { "No old runs to archive".to_owned() }
                else { format!("Archive {count} {}?", run_label(count)) }
            ));
        if count > 0 {
            panel = panel
                .child(div().text_color(theme.text_muted).child("You can restore archived runs from Archived sessions."))
                .child(
                    div()
                        .id("scheduled-run-cleanup-counts")
                        .max_h(px(220.0))
                        .overflow_y_scroll()
                        .children(cleanup.tasks.iter().map(|task| {
                            div().child(format!("{} · {} {}", task.task_name, task.count, run_label(task.count)))
                        })),
                );
        }
        let expected_chat_ids = cleanup.chat_ids().cloned().collect::<Vec<_>>();
        panel.child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.0))
                .child(
                    popover::btn_ghost(theme, if count == 0 { "Done" } else { "Cancel" }, "tasks-cleanup-cancel")
                        .id("tasks-cleanup-cancel")
                        .debug_selector(|| "tasks-cleanup-cancel".into())
                        .on_click(cx.listener(|p, _, _, cx| {
                            if !p.busy {
                                p.cleanup = None;
                                cx.notify();
                            }
                        })),
                )
                .when(count > 0, |el| {
                    el.child(
                        popover::btn_primary(theme, "Archive runs")
                            .id("tasks-cleanup-apply")
                            .debug_selector(|| "tasks-cleanup-apply".into())
                            .on_click(cx.listener(move |p, _, _, cx| {
                                p.call(ScheduledTasksRequest::Cleanup {
                                    mode: ScheduledRunCleanupMode::Apply { expected_chat_ids: expected_chat_ids.clone() },
                                }, cx);
                            })),
                    )
                }),
        )
    }

    fn render_task_history(
        &self,
        task_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut content = div()
            .id("task-run-history")
            .debug_selector(|| "task-run-history".into())
            .flex()
            .flex_col()
            .gap(px(8.0))
            .mt(px(16.0))
            .pt(px(16.0))
            .border_t_1()
            .border_color(theme.border)
            .child(div().text_size(px(16.0)).child("Run history"));
        let mut chat_by_id: HashMap<_, _> = self
            .state
            .read(cx)
            .chats
            .iter()
            .filter(|chat| {
                chat.scheduled_task_info
                    .as_ref()
                    .is_some_and(|info| info.task_id == task_id)
                    && Some(&chat.device_id) == self.target.as_ref()
            })
            .map(|chat| (chat.id.as_str(), chat))
            .collect();
        let mut rows: Vec<_> = self
            .snapshot
            .runs
            .iter()
            .filter(|run| run.task_id == task_id)
            .map(|run| {
                let chat = run.chat_id.as_deref().and_then(|id| chat_by_id.remove(id));
                let order = chat.map_or((run.created_at, run.id.as_str()), task_chat_order);
                (order, chat, Some(run))
            })
            .collect();
        rows.extend(chat_by_id.into_values().map(|chat| {
            (task_chat_order(chat), Some(chat), None)
        }));
        rows.sort_unstable_by(|a, b| b.0.cmp(&a.0));

        if rows.is_empty() {
            return content
                .child(div().text_color(theme.text_muted).child("No runs yet for this task."))
                .into_any_element();
        }
        let mut latest_chat = true;
        for ((created_at, _), chat, run) in rows {
            let date = chrono::DateTime::from_timestamp_millis(created_at)
                .map(|date| date.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_default();
            let title = chat.and_then(|chat| chat.title.as_deref())
                .or_else(|| run.map(|run| run.snapshot.name.as_str()))
                .unwrap_or("Task run");
            let mut label = format!("{date} · {title}");
            if let Some(run) = run {
                label.push_str(" · ");
                label.push_str(run_state_label(run.state));
            }
            if let Some(chat) = chat {
                if chat.archived {
                    label.push_str(" · Archived");
                }
                if std::mem::take(&mut latest_chat) {
                    label.push_str(" · Latest");
                }
                let id = chat.id.clone();
                let selector_id = id.clone();
                content = content.child(
                    popover::btn_ghost(theme, &label, format!("history-{id}"))
                        .id(SharedString::from(format!("history-{id}")))
                        .debug_selector(move || format!("task-run-{selector_id}").into())
                        .on_click(cx.listener(move |p, _, _, cx| p.open_chat(id.clone(), cx))),
                );
            } else {
                content = content.child(div().text_color(theme.text_muted).child(label));
            }
        }
        content.into_any_element()
    }
    fn open_chat(&mut self, id: String, cx: &mut Context<Self>) {
        cx.emit(TaskPageEvent::OpenChat(id));
    }
}

/// "cron · timezone" or "Once · date": the task page and Overview task rows.
pub(crate) fn schedule_label(schedule: &TaskSchedule) -> String {
    match schedule {
        TaskSchedule::Cron {
            expression,
            timezone,
        } => format!("{expression} · {timezone}"),
        TaskSchedule::Once { at } => format!(
            "Once · {}",
            chrono::DateTime::from_timestamp_millis(*at)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default()
        ),
    }
}

/// "Next: …" in the schedule's own timezone, when the task will run again.
pub(crate) fn next_run_label(task: &ScheduledTask) -> Option<String> {
    task.next_due.map(|at| {
        format!(
            "Next: {}",
            mast_engine::scheduled_tasks::evaluator::display_time(&task.draft.schedule, at)
        )
    })
}

fn run_state_label(state: ScheduledRunState) -> &'static str {
    match state {
        ScheduledRunState::Launching => "Launching",
        ScheduledRunState::Queued => "Queued",
        ScheduledRunState::Running => "Running",
        ScheduledRunState::AwaitingInput => "Awaiting input",
        ScheduledRunState::Completed => "Completed",
        ScheduledRunState::Failed => "Failed",
        ScheduledRunState::Interrupted => "Interrupted",
        ScheduledRunState::Cancelled => "Cancelled",
        ScheduledRunState::Skipped => "Skipped",
    }
}

/// The newest run's state and detail (`runs` come newest first).
pub(crate) fn last_outcome_label(runs: &[ScheduledRun], task_id: &str) -> String {
    runs.iter()
        .find(|r| r.task_id == task_id)
        .map(|r| {
            format!(
                "{}{}",
                run_state_label(r.state),
                r.detail
                    .as_ref()
                    .map(|d| format!(" · {d}"))
                    .unwrap_or_default()
            )
        })
        .unwrap_or("No sessions yet".into())
}

/// Retired task identities on `device`, newest run first: (task id, name, runs).
fn retired_identities<'a>(chats: &'a [Chat], device: &str) -> Vec<(&'a str, &'a str, usize)> {
    let mut found: Vec<(&str, &str, usize, (i64, &str))> = Vec::new();
    for chat in chats.iter().filter(|c| c.device_id == device) {
        let Some(info) = chat.scheduled_task_info.as_ref().filter(|i| i.retired) else {
            continue;
        };
        let order = mast_proto::task_chat_order(chat);
        match found.iter_mut().find(|f| f.0 == info.task_id) {
            Some(entry) => {
                entry.2 += 1;
                if order > entry.3 {
                    entry.1 = &info.task_name;
                    entry.3 = order;
                }
            }
            None => found.push((&info.task_id, &info.task_name, 1, order)),
        }
    }
    found.sort_by(|a, b| b.3.cmp(&a.3));
    found
        .into_iter()
        .map(|(id, name, n, _)| (id, name, n))
        .collect()
}

fn field(theme: &Theme, label: &str, input: Entity<ComposerInput>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(label.to_owned())),
        )
        .child(popover::dialog_field(theme, input.into_any_element()))
        .into_any_element()
}
fn run_label(count: usize) -> &'static str {
    if count == 1 { "run" } else { "runs" }
}

impl Render for ScheduledTasksPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let mut content = crate::components::shell_parts::scheduled_tasks_page(&theme);
        let title = if self.editor.is_some() {
            "Edit scheduled task"
        } else if self.requested_task.is_some() {
            "Scheduled task"
        } else {
            "Scheduled tasks"
        };
        content = content.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(10.0))
                .child(div().text_size(px(22.0)).child(title))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(8.0))
                        .when(
                            self.editor.is_some() || self.requested_task.is_some(),
                            |el| {
                                el.child(
                                    popover::btn_ghost(&theme, "Manage tasks", "tasks-manage")
                                        .id("tasks-manage")
                                        .debug_selector(|| "tasks-manage".into())
                                        .on_click(cx.listener(|p, _, _, cx| p.show_tasks(cx))),
                                )
                            },
                        )
                        .child(
                            popover::btn_ghost(&theme, "Refresh", "tasks-refresh")
                                .id("tasks-refresh")
                                .on_click(cx.listener(|p, _, _, cx| p.refresh(cx))),
                        )
                        .child(
                            popover::btn_primary(&theme, "New task")
                                .id("tasks-new")
                                .on_click(cx.listener(|p, _, _, cx| {
                                    if !p.busy {
                                        p.edit(None, cx)
                                    }
                                })),
                        ),
                ),
        );
        let devices = self.state.read(cx).devices.clone();
        let device_label = self
            .target
            .as_deref()
            .and_then(|id| self.state.read(cx).device_name(id))
            .unwrap_or("This device")
            .to_owned();
        content = content.child(
            div()
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child(format!(
                    "Run on {device_label} · Host must be awake with Mast’s engine running"
                )),
        );
        let mut hosts = div().flex().flex_wrap().gap(px(6.0));
        for d in devices {
            let id = d.id.clone();
            hosts = hosts.child(
                popover::btn_ghost(&theme, &d.name, format!("task-host-{}", d.id))
                    .id(SharedString::from(format!("task-host-{}", d.id)))
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if !p.busy {
                            p.target = Some(id.clone());
                            p.editor = None;
                            p.requested_task = None;
                            p.snapshot = Default::default();
                            p.cleanup = None;
                            p.models.clear();
                            p.catalogs.clear();
                            p.harnesses.clear();
                            p.refresh(cx)
                        }
                    })),
            );
        }
        content = content.child(hosts);
        if let Some(error) = &self.error {
            content = content.child(
                div()
                    .text_color(theme.danger)
                    .child(SharedString::from(error.clone())),
            );
        }
        if let Some(msg) = &self.message {
            content = content.child(
                div()
                    .text_color(theme.text_muted)
                    .child(SharedString::from(msg.clone())),
            );
        }
        if let Some(error) = &self.catalog_error {
            content = content.child(div().text_color(theme.danger).child(error.clone()));
        }
        if self.catalog_loading {
            content = content.child(
                div()
                    .text_color(theme.text_muted)
                    .child("Loading available agents and models…"),
            );
        }
        if self.busy {
            content = content.child(div().text_color(theme.text_muted).child("Loading…"));
        }
        if let Some(e) = &self.editor {
            let _ = &e.subscriptions;
            content = content
                .child(field(&theme, "Task name", e.name.clone()))
                .child(field(&theme, "Working directory", e.cwd.clone()));
            let config = e.draft.config.clone();
            let model = self
                .models
                .iter()
                .find(|m| Some(&m.id) == config.model.as_ref());
            let model_label = model
                .map(|m| m.label.clone())
                .or(config.model.clone())
                .unwrap_or("Choose model".into());
            let thinking_option = model
                .filter(|m| m.reasoning_levels.is_empty())
                .and_then(|m| m.options.iter().find(|o| o.id == "thinking"));
            let thinking_label = thinking_option
                .and_then(|o| {
                    config
                        .model_options
                        .get("thinking")
                        .and_then(|v| v.as_str())
                        .and_then(|value| o.choices.iter().find(|c| c.id == value))
                })
                .map(|c| c.label.clone())
                .unwrap_or_else(|| {
                    config
                        .reasoning
                        .map(|r| crate::pickers::reasoning_label(r).to_owned())
                        .unwrap_or("Model default".into())
                });
            let context_option =
                model.and_then(|m| m.options.iter().find(|o| o.id == "contextWindow"));
            let context_label = context_option
                .and_then(|o| {
                    let value = config
                        .model_options
                        .get("contextWindow")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&o.default_choice);
                    o.choices
                        .iter()
                        .find(|c| c.id == value)
                        .map(|c| c.label.as_str())
                })
                .unwrap_or("Model default");
            let mut selects = div().flex().flex_wrap().gap(px(10.0));
            let space_label = e
                .draft
                .space_id
                .as_deref()
                .and_then(|id| self.state.read(cx).spaces.iter().find(|s| s.id == id))
                .map(|s| s.name.clone().unwrap_or_else(|| s.path.clone()))
                .unwrap_or("No Space".into());
            let work_items = e
                .draft
                .space_id
                .as_deref()
                .and_then(|space| self.state.read(cx).work_outline(space).snapshot().cloned());
            let background = e.draft.output == ScheduledTaskOutput::Background;
            let output_supported = self.output_supported(cx);
            let work_item_label = e
                .draft
                .work_item_id
                .as_deref()
                .map(|id| {
                    work_items
                        .as_ref()
                        .and_then(|s| s.item(id))
                        .map_or_else(|| id.to_owned(), |item| item.title.clone())
                })
                .unwrap_or_else(|| {
                    if background {
                        "Choose work item"
                    } else {
                        "None"
                    }
                    .into()
                });
            let harness_label = self
                .harnesses
                .iter()
                .find(|h| h.id == config.harness)
                .map(|h| h.name.clone())
                .unwrap_or_else(|| format!("{:?}", config.harness));
            // An older host would drop the field on save, so it is not offered.
            let work_item_supported = self.target.as_deref().is_some_and(|device| {
                self.state.read(cx).device_supports(
                    device,
                    mast_proto::capabilities::SCHEDULED_TASK_WORK_ITEM_V1,
                )
            });
            for (key, label, value) in [
                ("harness", "Agent", harness_label.as_str()),
                ("space", "Space", space_label.as_str()),
                (
                    "output",
                    "Output",
                    if background {
                        "Background"
                    } else {
                        "Interactive"
                    },
                ),
                (
                    "afterSuccess",
                    "After a successful run",
                    match e.draft.after_success {
                        ScheduledTaskAfterSuccess::Archive => "Archive",
                        ScheduledTaskAfterSuccess::Keep => "Keep",
                    },
                ),
                (
                    "workItem",
                    if background {
                        "Work item (required)"
                    } else {
                        "Work item"
                    },
                    work_item_label.as_str(),
                ),
                ("model", "Model", model_label.as_str()),
                ("thinking", "Thinking level", thinking_label.as_str()),
                ("contextWindow", "Context window", context_label),
            ]
            .into_iter()
            .filter(|(key, ..)| match *key {
                "output" => output_supported,
                "afterSuccess" => background && output_supported && self.after_success_supported(cx),
                "workItem" => work_item_supported,
                _ => true,
            }) {
                selects = selects.child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .max_w_full()
                        .gap(px(4.0))
                        .child(label)
                        .child(
                            popover::btn_ghost(&theme, value, format!("task-select-{key}"))
                                .max_w_full()
                                .id(SharedString::from(format!("task-select-{key}")))
                                .debug_selector(move || format!("task-select-{key}").into())
                                .on_click(cx.listener(move |p, _, _, cx| {
                                    p.choices = if p.choices == Some(key) {
                                        None
                                    } else {
                                        Some(key)
                                    };
                                    cx.notify();
                                })),
                        ),
                );
            }
            content = content.child(selects);
            if output_supported {
                content = content.child(
                    div().text_size(px(12.0)).text_color(theme.text_muted).child(
                        if background {
                            "Runs attach to a work item and stay in Run history, out of the sidebar and Overview."
                        } else {
                            "The latest run appears in the sidebar and Overview."
                        },
                    ),
                );
            }
            if e.draft.work_item_id.is_some() {
                content = content.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child("Runs attached to this work item receive its instructions."),
                );
            }
            if let Some(key) = self.choices {
                let choices: Vec<(String, String)> = match key {
                    "harness" => self
                        .harnesses
                        .iter()
                        .map(|h| {
                            (
                                serde_json::to_value(h.id)
                                    .unwrap()
                                    .as_str()
                                    .unwrap()
                                    .to_owned(),
                                h.name.clone(),
                            )
                        })
                        .collect(),
                    "space" => std::iter::once((String::new(), "No Space".into()))
                        .chain(
                            self.state
                                .read(cx)
                                .spaces
                                .iter()
                                .filter(|s| Some(&s.device_id) == self.target.as_ref())
                                .map(|s| {
                                    (
                                        s.id.clone(),
                                        s.name.clone().unwrap_or_else(|| s.path.clone()),
                                    )
                                }),
                        )
                        .collect(),
                    "output" => vec![
                        ("interactive".into(), "Interactive".into()),
                        ("background".into(), "Background".into()),
                    ],
                    "afterSuccess" => vec![
                        ("archive".into(), "Archive".into()),
                        ("keep".into(), "Keep".into()),
                    ],
                    "workItem" => (!background)
                        .then(|| (String::new(), "None".into()))
                        .into_iter()
                        .chain(work_items.iter().flat_map(|s| {
                            s.items.iter().filter(|i| i.archived_at.is_none()).map(|i| {
                                (
                                    i.id.clone(),
                                    format!("{}{}", "  ".repeat(i.depth as usize), i.title),
                                )
                            })
                        }))
                        .collect(),
                    "model" => self
                        .models
                        .iter()
                        .map(|m| (m.id.clone(), m.label.clone()))
                        .collect(),
                    "thinking" if thinking_option.is_some() => {
                        std::iter::once(("null".into(), "Model default".into()))
                            .chain(
                                thinking_option
                                    .into_iter()
                                    .flat_map(|o| o.choices.iter())
                                    .map(|c| (c.id.clone(), c.label.clone())),
                            )
                            .collect()
                    }
                    "thinking" => std::iter::once(("null".into(), "Model default".into()))
                        .chain(
                            model
                                .into_iter()
                                .flat_map(|m| m.reasoning_levels.iter())
                                .map(|r| {
                                    (
                                        serde_json::to_value(r)
                                            .unwrap()
                                            .as_str()
                                            .unwrap()
                                            .to_owned(),
                                        crate::pickers::reasoning_label(*r).to_owned(),
                                    )
                                }),
                        )
                        .collect(),
                    _ => model
                        .into_iter()
                        .flat_map(|m| m.options.iter())
                        .filter(|o| o.id == "contextWindow")
                        .flat_map(|o| o.choices.iter())
                        .map(|c| (c.id.clone(), c.label.clone()))
                        .collect(),
                };
                let mut menu = div().flex().flex_wrap().gap(px(5.0));
                if choices.is_empty() {
                    menu = menu.child(if key == "workItem" {
                        "Choose a Space with an active work item."
                    } else {
                        "This model uses its default setting."
                    });
                }
                for (value, label) in choices {
                    menu = menu.child(
                        popover::btn_ghost(&theme, &label, format!("task-option-{key}-{value}"))
                            .id(SharedString::from(format!("task-option-{key}-{value}")))
                            .on_click(
                                cx.listener(move |p, _, _, cx| p.choice(key, value.clone(), cx)),
                            ),
                    );
                }
                content = content.child(menu);
            }
            content=content.child(div().text_size(px(12.0)).text_color(theme.text_muted).child("Model settings apply to future editions. Past sessions keep their original settings."));
            let mut presets = div().flex().flex_wrap().gap(px(6.0));
            for (key, label, expression) in [
                ("weekdays", "Weekdays at 08:00", "0 8 * * 1-5"),
                ("daily", "Daily at 08:00", "0 8 * * *"),
                ("weekly", "Mondays at 08:00", "0 8 * * 1"),
                (
                    "half-hour",
                    "Every 30 min during work hours",
                    "*/30 9-17 * * 1-5",
                ),
            ] {
                presets = presets.child(
                    popover::btn_ghost(&theme, label, format!("task-preset-{key}"))
                        .id(SharedString::from(format!("task-preset-{key}")))
                        .on_click(cx.listener(move |p, _, _, cx| {
                            if p.busy {
                                return;
                            }
                            if let Some(e) = p.editor.as_ref() {
                                e.cron.update(cx, |i, cx| i.set_text(expression, cx));
                                e.once.update(cx, |i, cx| i.set_text("", cx));
                            }
                            p.snapshot.preview.clear();
                            cx.notify();
                        })),
                );
            }
            content = content.child(presets);
            content = content
                .child(field(&theme, "Instructions", e.prompt.clone()))
                .child(field(
                    &theme,
                    "Cron · minute hour day month weekday",
                    e.cron.clone(),
                ))
                .child(field(&theme, "Timezone", e.timezone.clone()))
                .child(field(
                    &theme,
                    "One-time date (optional, replaces cron)",
                    e.once.clone(),
                ))
                .child(field(
                    &theme,
                    "Run latest missed edition within this many minutes (0 = skip)",
                    e.grace.clone(),
                ));
            if self
                .snapshot
                .tasks
                .iter()
                .any(|t| t.id == e.id && t.import_source.is_some())
            {
                let confirmed = e.draft.cutover_confirmed;
                content = content.child(
                    popover::btn_ghost(
                        &theme,
                        if confirmed {
                            "✓ Claude schedule paused; no source run active"
                        } else {
                            "○ Confirm Claude is paused and no source run is active"
                        },
                        "task-cutover",
                    )
                    .id("task-cutover")
                    .on_click(cx.listener(|p, _, _, cx| {
                        if let Some(e) = p.editor.as_mut() {
                            e.draft.cutover_confirmed = !e.draft.cutover_confirmed;
                        }
                        cx.notify();
                    })),
                );
            }
            let enabled = e.draft.enabled;
            // A task saved before the setting existed already runs unrestricted.
            let notion_writes = e.draft.write_to_notion.unwrap_or(true);
            let notion_writes_supported = self.notion_writes_supported(cx);
            let mode = crate::pickers::approval::ApprovalMode::from_config(&e.draft.config);
            content = content.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(
                            &theme,
                            if enabled {
                                "✓ Scheduled runs enabled"
                            } else {
                                "○ Scheduled runs paused"
                            },
                            "task-enabled",
                        )
                        .id("task-enabled")
                        .on_click(cx.listener(|p, _, _, cx| {
                            if let Some(e) = p.editor.as_mut() {
                                e.draft.enabled = !e.draft.enabled;
                            }
                            cx.notify();
                        })),
                    )
                    .when(notion_writes_supported, |row| {
                        row.child(
                            popover::btn_ghost(
                                &theme,
                                if notion_writes {
                                    "✓ Write to Notion on"
                                } else {
                                    "○ Write to Notion off"
                                },
                                "task-notion-writes",
                            )
                            .id("task-notion-writes")
                            .debug_selector(|| "task-notion-writes".into())
                            .on_click(cx.listener(|p, _, _, cx| {
                                if let Some(e) = p.editor.as_mut() {
                                    e.draft.write_to_notion =
                                        Some(!e.draft.write_to_notion.unwrap_or(true));
                                }
                                cx.notify();
                            })),
                        )
                    })
                    .child(
                        popover::btn_ghost(
                            &theme,
                            match mode {
                                crate::pickers::approval::ApprovalMode::Ask => "Permissions: Ask",
                                crate::pickers::approval::ApprovalMode::Rules => "Permissions: Rules (~/.mast/settings.json)",
                                crate::pickers::approval::ApprovalMode::Auto => "Permissions: Auto-approve",
                            },
                            "task-permissions",
                        )
                        .id("task-permissions")
                        .on_click(cx.listener(|p, _, _, cx| {
                            if let Some(e) = p.editor.as_mut() {
                                let next = crate::pickers::approval::ApprovalMode::from_config(&e.draft.config).next();
                                e.draft.config.auto_approve = next.auto_approve();
                                e.draft.config.approval_rules = next.approval_rules();
                            }
                            cx.notify();
                        })),
                    ),
            );
            content = content.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.0))
                    .child(
                        popover::btn_primary(&theme, "Save changes")
                            .id("task-save")
                            .debug_selector(|| "task-save".into())
                            .on_click(cx.listener(|p, _, _, cx| p.save(cx))),
                    )
                    .child(
                        popover::btn_ghost(&theme, "Preview next five runs", "task-preview")
                            .id("task-preview")
                            .on_click(cx.listener(|p, _, _, cx| match p.editor_draft(cx) {
                                Ok(d) => p.call(
                                    ScheduledTasksRequest::Preview {
                                        schedule: d.schedule,
                                    },
                                    cx,
                                ),
                                Err(e) => {
                                    p.error = Some(e);
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        popover::btn_ghost(&theme, "Back to tasks", "task-back")
                            .id("task-back")
                            .on_click(cx.listener(|p, _, _, cx| p.show_tasks(cx))),
                    ),
            );
            for at in &self.snapshot.preview {
                let stamp = self
                    .editor_draft(cx)
                    .map(|d| {
                        mast_engine::scheduled_tasks::evaluator::display_time(&d.schedule, *at)
                    })
                    .unwrap_or_default();
                content = content.child(div().text_size(px(12.0)).child(stamp));
            }
            content = content.child(self.render_task_history(&e.id, &theme, cx));
        } else if let Some(task_id) = &self.requested_task {
            if self.busy {
                content = content.child("Loading task settings…");
            } else if self.error.is_none() {
                content = content.child("This schedule is no longer available on this device. Its past runs remain below; use Manage tasks to view or import schedules.");
                content = content.child(self.render_task_history(task_id, &theme, cx));
            }
        } else {
            content = content.child(
                popover::btn_ghost(&theme, "Import from Claude…", "tasks-import")
                    .id("tasks-import")
                    .on_click(cx.listener(|p, _, _, cx| {
                        p.call(
                            ScheduledTasksRequest::ImportPreview {
                                registry_path: None,
                                timezone: "America/New_York".into(),
                                space_id: None,
                            },
                            cx,
                        )
                    })),
            );
            if self.target.as_deref().is_some_and(|device| {
                self.state.read(cx).device_supports(device, capabilities::SCHEDULED_RUN_CLEANUP_V1)
            }) {
                content = content.child(
                    popover::btn_ghost(&theme, "Clean up old runs", "tasks-cleanup-preview")
                        .id("tasks-cleanup-preview")
                        .on_click(cx.listener(|p, _, _, cx| {
                            p.call(ScheduledTasksRequest::Cleanup { mode: ScheduledRunCleanupMode::Preview }, cx);
                        })),
                );
            }
            if let Some(cleanup) = self.cleanup.clone() {
                content = content.child(self.render_cleanup(&cleanup, &theme, cx));
            }
            if self.snapshot.tasks.is_empty() {
                content = content.child(
                    "No scheduled tasks yet. Create a task or preview your Claude schedules.",
                );
            }
            for task in self.snapshot.tasks.clone() {
                let edit = task.clone();
                let toggle = task.clone();
                let delete = task.clone();
                let run_id = task.id.clone();
                let detail = last_outcome_label(&self.snapshot.runs, &task.id);
                let schedule = schedule_label(&task.draft.schedule);
                let latest = self
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .filter(|c| {
                        c.scheduled_task_info
                            .as_ref()
                            .is_some_and(|s| s.task_id == task.id)
                            && c.device_id == task.device_id
                    })
                    .max_by_key(|c| mast_proto::task_chat_order(c))
                    .map(|c| c.id.clone());
                let mut row = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .py(px(12.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().text_size(px(15.0)).child(task.draft.name.clone()))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child(format!(
                                "{} · {schedule} · {detail}{}",
                                if task.draft.enabled {
                                    "Enabled"
                                } else {
                                    "Paused"
                                },
                                next_run_label(&task)
                                    .map(|next| format!(" · {next}"))
                                    .unwrap_or_default()
                            )),
                    );
                let mut actions = div().flex().flex_wrap().gap(px(6.0));
                if let Some(chat) = latest {
                    actions = actions.child(
                        popover::btn_ghost(&theme, "Open latest", format!("latest-{}", task.id))
                            .id(SharedString::from(format!("latest-{}", task.id)))
                            .on_click(
                                cx.listener(move |p, _, _, cx| p.open_chat(chat.clone(), cx)),
                            ),
                    );
                }
                actions = actions
                    .child(
                        popover::btn_ghost(&theme, "Edit", format!("edit-{}", task.id))
                            .id(SharedString::from(format!("edit-{}", task.id)))
                            .debug_selector({
                                let id = task.id.clone();
                                move || format!("task-edit-{id}").into()
                            })
                            .on_click(cx.listener(move |p, _, _, cx| {
                                if !p.busy {
                                    p.edit(Some(edit.clone()), cx)
                                }
                            })),
                    )
                    .child(
                        popover::btn_ghost(&theme, "Run now", format!("run-{}", task.id))
                            .id(SharedString::from(format!("run-{}", task.id)))
                            .on_click(cx.listener(move |p, _, _, cx| {
                                p.call(
                                    ScheduledTasksRequest::RunNow {
                                        id: run_id.clone(),
                                        operation_id: uuid::Uuid::new_v4().to_string(),
                                    },
                                    cx,
                                )
                            })),
                    )
                    .child(
                        popover::btn_ghost(
                            &theme,
                            if task.draft.enabled {
                                "Pause"
                            } else {
                                "Enable"
                            },
                            format!("toggle-{}", task.id),
                        )
                        .id(SharedString::from(format!("toggle-{}", task.id)))
                        .on_click(cx.listener(move |p, _, _, cx| {
                            let mut draft = toggle.draft.clone();
                            draft.enabled = !draft.enabled;
                            p.call(
                                ScheduledTasksRequest::Save {
                                    id: toggle.id.clone(),
                                    expected_revision: toggle.revision,
                                    draft,
                                    operation_id: uuid::Uuid::new_v4().to_string(),
                                },
                                cx,
                            )
                        })),
                    )
                    .child(
                        popover::btn_ghost(&theme, "Delete", format!("delete-{}", task.id))
                            .id(SharedString::from(format!("delete-{}", task.id)))
                            .on_click(cx.listener(move |p, _, _, cx| {
                                p.call(
                                    ScheduledTasksRequest::Delete {
                                        id: delete.id.clone(),
                                        expected_revision: delete.revision,
                                        operation_id: uuid::Uuid::new_v4().to_string(),
                                    },
                                    cx,
                                )
                            })),
                    );
                row = row.child(actions);
                content = content.child(row);
            }
            let retired: Vec<(String, String, usize)> = self
                .target
                .as_deref()
                .map(|device| {
                    retired_identities(&self.state.read(cx).chats, device)
                        .into_iter()
                        .map(|(id, name, n)| (id.to_owned(), name.to_owned(), n))
                        .collect()
                })
                .unwrap_or_default();
            if !retired.is_empty() {
                content = content.child(
                    div()
                        .id("retired-tasks")
                        .debug_selector(|| "retired-tasks".into())
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .mt(px(16.0))
                        .child(div().text_size(px(16.0)).child("Retired tasks"))
                        .child(div().text_size(px(12.0)).text_color(theme.text_muted).child(
                            "Runs of deleted tasks and Claude tasks never imported into Mast. They are hidden from the sidebar, Overview and Work Outline.",
                        ))
                        .children(retired.into_iter().map(|(id, name, runs)| {
                            let open = id.clone();
                            let selector = id.clone();
                            popover::btn_ghost(
                                &theme,
                                &format!("{name} · {runs} run{}", if runs == 1 { "" } else { "s" }),
                                format!("retired-{id}"),
                            )
                            .id(SharedString::from(format!("retired-{id}")))
                            .debug_selector(move || format!("retired-task-{selector}").into())
                            .on_click(cx.listener(move |p, _, _, cx| {
                                let device = p.target.clone();
                                p.show_task(open.clone(), device, cx)
                            }))
                        })),
                );
            }
            for candidate in self.snapshot.candidates.clone() {
                let draft = candidate.clone();
                let name = candidate.draft.name.clone();
                content = content.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .py(px(10.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .child(format!(
                            "{name} · {} in Claude",
                            if candidate.source_enabled {
                                "Enabled"
                            } else {
                                "Disabled"
                            }
                        ))
                        .children(candidate.warnings.into_iter().map(|w| {
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.text_muted)
                                .child(w)
                        }))
                        .child(
                            popover::btn_ghost(&theme, "Import paused", format!("import-{name}"))
                                .id(SharedString::from(format!("import-{name}")))
                                .on_click(cx.listener(move |p, _, _, cx| {
                                    p.call(
                                        ScheduledTasksRequest::Import {
                                            candidate: draft.clone(),
                                            operation_id: uuid::Uuid::new_v4().to_string(),
                                        },
                                        cx,
                                    )
                                })),
                        ),
                );
            }
        }
        div()
            .id("scheduled-tasks-page")
            .size_full()
            .overflow_y_scroll()
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[gpui::test]
    fn the_work_item_field_needs_a_host_that_stores_it(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let state = cx.new(|_| AppState::new());
        state.update(cx, |s, _| {
            for (id, capabilities) in [
                (
                    "current",
                    vec![
                        capabilities::SCHEDULED_TASK_WORK_ITEM_V1,
                        capabilities::SCHEDULED_TASK_OUTPUT_V1,
                        capabilities::SCHEDULED_TASK_AFTER_SUCCESS_V1,
                        capabilities::SCHEDULED_TASK_NOTION_WRITES_V1,
                    ],
                ),
                ("item-only", vec![capabilities::SCHEDULED_TASK_WORK_ITEM_V1]),
                ("older", vec![]),
            ] {
                s.devices.push(
                    serde_json::from_value(serde_json::json!({
                        "id": id, "name": id, "platform": "macos", "capabilities": capabilities,
                    }))
                    .unwrap(),
                );
            }
        });
        let (page, cx) = cx.add_window_view(|_, cx| {
            let mut p = ScheduledTasksPage::new(state.clone(), cx);
            p.target = Some("current".into());
            p.edit(None, cx);
            p.error = None;
            p
        });
        cx.simulate_resize(gpui::size(px(900.0), px(1600.0)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("task-select-workItem").is_some());
        assert!(cx.debug_bounds("task-select-output").is_some());
        assert!(cx.debug_bounds("task-select-afterSuccess").is_some());
        assert!(cx.debug_bounds("task-notion-writes").is_some());
        page.update(cx, |p, cx| {
            assert_eq!(p.editor_draft(cx).unwrap().write_to_notion, Some(false));
            assert_eq!(
                p.editor_draft(cx).unwrap().output,
                ScheduledTaskOutput::Background
            );
            assert_eq!(p.editor_draft(cx).unwrap().after_success, ScheduledTaskAfterSuccess::Archive);
            p.choice("afterSuccess", "keep".into(), cx);
            assert_eq!(p.editor_draft(cx).unwrap().after_success, ScheduledTaskAfterSuccess::Keep);
            p.save(cx);
            assert_eq!(
                p.error.as_deref(),
                Some("Choose a work item for background output")
            );
            p.choice("output", "interactive".into(), cx);
            assert_eq!(
                p.editor_draft(cx).unwrap().output,
                ScheduledTaskOutput::Interactive
            );
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("task-select-afterSuccess").is_none());
        page.update(cx, |p, cx| {
            p.target = Some("item-only".into());
            p.edit(None, cx);
            assert_eq!(
                p.editor_draft(cx).unwrap().output,
                ScheduledTaskOutput::Interactive
            );
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("task-notion-writes").is_none());
        page.update(cx, |p, cx| {
            assert_eq!(p.editor_draft(cx).unwrap().write_to_notion, None);
        });
        assert!(cx.debug_bounds("task-select-workItem").is_some());
        assert!(cx.debug_bounds("task-select-output").is_none());
        assert!(cx.debug_bounds("task-select-afterSuccess").is_none());
        page.update(cx, |p, cx| {
            p.target = Some("older".into());
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("task-select-workItem").is_none());
        assert!(cx.debug_bounds("task-select-space").is_some());
    }

    #[gpui::test]
    fn task_list_opens_settings_with_only_that_tasks_history_including_background_runs(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let state = cx.new(|_| AppState::new());
        let (page, cx) = cx.add_window_view(|_, cx| {
            let mut p = ScheduledTasksPage::new(state.clone(), cx);
            state.update(cx, |s, _| {
                s.chats = [("morning-run", "morning", "background"), ("weekly-run", "weekly", "interactive")].into_iter().map(|(id, task, output)| {
                    serde_json::from_value(serde_json::json!({"id":id,"deviceId":"host","archived":false,"createdAt":"2026-01-01T00:00:00Z","scheduledTaskInfo":{"taskId":task,"taskName":task,"runId":id,"output":output}})).unwrap()
                }).collect();
                let now = chrono::Utc::now();
                assert_eq!(s.overview_chats(now).iter().map(|(_, c)| c.id.as_str()).collect::<Vec<_>>(), ["weekly-run"]);
                assert_eq!(s.sidebar_chats(now, None).iter().map(|(_, c)| c.id.as_str()).collect::<Vec<_>>(), ["weekly-run"]);
            });
            p.target = Some("host".into());
            p.edit(None, cx);
            let mut draft = p.editor.take().unwrap().draft;
            draft.name = "Morning briefing".into();
            draft.prompt = "Read the morning procedure".into();
            let task = ScheduledTask {
                id: "morning".into(),
                revision: 7,
                device_id: "host".into(),
                draft,
                created_at: 1,
                updated_at: 2,
                next_due: None,
                deleted: false,
                import_source: None,
                source_handoff_at: None,
            };
            let mut second = task.clone();
            second.id = "weekly".into();
            second.draft.name = "Weekly report".into();
            p.snapshot.tasks = vec![task, second];
            p.show_tasks(cx);
            p.error = None;
            p
        });
        cx.simulate_resize(gpui::size(px(900.0), px(2200.0)));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("task-run-history").is_none(),
            "the main view lists tasks, not runs"
        );
        let edit = cx
            .debug_bounds("task-edit-morning")
            .expect("the task has an edit action")
            .center();
        cx.simulate_click(edit, gpui::Modifiers::default());
        cx.run_until_parked();
        page.read_with(cx, |p, cx| {
            let e = p.editor.as_ref().unwrap();
            assert_eq!(e.id, "morning");
            assert_eq!(e.revision, 7);
            assert_eq!(e.prompt.read(cx).text(), "Read the morning procedure");
            assert!(p.requested_task.is_none());
        });
        assert!(cx.debug_bounds("task-run-morning-run").is_some());
        assert!(cx.debug_bounds("task-run-weekly-run").is_none());
        assert!(
            cx.debug_bounds("task-run-history").unwrap().top()
                > cx.debug_bounds("task-save").unwrap().bottom()
        );
        page.update(cx, |p, cx| {
            p.catalogs.insert(
                HarnessId::Codex,
                vec![Model {
                    id: "codex-fixture".into(),
                    label: "Codex fixture".into(),
                    description: None,
                    reasoning_levels: vec![],
                    options: vec![],
                }],
            );
            p.choice("harness", "codex".into(), cx);
            let draft = p.editor_draft(cx).unwrap();
            assert_eq!(draft.config.harness, HarnessId::Codex);
            assert_eq!(draft.config.model.as_deref(), Some("codex-fixture"));
            assert_eq!(p.editor.as_ref().unwrap().revision, 7);
            p.snapshot.tasks[0].draft = draft;
            p.snapshot.tasks[0].revision = 8;
            p.finish_task_load(Some("morning"), cx);
            assert_eq!(p.editor.as_ref().unwrap().revision, 8);
            assert_eq!(p.editor_draft(cx).unwrap().config.harness, HarnessId::Codex);
        });
        cx.run_until_parked();
        let manage = cx
            .debug_bounds("tasks-manage")
            .expect("editor exposes Manage tasks")
            .center();
        cx.simulate_click(manage, gpui::Modifiers::default());
        cx.run_until_parked();
        page.read_with(cx, |p, _| {
            assert!(p.requested_task.is_none());
            assert!(p.editor.is_none());
            assert_eq!(p.snapshot.tasks.len(), 2);
        });
        let edit_second = cx
            .debug_bounds("task-edit-weekly")
            .expect("every configured task is editable")
            .center();
        cx.simulate_click(edit_second, gpui::Modifiers::default());
        cx.run_until_parked();
        page.read_with(cx, |p, _| {
            assert_eq!(p.editor.as_ref().unwrap().id, "weekly");
        });
        assert!(cx.debug_bounds("task-run-weekly-run").is_some());
        assert!(cx.debug_bounds("task-run-morning-run").is_none());
        page.update(cx, |p, cx| {
            p.show_task("morning".into(), None, cx);
            assert_eq!(p.editor.as_ref().unwrap().id, "morning");
            let mut remote_task = p.snapshot.tasks[0].clone();
            p.show_task("morning".into(), Some("another-host".into()), cx);
            assert!(
                p.requested_task_definition().is_none(),
                "never edit another device's definition"
            );
            remote_task.device_id = "another-host".into();
            p.snapshot.tasks = vec![remote_task];
            p.finish_task_load(None, cx);
            assert_eq!(
                p.editor.as_ref().unwrap().id,
                "morning",
                "a loaded definition opens directly in its editor"
            );
            p.show_tasks(cx);
            assert!(p.editor.is_none() && p.requested_task.is_none());
        });
    }

    #[gpui::test]
    fn latest_editions_and_side_chat_filtering_work_together(cx: &mut gpui::TestAppContext) {
        let state = cx.new(|_| AppState::new());
        let row = |id: &str, created: i64, side: Option<&str>| {
            serde_json::from_value(serde_json::json!({
                "id": id, "deviceId": "host", "archived": false,
                "createdAt": "2026-01-01T00:00:00Z", "sideOf": side,
                "scheduledTask": "Daily",
                "scheduledTaskInfo": {
                    "taskId": if side.is_some() { "other-task" } else { "task" },
                    "taskName": "Daily", "runId": id, "sessionCreatedAt": created
                }
            }))
            .unwrap()
        };
        state.update(cx, |s, _| {
            s.chats = vec![
                row("older", 1, None),
                row("latest", 2, None),
                row("side", 3, Some("latest")),
            ];
            let ids: Vec<_> = s
                .overview_chats(chrono::Utc::now())
                .into_iter()
                .map(|(_, c)| c.id.as_str())
                .collect();
            assert_eq!(ids, vec!["latest"]);
        });
    }

    #[gpui::test]
    fn retired_runs_leave_default_views_but_keep_their_history(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let state = cx.new(|_| AppState::new());
        let run = |id: &str, task: &str, created: i64| -> Chat {
            serde_json::from_value(serde_json::json!({
                "id": id, "deviceId": "host", "archived": false,
                "createdAt": "2026-01-01T00:00:00Z",
                "scheduledTaskInfo": {"taskId": task, "taskName": "retired-routine",
                    "runId": id, "sessionCreatedAt": created, "retired": true}
            }))
            .unwrap()
        };
        let (page, cx) = cx.add_window_view(|_, cx| {
            state.update(cx, |s, _| {
                s.chats = vec![run("first", "retired", 1), run("second", "retired", 2)];
                assert!(s.overview_chats(chrono::Utc::now()).is_empty());
            });
            let mut p = ScheduledTasksPage::new(state.clone(), cx);
            p.target = Some("host".into());
            p.show_tasks(cx);
            p.error = None;
            p
        });
        cx.simulate_resize(gpui::size(px(900.0), px(1600.0)));
        cx.run_until_parked();
        let retired = cx
            .debug_bounds("retired-task-retired")
            .expect("retired identities are listed")
            .center();
        cx.simulate_click(retired, gpui::Modifiers::default());
        page.update(cx, |p, cx| {
            assert_eq!(p.requested_task.as_deref(), Some("retired"));
            p.error = None;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("task-run-first").is_some());
        assert!(cx.debug_bounds("task-run-second").is_some());
    }

    #[gpui::test]
    fn native_editor_preserves_instructions_and_constrains_model_options(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let state = cx.new(|_| AppState::new());
        let (page, cx) = cx.add_window_view(|_, cx| {
            let mut page = ScheduledTasksPage::new(state, cx);
            page.models = vec![
                Model {
                    id: "capable".into(),
                    label: "Capable model".into(),
                    description: None,
                    reasoning_levels: vec![ReasoningLevel::Low, ReasoningLevel::High],
                    options: vec![ModelOption {
                        id: "contextWindow".into(),
                        label: "Context window".into(),
                        choices: vec![
                            ModelOptionChoice {
                                id: "200k".into(),
                                label: "200K".into(),
                            },
                            ModelOptionChoice {
                                id: "1m".into(),
                                label: "1M".into(),
                            },
                        ],
                        default_choice: "200k".into(),
                    }],
                },
                Model {
                    id: "small".into(),
                    label: "Small model".into(),
                    description: None,
                    reasoning_levels: vec![],
                    options: vec![ModelOption {
                        id: "thinking".into(),
                        label: "Thinking".into(),
                        choices: vec![
                            ModelOptionChoice {
                                id: "on".into(),
                                label: "On".into(),
                            },
                            ModelOptionChoice {
                                id: "off".into(),
                                label: "Off".into(),
                            },
                        ],
                        default_choice: "off".into(),
                    }],
                },
            ];
            page.edit(None, cx);
            page
        });
        page.update(cx, |p, cx| {
            p.editor.as_ref().unwrap().prompt.update(cx, |input, cx| {
                input.set_text("Keep these instructions", cx)
            });
            p.choice("thinking", "high".into(), cx);
            p.choice("contextWindow", "1m".into(), cx);
            assert_eq!(
                p.editor_draft(cx).unwrap().config.reasoning,
                Some(ReasoningLevel::High)
            );
            assert_eq!(
                p.editor_draft(cx).unwrap().config.model_options["contextWindow"],
                "1m"
            );
            p.choice("model", "small".into(), cx);
            let draft = p.editor_draft(cx).unwrap();
            assert!(draft.config.reasoning.is_none());
            assert!(draft.config.model_options.is_empty());
            assert_eq!(draft.prompt, "Keep these instructions");
            p.choice("thinking", "on".into(), cx);
            assert_eq!(
                p.editor_draft(cx).unwrap().config.model_options["thinking"],
                "on"
            );
            assert!(p.editor_draft(cx).unwrap().config.reasoning.is_none());

            p.catalogs.insert(HarnessId::ClaudeCode, p.models.clone());
            let other = Model {
                id: "session-model".into(),
                label: "Session model".into(),
                description: None,
                reasoning_levels: vec![ReasoningLevel::High],
                options: vec![ModelOption {
                    id: "contextWindow".into(),
                    label: "Context Window".into(),
                    default_choice: "default".into(),
                    choices: vec![
                        ModelOptionChoice {
                            id: "default".into(),
                            label: "Model default (500K)".into(),
                        },
                        ModelOptionChoice {
                            id: "872000".into(),
                            label: "872K (max)".into(),
                        },
                    ],
                }],
            };
            p.catalogs.insert(HarnessId::Codex, vec![other]);
            p.choice("harness", "codex".into(), cx);
            p.choice("thinking", "high".into(), cx);
            let draft = p.editor_draft(cx).unwrap();
            assert_eq!(draft.config.harness, HarnessId::Codex);
            assert_eq!(draft.config.model.as_deref(), Some("session-model"));
            assert_eq!(draft.config.reasoning, Some(ReasoningLevel::High));
            assert!(draft.config.model_options.is_empty());
            assert_eq!(draft.prompt, "Keep these instructions");
            p.choice("contextWindow", "872000".into(), cx);
            assert_eq!(
                p.editor_draft(cx).unwrap().config.model_options["contextWindow"],
                "872000"
            );
            p.choice("contextWindow", "default".into(), cx);
            assert_eq!(
                p.editor_draft(cx).unwrap().config.model_options["contextWindow"],
                "default"
            );
            p.choice("harness", "claude-code".into(), cx);
            assert_eq!(
                p.editor_draft(cx).unwrap().config.model.as_deref(),
                Some("capable")
            );
            assert!(p.editor_draft(cx).unwrap().config.reasoning.is_none());

            p.choices = Some("contextWindow");
            cx.notify();
        });
        cx.simulate_resize(gpui::size(px(800.0), px(900.0)));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn editor_long_fields_stay_single_line_and_instructions_grow(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let state = cx.new(|_| AppState::new());
        let (page, cx) = cx.add_window_view(|_, cx| {
            let mut p = ScheduledTasksPage::new(state, cx);
            p.edit(None, cx);
            let e = p.editor.as_ref().unwrap();
            e.cwd.update(cx, |i, cx| {
                i.set_text(format!("/work/{}", "very-long-directory/".repeat(80)), cx)
            });
            e.prompt.update(cx, |i, cx| {
                i.set_text("Read the current procedure.\n".repeat(100), cx)
            });
            p
        });
        cx.simulate_resize(gpui::size(px(640.0), px(900.0)));
        cx.run_until_parked();
        page.read_with(cx, |p, cx| {
            let e = p.editor.as_ref().unwrap();
            assert_eq!(e.cwd.read(cx).measured_content_height(), 20.0);
            assert!(e.prompt.read(cx).measured_content_height() > 100.0);
            assert!(e.cwd.read(cx).measured_width().unwrap() < 640.0);
        });
    }

    #[gpui::test]
    fn cleanup_confirmation_refreshes_cancels_and_closes(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let state = cx.new(|_| AppState::new());
        let (page, cx) = cx.add_window_view(|_, cx| ScheduledTasksPage::new(state, cx));
        let preview = |id: &str| ScheduledRunCleanup {
            tasks: vec![ScheduledRunCleanupTask {
                task_id: "task".into(),
                task_name: "Briefing".into(),
                space_id: None,
                count: 1,
                chat_ids: vec![id.into()],
            }],
            applied: false,
        };
        page.update(cx, |p, cx| {
            p.error = None;
            p.show_cleanup_result(preview("original"), false);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("tasks-cleanup-apply").is_some());

        page.update(cx, |p, cx| {
            p.show_cleanup_result(preview("replacement"), true);
            assert_eq!(p.cleanup.as_ref().unwrap().chat_ids().next().unwrap(), "replacement");
            assert!(p.message.as_deref().unwrap().contains("Eligible runs changed"));
            cx.notify();
        });
        cx.run_until_parked();
        let cancel = cx.debug_bounds("tasks-cleanup-cancel").unwrap().center();
        cx.simulate_click(cancel, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("tasks-cleanup-apply").is_none());

        page.update(cx, |p, cx| {
            p.show_cleanup_result(preview("replacement"), false);
            p.set_active(false);
            p.show_cleanup_result(preview("late"), false);
            assert!(p.cleanup.is_none());
            p.set_active(true);
            p.show_cleanup_result(ScheduledRunCleanup { applied: true, ..preview("replacement") }, true);
            assert!(p.cleanup.is_none());
            assert_eq!(p.message.as_deref(), Some("Archived 1 run."));
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("tasks-cleanup-apply").is_none());
    }

    #[cfg(all(feature = "snapshot", target_os = "macos"))]
    #[test]
    #[ignore = "requires macOS Metal and access to macOS services"]
    fn scheduled_run_cleanup_snapshot() -> anyhow::Result<()> {
        struct Fixture(Entity<ScheduledTasksPage>);
        impl Render for Fixture {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().size_full().bg(Theme::of(cx).surface).child(self.0.clone())
            }
        }

        let path = crate::snapshot::render_to_png(
            "scheduled-run-cleanup",
            gpui::size(px(850.0), px(700.0)),
            crate::ui_style::UiStyleId::Default,
            crate::theme::Appearance::Light,
            |_, cx| {
                let _ = crate::typography::register_fonts(cx);
                let state = cx.new(|_| {
                    let mut state = AppState::new();
                    state.devices.push(serde_json::from_value(serde_json::json!({
                        "id":"host", "name":"This Mac", "platform":"macos",
                        "capabilities":[capabilities::SCHEDULED_RUN_CLEANUP_V1],
                    })).unwrap());
                    state
                });
                let page = cx.new(|cx| {
                    let mut page = ScheduledTasksPage::new(state, cx);
                    page.error = None;
                    page.target = Some("host".into());
                    page.cleanup = Some(ScheduledRunCleanup {
                        applied: false,
                        tasks: [
                            ("briefing", "Daily briefing", vec!["run-a", "run-b", "run-c"]),
                            ("nightly", "Nightly checks", vec!["run-d"]),
                        ].into_iter().map(|(id, name, runs)| ScheduledRunCleanupTask {
                            task_id: id.into(),
                            task_name: name.into(),
                            space_id: None,
                            count: runs.len(),
                            chat_ids: runs.into_iter().map(str::to_owned).collect(),
                        }).collect(),
                    });
                    page
                });
                cx.new(|_| Fixture(page))
            },
        )?;
        println!("{}", path.display());
        Ok(())
    }

    #[cfg(all(feature = "snapshot", target_os = "macos"))]
    #[test]
    #[ignore = "requires macOS Metal and access to macOS services"]
    fn scheduled_task_editor_snapshot() -> anyhow::Result<()> {
        struct Fixture(Entity<ScheduledTasksPage>);
        impl Render for Fixture {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .bg(Theme::of(cx).surface)
                    .child(self.0.clone())
            }
        }
        for (name, appearance) in [
            ("dark", crate::theme::Appearance::Dark),
            ("light", crate::theme::Appearance::Light),
        ] {
            for (host, output) in [
                ("interactive", ScheduledTaskOutput::Interactive),
                ("background", ScheduledTaskOutput::Background),
                ("older", ScheduledTaskOutput::Interactive),
            ] {
                let capabilities = if host == "older" {
                    vec![]
                } else {
                    vec![
                        capabilities::SCHEDULED_TASK_WORK_ITEM_V1,
                        capabilities::SCHEDULED_TASK_OUTPUT_V1,
                        capabilities::SCHEDULED_TASK_AFTER_SUCCESS_V1,
                    ]
                };
                crate::snapshot::render_to_png(
                    &format!("scheduled-task-retention-editor-{host}-{name}"),
                    gpui::size(px(850.0), px(1500.0)),
                    crate::ui_style::UiStyleId::Default,
                    appearance,
                    |_, cx| {
                        let _ = crate::typography::register_fonts(cx);
                        let state = cx.new(|_| AppState::new());
                        let page = cx.new(|cx| {
                            let mut p = ScheduledTasksPage::new(state.clone(), cx);
                            p.target = Some("host".into());
                            state.update(cx, |s, _| {
                                s.devices.push(serde_json::from_value(serde_json::json!({"id":"host","name":"This Mac","platform":"macos","capabilities":capabilities})).unwrap());
                                s.chats = vec![serde_json::from_value(serde_json::json!({"id":"morning-previous","deviceId":"host","title":"Daily summary","archived":host == "background","createdAt":"2026-11-11T08:00:00Z","scheduledTaskInfo":{"taskId":"morning","taskName":"Daily summary","runId":"morning-previous"}})).unwrap()];
                            });
                            p.edit(None, cx);
                            p.editor.as_mut().unwrap().id = "morning".into();
                            p.editor.as_mut().unwrap().revision = 2;
                            let e = p.editor.as_ref().unwrap();
                            e.name.update(cx, |i,cx|i.set_text("Daily summary",cx));
                            e.cwd.update(cx, |i,cx|i.set_text("/work/scheduled-tasks/very-long-working-directory-for-the-daily-summary",cx));
                            e.prompt.update(cx, |i,cx|i.set_text("Read the current procedure.\nSummarize open work, changes, and outstanding questions.\nKeep the links to original sources.\n",cx));
                            let draft = &mut p.editor.as_mut().unwrap().draft;
                            draft.work_item_id = (host != "older").then(|| "routines".into());
                            draft.output = output;
                            p.snapshot.runs = vec![ScheduledRun {
                                id: "morning-previous".into(),
                                task_id: "morning".into(),
                                scheduled_for: 1_794_384_000_000,
                                created_at: 1_794_384_000_000,
                                finished_at: Some(1_794_384_060_000),
                                manual: false,
                                state: ScheduledRunState::Completed,
                                detail: None,
                                chat_id: Some("morning-previous".into()),
                                task_revision: 1,
                                snapshot: draft.clone(),
                                awaiting_request_id: None,
                                awaiting_since: None,
                            }];
                            p.error = None;
                            p
                        });
                        cx.new(|_| Fixture(page))
                    },
                )?;
            }
            crate::snapshot::render_to_png(
                &format!("scheduled-task-list-retired-{name}"),
                gpui::size(px(850.0), px(700.0)),
                crate::ui_style::UiStyleId::Default,
                appearance,
                |_, cx| {
                    let _ = crate::typography::register_fonts(cx);
                    let state = cx.new(|_| AppState::new());
                    let retired = |id: &str, task: &str, name: &str| -> Chat {
                        serde_json::from_value(serde_json::json!({"id":id,"deviceId":"host","archived":false,"createdAt":"2026-11-11T08:00:00Z","scheduledTaskInfo":{"taskId":task,"taskName":name,"runId":id,"retired":true}})).unwrap()
                    };
                    state.update(cx, |s, _| {
                        s.chats = vec![
                            retired("a", "old-routine", "routine-a"),
                            retired("b", "old-routine", "routine-a"),
                            retired("c", "nightly", "nightly-check"),
                        ];
                    });
                    let page = cx.new(|cx| {
                        let mut p = ScheduledTasksPage::new(state.clone(), cx);
                        p.target = Some("host".into());
                        p.show_tasks(cx);
                        p.error = None;
                        p
                    });
                    cx.new(|_| Fixture(page))
                },
            )?;
        }
        Ok(())
    }
    #[gpui::test]
    fn history_from_a_session_targets_its_host_without_retargeting_task_rows(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let state = cx.new(|_| AppState::new());
        let page = cx.new(|cx| ScheduledTasksPage::new(state, cx));
        page.update(cx, |p, cx| {
            p.target = Some("device-b".into());
            p.show_task("task".into(), Some("device-a".into()), cx);
            assert_eq!(p.target.as_deref(), Some("device-a"));
            p.target = Some("device-b".into());
            p.show_task("task".into(), None, cx);
            assert_eq!(p.target.as_deref(), Some("device-b"));
        });
    }
}
