//! The models a project Manager's Settings tab offers for its workers and its
//! credit fallback chain, read from each harness's own model catalog instead of
//! a list written into the app.
//!
//! The Settings tab shows one harness's models at a time (an OpenCode catalog
//! runs to hundreds), starting on the stored worker model's harness. A
//! catalog loads once per (device, harness) the first time it is shown, from
//! the device that owns the project's Space, and reloads quietly once it is a
//! few minutes old. A failed load is kept, not retried on every draw; the tab
//! offers a retry that bypasses the engine's cache and failure cooldown.
//!
//! Nothing here touches the network: callers start a load when
//! [`ManagerModels::begin_load`] says so and hand the result back to
//! [`ManagerModels::finish`].

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use mast_proto::{HarnessId, Model, StandingRoleModel};

/// What a (device, harness) model catalog currently holds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CatalogState {
    Loading,
    Ready(Vec<Model>),
    Failed(String),
}

/// How old a loaded catalog gets before the next draw of the tab reloads it, so
/// a harness signed in or updated since is picked up without an app restart.
pub(crate) const REFRESH_AFTER: Duration = Duration::from_secs(5 * 60);

/// A selected model at or beyond this row moves up next to Default (about one
/// screenful of chips in the scrolling list).
const SELECTED_PINNED_FROM: usize = 12;

/// A catalog's key: the device that lists it (`None` for this device) and the
/// harness.
type CatalogKey = (Option<String>, HarnessId);

/// Catalogs loaded for the Manager settings, and which harness each project's
/// tab is showing.
#[derive(Debug, Default)]
pub(crate) struct ManagerModels {
    catalogs: HashMap<CatalogKey, CatalogState>,
    /// When each catalog last loaded (or last failed to refresh).
    loaded: HashMap<CatalogKey, Instant>,
    /// Loaded catalogs being reloaded; their rows stay in place.
    refreshing: HashSet<CatalogKey>,
    viewing: HashMap<String, HarnessId>,
}

impl ManagerModels {
    /// Whether the caller should start a load for this catalog now. A catalog
    /// that is loading is left alone; a failed one reloads only when `retry`
    /// (a click), never because the tab drew again; a loaded one reloads once
    /// it is [`REFRESH_AFTER`] old, keeping its rows on screen meanwhile.
    pub(crate) fn begin_load(
        &mut self,
        target: &Option<String>,
        harness: HarnessId,
        retry: bool,
        now: Instant,
    ) -> bool {
        let key = (target.clone(), harness);
        match self.catalogs.get(&key) {
            None => {}
            Some(CatalogState::Failed(_)) if retry => {}
            Some(CatalogState::Ready(_))
                if !self.refreshing.contains(&key)
                    && self
                        .loaded
                        .get(&key)
                        .is_some_and(|at| now.saturating_duration_since(*at) >= REFRESH_AFTER) =>
            {
                self.refreshing.insert(key);
                return true;
            }
            Some(_) => return false,
        }
        self.catalogs.insert(key, CatalogState::Loading);
        true
    }

    /// Record how a load ended. A refresh that fails keeps the rows it had and
    /// waits another [`REFRESH_AFTER`].
    pub(crate) fn finish(
        &mut self,
        target: &Option<String>,
        harness: HarnessId,
        result: Result<Vec<Model>, String>,
        now: Instant,
    ) {
        let key = (target.clone(), harness);
        self.refreshing.remove(&key);
        match result {
            Ok(models) => {
                self.catalogs
                    .insert(key.clone(), CatalogState::Ready(models));
                self.loaded.insert(key, now);
            }
            Err(_) if matches!(self.catalogs.get(&key), Some(CatalogState::Ready(_))) => {
                self.loaded.insert(key, now);
            }
            Err(error) => {
                self.catalogs.insert(key, CatalogState::Failed(error));
            }
        }
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
    // The chips sit in a scrolling box: a selected row far down a long catalog
    // would be out of sight, so it moves up beside Default.
    if let Some(at) = rows
        .iter()
        .position(|row| row.2)
        .filter(|at| *at >= SELECTED_PINNED_FROM)
    {
        let selected = rows.remove(at);
        rows.insert(1, selected);
    }
    rows
}

/// Entries a Manager's chain can grow by: Claude Code on its own default, then
/// each Claude Code model its catalog lists, then every other harness on its
/// default model. The default chip stays once the catalog loads, so an entry
/// added before then can still be toggled off. The mock harness is never offered.
pub(crate) fn fallback_add_choices(
    claude_models: Option<&[Model]>,
) -> Vec<(String, StandingRoleModel)> {
    let mut rows = Vec::new();
    for harness in selectable_harnesses() {
        let name = harness_name(harness);
        let label = if harness == HarnessId::ClaudeCode {
            "Claude Code".to_owned()
        } else {
            name.clone()
        };
        rows.push((
            label,
            StandingRoleModel {
                harness: name.clone(),
                model: None,
            },
        ));
        if harness == HarnessId::ClaudeCode {
            for model in claude_models.unwrap_or_default() {
                rows.push((
                    format!("Claude Code {}", model.label),
                    StandingRoleModel {
                        harness: name.clone(),
                        model: Some(model.id.clone()),
                    },
                ));
            }
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
    fn a_selected_model_far_down_a_long_catalog_moves_up_beside_default() {
        let long: Vec<Model> = (0..40)
            .map(|n| model(&format!("m{n}"), &format!("Model {n}")))
            .collect();
        let long: &'static [Model] = Box::leak(long.into_boxed_slice());
        let catalog = move |harness| (harness == HarnessId::Opencode).then_some(long);
        let deep = entry("opencode", Some("m35"));
        let rows = worker_model_choices(Some(&deep), HarnessId::Opencode, catalog);
        assert_eq!(rows[1].0, "Model 35", "right after Default");
        assert!(rows[1].2);
        assert_eq!(rows.iter().filter(|row| row.2).count(), 1);
        assert_eq!(rows.len(), 1 + 1 + 40, "nothing is dropped or doubled");
        let near = entry("opencode", Some("m2"));
        let rows = worker_model_choices(Some(&near), HarnessId::Opencode, catalog);
        assert_eq!(
            rows[1].0, "OpenCode default",
            "a row already on screen keeps its place"
        );
        assert_eq!(rows.iter().position(|row| row.2), Some(4));
        let unlisted = entry("opencode", Some("m99"));
        let rows = worker_model_choices(Some(&unlisted), HarnessId::Opencode, catalog);
        assert_eq!(
            rows[1].0, "m99",
            "an unlisted stored model is pulled up too"
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
            .take(4)
            .collect();
        assert_eq!(
            labels,
            [
                "Claude Code",
                "Claude Code Sonnet 5.5",
                "Claude Code Opus 5.5",
                "Claude Code Haiku 4.5"
            ],
            "the harness default stays beside the listed models"
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
        let now = Instant::now();
        let here = None;
        let elsewhere = Some("device-b".to_owned());
        assert!(models.begin_load(&here, HarnessId::Codex, false, now));
        assert!(
            !models.begin_load(&here, HarnessId::Codex, false, now),
            "already loading"
        );
        assert!(
            !models.begin_load(&here, HarnessId::Codex, true, now),
            "a retry does not duplicate a load in flight"
        );
        assert!(
            models.begin_load(&elsewhere, HarnessId::Codex, false, now),
            "another device lists its own"
        );
        models.finish(&here, HarnessId::Codex, Err("probe failed".into()), now);
        assert_eq!(models.models(&here, HarnessId::Codex), None);
        let much_later = now + REFRESH_AFTER * 10;
        assert!(
            !models.begin_load(&here, HarnessId::Codex, false, much_later),
            "drawing again, however much later, does not retry a failure"
        );
        assert!(
            models.begin_load(&here, HarnessId::Codex, true, now),
            "a click does"
        );
        models.finish(&here, HarnessId::Codex, Ok(codex()), now);
        assert_eq!(
            models.models(&here, HarnessId::Codex).map(<[Model]>::len),
            Some(2)
        );
        assert!(
            !models.begin_load(&here, HarnessId::Codex, true, now),
            "a fresh catalog is kept"
        );
    }

    #[test]
    fn an_old_catalog_refreshes_once_and_keeps_its_rows_if_the_refresh_fails() {
        let mut models = ManagerModels::default();
        let start = Instant::now();
        let here = None;
        assert!(models.begin_load(&here, HarnessId::Codex, false, start));
        models.finish(&here, HarnessId::Codex, Ok(codex()), start);
        let soon = start + REFRESH_AFTER - Duration::from_secs(1);
        assert!(!models.begin_load(&here, HarnessId::Codex, false, soon));
        let due = start + REFRESH_AFTER;
        assert!(
            models.begin_load(&here, HarnessId::Codex, false, due),
            "an old catalog reloads"
        );
        assert!(
            !models.begin_load(&here, HarnessId::Codex, false, due),
            "once, not on every draw"
        );
        assert_eq!(
            models.models(&here, HarnessId::Codex).map(<[Model]>::len),
            Some(2),
            "the old rows stay on screen while it loads"
        );
        models.finish(&here, HarnessId::Codex, Err("offline".into()), due);
        assert_eq!(
            models.models(&here, HarnessId::Codex).map(<[Model]>::len),
            Some(2),
            "a failed refresh does not replace the rows with an error"
        );
        assert!(
            !models.begin_load(&here, HarnessId::Codex, false, due + Duration::from_secs(1)),
            "and waits a full interval before trying again"
        );
        assert!(models.begin_load(&here, HarnessId::Codex, false, due + REFRESH_AFTER));
        models.finish(
            &here,
            HarnessId::Codex,
            Ok(vec![model("gpt-6", "GPT-6")]),
            due + REFRESH_AFTER,
        );
        assert_eq!(
            models.models(&here, HarnessId::Codex).map(<[Model]>::len),
            Some(1)
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
