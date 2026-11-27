//! Pure scheduling: which queued jobs start on which slots now.
//!
//! Rules (C3, reconciled in the unified plan):
//!
//! - **Round-robin across worktrees, FIFO inside one.** The next job is the oldest head of any
//!   worktree's queue, except that the worktree that ran last, and any worktree that already has a
//!   running job, go behind every other waiting worktree. One chat flooding the queue cannot
//!   starve the others.
//! - **One job per slot**, at most `max_runners` running jobs per host. An integration job runs
//!   only on the incremental slot (`a`); a worker job prefers the non-incremental slots and may
//!   take `a` only while no integration job is waiting.
//! - **Disk gates** on the host's free space: a job starts only at `start_free_gib` (50, or the
//!   job's project override such as 30); below `hold_free_gib` (15) nothing starts; when free space
//!   is unknown nothing starts.
//! - **Quiet-window lease**: while a lease exists no job starts, except jobs carrying the lease id
//!   once it is granted (granted = the host drained).

use super::HostProfile;
use std::collections::HashSet;

/// A queued job as the scheduler sees it. The slice passed to [`plan`] is ordered by `seq`.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub seq: i64,
    pub worktree_key: String,
    pub integration: bool,
    pub start_free_gib: Option<u64>,
    pub lease_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuietWindowView {
    pub lease_id: String,
    pub granted: bool,
}

/// Why queued jobs are waiting although the host has not started them.
#[derive(Debug, Clone, PartialEq)]
pub enum Hold {
    QuietWindow,
    RunnerCap,
    NoFreeSlot,
    DiskBelowStart { free_gib: f64, need_gib: u64 },
    DiskBelowHold { free_gib: f64 },
    DiskUnknown(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// `(job id, slot name)` in start order.
    pub starts: Vec<(String, String)>,
    pub held: Option<Hold>,
}

/// Decide the starts for one host. `busy` is `(worktree_key, slot)` of every running job.
pub fn plan(
    profile: &HostProfile,
    candidates: &[Candidate],
    busy: &[(String, String)],
    last_ran: Option<&str>,
    free_gib: Result<f64, String>,
    window: Option<&QuietWindowView>,
) -> Plan {
    let mut plan = Plan::default();
    let mut remaining: Vec<&Candidate> = candidates.iter().collect();
    if let Some(window) = window {
        // Lease jobs run only once the host has drained; everything else holds.
        remaining
            .retain(|c| window.granted && c.lease_id.as_deref() == Some(window.lease_id.as_str()));
        if remaining.is_empty() {
            plan.held = Some(Hold::QuietWindow);
            return plan;
        }
    }
    let free = match free_gib {
        Ok(free) => free,
        Err(error) => {
            plan.held = Some(Hold::DiskUnknown(error));
            return plan;
        }
    };
    if free < profile.disk.hold_free_gib as f64 {
        plan.held = Some(Hold::DiskBelowHold { free_gib: free });
        return plan;
    }
    let mut taken: HashSet<String> = busy.iter().map(|(_, slot)| slot.clone()).collect();
    let mut running_keys: HashSet<String> = busy.iter().map(|(key, _)| key.clone()).collect();
    let mut last = last_ran.map(str::to_owned);
    let mut running = busy.len();
    loop {
        if remaining.is_empty() {
            break;
        }
        if running >= profile.max_runners {
            plan.held = Some(Hold::RunnerCap);
            break;
        }
        // Head of each worktree's FIFO.
        let mut heads: Vec<&Candidate> = Vec::new();
        for candidate in &remaining {
            if !heads
                .iter()
                .any(|head| head.worktree_key == candidate.worktree_key)
            {
                heads.push(candidate);
            }
        }
        // Everyone else first; the worktree that ran last or is running goes behind.
        heads.sort_by_key(|head| {
            let behind = last.as_deref() == Some(head.worktree_key.as_str())
                || running_keys.contains(&head.worktree_key);
            (behind, head.seq)
        });
        let integration_waiting = remaining.iter().any(|c| c.integration);
        let mut chosen: Option<(&Candidate, String)> = None;
        let mut hold = None;
        for head in heads {
            let need = head.start_free_gib.unwrap_or(profile.disk.start_free_gib);
            if free < need as f64 {
                hold.get_or_insert(Hold::DiskBelowStart {
                    free_gib: free,
                    need_gib: need,
                });
                continue;
            }
            match choose_slot(profile, head, &taken, integration_waiting) {
                Some(slot) => {
                    chosen = Some((head, slot));
                    break;
                }
                None => {
                    hold.get_or_insert(Hold::NoFreeSlot);
                }
            }
        }
        let Some((job, slot)) = chosen else {
            plan.held = hold;
            break;
        };
        plan.starts.push((job.id.clone(), slot.clone()));
        taken.insert(slot);
        running_keys.insert(job.worktree_key.clone());
        last = Some(job.worktree_key.clone());
        running += 1;
        let id = job.id.clone();
        remaining.retain(|c| c.id != id);
    }
    plan
}

fn choose_slot(
    profile: &HostProfile,
    job: &Candidate,
    taken: &HashSet<String>,
    integration_waiting: bool,
) -> Option<String> {
    let free = |slot: &&super::SlotConfig| !taken.contains(&slot.name);
    if job.integration {
        if !profile.slots.iter().any(|slot| slot.incremental) {
            return profile
                .slots
                .iter()
                .find(|slot| !taken.contains(&slot.name))
                .map(|slot| slot.name.clone());
        }
        return profile
            .slots
            .iter()
            .filter(free)
            .find(|slot| slot.incremental)
            .map(|slot| slot.name.clone());
    }
    let worker = profile
        .slots
        .iter()
        .filter(free)
        .find(|slot| !slot.incremental);
    let fallback = || {
        // The integration slot is lent to a worker only while no integration job waits for it.
        (!integration_waiting)
            .then(|| {
                profile
                    .slots
                    .iter()
                    .filter(free)
                    .find(|slot| slot.incremental)
            })
            .flatten()
    };
    worker.or_else(fallback).map(|slot| slot.name.clone())
}
