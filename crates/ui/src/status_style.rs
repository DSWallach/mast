//! Accent-independent presentation of chat and worker states.

use crate::theme::Appearance;
use mast_proto::ChatIndicator;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Status {
    Working,
    Waiting,
    Errored,
    Ready,
    Done,
    Idle,
}

impl From<ChatIndicator> for Status {
    fn from(value: ChatIndicator) -> Self {
        match value {
            ChatIndicator::Working => Self::Working,
            ChatIndicator::AwaitingInput => Self::Waiting,
            ChatIndicator::Errored => Self::Errored,
            ChatIndicator::Completed => Self::Done,
            ChatIndicator::Idle => Self::Idle,
        }
    }
}

impl From<crate::team::WorkerState> for Status {
    fn from(value: crate::team::WorkerState) -> Self {
        use crate::team::WorkerState;
        match value {
            WorkerState::Working => Self::Working,
            WorkerState::Waiting => Self::Waiting,
            WorkerState::Errored => Self::Errored,
            WorkerState::Done => Self::Done,
            WorkerState::Idle => Self::Idle,
        }
    }
}

pub(crate) struct StatusStyle {
    pub color: gpui::Hsla,
    pub glyph: &'static str,
}

pub(crate) fn status_style(status: Status, appearance: Appearance) -> StatusStyle {
    let dark = appearance.is_dark();
    let (color, glyph) = match (status, dark) {
        (Status::Working, true) => (0x38d059, "◌"),
        (Status::Working, false) => (0x245f31, "◌"),
        (Status::Waiting, true) => (0xdfbe1b, "!"),
        (Status::Waiting, false) => (0xa68a00, "!"),
        (Status::Errored, true) => (0xfe4b25, "✕"),
        (Status::Errored, false) => (0xd93a15, "✕"),
        (Status::Ready, _) => (0x3f69cc, "→"),
        (Status::Done, true) => (0x619c82, "✓"),
        (Status::Done, false) => (0x619c82, "✓"),
        (Status::Idle, true) => (0xb5b5b5, "○"),
        (Status::Idle, false) => (0x183028, "○"),
    };
    StatusStyle {
        color: gpui::rgb(color).into(),
        glyph,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{AccentColor, Theme};

    #[test]
    fn status_pairs_are_distinct_for_every_accent_and_appearance() {
        let states = [
            Status::Working,
            Status::Waiting,
            Status::Errored,
            Status::Ready,
            Status::Done,
            Status::Idle,
        ];
        for appearance in [Appearance::Light, Appearance::Dark] {
            for accent in AccentColor::ALL {
                let theme = Theme::for_preferences(appearance, accent);
                for (index, state) in states.iter().enumerate() {
                    let style = status_style(*state, theme.appearance);
                    for other in &states[index + 1..] {
                        let other = status_style(*other, theme.appearance);
                        assert_ne!(style.glyph, other.glyph);
                    }
                    let waiting = status_style(Status::Waiting, theme.appearance);
                    let working = status_style(Status::Working, theme.appearance);
                    assert!((working.color.h - waiting.color.h).abs() > 0.08);
                }
            }
        }
    }
}
