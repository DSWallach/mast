use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MentionToken {
    pub(super) range: Range<usize>,
    pub(super) query: String,
}

/// The `@` must begin a token. This intentionally excludes `name@example.com`
/// and ordinary words while allowing punctuation such as `(@src`.
pub(super) fn mention_token(text: &str, cursor: usize) -> Option<MentionToken> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let token_start = text[..cursor]
        .char_indices()
        .rev()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(at + ch.len_utf8()))
        .unwrap_or(0);
    let relative_at = text[token_start..cursor].rfind('@')?;
    let at = token_start + relative_at;
    let valid_boundary = at == 0
        || text[..at]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{'));
    if text[at + 1..cursor].contains('@') || !valid_boundary {
        return None;
    }
    let end = text[cursor..]
        .char_indices()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(cursor + at))
        .unwrap_or(text.len());
    Some(MentionToken {
        range: at..end,
        query: text[at + 1..cursor].to_string(),
    })
}

/// The `/` must begin a whitespace-delimited token — the start of the input
/// or anywhere after whitespace. Offset 0 is the whole-prompt command
/// (`/compact`, `/goal ship it`) the CLI expands natively; mid-message
/// tokens are plain-text skill mentions (`fix this /code-review`) the model
/// reads and loads via its Skill tool. Either way the popup offers the same
/// list and acceptance replaces the token with `/name `.
///
/// Guards, all per-token: a token containing another `/` (a typed path such
/// as `/usr/bin` or `see /tmp/x`) never triggers, nor does one containing
/// `@` (that shape belongs to the file-mention popup); the cursor must sit
/// inside the token after its `/`.
pub(super) fn slash_token(text: &str, cursor: usize) -> Option<MentionToken> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let start = text[..cursor]
        .char_indices()
        .rev()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(at + ch.len_utf8()))
        .unwrap_or(0);
    if !text[start..].starts_with('/') || cursor == start {
        return None;
    }
    let end = text[cursor..]
        .char_indices()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(cursor + at))
        .unwrap_or(text.len());
    let body = &text[start + 1..end];
    if body.contains('/') || body.contains('@') {
        return None;
    }
    Some(MentionToken {
        range: start..end,
        query: text[start + 1..cursor].to_string(),
    })
}

/// Height of the slash popup's key legend (22px caps + breathing room).
pub(super) const SLASH_FOOTER_HEIGHT: f32 = 30.0;

/// The attach chord as the legend spells it (the `mod-enter` binding).
pub(super) fn slash_attach_key_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘↵"
    } else {
        "ctrl ↵"
    }
}

/// The preamble attached-skill chips serialize into, prepended to the
/// user's text. Plain, visible text — the model reads it and loads each
/// named skill via its Skill tool; nothing is hidden from the transcript.
pub(super) fn skill_preamble(skills: &[String]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let list = skills
        .iter()
        .map(|name| format!("/{name}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "Load and follow these skills for this task: {list}."
    ))
}

/// `text` with the attached-skills preamble prepended (unchanged when no
/// skills are attached). An otherwise-empty message sends the preamble
/// alone — a legitimate "load these" turn.
pub(super) fn with_skill_preamble(skills: &[String], text: &str) -> String {
    match skill_preamble(skills) {
        None => text.to_string(),
        Some(preamble) if text.trim().is_empty() => preamble,
        Some(preamble) => format!("{preamble}\n\n{text}"),
    }
}

/// Add `name` to a draft's attached skills; no-op (false) if already there.
pub(super) fn attach_skill(skills: &mut Vec<String>, name: &str) -> bool {
    if name.is_empty() || skills.iter().any(|s| s == name) {
        return false;
    }
    skills.push(name.to_string());
    true
}

/// Drop `name` from a draft's attached skills; false if it wasn't attached.
pub(super) fn detach_skill(skills: &mut Vec<String>, name: &str) -> bool {
    let before = skills.len();
    skills.retain(|s| s != name);
    skills.len() != before
}

/// The span to delete when a typed `/query` token becomes a chip: the token
/// plus one following space, so `fix /cod now` → `fix now` (not a double
/// space) while `fix /cod` → `fix ` keeps the caret ready to type on.
pub(super) fn attach_removal_range(text: &str, token: Range<usize>) -> Range<usize> {
    let end = if text[token.end..].starts_with(' ') {
        token.end + 1
    } else {
        token.end
    };
    token.start..end
}

/// Slash-command completion state: like [`FileMentionState`] but the
/// candidate list is fetched once per (harness, cwd) (`ListCommands`) and
/// filtered locally per keystroke — no RPC, debounce, or skeleton churn while
/// typing.
#[derive(Debug, Clone, Default)]
pub(super) struct SlashState {
    pub(super) token: Option<MentionToken>,
    /// Indices into the cached command list, filter-ranked for the query.
    pub(super) filtered: Vec<usize>,
    pub(super) active: Option<usize>,
    /// Harness the popup is showing commands for (half of the cache key).
    pub(super) harness: Option<HarnessId>,
    /// The active chat's cwd when the popup last (re)opened — empty when
    /// none is known yet (a blank new-chat screen). The other half of the
    /// cache key: project-scoped commands/skills differ per repo/worktree,
    /// so a chat must never show another chat's cached list just because
    /// they share a harness.
    pub(super) cwd: String,
    pub(super) request: u64,
    pub(super) loading: bool,
    pub(super) error: Option<SharedString>,
    pub(super) dismissed: Option<(Range<usize>, String)>,
}

/// `(harness, cwd)`: cwd is part of the cache key because a harness's
/// advertised commands can be project-scoped (Claude Code's
/// `.claude/commands` and skills) — two chats on the same harness but
/// different repos/worktrees must each get their own list.
pub(super) type SlashCacheKey = (HarnessId, String);

#[derive(Debug, Clone)]
pub(super) struct SlashCacheEntry {
    pub(super) commands: Vec<SlashCommand>,
    pub(super) fetched_at: Instant,
}

/// A skill/command added to a project mid-session should eventually show up
/// without restarting zeron, but a picker reopen (or a quick tab-switch
/// between a couple of chats) shouldn't re-fetch every time either — this is
/// the same order of magnitude as the engine's own per-cwd discovery cache
/// (see `mast_harness::claude::discovery::CommandsCache`).
const SLASH_CACHE_TTL: Duration = Duration::from_secs(300);

/// Whether a cached entry for `cached_key` can still answer a lookup for
/// `key`: same (harness, cwd) AND not yet past the TTL. Pulled out as a pure
/// function so the cache-key/invalidation decision is unit-testable without
/// a GPUI context.
pub(super) fn slash_cache_entry_is_fresh(
    cached_key: &SlashCacheKey,
    fetched_at: Instant,
    key: &SlashCacheKey,
    now: Instant,
) -> bool {
    cached_key == key && now.saturating_duration_since(fetched_at) < SLASH_CACHE_TTL
}

#[derive(Debug, Clone, Default)]
pub(super) struct FileMentionState {
    pub(super) token: Option<MentionToken>,
    pub(super) results: Vec<FileSearchMatch>,
    pub(super) active: Option<usize>,
    pub(super) request: u64,
    pub(super) loading: bool,
    /// Why the last search failed, for the popup. A failure MUST NOT render
    /// as "No matching files": cross-device searches fail for reasons the
    /// user can act on (host daemon too old for `SearchFiles`, device
    /// offline), and the empty state hid them (user report).
    pub(super) error: Option<SharedString>,
    /// Full token text, not just the cursor-relative query: moving within a
    /// dismissed token keeps it closed, while any edit re-enables completion.
    pub(super) dismissed: Option<(Range<usize>, String)>,
}

pub(super) fn mention_response_is_current(state: &FileMentionState, request: u64) -> bool {
    state.request == request && state.token.is_some()
}

/// A failed file search, translated for the popup. `UnknownMethod` is the
/// version-skew case: `SearchFiles` shipped after v0.1.9, so a session hosted
/// by a device on an older daemon answers "unknown method" while the same
/// search works for local sessions.
pub(super) fn mention_error_message(err: &RpcError) -> SharedString {
    match err {
        RpcError::UnknownMethod(_) => {
            "The session's device runs an older Mast — update it to search its files".into()
        }
        RpcError::Transport(_) | RpcError::Closed => "The session's device is unreachable".into(),
        RpcError::BadParams(_) | RpcError::Failed(_) => "File search failed".into(),
    }
}

/// The folder the chat composer's `/` list discovers project skills in: the
/// folder the agent runs (or will run) in, because Claude Code loads
/// `<cwd>/.claude/skills` (walking up to the cwd's git root, stopping at a
/// nested repo/submodule) and nothing else.
/// - A selected chat: its own cwd (a Work Outline chat's validated
///   execution root, e.g. a submodule), else its Space folder.
/// - A new-chat draft: the reused worktree when one is picked, else the
///   selected Space folder. Empty only with no project at all — that list
///   holds user-scoped skills only.
pub(super) fn slash_discovery_cwd(
    chat: Option<&mast_proto::Chat>,
    chat_space: Option<&mast_proto::Space>,
    selected_space: Option<&mast_proto::Space>,
    reuse_worktree: Option<String>,
) -> String {
    let non_empty = |path: &str| (!path.trim().is_empty()).then(|| path.to_owned());
    match chat {
        Some(chat) => chat
            .cwd
            .as_deref()
            .and_then(non_empty)
            .or_else(|| chat_space.and_then(|space| non_empty(&space.path)))
            .unwrap_or_default(),
        None => reuse_worktree
            .as_deref()
            .and_then(non_empty)
            .or_else(|| selected_space.and_then(|space| non_empty(&space.path)))
            .unwrap_or_default(),
    }
}

/// `ListCommands` params for `(harness, cwd)`, targeted like file search:
/// the chat/space host device owns the agent binary, and a chat id lets the
/// engine resolve the chat's own folder safely. Shared by the chat composer
/// and [`skill_completion::SkillCompletion`].
pub(super) fn list_commands_params(
    harness: HarnessId,
    cwd: &str,
    target_device_id: Option<&str>,
    chat_id: Option<&str>,
) -> serde_json::Value {
    let mut params = serde_json::json!({ "harness": harness, "cwd": cwd });
    if let Some(object) = params.as_object_mut() {
        if let Some(target) = target_device_id {
            object.insert("targetDeviceId".into(), target.into());
        }
        if let Some(chat_id) = chat_id {
            object.insert("chatId".into(), chat_id.into());
        }
    }
    params
}

/// One slash-popup row's body: the command icon, `/name`, and the muted
/// description (with its argument hint). Shared by the chat composer's
/// popup and [`skill_completion::SkillCompletion`].
pub(super) fn slash_row_body(theme: &Theme, command: &SlashCommand) -> gpui::Div {
    let name: SharedString = format!("/{}", command.name).into();
    let mut description = command.description.clone();
    if let Some(hint) = &command.input_hint {
        if description.is_empty() {
            description = format!("<{hint}>");
        } else {
            description = format!("{description} · <{hint}>");
        }
    }
    let description: SharedString = description.into();
    div()
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(
            crate::icons::icon(crate::icons::COMMAND)
                .size(px(14.0))
                .text_color(theme.text_muted),
        )
        .child(
            div()
                .flex_none()
                .text_size(crate::typography::ui_rems(12.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(name),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .truncate()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .child(description),
        )
}

/// A failed command discovery, translated for the popup.
pub(super) fn slash_error_message(err: &RpcError) -> SharedString {
    match err {
        RpcError::UnknownMethod(_) => {
            "The session's device runs an older Mast — update it to list commands".into()
        }
        RpcError::Transport(_) | RpcError::Closed => "The session's device is unreachable".into(),
        RpcError::BadParams(_) | RpcError::Failed(_) => {
            "Couldn't load this agent's commands".into()
        }
    }
}

