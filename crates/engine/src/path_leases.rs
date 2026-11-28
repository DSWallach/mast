//! Path leases: at most [`MAX_CONCURRENT_LEASES`] workers per project run at
//! once, and only when their `ownedPaths` are disjoint.
//!
//! A worker launched for a work item whose task spec names `ownedPaths` holds
//! a lease on them until it stops being an active worker. A launch that would
//! overlap a held lease, take a second hot-path lease, or exceed the cap is
//! refused with a [`LeaseWait`] naming the holder, so the caller queues it
//! behind that holder instead of starting a second writer on the same files.
//!
//! The registry is built per decision from the Work Outline snapshot
//! ([`active_holders`]); nothing is stored, so a crash or restart can never
//! leave a lease behind. Two launches are serialized by the launch gate the
//! caller holds.
//!
//! Path matching is conservative: a path with no glob covers everything under
//! it, `dir/**` and `dir/*` cover the directory, `{a,b}` expands, and a glob
//! segment overlaps any segment it could match. Whenever two patterns might
//! name the same file they are treated as overlapping.

use std::collections::HashMap;

use mast_doc::WorkOutlineInputs;
use mast_proto::{
    ComplexityTier, SandboxLevel, StandingRoleKind, TaskRole, WorkItemTaskSpec, WorkItemView, WorkOutlineSnapshot,
};

/// Workers that may hold a lease in one project at the same time.
pub const MAX_CONCURRENT_LEASES: usize = 4;

/// Most `ownedPaths` a `DirectSingleAgent` task may name.
pub const DIRECT_SINGLE_AGENT_MAX_PATHS: usize = 2;

/// Paths every worker in a project contends on. Only one lease at a time may
/// touch them: `Cargo.lock`, the workspace `Cargo.toml`, the proto crate, the
/// tool catalog, the app entry point, and any crate root `lib.rs`.
pub const HOT_PATHS: &[&str] = &[
    "Cargo.lock",
    "Cargo.toml",
    "crates/proto/**",
    "crates/mcp/src/tools/tool_catalog.rs",
    "apps/mast/src/main.rs",
    "crates/*/src/lib.rs",
];

/// Why a launch cannot take its lease now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseWaitReason {
    /// `path` overlaps `held_path`, which the holder owns.
    OverlappingPath { path: String, held_path: String },
    /// The launch touches a hot path and the holder already has the hot lease.
    HotPath { path: String },
    /// [`MAX_CONCURRENT_LEASES`] leases are held.
    Capacity,
}

/// A refused lease: wait for `holder` (a work item id) to finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseWait {
    pub holder: String,
    pub reason: LeaseWaitReason,
}

impl std::fmt::Display for LeaseWait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reason {
            LeaseWaitReason::OverlappingPath { path, held_path } => write!(
                f,
                "queued behind {}: ownedPaths `{path}` overlaps `{held_path}`",
                self.holder
            ),
            LeaseWaitReason::HotPath { path } => write!(
                f,
                "queued behind {}: only one worker at a time may own hot path `{path}`",
                self.holder
            ),
            LeaseWaitReason::Capacity => write!(
                f,
                "queued behind {}: {MAX_CONCURRENT_LEASES} workers already hold leases in this project",
                self.holder
            ),
        }
    }
}

/// One active worker's claim: the work item and the paths it owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub holder: String,
    pub owned_paths: Vec<String>,
    /// The task spec said it touches hot paths, whatever its paths are.
    pub touches_hot_paths: bool,
}

impl Lease {
    pub fn from_spec(holder: &str, spec: &WorkItemTaskSpec) -> Self {
        Self {
            holder: holder.to_owned(),
            owned_paths: spec.owned_paths.clone(),
            touches_hot_paths: spec.touches_hot_paths,
        }
    }

    /// The hot path this lease touches, if any (the declared flag counts too).
    fn hot_path(&self) -> Option<String> {
        self.owned_paths
            .iter()
            .find(|path| HOT_PATHS.iter().any(|hot| names_hot_path(path, hot)))
            .cloned()
            .or_else(|| self.touches_hot_paths.then(|| "(declared touchesHotPaths)".to_owned()))
    }
}

/// The held leases of one project.
#[derive(Debug, Default, Clone)]
pub struct PathLeaseRegistry {
    held: Vec<Lease>,
}

impl PathLeaseRegistry {
    pub fn new(held: impl IntoIterator<Item = Lease>) -> Self {
        Self { held: held.into_iter().collect() }
    }

    pub fn held(&self) -> &[Lease] {
        &self.held
    }

    /// Whether `candidate` can start now. A holder asking again for its own
    /// lease is granted (a retried launch). Checks run in this order: overlap
    /// with a holder, a second hot lease, the cap.
    pub fn check(&self, candidate: &Lease) -> Result<(), LeaseWait> {
        let others = || self.held.iter().filter(|lease| lease.holder != candidate.holder);
        for lease in others() {
            for path in &candidate.owned_paths {
                if let Some(held_path) = lease.owned_paths.iter().find(|held| paths_overlap(path, held)) {
                    return Err(LeaseWait {
                        holder: lease.holder.clone(),
                        reason: LeaseWaitReason::OverlappingPath {
                            path: path.clone(),
                            held_path: held_path.clone(),
                        },
                    });
                }
            }
        }
        if let Some(path) = candidate.hot_path()
            && let Some(lease) = others().find(|lease| lease.hot_path().is_some())
        {
            return Err(LeaseWait { holder: lease.holder.clone(), reason: LeaseWaitReason::HotPath { path } });
        }
        if others().count() >= MAX_CONCURRENT_LEASES
            && let Some(first) = others().next()
        {
            return Err(LeaseWait { holder: first.holder.clone(), reason: LeaseWaitReason::Capacity });
        }
        Ok(())
    }
}

/// Whether a task is small enough for the `DirectSingleAgent` fast path: one
/// worker, no Lead. Trivial complexity, one or two owned files, no hot paths.
pub fn direct_single_agent_eligible(spec: &WorkItemTaskSpec) -> bool {
    spec.task_class.complexity == ComplexityTier::Trivial
        && !spec.owned_paths.is_empty()
        && spec.owned_paths.len() <= DIRECT_SINGLE_AGENT_MAX_PATHS
        && !spec.touches_hot_paths
        && Lease::from_spec("", spec).hot_path().is_none()
}

/// How a launched task is run. `DirectSingleAgent` is one worker and no Lead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchScale {
    DirectSingleAgent,
    Standard,
}

pub fn launch_scale(spec: Option<&WorkItemTaskSpec>) -> LaunchScale {
    if spec.is_some_and(direct_single_agent_eligible) {
        LaunchScale::DirectSingleAgent
    } else {
        LaunchScale::Standard
    }
}

/// Whether a launch gets a pre-created worktree: only a `WorkspaceWrite`
/// implementation worker. A standing role (Manager, Lead), a `ReadOnly` or
/// full-access chat, and a task whose role is not implementation (Reviewer,
/// Lead integrator, architect, `SpecAuditor`, advisor) start in the shared
/// checkout, so none of them waits on `git worktree add` before turn 1. A
/// launch with no task spec is an ordinary worker.
pub fn provisions_worktree(
    sandbox: SandboxLevel,
    standing_role: Option<StandingRoleKind>,
    spec: Option<&WorkItemTaskSpec>,
) -> bool {
    sandbox == SandboxLevel::WorkspaceWrite
        && standing_role.is_none()
        && spec.is_none_or(|spec| {
            matches!(spec.task_class.role, TaskRole::Implementer | TaskRole::TestAuthor | TaskRole::DocWriter)
        })
}

/// The task spec of every live work item, read from the rows the outline is
/// built from (the snapshot itself does not carry it).
pub fn task_specs(inputs: &WorkOutlineInputs) -> HashMap<String, WorkItemTaskSpec> {
    inputs
        .items
        .iter()
        .filter(|row| !row.deleted)
        .filter_map(|row| {
            let spec = serde_json::from_value(row.fields.get("taskSpec")?.clone()).ok()?;
            Some((row.id.clone(), spec))
        })
        .collect()
}

/// The leases currently held in the project of `item_id` (the Space's
/// unprojected items when it has none): every other item of that scope with a
/// task spec that names `ownedPaths`, that is not done or archived, and that
/// has a worker chat which is not archived. `spec_of` reads an item's task
/// spec; `has_live_worker` says whether a chat is a live worker.
pub fn active_holders(
    snapshot: &WorkOutlineSnapshot,
    item_id: &str,
    spec_of: &dyn Fn(&str) -> Option<WorkItemTaskSpec>,
    has_live_worker: &dyn Fn(&str) -> bool,
) -> Vec<Lease> {
    // Items outside any project share one scope, so an unprojected Space still
    // keeps its writers apart.
    let project = snapshot.project_root(item_id).map(|root| root.id.as_str());
    let mut holders: Vec<&WorkItemView> = snapshot
        .items
        .iter()
        .filter(|other| {
            other.id != item_id
                && other.archived_at.is_none()
                && other.status != mast_proto::WorkItemStatus::Completed
                && !snapshot.handed_back_work_item_ids.iter().any(|id| id == &other.id)
                && snapshot.project_root(&other.id).map(|root| root.id.as_str()) == project
        })
        .collect();
    holders.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    holders
        .into_iter()
        .filter(|other| snapshot.listed_chat_ids(other).any(|chat| has_live_worker(chat)))
        .filter_map(|other| {
            let spec = spec_of(&other.id)?;
            (!spec.owned_paths.is_empty()).then(|| Lease::from_spec(&other.id, &spec))
        })
        .collect()
}

// ── path matching ──

/// `ownedPaths` entries expanded and split into segments. A bare directory
/// and `dir/**` cover the same files, so trailing `**`/`*` segments drop.
fn expand(pattern: &str) -> Vec<Vec<String>> {
    expand_braces(pattern.trim())
        .into_iter()
        .map(|path| {
            let mut segments: Vec<String> = path
                .split('/')
                .filter(|segment| !segment.is_empty() && *segment != ".")
                .map(str::to_owned)
                .collect();
            while segments.last().is_some_and(|last| last == "**" || last == "*") {
                segments.pop();
            }
            segments
        })
        .collect()
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let mut depth = 0usize;
    let mut close = None;
    for (offset, ch) in pattern[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    // An unbalanced brace is a literal.
    let Some(close) = close else {
        return vec![pattern.to_owned()];
    };
    let (head, body, tail) = (&pattern[..open], &pattern[open + 1..close], &pattern[close + 1..]);
    let (mut options, mut depth, mut start) = (Vec::new(), 0usize, 0usize);
    for (offset, ch) in body.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                options.push(&body[start..offset]);
                start = offset + 1;
            }
            _ => {}
        }
    }
    options.push(&body[start..]);
    options
        .into_iter()
        .flat_map(|option| expand_braces(&format!("{head}{option}{tail}")))
        .collect()
}

/// Whether an `ownedPaths` entry names `hot` or something under it. An entry
/// that is a broader directory (`crates/engine/**` holds a `lib.rs`) is not hot
/// by itself, or no two crate-wide owners could ever run together; such a task
/// sets `touchesHotPaths` to claim the hot lease.
fn names_hot_path(path: &str, hot: &str) -> bool {
    let hot_depth = expand(hot).iter().map(Vec::len).min().unwrap_or(0);
    expand(path).iter().any(|owned| owned.len() >= hot_depth && expand(hot).iter().any(|hot| segments_overlap(owned, hot)))
}

/// Whether two `ownedPaths` entries may name a common file.
pub fn paths_overlap(a: &str, b: &str) -> bool {
    let (a, b) = (expand(a), expand(b));
    a.iter().any(|a| b.iter().any(|b| segments_overlap(a, b)))
}

/// Whether every file `inner` may name lies under `outer`. Conservative: a
/// glob segment only matches the same glob text, so unsure means not covered.
pub fn path_covered_by(inner: &str, outer: &str) -> bool {
    let (inner, outer) = (expand(inner), expand(outer));
    inner
        .iter()
        .all(|i| outer.iter().any(|o| o.len() <= i.len() && o.iter().zip(i).all(|(a, b)| a == b)))
}

fn segments_overlap(a: &[String], b: &[String]) -> bool {
    // The shorter one covers everything under it, so only the shared prefix
    // has to agree.
    for (x, y) in a.iter().zip(b) {
        if x == "**" || y == "**" {
            return true;
        }
        if !segment_overlap(x, y) {
            return false;
        }
    }
    true
}

fn is_glob(segment: &str) -> bool {
    segment.contains(['*', '?', '['])
}

fn segment_overlap(x: &str, y: &str) -> bool {
    match (is_glob(x), is_glob(y)) {
        (false, false) => x == y,
        (true, false) => glob_match(x, y),
        (false, true) => glob_match(y, x),
        (true, true) => true,
    }
}

/// `*` (any run) and `?` (any one char) within a single segment.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match p.split_first() {
            None => t.is_empty(),
            Some(('*', rest)) => (0..=t.len()).any(|skip| go(rest, &t[skip..])),
            Some(('?', rest)) => t.split_first().is_some_and(|(_, tail)| go(rest, tail)),
            Some((&c, rest)) => t.split_first().is_some_and(|(&h, tail)| h == c && go(rest, tail)),
        }
    }
    go(&pattern.chars().collect::<Vec<_>>(), &text.chars().collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(holder: &str, paths: &[&str]) -> Lease {
        Lease {
            holder: holder.into(),
            owned_paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            touches_hot_paths: false,
        }
    }

    #[test]
    fn disjoint_directories_and_files_do_not_overlap() {
        assert!(!paths_overlap("crates/engine/src/path_leases.rs", "crates/engine/src/work_outline_launch.rs"));
        assert!(!paths_overlap("crates/ui/**", "crates/engine/**"));
        assert!(!paths_overlap("docs/guide/orchestrating.md", "docs/guide/accounts.md"));
        assert!(!paths_overlap("crates/engine/src/{a.rs,b.rs}", "crates/engine/src/c.rs"));
    }

    #[test]
    fn a_directory_and_what_is_under_it_overlap() {
        assert!(paths_overlap("crates/engine", "crates/engine/src/lib.rs"));
        assert!(paths_overlap("crates/engine/**", "crates/engine/src/x.rs"));
        assert!(paths_overlap("crates/engine/src/*.rs", "crates/engine/src/lib.rs"));
        assert!(paths_overlap("crates/engine/src/{a.rs,b.rs}", "crates/engine/src/b.rs"));
        assert!(paths_overlap("./crates/engine/", "crates/engine/src"));
        assert!(paths_overlap("**", "anything/at/all"));
        assert!(paths_overlap("crates/**/mod.rs", "crates/ui/src/mod.rs"));
    }

    #[test]
    fn glob_segments_overlap_only_what_they_could_match() {
        assert!(!paths_overlap("crates/*/src/lib.rs", "crates/engine/src/other.rs"));
        assert!(paths_overlap("crates/*/src/lib.rs", "crates/engine/src/lib.rs"));
        assert!(!paths_overlap("crates/engine/src/*.rs", "crates/engine/tests/x.rs"));
    }

    #[test]
    fn four_disjoint_workers_run_and_a_fifth_waits_on_capacity() {
        let held: Vec<Lease> = (0..MAX_CONCURRENT_LEASES)
            .map(|n| lease(&format!("w{n}"), &[&format!("crates/c{n}/**")]))
            .collect();
        let registry = PathLeaseRegistry::new(held[..MAX_CONCURRENT_LEASES - 1].to_vec());
        assert_eq!(registry.check(&held[MAX_CONCURRENT_LEASES - 1]), Ok(()));
        let full = PathLeaseRegistry::new(held);
        let wait = full.check(&lease("w9", &["crates/other/**"])).unwrap_err();
        assert_eq!(wait.reason, LeaseWaitReason::Capacity);
        assert_eq!(wait.holder, "w0");
    }

    #[test]
    fn an_overlapping_path_queues_behind_its_holder() {
        let registry = PathLeaseRegistry::new([lease("w1", &["crates/engine/src/{a.rs,b.rs}"])]);
        let wait = registry.check(&lease("w2", &["crates/engine/src/b.rs"])).unwrap_err();
        assert_eq!(wait.holder, "w1");
        assert!(matches!(wait.reason, LeaseWaitReason::OverlappingPath { .. }));
        assert!(wait.to_string().starts_with("queued behind w1"));
        assert_eq!(registry.check(&lease("w2", &["crates/engine/src/c.rs"])), Ok(()));
    }

    #[test]
    fn a_second_hot_path_ticket_queues_behind_the_first() {
        let registry = PathLeaseRegistry::new([lease("w1", &["Cargo.lock"])]);
        let wait = registry.check(&lease("w2", &["crates/proto/src/work_outline.rs"])).unwrap_err();
        assert_eq!(wait, LeaseWait {
            holder: "w1".into(),
            reason: LeaseWaitReason::HotPath { path: "crates/proto/src/work_outline.rs".into() },
        });
        // The declared flag counts even when the paths look harmless.
        let mut flagged = lease("w3", &["docs/x.md"]);
        flagged.touches_hot_paths = true;
        assert!(matches!(registry.check(&flagged).unwrap_err().reason, LeaseWaitReason::HotPath { .. }));
        // A non-hot disjoint ticket runs beside the hot holder.
        assert_eq!(registry.check(&lease("w4", &["docs/x.md"])), Ok(()));
    }

    #[test]
    fn crate_root_lib_rs_and_the_tool_catalog_are_hot() {
        for hot in ["crates/engine/src/lib.rs", "crates/mcp/src/tools/tool_catalog.rs", "Cargo.toml", "apps/mast/src/main.rs"] {
            assert!(lease("w", &[hot]).hot_path().is_some(), "{hot}");
        }
        for cold in ["crates/engine/src/work_outline_launch.rs", "crates/engine/Cargo.toml", "docs/guide/orchestrating.md"] {
            assert!(lease("w", &[cold]).hot_path().is_none(), "{cold}");
        }
    }

    #[test]
    fn a_crate_wide_directory_is_not_hot_unless_it_declares_so() {
        assert!(lease("w", &["crates/engine/**"]).hot_path().is_none());
        assert!(lease("w", &["crates/engine/src/*.rs"]).hot_path().is_some(), "could be lib.rs");
        let mut declared = lease("w", &["crates/engine/**"]);
        declared.touches_hot_paths = true;
        assert!(declared.hot_path().is_some());
    }

    #[test]
    fn a_holder_asking_again_is_granted() {
        let registry = PathLeaseRegistry::new([lease("w1", &["crates/engine/src/a.rs"])]);
        assert_eq!(registry.check(&lease("w1", &["crates/engine/src/a.rs"])), Ok(()));
    }

    #[test]
    fn direct_single_agent_needs_a_trivial_small_cold_task() {
        let mut spec = WorkItemTaskSpec::default();
        spec.task_class.complexity = ComplexityTier::Trivial;
        spec.owned_paths = vec!["docs/a.md".into(), "docs/b.md".into()];
        assert!(direct_single_agent_eligible(&spec));
        spec.owned_paths.push("docs/c.md".into());
        assert!(!direct_single_agent_eligible(&spec), "three files");
        spec.owned_paths = vec!["Cargo.lock".into()];
        assert!(!direct_single_agent_eligible(&spec), "hot path");
        spec.owned_paths = vec!["docs/a.md".into()];
        spec.touches_hot_paths = true;
        assert!(!direct_single_agent_eligible(&spec), "declared hot");
        spec.touches_hot_paths = false;
        spec.task_class.complexity = ComplexityTier::Standard;
        assert!(!direct_single_agent_eligible(&spec), "not trivial");
        spec.task_class.complexity = ComplexityTier::Trivial;
        spec.owned_paths.clear();
        assert!(!direct_single_agent_eligible(&spec), "no paths");
    }

    #[test]
    fn only_a_workspace_write_implementation_worker_gets_a_worktree() {
        use mast_proto::TaskRole::*;
        let spec = |role| {
            let mut spec = WorkItemTaskSpec::default();
            spec.task_class.role = role;
            spec
        };
        for role in [Implementer, TestAuthor, DocWriter] {
            assert!(provisions_worktree(SandboxLevel::WorkspaceWrite, None, Some(&spec(role))), "{role:?}");
        }
        for role in [Reviewer, LeadIntegrator, ArchitectPlanner, SpecAuditor] {
            assert!(!provisions_worktree(SandboxLevel::WorkspaceWrite, None, Some(&spec(role))), "{role:?}");
        }
        assert!(provisions_worktree(SandboxLevel::WorkspaceWrite, None, None), "no spec is an ordinary worker");
        assert!(!provisions_worktree(SandboxLevel::ReadOnly, None, Some(&spec(Implementer))));
        assert!(!provisions_worktree(SandboxLevel::DangerFullAccess, None, None));
        for kind in [StandingRoleKind::Manager, StandingRoleKind::Lead] {
            assert!(!provisions_worktree(SandboxLevel::WorkspaceWrite, Some(kind), None), "{kind:?}");
        }
    }

    #[test]
    fn launch_scale_marks_only_trivial_small_cold_tasks() {
        let mut spec = WorkItemTaskSpec::default();
        assert_eq!(launch_scale(Some(&spec)), LaunchScale::Standard);
        assert_eq!(launch_scale(None), LaunchScale::Standard);
        spec.task_class.complexity = ComplexityTier::Trivial;
        spec.owned_paths = vec!["docs/a.md".into()];
        assert_eq!(launch_scale(Some(&spec)), LaunchScale::DirectSingleAgent);
    }
}
