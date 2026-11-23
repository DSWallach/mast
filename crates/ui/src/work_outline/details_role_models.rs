//! Default models per role in the Space-root details.
//!
//! The Manager and worker defaults (`setRoleDefaultModel`) are the models a
//! Claude Code chat starts on when its launch names none. Only ids that
//! `list_models` reports for Claude Code can be chosen: the chips come from
//! that catalog and a save checks the id against it again, so a stale or
//! mistyped id never reaches the Space. A stored id the catalog no longer
//! lists stays visible (selected, flagged) and can be replaced or cleared.
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use mast_proto::work_outline::{DefaultModelRole, SetRoleDefaultModelParams, WorkOutlineSpaceSettings};
use mast_proto::{DEFAULT_CLAUDE_MODEL, DEFAULT_WORKER_CLAUDE_MODEL, HarnessId, Model};

use super::super::actions::new_operation_id;
use super::super::store::WorkOutlineSnapshot;
use super::WorkItemDetails;
use crate::theme::Theme;

/// The roles in display order.
const ROLES: [DefaultModelRole; 2] = [DefaultModelRole::Manager, DefaultModelRole::Worker];

/// The Claude Code model catalog for the Space's device, loaded once per
/// (Space, device) and again on Reload.
#[derive(Default)]
pub(super) struct RoleModelsState {
    /// The (Space, device) the catalog was requested for. The device is
    /// `None` until the Space row is known, so the request repeats once it is.
    requested_for: Option<(String, Option<String>)>,
    /// `None` while loading.
    catalog: Option<Result<Vec<Model>, String>>,
    /// Bumped per request so a late reply for an old Space is dropped.
    generation: u64,
    error: Option<String>,
    /// A save is in flight. Chip clicks wait for it: every click carries its
    /// own operation id, so two overlapping saves could land out of order.
    saving: bool,
}

/// One chip of a role's model control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelChoice {
    pub(super) id: String,
    pub(super) label: String,
    /// What it sends: `None` = the built-in default.
    pub(super) model: Option<String>,
    pub(super) selected: bool,
}

fn role_key(role: DefaultModelRole) -> &'static str {
    match role {
        DefaultModelRole::Manager => "manager",
        DefaultModelRole::Worker => "worker",
    }
}

fn role_title(role: DefaultModelRole) -> &'static str {
    match role {
        DefaultModelRole::Manager => "Manager default model",
        DefaultModelRole::Worker => "Worker default model",
    }
}

/// The model a role starts on when nothing is chosen.
pub(super) fn built_in_model(role: DefaultModelRole) -> &'static str {
    match role {
        DefaultModelRole::Manager => DEFAULT_CLAUDE_MODEL,
        DefaultModelRole::Worker => DEFAULT_WORKER_CLAUDE_MODEL,
    }
}

fn stored_model(settings: &WorkOutlineSpaceSettings, role: DefaultModelRole) -> Option<&str> {
    match role {
        DefaultModelRole::Manager => settings.manager_default_model.as_deref(),
        DefaultModelRole::Worker => settings.worker_default_model.as_deref(),
    }
}

fn label_of<'a>(catalog: &'a [Model], id: &'a str) -> &'a str {
    catalog.iter().find(|m| m.id == id).map_or(id, |m| m.label.as_str())
}

/// Default (the built-in), then every catalog model, then the stored id when
/// the catalog does not list it. The built-in has no chip of its own: it is
/// the Default, and a stored id equal to it selects the Default. `catalog` is
/// `None` while the list loads or when it failed: no model chips are offered
/// and the stored id is shown without a verdict on it.
pub(super) fn role_model_choices(role: DefaultModelRole, stored: Option<&str>, catalog: Option<&[Model]>) -> Vec<ModelChoice> {
    let listed_known = catalog.is_some();
    let catalog = catalog.unwrap_or_default();
    let built_in = built_in_model(role);
    let key = role_key(role);
    let mut choices = vec![ModelChoice {
        id: format!("role-model-{key}-default"),
        label: format!("Default ({})", label_of(catalog, built_in)),
        model: None,
        selected: stored.is_none_or(|s| s == built_in),
    }];
    choices.extend(catalog.iter().filter(|m| m.id != built_in).map(|m| ModelChoice {
        id: format!("role-model-{key}-{}", m.id),
        label: m.label.clone(),
        model: Some(m.id.clone()),
        selected: stored == Some(m.id.as_str()),
    }));
    if let Some(stored) = stored
        && stored != built_in
        && !catalog.iter().any(|m| m.id == stored)
    {
        let verdict = if listed_known { " (not in the model list)" } else { "" };
        choices.push(ModelChoice {
            id: format!("role-model-{key}-{stored}"),
            label: format!("{stored}{verdict}"),
            model: Some(stored.to_owned()),
            selected: true,
        });
    }
    choices
}

/// `model` when the catalog lists it; `None` (the built-in) always passes.
/// An unloaded, failed or empty catalog accepts no explicit id.
pub(super) fn validated_role_model(catalog: Option<&Result<Vec<Model>, String>>, model: Option<&str>) -> Result<Option<String>, String> {
    let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) else {
        return Ok(None);
    };
    match catalog {
        Some(Ok(models)) if models.iter().any(|m| m.id == model) => Ok(Some(model.to_owned())),
        Some(Ok(models)) if models.is_empty() => Err("No Claude Code models are available on this device.".into()),
        Some(Ok(_)) => Err(format!("{model} is not a Claude Code model this device lists.")),
        Some(Err(error)) => Err(format!("Could not check {model}: the model list failed to load ({error}).")),
        None => Err("The model list is still loading.".into()),
    }
}

/// The line under a role's chips.
fn effective_line(role: DefaultModelRole, stored: Option<&str>, catalog: &[Model]) -> String {
    match stored {
        Some(model) if model != built_in_model(role) => format!("Effective: {}", label_of(catalog, model)),
        _ => format!("Effective: {} (built-in default)", label_of(catalog, built_in_model(role))),
    }
}

impl WorkItemDetails {
    /// The device that lists the Space's models: the one that owns the Space.
    fn role_model_target(&self, cx: &Context<Self>) -> Option<String> {
        self.state.read(cx).space_row(&self.space_id).map(|space| space.device_id.clone())
    }

    fn load_role_model_catalog(&mut self, cx: &mut Context<Self>) {
        let target = self.role_model_target(cx);
        self.role_models.requested_for = Some((self.space_id.clone(), target.clone()));
        self.role_models.catalog = None;
        self.role_models.generation += 1;
        let generation = self.role_models.generation;
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .list_models(HarnessId::ClaudeCode, target);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.role_models.generation == generation {
                    // The same rows the pickers show: no literal `default` row,
                    // and a `[1m]` variant folds into its base model.
                    this.role_models.catalog = Some(
                        result
                            .map(|models| crate::pickers::normalize_model_rows(HarnessId::ClaudeCode, models))
                            .map_err(|error| error.message),
                    );
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn save_role_default_model(&mut self, role: DefaultModelRole, model: Option<String>, cx: &mut Context<Self>) {
        if self.role_models.saving {
            return;
        }
        // Choosing what is already stored changes nothing, so it neither
        // checks the catalog nor sends.
        let stored = self
            .snapshot
            .as_deref()
            .map(|snapshot| stored_model(&snapshot.space_settings, role));
        if stored.is_some_and(|stored| stored == model.as_deref()) {
            return;
        }
        let model = match validated_role_model(self.role_models.catalog.as_ref(), model.as_deref()) {
            Ok(model) => model,
            Err(error) => {
                self.role_models.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.role_models.error = None;
        self.role_models.saving = true;
        // A fresh operation id per choice is right for a set-to-value change;
        // the actions facade retries a lost reply with the same params.
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .mutate(mast_proto::WorkOutlineMutation::SetRoleDefaultModel(SetRoleDefaultModelParams {
                space_id: self.space_id.clone(),
                role,
                model,
                operation_id: new_operation_id(),
            }));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.role_models.saving = false;
                if let Err(error) = result {
                    this.role_models.error = Some(error.message);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_role_default_models(
        &mut self,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wanted = (self.space_id.clone(), self.role_model_target(cx));
        if self.role_models.requested_for.as_ref() != Some(&wanted) {
            self.role_models.error = None;
            self.load_role_model_catalog(cx);
        }
        let catalog: Option<&[Model]> = match &self.role_models.catalog {
            Some(Ok(models)) => Some(models),
            _ => None,
        };
        let mut section = crate::components::outline_section("role-default-models", theme)
            .child(crate::components::outline_detail_heading("Default models", theme));
        let mut rows = Vec::new();
        for role in ROLES {
            let stored = stored_model(&snapshot.space_settings, role);
            let mut chips = crate::components::outline_detail_row(theme).flex_wrap().gap(px(6.0));
            for choice in role_model_choices(role, stored, catalog) {
                let model = choice.model.clone();
                let selector = choice.id.clone();
                chips = chips.child(
                    div()
                        .id(SharedString::from(choice.id))
                        .debug_selector(move || selector.clone())
                        .cursor_pointer()
                        .px(px(8.0))
                        .py(px(4.0))
                        .border_1()
                        .border_color(theme.border)
                        .text_color(theme.text)
                        .child(if choice.selected {
                            format!("✓ {}", choice.label)
                        } else {
                            choice.label
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.save_role_default_model(role, model.clone(), cx))),
                );
            }
            rows.push((role, chips, effective_line(role, stored, catalog.unwrap_or_default())));
        }
        for (role, chips, effective) in rows {
            let key = role_key(role);
            let effective_id = format!("role-model-{key}-effective");
            section = section
                .child(crate::components::outline_execution_line(
                    role_title(role).into(),
                    11.0,
                    theme.text,
                    theme,
                ))
                .child(chips)
                .child(
                    crate::components::outline_execution_line(effective, 11.0, theme.text, theme)
                        .debug_selector(move || effective_id.clone()),
                );
        }
        let note = |text: String, selector: &'static str| {
            crate::components::outline_execution_line(text, 11.0, theme.text_faint, theme).debug_selector(move || selector.into())
        };
        match &self.role_models.catalog {
            None => section = section.child(note("Loading models…".into(), "role-model-loading")),
            Some(Err(error)) => {
                section = section.child(note(format!("Could not load the model list: {error}"), "role-model-catalog-error"))
            }
            Some(Ok(models)) if models.is_empty() => {
                section = section.child(note(
                    "Claude Code lists no models on this device.".into(),
                    "role-model-catalog-empty",
                ))
            }
            Some(Ok(_)) => {}
        }
        if self.role_models.saving {
            section = section.child(note("Saving…".into(), "role-model-saving"));
        }
        if self.role_models.catalog.is_some() {
            section = section.child(
                div()
                    .id("role-model-reload")
                    .debug_selector(|| "role-model-reload".into())
                    .cursor_pointer()
                    .px(px(8.0))
                    .py(px(4.0))
                    .border_1()
                    .border_color(theme.border)
                    .text_color(theme.text)
                    .child("Reload models")
                    .on_click(cx.listener(|this, _, _, cx| this.load_role_model_catalog(cx))),
            );
        }
        if let Some(error) = &self.role_models.error {
            section = section.child(
                crate::components::outline_execution_line(error.clone(), 11.0, theme.text, theme)
                    .debug_selector(|| "role-model-error".into()),
            );
        }
        section
            .child(crate::components::outline_execution_line(
                "Used by new Claude Code chats started without a model: Managers and workers. Running chats keep their model.".into(),
                11.0,
                theme.text_faint,
                theme,
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, label: &str) -> Model {
        serde_json::from_value(serde_json::json!({"id": id, "label": label})).unwrap()
    }

    fn catalog() -> Vec<Model> {
        vec![
            model("claude-opus-5-5", "Opus 5.5"),
            model("claude-sonnet-5-5", "Sonnet 5.5"),
            model("claude-haiku-4-5-20251001", "Haiku 4.5"),
        ]
    }

    fn selected(choices: &[ModelChoice]) -> Vec<&str> {
        choices.iter().filter(|c| c.selected).map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn each_role_defaults_to_its_built_in_and_has_no_chip_for_it() {
        let worker = role_model_choices(DefaultModelRole::Worker, None, Some(&catalog()));
        assert_eq!(selected(&worker), ["Default (Sonnet 5.5)"]);
        assert_eq!(
            worker.iter().map(|c| c.model.as_deref()).collect::<Vec<_>>(),
            [None, Some("claude-opus-5-5"), Some("claude-haiku-4-5-20251001")]
        );
        let manager = role_model_choices(DefaultModelRole::Manager, None, Some(&catalog()));
        assert_eq!(selected(&manager), ["Default (Opus 5.5)"]);
        assert_eq!(
            manager.iter().map(|c| c.model.as_deref()).collect::<Vec<_>>(),
            [None, Some("claude-sonnet-5-5"), Some("claude-haiku-4-5-20251001")]
        );
    }

    #[test]
    fn a_stored_choice_selects_its_chip_and_an_unlisted_one_stays_visible() {
        let chosen = role_model_choices(DefaultModelRole::Worker, Some("claude-haiku-4-5-20251001"), Some(&catalog()));
        assert_eq!(selected(&chosen), ["Haiku 4.5"]);
        // An explicit built-in is the Default.
        let explicit = role_model_choices(DefaultModelRole::Worker, Some("claude-sonnet-5-5"), Some(&catalog()));
        assert_eq!(selected(&explicit), ["Default (Sonnet 5.5)"]);
        // A stored id the catalog dropped is shown, selected and flagged.
        let stale = role_model_choices(DefaultModelRole::Worker, Some("claude-retired-1"), Some(&catalog()));
        assert_eq!(selected(&stale), ["claude-retired-1 (not in the model list)"]);
        assert_eq!(stale.last().unwrap().model.as_deref(), Some("claude-retired-1"));
        // Without a catalog only the Default and the stored id are offered.
        let offline = role_model_choices(DefaultModelRole::Manager, Some("claude-retired-1"), None);
        assert_eq!(offline.len(), 2);
        assert_eq!(offline[0].label, "Default (claude-opus-5-5)");
        // No list means no verdict: the stored id is not called unlisted.
        assert_eq!(offline[1].label, "claude-retired-1");
        assert!(offline[1].selected);
        // A list that loaded empty does give one.
        let empty = role_model_choices(DefaultModelRole::Manager, Some("claude-retired-1"), Some(&[]));
        assert_eq!(empty[1].label, "claude-retired-1 (not in the model list)");
    }

    #[test]
    fn only_listed_models_validate() {
        let ok: Result<Vec<Model>, String> = Ok(catalog());
        assert_eq!(
            validated_role_model(Some(&ok), Some("claude-sonnet-5-5")),
            Ok(Some("claude-sonnet-5-5".into()))
        );
        assert_eq!(
            validated_role_model(Some(&ok), Some(" claude-sonnet-5-5 ")),
            Ok(Some("claude-sonnet-5-5".into()))
        );
        // Clearing never needs the catalog.
        assert_eq!(validated_role_model(None, None), Ok(None));
        assert_eq!(validated_role_model(Some(&ok), Some("  ")), Ok(None));
        assert!(
            validated_role_model(Some(&ok), Some("sonnet"))
                .unwrap_err()
                .contains("not a Claude Code model")
        );
        let empty: Result<Vec<Model>, String> = Ok(Vec::new());
        assert!(
            validated_role_model(Some(&empty), Some("claude-sonnet-5-5"))
                .unwrap_err()
                .contains("No Claude Code models")
        );
        let failed: Result<Vec<Model>, String> = Err("offline".into());
        assert!(
            validated_role_model(Some(&failed), Some("claude-sonnet-5-5"))
                .unwrap_err()
                .contains("offline")
        );
        assert!(
            validated_role_model(None, Some("claude-sonnet-5-5"))
                .unwrap_err()
                .contains("loading")
        );
    }

    #[test]
    fn the_effective_line_names_the_model_a_new_chat_gets() {
        assert_eq!(
            effective_line(DefaultModelRole::Worker, None, &catalog()),
            "Effective: Sonnet 5.5 (built-in default)"
        );
        assert_eq!(
            effective_line(DefaultModelRole::Worker, Some("claude-haiku-4-5-20251001"), &catalog()),
            "Effective: Haiku 4.5"
        );
        assert_eq!(
            effective_line(DefaultModelRole::Manager, Some("claude-retired-1"), &catalog()),
            "Effective: claude-retired-1"
        );
    }
}
