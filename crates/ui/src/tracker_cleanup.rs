//! Rows for the Tracker's Cleanup section. Pure: engine health entries and
//! chat facts in, rows out. Each row says why the entry needs cleanup and what
//! its fix does; repeated chats collapse into one group row. The shell renders
//! the rows and runs the fixes.

use std::collections::{HashMap, HashSet};

use mast_engine::work_outline_health::{HealthBlock, HealthChatFacts, HealthEntry};

/// Cleanup the Tracker finds from data it already holds, beside the engine's
/// health entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TrackerCleanup {
    LinkIssue { item_id: String, identifier: String },
    PossibleDuplicate { item_id: String, identifier: String },
    StaleRequest { request_id: String, chat_id: String, text: String },
}

/// Declaration order is the display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum CleanupKind {
    ChatOnContainer,
    StaleChat,
    SeveralLiveChats,
    WorkerOnDefaultBranch,
    MergedPr,
    StaleRequest,
    LinkIssue,
    UnlinkedPr,
    PossibleDuplicate,
}

impl CleanupKind {
    /// Whether the row's chats can be archived from the row.
    pub(crate) fn archives_chats(self) -> bool {
        matches!(self, Self::ChatOnContainer | Self::StaleChat | Self::SeveralLiveChats)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CleanupChat {
    pub id: String,
    pub title: String,
    /// Why this chat is kept (shown after "Kept: "); `None` when it can be archived.
    pub kept: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CleanupFix {
    /// Archive these chats (only the ones that are safe to archive).
    ArchiveChats(Vec<String>),
    MarkCompleted(String),
    OpenLinear(String),
    ResolveRequest(String),
    LinkIssue { item_id: String, identifier: String },
    LinkPr(String),
}

impl CleanupFix {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::ArchiveChats(ids) if ids.len() > 1 => "Archive all",
            Self::ArchiveChats(_) => "Archive",
            Self::MarkCompleted(_) => "Mark completed",
            Self::OpenLinear(_) => "Open Linear",
            Self::ResolveRequest(_) => "Resolve",
            Self::LinkIssue { .. } => "Link",
            Self::LinkPr(_) => "Link PR",
        }
    }

    /// Selector stem for the fix button (`cleanup-<stem>-<row>`).
    pub(crate) fn selector(&self) -> &'static str {
        match self {
            Self::ArchiveChats(_) => "archive",
            Self::MarkCompleted(_) => "complete",
            Self::OpenLinear(_) => "open-linear",
            Self::ResolveRequest(_) => "resolve-request",
            Self::LinkIssue { .. } => "link-issue",
            Self::LinkPr(_) => "link",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CleanupRow {
    pub kind: CleanupKind,
    /// Stable across refreshes; keys the group's expanded state.
    pub key: String,
    pub heading: String,
    /// Plain language: why this needs cleanup and what the fix does.
    pub reason: String,
    pub chats: Vec<CleanupChat>,
    pub fix: Option<CleanupFix>,
    /// "Kept: ..." when some chats are blocked from archiving.
    pub kept: Option<String>,
}

pub(crate) struct CleanupContext<'a> {
    pub chat_facts: &'a HashMap<String, HealthChatFacts>,
    pub chat_title: &'a dyn Fn(&str) -> String,
    pub item_title: &'a dyn Fn(&str) -> String,
    /// Items already completed locally.
    pub completed_items: &'a HashSet<String>,
    /// Linear issue URL per item.
    pub issue_urls: &'a HashMap<String, String>,
    /// The chat showing the Tracker; never archived from the Tracker.
    pub active_chat: &'a str,
    /// Whether the chat works on a live item of the work queue, or under one.
    pub on_queue: &'a dyn Fn(&str) -> bool,
}

fn plural(count: i64, unit: &str) -> String {
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

pub(crate) fn duration_phrase(seconds: i64) -> String {
    let hours = seconds / 3600;
    let days = hours / 24;
    if days >= 1 {
        plural(days, "day")
    } else if hours >= 1 {
        plural(hours, "hour")
    } else {
        "under an hour".into()
    }
}

fn clip(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() { format!("{}…", head.trim_end()) } else { head }
}

fn kept_reason(chat_id: &str, ctx: &CleanupContext<'_>) -> Option<&'static str> {
    if chat_id == ctx.active_chat {
        return Some("this chat shows the Tracker");
    }
    if (ctx.on_queue)(chat_id) {
        return Some("works on a live queued item");
    }
    ctx.chat_facts.get(chat_id).and_then(|facts| facts.blocked).map(HealthBlock::note)
}

fn cleanup_chat(chat_id: &str, ctx: &CleanupContext<'_>) -> CleanupChat {
    CleanupChat { id: chat_id.to_owned(), title: (ctx.chat_title)(chat_id), kept: kept_reason(chat_id, ctx) }
}

fn kept_summary(chats: &[CleanupChat]) -> Option<String> {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    for note in chats.iter().filter_map(|chat| chat.kept) {
        match counts.iter_mut().find(|(existing, _)| *existing == note) {
            Some((_, count)) => *count += 1,
            None => counts.push((note, 1)),
        }
    }
    if counts.is_empty() {
        return None;
    }
    let single = chats.len() == 1;
    let parts: Vec<String> = counts.into_iter()
        .map(|(note, count)| if single { note.to_owned() } else { format!("{count} {note}") }).collect();
    Some(format!("Kept: {}", parts.join(", ")))
}

struct ChatGroup {
    kind: CleanupKind,
    container: Option<String>,
    title: String,
    chats: Vec<CleanupChat>,
}

fn chat_group_row(group: ChatGroup, ctx: &CleanupContext<'_>) -> CleanupRow {
    let ChatGroup { kind, container, title, chats } = group;
    let count = chats.len();
    let many = count > 1;
    let container_name = container.as_deref().map(|id| (ctx.item_title)(id)).unwrap_or_default();
    // A group is idle for as long as its most recently active chat.
    let idle = chats.iter().filter_map(|chat| ctx.chat_facts.get(&chat.id)).map(|facts| facts.idle_seconds).min();
    let idle_clause = |each: &str, single: &str| match idle {
        Some(seconds) if many => format!("{each} {}", duration_phrase(seconds)),
        Some(seconds) => format!("{single} {}", duration_phrase(seconds)),
        None => String::new(),
    };
    let reason = match kind {
        CleanupKind::ChatOnContainer => {
            let idle_text = idle_clause("each idle at least", "idle");
            let idle_text = if idle_text.is_empty() { String::new() } else { format!(", {idle_text}") };
            if many {
                format!("Attached to the container '{container_name}' instead of a task{idle_text}. Archive them, or move each to a task.")
            } else {
                format!("Attached to the container '{container_name}' instead of a task{idle_text}. Archive it, or move it to a task.")
            }
        }
        CleanupKind::StaleChat => {
            let idle_text = match idle {
                Some(seconds) if many => format!("Each idle at least {}", duration_phrase(seconds)),
                Some(seconds) => format!("Idle {}", duration_phrase(seconds)),
                None => if many { "Each idle for days".into() } else { "Idle for days".into() },
            };
            if many {
                format!("{idle_text} with no open PR, and its item isn't completed. Archive them.")
            } else {
                format!("{idle_text} with no open PR, and its item isn't completed. Archive it.")
            }
        }
        CleanupKind::WorkerOnDefaultBranch => if many {
            "Working directly on the default branch without their own worktree. Open each chat and move the work to a branch. Nothing is changed automatically.".into()
        } else {
            "Working directly on the default branch without its own worktree. Open the chat and move the work to a branch. Nothing is changed automatically.".into()
        },
        _ => String::new(),
    };
    let eligible: Vec<String> = chats.iter().filter(|chat| chat.kept.is_none()).map(|chat| chat.id.clone()).collect();
    let fix = (kind.archives_chats() && !eligible.is_empty()).then(|| CleanupFix::ArchiveChats(eligible));
    CleanupRow {
        kind,
        key: format!("{kind:?}\u{1f}{}\u{1f}{title}", container.as_deref().unwrap_or("")),
        heading: if many { format!("{title} · {count} chats") } else { title },
        reason,
        kept: kept_summary(&chats),
        chats,
        fix,
    }
}

fn add_to_group(groups: &mut Vec<ChatGroup>, kind: CleanupKind, container: Option<&str>, chat_id: &str, ctx: &CleanupContext<'_>) {
    let chat = cleanup_chat(chat_id, ctx);
    match groups.iter_mut().find(|group| group.kind == kind && group.container.as_deref() == container && group.title == chat.title) {
        Some(group) => if !group.chats.iter().any(|existing| existing.id == chat.id) { group.chats.push(chat); },
        None => groups.push(ChatGroup { kind, container: container.map(str::to_owned), title: chat.title.clone(), chats: vec![chat] }),
    }
}

fn pr_label(url: &str) -> String {
    crate::chat_work_context::parse_pr_url(url).map_or_else(|| "A PR".to_owned(), |pr| format!("PR #{}", pr.number))
}

/// Build the rows. Order: by kind, then in the order entries arrived.
pub(crate) fn cleanup_rows(entries: &[HealthEntry], local: &[TrackerCleanup], ctx: &CleanupContext<'_>) -> Vec<CleanupRow> {
    let mut rows = Vec::new();
    let mut groups: Vec<ChatGroup> = Vec::new();
    let mut container_chats = HashSet::new();
    let mut merged_items = HashSet::new();
    // Container chats first, so a chat on a container that is also stale is listed once.
    for entry in entries {
        if let HealthEntry::ChatOnContainer { item_id, chat_id } = entry {
            container_chats.insert(chat_id.as_str());
            add_to_group(&mut groups, CleanupKind::ChatOnContainer, Some(item_id.as_str()), chat_id, ctx);
        }
    }
    for entry in entries {
        match entry {
            HealthEntry::ChatOnContainer { .. } => {}
            HealthEntry::StaleChat { chat_id } => {
                if !container_chats.contains(chat_id.as_str()) {
                    add_to_group(&mut groups, CleanupKind::StaleChat, None, chat_id, ctx);
                }
            }
            HealthEntry::WorkerOnDefaultBranch { chat_id } => {
                add_to_group(&mut groups, CleanupKind::WorkerOnDefaultBranch, None, chat_id, ctx);
            }
            HealthEntry::SeveralLiveChats { item_id, chat_ids } => {
                let chats: Vec<CleanupChat> = chat_ids.iter().map(|id| cleanup_chat(id, ctx)).collect();
                let task = (ctx.item_title)(item_id);
                rows.push(CleanupRow {
                    kind: CleanupKind::SeveralLiveChats,
                    key: format!("{:?}\u{1f}{item_id}", CleanupKind::SeveralLiveChats),
                    heading: format!("{task} · {} live chats", chats.len()),
                    reason: "Several chats are working on one task. Keep the one you're using and archive the others.".into(),
                    kept: kept_summary(&chats),
                    chats,
                    fix: None,
                });
            }
            HealthEntry::MergedPrOnOpenItem { item_id, url } => {
                if !merged_items.insert(item_id.as_str()) { continue; }
                let linear_open = ctx.completed_items.contains(item_id);
                let pr = pr_label(url);
                let (reason, fix) = if linear_open {
                    (format!("{pr} is merged and the item is completed, but its Linear ticket is still open. Close the ticket in Linear."),
                        ctx.issue_urls.get(item_id).map(|url| CleanupFix::OpenLinear(url.clone())))
                } else {
                    (format!("{pr} is merged but the item is still open. Mark it completed."), Some(CleanupFix::MarkCompleted(item_id.clone())))
                };
                rows.push(CleanupRow { kind: CleanupKind::MergedPr, key: format!("{:?}\u{1f}{item_id}", CleanupKind::MergedPr),
                    heading: (ctx.item_title)(item_id), reason, chats: Vec::new(), fix, kept: None });
            }
            HealthEntry::UnlinkedPr { url } => {
                let heading = crate::chat_work_context::parse_pr_url(url).map_or_else(|| url.clone(), |pr| pr.external_id());
                rows.push(CleanupRow { kind: CleanupKind::UnlinkedPr, key: format!("{:?}\u{1f}{url}", CleanupKind::UnlinkedPr),
                    heading, reason: "This PR isn't linked to any task, so the Tracker can't follow it. Link it to a task.".into(),
                    chats: Vec::new(), fix: Some(CleanupFix::LinkPr(url.clone())), kept: None });
            }
        }
    }
    rows.extend(groups.into_iter().map(|group| chat_group_row(group, ctx)));
    for entry in local {
        rows.push(match entry {
            TrackerCleanup::LinkIssue { item_id, identifier } => CleanupRow {
                kind: CleanupKind::LinkIssue, key: format!("{:?}\u{1f}{item_id}", CleanupKind::LinkIssue),
                heading: (ctx.item_title)(item_id),
                reason: format!("The task name mentions {identifier}, but the task isn't linked to that Linear issue. Link it."),
                chats: Vec::new(), fix: Some(CleanupFix::LinkIssue { item_id: item_id.clone(), identifier: identifier.clone() }), kept: None,
            },
            TrackerCleanup::PossibleDuplicate { item_id, identifier } => CleanupRow {
                kind: CleanupKind::PossibleDuplicate, key: format!("{:?}\u{1f}{item_id}\u{1f}{identifier}", CleanupKind::PossibleDuplicate),
                heading: (ctx.item_title)(item_id),
                reason: format!("Has the same title as Linear issue {identifier}. Check whether they are the same work. Nothing is changed automatically."),
                chats: Vec::new(), fix: Some(CleanupFix::OpenLinear(format!("https://linear.app/issue/{identifier}"))), kept: None,
            },
            TrackerCleanup::StaleRequest { request_id, chat_id, text } => CleanupRow {
                kind: CleanupKind::StaleRequest, key: format!("{:?}\u{1f}{request_id}", CleanupKind::StaleRequest),
                heading: (ctx.chat_title)(chat_id),
                reason: if text.is_empty() {
                    "A Required Action is still open, but its PR is merged. Resolve it.".into()
                } else {
                    format!("\"{}\" is still open as a Required Action, but its PR is merged. Resolve it.", clip(text, 80))
                },
                chats: Vec::new(), fix: Some(CleanupFix::ResolveRequest(request_id.clone())), kept: None,
            },
        });
    }
    rows.sort_by_key(|row| row.kind);
    rows
}

/// A chat on a container is archived by Clean all only after this much idle time.
pub(crate) const CONTAINER_MIN_IDLE_SECONDS: i64 = 86_400;

/// What Clean all would do right now, and what it leaves alone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CleanAllPlan {
    /// Chats to archive, each with the idle time the engine must still find.
    pub archive: Vec<(String, i64)>,
    pub complete: Vec<String>,
    pub resolve: Vec<String>,
    /// Plain "label: count" lines for what stays, in a fixed order.
    pub skipped: Vec<String>,
}

fn count_text(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

impl CleanAllPlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.archive.is_empty() && self.complete.is_empty() && self.resolve.is_empty()
    }

    /// "Archive 18 chats, mark 2 items completed, resolve 1 stale request".
    pub(crate) fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.archive.is_empty() { parts.push(format!("archive {}", count_text(self.archive.len(), "chat"))); }
        if !self.complete.is_empty() { parts.push(format!("mark {} completed", count_text(self.complete.len(), "item"))); }
        if !self.resolve.is_empty() { parts.push(format!("resolve {}", count_text(self.resolve.len(), "stale request"))); }
        sentence(&parts)
    }
}

/// The parts joined by commas, the first one capitalized.
fn sentence(parts: &[String]) -> String {
    let text = parts.join(", ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

/// What applying a cleanup did, for the line the Cleanup section shows after.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CleanupOutcome {
    pub archived: usize,
    /// Chats the engine kept because they were busy or active since the plan.
    pub kept: usize,
    pub completed: usize,
    pub resolved: usize,
    pub failed: usize,
    /// Why the archive call failed, when it did.
    pub problem: Option<String>,
}

impl CleanupOutcome {
    pub(crate) fn record_archive(&mut self, reply: &mast_proto::WorkOutlineHealthArchiveReply) {
        for result in &reply.results {
            // A chat archived before this request is as good as one archived by it.
            if result.archived || result.reason.as_deref() == Some(mast_proto::HEALTH_ALREADY_ARCHIVED) {
                self.archived += 1;
            } else {
                self.kept += 1;
            }
        }
    }

    pub(crate) fn message(&self) -> String {
        let mut parts = Vec::new();
        if self.archived > 0 { parts.push(format!("archived {}", count_text(self.archived, "chat"))); }
        if self.completed > 0 { parts.push(format!("marked {} completed", count_text(self.completed, "item"))); }
        if self.resolved > 0 { parts.push(format!("resolved {}", count_text(self.resolved, "stale request"))); }
        if self.kept > 0 { parts.push(format!("kept {} that changed or became busy", count_text(self.kept, "chat"))); }
        if self.failed > 0 { parts.push(format!("{} failed, try again", count_text(self.failed, "change"))); }
        let mut text = if parts.is_empty() { "Nothing to clean up".to_owned() } else { sentence(&parts) };
        if let Some(problem) = &self.problem { text = format!("{text}. {problem}"); }
        text
    }
}

#[derive(Default)]
struct Skips(Vec<(String, usize)>);

impl Skips {
    fn add(&mut self, label: impl Into<String>, count: usize) {
        if count == 0 { return; }
        let label = label.into();
        match self.0.iter_mut().find(|(existing, _)| *existing == label) {
            Some((_, total)) => *total += count,
            None => self.0.push((label, count)),
        }
    }
}

/// Apply the confirmed table: stale chats, safe container chats (idle at least
/// a day; the engine blocks any chat with live child chats) and all but the newest of several live chats
/// are archived; merged-PR items are marked completed; stale requests are
/// resolved. Everything else is left alone and listed. A chat the engine
/// reports as blocked is never in the plan.
pub(crate) fn clean_all_plan(rows: &[CleanupRow], ctx: &CleanupContext<'_>) -> CleanAllPlan {
    let mut plan = CleanAllPlan::default();
    let mut skips = Skips::default();
    let mut planned = HashSet::new();
    let mut unlinked_prs = 0;
    let mut duplicates = 0;
    let mut link_issues = 0;
    let mut default_branch = 0;
    let mut linear_open = 0;
    // The idle time the engine must still find, with a minute of slack, and
    // never below the rule's own floor.
    let min_idle = |idle: i64, floor: i64| idle.saturating_sub(60).max(floor);
    let mut archive = |plan: &mut CleanAllPlan, chat: &CleanupChat, idle: i64, floor: i64| {
        if planned.insert(chat.id.clone()) { plan.archive.push((chat.id.clone(), min_idle(idle, floor))); }
    };
    for row in rows {
        match row.kind {
            CleanupKind::StaleChat | CleanupKind::ChatOnContainer => {
                let container = row.kind == CleanupKind::ChatOnContainer;
                for chat in &row.chats {
                    if let Some(kept) = chat.kept {
                        skips.add(format!("Chats kept ({kept})"), 1);
                        continue;
                    }
                    let Some(facts) = ctx.chat_facts.get(&chat.id) else {
                        skips.add("Chats kept (details unavailable)", 1);
                        continue;
                    };
                    if container && facts.idle_seconds < CONTAINER_MIN_IDLE_SECONDS {
                        skips.add("Chats kept (idle under a day)", 1);
                    } else {
                        archive(&mut plan, chat, facts.idle_seconds, if container { CONTAINER_MIN_IDLE_SECONDS } else { 0 });
                    }
                }
            }
            CleanupKind::SeveralLiveChats => {
                let idle = |chat: &CleanupChat| ctx.chat_facts.get(&chat.id).map(|facts| facts.idle_seconds);
                if row.chats.iter().any(|chat| idle(chat).is_none()) {
                    skips.add("Chats kept (details unavailable)", row.chats.len());
                    continue;
                }
                // The most recently active chat stays; the first one wins a tie.
                let newest = row.chats.iter().enumerate().min_by_key(|(index, chat)| (idle(*chat).unwrap_or(i64::MAX), *index)).map(|(index, _)| index);
                for (index, chat) in row.chats.iter().enumerate() {
                    if Some(index) == newest {
                        skips.add("Chats kept (newest on their task)", 1);
                    } else if let Some(kept) = chat.kept {
                        skips.add(format!("Chats kept ({kept})"), 1);
                    } else {
                        archive(&mut plan, chat, idle(chat).unwrap_or(0), 0);
                    }
                }
            }
            CleanupKind::WorkerOnDefaultBranch => default_branch += row.chats.len(),
            CleanupKind::MergedPr => match &row.fix {
                Some(CleanupFix::MarkCompleted(item_id)) => plan.complete.push(item_id.clone()),
                _ => linear_open += 1,
            },
            CleanupKind::StaleRequest => {
                if let Some(CleanupFix::ResolveRequest(request_id)) = &row.fix { plan.resolve.push(request_id.clone()); }
            }
            CleanupKind::UnlinkedPr => unlinked_prs += 1,
            CleanupKind::PossibleDuplicate => duplicates += 1,
            CleanupKind::LinkIssue => link_issues += 1,
        }
    }
    skips.add("Chats on the default branch (move the work to a branch)", default_branch);
    skips.add("Items whose Linear ticket is still open (close it in Linear)", linear_open);
    skips.add("Unlinked PRs (link each to a task)", unlinked_prs);
    skips.add("Possible duplicates (check by hand)", duplicates);
    skips.add("Tasks to link to a Linear issue (identifier unverified)", link_issues);
    plan.skipped = skips.0.into_iter().map(|(label, count)| format!("{label}: {count}")).collect();
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        facts: HashMap<String, HealthChatFacts>,
        completed: HashSet<String>,
        urls: HashMap<String, String>,
        queued: HashSet<String>,
    }

    impl Fixture {
        fn new() -> Self {
            Self { facts: HashMap::new(), completed: HashSet::new(), urls: HashMap::new(), queued: HashSet::new() }
        }

        fn idle(mut self, chat: &str, days: i64, blocked: Option<HealthBlock>) -> Self {
            self.facts.insert(chat.into(), HealthChatFacts { idle_seconds: days * 86_400, blocked, live_children: 0 });
            self
        }

        fn rows(&self, entries: &[HealthEntry], local: &[TrackerCleanup]) -> Vec<CleanupRow> {
            let chat_title = |id: &str| if id.starts_with("queue") { "Mast development queue check".to_owned() } else { format!("Chat {id}") };
            let item_title = |id: &str| match id { "container" => "Mast development".to_owned(), other => format!("Item {other}") };
            cleanup_rows(entries, local, &CleanupContext {
                chat_facts: &self.facts, chat_title: &chat_title, item_title: &item_title,
                completed_items: &self.completed, issue_urls: &self.urls, active_chat: "host",
                on_queue: &|chat| self.queued.contains(chat),
            })
        }
    }

    fn on_container(chat: &str) -> HealthEntry {
        HealthEntry::ChatOnContainer { item_id: "container".into(), chat_id: chat.into() }
    }

    #[test]
    fn repeated_queue_check_chats_become_one_group_with_a_single_archive_all() {
        let entries: Vec<_> = (0..3).map(|index| on_container(&format!("queue-{index}"))).collect();
        let rows = Fixture::new().idle("queue-0", 2, None).idle("queue-1", 5, None).idle("queue-2", 9, None).rows(&entries, &[]);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.heading, "Mast development queue check · 3 chats");
        assert_eq!(row.reason, "Attached to the container 'Mast development' instead of a task, each idle at least 2 days. Archive them, or move each to a task.");
        assert_eq!(row.fix, Some(CleanupFix::ArchiveChats(vec!["queue-0".into(), "queue-1".into(), "queue-2".into()])));
        assert_eq!(row.fix.as_ref().map(CleanupFix::label), Some("Archive all"));
        assert_eq!(row.kept, None);
    }

    #[test]
    fn a_single_container_chat_gets_archive_not_a_generic_open_chat() {
        let rows = Fixture::new().idle("a", 3, None).rows(&[on_container("a")], &[]);
        assert_eq!(rows[0].heading, "Chat a");
        assert_eq!(rows[0].reason, "Attached to the container 'Mast development' instead of a task, idle 3 days. Archive it, or move it to a task.");
        assert_eq!(rows[0].fix.as_ref().map(CleanupFix::label), Some("Archive"));
    }

    #[test]
    fn running_waiting_and_active_chats_are_never_offered_for_archive() {
        let entries: Vec<_> = ["queue-0", "queue-1", "queue-2", "queue-3", "queue-4"].into_iter().map(on_container).collect();
        let fixture = Fixture::new()
            .idle("queue-0", 4, Some(HealthBlock::Running))
            .idle("queue-1", 4, Some(HealthBlock::OpenRequest))
            .idle("queue-2", 4, Some(HealthBlock::AwaitingInput))
            .idle("queue-3", 4, Some(HealthBlock::StandingRole))
            .idle("queue-4", 4, None);
        let rows = fixture.rows(&entries, &[]);
        assert_eq!(rows[0].fix, Some(CleanupFix::ArchiveChats(vec!["queue-4".into()])));
        assert_eq!(rows[0].fix.as_ref().map(CleanupFix::label), Some("Archive"));
        assert_eq!(rows[0].kept.as_deref(), Some("Kept: 1 running, 1 has an open Required Action, 1 waiting for input, 1 holds a Manager or assistant role"));
        let blocked_only = fixture.rows(&[on_container("queue-0"), on_container("host")], &[]);
        assert_eq!(blocked_only.len(), 2);
        assert!(blocked_only.iter().all(|row| row.fix.is_none() && row.kept.is_some()));
        assert_eq!(blocked_only[1].kept.as_deref(), Some("Kept: this chat shows the Tracker"));
    }

    #[test]
    fn a_chat_both_on_a_container_and_stale_is_listed_once() {
        let rows = Fixture::new().rows(&[HealthEntry::StaleChat { chat_id: "a".into() }, on_container("a"), HealthEntry::StaleChat { chat_id: "b".into() }], &[]);
        assert_eq!(rows.iter().map(|row| (row.kind, row.chats.len())).collect::<Vec<_>>(), [(CleanupKind::ChatOnContainer, 1), (CleanupKind::StaleChat, 1)]);
    }

    #[test]
    fn each_entry_type_states_its_reason_and_fix() {
        let mut fixture = Fixture::new().idle("stale", 4, None).idle("main", 1, None);
        fixture.completed.insert("done".into());
        fixture.urls.insert("done".into(), "https://linear.app/issue/EX-9".into());
        let entries = [
            HealthEntry::StaleChat { chat_id: "stale".into() },
            HealthEntry::WorkerOnDefaultBranch { chat_id: "main".into() },
            HealthEntry::SeveralLiveChats { item_id: "task".into(), chat_ids: vec!["one".into(), "two".into()] },
            HealthEntry::MergedPrOnOpenItem { item_id: "open".into(), url: "https://github.com/example/project/pull/22".into() },
            HealthEntry::MergedPrOnOpenItem { item_id: "done".into(), url: "https://github.com/example/project/pull/23".into() },
            HealthEntry::UnlinkedPr { url: "https://github.com/example/project/pull/42".into() },
        ];
        let local = [
            TrackerCleanup::LinkIssue { item_id: "link".into(), identifier: "EX-5".into() },
            TrackerCleanup::PossibleDuplicate { item_id: "dup".into(), identifier: "EX-6".into() },
            TrackerCleanup::StaleRequest { request_id: "request".into(), chat_id: "w".into(), text: "Ready for review".into() },
        ];
        let rows = fixture.rows(&entries, &local);
        let summary: Vec<_> = rows.iter().map(|row| (row.kind, row.heading.as_str(), row.fix.as_ref().map(CleanupFix::label))).collect();
        assert_eq!(summary, [
            (CleanupKind::StaleChat, "Chat stale", Some("Archive")),
            (CleanupKind::SeveralLiveChats, "Item task · 2 live chats", None),
            (CleanupKind::WorkerOnDefaultBranch, "Chat main", None),
            (CleanupKind::MergedPr, "Item open", Some("Mark completed")),
            (CleanupKind::MergedPr, "Item done", Some("Open Linear")),
            (CleanupKind::StaleRequest, "Chat w", Some("Resolve")),
            (CleanupKind::LinkIssue, "Item link", Some("Link")),
            (CleanupKind::UnlinkedPr, "example/project#42", Some("Link PR")),
            (CleanupKind::PossibleDuplicate, "Item dup", Some("Open Linear")),
        ]);
        assert_eq!(rows[0].reason, "Idle 4 days with no open PR, and its item isn't completed. Archive it.");
        assert_eq!(rows[3].reason, "PR #22 is merged but the item is still open. Mark it completed.");
        assert_eq!(rows[4].reason, "PR #23 is merged and the item is completed, but its Linear ticket is still open. Close the ticket in Linear.");
        assert_eq!(rows[5].reason, "\"Ready for review\" is still open as a Required Action, but its PR is merged. Resolve it.");
        assert!(rows[2].reason.starts_with("Working directly on the default branch"));
        assert!(rows[7].reason.contains("isn't linked to any task"));
        assert!(rows[8].reason.contains("Nothing is changed automatically"));
    }

    #[test]
    fn several_live_chats_list_each_chat_with_its_own_kept_note() {
        let rows = Fixture::new().idle("one", 1, Some(HealthBlock::Running)).idle("two", 1, None)
            .rows(&[HealthEntry::SeveralLiveChats { item_id: "task".into(), chat_ids: vec!["one".into(), "two".into()] }], &[]);
        assert_eq!(rows[0].chats.iter().map(|chat| chat.kept).collect::<Vec<_>>(), [Some("running"), None]);
        assert_eq!(rows[0].kept.as_deref(), Some("Kept: 1 running"));
    }

    impl Fixture {
        fn facts(mut self, chat: &str, idle_seconds: i64, live_children: usize) -> Self {
            self.facts.insert(chat.into(), HealthChatFacts { idle_seconds, blocked: None, live_children });
            self
        }

        fn plan(&self, entries: &[HealthEntry], local: &[TrackerCleanup]) -> CleanAllPlan {
            let chat_title = |id: &str| format!("Chat {id}");
            let item_title = |id: &str| format!("Item {id}");
            let rows = self.rows(entries, local);
            clean_all_plan(&rows, &CleanupContext {
                chat_facts: &self.facts, chat_title: &chat_title, item_title: &item_title,
                completed_items: &self.completed, issue_urls: &self.urls, active_chat: "host",
                on_queue: &|chat| self.queued.contains(chat),
            })
        }
    }

    fn archived_ids(plan: &CleanAllPlan) -> Vec<&str> {
        plan.archive.iter().map(|(id, _)| id.as_str()).collect()
    }

    #[test]
    fn clean_all_applies_each_safe_fix_and_lists_what_it_skips() {
        let mut fixture = Fixture::new()
            .idle("idle-a", 2, None).idle("idle-b", 5, None)
            .facts("fresh", 3_600, 0)
            .idle("parent", 3, Some(HealthBlock::Coordinating))
            .idle("stale", 4, None)
            .facts("new", 600, 0).idle("old", 3, None)
            .idle("main", 1, None);
        fixture.completed.insert("done".into());
        fixture.urls.insert("done".into(), "https://linear.app/issue/EX-9".into());
        let entries = [
            on_container("idle-a"), on_container("idle-b"), on_container("fresh"), on_container("parent"),
            HealthEntry::StaleChat { chat_id: "stale".into() },
            HealthEntry::SeveralLiveChats { item_id: "task".into(), chat_ids: vec!["old".into(), "new".into()] },
            HealthEntry::WorkerOnDefaultBranch { chat_id: "main".into() },
            HealthEntry::MergedPrOnOpenItem { item_id: "open".into(), url: "https://github.com/example/project/pull/22".into() },
            HealthEntry::MergedPrOnOpenItem { item_id: "done".into(), url: "https://github.com/example/project/pull/23".into() },
            HealthEntry::UnlinkedPr { url: "https://github.com/example/project/pull/42".into() },
        ];
        let local = [
            TrackerCleanup::LinkIssue { item_id: "link".into(), identifier: "EX-5".into() },
            TrackerCleanup::PossibleDuplicate { item_id: "dup".into(), identifier: "EX-6".into() },
            TrackerCleanup::StaleRequest { request_id: "request".into(), chat_id: "w".into(), text: "Ready".into() },
        ];
        let plan = fixture.plan(&entries, &local);
        assert_eq!(archived_ids(&plan), ["idle-a", "idle-b", "stale", "old"]);
        assert_eq!((plan.complete.as_slice(), plan.resolve.as_slice()), (&["open".to_owned()][..], &["request".to_owned()][..]));
        assert_eq!(plan.summary(), "Archive 4 chats, mark 1 item completed, resolve 1 stale request");
        assert_eq!(plan.skipped, [
            "Chats kept (idle under a day): 1",
            "Chats kept (is coordinating chats that are still live): 1",
            "Chats kept (newest on their task): 1",
            "Chats on the default branch (move the work to a branch): 1",
            "Items whose Linear ticket is still open (close it in Linear): 1",
            "Unlinked PRs (link each to a task): 1",
            "Possible duplicates (check by hand): 1",
            "Tasks to link to a Linear issue (identifier unverified): 1",
        ]);
    }

    #[test]
    fn clean_all_asks_the_engine_for_the_idle_time_it_saw() {
        let plan = Fixture::new().facts("barely", 86_430, 0).idle("long", 5, None).facts("stale", 600, 0)
            .plan(&[on_container("barely"), on_container("long"), HealthEntry::StaleChat { chat_id: "stale".into() }], &[]);
        // A container chat needs a full day; the others need what they showed, less a minute.
        assert_eq!(plan.archive, [("barely".to_owned(), 86_400), ("long".to_owned(), 5 * 86_400 - 60), ("stale".to_owned(), 540)]);
    }

    #[test]
    fn clean_all_never_plans_a_blocked_chat_or_the_one_showing_the_tracker() {
        let entries: Vec<_> = ["run", "ask", "request", "queued", "role", "host", "ok"].into_iter().map(on_container).collect();
        let plan = Fixture::new()
            .idle("run", 4, Some(HealthBlock::Running)).idle("ask", 4, Some(HealthBlock::AwaitingInput))
            .idle("request", 4, Some(HealthBlock::OpenRequest)).idle("queued", 4, Some(HealthBlock::Queued))
            .idle("role", 4, Some(HealthBlock::StandingRole)).idle("host", 4, None).idle("ok", 4, None)
            .plan(&entries, &[]);
        assert_eq!(archived_ids(&plan), ["ok"]);
        assert_eq!(plan.skipped, ["Chats kept (running): 1", "Chats kept (waiting for input): 1", "Chats kept (has an open Required Action): 1",
            "Chats kept (has a queued message): 1", "Chats kept (holds a Manager or assistant role): 1", "Chats kept (this chat shows the Tracker): 1"]);
    }

    #[test]
    fn clean_all_skips_chats_whose_facts_are_unknown_and_keeps_the_newest_live_chat() {
        let plan = Fixture::new().idle("known", 4, None)
            .plan(&[on_container("known"), on_container("unknown"), HealthEntry::SeveralLiveChats { item_id: "task".into(), chat_ids: vec!["x".into(), "y".into()] }], &[]);
        assert_eq!(archived_ids(&plan), ["known"]);
        assert_eq!(plan.skipped, ["Chats kept (details unavailable): 3"]);
        let tie = Fixture::new().facts("a", 100, 0).facts("b", 100, 0)
            .plan(&[HealthEntry::SeveralLiveChats { item_id: "task".into(), chat_ids: vec!["a".into(), "b".into()] }], &[]);
        assert_eq!(archived_ids(&tie), ["b"]);
    }

    #[test]
    fn the_outcome_counts_archived_and_kept_chats_and_says_what_failed() {
        let reply = |results: &[(&str, bool, Option<&str>)]| mast_proto::WorkOutlineHealthArchiveReply {
            results: results.iter().map(|(chat_id, archived, reason)| mast_proto::HealthArchiveOutcome {
                chat_id: (*chat_id).into(), archived: *archived, reason: reason.map(Into::into),
            }).collect(),
        };
        let mut outcome = CleanupOutcome::default();
        assert_eq!(outcome.message(), "Nothing to clean up");
        outcome.record_archive(&reply(&[("a", true, None), ("b", false, Some("running")), ("c", false, Some(mast_proto::HEALTH_ALREADY_ARCHIVED))]));
        assert_eq!((outcome.archived, outcome.kept), (2, 1));
        outcome.completed = 1;
        outcome.failed = 2;
        assert_eq!(outcome.message(), "Archived 2 chats, marked 1 item completed, kept 1 chat that changed or became busy, 2 changes failed, try again");
        outcome.problem = Some("This engine is too old.".into());
        assert!(outcome.message().ends_with("try again. This engine is too old."));
    }

    #[test]
    fn clean_all_never_plans_a_chat_working_on_a_live_queued_item() {
        let mut fixture = Fixture::new().idle("queued", 5, None).idle("free", 5, None).idle("stale-queued", 5, None);
        fixture.queued.extend(["queued".to_owned(), "stale-queued".to_owned()]);
        let entries = [on_container("queued"), on_container("free"), HealthEntry::StaleChat { chat_id: "stale-queued".into() }];
        let plan = fixture.plan(&entries, &[]);
        assert_eq!(archived_ids(&plan), ["free"]);
        assert_eq!(plan.skipped, ["Chats kept (works on a live queued item): 2"]);
        // The rows say so too, so no row offers an archive for it.
        let rows = fixture.rows(&entries, &[]);
        assert!(rows.iter().filter(|row| row.chats[0].id != "free").all(|row| row.fix.is_none()));
    }

    #[test]
    fn clean_all_is_empty_when_nothing_is_safe() {
        let plan = Fixture::new().plan(&[HealthEntry::UnlinkedPr { url: "https://github.com/example/project/pull/42".into() }], &[]);
        assert!(plan.is_empty());
        assert_eq!(plan.summary(), "");
    }

    #[test]
    fn durations_use_the_largest_whole_unit() {
        assert_eq!(duration_phrase(59 * 60), "under an hour");
        assert_eq!(duration_phrase(3 * 3600), "3 hours");
        assert_eq!(duration_phrase(86_400), "1 day");
        assert_eq!(duration_phrase(3 * 86_400 + 5), "3 days");
    }
}
