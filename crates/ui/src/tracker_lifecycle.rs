//! The Tracker's lifecycle is the shared work status (`mast_proto::work_status`),
//! the same derivation the project queue reads. These names are the Tracker's
//! own vocabulary for it.

use mast_proto::work_status::{ItemFlags, derive_ticket_status};
pub(crate) use mast_proto::work_status::{
    ChatFacts, PullRequestFacts, WorkStage as LifecycleStage, WorkStatus as LifecycleRow,
};

pub(crate) fn derive_ticket_lifecycle(
    chats: &[ChatFacts],
    pull_requests: &[PullRequestFacts],
    item_completed: Option<bool>,
    linear_completed: Option<bool>,
) -> LifecycleRow {
    derive_ticket_status(
        chats,
        pull_requests,
        item_completed,
        linear_completed,
        ItemFlags::default(),
    )
}
