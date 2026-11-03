//! Model catalog + effort mapping for Codex, ported from zeron's
//! `packages/harness/src/codex.ts`.
//!
//! The live catalog comes from the app server's paginated `model/list`
//! (experimentalApi). This snapshot is the failure/offline fallback, kept in
//! newest-first order so a picker remains useful when discovery cannot run.

use mast_proto::{Model, ModelOption, ModelOptionChoice, ReasoningLevel, SandboxLevel};

/// The unified reasoning ladder Codex accepts (`minimal` is offered but clamped
/// on the wire — see [`to_effort`]).
pub(crate) const REASONING_LEVELS: &[ReasoningLevel] = &[
    ReasoningLevel::Minimal,
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
    ReasoningLevel::Ultra,
];

/// Codex's API rejects `minimal` when default tools (web_search, image_gen)
/// are enabled, and doesn't know Claude's ultracode/ultrathink modes. It DOES
/// accept `max` and `ultra` natively (gpt-5.6+), so those pass straight
/// through — only the levels Codex can't take are clamped to the nearest
/// effort (port of codex.ts `toEffort`).
pub(crate) fn to_effort(reasoning: Option<ReasoningLevel>) -> Option<&'static str> {
    Some(match reasoning? {
        ReasoningLevel::Minimal | ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High => "high",
        ReasoningLevel::XHigh | ReasoningLevel::Ultracode | ReasoningLevel::Ultrathink => "xhigh",
        ReasoningLevel::Max => "max",
        ReasoningLevel::Ultra => "ultra",
    })
}

/// `thread/start`'s `sandbox` param (kebab-case wire words).
pub(crate) fn sandbox_mode(sandbox: SandboxLevel) -> &'static str {
    match sandbox {
        SandboxLevel::ReadOnly => "read-only",
        SandboxLevel::WorkspaceWrite => "workspace-write",
        SandboxLevel::DangerFullAccess => "danger-full-access",
    }
}

/// `turn/start`'s `sandboxPolicy.type` (camelCase variant of the same policy).
pub(crate) fn sandbox_policy_type(sandbox: SandboxLevel) -> &'static str {
    match sandbox {
        SandboxLevel::ReadOnly => "readOnly",
        SandboxLevel::WorkspaceWrite => "workspaceWrite",
        SandboxLevel::DangerFullAccess => "dangerFullAccess",
    }
}

/// `turn/start`'s full `sandboxPolicy` object. Workspace-write keeps network
/// access: zeron agents fetch deps and hit APIs unattended, and with the
/// approval policy pinned to "never" a network-less sandbox would fail those
/// commands with no escalation path.
///
/// `writable_roots` are extra absolute roots (see [`git_writable_roots`]):
/// codex keeps `.git` read-only inside every writable root, and a linked
/// worktree's or submodule's git data lives outside the cwd, so without them
/// every commit stalls on an approval prompt.
pub(crate) fn sandbox_policy_value(
    sandbox: SandboxLevel,
    writable_roots: &[String],
) -> serde_json::Value {
    let mut policy = serde_json::Map::new();
    policy.insert("type".into(), sandbox_policy_type(sandbox).into());
    if matches!(sandbox, SandboxLevel::WorkspaceWrite) {
        policy.insert("networkAccess".into(), true.into());
        if !writable_roots.is_empty() {
            policy.insert("writableRoots".into(), writable_roots.into());
        }
    }
    serde_json::Value::Object(policy)
}

/// The git directories a workspace-write sandbox must be able to write for
/// `cwd`: the git dir (a linked worktree's `<common>/worktrees/<name>`) and
/// the common dir (refs, objects, config). Verified against the real codex
/// seatbelt sandbox: the common dir alone still denies the worktree's
/// `index.lock`; both together let `git commit` and `git worktree add` run
/// with no prompt. Empty when `cwd` is not in a git repository.
pub(crate) fn git_writable_roots(cwd: &str) -> Vec<String> {
    if cwd.is_empty() {
        return Vec::new();
    }
    let out = crate::git_command()
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ])
        .current_dir(cwd)
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    let mut roots: Vec<String> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim();
        if line.starts_with('/') && !roots.iter().any(|r| r == line) {
            roots.push(line.to_string());
        }
    }
    roots
}

const ULTRA_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
    ReasoningLevel::Ultra,
];

const MAX_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
];

const XHIGH_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
];

/// The service-tier select the app server reports per model (`serviceTiers` /
/// `additionalSpeedTiers` in `model/list`); "default" means Standard and is
/// omitted from the wire params entirely.
fn service_tier() -> ModelOption {
    ModelOption {
        id: "serviceTier".into(),
        label: "Service Tier".into(),
        choices: vec![
            ModelOptionChoice {
                id: "default".into(),
                label: "Standard".into(),
            },
            ModelOptionChoice {
                id: "fast".into(),
                label: "Fast".into(),
            },
        ],
        default_choice: "default".into(),
    }
}

fn model(
    id: &str,
    label: &str,
    description: &str,
    ladder: &[ReasoningLevel],
    options: Vec<ModelOption>,
) -> Model {
    Model {
        id: id.into(),
        label: label.into(),
        description: (!description.is_empty()).then(|| description.into()),
        reasoning_levels: ladder.to_vec(),
        options,
    }
}

/// The curated fallback catalog: newest family first, with efforts as the
/// app server reports them. Daybreak Blue reports NO service tiers, so it
/// carries no trait — sending `serviceTier: priority` for it would be
/// rejected. The live `model/list` remains authoritative whenever available.
pub(crate) fn static_models() -> Vec<Model> {
    vec![
        model(
            "gpt-6-astra",
            "GPT-6-Astra",
            "Our most capable model for complex, demanding work.",
            ULTRA_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-5.6-sol",
            "GPT-5.6-Sol",
            "Frontier reasoning flagship",
            ULTRA_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-5.6-terra",
            "GPT-5.6-Terra",
            "Deep multi-step agentic work",
            ULTRA_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-5.6-luna",
            "GPT-5.6-Luna",
            "Fast frontier model",
            MAX_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-daybreak-blue-latest",
            "Daybreak Blue",
            "Frontier model for defensive cybersecurity work",
            ULTRA_LADDER,
            Vec::new(),
        ),
        model(
            "gpt-5.5",
            "GPT-5.5",
            "Previous generation flagship",
            XHIGH_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-5.4",
            "GPT-5.4",
            "Reliable general coding",
            XHIGH_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-5.4-mini",
            "GPT-5.4-Mini",
            "Small, fast and capable",
            XHIGH_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-5.3-codex-spark",
            "GPT-5.3-Codex-Spark",
            "Ultra-fast lightweight coding",
            XHIGH_LADDER,
            vec![service_tier()],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_write_policy_carries_writable_roots() {
        let roots = vec!["/r/.git".to_string(), "/r/.git/worktrees/w".to_string()];
        let v = sandbox_policy_value(SandboxLevel::WorkspaceWrite, &roots);
        assert_eq!(v["type"], "workspaceWrite");
        assert_eq!(v["networkAccess"], true);
        assert_eq!(v["writableRoots"], serde_json::json!(roots));
        let none = sandbox_policy_value(SandboxLevel::WorkspaceWrite, &[]);
        assert!(none.get("writableRoots").is_none());
        for level in [SandboxLevel::ReadOnly, SandboxLevel::DangerFullAccess] {
            let v = sandbox_policy_value(level, &roots);
            assert!(v.get("writableRoots").is_none());
        }
    }

    #[test]
    fn git_roots_for_main_and_linked_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            let st = crate::git_command()
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap();
            assert!(st.success(), "git {args:?}");
        };
        let main = base.join("main");
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "i"]);
        let main_git = main.join(".git").to_string_lossy().to_string();
        assert_eq!(
            git_writable_roots(main.to_str().unwrap()),
            vec![main_git.clone()]
        );
        let wt = base.join("wt");
        git(
            &main,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "f"],
        );
        let roots = git_writable_roots(wt.to_str().unwrap());
        assert_eq!(roots.len(), 2);
        assert!(roots[0].ends_with("/.git/worktrees/wt"), "{roots:?}");
        assert_eq!(roots[1], main_git);
        assert!(git_writable_roots(tmp.path().join("nope").to_str().unwrap()).is_empty());
    }

    /// Runs the real codex seatbelt sandbox: a commit in a linked worktree is
    /// denied with no roots and succeeds with `git_writable_roots`. Skipped
    /// when `codex` is not installed or the sandbox cannot nest.
    #[cfg(target_os = "macos")]
    #[test]
    fn codex_sandbox_commits_in_linked_worktree_with_git_roots() {
        let probe = std::process::Command::new("codex")
            .arg("--version")
            .output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            let st = crate::git_command()
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap();
            assert!(st.success(), "git {args:?}");
        };
        let main = base.join("main");
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "i"]);
        let wt = base.join("wt");
        git(
            &main,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "f"],
        );
        let commit = |roots: &[String]| -> bool {
            let out = std::process::Command::new("codex")
                .args(["sandbox", "-c", "sandbox_mode=workspace-write"])
                .args([
                    "-c".to_string(),
                    format!(
                        "sandbox_workspace_write.writable_roots={}",
                        serde_json::json!(roots)
                    ),
                ])
                .args(["--", "git", "-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["commit", "-q", "--allow-empty", "-m", "x"])
                .current_dir(&wt)
                .output()
                .unwrap();
            out.status.success()
        };
        if commit(&[]) {
            return; // sandbox not enforcing here (nested/unsupported): nothing to prove
        }
        assert!(commit(&git_writable_roots(wt.to_str().unwrap())));
    }

    #[test]
    fn effort_clamps_like_codex_ts() {
        assert_eq!(to_effort(None), None);
        assert_eq!(to_effort(Some(ReasoningLevel::Minimal)), Some("low"));
        assert_eq!(to_effort(Some(ReasoningLevel::Ultracode)), Some("xhigh"));
        assert_eq!(to_effort(Some(ReasoningLevel::Ultrathink)), Some("xhigh"));
        assert_eq!(to_effort(Some(ReasoningLevel::Max)), Some("max"));
        assert_eq!(to_effort(Some(ReasoningLevel::Ultra)), Some("ultra"));
    }

    #[test]
    fn catalog_is_newest_first_with_service_tiers() {
        let models = static_models();
        assert_eq!(models.len(), 9);
        assert_eq!(models[0].id, "gpt-6-astra");
        assert!(models[0].reasoning_levels.contains(&ReasoningLevel::Ultra));
        assert!(!models[5].reasoning_levels.contains(&ReasoningLevel::Max));
        for m in &models {
            let tier = m.options.iter().find(|o| o.id == "serviceTier");
            // Daybreak Blue reports no service tiers on the wire.
            if m.id == "gpt-daybreak-blue-latest" {
                assert!(tier.is_none(), "{} must not carry serviceTier", m.id);
            } else {
                assert!(tier.is_some(), "{} missing serviceTier", m.id);
            }
        }
    }

    #[test]
    fn daybreak_blue_rides_the_full_ultra_ladder() {
        let models = static_models();
        let daybreak = models
            .iter()
            .find(|m| m.id == "gpt-daybreak-blue-latest")
            .expect("daybreak blue in catalog");
        assert_eq!(daybreak.label, "Daybreak Blue");
        assert!(daybreak.reasoning_levels.contains(&ReasoningLevel::Ultra));
        assert!(daybreak.options.is_empty());
    }
}
