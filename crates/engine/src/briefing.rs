//! The read path for briefings: the latest revision of a results artifact,
//! with run times, for any chat. Nothing here writes.
//!
//! The lookup is shared. A caller supplies a [`BriefingKind`] (which artifacts
//! count), a [`BriefingScope`] (which of them this caller may see) and the
//! kind's provenance check (who may have written them). The scope runs on every
//! candidate before it is listed or its bytes are read, and a candidate it
//! refuses is indistinguishable from one that does not exist. Scheduled results
//! use [`SpaceScope`] and [`run_provenance`]. Manager sessions add a
//! `BriefingKind::ManagerStatus`, a project-subtree scope and their own
//! provenance; the rest of the path stays as it is.

use std::collections::HashMap;

use chrono::{Local, TimeZone};
use mast_proto::artifacts::{ArtifactError, ArtifactRevision, ArtifactView};
use mast_proto::{ScheduledRunResults, UserActionRequestView, WorkOutlineActorKind};
use serde_json::{Value, json};

use crate::WorkspaceHost;
use crate::artifacts::ArtifactsService;

/// Largest content returned for one briefing when it is the only one.
pub const SINGLE_CONTENT_LIMIT: usize = 128 * 1024;
/// Largest content per briefing when several are returned together.
pub const MANY_CONTENT_LIMIT: usize = 32 * 1024;
/// Entries named in the chat-start line before "+N more".
const INDEX_ENTRIES: usize = 6;
const RESULTS_TITLE_PREFIX: &str = "Results: ";

/// Which artifacts a lookup is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BriefingKind {
    /// A scheduled task's results artifact (`scheduled-results:` role).
    ScheduledResults,
}

impl BriefingKind {
    fn matches(self, role: Option<&str>) -> bool {
        match self {
            Self::ScheduledResults => role.is_some_and(ScheduledRunResults::is_results_role),
        }
    }

    /// Identifies one briefing of this kind across renames and items.
    fn identity(self, artifact: &ArtifactView) -> String {
        artifact
            .role
            .as_deref()
            .map(ScheduledRunResults::normalize_role)
            .unwrap_or_else(|| artifact.id.clone())
    }

    /// The newest revision written by whoever may write this kind, if any. An
    /// artifact with none is treated as absent, and a later edit by anyone
    /// else never hides the last genuine revision.
    fn newest_trusted(self, workspace: &WorkspaceHost, artifact: &ArtifactView) -> Option<u32> {
        match self {
            Self::ScheduledResults => artifact
                .revisions
                .iter()
                .find(|revision| run_provenance(workspace, artifact, revision))
                .map(|revision| revision.id),
        }
    }
}

/// A scheduled results revision is genuine only when a Background run of the
/// task its role names published it. The publication carries that identity
/// (`scheduled_task`), so it holds after the publishing chat is deleted. A
/// revision recorded before the stamp existed is verified through its chat
/// while that chat remains. A chat that merely used the role never qualifies.
pub fn run_provenance(
    workspace: &WorkspaceHost,
    artifact: &ArtifactView,
    revision: &ArtifactRevision,
) -> bool {
    if revision.by.kind != WorkOutlineActorKind::Agent {
        return false;
    }
    let Some(key) = artifact
        .role
        .as_deref()
        .map(ScheduledRunResults::normalize_role)
        .and_then(|role| ScheduledRunResults::key_of_role(&role))
    else {
        return false;
    };
    if let Some(stamp) = &revision.scheduled_task {
        return *stamp == key;
    }
    let Some(info) = revision
        .by
        .chat_id
        .as_deref()
        .and_then(|chat_id| workspace.chat(chat_id).ok().flatten())
        .and_then(|chat| chat.scheduled_task_info)
    else {
        return false;
    };
    info.output == mast_proto::ScheduledTaskOutput::Background
        && ScheduledRunResults::key_of_role(&ScheduledRunResults::role_for(&info.task_id))
            .as_deref()
            == Some(key.as_str())
}

/// A candidate offered to a [`BriefingScope`].
pub struct BriefingSource<'a> {
    pub kind: BriefingKind,
    pub artifact: &'a ArtifactView,
}

/// Decides what a caller may read. Checked before a candidate is listed and
/// before any of its content is read.
pub trait BriefingScope {
    fn permits(&self, source: &BriefingSource<'_>) -> bool;

    /// Whether Required Actions on this work item may be counted for the
    /// caller. Required, with no default: a scope narrower than a Space (a
    /// project subtree) must decide for itself, so it can never count another
    /// project's actions by accident.
    fn permits_work_item(&self, space_id: &str, work_item_id: &str) -> bool;
}

/// Everything in one Space, which is what scheduled results are shared across.
pub struct SpaceScope<'a>(pub &'a str);

impl BriefingScope for SpaceScope<'_> {
    fn permits(&self, source: &BriefingSource<'_>) -> bool {
        source.artifact.space_id == self.0
    }

    fn permits_work_item(&self, space_id: &str, _work_item_id: &str) -> bool {
        space_id == self.0
    }
}

/// A scheduled task on this device: its stable id, current name and Space.
/// Names apply only to briefings in the task's own Space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAlias {
    pub id: String,
    pub name: String,
    pub space_id: Option<String>,
}

pub struct BriefingQuery<'a> {
    pub kind: BriefingKind,
    /// A task name, task id or artifact id; `None` selects every briefing.
    pub task: Option<&'a str>,
    pub include_content: bool,
    /// The device's scheduled tasks. A briefing keeps the title it was first
    /// published under, so this is how the current name, and the task's
    /// identity, are resolved.
    pub tasks: &'a [TaskAlias],
}

impl<'a> BriefingQuery<'a> {
    pub fn new(kind: BriefingKind) -> Self {
        Self {
            kind,
            task: None,
            include_content: true,
            tasks: &[],
        }
    }

    pub fn task(mut self, task: Option<&'a str>) -> Self {
        self.task = task;
        self
    }

    pub fn index_only(mut self, index_only: bool) -> Self {
        self.include_content = !index_only;
        self
    }

    pub fn tasks(mut self, tasks: &'a [TaskAlias]) -> Self {
        self.tasks = tasks;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BriefingEntry {
    /// The task's current name when known, else the title without "Results: ".
    pub task: String,
    /// The stable task id, when this device knows the task.
    pub task_id: Option<String>,
    /// Identity shared with the task's Required Action keys (the hash in its role).
    pub task_key: Option<String>,
    pub artifact_id: String,
    pub title: String,
    pub revision: u32,
    /// Epoch milliseconds the newest revision was published.
    pub run_at: i64,
    pub run_id: Option<String>,
    pub work_item_id: Option<String>,
    pub content: Option<String>,
    pub content_truncated: bool,
    /// With content requested: whether it was read. Run times only: whether
    /// this device could serve it (the file lives here).
    pub content_available: bool,
}

impl BriefingEntry {
    pub fn to_json(&self) -> Value {
        json!({
            "task": self.task,
            "taskId": self.task_id,
            "artifactId": self.artifact_id,
            "title": self.title,
            "revision": self.revision,
            "runAt": self.run_at,
            "runAtLocal": local_stamp(self.run_at),
            "runId": self.run_id,
            "workItemId": self.work_item_id,
            "content": self.content,
            "contentTruncated": self.content_truncated,
            "contentAvailable": self.content_available,
        })
    }
}

fn task_of<'a>(tasks: &'a [TaskAlias], artifact: &ArtifactView) -> Option<&'a TaskAlias> {
    let role = artifact
        .role
        .as_deref()
        .map(ScheduledRunResults::normalize_role)?;
    tasks.iter().find(|task| {
        task.space_id.as_deref() == Some(artifact.space_id.as_str())
            && ScheduledRunResults::role_for(&task.id) == role
    })
}

fn task_name(artifact: &ArtifactView, tasks: &[TaskAlias]) -> String {
    task_of(tasks, artifact)
        .map(|task| task.name.clone())
        .unwrap_or_else(|| {
            artifact
                .title
                .strip_prefix(RESULTS_TITLE_PREFIX)
                .unwrap_or(&artifact.title)
                .trim()
                .to_owned()
        })
}

fn matches_task(artifact: &ArtifactView, wanted: &str, tasks: &[TaskAlias]) -> bool {
    let wanted = wanted.trim();
    artifact.id.eq_ignore_ascii_case(wanted)
        || artifact.role.as_deref().is_some_and(|role| {
            ScheduledRunResults::normalize_role(role) == ScheduledRunResults::role_for(wanted)
        })
        || task_name(artifact, tasks).eq_ignore_ascii_case(wanted)
        || artifact
            .title
            .strip_prefix(RESULTS_TITLE_PREFIX)
            .unwrap_or(&artifact.title)
            .trim()
            .eq_ignore_ascii_case(wanted)
        || artifact.title.eq_ignore_ascii_case(wanted)
}

/// The briefings `scope` permits and `trusted` accepts, one per identity (the
/// newest wins when a task's artifact exists on more than one item), newest
/// first. `trusted` names an artifact's newest genuine revision; each result
/// is pinned to it, so its revision, time and content are that revision's. A named task that matches nothing permitted is `not_found`, listing
/// what is available.
pub fn select(
    artifacts: Vec<ArtifactView>,
    scope: &dyn BriefingScope,
    query: &BriefingQuery<'_>,
    trusted: &dyn Fn(&ArtifactView) -> Option<u32>,
) -> Result<Vec<ArtifactView>, ArtifactError> {
    let kind = query.kind;
    let permitted: Vec<ArtifactView> = artifacts
        .into_iter()
        .filter(|a| a.archived_at.is_none() && a.current().is_some())
        .filter(|a| kind.matches(a.role.as_deref()))
        .filter(|a| scope.permits(&BriefingSource { kind, artifact: a }))
        .filter_map(|a| {
            let revision = trusted(&a)?;
            Some(pinned_to(a, revision))
        })
        .collect();
    let at = |a: &ArtifactView| a.current().map_or(0, |r| r.at);
    let mut newest: HashMap<String, ArtifactView> = HashMap::new();
    for artifact in permitted.iter().filter(|a| match query.task {
        Some(wanted) => matches_task(a, wanted, query.tasks),
        None => true,
    }) {
        let key = kind.identity(artifact);
        if newest.get(&key).is_none_or(|held| at(artifact) > at(held)) {
            newest.insert(key, artifact.clone());
        }
    }
    let mut found: Vec<ArtifactView> = newest.into_values().collect();
    found.sort_by(|a, b| at(b).cmp(&at(a)).then_with(|| a.id.cmp(&b.id)));
    if let Some(wanted) = query.task
        && found.is_empty()
    {
        let mut names: Vec<String> = permitted
            .iter()
            .map(|a| task_name(a, query.tasks))
            .collect();
        names.sort();
        names.dedup();
        return Err(ArtifactError::not_found(if names.is_empty() {
            format!("no briefing for “{wanted}”; no scheduled task has published results yet")
        } else {
            format!(
                "no briefing for “{wanted}”; available: {}",
                names.join(", ")
            )
        }));
    }
    Ok(found)
}

/// The artifact as of `revision`: later revisions are dropped, so `current()`
/// and `current_revision` are that revision.
fn pinned_to(mut artifact: ArtifactView, revision: u32) -> ArtifactView {
    artifact.revisions.retain(|r| r.id <= revision);
    artifact.current_revision = revision;
    artifact
}

/// The latest briefings a caller may read, with content unless the query asks
/// for run times only. Content is the newest genuine revision's recorded bytes, read
/// after the scope accepted the candidate, and only when the file is on this
/// device.
pub fn read(
    service: &ArtifactsService,
    scope: &dyn BriefingScope,
    query: &BriefingQuery<'_>,
    space_id: &str,
) -> Result<Vec<BriefingEntry>, ArtifactError> {
    let workspace = service.workspace();
    let kind = query.kind;
    // Revisions published before stamps existed are stamped while their
    // publishing chat can still prove them.
    service.backfill_run_stamps(Some(space_id));
    let selected = select(
        workspace.artifacts_of_space(space_id),
        scope,
        query,
        &|artifact| kind.newest_trusted(workspace, artifact),
    )?;
    let limit = if selected.len() == 1 {
        SINGLE_CONTENT_LIMIT
    } else {
        MANY_CONTENT_LIMIT
    };
    Ok(selected
        .into_iter()
        .map(|artifact| {
            let on_this_device = artifact.device_id == service.device_id();
            let text = (query.include_content && on_this_device)
                .then(|| service.revision_bytes(&artifact).ok())
                .flatten()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            let content_available = if query.include_content {
                text.is_some()
            } else {
                on_this_device
            };
            let run_id = text.as_deref().and_then(run_id_of);
            let (content, content_truncated) = match text {
                Some(text) => {
                    let (cut, truncated) = truncate(&text, limit);
                    (Some(cut), truncated)
                }
                None => (None, false),
            };
            let task = task_of(query.tasks, &artifact);
            BriefingEntry {
                task: task_name(&artifact, query.tasks),
                task_id: task.map(|task| task.id.clone()),
                task_key: artifact
                    .role
                    .as_deref()
                    .map(ScheduledRunResults::normalize_role)
                    .and_then(|role| ScheduledRunResults::key_of_role(&role)),
                artifact_id: artifact.id.clone(),
                title: artifact.title.clone(),
                revision: artifact.current_revision,
                run_at: artifact.current().map_or(0, |r| r.at),
                run_id,
                work_item_id: artifact.work_item_id.clone(),
                content,
                content_truncated,
                content_available,
            }
        })
        .collect())
}

/// The id on the contract's `Run: <id>` first line.
fn run_id_of(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .strip_prefix("Run:")
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
}

fn truncate(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_owned(), false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// Open scheduled-task Required Actions the scope lets the caller see.
fn open_scheduled<'a>(
    requests: &'a [UserActionRequestView],
    scope: &'a dyn BriefingScope,
    space_id: &'a str,
) -> impl Iterator<Item = (&'a UserActionRequestView, &'a str)> + 'a {
    requests
        .iter()
        .filter(move |r| r.is_unresolved() && r.space_id == space_id)
        .filter(move |r| scope.permits_work_item(space_id, &r.work_item_id))
        .filter_map(|r| r.dedupe_key.as_deref().map(|key| (r, key)))
}

/// Every open Required Action a scheduled task raised in the Space: what a
/// run asked for, and a run that failed or never published.
pub fn need_you_in_space(
    requests: &[UserActionRequestView],
    scope: &dyn BriefingScope,
    space_id: &str,
) -> usize {
    open_scheduled(requests, scope, space_id)
        .filter(|(_, key)| {
            key.starts_with(ScheduledRunResults::NEED_DEDUPE_PREFIX)
                || key.starts_with(ScheduledRunResults::FAILED_DEDUPE_PREFIX)
        })
        .count()
}

/// The open Required Actions of just these briefings' tasks, found by stable
/// task identity, so another task sharing the item is never counted. A failed
/// run's action is keyed by the task id, which hashes to the same identity the
/// results role carries, so it is found without the local schedule.
pub fn need_you_for(
    requests: &[UserActionRequestView],
    scope: &dyn BriefingScope,
    space_id: &str,
    entries: &[BriefingEntry],
) -> usize {
    let keys: Vec<&str> = entries
        .iter()
        .filter_map(|e| e.task_key.as_deref())
        .collect();
    let needs: Vec<String> = keys
        .iter()
        .map(|key| ScheduledRunResults::need_prefix_for_key(key))
        .collect();
    let failed_task_key = |key: &str| {
        let task_id = key.strip_prefix(ScheduledRunResults::FAILED_DEDUPE_PREFIX)?;
        ScheduledRunResults::key_of_role(&ScheduledRunResults::role_for(task_id))
    };
    open_scheduled(requests, scope, space_id)
        .filter(|(_, key)| {
            needs.iter().any(|prefix| key.starts_with(prefix.as_str()))
                || failed_task_key(key).is_some_and(|task| keys.contains(&task.as_str()))
        })
        .count()
}

fn local_stamp(ms: i64) -> Option<String> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
}

/// "05:31" today, "Tue 05:31" within the last week, "Nov 3" before that, in
/// the host's local time.
fn short_stamp(run_at: i64, now: i64) -> String {
    let (Some(at), Some(now)) = (
        Local.timestamp_millis_opt(run_at).single(),
        Local.timestamp_millis_opt(now).single(),
    ) else {
        return String::new();
    };
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        ..=0 => at.format("%H:%M").to_string(),
        1..=6 => at.format("%a %H:%M").to_string(),
        _ => at.format("%b %-d").to_string(),
    }
}

/// The chat-start line: "Slack 05:31 · GitHub 04:52 · 3 need you". `None`
/// when there is nothing to say.
pub fn index_line(entries: &[BriefingEntry], need_you: usize, now: i64) -> Option<String> {
    let mut parts: Vec<String> = entries
        .iter()
        .take(INDEX_ENTRIES)
        .map(|e| format!("{} {}", e.task, short_stamp(e.run_at, now)))
        .collect();
    if entries.len() > INDEX_ENTRIES {
        parts.push(format!("+{} more", entries.len() - INDEX_ENTRIES));
    }
    if need_you > 0 {
        parts.push(format!("{need_you} need you"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// What a new chat's agent is told when the index is on.
pub fn chat_start_notice(line: &str) -> String {
    format!(
        "Latest briefings: {line}. Read them with briefing_read; no need for read_chat or Notion.\n\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::artifacts::ArtifactRevision;

    fn artifact(id: &str, task_id: &str, title: &str, space: &str, at: i64) -> ArtifactView {
        ArtifactView {
            id: id.into(),
            space_id: space.into(),
            title: title.into(),
            role: Some(ScheduledRunResults::role_for(task_id)),
            work_item_id: Some(format!("item-{id}")),
            current_revision: 1,
            revisions: vec![ArtifactRevision {
                id: 1,
                at,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    struct Only(&'static str);
    impl BriefingScope for Only {
        fn permits(&self, source: &BriefingSource<'_>) -> bool {
            source.artifact.work_item_id.as_deref() == Some(self.0)
        }
        fn permits_work_item(&self, _space_id: &str, work_item_id: &str) -> bool {
            work_item_id == self.0
        }
    }

    fn alias(id: &str, name: &str, space: &str) -> TaskAlias {
        TaskAlias {
            id: id.into(),
            name: name.into(),
            space_id: Some(space.into()),
        }
    }

    fn query(task: Option<&str>) -> BriefingQuery<'_> {
        BriefingQuery::new(BriefingKind::ScheduledResults).task(task)
    }

    fn pick(
        artifacts: Vec<ArtifactView>,
        scope: &dyn BriefingScope,
        query: &BriefingQuery<'_>,
    ) -> Result<Vec<ArtifactView>, ArtifactError> {
        select(artifacts, scope, query, &|a| a.current().map(|r| r.id))
    }

    fn ids(found: &[ArtifactView]) -> Vec<&str> {
        found.iter().map(|a| a.id.as_str()).collect()
    }

    #[test]
    fn lists_newest_first_and_only_results() {
        let mut other = artifact("doc", "t3", "Plan", "s", 9);
        other.role = Some("plan".into());
        let found = pick(
            vec![
                artifact("a", "t1", "Results: Slack", "s", 100),
                artifact("b", "t2", "Results: GitHub", "s", 200),
                other,
            ],
            &SpaceScope("s"),
            &query(None),
        )
        .unwrap();
        assert_eq!(ids(&found), ["b", "a"]);
    }

    #[test]
    fn a_task_is_found_by_name_task_id_or_artifact_id() {
        let all = || {
            vec![
                artifact("a", "t1", "Results: Slack", "s", 100),
                artifact("b", "t2", "Results: GitHub", "s", 200),
            ]
        };
        for wanted in ["slack", " Slack ", "t1", "a", "Results: Slack"] {
            let found = pick(all(), &SpaceScope("s"), &query(Some(wanted))).unwrap();
            assert_eq!(ids(&found), ["a"], "{wanted}");
        }
    }

    #[test]
    fn a_renamed_task_is_found_and_shown_under_its_current_name() {
        let tasks = vec![alias("t1", "Slack digest", "s")];
        let all = || vec![artifact("a", "t1", "Results: Slack", "s", 100)];
        for wanted in ["Slack digest", "slack", "t1"] {
            let q = query(Some(wanted)).tasks(&tasks);
            assert_eq!(
                ids(&pick(all(), &SpaceScope("s"), &q).unwrap()),
                ["a"],
                "{wanted}"
            );
        }
        assert_eq!(task_name(&all()[0], &tasks), "Slack digest");
        assert_eq!(task_name(&all()[0], &[]), "Slack");
    }

    #[test]
    fn a_task_name_never_applies_across_spaces() {
        // The task moved to another Space and was renamed: its new name must not
        // surface through the artifact it left behind.
        let tasks = vec![alias("t1", "Moved and renamed", "elsewhere")];
        let artifact = artifact("a", "t1", "Results: Slack", "s", 100);
        assert_eq!(task_name(&artifact, &tasks), "Slack");
        let q = query(Some("Moved and renamed")).tasks(&tasks);
        assert!(pick(vec![artifact], &SpaceScope("s"), &q).is_err());
    }

    #[test]
    fn the_newest_genuine_revision_is_returned_even_after_a_later_edit() {
        let mut view = artifact("a", "t1", "Results: Slack", "s", 10);
        view.current_revision = 3;
        view.revisions = [(3, 30), (2, 20), (1, 10)]
            .map(|(id, at)| ArtifactRevision {
                id,
                at,
                ..Default::default()
            })
            .to_vec();
        let found = select(vec![view], &SpaceScope("s"), &query(None), &|_| Some(2)).unwrap();
        assert_eq!(found[0].current_revision, 2);
        assert_eq!(found[0].current().map(|r| r.at), Some(20));
    }

    #[test]
    fn an_unknown_task_lists_what_exists_and_other_spaces_stay_hidden() {
        let err = pick(
            vec![
                artifact("a", "t1", "Results: Slack", "s", 1),
                artifact("x", "t9", "Results: Secret", "other", 1),
            ],
            &SpaceScope("s"),
            &query(Some("nope")),
        )
        .unwrap_err();
        assert!(
            err.message.contains("Slack") && !err.message.contains("Secret"),
            "{}",
            err.message
        );
    }

    #[test]
    fn the_newest_copy_of_one_task_wins() {
        let found = pick(
            vec![
                artifact("old", "t1", "Results: Slack", "s", 1),
                artifact("new", "t1", "Results: Slack", "s", 2),
            ],
            &SpaceScope("s"),
            &query(None),
        )
        .unwrap();
        assert_eq!(ids(&found), ["new"]);
    }

    #[test]
    fn the_scope_hides_a_refused_briefing_as_if_absent() {
        let all = || {
            vec![
                artifact("a", "t1", "Results: Slack", "s", 1),
                artifact("b", "t2", "Results: GitHub", "s", 2),
            ]
        };
        let found = pick(all(), &Only("item-a"), &query(None)).unwrap();
        assert_eq!(ids(&found), ["a"]);
        let err = pick(all(), &Only("item-a"), &query(Some("GitHub"))).unwrap_err();
        assert!(!err.message.contains("GitHub, "), "{}", err.message);
        assert!(
            pick(all(), &Only("nobody"), &query(None))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn an_untrusted_writer_is_treated_as_absent_and_cannot_displace_the_real_one() {
        let real = artifact("real", "t1", "Results: Slack", "s", 1);
        let fake = artifact("fake", "t1", "Results: Slack", "s", 99);
        let found = select(vec![real, fake], &SpaceScope("s"), &query(None), &|a| {
            (a.id != "fake").then_some(1)
        })
        .unwrap();
        assert_eq!(ids(&found), ["real"]);
        let err = select(
            vec![artifact("fake", "t1", "Results: Slack", "s", 99)],
            &SpaceScope("s"),
            &query(Some("Slack")),
            &|_| None,
        )
        .unwrap_err();
        assert!(
            err.message.contains("no scheduled task has published"),
            "{}",
            err.message
        );
    }

    #[test]
    fn unpublished_and_archived_artifacts_are_skipped() {
        let mut unpublished = artifact("u", "t1", "Results: A", "s", 1);
        unpublished.revisions.clear();
        let mut archived = artifact("r", "t2", "Results: B", "s", 1);
        archived.archived_at = Some(5);
        let found = pick(vec![unpublished, archived], &SpaceScope("s"), &query(None)).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn run_id_comes_from_the_contract_line() {
        assert_eq!(
            run_id_of("\n Run: run-7 \n# Slack").as_deref(),
            Some("run-7")
        );
        assert_eq!(run_id_of("# Slack\nRun: run-7"), None);
        assert_eq!(run_id_of("Run:  "), None);
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        assert_eq!(truncate("héllo", 2), ("h".into(), true));
        assert_eq!(truncate("abc", 3), ("abc".into(), false));
    }

    fn entry(task: &str, run_at: i64) -> BriefingEntry {
        BriefingEntry {
            task: task.into(),
            task_id: None,
            task_key: None,
            artifact_id: task.into(),
            title: task.into(),
            revision: 1,
            run_at,
            run_id: None,
            work_item_id: None,
            content: None,
            content_truncated: false,
            content_available: true,
        }
    }

    #[test]
    fn the_index_line_names_runs_and_needs() {
        let now = Local
            .with_ymd_and_hms(2026, 11, 18, 9, 0, 0)
            .unwrap()
            .timestamp_millis();
        let at = |h, m| {
            Local
                .with_ymd_and_hms(2026, 11, 18, h, m, 0)
                .unwrap()
                .timestamp_millis()
        };
        let line = index_line(
            &[entry("Slack", at(5, 31)), entry("GitHub", at(4, 52))],
            3,
            now,
        );
        assert_eq!(
            line.as_deref(),
            Some("Slack 05:31 · GitHub 04:52 · 3 need you")
        );
        assert_eq!(index_line(&[], 0, now), None);
        assert_eq!(index_line(&[], 2, now).as_deref(), Some("2 need you"));
    }

    #[test]
    fn older_runs_carry_a_day_and_the_list_is_capped() {
        let now = Local
            .with_ymd_and_hms(2026, 11, 18, 9, 0, 0)
            .unwrap()
            .timestamp_millis();
        let yesterday = Local
            .with_ymd_and_hms(2026, 11, 17, 5, 31, 0)
            .unwrap()
            .timestamp_millis();
        let stale = Local
            .with_ymd_and_hms(2026, 10, 1, 5, 31, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(short_stamp(yesterday, now), "Tue 05:31");
        assert_eq!(short_stamp(stale, now), "Oct 1");
        let many: Vec<_> = (0..8).map(|i| entry(&format!("T{i}"), now)).collect();
        assert!(index_line(&many, 0, now).unwrap().ends_with("+2 more"));
    }

    fn requests() -> Vec<UserActionRequestView> {
        let slack = ScheduledRunResults::dedupe_prefix("t-slack");
        let gh = ScheduledRunResults::dedupe_prefix("t-gh");
        let request =
            |key: Option<String>, item: &str, state: &str, space: &str| -> UserActionRequestView {
                serde_json::from_value(json!({
                    "id": format!("{item}-{state}-{key:?}"), "workItemId": item,
                    "dedupeKey": key, "state": state, "spaceId": space,
                }))
                .unwrap()
            };
        vec![
            request(Some(format!("{slack}x")), "shared", "open", "s"),
            request(Some(format!("{slack}y")), "shared", "open", "s"),
            request(Some(format!("{gh}z")), "shared", "open", "s"),
            request(Some(format!("{slack}done")), "shared", "resolved", "s"),
            request(
                Some("scheduled-run-failed:t-slack".into()),
                "elsewhere",
                "open",
                "s",
            ),
            request(
                Some("scheduled-run-failed:t-never-published".into()),
                "elsewhere",
                "open",
                "s",
            ),
            request(
                Some(format!("{slack}other-space")),
                "shared",
                "open",
                "other",
            ),
            request(Some("manual".into()), "shared", "open", "s"),
            request(None, "shared", "open", "s"),
        ]
    }

    #[test]
    fn the_space_total_counts_failures_and_tasks_that_never_published() {
        assert_eq!(need_you_in_space(&requests(), &SpaceScope("s"), "s"), 5);
        // A scope narrower than the Space counts only its own items.
        assert_eq!(need_you_in_space(&requests(), &Only("nobody"), "s"), 0);
        assert_eq!(need_you_in_space(&requests(), &Only("shared"), "s"), 3);
    }

    #[test]
    fn a_named_task_counts_only_its_own_actions_by_identity() {
        let mut slack = entry("Slack", 1);
        slack.task_id = Some("t-slack".into());
        slack.task_key =
            ScheduledRunResults::key_of_role(&ScheduledRunResults::role_for("t-slack"));
        // Two of its own needs on a shared item, plus its failure elsewhere;
        // the other task's need and the resolved one are not counted.
        assert_eq!(
            need_you_for(&requests(), &SpaceScope("s"), "s", &[slack.clone()]),
            3
        );
        // Without the local schedule the failure is still found, by identity.
        let mut unknown = slack.clone();
        unknown.task_id = None;
        assert_eq!(
            need_you_for(&requests(), &SpaceScope("s"), "s", &[unknown]),
            3
        );
        let mut keyless = slack;
        keyless.task_key = None;
        assert_eq!(
            need_you_for(&requests(), &SpaceScope("s"), "s", &[keyless]),
            0
        );
    }
}
