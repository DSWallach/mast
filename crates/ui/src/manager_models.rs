//! The models a project Manager's Settings tab offers for its workers and its
//! credit fallback chain, read from each harness's own model catalog instead of
//! a list written into the app.
//!
//! The Settings tab shows one harness's models at a time (an OpenCode catalog
//! runs to hundreds), starting on the stored worker model's harness. A
//! catalog loads once per (device, harness) the first time it is shown, from
//! the device that owns the project's Space, and stays until the app restarts.
//! A failed load is kept, not retried on every draw; the tab offers a retry.
//!
//! Nothing here touches the network: callers start a load when
//! [`ManagerModels::begin_load`] says so and hand the result back to
//! [`ManagerModels::finish`].

use std::collections::HashMap;

use mast_proto::{HarnessId, Model, StandingRoleModel};

/// What a (device, harness) model catalog currently holds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CatalogState {
    Loading,
    Ready(Vec<Model>),
    Failed(String),
}

/// A catalog's key: the device that lists it (`None` for this device) and the
/// harness.
type CatalogKey = (Option<String>, HarnessId);

/// Catalogs loaded for the Manager settings, and which harness each project's
/// tab is showing.
#[derive(Debug, Default)]
pub(crate) struct ManagerModels {
    catalogs: HashMap<CatalogKey, CatalogState>,
    viewing: HashMap<String, HarnessId>,
}

impl ManagerModels {
    /// Whether the caller should start a load for this catalog now. A catalog
    /// that is loading or loaded is left alone; a failed one reloads only when
    /// `retry` (a click), never because the tab drew again.
    pub(crate) fn begin_load(
        &mut self,
        target: &Option<String>,
        harness: HarnessId,
        retry: bool,
    ) -> bool {
        let key = (target.clone(), harness);
        match self.catalogs.get(&key) {
            Some(CatalogState::Failed(_)) if retry => {}
            Some(_) => return false,
            None => {}
        }
        self.catalogs.insert(key, CatalogState::Loading);
        true
    }

    /// Record how a load ended.
    pub(crate) fn finish(
        &mut self,
        target: &Option<String>,
        harness: HarnessId,
        result: Result<Vec<Model>, String>,
    ) {
        let state = match result {
            Ok(models) => CatalogState::Ready(models),
            Err(error) => CatalogState::Failed(error),
        };
        self.catalogs.insert((target.clone(), harness), state);
    }

    pub(crate) fn state(
        &self,
        target: &Option<String>,
        harness: HarnessId,
    ) -> Option<&CatalogState> {
        self.catalogs.get(&(target.clone(), harness))
    }

    /// The models of a loaded catalog.
    pub(crate) fn models(&self, target: &Option<String>, harness: HarnessId) -> Option<&[Model]> {
        match self.state(target, harness) {
            Some(CatalogState::Ready(models)) => Some(models),
            _ => None,
        }
    }

    /// The harness shown for a project: the one last picked on its tab, else
    /// the stored worker model's, else the built-in default's.
    pub(crate) fn viewed(&self, item_id: &str, chosen: Option<&StandingRoleModel>) -> HarnessId {
        self.viewing
            .get(item_id)
            .copied()
            .or_else(|| chosen.and_then(|model| harness_of(&model.harness)))
            .or_else(|| harness_of(&mast_proto::default_manager_worker_model().harness))
            .unwrap_or(HarnessId::ClaudeCode)
    }

    pub(crate) fn view(&mut self, item_id: &str, harness: HarnessId) {
        self.viewing.insert(item_id.to_owned(), harness);
    }
}

/// The harness a stored entry names, or `None` for one this build does not know.
pub(crate) fn harness_of(name: &str) -> Option<HarnessId> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

/// The wire name of a harness, as a stored entry spells it.
pub(crate) fn harness_name(harness: HarnessId) -> String {
    serde_json::to_value(harness)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The harness as a person reads it.
pub(crate) fn harness_label(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::ClaudeCode => "Claude Code",
        HarnessId::Codex => "Codex",
        HarnessId::Cursor => "Cursor",
        HarnessId::Devin => "Devin",
        HarnessId::Grok => "Grok",
        HarnessId::Hermes => "Hermes",
        HarnessId::Pi => "Pi",
        HarnessId::Opencode => "OpenCode",
        HarnessId::Antigravity => "Antigravity",
        HarnessId::Mock => "Mock",
    }
}

/// The harnesses a worker or fallback entry can name: every real one.
pub(crate) fn selectable_harnesses() -> Vec<HarnessId> {
    HarnessId::ALL
        .into_iter()
        .filter(|harness| *harness != HarnessId::Mock)
        .collect()
}

/// One model button: (label, value to set, selected).
pub(crate) type WorkerModelRow = (String, Option<StandingRoleModel>, bool);

/// Buttons for the Manager worker model while `viewed`'s models are shown. The
/// first is the built-in default, selected for no stored model or one equal to
/// it; then the viewed harness's own default model and each model its catalog
/// lists. A stored model the rows do not show (another harness, or one the
/// catalog does not list) gets its own selected button, so the stored choice
/// is always visible. `catalog_of` gives a harness's loaded models.
pub(crate) fn worker_model_choices<'a>(
    chosen: Option<&StandingRoleModel>,
    viewed: HarnessId,
    catalog_of: impl Fn(HarnessId) -> Option<&'a [Model]>,
) -> Vec<WorkerModelRow> {
    let default = mast_proto::default_manager_worker_model();
    let default_label = match (&default.model, harness_of(&default.harness)) {
        (Some(id), Some(harness)) => catalog_of(harness)
            .and_then(|models| models.iter().find(|m| &m.id == id))
            .map_or(id.as_str(), |m| m.label.as_str()),
        (Some(id), None) => id.as_str(),
        (None, _) => "harness default",
    };
    let mut rows = vec![(
        format!("Default ({default_label})"),
        None,
        chosen.is_none_or(|model| *model == default),
    )];
    let viewed_name = harness_name(viewed);
    let offer = |label: String, model: Option<&str>, rows: &mut Vec<WorkerModelRow>| {
        let value = StandingRoleModel {
            harness: viewed_name.clone(),
            model: model.map(str::to_owned),
        };
        if value != default {
            let selected = chosen == Some(&value);
            rows.push((label, Some(value), selected));
        }
    };
    offer(
        format!("{} default", harness_label(viewed)),
        None,
        &mut rows,
    );
    for model in catalog_of(viewed).unwrap_or_default() {
        offer(model.label.clone(), Some(model.id.as_str()), &mut rows);
    }
    if let (false, Some(model)) = (rows.iter().any(|row| row.2), chosen) {
        let label = match (&model.model, harness_of(&model.harness)) {
            (Some(id), Some(harness)) if harness == viewed => id.clone(),
            (Some(id), Some(harness)) => {
                let shown = catalog_of(harness)
                    .and_then(|models| models.iter().find(|m| &m.id == id))
                    .map_or(id.as_str(), |m| m.label.as_str());
                format!("{}: {shown}", harness_label(harness))
            }
            (Some(id), None) => format!("{}: {id}", model.harness),
            (None, Some(harness)) => format!("{} default", harness_label(harness)),
            (None, None) => model.harness.clone(),
        };
        rows.push((label, Some(model.clone()), true));
    }
    rows
}

/// Entries a Manager's chain can grow by: each Claude Code model its catalog
/// lists (the harness's own default while the catalog is not loaded), then
/// every other harness on its default model. The mock harness is never offered.
pub(crate) fn fallback_add_choices(
    claude_models: Option<&[Model]>,
) -> Vec<(String, StandingRoleModel)> {
    let mut rows = Vec::new();
    for harness in selectable_harnesses() {
        let name = harness_name(harness);
        match (harness, claude_models) {
            (HarnessId::ClaudeCode, Some(models)) if !models.is_empty() => {
                for model in models {
                    rows.push((
                        format!("Claude Code {}", model.label),
                        StandingRoleModel {
                            harness: name.clone(),
                            model: Some(model.id.clone()),
                        },
                    ));
                }
            }
            (HarnessId::ClaudeCode, _) => rows.push((
                "Claude Code".to_owned(),
                StandingRoleModel {
                    harness: name,
                    model: None,
                },
            )),
            _ => rows.push((
                name.clone(),
                StandingRoleModel {
                    harness: name,
                    model: None,
                },
            )),
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, label: &str) -> Model {
        Model {
            id: id.into(),
            label: label.into(),
            description: None,
            reasoning_levels: Vec::new(),
            options: Vec::new(),
        }
    }

    fn claude() -> Vec<Model> {
        vec![
            model("claude-sonnet-5-5", "Sonnet 5.5"),
            model("claude-opus-5-5", "Opus 5.5"),
            model("claude-haiku-4-5-20251001", "Haiku 4.5"),
        ]
    }

    fn codex() -> Vec<Model> {
        vec![
            model("gpt-5-codex", "GPT-5 Codex"),
            model("gpt-5-mini", "GPT-5 Mini"),
        ]
    }

    fn entry(harness: &str, model: Option<&str>) -> StandingRoleModel {
        StandingRoleModel {
            harness: harness.into(),
            model: model.map(str::to_owned),
        }
    }

    fn catalogs(harness: HarnessId) -> Option<&'static [Model]> {
        // Leaked once per test: a tiny fixture keeps the closure `'static`.
        match harness {
            HarnessId::ClaudeCode => Some(&*Box::leak(claude().into_boxed_slice())),
            HarnessId::Codex => Some(&*Box::leak(codex().into_boxed_slice())),
            _ => None,
        }
    }

    fn selected(rows: &[WorkerModelRow]) -> Vec<&str> {
        rows.iter()
            .filter(|row| row.2)
            .map(|row| row.0.as_str())
            .collect()
    }

    #[test]
    fn the_rows_are_the_viewed_harnesss_own_catalog() {
        let rows = worker_model_choices(None, HarnessId::Codex, catalogs);
        let labels: Vec<_> = rows.iter().map(|row| row.0.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Default (Sonnet 5.5)",
                "Codex default",
                "GPT-5 Codex",
                "GPT-5 Mini"
            ]
        );
        assert_eq!(
            rows[2].1,
            Some(entry("codex", Some("gpt-5-codex"))),
            "a Codex model names the codex harness"
        );
        assert_eq!(
            rows.iter().filter(|row| row.1.is_none()).count(),
            1,
            "only Default clears the setting"
        );
        assert_eq!(selected(&rows), ["Default (Sonnet 5.5)"]);
    }

    #[test]
    fn the_default_model_is_not_offered_twice() {
        let rows = worker_model_choices(None, HarnessId::ClaudeCode, catalogs);
        let labels: Vec<_> = rows.iter().map(|row| row.0.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Default (Sonnet 5.5)",
                "Claude Code default",
                "Opus 5.5",
                "Haiku 4.5"
            ]
        );
    }

    #[test]
    fn the_stored_choice_is_selected_on_its_harness_and_stays_visible_on_another() {
        let codex_mini = entry("codex", Some("gpt-5-mini"));
        let on_codex = worker_model_choices(Some(&codex_mini), HarnessId::Codex, catalogs);
        assert_eq!(selected(&on_codex), ["GPT-5 Mini"]);
        let on_claude = worker_model_choices(Some(&codex_mini), HarnessId::ClaudeCode, catalogs);
        assert_eq!(
            selected(&on_claude),
            ["Codex: GPT-5 Mini"],
            "viewing another harness still shows what is stored"
        );
        assert_eq!(on_claude.iter().filter(|row| row.2).count(), 1);
        let opus = entry("claude-code", Some("claude-opus-5-5"));
        assert_eq!(
            selected(&worker_model_choices(
                Some(&opus),
                HarnessId::ClaudeCode,
                catalogs
            )),
            ["Opus 5.5"]
        );
        assert_eq!(
            selected(&worker_model_choices(
                Some(&mast_proto::default_manager_worker_model()),
                HarnessId::Codex,
                catalogs
            )),
            ["Default (Sonnet 5.5)"],
            "an explicit default is the default"
        );
    }

    #[test]
    fn a_stored_model_the_catalog_does_not_list_stays_visible() {
        let retired = entry("codex", Some("gpt-4-retired"));
        let rows = worker_model_choices(Some(&retired), HarnessId::Codex, catalogs);
        assert_eq!(selected(&rows), ["gpt-4-retired"]);
        assert_eq!(rows.last().unwrap().1, Some(retired));
        let harness_only = entry("cursor", None);
        assert_eq!(
            selected(&worker_model_choices(
                Some(&harness_only),
                HarnessId::Codex,
                catalogs
            )),
            ["Cursor default"]
        );
        let unknown = entry("future-harness", Some("m1"));
        assert_eq!(
            selected(&worker_model_choices(
                Some(&unknown),
                HarnessId::Codex,
                catalogs
            )),
            ["future-harness: m1"]
        );
    }

    #[test]
    fn without_a_catalog_the_harness_default_and_the_stored_choice_still_show() {
        let none = |_: HarnessId| -> Option<&'static [Model]> { None };
        let rows = worker_model_choices(None, HarnessId::Opencode, none);
        let labels: Vec<_> = rows.iter().map(|row| row.0.as_str()).collect();
        assert_eq!(
            labels,
            ["Default (claude-sonnet-5-5)", "OpenCode default"],
            "the default shows its id until the catalog loads"
        );
        let stored = entry("opencode", Some("some/model"));
        assert_eq!(
            selected(&worker_model_choices(
                Some(&stored),
                HarnessId::Opencode,
                none
            )),
            ["some/model"]
        );
    }

    #[test]
    fn fallback_choices_follow_the_claude_catalog_and_never_offer_mock() {
        let models = claude();
        let rows = fallback_add_choices(Some(&models));
        assert!(rows.iter().all(|(_, e)| e.harness != "mock"));
        let labels: Vec<_> = rows
            .iter()
            .map(|(label, _)| label.as_str())
            .take(3)
            .collect();
        assert_eq!(
            labels,
            [
                "Claude Code Sonnet 5.5",
                "Claude Code Opus 5.5",
                "Claude Code Haiku 4.5"
            ]
        );
        assert!(
            rows.iter()
                .any(|(_, e)| e.harness == "codex" && e.model.is_none())
        );
        assert!(
            !rows
                .iter()
                .any(|(_, e)| e.model.as_deref() == Some("claude-fable-5-1")),
            "only listed models are offered"
        );
        for (_, e) in &rows {
            assert!(harness_of(&e.harness).is_some(), "{}", e.harness);
        }
        StandingRoleModel::validate_chain(
            &rows
                .iter()
                .map(|(_, e)| e.clone())
                .take(8)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for unloaded in [None, Some(&[][..])] {
            let rows = fallback_add_choices(unloaded);
            assert_eq!(
                rows[0],
                ("Claude Code".to_owned(), entry("claude-code", None)),
                "one entry on the harness default"
            );
            assert_eq!(
                rows.iter()
                    .filter(|(_, e)| e.harness == "claude-code")
                    .count(),
                1
            );
        }
    }

    #[test]
    fn a_catalog_loads_once_and_a_failure_waits_for_a_retry() {
        let mut models = ManagerModels::default();
        let here = None;
        let elsewhere = Some("device-b".to_owned());
        assert!(models.begin_load(&here, HarnessId::Codex, false));
        assert!(
            !models.begin_load(&here, HarnessId::Codex, false),
            "already loading"
        );
        assert!(
            !models.begin_load(&here, HarnessId::Codex, true),
            "a retry does not duplicate a load in flight"
        );
        assert!(
            models.begin_load(&elsewhere, HarnessId::Codex, false),
            "another device lists its own"
        );
        models.finish(&here, HarnessId::Codex, Err("probe failed".into()));
        assert_eq!(models.models(&here, HarnessId::Codex), None);
        assert!(
            !models.begin_load(&here, HarnessId::Codex, false),
            "drawing again does not retry a failure"
        );
        assert!(
            models.begin_load(&here, HarnessId::Codex, true),
            "a click does"
        );
        models.finish(&here, HarnessId::Codex, Ok(codex()));
        assert_eq!(
            models.models(&here, HarnessId::Codex).map(<[Model]>::len),
            Some(2)
        );
        assert!(
            !models.begin_load(&here, HarnessId::Codex, true),
            "a loaded catalog is kept"
        );
    }

    #[test]
    fn the_tab_starts_on_the_stored_harness_and_remembers_a_pick_per_project() {
        let mut models = ManagerModels::default();
        assert_eq!(models.viewed("p1", None), HarnessId::ClaudeCode);
        assert_eq!(
            models.viewed("p1", Some(&entry("codex", Some("gpt-5-mini")))),
            HarnessId::Codex
        );
        assert_eq!(
            models.viewed("p1", Some(&entry("future-harness", None))),
            HarnessId::ClaudeCode,
            "an unknown harness falls back"
        );
        models.view("p1", HarnessId::Cursor);
        assert_eq!(
            models.viewed("p1", Some(&entry("codex", None))),
            HarnessId::Cursor
        );
        assert_eq!(models.viewed("p2", None), HarnessId::ClaudeCode);
    }
}
