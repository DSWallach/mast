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
}

impl TokenOverrides {
    fn apply(&self, style: &mut UiStyle) {
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
                if !["default", "industrial", "area"].contains(&name.as_str()) {
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
                result.styles.insert(name.clone(), style);
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
            UiStyleId::Area => "area",
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
        let target = match key.as_str() {
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
    fn empty_overrides_preserve_default_minimal() {
        let directory = tempfile::tempdir().unwrap();
        let original = UiStyle::resolve(UiStyleId::Default, UiLayout::Minimal);
        assert_eq!(Overrides::load(directory.path()).apply(original), original);
        assert_eq!(Overrides::parse("").apply(original), original);
    }

    #[test]
    fn valid_values_survive_invalid_values_and_layout_wins() {
        let overrides = Overrides::parse(
            r#"{"version":"mast.style-overrides/v1","styles":{"area":{"controlRadius":8,"panelRadius":"bad","unknown":5,"layouts":{"compact":{"controlRadius":12,"rowHeight":40}}}}}"#,
        );
        let compact = overrides.apply(UiStyle::resolve(UiStyleId::Area, UiLayout::Compact));
        assert_eq!(compact.tokens.control_radius, Some(12.0));
        assert_eq!(compact.tokens.sidebar_row_compact_h, 40.0);
        let minimal = overrides.apply(UiStyle::resolve(UiStyleId::Area, UiLayout::Minimal));
        assert_eq!(minimal.tokens.control_radius, Some(8.0));
        assert_eq!(minimal.tokens.panel_radius, Some(14.0));
    }
}

struct OverrideState {
    overrides: Overrides,
    directory: PathBuf,
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
    let path = Overrides::path(&directory);
    let mut watch = FileWatch::new(path);
    let overrides = Overrides::load(&directory);
    let task = cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            if let Some(overrides) = watch.poll() {
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
    std::fs::create_dir_all(&state.directory)?;
    let contents = serde_json::to_vec_pretty(overrides).map_err(std::io::Error::other)?;
    let path = Overrides::path(&state.directory);
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, contents)?;
    std::fs::rename(temporary, path)
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
            r#"{"version":"mast.style-overrides/v1","styles":{"area":{"controlRadius":9}}}"#,
        )
        .unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let result = watch
            .poll()
            .unwrap()
            .apply(UiStyle::resolve(UiStyleId::Area, UiLayout::Minimal));
        assert_eq!(result.tokens.control_radius, Some(9.0));
        assert!(watch.poll().is_none());
        std::fs::remove_file(path).unwrap();
        assert_eq!(watch.poll(), Some(Overrides::default()));
    }
}
