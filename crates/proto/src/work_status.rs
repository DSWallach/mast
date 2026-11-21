//! The one derived status of a work item. The project queue and the Tracker
//! both read it, so a merged PR means the same thing in each. Nothing here is
//! stored: the status is a pure function of the facts a caller has gathered.

/// How far an item has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkStage {
    NotStarted,
    Building,
    CodeReview,
    PrReview,
    ReadyToMerge,
    Done,
}

impl WorkStage {
    pub const ALL: [Self; 6] = [
        Self::NotStarted,
        Self::Building,
        Self::CodeReview,
        Self::PrReview,
        Self::ReadyToMerge,
        Self::Done,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::NotStarted => "Not started",
            Self::Building => "Building",
            Self::CodeReview => "Code review",
            Self::PrReview => "PR review",
            Self::ReadyToMerge => "Ready to merge",
            Self::Done => "Done",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|stage| *stage == self).unwrap()
    }
}

/// What holds an item back from starting or continuing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkHold {
    /// An open Required Action.
    UserAction,
    /// A future `waitingUntil`.
    Waiting,
}

/// One pull request. `state_known` is false until a lookup has answered, so
/// "not merged" and "not looked up yet" stay different facts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PullRequestFacts {
    pub state_known: bool,
    pub open: bool,
    pub merged: bool,
    pub approved: bool,
    pub changes_requested: bool,
    pub checks_passing: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkFacts {
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
    pub archived: bool,
    pub user_action_open: bool,
    pub waiting: bool,
}

/// Facts that belong to one non-archived chat on an item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChatFacts {
    pub branch_review: bool,
    pub ready_request: bool,
    pub needs_answer: bool,
    pub idle_over_three_days: bool,
}

/// Facts about the item itself rather than its chats or PRs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItemFlags {
    pub archived: bool,
    pub user_action_open: bool,
    pub waiting: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkStatus {
    pub stage: WorkStage,
    pub hold: Option<WorkHold>,
    pub needs_you: bool,
    pub stale: bool,
    /// A linked PR has no answer yet, so the stage may still change.
    pub facts_pending: bool,
    pub merged_with_open_ticket: bool,
    pub linear_completed_with_open_pr: bool,
    pub item_completed_with_open_pr: bool,
    pub linear_completed_without_pr: bool,
}

/// Derive one status from all chats and PRs without mixing review and check
/// results from different PRs. An item is stale only when every chat is idle.
pub fn derive_ticket_status(
    chats: &[ChatFacts],
    pull_requests: &[PullRequestFacts],
    item_completed: Option<bool>,
    linear_completed: Option<bool>,
    flags: ItemFlags,
) -> WorkStatus {
    let merged = pull_requests.iter().any(|pr| pr.merged);
    let open = pull_requests.iter().any(|pr| pr.open);
    let ready = pull_requests
        .iter()
        .any(|pr| pr.open && pr.approved && !pr.changes_requested && pr.checks_passing);
    let changes_requested = pull_requests
        .iter()
        .any(|pr| pr.open && pr.changes_requested);
    let all_known = pull_requests.iter().all(|pr| pr.state_known);
    let facts = WorkFacts {
        has_chat: !chats.is_empty(),
        branch_review: chats.iter().any(|chat| chat.branch_review),
        ready_request: chats.iter().any(|chat| chat.ready_request),
        has_item: item_completed.is_some(),
        item_completed: item_completed.unwrap_or(false),
        has_linear_ticket: linear_completed.is_some(),
        linear_completed: linear_completed.unwrap_or(false),
        pull_request: PullRequestFacts {
            state_known: all_known,
            open,
            merged,
            approved: ready,
            // A ready PR is the furthest-along fact, while changes requested
            // on another PR still needs the user's attention.
            changes_requested: !ready && changes_requested,
            checks_passing: ready,
        },
        needs_answer: chats.iter().any(|chat| chat.needs_answer) || changes_requested,
        idle_over_three_days: !chats.is_empty()
            && chats.iter().all(|chat| chat.idle_over_three_days)
            && all_known,
        archived: flags.archived,
        user_action_open: flags.user_action_open,
        waiting: flags.waiting,
    };
    let mut status = derive_work_status(facts);
    status.facts_pending = !all_known && status.stage != WorkStage::Done;
    status
}

pub fn derive_work_status(facts: WorkFacts) -> WorkStatus {
    let pr = facts.pull_request;
    let stage = if facts.archived || pr.merged {
        WorkStage::Done
    } else if pr.open {
        if !facts.linear_completed && pr.approved && !pr.changes_requested && pr.checks_passing {
            WorkStage::ReadyToMerge
        } else {
            WorkStage::PrReview
        }
    } else if facts.item_completed || facts.linear_completed {
        WorkStage::Done
    } else if facts.ready_request || facts.branch_review {
        WorkStage::CodeReview
    } else if facts.has_chat {
        WorkStage::Building
    } else {
        WorkStage::NotStarted
    };
    let hold = if facts.user_action_open {
        Some(WorkHold::UserAction)
    } else if facts.waiting {
        Some(WorkHold::Waiting)
    } else {
        None
    };
    WorkStatus {
        stage,
        hold,
        needs_you: facts.needs_answer || facts.ready_request || (pr.open && pr.changes_requested),
        stale: facts.has_chat && facts.idle_over_three_days && !pr.open && stage != WorkStage::Done,
        facts_pending: false,
        merged_with_open_ticket: pr.merged
            && ((facts.has_item && !facts.item_completed)
                || (facts.has_linear_ticket && !facts.linear_completed)),
        linear_completed_with_open_pr: facts.linear_completed && pr.open && !pr.merged,
        item_completed_with_open_pr: facts.item_completed && pr.open && !pr.merged,
        linear_completed_without_pr: facts.linear_completed
            && !pr.open
            && !pr.merged
            && !facts.item_completed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(
        chats: &[ChatFacts],
        prs: &[PullRequestFacts],
        item_completed: Option<bool>,
        linear_completed: Option<bool>,
    ) -> WorkStatus {
        derive_ticket_status(
            chats,
            prs,
            item_completed,
            linear_completed,
            ItemFlags::default(),
        )
    }

    #[test]
    fn derives_stages_from_facts() {
        let mut facts = WorkFacts::default();
        assert_eq!(derive_work_status(facts).stage, WorkStage::NotStarted);
        facts.has_chat = true;
        assert_eq!(derive_work_status(facts).stage, WorkStage::Building);
        facts.ready_request = true;
        assert_eq!(derive_work_status(facts).stage, WorkStage::CodeReview);
        facts.pull_request.open = true;
        assert_eq!(derive_work_status(facts).stage, WorkStage::PrReview);
        facts.pull_request.approved = true;
        facts.pull_request.checks_passing = true;
        assert_eq!(derive_work_status(facts).stage, WorkStage::ReadyToMerge);
        facts.pull_request.merged = true;
        assert_eq!(derive_work_status(facts).stage, WorkStage::Done);
    }

    #[test]
    fn resolves_conflicting_completion_and_review_facts() {
        let mut facts = WorkFacts {
            linear_completed: true,
            ..Default::default()
        };
        assert_eq!(derive_work_status(facts).stage, WorkStage::Done);
        assert!(derive_work_status(facts).linear_completed_without_pr);
        facts.pull_request.open = true;
        let row = derive_work_status(facts);
        assert_eq!(row.stage, WorkStage::PrReview);
        assert!(row.linear_completed_with_open_pr);
        facts.item_completed = true;
        assert_eq!(derive_work_status(facts).stage, WorkStage::PrReview);
        facts.linear_completed = false;
        let row = derive_work_status(facts);
        assert_eq!(row.stage, WorkStage::PrReview);
        assert!(row.item_completed_with_open_pr);
        facts.pull_request.approved = true;
        facts.pull_request.checks_passing = true;
        assert_eq!(derive_work_status(facts).stage, WorkStage::ReadyToMerge);
        facts.pull_request.approved = false;
        facts.pull_request.checks_passing = false;
        facts.linear_completed = true;
        facts.item_completed = false;
        facts.pull_request.approved = true;
        facts.pull_request.changes_requested = true;
        facts.pull_request.checks_passing = true;
        let row = derive_work_status(facts);
        assert_eq!(row.stage, WorkStage::PrReview);
        assert!(row.needs_you);
        facts.pull_request.changes_requested = false;
        facts.pull_request.checks_passing = false;
        assert_eq!(derive_work_status(facts).stage, WorkStage::PrReview);
        facts.pull_request.checks_passing = true;
        assert_eq!(derive_work_status(facts).stage, WorkStage::PrReview);
        facts.linear_completed = false;
        assert_eq!(derive_work_status(facts).stage, WorkStage::ReadyToMerge);
        facts.pull_request.merged = true;
        facts.has_item = true;
        facts.linear_completed = false;
        assert!(derive_work_status(facts).merged_with_open_ticket);
    }

    #[test]
    fn stale_requires_chat_without_open_pr_or_completion() {
        let mut facts = WorkFacts {
            has_chat: true,
            idle_over_three_days: true,
            ..Default::default()
        };
        assert!(derive_work_status(facts).stale);
        facts.pull_request.open = true;
        assert!(!derive_work_status(facts).stale);
        facts.pull_request.open = false;
        facts.item_completed = true;
        assert!(!derive_work_status(facts).stale);
        facts.item_completed = false;
        facts.has_chat = false;
        assert_eq!(derive_work_status(facts).stage, WorkStage::NotStarted);
    }

    #[test]
    fn combines_chats_and_prs_without_combining_independent_checks() {
        let chats = [
            ChatFacts {
                ready_request: true,
                idle_over_three_days: true,
                ..Default::default()
            },
            ChatFacts {
                needs_answer: true,
                ..Default::default()
            },
        ];
        let mut prs = [
            PullRequestFacts {
                open: true,
                approved: true,
                ..Default::default()
            },
            PullRequestFacts {
                open: true,
                checks_passing: true,
                ..Default::default()
            },
        ];
        let row = status(&chats, &prs, None, None);
        assert_eq!(row.stage, WorkStage::PrReview);
        assert!(row.needs_you);
        assert!(!row.stale);
        prs[0].checks_passing = true;
        assert_eq!(
            status(&chats, &prs, None, None).stage,
            WorkStage::ReadyToMerge
        );
        prs[1].merged = true;
        assert_eq!(status(&chats, &prs, None, None).stage, WorkStage::Done);
        assert!(status(&chats, &prs, Some(false), Some(true)).merged_with_open_ticket);
        assert!(!status(&chats, &prs, Some(true), None).merged_with_open_ticket);
    }

    #[test]
    fn branch_review_and_reopened_items_follow_current_facts() {
        let chats = [ChatFacts {
            branch_review: true,
            ..Default::default()
        }];
        assert_eq!(status(&chats, &[], None, None).stage, WorkStage::CodeReview);
        assert_eq!(status(&[], &[], Some(true), None).stage, WorkStage::Done);
        assert_eq!(
            status(&[], &[], Some(false), None).stage,
            WorkStage::NotStarted
        );
    }

    #[test]
    fn unknown_pull_request_state_does_not_hide_a_ticket_as_stale() {
        let chat = ChatFacts {
            idle_over_three_days: true,
            ..Default::default()
        };
        let pr = PullRequestFacts::default();
        assert!(!status(&[chat], &[pr], Some(false), None).stale);
        assert!(status(&[chat], &[], Some(false), None).stale);
    }

    #[test]
    fn completed_item_or_merged_pr_is_done_and_never_stale() {
        let idle = ChatFacts {
            ready_request: true,
            idle_over_three_days: true,
            ..Default::default()
        };
        let completed = status(&[idle], &[], Some(true), None);
        assert_eq!(completed.stage, WorkStage::Done);
        assert!(!completed.stale);
        let merged = PullRequestFacts {
            state_known: true,
            merged: true,
            ..Default::default()
        };
        let row = status(&[idle], &[merged], Some(true), None);
        assert_eq!(row.stage, WorkStage::Done);
        assert!(!row.stale);
    }

    #[test]
    fn archived_item_is_done_and_a_merged_pr_wins_over_a_hold() {
        let flags = ItemFlags {
            archived: true,
            ..Default::default()
        };
        assert_eq!(
            derive_ticket_status(&[], &[], None, None, flags).stage,
            WorkStage::Done
        );
        let merged = PullRequestFacts {
            state_known: true,
            merged: true,
            ..Default::default()
        };
        let held = ItemFlags {
            user_action_open: true,
            ..Default::default()
        };
        let row = derive_ticket_status(&[], &[merged], Some(false), None, held);
        assert_eq!(row.stage, WorkStage::Done);
        assert_eq!(row.hold, Some(WorkHold::UserAction));
    }

    #[test]
    fn holds_come_from_open_actions_before_waiting() {
        let waiting = ItemFlags {
            waiting: true,
            ..Default::default()
        };
        assert_eq!(
            derive_ticket_status(&[], &[], None, None, waiting).hold,
            Some(WorkHold::Waiting)
        );
        let both = ItemFlags {
            waiting: true,
            user_action_open: true,
            ..Default::default()
        };
        assert_eq!(
            derive_ticket_status(&[], &[], None, None, both).hold,
            Some(WorkHold::UserAction)
        );
        assert_eq!(status(&[], &[], None, None).hold, None);
    }

    #[test]
    fn a_linked_pr_without_an_answer_is_pending_until_it_resolves_or_merges() {
        let unknown = PullRequestFacts::default();
        assert!(status(&[], &[unknown], Some(false), None).facts_pending);
        assert!(!status(&[], &[], Some(false), None).facts_pending);
        let known = PullRequestFacts {
            state_known: true,
            ..Default::default()
        };
        assert!(!status(&[], &[known], Some(false), None).facts_pending);
        let merged = PullRequestFacts {
            state_known: true,
            merged: true,
            ..Default::default()
        };
        // One merged PR settles the stage even while another has no answer.
        let row = status(&[], &[merged, unknown], Some(false), None);
        assert_eq!(row.stage, WorkStage::Done);
        assert!(!row.facts_pending);
    }
}
