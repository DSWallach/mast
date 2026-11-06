//! Device-local presentation style and its structural metrics.

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UiStyleId {
    #[default]
    Minimal,
    Dense,
    Guided,
}

impl<'de> Deserialize<'de> for UiStyleId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "dense" => Self::Dense,
            "guided" => Self::Guided,
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
        },
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
        },
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
        },
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
    pub const fn of(id: UiStyleId) -> Self {
        match id {
            UiStyleId::Minimal => Self::MINIMAL,
            UiStyleId::Dense => Self::DENSE,
            UiStyleId::Guided => Self::GUIDED,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn minimal_preserves_existing_metrics() {
        let t = UiStyle::MINIMAL.tokens;
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
