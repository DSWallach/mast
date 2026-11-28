//! Path-derived task domain.

use mast_proto::{ComplexityTier, TaskClass, TaskDomain, TaskRole};

/// The domain of a task that touches `paths` (repository-relative changed or
/// owned paths; globs such as `crates/ui/**` work).
///
/// A task spanning several domains takes the hardest one, so a code change that
/// also updates its docs is never routed as a docs task. A glob takes the
/// hardest domain of any area it can reach, so `crates/**` counts as engine
/// code. Source paths that match no known area, and an empty list, are
/// [`TaskDomain::MechanicalRefactor`].
pub fn classify_domain<I>(paths: I) -> TaskDomain
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    paths
        .into_iter()
        .map(|path| path_domain(path.as_ref()))
        .max_by_key(|domain| difficulty(*domain))
        .unwrap_or_default()
}

/// [`TaskClass`] for a task with the given role, complexity and paths.
pub fn task_class<I>(role: TaskRole, complexity: ComplexityTier, paths: I) -> TaskClass
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    TaskClass {
        role,
        domain: classify_domain(paths),
        complexity,
    }
}

/// Order used to combine the domains of several paths.
fn difficulty(domain: TaskDomain) -> u8 {
    match domain {
        TaskDomain::DocsConfig => 0,
        TaskDomain::MechanicalRefactor => 1,
        TaskDomain::McpOrCli => 2,
        TaskDomain::UiGpui => 3,
        TaskDomain::HarnessProtocol => 4,
        TaskDomain::ProtoOrDocSchema => 5,
        TaskDomain::EngineConcurrencyState => 6,
    }
}

/// Directory prefixes and the domain of the code below them.
const AREAS: &[(&str, TaskDomain)] = &[
    ("crates/engine", TaskDomain::EngineConcurrencyState),
    ("crates/sync", TaskDomain::EngineConcurrencyState),
    ("crates/proto", TaskDomain::ProtoOrDocSchema),
    ("crates/doc", TaskDomain::ProtoOrDocSchema),
    ("crates/harness", TaskDomain::HarnessProtocol),
    ("crates/rpc", TaskDomain::HarnessProtocol),
    ("crates/ui", TaskDomain::UiGpui),
    ("crates/ui-driver", TaskDomain::UiGpui),
    ("crates/theme", TaskDomain::UiGpui),
    ("apps/mast-gallery", TaskDomain::UiGpui),
    ("crates/mcp", TaskDomain::McpOrCli),
    ("apps/mast", TaskDomain::McpOrCli),
];

fn path_domain(path: &str) -> TaskDomain {
    let path = path.trim();
    let path = path.strip_prefix("./").unwrap_or(path);
    if is_docs_or_config(path) {
        return TaskDomain::DocsConfig;
    }
    // A glob can reach every area that agrees with the text before its first
    // wildcard.
    let glob = path.find(['*', '?', '[', '{']).map(|at| &path[..at]);
    AREAS
        .iter()
        .filter(|(area, _)| {
            let inside = |path: &str| {
                path.strip_prefix(area)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            };
            match glob {
                Some(prefix) => inside(prefix) || format!("{area}/").starts_with(prefix),
                None => inside(path),
            }
        })
        .map(|(_, domain)| *domain)
        .max_by_key(|domain| difficulty(*domain))
        .unwrap_or_default()
}

fn is_docs_or_config(path: &str) -> bool {
    if path.starts_with("docs/") || path.starts_with(".github/") || path.starts_with(".claude/") {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "Cargo.lock" || name.starts_with("LICENSE") {
        return true;
    }
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        matches!(extension, "md" | "mdx" | "txt" | "toml" | "yml" | "yaml")
    })
}
