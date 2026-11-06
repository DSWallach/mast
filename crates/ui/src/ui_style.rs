//! Device-local presentation style and its structural metrics.

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UiStyleId {
    #[default]
    Minimal,
    Dense,
    Guided,
    Industrial,
}

impl<'de> Deserialize<'de> for UiStyleId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "dense" => Self::Dense,
            "guided" => Self::Guided,
            "industrial" => Self::Industrial,
            _ => Self::Minimal,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyleTokens {
    pub type_scale: f32,
    pub sidebar_row_compact_h: f32,
    pub sidebar_row_extra_h: f32,
    pub outline_row_delta: f32,
    pub control_size: f32,
    pub control_icon_size: f32,
    /// `None` preserves the primitive's existing geometry.
    pub control_radius: Option<f32>,
    pub panel_radius: Option<f32>,
    pub chip_radius: Option<f32>,
    pub rule_width: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceModel {
    ThemeDefault,
    Opaque,
    Glass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlVariant {
    Minimal,
    Key,
    FlatOutline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusIndicator {
    Minimal,
    Dot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontPreference {
    UserDefault,
    Geist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairedTheme {
    pub light: &'static str,
    pub dark: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelMode {
    IconOnly,
    Primary,
    All,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reveal {
    Hover,
    Always,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintMode {
    Tooltip,
    Inline,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Soft,
    Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleBehavior {
    pub labels: LabelMode,
    pub reveal: Reveal,
    pub hints: HintMode,
    pub motion: Motion,
    pub status_words: bool,
    pub help_cards: bool,
    pub status_bar: bool,
    pub outline_columns: bool,
    pub walkthrough_rail: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiStyle {
    pub id: UiStyleId,
    pub tokens: StyleTokens,
    pub behavior: StyleBehavior,
    pub surface: SurfaceModel,
    pub controls: ControlVariant,
    pub status_indicator: StatusIndicator,
    pub font_preference: FontPreference,
    pub paired_theme: Option<PairedTheme>,
}

impl UiStyle {
    pub const MINIMAL: Self = Self {
        id: UiStyleId::Minimal,
        tokens: StyleTokens {
            type_scale: 1.0,
            sidebar_row_compact_h: 29.0,
            sidebar_row_extra_h: 0.0,
            outline_row_delta: 0.0,
            control_size: 28.0,
            control_icon_size: 16.0,
            control_radius: None,
            panel_radius: None,
            chip_radius: None,
            rule_width: None,
        },
        surface: SurfaceModel::ThemeDefault,
        controls: ControlVariant::Minimal,
        status_indicator: StatusIndicator::Minimal,
        font_preference: FontPreference::UserDefault,
        paired_theme: None,
        behavior: StyleBehavior {
            labels: LabelMode::IconOnly,
            reveal: Reveal::Hover,
            hints: HintMode::Tooltip,
            motion: Motion::Soft,
            status_words: false,
            help_cards: false,
            status_bar: false,
            outline_columns: false,
            walkthrough_rail: false,
        },
    };
    pub const DENSE: Self = Self {
        id: UiStyleId::Dense,
        tokens: StyleTokens {
            type_scale: 0.9375,
            sidebar_row_compact_h: 22.0,
            sidebar_row_extra_h: 0.0,
            outline_row_delta: -4.0,
            control_size: 24.0,
            control_icon_size: 14.0,
            control_radius: None,
            panel_radius: None,
            chip_radius: None,
            rule_width: None,
        },
        surface: SurfaceModel::ThemeDefault,
        controls: ControlVariant::Minimal,
        status_indicator: StatusIndicator::Minimal,
        font_preference: FontPreference::UserDefault,
        paired_theme: None,
        behavior: StyleBehavior {
            labels: LabelMode::Primary,
            reveal: Reveal::Always,
            hints: HintMode::Inline,
            motion: Motion::Instant,
            status_words: true,
            help_cards: false,
            status_bar: true,
            outline_columns: true,
            walkthrough_rail: true,
        },
    };
    pub const GUIDED: Self = Self {
        id: UiStyleId::Guided,
        tokens: StyleTokens {
            type_scale: 1.0,
            sidebar_row_compact_h: 29.0,
            sidebar_row_extra_h: 4.0,
            outline_row_delta: 6.0,
            control_size: 32.0,
            control_icon_size: 16.0,
            control_radius: None,
            panel_radius: None,
            chip_radius: None,
            rule_width: None,
        },
        surface: SurfaceModel::ThemeDefault,
        controls: ControlVariant::Minimal,
        status_indicator: StatusIndicator::Minimal,
        font_preference: FontPreference::UserDefault,
        paired_theme: None,
        behavior: StyleBehavior {
            labels: LabelMode::All,
            reveal: Reveal::Always,
            hints: HintMode::Inline,
            motion: Motion::Soft,
            status_words: true,
            help_cards: true,
            status_bar: false,
            outline_columns: false,
            walkthrough_rail: false,
        },
    };
    pub const INDUSTRIAL: Self = Self {
        id: UiStyleId::Industrial,
        tokens: StyleTokens {
            sidebar_row_compact_h: 32.0,
            control_size: 32.0,
            control_radius: Some(4.0),
            panel_radius: Some(0.0),
            chip_radius: Some(4.0),
            rule_width: Some(1.0),
            ..Self::MINIMAL.tokens
        },
        surface: SurfaceModel::Opaque,
        controls: ControlVariant::Key,
        status_indicator: StatusIndicator::Dot,
        font_preference: FontPreference::Geist,
        paired_theme: Some(PairedTheme {
            light: "industrial-light",
            dark: "industrial-dark",
        }),
        behavior: StyleBehavior {
            motion: Motion::Instant,
            ..Self::MINIMAL.behavior
        },
    };
    pub const fn of(id: UiStyleId) -> Self {
        match id {
            UiStyleId::Minimal => Self::MINIMAL,
            UiStyleId::Dense => Self::DENSE,
            UiStyleId::Guided => Self::GUIDED,
            UiStyleId::Industrial => Self::INDUSTRIAL,
        }
    }
}

/// Keep hover-only actions visible in styles that request persistent controls.
pub fn reveal_on_hover<T: gpui::Styled + gpui::InteractiveElement>(
    element: T,
    group: impl Into<gpui::SharedString>,
    style: UiStyle,
    visible: bool,
) -> T {
    use gpui::Styled as _;
    if style.behavior.reveal == Reveal::Always {
        element.opacity(1.0)
    } else {
        element
            .opacity(if visible { 1.0 } else { 0.0 })
            .group_hover(group, |s| s.opacity(1.0))
    }
}

struct UiStyleState(UiStyleId);
impl gpui::Global for UiStyleState {}

pub fn init(id: UiStyleId, cx: &mut gpui::App) {
    cx.set_global(UiStyleState(id));
}

pub fn current(cx: &gpui::App) -> UiStyle {
    UiStyle::of(
        cx.try_global::<UiStyleState>()
            .map(|state| state.0)
            .unwrap_or_default(),
    )
}

pub fn set_style(id: UiStyleId, window: &mut gpui::Window, cx: &mut gpui::App) -> bool {
    if current(cx).id == id {
        return false;
    }
    cx.set_global(UiStyleState(id));
    window.set_rem_size(gpui::px(crate::typography::effective_rem_px(cx)));
    crate::typography::reinstall_theme(cx);
    crate::appearance::reapply_window_background(cx);
    crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
        settings.ui_style = id
    });
    true
}

/// A device-local, dismissible explanation shown in the Guided style.
pub fn help_card(
    id: &'static str,
    message: &'static str,
    theme: &crate::theme::Theme,
    cx: &gpui::App,
) -> Option<gpui::AnyElement> {
    use gpui::{div, prelude::*, px};
    if !theme.ui_style.behavior.help_cards
        || crate::settings::current(cx)
            .dismissed_help_cards
            .iter()
            .any(|card| card == id)
    {
        return None;
    }
    Some(
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .p(px(8.0))
            .rounded(px(6.0))
            .bg(theme.surface_raised)
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted)
            .child(div().flex_1().min_w_0().child(message))
            .child(
                div()
                    .id(gpui::SharedString::from(format!("dismiss-help-{id}")))
                    .debug_selector(move || format!("dismiss-help-{id}"))
                    .flex_none()
                    .cursor_pointer()
                    .child("Dismiss")
                    .on_click(move |_, _, cx| {
                        crate::settings::update(
                            crate::settings::SavePolicy::Immediate,
                            cx,
                            |settings| {
                                if !settings.dismissed_help_cards.iter().any(|card| card == id) {
                                    settings.dismissed_help_cards.push(id.to_owned());
                                }
                            },
                        );
                        cx.refresh_windows();
                    }),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[gpui::test]
    fn industrial_switch_restores_saved_themes(cx: &mut gpui::TestAppContext) {
        use gpui::{Context, Render, Window, div};
        struct Host;
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
                div()
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let (_, vcx) = cx.add_window_view(|_, _| Host);
        vcx.update(|_, cx| {
            crate::settings::init(Default::default(), directory.path(), cx);
            init(UiStyleId::Minimal, cx);
            crate::appearance::init(
                crate::appearance::AppearanceMode::Light,
                mast_theme::ThemeSelection {
                    light: "github-light".into(),
                    dark: "github-dark".into(),
                },
                Default::default(),
                Default::default(),
                cx,
            );
        });
        vcx.update(|window, cx| {
            assert_eq!(
                crate::theme::Theme::of(cx).variant_id.as_ref(),
                "github-light"
            );
            set_style(UiStyleId::Industrial, window, cx);
            assert_eq!(
                crate::theme::Theme::of(cx).variant_id.as_ref(),
                "industrial-light"
            );
            crate::appearance::set_mode(crate::appearance::AppearanceMode::Dark, cx);
            assert_eq!(
                crate::theme::Theme::of(cx).variant_id.as_ref(),
                "industrial-dark"
            );
            set_style(UiStyleId::Minimal, window, cx);
            assert_eq!(
                crate::theme::Theme::of(cx).variant_id.as_ref(),
                "github-dark"
            );
            assert_eq!(crate::appearance::themes(cx).light, "github-light");
        });
    }

    #[test]
    fn industrial_uses_key_controls_and_opaque_surfaces() {
        let style = UiStyle::INDUSTRIAL;
        assert_eq!(style.controls, ControlVariant::Key);
        assert_eq!(style.status_indicator, StatusIndicator::Dot);
        assert_eq!(style.surface, SurfaceModel::Opaque);
        assert_eq!(
            (
                style.tokens.control_radius,
                style.tokens.panel_radius,
                style.tokens.chip_radius,
                style.tokens.rule_width
            ),
            (Some(4.0), Some(0.0), Some(4.0), Some(1.0))
        );
        assert_eq!(
            serde_json::from_str::<UiStyleId>("\"industrial\"").unwrap(),
            UiStyleId::Industrial
        );
    }

    #[gpui::test]
    fn industrial_shared_controls_render_in_both_appearances(cx: &mut gpui::TestAppContext) {
        use gpui::{Context, Render, Window, div, prelude::*};
        struct Host;
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
                div().children(
                    [
                        crate::theme::Appearance::Light,
                        crate::theme::Appearance::Dark,
                    ]
                    .map(|appearance| {
                        let id = if appearance.is_dark() {
                            "industrial-dark"
                        } else {
                            "industrial-light"
                        };
                        let theme = crate::theme::Theme::for_selection(
                            appearance,
                            id,
                            Default::default(),
                            mast_theme::SurfacePreference::Opaque,
                        )
                        .with_ui_style(UiStyle::INDUSTRIAL);
                        div()
                            .child(
                                crate::settings::widgets::section_card(&theme)
                                    .child(crate::settings::widgets::badge(&theme, "Idle"))
                                    .child(crate::settings::widgets::toggle_switch(&theme, true))
                                    .child(crate::settings::widgets::toggle_switch(&theme, false))
                                    .child(
                                        crate::settings::widgets::ghost_action(&theme)
                                            .child("Action"),
                                    ),
                            )
                            .child(crate::popover::popover_card(&theme).child("Menu"))
                    }),
                )
            }
        }
        let (_, vcx) = cx.add_window_view(|_, _| Host);
        vcx.update(|window, cx| window.draw(cx).clear());
    }
    #[gpui::test]
    fn help_card_dismissal_persists(cx: &mut gpui::TestAppContext) {
        use gpui::{Context, Render, Window, div, prelude::*};
        struct Host;
        impl Render for Host {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
                let theme = crate::theme::Theme::default().with_ui_style(UiStyle::GUIDED);
                div().children(help_card("test-card", "Help", &theme, cx))
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let (_, vcx) = cx.add_window_view(|_, _| Host);
        vcx.update(|_, cx| crate::settings::init(Default::default(), directory.path(), cx));
        vcx.update(|window, cx| window.draw(cx).clear());
        let bounds = vcx.debug_bounds("dismiss-help-test-card").unwrap();
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("dismiss-help-test-card").is_none());
        assert_eq!(
            crate::settings::UiSettings::load(directory.path()).dismissed_help_cards,
            ["test-card"]
        );
    }
    #[test]
    fn minimal_preserves_existing_metrics() {
        let t = UiStyle::MINIMAL.tokens;
        assert_eq!(
            (
                t.control_radius,
                t.panel_radius,
                t.chip_radius,
                t.rule_width
            ),
            (None, None, None, None)
        );
        assert_eq!(UiStyle::MINIMAL.surface, SurfaceModel::ThemeDefault);
        assert_eq!(UiStyle::MINIMAL.controls, ControlVariant::Minimal);
        assert_eq!(UiStyle::MINIMAL.status_indicator, StatusIndicator::Minimal);
        assert_eq!(
            UiStyle::MINIMAL.font_preference,
            FontPreference::UserDefault
        );
        assert_eq!(UiStyle::MINIMAL.paired_theme, None);
        assert_eq!(
            (
                t.type_scale,
                t.sidebar_row_compact_h,
                t.sidebar_row_extra_h,
                t.outline_row_delta,
                t.control_size,
                t.control_icon_size
            ),
            (1.0, 29.0, 0.0, 0.0, 28.0, 16.0)
        );
        assert_eq!(
            UiStyle::MINIMAL.behavior,
            StyleBehavior {
                labels: LabelMode::IconOnly,
                reveal: Reveal::Hover,
                hints: HintMode::Tooltip,
                motion: Motion::Soft,
                status_words: false,
                help_cards: false,
                status_bar: false,
                outline_columns: false,
                walkthrough_rail: false,
            }
        );
    }
    #[test]
    fn guided_metrics_and_help_card_dismissals() {
        let style = UiStyle::GUIDED;
        assert_eq!(
            (
                style.tokens.control_size,
                style.tokens.sidebar_row_extra_h,
                style.tokens.outline_row_delta
            ),
            (32.0, 4.0, 6.0)
        );
        assert_eq!(style.behavior.labels, LabelMode::All);
        assert!(style.behavior.help_cards);
        let mut settings = crate::settings::UiSettings::default();
        assert!(settings.dismissed_help_cards.is_empty());
        settings.dismissed_help_cards.push("review-intro".into());
        let encoded = serde_json::to_string(&settings).unwrap();
        assert!(encoded.contains("dismissedHelpCards"));
        let decoded: crate::settings::UiSettings = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.dismissed_help_cards, settings.dismissed_help_cards);
    }
    #[test]
    fn settings_default_unknown_and_dense() {
        use crate::settings::UiSettings;
        assert_eq!(UiSettings::default().ui_style, UiStyleId::Minimal);
        for json in ["{}", r#"{"uiStyle":"bogus"}"#] {
            assert_eq!(
                serde_json::from_str::<UiSettings>(json).unwrap().ui_style,
                UiStyleId::Minimal
            );
        }
        let mut settings = UiSettings::default();
        settings.ui_style = UiStyleId::Dense;
        assert_eq!(
            serde_json::from_str::<UiSettings>(&serde_json::to_string(&settings).unwrap())
                .unwrap()
                .ui_style,
            UiStyleId::Dense
        );
    }
    #[test]
    fn style_id_defaults_and_round_trips() {
        assert_eq!(UiStyleId::default(), UiStyleId::Minimal);
        assert_eq!(
            serde_json::from_str::<UiStyleId>("\"unknown\"").unwrap(),
            UiStyleId::Minimal
        );
        assert_eq!(
            serde_json::from_str::<UiStyleId>(&serde_json::to_string(&UiStyleId::Dense).unwrap())
                .unwrap(),
            UiStyleId::Dense
        );
    }
}
