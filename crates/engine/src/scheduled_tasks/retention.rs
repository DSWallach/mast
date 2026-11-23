//! Which archived scheduled-run chats have outlived their retention window.
//!
//! This only decides; nothing here deletes. A chat is eligible only when
//! every protection below passes, and a missing fact (no archive stamp, no
//! ledger record, no terminal result) defers rather than allows. The caller
//! gathers the facts from one registry read, then asks for a verdict per chat.
use super::ScheduledTasks;
use crate::EngineError;
use mast_doc::registry::retention::ChatRetentionStamps;
use mast_proto::Chat;
use std::collections::{HashMap, HashSet};

/// The period offered when a user turns deletion on.
pub const DEFAULT_RETENTION_DAYS: u32 = 14;
const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Policy {
    /// `None` is Off: no chat is eligible.
    pub days: Option<u32>,
    /// Count an archive this device first saw after archives were stamped
    /// (see `ChatRetentionStamps::archive_observed`). Off defers those runs
    /// until the user archives them again.
    pub count_observed_archives: bool,
    /// When the user turned deletion on. No archive's period starts before
    /// this, so enabling deletes nothing on the first day however old the
    /// archive is.
    pub enabled_at: i64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            days: Some(DEFAULT_RETENTION_DAYS),
            count_observed_archives: true,
            enabled_at: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ineligible {
    RetentionOff,
    NotScheduledRun,
    /// Another device hosts and owns the run, and only the owner decides.
    OtherDevice,
    NotArchived,
    /// The task or run identity is missing, so linkage cannot be proved.
    AmbiguousIdentity,
    /// Imported or legacy runs are outside the first release.
    Imported,
    /// A user restored it at least once; never purged automatically.
    RestoreProtected,
    /// No archive time yet; the owner stamps one once the registry syncs.
    AwaitingArchiveStamp,
    /// The archive time is only the time this device first saw the row.
    ObservedArchive,
    NotDue {
        due_at: i64,
    },
    StandingRole,
    Pinned,
    OpenRequest,
    /// The newest run of its task, whatever its state.
    NewestRun,
    /// A surviving chat is a child, side chat or follow-up of this one.
    RelatedChat,
    /// A turn is running or queued.
    ActiveWork,
    /// The scheduler's ledger has no run linked to this chat and run ID.
    NoRunRecord,
    /// The scheduler's ledger still has a live run on this chat.
    ActiveRun,
}

/// A scheduler ledger run recorded for a chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LedgerRun {
    pub id: String,
    pub task_id: String,
    pub active: bool,
}

pub(crate) struct Facts<'a> {
    pub device_id: &'a str,
    /// Every live chat row, archived or not.
    pub chats: &'a [Chat],
    pub stamps: &'a HashMap<String, ChatRetentionStamps>,
    pub pinned: &'a HashSet<String>,
    /// Chats held by a standing role, now or earlier in its chain.
    pub standing_roles: &'a HashSet<String>,
    pub open_requests: &'a HashSet<String>,
    /// Whether the chat was created by an external import.
    pub imported: &'a dyn Fn(&Chat) -> bool,
    /// Whether a turn is in flight or queued. Consulted last.
    pub busy: &'a dyn Fn(&str) -> Result<bool, EngineError>,
    /// The ledger's runs for a chat. Consulted last.
    pub ledger: &'a dyn Fn(&str) -> Result<Vec<LedgerRun>, EngineError>,
}

/// A verdict for every chat, in input order.
pub(crate) fn assess(
    facts: &Facts<'_>,
    policy: Policy,
    now_ms: i64,
) -> Result<Vec<(String, Result<(), Ineligible>)>, EngineError> {
    let newest = newest_runs(facts.chats);
    let mut with_children = HashSet::new();
    for chat in facts.chats {
        for parent in [&chat.parent_chat_id, &chat.side_of, &chat.followup_of]
            .into_iter()
            .flatten()
        {
            with_children.insert(parent.as_str());
        }
    }
    facts
        .chats
        .iter()
        .map(|chat| {
            let verdict = verdict(facts, policy, now_ms, chat, &newest, &with_children)?;
            Ok((chat.id.clone(), verdict))
        })
        .collect()
}

fn verdict(
    facts: &Facts<'_>,
    policy: Policy,
    now_ms: i64,
    chat: &Chat,
    newest: &HashSet<&str>,
    with_children: &HashSet<&str>,
) -> Result<Result<(), Ineligible>, EngineError> {
    use Ineligible::*;
    let Some(days) = policy.days else {
        return Ok(Err(RetentionOff));
    };
    let Some(info) = &chat.scheduled_task_info else {
        return Ok(Err(NotScheduledRun));
    };
    if chat.device_id != facts.device_id {
        return Ok(Err(OtherDevice));
    }
    if !chat.archived {
        return Ok(Err(NotArchived));
    }
    if info.task_id.is_empty() || info.run_id.is_empty() {
        return Ok(Err(AmbiguousIdentity));
    }
    if info.source_file.is_some() || info.legacy_task_id.is_some() || (facts.imported)(chat) {
        return Ok(Err(Imported));
    }
    let stamps = facts.stamps.get(&chat.id).copied().unwrap_or_default();
    if stamps.restore_protected {
        return Ok(Err(RestoreProtected));
    }
    let Some(archived_at) = stamps.archived_at else {
        return Ok(Err(AwaitingArchiveStamp));
    };
    if stamps.archive_observed && !policy.count_observed_archives {
        return Ok(Err(ObservedArchive));
    }
    let started = archived_at.max(policy.enabled_at);
    let due_at = started.saturating_add(i64::from(days).saturating_mul(DAY_MS));
    if now_ms < due_at {
        return Ok(Err(NotDue { due_at }));
    }
    if chat.standing_role_successor.is_some() || facts.standing_roles.contains(&chat.id) {
        return Ok(Err(StandingRole));
    }
    if facts.pinned.contains(&chat.id) {
        return Ok(Err(Pinned));
    }
    if facts.open_requests.contains(&chat.id) {
        return Ok(Err(OpenRequest));
    }
    if newest.contains(chat.id.as_str()) {
        return Ok(Err(NewestRun));
    }
    if with_children.contains(chat.id.as_str()) {
        return Ok(Err(RelatedChat));
    }
    if (facts.busy)(&chat.id)? {
        return Ok(Err(ActiveWork));
    }
    let runs = (facts.ledger)(&chat.id)?;
    if runs.iter().any(|run| run.active) {
        return Ok(Err(ActiveRun));
    }
    if !runs
        .iter()
        .any(|run| run.id == info.run_id && run.task_id == info.task_id)
    {
        return Ok(Err(NoRunRecord));
    }
    Ok(Ok(()))
}

/// The newest run of each task across its whole history, archived runs
/// included, by the scheduler's own ordering. Unlike the sidebar projection,
/// archiving the newest run does not release its predecessor's protection.
fn newest_runs(chats: &[Chat]) -> HashSet<&str> {
    let mut newest: HashMap<(&str, &Option<String>, &str), &Chat> = HashMap::new();
    for chat in chats {
        let Some(info) = &chat.scheduled_task_info else {
            continue;
        };
        let slot = newest
            .entry((
                chat.device_id.as_str(),
                &chat.space_id,
                info.task_id.as_str(),
            ))
            .or_insert(chat);
        if mast_proto::task_chat_order(chat) > mast_proto::task_chat_order(slot) {
            *slot = chat;
        }
    }
    newest.into_values().map(|chat| chat.id.as_str()).collect()
}

impl ScheduledTasks {
    /// Chats on this device whose archived scheduled run is due for deletion
    /// under `policy` at `now_ms`. Read-only.
    pub(crate) fn retention_candidates(
        &self,
        policy: Policy,
        now_ms: i64,
    ) -> Result<Vec<String>, EngineError> {
        let workspace = &self.0.workspace;
        let chats = workspace.read_chats()?;
        let stamps = workspace.chat_retention_stamps();
        let pinned = workspace
            .sidebar_preferences_snapshot()
            .pinned_session_ids
            .into_iter()
            .collect();
        let standing_roles = workspace.standing_role_chat_ids();
        let open_requests = workspace.open_user_action_chats();
        let root = std::path::Path::new(self.0.store.scope());
        let facts = Facts {
            device_id: workspace.device_id(),
            chats: &chats,
            stamps: &stamps,
            pinned: &pinned,
            standing_roles: &standing_roles,
            open_requests: &open_requests,
            imported: &|chat| crate::external_import::import_cursor_exists(root, &chat.id),
            busy: &|id| self.has_active_work(id),
            ledger: &|id| {
                Ok(self
                    .0
                    .store
                    .runs_for_chat(id)?
                    .into_iter()
                    .map(|run| LedgerRun {
                        active: run.state.active(),
                        id: run.id,
                        task_id: run.task_id,
                    })
                    .collect())
            },
        };
        Ok(assess(&facts, policy, now_ms)?
            .into_iter()
            .filter_map(|(id, verdict)| verdict.is_ok().then_some(id))
            .collect())
    }

    /// [`Self::retention_candidates`] for tests and diagnostics: `days` of
    /// `None` is Off. Nothing deletes yet; a purge pending a decision on its
    /// scope is the intended consumer.
    #[doc(hidden)]
    pub fn retention_candidate_ids(
        &self,
        days: Option<u32>,
        count_observed_archives: bool,
        now_ms: i64,
    ) -> Result<Vec<String>, EngineError> {
        self.retention_candidates(
            Policy {
                days,
                count_observed_archives,
                enabled_at: 0,
            },
            now_ms,
        )
    }

    /// Start the clock of runs archived before archives were stamped.
    pub(super) fn observe_archived_runs(&self, now_ms: i64) -> Result<usize, EngineError> {
        self.0.workspace.backfill_archived_at(now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    const NOW: i64 = 100 * DAY_MS;
    const ARCHIVED: i64 = NOW - 14 * DAY_MS;

    fn chat(id: &str, task: &str, created: i64) -> Chat {
        serde_json::from_value(json!({
            "id": id, "deviceId": "here", "archived": true, "spaceId": "space",
            "createdAt": "2026-01-01T00:00:00Z",
            "scheduledTaskInfo": {
                "taskId": task, "taskName": "Daily", "runId": format!("run-{id}"),
                "sessionCreatedAt": created,
            },
        }))
        .unwrap()
    }

    #[derive(Default)]
    struct World {
        chats: Vec<Chat>,
        stamps: HashMap<String, ChatRetentionStamps>,
        pinned: HashSet<String>,
        roles: HashSet<String>,
        requests: HashSet<String>,
        imported: HashSet<String>,
        busy: HashSet<String>,
        /// Chats whose ledger runs differ from the default terminal run.
        ledger: HashMap<String, Vec<LedgerRun>>,
        consulted: RefCell<Vec<String>>,
    }

    impl World {
        /// An archived run `old` that is due, with a newer run `new` of the same task.
        fn due() -> Self {
            let mut world = World::default();
            world.add(chat("old", "task", 10), Some(ARCHIVED));
            let mut new = chat("new", "task", 20);
            new.archived = false;
            world.add(new, None);
            world
        }

        fn add(&mut self, chat: Chat, archived_at: Option<i64>) {
            if let Some(at) = archived_at {
                self.stamps.insert(
                    chat.id.clone(),
                    ChatRetentionStamps {
                        archived_at: Some(at),
                        ..Default::default()
                    },
                );
            }
            self.chats.push(chat);
        }

        fn stamp(&mut self, id: &str, edit: impl FnOnce(&mut ChatRetentionStamps)) {
            edit(self.stamps.entry(id.into()).or_default());
        }

        fn chat_mut(&mut self, id: &str) -> &mut Chat {
            self.chats.iter_mut().find(|chat| chat.id == id).unwrap()
        }

        fn verdicts(&self, policy: Policy, now: i64) -> HashMap<String, Result<(), Ineligible>> {
            let facts = Facts {
                device_id: "here",
                chats: &self.chats,
                stamps: &self.stamps,
                pinned: &self.pinned,
                standing_roles: &self.roles,
                open_requests: &self.requests,
                imported: &|chat| self.imported.contains(&chat.id),
                busy: &|id| {
                    self.consulted.borrow_mut().push(id.into());
                    Ok(self.busy.contains(id))
                },
                ledger: &|id| {
                    Ok(self.ledger.get(id).cloned().unwrap_or_else(|| {
                        let info = self
                            .chats
                            .iter()
                            .find(|chat| chat.id == id)
                            .unwrap()
                            .scheduled_task_info
                            .clone()
                            .unwrap();
                        vec![LedgerRun {
                            id: info.run_id,
                            task_id: info.task_id,
                            active: false,
                        }]
                    }))
                },
            };
            assess(&facts, policy, now).unwrap().into_iter().collect()
        }

        fn verdict(&self, id: &str) -> Result<(), Ineligible> {
            self.verdicts(Policy::default(), NOW).remove(id).unwrap()
        }
    }

    #[test]
    fn a_due_run_with_a_newer_sibling_is_eligible() {
        let world = World::due();
        assert_eq!(world.verdict("old"), Ok(()));
        assert_eq!(world.verdict("new"), Err(Ineligible::NotArchived));
    }

    #[test]
    fn the_boundary_is_exactly_the_retention_interval() {
        let world = World::due();
        let policy = Policy::default();
        assert_eq!(world.verdicts(policy, NOW).remove("old").unwrap(), Ok(()));
        assert_eq!(
            world.verdicts(policy, NOW - 1).remove("old").unwrap(),
            Err(Ineligible::NotDue { due_at: NOW })
        );
        let thirty = Policy {
            days: Some(30),
            ..policy
        };
        assert_eq!(
            world.verdicts(thirty, NOW).remove("old").unwrap(),
            Err(Ineligible::NotDue {
                due_at: ARCHIVED + 30 * DAY_MS
            })
        );
    }

    #[test]
    fn off_makes_nothing_eligible() {
        let world = World::due();
        let off = Policy {
            days: None,
            ..Policy::default()
        };
        assert!(
            world
                .verdicts(off, NOW)
                .values()
                .all(|v| *v == Err(Ineligible::RetentionOff))
        );
    }

    #[test]
    fn the_newest_run_stays_even_when_archived_and_old() {
        let mut world = World::due();
        // The newer run is archived too, long ago. It is still the newest.
        world.chat_mut("new").archived = true;
        world.stamp("new", |s| s.archived_at = Some(ARCHIVED - DAY_MS));
        assert_eq!(world.verdict("new"), Err(Ineligible::NewestRun));
        assert_eq!(world.verdict("old"), Ok(()));
    }

    #[test]
    fn the_newest_run_is_per_task_device_and_space() {
        let mut world = World::due();
        world.add(chat("other-task", "task-2", 5), Some(ARCHIVED));
        assert_eq!(world.verdict("other-task"), Err(Ineligible::NewestRun));
        // Same task id in another space is its own history.
        let mut elsewhere = chat("elsewhere", "task", 5);
        elsewhere.space_id = Some("other-space".into());
        world.add(elsewhere, Some(ARCHIVED));
        assert_eq!(world.verdict("elsewhere"), Err(Ineligible::NewestRun));
        assert_eq!(world.verdict("old"), Ok(()));
    }

    #[test]
    fn a_tie_on_creation_time_protects_exactly_one_run() {
        let mut world = World::default();
        world.add(chat("a", "task", 10), Some(ARCHIVED));
        world.add(chat("b", "task", 10), Some(ARCHIVED));
        let verdicts = world.verdicts(Policy::default(), NOW);
        assert_eq!(verdicts["a"], Ok(()));
        assert_eq!(verdicts["b"], Err(Ineligible::NewestRun));
    }

    #[test]
    fn a_restored_run_is_never_eligible_even_after_rearchiving() {
        let mut world = World::due();
        world.stamp("old", |s| s.restore_protected = true);
        assert_eq!(world.verdict("old"), Err(Ineligible::RestoreProtected));
    }

    #[test]
    fn a_run_without_an_archive_stamp_waits_for_one() {
        let mut world = World::due();
        world.stamps.remove("old");
        assert_eq!(world.verdict("old"), Err(Ineligible::AwaitingArchiveStamp));
    }

    #[test]
    fn upgrade_observed_archives_wait_a_full_interval_and_can_be_excluded() {
        let mut world = World::due();
        world.stamp("old", |s| {
            s.archived_at = Some(NOW - DAY_MS);
            s.archive_observed = true;
        });
        // Observed yesterday: counted, but not yet due.
        assert_eq!(
            world.verdict("old"),
            Err(Ineligible::NotDue {
                due_at: NOW - DAY_MS + 14 * DAY_MS
            })
        );
        world.stamp("old", |s| s.archived_at = Some(ARCHIVED));
        assert_eq!(world.verdict("old"), Ok(()));
        let skip = Policy {
            count_observed_archives: false,
            ..Policy::default()
        };
        assert_eq!(
            world.verdicts(skip, NOW).remove("old").unwrap(),
            Err(Ineligible::ObservedArchive)
        );
    }

    #[test]
    fn turning_deletion_on_never_makes_an_older_archive_due_that_day() {
        let mut world = World::due();
        assert_eq!(world.verdict("old"), Ok(()));
        // Switched on a day ago: the period runs from then, not the archive.
        let late = Policy {
            enabled_at: NOW - DAY_MS,
            ..Policy::default()
        };
        assert_eq!(
            world.verdicts(late, NOW).remove("old").unwrap(),
            Err(Ineligible::NotDue {
                due_at: NOW - DAY_MS + 14 * DAY_MS
            })
        );
        let later = NOW + 13 * DAY_MS;
        assert_eq!(world.verdicts(late, later).remove("old").unwrap(), Ok(()));
        // An archive made after switching on keeps its own clock.
        world.stamp("old", |s| s.archived_at = Some(NOW - DAY_MS / 2));
        assert_eq!(
            world.verdicts(late, NOW).remove("old").unwrap(),
            Err(Ineligible::NotDue {
                due_at: NOW - DAY_MS / 2 + 14 * DAY_MS
            })
        );
    }

    #[test]
    fn imported_legacy_and_non_scheduled_chats_are_outside_the_release() {
        let mut world = World::due();
        world
            .chat_mut("old")
            .scheduled_task_info
            .as_mut()
            .unwrap()
            .source_file = Some("task.md".into());
        assert_eq!(world.verdict("old"), Err(Ineligible::Imported));
        world
            .chat_mut("old")
            .scheduled_task_info
            .as_mut()
            .unwrap()
            .source_file = None;
        world
            .chat_mut("old")
            .scheduled_task_info
            .as_mut()
            .unwrap()
            .legacy_task_id = Some("legacy".into());
        assert_eq!(world.verdict("old"), Err(Ineligible::Imported));
        world
            .chat_mut("old")
            .scheduled_task_info
            .as_mut()
            .unwrap()
            .legacy_task_id = None;
        world.imported.insert("old".into());
        assert_eq!(world.verdict("old"), Err(Ineligible::Imported));

        let mut plain = chat("plain", "task", 1);
        plain.scheduled_task_info = None;
        world.add(plain, Some(ARCHIVED));
        assert_eq!(world.verdict("plain"), Err(Ineligible::NotScheduledRun));
    }

    #[test]
    fn a_run_hosted_by_another_device_is_not_decided_here() {
        let mut world = World::due();
        world.chat_mut("old").device_id = "elsewhere".into();
        assert_eq!(world.verdict("old"), Err(Ineligible::OtherDevice));
    }

    #[test]
    fn missing_identity_defers() {
        let mut world = World::due();
        world
            .chat_mut("old")
            .scheduled_task_info
            .as_mut()
            .unwrap()
            .run_id
            .clear();
        assert_eq!(world.verdict("old"), Err(Ineligible::AmbiguousIdentity));
        world
            .chat_mut("old")
            .scheduled_task_info
            .as_mut()
            .unwrap()
            .run_id = "run-old".into();
        world
            .chat_mut("old")
            .scheduled_task_info
            .as_mut()
            .unwrap()
            .task_id
            .clear();
        assert_eq!(world.verdict("old"), Err(Ineligible::AmbiguousIdentity));
    }

    #[test]
    fn pinned_standing_role_and_open_request_chats_are_protected() {
        let mut world = World::due();
        world.pinned.insert("old".into());
        assert_eq!(world.verdict("old"), Err(Ineligible::Pinned));
        world.pinned.clear();
        world.requests.insert("old".into());
        assert_eq!(world.verdict("old"), Err(Ineligible::OpenRequest));
        world.requests.clear();
        world.roles.insert("old".into());
        assert_eq!(world.verdict("old"), Err(Ineligible::StandingRole));
        world.roles.clear();
        world.chat_mut("old").standing_role_successor = Some("next".into());
        assert_eq!(world.verdict("old"), Err(Ineligible::StandingRole));
    }

    #[test]
    fn a_surviving_child_side_chat_or_follow_up_defers_the_parent() {
        for relation in ["parentChatId", "sideOf", "followupOf"] {
            let mut world = World::due();
            let mut related = chat("related", "task-2", 1);
            related.scheduled_task_info = None;
            related.archived = false;
            let mut value = serde_json::to_value(&related).unwrap();
            value[relation] = json!("old");
            world.chats.push(serde_json::from_value(value).unwrap());
            assert_eq!(
                world.verdict("old"),
                Err(Ineligible::RelatedChat),
                "{relation}"
            );
        }
    }

    #[test]
    fn live_work_and_active_ledger_runs_defer() {
        let mut world = World::due();
        world.busy.insert("old".into());
        assert_eq!(world.verdict("old"), Err(Ineligible::ActiveWork));
        world.busy.clear();
        world.ledger.insert(
            "old".into(),
            vec![LedgerRun {
                id: "run-old".into(),
                task_id: "task".into(),
                active: true,
            }],
        );
        assert_eq!(world.verdict("old"), Err(Ineligible::ActiveRun));
    }

    #[test]
    fn a_run_the_ledger_cannot_link_by_task_and_run_id_defers() {
        let mut world = World::due();
        world.ledger.insert("old".into(), Vec::new());
        assert_eq!(world.verdict("old"), Err(Ineligible::NoRunRecord));
        world.ledger.insert(
            "old".into(),
            vec![LedgerRun {
                id: "run-old".into(),
                task_id: "another-task".into(),
                active: false,
            }],
        );
        assert_eq!(world.verdict("old"), Err(Ineligible::NoRunRecord));
        world.ledger.insert(
            "old".into(),
            vec![LedgerRun {
                id: "someone-else".into(),
                task_id: "task".into(),
                active: false,
            }],
        );
        assert_eq!(world.verdict("old"), Err(Ineligible::NoRunRecord));
    }

    #[test]
    fn expensive_lookups_are_skipped_for_chats_a_cheap_rule_already_excluded() {
        let mut world = World::due();
        world.stamp("old", |s| s.restore_protected = true);
        world.verdicts(Policy::default(), NOW);
        assert!(world.consulted.borrow().is_empty());
    }
}
