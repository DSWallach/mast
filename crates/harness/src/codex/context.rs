//! Context-window choices from the owning Codex account's model metadata.
//! `model/list` omits window sizes; the CLI's model-info cache includes them.
use mast_proto::{Model, ModelOption, ModelOptionChoice, RunRequest};
use serde_json::Value;
use std::{io::Read, path::Path};

fn read_metadata(account_home: Option<&Path>) -> Option<Value> {
    let root = account_home.map(Path::to_path_buf).unwrap_or_else(|| {
        crate::model_context::root(
            "CODEX_HOME",
            crate::executable::home_or_current_dir().join(".codex"),
        )
    });
    let file = std::fs::File::open(root.join("models_cache.json")).ok()?;
    let mut bytes = Vec::new();
    const LIMIT: u64 = 8 * 1024 * 1024;
    if file.take(LIMIT + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > LIMIT {
        return None;
    }
    serde_json::from_slice::<Value>(&bytes).ok()
}

pub(super) fn enrich(models: &mut [Model], account_home: Option<&Path>) {
    if let Some(metadata) = read_metadata(account_home) {
        apply_metadata(models, &metadata);
    }
}

/// `request` without a window larger than its model can hold. A default
/// chosen for a whole Space (not for this model) must not push a smaller
/// model past its real limit. A model the cache does not list, or a request
/// without a model, is left as it is.
pub(super) fn within_model_limit(mut request: RunRequest, account_home: Option<&Path>) -> RunRequest {
    if let (Some(tokens), Some(model)) = (selected(&request), request.model.as_deref())
        && let Some(metadata) = read_metadata(account_home)
        && exceeds_model_limit(&metadata, model, tokens)
    {
        tracing::debug!(model, tokens, "dropping a context window above the model's limit");
        request.model_options.remove("contextWindow");
    }
    request
}

fn exceeds_model_limit(metadata: &Value, model: &str, tokens: u64) -> bool {
    metadata["models"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|info| info["slug"].as_str() == Some(model))
        .and_then(|info| {
            let default = info["context_window"].as_u64().filter(|n| *n > 0)?;
            Some(info["max_context_window"].as_u64().filter(|n| *n >= default).unwrap_or(default))
        })
        .is_some_and(|max| tokens > max)
}

fn apply_metadata(models: &mut [Model], metadata: &Value) {
    for model in models {
        if model.options.iter().any(|o| o.id == "contextWindow") {
            continue;
        }
        let info = metadata["models"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|info| info["slug"].as_str() == Some(model.id.as_str()));
        if let Some(option) = info.and_then(context_option) {
            model.options.push(option);
        }
    }
}

fn context_option(info: &Value) -> Option<ModelOption> {
    let default = info["context_window"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= i64::MAX as u64)?;
    let max = info["max_context_window"]
        .as_u64()
        .filter(|n| *n >= default && *n <= i64::MAX as u64)
        .unwrap_or(default);
    let mut sizes: Vec<_> = mast_proto::work_outline::WORKER_CONTEXT_WINDOW_SIZES
        .into_iter()
        .filter(|n| *n <= max)
        .collect();
    sizes.extend([default, max]);
    sizes.sort_unstable();
    sizes.dedup();
    let mut choices = vec![ModelOptionChoice {
        id: "default".into(),
        label: format!("Model default ({})", label(default)),
    }];
    choices.extend(sizes.into_iter().map(|n| ModelOptionChoice {
        id: n.to_string(),
        label: if n == max {
            format!("{} (max)", label(n))
        } else {
            label(n)
        },
    }));
    Some(ModelOption {
        id: "contextWindow".into(),
        label: "Context Window".into(),
        choices,
        default_choice: "default".into(),
    })
}

fn label(n: u64) -> String {
    if n % 1_000_000 == 0 {
        format!("{}M", n / 1_000_000)
    } else if n % 1_000 == 0 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

/// Default means no override; this preserves the CLI/account's own settings.
pub(super) fn selected(request: &RunRequest) -> Option<u64> {
    request
        .model_options
        .get("contextWindow")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|n| *n > 0 && *n <= i64::MAX as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn model(id: &str) -> Model {
        Model {
            id: id.into(),
            label: id.into(),
            description: None,
            reasoning_levels: vec![],
            options: vec![],
        }
    }
    #[test]
    fn context_choices_follow_model_limits_and_do_not_add_models() {
        let mut models = vec![model("wide"), model("small"), model("unknown")];
        apply_metadata(
            &mut models,
            &json!({"models":[
                {"slug":"wide","context_window":500000,"max_context_window":872000},
                {"slug":"small","context_window":128000},
                {"slug":"not-offered","context_window":1000000}
            ]}),
        );
        assert_eq!(models.len(), 3);
        let wide = &models[0].options[0];
        assert_eq!(wide.default_choice, "default");
        assert_eq!(wide.choices[0].label, "Model default (500K)");
        assert_eq!(wide.choices.last().unwrap().id, "872000");
        assert!(!wide.choices.iter().any(|c| c.id == "1000000"));
        assert!(
            models[1].options[0]
                .choices
                .iter()
                .skip(1)
                .all(|c| c.id.parse::<u64>().unwrap() <= 128000)
        );
        assert!(models[2].options.is_empty());
        let before = models.clone();
        apply_metadata(
            &mut models,
            &json!({"models":[{"slug":"wide","context_window":1}]}),
        );
        assert_eq!(
            before, models,
            "an advertised option is not replaced by cache data"
        );
    }
    #[test]
    fn metadata_is_read_only_from_the_selected_account_home() {
        let account = tempfile::tempdir().unwrap();
        let mut models = vec![model("selected")];
        enrich(&mut models, Some(account.path()));
        assert!(models[0].options.is_empty());
        std::fs::write(account.path().join("models_cache.json"), r#"{"models":[{"slug":"selected","context_window":272000,"max_context_window":872000}]}"#).unwrap();
        enrich(&mut models, Some(account.path()));
        assert_eq!(
            models[0].options[0].choices[0].label,
            "Model default (272K)"
        );
        assert!(context_option(&json!({"context_window":0})).is_none());
    }
    #[test]
    fn a_window_larger_than_the_model_holds_is_dropped_and_unknown_models_are_kept() {
        let account = tempfile::tempdir().unwrap();
        std::fs::write(
            account.path().join("models_cache.json"),
            r#"{"models":[{"slug":"small","context_window":128000},{"slug":"wide","context_window":272000,"max_context_window":872000}]}"#,
        )
        .unwrap();
        let request = |model: Option<&str>, window: &str| -> RunRequest {
            let mut r: RunRequest = serde_json::from_value(json!({
                "prompt": "p", "model": model, "reasoning": null, "cwd": "/w",
                "sandbox": "workspace-write"
            }))
            .unwrap();
            r.model_options.insert("contextWindow".into(), json!(window));
            r
        };
        let kept = |model: Option<&str>, window: &str| {
            selected(&within_model_limit(request(model, window), Some(account.path()))).is_some()
        };
        assert!(!kept(Some("small"), "200000"), "200K exceeds a 128K model");
        assert!(kept(Some("small"), "64000"));
        assert!(kept(Some("wide"), "200000"));
        assert!(!kept(Some("wide"), "900000"));
        assert!(kept(Some("unlisted"), "200000"));
        assert!(kept(None, "200000"));
    }
}
