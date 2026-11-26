//! Collapse state for the Tracker tab's sections. Each section has its own
//! key and its own heading toggle, so collapsing one never moves another.
//! The user's choice, either way, is saved in `UiSettings::tracker_sections`
//! and survives a tab switch and a restart. A section the user never touched
//! shows its default, which is what the Tracker showed before the setting
//! existed.
use super::*;

/// The settings key of a lifecycle group's section.
pub(super) fn stage_section_key(stage: LifecycleStage) -> &'static str {
    match stage {
        LifecycleStage::NotStarted => "stage-not-started",
        LifecycleStage::Building => "stage-building",
        LifecycleStage::CodeReview => "stage-code-review",
        LifecycleStage::PrReview => "stage-pr-review",
        LifecycleStage::ReadyToMerge => "stage-ready-to-merge",
        LifecycleStage::Done => "stage-done",
    }
}

impl Shell {
    /// Whether the section is collapsed: the user's saved choice, else `default_collapsed`.
    pub(in crate::shell) fn tracker_section_collapsed(&self, key: &str, default_collapsed: bool) -> bool {
        self.settings.tracker_sections.get(key).copied().unwrap_or(default_collapsed)
    }

    pub(in crate::shell) fn toggle_tracker_section(&mut self, key: &str, default_collapsed: bool, cx: &mut Context<Self>) {
        let collapsed = self.tracker_section_collapsed(key, default_collapsed);
        self.settings.tracker_sections.insert(key.to_owned(), !collapsed);
        self.schedule_save(cx);
        cx.notify();
    }

    /// A section heading that collapses and expands that section alone.
    pub(super) fn tracker_section_heading(
        &self,
        key: &'static str,
        label: &str,
        count: usize,
        default_collapsed: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let glyph = if self.tracker_section_collapsed(key, default_collapsed) { "▸" } else { "▾" };
        let heading = crate::components::shell_parts::tracker_group_heading(theme, &format!("{glyph} {label}"), count);
        div()
            .id(SharedString::from(format!("tracker-section-{key}")))
            .debug_selector(move || format!("tracker-section-{key}"))
            .cursor_pointer()
            .child(heading)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_tracker_section(key, default_collapsed, cx)))
    }
}
