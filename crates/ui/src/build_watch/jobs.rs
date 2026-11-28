//! The Space Builds panel's model: the build queue (`WatchBuildJobs` snapshots) as the UI shows
//! it. Pure: no GPUI, no clock reads (callers pass `now_ms`), so every string here is testable.
//!
//! Two consumers read it:
//! - the **Builds panel** ([`BuildJobsStore::panel`]): active jobs, recent finished jobs, the
//!   owning chat, cancel and log affordances, and the quiet-window indicator;
//! - the **chat row badge** ([`BuildJobsStore::badge`], `queued #2` / `building`) and the
//!   "idle + background process" count ([`BuildJobsStore::active_for_chat`]).
//!
//! With no jobs anywhere both lookups return before touching a map entry, so a profile that never
//! builds pays one `is_empty` check per chat row (the 250-chat Overview and sidebar fast paths).

use mast_proto::{BuildJobState, BuildJobView, BuildJobsSnapshot};
use std::collections::HashMap;

/// Finished jobs the panel lists below the active ones.
pub const RECENT_SHOWN: usize = 5;
/// Longest command text the panel shows for a job without a label.
const TITLE_MAX_CHARS: usize = 64;

/// What a chat row says about the chat's builds. Order is display priority: a chat with one
/// running job and one queued job reads `building`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatBadge {
    AwaitingApproval,
    /// Waiting for a slot; `Some(n)` is the 1-based position on the host's queue.
    Queued(Option<u32>),
    Building,
}

/// `queued #1` .. `queued #32`: sidebar labels are `&'static str`, and a Space may hold 32
/// unfinished jobs, so the whole range is a table instead of a leaked or allocated string.
const QUEUED_LABELS: [&str; 32] = [
    "queued #1",
    "queued #2",
    "queued #3",
    "queued #4",
    "queued #5",
    "queued #6",
    "queued #7",
    "queued #8",
    "queued #9",
    "queued #10",
    "queued #11",
    "queued #12",
    "queued #13",
    "queued #14",
    "queued #15",
    "queued #16",
    "queued #17",
    "queued #18",
    "queued #19",
    "queued #20",
    "queued #21",
    "queued #22",
    "queued #23",
    "queued #24",
    "queued #25",
    "queued #26",
    "queued #27",
    "queued #28",
    "queued #29",
    "queued #30",
    "queued #31",
    "queued #32",
];

impl ChatBadge {
    /// What the row says: `queued #2`, `building`, ...; a position past 32 reads `queued #32+`.
    pub fn label(self) -> &'static str {
        match self {
            Self::AwaitingApproval => "build needs approval",
            Self::Queued(Some(position)) if position as usize > QUEUED_LABELS.len() => {
                "queued #32+"
            }
            Self::Queued(Some(position)) if position > 0 => QUEUED_LABELS[position as usize - 1],
            Self::Queued(_) => "queued",
            Self::Building => "building",
        }
    }

    fn rank(self) -> (u8, u32) {
        match self {
            Self::AwaitingApproval => (0, 0),
            // A lower queue position is closer to running, so it ranks higher.
            Self::Queued(position) => (1, u32::MAX - position.unwrap_or(u32::MAX)),
            Self::Building => (2, 0),
        }
    }
}

fn badge_of(job: &BuildJobView) -> Option<ChatBadge> {
    match job.state {
        BuildJobState::AwaitingApproval => Some(ChatBadge::AwaitingApproval),
        BuildJobState::Queued => Some(ChatBadge::Queued(job.position.filter(|p| *p > 0))),
        BuildJobState::Running => Some(ChatBadge::Building),
        _ => None,
    }
}

fn is_active(state: BuildJobState) -> bool {
    matches!(
        state,
        BuildJobState::AwaitingApproval | BuildJobState::Queued | BuildJobState::Running
    )
}

/// Test-only count of per-chat map lookups, to prove the empty-store fast path never reaches one
/// (thread-local: gpui tests run on one thread).
#[cfg(test)]
pub(crate) mod counts {
    use std::cell::Cell;
    thread_local! {
        pub(crate) static MAP_LOOKUPS: Cell<u64> = const { Cell::new(0) };
    }
    pub(super) fn lookup() {
        MAP_LOOKUPS.with(|c| c.set(c.get() + 1));
    }
    pub(crate) fn lookups() -> u64 {
        MAP_LOOKUPS.with(Cell::get)
    }
}
#[cfg(not(test))]
mod counts {
    #[inline(always)]
    pub(super) fn lookup() {}
}

/// Per-Space snapshots plus the per-chat indexes derived from them.
#[derive(Debug, Default)]
pub struct BuildJobsStore {
    spaces: HashMap<String, BuildJobsSnapshot>,
    /// chat id -> (badge, unfinished job count); only chats with an unfinished job.
    chats: HashMap<String, (ChatBadge, u32)>,
}

impl BuildJobsStore {
    /// Install a Space's snapshot; whether anything changed.
    pub fn set(&mut self, space_id: &str, snapshot: BuildJobsSnapshot) -> bool {
        if self.spaces.get(space_id) == Some(&snapshot) {
            return false;
        }
        // An empty snapshot is the same as no snapshot: keep `is_empty` honest.
        if snapshot.jobs.is_empty() && !snapshot.quiet_window_active {
            let removed = self.spaces.remove(space_id).is_some();
            if removed {
                self.reindex();
            }
            return removed;
        }
        self.spaces.insert(space_id.to_owned(), snapshot);
        self.reindex();
        true
    }

    /// Keep only the Spaces `keep` accepts; whether anything was dropped.
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) -> bool {
        let before = self.spaces.len();
        self.spaces.retain(|space_id, _| keep(space_id));
        let dropped = self.spaces.len() != before;
        if dropped {
            self.reindex();
        }
        dropped
    }

    fn reindex(&mut self) {
        self.chats.clear();
        for job in self.spaces.values().flat_map(|snapshot| &snapshot.jobs) {
            let Some(badge) = badge_of(job) else { continue };
            self.chats
                .entry(job.chat_id.clone())
                .and_modify(|(best, count)| {
                    *count += 1;
                    if badge.rank() > best.rank() {
                        *best = badge;
                    }
                })
                .or_insert((badge, 1));
        }
    }

    /// No snapshot holds a job or a quiet window: nothing to draw, nothing to badge.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.spaces.is_empty()
    }

    /// The badge for one chat row. O(1), and a single `is_empty` check when nothing builds.
    pub fn badge(&self, chat_id: &str) -> Option<ChatBadge> {
        if self.chats.is_empty() {
            return None;
        }
        counts::lookup();
        self.chats.get(chat_id).map(|(badge, _)| *badge)
    }

    /// Unfinished build jobs the chat waits on: the "background process" count of an idle chat.
    pub fn active_for_chat(&self, chat_id: &str) -> u32 {
        if self.chats.is_empty() {
            return 0;
        }
        counts::lookup();
        self.chats.get(chat_id).map_or(0, |(_, count)| *count)
    }

    /// Hash of everything the panel and the row badges read; 0 while nothing builds, so the
    /// shell's repaint gate pays nothing for a profile with no jobs.
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        if self.spaces.is_empty() {
            return 0;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut spaces: Vec<_> = self.spaces.iter().collect();
        spaces.sort_by(|a, b| a.0.cmp(b.0));
        for (space_id, snapshot) in spaces {
            space_id.hash(&mut h);
            snapshot.quiet_window_active.hash(&mut h);
            for job in &snapshot.jobs {
                (
                    &job.job_id,
                    job.state as u8,
                    job.position,
                    job.started_at,
                    job.finished_at,
                    job.exit_code,
                    &job.summary,
                )
                    .hash(&mut h);
            }
        }
        h.finish() | 1
    }

    /// The panel's rows, or `None` when there is nothing to show. `chat_title` resolves the
    /// owning chat's title (the panel links to it).
    pub fn panel(
        &self,
        now_ms: i64,
        chat_title: &dyn Fn(&str) -> Option<String>,
    ) -> Option<PanelModel> {
        if self.spaces.is_empty() {
            return None;
        }
        let quiet_window = self.spaces.values().any(|s| s.quiet_window_active);
        let mut active: Vec<&BuildJobView> = Vec::new();
        let mut recent: Vec<&BuildJobView> = Vec::new();
        for job in self.spaces.values().flat_map(|snapshot| &snapshot.jobs) {
            if is_active(job.state) {
                active.push(job);
            } else {
                recent.push(job);
            }
        }
        // Running first, then the queue in order, then approvals; ties oldest first.
        active.sort_by_key(|job| {
            (
                match job.state {
                    BuildJobState::Running => 0,
                    BuildJobState::Queued => 1,
                    _ => 2,
                },
                job.position.unwrap_or(u32::MAX),
                job.created_at,
            )
        });
        recent.sort_by_key(|job| std::cmp::Reverse(job.finished_at.unwrap_or(job.created_at)));
        recent.truncate(RECENT_SHOWN);
        let row = |job: &BuildJobView| row_of(job, now_ms, chat_title(&job.chat_id));
        let model = PanelModel {
            quiet_window,
            active: active.into_iter().map(row).collect(),
            recent: recent.into_iter().map(row).collect(),
        };
        (!model.active.is_empty() || !model.recent.is_empty() || quiet_window).then_some(model)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Building,
    Waiting,
    Good,
    Bad,
    Muted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelRow {
    pub job_id: String,
    pub chat_id: String,
    /// The owning chat's title, when the chat is known on this device.
    pub chat_title: Option<String>,
    /// The job's label, else its command.
    pub title: String,
    /// `queued #2`, `building`, `passed`, `failed · exit 101`, ...
    pub status: String,
    pub tone: Tone,
    /// `waiting 45s`, `running 3m 12s`, `took 4m 02s`.
    pub timing: String,
    /// The first line of the failure report or reason, when there is one.
    pub detail: Option<String>,
    pub can_cancel: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelModel {
    /// A quiet window holds the host: queued jobs wait, running jobs drain.
    pub quiet_window: bool,
    pub active: Vec<PanelRow>,
    pub recent: Vec<PanelRow>,
}

impl PanelModel {
    /// `2 building · 1 queued`, `Idle`, ... plus the quiet-window note.
    pub fn headline(&self) -> String {
        let count = |tone: Tone| self.active.iter().filter(|row| row.tone == tone).count();
        let (building, waiting) = (count(Tone::Building), count(Tone::Waiting));
        let mut parts = Vec::new();
        if building > 0 {
            parts.push(format!("{building} building"));
        }
        if waiting > 0 {
            parts.push(format!("{waiting} waiting"));
        }
        if parts.is_empty() {
            parts.push("No builds running".to_owned());
        }
        if self.quiet_window {
            parts.push("quiet window".to_owned());
        }
        parts.join(" \u{b7} ")
    }
}

/// `1h 02m`, `4m 02s`, `45s`.
pub fn fmt_secs(secs: u64) -> String {
    match (secs / 3600, secs / 60 % 60, secs % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m {s:02}s"),
        (h, m, _) => format!("{h}h {m:02}m"),
    }
}

fn secs_between(from_ms: i64, to_ms: i64) -> u64 {
    (to_ms - from_ms).max(0) as u64 / 1000
}

fn title_of(job: &BuildJobView) -> String {
    let text = job
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| job.argv.join(" "));
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= TITLE_MAX_CHARS {
        return text;
    }
    let cut: String = text.chars().take(TITLE_MAX_CHARS - 1).collect();
    format!("{cut}\u{2026}")
}

fn row_of(job: &BuildJobView, now_ms: i64, chat_title: Option<String>) -> PanelRow {
    let (status, tone) = match job.state {
        BuildJobState::AwaitingApproval => ("needs approval".to_owned(), Tone::Waiting),
        BuildJobState::Queued => (
            ChatBadge::Queued(job.position.filter(|p| *p > 0))
                .label()
                .to_owned(),
            Tone::Waiting,
        ),
        BuildJobState::Running => ("building".to_owned(), Tone::Building),
        BuildJobState::Succeeded => ("passed".to_owned(), Tone::Good),
        BuildJobState::Failed => (
            match (job.exit_code, job.signal) {
                (Some(code), _) => format!("failed \u{b7} exit {code}"),
                (None, Some(signal)) => format!("failed \u{b7} signal {signal}"),
                (None, None) => "failed".to_owned(),
            },
            Tone::Bad,
        ),
        BuildJobState::Cancelled => ("cancelled".to_owned(), Tone::Muted),
        BuildJobState::TimedOut => ("timed out".to_owned(), Tone::Bad),
        BuildJobState::Lost => ("lost".to_owned(), Tone::Bad),
    };
    let timing = match job.state {
        BuildJobState::AwaitingApproval | BuildJobState::Queued => {
            format!("waiting {}", fmt_secs(secs_between(job.created_at, now_ms)))
        }
        BuildJobState::Running => format!(
            "running {}",
            fmt_secs(secs_between(
                job.started_at.unwrap_or(job.created_at),
                now_ms
            ))
        ),
        _ => match (job.started_at, job.finished_at) {
            (Some(started), Some(finished)) => {
                format!("took {}", fmt_secs(secs_between(started, finished)))
            }
            _ => String::new(),
        },
    };
    // The failure report of a finished job, or the reason an unfinished one is not moving
    // (`host unreachable; retrying`, `waiting for cargo lock`).
    let detail = (job.state != BuildJobState::Succeeded)
        .then(|| {
            job.summary
                .as_deref()
                .and_then(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
                .map(|line| line.chars().take(120).collect::<String>())
        })
        .flatten();
    PanelRow {
        job_id: job.job_id.clone(),
        chat_id: job.chat_id.clone(),
        chat_title,
        title: title_of(job),
        status,
        tone,
        timing,
        detail,
        can_cancel: matches!(
            job.state,
            BuildJobState::AwaitingApproval | BuildJobState::Queued | BuildJobState::Running
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, chat: &str, state: BuildJobState) -> BuildJobView {
        BuildJobView {
            job_id: id.into(),
            space_id: "space".into(),
            chat_id: chat.into(),
            argv: vec!["cargo".into(), "test".into(), "-p".into(), "mast-ui".into()],
            state,
            created_at: 1_000,
            ..Default::default()
        }
    }

    fn queued(id: &str, chat: &str, position: u32) -> BuildJobView {
        BuildJobView {
            position: Some(position),
            ..job(id, chat, BuildJobState::Queued)
        }
    }

    fn snapshot(jobs: Vec<BuildJobView>) -> BuildJobsSnapshot {
        BuildJobsSnapshot {
            space_id: "space".into(),
            jobs,
            quiet_window_active: false,
        }
    }

    fn titles(_: &str) -> Option<String> {
        Some("Fix the sidebar".into())
    }

    #[test]
    fn an_empty_store_has_no_badges_counts_or_panel() {
        let store = BuildJobsStore::default();
        assert!(store.is_empty());
        assert_eq!(store.badge("chat"), None);
        assert_eq!(store.active_for_chat("chat"), 0);
        assert_eq!(store.panel(0, &titles), None);
    }

    #[test]
    fn with_no_jobs_a_row_lookup_never_reaches_the_map_and_the_fingerprint_is_zero() {
        let before = counts::lookups();
        let mut store = BuildJobsStore::default();
        for n in 0..250 {
            let chat = format!("chat-{n}");
            assert_eq!(store.badge(&chat), None);
            assert_eq!(store.active_for_chat(&chat), 0);
        }
        assert_eq!(counts::lookups(), before, "250 chat rows, 0 map lookups");
        assert_eq!(store.fingerprint(), 0);

        // Finished jobs only: still nothing per chat to look up.
        store.set(
            "space",
            snapshot(vec![job("j", "chat-1", BuildJobState::Succeeded)]),
        );
        assert_eq!(store.badge("chat-1"), None);
        assert_eq!(counts::lookups(), before);

        // An unfinished job turns lookups on, and the fingerprint with them.
        store.set("space", snapshot(vec![queued("q", "chat-1", 1)]));
        assert_ne!(store.fingerprint(), 0);
        let _ = store.badge("chat-2");
        assert_eq!(counts::lookups(), before + 1);
    }

    #[test]
    fn an_empty_snapshot_leaves_the_store_empty() {
        let mut store = BuildJobsStore::default();
        assert!(!store.set("space", snapshot(vec![])));
        assert!(store.is_empty());
        assert!(store.set("space", snapshot(vec![queued("j1", "a", 1)])));
        assert!(!store.is_empty());
        assert!(store.set("space", snapshot(vec![])));
        assert!(store.is_empty());
        assert_eq!(store.badge("a"), None);
    }

    #[test]
    fn a_queued_chat_reads_queued_with_its_position_and_a_running_one_reads_building() {
        let mut store = BuildJobsStore::default();
        store.set(
            "space",
            snapshot(vec![
                job("j1", "runner", BuildJobState::Running),
                queued("j2", "second", 1),
                queued("j3", "third", 2),
            ]),
        );
        assert_eq!(store.badge("runner").unwrap().label(), "building");
        assert_eq!(store.badge("second").unwrap().label(), "queued #1");
        assert_eq!(store.badge("third").unwrap().label(), "queued #2");
        assert_eq!(store.badge("stranger"), None);
    }

    #[test]
    fn finished_jobs_leave_no_badge_and_no_background_count() {
        let mut store = BuildJobsStore::default();
        store.set(
            "space",
            snapshot(vec![
                job("j1", "a", BuildJobState::Succeeded),
                job("j2", "a", BuildJobState::Failed),
            ]),
        );
        assert_eq!(store.badge("a"), None);
        assert_eq!(store.active_for_chat("a"), 0);
    }

    #[test]
    fn a_chat_with_several_jobs_shows_the_most_advanced_one_and_counts_them_all() {
        let mut store = BuildJobsStore::default();
        store.set(
            "space",
            snapshot(vec![
                queued("j1", "a", 3),
                queued("j2", "a", 2),
                job("j3", "a", BuildJobState::Running),
                job("j4", "a", BuildJobState::Cancelled),
            ]),
        );
        assert_eq!(store.badge("a"), Some(ChatBadge::Building));
        assert_eq!(store.active_for_chat("a"), 3);

        store.set(
            "space",
            snapshot(vec![queued("j1", "a", 3), queued("j2", "a", 2)]),
        );
        assert_eq!(store.badge("a").unwrap().label(), "queued #2");
    }

    #[test]
    fn a_queued_job_without_a_position_reads_plain_queued() {
        let mut store = BuildJobsStore::default();
        store.set(
            "space",
            snapshot(vec![job("j1", "a", BuildJobState::Queued)]),
        );
        assert_eq!(store.badge("a").unwrap().label(), "queued");
    }

    #[test]
    fn dropping_a_space_drops_its_badges() {
        let mut store = BuildJobsStore::default();
        store.set("space", snapshot(vec![queued("j1", "a", 1)]));
        store.set(
            "other",
            snapshot(vec![job("j2", "b", BuildJobState::Running)]),
        );
        assert!(store.retain(|space| space == "other"));
        assert_eq!(store.badge("a"), None);
        assert_eq!(store.badge("b"), Some(ChatBadge::Building));
        assert!(!store.retain(|space| space == "other"));
    }

    #[test]
    fn the_panel_lists_running_then_the_queue_in_order_then_recent_finished() {
        let mut store = BuildJobsStore::default();
        let mut done = job("done", "a", BuildJobState::Succeeded);
        done.started_at = Some(2_000);
        done.finished_at = Some(125_000);
        let mut running = job("run", "b", BuildJobState::Running);
        running.started_at = Some(10_000);
        store.set(
            "space",
            snapshot(vec![
                done,
                queued("q2", "d", 2),
                running,
                queued("q1", "c", 1),
            ]),
        );
        let panel = store.panel(73_000, &titles).unwrap();
        let ids: Vec<_> = panel.active.iter().map(|row| row.job_id.as_str()).collect();
        assert_eq!(ids, ["run", "q1", "q2"]);
        assert_eq!(panel.active[0].status, "building");
        assert_eq!(panel.active[0].timing, "running 1m 03s");
        assert_eq!(panel.active[1].status, "queued #1");
        assert_eq!(panel.active[1].timing, "waiting 1m 12s");
        assert_eq!(panel.recent.len(), 1);
        assert_eq!(panel.recent[0].status, "passed");
        assert_eq!(panel.recent[0].timing, "took 2m 03s");
        assert_eq!(panel.headline(), "1 building \u{b7} 2 waiting");
    }

    #[test]
    fn only_unfinished_jobs_can_be_cancelled() {
        let mut store = BuildJobsStore::default();
        store.set(
            "space",
            snapshot(vec![
                job("run", "a", BuildJobState::Running),
                queued("q", "a", 1),
                job("ok", "a", BuildJobState::Succeeded),
                job("bad", "a", BuildJobState::Failed),
            ]),
        );
        let panel = store.panel(0, &titles).unwrap();
        assert!(panel.active.iter().all(|row| row.can_cancel));
        assert!(panel.recent.iter().all(|row| !row.can_cancel));
    }

    #[test]
    fn a_failed_job_shows_its_exit_code_and_the_first_line_of_its_report() {
        let mut failed = job("j", "a", BuildJobState::Failed);
        failed.exit_code = Some(101);
        failed.summary = Some("\nerror[E0308]: mismatched types\nmore".into());
        let row = row_of(&failed, 0, None);
        assert_eq!(row.status, "failed \u{b7} exit 101");
        assert_eq!(row.tone, Tone::Bad);
        assert_eq!(
            row.detail.as_deref(),
            Some("error[E0308]: mismatched types")
        );

        let mut killed = job("k", "a", BuildJobState::Failed);
        killed.signal = Some(9);
        assert_eq!(row_of(&killed, 0, None).status, "failed \u{b7} signal 9");
    }

    #[test]
    fn a_running_job_shows_why_it_is_not_moving() {
        let mut running = job("j", "a", BuildJobState::Running);
        running.summary = Some("host unreachable; retrying".into());
        assert_eq!(
            row_of(&running, 0, None).detail.as_deref(),
            Some("host unreachable; retrying")
        );
        let mut passed = job("p", "a", BuildJobState::Succeeded);
        passed.summary = Some("all good".into());
        assert_eq!(row_of(&passed, 0, None).detail, None);
    }

    #[test]
    fn recent_jobs_are_capped_newest_first() {
        let mut store = BuildJobsStore::default();
        let jobs = (0..8)
            .map(|n| BuildJobView {
                finished_at: Some(1_000 + n),
                ..job(&format!("j{n}"), "a", BuildJobState::Succeeded)
            })
            .collect();
        store.set("space", snapshot(jobs));
        let panel = store.panel(0, &titles).unwrap();
        let ids: Vec<_> = panel.recent.iter().map(|row| row.job_id.clone()).collect();
        assert_eq!(ids, ["j7", "j6", "j5", "j4", "j3"]);
    }

    #[test]
    fn the_quiet_window_shows_in_the_headline_even_with_no_jobs() {
        let mut store = BuildJobsStore::default();
        store.set(
            "space",
            BuildJobsSnapshot {
                space_id: "space".into(),
                jobs: vec![],
                quiet_window_active: true,
            },
        );
        assert!(!store.is_empty());
        let panel = store.panel(0, &titles).unwrap();
        assert!(panel.quiet_window);
        assert_eq!(panel.headline(), "No builds running \u{b7} quiet window");
    }

    #[test]
    fn the_title_is_the_label_else_the_command_and_long_ones_are_cut() {
        let mut labelled = job("j", "a", BuildJobState::Running);
        labelled.label = Some("  mast-ui   tests ".into());
        assert_eq!(title_of(&labelled), "mast-ui tests");
        assert_eq!(
            title_of(&job("j", "a", BuildJobState::Running)),
            "cargo test -p mast-ui"
        );
        let mut long = job("j", "a", BuildJobState::Running);
        long.label = Some("x".repeat(200));
        let cut = title_of(&long);
        assert_eq!(cut.chars().count(), TITLE_MAX_CHARS);
        assert!(cut.ends_with('\u{2026}'));
    }

    #[test]
    fn durations_read_as_seconds_minutes_or_hours() {
        assert_eq!(fmt_secs(0), "0s");
        assert_eq!(fmt_secs(45), "45s");
        assert_eq!(fmt_secs(242), "4m 02s");
        assert_eq!(fmt_secs(3720), "1h 02m");
    }

    #[test]
    fn the_panel_resolves_the_owning_chat_title() {
        let mut store = BuildJobsStore::default();
        store.set("space", snapshot(vec![queued("q", "chat-1", 1)]));
        let panel = store
            .panel(0, &|id| {
                (id == "chat-1").then(|| "Fix the sidebar".to_owned())
            })
            .unwrap();
        assert_eq!(
            panel.active[0].chat_title.as_deref(),
            Some("Fix the sidebar")
        );
        assert_eq!(panel.active[0].chat_id, "chat-1");
    }
}
