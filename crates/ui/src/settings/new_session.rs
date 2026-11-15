//! Settings → Agents "New sessions" card: the harness, model, reasoning and
//! model options a new chat starts from (`UiSettings::new_session_defaults`).
//!
//! Every row offers "Last picked", which leaves that field to the composer's
//! last-used memory. Choices come from this device's catalog; the composer
//! falls back to the last picks when a saved harness or model is no longer
//! offered where the chat runs.

use std::collections::HashMap;

use gpui::{
    AnyElement, Context, Entity, IntoElement, Render, SharedString, Task, Window, div, prelude::*,
    px,
};

use mast_engine::registry::HarnessDescriptor;
use mast_proto::{HarnessId, Model, ModelOption, ReasoningLevel};
use mast_rpc::methods;

use crate::pickers::{offered_harnesses, reasoning_label};
use crate::popover::{self, Loadable};
use crate::settings::composer::{NewSessionDefaults, RememberedModel, SessionModelDefaults};
use crate::settings::{SavePolicy, widgets};
use crate::state::AppState;
use crate::theme::Theme;

const LAST_PICKED: &str = "Last picked";

#[derive(Debug, Clone, PartialEq)]
enum Menu {
    Harness,
    Editing,
    Model,
    Reasoning,
    Option(String),
}

impl Menu {
    /// The debug-selector stem: `new-session-{key}` and `…-choice-{ix}`.
    fn key(&self) -> String {
        match self {
            Menu::Harness => "harness".into(),
            Menu::Editing => "editing".into(),
            Menu::Model => "model".into(),
            Menu::Reasoning => "reasoning".into(),
            Menu::Option(id) => format!("option-{id}"),
        }
    }
}

enum Action {
    /// Change the saved defaults.
    Write(Box<dyn Fn(&mut NewSessionDefaults)>),
    /// Switch which harness the model rows edit.
    Edit(HarnessId),
}

/// One menu entry: its label, whether it is the current value, and its action.
type Choice = (SharedString, bool, Action);

fn write(change: impl Fn(&mut NewSessionDefaults) + 'static) -> Action {
    Action::Write(Box::new(change))
}

/// One labelled row; `choices: None` renders its trigger disabled.
struct Row {
    menu: Menu,
    title: String,
    detail: Option<String>,
    value: String,
    choices: Option<Vec<Choice>>,
}

pub struct NewSessionDefaultsCard {
    state: Entity<AppState>,
    harnesses: Loadable<Vec<HarnessDescriptor>>,
    models: HashMap<HarnessId, Loadable<Vec<Model>>>,
    /// The harness whose model rows are shown; `None` follows the default.
    editing: Option<HarnessId>,
    menu: Option<Menu>,
    load_task: Option<Task<()>>,
    model_tasks: HashMap<HarnessId, Task<()>>,
}

impl NewSessionDefaultsCard {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut card = Self {
            state,
            harnesses: Loadable::Idle,
            models: HashMap::new(),
            editing: None,
            menu: None,
            load_task: None,
            model_tasks: HashMap::new(),
        };
        card.load(cx);
        card
    }

    /// A card with a fixed catalog and no RPC (tests and the preview).
    #[cfg(any(test, feature = "snapshot"))]
    pub fn with_catalog(
        state: Entity<AppState>,
        harnesses: Vec<HarnessDescriptor>,
        models: Vec<(HarnessId, Vec<Model>)>,
    ) -> Self {
        Self {
            state,
            harnesses: Loadable::Ready(harnesses),
            models: models
                .into_iter()
                .map(|(harness, list)| (harness, Loadable::Ready(list)))
                .collect(),
            editing: None,
            menu: None,
            load_task: None,
            model_tasks: HashMap::new(),
        }
    }

    /// Open the model menu, so the preview shows the inline choices.
    #[cfg(feature = "snapshot")]
    pub fn open_model_menu(&mut self) {
        self.menu = Some(Menu::Model);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.harnesses = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::LIST_HARNESSES, serde_json::json!({}))
                .await
                .map_err(|e| e.to_string())
                .and_then(|v| {
                    serde_json::from_value::<Vec<HarnessDescriptor>>(v).map_err(|e| e.to_string())
                });
            this.update(cx, |card, cx| {
                card.harnesses = match result {
                    Ok(list) => Loadable::Ready(list),
                    Err(error) => Loadable::Error(error),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn ensure_models(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        if self.models.contains_key(&harness) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.models.insert(harness, Loadable::Loading);
        let task = cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::LIST_MODELS,
                    serde_json::json!({ "harness": harness }),
                )
                .await
                .map_err(|e| e.to_string())
                .and_then(|v| serde_json::from_value::<Vec<Model>>(v).map_err(|e| e.to_string()));
            this.update(cx, |card, cx| {
                let models = match result {
                    Ok(models) => Loadable::Ready(models),
                    Err(error) => Loadable::Error(error),
                };
                card.models.insert(harness, models);
                cx.notify();
            })
            .ok();
        });
        self.model_tasks.insert(harness, task);
    }

    fn offered(&self) -> Vec<HarnessDescriptor> {
        self.harnesses
            .ready()
            .map(|list| offered_harnesses(list))
            .unwrap_or_default()
    }

    fn harness_name(&self, harness: HarnessId) -> String {
        self.harnesses
            .ready()
            .and_then(|list| list.iter().find(|d| d.id == harness))
            .map(|d| d.name.clone())
            .unwrap_or_else(|| format!("{harness:?}"))
    }

    /// The harness the model rows edit: the picked one, else the default
    /// agent, else the first offered. Never one this device doesn't offer.
    fn editing_harness(&self, defaults: &NewSessionDefaults) -> Option<HarnessId> {
        let offered = self.offered();
        [self.editing, defaults.harness]
            .into_iter()
            .flatten()
            .find(|id| offered.iter().any(|d| d.id == *id))
            .or_else(|| offered.first().map(|d| d.id))
    }

    fn catalog(&self, harness: HarnessId) -> Option<&Vec<Model>> {
        self.models.get(&harness).and_then(Loadable::ready)
    }

    fn configured_catalog_model(
        &self,
        harness: HarnessId,
        current: Option<&SessionModelDefaults>,
    ) -> Option<&Model> {
        let id = &current?.model.as_ref()?.id;
        self.catalog(harness)?.iter().find(|m| &m.id == id)
    }

    fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.menu = if self.menu.as_ref() == Some(&menu) {
            None
        } else {
            Some(menu)
        };
        cx.notify();
    }

    fn apply(&mut self, action: &Action, cx: &mut Context<Self>) {
        self.menu = None;
        match action {
            Action::Write(change) => {
                crate::settings::update(SavePolicy::Debounced, cx, |settings| {
                    change(&mut settings.new_session_defaults)
                });
            }
            Action::Edit(harness) => self.editing = Some(*harness),
        }
        cx.notify();
    }

    fn harness_row(&self, defaults: &NewSessionDefaults) -> Row {
        let offered = self.offered();
        let value = match defaults.harness {
            Some(id) if !offered.iter().any(|d| d.id == id) => {
                format!("{} (unavailable)", self.harness_name(id))
            }
            Some(id) => self.harness_name(id),
            None => LAST_PICKED.into(),
        };
        let mut choices: Vec<Choice> = vec![(
            LAST_PICKED.into(),
            defaults.harness.is_none(),
            write(|d| d.harness = None),
        )];
        for descriptor in offered {
            let id = descriptor.id;
            choices.push((
                descriptor.name.into(),
                defaults.harness == Some(id),
                write(move |d| d.harness = Some(id)),
            ));
        }
        Row {
            menu: Menu::Harness,
            title: "Default agent".into(),
            detail: Some("The agent a new chat starts on.".into()),
            value,
            choices: Some(choices),
        }
    }

    fn editing_row(&self, defaults: &NewSessionDefaults, editing: HarnessId) -> Row {
        let offered = self.offered();
        let choices = offered
            .iter()
            .map(|d| {
                let label = if defaults.for_harness(d.id).is_some() {
                    format!("{} · customized", d.name)
                } else {
                    d.name.clone()
                };
                (label.into(), d.id == editing, Action::Edit(d.id))
            })
            .collect();
        Row {
            menu: Menu::Editing,
            title: "Defaults for".into(),
            detail: Some("Each agent keeps its own model, reasoning and options.".into()),
            value: self.harness_name(editing),
            choices: (offered.len() > 1).then_some(choices),
        }
    }

    fn model_row(&self, harness: HarnessId, current: Option<&SessionModelDefaults>) -> Row {
        let picked = current.and_then(|c| c.model.as_ref());
        let catalog = self.catalog(harness);
        let value = match (picked, catalog) {
            (None, _) => LAST_PICKED.to_string(),
            (Some(model), Some(list)) => match list.iter().find(|m| m.id == model.id) {
                Some(found) => found.label.clone(),
                None => format!("{} (unavailable)", model.label),
            },
            (Some(model), None) => model.label.clone(),
        };
        let detail = match self.models.get(&harness) {
            Some(Loadable::Error(error)) => Some(format!("Couldn't load models: {error}")),
            Some(Loadable::Ready(_)) => None,
            _ => Some("Loading models…".into()),
        };
        let choices = catalog.map(|list| {
            let picked = picked.map(|m| m.id.as_str());
            let mut choices: Vec<Choice> = vec![(
                LAST_PICKED.into(),
                picked.is_none(),
                write(move |d| d.set_model(harness, None)),
            )];
            for model in list {
                let remembered = RememberedModel {
                    id: model.id.clone(),
                    label: model.label.clone(),
                };
                choices.push((
                    model.label.clone().into(),
                    picked == Some(model.id.as_str()),
                    write(move |d| d.set_model(harness, Some(remembered.clone()))),
                ));
            }
            choices
        });
        Row {
            menu: Menu::Model,
            title: "Model".into(),
            detail,
            value,
            choices,
        }
    }

    /// The configured model's ladder, else the harness's, else every level
    /// any of its models offers.
    fn ladder(
        &self,
        harness: HarnessId,
        current: Option<&SessionModelDefaults>,
    ) -> Vec<ReasoningLevel> {
        if let Some(model) = self.configured_catalog_model(harness, current)
            && !model.reasoning_levels.is_empty()
        {
            return model.reasoning_levels.clone();
        }
        if let Some(descriptor) = self
            .harnesses
            .ready()
            .and_then(|list| list.iter().find(|d| d.id == harness))
            && !descriptor.reasoning_levels.is_empty()
        {
            return descriptor.reasoning_levels.clone();
        }
        let mut levels: Vec<ReasoningLevel> = self
            .catalog(harness)
            .into_iter()
            .flatten()
            .flat_map(|m| m.reasoning_levels.iter().copied())
            .collect();
        levels.sort();
        levels.dedup();
        levels
    }

    fn reasoning_row(&self, harness: HarnessId, current: Option<&SessionModelDefaults>) -> Row {
        let picked = current.and_then(|c| c.reasoning);
        let ladder = self.ladder(harness, current);
        let detail = ladder
            .is_empty()
            .then(|| "This agent's models don't offer reasoning levels.".to_string());
        // A saved level stays clearable even when the ladder is empty.
        let choices = (!ladder.is_empty() || picked.is_some()).then(|| {
            let mut choices: Vec<Choice> = vec![(
                LAST_PICKED.into(),
                picked.is_none(),
                write(move |d| d.set_reasoning(harness, None)),
            )];
            for level in ladder {
                choices.push((
                    reasoning_label(level).into(),
                    picked == Some(level),
                    write(move |d| d.set_reasoning(harness, Some(level))),
                ));
            }
            choices
        });
        Row {
            menu: Menu::Reasoning,
            title: "Reasoning".into(),
            detail,
            value: picked
                .map(|level| reasoning_label(level).to_string())
                .unwrap_or_else(|| LAST_PICKED.into()),
            choices,
        }
    }

    fn option_row(
        harness: HarnessId,
        option: &ModelOption,
        current: Option<&SessionModelDefaults>,
    ) -> Row {
        let picked = current
            .and_then(|c| c.model_options.get(&option.id))
            .and_then(|v| v.as_str());
        let value = picked
            .and_then(|id| option.choices.iter().find(|c| c.id == id))
            .map(|c| c.label.clone())
            .unwrap_or_else(|| LAST_PICKED.into());
        let id = option.id.clone();
        let mut choices: Vec<Choice> = vec![(
            LAST_PICKED.into(),
            picked.is_none(),
            write(move |d| d.set_model_option(harness, &id, None)),
        )];
        for choice in &option.choices {
            let id = option.id.clone();
            let choice_id = choice.id.clone();
            let label = if choice.id == option.default_choice {
                format!("{} (default)", choice.label)
            } else {
                choice.label.clone()
            };
            choices.push((
                label.into(),
                picked == Some(choice.id.as_str()),
                write(move |d| d.set_model_option(harness, &id, Some(choice_id.clone()))),
            ));
        }
        Row {
            menu: Menu::Option(option.id.clone()),
            title: option.label.clone(),
            detail: None,
            value,
            choices: Some(choices),
        }
    }

    fn rows(&mut self, cx: &mut Context<Self>) -> Vec<Row> {
        let defaults = crate::settings::new_session_defaults(cx)
            .cloned()
            .unwrap_or_default();
        let mut rows = vec![self.harness_row(&defaults)];
        let Some(editing) = self.editing_harness(&defaults) else {
            return rows;
        };
        self.ensure_models(editing, cx);
        let current = defaults.for_harness(editing);
        rows.push(self.editing_row(&defaults, editing));
        rows.push(self.model_row(editing, current));
        rows.push(self.reasoning_row(editing, current));
        if let Some(model) = self.configured_catalog_model(editing, current) {
            for option in &model.options {
                rows.push(Self::option_row(editing, option, current));
            }
        }
        rows
    }

    fn render_row(&self, theme: &Theme, row: Row, cx: &mut Context<Self>) -> AnyElement {
        let key = row.menu.key();
        let interactive = row.choices.is_some();
        let open = interactive && self.menu.as_ref() == Some(&row.menu);
        let menu = row.menu.clone();
        let trigger_selector = format!("new-session-{key}");
        let header =
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(widgets::row_title(theme, row.title))
                        .children(row.detail.map(|detail| {
                            widgets::meta_line(theme, vec![div().child(detail).into_any_element()])
                        })),
                )
                .child(
                    widgets::ghost_action(theme)
                        .id(SharedString::from(trigger_selector.clone()))
                        .debug_selector(move || trigger_selector)
                        .flex_none()
                        .when(interactive, |el| {
                            el.cursor_pointer()
                                .hover(|s| widgets::ghost_hover(theme, s))
                                .on_click(cx.listener(move |card, _, _, cx| {
                                    card.toggle_menu(menu.clone(), cx)
                                }))
                        })
                        .when(!interactive, |el| el.opacity(0.5))
                        .child(row.value),
                );
        let choices =
            row.choices.filter(|_| open).map(|choices| {
                div()
                    .id(SharedString::from(format!("new-session-{key}-choices")))
                    .mt(px(6.0))
                    .max_h(px(240.0))
                    .overflow_y_scroll()
                    .children(choices.into_iter().enumerate().map(
                        |(ix, (label, active, action))| {
                            let selector = format!("new-session-{key}-choice-{ix}");
                            popover::menu_row(theme, active, selector.clone())
                                .id(SharedString::from(selector.clone()))
                                .debug_selector(move || selector)
                                .on_click(
                                    cx.listener(move |card, _, _, cx| card.apply(&action, cx)),
                                )
                                .child(label)
                        },
                    ))
            });
        div()
            .mt(px(12.0))
            .child(header)
            .children(choices)
            .into_any_element()
    }
}

impl Render for NewSessionDefaultsCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let card = widgets::section_card(&theme)
            .id("new-session-defaults")
            .mt(px(20.0))
            .p(px(16.0))
            .child(widgets::row_title(&theme, "New sessions"))
            .child(widgets::page_subtitle(
                &theme,
                "What a new chat starts with on this device. \"Last picked\" keeps the \
                 composer's most recent choice, which is also used when a default is no longer \
                 available.",
            ));
        match &self.harnesses {
            Loadable::Ready(_) => {
                let rows = self.rows(cx);
                let rows: Vec<AnyElement> = rows
                    .into_iter()
                    .map(|row| self.render_row(&theme, row, cx))
                    .collect();
                card.children(rows)
            }
            Loadable::Error(error) => {
                card.child(widgets::error_strip(&theme, error.clone()).mt(px(8.0)))
            }
            Loadable::Idle | Loadable::Loading => {
                card.child(div().mt(px(8.0)).child("Loading agents…"))
            }
        }
    }
}

#[cfg(test)]
mod tests;
