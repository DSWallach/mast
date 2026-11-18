//! Pure lifecycle derivation from a ticket's cached facts.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LifecycleStage {
    NotStarted,
    Building,
    CodeReview,
    PrReview,
    ReadyToMerge,
    Done,
}

impl LifecycleStage {
    pub(crate) const ALL: [Self; 6] = [Self::NotStarted, Self::Building, Self::CodeReview,
        Self::PrReview, Self::ReadyToMerge, Self::Done];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NotStarted => "Not started", Self::Building => "Building",
            Self::CodeReview => "Code review", Self::PrReview => "PR review",
            Self::ReadyToMerge => "Ready to merge", Self::Done => "Done",
        }
    }
    pub(crate) fn index(self) -> usize { Self::ALL.iter().position(|stage| *stage == self).unwrap() }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PullRequestFacts {
    pub state_known: bool,
    pub open: bool,
    pub merged: bool,
    pub approved: bool,
    pub changes_requested: bool,
    pub checks_passing: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LifecycleFacts {
    pub has_chat: bool,
    pub branch_review: bool,
    pub ready_request: bool,
    pub has_item: bool,
    pub item_completed: bool,
    pub has_linear_ticket: bool,
    pub linear_completed: bool,
    pub pull_request: PullRequestFacts,
    pub needs_answer: bool,
    pub idle_over_three_days: bool,
}

/// Facts that belong to one non-archived chat on a ticket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChatFacts {
    pub branch_review: bool,
    pub ready_request: bool,
    pub needs_answer: bool,
    pub idle_over_three_days: bool,
}

/// Derive one row from all chats and PRs without mixing review and check
/// results from different PRs. A ticket is stale only when every chat is idle.
pub(crate) fn derive_ticket_lifecycle(
    chats: &[ChatFacts],
    pull_requests: &[PullRequestFacts],
    item_completed: Option<bool>,
    linear_completed: Option<bool>,
) -> LifecycleRow {
    let merged = pull_requests.iter().any(|pr| pr.merged);
    let open = pull_requests.iter().any(|pr| pr.open);
    let ready = pull_requests.iter().any(|pr| {
        pr.open && pr.approved && !pr.changes_requested && pr.checks_passing
    });
    let changes_requested = pull_requests.iter().any(|pr| pr.open && pr.changes_requested);
    let facts = LifecycleFacts {
        has_chat: !chats.is_empty(),
        branch_review: chats.iter().any(|chat| chat.branch_review),
        ready_request: chats.iter().any(|chat| chat.ready_request),
        has_item: item_completed.is_some(),
        item_completed: item_completed.unwrap_or(false),
        has_linear_ticket: linear_completed.is_some(),
        linear_completed: linear_completed.unwrap_or(false),
        pull_request: PullRequestFacts {
            state_known: pull_requests.iter().all(|pr| pr.state_known),
            open,
            merged,
            approved: ready,
            // A ready PR is the furthest-along fact, while changes requested
            // on another PR still needs the user's attention.
            changes_requested: !ready && changes_requested,
            checks_passing: ready,
        },
        needs_answer: chats.iter().any(|chat| chat.needs_answer) || changes_requested,
        idle_over_three_days: !chats.is_empty() && chats.iter().all(|chat| chat.idle_over_three_days)
            && pull_requests.iter().all(|pr| pr.state_known),
    };
    derive_lifecycle(facts)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LifecycleRow {
    pub stage: LifecycleStage,
    pub needs_you: bool,
    pub stale: bool,
    pub merged_with_open_ticket: bool,
    pub linear_completed_with_open_pr: bool,
    pub linear_completed_without_pr: bool,
}

pub(crate) fn derive_lifecycle(facts: LifecycleFacts) -> LifecycleRow {
    let pr = facts.pull_request;
    let stage = if pr.merged || (facts.item_completed && !(facts.linear_completed && pr.open)) {
        LifecycleStage::Done
    } else if pr.open {
        if !facts.linear_completed && pr.approved && !pr.changes_requested && pr.checks_passing {
            LifecycleStage::ReadyToMerge
        } else {
            LifecycleStage::PrReview
        }
    } else if facts.linear_completed {
        LifecycleStage::Done
    } else if facts.ready_request || facts.branch_review {
        LifecycleStage::CodeReview
    } else if facts.has_chat {
        LifecycleStage::Building
    } else {
        LifecycleStage::NotStarted
    };
    LifecycleRow {
        stage,
        needs_you: facts.needs_answer || facts.ready_request || (pr.open && pr.changes_requested),
        stale: facts.has_chat && facts.idle_over_three_days && !pr.open && stage != LifecycleStage::Done,
        merged_with_open_ticket: pr.merged && ((facts.has_item && !facts.item_completed)
            || (facts.has_linear_ticket && !facts.linear_completed)),
        linear_completed_with_open_pr: facts.linear_completed && pr.open && !pr.merged,
        linear_completed_without_pr: facts.linear_completed && !pr.open && !pr.merged && !facts.item_completed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_stages_from_facts() {
        let mut facts = LifecycleFacts::default();
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::NotStarted);
        facts.has_chat = true;
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::Building);
        facts.ready_request = true;
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::CodeReview);
        facts.pull_request.open = true;
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::PrReview);
        facts.pull_request.approved = true;
        facts.pull_request.checks_passing = true;
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::ReadyToMerge);
        facts.pull_request.merged = true;
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::Done);
    }

    #[test]
    fn resolves_conflicting_completion_and_review_facts() {
        let mut facts = LifecycleFacts { linear_completed: true, ..Default::default() };
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::Done);
        assert!(derive_lifecycle(facts).linear_completed_without_pr);
        facts.pull_request.open = true;
        let row = derive_lifecycle(facts);
        assert_eq!(row.stage, LifecycleStage::PrReview);
        assert!(row.linear_completed_with_open_pr);
        facts.item_completed = true;
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::PrReview);
        facts.linear_completed = false;
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::Done);
        facts.linear_completed = true;
        facts.item_completed = false;
        facts.pull_request.approved = true;
        facts.pull_request.changes_requested = true;
        facts.pull_request.checks_passing = true;
        let row = derive_lifecycle(facts);
        assert_eq!(row.stage, LifecycleStage::PrReview);
        assert!(row.needs_you);
        facts.pull_request.changes_requested = false;
        facts.pull_request.checks_passing = false;
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::PrReview);
        facts.pull_request.checks_passing = true;
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::PrReview);
        facts.linear_completed = false;
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::ReadyToMerge);
        facts.pull_request.merged = true;
        facts.has_item = true;
        facts.linear_completed = false;
        assert!(derive_lifecycle(facts).merged_with_open_ticket);
    }

    #[test]
    fn stale_requires_chat_without_open_pr_or_completion() {
        let mut facts = LifecycleFacts { has_chat: true, idle_over_three_days: true, ..Default::default() };
        assert!(derive_lifecycle(facts).stale);
        facts.pull_request.open = true;
        assert!(!derive_lifecycle(facts).stale);
        facts.pull_request.open = false;
        facts.item_completed = true;
        assert!(!derive_lifecycle(facts).stale);
        facts.item_completed = false;
        facts.has_chat = false;
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::NotStarted);
    }

    #[test]
    fn combines_chats_and_prs_without_combining_independent_checks() {
        let chats = [ChatFacts { ready_request: true, idle_over_three_days: true, ..Default::default() },
            ChatFacts { needs_answer: true, ..Default::default() }];
        let mut prs = [PullRequestFacts { open: true, approved: true, ..Default::default() },
            PullRequestFacts { open: true, checks_passing: true, ..Default::default() }];
        let row = derive_ticket_lifecycle(&chats, &prs, None, None);
        assert_eq!(row.stage, LifecycleStage::PrReview);
        assert!(row.needs_you);
        assert!(!row.stale);
        prs[0].checks_passing = true;
        assert_eq!(derive_ticket_lifecycle(&chats, &prs, None, None).stage, LifecycleStage::ReadyToMerge);
        prs[1].merged = true;
        assert_eq!(derive_ticket_lifecycle(&chats, &prs, None, None).stage, LifecycleStage::Done);
        assert!(derive_ticket_lifecycle(&chats, &prs, Some(false), Some(true)).merged_with_open_ticket);
        assert!(!derive_ticket_lifecycle(&chats, &prs, Some(true), None).merged_with_open_ticket);
    }

    #[test]
    fn branch_review_and_reopened_items_follow_current_facts() {
        let chats = [ChatFacts { branch_review: true, ..Default::default() }];
        assert_eq!(derive_ticket_lifecycle(&chats, &[], None, None).stage, LifecycleStage::CodeReview);
        assert_eq!(derive_ticket_lifecycle(&[], &[], Some(true), None).stage, LifecycleStage::Done);
        assert_eq!(derive_ticket_lifecycle(&[], &[], Some(false), None).stage, LifecycleStage::NotStarted);
    }

    #[test]
    fn unknown_pull_request_state_does_not_hide_a_ticket_as_stale() {
        let chat = ChatFacts { idle_over_three_days: true, ..Default::default() };
        let pr = PullRequestFacts::default();
        assert!(!derive_ticket_lifecycle(&[chat], &[pr], Some(false), None).stale);
        assert!(derive_ticket_lifecycle(&[chat], &[], Some(false), None).stale);
    }
}
