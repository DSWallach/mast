//! Permanent purge records. A `chatPurges` row says a chat was deliberately
//! and permanently removed, which a bare tombstone does not: ordinary
//! tombstones revive on a newer upsert and are swept after 30 days. Every
//! device reads the rows to erase its own local copies, and this crate refuses
//! to recreate a purged chat id.
//!
//! The row holds identifiers only (no title, prompt, path or transcript text)
//! and is never deleted. It is the durable record a crashed purge resumes
//! from: the record commits first, then the chat's registry rows are
//! tombstoned, then each device cleans its stores from the record.
use super::*;

pub const KIND_CHAT_PURGES: &str = "chatPurges";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPurge {
    pub chat_id: String,
    /// Stable across retries of one purge.
    pub purge_id: String,
    /// The device that decided and owns the chat.
    pub device_id: String,
    /// The scheduled run's identity, for the run-history marker.
    pub task_id: String,
    pub run_id: String,
    pub committed_at: i64,
}

/// Why a scheduled-run purge was not committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeBlocker {
    NotScheduledRun,
    NotOwner,
    NotArchived,
    /// Archived again since the candidate was chosen.
    ArchiveChanged,
    RestoreProtected,
    Pinned,
    StandingRole,
    OpenRequest,
    RelatedChat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PurgeOutcome {
    Committed(ChatPurge),
    /// A record already exists. Returned as found, so a retry is idempotent.
    AlreadyPurged(ChatPurge),
    /// The chat row is gone and was never purged.
    Missing,
    Blocked(PurgeBlocker),
}

impl RegistryDoc {
    pub fn chat_purge(&self, chat_id: &str) -> Option<ChatPurge> {
        self.overlay_row(KIND_CHAT_PURGES, chat_id)
            .and_then(|row| row_to(&row))
    }

    pub fn chat_purged(&self, chat_id: &str) -> bool {
        self.overlay_row(KIND_CHAT_PURGES, chat_id).is_some()
    }

    /// Every purge record, by chat id.
    pub fn chat_purges(&self) -> Vec<ChatPurge> {
        let mut purges = self.read_kind::<ChatPurge>(KIND_CHAT_PURGES);
        purges.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        purges
    }

    /// Purge an archived scheduled run its owner chose at `expected_archived_at`.
    /// Rechecks every registry-visible protection in the same write, so a pin,
    /// restore, request or new child that arrived since selection wins. The
    /// record is queued before the tombstones.
    pub fn commit_scheduled_run_purge(
        &mut self,
        chat_id: &str,
        owner_device: &str,
        expected_archived_at: i64,
        purge_id: &str,
        now_ms: i64,
    ) -> Result<PurgeOutcome, DocError> {
        if let Some(existing) = self.chat_purge(chat_id) {
            // A client that predates the record can revive the tombstoned row.
            // The record stands, so write the tombstones again; this is
            // idempotent and, once the row is gone, a no-op.
            if self.row_exists(KIND_CHATS, chat_id) {
                let _ = self.delete_chat(chat_id);
            }
            return Ok(PurgeOutcome::AlreadyPurged(existing));
        }
        let Some(chat) = self.chat(chat_id)? else {
            return Ok(PurgeOutcome::Missing);
        };
        let blocked = |blocker| Ok(PurgeOutcome::Blocked(blocker));
        let Some(info) = &chat.scheduled_task_info else {
            return blocked(PurgeBlocker::NotScheduledRun);
        };
        if chat.device_id != owner_device {
            return blocked(PurgeBlocker::NotOwner);
        }
        if !chat.archived {
            return blocked(PurgeBlocker::NotArchived);
        }
        let stamps = self
            .chat_retention_stamps()
            .remove(chat_id)
            .unwrap_or_default();
        if stamps.restore_protected {
            return blocked(PurgeBlocker::RestoreProtected);
        }
        if stamps.archived_at != Some(expected_archived_at) {
            return blocked(PurgeBlocker::ArchiveChanged);
        }
        if self
            .sidebar_preferences()
            .is_some_and(|prefs| prefs.pinned_session_ids.iter().any(|id| id == chat_id))
        {
            return blocked(PurgeBlocker::Pinned);
        }
        if chat.standing_role_successor.is_some()
            || self.standing_role_of_chat(chat_id).is_some()
            || self.standing_roles().iter().any(|role| {
                role.current_chat_id.as_deref() == Some(chat_id)
                    || role.chain.iter().any(|id| id == chat_id)
            })
        {
            return blocked(PurgeBlocker::StandingRole);
        }
        if self.has_open_action_for_chat(chat_id) {
            return blocked(PurgeBlocker::OpenRequest);
        }
        if self.read_chats()?.iter().any(|other| {
            other.id != chat_id
                && [&other.parent_chat_id, &other.side_of, &other.followup_of]
                    .into_iter()
                    .flatten()
                    .any(|parent| parent == chat_id)
        }) {
            return blocked(PurgeBlocker::RelatedChat);
        }
        let purge = ChatPurge {
            chat_id: chat_id.to_owned(),
            purge_id: purge_id.to_owned(),
            device_id: owner_device.to_owned(),
            task_id: info.task_id.clone(),
            run_id: info.run_id.clone(),
            committed_at: now_ms,
        };
        let set = match serde_json::to_value(&purge)? {
            Value::Object(map) => map.into_iter().collect(),
            _ => unreachable!("a struct serializes to an object"),
        };
        self.write(KIND_CHAT_PURGES, chat_id, OpKind::Upsert, set);
        // The same tombstones an ordinary delete writes, so older readers see
        // the chat disappear the way they already understand.
        self.delete_chat(chat_id)?;
        Ok(PurgeOutcome::Committed(purge))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::chat;
    use super::*;
    use mast_proto::ScheduledChat;

    const OWNER: &str = "dev-a";

    fn run_doc() -> RegistryDoc {
        let mut doc = RegistryDoc::new(OWNER);
        let mut row = chat("run", OWNER);
        row.parent_chat_id = None;
        row.scheduled_task_info = Some(ScheduledChat {
            task_id: "task".into(),
            task_name: "Daily".into(),
            run_id: "run-1".into(),
            ..Default::default()
        });
        doc.upsert_chat(&row).unwrap();
        doc.set_chat_archived("run", true).unwrap();
        doc
    }

    fn archived_at(doc: &RegistryDoc) -> i64 {
        doc.chat_retention_stamps()["run"].archived_at.unwrap()
    }

    fn commit(doc: &mut RegistryDoc) -> PurgeOutcome {
        let at = archived_at(doc);
        doc.commit_scheduled_run_purge("run", OWNER, at, "purge-1", 5_000)
            .unwrap()
    }

    #[test]
    fn a_row_revived_by_an_older_client_is_tombstoned_again() {
        let mut doc = run_doc();
        assert!(matches!(commit(&mut doc), PurgeOutcome::Committed(_)));
        // A client that predates the record rewrites the chat row.
        let revived: BTreeMap<String, Value> =
            [("archived".to_owned(), serde_json::json!(true))].into();
        doc.write(KIND_CHATS, "run", OpKind::Upsert, revived);
        assert!(doc.row_exists(KIND_CHATS, "run"), "the zombie row is live");
        let at = archived_at(&doc);
        let outcome = doc
            .commit_scheduled_run_purge("run", OWNER, at, "purge-1", 6_000)
            .unwrap();
        assert!(matches!(outcome, PurgeOutcome::AlreadyPurged(_)));
        assert!(!doc.row_exists(KIND_CHATS, "run"));
        assert_eq!(doc.chat_purges().len(), 1, "still one record");
    }

    #[test]
    fn a_purge_records_the_run_and_tombstones_the_chat() {
        let mut doc = run_doc();
        let PurgeOutcome::Committed(purge) = commit(&mut doc) else {
            panic!("expected a commit");
        };
        assert_eq!(
            (
                purge.task_id.as_str(),
                purge.run_id.as_str(),
                purge.committed_at
            ),
            ("task", "run-1", 5_000)
        );
        assert!(doc.chat("run").unwrap().is_none());
        assert!(doc.chat_purged("run"));
        assert_eq!(doc.chat_purges(), vec![purge]);
        // Nothing about the chat's content is in the record.
        let row = doc.row(KIND_CHAT_PURGES, "run").unwrap();
        let mut keys: Vec<_> = row.fields.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "chatId",
                "committedAt",
                "deviceId",
                "purgeId",
                "runId",
                "taskId"
            ]
        );
    }

    #[test]
    fn the_record_is_queued_before_the_tombstones() {
        let mut doc = run_doc();
        doc.take_pushable();
        commit(&mut doc);
        let kinds: Vec<_> = doc
            .take_pushable()
            .into_iter()
            .flat_map(|batch| batch.ops)
            .map(|op| (op.kind, op.op))
            .collect();
        assert_eq!(
            kinds.first(),
            Some(&(KIND_CHAT_PURGES.to_owned(), OpKind::Upsert))
        );
        assert!(kinds.contains(&(KIND_CHATS.to_owned(), OpKind::Delete)));
    }

    #[test]
    fn a_repeat_commit_returns_the_first_record() {
        let mut doc = run_doc();
        let first = commit(&mut doc);
        let PurgeOutcome::Committed(purge) = first else {
            panic!("expected a commit");
        };
        doc.take_pushable();
        let again = doc
            .commit_scheduled_run_purge("run", OWNER, 0, "another-id", 9_000)
            .unwrap();
        assert_eq!(again, PurgeOutcome::AlreadyPurged(purge));
        assert!(doc.take_pushable().is_empty(), "a repeat writes nothing");
    }

    #[test]
    fn a_purged_id_is_not_recreated_or_claimed() {
        let mut doc = run_doc();
        commit(&mut doc);
        doc.upsert_chat(&chat("run", OWNER)).unwrap();
        doc.claim_chat("run", Some("/tmp"), None, DateTime::UNIX_EPOCH);
        assert!(doc.chat("run").unwrap().is_none());
        assert!(doc.chat_purged("run"));
    }

    #[test]
    fn a_replica_that_receives_the_record_refuses_to_recreate_the_chat() {
        let mut doc = run_doc();
        commit(&mut doc);
        let mut rows: BTreeMap<(String, String), RegistryRow> = BTreeMap::new();
        for op in doc.take_pushable().into_iter().flat_map(|batch| batch.ops) {
            let key = (op.kind.clone(), op.id.clone());
            let (next, _) = apply_op(rows.get(&key), &op);
            if let Some(next) = next {
                rows.insert(key, next);
            }
        }
        let mut replica = RegistryDoc::new("dev-b");
        let _ = replica.apply_rows(1, rows.into_values().collect());
        assert!(replica.chat_purged("run"));
        assert!(replica.chat("run").unwrap().is_none());
        replica.upsert_chat(&chat("run", "dev-b")).unwrap();
        assert!(replica.chat("run").unwrap().is_none());
    }

    #[test]
    fn a_purge_is_refused_when_a_protection_appeared_since_selection() {
        let blocked = |edit: &dyn Fn(&mut RegistryDoc), expected: PurgeBlocker| {
            let mut doc = run_doc();
            let at = archived_at(&doc);
            edit(&mut doc);
            assert_eq!(
                doc.commit_scheduled_run_purge("run", OWNER, at, "p", 1)
                    .unwrap(),
                PurgeOutcome::Blocked(expected),
                "{expected:?}"
            );
            assert!(
                doc.chat("run").unwrap().is_some(),
                "{expected:?} leaves the chat"
            );
            assert!(!doc.chat_purged("run"), "{expected:?} writes no record");
        };
        blocked(
            &|doc| {
                doc.set_chat_archived("run", false).unwrap();
            },
            PurgeBlocker::NotArchived,
        );
        blocked(
            &|doc| {
                doc.set_chat_archived("run", false).unwrap();
                doc.set_chat_archived("run", true).unwrap();
            },
            PurgeBlocker::RestoreProtected,
        );
        blocked(
            &|doc| {
                // Written by another device after the archive: a newer archive.
                doc.write(
                    KIND_CHATS,
                    "run",
                    OpKind::Update,
                    fields([("archivedAt", json!(i64::MAX))]),
                );
            },
            PurgeBlocker::ArchiveChanged,
        );
        blocked(
            &|doc| {
                let mut child = chat("child", OWNER);
                child.parent_chat_id = Some("run".into());
                doc.upsert_chat(&child).unwrap();
            },
            PurgeBlocker::RelatedChat,
        );
        blocked(
            &|doc| {
                let mut side = chat("side", OWNER);
                side.parent_chat_id = None;
                side.side_of = Some("run".into());
                doc.upsert_chat(&side).unwrap();
            },
            PurgeBlocker::RelatedChat,
        );
    }

    #[test]
    fn only_the_owner_purges_and_only_scheduled_runs() {
        let mut doc = run_doc();
        let at = archived_at(&doc);
        assert_eq!(
            doc.commit_scheduled_run_purge("run", "dev-b", at, "p", 1)
                .unwrap(),
            PurgeOutcome::Blocked(PurgeBlocker::NotOwner)
        );
        assert_eq!(
            doc.commit_scheduled_run_purge("missing", OWNER, at, "p", 1)
                .unwrap(),
            PurgeOutcome::Missing
        );
        let mut plain = chat("plain", OWNER);
        plain.archived = true;
        doc.upsert_chat(&plain).unwrap();
        assert_eq!(
            doc.commit_scheduled_run_purge("plain", OWNER, 0, "p", 1)
                .unwrap(),
            PurgeOutcome::Blocked(PurgeBlocker::NotScheduledRun)
        );
    }
}
