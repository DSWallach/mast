use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use mast_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use mast_proto::{
    work_outline::WorkOutlineActorKind,
    work_outline_user_actions::{
        UserActionRequestView, UserActionResolution, is_relayed_or_system_message,
    },
};
use serde::{Deserialize, Serialize};

use crate::{elapsed_millis, is_question, is_status_question, percentile, snapshot::Snapshot};

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Distribution {
    pub count: usize,
    pub median: Option<i64>,
    pub p90: Option<i64>,
}

impl Distribution {
    fn from_values(values: &[i64]) -> Self {
        Self {
            count: values.len(),
            median: percentile(values, 50),
            p90: percentile(values, 90),
        }
    }
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Aggregate {
    pub requests_by_kind: BTreeMap<String, u64>,
    pub prose_questions_without_card: u64,
    pub card_to_prose_ratio: Option<f64>,
    pub resolution_millis: BTreeMap<String, Distribution>,
    pub status_questions_by_day: BTreeMap<String, u64>,
    pub reply_words: Distribution,
    pub auto_continue_messages: u64,
    pub worker_stopped_wakes: u64,
}

#[derive(Default)]
struct Accumulator {
    aggregate: Aggregate,
    durations: BTreeMap<String, Vec<i64>>,
    words: Vec<i64>,
}

impl Accumulator {
    fn finish(mut self) -> Aggregate {
        let count: u64 = self.aggregate.requests_by_kind.values().sum();
        self.aggregate.card_to_prose_ratio = (self.aggregate.prose_questions_without_card > 0)
            .then(|| count as f64 / self.aggregate.prose_questions_without_card as f64);
        self.aggregate.reply_words = Distribution::from_values(&self.words);
        self.aggregate.resolution_millis = self
            .durations
            .iter()
            .map(|(k, v)| (k.clone(), Distribution::from_values(v)))
            .collect();
        self.aggregate
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub since: String,
    pub until: String,
    pub totals: Aggregate,
    /// Keys use IDs, not names or message content. Missing project is `none`.
    pub groups: BTreeMap<String, Aggregate>,
    pub coverage: Vec<String>,
}

fn text(entry: &SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn resolution_category(r: &UserActionResolution, origin_chat: &str) -> &'static str {
    if r.outcome.as_deref() == Some("superseded") {
        return "superseded";
    }
    if r.answer.is_some() || r.via.as_deref() == Some("chat") {
        return "chat_reply";
    }
    match r.via.as_deref() {
        Some("card") | Some("banner") | Some("menu") => "card_reply_or_choice",
        Some("prMerged") => "pr_merged",
        Some("agent") if r.by.chat_id.as_deref() == Some(origin_chat) => "agent_self_resolve",
        Some("agent") => "agent_other",
        _ if r.by.kind == WorkOutlineActorKind::Agent
            && r.by.chat_id.as_deref() == Some(origin_chat) =>
        {
            "agent_self_resolve"
        }
        _ if r.by.kind == WorkOutlineActorKind::Agent => "agent_other",
        _ => "unknown",
    }
}

fn update(
    groups: &mut BTreeMap<String, Accumulator>,
    keys: &[String],
    f: impl Fn(&mut Accumulator),
) {
    f(groups.entry("all".into()).or_default());
    for key in keys {
        f(groups.entry(key.clone()).or_default());
    }
}

fn keys(space: &str, project: Option<&str>, role: &str) -> Vec<String> {
    vec![
        format!("space:{space}"),
        format!("space:{space}/project:{}", project.unwrap_or("none")),
        format!("role:{role}"),
        format!(
            "space:{space}/project:{}/role:{role}",
            project.unwrap_or("none")
        ),
    ]
}

pub fn measure(
    snapshot: &Snapshot,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> anyhow::Result<Report> {
    anyhow::ensure!(since < until, "since must be before until");
    let in_window = |at: i64| at >= since.timestamp_millis() && at < until.timestamp_millis();
    let parents: HashSet<_> = snapshot
        .chats
        .iter()
        .filter_map(|chat| chat.parent_chat_id.as_deref())
        .collect();
    let roles: HashMap<_, _> = snapshot
        .chats
        .iter()
        .map(|chat| {
            (
                chat.id.as_str(),
                if parents.contains(chat.id.as_str()) {
                    "coordinator"
                } else if chat.parent_chat_id.is_some() {
                    "worker"
                } else {
                    "plain"
                },
            )
        })
        .collect();
    let mut projects = HashMap::new();
    let mut item_projects = HashMap::new();
    let mut requests: Vec<UserActionRequestView> = Vec::new();
    for space in snapshot.registry.read_spaces()? {
        let outline = snapshot.registry.work_outline_snapshot(&space.id, true);
        for item in &outline.items {
            item_projects.insert(
                item.id.clone(),
                outline
                    .project_root(&item.id)
                    .map(|project| project.id.clone()),
            );
        }
        for assignment in &outline.assignments {
            let project = assignment
                .work_item_id
                .as_deref()
                .and_then(|id| outline.project_root(id))
                .map(|item| item.id.clone());
            projects.insert(assignment.chat_id.clone(), project);
        }
        let inputs = snapshot.registry.work_outline_inputs(&space.id, true);
        for row in inputs.user_action_requests {
            if let Some(request) = snapshot.registry.user_action_request(&row.id) {
                requests.push(request);
            }
        }
    }
    let mut groups = BTreeMap::<String, Accumulator>::new();
    for request in &requests {
        if !in_window(request.created_at) {
            continue;
        }
        let role = roles
            .get(request.chat_id.as_str())
            .copied()
            .unwrap_or("unknown");
        let project = item_projects
            .get(&request.work_item_id)
            .and_then(Option::as_deref);
        let group_keys = keys(&request.space_id, project, role);
        let kind = serde_json::to_value(request.effective_kind())?
            .as_str()
            .unwrap_or("unknown")
            .to_owned();
        update(&mut groups, &group_keys, |a| {
            *a.aggregate
                .requests_by_kind
                .entry(kind.clone())
                .or_default() += 1;
        });
        if let Some(resolution) = &request.resolution
            && resolution.at < until.timestamp_millis()
            && let Some(duration) = elapsed_millis(request.created_at, resolution.at)
        {
            let category = resolution_category(resolution, &request.chat_id).to_owned();
            update(&mut groups, &group_keys, |a| {
                a.durations
                    .entry(category.clone())
                    .or_default()
                    .push(duration)
            });
        }
    }
    for chat in &snapshot.chats {
        let Some(entries) = snapshot.transcripts.get(&chat.id) else {
            continue;
        };
        let role = roles.get(chat.id.as_str()).copied().unwrap_or("unknown");
        let space = chat.space_id.as_deref().unwrap_or("none");
        let project = projects.get(&chat.id).and_then(Option::as_deref);
        let group_keys = keys(space, project, role);
        for (index, entry) in entries.iter().enumerate() {
            if !in_window(entry.created_at) {
                continue;
            }
            let body = text(entry);
            if entry.role == MessageRole::User {
                if role == "coordinator"
                    && !is_relayed_or_system_message(&body)
                    && is_status_question(&body)
                {
                    if let Some(date) = DateTime::from_timestamp_millis(entry.created_at) {
                        let day = date.format("%Y-%m-%d").to_string();
                        update(&mut groups, &group_keys, |a| {
                            *a.aggregate
                                .status_questions_by_day
                                .entry(day.clone())
                                .or_default() += 1
                        });
                    }
                }
                if body.starts_with("[Mast system message, not from the user: auto-continue ") {
                    update(&mut groups, &group_keys, |a| {
                        a.aggregate.auto_continue_messages += 1
                    });
                }
                if body.starts_with("[Mast system message") && body.contains("worker stopped") {
                    update(&mut groups, &group_keys, |a| {
                        a.aggregate.worker_stopped_wakes += 1
                    });
                }
            }
            if entry.role != MessageRole::Assistant
                || entry.status == Some(MessageStatus::Aborted)
                || entry.status == Some(MessageStatus::Streaming)
            {
                continue;
            }
            let word_count = body.split_whitespace().count() as i64;
            if word_count > 0 {
                update(&mut groups, &group_keys, |a| a.words.push(word_count));
            }
            let next_user = entries[index + 1..]
                .iter()
                .position(|e| e.role == MessageRole::User)
                .map(|n| index + 1 + n)
                .unwrap_or(entries.len());
            if entries[index + 1..next_user].iter().any(|e| {
                e.role == MessageRole::Assistant && e.status != Some(MessageStatus::Aborted)
            }) {
                continue;
            }
            let turn_start = entries[..index]
                .iter()
                .rfind(|e| e.role == MessageRole::User)
                .map(|e| e.created_at)
                .unwrap_or(entry.created_at);
            let turn_end = entries
                .get(next_user)
                .map(|e| e.created_at)
                .unwrap_or(i64::MAX);
            let has_card = requests.iter().any(|r| {
                r.chat_id == chat.id && r.created_at >= turn_start && r.created_at < turn_end
            });
            if !has_card && is_question(&body) {
                update(&mut groups, &group_keys, |a| {
                    a.aggregate.prose_questions_without_card += 1
                });
            }
        }
    }
    let totals = groups.remove("all").unwrap_or_default().finish();
    Ok(Report { schema_version: 1, since: since.to_rfc3339(), until: until.to_rfc3339(), totals, groups: groups.into_iter().map(|(k, v)| (k, v.finish())).collect(), coverage: vec![
        "Window is UTC, inclusive since and exclusive until; request cohorts are based on creation time, resolved before until.".into(),
            "Only the supplied profile snapshot is included. Deleted or absent chats and documents are unavailable; chat2 snapshots may contain only a transcript tail and pending outbox updates are not included.".into(),
        "Coordinator means a chat with a recorded child; worker means a child without its own recorded child; others are plain. Assignments and project ancestry reflect snapshot state, not historical state.".into(),
        "Prose questions are heuristics on the last assistant entry before the next user entry. Turn boundaries and request matching are inferred from timestamps; classifier counts are not audited decisions.".into(),
        "Reply word counts cover nonempty completed assistant entries, including intermediate entries; status checks exclude Mast relay and system messages.".into(),
        "Wakes and auto-continues are lower bounds from transcript text; worker waiting durations cannot be reconstructed reliably from these snapshots. Journal events have no complete time history.".into(),
        format!("Missing chat documents: {}; failed chat decodes: {}; malformed journal lines: {}; decoded journal events: {}.", snapshot.chats.len().saturating_sub(snapshot.transcripts.len()+snapshot.failed_transcripts), snapshot.failed_transcripts, snapshot.malformed_journal_lines, snapshot.journal_events),
        "Resolution provenance absent or unrecognized is unknown; duplicate resolved rows are counted individually. No message text or classifier examples are exported.".into(),
    ] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::work_outline::WorkOutlineActor;

    #[test]
    fn resolution_provenance_distinguishes_agents_and_user_answers() {
        let mut resolution = UserActionResolution {
            outcome: None,
            values: None,
            via: Some("agent".into()),
            by: WorkOutlineActor {
                kind: WorkOutlineActorKind::Agent,
                device_id: String::new(),
                chat_id: Some("worker".into()),
                message_id: None,
            },
            at: 200,
            note: None,
            answer: None,
        };
        assert_eq!(
            resolution_category(&resolution, "worker"),
            "agent_self_resolve"
        );
        assert_eq!(resolution_category(&resolution, "other"), "agent_other");
        resolution.outcome = Some("superseded".into());
        assert_eq!(resolution_category(&resolution, "worker"), "superseded");
        resolution.outcome = None;
        resolution.via = Some("card".into());
        assert_eq!(
            resolution_category(&resolution, "worker"),
            "card_reply_or_choice"
        );
    }
}
