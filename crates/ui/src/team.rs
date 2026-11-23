//! What the side panel's "Team" tab (and the Item tab's roll-up) shows about
//! the workers of an orchestrating chat: one pure fact sheet ([`TeamFacts`])
//! built from snapshots. Nothing here fetches; activity lines come from the
//! chat's last message preview (the transcript tail the engine already keeps),
//! PR checks/review and diff stats from the existing link-status cache.
//!
//! A *worker* is any other chat that is a child chat of the selected chat
//! (`parent_chat_id`) or that is assigned to a descendant of the selected
//! chat's work item.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use mast_engine::archivable::{Archivable, ArchivableInput, archivable, reconcile};
use mast_engine::pr_ticket_cache::{ChatLinkStatus, ChecksStatus, PrStatus};
use mast_proto::{Chat, ChatIndicator, WorkItemEffectiveStatus, WorkOutlineSnapshot};

use crate::chat_metadata::HeaderTicket;
use mast_proto::work_outline_user_actions::UserActionKind;

/// Longest activity line, in characters.
pub(crate) const ACTIVITY_MAX_CHARS: usize = 110;
/// Finished workers at or above which the Team tab suggests archiving them.
pub(crate) const SUGGEST_ARCHIVE_AT: usize = 3;
/// Workers the Item tab lists before pointing at the Team tab.
pub(crate) const ITEM_TAB_WORKERS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkerState {
    /// Needs an answer from the user (awaiting input, or an open ask).
    Waiting,
    Errored,
    Working,
    Idle,
    /// Its item is completed or the chat is archived.
    Done,
}

/// Display order for the Tracker's worker groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum TrackerGroup {
    NeedsYou,
    ReadyToMerge,
    Errored,
    Working,
    Idle,
    NotStarted,
    Done,
}

/// Evidence for each stage, in display order. Missing evidence stays unlit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TrackerStages {
    pub chat_started: bool,
    pub changes: bool,
    pub pr_open: bool,
    pub checks: bool,
    pub review: bool,
    pub merged: bool,
}

impl WorkerState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Errored => "errored",
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Done => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerPr {
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub number: u64,
    pub url: String,
    pub checks: Option<ChecksStatus>,
    /// "approved" / "changes requested" / "review required", when known.
    pub review: Option<&'static str>,
    pub draft: bool,
    pub state: String,
}

impl WorkerPr {
    fn of(number: u64, url: String, detail: Option<&PrStatus>) -> Self {
        Self {
            additions: detail.and_then(|pr| pr.additions),
            deletions: detail.and_then(|pr| pr.deletions),
            branch: detail.and_then(|pr| pr.branch.clone()),
            head_sha: detail.and_then(|pr| pr.head_sha.clone()),
            number,
            url,
            checks: detail.and_then(|pr| pr.checks),
            review: detail.and_then(|pr| match pr.review_decision.as_deref() {
                Some("APPROVED") => Some("approved"),
                Some("CHANGES_REQUESTED") => Some("changes requested"),
                Some("REVIEW_REQUIRED") => Some("review required"),
                _ => None,
            }),
            draft: detail.is_some_and(|pr| pr.is_draft),
            state: detail.map(|pr| pr.state.clone()).unwrap_or_default(),
        }
    }

    /// "PR #12 · checks passing · approved".
    pub(crate) fn label(&self) -> String {
        let mut out = if self.number > 0 {
            format!("PR #{}", self.number)
        } else {
            "PR".to_owned()
        };
        if self.draft {
            out.push_str(" \u{b7} draft");
        }
        match self.checks {
            Some(ChecksStatus::Passing) => out.push_str(" \u{b7} checks passing"),
            Some(ChecksStatus::Pending) => out.push_str(" \u{b7} checks running"),
            Some(ChecksStatus::Failing) => out.push_str(" \u{b7} checks failing"),
            None => {}
        }
        if let Some(review) = self.review {
            out.push_str(" \u{b7} ");
            out.push_str(review);
        }
        if let (Some(additions), Some(deletions)) = (self.additions, self.deletions) {
            out.push_str(&format!(" · +{additions} −{deletions}"));
        }
        out
    }
}

/// An open "Ready to merge: <branch>" request a worker raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadyAction {
    pub space_id: String,
    pub item_id: String,
    pub request_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerFacts {
    pub chat_id: String,
    /// The worker's item title, else its chat title.
    pub title: String,
    pub work_item_title: Option<String>,
    pub state: WorkerState,
    pub last_active: Option<DateTime<Utc>>,
    /// One line: the last message's first line, clipped.
    pub activity: Option<String>,
    pub branch: Option<String>,
    pub prs: Vec<WorkerPr>,
    pub ticket: Option<HeaderTicket>,
    /// `(added, removed)` lines from the worker checkout's diff stat.
    pub diff: Option<(u32, u32)>,
    pub ready: Option<ReadyAction>,
    /// The chat is archived (the Team tab hides it unless asked).
    pub archived: bool,
    /// The engine's finished-worker verdict; `None` once archived.
    pub archivable: Option<Archivable>,
    /// Why an idle worker was kept by the automatic archive pass; offered
    /// for the user's review.
    pub retained: Option<String>,
}

/// Derive a ticket's lifecycle once when its cached worker facts change.
pub(crate) fn ticket_lifecycle(
    workers: &[&WorkerFacts],
    item_completed: Option<bool>,
    linear_completed: Option<bool>,
    branch_review: bool,
    now: DateTime<Utc>,
) -> crate::tracker_lifecycle::LifecycleRow {
    use crate::tracker_lifecycle::{ChatFacts, PullRequestFacts, derive_ticket_lifecycle};
    let chats: Vec<_> = workers.iter().filter(|worker| !worker.archived).map(|worker| ChatFacts {
        branch_review,
        ready_request: worker.ready.as_ref().is_some_and(|action| action.text.trim().to_ascii_lowercase().starts_with("ready to merge")),
        needs_answer: worker.state == WorkerState::Waiting || worker.ready.is_some(),
        idle_over_three_days: worker.state == WorkerState::Idle && worker.last_active
            .is_some_and(|at| now.signed_duration_since(at) > chrono::Duration::days(3)),
    }).collect();
    let prs: Vec<_> = workers.iter().flat_map(|worker| &worker.prs).map(|pr| PullRequestFacts {
        state_known: !pr.state.is_empty(),
        open: pr.state.eq_ignore_ascii_case("open"),
        merged: pr.state.eq_ignore_ascii_case("merged"),
        approved: pr.review == Some("approved") && !pr.draft,
        changes_requested: pr.review == Some("changes requested"),
        checks_passing: pr.checks == Some(ChecksStatus::Passing),
    }).collect();
    derive_ticket_lifecycle(&chats, &prs, item_completed, linear_completed)
}

impl WorkerFacts {
    pub(crate) fn tracker_group(&self) -> TrackerGroup {
        match (self.state, self.ready.is_some()) {
            (WorkerState::Done, _) => TrackerGroup::Done,
            (WorkerState::Waiting, _) => TrackerGroup::NeedsYou,
            (_, true) => TrackerGroup::ReadyToMerge,
            (WorkerState::Errored, _) => TrackerGroup::Errored,
            (WorkerState::Working, _) => TrackerGroup::Working,
            (WorkerState::Idle, _) => TrackerGroup::Idle,
        }
    }

    pub(crate) fn tracker_stages(&self) -> TrackerStages {
        if self.state == WorkerState::Done || self.prs.iter().any(|pr| pr.state.eq_ignore_ascii_case("merged")) {
            return TrackerStages { chat_started: true, changes: true, pr_open: true, checks: true, review: true, merged: true };
        }
        TrackerStages {
            chat_started: true,
            changes: self.diff.is_some_and(|(added, removed)| added > 0 || removed > 0),
            pr_open: !self.prs.is_empty(),
            checks: self.prs.iter().any(|pr| pr.checks.is_some()),
            review: self.prs.iter().any(|pr| pr.review.is_some()),
            merged: self.prs.iter().any(|pr| pr.state.eq_ignore_ascii_case("merged")),
        }
    }

    /// Sort rank: waiting on you, ready to merge, errored, working, idle, done.
    fn rank(&self) -> u8 {
        self.tracker_group() as u8
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TeamFacts {
    pub workers: Vec<WorkerFacts>,
}

impl TeamFacts {
    pub(crate) fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }

    /// Workers that are not archived: what the Team tab lists and counts.
    pub(crate) fn live(&self) -> impl Iterator<Item = &WorkerFacts> {
        self.workers.iter().filter(|w| !w.archived)
    }

    pub(crate) fn archived(&self) -> impl Iterator<Item = &WorkerFacts> {
        self.workers.iter().filter(|w| w.archived)
    }

    /// Live workers the finished rule says can be archived.
    pub(crate) fn finished(&self) -> Vec<&WorkerFacts> {
        self.live()
            .filter(|w| w.archivable.as_ref().is_some_and(|a| a.archivable))
            .collect()
    }

    fn count(&self, pred: impl Fn(&WorkerFacts) -> bool) -> usize {
        self.live().filter(|w| pred(w)).count()
    }

    pub(crate) fn working(&self) -> usize {
        self.count(|w| w.state == WorkerState::Working)
    }

    pub(crate) fn waiting(&self) -> usize {
        self.count(|w| w.state == WorkerState::Waiting)
    }

    pub(crate) fn ready(&self) -> usize {
        self.count(|w| w.ready.is_some() && w.state != WorkerState::Done)
    }

    /// Nonzero counts in the order shown by the Team summary.
    pub(crate) fn roll_up_parts(&self) -> Vec<(crate::status_style::Status, String)> {
        use crate::status_style::Status;
        let mut parts = Vec::new();
        for (status, count, label) in [
            (Status::Working, self.working(), "working"),
            (Status::Waiting, self.waiting(), "waiting on you"),
            (Status::Ready, self.ready(), "ready to merge"),
            (Status::Errored, self.count(|w| w.state == WorkerState::Errored), "errored"),
        ] {
            if count > 0 { parts.push((status, format!("{count} {label}"))); }
        }
        if parts.is_empty() {
            let done = self.count(|w| w.state == WorkerState::Done);
            if done > 0 && done == self.live().count() {
                parts.push((Status::Done, format!("{done} done")));
            }
        }
        parts
    }

    #[cfg(test)]
    pub(crate) fn roll_up(&self) -> String {
        let parts = self.roll_up_parts();
        if parts.is_empty() { return "No active workers".to_owned(); }
        parts.into_iter().map(|(status, label)| format!("{} {label}", crate::status_style::glyph(status))).collect::<Vec<_>>().join(" · ")
    }

}

/// Everything [`team`] reads.
pub(crate) struct TeamInput<'a> {
    pub chat_id: &'a str,
    pub chats: &'a [Chat],
    pub indicators: &'a HashMap<String, ChatIndicator>,
    pub snapshot: Option<&'a WorkOutlineSnapshot>,
    pub links: &'a HashMap<String, ChatLinkStatus>,
    pub now: DateTime<Utc>,
}

/// One line from a message preview: first non-empty line, whitespace
/// collapsed, clipped with an ellipsis.
pub(crate) fn activity_line(preview: &str) -> Option<String> {
    activity_line_max(preview, ACTIVITY_MAX_CHARS)
}

/// [`activity_line`] with an explicit length.
pub(crate) fn activity_line_max(preview: &str, max: usize) -> Option<String> {
    let line = preview.lines().map(str::trim).find(|l| !l.is_empty())?;
    let plain: String = pulldown_cmark::Parser::new(line).filter_map(|event| match event {
        pulldown_cmark::Event::Text(text) | pulldown_cmark::Event::Code(text) => Some(text.into_string()),
        pulldown_cmark::Event::SoftBreak | pulldown_cmark::Event::HardBreak => Some(" ".to_owned()),
        _ => None,
    }).collect();
    let line = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('\u{2026}');
    }
    Some(out)
}

pub(crate) fn team(input: &TeamInput<'_>) -> TeamFacts {
    let Some(me) = input.chats.iter().find(|c| c.id == input.chat_id) else {
        return TeamFacts::default();
    };
    // Items below the chat's own item.
    let below: HashSet<String> = input
        .snapshot
        .and_then(|snapshot| {
            let item = crate::chat_work_context::assigned_item(snapshot, input.chat_id)?;
            Some(crate::work_outline::model::descendants(snapshot, item))
        })
        .unwrap_or_default();
    // Index the snapshot once. A worker must not search the entire snapshot
    // for its assignment, requests and inherited ticket.
    let mut assignments = HashMap::new();
    let mut items = HashMap::new();
    let mut requests: HashMap<&str, Vec<&mast_proto::UserActionRequestView>> = HashMap::new();
    let mut tickets: HashMap<&str, Option<HeaderTicket>> = HashMap::new();
    let mut item_prs: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut cached_prs = HashMap::new();
    for status in input.links.values() {
        if let Some(pr) = &status.pr && let Some(url) = &pr.url { cached_prs.insert(url.as_str(), pr); }
        for link in &status.pr_links {
            if let Some(detail) = &link.detail { cached_prs.insert(link.url.as_str(), detail); }
        }
    }
    if let Some(snapshot) = input.snapshot {
        let dangling: HashSet<_> = snapshot.assignments.iter()
            .filter(|a| a.state == mast_proto::WorkItemChatAssignmentState::DanglingChat)
            .map(|a| a.chat_id.as_str()).collect();
        let mut direct = HashMap::new();
        for reference in &snapshot.external_references {
            if reference.provider == mast_proto::pr_review_sync::GITHUB_PROVIDER && reference.reference_type == mast_proto::pr_review_sync::GITHUB_PULL_REQUEST_REFERENCE {
                item_prs.entry(reference.work_item_id.as_str()).or_default().push(reference.url.as_str());
            }
            if reference.provider == mast_proto::linear::LINEAR_PROVIDER && reference.reference_type == mast_proto::linear::LINEAR_ISSUE_REFERENCE {
                direct.entry(reference.work_item_id.as_str()).and_modify(|prior: &mut &mast_proto::WorkItemExternalReferenceView| {
                    if (reference.created_at, &reference.id) < (prior.created_at, &prior.id) { *prior = reference; }
                }).or_insert(reference);
            }
        }
        for item in &snapshot.items {
            items.insert(item.id.as_str(), item);
            for id in &item.chat_ids {
                if !dangling.contains(id.as_str()) { assignments.entry(id.as_str()).or_insert(item.id.as_str()); }
            }
            let inherited = item.parent_work_item_id.as_deref().and_then(|id| tickets.get(id)).cloned().flatten();
            let ticket = direct.get(item.id.as_str()).map(|reference| HeaderTicket {
                identifier: reference.external_id.clone(), title: None, url: Some(reference.url.clone()), state: None,
            }).or(inherited);
            tickets.insert(item.id.as_str(), ticket);
        }
        for request in &snapshot.user_action_requests {
            if !request.deduped { requests.entry(request.chat_id.as_str()).or_default().push(request); }
        }
    }
    let mut workers = Vec::new();
    for chat in input.chats {
        // A Manager's first start is hidden until its message is queued.
        if chat.id == input.chat_id || chat.standing_role_provisional().is_some() {
            continue;
        }
        let item_id = (chat.space_id == me.space_id).then(|| assignments.get(chat.id.as_str()).copied()).flatten();
        let under_item = item_id.is_some_and(|id| below.contains(id));
        let child_chat = chat.parent_chat_id.as_deref() == Some(input.chat_id);
        if !(under_item || child_chat) {
            continue;
        }
        let item = item_id.and_then(|id| items.get(id).copied());
        let worker_requests = requests.get(chat.id.as_str()).map(Vec::as_slice).unwrap_or_default();
        let inherited_ticket = item_id.and_then(|id| tickets.get(id)).cloned().flatten();
        let mut worker = worker_with_index(chat, item, input, worker_requests, inherited_ticket);
        if let Some(urls) = item_id.and_then(|id| item_prs.get(id)) {
            for &url in urls {
                if worker.prs.iter().any(|pr| pr.url.eq_ignore_ascii_case(url)) { continue; }
                let number = crate::chat_work_context::parse_pr_url(url).map_or(0, |pr| pr.number);
                worker.prs.push(WorkerPr::of(number, url.to_owned(), cached_prs.get(url).copied()));
            }
        }
        if let Some(branch) = worker.prs.iter().filter(|pr| pr.branch.is_some()).min_by_key(|pr| {
            if pr.state.eq_ignore_ascii_case("open") { 0 } else if pr.state.eq_ignore_ascii_case("merged") { 1 } else { 2 }
        }).and_then(|pr| pr.branch.clone()) {
            worker.branch = Some(branch);
        } else if !input.links.get(&chat.id).is_some_and(|status| status.is_worktree)
            && worker.branch.as_deref().is_some_and(|branch| branch == "main" || branch == "master") {
            worker.branch = None;
        }
        workers.push(worker);
    }
    workers.sort_by(|a, b| {
        (a.rank(), std::cmp::Reverse(a.last_active), &a.chat_id).cmp(&(
            b.rank(),
            std::cmp::Reverse(b.last_active),
            &b.chat_id,
        ))
    });
    TeamFacts { workers }
}

/// Build a worker from already indexed item, requests and inherited ticket.
pub(crate) fn worker_with_index(
    chat: &Chat,
    item: Option<&mast_proto::WorkItemView>,
    input: &TeamInput<'_>,
    requests: &[&mast_proto::UserActionRequestView],
    inherited_ticket: Option<HeaderTicket>,
) -> WorkerFacts {
    let mut ready = None;
    let mut asks = false;
    if let Some(snapshot) = input.snapshot {
        for request in requests.iter().copied().filter(|request| request.is_open()) {
            if request.effective_kind() == UserActionKind::Notice
                || request.priority == Some(mast_proto::work_outline_user_actions::UserActionPriority::Fyi)
            {
                continue;
            }
            if request.effective_kind() == UserActionKind::Handoff
                && request.priority != Some(mast_proto::work_outline_user_actions::UserActionPriority::Blocking)
            {
                ready.get_or_insert_with(|| ReadyAction {
                    space_id: snapshot.space_id.clone(),
                    item_id: request.work_item_id.clone(),
                    request_id: request.id.clone(),
                    text: request.text.clone(),
                });
            } else if request.priority != Some(mast_proto::work_outline_user_actions::UserActionPriority::Normal) {
                asks = true;
            }
        }
    }
    let indicator = input
        .indicators
        .get(&chat.id)
        .copied()
        .unwrap_or(ChatIndicator::Idle);
    let item_done = item.is_some_and(|i| matches!(i.effective_status, WorkItemEffectiveStatus::Completed));
    let state = if item_done || chat.archived {
        WorkerState::Done
    } else if asks || indicator == ChatIndicator::AwaitingInput {
        WorkerState::Waiting
    } else {
        match indicator {
            ChatIndicator::Working => WorkerState::Working,
            ChatIndicator::Errored => WorkerState::Errored,
            _ => WorkerState::Idle,
        }
    };
    let status = input.links.get(&chat.id);
    let mut prs: Vec<WorkerPr> = status
        .map(|s| {
            if s.pr_links.is_empty() {
                s.pr.iter()
                    .map(|pr| WorkerPr::of(pr.number, pr.url.clone().unwrap_or_default(), Some(pr)))
                    .collect()
            } else {
                s.pr_links
                    .iter()
                    .map(|l| {
                        let number = l.detail.as_ref().map(|d| d.number).or_else(|| crate::chat_work_context::parse_pr_url(&l.url).map(|pr| pr.number)).unwrap_or(0);
                        WorkerPr::of(number, l.url.clone(), l.detail.as_ref())
                    })
                    .collect()
            }
        })
        .unwrap_or_default();
    if prs.is_empty()
        && let Some(url) = chat.linked_pr_url.clone()
    {
        let number = crate::chat_work_context::parse_pr_url(&url).map_or(0, |pr| pr.number);
        prs.push(WorkerPr::of(number, url, None));
    }
    let ticket = status
        .and_then(|s| s.ticket.as_ref())
        .map(|t| HeaderTicket {
            identifier: t.identifier.clone(),
            title: t.title.clone(),
            url: t.url.clone(),
            state: t.status.clone(),
        })
        .or(inherited_ticket);
    let title = item
        .map(|i| i.title.trim())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .or_else(|| chat.title.as_deref().map(crate::transcript::single_line))
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "New session".to_owned());
    let verdict_input = ArchivableInput {
        chat,
        indicator,
        queued: false,
        item,
        requests: requests.to_vec(),
        pr_states: prs
            .iter()
            .filter(|pr| !pr.state.is_empty())
            .map(|pr| pr.state.clone())
            .collect(),
        now: input.now,
    };
    let archivable = (!chat.archived).then(|| archivable(&verdict_input));
    let retained = (!chat.archived && state == WorkerState::Idle)
        .then(|| reconcile(&verdict_input))
        .filter(|verdict| !verdict.archivable)
        .map(|verdict| verdict.reason);
    WorkerFacts {
        chat_id: chat.id.clone(),
        title,
        work_item_title: item.map(|item| item.title.trim()).filter(|title| !title.is_empty()).map(str::to_owned),
        state,
        last_active: chat.last_message_at.or(Some(chat.created_at)),
        activity: chat.last_message_preview.as_deref().and_then(|text| activity_line(&crate::message_source::preview(text))),
        branch: chat
            .branch
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .map(str::to_owned),
        prs,
        ticket,
        diff: status
            .and_then(|s| s.diff_stat.as_ref())
            .map(|d| (d.lines_added, d.lines_removed)),
        ready,
        archived: chat.archived,
        archivable,
        retained,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts_of(states: &[(WorkerState, bool)]) -> TeamFacts {
        TeamFacts {
            workers: states
                .iter()
                .enumerate()
                .map(|(i, (state, ready))| WorkerFacts {
                    chat_id: format!("c{i}"),
                    title: format!("w{i}"),
                    work_item_title: Some(format!("w{i}")),
                    state: *state,
                    last_active: None,
                    activity: None,
                    branch: None,
                    prs: Vec::new(),
                    ticket: None,
                    diff: None,
                    archived: false,
                    archivable: None,
                    retained: None,
                    ready: ready.then(|| ReadyAction {
                        space_id: "s".into(),
                        item_id: "i".into(),
                        request_id: "r".into(),
                        text: "Ready to merge: b".into(),
                    }),
                })
                .collect(),
        }
    }

    #[test]
    fn a_side_chat_is_never_a_worker() {
        let chat = |id: &str| -> Chat {
            serde_json::from_value(serde_json::json!({
                "id": id, "deviceId": "d", "title": id, "archived": false, "cwd": "/w",
                "branch": null, "checkoutId": null, "config": null, "lastMessagePreview": null,
                "lastMessageAt": null, "createdAt": "2026-01-01T00:00:00Z", "spaceId": "s",
            }))
            .unwrap()
        };
        let mut side = chat("side");
        side.side_of = Some("main".into());
        let mut worker = chat("worker");
        worker.parent_chat_id = Some("main".into());
        let chats = vec![chat("main"), side, worker];
        let facts = team(&TeamInput {
            chat_id: "main",
            chats: &chats,
            indicators: &HashMap::new(),
            snapshot: None,
            links: &HashMap::new(),
            now: Utc::now(),
        });
        let ids: Vec<&str> = facts.workers.iter().map(|w| w.chat_id.as_str()).collect();
        assert_eq!(ids, ["worker"]);
    }

    #[test]
    fn the_roll_up_counts_and_skips_zero_parts() {
        use WorkerState::*;
        let team = facts_of(&[
            (Working, false),
            (Working, false),
            (Working, false),
            (Working, false),
            (Waiting, false),
            (Idle, true),
            (Idle, true),
            (Done, true),
        ]);
        assert_eq!(team.roll_up(), "◌ 4 working \u{b7} ! 1 waiting on you \u{b7} → 2 ready to merge");
        assert_eq!(facts_of(&[(Done, false)]).roll_up(), "✓ 1 done");
        assert_eq!(facts_of(&[(Idle, false)]).roll_up(), "No active workers");
    }

    #[test]
    fn activity_is_the_first_line_collapsed_and_clipped() {
        assert_eq!(activity_line("  \n Running   cargo test\nmore").as_deref(), Some("Running cargo test"));
        assert_eq!(activity_line("[project#42](https://github.com/example/project/pull/42) remains **open**").as_deref(), Some("project#42 remains open"));
        assert_eq!(activity_line("   "), None);
        let long = activity_line(&"x".repeat(300)).unwrap();
        assert_eq!(long.chars().count(), ACTIVITY_MAX_CHARS + 1);
        assert!(long.ends_with('\u{2026}'));
    }

    #[test]
    fn item_pull_request_reference_advances_tracker_stage() {
        let chats: Vec<Chat> = ["coordinator", "worker"].into_iter().map(|id| serde_json::from_value(serde_json::json!({
            "id": id, "deviceId": "fixture", "spaceId": "fixture", "archived": false, "createdAt": "2026-01-01T00:00:00Z"
        })).unwrap()).collect();
        let snapshot: WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({
            "spaceId": "fixture", "items": [
                {"id":"root", "spaceId":"fixture", "title":"Root", "chatIds":["coordinator"], "childIds":["item"]},
                {"id":"item", "spaceId":"fixture", "title":"Item", "parentWorkItemId":"root", "chatIds":["worker"]}
            ], "externalReferences": [{"id":"pr", "spaceId":"fixture", "workItemId":"item", "provider":"github",
                "referenceType":"pullRequest", "externalId":"example/project#42", "url":"https://github.com/example/project/pull/42"}]
        })).unwrap();
        let indicators = HashMap::new();
        let links = HashMap::new();
        let facts = team(&TeamInput { chat_id: "coordinator", chats: &chats, indicators: &indicators,
            snapshot: Some(&snapshot), links: &links, now: Utc::now() });
        assert_eq!(facts.workers[0].prs[0].number, 42);
        assert!(facts.workers[0].tracker_stages().pr_open);
    }

    #[test]
    fn pr_chips_carry_checks_and_review() {
        let pr = WorkerPr {
            branch: None, head_sha: None, additions: Some(312), deletions: Some(48),
            number: 12,
            url: "u".into(),
            checks: Some(ChecksStatus::Passing),
            review: Some("approved"),
            draft: false,
            state: "OPEN".into(),
        };
        assert_eq!(pr.label(), "PR #12 \u{b7} checks passing \u{b7} approved · +312 −48");
    }

    #[test]
    fn tracker_groups_prioritize_requests_and_completed_items() {
        use WorkerState::*;
        let facts = facts_of(&[(Waiting, true), (Idle, true), (Errored, false),
            (Working, false), (Idle, false), (Done, false), (Done, true)]);
        let groups: Vec<_> = facts.workers.iter().map(WorkerFacts::tracker_group).collect();
        assert_eq!(groups, [TrackerGroup::NeedsYou, TrackerGroup::ReadyToMerge,
            TrackerGroup::Errored, TrackerGroup::Working, TrackerGroup::Idle, TrackerGroup::Done, TrackerGroup::Done]);
        assert!(TrackerGroup::Idle < TrackerGroup::NotStarted);
        assert!(TrackerGroup::NotStarted < TrackerGroup::Done);
    }

    #[test]
    fn tracker_stages_require_observed_facts() {
        let mut worker = facts_of(&[(WorkerState::Working, false)]).workers.remove(0);
        assert_eq!(worker.tracker_stages(), TrackerStages {
            chat_started: true, changes: false, pr_open: false, checks: false,
            review: false, merged: false,
        });
        worker.diff = Some((2, 0));
        worker.prs.push(WorkerPr { branch: None, head_sha: None, additions: None, deletions: None, number: 12, url: "u".into(), checks: Some(ChecksStatus::Pending),
            review: Some("approved"), draft: false, state: "OPEN".into() });
        assert_eq!(worker.tracker_stages(), TrackerStages {
            chat_started: true, changes: true, pr_open: true, checks: true,
            review: true, merged: false,
        });
        worker.prs[0].state = "MERGED".into();
        assert!(worker.tracker_stages().merged);
        assert!(worker.tracker_stages().pr_open);
    }

    #[test]
    fn ticket_lifecycle_uses_all_workers_and_preserves_pr_precedence() {
        use crate::tracker_lifecycle::LifecycleStage;
        let mut facts = facts_of(&[(WorkerState::Waiting, false), (WorkerState::Idle, true)]);
        facts.workers[1].prs.push(WorkerPr { branch: None, head_sha: None, additions: None,
            deletions: None, number: 1, url: "u".into(), checks: Some(ChecksStatus::Passing),
            review: Some("approved"), draft: false, state: "OPEN".into() });
        let workers: Vec<_> = facts.workers.iter().collect();
        let row = ticket_lifecycle(&workers, Some(false), None, false, Utc::now());
        assert_eq!(row.stage, LifecycleStage::ReadyToMerge);
        assert!(row.needs_you);
        facts.workers[1].prs[0].draft = true;
        let workers: Vec<_> = facts.workers.iter().collect();
        assert_eq!(ticket_lifecycle(&workers, Some(false), None, false, Utc::now()).stage, LifecycleStage::PrReview);
    }
}
