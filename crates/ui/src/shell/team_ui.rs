//! The side panel's Team tab: one row per worker of an orchestrating chat,
//! a roll-up line on top, and the Item tab's compact "top workers" block. Both
//! read one [`TeamFacts`] sheet (`crate::team`), so they cannot disagree.
//!
//! ⌥⌘T opens the tab from anywhere; a plain `t` does the same while the side
//! panel itself (not a text field inside it) holds focus. Both do nothing for
//! a chat without workers.
use super::*;
use crate::components::shell_parts::team_button;

use crate::shell::chat_item_ui::{ChipTone, chip_palette};
use crate::team::{
    ITEM_TAB_WORKERS, SUGGEST_ARCHIVE_AT, TeamFacts, TeamInput, WorkerFacts, WorkerPr,
    team,
};
use crate::tracker_lifecycle::{LifecycleRow, LifecycleStage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
enum TrackerFilter {
    #[default]
    All,
    NeedsYou,
    Stage(LifecycleStage),
}

#[derive(Default)]
pub(super) struct TrackerState {
    key: Option<(String, Option<u64>, u64, u64, u64)>,
    facts: TeamFacts,
    all_workers: TeamFacts,
    not_started: Vec<crate::board::BoardItem>,
    completed_items: Vec<crate::board::BoardItem>,
    other_chats: Vec<WorkerFacts>,
    additional_chats: std::collections::HashMap<String, Vec<WorkerFacts>>,
    expanded_chats: std::collections::HashSet<String>,
    grouped_rows: std::collections::HashMap<(TrackerFilter, LifecycleStage), Vec<usize>>,
    stale_rows: std::collections::HashMap<TrackerFilter, Vec<usize>>,
    lifecycle_rows: std::collections::HashMap<String, LifecycleRow>,
    linear_projects: std::collections::HashMap<(String, String, u64), Vec<mast_proto::linear::LinearIssueSummary>>,
    linear_loading: std::collections::HashSet<(String, String, u64)>,
    linear_errors: std::collections::HashMap<(String, String, u64), crate::linear::client::LinearUiError>,
    linear_project_linked: bool,
    source_label: String,
    source_notice: Option<String>,
    other_expanded: bool,
    stale_expanded: bool,
    counts: [usize; 6],
    needs_you: usize,
    metadata: std::collections::HashMap<String, (String, String, Option<String>)>,
    requests: std::collections::HashMap<String, String>,
    stalled: std::collections::HashMap<String, (String, bool)>,
    todos: std::collections::HashMap<String, mast_proto::TodoProgress>,
    review_progress: std::collections::HashMap<(String, Option<String>), (usize, usize)>,
    review_targets: std::collections::HashMap<String, (String, Option<String>)>,
    review_generation: u64,
    review_task: Option<Task<()>>,
    filter: TrackerFilter,
    wake_settings_open: bool,
    #[cfg(test)]
    pub(super) rebuilds: usize,
}

fn tracker_matches(row: &LifecycleRow, filter: TrackerFilter) -> bool {
    match filter {
        TrackerFilter::All => true,
        TrackerFilter::NeedsYou => row.needs_you,
        TrackerFilter::Stage(stage) => row.stage == stage,
    }
}

fn tracker_headline(ticket: Option<&crate::chat_metadata::HeaderTicket>, fallback: &str) -> String {
    ticket.and_then(|ticket| ticket.title.as_deref()).filter(|title| !title.is_empty()).unwrap_or(fallback).to_owned()
}

fn tracker_todo_label(progress: mast_proto::TodoProgress) -> String {
    format!("{}/{}", progress.done, progress.total)
}

fn tracker_select_rows(facts: &mut TeamFacts, snapshot: Option<&mast_proto::WorkOutlineSnapshot>, chat_id: &str) -> (Vec<WorkerFacts>, std::collections::HashMap<String, Vec<WorkerFacts>>) {
    let mut other_chats = Vec::new();
    let mut additional_chats = std::collections::HashMap::new();
    if let Some(snapshot) = snapshot {
        let root = crate::chat_work_context::assigned_item(snapshot, chat_id);
        let mut assignments = std::collections::HashMap::new();
        for item in &snapshot.items {
            for id in &item.chat_ids { assignments.insert(id.as_str(), item); }
        }
        facts.workers.sort_by_key(|worker| {
            let rank = match worker.state { crate::team::WorkerState::Waiting => 0, crate::team::WorkerState::Working => 1, _ => 2 };
            (worker.archived, rank, std::cmp::Reverse(worker.last_active))
        });
        let mut selected = std::collections::HashMap::<String, usize>::new();
        let mut rows: Vec<WorkerFacts> = Vec::new();
        for worker in facts.workers.drain(..) {
            let Some(item) = assignments.get(worker.chat_id.as_str()) else { other_chats.push(worker); continue; };
            if Some(item.id.as_str()) == root || !item.child_ids.is_empty() { other_chats.push(worker); continue; }
            if let Some(&index) = selected.get(&item.id) {
                for pr in &worker.prs { if !rows[index].prs.iter().any(|existing| existing.url == pr.url) { rows[index].prs.push(pr.clone()); } }
                additional_chats.entry(rows[index].chat_id.clone()).or_insert_with(Vec::new).push(worker);
            } else {
                selected.insert(item.id.clone(), rows.len());
                rows.push(worker);
            }
        }
        rows.sort_by_key(|worker| (worker.tracker_group(), std::cmp::Reverse(worker.last_active), worker.chat_id.clone()));
        facts.workers = rows;
    }
    (other_chats, additional_chats)
}

fn tracker_reconcile_project(
    snapshot: &mast_proto::WorkOutlineSnapshot,
    root: &str,
    items: &[(crate::board::BoardItem, bool)],
    workers: &TeamFacts,
    issues: &[mast_proto::linear::LinearIssueSummary],
) -> (TeamFacts, Vec<WorkerFacts>, std::collections::HashMap<String, Vec<WorkerFacts>>, Vec<crate::board::BoardItem>, std::collections::HashMap<String, (Option<bool>, Option<bool>)>) {
    use crate::tracker_tickets::{BranchChat, LocalTicket, reconcile_tickets};
    let item_map: std::collections::HashMap<_, _> = items.iter().map(|(item, _)| (item.id.as_str(), item)).collect();
    let worker_map: std::collections::HashMap<_, _> = workers.workers.iter().enumerate().map(|(index, worker)| (worker.chat_id.as_str(), (index, worker))).collect();
    let snapshot_items: std::collections::HashMap<_, _> = snapshot.items.iter().map(|item| (item.id.as_str(), item)).collect();
    let assignments: std::collections::HashMap<_, _> = snapshot.items.iter().flat_map(|item| item.chat_ids.iter().map(move |id| (id.as_str(), item))).collect();
    let mut references = std::collections::HashMap::<&str, &mast_proto::WorkItemExternalReferenceView>::new();
    for reference in snapshot.external_references.iter().filter(|reference| reference.provider == mast_proto::linear::LINEAR_PROVIDER
        && reference.reference_type == mast_proto::linear::LINEAR_ISSUE_REFERENCE) {
        references.entry(reference.work_item_id.as_str()).and_modify(|prior| {
            if (reference.created_at, &reference.id) < (prior.created_at, &prior.id) { *prior = reference; }
        }).or_insert(reference);
    }
    let local: Vec<_> = items.iter().filter_map(|(item, _)| {
        let source = snapshot_items.get(item.id.as_str())?;
        Some(LocalTicket { item_id: item.id.clone(), issue_identifier: references.get(item.id.as_str()).map(|reference| reference.external_id.clone()),
            chat_ids: source.chat_ids.iter().filter(|id| worker_map.get(id.as_str()).is_some_and(|(_, worker)| !worker.archived)).cloned().collect() })
    }).collect();
    let branches: Vec<_> = workers.live().filter(|worker| assignments.get(worker.chat_id.as_str())
        .is_none_or(|item| item.id != root && item.child_ids.is_empty()))
        .filter_map(|worker| Some(BranchChat { chat_id: worker.chat_id.clone(), branch: worker.branch.clone()? })).collect();
    let identifiers: Vec<_> = issues.iter().map(|issue| issue.identifier.clone()).collect();
    let issue_map: std::collections::HashMap<_, _> = issues.iter().map(|issue| (issue.identifier.to_ascii_uppercase(), issue)).collect();
    let mut result = TeamFacts::default();
    let mut additional = std::collections::HashMap::new();
    let mut empty = Vec::new();
    let mut completion = std::collections::HashMap::new();
    let mut selected = std::collections::HashSet::new();
    for row in reconcile_tickets(&identifiers, &local, &branches) {
        let issue = row.issue_identifier.as_ref().and_then(|id| issue_map.get(&id.to_ascii_uppercase()).copied());
        let mut chats: Vec<_> = row.chat_ids.iter().filter_map(|id| worker_map.get(id.as_str()).copied()).collect();
        chats.sort_by_key(|(index, _)| *index);
        if let Some((_, first)) = chats.first().copied() {
            let mut primary = first.clone();
            selected.insert(primary.chat_id.clone());
            let mut others = Vec::new();
            for (_, worker) in chats.into_iter().skip(1) {
                selected.insert(worker.chat_id.clone());
                for pr in &worker.prs { if !primary.prs.iter().any(|existing| existing.url == pr.url) { primary.prs.push(pr.clone()); } }
                others.push(worker.clone());
            }
            if let Some(issue) = issue {
                primary.ticket = Some(crate::chat_metadata::HeaderTicket { identifier: issue.identifier.clone(), title: Some(issue.title.clone()),
                    url: Some(issue.url.clone()), state: Some(issue.state.name.clone()) });
            } else {
                primary.ticket = None;
            }
            let item_done = (!row.item_ids.is_empty()).then(|| row.item_ids.iter().any(|id|
                snapshot_items.get(id.as_str()).is_some_and(|item| item.effective_status == mast_proto::WorkItemEffectiveStatus::Completed)));
            completion.insert(primary.chat_id.clone(), (item_done, issue.map(|issue| issue.state.state_type.is_closed())));
            if !others.is_empty() { additional.insert(primary.chat_id.clone(), others); }
            result.workers.push(primary);
        } else {
            let mut item = row.item_ids.first().and_then(|id| item_map.get(id.as_str()).copied()).cloned().unwrap_or_else(|| crate::board::BoardItem {
                id: format!("linear:{}", row.issue_identifier.as_deref().unwrap_or_default()), title: String::new(),
                status: crate::board::BoardStatus::Todo, updated_at: 0, created_at: 0, worker: None, worker_title: None,
                report: None, ticket: None, request_id: None,
            });
            if let Some(issue) = issue {
                item.title = issue.title.clone();
                item.ticket = Some(issue.identifier.clone());
                if issue.state.state_type.is_closed() { item.status = crate::board::BoardStatus::Done; }
            } else {
                item.ticket = None;
            }
            if row.item_ids.iter().any(|id| item_map.get(id.as_str()).is_some_and(|item| item.status == crate::board::BoardStatus::Done)) {
                item.status = crate::board::BoardStatus::Done;
            }
            empty.push(item);
        }
    }
    let other = workers.workers.iter().filter(|worker| !worker.archived && !selected.contains(&worker.chat_id)).cloned().collect();
    (result, other, additional, empty, completion)
}

fn tracker_primary_pr(prs: &[WorkerPr]) -> Option<&WorkerPr> {
    prs.iter().min_by_key(|pr| if pr.state.eq_ignore_ascii_case("open") { 0 } else if pr.state.eq_ignore_ascii_case("merged") { 1 } else if pr.state.eq_ignore_ascii_case("closed") { 2 } else { 3 })
}

fn tracker_pr_reference(pr: &WorkerPr) -> String {
    crate::chat_work_context::parse_pr_url(&pr.url).map(|reference| format!("{}#{}", reference.repo, reference.number))
        .unwrap_or_else(|| if pr.number > 0 { format!("#{}", pr.number) } else { pr.url.clone() })
}

fn reviewed_files_at_head(state: Option<mast_proto::DiffReviewState>, head: Option<&str>) -> usize {
    state.filter(|state| head.is_some_and(|head| state.head_sha.as_deref().is_some_and(|saved| saved.eq_ignore_ascii_case(head))))
        .map_or(0, |state| state.viewed_files.len())
}

fn tracker_stage_track(id: &str, stages: [bool; 6], theme: &Theme) -> AnyElement {
    let labels = ["Not started", "Building", "Code review", "PR review", "Ready to merge", "Done"];
    let mut track = div().flex().items_center().gap(px(Theme::SPACE_XS));
    for (index, (label, lit)) in labels.into_iter().zip(stages).enumerate() {
        let tip = SharedString::from(format!("{label}: {}", if lit { "observed" } else { "not observed" }));
        track = track.child(crate::components::shell_parts::tracker_stage_indicator(theme, SharedString::from(format!("tracker-stage-{id}-{index}")), lit)
            .debug_selector(move || format!("tracker-stage-{label}-{lit}"))
            .tooltip(move |_, cx| cx.new(|_| SurfaceTabTooltip { text: tip.clone() }).into()));
    }
    if let Some(label) = labels.into_iter().zip(stages).filter(|(_, lit)| *lit).map(|(label, _)| label).last() {
        track = track.child(crate::components::shell_parts::tracker_stage_label(theme, if stages[5] { "Done" } else { label }));
    }
    track.into_any_element()
}

#[cfg(test)]
mod tracker_tests {
    use super::*;
    use crate::team::WorkerState;

    fn worker(id: &str, state: WorkerState) -> WorkerFacts {
        WorkerFacts { chat_id: id.into(), title: id.into(), work_item_title: Some(id.into()), state, last_active: None, activity: None,
            branch: None, prs: Vec::new(), ticket: None, diff: None, ready: None, archived: false, archivable: None }
    }

    #[test]
    fn tracker_filters_by_lifecycle_and_needs_you() {
        let worker = worker("first", WorkerState::Waiting);
        let row = crate::team::ticket_lifecycle(&[&worker], Some(false), None, false, chrono::Utc::now());
        assert!(tracker_matches(&row, TrackerFilter::NeedsYou));
        assert!(tracker_matches(&row, TrackerFilter::Stage(LifecycleStage::Building)));
        assert!(!tracker_matches(&row, TrackerFilter::Stage(LifecycleStage::Done)));
    }

    #[test]
    fn tracker_selects_open_pr_and_preserves_unloaded_reference() {
        let pr = |number, state: &str| WorkerPr { branch: None, head_sha: None, additions: None, deletions: None, number,
            url: format!("https://github.com/example/project/pull/{number}"), checks: None, review: None, draft: false, state: state.into() };
        let prs = vec![pr(1, "CLOSED"), pr(2, "MERGED"), pr(3, "OPEN"), pr(4, "")];
        assert_eq!(tracker_primary_pr(&prs).unwrap().number, 3);
        assert_eq!(tracker_pr_reference(&prs[3]), "project#4");
    }

    #[test]
    fn tracker_selects_one_chat_per_leaf_and_excludes_containers() {
        let snapshot: mast_proto::WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({
            "spaceId": "fixture", "items": [
                {"id":"root", "spaceId":"fixture", "title":"Root", "chatIds":["coordinator", "planning"], "childIds":["container"]},
                {"id":"container", "spaceId":"fixture", "title":"Container", "chatIds":["container-chat"], "childIds":["item"]},
                {"id":"item", "spaceId":"fixture", "title":"Item", "chatIds":["idle", "waiting"]}
            ]
        })).unwrap();
        let mut facts = TeamFacts { workers: vec![worker("idle", WorkerState::Idle), worker("waiting", WorkerState::Waiting),
            worker("planning", WorkerState::Idle), worker("container-chat", WorkerState::Working)] };
        let (other, additional) = tracker_select_rows(&mut facts, Some(&snapshot), "coordinator");
        assert_eq!(facts.workers.len(), 1);
        assert_eq!(facts.workers[0].chat_id, "waiting");
        assert_eq!(additional["waiting"][0].chat_id, "idle");
        assert_eq!(other.len(), 2);
    }

    #[test]
    fn tracker_reconciles_linear_items_branches_and_local_tickets() {
        let snapshot: mast_proto::WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({
            "spaceId": "fixture", "items": [
                {"id":"root", "spaceId":"fixture", "title":"Root", "childIds":["a","b"]},
                {"id":"a", "spaceId":"fixture", "title":"A", "parentWorkItemId":"root", "chatIds":["one"]},
                {"id":"b", "spaceId":"fixture", "title":"B", "parentWorkItemId":"root"}
            ], "externalReferences": [{"id":"ref", "spaceId":"fixture", "workItemId":"a", "provider":"linear", "referenceType":"issue", "externalId":"EX-1"}]
        })).unwrap();
        let item = |id: &str| crate::board::BoardItem { id: id.into(), title: id.into(), status: crate::board::BoardStatus::Todo,
            updated_at: 0, created_at: 0, worker: None, worker_title: None, report: None, ticket: None, request_id: None };
        let items = vec![(item("a"), false), (item("b"), true)];
        let mut branch = worker("two", WorkerState::Working);
        branch.branch = Some("feature/ex-1-work".into());
        let workers = TeamFacts { workers: vec![worker("one", WorkerState::Waiting), branch] };
        let issues = vec![mast_proto::linear::LinearIssueSummary { id: "i".into(), identifier: "EX-1".into(), title: "Issue".into(), ..Default::default() },
            mast_proto::linear::LinearIssueSummary { id: "j".into(), identifier: "EX-2".into(), title: "No worker".into(),
                state: mast_proto::linear::LinearState { state_type: mast_proto::linear::LinearStateType::Completed, ..Default::default() }, ..Default::default() }];
        let (rows, other, additional, empty, statuses) = tracker_reconcile_project(&snapshot, "root", &items, &workers, &issues);
        assert_eq!(rows.workers.len(), 1);
        assert_eq!(rows.workers[0].ticket.as_ref().unwrap().identifier, "EX-1");
        assert_eq!(additional["one"][0].chat_id, "two");
        assert!(other.is_empty());
        assert_eq!(statuses["one"], (Some(false), Some(false)));
        assert_eq!(empty.len(), 2);
        assert!(empty.iter().any(|item| item.ticket.as_deref() == Some("EX-2") && item.status == crate::board::BoardStatus::Done));
        assert!(empty.iter().any(|item| item.id == "b"));
    }

    #[test]
    fn tracker_headline_uses_ticket_title_or_work_item() {
        let mut ticket = crate::chat_metadata::HeaderTicket { identifier: "ENG-42".into(), title: Some("Review changes".into()), ..Default::default() };
        assert_eq!(tracker_headline(Some(&ticket), "Work item"), "Review changes");
        ticket.title = None;
        assert_eq!(tracker_headline(Some(&ticket), "Work item"), "Work item");
    }

    #[test]
    fn tracker_todo_label_is_a_count() {
        assert_eq!(tracker_todo_label(mast_proto::TodoProgress { done: 3, total: 7 }), "3/7");
    }

    #[test]
    fn tracker_review_marks_require_the_current_head() {
        let mut state = mast_proto::DiffReviewState { target: "pr:example/project#1".into(), head_sha: Some("abc".into()), ..Default::default() };
        state.viewed_files.insert("file.rs".into());
        assert_eq!(reviewed_files_at_head(Some(state.clone()), Some("ABC")), 1);
        assert_eq!(reviewed_files_at_head(Some(state.clone()), Some("def")), 0);
        assert_eq!(reviewed_files_at_head(Some(state), None), 0);
    }
}

impl Shell {
    #[cfg(test)]
    pub(super) fn set_tracker_project_for_test(&mut self, account: &str, project: &str, issues: Vec<mast_proto::linear::LinearIssueSummary>) {
        self.tracker.linear_projects.insert((account.into(), project.into(), 0), issues);
        self.tracker.key = None;
    }

    #[cfg(test)]
    pub(super) fn tracker_has_item_for_test(&self, id: &str) -> bool {
        self.tracker.not_started.iter().chain(&self.tracker.completed_items).any(|item| item.id == id)
    }

    fn tracker_linear_project(&self, chat_id: &str, cx: &App) -> Option<(String, String, String, u64)> {
        let state = self.state.read(cx);
        let space = state.chats.iter().find(|chat| chat.id == chat_id)?.space_id.as_deref()?;
        let outline = state.work_outline(space);
        let snapshot = outline.snapshot()?;
        if !crate::linear::space_enabled(Some(snapshot)) { return None; }
        let item = crate::chat_work_context::assigned_item(snapshot, chat_id)?;
        let project = snapshot.linear_project_reference(item)?;
        if project.external_id.is_empty() { return None; }
        let account = crate::linear::store::account_scope(snapshot).to_owned();
        let revision = state.linear.revision(&account);
        Some((space.to_owned(), account, project.external_id.clone(), revision))
    }

    pub(super) fn load_tracker_project(&mut self, chat_id: &str, force: bool, cx: &mut Context<Self>) {
        let Some((space, account, project, revision)) = self.tracker_linear_project(chat_id, cx) else { return; };
        let key = (account.clone(), project.clone(), revision);
        if !self.tracker.linear_loading.insert(key.clone()) { return; }
        self.tracker.key = None;
        let actions = self.state.read(cx).work_outline_actions();
        let stored = (!self.tracker.linear_projects.contains_key(&key)).then(|| actions.linear_project_issue_list(
            &space, &account, &project, true, crate::linear::client::ListRead::Stored));
        let read = if force { crate::linear::client::ListRead::Force } else { crate::linear::client::ListRead::Cached };
        let task = actions.linear_project_issue_list(&space, &account, &project, true, read);
        cx.spawn(async move |shell, cx| {
            if let Some(stored) = stored && let Ok(issues) = stored.await {
                shell.update(cx, |shell, cx| {
                    shell.tracker.linear_projects.entry(key.clone()).or_insert(issues);
                    shell.tracker.key = None;
                    cx.notify();
                }).ok();
            }
            let result = task.await;
            shell.update(cx, |shell, cx| {
                shell.tracker.linear_loading.remove(&key);
                match result {
                    Ok(issues) => {
                        if shell.tracker.linear_projects.len() >= 32 && !shell.tracker.linear_projects.contains_key(&key)
                            && let Some(evicted) = shell.tracker.linear_projects.keys().next().cloned() { shell.tracker.linear_projects.remove(&evicted); }
                        shell.tracker.linear_projects.insert(key.clone(), issues);
                        shell.tracker.linear_errors.remove(&key);
                        shell.tracker.key = None;
                    }
                    Err(error) => {
                        if shell.tracker.linear_errors.len() >= 32 && !shell.tracker.linear_errors.contains_key(&key)
                            && let Some(evicted) = shell.tracker.linear_errors.keys().next().cloned() { shell.tracker.linear_errors.remove(&evicted); }
                        shell.tracker.linear_errors.insert(key, error);
                        shell.tracker.key = None;
                    }
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    pub(super) fn invalidate_tracker_review(&mut self) {
        self.tracker.key = None;
    }

    fn open_tracker_review(&mut self, chat_id: String, branch: Option<String>, pr_url: String, head_sha: Option<String>, cx: &mut Context<Self>) {
        self.open_request_diff(chat_id.clone(), branch.clone(), cx);
        let Some(pr) = crate::chat_work_context::parse_pr_url(&pr_url) else { return; };
        let Some(artifact) = self.review_artifact(cx) else { return; };
        let state = self.state.read(cx);
        let target_device_id = (state.local_device_id.as_deref() != Some(artifact.device_id.as_str()))
            .then(|| artifact.device_id.clone());
        let call = state.artifact_actions().content(mast_proto::ArtifactContentParams {
            artifact_id: artifact.id.clone(), revision: None, companion: None, target_device_id,
        });
        let expected_pr = format!("pr:{}", pr.external_id().to_lowercase());
        let expected_branch = branch.map(|branch| format!("branch:{}/{}#{branch}", pr.owner.to_lowercase(), pr.repo.to_lowercase()));
        cx.spawn(async move |shell, cx| {
            let Ok(reply) = call.await else { return; };
            if reply.encoding != mast_proto::ArtifactContentEncoding::Utf8 { return; }
            let Ok(document) = serde_json::from_str::<serde_json::Value>(&reply.content) else { return; };
            let target = document.get("target").cloned().or_else(|| {
                let mut pr = document.get("pr")?.clone();
                pr.as_object_mut()?.insert("kind".into(), "pr".into());
                Some(pr)
            });
            let Some(target) = target.and_then(|value| serde_json::from_value::<mast_proto::ReviewTarget>(value).ok()) else { return; };
            let target_head = match &target { mast_proto::ReviewTarget::PullRequest { head_sha, .. } | mast_proto::ReviewTarget::Branch { head_sha, .. } => head_sha };
            if !head_sha.as_deref().is_some_and(|head| head.eq_ignore_ascii_case(target_head)) { return; }
            let key = target.state_key();
            if key != expected_pr && expected_branch.as_deref() != Some(key.as_str()) { return; }
            let _ = shell.update(cx, |shell, cx| {
                if shell.active_chat == chat_id && matches!(shell.resolved_right_active(cx), RightSurface::Diff(_)) && shell.review_artifact(cx).as_ref().is_some_and(|current| current.id == artifact.id && current.current_revision == artifact.current_revision) {
                    shell.set_right_active(RightSurface::Review, cx);
                    cx.notify();
                }
            });
        }).detach();
    }

    pub(crate) fn board_links_revision(&self) -> u64 { self.board_links_revision }

    pub(crate) fn board_link_versions(&self, ids: &[String]) -> Vec<u64> {
        ids.iter().map(|id| self.board_link_versions.get(id).copied().unwrap_or(0)).collect()
    }

    pub(crate) fn board_link_statuses(&self, ids: &[String]) -> std::collections::HashMap<String, mast_engine::pr_ticket_cache::ChatLinkStatus> {
        ids.iter().filter_map(|id| Some((id.clone(), self.chat_header_links.get(id)?.status.clone()?))).collect()
    }

    fn board_links_due(&self, ids: &[String], cx: &App) -> Vec<String> {
        ids.iter().filter(|id| {
            let checkout = self.state.read(cx).selected_checkout_key(id);
            !self.chat_header_links_pending.contains(*id) && !self.chat_header_links.get(*id).is_some_and(|entry| entry.checkout == checkout && entry.fetched_at.elapsed() < CHAT_HEADER_LINK_REFRESH)
        }).take(8usize.saturating_sub(self.chat_header_links_pending.len())).cloned().collect()
    }

    pub(crate) fn request_board_links(&mut self, ids: &[String], cx: &mut Context<Self>) {
        let ids = self.board_links_due(ids, cx);
        self.board_links_generation = self.board_links_generation.wrapping_add(1);
        let generation = self.board_links_generation;
        if ids.is_empty() { return; }
        cx.spawn(async move |shell, cx| {
            for id in ids {
                if !shell.update(cx, |shell, cx| { if shell.board_links_generation != generation { return false; } shell.ensure_chat_header_links(id, cx); true }).unwrap_or(false) { break; }
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
            }
        }).detach();
    }

    pub(crate) fn cancel_board_links(&mut self) { self.board_links_generation = self.board_links_generation.wrapping_add(1); }

    #[cfg(feature = "snapshot")]
    pub(crate) fn fixture_snapshot_team(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, _| {
            let titles = ["Validate the release", "Implement tracker rows", "Update the cache", "Review accessibility",
                "Add filtering", "Verify light themes", "Inspect status facts", "Test the expanded view",
                "Update documentation", "Check keyboard navigation", "Measure rendering", "Prepare handoff",
                "Check exports", "Improve search", "Verify sorting", "Update labels", "Review tests", "Check errors", "Document setup"];
            for (index, title) in titles.into_iter().enumerate() {
                let id = format!("fixture-worker-{index}");
                state.chats.push(serde_json::from_value(serde_json::json!({
                    "id": id, "deviceId": "local", "spaceId": "fixture-space",
                    "parentChatId": "fixture-chat", "title": title,
                    "branch": format!("tracker/task-{index}"), "actualModel": "example-model",
                    "lastMessagePreview": "Reviewing [project#20](https://github.com/example/project/pull/20) changes",
                    "archived": false, "createdAt": (chrono::Utc::now() - chrono::Duration::hours(if index > 14 { 100 } else { 2 })).to_rfc3339()
                })).unwrap());
                if index < 5 {
                    state.begin_pending_send(&id, &format!("fixture-message-{index}"), chrono::Utc::now() + chrono::Duration::hours(1));
                }
                if index == 5 || index == 6 {
                    let now = chrono::Utc::now();
                    state.sessions.push(mast_proto::Session {
                        chat_id: id, device_id: "local".into(),
                        status: if index == 5 { mast_proto::SessionStatus::AwaitingInput } else { mast_proto::SessionStatus::Errored },
                        started_at: Some(now), updated_at: now, last_completed_turn: None,
                        awaiting_input: None, last_turn_tool_failures: None, todo_progress: Some(mast_proto::TodoProgress { done: 3, total: 7 }),
                    });
                }
            }
            let mut snapshot: mast_proto::WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({
                "spaceId": "fixture-space", "available": true, "revision": 1,
                "items": [
                    {"id":"fixture-root", "spaceId":"fixture-space", "title":"Tracker", "chatIds":["fixture-chat"], "childIds":["fixture-todo", "fixture-done"]},
                    {"id":"fixture-todo", "spaceId":"fixture-space", "title":"Add documentation", "parentWorkItemId":"fixture-root"},
                    {"id":"fixture-done", "spaceId":"fixture-space", "title":"Complete design", "parentWorkItemId":"fixture-root", "effectiveStatus":"completed"}
                ]
            })).unwrap();
            snapshot.space_settings.linear_enabled = true;
            snapshot.external_references.push(serde_json::from_value(serde_json::json!({
                "id":"fixture-project-ref", "spaceId":"fixture-space", "workItemId":"fixture-root", "provider":"linear",
                "referenceType":"project", "externalId":"fixture-project", "cachedTitle":"Harbor workspace"
            })).unwrap());
            snapshot.items[0].chat_ids.push("fixture-worker-18".into());
            for (index, title) in titles.iter().enumerate().take(18) {
                if index == 15 || index == 17 { continue; }
                let id = format!("fixture-item-{index}");
                let chat_ids = if index == 16 { vec![format!("fixture-worker-{index}"), "fixture-worker-17".into()] } else { vec![format!("fixture-worker-{index}")] };
                snapshot.items[0].child_ids.push(id.clone());
                snapshot.items.push(serde_json::from_value(serde_json::json!({
                    "id": id, "spaceId": "fixture-space", "title": title, "parentWorkItemId": "fixture-root",
                    "chatIds": chat_ids, "effectiveStatus": if index == 3 || index == 13 { "completed" } else { "active" }
                })).unwrap());
                if index < 8 {
                    snapshot.external_references.push(serde_json::from_value(serde_json::json!({
                        "id": format!("fixture-issue-ref-{index}"), "spaceId":"fixture-space", "workItemId": format!("fixture-item-{index}"),
                        "provider":"linear", "referenceType":"issue", "externalId": format!("EX-{}", index + 100)
                    })).unwrap());
                }
            }
            snapshot.items[0].child_ids.push("fixture-container".into());
            snapshot.items.push(serde_json::from_value(serde_json::json!({
                "id": "fixture-container", "spaceId": "fixture-space", "title": "Planning", "parentWorkItemId": "fixture-root",
                "chatIds": ["fixture-worker-15"], "childIds": ["fixture-container-child"]
            })).unwrap());
            snapshot.items.push(serde_json::from_value(serde_json::json!({
                "id": "fixture-container-child", "spaceId": "fixture-space", "title": "Plan migration", "parentWorkItemId": "fixture-container"
            })).unwrap());
            for number in [20, 21, 44] {
                snapshot.external_references.push(serde_json::from_value(serde_json::json!({
                    "id": format!("fixture-pr-{number}"), "spaceId": "fixture-space", "workItemId": "fixture-item-0",
                    "provider": "github", "referenceType": "pullRequest", "externalId": format!("example/project#{number}"),
                    "url": format!("https://github.com/example/project/pull/{number}")
                })).unwrap());
            }
            snapshot.user_action_requests.push(mast_proto::UserActionRequestView {
                id: "fixture-handoff".into(), space_id: "fixture-space".into(), work_item_id: "fixture-root".into(),
                chat_id: "fixture-worker-7".into(), text: "Ready to merge: tracker/task-7".into(),
                kind: Some(mast_proto::work_outline_user_actions::UserActionKind::Handoff), priority: None,
                origin_kind: Default::default(), message_id: None, tool_call_id: None, info: None, options: Vec::new(),
                links: Vec::new(), facts: Vec::new(), dedupe_key: None, created_by: None, created_at: 1,
                state: mast_proto::UserActionRequestState::Open, resolution: None, deduped: false, origin_chat_available: true,
            });
            snapshot.user_action_requests.push(mast_proto::UserActionRequestView {
                id: "fixture-question".into(), space_id: "fixture-space".into(), work_item_id: "fixture-root".into(),
                chat_id: "fixture-worker-5".into(), text: "Use the compact layout first?".into(),
                kind: Some(mast_proto::work_outline_user_actions::UserActionKind::Ask), priority: None,
                origin_kind: Default::default(), message_id: None, tool_call_id: None, info: None, options: Vec::new(),
                links: Vec::new(), facts: Vec::new(), dedupe_key: None, created_by: None, created_at: 1,
                state: mast_proto::UserActionRequestState::Open, resolution: None, deduped: false, origin_chat_available: true,
            });
            state.set_work_outline_for_test(snapshot);
        });
        let titles = ["Validate the release", "Implement tracker rows", "Update the cache", "Review accessibility",
            "Add filtering", "Verify light themes", "Inspect status facts", "Test the expanded view"];
        let issues = titles.into_iter().enumerate().map(|(index, title)| mast_proto::linear::LinearIssueSummary {
            id: format!("fixture-issue-{index}"), identifier: format!("EX-{}", index + 100), title: title.into(),
            url: format!("https://linear.app/example/issue/EX-{}", index + 100),
            state: mast_proto::linear::LinearState { state_type: if index == 3 { mast_proto::linear::LinearStateType::Completed } else { mast_proto::linear::LinearStateType::Started }, ..Default::default() },
            ..Default::default()
        }).collect();
        self.tracker.linear_projects.insert(("default".into(), "fixture-project".into(), 0), issues);
        for index in 0..8 {
            use mast_engine::pr_ticket_cache::{ChatLinkStatus, ChecksStatus, DiffStat, PrStatus, TicketStatus};
            let titles = ["Validate the release", "Implement tracker rows", "Update the cache", "Review accessibility",
                "Add filtering", "Verify light themes", "Inspect status facts", "Test the expanded view"];
            let url = format!("https://github.com/example/project/pull/{}", index + 20);
            let pr = (index < 4 || index == 7).then(|| PrStatus {
                additions: Some(312), deletions: Some(48),
                number: index + 20, url: Some(url), state: if index == 3 { "MERGED" } else { "OPEN" }.into(),
                is_draft: false, review_decision: (index == 0).then(|| "APPROVED".into()), reviewers: Vec::new(),
                checks: Some(if index == 1 { ChecksStatus::Failing } else { ChecksStatus::Passing }),
                title: None, branch: Some(format!("tracker/task-{index}")), head_sha: Some("fixture-head".into()), mergeable: String::new(),
                has_reviewer_requested: false, merged_at: None,
            });
            let status = ChatLinkStatus {
                pr, ticket: Some(TicketStatus { identifier: format!("EX-{}", index + 100), title: Some(titles[index as usize].into()), url: None, status: None }),
                diff_stat: Some(DiffStat { files_changed: 1, lines_added: 4, lines_removed: 2 }),
                ..Default::default()
            };
            self.chat_header_links.insert(format!("fixture-worker-{index}"), ChatHeaderLinkCache {
                checkout: None, status: Some(status), fetched_at: Instant::now(),
            });
        }
        self.active_chat = "fixture-chat".into();
        let key = self.panel_key(cx);
        self.panels.update(&key, |panel| {
            panel.changes_open = true;
            panel.right_active = RightSurface::Team;
        });
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn fixture_snapshot_tracker_expanded(&mut self, cx: &mut Context<Self>) {
        self.fixture_snapshot_team(cx);
        self.right_pane_expanded = true;
    }
    /// The selected chat's workers. Link statuses (PR checks, diff stats) come
    /// from the header-link cache; missing ones are requested and the tab
    /// repaints when they land.
    pub(super) fn team_facts(&mut self, chat_id: &str, cx: &mut Context<Self>) -> TeamFacts {
        let state = self.state.read(cx);
        let snapshot = state.chats.iter().find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.space_id.as_deref())
            .and_then(|id| state.work_outline(id).snapshot().cloned());
        let account_revision = snapshot.as_deref().map(|snapshot| state.linear.revision(crate::linear::store::account_scope(snapshot))).unwrap_or_default();
        let key = (chat_id.to_owned(), snapshot.as_ref().map(|s| s.revision),
            state.board_status_revision, self.board_links_revision, account_revision);
        if self.tracker.key.as_ref() == Some(&key) {
            return self.tracker.facts.clone();
        }
        let mut facts = {
            let state = self.state.read(cx);
            let Some(chat) = state.chats.iter().find(|chat| chat.id == chat_id) else {
                return TeamFacts::default();
            };
            let now = chrono::Utc::now();
            let indicators = state.board_indicators(now);
            let snapshot = chat
                .space_id
                .as_deref()
                .and_then(|id| state.work_outline(id).snapshot().cloned());
            let links = self
                .chat_header_links
                .iter()
                .filter_map(|(id, entry)| Some((id.clone(), entry.status.clone()?)))
                .collect();
            team(&TeamInput {
                chat_id,
                chats: &state.chats,
                indicators: &indicators,
                snapshot: snapshot.as_deref(),
                links: &links,
                now,
            })
        };
        let all_workers = facts.clone();
        let (mut other_chats, mut additional_chats) = tracker_select_rows(&mut facts, snapshot.as_deref(), chat_id);
        let state = self.state.read(cx);
        let mut metadata = std::collections::HashMap::new();
        for chat in &state.chats {
            let harness = chat.config.as_ref().map(|config| crate::managed_accounts::harness_name(config.harness).to_owned()).unwrap_or_default();
            let model = chat.actual_model.clone().or_else(|| chat.config.as_ref().and_then(|config| config.model.clone()));
            metadata.insert(chat.id.clone(), (chat.title.clone().unwrap_or_else(|| "New session".into()), harness, model));
        }
        let mut requests = std::collections::HashMap::new();
        let todos = state.sessions.iter().filter_map(|session| session.todo_progress.map(|progress| (session.chat_id.clone(), progress))).collect();
        let stalled = state.sessions.iter().filter_map(|session| {
            if session.status == mast_proto::SessionStatus::AwaitingInput {
                Some((session.chat_id.clone(), ("waiting for input".to_owned(), false)))
            } else {
                session.last_turn_tool_failures.as_ref().map(|failure| (session.chat_id.clone(), (format!("{} tool calls failed", failure.count), true)))
            }
        }).collect();
        if let Some(snapshot) = &snapshot {
            for request in &snapshot.user_action_requests {
                if request.is_open() && !request.deduped
                    && request.effective_kind() != mast_proto::work_outline_user_actions::UserActionKind::Notice
                    && request.priority != Some(mast_proto::work_outline_user_actions::UserActionPriority::Fyi) {
                    requests.entry(request.chat_id.clone()).or_insert_with(|| crate::team::activity_line(&request.text).unwrap_or_default());
                }
            }
        }
        for session in &state.sessions {
            if session.status == mast_proto::SessionStatus::AwaitingInput
                && let Some(question) = session.awaiting_input.as_ref().and_then(|input| input.questions.first())
                && !question.question.trim().is_empty()
            {
                requests.entry(session.chat_id.clone()).or_insert_with(|| crate::team::activity_line(&question.question).unwrap_or_default());
            }
        }
        let items = snapshot.as_ref().and_then(|snapshot| {
            let root = crate::chat_work_context::assigned_item(snapshot, chat_id)?;
            let live_chats: std::collections::HashSet<_> = state.chats.iter().filter(|chat| !chat.archived).map(|chat| chat.id.as_str()).collect();
            let without_chats: std::collections::HashSet<_> = snapshot.items.iter()
                .filter(|item| !item.chat_ids.iter().any(|id| live_chats.contains(id.as_str())))
                .map(|item| item.id.as_str()).collect();
            let leaves: std::collections::HashSet<_> = snapshot.items.iter().filter(|item| item.child_ids.is_empty()).map(|item| item.id.as_str()).collect();
            let indicators = state.board_indicators(chrono::Utc::now());
            let links = self.chat_header_links.iter().filter_map(|(id, entry)| Some((id.clone(), entry.status.clone()?))).collect();
            let input = TeamInput { chat_id, chats: &state.chats, indicators: &indicators, snapshot: Some(snapshot), links: &links, now: chrono::Utc::now() };
            Some(crate::board::board(snapshot, Some(root), &input).into_iter()
                .filter(|item| item.id != root && leaves.contains(item.id.as_str()))
                .map(|item| { let unassigned = without_chats.contains(item.id.as_str()); (item, unassigned) })
                .collect::<Vec<_>>())
        }).unwrap_or_default();
        let project = snapshot.as_deref().filter(|snapshot| crate::linear::space_enabled(Some(snapshot))).and_then(|snapshot| {
            let root = crate::chat_work_context::assigned_item(snapshot, chat_id)?;
            let project = snapshot.linear_project_reference(root)?;
            Some(((crate::linear::store::account_scope(snapshot).to_owned(), project.external_id.clone(), account_revision), project.cached_title.clone()))
        });
        let mut empty: Vec<_> = items.iter().filter(|(_, unassigned)| *unassigned).map(|(item, _)| item.clone()).collect();
        let mut completion = std::collections::HashMap::new();
        if let Some(snapshot) = snapshot.as_deref()
            && let Some(root) = crate::chat_work_context::assigned_item(snapshot, chat_id)
            && let Some((key, _)) = &project
            && let Some(issues) = self.tracker.linear_projects.get(key)
        {
            (facts, other_chats, additional_chats, empty, completion) = tracker_reconcile_project(snapshot, root, &items, &all_workers, issues);
        }
        let not_started: Vec<_> = empty.iter().filter(|item| item.status != crate::board::BoardStatus::Done).cloned().collect();
        let completed_items: Vec<_> = empty.into_iter().filter(|item| item.status == crate::board::BoardStatus::Done).collect();
        let now = chrono::Utc::now();
        let lifecycle_rows: std::collections::HashMap<_, _> = facts.workers.iter().map(|worker| {
            let mut workers = vec![worker];
            workers.extend(additional_chats.get(&worker.chat_id).into_iter().flatten());
            let (completed, linear_completed) = completion.get(&worker.chat_id).copied().unwrap_or_else(||
                (worker.work_item_title.as_ref().map(|_| worker.state == crate::team::WorkerState::Done && !worker.archived), None));
            (worker.chat_id.clone(), crate::team::ticket_lifecycle(&workers, completed, linear_completed, false, now))
        }).collect();
        let mut grouped_rows = std::collections::HashMap::<_, Vec<usize>>::new();
        let mut stale_rows = std::collections::HashMap::<_, Vec<usize>>::new();
        for (index, worker) in facts.workers.iter().enumerate().filter(|(_, worker)| !worker.archived) {
            let row = &lifecycle_rows[&worker.chat_id];
            for filter in [TrackerFilter::All, TrackerFilter::NeedsYou].into_iter().chain(LifecycleStage::ALL.map(TrackerFilter::Stage)) {
                if tracker_matches(row, filter) {
                    if row.stale { stale_rows.entry(filter).or_default().push(index); }
                    else { grouped_rows.entry((filter, row.stage)).or_default().push(index); }
                }
            }
        }
        let mut counts = [0; 6];
        let mut needs_you = 0;
        for worker in facts.live() {
            let row = &lifecycle_rows[&worker.chat_id];
            counts[row.stage.index()] += 1;
            needs_you += usize::from(row.needs_you);
        }
        counts[LifecycleStage::NotStarted.index()] += not_started.len();
        counts[LifecycleStage::Done.index()] += completed_items.len();
        let ids: Vec<_> = facts.workers.iter().map(|worker| worker.chat_id.clone()).collect();
        let review_targets: std::collections::HashMap<_, _> = facts.live().filter_map(|worker| {
            if let Some(pr) = worker.prs.iter().find(|pr| pr.state.eq_ignore_ascii_case("open")
                && pr.branch.as_deref().zip(worker.branch.as_deref()).is_some_and(|(pr_branch, branch)| pr_branch == branch)) {
                let target = crate::chat_work_context::parse_pr_url(&pr.url)?;
                return Some((worker.chat_id.clone(), (format!("pr:{}", target.external_id().to_lowercase()), pr.head_sha.clone())));
            }
            if !worker.prs.is_empty() { return None; }
            let member = state.selected_execution_member(&worker.chat_id)?;
            if !member.branch.as_deref().zip(worker.branch.as_deref()).is_some_and(|(member_branch, worker_branch)| member_branch == worker_branch) { return None; }
            let target = self.branch_review_targets.get(&worker.chat_id).filter(|target| {
                matches!(target, mast_proto::ReviewTarget::Branch { checkout_path: Some(path), branch: Some(branch), .. }
                    if *path == member.checkout_path && Some(branch.as_str()) == member.branch.as_deref())
            }).map(mast_proto::ReviewTarget::state_key).or_else(|| {
                let chat = state.chats.iter().find(|chat| chat.id == worker.chat_id)?;
                Some(format!("diff:{}:{}", chat.device_id, member.checkout_path))
            })?;
            Some((worker.chat_id.clone(), (target, member.head_sha)))
        }).collect();
        let visible_targets: std::collections::HashSet<_> = review_targets.values().cloned().collect();
        self.tracker.review_targets = review_targets;
        // Review marks may have changed on another surface or device since the
        // previous board revision. Re-read them once for this revision.
        self.tracker.review_progress.clear();
        let review_targets: Vec<_> = visible_targets.into_iter().collect();
        self.tracker.review_generation = self.tracker.review_generation.wrapping_add(1);
        let generation = self.tracker.review_generation;
        self.tracker.review_task = None;
        self.tracker.key = Some(key);
        self.tracker.facts = facts.clone();
        self.tracker.all_workers = all_workers;
        self.tracker.not_started = not_started;
        self.tracker.completed_items = completed_items;
        self.tracker.other_chats = other_chats;
        self.tracker.additional_chats = additional_chats;
        self.tracker.lifecycle_rows = lifecycle_rows;
        self.tracker.linear_project_linked = project.is_some();
        self.tracker.source_label = project.as_ref().map(|(key, title)| {
            let name = title.as_deref().map_or_else(|| "Linear project".to_owned(), |title| format!("Linear project · {title}"));
            self.tracker.linear_projects.get(key).map_or(name.clone(), |issues| {
                let local = facts.live().filter(|worker| worker.ticket.is_none()).count()
                    + self.tracker.not_started.iter().chain(&self.tracker.completed_items).filter(|item| item.ticket.is_none()).count();
                format!("{name} · {} tickets, plus {local} local", issues.len())
            })
        }).unwrap_or_else(|| "Local tickets".into());
        self.tracker.source_notice = project.as_ref().and_then(|(key, _)| self.tracker.linear_errors.get(key).map(|error| error.message.clone())
            .or_else(|| self.tracker.linear_loading.contains(key).then(|| "Loading Linear tickets…".into())));
        self.tracker.grouped_rows = grouped_rows;
        self.tracker.stale_rows = stale_rows;
        self.tracker.counts = counts;
        self.tracker.needs_you = needs_you;
        self.tracker.metadata = metadata;
        self.tracker.requests = requests;
        self.tracker.stalled = stalled;
        self.tracker.todos = todos;
        #[cfg(test)] { self.tracker.rebuilds += 1; }
        let shell = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = shell.update(cx, |shell, cx| {
                for id in shell.board_links_due(&ids, cx) { shell.ensure_chat_header_links(id, cx); }
                if shell.tracker.review_generation != generation { return; }
                shell.tracker.review_task = Some(cx.spawn(async move |shell, cx| {
                    for (target, head) in review_targets {
                        let tasks = shell.update(cx, |shell, cx| {
                            if shell.tracker.review_generation != generation { return None; }
                            Some(shell.state.update(cx, |state, cx| {
                                let review = state.chat_diff_rpc(mast_rpc::methods::GET_DIFF_REVIEW_STATE, serde_json::json!({"target": target, "readOnly": true}), cx);
                                let drafts = state.chat_diff_rpc(mast_rpc::methods::GET_REVIEW_DRAFTS, serde_json::json!({"target": target}), cx);
                                (review, drafts)
                            }))
                        }).ok().flatten();
                        let Some((review, drafts)) = tasks else { return; };
                        let review = review.await.ok().and_then(|value| serde_json::from_value::<mast_proto::DiffReviewStateReply>(value).ok());
                        let drafts = drafts.await.ok().and_then(|value| serde_json::from_value::<Vec<mast_proto::ReviewDraft>>(value).ok());
                        if let (Some(review), Some(drafts)) = (review, drafts) {
                            let viewed = reviewed_files_at_head(review.state, head.as_deref());
                            let _ = shell.update(cx, |shell, cx| {
                                if shell.tracker.review_generation == generation {
                                    if viewed > 0 || !drafts.is_empty() {
                                        shell.tracker.review_progress.insert((target, head), (viewed, drafts.len()));
                                        cx.notify();
                                    }
                                }
                            });
                        }
                    }
                }));
            });
        });
        facts
    }

    /// ⌥⌘T / `t`: open the Team tab; nothing happens without workers.
    pub(crate) fn open_team_tab(&mut self, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        let chat_id = self.active_chat.clone();
        let facts = self.team_facts(&chat_id, cx);
        if !self.settings.team_tab_visibility.visible(!facts.is_empty() || !self.tracker.other_chats.is_empty() || !self.tracker.not_started.is_empty() || !self.tracker.completed_items.is_empty() || self.tracker.linear_project_linked) {
            return;
        }
        self.leave_overview_for_chat(cx);
        self.set_surfaces_open(true, cx);
        self.set_right_active(RightSurface::Team, cx);
        cx.notify();
    }

    /// The focus target of the Item and Team tabs' bodies.
    pub(super) fn team_panel_focus(&mut self, cx: &mut Context<Self>) -> FocusHandle {
        self.team_panel_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// The sticky chip after "Item": present only while the chat has workers.
    pub(super) fn render_team_tab_chip(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat_id = self.active_chat.clone();
        let team = self.team_facts(&chat_id, cx);
        if !self.settings.team_tab_visibility.visible(!team.is_empty() || !self.tracker.other_chats.is_empty() || !self.tracker.not_started.is_empty() || !self.tracker.completed_items.is_empty() || self.tracker.linear_project_linked) {
            return None;
        }
        let theme = Theme::of(cx).clone();
        let active = self.resolved_right_active(cx) == RightSurface::Team;
        let waiting = team.waiting() > 0;
        Some(
            crate::components::shell_parts::panel_tab(&theme, "right-surface-team-tab", "Tracker", format!("Tracker {}", self.tracker.counts.iter().sum::<usize>()), active)
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.set_right_active(RightSurface::Team, cx);
                }))
                .when(waiting, |el| {
                    el.child(
                        div()
                            .debug_selector(|| "right-surface-team-dot".into())
                            .child(crate::status_style::indicator_for_theme(crate::status_style::Status::Waiting, &theme)),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_team_summary(team: &TeamFacts, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let parts = team.roll_up_parts();
        if parts.is_empty() { return div().child("No active workers").into_any_element(); }
        let mut summary = crate::components::linear_parts::team_summary(theme);
        for (index, (status, label)) in parts.into_iter().enumerate() {
            if index > 0 { summary = summary.child("·"); }
            let indicator = crate::status_style::animated_indicator(status, theme.appearance, format!("team-summary-{index}"), cx.entity_id(), cx);
            summary = summary.child(indicator).child(SharedString::from(label));
        }
        summary.into_any_element()
    }

    /// The Team tab's body: roll-up line, then one row per worker.
    pub(super) fn render_team_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let chat_id = self.active_chat.clone();
        let team = self.team_facts(&chat_id, cx);
        let all_workers = self.tracker.all_workers.clone();
        let focus = self.team_panel_focus(cx);
        let mut col = crate::components::linear_parts::team_column(&theme, false);
        let mut source = div().flex().items_center().gap(px(Theme::SPACE_SM))
            .child(crate::components::shell_parts::tracker_metadata(&theme, self.tracker.source_label.clone()));
        if self.tracker.linear_project_linked {
            source = source.child(team_button(&theme, "tracker-refresh", "Refresh".into(), false)
                .on_click(cx.listener(|this, _, _, cx| {
                    let chat_id = this.active_chat.clone();
                    this.load_tracker_project(&chat_id, true, cx);
                })));
        }
        col = col.child(source);
        if let Some(notice) = &self.tracker.source_notice {
            col = col.child(crate::components::shell_parts::tracker_metadata(&theme, notice.clone()));
        }
        if self
            .state
            .read(cx)
            .chats
            .iter()
            .any(|chat| chat.id == chat_id && chat.archived)
        {
            col = col.child(
                crate::components::shell_parts::team_notice(&theme, "team-coordinator-archived", "Coordinator archived — hand off or unarchive"),
            );
        }
        if team.is_empty() && self.tracker.other_chats.is_empty() && self.tracker.not_started.is_empty() && self.tracker.completed_items.is_empty() {
            col = col.child(
                crate::components::shell_parts::team_notice(&theme, "team-empty", "No tickets yet."),
            );
        } else {
            let reports = self.wakes.statuses.get(&chat_id).map(crate::orchestrator_wakes::latest_reports).unwrap_or_default();
            let unseen = self.wakes.statuses.get(&chat_id).map(crate::orchestrator_wakes::unseen_by_worker).unwrap_or_default();
            self.ensure_wake_status(&chat_id, cx);
            let colors = [theme.border_strong, theme.accent, theme.accent, theme.warning, theme.success, theme.text_faint];
            let mut bar = div().debug_selector(|| "tracker-stage-bar".into()).flex().w_full().h(px(5.0)).gap(px(2.0));
            for (count, color) in self.tracker.counts.into_iter().zip(colors) {
                if count > 0 {
                    bar = bar.child(div().h_full().flex_grow(count as f32).flex_basis(px(0.0)).rounded_full().bg(color));
                }
            }
            col = col.child(bar);
            let mut rollup = div().debug_selector(|| "team-rollup".into()).flex().flex_wrap().items_center().gap(px(Theme::SPACE_SM));
            let filters = LifecycleStage::ALL.into_iter().map(|stage| (TrackerFilter::Stage(stage), stage.label(), self.tracker.counts[stage.index()]))
                .chain(std::iter::once((TrackerFilter::NeedsYou, "need you", self.tracker.needs_you)));
            for (filter, label, count) in filters {
                let selected = self.tracker.filter == filter;
                rollup = rollup.child(Self::chip(&theme, format!("tracker-filter-{filter:?}"), format!("{count} {label}"), if selected { ChipTone::Link } else { ChipTone::Dim })
                    .cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                        this.tracker.filter = if this.tracker.filter == filter { TrackerFilter::All } else { filter };
                        cx.notify();
                    })));
            }
            let mut settings = div().relative().child(Self::chip(&theme, "tracker-wake-settings".into(), "Wake settings".into(), ChipTone::Dim)
                .cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
                    this.tracker.wake_settings_open = !this.tracker.wake_settings_open;
                    cx.notify();
                })));
            if self.tracker.wake_settings_open {
                let menu = crate::popover::popover_card(&theme).w(px(300.0)).p(px(Theme::SPACE_SM))
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| { this.tracker.wake_settings_open = false; cx.notify(); }))
                    .child(self.render_team_wake_header(&chat_id, &theme, cx));
                settings = settings.child(crate::popover::anchored_menu_below_end("tracker-wake-menu", menu.into_any_element(), None));
            }
            rollup = rollup.child(settings);
            col = col.child(rollup);
            col = col.child(self.render_team_archive_block(&chat_id, &all_workers, &theme, cx));
            col = col.children(self.render_team_archived(&chat_id, &all_workers, &theme, cx));
            for group in [LifecycleStage::ReadyToMerge, LifecycleStage::PrReview, LifecycleStage::CodeReview,
                LifecycleStage::Building, LifecycleStage::NotStarted, LifecycleStage::Done] {
                let label = group.label();
                let matching = self.tracker.grouped_rows.get(&(self.tracker.filter, group)).map(Vec::as_slice).unwrap_or_default();
                let show_not_started = group == LifecycleStage::NotStarted && matches!(self.tracker.filter, TrackerFilter::All | TrackerFilter::Stage(LifecycleStage::NotStarted)) && !self.tracker.not_started.is_empty();
                let show_completed = group == LifecycleStage::Done && matches!(self.tracker.filter, TrackerFilter::All | TrackerFilter::Stage(LifecycleStage::Done))
                    && !self.tracker.completed_items.is_empty();
                if matching.is_empty() && !show_not_started && !show_completed { continue; }
                let group_count = matching.len() + if show_not_started { self.tracker.not_started.len() } else { 0 }
                    + if show_completed { self.tracker.completed_items.len() } else { 0 };
                col = col.child(crate::components::shell_parts::tracker_group_heading(&theme, label, group_count));
                for &index in matching { let worker = &team.workers[index]; col = col.child(self.render_tracker_worker(worker, reports.get(&worker.chat_id), unseen.get(&worker.chat_id), &theme, cx)); }
                if show_not_started {
                    for item in &self.tracker.not_started {
                        col = col.child(crate::components::shell_parts::tracker_row(&theme, format!("tracker-item-{}", item.id))
                            .child(div().flex().items_center().gap(px(Theme::SPACE_SM))
                                .child(div().size(px(7.0)).rounded_full().bg(theme.border))
                                .children(self.tracker.linear_project_linked.then(|| item.ticket.clone()).flatten().map(|ticket| crate::components::shell_parts::tracker_mono(&theme, ticket, theme.text_faint)))
                                .children((self.tracker.linear_project_linked && item.ticket.is_none()).then(|| crate::components::shell_parts::tracker_metadata(&theme, "local".into())))
                                .child(crate::components::shell_parts::worker_title(&theme, item.title.clone())))
                            .child(tracker_stage_track(&item.id, [true, false, false, false, false, false], &theme)));
                    }
                }
                if show_completed {
                    for item in &self.tracker.completed_items {
                        col = col.child(crate::components::shell_parts::tracker_row(&theme, format!("tracker-item-{}", item.id))
                            .child(div().flex().items_center().gap(px(Theme::SPACE_SM))
                                .child(div().size(px(7.0)).rounded_full().bg(theme.success))
                                .children(self.tracker.linear_project_linked.then(|| item.ticket.clone()).flatten().map(|ticket| crate::components::shell_parts::tracker_mono(&theme, ticket, theme.text_faint)))
                                .children((self.tracker.linear_project_linked && item.ticket.is_none()).then(|| crate::components::shell_parts::tracker_metadata(&theme, "local".into())))
                                .child(crate::components::shell_parts::worker_title(&theme, item.title.clone())))
                            .child(tracker_stage_track(&item.id, [true; 6], &theme)));
                    }
                }
            }
            let stale = self.tracker.stale_rows.get(&self.tracker.filter).map(Vec::as_slice).unwrap_or_default();
            if !stale.is_empty() {
                col = col.child(div().id("tracker-stale-toggle").cursor_pointer().child(format!("{} Stale ({})", if self.tracker.stale_expanded { "▾" } else { "▸" }, stale.len()))
                    .on_click(cx.listener(|this, _, _, cx| { this.tracker.stale_expanded = !this.tracker.stale_expanded; cx.notify(); })));
                if self.tracker.stale_expanded { for &index in stale { let worker = &team.workers[index]; col = col.child(self.render_tracker_worker(worker, reports.get(&worker.chat_id), unseen.get(&worker.chat_id), &theme, cx)); } }
            }
            if !self.tracker.other_chats.is_empty() && self.tracker.filter == TrackerFilter::All {
                col = col.child(div().id("tracker-other-toggle").cursor_pointer().child(format!("{} Other chats ({})", if self.tracker.other_expanded { "▾" } else { "▸" }, self.tracker.other_chats.len()))
                    .on_click(cx.listener(|this, _, _, cx| { this.tracker.other_expanded = !this.tracker.other_expanded; cx.notify(); })));
                if self.tracker.other_expanded { for worker in &self.tracker.other_chats { col = col.child(self.render_tracker_worker(worker, reports.get(&worker.chat_id), unseen.get(&worker.chat_id), &theme, cx)); } }
            }
        }
        self.panel_root("team-tab", focus, col, cx)
    }

    fn render_tracker_worker(&self, worker: &WorkerFacts, report: Option<&String>, unseen: Option<&Vec<String>>, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        use crate::team::TrackerGroup;
        let id = worker.chat_id.clone();
        let open_id = id.clone();
        let menu_id = id.clone();
        let (_, harness, model) = self.tracker.metadata.get(&id).cloned().unwrap_or_default();
        let identity = model.filter(|model| !model.is_empty()).unwrap_or(harness);
        let group = worker.tracker_group();
        let stage = self.tracker.lifecycle_rows.get(&id).map(|row| row.stage).unwrap_or(LifecycleStage::Building);
        let open_pr = worker.prs.iter().find(|pr| pr.state.eq_ignore_ascii_case("open") && !pr.url.is_empty()
            && pr.branch.as_deref().zip(worker.branch.as_deref()).is_some_and(|(pr_branch, branch)| pr_branch == branch));
        let status = if stage == LifecycleStage::ReadyToMerge { crate::status_style::Status::Ready } else { worker.state.into() };
        let status_color = crate::status_style::status_style(status, theme.appearance).color;
        let stalled = self.tracker.stalled.get(&id).cloned();
        let title = tracker_headline(worker.ticket.as_ref().filter(|_| self.tracker.linear_project_linked), worker.work_item_title.as_deref().unwrap_or("Unassigned work"));
        let mut header = div().flex().items_center().gap(px(Theme::SPACE_SM))
            .debug_selector({ let id = id.clone(); move || format!("tracker-heading-{id}") })
            .child(div().flex_none().size(px(7.0)).rounded_full().bg(status_color));
        if let Some(ticket) = worker.ticket.as_ref().filter(|_| self.tracker.linear_project_linked) {
            let url = ticket.url.clone();
            header = header.child(crate::components::shell_parts::tracker_mono(theme, ticket.identifier.clone(), theme.text_faint)
                .id(SharedString::from(format!("team-ticket-{id}"))).cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if let Some(url) = &url { this.open_web_reference(url, window.modifiers().secondary(), window, cx); }
                })));
        } else if self.tracker.linear_project_linked {
            header = header.child(crate::components::shell_parts::tracker_metadata(theme, "local".into()));
        }
        header = header.child(crate::components::shell_parts::worker_title(theme, title));
        if let Some(chats) = self.tracker.additional_chats.get(&id) {
            let toggle_id = id.clone();
            header = header.child(div().id(SharedString::from(format!("tracker-chats-{id}"))).cursor_pointer().child(format!("{} chats", chats.len() + 1))
                .on_click(cx.listener(move |this, _, _, cx| { cx.stop_propagation(); if !this.tracker.expanded_chats.remove(&toggle_id) { this.tracker.expanded_chats.insert(toggle_id.clone()); } cx.notify(); })));
        }
        if let Some(unseen) = unseen.filter(|unseen| !unseen.is_empty()) {
            let tip = SharedString::from(format!("Unseen by the orchestrator:\n{}", unseen.join("\n")));
            header = header.child(crate::components::shell_parts::worker_unseen_badge(theme, format!("team-worker-unseen-{id}"))
                .tooltip(move |_, cx| cx.new(|_| SurfaceTabTooltip { text: tip.clone() }).into()));
        }
        if self.tracker.lifecycle_rows.get(&id).is_some_and(|row| row.needs_you) {
            header = header.child(Self::chip(theme, format!("tracker-needs-you-{id}"), "Needs you".into(), ChipTone::Warn));
        }
        if let Some(row) = self.tracker.lifecycle_rows.get(&id) {
            if row.linear_completed_with_open_pr {
                header = header.child(crate::components::shell_parts::tracker_metadata(theme, "Linear says done".into()));
            }
            if row.merged_with_open_ticket {
                header = header.child(crate::components::shell_parts::tracker_metadata(theme, "Merged, ticket still open".into()));
            }
        }
        let track = tracker_stage_track(&id, std::array::from_fn(|index| index <= stage.index()), theme);
        let mut progress = div().flex().items_center().gap(px(Theme::SPACE_SM)).child(track)
            .debug_selector({ let id = id.clone(); move || format!("tracker-progress-{id}") });
        if let Some(todo) = self.tracker.todos.get(&id) {
            progress = progress.child(crate::components::shell_parts::tracker_checklist(theme, *todo, tracker_todo_label(*todo)));
        }
        let mut identity_line = div().flex().items_center().gap(px(Theme::SPACE_SM)).min_w_0()
            .debug_selector({ let id = id.clone(); move || format!("tracker-identity-{id}") });
        let has_identity = !identity.is_empty();
        if has_identity { identity_line = identity_line.child(crate::components::shell_parts::tracker_metadata(theme, identity)); }
        if let Some(branch) = &worker.branch {
            if has_identity { identity_line = identity_line.child(crate::components::shell_parts::worker_metadata(theme, "·".into(), theme.text_faint)); }
            identity_line = identity_line.child(div().id(SharedString::from(format!("team-branch-{id}")))
                .debug_selector({ let id = id.clone(); move || format!("team-branch-{id}") })
                .flex().items_center().gap(px(Theme::SPACE_XS)).min_w_0().text_color(theme.text_faint)
                .child(crate::icons::icon(crate::icons::GIT_BRANCH).size(px(11.0)).text_color(theme.text_faint))
                .child(crate::components::shell_parts::tracker_mono(theme, branch.clone(), theme.text_faint)));
        }
        let mut row = crate::components::shell_parts::tracker_row(theme, format!("team-worker-{id}"))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                let closeable = crate::chat_closeout::is_closeout_candidate(this.state.read(cx).chats.iter()
                    .find(|chat| chat.id == menu_id).and_then(|chat| chat.cwd.as_deref()));
                this.chat_menu.open(ChatMenuState { chat_id: menu_id.clone(), position: event.position, page: ChatMenuPage::Root, closeable });
                this.load_branch_review_target(menu_id.clone(), cx);
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.open_chat(open_id.clone(), cx)))
            .child(header).child(progress).child(identity_line);
        if let Some(pr) = tracker_primary_pr(&worker.prs).filter(|pr| pr.number > 0 || !pr.url.is_empty()) {
            let index = 0;
            let url = pr.url.clone();
            let mut line = crate::components::shell_parts::tracker_pr_line(theme, format!("team-pr-{id}-{index}"))
                .child(crate::components::shell_parts::tracker_mono(theme, tracker_pr_reference(pr), theme.text_muted));
            if !pr.state.is_empty() { line = line.child(SharedString::from(format!(" {}", pr.state.to_lowercase()))); }
            if worker.prs.len() > 1 { line = line.child(SharedString::from(format!(" +{} more", worker.prs.len() - 1))); }
            if let Some(checks) = pr.checks {
                let (label, color) = match checks {
                    mast_engine::pr_ticket_cache::ChecksStatus::Passing => ("checks passing", theme.success),
                    mast_engine::pr_ticket_cache::ChecksStatus::Failing => ("checks failing", theme.danger),
                    mast_engine::pr_ticket_cache::ChecksStatus::Pending => ("checks running", theme.text_muted),
                };
                line = line.child(" · ").child(crate::components::shell_parts::worker_metadata(theme, label.into(), color));
            }
            if let Some(review) = pr.review { line = line.child(SharedString::from(format!(" · {review}"))); }
            if let (Some(added), Some(removed)) = (pr.additions, pr.deletions) {
                line = line.child(" · ").child(crate::components::shell_parts::worker_metadata(theme, format!("+{added}"), theme.success))
                    .child(crate::components::shell_parts::worker_metadata(theme, format!("−{removed}"), theme.danger));
            }
            if index == 0 && let Some((viewed, drafts)) = self.tracker.review_targets.get(&id).and_then(|target| self.tracker.review_progress.get(target)) {
                line = line.child(Self::chip(theme, format!("tracker-review-progress-{id}"), format!("Review {viewed} viewed · {drafts} drafts"), ChipTone::Dim));
            }
            row = row.child(line.on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                if !url.is_empty() { this.open_web_reference(&url, window.modifiers().secondary(), window, cx); }
            })));
        }
        if worker.prs.is_empty() && let Some((viewed, drafts)) = self.tracker.review_targets.get(&id).and_then(|target| self.tracker.review_progress.get(target)) {
            row = row.child(Self::chip(theme, format!("tracker-review-progress-{id}"), format!("Review {viewed} viewed · {drafts} drafts"), ChipTone::Dim));
        }
        let age = worker.last_active.map(|at| mast_proto::view::format_time_ago(at, chrono::Utc::now())).unwrap_or_default();
        let request = self.tracker.requests.get(&id);
        let activity = request.map(|request| if group == TrackerGroup::NeedsYou { format!("Asked: {request}") } else { request.clone() })
            .or_else(|| worker.activity.clone())
            .or_else(|| stalled.as_ref().map(|(label, _)| label.clone())).unwrap_or_default();
        let activity = [activity, age.clone()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
        if let Some(report) = report.filter(|_| request.is_none()) {
            let expanded = self.wakes.expanded_reports.contains(&id);
            let toggle_id = id.clone();
            let summary = [crate::team::activity_line(report).unwrap_or_default(), age].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
            row = row.child(crate::components::shell_parts::worker_report_toggle(theme, format!("team-worker-report-toggle-{id}"), format!("team-worker-activity-{id}"), format!("{} Report: {summary}", if expanded { "▾" } else { "▸" }))
                .on_click(cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.toggle_report_expanded(&toggle_id, cx); })));
            if expanded { row = row.child(crate::components::shell_parts::worker_activity(theme, format!("team-worker-report-{id}"), report.clone(), true)); }
        } else {
            row = row.child(crate::components::shell_parts::worker_activity(theme, format!("team-worker-activity-{id}"), activity, false)
                .when(stalled.is_some(), |el| el.debug_selector({ let id = id.clone(); move || format!("team-worker-stalled-{id}") })));
        }
        let mut actions = div().flex().items_center().gap(px(Theme::SPACE_SM))
            .debug_selector({ let id = id.clone(); move || format!("tracker-actions-{id}") });
        if group == TrackerGroup::NeedsYou {
            let answer_id = id.clone();
            actions = actions.child(crate::components::shell_parts::tracker_action(theme, format!("tracker-answer-{id}"), "Answer", true)
                .debug_selector({ let id = id.clone(); move || format!("tracker-answer-{id}") })
                .on_click(cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.open_chat(answer_id.clone(), cx); })));
        } else if matches!(stage, LifecycleStage::PrReview | LifecycleStage::ReadyToMerge) && open_pr.is_some() {
            let pr = open_pr.unwrap();
            let review_id = id.clone();
            let review_branch = worker.branch.clone();
            let review_url = pr.url.clone();
            let review_head = pr.head_sha.clone();
            let merge_url = pr.url.clone();
            actions = actions.child(crate::components::shell_parts::tracker_action(theme, format!("tracker-review-{id}"), "Review PR", true)
                .debug_selector({ let id = id.clone(); move || format!("tracker-review-{id}") })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.tracker.key = None;
                    this.tracker.review_progress.clear();
                    this.tracker.review_generation = this.tracker.review_generation.wrapping_add(1);
                    this.tracker.review_task = None;
                    this.open_tracker_review(review_id.clone(), review_branch.clone(), review_url.clone(), review_head.clone(), cx);
                })));
            if stage == LifecycleStage::ReadyToMerge {
                actions = actions.child(crate::components::shell_parts::tracker_action(theme, format!("tracker-merge-{id}"), "Merge…", false)
                    .debug_selector({ let id = id.clone(); move || format!("tracker-merge-{id}") })
                    .on_click(cx.listener(move |this, _, window, cx| { cx.stop_propagation(); this.open_web_reference(&merge_url, window.modifiers().secondary(), window, cx); })));
            }
        } else {
            let changes_id = id.clone();
            let changes_branch = worker.branch.clone();
            actions = actions.child(crate::components::shell_parts::tracker_action(theme, format!("team-worker-changes-{id}"), "Changes", false)
                .debug_selector({ let id = id.clone(); move || format!("team-worker-changes-{id}") })
                .on_click(cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.open_request_diff(changes_id.clone(), changes_branch.clone(), cx); })));
        }
        row = row.child(actions);
        if let Some(verdict) = worker.archivable.as_ref().filter(|a| a.archivable) {
            row = row.child(crate::components::shell_parts::worker_archive_hint(theme, format!("team-archivable-{id}"), &verdict.reason));
        }
        if self.tracker.expanded_chats.contains(&id) && let Some(chats) = self.tracker.additional_chats.get(&id) {
            for chat in chats {
                let chat_id = chat.chat_id.clone();
                let title = self.tracker.metadata.get(&chat_id).map(|metadata| metadata.0.clone()).unwrap_or_else(|| chat.title.clone());
                row = row.child(div().id(SharedString::from(format!("tracker-additional-chat-{chat_id}"))).pl(px(16.0)).cursor_pointer().child(title)
                    .on_click(cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.open_chat(chat_id.clone(), cx); })));
            }
        }
        row.into_any_element()
    }

    /// The scrolling, focusable root shared by the Item and Team tabs.
    pub(super) fn panel_root(
        &mut self,
        id: &'static str,
        focus: FocusHandle,
        body: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tracked = focus.clone();
        let theme = Theme::of(cx).clone();
        crate::components::shell_parts::panel_root(&theme, id)
            .track_focus(&focus)
            .key_context("TeamPanel")
            .on_action(
                cx.listener(move |this, _: &OpenTeamTabFromPanel, window, cx| {
                    if tracked.is_focused(window)
                        && this.resolved_right_active(cx) != RightSurface::Team
                    {
                        this.open_team_tab(cx);
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .child(body)
            .into_any_element()
    }

    /// The Item tab's block: the roll-up line, the first workers, and a link
    /// to the Team tab.
    pub(super) fn render_item_tab_team(
        &mut self,
        team: &TeamFacts,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut block = crate::components::linear_parts::team_column(theme, true)
            .debug_selector(|| "chat-item-team".into())
            .child(
                crate::components::shell_parts::item_team_rollup(theme)
                    .child(Self::render_team_summary(team, theme, cx))
                    .on_click(cx.listener(|this, _, _, cx| this.open_team_tab(cx))),
            );
        for worker in team.live().take(ITEM_TAB_WORKERS) {
            block = block.child(self.render_worker_row(worker, true, theme, cx));
        }
        if team.live().count() > ITEM_TAB_WORKERS {
            block = block.child(
                crate::components::shell_parts::item_team_all(theme, team.live().count())
                    .on_click(cx.listener(|this, _, _, cx| this.open_team_tab(cx))),
            );
        }
        block.into_any_element()
    }

    fn chip(theme: &Theme, id: String, label: String, tone: ChipTone) -> gpui::Stateful<gpui::Div> {
        let (border, text) = chip_palette(theme, tone);
        crate::components::shell_parts::panel_chip(theme, id, label, border, text)
    }

    fn pr_tone(pr: &WorkerPr) -> ChipTone {
        use mast_engine::pr_ticket_cache::ChecksStatus;
        match (pr.checks, pr.review) {
            (Some(ChecksStatus::Failing), _) | (_, Some("changes requested")) => ChipTone::Warn,
            _ => ChipTone::Link,
        }
    }

    /// One worker. `compact` (the Item tab) is title, state and age plus the
    /// activity line; the Team tab adds chips, diff stats and the
    /// Ready-to-merge jump.
    fn render_worker_row(
        &mut self,
        worker: &WorkerFacts,
        compact: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let prefix = if compact {
            "chat-item-worker"
        } else {
            "team-worker"
        };
        let id = worker.chat_id.clone();
        let selector = format!("{prefix}-{id}");
        let now = chrono::Utc::now();
        let age = worker
            .last_active
            .map(|at| mast_proto::view::format_time_ago(at, now))
            .unwrap_or_default();
        let status = crate::status_style::status_style(worker.state.into(), theme.appearance);
        let indicator = crate::components::shell_parts::activity_indicator(worker.state.into(), format!("{prefix}-status-{id}"), theme, cx.entity_id(), cx);
        let open_id = id.clone();
        let changes_id = id.clone();
        let changes_branch = worker.branch.clone();
        let changes_selector = format!("{prefix}-changes-{id}");
        let menu_id = id.clone();
        let unseen = self.unseen_for_worker(&id);
        let stalled = self.state.read(cx).session_for(&id).and_then(|session| {
            if session.status == mast_proto::SessionStatus::AwaitingInput {
                Some(("waiting for input".to_owned(), crate::status_style::status_style(crate::status_style::Status::Waiting, theme.appearance).color))
            } else {
                session
                    .last_turn_tool_failures
                    .as_ref()
                    .map(|failure| (format!("{} tool calls failed", failure.count), theme.danger))
            }
        });
        let mut row = crate::components::shell_parts::worker_row(theme, selector)
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                let closeable = crate::chat_closeout::is_closeout_candidate(this.state.read(cx).chats.iter()
                    .find(|chat| chat.id == menu_id).and_then(|chat| chat.cwd.as_deref()));
                this.chat_menu.open(ChatMenuState { chat_id: menu_id.clone(), position: event.position, page: ChatMenuPage::Root, closeable });
                this.load_branch_review_target(menu_id.clone(), cx);
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.open_chat(open_id.clone(), cx)))
            .child(
                crate::components::linear_parts::team_worker_header(theme)
                    .child(div().flex_none().child(indicator))
                    .child(
                        crate::components::shell_parts::worker_title(theme, worker.title.clone()),
                    )
                    .children(stalled.map(|(label, color)| {
                        crate::components::shell_parts::worker_metadata(theme, label, color)
                            .debug_selector({
                                let selector = format!("{prefix}-stalled-{id}");
                                move || selector.clone()
                            })
                    }))
                    .children((!unseen.is_empty()).then(|| {
                        let tip = SharedString::from(format!(
                            "Unseen by the orchestrator:\n{}",
                            unseen.join("\n")
                        ));
                        crate::components::shell_parts::worker_unseen_badge(theme, format!("{prefix}-unseen-{id}"))
                            .tooltip(move |_, cx| {
                                cx.new(|_| SurfaceTabTooltip { text: tip.clone() }).into()
                            })
                    }))
                    .child(
                        crate::components::shell_parts::worker_metadata(theme, worker.state.label().to_owned(), status.color),
                    )
                    .child(
                        crate::components::shell_parts::worker_metadata(theme, age, theme.text_faint),
                    )
                    .child(
                        crate::components::shell_parts::worker_action_link(theme, crate::icons::FILE_CODE, "Changes")
                            .id(SharedString::from(changes_selector.clone()))
                            .debug_selector(move || changes_selector.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.open_request_diff(changes_id.clone(), changes_branch.clone(), cx);
                            })),
                    ),
            );
        if let Some(report) = self.latest_report_for_worker(&id) {
            // The worker's own report is its activity line; click for the full text.
            let expanded = self.wakes.expanded_reports.contains(&id);
            let line = crate::team::activity_line(&report).unwrap_or_default();
            let toggle_id = id.clone();
            row = row.child(
                crate::components::shell_parts::worker_report_toggle(theme, format!("{prefix}-report-toggle-{id}"), format!("{prefix}-activity-{id}"), format!(
                        "{} Report: {line}",
                        if expanded { "\u{25be}" } else { "\u{25b8}" }
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_report_expanded(&toggle_id, cx);
                    })),
            );
            if expanded {
                row = row.child(
                    crate::components::shell_parts::worker_activity(theme, format!("{prefix}-report-{id}"), report, true),
                );
            }
        } else if let Some(activity) = &worker.activity {
            row = row.child(
                crate::components::shell_parts::worker_activity(theme, format!("{prefix}-activity-{id}"), activity.clone(), false),
            );
        }
        if !compact {
            let mut chips = crate::components::linear_parts::team_worker_metadata(theme, true);
            let mut any = false;
            if let Some(branch) = &worker.branch {
                any = true;
                chips = chips.child(Self::chip(
                    theme,
                    format!("team-branch-{id}"),
                    format!("\u{2387} {branch}"),
                    ChipTone::Dim,
                ));
            }
            for (ix, pr) in worker.prs.iter().enumerate() {
                any = true;
                let url = pr.url.clone();
                chips = chips.child(
                    Self::chip(
                        theme,
                        format!("team-pr-{id}-{ix}"),
                        pr.label(),
                        Self::pr_tone(pr),
                    )
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if !url.is_empty() {
                            let external = window.modifiers().secondary();
                            this.open_web_reference(&url, external, window, cx)
                        }
                    })),
                );
            }
            if let Some(ticket) = &worker.ticket {
                any = true;
                let url = ticket.url.clone();
                chips = chips.child(
                    Self::chip(
                        theme,
                        format!("team-ticket-{id}"),
                        ticket.identifier.clone(),
                        ChipTone::Link,
                    )
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if let Some(url) = &url {
                            let external = window.modifiers().secondary();
                            this.open_web_reference(url, external, window, cx)
                        }
                    })),
                );
            }
            if let Some((added, removed)) = worker.diff {
                any = true;
                chips = chips.child(
                    crate::components::shell_parts::worker_diff(theme, format!("team-diff-{id}"), added, removed),
                );
            }
            if any {
                row = row.child(chips);
            }
        }
        if !compact && let Some(verdict) = worker.archivable.as_ref().filter(|a| a.archivable) {
            row = row.child(
                crate::components::shell_parts::worker_archive_hint(theme, format!("team-archivable-{id}"), &verdict.reason),
            );
        }
        if let Some(ready) = &worker.ready {
            let text = ready.text.clone();
            let mut references = {
                let state = self.state.read(cx);
                state
                    .work_outline(&ready.space_id)
                    .snapshot()
                    .and_then(|snapshot| snapshot.user_action_request(&ready.request_id))
                    .map(|request| {
                        request
                            .links
                            .iter()
                            .filter_map(|link| match &link.target {
                                mast_proto::work_outline_user_actions::RequestLinkTarget::Pr {
                                    url,
                                }
                                | mast_proto::work_outline_user_actions::RequestLinkTarget::Url {
                                    url,
                                } => crate::work_outline::required_actions::parse_handoff_pr(url),
                                _ => None,
                            })
                            .chain(
                                request
                                    .facts
                                    .iter()
                                    .filter(|fact| {
                                        fact.label.eq_ignore_ascii_case("pr")
                                            || fact.label.eq_ignore_ascii_case("pull request")
                                    })
                                    .filter_map(|fact| {
                                        crate::work_outline::required_actions::parse_handoff_pr(
                                            &fact.value,
                                        )
                                    }),
                            )
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            if worker.branch.is_some() {
                references.extend(worker.prs.iter().filter_map(|pr| {
                    crate::work_outline::required_actions::parse_handoff_pr(&pr.url)
                }));
            }
            for pr in references {
                crate::github::store::request_status(&self.state, &pr, false, cx);
            }
            let lifecycle = {
                let state = self.state.read(cx);
                state
                    .work_outline(&ready.space_id)
                    .snapshot()
                    .and_then(|snapshot| snapshot.user_action_request(&ready.request_id))
                    .and_then(|request| {
                        let explicit_pr = request.links.iter().any(|link| match &link.target {
                            mast_proto::work_outline_user_actions::RequestLinkTarget::Pr {
                                url,
                            }
                            | mast_proto::work_outline_user_actions::RequestLinkTarget::Url {
                                url,
                            } => crate::work_outline::required_actions::parse_handoff_pr(url)
                                .is_some(),
                            _ => false,
                        }) || request.facts.iter().any(|fact| {
                            (fact.label.eq_ignore_ascii_case("pr")
                                || fact.label.eq_ignore_ascii_case("pull request"))
                                && crate::work_outline::required_actions::parse_handoff_pr(
                                    &fact.value,
                                )
                                .is_some()
                        });
                        let worker_prs: std::collections::HashSet<_> = worker
                            .prs
                            .iter()
                            .filter_map(|pr| {
                                crate::work_outline::required_actions::parse_handoff_pr(&pr.url)
                            })
                            .map(|pr| pr.key())
                            .collect();
                        crate::work_outline::required_actions::handoff_lifecycle_parts(
                            request.effective_kind(),
                            &request.links,
                            &request.facts,
                            state.github.ready_statuses().filter(|status| {
                                explicit_pr || worker_prs.contains(&status.pr_ref.key())
                            }),
                        )
                    })
            };
            let ready_color = crate::status_style::status_style(crate::status_style::Status::Ready, theme.appearance).color;
            row = row.child(
                crate::components::linear_parts::team_worker_metadata(theme, false)
                    .child(
                        crate::components::shell_parts::worker_ready_label(text, ready_color),
                    )
                    .when_some(lifecycle, |el, lifecycle| {
                        el.child(Self::chip(
                            theme,
                            format!("team-handoff-lifecycle-{id}"),
                            lifecycle.to_owned(),
                            ChipTone::Dim,
                        ))
                    })
                    .child(
                        crate::components::shell_parts::worker_jump_button(theme, format!("{prefix}-jump-{id}"), ready_color)
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.open_item_tab(cx);
                            })),
                    ),
            );
        }
        row.into_any_element()
    }
}

/// Small text button used by the Team tab's archive controls.
impl Shell {
    /// "Archive finished (N)", the N >= 3 suggestion, and the confirmation.
    fn render_team_archive_block(
        &mut self,
        chat_id: &str,
        team: &TeamFacts,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let finished = team.finished();
        let n = finished.len();
        let block = crate::components::linear_parts::team_archive_block(theme);
        if n == 0 {
            self.wakes.archive_confirm.remove(chat_id);
            return block.into_any_element();
        }
        if self.wakes.archive_confirm.contains(chat_id) {
            let mut confirm = crate::components::shell_parts::archive_confirmation(theme, n);
            for worker in &finished {
                let selector = format!("team-archive-confirm-row-{}", worker.chat_id);
                let reason = worker
                    .archivable
                    .as_ref()
                    .map(|a| a.reason.clone())
                    .unwrap_or_default();
                confirm = confirm.child(
                    crate::components::shell_parts::archive_confirmation_worker(theme, selector, &worker.title, &reason),
                );
            }
            let go_chat = chat_id.to_owned();
            let cancel_chat = chat_id.to_owned();
            confirm = confirm
                .child(
                    crate::components::shell_parts::archive_confirmation_note(theme),
                )
                .child(
                    crate::components::linear_parts::team_archive_actions(theme, true)
                        .child(
                            team_button(
                                theme,
                                "team-archive-confirm-go",
                                format!("Archive {n}"),
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.archive_finished_workers(&go_chat, cx),
                            )),
                        )
                        .child(
                            team_button(
                                theme,
                                "team-archive-confirm-cancel",
                                "Cancel".into(),
                                false,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.wakes.archive_confirm.remove(&cancel_chat);
                                    cx.notify();
                                },
                            )),
                        ),
                );
            return block.child(confirm).into_any_element();
        }
        let ask_chat = chat_id.to_owned();
        let mut line = crate::components::linear_parts::team_archive_actions(theme, false);
        if n >= SUGGEST_ARCHIVE_AT {
            line = line.child(
                crate::components::shell_parts::archive_suggestion(theme, n),
            );
        } else {
            line = line.child(div().flex_1());
        }
        line = line.child(
            team_button(
                theme,
                "team-archive-finished",
                format!("Archive finished ({n})"),
                false,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.wakes.archive_confirm.insert(ask_chat.clone());
                cx.notify();
            })),
        );
        block.child(line).into_any_element()
    }

    /// "Show archived (N)" and, when on, the archived workers with Unarchive.
    fn render_team_archived(
        &mut self,
        chat_id: &str,
        team: &TeamFacts,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let count = team.archived().count();
        if count == 0 {
            return Vec::new();
        }
        let shown = self.wakes.show_archived.contains(chat_id);
        let toggle_chat = chat_id.to_owned();
        let mut out = vec![
            crate::components::shell_parts::archived_worker_toggle(theme, shown, count)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.wakes.show_archived.remove(&toggle_chat) {
                        this.wakes.show_archived.insert(toggle_chat.clone());
                    }
                    cx.notify();
                }))
                .into_any_element(),
        ];
        if shown {
            for worker in team.archived() {
                let id = worker.chat_id.clone();
                let selector = format!("team-archived-{id}");
                let restore_id = id.clone();
                let button_id = format!("team-unarchive-{id}");
                out.push(
                    crate::components::shell_parts::archived_worker_row(theme, selector)
                        .child(
                            crate::components::shell_parts::archived_worker_title(theme, worker.title.clone()),
                        )
                        .child(
                            team_button(theme, button_id, "Unarchive".into(), false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.set_worker_archived(&restore_id, false, cx)
                                }),
                            ),
                        )
                        .into_any_element(),
                );
            }
        }
        out
    }

    /// Confirmed "Archive finished": archive every worker the rule rates
    /// finished right now (re-evaluated, so a chat that started working since
    /// the dialog opened is skipped).
    fn archive_finished_workers(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        self.wakes.archive_confirm.remove(chat_id);
        self.team_facts(chat_id, cx);
        let ids: Vec<String> = self.tracker.all_workers
            .finished()
            .iter()
            .map(|w| w.chat_id.clone())
            .filter(|id| *id != self.active_chat)
            .collect();
        for id in ids {
            self.set_worker_archived(&id, true, cx);
        }
        cx.notify();
    }

    /// One `setChatArchived`; failures show in the sidebar notice.
    fn set_worker_archived(&mut self, worker_id: &str, archived: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let id = worker_id.to_owned();
        cx.spawn(async move |this, cx| {
            let failed = engine
                .client()
                .call(
                    mast_rpc::methods::MUTATE,
                    serde_json::json!({ "op": "setChatArchived", "chatId": id, "archived": archived }),
                )
                .await
                .is_err();
            if failed {
                let _ = this.update(cx, |this, cx| {
                    this.sidebar_notice = Some("Could not change that chat's archive state. Try again.".into());
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod board_link_tests {
    use super::*;

    #[gpui::test]
    fn board_link_requests_skip_fresh_entries(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            crate::history::init(Default::default(), Default::default(), Default::default(), Default::default(), cx);
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        });
        let state = cx.new(|_| AppState::new());
        let shell = cx.new(|cx| Shell::new(state, crate::EngineBootConfig { data_dir:dir.path().into(), ipc_port:0, edge_url:String::new(), edge_token:None, org_id:None, workos_client_id:None, default_harness:mast_proto::HarnessId::Mock }, cx));
        shell.update(cx, |shell, cx| {
            shell.chat_header_links.insert("fresh".into(), ChatHeaderLinkCache { checkout:None, status:None, fetched_at:Instant::now() });
            shell.chat_header_links_pending.insert("pending".into());
            assert_eq!(shell.board_links_due(&["fresh".into(), "pending".into(), "missing".into()], cx), vec!["missing"]);
            let many: Vec<_> = (0..100).map(|i| format!("worker-{i}")).collect();
            assert_eq!(shell.board_links_due(&many, cx).len(), 7);
        });
    }
}
