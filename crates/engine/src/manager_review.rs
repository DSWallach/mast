//! Owner-local storage for predeclared Manager review steps.
//!
//! This store serializes one engine's operations. It is not a distributed
//! ownership fence and must not be used to claim cross-device exclusivity.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use mast_proto::{ManagerReviewCompletion, ManagerReviewState, ManagerReviewStep};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewRecord {
    pub step: ManagerReviewStep,
    pub state: ManagerReviewState,
}

const REVIEW_SNAPSHOT_ID: &str = "internal:manager-review-steps:v1";

/// No production harness has yet demonstrated durable claim-keyed acceptance
/// and authoritative lookup across restart. Keep this fail-closed until a
/// receiver has passed the real crash-boundary qualification tests.
pub(crate) fn require_durable_review_receiver(_harness: mast_proto::HarnessId) -> Result<(), String> {
    Err("Automatic review requires a receiver with durable acceptance and lookup; no current receiver is qualified".into())
}

/// Inspect an existing local store without creating or migrating it. The caller
/// must hold the authority lock for this root for the entire decision.
pub(crate) fn inspect_local_review_state(data_dir: &std::path::Path) -> Result<bool, crate::EngineError> {
    let db = data_dir.join("profiles/local/docs.sqlite3");
    match std::fs::metadata(&db) {
        Ok(meta) if meta.is_file() => {},
        Ok(_) => return Err(crate::EngineError::Other("local review store is not a file".into())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if db.with_file_name("docs.sqlite3-wal").try_exists()? {
                return Err(crate::EngineError::Other("local review store has a WAL without a database".into()));
            }
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    }
    // SQLite can create or modify a shared-memory sidecar even when opened
    // read-only. Checking for a sidecar first would race its removal, and an
    // immutable connection would silently ignore committed WAL records. Until
    // a non-mutating WAL-aware reader is available, never open an existing DB.
    Err(crate::EngineError::Other("local review state is indeterminate: open the local profile for recovery; non-mutating WAL inspection is unavailable".into()))
}

pub(crate) struct ManagerReviews {
    store: Arc<mast_sync::DocsStore>,
    records: Mutex<BTreeMap<String, ReviewRecord>>,
}

/// A local-to-synced import cannot silently strand review obligations.
/// Until safe migration exists, even started records remain blocked: this
/// store does not yet have a durable terminal-completion state.
pub(crate) fn ensure_import_safe(store: &mast_sync::DocsStore) -> Result<(), crate::EngineError> {
    let Some(bytes) = store.load_snapshot(REVIEW_SNAPSHOT_ID)? else {
        return Ok(());
    };
    let records: BTreeMap<String, ReviewRecord> =
        serde_json::from_slice(&bytes).map_err(|error| {
            crate::EngineError::Other(format!(
                "Manager review state cannot be read; resolve it before importing: {error}"
            ))
        })?;
    if !records.is_empty() {
        return Err(crate::EngineError::Other(
            "Local Manager review steps cannot be imported safely yet. Finish or resolve them in the local profile before importing".into(),
        ));
    }
    Ok(())
}

impl ManagerReviews {
    pub(crate) fn open(store: Arc<mast_sync::DocsStore>) -> Result<Self, String> {
        let records = match store
            .load_snapshot(REVIEW_SNAPSHOT_ID)
            .map_err(|error| error.to_string())?
        {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string())?,
            None => BTreeMap::new(),
        };
        Ok(Self {
            store,
            records: Mutex::new(records),
        })
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, ReviewRecord>> {
        self.records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn persist(
        &self,
        records: &BTreeMap<String, ReviewRecord>,
        claims: &[String],
    ) -> Result<(), String> {
        let bytes = serde_json::to_vec(records).map_err(|error| error.to_string())?;
        self.store
            .save_snapshot_marking_processed(REVIEW_SNAPSHOT_ID, &bytes, claims)
            .map_err(|error| error.to_string())
    }

    /// Register immutable settings; exact retries replay, changed inputs fail.
    /// Caller must first authorize the current Manager and scoped worker.
    pub(crate) fn register(&self, step: ManagerReviewStep) -> Result<(), String> {
        let normalized_key =
            mast_proto::agent_tools::validate_idempotency_key(Some(&step.worker_create_key))?;
        if normalized_key != step.worker_create_key {
            return Err("worker launch key must be normalized".into());
        }
        if step.worker_inputs_hash.is_empty()
            || step.worker_inputs_hash.len() > 128
            || !step
                .worker_inputs_hash
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        {
            return Err("worker inputs hash must be a hex digest".into());
        }
        let worker_op = mast_proto::agent_tools::operation_id(
            &step.original_manager_chat_id,
            &step.worker_create_key,
        );
        if step.worker_launch.operation_id != worker_op
            || step.worker_launch.work_item_id != step.work_item_id
            || step.worker_launch.parent_chat_id.as_deref()
                != Some(step.original_manager_chat_id.as_str())
        {
            return Err("worker launch does not match the declared operation".into());
        }
        let expected_worker = mast_proto::derive_id(&worker_op, mast_proto::roles::CHAT);
        if step.worker_chat_id != expected_worker {
            return Err("worker identity does not match the planned launch".into());
        }
        let expected_operation = mast_proto::derive_id(
            &format!("{}\0{}", step.project_id, step.step_id),
            "manager-review-operation",
        );
        if step.operation_id != expected_operation {
            return Err("review operation does not match its project and step".into());
        }
        // Callers hold WorkspaceHost's shared child-claim mutex across this
        // read/commit, including all normal create_chat claims.
        let mut records = self.lock();
        let (operation_key, inputs_key) =
            crate::agent_tools::child_chat_ledger_keys(&worker_op, &step.worker_inputs_hash);
        let claimed = self
            .store
            .is_processed(&operation_key)
            .map_err(|error| error.to_string())?;
        let matching = self
            .store
            .is_processed(&inputs_key)
            .map_err(|error| error.to_string())?;
        if claimed && !matching {
            return Err("worker launch operation has different inputs".into());
        }
        if let Some(existing) = records.get(&step.step_id) {
            return if existing.step != step {
                Err("review step already has different inputs".into())
            } else if !claimed || !matching {
                Err("review step has no matching durable worker claim".into())
            } else {
                Ok(())
            };
        }
        if [
            &step.step_id,
            &step.project_id,
            &step.work_item_id,
            &step.worker_chat_id,
            &step.original_manager_chat_id,
            &step.operation_id,
            &step.review_prompt,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
        {
            return Err("review step fields must not be empty".into());
        }
        let mut next = records.clone();
        next.insert(
            step.step_id.clone(),
            ReviewRecord {
                step,
                state: ManagerReviewState::Pending,
            },
        );
        self.persist(&next, &[operation_key, inputs_key])?;
        *records = next;
        Ok(())
    }

    /// Accept only the worker identity authenticated by the engine grant.
    /// The exact reviewer launch, including its parent and evidence, is
    /// frozen in the same transaction as the completion.
    pub(crate) fn accept(
        &self,
        worker: &str,
        current_manager: &str,
        completion: ManagerReviewCompletion,
    ) -> Result<(), String> {
        let mut records = self.lock();
        let record = records
            .get(&completion.step_id)
            .ok_or("unknown review step")?;
        if record.step.worker_chat_id != worker {
            return Err("worker is not authorized for this review step".into());
        }
        if current_manager.trim().is_empty()
            || !matches!(completion.tested_sha.len(), 40 | 64)
            || !completion
                .tested_sha
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || completion.evidence.trim().is_empty()
        {
            return Err("A Manager, full tested commit SHA and evidence are required".into());
        }
        let accepted = match &record.state {
            ManagerReviewState::Pending => None,
            ManagerReviewState::Accepted { completion, .. }
            | ManagerReviewState::Started { completion, .. } => Some(completion),
            ManagerReviewState::Blocked {
                completion: Some(completion),
                ..
            } => Some(completion),
            ManagerReviewState::Blocked {
                completion: None, ..
            } => return Err("Review step is blocked; resolve it before reporting".into()),
        };
        if let Some(accepted) = accepted {
            return if accepted == &completion {
                Ok(())
            } else {
                Err("review completion conflicts with the accepted result".into())
            };
        }
        let launch = mast_proto::CreateChatForWorkItemParams {
            work_item_id: record.step.work_item_id.clone(),
            initial_prompt: mast_proto::WorkItemPrompt {
                text: format!(
                    "{}\n\nTested commit: {}\nTest evidence:\n{}",
                    record.step.review_prompt, completion.tested_sha, completion.evidence
                ),
                attachments: Vec::new(),
            },
            config: Some(record.step.reviewer_config.clone()),
            execution_override: None,
            worktree_choice: None,
            operation_id: record.step.operation_id.clone(),
            parent_chat_id: Some(current_manager.to_owned()),
            target_device_id: None,
            account_id: None,
            standing_role: None,
        };
        let mut next = records.clone();
        let step_id = completion.step_id.clone();
        next.get_mut(&step_id).unwrap().state = ManagerReviewState::Accepted {
            completion,
            review_launch: launch,
        };
        self.persist(&next, &[])?;
        *records = next;
        Ok(())
    }

    /// Record a recoverable failure without replacing the accepted result.
    pub(crate) fn block(&self, step_id: &str, reason: &str, owner: &str) -> Result<(), String> {
        if reason.trim().is_empty() || owner.trim().is_empty() {
            return Err("blocked review requires a reason and owner".into());
        }
        let mut records = self.lock();
        let record = records.get(step_id).ok_or("unknown review step")?;
        let (completion, review_launch) = match &record.state {
            ManagerReviewState::Pending => (None, None),
            ManagerReviewState::Accepted {
                completion,
                review_launch,
            } => (Some(completion.clone()), Some(review_launch.clone())),
            ManagerReviewState::Blocked {
                completion,
                review_launch,
                ..
            } => (completion.clone(), review_launch.clone()),
            ManagerReviewState::Started { .. } => {
                return Err("a started review cannot be blocked".into());
            }
        };
        let mut next = records.clone();
        next.get_mut(step_id).unwrap().state = ManagerReviewState::Blocked {
            completion,
            review_launch,
            reason: reason.into(),
            owner: owner.into(),
        };
        self.persist(&next, &[])?;
        *records = next;
        Ok(())
    }

    /// A deliberate unblock retains the original immutable completion and launch.
    pub(crate) fn unblock(&self, step_id: &str) -> Result<(), String> {
        let mut records = self.lock();
        let record = records.get(step_id).ok_or("unknown review step")?;
        let state = match &record.state {
            ManagerReviewState::Blocked {
                completion: Some(completion),
                review_launch: Some(review_launch),
                ..
            } => ManagerReviewState::Accepted {
                completion: completion.clone(),
                review_launch: review_launch.clone(),
            },
            ManagerReviewState::Blocked {
                completion: None,
                review_launch: None,
                ..
            } => ManagerReviewState::Pending,
            ManagerReviewState::Blocked { .. } => {
                return Err("blocked review has inconsistent launch data".into());
            }
            _ => return Err("review step is not blocked".into()),
        };
        let mut next = records.clone();
        next.get_mut(step_id).unwrap().state = state;
        self.persist(&next, &[])?;
        *records = next;
        Ok(())
    }

    /// Called only after the recorded first-turn command is durably queued.
    pub(crate) fn mark_started(&self, step_id: &str, reviewer_chat_id: &str) -> Result<(), String> {
        let mut records = self.lock();
        let record = records.get(step_id).ok_or("unknown review step")?;
        let expected = mast_proto::derive_id(&record.step.operation_id, mast_proto::roles::CHAT);
        if reviewer_chat_id != expected {
            return Err("reviewer identity does not match the recorded operation".into());
        }
        let (completion, review_launch) = match &record.state {
            ManagerReviewState::Accepted {
                completion,
                review_launch,
            } => (completion.clone(), review_launch.clone()),
            ManagerReviewState::Started {
                reviewer_chat_id: id,
                ..
            } if id == &expected => return Ok(()),
            _ => return Err("review step has no accepted completion".into()),
        };
        let mut next = records.clone();
        next.get_mut(step_id).unwrap().state = ManagerReviewState::Started {
            completion,
            review_launch,
            reviewer_chat_id: reviewer_chat_id.into(),
        };
        self.persist(&next, &[])?;
        *records = next;
        Ok(())
    }

    pub(crate) fn records(&self) -> Vec<ReviewRecord> {
        self.lock().values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{ChatConfig, HarnessId, SandboxLevel};

    #[test]
    fn current_review_receivers_are_not_qualified() {
        for harness in [HarnessId::ClaudeCode, HarnessId::Codex, HarnessId::Cursor, HarnessId::Devin, HarnessId::Grok, HarnessId::Hermes, HarnessId::Pi, HarnessId::Opencode, HarnessId::Antigravity, HarnessId::Mock] {
            assert!(require_durable_review_receiver(harness).unwrap_err().contains("no current receiver is qualified"));
        }
    }

    fn step() -> ManagerReviewStep {
        ManagerReviewStep {
            step_id: "step".into(),
            project_id: "project".into(),
            work_item_id: "item".into(),
            worker_chat_id: mast_proto::derive_id(
                &mast_proto::agent_tools::operation_id("manager", "worker-key"),
                mast_proto::roles::CHAT,
            ),
            worker_create_key: "worker-key".into(),
            worker_inputs_hash: "abcd".into(),
            worker_launch: mast_proto::CreateChatForWorkItemParams {
                work_item_id: "item".into(),
                initial_prompt: mast_proto::WorkItemPrompt {
                    text: "test task".into(),
                    attachments: Vec::new(),
                },
                config: None,
                execution_override: None,
                worktree_choice: None,
                operation_id: mast_proto::agent_tools::operation_id("manager", "worker-key"),
                parent_chat_id: Some("manager".into()),
                target_device_id: None,
                account_id: None,
                standing_role: None,
            },
            original_manager_chat_id: "manager".into(),
            operation_id: mast_proto::derive_id("project\0step", "manager-review-operation"),
            review_prompt: "Review the candidate".into(),
            reviewer_config: ChatConfig {
                harness: HarnessId::Mock,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::ReadOnly,
                auto_approve: false,
                approval_rules: false,
            },
        }
    }

    #[test]
    fn read_only_inspection_handles_absent_pre_feature_wal_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!inspect_local_review_state(dir.path()).unwrap());
        assert!(!dir.path().join("profiles").exists());
        let root = dir.path().join("profiles/local");
        let db = Arc::new(mast_sync::DocsStore::open(&root).unwrap());
        assert!(inspect_local_review_state(dir.path()).is_err());
        let reviews = ManagerReviews::open(db.clone()).unwrap();
        reviews.register(step()).unwrap();
        // The writer remains open: the committed snapshot may still be in WAL.
        assert!(inspect_local_review_state(dir.path()).is_err());
        db.save_snapshot(REVIEW_SNAPSHOT_ID, b"not json").unwrap();
        assert!(inspect_local_review_state(dir.path()).is_err());
        drop(reviews);
        drop(db);
        let bad = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(bad.path().join("profiles/local")).unwrap();
        std::fs::write(bad.path().join("profiles/local/docs.sqlite3"), b"invalid sqlite").unwrap();
        assert!(inspect_local_review_state(bad.path()).is_err());
    }

    #[test]
    fn inspection_refuses_wal_without_shm_without_touching_source() {
        let source = tempfile::tempdir().unwrap();
        let live = source.path().join("live");
        let db = Arc::new(mast_sync::DocsStore::open(&live).unwrap());
        let reviews = ManagerReviews::open(db).unwrap();
        reviews.register(step()).unwrap();
        let target = tempfile::tempdir().unwrap();
        let root = target.path().join("profiles/local");
        std::fs::create_dir_all(&root).unwrap();
        for name in ["docs.sqlite3", "docs.sqlite3-wal"] {
            std::fs::copy(live.join(name), root.join(name)).unwrap();
        }
        let before: Vec<_> = ["docs.sqlite3", "docs.sqlite3-wal"].iter().map(|n| std::fs::read(root.join(n)).unwrap()).collect();
        let error = inspect_local_review_state(target.path()).unwrap_err().to_string();
        assert!(error.contains("indeterminate"), "{error}");
        assert!(!root.join("docs.sqlite3-shm").exists());
        let mut names: Vec<_> = std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        assert_eq!(names, ["docs.sqlite3", "docs.sqlite3-wal"].map(std::ffi::OsString::from));
        for (index, name) in ["docs.sqlite3", "docs.sqlite3-wal"].iter().enumerate() {
            assert_eq!(std::fs::read(root.join(name)).unwrap(), before[index]);
        }
    }

    #[test]
    fn local_import_refuses_every_nonterminal_review_state() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(mast_sync::DocsStore::open(dir.path()).unwrap());
        assert!(ensure_import_safe(&db).is_ok());
        let reviews = ManagerReviews::open(db.clone()).unwrap();
        reviews.register(step()).unwrap();
        assert!(ensure_import_safe(&db).is_err());
        reviews
            .block("step", "worker unavailable", "manager")
            .unwrap();
        assert!(ensure_import_safe(&db).is_err());
        reviews.unblock("step").unwrap();
        let completion = ManagerReviewCompletion {
            step_id: "step".into(),
            tested_sha: "a".repeat(40),
            evidence: "tests passed".into(),
        };
        reviews
            .accept(&step().worker_chat_id, "manager", completion)
            .unwrap();
        assert!(ensure_import_safe(&db).is_err());
        let reviewer = mast_proto::derive_id(&step().operation_id, mast_proto::roles::CHAT);
        reviews.mark_started("step", &reviewer).unwrap();
        assert!(ensure_import_safe(&db).is_err());
    }

    #[test]
    fn failed_registration_transaction_leaves_no_partial_claim_or_step() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(mast_sync::DocsStore::open(dir.path()).unwrap());
        let reviews = ManagerReviews::open(db.clone()).unwrap();
        let connection = rusqlite::Connection::open(dir.path().join("docs.sqlite3")).unwrap();
        connection.execute_batch("CREATE TRIGGER reject_review_claim BEFORE INSERT ON processed_commands BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
        let planned = step();
        let operation = mast_proto::agent_tools::operation_id(
            &planned.original_manager_chat_id,
            &planned.worker_create_key,
        );
        let (operation_key, inputs_key) =
            crate::agent_tools::child_chat_ledger_keys(&operation, &planned.worker_inputs_hash);
        assert!(reviews.register(planned.clone()).is_err());
        assert!(reviews.records().is_empty());
        assert!(!db.is_processed(&operation_key).unwrap());
        assert!(!db.is_processed(&inputs_key).unwrap());
        assert!(db.load_snapshot(REVIEW_SNAPSHOT_ID).unwrap().is_none());
        connection
            .execute_batch("DROP TRIGGER reject_review_claim;")
            .unwrap();
        reviews.register(planned).unwrap();
        assert!(db.is_processed(&operation_key).unwrap());
        assert!(db.is_processed(&inputs_key).unwrap());
        assert_eq!(reviews.records().len(), 1);
    }

    #[test]
    fn claims_reject_changed_worker_inputs_and_keep_concurrent_steps() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(mast_sync::DocsStore::open(dir.path()).unwrap());
        let reviews = Arc::new(ManagerReviews::open(db.clone()).unwrap());
        reviews.register(step()).unwrap();
        let mut conflict = step();
        conflict.step_id = "other".into();
        conflict.operation_id = mast_proto::derive_id("project\0other", "manager-review-operation");
        conflict.worker_inputs_hash = "ef01".into();
        assert!(reviews.register(conflict).is_err());
        let workers: Vec<_> = (0..8)
            .map(|index| {
                let reviews = reviews.clone();
                std::thread::spawn(move || {
                    let mut next = step();
                    next.step_id = format!("step-{index}");
                    next.operation_id = mast_proto::derive_id(
                        &format!("project\0{}", next.step_id),
                        "manager-review-operation",
                    );
                    next.worker_create_key = format!("worker-{index}");
                    let op =
                        mast_proto::agent_tools::operation_id("manager", &next.worker_create_key);
                    next.worker_chat_id = mast_proto::derive_id(&op, mast_proto::roles::CHAT);
                    next.worker_launch.operation_id = op;
                    reviews.register(next).unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        drop(reviews);
        assert_eq!(ManagerReviews::open(db).unwrap().records().len(), 9);
    }

    #[test]
    fn completion_is_authorized_immutable_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(mast_sync::DocsStore::open(dir.path()).unwrap());
        let store = ManagerReviews::open(db.clone()).unwrap();
        store.register(step()).unwrap();
        let mut forged = step();
        forged.step_id = "forged".into();
        assert!(store.register(forged).is_err());
        let mut forged_worker = step();
        forged_worker.worker_chat_id = "some-other-chat".into();
        assert!(store.register(forged_worker).is_err());
        let completion = ManagerReviewCompletion {
            step_id: "step".into(),
            tested_sha: "a".repeat(40),
            evidence: "tests passed".into(),
        };
        assert!(store.accept("", "manager", completion.clone()).is_err());
        assert!(
            store
                .accept("other", "manager", completion.clone())
                .is_err()
        );
        assert!(matches!(
            store.records()[0].state,
            ManagerReviewState::Pending
        ));
        store
            .block("step", "worker launch failed", "manager")
            .unwrap();
        assert!(
            store
                .accept(&step().worker_chat_id, "manager", completion.clone())
                .is_err()
        );
        store.unblock("step").unwrap();
        store
            .accept(&step().worker_chat_id, "manager", completion.clone())
            .unwrap();
        store
            .accept(&step().worker_chat_id, "manager", completion.clone())
            .unwrap();
        let original_launch = match &store.records()[0].state {
            ManagerReviewState::Accepted { review_launch, .. } => review_launch.clone(),
            _ => panic!("completion was not accepted"),
        };
        assert!(
            original_launch
                .initial_prompt
                .text
                .contains(&completion.tested_sha)
        );
        assert!(
            original_launch
                .initial_prompt
                .text
                .contains(&completion.evidence)
        );
        assert!(
            original_launch
                .initial_prompt
                .text
                .contains(&step().review_prompt)
        );
        assert_eq!(original_launch.config, Some(step().reviewer_config));
        assert_eq!(original_launch.operation_id, step().operation_id);
        assert_eq!(original_launch.parent_chat_id.as_deref(), Some("manager"));
        store
            .block("step", "reviewer unavailable", "operator")
            .unwrap();
        drop(store);
        let store = ManagerReviews::open(db.clone()).unwrap();
        assert!(
            matches!(&store.records()[0].state, ManagerReviewState::Blocked { reason, owner, .. } if reason == "reviewer unavailable" && owner == "operator")
        );
        store.unblock("step").unwrap();
        let mut changed = completion.clone();
        changed.tested_sha = "b".repeat(40);
        assert!(
            store
                .accept(&step().worker_chat_id, "manager", changed)
                .is_err()
        );
        drop(store);
        let reopened = ManagerReviews::open(db.clone()).unwrap();
        assert!(
            matches!(&reopened.records()[0].state, ManagerReviewState::Accepted { completion: saved, .. } if saved == &completion)
        );
        reopened
            .accept(&step().worker_chat_id, "successor", completion.clone())
            .unwrap();
        match &reopened.records()[0].state {
            ManagerReviewState::Accepted { review_launch, .. } => {
                assert_eq!(review_launch, &original_launch)
            }
            _ => panic!("retry changed accepted review state"),
        }
        assert!(reopened.register(step()).is_ok());
        let mut changed_step = step();
        changed_step.review_prompt = "changed".into();
        assert!(reopened.register(changed_step).is_err());
    }
}
