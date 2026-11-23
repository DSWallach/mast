//! `await_background`: a worker's declaration that it is waiting on work the
//! engine cannot see (a harness background build or subagent). It only mutes
//! auto-continue: the next turn of the chat, from a wake or any message,
//! clears it, and it lapses after at most an hour.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{AgentToolError, AgentToolsService, CallContext, lock, parse_args};

const MAX_MINUTES: u64 = 60;

/// Chat id to the wall-clock millisecond its declaration lapses.
fn declared() -> &'static Mutex<HashMap<String, i64>> {
    static DECLARED: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
    DECLARED.get_or_init(Default::default)
}

pub fn is_awaiting_background(chat_id: &str, now_ms: i64) -> bool {
    lock(declared()).get(chat_id).is_some_and(|until| *until > now_ms)
}

/// A new turn of the chat ends its declaration.
pub fn clear_awaiting_background(chat_id: &str) {
    lock(declared()).remove(chat_id);
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
        let minutes = args.max_minutes.clamp(1, MAX_MINUTES);
        let until = chrono::Utc::now().timestamp_millis() + (minutes as i64) * 60_000;
        lock(declared()).insert(context.grant.chat_id.clone(), until);
        Ok(json!({"awaiting": true, "maxMinutes": minutes}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_lapses_and_clears() {
        lock(declared()).insert("chat-a".into(), 1_000);
        assert!(is_awaiting_background("chat-a", 999));
        assert!(!is_awaiting_background("chat-a", 1_000));
        assert!(!is_awaiting_background("other", 0));
        lock(declared()).insert("chat-a".into(), 5_000);
        clear_awaiting_background("chat-a");
        assert!(!is_awaiting_background("chat-a", 0));
    }
}
