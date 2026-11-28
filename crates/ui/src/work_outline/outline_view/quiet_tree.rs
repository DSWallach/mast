use super::*;

const ICON_SIZE: f32 = 12.0;
const ICON_GAP: f32 = 6.0;
const MODEL_MAX_WIDTH: f32 = 90.0;
const STATUS_MAX_WIDTH: f32 = 140.0;
pub(super) const METADATA_LINE_HEIGHT: f32 = 14.0;
pub(super) const LINE_GAP: f32 = 3.0;

#[derive(Default)]
pub(super) struct SidebarMetadata {
    references: std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>>,
    drift: HashSet<String>,
    space_summaries: std::collections::HashMap<String, String>,
}

impl SidebarMetadata {
    pub(super) fn new(inputs: &OutlineInputs) -> Self {
        let mut result = Self::default();
        for space in &inputs.spaces {
            let Some(snapshot) = space.load.snapshot() else {
                continue;
            };
            let mut summary = Vec::new();
            if let Some(totals) = super::super::required_actions::space_totals(snapshot) {
                summary.push(totals.label());
            }
            let proposals = super::super::required_actions::pending_proposal_count(snapshot, None);
            if proposals > 0 {
                summary.push(super::super::required_actions::proposals_label(proposals));
            }
            result
                .space_summaries
                .insert(space.id.clone(), summary.join(" · "));
            let references = result.references.entry(space.id.clone()).or_default();
            for reference in &snapshot.external_references {
                let label = match &reference.cached_title {
                    Some(title) => {
                        format!("{} {}: {title}", reference.provider, reference.external_id)
                    }
                    None => format!("{} {}", reference.provider, reference.external_id),
                };
                references
                    .entry(reference.work_item_id.clone())
                    .or_default()
                    .push(label);
            }
            result.drift.extend(
                snapshot
                    .chat_instruction_states
                    .iter()
                    .filter(|state| state.drift)
                    .map(|state| state.chat_id.clone()),
            );
        }
        result
    }
}

fn count_label(count: impl Into<u64>, name: &str) -> String {
    let count = count.into();
    format!("{count} {name}{}", if count == 1 { "" } else { "s" })
}

fn failed_tool_label(count: u64) -> String {
    format!("{} failed", count_label(count, "tool call"))
}

pub(super) fn entries_label(count: usize) -> String {
    format!("{count} {}", if count == 1 { "entry" } else { "entries" })
}

pub(super) fn notice_color(tone: NoticeTone, theme: &Theme) -> gpui::Hsla {
    match tone {
        NoticeTone::Warning => theme.warning,
        NoticeTone::Muted => theme.text_muted,
    }
}

fn title_line(key: &str, title: &str, item: bool, theme: &Theme) -> AnyElement {
    div()
        .id(SharedString::from(format!("outline-title-{key}")))
        .debug_selector({
            let selector = format!("outline-title-{key}");
            move || selector.clone()
        })
        .w_full()
        .flex()
        .items_center()
        .gap(px(ICON_GAP))
        .line_height(crate::typography::ui_rems(16.0))
        .child(
            crate::icons::icon(if item {
                crate::icons::FOLDER_WITH_FILES
            } else {
                crate::icons::CHAT_ROUND_LINE
            })
            .size(px(ICON_SIZE))
            .text_color(if item { theme.accent } else { theme.text_muted }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(item, |el| el.font_weight(gpui::FontWeight::SEMIBOLD))
                .text_color(theme.text)
                .child(SharedString::from(title.to_owned())),
        )
        .into_any_element()
}

fn metadata_line(key: &str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(format!("outline-metadata-{key}")))
        .debug_selector({
            let selector = format!("outline-metadata-{key}");
            move || selector.clone()
        })
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(ICON_GAP))
        .pl(px(ICON_SIZE + ICON_GAP))
        .line_height(crate::typography::ui_rems(METADATA_LINE_HEIGHT))
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_muted)
}

fn two_lines(key: &str, title: AnyElement, metadata: AnyElement, tip: SharedString) -> AnyElement {
    div()
        .id(SharedString::from(format!("outline-body-{key}")))
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(LINE_GAP))
        .child(title)
        .child(metadata)
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        .into_any_element()
}

impl OutlineView {
    pub(super) fn quiet_space_summary(&self, space_id: &str, count: usize) -> String {
        let mut text = entries_label(count);
        if let Some(summary) = self
            .sidebar_metadata
            .space_summaries
            .get(space_id)
            .filter(|summary| !summary.is_empty())
        {
            text.push_str(&format!(" · {summary}"));
        }
        text
    }

    /// A project's Manager row: the label with the holder's model badge, or
    /// the placeholder hint before the first message creates its Chat. The
    /// same row stands for every Chat that holds the role.
    pub(super) fn manager_body(
        &self,
        key: &str,
        manager: &super::ManagerRow,
        theme: &Theme,
    ) -> AnyElement {
        let summary = match (&manager.model, manager.chat_id.is_some()) {
            (Some(model), _) => model.clone(),
            (None, true) => String::new(),
            (None, false) => "Starts on your first message".to_owned(),
        };
        let label = theme.identity_foreground(theme.manager_accent());
        if self.sidebar {
            return self.quiet_group_body(key, manager.label(), summary, label, theme);
        }
        div()
            .debug_selector({
                let selector = format!("outline-manager-{key}");
                move || selector.clone()
            })
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(label)
                    .child(SharedString::from(manager.label().to_owned())),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text_faint)
                    .child(SharedString::from(summary)),
            )
            .into_any_element()
    }

    pub(super) fn quiet_group_body(
        &self,
        key: &str,
        title: &str,
        summary: String,
        color: gpui::Hsla,
        theme: &Theme,
    ) -> AnyElement {
        let tip: SharedString = if summary.is_empty() {
            title.to_owned()
        } else {
            format!("{title} · {summary}")
        }
        .into();
        let title = div()
            .w_full()
            .min_w_0()
            .truncate()
            .line_height(crate::typography::ui_rems(16.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(color)
            .child(SharedString::from(title.to_owned()))
            .into_any_element();
        let metadata = metadata_line(key, theme)
            .pl(px(0.0))
            .child(SharedString::from(summary))
            .into_any_element();
        two_lines(key, title, metadata, tip)
    }

    pub(super) fn quiet_item_body(
        &self,
        key: &str,
        item: &ItemRow,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (_, status, color) = chips::status_style(item.effective_status, theme);
        let mut details = Vec::new();
        if item.unresolved_actions > 0 {
            details.push(count_label(item.unresolved_actions, "action"));
        }
        let rollup = &item.rollup;
        if rollup.subtree_unresolved_user_action_count > item.unresolved_actions {
            details.push(format!(
                "{} in subtree",
                count_label(rollup.subtree_unresolved_user_action_count, "action")
            ));
        }
        if let Some(until) = item.waiting_until {
            let date = chrono::DateTime::from_timestamp_millis(until)
                .map(|date| {
                    date.with_timezone(&chrono::Local)
                        .format("%b %-d")
                        .to_string()
                })
                .unwrap_or_default();
            details.push(format!("Waiting until {date}"));
        }
        if item.execution_exceptions > 0 {
            details.push(count_label(
                item.execution_exceptions,
                "execution exception",
            ));
        }
        if item.pending_proposals > 0 {
            details.push(super::super::required_actions::proposals_label(
                item.pending_proposals,
            ));
        }
        if item.template_changed.is_some() {
            details.push("Template changed".into());
        }
        if let Some(references) = self
            .sidebar_metadata
            .references
            .get(&item.space_id)
            .and_then(|items| items.get(&item.item_id))
        {
            details.extend(references.iter().cloned());
        }
        details.push(count_label(item.chat_count, "chat"));
        if rollup.pending_count + rollup.active_count + rollup.completed_count > 1 {
            details.push(format!(
                "{} pending · {} active · {} done",
                rollup.pending_count, rollup.active_count, rollup.completed_count
            ));
        }
        if item.follows_template {
            details.push("Follows template".into());
        }
        let artifacts = self
            .state
            .read(cx)
            .artifacts()
            .for_work_item(&item.space_id, &item.item_id);
        if !artifacts.is_empty() {
            details.push(count_label(artifacts.len() as u32, "artifact"));
            let viewed = crate::settings::current(cx).artifact_last_viewed;
            if artifacts.iter().any(|artifact| {
                crate::artifacts::questions_ui::list_marker(
                    &artifact.id,
                    artifact.current_revision,
                    &viewed,
                )
                .is_some()
            }) {
                details.push("Artifacts updated".into());
            }
        }
        let summary: SharedString = details.join(" · ").into();
        let tip: SharedString = format!("{} · {status} · {summary}", item.title).into();
        let metadata = metadata_line(key, theme)
            .child(
                div()
                    .min_w_0()
                    .max_w(px(STATUS_MAX_WIDTH))
                    .truncate()
                    .text_color(color)
                    .child(status),
            )
            .child(div().flex_1().min_w_0().truncate().child(summary));
        two_lines(
            key,
            title_line(key, &item.title, true, theme),
            metadata.into_any_element(),
            tip,
        )
    }

    pub(super) fn quiet_chat_body(
        &self,
        key: &str,
        row: &ChatRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chat = &row.chat;
        let title = if row.dangling {
            format!("Missing chat · {}", chat.id)
        } else {
            chat.title.clone()
        };
        let status = if row.dangling {
            crate::status_style::Status::Errored
        } else {
            crate::status_style::Status::from(chat.status)
        };
        let color = crate::status_style::status_style(status, theme.appearance).color;
        let status_label = if row.dangling {
            "Missing"
        } else {
            match chat.status {
                mast_proto::ChatIndicator::Working => "Working",
                mast_proto::ChatIndicator::AwaitingInput => "Needs you",
                mast_proto::ChatIndicator::Errored => "Failed",
                mast_proto::ChatIndicator::Completed => "Done",
                mast_proto::ChatIndicator::Idle => "Idle",
            }
        };
        // A chat waiting on the build queue says where it stands (`queued #2`, `building`);
        // with no build jobs the lookup returns at once.
        let status_label = match row.dangling {
            false => self
                .state
                .read(cx)
                .build_jobs()
                .row_badge(chat.status, &chat.id)
                .map_or(status_label, |badge| badge.label()),
            true => status_label,
        };
        let drift = !row.dangling && self.sidebar_metadata.drift.contains(&chat.id);
        let mut details = Vec::new();
        if drift {
            details.push("Instructions changed".to_owned());
        } else {
            details.push(status_label.to_owned());
        }
        if let Some(failures) = &chat.tool_failures {
            details.push(failed_tool_label(failures.count));
        }
        if let Some(branch) = &chat.branch {
            details.push(format!("⎇ {branch}"));
        }
        if let Some(number) = chat.pr_number.filter(|number| *number > 0) {
            details.push(format!("PR #{number}"));
        }
        let state = self.state.read(cx);
        if state.is_personal_assistant(&chat.id) {
            details.push("Assistant".into());
        }
        if state.selected_chat.as_deref() == Some(chat.id.as_str()) {
            details.push("Current".into());
        }
        let summary: SharedString = details.join(" · ").into();
        let mut tip = if drift {
            format!("{title} · {status_label} · {summary}")
        } else {
            format!("{title} · {summary}")
        };
        if let Some(model) = &chat.harness {
            tip.push_str(&format!(" · {model}"));
        }
        if let Some(failures) = &chat.tool_failures {
            if !failures.tools.is_empty() {
                tip.push_str(&format!(" · Failed tools: {}", failures.tools.join(", ")));
            }
        }
        let tip: SharedString = tip.into();
        let selector = format!("outline-drift-{key}");
        let chat_id = chat.id.clone();
        let metadata = metadata_line(key, theme)
            .child(crate::components::outline_chat_indicator(
                status,
                format!("outline-chat-status-{key}"),
                color,
                theme,
                cx.entity_id(),
                cx,
            ))
            .child(
                div()
                    .id(SharedString::from(format!("outline-chat-summary-{key}")))
                    .when(drift, |el| el.debug_selector(move || selector.clone()))
                    .when(drift, |el| {
                        el.cursor_pointer()
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation()
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some(shell) =
                                    this.shell.as_ref().and_then(gpui::WeakEntity::upgrade)
                                {
                                    shell.update(cx, |shell, cx| {
                                        shell.open_instruction_compare(&chat_id, cx)
                                    });
                                }
                            }))
                    })
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if drift || chat.tool_failures.is_some() {
                        theme.warning
                    } else {
                        theme.text_muted
                    })
                    .child(summary),
            )
            .when_some(chat.harness.clone(), |el, model| {
                el.child(
                    div()
                        .flex_none()
                        .max_w(px(MODEL_MAX_WIDTH))
                        .truncate()
                        .child(SharedString::from(model)),
                )
            });
        two_lines(
            key,
            title_line(key, &title, false, theme),
            metadata.into_any_element(),
            tip,
        )
    }
}

/// A group row's body, drawn outside the outline (the Shell's Scheduled
/// section) so it reads like the outline's own group rows: a semibold title
/// over a quiet summary line.
pub(crate) fn group_body(
    key: &str,
    title: &str,
    summary: String,
    color: gpui::Hsla,
    theme: &Theme,
) -> AnyElement {
    let tip: SharedString = if summary.is_empty() {
        title.to_owned()
    } else {
        format!("{title} · {summary}")
    }
    .into();
    let title = div()
        .w_full()
        .min_w_0()
        .truncate()
        .line_height(crate::typography::ui_rems(16.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(color)
        .child(SharedString::from(title.to_owned()))
        .into_any_element();
    let metadata = metadata_line(key, theme)
        .pl(px(0.0))
        .child(SharedString::from(summary))
        .into_any_element();
    two_lines(key, title, metadata, tip)
}

/// A Chat row's body, drawn outside the outline: the chat icon and title
/// over a status marker, a summary and the optional model.
#[allow(clippy::too_many_arguments)]
pub(crate) fn chat_summary_body(
    key: &str,
    title: &str,
    status: crate::status_style::Status,
    summary: String,
    model: Option<String>,
    theme: &Theme,
    view: gpui::EntityId,
    cx: &mut gpui::App,
) -> AnyElement {
    let color = crate::status_style::status_style(status, theme.appearance).color;
    let tip: SharedString = match &model {
        Some(model) => format!("{title} · {summary} · {model}"),
        None => format!("{title} · {summary}"),
    }
    .into();
    let metadata = metadata_line(key, theme)
        .child(crate::components::outline_chat_indicator(
            status,
            format!("outline-chat-status-{key}"),
            color,
            theme,
            view,
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.text_muted)
                .child(SharedString::from(summary)),
        )
        .when_some(model, |el, model| {
            el.child(
                div()
                    .flex_none()
                    .max_w(px(MODEL_MAX_WIDTH))
                    .truncate()
                    .child(SharedString::from(model)),
            )
        });
    two_lines(
        key,
        title_line(key, title, false, theme),
        metadata.into_any_element(),
        tip,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_metadata_preserves_attention_and_warning_meaning() {
        let inputs = OutlineInputs {
            spaces: vec![model::tests::ready(
                "s1",
                "Repo",
                super::super::super::store::fixture_snapshot("s1"),
            )],
            chats: Vec::new(),
            options: OutlineOptions::default(),
        };
        let metadata = SidebarMetadata::new(&inputs);
        assert!(metadata.space_summaries["s1"].contains("user action required"));
        assert_eq!(entries_label(1), "1 entry");
        assert_eq!(entries_label(2), "2 entries");
        assert_eq!(failed_tool_label(1), "1 tool call failed");
        for theme in [Theme::light(), Theme::dark()] {
            assert_eq!(notice_color(NoticeTone::Warning, &theme), theme.warning);
            assert_eq!(notice_color(NoticeTone::Muted, &theme), theme.text_muted);
            assert_ne!(
                notice_color(NoticeTone::Warning, &theme),
                notice_color(NoticeTone::Muted, &theme)
            );
        }
    }
}
