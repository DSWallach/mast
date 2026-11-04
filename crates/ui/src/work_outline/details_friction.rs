//! Space friction reports.
use super::{DetailsEvent, WorkItemDetails};
use crate::theme::Theme;
use gpui::{Context, SharedString, div, prelude::*, px};
use mast_proto::{
    FrictionKind, FrictionReport, FrictionSource, FrictionStatus, ListFrictionReportsParams,
};
use std::collections::HashSet;

fn friction_markdown(reports: &[FrictionReport]) -> String {
    fn value(value: Option<&str>) -> &str {
        value
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("n/a")
    }
    reports.iter().map(|report| {
        let date = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(report.provenance.created_at)
            .map(|date| date.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "n/a".to_owned());
        format!("## {date} {}\n- Friction: {}\n- Approval requested: {}\n- Why approval was needed: {}\n- Workaround used: {}\n- Suggested rule/tooling improvement: {}",
            report.summary, value(Some(&report.what_happened)),
            value(report.approval.as_ref().map(|approval| approval.command.as_str())),
            value(report.approval.as_ref().map(|approval| approval.why.as_str())),
            value(report.workaround.as_deref()), value(report.suggestion.as_deref()))
    }).collect::<Vec<_>>().join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friction_export_markdown_snapshot() {
        let report: FrictionReport = serde_json::from_value(serde_json::json!({
            "id": "one", "kind": "bug", "summary": "Build failed", "whatHappened": "Compiler failed",
            "workaround": "Retry", "suggestion": null, "approval": {"command": "cargo build", "why": "Network access"},
            "provenance": {"chatId": "chat", "harness": "codex", "model": null, "spaceId": "space", "createdAt": 0, "source": "agent"},
            "fingerprint": "one", "occurrences": 1, "lastSeen": 0, "status": "open"
        })).unwrap();
        assert_eq!(
            friction_markdown(&[report]),
            "## 1970-01-01 Build failed\n- Friction: Compiler failed\n- Approval requested: cargo build\n- Why approval was needed: Network access\n- Workaround used: Retry\n- Suggested rule/tooling improvement: n/a"
        );
    }
}

#[derive(Default)]
pub(super) struct FrictionDetailsState {
    status: Option<FrictionStatus>,
    requested: bool,
    reports: Option<Vec<FrictionReport>>,
    error: Option<String>,
    expanded: HashSet<String>,
    generation: u64,
}

impl WorkItemDetails {
    fn fetch_friction(&mut self, cx: &mut Context<Self>) {
        self.friction.requested = true;
        self.friction.reports = None;
        self.friction.error = None;
        self.friction.generation += 1;
        let generation = self.friction.generation;
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .list_friction_reports(ListFrictionReportsParams {
                space_id: self.space_id.clone(),
                kind: None,
                status: self.friction.status,
                harness: None,
                model: None,
                since: None,
                limit: None,
            });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.friction.generation != generation {
                    return;
                }
                match result {
                    Ok(reports) => this.friction.reports = Some(reports),
                    Err(error) => this.friction.error = Some(error.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn render_friction_section(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        if !self.friction.requested {
            self.friction.status = Some(FrictionStatus::Open);
            self.fetch_friction(cx);
        }
        let mut section = div()
            .debug_selector(|| "space-friction".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted)
                    .child("Friction"),
            );
        let mut filters = div().flex().gap(px(6.0));
        for (index, (label, status)) in [
            ("Open", Some(FrictionStatus::Open)),
            ("Dismissed", Some(FrictionStatus::Dismissed)),
            ("All", None),
        ]
        .into_iter()
        .enumerate()
        {
            let selected = self.friction.status == status;
            filters = filters.child(
                Self::small_button(("friction-filter", index), label, theme)
                    .debug_selector({
                        let selector = format!("friction-filter-{label}");
                        move || selector.clone()
                    })
                    .when(selected, |el| el.text_color(theme.text))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.friction.status != status {
                            this.friction.status = status;
                            this.fetch_friction(cx);
                            cx.notify();
                        }
                    })),
            );
        }
        if let Some(reports) = &self.friction.reports {
            let markdown = friction_markdown(reports);
            filters = filters.child(
                Self::small_button("friction-export", "Export Markdown", theme)
                    .debug_selector(|| "friction-export".into())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(markdown.clone()));
                    })),
            );
        }
        section = section.child(filters);
        if let Some(error) = &self.friction.error {
            return section.child(
                div()
                    .text_color(theme.text_muted)
                    .text_size(crate::typography::ui_rems(10.5))
                    .child(SharedString::from(format!(
                        "Could not load friction: {error}"
                    ))),
            );
        }
        let Some(reports) = &self.friction.reports else {
            return section.child(
                div()
                    .text_color(theme.text_muted)
                    .text_size(crate::typography::ui_rems(10.5))
                    .child("Loading…"),
            );
        };
        if reports.is_empty() {
            return section.child(
                div()
                    .debug_selector(|| "friction-empty".into())
                    .text_color(theme.text_muted)
                    .text_size(crate::typography::ui_rems(10.5))
                    .child("No friction reported."),
            );
        }
        for (index, report) in reports.iter().enumerate() {
            let id = report.id.clone();
            let selector = format!("friction-row-{id}");
            let expanded = self.friction.expanded.contains(&id);
            let icon = match report.kind {
                FrictionKind::Bug | FrictionKind::ToolFailure => crate::icons::DANGER_TRIANGLE,
                FrictionKind::Permission => crate::icons::CHECK,
                FrictionKind::Environment => crate::icons::FOLDER_WITH_FILES,
                FrictionKind::Workflow => crate::icons::CHECKLIST,
                FrictionKind::Docs => crate::icons::DOCUMENT,
            };
            let age =
                super::super::required_actions::age_label(report.last_seen, chrono::Utc::now());
            let source = match report.provenance.source {
                FrictionSource::Auto => "auto",
                FrictionSource::Agent => "agent",
            };
            let mut row = div()
                .debug_selector(move || selector.clone())
                .flex()
                .flex_col()
                .gap(px(3.0));
            let mut heading = div()
                .id(("friction-row", index))
                .cursor_pointer()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text)
                .child(crate::icons::icon(icon).size(px(12.0)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(SharedString::from(report.summary.clone())),
                )
                .child(
                    div()
                        .debug_selector({
                            let selector =
                                format!("friction-count-{}-{}", report.id, report.occurrences);
                            move || selector.clone()
                        })
                        .child(SharedString::from(format!("×{}", report.occurrences))),
                )
                .child(
                    div()
                        .text_color(theme.text_muted)
                        .child(SharedString::from(age)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.friction.expanded.remove(&id) {
                        this.friction.expanded.insert(id.clone());
                    }
                    cx.notify();
                }));
            for chip in [
                Some(report.provenance.harness.as_str()),
                report.provenance.model.as_deref(),
                Some(source),
            ]
            .into_iter()
            .flatten()
            {
                heading = heading.child(
                    div()
                        .px(px(4.0))
                        .rounded(px(3.0))
                        .bg(theme.surface_overlay)
                        .text_color(theme.text_muted)
                        .text_size(crate::typography::ui_rems(10.0))
                        .child(SharedString::from(chip.to_owned())),
                );
            }
            row = row.child(heading);
            if expanded {
                let selector = format!("friction-details-{}", report.id);
                let mut details = div()
                    .debug_selector(move || selector.clone())
                    .pl(px(18.0))
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted);
                for (label, value) in [
                    ("What happened", Some(report.what_happened.as_str())),
                    ("Workaround", report.workaround.as_deref()),
                    ("Suggestion", report.suggestion.as_deref()),
                ] {
                    if let Some(value) = value {
                        details = details.child(
                            div()
                                .debug_selector({
                                    let selector = format!("friction-detail-{}-{label}", report.id);
                                    move || selector.clone()
                                })
                                .child(SharedString::from(format!("{label}: {value}"))),
                        );
                    }
                }
                if let Some(approval) = &report.approval {
                    let answer = match approval.answer {
                        Some(true) => "approved",
                        Some(false) => "denied",
                        None => "unanswered",
                    };
                    details = details.child(
                        div()
                            .debug_selector({
                                let selector = format!("friction-approval-{}", report.id);
                                move || selector.clone()
                            })
                            .child(SharedString::from(format!(
                                "Approval: {} · {} · {answer}",
                                approval.command, approval.why
                            ))),
                    );
                }
                let chat_id = report.provenance.chat_id.clone();
                details = details.child(
                    div()
                        .id(("friction-chat", index))
                        .cursor_pointer()
                        .text_color(theme.text)
                        .child("Open provenance chat")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(DetailsEvent::OpenChat(chat_id.clone()))
                        })),
                );
                row = row.child(details);
            }
            section = section.child(row);
        }
        section
    }
}
