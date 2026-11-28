//! Code-to-doc watch rules: the glob matcher, the rules seeded for this
//! repository, and the globs a plan's blast radius adds.

use mast_proto::plan::Blast;

use super::{LivingDocClass, LivingDocRow, REPO_PREFIX};

/// Changes under any of `watch_globs` owe the repo document `doc_path` an
/// update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeedRule {
    pub doc_path: &'static str,
    pub class: LivingDocClass,
    pub watch_globs: &'static [&'static str],
}

const SEED_RULES: &[SeedRule] = &[
    SeedRule {
        doc_path: "docs/mcp.md",
        class: LivingDocClass::Arch,
        watch_globs: &["crates/mcp/src/tools/**", "crates/mcp/src/tools.rs"],
    },
    SeedRule {
        doc_path: "docs/internal/peer-link.md",
        class: LivingDocClass::Subsystem,
        watch_globs: &["crates/engine/src/peer_*"],
    },
    SeedRule {
        doc_path: "ARCHITECTURE.md",
        class: LivingDocClass::Arch,
        watch_globs: &["crates/*/Cargo.toml"],
    },
    SeedRule {
        doc_path: "docs/work-outline.md",
        class: LivingDocClass::Arch,
        watch_globs: &[
            "crates/engine/src/work_outline*.rs",
            "crates/proto/src/work_outline*.rs",
        ],
    },
    SeedRule {
        doc_path: "docs/workflow-pack-format.md",
        class: LivingDocClass::Arch,
        watch_globs: &[
            "crates/engine/src/workflow_pack*.rs",
            "crates/proto/src/workflow_pack.rs",
        ],
    },
    SeedRule {
        doc_path: "docs/internal/context-usage.md",
        class: LivingDocClass::Subsystem,
        watch_globs: &["crates/engine/src/context_usage.rs"],
    },
    SeedRule {
        doc_path: "docs/artifact-kit/plan-schema.md",
        class: LivingDocClass::Arch,
        watch_globs: &["crates/proto/src/plan.rs"],
    },
    SeedRule {
        doc_path: "docs/artifact-kit/decision-sheet-schema.md",
        class: LivingDocClass::Arch,
        watch_globs: &["crates/proto/src/decision_sheet*.rs"],
    },
];

/// The watch rules seeded for this repository.
pub fn default_seed_rules() -> &'static [SeedRule] {
    SEED_RULES
}

/// Add a row for each rule whose document exists (`blob_hash_of` answers for
/// it), and add missing globs to a row already there without touching its
/// state. Running it again changes nothing.
pub fn seed_rows(
    rows: &mut Vec<LivingDocRow>,
    rules: &[SeedRule],
    owner_scope_item_id: &str,
    blob_hash_of: impl Fn(&str) -> Option<String>,
) {
    for rule in rules {
        let doc_id = format!("{REPO_PREFIX}{}", rule.doc_path);
        let globs = rule.watch_globs.iter().map(|glob| (*glob).to_owned());
        if let Some(row) = rows.iter_mut().find(|row| row.doc_id == doc_id) {
            add_globs(row, globs);
        } else if let Some(current_hash) = blob_hash_of(rule.doc_path) {
            let mut row = LivingDocRow {
                doc_id,
                class: rule.class,
                owner_scope_item_id: owner_scope_item_id.to_owned(),
                current_hash,
                ..Default::default()
            };
            add_globs(&mut row, globs);
            rows.push(row);
        }
    }
}

/// Watch a plan's blast radius from the plan's own row (`plan_doc_id`). Only
/// surfaces written as a repo path or glob count; prose surfaces such as
/// "CLI users" are ignored. Returns whether the row exists.
pub fn merge_blast_radius(
    rows: &mut [LivingDocRow],
    plan_doc_id: &str,
    blast_radius: &[Blast],
) -> bool {
    let Some(row) = rows.iter_mut().find(|row| row.doc_id == plan_doc_id) else {
        return false;
    };
    let globs: Vec<String> = blast_radius
        .iter()
        .filter_map(|blast| surface_glob(&blast.surface))
        .collect();
    add_globs(row, globs);
    true
}

fn add_globs(row: &mut LivingDocRow, globs: impl IntoIterator<Item = String>) {
    for glob in globs {
        if !row.watch_globs.contains(&glob) {
            row.watch_globs.push(glob);
        }
    }
}

fn surface_glob(surface: &str) -> Option<String> {
    let surface = surface.trim().trim_matches('`');
    let surface = surface.strip_prefix("./").unwrap_or(surface);
    let path_chars = surface
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-*?".contains(c));
    // A slash alone is not enough ("CI/CD"): ask for a file name or a glob.
    let names_files = surface.contains('*')
        || surface
            .rsplit('/')
            .next()
            .is_some_and(|name| name.contains('.'));
    (path_chars && surface.contains('/') && !surface.starts_with('/') && names_files)
        .then(|| surface.to_owned())
}

/// Whether the changed file `path` falls under `glob`. `**` matches any number
/// of segments, `*` and `?` match within one segment, and a glob with no
/// wildcard also covers everything under it as a directory.
pub fn watch_matches(glob: &str, path: &str) -> bool {
    let segments = |value: &'_ str| -> Vec<String> {
        value
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .map(str::to_owned)
            .collect()
    };
    let (pattern, target) = (segments(glob.trim()), segments(path));
    if pattern.is_empty() {
        return false;
    }
    if !glob.contains(['*', '?']) {
        return target.starts_with(&pattern);
    }
    match_segments(&pattern, &target)
}

fn match_segments(pattern: &[String], path: &[String]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((head, rest)) if head == "**" => {
            // A trailing `**` needs at least one segment under the directory.
            if rest.is_empty() {
                return !path.is_empty();
            }
            (0..=path.len()).any(|skip| match_segments(rest, &path[skip..]))
        }
        Some((head, rest)) => path
            .split_first()
            .is_some_and(|(first, tail)| match_segment(head, first) && match_segments(rest, tail)),
    }
}

/// `*` and `?` within one segment, without backtracking blow-up.
fn match_segment(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if let Some((star_p, star_t)) = star {
            pi = star_p + 1;
            ti = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn globs_match_files_the_way_the_rules_mean() {
        assert!(watch_matches(
            "crates/mcp/src/tools/**",
            "crates/mcp/src/tools/tool_catalog.rs"
        ));
        assert!(watch_matches(
            "crates/mcp/src/tools/**",
            "crates/mcp/src/tools/a/b.rs"
        ));
        assert!(!watch_matches(
            "crates/mcp/src/tools/**",
            "crates/mcp/src/tools.rs"
        ));
        assert!(watch_matches(
            "crates/engine/src/peer_*",
            "crates/engine/src/peer_link.rs"
        ));
        assert!(!watch_matches(
            "crates/engine/src/peer_*",
            "crates/engine/src/repos.rs"
        ));
        assert!(!watch_matches(
            "crates/engine/src/peer_*",
            "crates/engine/src"
        ));
        assert!(watch_matches("crates/*/Cargo.toml", "crates/ui/Cargo.toml"));
        assert!(!watch_matches("crates/*/Cargo.toml", "Cargo.toml"));
        assert!(!watch_matches("crates/*/Cargo.toml", "crates/ui"));
        assert!(watch_matches("**/mod.rs", "mod.rs"));
        assert!(watch_matches("crates/proto", "crates/proto/src/plan.rs"));
        assert!(!watch_matches(
            "crates/proto",
            "crates/protocol/src/plan.rs"
        ));
        assert!(!watch_matches("", "a"));
    }

    #[test]
    fn every_seed_rule_names_a_real_doc_and_watches_real_files() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        let mut stack = vec![root.join("crates")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                if entry.file_type().unwrap().is_dir() {
                    stack.push(entry.path());
                } else {
                    let path = entry.path();
                    files.push(
                        path.strip_prefix(&root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        for rule in default_seed_rules() {
            assert!(
                root.join(rule.doc_path).is_file(),
                "missing doc {}",
                rule.doc_path
            );
            for glob in rule.watch_globs {
                assert!(
                    files.iter().any(|file| watch_matches(glob, file)),
                    "{glob} matches nothing"
                );
            }
        }
    }

    #[test]
    fn seeding_skips_absent_docs_and_keeps_existing_state() {
        let mut rows = vec![LivingDocRow {
            doc_id: "repo:docs/mcp.md".into(),
            owed_since_commit: Some("c1".into()),
            watch_globs: vec!["custom/**".into()],
            ..Default::default()
        }];
        let hash = |path: &str| (path == "ARCHITECTURE.md").then(|| "h".to_owned());
        seed_rows(&mut rows, default_seed_rules(), "project", hash);
        seed_rows(&mut rows, default_seed_rules(), "project", hash);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].owed_since_commit.as_deref(), Some("c1"));
        assert_eq!(
            rows[0].watch_globs,
            [
                "custom/**",
                "crates/mcp/src/tools/**",
                "crates/mcp/src/tools.rs"
            ]
        );
        assert_eq!(rows[1].doc_id, "repo:ARCHITECTURE.md");
        assert_eq!(rows[1].current_hash, "h");
        assert_eq!(rows[1].owner_scope_item_id, "project");
    }

    #[test]
    fn blast_radius_adds_only_path_surfaces() {
        let blast = |surface: &str| Blast {
            surface: surface.into(),
            kind: String::new(),
            audience: String::new(),
            change: String::new(),
            gate: String::new(),
        };
        let mut rows = vec![LivingDocRow {
            doc_id: "artifact:plan".into(),
            ..Default::default()
        }];
        let surfaces = [
            blast("`crates/proto/src/agent.rs`"),
            blast("crates/rpc/**"),
            blast("CLI users"),
            blast("CI/CD"),
            blast("/etc/passwd.conf"),
        ];
        assert!(merge_blast_radius(&mut rows, "artifact:plan", &surfaces));
        assert_eq!(
            rows[0].watch_globs,
            ["crates/proto/src/agent.rs", "crates/rpc/**"]
        );
        assert!(!merge_blast_radius(&mut rows, "artifact:other", &surfaces));
    }
}
