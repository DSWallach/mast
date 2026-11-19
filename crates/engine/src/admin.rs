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
use std::sync::{Arc, Mutex};
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
    /// The execution (chat, idempotency key, raw steps) this approval is for.
    exec_key: String,
    /// The resolved preview the user was shown (targets and effect counts).
    preview_hash: String,
    request_id: String,
    /// What the user was shown: the batch's effect count at request time.
    weight: usize,
    expires_at_ms: i64,
    used: bool,
    /// The one execution that has claimed it; its retries may resume.
    claimed_by: Option<String>,
}

/// One batch's durable record: what it may spend, what it has spent, the
/// per-step plan the user approved (the digest each step must still match when
/// it is written, kept across partial runs and restarts), and whether it has
/// finished. Written ahead to disk: a step's charge is reserved before the
/// registry write and refunded if that write fails, so a crash can only
/// over-count, never let a retry spend twice.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct BatchRecord {
    total: usize,
    spent: usize,
    plan: Vec<String>,
    done: bool,
}

#[derive(Default)]
struct BudgetBook {
    entries: HashMap<String, BatchRecord>,
    order: std::collections::VecDeque<String>,
    path: Option<PathBuf>,
    /// The saved file exists but could not be read: nothing it held can be
    /// trusted, so no batch may be admitted or spend until it is repaired.
    broken: bool,
}

impl BudgetBook {
    /// Write the book durably: temp file, flushed to disk, renamed over the
    /// old one, then the directory flushed, so neither a crash nor a power
    /// loss leaves a charge missing while its registry write survives. `Ok`
    /// without a store: the in-memory book is all there is.
    fn persist(&self) -> std::io::Result<()> {
        use std::io::Write;
        let Some(path) = &self.path else { return Ok(()) };
        let rows: Vec<(&String, &BatchRecord)> = self.entries.iter().collect();
        let bytes = serde_json::to_vec(&rows).map_err(std::io::Error::other)?;
        let temp = path.with_extension("tmp");
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        if let Some(dir) = path.parent() {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }

    /// Drop the oldest finished batches beyond the cap. An unfinished batch is
    /// never dropped, so its record cannot vanish between a check and its use.
    fn trim_finished(&mut self) {
        while self.order.len() > MAX_FINISHED_BATCHES {
            let Some(index) = self.order.iter().position(|key| self.entries.get(key).is_some_and(|r| r.done)) else {
                break;
            };
            if let Some(key) = self.order.remove(index) {
                self.entries.remove(&key);
            }
        }
    }
}

/// The effect budget of one `work_outline_admin` batch (a manager's call and
/// its retries share one). The registry writer reserves each step's charge
/// under its own lock, so cumulative effects across steps and retries can
/// never exceed what was approved.
#[derive(Debug)]
pub struct BatchBudget {
    key: String,
    book: Arc<Mutex<BudgetBook>>,
}

impl std::fmt::Debug for BudgetBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BudgetBook")
    }
}

impl BatchBudget {
    /// A budget of its own (tests, one-off use); batches use
    /// [`AdminGrants::admit_batch`].
    pub fn new(total: usize) -> Arc<Self> {
        let book = Arc::new(Mutex::new(BudgetBook::default()));
        lock(&book).entries.insert(String::new(), BatchRecord { total, ..Default::default() });
        Arc::new(Self { key: String::new(), book })
    }

    pub fn remaining(&self) -> usize {
        lock(&self.book).entries.get(&self.key).map_or(0, |r| r.total.saturating_sub(r.spent))
    }

    /// The per-step plan the user approved (empty if none was recorded).
    pub fn plan(&self) -> Vec<String> {
        lock(&self.book).entries.get(&self.key).map(|r| r.plan.clone()).unwrap_or_default()
    }

    /// Take `n` if that much is left. Atomic, and written to disk (flushed)
    /// before it returns: if the store cannot be written (or was unreadable)
    /// it fails closed and takes nothing, so a registry write can never
    /// outlive its charge.
    pub fn reserve(&self, n: usize) -> bool {
        let mut book = lock(&self.book);
        if book.broken {
            return false;
        }
        let Some(record) = book.entries.get_mut(&self.key) else { return false };
        if n > record.total.saturating_sub(record.spent) {
            return false;
        }
        record.spent += n;
        if book.persist().is_err() {
            if let Some(record) = book.entries.get_mut(&self.key) {
                record.spent = record.spent.saturating_sub(n);
            }
            return false;
        }
        true
    }

    /// Give back a reservation whose write did not commit. A failed save
    /// only leaves the stored count higher, which is the safe direction.
    pub fn refund(&self, n: usize) {
        let mut book = lock(&self.book);
        if let Some(record) = book.entries.get_mut(&self.key) {
            record.spent = record.spent.saturating_sub(n);
        }
        let _ = book.persist();
    }

    /// A fresh approval for `allowance` more items over `plan`: the total
    /// becomes what is already spent plus that, so earlier committed steps
    /// stay counted, and the approved plan is replaced. All or nothing: if it
    /// cannot be saved, nothing changes and the claim must not go ahead.
    pub fn approve(&self, allowance: usize, plan: &[String]) -> Result<(), String> {
        let mut book = lock(&self.book);
        if book.broken {
            return Err("the batch budget store is unreadable".into());
        }
        let Some(record) = book.entries.get_mut(&self.key) else {
            return Err("no budget is recorded for this batch".into());
        };
        let before = record.clone();
        record.total = record.spent + allowance;
        record.plan = plan.to_vec();
        if book.persist().is_err() {
            if let Some(record) = book.entries.get_mut(&self.key) {
                *record = before;
            }
            return Err("the batch budget could not be saved".into());
        }
        Ok(())
    }

    /// The batch ran to completion: its record may be dropped when old.
    pub fn finish(&self) {
        let mut book = lock(&self.book);
        if let Some(record) = book.entries.get_mut(&self.key) {
            record.done = true;
        }
        book.trim_finished();
        let _ = book.persist();
    }
}

/// Finished batch records kept (oldest dropped first); unfinished ones never.
const MAX_FINISHED_BATCHES: usize = 512;
/// Most records, finished or not: past it new batches are refused rather than
/// forgetting one that has committed steps.
const MAX_BATCH_RECORDS: usize = 4096;

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
    budgets: Arc<Mutex<BudgetBook>>,
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
        let budget_path = path.with_file_name("admin-batch-budgets.json");
        {
            let mut book = lock(&self.budgets);
            match std::fs::read(&budget_path) {
                Ok(bytes) => match serde_json::from_slice::<Vec<(String, BatchRecord)>>(&bytes) {
                    Ok(saved) => {
                        for (key, record) in saved {
                            if book.entries.insert(key.clone(), record).is_none() {
                                book.order.push_back(key);
                            }
                        }
                    }
                    Err(_) => book.broken = true,
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => book.broken = true,
            }
            book.path = Some(budget_path);
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
        label: &str,
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
                        label: label.to_owned(),
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

    // ---- batch budgets ---------------------------------------------------------

    /// Admit batch `key` (the chat, its idempotency key and the steps): one
    /// critical section that finds its durable record or creates it with the
    /// default `total` and the approved `plan`. A batch that already committed
    /// steps (`resumed`) but has no record cannot prove what it spent, so it
    /// is refused rather than started over; so is a store that is unreadable,
    /// full of unfinished batches, or cannot save the new record.
    pub fn admit_batch(
        &self,
        key: &str,
        total: usize,
        plan: &[String],
        resumed: bool,
    ) -> Result<Arc<BatchBudget>, String> {
        let mut book = lock(&self.budgets);
        if book.broken {
            return Err("the batch budget store is unreadable: nothing can be admitted until it is repaired".into());
        }
        if !book.entries.contains_key(key) {
            if resumed {
                return Err(
                    "this batch already committed some steps but its budget record is gone, so what it \
                     spent cannot be proven: start it again under a new idempotencyKey"
                        .into(),
                );
            }
            if book.entries.len() >= MAX_BATCH_RECORDS {
                return Err("too many unfinished batches are recorded: finish or retire some first".into());
            }
            book.entries.insert(key.to_owned(), BatchRecord { total, plan: plan.to_vec(), ..Default::default() });
            book.order.push_back(key.to_owned());
            book.trim_finished();
            if book.persist().is_err() {
                book.entries.remove(key);
                book.order.retain(|k| k != key);
                return Err("the batch budget could not be saved".into());
            }
        }
        drop(book);
        Ok(Arc::new(BatchBudget { key: key.to_owned(), book: self.budgets.clone() }))
    }

    /// [`Self::admit_batch`] with no plan and nothing committed (tests).
    pub fn batch_budget(&self, key: &str, total: usize) -> Arc<BatchBudget> {
        self.admit_batch(key, total, &[], false).expect("a batch budget")
    }

    /// What batch `key` may still spend and the plan it was approved with, or
    /// `None` if no record exists for it (new). Read-only.
    pub fn batch_state(&self, key: &str) -> Option<(usize, Vec<String>)> {
        lock(&self.budgets)
            .entries
            .get(key)
            .map(|r| (r.total.saturating_sub(r.spent), r.plan.clone()))
    }

    /// What batch `key` may still spend (see [`Self::batch_state`]).
    pub fn batch_remaining(&self, key: &str) -> Option<usize> {
        self.batch_state(key).map(|(remaining, _)| remaining)
    }

    // ---- bulk approvals --------------------------------------------------------

    pub fn open_approval(&self, chat_id: &str, exec_key: &str, preview_hash: &str, request_id: &str, weight: usize) -> String {
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
                exec_key: exec_key.to_owned(),
                preview_hash: preview_hash.to_owned(),
                request_id: request_id.to_owned(),
                weight,
                expires_at_ms: self.now_ms() + BULK_APPROVAL_TTL_MS,
                used: false,
                claimed_by: None,
            },
        );
        token
    }

    /// What `token` would give the execution `exec_key` for the resolved
    /// preview `preview_hash`, without changing anything: the Required Action
    /// id, the count the user was shown and whether the claim would be fresh.
    /// The first claim must match the preview; a retry of the claiming
    /// execution resumes it (its own earlier steps moved the preview); any
    /// other chat, execution or preview is refused.
    fn evaluate(
        approval: &Approval,
        now: i64,
        chat_id: &str,
        exec_key: &str,
        preview_hash: &str,
    ) -> Result<(String, usize, bool), String> {
        if approval.chat_id != chat_id {
            return Err("that confirmToken belongs to another chat".into());
        }
        if now >= approval.expires_at_ms {
            return Err("that confirmToken expired: request a new preview".into());
        }
        if approval.exec_key != exec_key {
            return Err("that confirmToken is for another batch: request a new preview".into());
        }
        match &approval.claimed_by {
            Some(claimed) if claimed != exec_key => {
                Err("that confirmToken was claimed by another batch".into())
            }
            Some(_) => Ok((approval.request_id.clone(), approval.weight, false)),
            None => {
                if approval.used {
                    return Err("that confirmToken was already used: request a new preview".into());
                }
                if approval.preview_hash != preview_hash {
                    return Err(
                        "the preview changed since the user was shown it: request a new preview".into(),
                    );
                }
                Ok((approval.request_id.clone(), approval.weight, true))
            }
        }
    }

    /// [`Self::evaluate`] for a confirmation that may still be refused or only
    /// previewed (a dry run, an approval the user has not given): it claims
    /// nothing.
    pub fn peek_approval(
        &self,
        token: &str,
        chat_id: &str,
        exec_key: &str,
        preview_hash: &str,
    ) -> Result<(String, usize, bool), String> {
        let state = lock(&self.state);
        let Some(approval) = state.approvals.get(token) else {
            return Err("unknown confirmToken: request a new preview".into());
        };
        Self::evaluate(approval, self.now_ms(), chat_id, exec_key, preview_hash)
    }

    /// Claim `token` for the execution `exec_key` as it starts to run. One
    /// critical section: the claim and, for a fresh one, the batch's allowance
    /// (what is left to do on top of what is already spent) are installed
    /// together, so no other execution can slip in between and no claim exists
    /// without its allowance.
    pub fn claim_approval(
        &self,
        token: &str,
        chat_id: &str,
        exec_key: &str,
        preview_hash: &str,
        budget: Option<&BatchBudget>,
        plan: &[String],
    ) -> Result<(String, usize, bool), String> {
        let mut state = lock(&self.state);
        let now = self.now_ms();
        let Some(approval) = state.approvals.get_mut(token) else {
            return Err("unknown confirmToken: request a new preview".into());
        };
        let (request_id, weight, fresh) = Self::evaluate(approval, now, chat_id, exec_key, preview_hash)?;
        if fresh {
            // The allowance and the approved plan are saved first: if that
            // fails the token stays unclaimed and nothing runs.
            if let Some(budget) = budget {
                budget.approve(weight, plan)?;
            }
            approval.claimed_by = Some(exec_key.to_owned());
        }
        Ok((request_id, weight, fresh))
    }

    /// The claimed execution finished: the token authorizes nothing new.
    pub fn complete_approval(&self, token: &str) {
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

/// "project manager <title>": how a manager's outline changes are
/// attributed. It is an agent acting on standing authority, never the user.
pub fn manager_attribution(chat_title: &str) -> String {
    format!("by project manager {chat_title}")
}
