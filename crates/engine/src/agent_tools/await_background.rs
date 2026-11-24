//! `await_background`: a worker's declaration that it is waiting on work the
//! engine cannot see (a harness background build or subagent). It only mutes
//! auto-continue: the next turn of the chat, from a wake or any message,
//! clears it, and it lapses after at most an hour.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{AgentToolError, AgentToolsService, CallContext, lock, parse_args};

/// The longest any mute lasts, for a declaration and for a running command.
pub(super) const MAX_MINUTES: u64 = 60;

#[derive(Debug, Clone, Copy)]
struct Declaration {
    /// When the worker declared it (wall-clock milliseconds).
    at_ms: i64,
    /// When it lapses.
    until_ms: i64,
}

/// Chat id to its declaration. Lapsed ones are dropped whenever anyone
/// declares, so the map never grows beyond the declarations of the last hour.
/// Reading never removes anything, so a read at an odd time cannot lose a
/// live declaration.
#[derive(Default)]
struct Declarations(HashMap<String, Declaration>);

impl Declarations {
    fn declare(&mut self, chat_id: &str, now_ms: i64, max_minutes: u64) -> u64 {
        let minutes = max_minutes.clamp(1, MAX_MINUTES);
        self.0.retain(|_, declaration| declaration.until_ms > now_ms);
        self.0.insert(
            chat_id.to_owned(),
            Declaration { at_ms: now_ms, until_ms: now_ms + (minutes as i64) * 60_000 },
        );
        minutes
    }

    fn is_awaiting(&self, chat_id: &str, now_ms: i64) -> bool {
        self.0.get(chat_id).is_some_and(|declaration| declaration.until_ms > now_ms)
    }

    fn clear(&mut self, chat_id: &str, turn_started_ms: Option<i64>) {
        let made_this_turn = self
            .0
            .get(chat_id)
            .is_some_and(|declaration| turn_started_ms.is_some_and(|started| declaration.at_ms >= started));
        if !made_this_turn {
            self.0.remove(chat_id);
        }
    }
}

fn declared() -> &'static Mutex<Declarations> {
    static DECLARED: OnceLock<Mutex<Declarations>> = OnceLock::new();
    DECLARED.get_or_init(Default::default)
}

/// Records that `chat_id` waits on background work for at most `max_minutes`
/// (clamped to 1-60) from `now_ms`, and returns the minutes granted.
pub fn declare_awaiting_background(chat_id: &str, now_ms: i64, max_minutes: u64) -> u64 {
    lock(declared()).declare(chat_id, now_ms, max_minutes)
}

pub fn is_awaiting_background(chat_id: &str, now_ms: i64) -> bool {
    lock(declared()).is_awaiting(chat_id, now_ms)
}

/// A new turn of the chat, which began at `turn_started_ms`, ends the
/// declarations made before it. The engine learns of a turn start after the
/// fact, so a declaration the worker already made during this very turn (at or
/// after its start) is kept. Without a start time every declaration ends.
pub fn clear_awaiting_background(chat_id: &str, turn_started_ms: Option<i64>) {
    lock(declared()).clear(chat_id, turn_started_ms);
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Args {
    reason: String,
    max_minutes: u64,
}

impl AgentToolsService {
    pub(super) fn await_background(&self, context: &CallContext, arguments: &Value) -> Result<Value, AgentToolError> {
        let args: Args = parse_args(arguments)?;
        if args.reason.trim().is_empty() {
            return Err(AgentToolError::invalid("reason must name what you are waiting on"));
        }
        let minutes = declare_awaiting_background(
            &context.grant.chat_id,
            chrono::Utc::now().timestamp_millis(),
            args.max_minutes,
        );
        Ok(json!({"awaiting": true, "maxMinutes": minutes}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_lapses_and_clears() {
        let mut map = Declarations::default();
        map.declare("a", 0, 1);
        assert!(map.is_awaiting("a", 59_999));
        assert!(!map.is_awaiting("a", 60_000));
        assert!(!map.is_awaiting("other", 0));
        map.declare("a", 0, 1);
        map.clear("a", None);
        assert!(!map.is_awaiting("a", 0));
    }

    #[test]
    fn minutes_are_clamped_to_an_hour() {
        let mut map = Declarations::default();
        assert_eq!(map.declare("a", 0, 500), 60);
        assert!(map.is_awaiting("a", 59 * 60_000));
        assert!(!map.is_awaiting("a", 60 * 60_000));
        assert_eq!(map.declare("a", 0, 0), 1);
    }

    #[test]
    fn a_turn_start_keeps_only_a_declaration_made_during_that_turn() {
        let mut map = Declarations::default();
        map.declare("a", 1_000, 5);
        map.clear("a", Some(1_001));
        assert!(!map.is_awaiting("a", 1_002), "made before the turn began");
        map.declare("a", 1_000, 5);
        map.clear("a", Some(1_000));
        assert!(map.is_awaiting("a", 1_002), "made at the turn's start");
        map.clear("a", Some(900));
        assert!(map.is_awaiting("a", 1_002), "made after the turn began");
        map.clear("a", None);
        assert!(!map.is_awaiting("a", 1_002), "no start time ends it");
    }

    #[test]
    fn lapsed_declarations_are_pruned() {
        let mut map = Declarations::default();
        for n in 0..5 {
            map.declare(&format!("chat-{n}"), 0, 1);
        }
        assert_eq!(map.0.len(), 5);
        // Reading a lapsed entry reports it lapsed and removes nothing.
        assert!(!map.is_awaiting("chat-0", 60_000));
        assert_eq!(map.0.len(), 5);
        // Declaring for any chat drops every lapsed entry of the others.
        map.declare("live", 120_000, 1);
        assert_eq!(map.0.len(), 1);
        assert!(map.0.contains_key("live"));
    }
}
