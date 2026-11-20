use super::*;

impl Overview {

    /// Compact badge row for one chat: PR action (if any, highest-severity
    /// reason only), ticket identifier (if any). `None` when there's nothing
    /// to show yet (no data fetched, or fetched and genuinely nothing
    /// linked) — callers skip rendering the row. Subagents are not a badge
    /// here — see `subagent_tiles_for` for the nested compact rows.
    pub(super) fn badges_for(&self, chat_id: &str, theme: &Theme) -> Option<gpui::AnyElement> {
        let link = self.link_status.get(chat_id).map(|e| &e.status);
        let ticket_label = link
            .and_then(|s| s.ticket.as_ref())
            .map(|t| t.identifier.clone());
        if !link.is_some_and(link_has_badge_row) {
            return None;
        }
        let ticket_source = link.and_then(|s| s.ticket_source);
        // Keep the complete badge set in one horizontal line and clip at the
        // row edge instead of wrapping. The aggregate tooltip exposes the
        // full clipped sequence; each reachable badge retains its provenance
        // tooltip too.
        let mut summary = Vec::new();
        let mut row = div()
            .debug_selector(|| "overview-row-badges".into())
            .flex_none()
            .w_full()
            .h(px(BADGE_ROW_H))
            .flex()
            .items_center()
            .gap(px(6.0))
            .overflow_hidden()
            .whitespace_nowrap();
        for (index, pr_link) in link.into_iter().flat_map(effective_pr_links).enumerate() {
            let pr = pr_link
                .detail
                .as_ref()
                .cloned()
                .unwrap_or_else(|| placeholder_pr(&pr_link.url));
            let (action, color) = pr
                .action_reasons()
                .into_iter()
                .next()
                .map(pr_action_badge)
                .map(|(label, color)| (label.to_string(), color))
                .unwrap_or_else(|| (pr.state.clone(), theme.text_muted));
            let label = format!("#{} {action}", pr.number);
            summary.push(label.clone());
            let pr_source = pr_link.source;
            let debug_id = format!("overview-badge-pr-{chat_id}-{index}");
            let badge_id = SharedString::from(debug_id.clone());
            row = row.child(
                link_source_hover(
                    crate::components::outline_metadata_tag(color, color.opacity(0.15), 6.0)
                        .id(badge_id)
                        .debug_selector(move || debug_id.clone())
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(3.0)),
                    LinkKind::Pr,
                    pr_source,
                )
                .when(pr_source == Some(ChatLinkSource::Manual), |el| {
                    el.child(manual_link_pin(color))
                })
                .child(SharedString::from(label)),
            );
        }
        if let Some(identifier) = ticket_label {
            summary.push(identifier.clone());
            let debug_id = format!("overview-badge-ticket-{chat_id}");
            let badge_id = SharedString::from(debug_id.clone());
            row = row.child(
                link_source_hover(
                    crate::components::outline_metadata_tag(
                        theme.text_muted,
                        theme.element_hover,
                        6.0,
                    )
                    .id(badge_id)
                    .debug_selector(move || debug_id.clone())
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(3.0)),
                    LinkKind::Ticket,
                    ticket_source,
                )
                .when(ticket_source == Some(ChatLinkSource::Manual), |el| {
                    el.child(manual_link_pin(theme.text_muted))
                })
                .child(SharedString::from(identifier)),
            );
        }
        let tooltip: SharedString = summary.join(" · ").into();
        Some(
            row.id(SharedString::from(format!("overview-badges-{chat_id}")))
                .tooltip(move |_, cx| cx.new(|_| LinkSourceTooltip(tooltip.clone())).into())
                .into_any_element(),
        )
    }
    /// Small archive affordance on the list row. Always visible rather than
    /// hover-gated: simpler, and a stray click only archives (reversible via
    /// the sidebar's own archived-chats view), not a destructive delete.
    pub(super) fn archive_button(
        &self,
        chat_id: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(SharedString::from(format!("overview-archive-{chat_id}")))
            .flex_none()
            .size(px(16.0))
            .rounded(px(3.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted.opacity(0.5))
            .hover(|el| el.bg(theme.element_hover).text_color(theme.text_muted))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_archived(chat_id.clone(), true, cx);
            }))
            .child(SharedString::from("×"))
            .into_any_element()
    }

    /// Explicit "View full chat" affordance — opens the interactive chat
    /// panel (`session_canvas.html`'s separate "View chat" button,
    /// `~1137-1143`: its own `pointerdown` stop-propagation keeps it from
    /// also triggering the row's click-to-expand gesture). Only the panel's
    /// "Open full chat →" actually leaves the overview. Row body clicks
    /// toggle expand in place instead — see `toggle_expanded`.
    #[cfg(test)]
    pub(crate) fn expanded_is_empty(&self) -> bool {
        self.expanded.is_empty()
    }

    /// The Work Outline drift mark for a drifted Chat; clicking opens the
    /// Shell's Compare dialog (where Apply lives).
    pub(super) fn instruction_drift_mark(
        &self,
        id: String,
        chat_id: &str,
        with_label: bool,
        theme: &Theme,
    ) -> gpui::AnyElement {
        let shell = self.shell.clone();
        let chat_id = chat_id.to_owned();
        crate::work_outline::chips::instruction_drift_mark(
            id,
            with_label,
            theme,
            move |_, _, cx| {
                shell
                    .update(cx, |shell, cx| shell.open_instruction_compare(&chat_id, cx))
                    .ok();
            },
        )
    }

    pub(super) fn open_chat_button(
        &self,
        chat_id: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(SharedString::from(format!("overview-open-{chat_id}")))
            .flex_none()
            .px(px(8.0))
            .py(px(2.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(theme.accent)
            .bg(theme.accent.opacity(0.1))
            .hover(|el| el.bg(theme.accent.opacity(0.2)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_chat_panel(chat_id.clone(), cx);
            }))
            .child(SharedString::from("View full chat →"))
            .into_any_element()
    }

    /// Inline expanded detail — toggled by a row click, never navigates. Ports `renderDetails`
    /// (`session_canvas.html:1092-1113`) field by field, from data actually
    /// available here:
    /// - branch + worktree badge (engine `isWorktree` when the link status
    ///   carries it, else the reference's own cwd rule, [`cwd_is_worktree`])
    /// - changes (diff stat) — only when the engine's link status carries a
    ///   `diffStat` (see `LinkStatusEntry::diff_stat`); absent today
    /// - pull request (title/state/draft,
    ///   review decision, checks, reviewer chips — click opens GitHub),
    ///   linear ticket (identifier/title/status — click opens Linear)
    /// - last message (Mast has no separate `summary` field)
    /// - "Open chat →" + Archive/Unarchive actions. The web's "Open in
    ///   Terminal/Cursor" has no Mast equivalent (the chat IS here) —
    ///   "Open full chat →" in the preview panel is the analog.
    pub(super) fn expanded_detail_for(
        &self,
        row: &OverviewRow,
        theme: &Theme,
        chat_id: String,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let entry = self.link_status.get(&row.chat.id);
        let link = entry.map(|e| &e.status);
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let label_el = |label: &'static str| {
            div()
                .flex_none()
                .w(px(78.0))
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_muted.opacity(0.6))
                .child(SharedString::from(label))
        };
        let detail_row = |label: &'static str, value: gpui::AnyElement| {
            div()
                .flex()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(10.0))
                .child(label_el(label))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme.text_muted)
                        .child(value),
                )
                .into_any_element()
        };
        let text = |value: String| SharedString::from(value).into_any_element();
        let chip = |text: String, color: gpui::Hsla, bg: gpui::Hsla| {
            crate::components::outline_metadata_tag(color, bg, 6.0).child(SharedString::from(text))
        };
        // `renderChips`/`renderToolChips`: an empty list renders a single
        // "none" chip rather than hiding the row.
        let chip_row = |chips: Vec<String>| {
            let chips = if chips.is_empty() {
                vec!["none".to_string()]
            } else {
                chips
            };
            div()
                .flex()
                .flex_wrap()
                .gap(px(4.0))
                .children(
                    chips
                        .into_iter()
                        .map(|t| chip(t, theme.text_muted, theme.element_hover)),
                )
                .into_any_element()
        };

        // branch
        let is_worktree = entry
            .and_then(|e| e.is_worktree)
            .unwrap_or_else(|| cwd_is_worktree(row.chat.cwd.as_deref()));
        let state = self.state.read(cx);
        let checkout = crate::change_requests::display_checkout(
            &row.chat,
            state.selected_chat_diff_target(&row.chat.id),
        );
        let branch_value = match checkout.branch {
            Some(branch) => div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(5.0))
                .child(SharedString::from(format!("⎇ {branch}")))
                .when(is_worktree, |el| {
                    el.child(chip(
                        "worktree".into(),
                        theme.text_muted,
                        theme.element_hover,
                    ))
                })
                .into_any_element(),
            None => text("—".into()),
        };
        rows.push(detail_row("branch", branch_value));

        // changes
        if let Some(diff) = entry.and_then(|e| e.diff_stat) {
            let files = if diff.files_changed == 1 {
                "file"
            } else {
                "files"
            };
            rows.push(detail_row(
                "changes",
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_color(gpui::rgb(0x0ca30c))
                            .child(SharedString::from(format!("+{}", diff.lines_added))),
                    )
                    .child(
                        div()
                            .text_color(gpui::rgb(0xd03b3b))
                            .child(SharedString::from(format!("-{}", diff.lines_removed))),
                    )
                    .child(SharedString::from(format!(
                        "· {} {files}",
                        diff.files_changed
                    )))
                    .into_any_element(),
            ));
        }

        if let Some(link) = link {
            let prs = effective_pr_links(link);
            if !prs.is_empty() {
                let values = prs.into_iter().enumerate().map(|(index, pr_link)| {
                    let pr = pr_link
                        .detail
                        .clone()
                        .unwrap_or_else(|| placeholder_pr(&pr_link.url));
                    let mut summary =
                        format!("#{} {}", pr.number, pr.title.clone().unwrap_or_default());
                    summary.push_str(" — ");
                    summary.push_str(if pr.is_draft { "draft" } else { &pr.state });
                    if let Some(decision) = &pr.review_decision {
                        summary.push_str(", ");
                        summary.push_str(&decision.to_lowercase().replace('_', " "));
                    }
                    let checks = pr.checks.map(checks_style);
                    if let Some((_, _, checks_label)) = checks {
                        summary.push_str(&format!(", checks {checks_label}"));
                    }
                    let url = pr_link.url.clone();
                    let open_url = url.clone();
                    let open_shell = self.shell.clone();
                    let pr_source = pr_link.source;
                    div()
                        .min_w_0()
                        .max_w_full()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .when(pr_source == Some(ChatLinkSource::Manual), |el| {
                                    el.child(manual_link_pin(theme.text_muted))
                                })
                                .child(
                                    link_source_hover(
                                        div()
                                            .id(SharedString::from(format!(
                                                "overview-detail-pr-{chat_id}-{index}"
                                            )))
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .gap(px(4.0)),
                                        LinkKind::Pr,
                                        pr_source,
                                    )
                                    .overflow_hidden()
                                    .cursor_pointer()
                                    .text_color(theme.accent)
                                    .hover(|el| el.underline())
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .max_w_full()
                                            .max_h(px(32.0))
                                            .line_height(px(16.0))
                                            .whitespace_normal()
                                            .overflow_hidden()
                                            .child(SharedString::from(summary)),
                                    )
                                    .when_some(checks, |el, (color, icon, _)| {
                                        el.child(
                                            div()
                                                .flex_none()
                                                .text_color(color)
                                                .child(SharedString::from(icon)),
                                        )
                                    })
                                    .on_click(
                                        crate::github::chip::open_pr(Some(open_shell), open_url),
                                    ),
                                )
                                .when(pr_source.is_some(), |el| {
                                    el.child(self.unlink_button(
                                        chat_id.clone(),
                                        LinkKind::Pr,
                                        Some(url.clone()),
                                        theme,
                                        cx,
                                    ))
                                }),
                        )
                        .when(!pr.reviewers.is_empty(), |el| {
                            el.child(chip_row(pr.reviewers.clone()))
                        })
                });
                rows.push(detail_row(
                    if link.pr_links.len() > 1 {
                        "pull requests"
                    } else {
                        "pull request"
                    },
                    div()
                        .min_w_0()
                        .max_w_full()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .children(values)
                        .into_any_element(),
                ));
            }
        }

        if let Some(ticket) = link.and_then(|s| s.ticket.as_ref()) {
            let mut summary = ticket.identifier.clone();
            if let Some(title) = &ticket.title {
                summary.push_str(&format!(" — {title}"));
            }
            if let Some(status) = &ticket.status {
                summary.push_str(&format!(" ({status})"));
            }
            let url = ticket.url.clone();
            let ticket_shell = self.shell.clone();
            let ticket_source = link.and_then(|s| s.ticket_source);
            rows.push(detail_row(
                "linear ticket",
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .when(ticket_source == Some(ChatLinkSource::Manual), |el| {
                        el.child(manual_link_pin(theme.text_muted))
                    })
                    .child(
                        link_source_hover(
                            div()
                                .id(SharedString::from(format!(
                                    "overview-detail-ticket-{chat_id}"
                                )))
                                .flex_1()
                                .min_w_0(),
                            LinkKind::Ticket,
                            ticket_source,
                        )
                        .when(url.is_some(), |el| {
                            el.cursor_pointer()
                                .text_color(theme.accent)
                                .hover(|el| el.underline())
                        })
                        .child(SharedString::from(summary))
                        .when_some(url, |el, url| {
                            el.on_click(crate::github::chip::open_pr(Some(ticket_shell), url))
                        }),
                    )
                    .when(ticket_source.is_some(), |el| {
                        el.child(self.unlink_button(
                            chat_id.clone(),
                            LinkKind::Ticket,
                            None,
                            theme,
                            cx,
                        ))
                    })
                    .into_any_element(),
            ));
        }

        if let Some(preview) = &row.chat.last_message_preview {
            rows.push(detail_row(
                "last message",
                text(crate::message_source::preview(preview)),
            ));
        }

        let archived = row.chat.archived;
        let archive_id = chat_id.clone();
        let archive_toggle = div()
            .id(SharedString::from(format!(
                "overview-detail-archive-{chat_id}"
            )))
            .flex_none()
            .px(px(8.0))
            .py(px(2.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(theme.text_muted)
            .border_1()
            .border_color(theme.border)
            .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_archived(archive_id.clone(), !archived, cx);
            }))
            .child(SharedString::from(if archived {
                "Unarchive"
            } else {
                "Archive"
            }));

        let closeout_id = chat_id.clone();
        let closeout = row.closeable.then(|| {
            div()
                .id(SharedString::from(format!(
                    "overview-detail-closeout-{chat_id}"
                )))
                .flex_none()
                .px(px(8.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_muted)
                .border_1()
                .border_color(theme.border)
                .hover(|el| el.bg(theme.element_hover).text_color(theme.danger))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_closeout(closeout_id.clone(), cx);
                }))
                .child(SharedString::from("Close out worktree…"))
        });

        let editor_open = self.link_editor_chat.as_deref() == Some(chat_id.as_str());
        let link_toggle_id = chat_id.clone();
        let link_toggle = div()
            .id(SharedString::from(format!(
                "overview-detail-link-{chat_id}"
            )))
            .flex_none()
            .px(px(8.0))
            .py(px(2.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(theme.text_muted)
            .border_1()
            .border_color(theme.border)
            .when(editor_open, |el| {
                el.bg(theme.element_active).text_color(theme.text)
            })
            .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.link_editor_chat.as_deref() == Some(link_toggle_id.as_str()) {
                    this.close_link_editor(cx);
                } else {
                    this.open_link_editor(link_toggle_id.clone(), false, window, cx);
                }
            }))
            .child(SharedString::from(if editor_open {
                "Cancel"
            } else {
                "Add PR / ticket…"
            }));

        let link_error = self
            .link_error
            .as_ref()
            .filter(|(id, _)| *id == chat_id)
            .map(|(_, message)| message.clone());
        let editor = editor_open.then(|| {
            let hint = if self.link_submitting {
                "Linking…".to_string()
            } else {
                "Enter to add · Esc to cancel".to_string()
            };
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                            match event.keystroke.key.as_str() {
                                "enter" => this.submit_link_editor(cx),
                                "escape" => this.close_link_editor(cx),
                                _ => return,
                            }
                            cx.stop_propagation();
                            window.prevent_default();
                        }))
                        .child(popover::search_input_frame(
                            theme,
                            self.link_input.clone().into_any_element(),
                        )),
                )
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(9.5))
                        .text_color(theme.text_muted.opacity(0.6))
                        .child(SharedString::from(hint)),
                )
                .into_any_element()
        });
        let critical: gpui::Hsla = gpui::rgb(0xd03b3b).into();
        let error_line = link_error.map(|message| {
            div()
                .text_size(crate::typography::ui_rems(9.5))
                .text_color(critical)
                .child(SharedString::from(message))
                .into_any_element()
        });

        div()
            .debug_selector(|| "overview-expanded-detail".into())
            .w_full()
            .max_w_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .gap(px(4.0))
            .p(px(8.0))
            .mt(px(4.0))
            .rounded(px(6.0))
            // Was `theme.surface.opacity(0.6)` — see-through enough to hurt
            // contrast against whatever else is underneath. Solid background,
            // same as the row itself.
            .bg(theme.surface_raised)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(rows)
            .child(
                div()
                    .pt(px(4.0))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.0))
                    .child(self.open_chat_button(chat_id, theme, cx))
                    .child(archive_toggle)
                    .child(link_toggle)
                    .children(closeout),
            )
            .children(editor)
            .children(error_line)
            .into_any_element()
    }
    /// Small "✕" that clears a stored (any-source) PR/ticket link —
    /// `SET_CHAT_LINK` with `value: null`. Only rendered when the slot has a
    /// stored source; an inference-derived link has nothing to clear.
    pub(super) fn unlink_button(
        &self,
        chat_id: String,
        kind: LinkKind,
        value: Option<String>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tooltip = SharedString::from(match kind {
            LinkKind::Pr => "Unlink this PR",
            LinkKind::Ticket => "Unlink this ticket",
        });
        div()
            .id(SharedString::from(format!(
                "overview-unlink-{}-{chat_id}",
                kind.wire()
            )))
            .flex_none()
            .px(px(4.0))
            .rounded(px(3.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(theme.text_muted.opacity(0.5))
            .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                let operation = if kind == LinkKind::Pr {
                    LinkOperation::Remove
                } else {
                    LinkOperation::Clear
                };
                this.set_chat_link(chat_id.clone(), kind, value.clone(), operation, cx);
            }))
            .tooltip(move |_, cx| cx.new(|_| LinkSourceTooltip(tooltip.clone())).into())
            .child(SharedString::from("✕"))
            .into_any_element()
    }
}
