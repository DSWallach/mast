//! Pure lifecycle derivation from a ticket's cached facts.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LifecycleStage {
    NotStarted,
    Building,
    CodeReview,
    PrReview,
    ReadyToMerge,
    Done,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PullRequestFacts {
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
    pub item_completed: bool,
    pub linear_completed: bool,
    pub pull_request: PullRequestFacts,
    pub needs_answer: bool,
    pub idle_over_three_days: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LifecycleRow {
    pub stage: LifecycleStage,
    pub needs_you: bool,
    pub stale: bool,
    pub merged_with_open_ticket: bool,
    pub linear_completed_with_open_pr: bool,
}

pub(crate) fn derive_lifecycle(facts: LifecycleFacts) -> LifecycleRow {
    let pr = facts.pull_request;
    let stage = if pr.merged {
        LifecycleStage::Done
    } else if pr.open {
        if pr.approved && !pr.changes_requested && pr.checks_passing {
            LifecycleStage::ReadyToMerge
        } else {
            LifecycleStage::PrReview
        }
    } else if facts.item_completed || facts.linear_completed {
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
        merged_with_open_ticket: pr.merged && !facts.item_completed && !facts.linear_completed,
        linear_completed_with_open_pr: facts.linear_completed && pr.open && !pr.merged,
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
        facts.pull_request.open = true;
        let row = derive_lifecycle(facts);
        assert_eq!(row.stage, LifecycleStage::PrReview);
        assert!(row.linear_completed_with_open_pr);
        facts.pull_request.approved = true;
        facts.pull_request.changes_requested = true;
        facts.pull_request.checks_passing = true;
        let row = derive_lifecycle(facts);
        assert_eq!(row.stage, LifecycleStage::PrReview);
        assert!(row.needs_you);
        facts.pull_request.changes_requested = false;
        facts.pull_request.checks_passing = false;
        assert_eq!(derive_lifecycle(facts).stage, LifecycleStage::PrReview);
        facts.pull_request.merged = true;
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
}
