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
