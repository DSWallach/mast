//! Quota-hit prompt and temporary default-model switch UI.

use std::collections::HashSet;

use chrono::Utc;
use gpui::{AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, prelude::*, px};
use mast_proto::{HarnessId, Model};
use mast_rpc::methods;

use super::Shell;
use crate::popover;
use crate::quota_guard::{
    self, QuotaCandidate, QuotaHit, SwitchDurationOption,
};
use crate::theme::Theme;

#[derive(Default)]
pub(super) struct QuotaUi {
    pub(super) prompt: Option<QuotaPromptState>,
    dismissed_keys: HashSet<String>,
    candidates_task: Option<gpui::Task<()>>,
}

pub(super) struct QuotaPromptState {
    pub(super) hit: QuotaHit,
    pub(super) candidates: Vec<QuotaCandidate>,
    pub(super) loading_candidates: bool,
    pub(super) selected_candidate: usize,
    pub(super) durations: Vec<SwitchDurationOption>,
    pub(super) selected_duration: usize,
}

impl Shell {
    /// Expire any finished temporary default-model override and check whether
    /// the current default model/harness has hit its quota.
    pub(super) fn check_quota_and_override(&mut self, cx: &mut Context<Self>) {
        let now = Utc::now();
        if crate::settings::expire_temporary_default_override(cx, now).is_some() {
            self.settings = crate::settings::current(cx);
            cx.notify();
        }
        if self.quota.prompt.is_some() {
            return;
        }
        let ui_settings = crate::settings::current(cx);
        if ui_settings
            .temporary_default_override
            .as_ref()
            .is_some_and(|ov| ov.expires_at > now)
        {
            return;
        }
        let (active_harness, active_model_id, active_model_label) = {
            let pickers = self.composer.read(cx).pickers().read(cx);
            let Some(harness) = pickers.effective_harness(cx) else {
                return;
            };
            let model_id = pickers.effective_model_id(cx).map(str::to_owned);
            let pref = crate::settings::effective_default_model(&ui_settings, now);
            let label = pref
                .model_label
                .filter(|_| pref.model.as_deref() == model_id.as_deref());
            (harness, model_id, label)
        };

        let hit = self
            .menu_bar
            .last_accounts_snapshot
            .as_ref()
            .and_then(|snap| {
                quota_guard::detect_usage_quota_hit(
                    active_harness,
                    active_model_id.as_deref(),
                    active_model_label.as_deref(),
                    &snap.accounts,
                    now,
                )
            })
            .or_else(|| {
                let state = self.state.read(cx);
                quota_guard::detect_session_quota_hit(
                    active_harness,
                    active_model_id.as_deref(),
                    active_model_label.as_deref(),
                    &state.chats,
                    &state.sessions,
                    now,
                )
            });

        if let Some(hit) = hit
            && !self.quota.dismissed_keys.contains(&hit.dedupe_key)
        {
            self.open_quota_prompt(hit, cx);
        }
    }

    pub(super) fn open_quota_prompt(&mut self, hit: QuotaHit, cx: &mut Context<Self>) {
        let now = Utc::now();
        let durations = SwitchDurationOption::options_for(hit.resets_at, now);
        let hit_harness = hit.harness;
        let hit_model = hit.model_id.clone();
        self.quota.prompt = Some(QuotaPromptState {
            hit,
            candidates: Vec::new(),
            loading_candidates: true,
            selected_candidate: 0,
            durations,
            selected_duration: 0,
        });
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            if let Some(prompt) = &mut self.quota.prompt {
                prompt.loading_candidates = false;
            }
            cx.notify();
            return;
        };
        self.quota.candidates_task = Some(cx.spawn(async move |this, cx| {
            let harnesses = engine
                .client()
                .call(methods::LIST_HARNESSES, serde_json::json!({}))
                .await
                .ok()
                .and_then(|v| {
                    serde_json::from_value::<Vec<mast_engine::registry::HarnessDescriptor>>(v).ok()
                })
                .unwrap_or_default();

            let mut candidates = Vec::new();
            let mut ordered: Vec<_> = harnesses
                .into_iter()
                .filter(|h| {
                    mast_engine::registry::descriptor_enabled(h)
                        && h.installed
                        && h.id != HarnessId::Mock
                })
                .collect();
            // Prefer alternative harnesses first, then the current harness.
            ordered.sort_by_key(|h| h.id == hit_harness);

            for descriptor in ordered {
                let models = engine
                    .client()
                    .call(
                        methods::LIST_MODELS,
                        serde_json::json!({ "harness": descriptor.id }),
                    )
                    .await
                    .ok()
                    .and_then(|v| serde_json::from_value::<Vec<Model>>(v).ok())
                    .unwrap_or_default();
                for model in models {
                    if descriptor.id == hit_harness
                        && hit_model.as_deref() == Some(model.id.as_str())
                    {
                        continue;
                    }
                    candidates.push(QuotaCandidate {
                        harness: descriptor.id,
                        harness_name: descriptor.name.clone(),
                        model: model.id,
                        model_label: model.label,
                    });
                }
            }

            this.update(cx, |shell, cx| {
                if let Some(prompt) = &mut shell.quota.prompt {
                    prompt.candidates = candidates;
                    prompt.loading_candidates = false;
                    prompt.selected_candidate = 0;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn dismiss_quota_prompt(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = self.quota.prompt.take() {
            self.quota.dismissed_keys.insert(prompt.hit.dedupe_key);
        }
        self.quota.candidates_task = None;
        cx.notify();
    }

    pub(super) fn apply_quota_switch(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = self.quota.prompt.take() else {
            return;
        };
        self.quota.candidates_task = None;
        self.quota.dismissed_keys.insert(prompt.hit.dedupe_key);
        let Some(candidate) = prompt.candidates.get(prompt.selected_candidate) else {
            cx.notify();
            return;
        };
        let Some(duration) = prompt.durations.get(prompt.selected_duration) else {
            cx.notify();
            return;
        };
        let now = Utc::now();
        let previous = crate::settings::current(cx).default_model;
        let ov = quota_guard::build_temporary_override(
            candidate,
            previous,
            duration,
            prompt.hit.reason,
            now,
        );
        crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
            settings.temporary_default_override = Some(ov);
        });
        self.settings = crate::settings::current(cx);
        cx.notify();
    }

    pub(super) fn render_quota_prompt(
        &self,
        viewport: gpui::Size<gpui::Pixels>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let prompt = self.quota.prompt.as_ref()?;
        let now = Utc::now();
        let can_apply = !prompt.candidates.is_empty();

        let body = if prompt.loading_candidates {
            crate::components::quota_parts::empty_message(theme)
                .child("Loading available models…")
                .into_any_element()
        } else if prompt.candidates.is_empty() {
            crate::components::quota_parts::empty_message(theme)
                .child("No other enabled models are available to switch to.")
                .into_any_element()
        } else {
            div()
                .id("quota-candidate-list")
                .max_h(px(180.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .children(prompt.candidates.iter().enumerate().map(|(ix, c)| {
                    let active = ix == prompt.selected_candidate;
                    popover::menu_row(theme, active, format!("quota-candidate-{ix}"))
                        .id(("quota-candidate", ix))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(p) = &mut this.quota.prompt {
                                p.selected_candidate = ix;
                                cx.notify();
                            }
                        }))
                        .child(SharedString::from(format!(
                            "{} \u{b7} {}",
                            c.harness_name, c.model_label
                        )))
                }))
                .into_any_element()
        };

        let durations = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(6.0))
            .children(prompt.durations.iter().enumerate().map(|(ix, d)| {
                let active = ix == prompt.selected_duration;
                crate::components::quota_parts::duration(ix, active, theme)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(p) = &mut this.quota.prompt {
                            p.selected_duration = ix;
                            cx.notify();
                        }
                    }))
                    .child(SharedString::from(d.label(now)))
            }));

        let card = popover::popover_card(theme)
            .w(px(420.0))
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        crate::components::quota_parts::heading(theme)
                            .child("Model quota reached"),
                    )
                    .child(
                        crate::components::quota_parts::description(theme)
                            .child(SharedString::from(format!(
                                "{}. Switch your default model temporarily?",
                                prompt.hit.reason
                            ))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        crate::components::quota_parts::field_label(theme)
                            .child("Switch default to"),
                    )
                    .child(body),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        crate::components::quota_parts::field_label(theme)
                            .child("Time window (reverts automatically)"),
                    )
                    .child(durations),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .pt(px(4.0))
                    .child(
                        popover::btn_ghost(theme, "Keep current", "quota-prompt-dismiss")
                            .id("quota-prompt-dismiss")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.dismiss_quota_prompt(cx)),
                            ),
                    )
                    .child(
                        popover::btn_primary(theme, "Switch default")
                            .id("quota-prompt-apply")
                            .when(!can_apply, |el| el.opacity(0.5))
                            .when(can_apply, |el| {
                                el.on_click(cx.listener(|this, _, _, cx| {
                                    this.apply_quota_switch(cx)
                                }))
                            }),
                    ),
            )
            .into_any_element();

        Some(popover::modal("quota-switch-prompt", viewport, card))
    }
}
