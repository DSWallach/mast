//! Soft checkpoint nudge for worker turns that run many tool calls.

use crate::workspace_host::WorkspaceHost;

/// Usage diagnostic category recorded each time a nudge is sent.
pub(crate) const NUDGE_CATEGORY: &str = "callNudge";

/// Tool calls seen in the current turn. The Space's spacing is read once, when
/// the turn's first call arrives, so a setting change applies from the next turn.
#[derive(Default)]
pub(crate) struct TurnCallCount {
    calls: u32,
    every: Option<Option<u32>>,
}

impl TurnCallCount {
    /// Count one new tool call. Returns the running total when it lands on a
    /// multiple of the spacing; `resolve` runs once per turn.
    pub(crate) fn record(&mut self, resolve: impl FnOnce() -> Option<u32>) -> Option<u32> {
        self.calls += 1;
        let every = (*self.every.get_or_insert_with(resolve))?;
        (every > 0 && self.calls % every == 0).then_some(self.calls)
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The spacing for this chat's turn: `Some(n)` for a worker chat in a Space
/// whose setting is on, `None` for every other chat or when it is off.
pub(crate) fn spacing_for(workspace: &WorkspaceHost, chat_id: &str) -> Option<u32> {
    let chat = workspace.chat(chat_id).ok().flatten()?;
    // A worker has a parent chat. Standing-role holders and PR review chats
    // have one too but are not workers.
    chat.parent_chat_id.as_ref()?;
    if workspace.standing_role_of_chat(chat_id).is_some() {
        return None;
    }
    let reviewing = workspace.chat_pr_links(chat_id).ok()?.iter().any(|link| link.source == mast_proto::ChatLinkSource::ReviewLauncher);
    if reviewing {
        return None;
    }
    let space_id = chat.space_id?;
    let settings = mast_doc::materialize_work_outline(&space_id, &workspace.work_outline_inputs(&space_id)).space_settings;
    Some(settings.worker_call_nudge).filter(|every| *every > 0)
}

pub(crate) fn prompt(calls: u32) -> String {
    format!(
        "Checkpoint: this turn has made {calls} tool calls, and every further call resends your whole context. \
         Write a short summary of your progress so far. Then either continue with only the steps still needed, \
         keeping tool output small, or, if the work is done or no longer fits one turn, hand back to your Manager. \
         This is a reminder, not a stop."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn due_at(every: Option<u32>, calls: u32) -> Vec<u32> {
        let mut count = TurnCallCount::default();
        (0..calls).filter_map(|_| count.record(|| every)).collect()
    }

    #[test]
    fn nudges_at_each_multiple_of_the_spacing() {
        assert_eq!(due_at(Some(150), 449), [150, 300]);
        assert_eq!(due_at(Some(150), 450), [150, 300, 450]);
    }

    #[test]
    fn off_or_unresolved_never_nudges() {
        assert!(due_at(None, 500).is_empty());
        assert!(due_at(Some(0), 500).is_empty());
    }

    #[test]
    fn a_new_turn_starts_again_and_resolves_the_spacing_again() {
        let mut count = TurnCallCount::default();
        assert_eq!((0..150).filter_map(|_| count.record(|| Some(150))).count(), 1);
        count.reset();
        assert_eq!((0..149).filter_map(|_| count.record(|| Some(150))).count(), 0);
        count.reset();
        assert_eq!((0..100).filter_map(|_| count.record(|| Some(100))).count(), 1);
    }

    #[test]
    fn resolves_once_per_turn() {
        let mut count = TurnCallCount::default();
        let mut resolved = 0;
        for _ in 0..5 {
            count.record(|| {
                resolved += 1;
                Some(150)
            });
        }
        assert_eq!(resolved, 1);
    }
}
