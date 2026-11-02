//! Admin chats: the device-local grant store, the per-turn audit log with
//! inverse mutations (for one-step undo of a sweep), and bulk-change
//! approvals (contract amendment "admin chats").
//!
//! Only the RPC layer (`GrantAdmin`, `RevokeAdmin`) writes grants; no agent
//! tool does. A grant names one chat and one Space, may expire, and is
//! re-checked on every agent call (and between the steps of a batch), so a
//! revoke or expiry takes effect immediately.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use mast_proto::{
    AdminAuditAction, AdminAuditTurn, AdminGrant, ADMIN_DEFAULT_DURATION_SECS, WorkOutlineMutation,
};

/// How long a bulk-change approval token stays usable.
pub const BULK_APPROVAL_TTL_MS: i64 = 30 * 60 * 1000;
/// Audit turns kept in memory (oldest dropped first).
const MAX_AUDIT_TURNS: usize = 200;

struct TurnRecord {
    turn: AdminAuditTurn,
    /// One inverse per action (`None` = not undoable), same order.
    inverses: Vec<Option<WorkOutlineMutation>>,
}

#[derive(Clone)]
struct Approval {
    chat_id: String,
    steps_hash: String,
    request_id: String,
    expires_at_ms: i64,
    used: bool,
}

#[derive(Default)]
struct State {
    grants: HashMap<String, AdminGrant>,
    store: Option<PathBuf>,
    audit: Vec<TurnRecord>,
    approvals: HashMap<String, Approval>,
}

#[derive(Default)]
pub struct AdminGrants {
    state: Mutex<State>,
    /// Added to the wall clock (tests move time with `advance_clock_ms`).
    clock_offset_ms: AtomicI64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl AdminGrants {
    pub fn now_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis() + self.clock_offset_ms.load(Ordering::Relaxed)
    }

    pub fn advance_clock_ms(&self, ms: i64) {
        self.clock_offset_ms.fetch_add(ms, Ordering::Relaxed);
    }

    /// Persist grants at `path` (device-local) and load the live ones.
    pub fn set_store(&self, path: PathBuf) {
        let now = self.now_ms();
        let mut state = lock(&self.state);
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(saved) = serde_json::from_slice::<Vec<AdminGrant>>(&bytes)
        {
            for grant in saved.into_iter().filter(|g| g.is_live(now)) {
                state.grants.insert(grant.chat_id.clone(), grant);
            }
        }
        state.store = Some(path);
    }

    fn persist(state: &State) {
        if let Some(path) = &state.store {
            let list: Vec<&AdminGrant> = state.grants.values().collect();
            if let Ok(bytes) = serde_json::to_vec(&list) {
                let _ = std::fs::write(path, bytes);
            }
        }
    }

    /// Grant (or renew) admin for `chat_id` on `space_id`.
    pub fn grant(
        &self,
        chat_id: &str,
        space_id: &str,
        duration_secs: Option<u64>,
        skip_bulk_confirmation: bool,
    ) -> AdminGrant {
        let now = self.now_ms();
        let grant = AdminGrant {
            chat_id: chat_id.to_owned(),
            space_id: space_id.to_owned(),
            granted_at_ms: now,
            expires_at_ms: duration_secs
                .map(|s| now.saturating_add((s.min(365 * 24 * 3600) as i64) * 1000)),
            skip_bulk_confirmation,
        };
        let mut state = lock(&self.state);
        state.grants.insert(chat_id.to_owned(), grant.clone());
        Self::persist(&state);
        grant
    }

    /// The default grant length, for callers without a choice.
    pub fn default_duration_secs() -> u64 {
        ADMIN_DEFAULT_DURATION_SECS
    }

    pub fn revoke(&self, chat_id: &str) -> bool {
        let mut state = lock(&self.state);
        let had = state.grants.remove(chat_id).is_some();
        if had {
            Self::persist(&state);
        }
        had
    }

    /// The chat's live grant (an expired one is dropped here).
    pub fn live(&self, chat_id: &str) -> Option<AdminGrant> {
        let now = self.now_ms();
        let mut state = lock(&self.state);
        match state.grants.get(chat_id) {
            Some(g) if g.is_live(now) => Some(g.clone()),
            Some(_) => {
                state.grants.remove(chat_id);
                Self::persist(&state);
                None
            }
            None => None,
        }
    }

    pub fn list(&self) -> Vec<AdminGrant> {
        let now = self.now_ms();
        let mut state = lock(&self.state);
        let before = state.grants.len();
        state.grants.retain(|_, g| g.is_live(now));
        if state.grants.len() != before {
            Self::persist(&state);
        }
        let mut list: Vec<AdminGrant> = state.grants.values().cloned().collect();
        list.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        list
    }

    // ---- audit ---------------------------------------------------------------

    /// Append one action (and its inverse) to the turn `turn_id`, creating
    /// the turn on first use.
    pub fn record_action(
        &self,
        turn_id: &str,
        chat_id: &str,
        chat_title: &str,
        space_id: &str,
        kind: &str,
        summary: String,
        inverse: Option<WorkOutlineMutation>,
    ) {
        let now = self.now_ms();
        let mut state = lock(&self.state);
        let index = match state.audit.iter().position(|t| t.turn.turn_id == turn_id) {
            Some(i) => i,
            None => {
                state.audit.push(TurnRecord {
                    turn: AdminAuditTurn {
                        turn_id: turn_id.to_owned(),
                        chat_id: chat_id.to_owned(),
                        chat_title: chat_title.to_owned(),
                        space_id: space_id.to_owned(),
                        started_at_ms: now,
                        updated_at_ms: now,
                        label: attribution(chat_title),
                        actions: Vec::new(),
                        undone: false,
                    },
                    inverses: Vec::new(),
                });
                if state.audit.len() > MAX_AUDIT_TURNS {
                    state.audit.remove(0);
                }
                state.audit.len() - 1
            }
        };
        let record = &mut state.audit[index];
        record.turn.updated_at_ms = now;
        record.turn.actions.push(AdminAuditAction {
            at_ms: now,
            kind: kind.to_owned(),
            summary,
        });
        record.inverses.push(inverse);
    }

    pub fn turns(&self, space_id: Option<&str>) -> Vec<AdminAuditTurn> {
        lock(&self.state)
            .audit
            .iter()
            .filter(|t| space_id.is_none_or(|s| t.turn.space_id == s))
            .map(|t| t.turn.clone())
            .collect()
    }

    /// Mark a turn undone and hand back its inverses newest-first with their
    /// summaries. `None` if unknown or already undone.
    pub fn take_turn_for_undo(
        &self,
        turn_id: &str,
    ) -> Option<Vec<(String, Option<WorkOutlineMutation>)>> {
        let mut state = lock(&self.state);
        let record = state
            .audit
            .iter_mut()
            .find(|t| t.turn.turn_id == turn_id && !t.turn.undone)?;
        record.turn.undone = true;
        Some(
            record
                .turn
                .actions
                .iter()
                .zip(record.inverses.iter())
                .map(|(a, inv)| (a.summary.clone(), inv.clone()))
                .rev()
                .collect(),
        )
    }

    // ---- bulk approvals --------------------------------------------------------

    pub fn open_approval(&self, chat_id: &str, steps_hash: &str, request_id: &str) -> String {
        let token = format!(
            "bulk_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let mut state = lock(&self.state);
        state.approvals.insert(
            token.clone(),
            Approval {
                chat_id: chat_id.to_owned(),
                steps_hash: steps_hash.to_owned(),
                request_id: request_id.to_owned(),
                expires_at_ms: self.now_ms() + BULK_APPROVAL_TTL_MS,
                used: false,
            },
        );
        token
    }

    /// The Required Action id behind `token`, if the token is this chat's,
    /// for these exact steps, unexpired and unused.
    pub fn check_approval(
        &self,
        token: &str,
        chat_id: &str,
        steps_hash: &str,
    ) -> Result<String, String> {
        let state = lock(&self.state);
        let Some(a) = state.approvals.get(token) else {
            return Err("unknown confirmToken: request a new preview".into());
        };
        if a.chat_id != chat_id {
            return Err("that confirmToken belongs to another chat".into());
        }
        if a.used {
            return Err("that confirmToken was already used: request a new preview".into());
        }
        if self.now_ms() >= a.expires_at_ms {
            return Err("that confirmToken expired: request a new preview".into());
        }
        if a.steps_hash != steps_hash {
            return Err(
                "the steps differ from the ones the user was shown: request a new preview".into(),
            );
        }
        Ok(a.request_id.clone())
    }

    pub fn use_approval(&self, token: &str) {
        if let Some(a) = lock(&self.state).approvals.get_mut(token) {
            a.used = true;
        }
    }
}

/// "admin chat <title> (granted by Mikey)": how every admin action is
/// attributed, never as the user.
pub fn attribution(chat_title: &str) -> String {
    format!("by admin chat {chat_title} (granted by Mikey)")
}
