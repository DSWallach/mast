//! Model catalog + effort mapping for Claude Code, ported from zeron's
//! `packages/harness/src/claude.ts` (which itself mirrors Claude Code's own
//! picker via t3code's catalog).
//!
//! Runtime initialize discovery appends concrete model ids to this curated list.
//! Curated rows retain their labels, effort ladders, and option sets because the
//! CLI can under-report supported modes. The shared initialize probe also supplies
//! slash commands and is cached by credential and binary context.

use mast_proto::{Model, ModelOption, ModelOptionChoice, ReasoningLevel};

pub(super) const HAIKU_5_5: &str = "claude-haiku-5-5";

/// The ultrathink directive rides every user message as a prompt prefix — that
/// is how the mode actually works in Claude Code (a prompt convention, not an
/// effort flag). Applied to the initial prompt AND every steer.
pub(crate) const ULTRATHINK_PREFIX: &str = "Ultrathink:\n";

pub(crate) fn apply_ultrathink(reasoning: Option<ReasoningLevel>, text: &str) -> String {
    if reasoning == Some(ReasoningLevel::Ultrathink) {
        format!("{ULTRATHINK_PREFIX}{text}")
    } else {
        text.to_owned()
    }
}

fn contains_any(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

/// Models whose CLI accepts `xhigh` natively; elsewhere it clamps to `max`
/// (mirroring Claude Code's own normalization).
pub(crate) fn supports_xhigh(model: &str) -> bool {
    model == HAIKU_5_5
        || contains_any(
            model,
            &[
                "fable-5", "opus-4-7", "opus-4-8", "opus-4-9", "opus-5", "opus-6", "opus-7", "opus-8",
                "opus-9", "sonnet-5", "sonnet-6", "sonnet-7", "sonnet-8", "sonnet-9",
            ],
        )
}

/// Map the unified level to the `--effort` flag value the CLI accepts for this
/// model. The special modes don't translate directly: `ultrathink` is a prompt
/// prefix (no flag), `ultracode` runs as `xhigh` plus the ultracode setting,
/// and `ultra` is a Codex-only tier (Claude tops out at `max`).
pub(crate) fn to_effort(
    reasoning: Option<ReasoningLevel>,
    model: Option<&str>,
) -> Option<&'static str> {
    let base = match reasoning? {
        ReasoningLevel::Ultrathink => return None,
        ReasoningLevel::Minimal | ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High => "high",
        ReasoningLevel::XHigh | ReasoningLevel::Ultracode => "xhigh",
        ReasoningLevel::Max | ReasoningLevel::Ultra => "max",
    };
    if base == "xhigh" && !model.is_some_and(supports_xhigh) {
        return Some("max");
    }
    Some(base)
}

/// A boolean toggle rendered as an off/on select (the Rust `ModelOption` wire
/// type has no dedicated boolean kind).
fn toggle(id: &str, label: &str) -> ModelOption {
    ModelOption {
        id: id.into(),
        label: label.into(),
        choices: vec![
            ModelOptionChoice {
                id: "off".into(),
                label: "Off".into(),
            },
            ModelOptionChoice {
                id: "on".into(),
                label: "On".into(),
            },
        ],
        default_choice: "off".into(),
    }
}

/// The 200K/1M context-window select carried by the long-context models. The
/// 1M window is selected via a model-id suffix (`<model>[1m]`), exactly how the
/// CLI itself does it.
pub(crate) fn context_window() -> ModelOption {
    ModelOption {
        id: "contextWindow".into(),
        label: "Context Window".into(),
        choices: vec![
            ModelOptionChoice {
                id: "200k".into(),
                label: "200K".into(),
            },
            ModelOptionChoice {
                id: "1m".into(),
                label: "1M".into(),
            },
        ],
        default_choice: "200k".into(),
    }
}

/// The Claude CLI harness's select: [`context_window`] plus a leading
/// "Default (set by Claude)" that is also the default choice. Some models get
/// 1M from the CLI on its own, so before a window is reported and with no pick
/// stored, "200K" would name a window nothing chose. The harness passes
/// neither the `[1m]` suffix nor `CLAUDE_CODE_DISABLE_1M_CONTEXT` for it.
pub(crate) fn cli_context_window() -> ModelOption {
    let mut choices = vec![ModelOptionChoice {
        id: "default".into(),
        label: "Default (set by Claude)".into(),
    }];
    // The CLI takes any limit up to the model's long-context window through
    // its compact window, so every size is a real choice on these models.
    choices.extend(mast_proto::CONTEXT_WINDOW_CHOICES.map(|n| {
        let (id, label) = if n % 1_000_000 == 0 {
            (format!("{}m", n / 1_000_000), format!("{}M", n / 1_000_000))
        } else {
            (format!("{}k", n / 1_000), format!("{}K", n / 1_000))
        };
        ModelOptionChoice { id, label }
    }));
    ModelOption {
        id: "contextWindow".into(),
        label: "Context Window".into(),
        choices,
        default_choice: "default".into(),
    }
}

const FULL_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
    ReasoningLevel::Ultracode,
    ReasoningLevel::Ultrathink,
];

/// opus-4-7 / sonnet-5+ tier (claude.ts `claudeEffortsFor`): xhigh native,
/// no ultracode.
const XHIGH_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
    ReasoningLevel::Ultrathink,
];

const HAIKU_5_5_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
];

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

/// The curated model list, mirroring claude.ts's `claudeEffortsFor` /
/// `claudeOptionsFor` ladders: full ladder (through ultracode/ultrathink) on
/// Fable 5, `max`-topped ladders on Opus/Sonnet and Haiku 5.5, a thinking
/// toggle on Haiku 4.5, context-window select on the long-context models,
/// and fast mode on Opus 4.5+.
///
/// `pub`: besides the discovery-side enrichment here, the UI's display-side
/// normalization borrows these labels so alias rows served by older engines
/// still read with their version numbers ("Opus 5", not "Opus").
pub(crate) fn configured_models(account_home: Option<&std::path::Path>) -> Vec<Model> {
    let root = account_home
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|v| !v.is_empty())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| crate::executable::home_or_current_dir().join(".claude"))
        });
    models_with_settings(&root.join("settings.json"))
}

fn models_with_settings(path: &std::path::Path) -> Vec<Model> {
    let mut models = static_models();
    let settings = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or_default();
    let ids = std::iter::once(settings.get("model")).chain(
        [
            "ANTHROPIC_MODEL",
            "ANTHROPIC_SMALL_FAST_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        ]
        .into_iter()
        .map(|key| settings.get("env").and_then(|env| env.get(key))),
    );
    for id in ids
        .flatten()
        .filter_map(|v| v.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        if !models.iter().any(|m| m.id == id) {
            models.push(Model {
                id: id.into(),
                label: id.into(),
                description: None,
                reasoning_levels: FULL_LADDER.to_vec(),
                options: vec![],
            });
        }
    }
    models
}

#[cfg(test)]
#[test]
fn settings_models_keep_manifest_and_full_ladder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        serde_json::json!({"model":"gateway/model", "env": {
            "ANTHROPIC_MODEL":"gateway/model", "ANTHROPIC_SMALL_FAST_MODEL":"fast",
            "ANTHROPIC_DEFAULT_OPUS_MODEL":"opus", "ANTHROPIC_DEFAULT_SONNET_MODEL":"sonnet",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL":"haiku"
        }})
        .to_string(),
    )
    .unwrap();
    let models = models_with_settings(&path);
    assert_eq!(&models[..static_models().len()], static_models());
    assert_eq!(models.len(), static_models().len() + 5);
    for model in &models[static_models().len()..] {
        assert_eq!(model.label, model.id);
        assert_eq!(model.reasoning_levels, FULL_LADDER);
    }
    std::fs::write(&path, "invalid").unwrap();
    assert_eq!(models_with_settings(&path), static_models());
}

/// Overlay only new concrete model IDs; curated metadata remains authoritative.
pub(super) fn with_discovered_models(
    mut models: Vec<Model>,
    response: &serde_json::Value,
) -> Result<Vec<Model>, crate::HarnessError> {
    let entries = response
        .get("response")
        .and_then(|v| v.get("models"))
        .and_then(serde_json::Value::as_array)
        .filter(|entries| !entries.is_empty())
        .ok_or_else(|| {
            crate::HarnessError::Protocol("Claude returned an empty model catalog".into())
        })?;
    let mut default = None;
    let mut valid = false;
    for entry in entries {
        let text = |key: &str| {
            entry
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
        };
        let Some(id) = text("resolvedModel").or_else(|| text("value")) else {
            continue;
        };
        // Bare aliases cannot become persisted selections. Prefer resolvedModel.
        if matches!(
            id.strip_suffix("[1m]").unwrap_or(id),
            "default" | "opus" | "sonnet" | "haiku" | "fable"
        ) {
            continue;
        }
        valid = true;
        if text("value") == Some("default") {
            default = Some(id.to_owned());
        }
        if models.iter().any(|model| model.id == id) {
            continue;
        }
        let mut ladder = Vec::new();
        for effort in entry
            .get("supportedEffortLevels")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
        {
            let level = match effort {
                "low" => ReasoningLevel::Low,
                "medium" => ReasoningLevel::Medium,
                "high" => ReasoningLevel::High,
                "xhigh" => ReasoningLevel::XHigh,
                "max" => ReasoningLevel::Max,
                _ => continue,
            };
            if !ladder.contains(&level) {
                ladder.push(level);
            }
        }
        if ladder.contains(&ReasoningLevel::XHigh) {
            ladder.extend([ReasoningLevel::Ultracode, ReasoningLevel::Ultrathink]);
        }
        models.push(Model {
            id: id.into(),
            label: text("displayName").unwrap_or(id).into(),
            description: text("description").map(str::to_owned),
            reasoning_levels: ladder,
            options: if long_context_family(id) { vec![cli_context_window()] } else { vec![] },
        });
    }
    if !valid {
        return Err(crate::HarnessError::Protocol(
            "Claude returned an empty model catalog".into(),
        ));
    }
    if let Some(default) = default {
        // The picker folds long-context duplicates into their curated base.
        // Put that base immediately behind the concrete default so folding
        // keeps the CLI's default family first without changing curated rows.
        if let Some(base) = default.strip_suffix("[1m]")
            && let Some(index) = models.iter().position(|m| m.id == base)
        {
            let model = models.remove(index);
            models.insert(0, model);
        }
        if let Some(index) = models.iter().position(|m| m.id == default) {
            let model = models.remove(index);
            models.insert(0, model);
        }
    }
    Ok(models)
}

/// Model families the curated list gives a Context Window choice. A model the
/// CLI reports that the list lacks (a newer point release) belongs to the same
/// family and takes the same control.
fn long_context_family(id: &str) -> bool {
    // A `[1m]` row is folded into its base by the picker, which gives the base
    // its own 200K/1M choice defaulting to 1M.
    !(id.ends_with("[1m]") || id.ends_with("-1m"))
        && ["claude-fable-5", "claude-opus-5", "claude-sonnet-5"]
            .iter()
            .any(|family| id.starts_with(family))
}

pub fn static_models() -> Vec<Model> {
    vec![
        model(
            "claude-fable-5-1",
            "Fable 5.1",
            "Most intelligent model for building agents",
            FULL_LADDER,
            vec![cli_context_window()],
        ),
        model(
            "claude-fable-5",
            "Fable 5",
            "Previous generation Fable",
            FULL_LADDER,
            vec![cli_context_window()],
        ),
        model(
            "claude-opus-5-5",
            "Opus 5.5",
            "Powerful model for complex work",
            FULL_LADDER,
            vec![cli_context_window(), toggle("fastMode", "Fast Mode")],
        ),
        model(
            "claude-opus-5",
            "Opus 5",
            "Powerful model for complex work",
            FULL_LADDER,
            vec![cli_context_window(), toggle("fastMode", "Fast Mode")],
        ),
        model(
            "claude-opus-4-8",
            "Opus 4.8",
            "Previous generation Opus",
            FULL_LADDER,
            vec![toggle("fastMode", "Fast Mode")],
        ),
        model(
            "claude-opus-4-7",
            "Opus 4.7",
            "Older generation Opus",
            XHIGH_LADDER,
            vec![toggle("fastMode", "Fast Mode")],
        ),
        model(
            "claude-sonnet-5",
            "Sonnet 5",
            "Balanced speed and intelligence",
            XHIGH_LADDER,
            vec![cli_context_window()],
        ),
        model(
            "claude-haiku-4-5",
            "Haiku 4.5",
            "Fastest model for everyday tasks",
            &[],
            vec![toggle("thinking", "Thinking")],
        ),
        model(
            HAIKU_5_5,
            "Haiku 5.5",
            "Low-latency model for everyday tasks",
            HAIKU_5_5_LADDER,
            vec![cli_context_window()],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curated_catalog_offers_the_mast_default() {
        let id = mast_proto::HarnessId::ClaudeCode.default_model().unwrap();
        assert_eq!(id, "claude-opus-5-5");
        assert!(static_models().iter().any(|m| m.id == id));
    }

    /// An unstored Claude window must not read as 200K: the CLI gives some
    /// models 1M on its own, so the unchosen state is "Default (set by Claude)".
    #[test]
    fn the_curated_context_window_defaults_to_the_cli_choice() {
        let mut offered = 0;
        for model in static_models() {
            let Some(option) = model.options.iter().find(|o| o.id == "contextWindow") else {
                continue;
            };
            offered += 1;
            assert_eq!(option.default_choice, "default", "{}", model.id);
            let choices: Vec<_> = option.choices.iter().map(|c| c.id.as_str()).collect();
            assert_eq!(choices, ["default", "250k", "500k", "750k", "1m"], "{}", model.id);
            assert_eq!(option.choices[0].label, "Default (set by Claude)");
        }
        assert!(offered > 0);
    }

    #[test]
    fn a_discovered_model_of_a_long_context_family_gets_the_window_control() {
        assert!(long_context_family("claude-sonnet-5-5"));
        assert!(long_context_family("claude-opus-5-6"));
        assert!(!long_context_family("claude-sonnet-5-5[1m]"));
        assert!(!long_context_family("claude-opus-5-1m"));
        assert!(!long_context_family("claude-opus-4-8"));
        assert!(!long_context_family("claude-haiku-4-5"));
    }

    #[test]
    fn discovery_wires_the_window_control_into_the_catalog() {
        let response = serde_json::json!({ "response": { "models": [
            { "value": "claude-sonnet-5-9", "resolvedModel": "claude-sonnet-5-9", "displayName": "Sonnet 5.9" },
            { "value": "claude-opus-4-9", "resolvedModel": "claude-opus-4-9", "displayName": "Opus 4.9" },
            { "value": "claude-sonnet-5-9[1m]", "resolvedModel": "claude-sonnet-5-9[1m]", "displayName": "Sonnet 5.9 (1M)" },
        ] } });
        let models = with_discovered_models(vec![], &response).unwrap();
        let options = |id: &str| models.iter().find(|m| m.id == id).map(|m| m.options.len());
        assert_eq!(options("claude-sonnet-5-9"), Some(1));
        assert_eq!(options("claude-opus-4-9"), Some(0));
        assert_eq!(options("claude-sonnet-5-9[1m]"), Some(0));
    }

    #[test]
    fn effort_maps_special_modes() {
        assert_eq!(to_effort(None, None), None);
        assert_eq!(to_effort(Some(ReasoningLevel::Ultrathink), None), None);
        assert_eq!(
            to_effort(Some(ReasoningLevel::Minimal), Some("claude-fable-5")),
            Some("low")
        );
        assert_eq!(
            to_effort(Some(ReasoningLevel::Ultra), Some("claude-fable-5")),
            Some("max")
        );
        // ultracode -> xhigh where supported…
        assert_eq!(
            to_effort(Some(ReasoningLevel::Ultracode), Some("claude-fable-5")),
            Some("xhigh")
        );
        // …and xhigh clamps to max elsewhere.
        assert_eq!(
            to_effort(Some(ReasoningLevel::XHigh), Some("claude-opus-4-5")),
            Some("max")
        );
        assert_eq!(to_effort(Some(ReasoningLevel::XHigh), None), Some("max"));
    }

    #[test]
    fn xhigh_family_matching() {
        assert!(supports_xhigh("claude-fable-5"));
        assert!(supports_xhigh("claude-fable-5-1"));
        assert!(supports_xhigh("claude-opus-5"));
        assert!(supports_xhigh("claude-opus-5[1m]"));
        assert!(supports_xhigh("claude-opus-4-7-20260101"));
        assert!(!supports_xhigh("claude-opus-4-5"));
        assert!(!supports_xhigh("claude-sonnet-4-5"));
    }

    #[test]
    fn ultrathink_prefixes_prompt() {
        assert_eq!(
            apply_ultrathink(Some(ReasoningLevel::Ultrathink), "do it"),
            "Ultrathink:\ndo it"
        );
        assert_eq!(
            apply_ultrathink(Some(ReasoningLevel::Max), "do it"),
            "do it"
        );
    }
}
