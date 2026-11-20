use super::*;

impl Overview {
    pub(super) fn render_list_body(
        &mut self,
        rows: &std::rc::Rc<Vec<OverviewRow>>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mounted_ids: Vec<String> = rows.iter().map(|row| row.chat.id.clone()).collect();
        reconcile_hovered_row(&mut self.hovered_row, &mounted_ids);
        self.list_order.clear();
        if rows.is_empty() {
            self.list_selected = None;
            self.list.reset(0);
            self.list_items.clear();
            self.list_rows = rows.clone();
            return self.render_empty_state(theme, cx);
        }

        let row_indices: HashMap<&str, usize> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.chat.id.as_str(), index))
            .collect();
        let mut items = Vec::new();
        if self.active_groups.is_empty() {
            for (row_index, row) in rows.iter().enumerate() {
                items.push(ListPresentationItem::Row {
                    chat_id: row.chat.id.clone(),
                    row_index,
                });
            }
        } else {
            let mut flattened = Vec::new();
            let refs: Vec<&OverviewRow> = rows.iter().collect();
            flatten_partition(
                &overview_grouping::partition(refs, &self.active_groups),
                0,
                &mut flattened,
            );
            for item in flattened {
                items.push(match item {
                    ListItem::Header {
                        label,
                        color,
                        depth,
                        count,
                    } => ListPresentationItem::Header {
                        label,
                        color,
                        depth,
                        count,
                    },
                    ListItem::Row(row) => ListPresentationItem::Row {
                        chat_id: row.chat.id.clone(),
                        row_index: row_indices[row.chat.id.as_str()],
                    },
                });
            }
        }
        for (child, item) in items.iter().enumerate() {
            if let ListPresentationItem::Row { chat_id, .. } = item {
                self.list_order.push((chat_id.clone(), child));
            }
        }

        if self.list_items != items {
            let old = &self.list_items;
            let prefix = old.iter().zip(&items).take_while(|(a, b)| a == b).count();
            let suffix = old[prefix..]
                .iter()
                .rev()
                .zip(items[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            let mut anchor = self.list.logical_scroll_top();
            let anchor_item = old.get(anchor.item_ix).cloned();
            self.list
                .splice(prefix..old.len() - suffix, items.len() - prefix - suffix);
            if let Some(index) = anchor_item.and_then(|item| {
                items
                    .iter()
                    .position(|candidate| candidate.same_identity(&item))
            }) {
                anchor.item_ix = index;
                self.list.scroll_to(anchor);
            }
            self.list_items = items;
        }
        self.list_rows = rows.clone();
        if let Some(child) = self.focus_due.take().and_then(|id| {
            self.list_order
                .iter()
                .find_map(|(row, child)| (row == &id).then_some(*child))
        }) {
            self.reveal_list_item(child);
        }
        // Rows can change height when details, subagents, badges or link data
        // change. Keep measured heights as hints while rechecking visible rows.
        self.list.remeasure();
        if self
            .list_selected
            .as_ref()
            .is_some_and(|id| !self.list_order.iter().any(|(row, _)| row == id))
        {
            self.list_selected = None;
        }
        div()
            .id("overview-list")
            .debug_selector(|| "overview-list-viewport".into())
            .flex_1()
            .min_h_0()
            .px(px(8.0))
            .pb(px(8.0))
            .child(
                list(self.list.clone(), cx.processor(Self::render_list_item))
                    .size_full()
                    .with_sizing_behavior(gpui::ListSizingBehavior::Auto),
            )
            .into_any_element()
    }

    pub(super) fn render_list_item(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(item) = self.list_items.get(index).cloned() else {
            return div().into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let content = match item {
            ListPresentationItem::Header {
                label,
                color,
                depth,
                count,
            } => div()
                .flex()
                .pl(px(10.0 + depth as f32 * 14.0))
                .pr(px(10.0))
                .pt(px(if depth == 0 { 14.0 } else { 8.0 }))
                .pb(px(2.0))
                .child(crate::components::outline_lane_heading(
                    format!("{label} ({count})"),
                    color,
                    depth,
                    &theme,
                ))
                .into_any_element(),
            ListPresentationItem::Row { row_index, .. } => {
                let rows = self.list_rows.clone();
                let row = &rows[row_index];
                let row_number = self
                    .list_order
                    .iter()
                    .position(|(id, _)| id == &row.chat.id)
                    .unwrap_or(row_index);
                self.render_list_row(row_number, row, &theme, cx)
            }
        };
        div()
            .when(index + 1 < self.list_items.len(), |el| el.pb(px(1.0)))
            .child(content)
            .into_any_element()
    }

    /// One List-mode row. Same click split as the web's `renderTile`: clicking the row toggles its expanded detail in place
    /// (and acknowledges an unread done); the explicit "View full chat →"
    /// control opens the interactive chat panel. Neither navigates away
    /// — "Open full chat →" in that panel does.
    pub(super) fn render_list_row(
        &mut self,
        ix: usize,
        row: &OverviewRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let now = Utc::now();
        let chat_id = row.chat.id.clone();
        let title: SharedString = row
            .chat
            .title
            .clone()
            .unwrap_or_else(|| "New session".to_string())
            .into();
        let cwd: SharedString = row
            .chat
            .cwd
            .clone()
            .unwrap_or_else(|| "No folder".to_string())
            .into();
        let when: SharedString = row
            .chat
            .last_message_at
            .map(|at| format_time_ago(at, now))
            .unwrap_or_else(|| format_time_ago(row.chat.created_at, now))
            .into();
        let dot = crate::status_style::status_style(row.status.into(), theme.appearance).color;
        let indicator = crate::components::outline_chat_indicator(
            row.status.into(),
            format!("overview-status-{chat_id}"),
            dot,
            theme,
            cx.entity_id(),
            cx,
        );
        let (icon, label) = status_style(row.status);
        let unread = is_unread_done(
            &self.acknowledged_done,
            &chat_id,
            row.status == ChatIndicator::Completed,
        );
        let (dim, ribbon) = card_dimming(row.chat.archived, row.stale, unread);
        // A chat whose work item needs the user stays at full opacity.
        let dim = dim.filter(|_| !(row.user_action_required && !row.chat.archived));
        let attention = crate::work_outline::chips::attention_indicator(
            format!("overview-row-attention-{chat_id}"),
            row.user_action_required,
            row.user_action_count,
            theme,
        );
        let exception_mark = row.execution_exception.then(|| {
            crate::work_outline::chips::execution_exception_mark(
                format!("overview-row-execution-exception-{chat_id}"),
                theme,
            )
        });
        let drift_mark = row.instruction_drift.then(|| {
            self.instruction_drift_mark(
                format!("overview-row-instruction-drift-{chat_id}"),
                &chat_id,
                false,
                theme,
            )
        });
        let path_chip = row.work_item_path.as_ref().map(|path| {
            crate::work_outline::chips::path_chip(
                format!("overview-row-work-item-{chat_id}"),
                path,
                theme,
            )
        });
        let is_expanded = self.expanded.contains(&chat_id);
        let model = model_badge(&row.chat, theme);
        let badges = self.badges_for(&chat_id, theme);
        let archive = (!row.chat.archived).then(|| self.archive_button(chat_id.clone(), theme, cx));
        let open_button = self.open_chat_button(chat_id.clone(), theme, cx);
        let has_subagent_overflow = row.subagent_count > SUBAGENT_PREVIEW_LIMIT;
        let show_all_subagents =
            has_subagent_overflow && self.hovered_row.as_deref() == Some(chat_id.as_str());
        let subagent_tiles = self.subagent_tiles_for(&chat_id, theme, show_all_subagents, cx);
        let detail = is_expanded.then(|| self.expanded_detail_for(row, theme, chat_id.clone(), cx));
        let toggle_id = chat_id.clone();
        let row_selector = format!("overview-list-chat-{chat_id}");
        let hover_id = chat_id.clone();
        div()
            .id(("overview-row", ix))
            .debug_selector(|| "overview-list-row".into())
            .relative()
            // Never shrink inside the scrolling list column. The row is
            // `overflow_hidden` (below, for the ribbon), which zeroes its
            // automatic flex min-height — so without this, once the list
            // outgrows the viewport every row got squeezed toward 0 and the
            // squeezed part (the subagent rows at the bottom first) was
            // clipped away instead of the list scrolling.
            .flex_none()
            .flex()
            .flex_col()
            .rounded(px(6.0))
            // Clips the unread-done ribbon (an absolutely-positioned child
            // below) to the row's own rounded corner instead of squaring it
            // off; see `unread_done_ribbon`. A list row is normally
            // background-less and no longer needs an opaque plate now that
            // there's no halo to ring — the ribbon just paints on top.
            .overflow_hidden()
            .border_1()
            .border_color(gpui::transparent_black())
            // The chat open in the side panel.
            .when(
                self.panel_highlight.as_deref() == Some(chat_id.as_str()),
                |el| el.border_color(theme.accent),
            )
            .when(is_expanded, |el| el.bg(theme.element_hover.opacity(0.5)))
            .when(
                self.list_selected.as_deref() == Some(chat_id.as_str()),
                |el| el.bg(theme.element_active),
            )
            .when_some(dim, |el, opacity| el.opacity(opacity))
            .hover(|el| el.bg(theme.element_hover))
            .when(has_subagent_overflow, |el| {
                el.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        if this.hovered_row.as_deref() != Some(hover_id.as_str()) {
                            this.hovered_row = Some(hover_id.clone());
                            cx.notify();
                        }
                    } else if this.hovered_row.as_deref() == Some(hover_id.as_str()) {
                        this.hovered_row = None;
                        cx.notify();
                    }
                }))
            })
            .when(ribbon, |el| el.child(unread_done_ribbon(1.0)))
            .child(
                div()
                    .id(("overview-row-click", ix))
                    .debug_selector(move || row_selector.clone())
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_expanded(&toggle_id, cx);
                        cx.notify();
                    }))
                    .child(indicator)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(theme.text)
                                            .child(title),
                                    )
                                    .children(drift_mark)
                                    .children(exception_mark)
                                    .when_some(attention, |el, attention| el.child(attention)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(crate::typography::ui_rems(11.0))
                                            .text_color(theme.text_muted.opacity(0.7))
                                            .child(cwd),
                                    )
                                    .child(model)
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(crate::typography::ui_rems(10.0))
                                            .text_color(dot)
                                            .child(SharedString::from(format!("{icon} {label}"))),
                                    )
                                    .when_some(path_chip, |el, chip| el.child(chip)),
                            )
                            .when_some(badges, |el, badges| el.child(badges)),
                    )
                    // Same bar + label as the web's tile face, at a fixed width so
                    // the column lines up down the list.
                    .child(
                        div()
                            .flex_none()
                            .w(px(76.0))
                            .child(context_meter(row.context_pct, theme)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(10.0))
                            .text_color(theme.text_muted.opacity(0.6))
                            .child(when),
                    )
                    .child(open_button)
                    .when_some(archive, |el, archive| el.child(archive)),
            )
            .when(!subagent_tiles.is_empty(), |el| {
                el.child(
                    div()
                        .debug_selector(|| "overview-list-subagents".into())
                        .flex()
                        .flex_col()
                        .gap(px(SUBAGENT_TILE_GAP))
                        .pb(px(8.0))
                        .pl(px(27.0))
                        .pr(px(10.0))
                        .children(subagent_tiles),
                )
            })
            .when_some(detail, |el, detail| {
                el.child(div().px(px(10.0)).pb(px(8.0)).child(detail))
            })
            .into_any_element()
    }
}
