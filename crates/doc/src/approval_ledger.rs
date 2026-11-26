//! Device-local approval history. Serialized independently of the synced registry.

use std::collections::BTreeMap;

use mast_proto::{
    APPROVAL_LEDGER_RETENTION_DAYS, ApprovalLedgerEntry, LedgerDecider, LedgerDecision,
    MAX_APPROVAL_LEDGER_ROWS_PER_SPACE, derive_id,
};
use serde::{Deserialize, Serialize};

const DAY_MS: i64 = 86_400_000;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ApprovalLedger {
    rows: BTreeMap<String, ApprovalLedgerEntry>,
    #[serde(default)]
    command_denials: BTreeMap<String, CommandDenial>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CommandDenial {
    pub command_id: String,
    pub reason: String,
    pub expires_at: i64,
}

pub fn approval_ledger_row_id(chat_id: &str, request_id: &str) -> String {
    derive_id(&format!("{chat_id}\0{request_id}"), "approval-ledger")
}

impl ApprovalLedger {
    pub fn contains(&self, chat_id: &str, request_id: &str) -> bool {
        self.rows.contains_key(&approval_ledger_row_id(chat_id, request_id))
    }

    pub fn command_denial(&self, chat_id: &str, request_id: &str, now_ms: i64) -> Option<CommandDenial> {
        self.command_denials.get(&approval_ledger_row_id(chat_id, request_id))
            .filter(|denial| denial.expires_at > now_ms).cloned()
    }

    pub fn remember_command_denial(&mut self, row_id: String, denial: CommandDenial) {
        self.command_denials.insert(row_id, denial);
    }

    pub fn prune_command_denials(&mut self, now_ms: i64) -> bool {
        let before = self.command_denials.len();
        self.command_denials.retain(|_, denial| denial.expires_at > now_ms);
        self.command_denials.len() != before
    }

    pub fn list(&self, space_id: &str) -> Vec<ApprovalLedgerEntry> {
        let mut rows: Vec<_> = self.rows.values().filter(|row| row.space_id == space_id).cloned().collect();
        rows.sort_by(|a, b| b.requested_at.cmp(&a.requested_at).then_with(|| b.id.cmp(&a.id)));
        rows
    }

    /// Insert a request once; only a pending request can take an outcome.
    pub fn write(&mut self, row: ApprovalLedgerEntry) -> bool {
        use std::collections::btree_map::Entry;
        match self.rows.entry(row.id.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(row);
                true
            }
            Entry::Occupied(mut slot) if slot.get().decision == LedgerDecision::Pending
                && row.decision != LedgerDecision::Pending => {
                let pending = slot.get_mut();
                pending.decision = row.decision;
                pending.decider = row.decider;
                pending.decided_at = row.decided_at;
                pending.rule_id = row.rule_id;
                true
            }
            _ => false,
        }
    }

    pub fn cancel(&mut self, chat_id: &str, request_id: &str, now_ms: i64) -> bool {
        self.rows.get_mut(&approval_ledger_row_id(chat_id, request_id))
            .is_some_and(|row| Self::cancel_pending(row, now_ms))
    }

    /// Call once on startup, before any harness can make a request.
    pub fn recover(&mut self, now_ms: i64) -> bool {
        let mut changed = self.prune_command_denials(now_ms);
        for row in self.rows.values_mut() {
            changed |= Self::cancel_pending(row, now_ms);
        }
        let spaces: std::collections::BTreeSet<_> = self.rows.values().map(|row| row.space_id.clone()).collect();
        for space in spaces {
            changed |= self.prune(&space, now_ms);
        }
        changed
    }

    fn cancel_pending(row: &mut ApprovalLedgerEntry, now_ms: i64) -> bool {
        if row.decision != LedgerDecision::Pending {
            return false;
        }
        row.decision = LedgerDecision::Cancelled;
        row.decider = Some(LedgerDecider::Teardown);
        row.decided_at = Some(now_ms);
        true
    }

    /// Expire settled rows by age, then keep the newest settled rows per Space.
    pub fn prune(&mut self, space_id: &str, now_ms: i64) -> bool {
        let cutoff = now_ms.saturating_sub(APPROVAL_LEDGER_RETENTION_DAYS * DAY_MS);
        let before = self.rows.len();
        self.rows.retain(|_, row| {
            row.space_id != space_id || row.decision == LedgerDecision::Pending || row.requested_at >= cutoff
        });
        let mut settled: Vec<_> = self.rows.values()
            .filter(|row| row.space_id == space_id && row.decision != LedgerDecision::Pending)
            .map(|row| (row.requested_at, row.id.as_str())).collect();
        let excess = settled.len().saturating_sub(MAX_APPROVAL_LEDGER_ROWS_PER_SPACE);
        if excess != 0 {
            settled.sort_unstable();
            let expired: Vec<_> = settled.into_iter().take(excess).map(|(_, id)| id.to_owned()).collect();
            for id in expired {
                self.rows.remove(&id);
            }
        }
        self.rows.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::LedgerSource;

    fn row(space: &str, request: &str, at: i64, decision: LedgerDecision) -> ApprovalLedgerEntry {
        ApprovalLedgerEntry {
            id: approval_ledger_row_id(space, request), space_id: space.into(), chat_id: space.into(),
            request_id: request.into(), work_item_id: None, harness: "codex".into(),
            model: None, command: "cargo test".into(), redacted: false, cwd: None, source: LedgerSource::Prompt,
            requested_at: at, decided_at: None, decision, decider: None, rule_id: None,
        }
    }

    #[test]
    fn only_the_first_answer_wins_even_after_reopening() {
        let mut ledger = ApprovalLedger::default();
        let pending = row("space", "request", 10, LedgerDecision::Pending);
        assert!(ledger.write(pending.clone()));
        assert!(!ledger.write(pending));
        assert!(ledger.write(row("space", "request", 20, LedgerDecision::Denied)));
        let bytes = serde_json::to_vec(&ledger).unwrap();
        let mut reopened: ApprovalLedger = serde_json::from_slice(&bytes).unwrap();
        assert!(!reopened.write(row("space", "request", 25, LedgerDecision::Allowed)));
        let rows = reopened.list("space");
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].requested_at, rows[0].decision), (10, LedgerDecision::Denied));
    }

    #[test]
    fn retention_is_per_space_and_expired_keys_can_be_reused() {
        let mut ledger = ApprovalLedger::default();
        let now = 200 * DAY_MS;
        ledger.write(row("busy", "old", 1, LedgerDecision::Denied));
        ledger.remember_command_denial(approval_ledger_row_id("busy", "old"), CommandDenial {
            command_id: "command".into(), reason: "denied".into(), expires_at: now + DAY_MS,
        });
        ledger.write(row("busy", "active", 1, LedgerDecision::Pending));
        ledger.write(row("quiet", "recent", now, LedgerDecision::Allowed));
        for index in 0..=MAX_APPROVAL_LEDGER_ROWS_PER_SPACE {
            ledger.write(row("busy", &index.to_string(), now + index as i64, LedgerDecision::Allowed));
        }
        assert!(ledger.prune("busy", now));
        assert_eq!(ledger.list("busy").len(), MAX_APPROVAL_LEDGER_ROWS_PER_SPACE + 1);
        assert_eq!(ledger.list("quiet").len(), 1);
        assert!(ledger.contains("busy", "active"));
        assert!(!ledger.contains("busy", "old"));
        assert_eq!(ledger.command_denial("busy", "old", now).unwrap().reason, "denied");
        assert!(!ledger.contains("busy", "old"), "looking up a denial cannot resurrect history");
        assert!(ledger.prune_command_denials(now + DAY_MS));
        assert!(ledger.command_denial("busy", "old", now + DAY_MS).is_none());
        assert!(ledger.write(row("busy", "old", now, LedgerDecision::Allowed)));
    }

    #[test]
    fn startup_cancels_open_requests_once() {
        let mut ledger = ApprovalLedger::default();
        ledger.write(row("space", "open", 10, LedgerDecision::Pending));
        assert!(ledger.recover(20));
        assert!(!ledger.recover(30));
        let rows = ledger.list("space");
        assert_eq!(rows[0].decided_at, Some(20));
        assert_eq!(rows[0].decider, Some(LedgerDecider::Teardown));
        assert_eq!(rows[0].decision, LedgerDecision::Cancelled);
    }
}
