//! Permanent deletion of archived scheduled-run chats, off until a user sets a
//! retention period.
//!
//! The owning device decides (see `retention`), commits a `chatPurges` record
//! and the chat's tombstones through the registry, then every device erases
//! its own copies from that record. The record is the durable state: a crash
//! at any point resumes from it, every erase is idempotent, and a failed one
//! stays queued for the next pass. The run-history row stays, marked purged.
//!
//! This release erases what a device itself holds: the session document and
//! its rollback copy, the run journal and its sidecars, token-usage rows and
//! the ledger detail. It does not reach the synced document on the server,
//! other devices that never see the record, harness-owned transcripts,
//! artifacts or uploads.
use super::{ScheduledTasks, retention::Policy};
use crate::EngineError;
use mast_doc::registry::{ChatPurge, PurgeOutcome};

/// How often the owner looks for newly due chats, so a large registry is not
/// rescanned on every five-second tick.
const COMMIT_PASS_MS: i64 = 10 * 60_000;
/// How often every device looks for purge records it has not yet erased.
const RECONCILE_PASS_MS: i64 = 60_000;
/// A record whose erase failed is retried after this long.
const RETRY_BACKOFF_MS: i64 = 30_000;
const COMMITS_PER_PASS: usize = 10;
const ERASES_PER_PASS: usize = 20;

#[derive(Default)]
pub(super) struct PurgeState {
    /// Chats whose local copies this process has already erased.
    erased: std::collections::HashSet<String>,
    last_commit_pass: Option<i64>,
    last_reconcile: Option<i64>,
}

impl PurgeState {
    /// Run the next commit pass on the next tick, as when the setting changes.
    pub(super) fn commit_pass_due_now(&mut self) {
        self.last_commit_pass = None;
    }
}

fn purge_id(chat_id: &str) -> String {
    format!("purge:{chat_id}")
}

impl ScheduledTasks {
    fn purge_state(&self) -> std::sync::MutexGuard<'_, PurgeState> {
        self.0.purges.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One tick's purge work: commit what is due (owner), then erase.
    pub(super) fn purge_tick(&self, now_ms: i64) -> Result<(), EngineError> {
        // Committing and erasing are independent: a record from an earlier
        // pass or another device is erased even when committing fails.
        let committed = self.commit_due_purges(now_ms).unwrap_or_else(|error| {
            tracing::warn!(%error, "committing scheduled run purges failed");
            0
        });
        self.erase_purged(now_ms, committed > 0)?;
        Ok(())
    }

    /// Commit the purge of this device's archived scheduled runs that have
    /// outlived the retention period. Does nothing when retention is Off.
    pub(super) fn commit_due_purges(&self, now_ms: i64) -> Result<usize, EngineError> {
        let Some(days) = self.0.store.retention_days()? else {
            return Ok(0);
        };
        if !self.0.workspace.registry_synced() {
            return Ok(0);
        }
        {
            let mut state = self.purge_state();
            if state
                .last_commit_pass
                .is_some_and(|at| now_ms - at < COMMIT_PASS_MS)
            {
                return Ok(0);
            }
            state.last_commit_pass = Some(now_ms);
        }
        let policy = Policy {
            days: Some(days),
            count_observed_archives: true,
            enabled_at: self.0.store.retention_enabled_at(now_ms)?,
        };
        let stamps = self.0.workspace.chat_retention_stamps();
        let mut committed = 0;
        for chat_id in self
            .retention_candidates(policy, now_ms)?
            .into_iter()
            .take(COMMITS_PER_PASS)
        {
            let Some(archived_at) = stamps.get(&chat_id).and_then(|stamp| stamp.archived_at) else {
                continue;
            };
            let outcome = self.0.workspace.commit_scheduled_run_purge(
                &chat_id,
                archived_at,
                &purge_id(&chat_id),
                now_ms,
            )?;
            match outcome {
                PurgeOutcome::Committed(_) => {
                    committed += 1;
                    tracing::info!(chat = %chat_id, "permanently deleted an archived scheduled run");
                }
                // A record from an earlier attempt is erased by the pass below.
                PurgeOutcome::AlreadyPurged(_) | PurgeOutcome::Missing => {}
                PurgeOutcome::Blocked(why) => {
                    tracing::debug!(chat = %chat_id, ?why, "scheduled run purge deferred");
                }
            }
        }
        Ok(committed)
    }

    /// Erase this device's copies for each purge record it has not yet erased,
    /// and on the owner mark the run in history. `force` skips the interval.
    pub(super) fn erase_purged(&self, now_ms: i64, force: bool) -> Result<usize, EngineError> {
        {
            let mut state = self.purge_state();
            if !force
                && state
                    .last_reconcile
                    .is_some_and(|at| now_ms - at < RECONCILE_PASS_MS)
            {
                return Ok(0);
            }
            state.last_reconcile = Some(now_ms);
        }
        let pending: Vec<ChatPurge> = {
            let state = self.purge_state();
            self.0
                .workspace
                .chat_purges()
                .into_iter()
                .filter(|purge| !state.erased.contains(&purge.chat_id))
                .take(ERASES_PER_PASS)
                .collect()
        };
        if pending.is_empty() {
            return Ok(0);
        }
        // Erase only once the record and tombstones are acknowledged by the
        // server (no unacked batch; always true without an edge) and saved to
        // disk. A lost or rejected write must never leave a chat whose data is
        // gone and whose deletion no device will learn of.
        if self.0.workspace.registry_pending_len() > 0 {
            self.purge_state().last_reconcile = None;
            return Ok(0);
        }
        self.0.workspace.persist_registry_now()?;
        let mut erased = 0;
        for purge in pending {
            match self.erase_local_copies(&purge) {
                Ok(()) => {
                    self.purge_state().erased.insert(purge.chat_id);
                    erased += 1;
                }
                Err(error) => {
                    tracing::warn!(chat = %purge.chat_id, %error, "purged chat cleanup failed; will retry");
                    // Try again soon, but not every tick: each pass saves the
                    // registry, and a stuck record would repeat that forever.
                    self.purge_state().last_reconcile =
                        Some(now_ms - RECONCILE_PASS_MS + RETRY_BACKOFF_MS);
                }
            }
        }
        Ok(erased)
    }

    fn erase_local_copies(&self, purge: &ChatPurge) -> Result<(), EngineError> {
        if self.has_active_work(&purge.chat_id)? {
            return Err(super::fail("a turn is still running on the purged chat"));
        }
        self.0.host.purge_chat_data(&purge.chat_id)?;
        self.0.sessions.purge_chat_data(&purge.chat_id)?;
        if purge.device_id == self.0.workspace.device_id() {
            self.0
                .store
                .mark_chat_runs_purged(&purge.chat_id, purge.committed_at)?;
        }
        Ok(())
    }

    /// Whether this device has erased `chat_id`'s local copies since it
    /// started. Diagnostics and tests.
    #[doc(hidden)]
    pub fn purge_erased_locally(&self, chat_id: &str) -> bool {
        self.purge_state().erased.contains(chat_id)
    }
}
