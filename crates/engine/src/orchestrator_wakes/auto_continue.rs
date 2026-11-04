//! Eligibility and consecutive continuation accounting for worker turns.

use std::collections::HashMap;

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoContinue {
    workers: HashMap<String, HashMap<String, WorkerState>>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct WorkerState {
    last_message_id: Option<String>,
    count: u8,
    reported: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    None,
    Continue(u8),
    ReportStopped,
}

pub struct Turn<'a> {
    pub worker_id: &'a str,
    pub item_id: &'a str,
    /// Most recent user or orchestrator message, excluding Mast messages.
    pub last_message_id: Option<&'a str>,
    pub enabled: bool,
    pub item_open: bool,
    pub action_open: bool,
    pub completed: bool,
    pub awaiting_input: bool,
    pub tool_failures: bool,
    pub user_sent_during_turn: bool,
    pub queue_nonempty: bool,
}

impl AutoContinue {
    pub fn decide(&mut self, turn: Turn<'_>) -> Decision {
        let worker = self
            .workers
            .entry(turn.worker_id.to_owned())
            .or_default()
            .entry(turn.item_id.to_owned())
            .or_insert_with(|| WorkerState {
                last_message_id: turn.last_message_id.map(str::to_owned),
                count: 0,
                reported: false,
            });
        if worker.last_message_id.as_deref() != turn.last_message_id {
            worker.last_message_id = turn.last_message_id.map(str::to_owned);
            worker.count = 0;
            worker.reported = false;
        }
        if !turn.enabled
            || !turn.item_open
            || turn.action_open
            || !turn.completed
            || turn.awaiting_input
            || turn.tool_failures
            || turn.user_sent_during_turn
            || turn.queue_nonempty
        {
            return Decision::None;
        }
        if worker.count < 3 {
            worker.count += 1;
            Decision::Continue(worker.count)
        } else if !worker.reported {
            worker.reported = true;
            Decision::ReportStopped
        } else {
            Decision::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn() -> Turn<'static> {
        Turn {
            worker_id: "worker",
            item_id: "item",
            last_message_id: Some("message"),
            enabled: true,
            item_open: true,
            action_open: false,
            completed: true,
            awaiting_input: false,
            tool_failures: false,
            user_sent_during_turn: false,
            queue_nonempty: false,
        }
    }

    #[test]
    fn auto_continue_caps_and_resets_on_message_or_item() {
        let mut state = AutoContinue::default();
        for n in 1..=3 {
            assert_eq!(state.decide(turn()), Decision::Continue(n));
        }
        assert_eq!(state.decide(turn()), Decision::ReportStopped);
        assert_eq!(state.decide(turn()), Decision::None);
        assert_eq!(
            state.decide(Turn {
                last_message_id: Some("reply"),
                ..turn()
            }),
            Decision::Continue(1)
        );
        assert_eq!(
            state.decide(Turn {
                item_id: "other",
                ..turn()
            }),
            Decision::Continue(1)
        );
    }

    #[test]
    fn auto_continue_retains_each_items_limit_across_restart() {
        let mut state = AutoContinue::default();
        for n in 1..=3 {
            assert_eq!(state.decide(turn()), Decision::Continue(n));
        }
        let saved = serde_json::to_vec(&state).unwrap();
        let mut state: AutoContinue = serde_json::from_slice(&saved).unwrap();
        assert_eq!(
            state.decide(Turn {
                item_id: "other",
                ..turn()
            }),
            Decision::Continue(1)
        );
        assert_eq!(state.decide(turn()), Decision::ReportStopped);
    }

    #[test]
    fn auto_continue_requires_an_unblocked_completed_turn() {
        let mut state = AutoContinue::default();
        for t in [
            Turn {
                enabled: false,
                ..turn()
            },
            Turn {
                item_open: false,
                ..turn()
            },
            Turn {
                action_open: true,
                ..turn()
            },
            Turn {
                completed: false,
                ..turn()
            },
            Turn {
                awaiting_input: true,
                ..turn()
            },
            Turn {
                tool_failures: true,
                ..turn()
            },
            Turn {
                user_sent_during_turn: true,
                ..turn()
            },
            Turn {
                queue_nonempty: true,
                ..turn()
            },
        ] {
            assert_eq!(state.decide(t), Decision::None);
        }
        assert_eq!(state.decide(turn()), Decision::Continue(1));
    }
}
