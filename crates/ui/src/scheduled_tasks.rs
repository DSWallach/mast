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
        self.requested_task = Some(id);
        self.editor = None;
        self.open_requested_task(cx);
        self.refresh(cx);
        cx.notify();
    }
    pub fn show_tasks(&mut self, cx: &mut Context<Self>) {
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
    }
    fn load(&mut self, cx: &mut Context<Self>) {
        self.call(ScheduledTasksRequest::List, cx);
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
        self.load_catalog(cx);
    }
    fn call(&mut self, request: ScheduledTasksRequest, cx: &mut Context<Self>) {
        if self.busy {
            return;
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
        let mut params = serde_json::to_value(request).unwrap();
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
                        if is_preview {
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
                    config: ChatConfig {
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
        let mut chats: Vec<_> = self
            .state
            .read(cx)
            .chats
            .iter()
            .filter(|c| {
                c.scheduled_task_info
                    .as_ref()
                    .is_some_and(|t| t.task_id == task_id)
                    && Some(&c.device_id) == self.target.as_ref()
            })
            .cloned()
            .collect();
        chats.sort_by(|a, b| mast_proto::task_chat_order(b).cmp(&mast_proto::task_chat_order(a)));
        if chats.is_empty() {
            content = content.child(
                div()
                    .text_color(theme.text_muted)
                    .child("No runs yet for this task."),
            );
        }
        for (i, chat) in chats.into_iter().enumerate() {
            let id = chat.id.clone();
            let selector_id = id.clone();
            content = content.child(
                popover::btn_ghost(
                    theme,
                    &format!(
                        "{} · {}{}",
                        chrono::DateTime::from_timestamp_millis(
                            mast_proto::task_chat_order(&chat).0
                        )
                        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
                        .unwrap_or_default(),
                        chat.title.as_deref().unwrap_or("Task run"),
                        if i == 0 { " · Latest" } else { "" }
                    ),
                    format!("history-{}", chat.id),
                )
                .id(SharedString::from(format!("history-{}", chat.id)))
                .debug_selector(move || format!("task-run-{selector_id}").into())
                .on_click(cx.listener(move |p, _, _, cx| p.open_chat(id.clone(), cx))),
            );
        }
        content.into_any_element()
    }
    fn open_chat(&mut self, id: String, cx: &mut Context<Self>) {
        cx.emit(TaskPageEvent::OpenChat(id));
    }
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
impl Render for ScheduledTasksPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let mut content = div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .p(px(24.0))
            .max_w(px(1050.0))
            .w_full()
            .text_size(crate::typography::ui_rems(13.0))
            .font_family(theme.font_sans.clone())
            .text_color(theme.text);
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
            let harness_label = self
                .harnesses
                .iter()
                .find(|h| h.id == config.harness)
                .map(|h| h.name.clone())
                .unwrap_or_else(|| format!("{:?}", config.harness));
            for (key, label, value) in [
                ("harness", "Agent", harness_label.as_str()),
                ("space", "Space", space_label.as_str()),
                ("model", "Model", model_label.as_str()),
                ("thinking", "Thinking level", thinking_label.as_str()),
                ("contextWindow", "Context window", context_label),
            ] {
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
                    menu = menu.child("This model uses its default setting.");
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
            let auto = e.draft.config.auto_approve;
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
                    .child(
                        popover::btn_ghost(
                            &theme,
                            if auto {
                                "Permissions: Auto-approve"
                            } else {
                                "Permissions: Ask"
                            },
                            "task-permissions",
                        )
                        .id("task-permissions")
                        .on_click(cx.listener(|p, _, _, cx| {
                            if let Some(e) = p.editor.as_mut() {
                                e.draft.config.auto_approve = !e.draft.config.auto_approve;
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
                let recent = self.snapshot.runs.iter().find(|r| r.task_id == task.id);
                let detail = recent
                    .map(|r| {
                        format!(
                            "{:?}{}",
                            r.state,
                            r.detail
                                .as_ref()
                                .map(|d| format!(" · {d}"))
                                .unwrap_or_default()
                        )
                    })
                    .unwrap_or("No sessions yet".into());
                let schedule = match &task.draft.schedule {
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
                };
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
                                task.next_due
                                    .map(|at| format!(
                                        " · Next: {}",
                                        mast_engine::scheduled_tasks::evaluator::display_time(
                                            &task.draft.schedule,
                                            at
                                        )
                                    ))
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
    fn task_list_opens_settings_with_only_that_tasks_history_below(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let state = cx.new(|_| AppState::new());
        let (page, cx) = cx.add_window_view(|_, cx| {
            let mut p = ScheduledTasksPage::new(state.clone(), cx);
            state.update(cx, |s, _| {
                s.chats = [("morning-run", "morning"), ("weekly-run", "weekly")].into_iter().map(|(id, task)| {
                    serde_json::from_value(serde_json::json!({"id":id,"deviceId":"host","archived":false,"createdAt":"2026-01-01T00:00:00Z","scheduledTaskInfo":{"taskId":task,"taskName":task,"runId":id}})).unwrap()
                }).collect();
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
            crate::snapshot::render_to_png(
                &format!("scheduled-task-editor-{name}"),
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
                            s.chats = vec![serde_json::from_value(serde_json::json!({"id":"morning-previous","deviceId":"host","title":"Morning engineering briefing","archived":false,"createdAt":"2026-11-11T08:00:00Z","scheduledTaskInfo":{"taskId":"morning","taskName":"Morning engineering briefing","runId":"morning-previous"}})).unwrap()];
                        });
                        p.edit(None, cx);
                        p.editor.as_mut().unwrap().id = "morning".into();
                        p.editor.as_mut().unwrap().revision = 2;
                        let e = p.editor.as_ref().unwrap();
                        e.name.update(cx, |i,cx|i.set_text("Morning engineering briefing",cx));
                        e.cwd.update(cx, |i,cx|i.set_text("/Users/david/Work/mast/.worktrees/scheduled-tasks/very-long-working-directory-for-the-daily-briefing",cx));
                        e.prompt.update(cx, |i,cx|i.set_text("Read the current procedure.\nSummarize open work, changes, and outstanding questions.\nKeep the links to original sources.\n",cx));
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
