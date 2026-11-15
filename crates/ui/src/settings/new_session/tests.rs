use gpui::{AppContext as _, TestAppContext, VisualTestContext};
use mast_engine::registry::HarnessDescriptor;
use mast_proto::{HarnessId, Model, ModelOption, ModelOptionChoice, ReasoningLevel};

use super::NewSessionDefaultsCard;
use crate::settings::composer::NewSessionDefaults;
use crate::state::AppState;
use crate::theme::Theme;

fn descriptor(id: HarnessId, name: &str, installed: bool) -> HarnessDescriptor {
    HarnessDescriptor {
        id,
        name: name.into(),
        installed,
        can_install: false,
        enabled: Some(true),
        reasoning_levels: Vec::new(),
        steering_mode: mast_proto::SteeringMode::StepBoundary,
        supports_steering: false,
    }
}

fn model(id: &str, label: &str, levels: &[ReasoningLevel], options: Vec<ModelOption>) -> Model {
    Model {
        id: id.into(),
        label: label.into(),
        description: None,
        reasoning_levels: levels.to_vec(),
        options,
    }
}

fn service_tier() -> ModelOption {
    ModelOption {
        id: "serviceTier".into(),
        label: "Service tier".into(),
        choices: [("auto", "Standard"), ("fast", "Fast")]
            .map(|(id, label)| ModelOptionChoice {
                id: id.into(),
                label: label.into(),
            })
            .into(),
        default_choice: "auto".into(),
    }
}

/// Claude Code and Codex offered, Cursor enabled but not installed.
fn open_card(cx: &mut TestAppContext) -> (tempfile::TempDir, &mut VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_owned();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::settings::init(crate::settings::UiSettings::default(), data_dir, cx);
    });
    let (_, vcx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::new());
        let levels = [
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
        ];
        NewSessionDefaultsCard::with_catalog(
            state,
            vec![
                descriptor(HarnessId::ClaudeCode, "Claude Code", true),
                descriptor(HarnessId::Codex, "Codex", true),
                descriptor(HarnessId::Cursor, "Cursor", false),
            ],
            vec![
                (
                    HarnessId::ClaudeCode,
                    vec![
                        model("claude-opus-5-5", "Opus 5.5", &levels, Vec::new()),
                        model("claude-fable-5", "Fable 5", &levels, Vec::new()),
                    ],
                ),
                (
                    HarnessId::Codex,
                    vec![
                        model("gpt-plain", "GPT plain", &levels, Vec::new()),
                        model("gpt-tiered", "GPT tiered", &levels, vec![service_tier()]),
                    ],
                ),
            ],
        )
    });
    draw(vcx);
    (dir, vcx)
}

fn draw(vcx: &mut VisualTestContext) {
    vcx.update(|window, cx| window.draw(cx).clear());
}

fn click(vcx: &mut VisualTestContext, selector: &'static str) {
    let at = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} renders"))
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
}

fn saved(vcx: &mut VisualTestContext) -> NewSessionDefaults {
    vcx.update(|_, cx| crate::settings::current(cx).new_session_defaults)
}

#[gpui::test]
fn picking_each_control_writes_the_setting(cx: &mut TestAppContext) {
    let (dir, vcx) = open_card(cx);
    assert_eq!(saved(vcx), NewSessionDefaults::default());

    // Last picked, Claude Code, Codex; the uninstalled Cursor isn't offered.
    click(vcx, "new-session-harness");
    assert!(vcx.debug_bounds("new-session-harness-choice-3").is_none());
    click(vcx, "new-session-harness-choice-2");
    assert_eq!(saved(vcx).harness, Some(HarnessId::Codex));
    assert!(
        vcx.debug_bounds("new-session-harness-choice-0").is_none(),
        "a pick closes the menu"
    );

    // The model rows follow the default agent: Last picked, GPT plain, GPT tiered.
    click(vcx, "new-session-model");
    click(vcx, "new-session-model-choice-2");
    click(vcx, "new-session-reasoning");
    click(vcx, "new-session-reasoning-choice-1");
    // GPT tiered's option row appears once it is the default model.
    click(vcx, "new-session-option-serviceTier");
    click(vcx, "new-session-option-serviceTier-choice-2");
    let codex = saved(vcx).for_harness(HarnessId::Codex).cloned().unwrap();
    assert_eq!(
        codex.model.as_ref().map(|m| m.id.as_str()),
        Some("gpt-tiered")
    );
    assert_eq!(
        codex.model.as_ref().map(|m| m.label.as_str()),
        Some("GPT tiered")
    );
    assert_eq!(codex.reasoning, Some(ReasoningLevel::Low));
    assert_eq!(codex.model_options["serviceTier"], "fast");

    // The write reaches ui-settings.json.
    vcx.update(|_, cx| crate::settings::flush(cx));
    assert_eq!(
        crate::settings::UiSettings::load(dir.path()).new_session_defaults,
        saved(vcx)
    );

    // Back to Last picked: the options go with the model, reasoning stays.
    click(vcx, "new-session-model");
    click(vcx, "new-session-model-choice-0");
    let codex = saved(vcx).for_harness(HarnessId::Codex).cloned().unwrap();
    assert!(codex.model.is_none());
    assert!(codex.model_options.is_empty());
    assert_eq!(codex.reasoning, Some(ReasoningLevel::Low));
    assert!(vcx.debug_bounds("new-session-option-serviceTier").is_none());

    click(vcx, "new-session-reasoning");
    click(vcx, "new-session-reasoning-choice-0");
    click(vcx, "new-session-harness");
    click(vcx, "new-session-harness-choice-0");
    assert_eq!(
        saved(vcx),
        NewSessionDefaults::default(),
        "nothing left behind"
    );
}

#[gpui::test]
fn defaults_for_switches_harness_without_writing(cx: &mut TestAppContext) {
    let (_dir, vcx) = open_card(cx);
    // With no default agent the rows edit the first offered harness.
    click(vcx, "new-session-model");
    click(vcx, "new-session-model-choice-2");
    assert_eq!(
        saved(vcx)
            .for_harness(HarnessId::ClaudeCode)
            .and_then(|d| d.model.as_ref())
            .map(|m| m.id.as_str()),
        Some("claude-fable-5")
    );

    click(vcx, "new-session-editing");
    click(vcx, "new-session-editing-choice-1");
    assert_eq!(
        saved(vcx).harness,
        None,
        "switching is not a default agent pick"
    );
    click(vcx, "new-session-model");
    click(vcx, "new-session-model-choice-1");
    let defaults = saved(vcx);
    assert_eq!(
        defaults
            .for_harness(HarnessId::Codex)
            .and_then(|d| d.model.as_ref())
            .map(|m| m.id.as_str()),
        Some("gpt-plain")
    );
    assert!(defaults.for_harness(HarnessId::ClaudeCode).is_some());
}

#[gpui::test]
fn an_unavailable_default_agent_stays_clearable(cx: &mut TestAppContext) {
    let (_dir, vcx) = open_card(cx);
    vcx.update(|_, cx| {
        crate::settings::update(crate::settings::SavePolicy::Debounced, cx, |settings| {
            settings.new_session_defaults.harness = Some(HarnessId::Cursor);
        });
    });
    draw(vcx);
    // Cursor isn't installed here, so the model rows edit the first offered
    // harness instead of waiting on a catalog that never loads.
    click(vcx, "new-session-model");
    click(vcx, "new-session-model-choice-1");
    let defaults = saved(vcx);
    assert!(defaults.for_harness(HarnessId::ClaudeCode).is_some());
    assert!(defaults.for_harness(HarnessId::Cursor).is_none());

    click(vcx, "new-session-harness");
    click(vcx, "new-session-harness-choice-0");
    assert_eq!(saved(vcx).harness, None);
}
