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
            .map(|date| date.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "n/a".to_owned());
        format!("## {date} {}\n- Friction: {}\n- Approval requested: {}\n- Why approval was needed: {}\n- Workaround used: {}\n- Suggested rule/tooling improvement: {}\n- Kind: {}\n- Severity: {}\n- Links: {}",
            report.summary, value(Some(&report.what_happened)),
            value(report.approval.as_ref().map(|approval| approval.command.as_str())),
            value(report.approval.as_ref().map(|approval| approval.why.as_str())),
            value(report.workaround.as_deref()), value(report.suggestion.as_deref()),
            serde_json::to_value(report.kind).unwrap().as_str().unwrap_or("workflow"),
            report.severity.map(|s| serde_json::to_value(s).unwrap().as_str().unwrap_or("n/a").to_owned()).unwrap_or_else(|| "n/a".into()), serde_json::to_string(&report.links).unwrap())
    }).collect::<Vec<_>>().join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friction_export_import_preserves_fields() {
        let report: FrictionReport = serde_json::from_value(serde_json::json!({
            "id": "one", "kind": "bug", "summary": "Build failed", "whatHappened": "Compiler failed",
            "workaround": "Retry", "suggestion": "Update docs", "severity": "high", "approval": null,
            "links": [{"label":"guide", "target":{"type":"file", "path":"/tmp/guide.md"}}],
            "provenance": {"chatId":"chat", "harness":"codex", "model":null, "spaceId":"space", "createdAt":0, "source":"agent"},
            "fingerprint":"one", "occurrences":1, "lastSeen":0, "status":"open"
        })).unwrap();
        let parsed = mast_engine::friction_import::parse_friction_markdown(&friction_markdown(&[report.clone()]));
        assert!(parsed.skipped.is_empty());
        let entry = &parsed.reports[0];
        assert_eq!(entry.kind, report.kind);
        assert_eq!(entry.severity, report.severity);
        assert_eq!(entry.summary, report.summary);
        assert_eq!(entry.what_happened, report.what_happened);
        assert_eq!(entry.workaround, report.workaround);
        assert_eq!(entry.suggestion, report.suggestion);
        assert_eq!(entry.links, report.links);
        assert_eq!(entry.created_at, report.provenance.created_at);
    }

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
            "## 1970-01-01 00:00 Build failed\n- Friction: Compiler failed\n- Approval requested: cargo build\n- Why approval was needed: Network access\n- Workaround used: Retry\n- Suggested rule/tooling improvement: n/a\n- Kind: bug\n- Severity: n/a\n- Links: []"
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
    picker: Option<String>,
    generation: u64,
    import_preview: Option<(String, String, mast_proto::ImportFrictionResult)>,
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

    fn preview_friction_file(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true, directories: false, multiple: false, prompt: Some("Import Markdown friction log".into()),
        });
        cx.spawn(async move |this, cx| {
            let path = match receiver.await { Ok(Ok(Some(mut paths))) => paths.pop(), _ => None };
            let Some(path) = path else { return; };
            let loaded = (|| -> Result<(String, String), String> {
                if path.extension().and_then(|s| s.to_str()).is_none_or(|s| !s.eq_ignore_ascii_case("md")) {
                    return Err("Choose a Markdown (.md) file".into());
                }
                let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
                if metadata.len() > 10 * 1024 * 1024 { return Err("File exceeds 10 MB".into()); }
                let file_name = path.file_name().and_then(|s| s.to_str()).ok_or("Invalid file name")?.to_owned();
                let markdown = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                Ok((file_name, markdown))
            })();
            this.update(cx, |this, cx| {
                match loaded {
                    Ok((file_name, markdown)) => {
                        let task = this.state.read(cx).work_outline_actions().import_friction(mast_proto::ImportFrictionParams {
                            space_id: this.space_id.clone(), file_name: file_name.clone(), markdown: markdown.clone(), preview: true,
                        });
                        cx.spawn(async move |this, cx| {
                            let result = task.await;
                            this.update(cx, |this, cx| {
                                match result {
                                    Ok(preview) => this.friction.import_preview = Some((file_name, markdown, preview)),
                                    Err(error) => cx.emit(DetailsEvent::Notice { text: error.message, error: true }),
                                }
                                cx.notify();
                            }).ok();
                        }).detach();
                    }
                    Err(text) => cx.emit(DetailsEvent::Notice { text, error: true }),
                }
            }).ok();
        }).detach();
    }

    fn import_friction_preview(&mut self, cx: &mut Context<Self>) {
        let Some((file_name, markdown, _)) = self.friction.import_preview.take() else { return; };
        let task = self.state.read(cx).work_outline_actions().import_friction(mast_proto::ImportFrictionParams {
            space_id: self.space_id.clone(), file_name, markdown, preview: false,
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(result) => {
                        cx.emit(DetailsEvent::Notice { text: format!("Friction import: {} added, {} merged, {} skipped", result.added, result.merged, result.skipped.len()), error: false });
                        this.fetch_friction(cx);
                    }
                    Err(error) => cx.emit(DetailsEvent::Notice { text: error.message, error: true }),
                }
                cx.notify();
            }).ok();
        }).detach();
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
                    .flex().items_center().justify_between()
                    .child("Friction")
                    .child(Self::small_button("friction-import", "Import…", theme)
                        .on_click(cx.listener(|this, _, _, cx| this.preview_friction_file(cx)))),
            );
        if let Some((file_name, _, preview)) = &self.friction.import_preview {
            let mut counts = Vec::new();
            for (label, kind) in [("Bug", FrictionKind::Bug), ("Tool failure", FrictionKind::ToolFailure), ("Permission", FrictionKind::Permission), ("Environment", FrictionKind::Environment), ("Workflow", FrictionKind::Workflow), ("Docs", FrictionKind::Docs)] {
                let count = preview.entries.iter().filter(|entry| entry.kind == kind).count();
                if count > 0 { counts.push(format!("{label}: {count}")); }
            }
            let mut box_view = div().debug_selector(|| "friction-import-preview".into()).flex().flex_col().gap(px(3.0))
                .child(SharedString::from(format!("Preview: {file_name}")))
                .child(SharedString::from(counts.join(" · ")))
                .child(SharedString::from(format!("{} entries, {} skipped", preview.entries.len(), preview.skipped.len())));
            for entry in preview.entries.iter().take(10) { box_view = box_view.child(SharedString::from(entry.summary.clone())); }
            box_view = box_view.child(div().flex().gap(px(6.0))
                .child(Self::small_button("friction-import-confirm", "Import", theme).on_click(cx.listener(|this, _, _, cx| this.import_friction_preview(cx))))
                .child(Self::small_button("friction-import-cancel", "Cancel", theme).on_click(cx.listener(|this, _, _, cx| { this.friction.import_preview = None; cx.notify(); }))));
            section = section.child(box_view);
        }
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
                FrictionSource::Import => "import",
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
                let fingerprint = report.fingerprint.clone();
                let report_id = report.id.clone();
                let picker_open = self.friction.picker.as_deref() == Some(report.id.as_str());
                let create = Self::small_button(("friction-create", index), "Create bug", theme)
                    .debug_selector({
                        let selector = format!("friction-create-{}", report.id);
                        move || selector.clone()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.friction.picker =
                            if this.friction.picker.as_deref() == Some(&report_id) {
                                None
                            } else {
                                Some(report_id.clone())
                            };
                        cx.notify();
                    }));
                if let Some(item_id) = &report.promoted_item_id {
                    let title = self
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.items.iter().find(|item| &item.id == item_id))
                        .map(|item| item.title.clone())
                        .unwrap_or_else(|| report.summary.clone());
                    let item_id = item_id.clone();
                    let space_id = self.space_id.clone();
                    details = details.child(
                        div()
                            .id(("friction-item", index))
                            .cursor_pointer()
                            .text_color(theme.text)
                            .child(SharedString::from(format!("→ {title}")))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(DetailsEvent::OpenItem {
                                    space_id: space_id.clone(),
                                    item_id: item_id.clone(),
                                })
                            })),
                    );
                } else if picker_open {
                    if let Some(snapshot) = &self.snapshot {
                        let last = crate::settings::current(cx)
                            .friction_destination_by_space_id
                            .get(&self.space_id)
                            .cloned();
                        let mut choices: Vec<_> = snapshot
                            .items
                            .iter()
                            .filter(|item| item.archived_at.is_none())
                            .collect();
                        choices.sort_by_key(|item| last.as_deref() != Some(item.id.as_str()));
                        for (choice_index, item) in choices.into_iter().enumerate() {
                            let parent_item_id = item.id.clone();
                            let fingerprint = report.fingerprint.clone();
                            let report_id = report.id.clone();
                            details = details.child(
                                div()
                                    .id(("friction-parent", index * 10000 + choice_index))
                                    .debug_selector({
                                        let selector = format!("friction-parent-{}", item.id);
                                        move || selector.clone()
                                    })
                                    .cursor_pointer()
                                    .text_color(theme.text)
                                    .child(SharedString::from(item.title.clone()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let task = this
                                            .state
                                            .read(cx)
                                            .work_outline_actions()
                                            .promote_friction_to_item(
                                                mast_proto::PromoteFrictionToItemParams {
                                                    space_id: this.space_id.clone(),
                                                    fingerprint: fingerprint.clone(),
                                                    parent_item_id: parent_item_id.clone(),
                                                },
                                            );
                                        let report_id = report_id.clone();
                                        let parent_item_id = parent_item_id.clone();
                                        let space_id = this.space_id.clone();
                                        cx.spawn(async move |this, cx| {
                                            let result = task.await;
                                            this.update(cx, |this, cx| {
                                                match result {
                                                    Ok(result) => {
                                                        crate::settings::update(crate::settings::SavePolicy::Debounced, cx, |settings| {
                                                            settings.friction_destination_by_space_id.insert(space_id, parent_item_id);
                                                        });
                                                        if let Some(report) = this
                                                            .friction
                                                            .reports
                                                            .as_mut()
                                                            .and_then(|reports| {
                                                                reports.iter_mut().find(|report| {
                                                                    report.id == report_id
                                                                })
                                                            })
                                                        {
                                                            report.promoted_item_id =
                                                                Some(result.work_item_id);
                                                        }
                                                        this.friction.picker = None;
                                                    }
                                                    Err(error) => cx.emit(DetailsEvent::Notice {
                                                        text: error.message,
                                                        error: true,
                                                    }),
                                                }
                                                cx.notify();
                                            })
                                            .ok();
                                        })
                                        .detach();
                                    })),
                            );
                        }
                    }
                }
                let dismiss = Self::small_button(("friction-dismiss", index), "Dismiss", theme)
                    .debug_selector({
                        let selector = format!("friction-dismiss-{}", report.id);
                        move || selector.clone()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let task = this
                            .state
                            .read(cx)
                            .work_outline_actions()
                            .set_friction_status(mast_proto::SetFrictionStatusParams {
                                space_id: this.space_id.clone(),
                                fingerprint: fingerprint.clone(),
                                status: FrictionStatus::Dismissed,
                            });
                        cx.spawn(async move |this, cx| {
                            let result = task.await;
                            this.update(cx, |this, cx| match result {
                                Ok(_) => this.fetch_friction(cx),
                                Err(error) => cx.emit(DetailsEvent::Notice {
                                    text: error.message,
                                    error: true,
                                }),
                            })
                            .ok();
                        })
                        .detach();
                    }));
                let fingerprint = report.fingerprint.clone();
                let suggestion = report
                    .suggestion
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
                let propose =
                    Self::small_button(("friction-propose", index), "Propose instruction", theme)
                        .debug_selector({
                            let selector = format!("friction-propose-{}", report.id);
                            move || selector.clone()
                        })
                        .when(suggestion, |button| {
                            button.on_click(cx.listener(move |this, _, _, cx| {
                                let task = this
                                    .state
                                    .read(cx)
                                    .work_outline_actions()
                                    .propose_friction_instruction(
                                        mast_proto::ProposeFrictionInstructionParams {
                                            space_id: this.space_id.clone(),
                                            fingerprint: fingerprint.clone(),
                                        },
                                    );
                                cx.spawn(async move |this, cx| {
                                    let result = task.await;
                                    this.update(cx, |_, cx| {
                                        cx.emit(match result {
                                            Ok(_) => DetailsEvent::Notice {
                                                text: "Proposal created, review it in Proposals"
                                                    .into(),
                                                error: false,
                                            },
                                            Err(error) => DetailsEvent::Notice {
                                                text: error.message,
                                                error: true,
                                            },
                                        })
                                    })
                                    .ok();
                                })
                                .detach();
                            }))
                        })
                        .when(!suggestion, |button| {
                            button.opacity(0.5).tooltip(|_, cx| {
                                cx.new(|_| super::TextTooltip("No suggestion to propose".into()))
                                    .into()
                            })
                        });
                details = details.child(
                    div()
                        .flex()
                        .gap(px(6.0))
                        .child(create)
                        .child(propose)
                        .child(dismiss),
                );
                if let Some(file_name) = &report.provenance.file_name {
                    details = details.child(SharedString::from(format!("Imported from {file_name}")));
                } else {
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
                }
                row = row.child(details);
            }
            section = section.child(row);
        }
        section
    }
}
