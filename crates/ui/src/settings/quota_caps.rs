//! Settings → Accounts, "Quota caps": the cap mode, the global soft and freeze
//! caps and a hard cap for each provider (50–95%, 85% by default), of the
//! device picked in the Accounts page's device switcher (`GetQuotaSettings` /
//! `SetQuotaSettings`). Under each provider's slider sit live headroom bars,
//! one per usage window and per model, with the cap marked on each.
//!
//! Edits stay in a draft until Save, which is disabled while the draft is
//! invalid or unchanged. Steppers and sliders move in whole percents, but a
//! cap the user has not moved keeps its saved value exactly, even one that is
//! fractional or outside what the controls offer. No dispatch path reads the
//! caps yet, and the section's copy says so.

use gpui::{AnyElement, Context, Entity, SharedString, Task, Window, div, prelude::*, px};

use mast_engine::quota_settings::QuotaSettings;
use mast_proto::{HarnessId, ProviderQuotaPolicy};
use mast_rpc::methods;

use crate::components::{ButtonSize, ButtonVariant};
use crate::popover::Loadable;
use crate::settings::quota_headroom::{Headroom, HeadroomSource, HeadroomState, HeadroomWindow};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// The range Settings offers for the hard cap, in percent.
pub const HARD_CAP_MIN_PERCENT: u8 = 50;
pub const HARD_CAP_MAX_PERCENT: u8 = 95;

const SECTION_COPY: &str = "Caps reserve the rest of each provider\u{2019}s plan for your \
    interactive chats. Each provider has its own hard cap; the soft and freeze caps are shared.";
const NOT_WIRED_COPY: &str = "Quota-aware dispatch is upcoming. Your choices are saved now and \
    take effect once it is on; until then nothing in Mast acts on them.";

/// The providers that get a hard-cap slider: the key `provider_hard_caps` is
/// saved under (the harness id), its name and its harness.
pub const PROVIDERS: [(&str, &str, HarnessId); 3] = [
    ("claude-code", "Claude Code", HarnessId::ClaudeCode),
    ("codex", "Codex", HarnessId::Codex),
    ("antigravity", "Antigravity", HarnessId::Antigravity),
];

/// The key a harness's hard cap is saved under, for the providers that have one.
pub fn provider_key(harness: HarnessId) -> Option<&'static str> {
    PROVIDERS
        .iter()
        .find(|(_, _, candidate)| *candidate == harness)
        .map(|(key, ..)| *key)
}

/// The slider's notches, in percent: 50, 55 … 95.
pub const SLIDER_STEP_PERCENT: u8 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Soft,
    Hard,
    Freeze,
}

impl Cap {
    pub const ALL: [Cap; 3] = [Cap::Soft, Cap::Hard, Cap::Freeze];
    /// The caps still set with a stepper; the hard cap is set per provider.
    pub const STEPPED: [Cap; 2] = [Cap::Soft, Cap::Freeze];

    /// What a stepper can reach. Ordering between the caps is not clamped: it
    /// is reported by [`CapEditor::error`] so one cap can pass through another's
    /// value while the user rearranges them.
    pub fn range(self) -> (u8, u8) {
        match self {
            Cap::Hard => (HARD_CAP_MIN_PERCENT, HARD_CAP_MAX_PERCENT),
            Cap::Soft | Cap::Freeze => (1, 100),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Cap::Soft => "Soft cap",
            Cap::Hard => "Hard cap",
            Cap::Freeze => "Freeze cap",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Cap::Soft => {
                "Balanced failover leaves a provider here when another one below it has room."
            }
            Cap::Hard => "Balanced failover gives a provider no new work that would pass this.",
            Cap::Freeze => "Pinned freeze stops before new work would pass this.",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Cap::Soft => "soft",
            Cap::Hard => "hard",
            Cap::Freeze => "freeze",
        }
    }
}

pub const MODES: [(ProviderQuotaPolicy, &str, &str); 2] = [
    (
        ProviderQuotaPolicy::BalancedFailover,
        "Balanced failover",
        "Spreads new work by the room each provider has, and pauses when none can take it.",
    ),
    (
        ProviderQuotaPolicy::PinnedFreeze,
        "Pinned freeze",
        "Never switches provider; freezes before new work would pass the freeze cap.",
    ),
];

fn percent(fraction: f32) -> u8 {
    (fraction * 100.0).round().clamp(0.0, 100.0) as u8
}

fn fraction(percent: u8) -> f32 {
    f32::from(percent) / 100.0
}

/// A cap as the page shows it: whole percents, with one decimal only for a
/// saved value that has one.
pub fn percent_label(fraction: f32) -> String {
    let tenths = (fraction * 1000.0).round() as i32;
    if tenths % 10 == 0 {
        format!("{}%", tenths / 10)
    } else {
        format!("{}.{}%", tenths / 10, tenths % 10)
    }
}

impl Cap {
    pub fn get(self, settings: &QuotaSettings) -> f32 {
        match self {
            Cap::Soft => settings.soft_cap,
            Cap::Hard => settings.hard_cap,
            Cap::Freeze => settings.freeze_cap,
        }
    }

    fn set(self, settings: &mut QuotaSettings, value: f32) {
        match self {
            Cap::Soft => settings.soft_cap = value,
            Cap::Hard => settings.hard_cap = value,
            Cap::Freeze => settings.freeze_cap = value,
        }
    }
}

/// A loaded record: what the device has saved and what Save would send.
#[derive(Debug, Clone, PartialEq)]
pub struct CapEditor {
    pub saved: QuotaSettings,
    pub draft: QuotaSettings,
}

impl CapEditor {
    pub fn new(saved: QuotaSettings) -> Self {
        Self {
            draft: saved.clone(),
            saved,
        }
    }

    /// Move one cap by `delta` percent inside [`Cap::range`]. A value outside
    /// the range can only move towards it. Coming back to the saved percent
    /// restores the saved value exactly.
    pub fn step(&mut self, cap: Cap, delta: i8) {
        let (min, max) = cap.range();
        let current = i16::from(percent(cap.get(&self.draft)));
        let next = (current + i16::from(delta)).clamp(i16::from(min), i16::from(max));
        if (next - current).signum() != i16::from(delta).signum() {
            return;
        }
        let next = next as u8;
        let saved = cap.get(&self.saved);
        let value = if next == percent(saved) {
            saved
        } else {
            fraction(next)
        };
        cap.set(&mut self.draft, value);
    }

    /// Why the draft cannot be saved, in the words the page shows. The hard
    /// cap's range applies only once the user moves it, so a mode change can
    /// be saved beside a cap another client set outside that range.
    pub fn error(&self) -> Option<String> {
        self.global_error().or_else(|| self.provider_error())
    }

    /// The hard cap a provider is held to in the draft.
    pub fn provider_cap(&self, id: &str) -> f32 {
        self.draft.hard_cap_for(id)
    }

    /// Set a provider's hard cap to `percent` (clamped to the slider's range).
    /// Going back to the percent the provider has saved restores that saved
    /// value, or no entry at all when it had none.
    pub fn set_provider_cap(&mut self, id: &str, percent_value: u8) {
        let percent_value = percent_value.clamp(HARD_CAP_MIN_PERCENT, HARD_CAP_MAX_PERCENT);
        let saved = self.saved.hard_cap_for(id);
        if percent_value == percent(saved) {
            match self.saved.provider_hard_caps.get(id) {
                Some(exact) => self.draft.provider_hard_caps.insert(id.to_owned(), *exact),
                None => self.draft.provider_hard_caps.remove(id),
            };
        } else {
            self.draft
                .provider_hard_caps
                .insert(id.to_owned(), fraction(percent_value));
        }
    }

    /// Why a provider's hard cap cannot be saved. Its range applies only once
    /// the user moves it; its place between the soft and freeze caps always.
    fn provider_error(&self) -> Option<String> {
        let QuotaSettings {
            soft_cap,
            freeze_cap,
            ..
        } = self.draft;
        let range = fraction(HARD_CAP_MIN_PERCENT)..=fraction(HARD_CAP_MAX_PERCENT);
        for (id, cap) in &self.draft.provider_hard_caps {
            let name = PROVIDERS
                .iter()
                .find(|(key, ..)| key == id)
                .map_or(id.as_str(), |(_, name, _)| *name);
            let changed = self.saved.provider_hard_caps.get(id) != Some(cap);
            if changed && !range.contains(cap) {
                return Some(format!(
                    "The {name} hard cap must be between {HARD_CAP_MIN_PERCENT}% and \
                     {HARD_CAP_MAX_PERCENT}%."
                ));
            } else if *cap <= soft_cap {
                return Some(format!("The {name} hard cap must be above the soft cap."));
            } else if *cap >= freeze_cap {
                return Some(format!("The freeze cap must be above the {name} hard cap."));
            }
        }
        None
    }

    fn global_error(&self) -> Option<String> {
        let QuotaSettings {
            soft_cap,
            hard_cap,
            freeze_cap,
            ..
        } = self.draft;
        let hard_range = fraction(HARD_CAP_MIN_PERCENT)..=fraction(HARD_CAP_MAX_PERCENT);
        if hard_cap != self.saved.hard_cap && !hard_range.contains(&hard_cap) {
            Some(format!(
                "The hard cap must be between {HARD_CAP_MIN_PERCENT}% and \
                 {HARD_CAP_MAX_PERCENT}%."
            ))
        } else if soft_cap <= 0.0 {
            Some("The soft cap must be above 0%.".into())
        } else if soft_cap >= hard_cap {
            Some("The soft cap must be below the hard cap.".into())
        } else if hard_cap >= freeze_cap {
            Some("The freeze cap must be above the hard cap.".into())
        } else if freeze_cap > 1.0 {
            Some("The freeze cap cannot pass 100%.".into())
        } else {
            None
        }
    }

    pub fn dirty(&self) -> bool {
        self.draft != self.saved
    }

    pub fn can_save(&self) -> bool {
        self.dirty() && self.error().is_none()
    }
}

pub struct QuotaCaps {
    state: Entity<AppState>,
    /// `None` = this device, as on the Accounts page.
    target_device: Option<String>,
    editor: Loadable<CapEditor>,
    /// The usage windows behind the headroom bars, per provider.
    usage: Vec<(HarnessId, Vec<HeadroomWindow>)>,
    saving: bool,
    save_error: Option<SharedString>,
    /// The one load or save in flight. Replacing it drops the older reply, so
    /// a slow answer from another device can never land here.
    task: Option<Task<()>>,
}

impl QuotaCaps {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut section = Self {
            state,
            target_device: None,
            editor: Loadable::Idle,
            usage: Vec::new(),
            saving: false,
            save_error: None,
            task: None,
        };
        section.load(cx);
        section
    }

    /// Read the headroom windows of every provider from `source`. The page
    /// calls this whenever its account list changes.
    pub fn set_headroom(&mut self, source: &dyn HeadroomSource, cx: &mut Context<Self>) {
        self.usage = PROVIDERS
            .iter()
            .map(|(_, _, harness)| (*harness, source.windows(*harness)))
            .collect();
        cx.notify();
    }

    /// The headroom windows of one provider, as the bars show them.
    pub(crate) fn windows_of(&self, harness: HarnessId) -> &[HeadroomWindow] {
        self.usage
            .iter()
            .find(|(candidate, _)| *candidate == harness)
            .map(|(_, windows)| windows.as_slice())
            .unwrap_or_default()
    }

    /// A section that is already loaded, for the preview fixture: no request
    /// is sent.
    pub fn fixture(
        state: Entity<AppState>,
        settings: QuotaSettings,
        source: &dyn HeadroomSource,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut section = Self {
            state,
            target_device: None,
            editor: Loadable::Ready(CapEditor::new(settings)),
            usage: Vec::new(),
            saving: false,
            save_error: None,
            task: None,
        };
        section.set_headroom(source, cx);
        section
    }

    /// Follow the page's device switcher. The old device's draft is dropped.
    pub fn set_target_device(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        if self.target_device != target {
            self.target_device = target;
            self.load(cx);
        }
    }

    /// The page's Refresh: re-read, unless that would discard unsaved edits.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let editing = self.saving || self.editor.ready().is_some_and(CapEditor::dirty);
        if !editing {
            self.load(cx);
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.editor = Loadable::Loading;
        self.saving = false;
        self.save_error = None;
        self.request(methods::GET_QUOTA_SETTINGS, serde_json::json!({}), cx);
    }

    pub(crate) fn save(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.ready().cloned() else {
            return;
        };
        if self.saving || !editor.can_save() {
            return;
        }
        let params = match serde_json::to_value(&editor.draft) {
            Ok(params) => params,
            Err(error) => {
                self.save_error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        self.saving = true;
        self.save_error = None;
        self.request(methods::SET_QUOTA_SETTINGS, params, cx);
    }

    /// Send to the selected device; both methods reply with the saved record.
    fn request(&mut self, method: &'static str, params: serde_json::Value, cx: &mut Context<Self>) {
        let rpc = self
            .state
            .read(cx)
            .accounts_rpc(self.target_device.as_deref());
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = rpc.call(method, params).await.and_then(|value| {
                serde_json::from_value::<QuotaSettings>(value).map_err(|error| error.to_string())
            });
            this.update(cx, |section, cx| {
                section.finish(result);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn finish(&mut self, result: Result<QuotaSettings, String>) {
        let saving = std::mem::take(&mut self.saving);
        match result {
            Ok(settings) => self.editor = Loadable::Ready(CapEditor::new(settings)),
            // A refused save keeps the draft so the user can retry or fix it.
            Err(error) if saving => self.save_error = Some(error.into()),
            Err(error) => self.editor = Loadable::Error(error),
        }
    }

    /// Change the draft. Ignored while a save is in flight, whose reply would
    /// overwrite the change.
    pub(crate) fn edit(&mut self, change: impl FnOnce(&mut CapEditor), cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        if let Loadable::Ready(editor) = &mut self.editor {
            change(editor);
            self.save_error = None;
            cx.notify();
        }
    }

    fn render_modes(
        &self,
        draft: &QuotaSettings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        widgets::section_card(theme)
            .mt(px(8.0))
            .children(
                MODES
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (policy, title, description))| {
                        widgets::card_row(theme, ix == 0)
                            .id(("quota-mode", ix))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit(|editor| editor.draft.policy = policy, cx)
                            }))
                            .child(text_column(theme, title, description))
                            .when(draft.policy == policy, |el| {
                                el.child(widgets::badge_active(theme, "Selected"))
                            })
                    }),
            )
            .into_any_element()
    }

    /// One card per provider: its hard-cap slider and the headroom bars.
    fn render_providers(
        &self,
        editor: &CapEditor,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let now = chrono::Utc::now();
        widgets::section_card(theme)
            .mt(px(8.0))
            .children(PROVIDERS.into_iter().enumerate().map(|(ix, (id, name, harness))| {
                let cap = editor.provider_cap(id);
                let windows = self.windows_of(harness);
                widgets::card_row(theme, ix == 0)
                    .id(("quota-provider", ix))
                    .debug_selector(move || format!("quota-provider-{id}"))
                    .flex_col()
                    .items_stretch()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .child(text_column(
                                theme,
                                name,
                                "Hard cap: no new work that would pass it.",
                            ))
                            .child(
                                div()
                                    .id(("quota-provider-value", ix))
                                    .debug_selector(move || {
                                        format!("quota-cap-value-{id}-{}", percent(cap))
                                    })
                                    .flex_none()
                                    .text_size(crate::typography::ui_rems(widgets::ROW_TITLE_SIZE))
                                    .text_color(theme.text)
                                    .child(SharedString::from(percent_label(cap))),
                            ),
                    )
                    .child(self.render_slider(id, cap, theme, cx))
                    .child(render_headroom(windows, cap, theme, now))
            }))
            .into_any_element()
    }

    /// A slider of notches from the lowest to the highest hard cap. Click a
    /// notch to set the cap there.
    fn render_slider(
        &self,
        id: &'static str,
        cap: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = percent(cap);
        div()
            .flex()
            .flex_row()
            .gap(px(3.0))
            .children(
                (HARD_CAP_MIN_PERCENT..=HARD_CAP_MAX_PERCENT)
                    .step_by(usize::from(SLIDER_STEP_PERCENT))
                    .map(|notch| {
                        let filled = value >= notch;
                        div()
                            .id((id, usize::from(notch)))
                            .debug_selector(move || format!("quota-cap-{id}-{notch}"))
                            .flex_1()
                            .h(px(14.0))
                            .rounded(px(3.0))
                            .cursor_pointer()
                            .bg(if filled {
                                theme.accent.opacity(0.8)
                            } else {
                                crate::theme::ink(0.07)
                            })
                            .when(!self.saving, |el| {
                                el.hover(|s| s.opacity(0.8)).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.edit(|editor| editor.set_provider_cap(id, notch), cx)
                                    },
                                ))
                            })
                    }),
            )
            .into_any_element()
    }

    fn render_caps(&self, editor: &CapEditor, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        widgets::section_card(theme)
            .mt(px(8.0))
            .children(Cap::STEPPED.into_iter().enumerate().map(|(ix, cap)| {
                widgets::card_row(theme, ix == 0)
                    .child(text_column(theme, cap.title(), cap.description()))
                    .child(self.render_step(editor, cap, -1, theme, cx))
                    .child(
                        div()
                            .flex_none()
                            .w(px(44.0))
                            .text_center()
                            .text_size(crate::typography::ui_rems(widgets::ROW_TITLE_SIZE))
                            .text_color(theme.text)
                            .child(SharedString::from(percent_label(cap.get(&editor.draft)))),
                    )
                    .child(self.render_step(editor, cap, 1, theme, cx))
            }))
            .into_any_element()
    }

    fn render_step(
        &self,
        editor: &CapEditor,
        cap: Cap,
        delta: i8,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut next = editor.clone();
        next.step(cap, delta);
        let enabled = !self.saving && next != *editor;
        let (direction, glyph) = if delta < 0 {
            ("down", "\u{2212}")
        } else {
            ("up", "+")
        };
        widgets::ghost_action(theme)
            .id(SharedString::from(format!(
                "quota-cap-{}-{direction}",
                cap.slug()
            )))
            .flex_none()
            .when(enabled, |el| {
                el.hover(|s| widgets::ghost_hover(theme, s))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(|editor| editor.step(cap, delta), cx)
                    }))
            })
            .when(!enabled, |el| el.opacity(0.4))
            .child(SharedString::from(glyph))
            .into_any_element()
    }

    fn render_actions(
        &self,
        editor: &CapEditor,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let can_save = !self.saving && editor.can_save();
        let can_reset = !self.saving && editor.draft != QuotaSettings::default();
        let label = if self.saving {
            "Saving\u{2026}"
        } else {
            "Save"
        };
        div()
            .mt(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .gap(px(8.0))
            .child(
                widgets::ghost_action(theme)
                    .id("quota-caps-reset")
                    .debug_selector(|| "quota-caps-reset".into())
                    .when(can_reset, |el| {
                        el.hover(|s| widgets::ghost_hover(theme, s))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit(|editor| editor.draft = QuotaSettings::default(), cx)
                            }))
                    })
                    .when(!can_reset, |el| el.opacity(0.4))
                    .child(SharedString::from("Reset to defaults")),
            )
            .child(
                crate::components::button(
                    "quota-caps-save",
                    label,
                    ButtonVariant::Primary,
                    ButtonSize::Md,
                    theme,
                )
                .debug_selector(move || {
                    if can_save { "quota-caps-save-enabled" } else { "quota-caps-save-disabled" }.into()
                })
                .when(can_save, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| this.save(cx)))
                })
                .when(!can_save, |el| el.opacity(0.4).cursor_default()),
            )
            .into_any_element()
    }
}

fn text_column(theme: &Theme, title: &'static str, description: &'static str) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(widgets::row_title(theme, title))
        .child(
            div()
                .mt(px(2.0))
                .text_size(crate::typography::ui_rems(widgets::ROW_DESCRIPTION_SIZE))
                .text_color(theme.text_muted.opacity(0.65))
                .child(SharedString::from(description)),
        )
}

/// The headroom bars of one provider: its time windows, then its models.
fn render_headroom(
    windows: &[HeadroomWindow],
    cap: f32,
    theme: &Theme,
    now: chrono::DateTime<chrono::Utc>,
) -> AnyElement {
    let quiet = |text: &'static str| {
        div()
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text_muted.opacity(0.6))
            .child(SharedString::from(text))
    };
    if windows.is_empty() {
        return quiet("Usage unavailable").into_any_element();
    }
    let (models, times): (Vec<_>, Vec<_>) = windows.iter().partition(|window| window.model.is_some());
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .children(times.into_iter().map(|window| headroom_bar(window, cap, theme, now)))
        .when(!models.is_empty(), |el| {
            el.child(
                div()
                    .mt(px(2.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted.opacity(0.5))
                    .child(SharedString::from("Per model")),
            )
        })
        .children(models.into_iter().map(|window| headroom_bar(window, cap, theme, now)))
        .into_any_element()
}

/// One window: label · bar with the cap marked · percent used · room left.
fn headroom_bar(
    window: &HeadroomWindow,
    cap: f32,
    theme: &Theme,
    now: chrono::DateTime<chrono::Utc>,
) -> AnyElement {
    let headroom = Headroom::new(window.used, cap);
    let fill = match headroom.state() {
        HeadroomState::Clear => theme.accent.opacity(0.8),
        HeadroomState::Close => theme.warning.opacity(0.85),
        HeadroomState::Over => theme.danger.opacity(0.85),
    };
    let reset = crate::settings::accounts::format_reset(window.resets_at, now);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .text_size(crate::typography::ui_rems(11.5))
        .text_color(theme.text_muted.opacity(0.7))
        .child(
            div()
                .w(px(72.0))
                .flex_none()
                .truncate()
                .child(SharedString::from(window.label.clone())),
        )
        .child(
            // The track is taller than the bar so the cap marker reads as a
            // tick across it.
            div()
                .relative()
                .flex_1()
                .min_w(px(56.0))
                .max_w(px(260.0))
                .h(px(11.0))
                .child(
                    div()
                        .absolute()
                        .top(px(3.0))
                        .left_0()
                        .w_full()
                        .h(px(5.0))
                        .rounded_full()
                        .overflow_hidden()
                        .bg(crate::theme::ink(0.07))
                        .when(headroom.used > 0.0, |el| {
                            el.child(
                                div()
                                    .h_full()
                                    .w(gpui::relative(headroom.used.max(0.015)))
                                    .rounded_full()
                                    .bg(fill),
                            )
                        }),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("cap-marker-{}", window.label)))
                        .absolute()
                        .top_0()
                        .left(gpui::relative(headroom.cap))
                        .w(px(2.0))
                        .h(px(11.0))
                        .rounded(px(1.0))
                        .bg(theme.text.opacity(0.75)),
                ),
        )
        .child(
            div()
                .w(px(64.0))
                .flex_none()
                .text_right()
                .child(SharedString::from(format!(
                    "{}% used",
                    (headroom.used * 100.0).round() as u32
                ))),
        )
        .child(
            div()
                .flex_none()
                .w(px(104.0))
                .truncate()
                .text_color(match headroom.state() {
                    HeadroomState::Over => theme.danger,
                    HeadroomState::Close => theme.warning,
                    HeadroomState::Clear => theme.text_muted.opacity(0.7),
                })
                .child(SharedString::from(headroom.label())),
        )
        .when_some(reset, |el, reset| {
            el.child(
                div()
                    .flex_none()
                    .truncate()
                    .text_color(theme.text_muted.opacity(0.45))
                    .child(SharedString::from(reset)),
            )
        })
        .into_any_element()
}

impl Render for QuotaCaps {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let section = div()
            .id("quota-caps-section")
            .debug_selector(|| "quota-caps-section".into())
            .mt(px(32.0))
            .flex()
            .flex_col()
            .child(widgets::field_label(&theme, "Quota caps"))
            .child(widgets::page_subtitle(&theme, SECTION_COPY))
            .child(widgets::page_subtitle(&theme, NOT_WIRED_COPY));
        match &self.editor {
            Loadable::Idle | Loadable::Loading => {
                section.child(widgets::page_subtitle(&theme, "Loading\u{2026}"))
            }
            Loadable::Error(message) => section.child(
                widgets::error_strip(
                    &theme,
                    format!("Couldn\u{2019}t load quota caps: {message}"),
                )
                .id("quota-caps-load-error")
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| this.load(cx)))
                .child(
                    div()
                        .mt(px(4.0))
                        .text_size(crate::typography::ui_rems(widgets::ROW_DESCRIPTION_SIZE))
                        .text_color(theme.text_muted)
                        .child(SharedString::from("Click to retry")),
                ),
            ),
            Loadable::Ready(editor) => {
                let editor = editor.clone();
                let error = editor
                    .error()
                    .map(SharedString::from)
                    .or_else(|| self.save_error.clone());
                section
                    .child(self.render_modes(&editor.draft, &theme, cx))
                    .child(self.render_providers(&editor, &theme, cx))
                    .child(self.render_caps(&editor, &theme, cx))
                    .when_some(error, |el, message| {
                        el.child(
                            widgets::error_strip(&theme, message)
                                .id("quota-caps-error")
                                .debug_selector(|| "quota-caps-error".into()),
                        )
                    })
                    .child(self.render_actions(&editor, &theme, cx))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn record(soft_cap: f32, hard_cap: f32, freeze_cap: f32) -> QuotaSettings {
        QuotaSettings {
            policy: ProviderQuotaPolicy::BalancedFailover,
            soft_cap,
            hard_cap,
            freeze_cap,
            provider_hard_caps: Default::default(),
        }
    }

    /// A draft of whole percents edited over the saved defaults.
    fn drafted(soft: u8, hard: u8, freeze: u8) -> CapEditor {
        let mut editor = CapEditor::new(QuotaSettings::default());
        editor.draft = record(fraction(soft), fraction(hard), fraction(freeze));
        editor
    }

    #[test]
    fn ordering_is_enforced_with_an_inline_error() {
        assert_eq!(drafted(78, 85, 90).error(), None);
        assert_eq!(drafted(1, 50, 100).error(), None);
        for (soft, hard, freeze) in [
            (0, 85, 90),
            (85, 85, 90),
            (86, 85, 90),
            (78, 90, 90),
            (78, 91, 90),
            (78, 85, 101),
        ] {
            let editor = drafted(soft, hard, freeze);
            assert!(editor.error().is_some(), "{soft} {hard} {freeze}");
            assert!(!editor.can_save());
        }
    }

    #[test]
    fn every_savable_draft_passes_the_engine_check() {
        for soft in 0..=100u8 {
            for hard in 0..=100u8 {
                for freeze in [soft, hard, hard.saturating_add(1).min(100), 100] {
                    let editor = drafted(soft, hard, freeze);
                    if editor.error().is_none() {
                        editor.draft.validate().unwrap();
                    }
                }
            }
        }
    }

    #[test]
    fn an_edited_hard_cap_stays_between_50_and_95() {
        assert!(drafted(40, 49, 90).error().is_some());
        assert!(drafted(40, 96, 100).error().is_some());
        assert_eq!(drafted(40, 50, 90).error(), None);
        assert_eq!(drafted(40, 95, 96).error(), None);

        let mut low = CapEditor::new(record(0.4, 0.5, 0.9));
        low.step(Cap::Hard, -1);
        assert_eq!(low.draft.hard_cap, 0.5);
        low.step(Cap::Hard, 1);
        assert_eq!(low.draft.hard_cap, 0.51);

        let mut high = CapEditor::new(record(0.4, 0.95, 1.0));
        high.step(Cap::Hard, 1);
        high.step(Cap::Freeze, 1);
        assert!(!high.dirty());
    }

    #[test]
    fn out_of_range_saved_caps_show_and_do_not_block_a_mode_change() {
        for saved in [record(0.2, 0.4, 0.9), record(0.5, 0.97, 0.99)] {
            let mut editor = CapEditor::new(saved.clone());
            assert_eq!(editor.error(), None);
            editor.draft.policy = ProviderQuotaPolicy::PinnedFreeze;
            assert!(editor.can_save());
            assert_eq!(
                (
                    editor.draft.soft_cap,
                    editor.draft.hard_cap,
                    editor.draft.freeze_cap
                ),
                (saved.soft_cap, saved.hard_cap, saved.freeze_cap)
            );
        }
        // A stepper only moves such a value towards the range, never the
        // opposite way to the button pressed.
        let mut below = CapEditor::new(record(0.2, 0.4, 0.9));
        below.step(Cap::Hard, -1);
        assert!(!below.dirty());
        below.step(Cap::Hard, 1);
        assert_eq!(below.draft.hard_cap, 0.5);
        let mut above = CapEditor::new(record(0.5, 0.97, 0.99));
        above.step(Cap::Hard, 1);
        assert!(!above.dirty());
        above.step(Cap::Hard, -1);
        assert_eq!(above.draft.hard_cap, 0.95);
    }

    #[test]
    fn unedited_fractional_caps_are_not_rounded() {
        // Both round to 78%, yet they are validly ordered.
        let saved = record(0.781, 0.784, 0.9);
        let mut editor = CapEditor::new(saved.clone());
        assert_eq!(editor.error(), None);
        editor.draft.policy = ProviderQuotaPolicy::PinnedFreeze;
        assert!(editor.can_save());
        editor.step(Cap::Freeze, 1);
        assert_eq!(editor.draft.freeze_cap, 0.91);
        assert_eq!(
            (editor.draft.soft_cap, editor.draft.hard_cap),
            (0.781, 0.784)
        );

        // Stepping away and back restores the saved value.
        editor.step(Cap::Soft, -1);
        assert_eq!(editor.draft.soft_cap, 0.77);
        editor.step(Cap::Soft, 1);
        assert_eq!(editor.draft.soft_cap, 0.781);

        assert_eq!(percent_label(saved.soft_cap), "78.1%");
        assert_eq!(percent_label(saved.hard_cap), "78.4%");
        assert_eq!(percent_label(QuotaSettings::default().hard_cap), "85%");
        assert_eq!(percent_label(1.0), "100%");
    }

    #[test]
    fn soft_and_freeze_may_cross_the_hard_cap_but_cannot_be_saved() {
        let mut editor = CapEditor::new(QuotaSettings::default());
        assert!(!editor.can_save(), "unchanged");
        for _ in 0..7 {
            editor.step(Cap::Soft, 1);
        }
        assert_eq!(editor.draft.soft_cap, 0.85);
        assert!(editor.dirty() && !editor.can_save());
        editor.step(Cap::Hard, 1);
        assert!(editor.can_save());
    }

    #[test]
    fn reset_restores_the_exact_defaults() {
        let mut editor = CapEditor::new(record(0.781, 0.97, 0.99));
        editor.draft = QuotaSettings::default();
        assert!(editor.can_save());
        assert_eq!(editor.draft, record(0.78, 0.85, 0.90));
    }

    #[test]
    fn provider_caps_default_to_85_and_move_in_the_slider_range() {
        let mut editor = CapEditor::new(QuotaSettings::default());
        for (id, _, _) in PROVIDERS {
            assert_eq!(percent_label(editor.provider_cap(id)), "85%");
        }
        editor.set_provider_cap("codex", 80);
        assert_eq!(editor.draft.provider_hard_caps["codex"], 0.80);
        assert_eq!(percent_label(editor.provider_cap("claude-code")), "85%");
        assert!(editor.can_save());
        // Past either end of the slider is clamped, never saved out of range.
        editor.set_provider_cap("codex", 10);
        assert_eq!(editor.draft.provider_hard_caps["codex"], 0.50);
        editor.set_provider_cap("codex", 99);
        assert_eq!(editor.draft.provider_hard_caps["codex"], 0.95);
        // Back at the saved percent: no entry, so nothing to save.
        editor.set_provider_cap("codex", 85);
        assert!(editor.draft.provider_hard_caps.is_empty());
        assert!(!editor.dirty());
    }

    #[test]
    fn a_saved_provider_cap_comes_back_exactly() {
        let mut saved = QuotaSettings::default();
        saved.provider_hard_caps.insert("codex".into(), 0.731);
        let mut editor = CapEditor::new(saved);
        assert_eq!(percent_label(editor.provider_cap("codex")), "73.1%");
        editor.set_provider_cap("codex", 80);
        assert!(editor.dirty());
        editor.set_provider_cap("codex", 73);
        assert!(!editor.dirty());
        assert_eq!(editor.draft.provider_hard_caps["codex"], 0.731);
    }

    #[test]
    fn provider_caps_must_sit_between_the_soft_and_freeze_caps() {
        let mut editor = CapEditor::new(QuotaSettings::default());
        editor.set_provider_cap("claude-code", 80);
        assert_eq!(editor.error(), None);
        // Raising the soft cap (78%) to the provider's 80% is refused by name.
        editor.step(Cap::Soft, 1);
        editor.step(Cap::Soft, 1);
        let error = editor.error().unwrap();
        assert_eq!(error, "The Claude Code hard cap must be above the soft cap.");
        assert!(!editor.can_save());

        let mut editor = CapEditor::new(QuotaSettings::default());
        editor.set_provider_cap("antigravity", 95);
        for _ in 0..4 {
            editor.step(Cap::Freeze, -1);
        }
        assert_eq!(editor.draft.freeze_cap, 0.86);
        let error = editor.error().unwrap();
        assert_eq!(error, "The freeze cap must be above the Antigravity hard cap.");
        assert!(!editor.can_save());
    }

    type Calls = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    /// A section for local device `here`, answered by a script that echoes
    /// saves and fails them while `refuse` is set.
    fn scripted(cx: &mut gpui::TestAppContext) -> (Entity<QuotaCaps>, Calls, Arc<Mutex<bool>>) {
        let calls: Calls = Arc::default();
        let refuse: Arc<Mutex<bool>> = Arc::default();
        let (seen, refusing) = (calls.clone(), refuse.clone());
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.local_device_id = Some("here".into());
            state.accounts_rpc_override = Some(Arc::new(move |method, params| {
                seen.lock()
                    .unwrap()
                    .push((method.to_string(), params.clone()));
                match method {
                    methods::GET_QUOTA_SETTINGS => {
                        Ok(serde_json::to_value(QuotaSettings::default()).unwrap())
                    }
                    _ if *refusing.lock().unwrap() => Err("refused".into()),
                    _ => Ok(params),
                }
            }));
            state
        });
        let section = cx.new(|cx| QuotaCaps::new(state, cx));
        cx.run_until_parked();
        (section, calls, refuse)
    }

    fn editor(section: &Entity<QuotaCaps>, cx: &mut gpui::TestAppContext) -> CapEditor {
        section.read_with(cx, |section, _| section.editor.ready().unwrap().clone())
    }

    #[gpui::test]
    fn loads_and_saves_go_to_the_selected_device(cx: &mut gpui::TestAppContext) {
        let (section, calls, _) = scripted(cx);
        section.update(cx, |section, cx| {
            section.set_target_device(Some("there".into()), cx)
        });
        cx.run_until_parked();
        section.update(cx, |section, cx| {
            section.edit(
                |editor| {
                    editor.draft.policy = ProviderQuotaPolicy::PinnedFreeze;
                    editor.step(Cap::Hard, -1);
                },
                cx,
            );
            section.save(cx);
        });
        cx.run_until_parked();

        let calls = calls.lock().unwrap();
        let methods_sent: Vec<&str> = calls.iter().map(|(method, _)| method.as_str()).collect();
        assert_eq!(
            methods_sent,
            [
                methods::GET_QUOTA_SETTINGS,
                methods::GET_QUOTA_SETTINGS,
                methods::SET_QUOTA_SETTINGS
            ]
        );
        assert!(calls[0].1.get("targetDeviceId").is_none(), "this device");
        assert_eq!(calls[1].1["targetDeviceId"], "there");
        let sent = &calls[2].1;
        assert_eq!(sent["targetDeviceId"], "there");
        assert_eq!(sent["policy"], "pinnedFreeze");
        let parsed: QuotaSettings = serde_json::from_value(sent.clone()).unwrap();
        assert_eq!(
            (parsed.soft_cap, parsed.hard_cap, parsed.freeze_cap),
            (0.78, 0.84, 0.90)
        );
        drop(calls);
        assert!(!editor(&section, cx).dirty());
    }

    #[gpui::test]
    fn invalid_or_unchanged_drafts_are_never_sent(cx: &mut gpui::TestAppContext) {
        let (section, calls, _) = scripted(cx);
        section.update(cx, |section, cx| {
            section.save(cx);
            section.edit(|editor| editor.draft.soft_cap = editor.draft.hard_cap, cx);
            section.save(cx);
        });
        cx.run_until_parked();
        assert_eq!(calls.lock().unwrap().len(), 1, "only the first load");
    }

    #[gpui::test]
    fn a_refused_save_keeps_the_draft(cx: &mut gpui::TestAppContext) {
        let (section, calls, refuse) = scripted(cx);
        *refuse.lock().unwrap() = true;
        section.update(cx, |section, cx| {
            section.edit(|editor| editor.step(Cap::Freeze, 1), cx);
            section.save(cx);
        });
        cx.run_until_parked();
        let kept = editor(&section, cx);
        assert_eq!((kept.draft.freeze_cap, kept.saved.freeze_cap), (0.91, 0.90));
        section.read_with(cx, |section, _| {
            assert!(!section.saving);
            assert_eq!(section.save_error.as_deref(), Some("refused"));
        });

        // Refresh must not throw the unsaved edit away.
        section.update(cx, |section, cx| section.refresh(cx));
        cx.run_until_parked();
        assert_eq!(editor(&section, cx).draft.freeze_cap, 0.91);
        assert_eq!(calls.lock().unwrap().len(), 2);

        // Reset changes the draft only; the saved record needs a Save.
        section.update(cx, |section, cx| {
            section.edit(|editor| editor.draft = QuotaSettings::default(), cx)
        });
        assert!(!editor(&section, cx).dirty());
        assert_eq!(calls.lock().unwrap().len(), 2);
    }

    #[gpui::test]
    fn switching_device_drops_the_other_devices_draft(cx: &mut gpui::TestAppContext) {
        let (section, _, _) = scripted(cx);
        section.update(cx, |section, cx| {
            section.edit(|editor| editor.step(Cap::Hard, -1), cx);
            section.set_target_device(Some("there".into()), cx);
            assert!(
                section.editor.ready().is_none(),
                "no stale draft while loading"
            );
        });
        cx.run_until_parked();
        assert!(!editor(&section, cx).dirty());
    }
}
