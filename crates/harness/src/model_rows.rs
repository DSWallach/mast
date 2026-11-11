//! Shared model rows for composer and scheduled-task configuration.
use mast_proto::{HarnessId, Model};

pub fn normalize_model_rows(harness: HarnessId, models: Vec<Model>) -> Vec<Model> {
    fn strip_1m(id: &str) -> Option<&str> {
        id.strip_suffix("[1m]").or_else(|| id.strip_suffix("-1m"))
    }
    fn norm(id: &str) -> String {
        id.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase()
    }
    let catalog = match harness {
        HarnessId::ClaudeCode => crate::claude::catalog::static_models(),
        _ => Vec::new(),
    };
    // Curated label for an id: exact normalized match, else — for bare
    // alphabetic aliases like `opus` — the first (flagship-ordered) family
    // row. Versioned foreign ids never fuzzy-match.
    let curated_label = |id: &str| -> Option<String> {
        let id_norm = norm(id);
        if let Some(row) = catalog.iter().find(|m| norm(&m.id) == id_norm) {
            return Some(row.label.clone());
        }
        (!id_norm.is_empty() && id_norm.chars().all(|c| c.is_ascii_alphabetic()))
            .then(|| catalog.iter().find(|m| norm(&m.id).contains(&id_norm)))
            .flatten()
            .map(|m| m.label.clone())
    };
    let ids: Vec<String> = models.iter().map(|m| m.id.clone()).collect();
    let has_real = ids.iter().any(|id| !id.eq_ignore_ascii_case("default"));
    models
        .into_iter()
        .filter_map(|mut model| {
            if has_real && model.id.eq_ignore_ascii_case("default") {
                return None;
            }
            if let Some(base) = strip_1m(&model.id.clone()) {
                if ids.iter().any(|other| other == base) {
                    // The bare base is listed too — the engine already gave
                    // it the Context Window trait; the variant row is noise.
                    return None;
                }
                model.id = base.to_string();
                // "Opus (1M context)" → "Opus".
                if let Some(at) = model.label.rfind(" (")
                    && model.label.ends_with(')')
                {
                    model.label.truncate(at);
                    while model.label.ends_with(' ') {
                        model.label.pop();
                    }
                }
                if !model.options.iter().any(|o| o.id == "contextWindow") {
                    model.options.push(mast_proto::ModelOption {
                        id: "contextWindow".into(),
                        label: "Context Window".into(),
                        choices: vec![
                            mast_proto::ModelOptionChoice {
                                id: "200k".into(),
                                label: "200K".into(),
                            },
                            mast_proto::ModelOptionChoice {
                                id: "1m".into(),
                                label: "1M".into(),
                            },
                        ],
                        default_choice: "1m".into(),
                    });
                }
            }
            if let Some(label) = curated_label(&model.id) {
                model.label = label;
            }
            Some(model)
        })
        .collect()
}
