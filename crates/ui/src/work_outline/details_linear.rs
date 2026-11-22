//! Linear in Work Item Details.
//!
//! - Space-root variant: the per-Space **Linear** toggle
//!   (`setSpaceLinearEnabled`) with a one-line explanation, and "Connect
//!   Linear…" (Settings → Linear) while this device has no key.
//! - Items of a Linear-enabled Space: the **Linear** section (live issue
//!   chip, project chip, ↻ = `LinearRefreshLinkedItems` for the Space,
//!   Unlink; on a project link also "Add issues from <project>…" and, for a
//!   pasted-URL link My projects resolves, Fix link), and "Synced from Linear" on a title / description that still
//!   follows the issue (`externalSyncModifiedFields` does not list it).
//! - Pasted references: a Linear URL becomes an `issue` / `project`
//!   reference only with Linear on; otherwise it stays a plain link and the
//!   Space shows a one-time "Enable Linear for this Space?" hint.
//!
//! A child module of `details_panel` (like the instruction and execution
//! sections), so it uses the surface's private state directly.
use std::collections::BTreeSet;

use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use mast_proto::linear::{LINEAR_PROJECT_REFERENCE, LINEAR_PROVIDER, LinearProjectRef};
use mast_proto::{
    CreateWorkItemExternalReferenceParams, LinkLinearProjectParams,
    RemoveWorkItemExternalReferenceParams, SetSpaceLinearAccountParams, SetSpaceLinearEnabledParams,
};

use super::super::actions::{OperationSlot, new_operation_id};
use super::super::chips::TextTooltip;
use super::super::store::{WorkItemView, WorkOutlineSnapshot};
use super::{ParsedReference, WorkItemDetails};
use crate::linear::ListRead;
use crate::linear::store::{self, IssueEntry};
use crate::linear::{LinearUrl, parse_linear_url, project_short_id};
use crate::theme::Theme;

/// Spaces that already showed the "Enable Linear for this Space?" hint
/// (device-local, under the data directory).
pub(crate) const PASTE_HINT_SEEN_FILE: &str = "linear-paste-hint-seen.json";

/// One choice of the worker context window control.
struct WindowChoice {
    id: String,
    label: String,
    /// What it sends: `None` = Default, `Some(0)` = no cap.
    tokens: Option<u64>,
    selected: bool,
}

fn window_label(tokens: u64) -> String {
    format!("{}K", tokens / 1_000)
}

/// Default (200K), the offered sizes, then No cap. The 200K size is the
/// Default, so it has no chip of its own; a stored size outside the list
/// selects nothing.
fn worker_context_window_choices(effective: Option<u64>) -> Vec<WindowChoice> {
    use mast_proto::work_outline::{DEFAULT_WORKER_CONTEXT_WINDOW, WORKER_CONTEXT_WINDOW_SIZES};
    let mut choices = vec![WindowChoice {
        id: "worker-window-default".into(),
        label: format!("Default ({})", window_label(DEFAULT_WORKER_CONTEXT_WINDOW)),
        tokens: None,
        selected: effective == Some(DEFAULT_WORKER_CONTEXT_WINDOW),
    }];
    choices.extend(
        WORKER_CONTEXT_WINDOW_SIZES
            .into_iter()
            .filter(|n| *n != DEFAULT_WORKER_CONTEXT_WINDOW)
            .map(|n| WindowChoice {
                id: format!("worker-window-{n}"),
                label: window_label(n),
                tokens: Some(n),
                selected: effective == Some(n),
            }),
    );
    choices.push(WindowChoice {
        id: "worker-window-no-cap".into(),
        label: "No cap".into(),
        tokens: Some(0),
        selected: effective.is_none(),
    });
    choices
}

/// One choice of the worker call nudge control.
struct NudgeChoice {
    id: String,
    label: String,
    /// What it sends: `None` = Default, `Some(0)` = Off.
    calls: Option<u32>,
    selected: bool,
}

/// Default (150), the offered counts, then Off. The default count has no chip
/// of its own; a stored count outside the list selects nothing.
fn worker_call_nudge_choices(effective: u32) -> Vec<NudgeChoice> {
    use mast_proto::work_outline::{DEFAULT_WORKER_CALL_NUDGE, WORKER_CALL_NUDGE_CHOICES};
    let mut choices = vec![NudgeChoice {
        id: "worker-nudge-default".into(),
        label: format!("Default ({DEFAULT_WORKER_CALL_NUDGE})"),
        calls: None,
        selected: effective == DEFAULT_WORKER_CALL_NUDGE,
    }];
    choices.extend(WORKER_CALL_NUDGE_CHOICES.into_iter().map(|n| NudgeChoice {
        id: format!("worker-nudge-{n}"),
        label: n.to_string(),
        calls: Some(n),
        selected: effective == n,
    }));
    choices.push(NudgeChoice {
        id: "worker-nudge-off".into(),
        label: "Off".into(),
        calls: Some(0),
        selected: effective == 0,
    });
    choices
}

/// Per-surface Linear state.
#[derive(Debug, Default)]
pub(crate) struct LinearDetailsState {
    pub(crate) error: Option<String>,
    pub(crate) status_line: Option<String>,
    pub(crate) refreshing: bool,
    pub(crate) toggling: bool,
    pub(crate) observed_accounts: Option<Vec<mast_proto::linear::LinearAccount>>,
    pub(crate) observed_configured: Option<bool>,
    /// The one-time hint after a Linear URL was pasted with Linear off.
    pub(crate) paste_hint: bool,
    toggle_op: OperationSlot,
}

impl WorkItemDetails {
    fn save_worker_automation(&mut self, settings: mast_proto::work_outline::WorkOutlineSpaceSettings, field: mast_proto::work_outline::WorkerAutomationField, cx: &mut Context<Self>) {
        let task = self.state.read(cx).work_outline_actions().mutate(
            mast_proto::WorkOutlineMutation::SetWorkerAutomation(mast_proto::work_outline::SetWorkerAutomationParams {
                space_id: self.space_id.clone(), field: Some(field), worker_worktrees: settings.worker_worktrees,
                merged_pr_completion: settings.merged_pr_completion,
                archive_workers_after_days: settings.archive_workers_after_days,
                operation_id: new_operation_id(),
            })
        );
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| { if let Err(error) = result { this.error = Some(error.message); } cx.notify(); }).ok();
        }).detach();
    }

    fn save_worker_context_window(&mut self, tokens: Option<u64>, cx: &mut Context<Self>) {
        let task = self.state.read(cx).work_outline_actions().mutate(
            mast_proto::WorkOutlineMutation::SetWorkerContextWindow(mast_proto::work_outline::SetWorkerContextWindowParams {
                space_id: self.space_id.clone(), tokens, operation_id: new_operation_id(),
            })
        );
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| { if let Err(error) = result { this.error = Some(error.message); } cx.notify(); }).ok();
        }).detach();
    }

    fn save_worker_call_nudge(&mut self, calls: Option<u32>, cx: &mut Context<Self>) {
        let task = self.state.read(cx).work_outline_actions().mutate(
            mast_proto::WorkOutlineMutation::SetWorkerCallNudge(mast_proto::work_outline::SetWorkerCallNudgeParams {
                space_id: self.space_id.clone(), calls, operation_id: new_operation_id(),
            })
        );
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| { if let Err(error) = result { this.error = Some(error.message); } cx.notify(); }).ok();
        }).detach();
    }

    pub(super) fn render_worker_call_nudge(&mut self, snapshot: &WorkOutlineSnapshot, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let effective = snapshot.space_settings.worker_call_nudge;
        let mut chips = crate::components::outline_detail_row(theme).flex_wrap().gap(px(6.0));
        for choice in worker_call_nudge_choices(effective) {
            let calls = choice.calls;
            let selector = choice.id.clone();
            chips = chips.child(div().id(SharedString::from(choice.id))
                .debug_selector(move || selector.clone())
                .cursor_pointer().px(px(8.0)).py(px(4.0)).border_1().border_color(theme.border)
                .text_color(theme.text).child(if choice.selected { format!("✓ {}", choice.label) } else { choice.label })
                .on_click(cx.listener(move |this, _, _, cx| this.save_worker_call_nudge(calls, cx))));
        }
        let current = match effective {
            0 => "Effective: off".to_owned(),
            n => format!("Effective: a checkpoint nudge every {n} tool calls in one turn"),
        };
        crate::components::outline_section("worker-call-nudge", theme)
            .child(crate::components::outline_detail_heading("Worker tool-call checkpoint", theme))
            .child(chips)
            .child(crate::components::outline_execution_line(current, 11.0, theme.text, theme).debug_selector(|| "worker-nudge-effective".into()))
            .child(crate::components::outline_execution_line("Asks a worker to summarize and continue or hand back. Worker chats only; never stops a turn.".into(), 11.0, theme.text_faint, theme))
            .into_any_element()
    }

    pub(super) fn render_worker_context_window(&mut self, snapshot: &WorkOutlineSnapshot, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let effective = snapshot.space_settings.worker_context_window;
        let mut chips = crate::components::outline_detail_row(theme).flex_wrap().gap(px(6.0));
        for choice in worker_context_window_choices(effective) {
            let tokens = choice.tokens;
            let selector = choice.id.clone();
            chips = chips.child(div().id(SharedString::from(choice.id))
                .debug_selector(move || selector.clone())
                .cursor_pointer().px(px(8.0)).py(px(4.0)).border_1().border_color(theme.border)
                .text_color(theme.text).child(if choice.selected { format!("✓ {}", choice.label) } else { choice.label })
                .on_click(cx.listener(move |this, _, _, cx| this.save_worker_context_window(tokens, cx))));
        }
        let current = match effective {
            Some(tokens) => format!("Effective: {} tokens", window_label(tokens)),
            None => "Effective: no cap (the model's own window)".to_owned(),
        };
        crate::components::outline_section("worker-context-window", theme)
            .child(crate::components::outline_detail_heading("Worker context window", theme))
            .child(chips)
            .child(crate::components::outline_execution_line(current, 11.0, theme.text, theme).debug_selector(|| "worker-window-effective".into()))
            .child(crate::components::outline_execution_line("Applies to new Codex worker chats only. Running chats keep the window they started with.".into(), 11.0, theme.text_faint, theme))
            .into_any_element()
    }

    pub(super) fn render_worker_automation(&mut self, snapshot: &WorkOutlineSnapshot, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        use mast_proto::work_outline::{MergedPrCompletion, WorkerAutomationField};
        let settings = snapshot.space_settings.clone();
        let enabled = settings.worker_worktrees;
        let mut section = crate::components::outline_section("worker-automation", theme)
            .child(crate::components::outline_detail_heading("Worker automations", theme))
            .child(crate::components::outline_detail_row(theme)
                .child(crate::components::outline_execution_line("Start workers in their own worktree".into(), 11.0, theme.text, theme))
                .child(crate::components::toggle_switch(theme, enabled).id("worker-worktrees-toggle")
                    .on_click(cx.listener({ let settings = settings.clone(); move |this, _, _, cx| {
                        let mut settings = settings.clone(); settings.worker_worktrees = !enabled; this.save_worker_automation(settings, WorkerAutomationField::Worktrees, cx);
                    }}))))
            .child(crate::components::outline_execution_line("Create a branch and worktree for a worker when its item has no execution setup.".into(), 11.0, theme.text_faint, theme))
            .child(crate::components::outline_execution_line("When a linked PR merges".into(), 11.0, theme.text, theme));
        let mut modes = crate::components::outline_detail_row(theme).flex_wrap().gap(px(6.0));
        for (label, mode) in [("Off", None), ("Ask", Some(MergedPrCompletion::Ask)), ("Automatic", Some(MergedPrCompletion::Automatic))] {
            let selected = settings.merged_pr_completion == mode;
            modes = modes.child(div().id(SharedString::from(format!("worker-merge-{label}")))
                .cursor_pointer().px(px(8.0)).py(px(4.0)).border_1().border_color(theme.border)
                .text_color(theme.text).child(if selected { format!("✓ {label}") } else { label.to_owned() })
                .on_click(cx.listener({ let settings = settings.clone(); move |this, _, _, cx| {
                    let mut settings = settings.clone(); settings.merged_pr_completion = mode; this.save_worker_automation(settings, WorkerAutomationField::MergedPrCompletion, cx);
                }})));
        }
        section = section.child(modes).child(crate::components::outline_execution_line("Ask to mark the item done, or complete it automatically.".into(), 11.0, theme.text_faint, theme));
        let archive_days = settings.archive_workers_after_days;
        let mut archive = crate::components::outline_detail_row(theme)
            .child(crate::components::outline_execution_line("Archive finished workers".into(), 11.0, theme.text, theme))
            .child(crate::components::toggle_switch(theme, archive_days.is_some()).id("worker-archive-toggle")
                .on_click(cx.listener({ let settings = settings.clone(); move |this, _, _, cx| {
                    let mut settings = settings.clone(); settings.archive_workers_after_days = if archive_days.is_some() { None } else { Some(3) }; this.save_worker_automation(settings, WorkerAutomationField::ArchiveWorkers, cx);
                }})));
        if let Some(days) = archive_days {
            for (label, value) in [("−", days.saturating_sub(1).max(1)), ("+", days.saturating_add(1))] {
                archive = archive.child(div().id(SharedString::from(format!("worker-archive-{label}"))).cursor_pointer()
                    .px(px(8.0)).child(label).on_click(cx.listener({ let settings = settings.clone(); move |this, _, _, cx| {
                        let mut settings = settings.clone(); settings.archive_workers_after_days = Some(value); this.save_worker_automation(settings, WorkerAutomationField::ArchiveWorkers, cx);
                    }})));
            }
            archive = archive.child(crate::components::outline_execution_line(format!("After {days} days"), 11.0, theme.text, theme));
        }
        section.child(archive).child(crate::components::outline_execution_line("Archive eligible idle worker chats after the selected delay.".into(), 11.0, theme.text_faint, theme)).into_any_element()
    }

    fn linear_enabled(&self) -> bool {
        crate::linear::space_enabled(self.snapshot.as_deref())
    }

    // ----------------------------------------------------------------------
    // Pasted references
    // ----------------------------------------------------------------------

    /// What a pasted reference becomes here. `None` when this module takes
    /// the add over (a project URL in a Linear-enabled Space).
    pub(super) fn linear_gate_reference(
        &mut self,
        parsed: ParsedReference,
        cx: &mut Context<Self>,
    ) -> Option<ParsedReference> {
        let Some(linear) = parse_linear_url(&parsed.url) else {
            return Some(parsed);
        };
        if !self.linear_enabled() {
            self.offer_paste_hint(cx);
            return Some(ParsedReference {
                provider: "web".into(),
                reference_type: "link".into(),
                external_id: parsed.url.clone(),
                url: parsed.url,
            });
        }
        match linear {
            LinearUrl::Issue(_) => Some(parsed),
            LinearUrl::Project(slug) => {
                self.link_pasted_project(parsed.url, slug, cx);
                None
            }
        }
    }

    fn offer_paste_hint(&mut self, cx: &mut Context<Self>) {
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            self.linear.paste_hint = true;
            return;
        };
        let mut seen: BTreeSet<String> =
            crate::overview_positions::load_string_set(&data_dir, PASTE_HINT_SEEN_FILE);
        if !seen.insert(self.space_id.clone()) {
            return;
        }
        if let Err(err) =
            crate::overview_positions::save_string_set(&data_dir, PASTE_HINT_SEEN_FILE, &seen)
        {
            tracing::warn!(error = %err, "saving the Linear hint state failed");
        }
        self.linear.paste_hint = true;
    }

    /// A pasted project URL: link the matching "My projects" project
    /// (`linkLinearProject`); without a match, a `linear`/`project`
    /// reference keyed by the URL's slug.
    fn link_pasted_project(&mut self, url: String, slug: String, cx: &mut Context<Self>) {
        if !self.in_flight.insert("reference:add".into()) {
            return;
        }
        let actions = self.state.read(cx).work_outline_actions();
        let scope = self.snapshot.as_deref().map(crate::linear::store::account_scope).unwrap_or("default").to_owned();
        let cached = self.state.read(cx).linear.projects.get(&scope).cloned();
        let item_id = self.item_id.clone();
        let space_id = self.space_id.clone();
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let projects = match cached {
                Some(projects) => Some(projects),
                None => actions.linear_my_projects(&space_id, &scope, ListRead::Cached).await.ok(),
            };
            let short = project_short_id(&slug).map(str::to_owned);
            let matched = projects.and_then(|projects| {
                projects.into_iter().find(|project| {
                    project.url.trim_end_matches('/') == url.trim_end_matches('/')
                        || (short.is_some() && project_short_id(&project.url) == short.as_deref())
                })
            });
            let result = match matched {
                Some(project) => {
                    actions
                        .link_linear_project(LinkLinearProjectParams {
                            work_item_id: item_id,
                            project: LinearProjectRef {
                                id: project.id,
                                name: project.name,
                                url: project.url,
                            },
                            operation_id: new_operation_id(),
                        })
                        .await
                }
                None => {
                    actions
                        .link_linear_issue(CreateWorkItemExternalReferenceParams {
                            work_item_id: item_id,
                            provider: LINEAR_PROVIDER.into(),
                            reference_type: LINEAR_PROJECT_REFERENCE.into(),
                            external_id: slug,
                            url,
                            cached_title: None,
                            operation_id: new_operation_id(),
                        })
                        .await
                }
            };
            this.update(cx, |this, cx| {
                this.in_flight.remove("reference:add");
                match result {
                    Ok(_) => this.adding_reference = false,
                    Err(error) => this.error = Some(error.user_text()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn select_linear_account(&mut self, account_id: Option<String>, cx: &mut Context<Self>) {
        if self.linear.toggling { return; }
        let task = self.state.read(cx).work_outline_actions().set_space_linear_account(
            SetSpaceLinearAccountParams { space_id: self.space_id.clone(), account_id, operation_id: new_operation_id() }
        );
        self.linear.toggling = true;
        self.linear.error = None;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.linear.toggling = false;
                if let Err(error) = result { this.linear.error = Some(error.user_text()); }
                cx.notify();
            }).ok();
        }).detach();
        cx.notify();
    }

    pub(crate) fn set_linear_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.linear.toggling {
            return;
        }
        let operation_id = self
            .linear
            .toggle_op
            .id_for(&format!("{}:{enabled}", self.space_id));
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .set_space_linear_enabled(SetSpaceLinearEnabledParams {
                space_id: self.space_id.clone(),
                enabled,
                operation_id,
            });
        self.linear.toggling = true;
        self.linear.error = None;
        if enabled {
            store::ensure_status(&self.state, cx);
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.linear.toggling = false;
                match result {
                    Ok(_) => {
                        this.linear.toggle_op = OperationSlot::default();
                        this.linear.paste_hint = false;
                    }
                    Err(error) => this.linear.error = Some(error.user_text()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_linear_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(shell) = self.shell.upgrade() {
            cx.defer(move |cx| shell.update(cx, |shell, cx| shell.open_linear_settings(cx)));
        }
    }

    fn refresh_linear(&mut self, identifier: Option<String>, cx: &mut Context<Self>) {
        if self.linear.refreshing {
            return;
        }
        self.linear.refreshing = true;
        self.linear.error = None;
        self.linear.status_line = None;
        let weak = cx.entity().downgrade();
        store::refresh_linked(
            &self.state,
            &self.space_id,
            identifier.into_iter().collect(),
            cx,
            move |result, cx| {
                weak.update(cx, |this, cx| {
                    this.linear.refreshing = false;
                    match result {
                        Ok(0) => this.linear.status_line = Some("Up to date".into()),
                        Ok(n) => {
                            this.linear.status_line =
                                Some(format!("Updated {n} item{}", if n == 1 { "" } else { "s" }))
                        }
                        Err(error) => this.linear.error = Some(error.user_text()),
                    }
                    cx.notify();
                })
                .ok();
            },
        );
        cx.notify();
    }

    fn unlink_linear(&mut self, reference_id: String, cx: &mut Context<Self>) {
        // Local registry row only: undo relinks it, nothing goes to Linear.
        let change = self.snapshot.as_deref().and_then(|snapshot| {
            crate::work_outline::action_log::unlink_entry(snapshot, &reference_id, None)
        });
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .remove_external_reference(RemoveWorkItemExternalReferenceParams {
                reference_id: reference_id.clone(),
                operation_id: new_operation_id(),
            });
        self.run_logged(
            format!("reference:{reference_id}"),
            task,
            change,
            cx,
            |_| {},
        );
    }

    /// Fix link: relink a slug-keyed project link by the real project id.
    fn fix_linear_project_link(
        &mut self,
        reference: mast_proto::WorkItemExternalReferenceView,
        project: mast_proto::linear::LinearProjectSummary,
        cx: &mut Context<Self>,
    ) {
        let key = format!("linear:fix:{}", reference.id);
        if !self.in_flight.insert(key.clone()) {
            return;
        }
        self.linear.error = None;
        let task =
            crate::linear::project_links::fix_project_link(&self.state, &reference, &project, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.in_flight.remove(&key);
                match result {
                    Ok(()) => {
                        this.linear.status_line =
                            Some(format!("Fixed the link to {}", project.name))
                    }
                    Err(error) => this.linear.error = Some(error.user_text()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ----------------------------------------------------------------------
    // Rendering
    // ----------------------------------------------------------------------

    /// "Synced from Linear" under a field that still follows the issue.
    pub(super) fn linear_sync_marker(
        &self,
        item: &WorkItemView,
        snapshot: &WorkOutlineSnapshot,
        field: &'static str,
        theme: &Theme,
    ) -> Option<AnyElement> {
        if !crate::linear::space_enabled(Some(snapshot))
            || snapshot.linear_issue_reference(&item.id).is_none()
            || !item.follows_external_sync(field)
        {
            return None;
        }
        let selector = format!("work-item-{field}-synced");
        Some(
            crate::components::outline_linear_sync_marker(selector, theme)
                .tooltip(|_, cx| {
                    cx.new(|_| {
                        TextTooltip("Follows the Linear issue until you edit it here".into())
                    })
                    .into()
                })
                .into_any_element(),
        )
    }

    /// The item's Linear section (Linear-enabled Spaces, linked items only).
    pub(super) fn render_linear_section(
        &mut self,
        item: &WorkItemView,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !crate::linear::space_enabled(Some(snapshot)) {
            return None;
        }
        let issue = snapshot.linear_issue_reference(&item.id).cloned();
        let project = snapshot.linear_project_reference(&item.id).cloned();
        if issue.is_none() && project.is_none() {
            return None;
        }
        let shell = Some(self.shell.clone());
        let refresh_identifier = issue.as_ref().map(|r| r.external_id.clone());
        let mut section = crate::components::outline_detail_section(theme).child(
            crate::components::outline_detail_row(theme)
                .child(crate::components::outline_detail_heading("Linear", theme))
                .child(
                    crate::work_outline::hints::details_button(
                        "work-item-linear-refresh",
                        if self.linear.refreshing {
                            "Refreshing\u{2026}"
                        } else {
                            "\u{21bb} Refresh"
                        },
                        theme,
                    )
                    .debug_selector(|| "work-item-linear-refresh".into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.refresh_linear(refresh_identifier.clone(), cx)
                    })),
                ),
        );
        if let Some(reference) = issue {
            store::request_issue(&self.state, &self.space_id, &reference.external_id, false, cx);
            let entry = self
                .state
                .read(cx)
                .linear
                .issue_in_scope(crate::linear::store::account_scope(snapshot), &reference.external_id)
                .cloned();
            let title = match &entry {
                Some(IssueEntry::Ready { issue, .. }) => {
                    format!("{} \u{00b7} {}", issue.identifier, issue.title)
                }
                _ => reference.external_id.clone(),
            };
            let chip = super::super::outline_view::linear_outline::issue_chip(
                "work-item-linear-chip".into(),
                &reference.external_id,
                entry.as_ref(),
                reference.url.clone(),
                shell.clone(),
                theme,
            );
            let id = reference.id.clone();
            section = section.child(
                crate::components::outline_detail_row(theme)
                    .child(chip)
                    .child(
                        crate::components::outline_detail_row_text(title, theme),
                    )
                    .child(
                        crate::work_outline::hints::details_button("work-item-linear-unlink", "Unlink", theme)
                            .debug_selector(|| "work-item-linear-unlink".into())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.unlink_linear(id.clone(), cx)
                            })),
                    ),
            );
        }
        if let Some(reference) = project {
            let id = reference.id.clone();
            let project_name = reference
                .cached_title
                .clone()
                .unwrap_or_else(|| "project".into());
            let repair = self
                .state
                .read(cx)
                .linear
                .projects
                .get(crate::linear::store::account_scope(snapshot))
                .and_then(|projects| {
                    crate::linear::project_links::repair_target(&reference, projects).cloned()
                });
            let fix = repair.map(|project| {
                let reference = reference.clone();
                super::super::outline_view::linear_outline::fix_link_chip(
                    "work-item-linear-fix-link".into(),
                    &project.name,
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.fix_linear_project_link(reference.clone(), project.clone(), cx)
                }))
            });
            let chip = super::super::outline_view::linear_outline::project_chip(
                "work-item-linear-project".into(),
                reference
                    .cached_title
                    .clone()
                    .unwrap_or_else(|| "Linear project".into()),
                reference.url.clone(),
                shell,
                theme,
            );
            section = section.child(
                crate::components::outline_detail_row(theme)
                    .child(chip)
                    .children(fix)
                    .child(div().flex_1())
                    .child(
                        crate::work_outline::hints::details_button("work-item-linear-unlink-project", "Unlink", theme)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.unlink_linear(id.clone(), cx)
                            })),
                    ),
            );
            let add_selector = "work-item-linear-add-issues";
            let tip: SharedString =
                format!("Pick open issues of {project_name} to add as work items under this one")
                    .into();
            section = section.child(
                div().flex().child(
                    crate::components::outline_small_button(
                        add_selector,
                        format!("Add issues from {project_name}\u{2026}"),
                        theme,
                    )
                    .max_w(px(320.0))
                    .debug_selector(move || add_selector.into())
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.emit(super::DetailsEvent::AddLinearIssues {
                            space_id: this.space_id.clone(),
                            item_id: this.item_id.clone(),
                        })
                    })),
                ),
            );
        }
        if let Some(line) = self.linear.status_line.clone() {
            section = section.child(
                crate::components::outline_execution_line(line, 10.5, theme.text_faint, theme),
            );
        }
        if let Some(error) = self.linear.error.clone() {
            section = section.child(
                crate::components::outline_execution_line(error, 11.0, theme.warning, theme)
                    .debug_selector(|| "work-item-linear-error".into()),
            );
        }
        Some(section.into_any_element())
    }

    /// The one-time "Enable Linear for this Space?" hint.
    pub(super) fn render_linear_paste_hint(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.linear.paste_hint || self.linear_enabled() {
            return None;
        }
        Some(
            crate::components::outline_linear_hint(theme)
                .child(
                    crate::work_outline::hints::details_button("work-item-linear-hint-enable", "Enable", theme)
                        .debug_selector(|| "work-item-linear-hint-enable".into())
                        .on_click(cx.listener(|this, _, _, cx| this.set_linear_enabled(true, cx))),
                )
                .child(
                    crate::work_outline::hints::details_button("work-item-linear-hint-dismiss", "Dismiss", theme)
                        .debug_selector(|| "work-item-linear-hint-dismiss".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.linear.paste_hint = false;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// The Space-root Linear toggle.
    pub(super) fn render_space_linear(
        &mut self,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let enabled = snapshot.space_settings.linear_enabled;
        // A Space with Linear off makes no Linear calls; turning it on
        // fetches the status (`set_linear_enabled`).
        if enabled {
            store::ensure_status(&self.state, cx);
        }
        // Local metadata lets the user choose an account before enabling sync.
        store::ensure_accounts(&self.state, cx);
        let store = &self.state.read(cx).linear;
        let accounts = store.accounts.clone().unwrap_or_default();
        let selected = snapshot.space_settings.linear_account_id.as_deref().unwrap_or("default");
        let configured = accounts.iter().find(|account| account.id == selected).map(|account| account.key_set)
            .or_else(|| if selected == "default" { store.configured() } else if store.accounts.is_some() { Some(false) } else { None });
        let not_available = matches!(&store.status, Some(Err(error)) if error.not_available());
        let toggle = crate::components::toggle_switch(theme, enabled)
            .id("space-linear-toggle")
            .debug_selector(|| "space-linear-toggle".into())
            .cursor_pointer()
            .when(self.linear.toggling, |el| el.opacity(0.5))
            .on_click(cx.listener(move |this, _, _, cx| this.set_linear_enabled(!enabled, cx)));
        let mut section = crate::components::outline_section("space-linear", theme)
            .child(crate::components::outline_detail_row(theme)
                    .child(crate::components::outline_detail_heading("Linear", theme))
                    .child(toggle),
            )
            .child(
                crate::components::outline_execution_line(if enabled { "On: the Linear panel, chips and links work in this Space. Nothing is created or written automatically." } else { "Off: no Linear calls or UI in this Space; Linear URLs stay plain links." }.into(), 11.0, theme.text_faint, theme),
            );
        if accounts.len() > 1 {
            let mut choices = crate::components::outline_detail_row(theme).flex_wrap().gap(px(6.0))
                .child(crate::components::outline_execution_line("Account".into(), 11.0, theme.text_muted, theme));
            for account in accounts {
                let is_selected = account.id == selected;
                let id = account.id.clone();
                let choice = if id == "default" { None } else { Some(id.clone()) };
                let label = if is_selected { format!("✓ {}", account.label) } else { account.label };
                choices = choices.child(
                    div().id(SharedString::from(format!("space-linear-account-{id}")))
                        .debug_selector({ let id = id.clone(); move || format!("space-linear-account-{id}") })
                        .cursor_pointer().px(px(8.0)).py(px(4.0))
                        .border_1().border_color(theme.border).text_color(theme.text).child(label)
                        .on_click(cx.listener(move |this, _, _, cx| this.select_linear_account(choice.clone(), cx)))
                );
            }
            section = section.child(choices);
        }
        if not_available {
            section = section.child(
                crate::components::outline_execution_line("Linear isn’t available on this host yet".into(), 11.0, theme.text_muted, theme),
            );
        } else if configured == Some(false) {
            section = section.child(
                crate::components::outline_detail_row(theme)
                    .child(crate::components::outline_execution_line("No Linear API key on this device.".into(), 11.0, theme.text_muted, theme))
                    .child(
                        crate::work_outline::hints::details_button("space-linear-connect", "Connect Linear\u{2026}", theme)
                            .debug_selector(|| "space-linear-connect".into())
                            .on_click(cx.listener(|this, _, _, cx| this.open_linear_settings(cx))),
                    ),
            );
        }
        if let Some(error) = self.linear.error.clone() {
            section = section.child(
                crate::components::outline_execution_line(error, 11.0, theme.warning, theme)
                    .debug_selector(|| "space-linear-error".into()),
            );
        }
        section.into_any_element()
    }
}

#[cfg(test)]
impl WorkItemDetails {
    /// Paste `url` into the external-reference field and add it (tests).
    pub(crate) fn paste_reference_for_test(&mut self, url: &str, cx: &mut Context<Self>) {
        self.adding_reference = true;
        self.reference_input
            .update(cx, |input, cx| input.set_text(url, cx));
        self.add_reference(cx);
    }
}

#[cfg(test)]
mod window_choice_tests {
    use super::*;

    fn selected(effective: Option<u64>) -> Vec<String> {
        worker_context_window_choices(effective)
            .into_iter()
            .filter(|c| c.selected)
            .map(|c| c.label)
            .collect()
    }

    #[test]
    fn choices_are_default_the_offered_sizes_and_no_cap() {
        let choices = worker_context_window_choices(Some(200_000));
        let labels: Vec<_> = choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Default (200K)", "64K", "128K", "272K", "500K", "No cap"]);
        let sent: Vec<_> = choices.iter().map(|c| c.tokens).collect();
        assert_eq!(sent, [None, Some(64_000), Some(128_000), Some(272_000), Some(500_000), Some(0)]);
    }

    #[test]
    fn exactly_the_effective_value_is_selected() {
        assert_eq!(selected(Some(200_000)), ["Default (200K)"]);
        assert_eq!(selected(Some(128_000)), ["128K"]);
        assert_eq!(selected(None), ["No cap"]);
        // A hand-edited size outside the list selects nothing.
        assert!(selected(Some(150_000)).is_empty());
    }
}
