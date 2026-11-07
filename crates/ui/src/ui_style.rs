//! Device-local presentation style and its structural metrics.

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UiStyleId {
    #[default]
    Default,
    Industrial,
    Area,
}

impl UiStyleId {
    pub const ALL: &'static [Self] = &[Self::Default, Self::Industrial, Self::Area];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Industrial => "Industrial",
            Self::Area => "Area",
        }
    }
    pub const fn description(self) -> &'static str {
        match self {
            Self::Default => "Standard controls",
            Self::Industrial => "Instrument panel",
            Self::Area => "Character conversation",
        }
    }
    pub const fn preview_gap(self) -> f32 {
        match self {
            Self::Default => 4.0,
            Self::Industrial | Self::Area => 8.0,
        }
    }
}

impl<'de> Deserialize<'de> for UiStyleId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "industrial" => Self::Industrial,
            "area" => Self::Area,
            _ => Self::Default,
        })
    }
}

/// Density and guidance are independent of the visual style.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiLayout {
    #[default]
    Minimal,
    Compact,
    Guided,
}

impl UiLayout {
    pub const ALL: &'static [Self] = &[Self::Minimal, Self::Compact, Self::Guided];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Minimal => "Minimal",
            Self::Compact => "Compact",
            Self::Guided => "Guided",
        }
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
    pub bubble_radius: Option<f32>,
    pub rule_width: Option<f32>,
    /// Optional grid and type roles; absent values preserve existing metrics.
    pub grid: Option<f32>,
    pub label_size: Option<f32>,
    pub secondary_size: Option<f32>,
    pub body_size: Option<f32>,
    pub title_size: Option<f32>,
    pub display_size: Option<f32>,
}

/// Optional values preserve each existing primitive's Minimal metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypeRoles {
    pub label: Option<f32>,
    pub secondary: Option<f32>,
    pub body: Option<f32>,
    pub title: Option<f32>,
    pub display: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpacingScale {
    pub small: f32,
    pub medium: f32,
    pub large: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadiusRoles {
    pub control: Option<f32>,
    pub chip: Option<f32>,
    pub panel: Option<f32>,
    pub bubble: Option<f32>,
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

/// Conversation and composer presentation, independent of theme colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationVariant {
    Standard,
    Industrial,
    Area,
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
    pub layout: UiLayout,
    pub tokens: StyleTokens,
    pub behavior: StyleBehavior,
    pub surface: SurfaceModel,
    pub controls: ControlVariant,
    pub conversation: ConversationVariant,
    pub status_indicator: StatusIndicator,
    pub font_preference: FontPreference,
}

impl UiStyle {
    pub const MINIMAL: Self = Self {
        id: UiStyleId::Default,
        layout: UiLayout::Minimal,
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
            bubble_radius: None,
            rule_width: None,
            grid: None,
            label_size: None,
            secondary_size: None,
            body_size: None,
            title_size: None,
            display_size: None,
        },
        surface: SurfaceModel::ThemeDefault,
        controls: ControlVariant::Minimal,
        conversation: ConversationVariant::Standard,
        status_indicator: StatusIndicator::Minimal,
        font_preference: FontPreference::UserDefault,
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
        id: UiStyleId::Default,
        layout: UiLayout::Compact,
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
            bubble_radius: None,
            rule_width: None,
            grid: None,
            label_size: None,
            secondary_size: None,
            body_size: None,
            title_size: None,
            display_size: None,
        },
        surface: SurfaceModel::ThemeDefault,
        controls: ControlVariant::Minimal,
        conversation: ConversationVariant::Standard,
        status_indicator: StatusIndicator::Minimal,
        font_preference: FontPreference::UserDefault,
        behavior: StyleBehavior {
            labels: LabelMode::IconOnly,
            reveal: Reveal::Always,
            hints: HintMode::Tooltip,
            motion: Motion::Instant,
            status_words: false,
            help_cards: false,
            status_bar: true,
            outline_columns: true,
            walkthrough_rail: true,
        },
    };
    pub const GUIDED: Self = Self {
        id: UiStyleId::Default,
        layout: UiLayout::Guided,
        tokens: StyleTokens {
            type_scale: 1.0625,
            sidebar_row_compact_h: 29.0,
            sidebar_row_extra_h: 4.0,
            outline_row_delta: 6.0,
            control_size: 34.0,
            control_icon_size: 16.0,
            control_radius: None,
            panel_radius: None,
            chip_radius: None,
            bubble_radius: None,
            rule_width: None,
            grid: None,
            label_size: None,
            secondary_size: None,
            body_size: None,
            title_size: None,
            display_size: None,
        },
        surface: SurfaceModel::ThemeDefault,
        controls: ControlVariant::Minimal,
        conversation: ConversationVariant::Standard,
        status_indicator: StatusIndicator::Minimal,
        font_preference: FontPreference::UserDefault,
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
        layout: UiLayout::Minimal,
        tokens: StyleTokens {
            sidebar_row_compact_h: 32.0,
            control_size: 32.0,
            control_radius: Some(4.0),
            panel_radius: Some(0.0),
            chip_radius: Some(4.0),
            bubble_radius: Some(0.0),
            rule_width: Some(1.0),
            grid: Some(8.0),
            label_size: Some(10.0),
            secondary_size: Some(12.0),
            body_size: Some(13.0),
            title_size: Some(16.0),
            display_size: Some(10.0),
            ..Self::MINIMAL.tokens
        },
        surface: SurfaceModel::Opaque,
        controls: ControlVariant::Key,
        conversation: ConversationVariant::Industrial,
        status_indicator: StatusIndicator::Dot,
        font_preference: FontPreference::Geist,
        behavior: StyleBehavior {
            motion: Motion::Instant,
            ..Self::MINIMAL.behavior
        },
    };
    pub const AREA: Self = Self {
        id: UiStyleId::Area,
        layout: UiLayout::Minimal,
        tokens: StyleTokens {
            sidebar_row_compact_h: 36.0,
            control_size: 36.0,
            control_radius: Some(12.0),
            panel_radius: Some(14.0),
            chip_radius: Some(12.0),
            bubble_radius: Some(14.0),
            rule_width: None,
            grid: Some(8.0),
            label_size: Some(10.0),
            secondary_size: Some(12.0),
            body_size: Some(13.0),
            title_size: Some(20.0),
            display_size: Some(20.0),
            ..Self::MINIMAL.tokens
        },
        surface: SurfaceModel::Opaque,
        controls: ControlVariant::Minimal,
        conversation: ConversationVariant::Area,
        status_indicator: StatusIndicator::Dot,
        font_preference: FontPreference::Geist,
        behavior: StyleBehavior {
            motion: Motion::Soft,
            ..Self::MINIMAL.behavior
        },
    };
    pub const fn of(id: UiStyleId) -> Self {
        match id {
            UiStyleId::Default => Self::MINIMAL,
            UiStyleId::Industrial => Self::INDUSTRIAL,
            UiStyleId::Area => Self::AREA,
        }
    }

    pub const fn type_roles(self) -> TypeRoles {
        TypeRoles {
            label: self.tokens.label_size,
            secondary: self.tokens.secondary_size,
            body: self.tokens.body_size,
            title: self.tokens.title_size,
            display: self.tokens.display_size,
        }
    }

    /// Spacing derives from the existing control gap and the independent layout.
    pub fn spacing_scale(self) -> SpacingScale {
        let base = crate::surface_chrome::CONTROL_GAP;
        let factor = match self.layout {
            UiLayout::Minimal => 1.0,
            UiLayout::Compact => 0.75,
            UiLayout::Guided => 1.25,
        };
        SpacingScale {
            small: base * factor,
            medium: base * 2.0 * factor,
            large: base * 4.0 * factor,
        }
    }

    pub const fn radius_roles(self) -> RadiusRoles {
        RadiusRoles {
            control: self.tokens.control_radius,
            chip: self.tokens.chip_radius,
            panel: self.tokens.panel_radius,
            bubble: self.tokens.bubble_radius,
        }
    }

    /// Combine the look with independently selected density and guidance.
    pub const fn resolve(id: UiStyleId, layout: UiLayout) -> Self {
        let mut style = Self::of(id);
        let layout_style = match layout {
            UiLayout::Minimal => Self::MINIMAL,
            UiLayout::Compact => Self::DENSE,
            UiLayout::Guided => Self::GUIDED,
        };
        style.layout = layout;
        style.tokens.type_scale = layout_style.tokens.type_scale;
        style.tokens.sidebar_row_compact_h = layout_style.tokens.sidebar_row_compact_h;
        style.tokens.sidebar_row_extra_h = layout_style.tokens.sidebar_row_extra_h;
        style.tokens.outline_row_delta = layout_style.tokens.outline_row_delta;
        style.tokens.control_size = layout_style.tokens.control_size;
        style.tokens.control_icon_size = layout_style.tokens.control_icon_size;
        style.behavior = layout_style.behavior;
        style
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

struct UiStyleState(UiStyleId, UiLayout);
impl gpui::Global for UiStyleState {}

pub fn init(id: UiStyleId, cx: &mut gpui::App) {
    cx.set_global(UiStyleState(id, UiLayout::Minimal));
}

/// Change the presentation style without saving device settings.
pub fn set_style_in_memory(id: UiStyleId, cx: &mut gpui::App) {
    let layout = cx
        .try_global::<UiStyleState>()
        .map(|state| state.1)
        .unwrap_or_default();
    cx.set_global(UiStyleState(id, layout));
    crate::typography::reinstall_theme(cx);
    cx.refresh_windows();
}

/// Change layout without saving device settings (used by the gallery).
pub fn set_layout_in_memory(layout: UiLayout, cx: &mut gpui::App) {
    let id = current(cx).id;
    cx.set_global(UiStyleState(id, layout));
    crate::typography::reinstall_theme(cx);
    cx.refresh_windows();
}

pub fn init_with_layout(id: UiStyleId, layout: UiLayout, cx: &mut gpui::App) {
    cx.set_global(UiStyleState(id, layout));
}

pub fn current(cx: &gpui::App) -> UiStyle {
    let (id, layout) = cx
        .try_global::<UiStyleState>()
        .map(|state| (state.0, state.1))
        .unwrap_or_default();
    UiStyle::resolve(id, layout)
}

pub fn set_layout(layout: UiLayout, window: &mut gpui::Window, cx: &mut gpui::App) -> bool {
    let style = current(cx);
    if style.layout == layout {
        return false;
    }
    cx.set_global(UiStyleState(style.id, layout));
    window.set_rem_size(gpui::px(crate::typography::effective_rem_px(cx)));
    crate::typography::reinstall_theme(cx);
    crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
        settings.ui_layout = layout
    });
    cx.refresh_windows();
    true
}

pub fn set_style(id: UiStyleId, window: &mut gpui::Window, cx: &mut gpui::App) -> bool {
    if current(cx).id == id {
        return false;
    }
    cx.set_global(UiStyleState(id, current(cx).layout));
    window.set_rem_size(gpui::px(crate::typography::effective_rem_px(cx)));
    crate::typography::reinstall_theme(cx);
    window.set_background_appearance(crate::theme::Theme::of(cx).window_background_appearance());
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
            .when(
                theme.ui_style.id != UiStyleId::Area || id != "sidebar",
                |el| el.bg(theme.surface_raised),
            )
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
    #[test]
    fn spacing_and_radius_roles_follow_layout_and_style_independently() {
        let base = UiStyle::resolve(UiStyleId::Default, UiLayout::Minimal).spacing_scale();
        for &id in UiStyleId::ALL {
            let compact = UiStyle::resolve(id, UiLayout::Compact);
            let guided = UiStyle::resolve(id, UiLayout::Guided);
            assert_eq!(compact.spacing_scale().small, base.small * 0.75);
            assert_eq!(guided.spacing_scale().small, base.small * 1.25);
            assert_eq!(
                compact.radius_roles().bubble,
                UiStyle::of(id).radius_roles().bubble
            );
            assert_eq!(
                guided.type_roles().display,
                UiStyle::of(id).type_roles().display
            );
        }
    }

    #[test]
    fn each_layout_preserves_each_style_look() {
        for &id in UiStyleId::ALL {
            let look = UiStyle::of(id);
            for &layout in UiLayout::ALL {
                let resolved = UiStyle::resolve(id, layout);
                assert_eq!(resolved.id, id);
                assert_eq!(resolved.layout, layout);
                let layout_metrics = UiStyle::resolve(UiStyleId::Default, layout).tokens;
                assert_eq!(resolved.tokens.control_size, layout_metrics.control_size);
                assert_eq!(
                    resolved.tokens.sidebar_row_compact_h,
                    layout_metrics.sidebar_row_compact_h
                );
                assert_eq!(resolved.controls, look.controls);
                assert_eq!(resolved.conversation, look.conversation);
                assert_eq!(resolved.tokens.panel_radius, look.tokens.panel_radius);
                assert_eq!(
                    resolved.behavior.labels,
                    match layout {
                        UiLayout::Minimal => LabelMode::IconOnly,
                        UiLayout::Compact => LabelMode::IconOnly,
                        UiLayout::Guided => LabelMode::All,
                    }
                );
            }
        }
    }

    #[test]
    fn legacy_settings_migrate_and_explicit_layout_wins() {
        let directory = tempfile::tempdir().unwrap();
        let path = crate::settings::UiSettings::path(directory.path());
        for (legacy, style, layout) in [
            ("minimal", UiStyleId::Default, UiLayout::Minimal),
            ("dense", UiStyleId::Default, UiLayout::Compact),
            ("guided", UiStyleId::Default, UiLayout::Guided),
            ("industrial", UiStyleId::Industrial, UiLayout::Minimal),
            ("area", UiStyleId::Area, UiLayout::Minimal),
        ] {
            std::fs::write(&path, format!(r#"{{"uiStyle":"{legacy}"}}"#)).unwrap();
            let settings = crate::settings::UiSettings::load(directory.path());
            assert_eq!((settings.ui_style, settings.ui_layout), (style, layout));
            std::fs::write(
                &path,
                format!(r#"{{"uiStyle":"{legacy}","uiLayout":"guided"}}"#),
            )
            .unwrap();
            assert_eq!(
                crate::settings::UiSettings::load(directory.path()).ui_layout,
                UiLayout::Guided
            );
        }
    }

    #[test]
    fn conversation_variants_keep_styles_independent() {
        assert_eq!(UiStyle::MINIMAL.conversation, ConversationVariant::Standard);
        assert_eq!(
            UiStyle::INDUSTRIAL.conversation,
            ConversationVariant::Industrial
        );
        assert_eq!(UiStyle::AREA.conversation, ConversationVariant::Area);
    }
    #[test]
    fn registry_has_unique_labels_and_round_trips() {
        let mut labels = std::collections::HashSet::new();
        for &id in UiStyleId::ALL {
            assert!(!id.label().is_empty());
            assert!(labels.insert(id.label()));
            assert_eq!(
                serde_json::from_str::<UiStyleId>(&serde_json::to_string(&id).unwrap()).unwrap(),
                id
            );
            assert_eq!(UiStyle::of(id).id, id);
        }
    }
    #[gpui::test]
    fn style_switch_preserves_selected_themes(cx: &mut gpui::TestAppContext) {
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
            init(UiStyleId::Default, cx);
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
                "github-light"
            );
            set_style(UiStyleId::Area, window, cx);
            assert_eq!(
                crate::theme::Theme::of(cx).variant_id.as_ref(),
                "github-light"
            );
            crate::appearance::set_mode(crate::appearance::AppearanceMode::Dark, cx);
            assert_eq!(
                crate::theme::Theme::of(cx).variant_id.as_ref(),
                "github-dark"
            );
            set_style(UiStyleId::Default, window, cx);
            assert_eq!(
                crate::theme::Theme::of(cx).variant_id.as_ref(),
                "github-dark"
            );
            assert_eq!(crate::appearance::themes(cx).light, "github-light");
        });
    }

    #[test]
    fn area_uses_soft_character_geometry() {
        let style = UiStyle::AREA;
        assert_eq!(style.tokens.control_radius, Some(12.0));
        assert_eq!(style.tokens.panel_radius, Some(14.0));
        assert_eq!(style.tokens.rule_width, None);
        assert_eq!(style.status_indicator, StatusIndicator::Dot);
        assert_eq!(style.font_preference, FontPreference::Geist);
        assert_eq!(UiStyle::MINIMAL.tokens.control_radius, None);
    }

    #[test]
    fn industrial_uses_key_controls_and_opaque_surfaces() {
        let style = UiStyle::INDUSTRIAL;
        assert_eq!(style.controls, ControlVariant::Key);
        assert_eq!(style.status_indicator, StatusIndicator::Dot);
        assert_eq!(style.surface, SurfaceModel::Opaque);
        assert_eq!(
            (
                style.tokens.grid,
                style.tokens.label_size,
                style.tokens.secondary_size,
                style.tokens.body_size,
                style.tokens.title_size
            ),
            (Some(8.0), Some(10.0), Some(12.0), Some(13.0), Some(16.0))
        );
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
                            .child(
                                crate::popover::dialog_card(&theme)
                                    .child(crate::popover::dialog_title(&theme, "Title"))
                                    .child(crate::popover::dialog_field(
                                        &theme,
                                        div().child("Input").into_any_element(),
                                    ))
                                    .child(crate::popover::btn_primary(&theme, "Apply"))
                                    .child(crate::popover::btn_ghost(
                                        &theme,
                                        "Cancel",
                                        "industrial-test-cancel",
                                    )),
                            )
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
                t.grid,
                t.label_size,
                t.secondary_size,
                t.body_size,
                t.title_size
            ),
            (None, None, None, None, None)
        );
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
            (34.0, 4.0, 6.0)
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
        assert_eq!(UiSettings::default().ui_style, UiStyleId::Default);
        for json in ["{}", r#"{"uiStyle":"bogus"}"#] {
            assert_eq!(
                serde_json::from_str::<UiSettings>(json).unwrap().ui_style,
                UiStyleId::Default
            );
        }
        let mut settings = UiSettings::default();
        settings.ui_style = UiStyleId::Default;
        assert_eq!(
            serde_json::from_str::<UiSettings>(&serde_json::to_string(&settings).unwrap())
                .unwrap()
                .ui_style,
            UiStyleId::Default
        );
    }
    #[test]
    fn style_id_defaults_and_round_trips() {
        assert_eq!(UiStyleId::default(), UiStyleId::Default);
        assert_eq!(
            serde_json::from_str::<UiStyleId>("\"unknown\"").unwrap(),
            UiStyleId::Default
        );
        assert_eq!(
            serde_json::from_str::<UiStyleId>(&serde_json::to_string(&UiStyleId::Default).unwrap())
                .unwrap(),
            UiStyleId::Default
        );
    }
}
