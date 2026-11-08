//! Device-local, optional design token overrides.
//!
//! Style-level values apply to every layout; layout values take precedence.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ui_style::{UiLayout, UiStyle, UiStyleId};

pub const FILE_NAME: &str = "style-overrides.json";
const VERSION: &str = "mast.style-overrides/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColourRole {
    Accent,
    Text,
    Muted,
    Success,
    Danger,
}
impl ColourRole {
    pub fn colour(self, theme: &crate::theme::Theme) -> gpui::Hsla {
        match self {
            Self::Accent => theme.accent,
            Self::Text => theme.text,
            Self::Muted => theme.text_muted,
            Self::Success => theme.success,
            Self::Danger => theme.danger,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Accent => "Accent",
            Self::Text => "Text",
            Self::Muted => "Muted",
            Self::Success => "Success",
            Self::Danger => "Danger",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Accent => Self::Text,
            Self::Text => Self::Muted,
            Self::Muted => Self::Success,
            Self::Success => Self::Danger,
            Self::Danger => Self::Accent,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HeadingFamily {
    Sans,
    Mono,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HeadingWeight {
    Regular,
    Medium,
    Semibold,
    Bold,
}
impl HeadingWeight {
    pub fn weight(self) -> gpui::FontWeight {
        match self {
            Self::Regular => gpui::FontWeight::NORMAL,
            Self::Medium => gpui::FontWeight::MEDIUM,
            Self::Semibold => gpui::FontWeight::SEMIBOLD,
            Self::Bold => gpui::FontWeight::BOLD,
        }
    }
}

/// Surfaces with independent border and background decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesignSurface {
    Toolbar,
    SidebarRow,
    HeaderChip,
    Composer,
    Message,
    Tool,
    Code,
    OutlineRow,
}
impl DesignSurface {
    pub const ALL: [Self; 8] = [
        Self::Toolbar,
        Self::SidebarRow,
        Self::HeaderChip,
        Self::Composer,
        Self::Message,
        Self::Tool,
        Self::Code,
        Self::OutlineRow,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Toolbar => "toolbar",
            Self::SidebarRow => "sidebarRow",
            Self::HeaderChip => "headerChip",
            Self::Composer => "composer",
            Self::Message => "message",
            Self::Tool => "tool",
            Self::Code => "code",
            Self::OutlineRow => "outlineRow",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Toolbar => "Toolbar",
            Self::SidebarRow => "Sidebar row",
            Self::HeaderChip => "Chat header chip",
            Self::Composer => "Composer",
            Self::Message => "Message",
            Self::Tool => "Tool panel",
            Self::Code => "Code panel",
            Self::OutlineRow => "Outline row",
        }
    }
    fn index(self) -> usize {
        self as usize
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|surface| surface.name() == name)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_colour: Option<bool>,
}

/// Overrides for decisions whose default remains with the component.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DesignRoles {
    pub details_border: Option<bool>,
    pub sidebar_border: Option<bool>,
    pub details_page_surface: Option<bool>,
    pub outline_page_surface: Option<bool>,
    pub primary_colour: Option<ColourRole>,
    pub secondary_colour: Option<ColourRole>,
    pub heading_family: Option<HeadingFamily>,
    pub heading_weight: Option<HeadingWeight>,
    pub tracked_caps: Option<bool>,
    pub spacing_small: Option<f32>,
    pub control_radius: Option<f32>,
    pub panel_radius: Option<f32>,
    pub chip_radius: Option<f32>,
    pub bubble_radius: Option<f32>,
    pub surfaces: [SurfaceOverrides; 8],
}
impl DesignRoles {
    pub const NONE: Self = Self {
        details_border: None,
        sidebar_border: None,
        details_page_surface: None,
        outline_page_surface: None,
        primary_colour: None,
        secondary_colour: None,
        heading_family: None,
        heading_weight: None,
        tracked_caps: None,
        spacing_small: None,
        control_radius: None,
        panel_radius: None,
        chip_radius: None,
        bubble_radius: None,
        surfaces: [SurfaceOverrides {
            border: None,
            page_colour: None,
        }; 8],
    };
    pub fn surface(self, surface: DesignSurface) -> SurfaceOverrides {
        self.surfaces[surface.index()]
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panel_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chip_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bubble_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grid: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_width: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details_border: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_border: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details_page_surface: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline_page_surface: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_colour: Option<ColourRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_colour: Option<ColourRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_family: Option<HeadingFamily>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_weight: Option<HeadingWeight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracked_caps: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spacing_small: Option<f32>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub surfaces: BTreeMap<String, SurfaceOverrides>,
}

impl TokenOverrides {
    fn apply(&self, style: &mut UiStyle) {
        if self.details_border.is_some() {
            style.design_roles.details_border = self.details_border;
        }
        if self.sidebar_border.is_some() {
            style.design_roles.sidebar_border = self.sidebar_border;
        }
        if self.details_page_surface.is_some() {
            style.design_roles.details_page_surface = self.details_page_surface;
        }
        if self.outline_page_surface.is_some() {
            style.design_roles.outline_page_surface = self.outline_page_surface;
        }
        if self.primary_colour.is_some() {
            style.design_roles.primary_colour = self.primary_colour;
        }
        if self.secondary_colour.is_some() {
            style.design_roles.secondary_colour = self.secondary_colour;
        }
        if self.heading_family.is_some() {
            style.design_roles.heading_family = self.heading_family;
        }
        if self.heading_weight.is_some() {
            style.design_roles.heading_weight = self.heading_weight;
        }
        if self.tracked_caps.is_some() {
            style.design_roles.tracked_caps = self.tracked_caps;
        }
        if self.spacing_small.is_some() {
            style.design_roles.spacing_small = self.spacing_small;
        }
        if self.control_radius.is_some() {
            style.design_roles.control_radius = self.control_radius;
        }
        if self.panel_radius.is_some() {
            style.design_roles.panel_radius = self.panel_radius;
        }
        if self.chip_radius.is_some() {
            style.design_roles.chip_radius = self.chip_radius;
        }
        if self.bubble_radius.is_some() {
            style.design_roles.bubble_radius = self.bubble_radius;
        }
        for (name, values) in &self.surfaces {
            if let Some(surface) = DesignSurface::from_name(name) {
                let target = &mut style.design_roles.surfaces[surface.index()];
                if values.border.is_some() {
                    target.border = values.border;
                }
                if values.page_colour.is_some() {
                    target.page_colour = values.page_colour;
                }
            }
        }
        let tokens = &mut style.tokens;
        if let Some(v) = self.control_radius {
            tokens.control_radius = Some(v);
        }
        if let Some(v) = self.panel_radius {
            tokens.panel_radius = Some(v);
        }
        if let Some(v) = self.chip_radius {
            tokens.chip_radius = Some(v);
        }
        if let Some(v) = self.bubble_radius {
            tokens.bubble_radius = Some(v);
        }
        if let Some(v) = self.control_height {
            tokens.control_size = v;
        }
        if let Some(v) = self.row_height {
            tokens.sidebar_row_compact_h = v;
        }
        if let Some(v) = self.grid {
            tokens.grid = Some(v);
        }
        if let Some(v) = self.rule_width {
            tokens.rule_width = Some(v);
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StyleOverrides {
    #[serde(flatten)]
    pub tokens: TokenOverrides,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub layouts: BTreeMap<String, TokenOverrides>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Overrides {
    pub version: &'static str,
    pub styles: BTreeMap<String, StyleOverrides>,
}

impl Default for Overrides {
    fn default() -> Self {
        Self {
            version: VERSION,
            styles: BTreeMap::new(),
        }
    }
}

impl Overrides {
    pub fn path(directory: &Path) -> PathBuf {
        directory.join(FILE_NAME)
    }

    /// Ignore malformed individual fields while retaining valid neighbours.
    pub fn parse(contents: &str) -> Self {
        if contents.trim().is_empty() {
            return Self::default();
        }
        let value: Value = match serde_json::from_str(contents) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "invalid style overrides");
                return Self::default();
            }
        };
        let Some(root) = value.as_object() else {
            tracing::warn!("style overrides must be an object");
            return Self::default();
        };
        if root.get("version").and_then(Value::as_str) != Some(VERSION) {
            tracing::warn!("unknown style overrides version");
            return Self::default();
        }
        for key in root
            .keys()
            .filter(|key| *key != "version" && *key != "styles")
        {
            tracing::warn!(%key, "unknown style overrides key");
        }
        let mut result = Self::default();
        if let Some(styles) = root.get("styles").and_then(Value::as_object) {
            for (name, entry) in styles {
                // The new key wins when both spellings are present.
                if name == "area" && styles.contains_key("horizon") { continue; }
                let name = if name == "area" { "horizon" } else { name.as_str() };
                if !["default", "industrial", "horizon"].contains(&name) {
                    tracing::warn!(style = %name, "unknown style");
                    continue;
                }
                let Some(object) = entry.as_object() else {
                    tracing::warn!(style = %name, "style overrides must be an object");
                    continue;
                };
                let mut style = StyleOverrides {
                    tokens: parse_tokens(object, true),
                    ..Default::default()
                };
                if let Some(layouts) = object.get("layouts").and_then(Value::as_object) {
                    for (layout, tokens) in layouts {
                        if !["minimal", "compact", "guided"].contains(&layout.as_str()) {
                            tracing::warn!(%layout, "unknown layout");
                            continue;
                        }
                        if let Some(tokens) = tokens.as_object() {
                            style
                                .layouts
                                .insert(layout.clone(), parse_tokens(tokens, false));
                        } else {
                            tracing::warn!(%layout, "layout overrides must be an object");
                        }
                    }
                } else if object.contains_key("layouts") {
                    tracing::warn!("layouts must be an object");
                }
                result.styles.insert(name.to_owned(), style);
            }
        } else if root.contains_key("styles") {
            tracing::warn!("styles must be an object");
        }
        result
    }

    pub fn load(directory: &Path) -> Self {
        match std::fs::read_to_string(Self::path(directory)) {
            Ok(contents) => Self::parse(&contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!(%error, "cannot read style overrides");
                Self::default()
            }
        }
    }

    pub fn apply(&self, mut style: UiStyle) -> UiStyle {
        let name = match style.id {
            UiStyleId::Default => "default",
            UiStyleId::Industrial => "industrial",
            UiStyleId::Horizon => "horizon",
        };
        let layout = match style.layout {
            UiLayout::Minimal => "minimal",
            UiLayout::Compact => "compact",
            UiLayout::Guided => "guided",
        };
        if let Some(overrides) = self.styles.get(name) {
            overrides.tokens.apply(&mut style);
            if let Some(tokens) = overrides.layouts.get(layout) {
                tokens.apply(&mut style);
            }
        }
        style
    }
}

fn parse_tokens(object: &serde_json::Map<String, Value>, allow_layouts: bool) -> TokenOverrides {
    let mut tokens = TokenOverrides::default();
    for (key, value) in object {
        if allow_layouts && key == "layouts" {
            continue;
        }
        if key == "surfaces" {
            if let Some(surfaces) = value.as_object() {
                for (name, entry) in surfaces {
                    if DesignSurface::from_name(name).is_none() {
                        tracing::warn!(%name, "unknown design surface");
                        continue;
                    }
                    let Some(entry) = entry.as_object() else {
                        tracing::warn!(%name, "invalid design surface");
                        continue;
                    };
                    let mut parsed = SurfaceOverrides::default();
                    for (field, value) in entry {
                        let target = match field.as_str() {
                            "border" => &mut parsed.border,
                            "pageColour" => &mut parsed.page_colour,
                            _ => {
                                tracing::warn!(%field, "unknown surface token");
                                continue;
                            }
                        };
                        if let Some(value) = value.as_bool() {
                            *target = Some(value);
                        } else {
                            tracing::warn!(%field, "invalid surface token");
                        }
                    }
                    tokens.surfaces.insert(name.clone(), parsed);
                }
            } else {
                tracing::warn!("surfaces must be an object");
            }
            continue;
        }
        if key == "headingFamily" {
            match serde_json::from_value::<HeadingFamily>(value.clone()) {
                Ok(v) => tokens.heading_family = Some(v),
                Err(_) => tracing::warn!(%key, "invalid heading family"),
            }
            continue;
        }
        if key == "headingWeight" {
            match serde_json::from_value::<HeadingWeight>(value.clone()) {
                Ok(v) => tokens.heading_weight = Some(v),
                Err(_) => tracing::warn!(%key, "invalid heading weight"),
            }
            continue;
        }
        if key == "trackedCaps" {
            if let Some(v) = value.as_bool() {
                tokens.tracked_caps = Some(v);
            } else {
                tracing::warn!(%key, "invalid tracked caps value");
            }
            continue;
        }
        if key == "primaryColour" || key == "secondaryColour" {
            let target = if key == "primaryColour" {
                &mut tokens.primary_colour
            } else {
                &mut tokens.secondary_colour
            };
            match serde_json::from_value::<ColourRole>(value.clone()) {
                Ok(value) => *target = Some(value),
                Err(_) => tracing::warn!(%key, "invalid colour role"),
            }
            continue;
        }
        if key == "sidebarBorder" {
            if let Some(v) = value.as_bool() {
                tokens.sidebar_border = Some(v);
            } else {
                tracing::warn!(%key, "invalid style token value");
            }
            continue;
        }
        if key == "outlinePageSurface" {
            if let Some(v) = value.as_bool() {
                tokens.outline_page_surface = Some(v);
            } else {
                tracing::warn!(%key, "invalid style token value");
            }
            continue;
        }
        if key == "detailsBorder" || key == "detailsPageSurface" {
            let target = if key == "detailsBorder" {
                &mut tokens.details_border
            } else {
                &mut tokens.details_page_surface
            };
            if let Some(value) = value.as_bool() {
                *target = Some(value);
            } else {
                tracing::warn!(%key, "invalid style token value");
            }
            continue;
        }
        let target = match key.as_str() {
            "spacingSmall" => &mut tokens.spacing_small,
            "controlRadius" => &mut tokens.control_radius,
            "panelRadius" => &mut tokens.panel_radius,
            "chipRadius" => &mut tokens.chip_radius,
            "bubbleRadius" => &mut tokens.bubble_radius,
            "controlHeight" => &mut tokens.control_height,
            "rowHeight" => &mut tokens.row_height,
            "grid" => &mut tokens.grid,
            "ruleWidth" => &mut tokens.rule_width,
            _ => {
                tracing::warn!(%key, "unknown style token");
                continue;
            }
        };
        match value
            .as_f64()
            .filter(|v| v.is_finite() && (0.0..=200.0).contains(v))
        {
            Some(value) => *target = Some(value as f32),
            None => tracing::warn!(%key, "invalid style token value"),
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_area_overrides_load_and_horizon_takes_precedence() {
        let legacy = Overrides::parse(r#"{"version":"mast.style-overrides/v1","styles":{"area":{"controlRadius":9}}}"#);
        let style = UiStyle::resolve(UiStyleId::Horizon, UiLayout::Minimal);
        assert_eq!(legacy.apply(style).tokens.control_radius, Some(9.0));
        let both = Overrides::parse(r#"{"version":"mast.style-overrides/v1","styles":{"area":{"controlRadius":9},"horizon":{"controlRadius":12}}}"#);
        assert_eq!(both.apply(style).tokens.control_radius, Some(12.0));
    }

    #[test]
    fn empty_overrides_preserve_default_minimal() {
        let directory = tempfile::tempdir().unwrap();
        let original = UiStyle::resolve(UiStyleId::Default, UiLayout::Minimal);
        assert_eq!(Overrides::load(directory.path()).apply(original), original);
        assert_eq!(Overrides::parse("").apply(original), original);
    }

    #[test]
    fn valid_values_survive_invalid_values_and_layout_wins() {
        let overrides = Overrides::parse(
            r#"{"version":"mast.style-overrides/v1","styles":{"horizon":{"controlRadius":8,"panelRadius":"bad","unknown":5,"layouts":{"compact":{"controlRadius":12,"rowHeight":40}}}}}"#,
        );
        let compact = overrides.apply(UiStyle::resolve(UiStyleId::Horizon, UiLayout::Compact));
        assert_eq!(compact.tokens.control_radius, Some(12.0));
        assert_eq!(compact.tokens.sidebar_row_compact_h, 40.0);
        let minimal = overrides.apply(UiStyle::resolve(UiStyleId::Horizon, UiLayout::Minimal));
        assert_eq!(minimal.tokens.control_radius, Some(8.0));
        assert_eq!(minimal.tokens.panel_radius, Some(14.0));
    }

    #[test]
    fn component_roles_round_trip_and_invalid_values_do_not_hide_neighbours() {
        let input = r#"{"version":"mast.style-overrides/v1","styles":{"industrial":{"primaryColour":"success","secondaryColour":"invalid","headingFamily":"mono","headingWeight":"bold","trackedCaps":true}}}"#;
        let overrides = Overrides::parse(input);
        let result = overrides.apply(UiStyle::resolve(UiStyleId::Industrial, UiLayout::Minimal));
        assert_eq!(
            result.design_roles.primary_colour,
            Some(ColourRole::Success)
        );
        assert_eq!(result.design_roles.secondary_colour, None);
        assert_eq!(
            result.design_roles.heading_family,
            Some(HeadingFamily::Mono)
        );
        assert_eq!(
            result.design_roles.heading_weight,
            Some(HeadingWeight::Bold)
        );
        assert_eq!(result.design_roles.tracked_caps, Some(true));
        assert_eq!(
            Overrides::parse(&serde_json::to_string(&overrides).unwrap()),
            overrides
        );
    }

    #[test]
    fn surface_overrides_merge_independently_and_ignore_invalid_fields() {
        let input = r#"{"version":"mast.style-overrides/v1","styles":{"horizon":{"surfaces":{"composer":{"border":true,"pageColour":false,"unknown":1},"code":{"border":"bad","pageColour":true},"missing":{"border":true}},"layouts":{"compact":{"surfaces":{"composer":{"border":false}}}}}}}"#;
        let overrides = Overrides::parse(input);
        let style = overrides.apply(UiStyle::resolve(UiStyleId::Horizon, UiLayout::Compact));
        let composer = style.design_roles.surface(DesignSurface::Composer);
        assert_eq!(composer.border, Some(false));
        assert_eq!(composer.page_colour, Some(false));
        assert_eq!(
            style.design_roles.surface(DesignSurface::Code).page_colour,
            Some(true)
        );
        assert_eq!(style.design_roles.surface(DesignSurface::Code).border, None);
        assert_eq!(
            Overrides::parse(&serde_json::to_string(&overrides).unwrap()),
            overrides
        );
    }

    #[test]
    fn small_spacing_changes_the_resolved_scale() {
        let overrides = Overrides::parse(
            r#"{"version":"mast.style-overrides/v1","styles":{"horizon":{"spacingSmall":7}}}"#,
        );
        let style = overrides.apply(UiStyle::resolve(UiStyleId::Horizon, UiLayout::Minimal));
        assert_eq!(style.spacing_scale().small, 7.0);
    }

    #[test]
    fn component_roles_merge_with_layout_precedence() {
        let overrides = Overrides::parse(
            r#"{"version":"mast.style-overrides/v1","styles":{"horizon":{"detailsBorder":true,"detailsPageSurface":false,"layouts":{"compact":{"detailsBorder":false}}}}}"#,
        );
        let minimal = overrides.apply(UiStyle::resolve(UiStyleId::Horizon, UiLayout::Minimal));
        assert_eq!(minimal.design_roles.details_border, Some(true));
        assert_eq!(minimal.design_roles.details_page_surface, Some(false));
        let compact = overrides.apply(UiStyle::resolve(UiStyleId::Horizon, UiLayout::Compact));
        assert_eq!(compact.design_roles.details_border, Some(false));
        assert_eq!(compact.design_roles.details_page_surface, Some(false));
    }
}

struct OverrideState {
    overrides: Overrides,
    directory: PathBuf,
    read_only: bool,
    file_watch: std::rc::Rc<std::cell::RefCell<FileWatch>>,
    _watch: gpui::Task<()>,
}
impl gpui::Global for OverrideState {}

pub fn current(cx: &gpui::App) -> Overrides {
    cx.try_global::<OverrideState>()
        .map(|state| state.overrides.clone())
        .unwrap_or_default()
}

/// Installs the profile file and polls its contents, including atomic replacement.
pub fn init(directory: PathBuf, cx: &mut gpui::App) {
    init_inner(directory, false, cx);
}

/// Watch an app profile without granting the preview process write access.
pub fn init_read_only(directory: PathBuf, cx: &mut gpui::App) {
    init_inner(directory, true, cx);
}

fn init_inner(directory: PathBuf, read_only: bool, cx: &mut gpui::App) {
    let path = Overrides::path(&directory);
    let file_watch = std::rc::Rc::new(std::cell::RefCell::new(FileWatch::new(path)));
    let watch = file_watch.clone();
    let overrides = Overrides::load(&directory);
    let task = cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            let changed = watch.borrow_mut().poll();
            if let Some(overrides) = changed {
                cx.update(|cx| {
                    if cx.has_global::<OverrideState>() {
                        cx.global_mut::<OverrideState>().overrides = overrides;
                        crate::typography::reinstall_theme(cx);
                    }
                });
            }
        }
    });
    cx.set_global(OverrideState {
        overrides,
        directory,
        read_only,
        file_watch,
        _watch: task,
    });
}

pub fn preview(overrides: Overrides, cx: &mut gpui::App) {
    if cx.has_global::<OverrideState>() {
        cx.global_mut::<OverrideState>().overrides = overrides;
        crate::typography::reinstall_theme(cx);
    }
}

pub fn save(overrides: &Overrides, cx: &gpui::App) -> std::io::Result<()> {
    let state = cx
        .try_global::<OverrideState>()
        .ok_or_else(|| std::io::Error::other("style overrides not initialized"))?;
    if state.read_only {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "style overrides are read-only",
        ));
    }
    std::fs::create_dir_all(&state.directory)?;
    let contents = serde_json::to_vec_pretty(overrides).map_err(std::io::Error::other)?;
    let path = Overrides::path(&state.directory);
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, &contents)?;
    std::fs::rename(temporary, path)?;
    state.file_watch.borrow_mut().previous = Some(contents);
    Ok(())
}

struct FileWatch {
    path: PathBuf,
    previous: Option<Vec<u8>>,
}

impl FileWatch {
    fn new(path: PathBuf) -> Self {
        let previous = std::fs::read(&path).ok();
        Self { path, previous }
    }

    fn poll(&mut self) -> Option<Overrides> {
        let next = std::fs::read(&self.path).ok();
        if next == self.previous {
            return None;
        }
        self.previous = next.clone();
        Some(
            next.as_deref()
                .map(|bytes| Overrides::parse(&String::from_utf8_lossy(bytes)))
                .unwrap_or_default(),
        )
    }
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    #[test]
    fn watches_creation_replacement_and_removal() {
        let dir = tempfile::tempdir().unwrap();
        let path = Overrides::path(dir.path());
        let mut watch = FileWatch::new(path.clone());
        assert!(watch.poll().is_none());
        let replacement = dir.path().join("replacement");
        std::fs::write(
            &replacement,
            r#"{"version":"mast.style-overrides/v1","styles":{"horizon":{"controlRadius":9}}}"#,
        )
        .unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let result = watch
            .poll()
            .unwrap()
            .apply(UiStyle::resolve(UiStyleId::Horizon, UiLayout::Minimal));
        assert_eq!(result.tokens.control_radius, Some(9.0));
        assert!(watch.poll().is_none());
        std::fs::remove_file(path).unwrap();
        assert_eq!(watch.poll(), Some(Overrides::default()));
    }
}
