use mast_proto::{HarnessId, Model, ModelOption, ModelOptionChoice};
use serde_json::Value;

/// Build the `contextWindow` ModelOption for a model hosted under Antigravity.
pub(crate) fn antigravity_context_option(model_id: &str) -> ModelOption {
    let lower = model_id.to_ascii_lowercase();
    let (default_tokens, max_tokens, default_label) = if lower.contains("claude") {
        (200_000, 1_000_000, "200K")
    } else if lower.contains("gpt") {
        (128_000, 128_000, "128K")
    } else {
        // Standard Gemini models (gemini-4-argon, gemini-3.8-flash, etc.)
        (1_000_000, 1_000_000, "1M")
    };

    let mut sizes: Vec<u64> = vec![128_000, 200_000, 500_000, 1_000_000];
    if max_tokens > 1_000_000 {
        sizes.push(max_tokens);
    }
    sizes.retain(|&n| n <= max_tokens);
    sizes.push(default_tokens);
    sizes.sort_unstable();
    sizes.dedup();

    let mut choices = vec![ModelOptionChoice {
        id: "default".into(),
        label: format!("Model default ({default_label})"),
    }];
    for size in sizes {
        let label = if size >= 1_000_000 {
            format!("{}M", size / 1_000_000)
        } else {
            format!("{}K", size / 1_000)
        };
        let choice_label = if size == max_tokens {
            format!("{label} (max)")
        } else {
            label
        };
        choices.push(ModelOptionChoice {
            id: size.to_string(),
            label: choice_label,
        });
    }

    ModelOption {
        id: "contextWindow".into(),
        label: "Context Window".into(),
        choices,
        default_choice: "default".into(),
    }
}

/// Enrich Antigravity models with a `contextWindow` selectable option.
pub(crate) fn enrich_antigravity_models(models: &mut [Model]) {
    for model in models {
        if !model.options.iter().any(|o| o.id == "contextWindow") {
            model.options.push(antigravity_context_option(&model.id));
        }
    }
}

/// Parse a context window selection value from user model options.
pub(crate) fn parse_context_window(val: &Value) -> Option<u64> {
    if let Some(s) = val.as_str() {
        match s.trim().to_ascii_lowercase().as_str() {
            "default" => None,
            "128k" | "128000" => Some(128_000),
            "200k" | "200000" => Some(200_000),
            "500k" | "500000" => Some(500_000),
            "1m" | "1000000" => Some(1_000_000),
            "2m" | "2000000" => Some(2_000_000),
            other => other.parse::<u64>().ok().filter(|n| *n > 0),
        }
    } else {
        val.as_u64().filter(|n| *n > 0)
    }
}

/// Compute the effective context window for a turn under Antigravity.
pub(crate) fn effective_context_window(
    harness: HarnessId,
    model: Option<&str>,
    model_options: &serde_json::Map<String, Value>,
) -> Option<u64> {
    if harness != HarnessId::Antigravity {
        return None;
    }
    if let Some(selected) = model_options
        .get("contextWindow")
        .and_then(parse_context_window)
    {
        return Some(selected);
    }
    let model = model.unwrap_or("");
    let lower = model.to_ascii_lowercase();
    if lower.contains("claude") {
        Some(200_000)
    } else if lower.contains("gpt") {
        Some(128_000)
    } else {
        Some(1_000_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_context_option_for_gemini() {
        let opt = antigravity_context_option("gemini-4-argon");
        assert_eq!(opt.id, "contextWindow");
        assert_eq!(opt.default_choice, "default");
        assert_eq!(opt.choices[0].label, "Model default (1M)");
        let ids: Vec<&str> = opt.choices.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["default", "128000", "200000", "500000", "1000000"]);
        assert_eq!(opt.choices.last().unwrap().label, "1M (max)");
    }

    #[test]
    fn antigravity_context_option_for_claude() {
        let opt = antigravity_context_option("claude-opus-5-5");
        assert_eq!(opt.id, "contextWindow");
        assert_eq!(opt.default_choice, "default");
        assert_eq!(opt.choices[0].label, "Model default (200K)");
        let ids: Vec<&str> = opt.choices.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["default", "128000", "200000", "500000", "1000000"]);
    }

    #[test]
    fn parse_context_window_values() {
        assert_eq!(parse_context_window(&serde_json::json!("default")), None);
        assert_eq!(parse_context_window(&serde_json::json!("128k")), Some(128_000));
        assert_eq!(parse_context_window(&serde_json::json!("128000")), Some(128_000));
        assert_eq!(parse_context_window(&serde_json::json!("200k")), Some(200_000));
        assert_eq!(parse_context_window(&serde_json::json!("500000")), Some(500_000));
        assert_eq!(parse_context_window(&serde_json::json!("1m")), Some(1_000_000));
        assert_eq!(parse_context_window(&serde_json::json!("1000000")), Some(1_000_000));
        assert_eq!(parse_context_window(&serde_json::json!(500_000)), Some(500_000));
    }

    #[test]
    fn effective_context_window_selection() {
        let mut opts = serde_json::Map::new();
        // default for gemini
        assert_eq!(
            effective_context_window(HarnessId::Antigravity, Some("gemini-3.8-flash"), &opts),
            Some(1_000_000)
        );
        // default for claude
        assert_eq!(
            effective_context_window(HarnessId::Antigravity, Some("claude-opus-5-5"), &opts),
            Some(200_000)
        );
        // overridden
        opts.insert("contextWindow".into(), serde_json::json!("500000"));
        assert_eq!(
            effective_context_window(HarnessId::Antigravity, Some("gemini-3.8-flash"), &opts),
            Some(500_000)
        );
        // other harnesses return None
        assert_eq!(
            effective_context_window(HarnessId::Codex, Some("gemini-3.8-flash"), &opts),
            None
        );
    }
}
