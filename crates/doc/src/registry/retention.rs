//! Retention stamps on chat rows: when the chat was archived and whether a
//! user ever restored it. Retention clocks need both; the chat's `archived`
//! flag alone records neither.
//!
//! The stamps are additive row fields (readers that predate them ignore them)
//! and are written by the archive writers in this crate, so no caller has to
//! remember to stamp. Standing-role rotation archives chats in its own batch
//! without stamping; those chats hold or held a role, which retention never
//! touches, and a missed stamp is repaired by the backfill below:
//! - `archivedAt` (epoch ms): set on each unarchived-to-archived transition.
//!   Archiving an already archived chat keeps the existing stamp.
//! - `archivedAtObserved`: the stamp was not written by the archive itself.
//!   [`RegistryDoc::backfill_archived_at`] wrote it when its device first saw
//!   a row that was archived before stamps existed.
//! - `restoreProtected`: set on every archived-to-unarchived transition and
//!   never cleared, so a later re-archive stays protected. It is its own
//!   field, so a concurrent archive elsewhere cannot overwrite it.
use super::*;

const ARCHIVED_AT: &str = "archivedAt";
const ARCHIVED_AT_OBSERVED: &str = "archivedAtObserved";
const RESTORE_PROTECTED: &str = "restoreProtected";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChatRetentionStamps {
    pub archived_at: Option<i64>,
    /// `archived_at` is the time a device first observed the archive, not the
    /// time of the archive itself.
    pub archive_observed: bool,
    pub restore_protected: bool,
}

impl RegistryDoc {
    /// The stamp fields that belong with an archive write for `chat_id`. Reads
    /// the row as this device sees it, including its own queued writes.
    pub(super) fn archive_stamp_fields(
        &self,
        chat_id: &str,
        archived: bool,
    ) -> BTreeMap<String, Value> {
        let row = self.overlay_row(KIND_CHATS, chat_id);
        let field = |name: &str| row.as_ref().and_then(|row| row.fields.get(name));
        let was_archived = field("archived").and_then(Value::as_bool) == Some(true);
        let has_stamp = field(ARCHIVED_AT).and_then(Value::as_i64).is_some();
        let mut set = BTreeMap::new();
        if archived && (!was_archived || !has_stamp) {
            set.insert(ARCHIVED_AT.into(), json!(Self::now_ms()));
            // A row that was already archived and lacks a stamp predates them.
            set.insert(ARCHIVED_AT_OBSERVED.into(), json!(was_archived));
        } else if !archived && was_archived {
            set.insert(RESTORE_PROTECTED.into(), json!(true));
        }
        set
    }

    /// `owner_device`'s archived scheduled-run chats that have no
    /// `archivedAt`. Other chats have no retention clock.
    pub fn unstamped_archived_runs(&self, owner_device: &str) -> Vec<String> {
        self.overlay_rows(KIND_CHATS)
            .into_iter()
            .filter(|row| {
                row.fields.get("archived").and_then(Value::as_bool) == Some(true)
                    && row.fields.get("deviceId").and_then(Value::as_str) == Some(owner_device)
                    && row
                        .fields
                        .get("scheduledTaskInfo")
                        .is_some_and(Value::is_object)
                    && row
                        .fields
                        .get(ARCHIVED_AT)
                        .and_then(Value::as_i64)
                        .is_none()
            })
            .map(|row| row.id)
            .collect()
    }

    /// `owner_device`'s scheduled-run chats that are unarchived now, carry an
    /// archive stamp, and have no `restoreProtected`: a client that predates
    /// the stamps restored them. Their old stamp must not time a re-archive.
    pub fn unprotected_restored_runs(&self, owner_device: &str) -> Vec<String> {
        self.overlay_rows(KIND_CHATS)
            .into_iter()
            .filter(|row| {
                row.fields.get("archived").and_then(Value::as_bool) != Some(true)
                    && row.fields.get("deviceId").and_then(Value::as_str) == Some(owner_device)
                    && row
                        .fields
                        .get("scheduledTaskInfo")
                        .is_some_and(Value::is_object)
                    && row
                        .fields
                        .get(ARCHIVED_AT)
                        .and_then(Value::as_i64)
                        .is_some()
                    && row
                        .fields
                        .get(RESTORE_PROTECTED)
                        .and_then(Value::as_bool)
                        != Some(true)
            })
            .map(|row| row.id)
            .collect()
    }

    /// Stamp [`Self::unstamped_archived_runs`] once, at `now_ms`, and protect
    /// [`Self::unprotected_restored_runs`]. Only the owner stamps, so devices
    /// do not race to restart each other's clocks; callers also wait for a
    /// synced registry so a stamp already written elsewhere is not replaced.
    /// Returns how many rows were written.
    pub fn backfill_archived_at(&mut self, owner_device: &str, now_ms: i64) -> usize {
        let restored = self.unprotected_restored_runs(owner_device);
        for id in &restored {
            self.write(
                KIND_CHATS,
                id,
                OpKind::Update,
                fields([(RESTORE_PROTECTED, json!(true))]),
            );
        }
        let rows = self.unstamped_archived_runs(owner_device);
        let count = rows.len() + restored.len();
        for id in &rows {
            self.write(
                KIND_CHATS,
                id,
                OpKind::Update,
                fields([
                    (ARCHIVED_AT, json!(now_ms)),
                    (ARCHIVED_AT_OBSERVED, json!(true)),
                ]),
            );
        }
        count
    }

    /// Retention stamps of every live chat row that has any.
    pub fn chat_retention_stamps(&self) -> HashMap<String, ChatRetentionStamps> {
        self.overlay_rows(KIND_CHATS)
            .into_iter()
            .filter_map(|row| {
                let stamps = ChatRetentionStamps {
                    archived_at: row.fields.get(ARCHIVED_AT).and_then(Value::as_i64),
                    archive_observed: row
                        .fields
                        .get(ARCHIVED_AT_OBSERVED)
                        .and_then(Value::as_bool)
                        .unwrap_or_default(),
                    restore_protected: row
                        .fields
                        .get(RESTORE_PROTECTED)
                        .and_then(Value::as_bool)
                        .unwrap_or_default(),
                };
                (stamps != ChatRetentionStamps::default()).then_some((row.id, stamps))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::chat;
    use super::*;

    fn doc_with(id: &str, device: &str, archived: bool) -> RegistryDoc {
        let mut doc = RegistryDoc::new(device);
        let mut row = chat(id, device);
        row.archived = archived;
        doc.upsert_chat(&row).unwrap();
        doc
    }

    /// A row as an older client wrote it: archived, with no stamps.
    fn legacy_archived(doc: &mut RegistryDoc, id: &str, device: &str) {
        doc.write(
            KIND_CHATS,
            id,
            OpKind::Upsert,
            fields([
                ("id", json!(id)),
                ("deviceId", json!(device)),
                ("archived", json!(true)),
                (
                    "scheduledTaskInfo",
                    json!({"taskId": "task", "taskName": "Task", "runId": id}),
                ),
            ]),
        );
    }

    fn stamps(doc: &RegistryDoc, id: &str) -> ChatRetentionStamps {
        doc.chat_retention_stamps().remove(id).unwrap_or_default()
    }

    #[test]
    fn archiving_stamps_the_transition_and_rearchiving_keeps_it() {
        let mut doc = doc_with("run", "dev-a", false);
        assert_eq!(stamps(&doc, "run"), ChatRetentionStamps::default());
        doc.set_chat_archived("run", true).unwrap();
        let first = stamps(&doc, "run");
        assert!(first.archived_at.is_some());
        assert!(!first.archive_observed && !first.restore_protected);
        // An archive of an already archived chat is not a new transition.
        doc.set_chat_archived("run", true).unwrap();
        assert_eq!(stamps(&doc, "run"), first);
    }

    #[test]
    fn restore_protection_is_sticky_and_a_new_archive_restarts_the_clock() {
        let mut doc = doc_with("run", "dev-a", false);
        doc.set_chat_archived("run", true).unwrap();
        doc.set_chat_archived("run", false).unwrap();
        assert!(stamps(&doc, "run").restore_protected);
        doc.set_chat_archived("run", true).unwrap();
        let again = stamps(&doc, "run");
        assert!(again.restore_protected, "re-archive keeps the protection");
        assert!(again.archived_at.is_some());
        doc.set_chat_archived("run", false).unwrap();
        doc.set_chat_archived("run", true).unwrap();
        assert!(stamps(&doc, "run").restore_protected);
    }

    #[test]
    fn unarchiving_a_chat_that_was_not_archived_protects_nothing() {
        let mut doc = doc_with("run", "dev-a", false);
        doc.set_chat_archived("run", false).unwrap();
        assert_eq!(stamps(&doc, "run"), ChatRetentionStamps::default());
    }

    #[test]
    fn a_restore_by_an_older_client_is_protected_by_the_owner_backfill() {
        let mut doc = doc_with("run", "dev-a", false);
        legacy_archived(&mut doc, "run", "dev-a");
        doc.backfill_archived_at("dev-a", 1_000);
        assert!(stamps(&doc, "run").archived_at.is_some());
        // The older client restores: archived=false, no protection written.
        doc.write(
            KIND_CHATS,
            "run",
            OpKind::Update,
            fields([("archived", json!(false))]),
        );
        assert!(!stamps(&doc, "run").restore_protected);
        assert_eq!(doc.unprotected_restored_runs("dev-a"), vec!["run".to_owned()]);
        assert!(doc.unprotected_restored_runs("dev-b").is_empty(), "owner only");
        assert_eq!(doc.backfill_archived_at("dev-a", 2_000), 1);
        assert!(stamps(&doc, "run").restore_protected);
        assert!(doc.unprotected_restored_runs("dev-a").is_empty());
        assert_eq!(doc.backfill_archived_at("dev-a", 3_000), 0, "idempotent");
    }

    #[test]
    fn full_row_upserts_stamp_like_the_dedicated_writers() {
        let mut doc = doc_with("run", "dev-a", true);
        assert!(stamps(&doc, "run").archived_at.is_some());
        let mut row = chat("run", "dev-a");
        row.archived = false;
        doc.upsert_chat(&row).unwrap();
        assert!(stamps(&doc, "run").restore_protected);
    }

    #[test]
    fn a_stale_full_row_upsert_cannot_clear_a_restore() {
        let mut doc = doc_with("run", "dev-a", true);
        doc.set_chat_archived("run", false).unwrap();
        let mut stale = chat("run", "dev-a");
        stale.archived = true;
        doc.upsert_chat(&stale).unwrap();
        assert!(stamps(&doc, "run").restore_protected);
    }

    #[test]
    fn backfill_stamps_only_the_owners_unstamped_archived_runs_once() {
        let mut doc = RegistryDoc::new("dev-a");
        legacy_archived(&mut doc, "mine", "dev-a");
        legacy_archived(&mut doc, "theirs", "dev-b");
        doc.write(
            KIND_CHATS,
            "plain",
            OpKind::Upsert,
            fields([
                ("id", json!("plain")),
                ("deviceId", json!("dev-a")),
                ("archived", json!(true)),
            ]),
        );
        doc.upsert_chat(&chat("live", "dev-a")).unwrap();
        doc.upsert_chat(&{
            let mut stamped = chat("stamped", "dev-a");
            stamped.archived = true;
            stamped
        })
        .unwrap();
        let before = stamps(&doc, "stamped");

        assert_eq!(doc.backfill_archived_at("dev-a", 1_000), 1);
        assert_eq!(
            stamps(&doc, "mine"),
            ChatRetentionStamps {
                archived_at: Some(1_000),
                archive_observed: true,
                restore_protected: false
            }
        );
        assert_eq!(stamps(&doc, "theirs"), ChatRetentionStamps::default());
        assert_eq!(
            stamps(&doc, "plain"),
            ChatRetentionStamps::default(),
            "not a scheduled run"
        );
        assert_eq!(stamps(&doc, "live"), ChatRetentionStamps::default());
        assert_eq!(stamps(&doc, "stamped"), before);

        // Idempotent: a later pass keeps the first observation.
        assert_eq!(doc.backfill_archived_at("dev-a", 9_000), 0);
        assert_eq!(stamps(&doc, "mine").archived_at, Some(1_000));
    }

    #[test]
    fn restoring_a_backfilled_chat_protects_it_and_a_new_archive_is_not_observed() {
        let mut doc = RegistryDoc::new("dev-a");
        legacy_archived(&mut doc, "run", "dev-a");
        doc.backfill_archived_at("dev-a", 1_000);
        doc.set_chat_archived("run", false).unwrap();
        doc.set_chat_archived("run", true).unwrap();
        let now = stamps(&doc, "run");
        assert!(now.restore_protected);
        assert!(!now.archive_observed);
        assert!(now.archived_at.is_some_and(|at| at > 1_000));
    }

    #[test]
    fn an_archive_without_a_stamp_is_stamped_as_observed_by_the_next_archive_write() {
        let mut doc = RegistryDoc::new("dev-a");
        legacy_archived(&mut doc, "run", "dev-a");
        doc.set_chat_archived("run", true).unwrap();
        let stamped = stamps(&doc, "run");
        assert!(stamped.archived_at.is_some());
        assert!(stamped.archive_observed);
    }

    #[test]
    fn a_restore_survives_a_concurrent_archive_from_another_device() {
        let op = |set: &[(&str, Value)], at: i64, device: &str| RowOp {
            kind: KIND_CHATS.into(),
            id: "run".into(),
            op: OpKind::Update,
            set: Some(
                set.iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
            ),
            hlc: encode_hlc(at, 0, device),
            clocks: None,
        };
        let base = RegistryRow {
            kind: KIND_CHATS.into(),
            id: "run".into(),
            seq: 1,
            deleted: false,
            del_hlc: None,
            fields: fields([("archived", json!(true)), ("archivedAt", json!(1_000))]),
            clocks: BTreeMap::new(),
        };
        let restore = op(
            &[
                ("archived", json!(false)),
                ("restoreProtected", json!(true)),
            ],
            2_000,
            "dev-a",
        );
        let archive = op(
            &[("archived", json!(true)), ("archivedAt", json!(2_500))],
            3_000,
            "dev-b",
        );
        let (row, _) = apply_op(Some(&base), &restore);
        let (row, _) = apply_op(row.as_ref(), &archive);
        let row = row.unwrap();
        assert_eq!(row.fields["restoreProtected"], json!(true));
        assert_eq!(row.fields["archivedAt"], json!(2_500));
    }
}
