//! The read path for briefings: the latest revision of a results artifact,
//! with run times, for any chat. Nothing here writes.
//!
//! The lookup is shared. A caller supplies a [`BriefingKind`] (which artifacts
//! count) and a [`BriefingScope`] (which of them this caller may see). The
//! scope runs on every candidate before it is listed or its bytes are read,
//! and a candidate it refuses is indistinguishable from one that does not
//! exist. Scheduled results use [`SpaceScope`]. Manager sessions add a
//! `BriefingKind::ManagerStatus` and a project-subtree scope; the rest of the
//! path stays as it is.

use std::collections::HashMap;

use chrono::{Local, TimeZone};
use mast_proto::artifacts::{ArtifactError, ArtifactView};
use mast_proto::{ScheduledRunResults, UserActionRequestView};
use serde_json::{Value, json};

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
}

/// Everything in one Space, which is what scheduled results are shared across.
pub struct SpaceScope<'a>(pub &'a str);

impl BriefingScope for SpaceScope<'_> {
    fn permits(&self, source: &BriefingSource<'_>) -> bool {
        source.artifact.space_id == self.0
    }
}

pub struct BriefingQuery<'a> {
    pub kind: BriefingKind,
    /// A task name, task id or artifact id; `None` selects every briefing.
    pub task: Option<&'a str>,
    pub include_content: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BriefingEntry {
    /// The name shown to people: the title without its "Results: " prefix.
    pub task: String,
    pub artifact_id: String,
    pub title: String,
    pub revision: u32,
    /// Epoch milliseconds the newest revision was published.
    pub run_at: i64,
    pub run_id: Option<String>,
    pub work_item_id: Option<String>,
    pub content: Option<String>,
    pub content_truncated: bool,
    /// False when the file lives on another device.
    pub content_available: bool,
}

impl BriefingEntry {
    pub fn to_json(&self) -> Value {
        json!({
            "task": self.task,
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

fn task_name(artifact: &ArtifactView) -> String {
    artifact
        .title
        .strip_prefix(RESULTS_TITLE_PREFIX)
        .unwrap_or(&artifact.title)
        .trim()
        .to_owned()
}

fn matches_task(artifact: &ArtifactView, wanted: &str) -> bool {
    let wanted = wanted.trim();
    artifact.id.eq_ignore_ascii_case(wanted)
        || artifact.role.as_deref().is_some_and(|role| {
            ScheduledRunResults::normalize_role(role) == ScheduledRunResults::role_for(wanted)
        })
        || task_name(artifact).eq_ignore_ascii_case(wanted)
        || artifact.title.eq_ignore_ascii_case(wanted)
}

/// The briefings `scope` permits, one per identity (the newest wins when a
/// task's artifact exists on more than one item), newest first. A named task
/// that matches nothing permitted is `not_found`, listing what is available.
pub fn select(
    artifacts: Vec<ArtifactView>,
    scope: &dyn BriefingScope,
    kind: BriefingKind,
    task: Option<&str>,
) -> Result<Vec<ArtifactView>, ArtifactError> {
    let permitted: Vec<ArtifactView> = artifacts
        .into_iter()
        .filter(|a| a.archived_at.is_none() && a.current().is_some())
        .filter(|a| kind.matches(a.role.as_deref()))
        .filter(|a| scope.permits(&BriefingSource { kind, artifact: a }))
        .collect();
    let mut newest: HashMap<String, ArtifactView> = HashMap::new();
    for artifact in permitted.iter().filter(|a| match task {
        Some(wanted) => matches_task(a, wanted),
        None => true,
    }) {
        let at = |a: &ArtifactView| a.current().map_or(0, |r| r.at);
        let key = kind.identity(artifact);
        if newest.get(&key).is_none_or(|held| at(artifact) > at(held)) {
            newest.insert(key, artifact.clone());
        }
    }
    let mut found: Vec<ArtifactView> = newest.into_values().collect();
    found.sort_by(|a, b| {
        let at = |a: &ArtifactView| a.current().map_or(0, |r| r.at);
        at(b).cmp(&at(a)).then_with(|| a.id.cmp(&b.id))
    });
    if let Some(wanted) = task
        && found.is_empty()
    {
        let mut names: Vec<String> = permitted.iter().map(task_name).collect();
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

/// The latest briefings a caller may read, with content unless the query asks
/// for run times only. Content is read after the scope has accepted the
/// candidate, and only when the file is on this device.
pub fn read(
    service: &ArtifactsService,
    scope: &dyn BriefingScope,
    query: &BriefingQuery<'_>,
    space_id: &str,
) -> Result<Vec<BriefingEntry>, ArtifactError> {
    let selected = select(
        service.workspace().artifacts_of_space(space_id),
        scope,
        query.kind,
        query.task,
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
                .then(|| service.current_bytes(&artifact).ok())
                .flatten()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            let content_available = !query.include_content || text.is_some();
            let run_id = text.as_deref().and_then(run_id_of);
            let (content, content_truncated) = match text {
                Some(text) => {
                    let (cut, truncated) = truncate(&text, limit);
                    (Some(cut), truncated)
                }
                None => (None, false),
            };
            let revision = artifact.current();
            BriefingEntry {
                task: task_name(&artifact),
                artifact_id: artifact.id.clone(),
                title: artifact.title.clone(),
                revision: artifact.current_revision,
                run_at: revision.map_or(0, |r| r.at),
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

/// Open Required Actions raised by scheduled tasks on `work_item_ids`.
pub fn need_you_count(requests: &[UserActionRequestView], work_item_ids: &[String]) -> usize {
    requests
        .iter()
        .filter(|r| r.is_unresolved())
        .filter(|r| {
            r.dedupe_key
                .as_deref()
                .is_some_and(|key| key.starts_with(ScheduledRunResults::NEED_DEDUPE_PREFIX))
        })
        .filter(|r| work_item_ids.iter().any(|id| *id == r.work_item_id))
        .count()
}

fn local_stamp(ms: i64) -> Option<String> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
}

/// "05:31" today, "Tue 05:31" within the last week, "Nov 3" before that.
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
    }

    fn ids(found: &[ArtifactView]) -> Vec<&str> {
        found.iter().map(|a| a.id.as_str()).collect()
    }

    #[test]
    fn lists_newest_first_and_only_results() {
        let mut other = artifact("doc", "t3", "Plan", "s", 9);
        other.role = Some("plan".into());
        let found = select(
            vec![
                artifact("a", "t1", "Results: Slack", "s", 100),
                artifact("b", "t2", "Results: GitHub", "s", 200),
                other,
            ],
            &SpaceScope("s"),
            BriefingKind::ScheduledResults,
            None,
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
            let found = select(
                all(),
                &SpaceScope("s"),
                BriefingKind::ScheduledResults,
                Some(wanted),
            )
            .unwrap();
            assert_eq!(ids(&found), ["a"], "{wanted}");
        }
    }

    #[test]
    fn an_unknown_task_lists_what_exists_and_other_spaces_stay_hidden() {
        let err = select(
            vec![
                artifact("a", "t1", "Results: Slack", "s", 1),
                artifact("x", "t9", "Results: Secret", "other", 1),
            ],
            &SpaceScope("s"),
            BriefingKind::ScheduledResults,
            Some("nope"),
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
        let found = select(
            vec![
                artifact("old", "t1", "Results: Slack", "s", 1),
                artifact("new", "t1", "Results: Slack", "s", 2),
            ],
            &SpaceScope("s"),
            BriefingKind::ScheduledResults,
            None,
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
        let found = select(all(), &Only("item-a"), BriefingKind::ScheduledResults, None).unwrap();
        assert_eq!(ids(&found), ["a"]);
        let err = select(
            all(),
            &Only("item-a"),
            BriefingKind::ScheduledResults,
            Some("GitHub"),
        )
        .unwrap_err();
        assert!(!err.message.contains("GitHub, "), "{}", err.message);
        assert!(
            select(all(), &Only("nobody"), BriefingKind::ScheduledResults, None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unpublished_and_archived_artifacts_are_skipped() {
        let mut unpublished = artifact("u", "t1", "Results: A", "s", 1);
        unpublished.revisions.clear();
        let mut archived = artifact("r", "t2", "Results: B", "s", 1);
        archived.archived_at = Some(5);
        let found = select(
            vec![unpublished, archived],
            &SpaceScope("s"),
            BriefingKind::ScheduledResults,
            None,
        )
        .unwrap();
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

    #[test]
    fn needs_count_only_open_scheduled_requests_on_permitted_items() {
        let request = |key: Option<&str>, item: &str, state: &str| -> UserActionRequestView {
            serde_json::from_value(json!({
                "id": format!("{item}-{state}-{key:?}"), "workItemId": item, "dedupeKey": key, "state": state,
            }))
            .unwrap()
        };
        let requests = vec![
            request(Some("scheduled-need:abc:x"), "i1", "open"),
            request(Some("scheduled-need:abc:y"), "i1", "open"),
            request(Some("scheduled-need:abc:z"), "i2", "open"),
            request(Some("scheduled-need:abc:done"), "i1", "resolved"),
            request(Some("manual"), "i1", "open"),
            request(None, "i1", "open"),
        ];
        assert_eq!(need_you_count(&requests, &["i1".into()]), 2);
        assert_eq!(need_you_count(&requests, &[]), 0);
    }
}
