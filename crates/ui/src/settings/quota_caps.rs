//! Settings → Accounts, "Quota caps": the cap mode and the global soft, hard
//! and freeze caps of the device picked in the Accounts page's device
//! switcher (`GetQuotaSettings` / `SetQuotaSettings`).
//!
//! Edits stay in a draft of whole percents until Save, which is disabled while
//! the draft is invalid or unchanged. No dispatch path reads the caps yet, and
//! the section's copy says so.

use gpui::{AnyElement, Context, Entity, SharedString, Task, Window, div, prelude::*, px};

use mast_engine::quota_settings::QuotaSettings;
use mast_proto::ProviderQuotaPolicy;
use mast_rpc::methods;

use crate::components::{ButtonSize, ButtonVariant};
use crate::popover::Loadable;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// The range Settings offers for the hard cap, in percent.
pub const HARD_CAP_MIN_PERCENT: u8 = 50;
pub const HARD_CAP_MAX_PERCENT: u8 = 95;

const SECTION_COPY: &str = "Caps reserve the rest of each provider\u{2019}s plan for your \
    interactive chats. They apply to every provider alike.";
const NOT_WIRED_COPY: &str = "Quota-aware dispatch is upcoming. Your choices are saved now and \
    take effect once it is on; until then nothing in Mast acts on them.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Soft,
    Hard,
    Freeze,
}

impl Cap {
    pub const ALL: [Cap; 3] = [Cap::Soft, Cap::Hard, Cap::Freeze];

    /// What a stepper can reach. Ordering between the caps is not clamped: it
    /// is reported by [`CapDraft::error`] so one cap can pass through another's
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

/// The settings as the controls show them: caps in whole percents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapDraft {
    pub policy: ProviderQuotaPolicy,
    pub soft: u8,
    pub hard: u8,
    pub freeze: u8,
}

fn percent(fraction: f32) -> u8 {
    (fraction * 100.0).round().clamp(0.0, 100.0) as u8
}

fn fraction(percent: u8) -> f32 {
    f32::from(percent) / 100.0
}

impl CapDraft {
    pub fn from_settings(settings: &QuotaSettings) -> Self {
        Self {
            policy: settings.policy,
            soft: percent(settings.soft_cap),
            hard: percent(settings.hard_cap),
            freeze: percent(settings.freeze_cap),
        }
    }

    pub fn to_settings(self) -> QuotaSettings {
        QuotaSettings {
            policy: self.policy,
            soft_cap: fraction(self.soft),
            hard_cap: fraction(self.hard),
            freeze_cap: fraction(self.freeze),
        }
    }

    pub fn defaults() -> Self {
        Self::from_settings(&QuotaSettings::default())
    }

    pub fn get(self, cap: Cap) -> u8 {
        match cap {
            Cap::Soft => self.soft,
            Cap::Hard => self.hard,
            Cap::Freeze => self.freeze,
        }
    }

    /// Move one cap by `delta` percent, staying inside [`Cap::range`]. A
    /// stored value outside the range lands on its nearest edge.
    pub fn step(&mut self, cap: Cap, delta: i8) {
        let (min, max) = cap.range();
        let next = (i16::from(self.get(cap)) + i16::from(delta))
            .clamp(i16::from(min), i16::from(max)) as u8;
        match cap {
            Cap::Soft => self.soft = next,
            Cap::Hard => self.hard = next,
            Cap::Freeze => self.freeze = next,
        }
    }

    /// Why these caps cannot be saved, in the words the page shows.
    pub fn error(self) -> Option<String> {
        if !(HARD_CAP_MIN_PERCENT..=HARD_CAP_MAX_PERCENT).contains(&self.hard) {
            Some(format!(
                "The hard cap must be between {HARD_CAP_MIN_PERCENT}% and \
                 {HARD_CAP_MAX_PERCENT}%."
            ))
        } else if self.soft == 0 {
            Some("The soft cap must be above 0%.".into())
        } else if self.soft >= self.hard {
            Some("The soft cap must be below the hard cap.".into())
        } else if self.hard >= self.freeze {
            Some("The freeze cap must be above the hard cap.".into())
        } else if self.freeze > 100 {
            Some("The freeze cap cannot pass 100%.".into())
        } else {
            None
        }
    }
}

/// A loaded record: what the device has saved and what the controls show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapEditor {
    pub saved: CapDraft,
    pub draft: CapDraft,
}

impl CapEditor {
    fn new(settings: &QuotaSettings) -> Self {
        let saved = CapDraft::from_settings(settings);
        Self {
            saved,
            draft: saved,
        }
    }

    pub fn dirty(&self) -> bool {
        self.draft != self.saved
    }

    pub fn can_save(&self) -> bool {
        self.dirty() && self.draft.error().is_none()
    }
}

pub struct QuotaCaps {
    state: Entity<AppState>,
    /// `None` = this device, as on the Accounts page.
    target_device: Option<String>,
    editor: Loadable<CapEditor>,
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
            saving: false,
            save_error: None,
            task: None,
        };
        section.load(cx);
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

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.ready().copied() else {
            return;
        };
        if self.saving || !editor.can_save() {
            return;
        }
        let params = match serde_json::to_value(editor.draft.to_settings()) {
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
            Ok(settings) => self.editor = Loadable::Ready(CapEditor::new(&settings)),
            // A refused save keeps the draft so the user can retry or fix it.
            Err(error) if saving => self.save_error = Some(error.into()),
            Err(error) => self.editor = Loadable::Error(error),
        }
    }

    /// Change the draft. Ignored while a save is in flight, whose reply would
    /// overwrite the change.
    fn edit(&mut self, change: impl FnOnce(&mut CapDraft), cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        if let Loadable::Ready(editor) = &mut self.editor {
            change(&mut editor.draft);
            self.save_error = None;
            cx.notify();
        }
    }

    fn render_modes(&self, draft: CapDraft, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
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
                                this.edit(|draft| draft.policy = policy, cx)
                            }))
                            .child(text_column(theme, title, description))
                            .when(draft.policy == policy, |el| {
                                el.child(widgets::badge_active(theme, "Selected"))
                            })
                    }),
            )
            .into_any_element()
    }

    fn render_caps(&self, draft: CapDraft, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        widgets::section_card(theme)
            .mt(px(8.0))
            .children(Cap::ALL.into_iter().enumerate().map(|(ix, cap)| {
                widgets::card_row(theme, ix == 0)
                    .child(text_column(theme, cap.title(), cap.description()))
                    .child(self.render_step(draft, cap, -1, theme, cx))
                    .child(
                        div()
                            .flex_none()
                            .w(px(44.0))
                            .text_center()
                            .text_size(crate::typography::ui_rems(13.0))
                            .text_color(theme.text)
                            .child(SharedString::from(format!("{}%", draft.get(cap)))),
                    )
                    .child(self.render_step(draft, cap, 1, theme, cx))
            }))
            .into_any_element()
    }

    fn render_step(
        &self,
        draft: CapDraft,
        cap: Cap,
        delta: i8,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut next = draft;
        next.step(cap, delta);
        let enabled = !self.saving && next != draft;
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
                el.hover(|s| widgets::ghost_hover(theme, s)).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.edit(|draft| draft.step(cap, delta), cx)
                    }),
                )
            })
            .when(!enabled, |el| el.opacity(0.4))
            .child(SharedString::from(glyph))
            .into_any_element()
    }

    fn render_actions(
        &self,
        editor: CapEditor,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let can_save = !self.saving && editor.can_save();
        let can_reset = !self.saving && editor.draft != CapDraft::defaults();
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
                    .when(can_reset, |el| {
                        el.hover(|s| widgets::ghost_hover(theme, s))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit(|draft| *draft = CapDraft::defaults(), cx)
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

impl Render for QuotaCaps {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let section = div()
            .id("quota-caps-section")
            .mt(px(32.0))
            .flex()
            .flex_col()
            .child(
                div()
                    .text_size(crate::typography::ui_rems(14.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from("Quota caps")),
            )
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
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from("Click to retry")),
                ),
            ),
            Loadable::Ready(editor) => {
                let editor = *editor;
                let error = editor
                    .draft
                    .error()
                    .map(SharedString::from)
                    .or_else(|| self.save_error.clone());
                section
                    .child(self.render_modes(editor.draft, &theme, cx))
                    .child(self.render_caps(editor.draft, &theme, cx))
                    .when_some(error, |el, message| {
                        el.child(widgets::error_strip(&theme, message))
                    })
                    .child(self.render_actions(editor, &theme, cx))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn draft(soft: u8, hard: u8, freeze: u8) -> CapDraft {
        CapDraft {
            policy: ProviderQuotaPolicy::BalancedFailover,
            soft,
            hard,
            freeze,
        }
    }

    #[test]
    fn ordering_is_enforced_with_an_inline_error() {
        assert_eq!(draft(78, 85, 90).error(), None);
        assert_eq!(draft(1, 50, 100).error(), None);
        for bad in [
            draft(0, 85, 90),
            draft(85, 85, 90),
            draft(86, 85, 90),
            draft(78, 90, 90),
            draft(78, 91, 90),
            draft(78, 85, 101),
        ] {
            assert!(bad.error().is_some(), "{bad:?}");
        }
    }

    #[test]
    fn every_savable_draft_passes_the_engine_check() {
        for soft in 0..=100 {
            for hard in 0..=100 {
                for freeze in [soft, hard, hard.saturating_add(1).min(100), 100] {
                    let candidate = draft(soft, hard, freeze);
                    if candidate.error().is_none() {
                        candidate.to_settings().validate().unwrap();
                        assert_eq!(CapDraft::from_settings(&candidate.to_settings()), candidate);
                    }
                }
            }
        }
    }

    #[test]
    fn hard_cap_stays_between_50_and_95() {
        assert!(draft(40, 49, 90).error().is_some());
        assert!(draft(40, 96, 100).error().is_some());
        assert_eq!(draft(40, 50, 90).error(), None);
        assert_eq!(draft(40, 95, 96).error(), None);

        let mut low = draft(40, 50, 90);
        low.step(Cap::Hard, -1);
        assert_eq!(low.hard, 50);
        low.step(Cap::Hard, 1);
        assert_eq!(low.hard, 51);

        let mut high = draft(40, 95, 100);
        high.step(Cap::Hard, 1);
        assert_eq!(high.hard, 95);
        high.step(Cap::Freeze, 1);
        assert_eq!(high.freeze, 100);

        // A value saved by another client lands on the nearest edge.
        let mut outside = draft(30, 40, 90);
        outside.step(Cap::Hard, 1);
        assert_eq!(outside.hard, 50);
    }

    #[test]
    fn soft_and_freeze_may_cross_the_hard_cap_but_cannot_be_saved() {
        let mut editor = CapEditor::new(&QuotaSettings::default());
        assert!(!editor.can_save(), "unchanged");
        for _ in 0..7 {
            editor.draft.step(Cap::Soft, 1);
        }
        assert_eq!(editor.draft.soft, 85);
        assert!(editor.dirty() && !editor.can_save());
        editor.draft.step(Cap::Hard, 1);
        assert!(editor.can_save());
    }

    #[test]
    fn reset_restores_the_defaults() {
        let defaults = CapDraft::defaults();
        assert_eq!(defaults, draft(78, 85, 90));
        assert_eq!(defaults.to_settings(), QuotaSettings::default());
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
        section.read_with(cx, |section, _| *section.editor.ready().unwrap())
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
                |draft| {
                    draft.policy = ProviderQuotaPolicy::PinnedFreeze;
                    draft.step(Cap::Hard, -1);
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
            section.edit(|draft| draft.soft = draft.hard, cx);
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
            section.edit(|draft| draft.step(Cap::Freeze, 1), cx);
            section.save(cx);
        });
        cx.run_until_parked();
        let kept = editor(&section, cx);
        assert_eq!((kept.draft.freeze, kept.saved.freeze), (91, 90));
        section.read_with(cx, |section, _| {
            assert!(!section.saving);
            assert_eq!(section.save_error.as_deref(), Some("refused"));
        });

        // Refresh must not throw the unsaved edit away.
        section.update(cx, |section, cx| section.refresh(cx));
        cx.run_until_parked();
        assert_eq!(editor(&section, cx).draft.freeze, 91);
        assert_eq!(calls.lock().unwrap().len(), 2);

        // Reset changes the draft only; the saved record needs a Save.
        section.update(cx, |section, cx| {
            section.edit(|draft| *draft = CapDraft::defaults(), cx)
        });
        assert!(!editor(&section, cx).dirty());
        assert_eq!(calls.lock().unwrap().len(), 2);
    }

    #[gpui::test]
    fn switching_device_drops_the_other_devices_draft(cx: &mut gpui::TestAppContext) {
        let (section, _, _) = scripted(cx);
        section.update(cx, |section, cx| {
            section.edit(|draft| draft.step(Cap::Hard, -1), cx);
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
