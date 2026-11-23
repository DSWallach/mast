//! Tool calls that have been open for a long time with no result.
//!
//! A chat can show "working" for as long as one of its tool calls never
//! returns (a wait loop on a host that stopped answering), and its turn
//! looks alive to every idle check. The run loop records when each call
//! opens and closes; the orchestrator-wake tick asks for calls that have been
//! quiet longer than [`LONG_TOOL_CALL_MS`] and tells the Manager once per call.
//! In memory only: a restart ends the turns it describes.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use mast_proto::ToolCall;

/// How long one call may stay open with no result before it is flagged. Longer
/// than the 15-minute idle check, so a flag is never about a normal pause.
pub const LONG_TOOL_CALL_MS: i64 = 20 * 60_000;
/// Open calls remembered per chat; a turn never has more than a handful at once.
const MAX_CALLS_PER_CHAT: usize = 64;
/// Finished call ids remembered per chat, so a late refresh of a finished
/// call does not reopen it.
const MAX_FINISHED_PER_CHAT: usize = 512;
/// A call this quiet for this long is forgotten whatever the chat does.
const FORGET_AFTER_MS: i64 = 6 * 60 * 60_000;
/// Mast tools that block by design (`wait: true`, waiting for a turn): their
/// length is the point, not a hang.
const BLOCKING_MAST_TOOLS: [&str; 4] = [
    "wait_for_turn",
    "create_chat",
    "send_message",
    "respond_to_input",
];

#[derive(Debug, Clone)]
struct Open {
    label: String,
    /// When the call opened or last refreshed its chip.
    quiet_since_ms: i64,
    flagged: bool,
}

#[derive(Debug, Default)]
struct ChatCalls {
    open: HashMap<String, Open>,
    finished: HashSet<String>,
}

type Registry = HashMap<String, ChatCalls>;

/// A call to tell the orchestrator about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongCall {
    pub chat_id: String,
    pub tool_id: String,
    pub label: String,
    pub quiet_ms: i64,
}

fn registry() -> &'static Mutex<Registry> {
    static OPEN: OnceLock<Mutex<Registry>> = OnceLock::new();
    OPEN.get_or_init(Default::default)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A short name for the call, as the Manager reads it. A command line can
/// carry a credential, so the text goes through the same redaction as other
/// diagnostics shown in the UI.
fn label_of(call: &ToolCall) -> String {
    let cut = |text: &str| -> String {
        let flat = mast_harness::redact::redact_secrets(text)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if flat.chars().count() > 120 {
            format!("{}…", flat.chars().take(120).collect::<String>())
        } else {
            flat
        }
    };
    match call {
        ToolCall::Exec { command } => format!("shell command `{}`", cut(command)),
        ToolCall::Mcp { server, tool, .. } => format!("{server} tool {tool}"),
        ToolCall::Unknown { name, .. } => format!("tool {}", cut(name)),
        ToolCall::WebFetch { url, .. } => format!("web fetch {}", cut(url)),
        ToolCall::WebSearch { .. } => "web search".to_owned(),
        _ => "file or search tool".to_owned(),
    }
}

/// Whether a call may legitimately stay open for a long time.
fn expected_to_block(call: &ToolCall) -> bool {
    match call {
        // A subagent works in its own transcript, quietly from here.
        call if call.is_subagent_spawn() => true,
        ToolCall::Todo { .. } => true,
        ToolCall::Mcp { server, tool, .. } => {
            mast_proto::agent_tools::is_server_name(server)
                && BLOCKING_MAST_TOOLS.contains(&tool.as_str())
        }
        // ACP agents name every tool by its display title: a subagent task, or
        // a blocking Mast tool, shows up only in the name.
        ToolCall::Unknown { name, .. } => {
            name.starts_with("Task") || BLOCKING_MAST_TOOLS.iter().any(|tool| name.contains(tool))
        }
        _ => false,
    }
}

/// A call `tool_id` of `chat_id` was emitted at `now_ms`. The first emission
/// opens it; a later one with the same id is output from it, so it restarts
/// the quiet time. A call that already returned stays closed.
pub fn call_emitted(chat_id: &str, tool_id: &str, call: &ToolCall, now_ms: i64) {
    if expected_to_block(call) {
        return;
    }
    let mut registry = lock(registry());
    let chat = registry.entry(chat_id.to_owned()).or_default();
    if chat.finished.contains(tool_id) {
        return;
    }
    if let Some(known) = chat.open.get_mut(tool_id) {
        known.quiet_since_ms = now_ms;
        return;
    }
    if chat.open.len() >= MAX_CALLS_PER_CHAT {
        // Unclosed calls must not crowd out new ones: drop the oldest.
        if let Some(oldest) = chat
            .open
            .iter()
            .min_by_key(|(_, call)| call.quiet_since_ms)
            .map(|(id, _)| id.clone())
        {
            chat.open.remove(&oldest);
        }
    }
    chat.open.insert(
        tool_id.to_owned(),
        Open {
            label: label_of(call),
            quiet_since_ms: now_ms,
            flagged: false,
        },
    );
}

/// The call returned.
pub fn call_finished(chat_id: &str, tool_id: &str) {
    let mut registry = lock(registry());
    let chat = registry.entry(chat_id.to_owned()).or_default();
    chat.open.remove(tool_id);
    if chat.finished.len() >= MAX_FINISHED_PER_CHAT {
        chat.finished.clear();
    }
    chat.finished.insert(tool_id.to_owned());
}

/// The chat did something that makes every open call look quiet again: an
/// approval or question was answered, so the time since the call began was
/// spent waiting for the user, not for the tool.
pub fn chat_resumed(chat_id: &str, now_ms: i64) {
    if let Some(chat) = lock(registry()).get_mut(chat_id) {
        for call in chat.open.values_mut() {
            call.quiet_since_ms = now_ms;
        }
    }
}

/// The host was asleep or stalled: no call was quiet for the time that passed.
pub fn all_resumed(now_ms: i64) {
    resume_all(&mut lock(registry()), now_ms);
}

fn resume_all(registry: &mut Registry, now_ms: i64) {
    for chat in registry.values_mut() {
        for call in chat.open.values_mut() {
            call.quiet_since_ms = now_ms;
        }
    }
}

/// The chat's turn ended or a new one began: nothing it had open is open.
pub fn chat_turn_boundary(chat_id: &str) {
    lock(registry()).remove(chat_id);
}

/// The call was told to an orchestrator; it is not offered again.
pub fn mark_flagged(chat_id: &str, tool_id: &str) {
    if let Some(call) = lock(registry())
        .get_mut(chat_id)
        .and_then(|chat| chat.open.get_mut(tool_id))
    {
        call.flagged = true;
    }
}

/// Drops calls quiet for hours: a turn that ended without a boundary leaves
/// nothing behind for long.
fn forget_stale(registry: &mut Registry, now_ms: i64) {
    registry.retain(|_, chat| {
        chat.open
            .retain(|_, call| now_ms.saturating_sub(call.quiet_since_ms) < FORGET_AFTER_MS);
        !chat.open.is_empty() || !chat.finished.is_empty()
    });
}

/// Calls quiet for at least `threshold_ms` at `now_ms` and not yet flagged,
/// among the chats for which `is_working` holds. A chat that is not working
/// (waiting on the user, say) is skipped, not forgotten: its turn's end
/// ([`chat_turn_boundary`]) is what clears its calls. The caller reports each
/// call and then marks it with [`mark_flagged`].
pub fn long_calls(
    now_ms: i64,
    threshold_ms: i64,
    is_working: impl Fn(&str) -> bool,
) -> Vec<LongCall> {
    let mut registry = lock(registry());
    forget_stale(&mut registry, now_ms);
    let mut found = Vec::new();
    for (chat_id, chat) in registry.iter().filter(|(chat_id, _)| is_working(chat_id)) {
        for (tool_id, call) in &chat.open {
            let quiet_ms = now_ms.saturating_sub(call.quiet_since_ms);
            if !call.flagged && quiet_ms >= threshold_ms {
                found.push(LongCall {
                    chat_id: chat_id.clone(),
                    tool_id: tool_id.clone(),
                    label: call.label.clone(),
                    quiet_ms,
                });
            }
        }
    }
    found.sort_by(|a, b| (&a.chat_id, &a.tool_id).cmp(&(&b.chat_id, &b.tool_id)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec(command: &str) -> ToolCall {
        ToolCall::Exec {
            command: command.into(),
        }
    }

    /// Tests run beside engines whose own tick reads this registry at the real
    /// time, so their calls open at (about) the real time and are never old
    /// enough to be flagged or forgotten by another test's tick.
    fn base() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    /// What one tick does for `chat`: take the long calls and report them.
    fn tick(chat: &str, now_ms: i64) -> Vec<LongCall> {
        let found = long_calls(now_ms, 10_000, |c| c == chat);
        for call in &found {
            mark_flagged(&call.chat_id, &call.tool_id);
        }
        found
    }

    #[test]
    fn a_quiet_call_is_flagged_once_after_the_threshold() {
        let chat = "tw-once";
        chat_turn_boundary(chat);
        let start = base();
        call_emitted(
            chat,
            "t1",
            &exec("scripts/remote-cargo.sh wait h n 30"),
            start,
        );
        assert!(tick(chat, start + 9_999).is_empty());
        let found = tick(chat, start + 10_000);
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].tool_id.as_str(), found[0].quiet_ms),
            ("t1", 10_000)
        );
        assert!(found[0].label.contains("remote-cargo.sh wait"));
        assert!(tick(chat, start + 99_000).is_empty(), "told once");
        chat_turn_boundary(chat);
    }

    #[test]
    fn a_call_nobody_was_told_about_is_offered_again() {
        let chat = "tw-unreported";
        chat_turn_boundary(chat);
        let start = base();
        call_emitted(chat, "t", &exec("sleep 9999"), start);
        assert_eq!(long_calls(start + 20_000, 10_000, |c| c == chat).len(), 1);
        assert_eq!(
            long_calls(start + 30_000, 10_000, |c| c == chat).len(),
            1,
            "no Manager recorded it yet"
        );
        chat_turn_boundary(chat);
    }

    #[test]
    fn a_finished_or_refreshed_call_is_not_flagged_and_a_late_refresh_does_not_reopen_it() {
        let chat = "tw-finish";
        chat_turn_boundary(chat);
        let start = base();
        call_emitted(chat, "done", &exec("make"), start);
        call_finished(chat, "done");
        call_emitted(chat, "done", &exec("make"), start + 1_000);
        call_emitted(chat, "chatty", &exec("make"), start);
        call_emitted(chat, "chatty", &exec("make"), start + 9_000);
        assert!(tick(chat, start + 12_000).is_empty());
        let found = tick(chat, start + 19_000);
        assert_eq!(
            found
                .iter()
                .map(|call| call.tool_id.as_str())
                .collect::<Vec<_>>(),
            ["chatty"]
        );
        chat_turn_boundary(chat);
    }

    #[test]
    fn a_result_seen_before_its_call_still_closes_it() {
        let chat = "tw-early-result";
        chat_turn_boundary(chat);
        let start = base();
        call_finished(chat, "t");
        call_emitted(chat, "t", &exec("make"), start);
        assert!(tick(chat, start + 1_000_000).is_empty());
        chat_turn_boundary(chat);
    }

    #[test]
    fn a_chat_that_is_not_working_is_skipped_until_it_is_or_its_turn_ends() {
        let chat = "tw-idle";
        chat_turn_boundary(chat);
        let start = base();
        call_emitted(chat, "t", &exec("sleep 9999"), start);
        assert!(long_calls(start + 1_000_000, 10_000, |_| false).is_empty());
        assert_eq!(
            tick(chat, start + 1_000_000).len(),
            1,
            "still open once it works again"
        );
        call_emitted(chat, "u", &exec("sleep 9999"), start);
        chat_turn_boundary(chat);
        assert!(
            tick(chat, start + 2_000_000).is_empty(),
            "a finished turn has no open calls"
        );
    }

    #[test]
    fn a_call_quiet_for_hours_is_forgotten() {
        // On a private map: the real registry is shared with parallel tests.
        let open_call = |quiet_since_ms| Open {
            label: String::new(),
            quiet_since_ms,
            flagged: false,
        };
        let mut registry = Registry::from([(
            "chat".to_owned(),
            ChatCalls {
                open: HashMap::from([
                    ("old".to_owned(), open_call(0)),
                    ("new".to_owned(), open_call(FORGET_AFTER_MS)),
                ]),
                finished: HashSet::new(),
            },
        )]);
        forget_stale(&mut registry, FORGET_AFTER_MS);
        assert_eq!(registry["chat"].open.keys().collect::<Vec<_>>(), ["new"]);
        forget_stale(&mut registry, 2 * FORGET_AFTER_MS);
        assert!(registry.is_empty());
    }

    #[test]
    fn waking_from_sleep_restarts_every_chats_quiet_time() {
        // On a private map: the real registry is shared with parallel tests.
        let mut registry = Registry::new();
        for chat in ["a", "b"] {
            registry.entry(chat.to_owned()).or_default().open.insert(
                "call".to_owned(),
                Open {
                    label: String::new(),
                    quiet_since_ms: 5,
                    flagged: false,
                },
            );
        }
        resume_all(&mut registry, 900);
        assert!(
            registry
                .values()
                .all(|chat| chat.open["call"].quiet_since_ms == 900)
        );
    }

    #[test]
    fn answering_a_question_or_waking_from_sleep_restarts_the_quiet_time() {
        let chat = "tw-input";
        chat_turn_boundary(chat);
        let start = base();
        call_emitted(chat, "t", &exec("git push"), start);
        chat_resumed(chat, start + 50_000);
        assert!(tick(chat, start + 55_000).is_empty());
        chat_turn_boundary(chat);
    }

    #[test]
    fn the_oldest_call_makes_room_when_a_chat_has_too_many_open() {
        let chat = "tw-many";
        chat_turn_boundary(chat);
        let start = base();
        for i in 0..MAX_CALLS_PER_CHAT + 1 {
            call_emitted(chat, &format!("t{i}"), &exec("sleep 1"), start + i as i64);
        }
        let found = long_calls(start + 1_000_000, 10_000, |c| c == chat);
        assert_eq!(found.len(), MAX_CALLS_PER_CHAT);
        assert!(
            found.iter().all(|call| call.tool_id != "t0"),
            "the oldest was dropped"
        );
        assert!(
            found
                .iter()
                .any(|call| call.tool_id == format!("t{MAX_CALLS_PER_CHAT}"))
        );
        chat_turn_boundary(chat);
    }

    #[test]
    fn calls_that_block_by_design_are_not_tracked() {
        let chat = "tw-blocking";
        chat_turn_boundary(chat);
        let start = base();
        let mcp = |server: &str, tool: &str| ToolCall::Mcp {
            server: server.into(),
            tool: tool.into(),
            input: None,
        };
        let unknown = |name: &str| ToolCall::Unknown {
            name: name.into(),
            input: None,
        };
        call_emitted(chat, "mast", &mcp("mast-agent", "wait_for_turn"), start);
        call_emitted(chat, "agent", &unknown("Agent: review"), start);
        // ACP agents name a tool by its title.
        call_emitted(chat, "task", &unknown("Task: review the diff"), start);
        call_emitted(
            chat,
            "acp-wait",
            &unknown("mast-agent: wait_for_turn"),
            start,
        );
        call_emitted(chat, "other", &mcp("other", "wait_for_turn"), start);
        let found = tick(chat, start + 1_000_000);
        assert_eq!(
            found
                .iter()
                .map(|call| call.tool_id.as_str())
                .collect::<Vec<_>>(),
            ["other"]
        );
        assert_eq!(found[0].label, "other tool wait_for_turn");
        chat_turn_boundary(chat);
    }

    #[test]
    fn labels_are_one_short_line_without_credentials() {
        let chat = "tw-label";
        chat_turn_boundary(chat);
        let start = base();
        call_emitted(
            chat,
            "t",
            &exec(&format!("echo {}\nsecond line", "x".repeat(400))),
            start,
        );
        call_emitted(
            chat,
            "u",
            &exec("curl -H 'Authorization: Bearer abc123secret' https://example.test"),
            start,
        );
        let found = tick(chat, start + 100_000);
        let label = |id: &str| {
            found
                .iter()
                .find(|call| call.tool_id == id)
                .unwrap()
                .label
                .clone()
        };
        assert!(
            label("t").chars().count() < 160 && !label("t").contains('\n'),
            "{}",
            label("t")
        );
        assert!(!label("u").contains("abc123secret"), "{}", label("u"));
        chat_turn_boundary(chat);
    }
}
