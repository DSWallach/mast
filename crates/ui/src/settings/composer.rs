//! Sticky composer defaults — the new-chat "remember my last picks" store
//! (zeron parity: localStorage `zeron.composer.defaults:v1`, defaults.ts).
//!
//! A small JSON file beside `ui-settings.json` (that file is the shell's and
//! is saved debounced from its own boot-time copy, so the composer keeps its
//! own file rather than racing it): last harness, last model per harness
//! (id + label, so the chip names the pick before the model list loads),
//! last reasoning level, and last model option picks per harness. Written
//! synchronously on every pick (picks are rare); corrupt or missing files fall
//! back to defaults.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use mast_proto::{HarnessId, ReasoningLevel};

const FILE_NAME: &str = "composer-defaults.json";

/// Model option picks: option id → choice id (the `ChatConfig` shape).
pub type ModelOptions = serde_json::Map<String, serde_json::Value>;

/// Remembered model per harness — id plus display label, mirroring zeron's
/// `modelByHarness` storing the full `Model` object "so the pill never flashes
/// a raw id or 'Default'".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RememberedModel {
    pub id: String,
    pub label: String,
}

/// Device-local preferences for fresh sessions. Every unset field keeps the
/// last-picked behavior, field by field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NewSessionDefaults {
    pub harness: Option<HarnessId>,
    pub by_harness: HashMap<HarnessId, SessionModelDefaults>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionModelDefaults {
    pub model: Option<RememberedModel>,
    pub reasoning: Option<ReasoningLevel>,
    /// Option id → choice id for `model`; an absent option keeps its last pick.
    pub model_options: ModelOptions,
}

impl SessionModelDefaults {
    fn is_empty(&self) -> bool {
        self.model.is_none() && self.reasoning.is_none() && self.model_options.is_empty()
    }
}

impl NewSessionDefaults {
    pub fn for_harness(&self, harness: HarnessId) -> Option<&SessionModelDefaults> {
        self.by_harness.get(&harness)
    }

    /// Options were validated against one model, so a model change drops them.
    pub fn set_model(&mut self, harness: HarnessId, model: Option<RememberedModel>) {
        self.edit(harness, |defaults| {
            if defaults.model.as_ref().map(|m| &m.id) != model.as_ref().map(|m| &m.id) {
                defaults.model_options.clear();
            }
            defaults.model = model;
        });
    }

    pub fn set_reasoning(&mut self, harness: HarnessId, reasoning: Option<ReasoningLevel>) {
        self.edit(harness, |defaults| defaults.reasoning = reasoning);
    }

    /// Options only apply on top of a configured model.
    pub fn set_model_option(&mut self, harness: HarnessId, option: &str, choice: Option<String>) {
        self.edit(harness, |defaults| {
            if defaults.model.is_none() {
                return;
            }
            match choice {
                Some(choice) => {
                    defaults
                        .model_options
                        .insert(option.to_owned(), serde_json::Value::String(choice));
                }
                None => {
                    defaults.model_options.remove(option);
                }
            }
        });
    }

    fn edit(&mut self, harness: HarnessId, change: impl FnOnce(&mut SessionModelDefaults)) {
        let defaults = self.by_harness.entry(harness).or_default();
        change(defaults);
        if defaults.is_empty() {
            self.by_harness.remove(&harness);
        }
    }
}

/// One starred model in the picker (t3code client-settings `favorites`,
/// keyed `provider:model`) — harness + model id, insertion-ordered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FavoriteModel {
    pub harness: HarnessId,
    pub model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ComposerDefaults {
    /// Last harness picked on the new-chat canvas.
    pub harness: Option<HarnessId>,
    /// Last model picked, per harness (restored on harness switch).
    pub model_by_harness: HashMap<HarnessId, RememberedModel>,
    /// Last reasoning level picked (global, like zeron's `reasoning` key).
    pub reasoning: Option<ReasoningLevel>,
    /// Last non-default model option picks (option id → choice id), per
    /// harness and model id. Model-scoped because each pick was validated
    /// against that model's catalog row, so it stays safe to send before the
    /// catalog reloads (the Claude harness appends `[1m]` to any model id).
    pub model_options_by_model: HashMap<HarnessId, HashMap<String, ModelOptions>>,
    /// Every model label ever seen (id → label), fed from catalog loads.
    /// The chip's fallback while a harness's list is still loading — a
    /// session whose configured model differs from the remembered pick
    /// would otherwise flash the raw id on switch.
    pub model_labels: HashMap<String, String>,
    /// Last device picked for new sessions (the composer's device selector).
    pub device: Option<String>,
    /// Last project picked for new sessions; `None` + `no_project` = the
    /// remembered "Don't work in a project" state.
    pub project: Option<String>,
    /// Remembered "Don't work in a project" opt-out.
    pub no_project: bool,
    /// Starred models (the picker's favorites rail), in starring order.
    pub favorites: Vec<FavoriteModel>,
}

impl ComposerDefaults {
    /// Load from `{data_dir}/composer-defaults.json`; defaults on any failure.
    pub fn load(data_dir: &Path) -> Self {
        match std::fs::read_to_string(Self::path(data_dir)) {
            Ok(text) => match serde_json::from_str::<ComposerDefaults>(&text) {
                Ok(defaults) => defaults,
                Err(err) => {
                    tracing::warn!(error = %err, "composer-defaults corrupt; using defaults");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Write atomically (temp file + rename) so a crash mid-write never corrupts.
    pub fn save(&self, data_dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = Self::path(data_dir);
        // Each writer owns its temporary file; overlapping windows must not
        // truncate or rename one another's in-progress writes.
        let tmp = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4()));
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            file.write_all(&json)?;
            file.sync_all()?;
            std::fs::rename(&tmp, &path)?;
            #[cfg(unix)]
            std::fs::File::open(data_dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(FILE_NAME)
    }

    /// The remembered model for a harness, if any.
    pub fn model_for(&self, harness: HarnessId) -> Option<&RememberedModel> {
        self.model_by_harness.get(&harness)
    }

    /// Remember a pick (zeron `saveDefaults({ harness, modelByHarness })`).
    pub fn remember_model(&mut self, harness: HarnessId, id: String, label: String) {
        self.harness = Some(harness);
        self.model_by_harness
            .insert(harness, RememberedModel { id, label });
    }

    /// The remembered option picks for one model, if any.
    pub fn model_options_for(&self, harness: HarnessId, model: &str) -> Option<&ModelOptions> {
        self.model_options_by_model.get(&harness)?.get(model)
    }

    /// Mutable option picks for one model, created empty on first use.
    pub fn model_options_mut(&mut self, harness: HarnessId, model: &str) -> &mut ModelOptions {
        self.model_options_by_model
            .entry(harness)
            .or_default()
            .entry(model.to_string())
            .or_default()
    }

    /// The cached display label for a model id, if ever seen.
    pub fn label_for(&self, id: &str) -> Option<&str> {
        self.model_labels.get(id).map(String::as_str)
    }

    /// Whether a model is starred.
    pub fn is_favorite(&self, harness: HarnessId, model: &str) -> bool {
        self.favorites
            .iter()
            .any(|f| f.harness == harness && f.model == model)
    }

    /// Star/unstar a model; returns whether it is starred AFTER the toggle.
    pub fn toggle_favorite(&mut self, harness: HarnessId, model: &str) -> bool {
        if let Some(at) = self
            .favorites
            .iter()
            .position(|f| f.harness == harness && f.model == model)
        {
            self.favorites.remove(at);
            false
        } else {
            self.favorites.push(FavoriteModel {
                harness,
                model: model.to_string(),
            });
            true
        }
    }

    /// Merge a loaded catalog into the label cache. Returns whether anything
    /// changed (callers only save when it did).
    pub fn remember_labels<'a>(
        &mut self,
        models: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> bool {
        let mut changed = false;
        for (id, label) in models {
            if self.model_labels.get(id).map(String::as_str) != Some(label) {
                self.model_labels.insert(id.to_string(), label.to_string());
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut defaults = ComposerDefaults {
            harness: Some(HarnessId::ClaudeCode),
            reasoning: Some(ReasoningLevel::XHigh),
            ..Default::default()
        };
        defaults.remember_model(
            HarnessId::ClaudeCode,
            "claude-fable-5".into(),
            "Fable 5".into(),
        );
        defaults.remember_model(HarnessId::Codex, "gpt-5.2-codex".into(), "GPT-5.2".into());
        defaults
            .model_options_mut(HarnessId::ClaudeCode, "claude-fable-5")
            .insert("contextWindow".into(), "1m".into());
        defaults.save(dir.path()).unwrap();
        let loaded = ComposerDefaults::load(dir.path());
        assert_eq!(loaded, defaults);
        assert_eq!(
            loaded.model_for(HarnessId::ClaudeCode).map(|m| &*m.label),
            Some("Fable 5")
        );
    }

    #[test]
    fn missing_and_corrupt_files_yield_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            ComposerDefaults::load(dir.path()),
            ComposerDefaults::default()
        );
        std::fs::write(ComposerDefaults::path(dir.path()), "{nope").unwrap();
        assert_eq!(
            ComposerDefaults::load(dir.path()),
            ComposerDefaults::default()
        );
    }

    #[test]
    fn concurrent_projectless_saves_leave_a_complete_preference() {
        let dir = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for i in 0..8 {
                let path = dir.path();
                scope.spawn(move || {
                    let defaults = ComposerDefaults {
                        device: Some(format!("device-{i}")),
                        no_project: true,
                        ..Default::default()
                    };
                    for _ in 0..10 {
                        defaults.save(path).unwrap();
                        let saved = ComposerDefaults::load(path);
                        assert!(saved.no_project);
                        assert!(saved.project.is_none());
                        assert!(saved.device.is_some());
                    }
                });
            }
        });
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn favorites_toggle_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let mut defaults = ComposerDefaults::default();
        assert!(defaults.toggle_favorite(HarnessId::ClaudeCode, "claude-opus-5"));
        assert!(defaults.toggle_favorite(HarnessId::Codex, "gpt-5.2-codex"));
        assert!(defaults.is_favorite(HarnessId::ClaudeCode, "claude-opus-5"));
        // Same id under a different harness is a distinct star.
        assert!(!defaults.is_favorite(HarnessId::Codex, "claude-opus-5"));
        defaults.save(dir.path()).unwrap();
        assert_eq!(ComposerDefaults::load(dir.path()), defaults);
        // Untoggle removes, preserving the other's order.
        assert!(!defaults.toggle_favorite(HarnessId::ClaudeCode, "claude-opus-5"));
        assert!(!defaults.is_favorite(HarnessId::ClaudeCode, "claude-opus-5"));
        assert!(defaults.is_favorite(HarnessId::Codex, "gpt-5.2-codex"));
    }

    #[test]
    fn new_session_model_change_drops_options_and_empty_entries_vanish() {
        let fable = RememberedModel {
            id: "claude-fable-5".into(),
            label: "Fable 5".into(),
        };
        let mut defaults = NewSessionDefaults::default();
        defaults.set_model_option(HarnessId::ClaudeCode, "contextWindow", Some("1m".into()));
        assert!(defaults.by_harness.is_empty(), "options need a model");

        defaults.set_model(HarnessId::ClaudeCode, Some(fable.clone()));
        defaults.set_model_option(HarnessId::ClaudeCode, "contextWindow", Some("1m".into()));
        defaults.set_model(HarnessId::ClaudeCode, Some(fable));
        assert_eq!(
            defaults
                .for_harness(HarnessId::ClaudeCode)
                .unwrap()
                .model_options["contextWindow"],
            "1m",
            "re-picking the same model keeps its options"
        );

        defaults.set_model(
            HarnessId::ClaudeCode,
            Some(RememberedModel {
                id: "claude-opus-5-5".into(),
                label: "Opus 5.5".into(),
            }),
        );
        assert!(
            defaults
                .for_harness(HarnessId::ClaudeCode)
                .unwrap()
                .model_options
                .is_empty()
        );

        defaults.set_model(HarnessId::ClaudeCode, None);
        assert!(defaults.for_harness(HarnessId::ClaudeCode).is_none());
    }

    #[test]
    fn remember_model_updates_harness_and_row() {
        let mut defaults = ComposerDefaults::default();
        defaults.remember_model(HarnessId::Codex, "m1".into(), "One".into());
        defaults.remember_model(HarnessId::Codex, "m2".into(), "Two".into());
        assert_eq!(defaults.harness, Some(HarnessId::Codex));
        assert_eq!(
            defaults.model_for(HarnessId::Codex).map(|m| &*m.id),
            Some("m2")
        );
        assert!(defaults.model_for(HarnessId::ClaudeCode).is_none());
    }
}
