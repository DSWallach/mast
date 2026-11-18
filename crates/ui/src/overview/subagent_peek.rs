use super::*;

impl Overview {

    /// Clicking a compact subagent row: open the right-side panel in its
    /// read-only transcript mode (web `openTranscriptPanel(session.id,
    /// sub.id, title)`), replacing whatever the panel showed — including the
    /// live chat panel, which is closed, not stacked underneath. Issues a
    /// fresh `READ_SUBAGENT_TRANSCRIPT` under a new request id every time
    /// (re-clicking the same subagent reloads it), so a slower earlier reply
    /// can never overwrite a newer peek ([`peek_reply_is_current`]).
    pub(super) fn open_subagent_peek(
        &mut self,
        chat_id: String,
        sub: &SubagentItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.peek_request_seq += 1;
        let request_id = self.peek_request_seq;
        self.chat_panel_open = false;
        self.subagent_peek = Some(SubagentPeek {
            chat_id: chat_id.clone(),
            agent_id: sub.agent_id.clone(),
            name: sub.display_name(),
            agent_type: sub.agent_type.clone(),
            request_id,
            load: PeekLoad::Loading,
            open_tools: HashSet::new(),
        });
        window.focus(&self.peek_focus, cx);
        cx.notify();
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.apply_peek_reply(request_id, None, cx);
            return;
        };
        let agent_id = sub.agent_id.clone();
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::READ_SUBAGENT_TRANSCRIPT,
                    serde_json::json!({ "chatId": chat_id, "agentId": agent_id }),
                )
                .await;
            let decoded = result
                .ok()
                .and_then(|value| serde_json::from_value::<SubagentTranscript>(value).ok());
            this.update(cx, |this, cx| {
                this.apply_peek_reply(request_id, decoded, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Land a peek fetch (`None` = RPC or decode failure) — dropped unless
    /// it's still the current peek's request.
    pub(super) fn apply_peek_reply(
        &mut self,
        request_id: u64,
        reply: Option<SubagentTranscript>,
        cx: &mut Context<Self>,
    ) {
        if !peek_reply_is_current(self.subagent_peek.as_ref(), request_id) {
            return;
        }
        let Some(peek) = self.subagent_peek.as_mut() else {
            return;
        };
        peek.load = match reply {
            Some(transcript) => PeekLoad::Loaded(transcript),
            None => PeekLoad::Failed,
        };
        // Web `renderTurns` → `scrollTop = scrollHeight`: open at the end.
        self.peek_scroll.scroll_to_bottom();
        cx.notify();
    }

    /// ✕ / Escape on a peek: back to the bare list.
    pub(super) fn close_subagent_peek(&mut self, cx: &mut Context<Self>) {
        if self.subagent_peek.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn ensure_subagents(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let ttl = if self
            .subagents
            .get(&chat_id)
            .is_some_and(|subs| subs.iter().any(|s| s.status == SubagentStatus::Running))
        {
            SUBAGENTS_RUNNING_REFRESH
        } else {
            SUBAGENTS_REFRESH
        };
        let fresh_enough = self
            .subagents_fetched_at
            .get(&chat_id)
            .is_some_and(|at| at.elapsed() < ttl);
        if fresh_enough || self.subagent_pending.contains(&chat_id) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.subagent_pending.insert(chat_id.clone());
        self.perf.fetch_requests += 1;
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SCAN_CHAT_SUBAGENTS,
                    serde_json::json!({ "chatId": chat_id }),
                )
                .await;
            this.update(cx, |this, cx| {
                this.subagent_pending.remove(&chat_id);
                this.subagents_fetched_at
                    .insert(chat_id.clone(), Instant::now());
                if let Ok(value) = result
                    && let Ok(subagents) = serde_json::from_value::<Vec<SubagentItem>>(value)
                {
                    // `subagent_count` is baked onto `OverviewRow`, so only a
                    // COUNT change rebuilds rows; a status flip (running →
                    // done) or a renamed description just repaints, matching
                    // how `ensure_context_usage` only dirties rows on a
                    // visible change.
                    let previous_count = this.subagents.get(&chat_id).map_or(0, Vec::len);
                    let next_count = subagents.len();
                    this.subagents.insert(chat_id, subagents);
                    if previous_count != next_count {
                        this.rows_dirty = true;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Compact nested rows for a chat's subagents — `session_canvas.html`'s
    /// `.tile.compact` sub-agent tiles (`~441-517`, `renderTile`'s compact
    /// branch `~1150-1160`, the stack itself `~1864-1906`). At rest this
    /// returns three agents plus a "+N more" row; `show_all` returns the full
    /// stack while the parent row is hovered. Each agent is a single
    /// fixed-height row — status dot (running = accent, done = green),
    /// truncated name, muted agent type, and a "⋯" affordance — with the full
    /// name in a hover tooltip (gpui's deferred tooltip layer: floats above
    /// everything, never reflows or gets clipped by the parent row's
    /// `overflow_hidden`). Spans the parent's width; never sized by its own
    /// context (the web's rule).
    ///
    /// Clicking one opens its read-only transcript peek
    /// (`open_subagent_peek`). The press stops propagation at mouse-DOWN —
    /// like the web's `subTile.addEventListener("pointerdown", e =>
    /// e.stopPropagation())` — so the parent row never toggles its expand,
    /// and the click itself stops again for good measure.
    ///
    /// Empty when the chat has no subagents (or they haven't been fetched
    /// yet) — callers skip rendering.
    pub(super) fn subagent_tiles_for(
        &self,
        chat_id: &str,
        theme: &Theme,
        show_all: bool,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let Some(subagents) = self.subagents.get(chat_id) else {
            return Vec::new();
        };
        let visible_count = if show_all {
            subagents.len()
        } else {
            subagents.len().min(SUBAGENT_PREVIEW_LIMIT)
        };
        let mut tiles: Vec<gpui::AnyElement> = subagents
            .iter()
            .take(visible_count)
            .map(|sub| {
                let color = sub.status.color(theme.appearance);
                let status = if sub.status == SubagentStatus::Running {
                    crate::status_style::Status::Working
                } else {
                    crate::status_style::Status::Done
                };
                let indicator = crate::components::outline_chat_indicator(
                    status,
                    format!("overview-subagent-status-{chat_id}-{}", sub.agent_id),
                    color,
                    theme,
                    cx.entity_id(),
                    cx,
                );
                let name = sub.display_name();
                let peeking = self
                    .subagent_peek
                    .as_ref()
                    .is_some_and(|p| p.chat_id == chat_id && p.agent_id == sub.agent_id);
                let tooltip: SharedString = match sub.agent_type.as_deref() {
                    Some(t) if !t.is_empty() => format!("{name} · {t} · {}", sub.status.label()),
                    _ => format!("{name} · {}", sub.status.label()),
                }
                .into();
                let click_chat = chat_id.to_string();
                let click_sub = sub.clone();
                div()
                    .id(SharedString::from(format!(
                        "overview-subagent-{chat_id}-{}",
                        sub.agent_id
                    )))
                    .debug_selector(|| "overview-subagent-tile".into())
                    .flex_none()
                    .w_full()
                    .h(px(SUBAGENT_TILE_H))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .px(px(7.0))
                    .rounded(px(8.0))
                    .border_1()
                    // Same status tint as the parent card (`renderTile`'s
                    // 14% background / 30% border color-mix).
                    .border_color(theme.border.blend(color.opacity(0.30)))
                    .bg(theme.surface_raised.blend(color.opacity(0.14)))
                    .cursor_pointer()
                    .hover(|el| el.border_color(color.opacity(0.6)))
                    .when(peeking, |el| el.border_color(theme.accent))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_subagent_peek(click_chat.clone(), &click_sub, window, cx);
                    }))
                    .tooltip(move |_, cx| cx.new(|_| LinkSourceTooltip(tooltip.clone())).into())
                    .child(indicator)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(20.0))
                            .truncate()
                            .text_size(crate::typography::ui_rems(10.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(SharedString::from(name)),
                    )
                    .when_some(
                        sub.agent_type.clone().filter(|t| !t.is_empty()),
                        |el, agent_type| {
                            el.child(
                                div()
                                    .flex_shrink(1.0)
                                    .min_w_0()
                                    .max_w(px(120.0))
                                    .truncate()
                                    .text_size(crate::typography::ui_rems(9.0))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(agent_type)),
                            )
                        },
                    )
                    .child(
                        div()
                            .flex_none()
                            .size(px(15.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.0))
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from("⋯")),
                    )
                    .into_any_element()
            })
            .collect();

        if !show_all && subagents.len() > SUBAGENT_PREVIEW_LIMIT {
            let remaining = subagents.len() - SUBAGENT_PREVIEW_LIMIT;
            tiles.push(
                div()
                    .debug_selector(|| "overview-subagent-more".into())
                    .flex_none()
                    .w_full()
                    .h(px(SUBAGENT_TILE_H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border.opacity(0.65))
                    .bg(theme.surface_raised.opacity(0.6))
                    .text_size(crate::typography::ui_rems(9.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!(
                        "+{remaining} more · hover to expand"
                    )))
                    .into_any_element(),
            );
        }
        tiles
    }

    /// The right-side panel's read-only subagent-transcript mode — `Some`
    /// only while `subagent_peek` is set. Web `#transcript-panel` opened for
    /// a sub-agent: header = the subagent's name + agent type (+ model once
    /// loaded) and a ✕; body = its turns (role label + time, text, tool
    /// chips), scrolled to the end on load. No composer: a subagent can't be
    /// continued. Same width/chrome as the live chat panel so switching
    /// between the two modes doesn't jump the layout.
    pub(super) fn render_subagent_peek(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let peek = self.subagent_peek.as_ref()?;
        let code_family = crate::typography::code_effective_family_name(cx);
        let mut subtitle = String::from("sub-agent transcript");
        if let PeekLoad::Loaded(t) = &peek.load
            && let Some(model) = t.model.as_deref().filter(|m| !m.is_empty())
        {
            subtitle.push_str(" · ");
            subtitle.push_str(model);
        }
        let hint = |text: &'static str| {
            div()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted.opacity(0.7))
                .child(SharedString::from(text))
                .into_any_element()
        };
        let body: Vec<gpui::AnyElement> = match &peek.load {
            PeekLoad::Loading => vec![hint("Loading…")],
            PeekLoad::Failed => vec![hint("Failed to load transcript.")],
            PeekLoad::Loaded(t) if t.turns.is_empty() => {
                vec![hint("No turns found in this transcript.")]
            }
            PeekLoad::Loaded(t) => t
                .turns
                .iter()
                .enumerate()
                .map(|(ti, turn)| {
                    self.render_peek_turn(ti, turn, &peek.open_tools, theme, &code_family, cx)
                })
                .collect(),
        };
        let name: SharedString = peek.name.clone().into();
        let agent_type = peek.agent_type.clone().filter(|t| !t.is_empty());
        Some(
            div()
                .id("overview-subagent-peek")
                .debug_selector(|| "overview-subagent-peek".into())
                .track_focus(&self.peek_focus)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.close_subagent_peek(cx);
                        cx.stop_propagation();
                    }
                }))
                .flex_none()
                .w(px(CHAT_PANEL_W))
                .h_full()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .px(px(12.0))
                        .py(px(10.0))
                        .border_b_1()
                        .border_color(theme.border)
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
                                                .text_size(crate::typography::ui_rems(12.0))
                                                .text_color(theme.text)
                                                .child(name),
                                        )
                                        .when_some(agent_type, |el, agent_type| {
                                            el.child(
                                                div()
                                                    .flex_none()
                                                    .max_w(px(160.0))
                                                    .truncate()
                                                    .text_size(crate::typography::ui_rems(10.5))
                                                    .text_color(theme.text_muted)
                                                    .child(SharedString::from(agent_type)),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(crate::typography::ui_rems(10.0))
                                        .text_color(theme.text_muted.opacity(0.7))
                                        .child(SharedString::from(subtitle)),
                                ),
                        )
                        .child(
                            div()
                                .id("overview-subagent-peek-close")
                                .flex_none()
                                .cursor_pointer()
                                .text_color(theme.text_muted)
                                .hover(|el| el.text_color(theme.text))
                                .child(SharedString::from("✕"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.close_subagent_peek(cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .id("overview-subagent-peek-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.peek_scroll)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(14.0))
                                .px(px(14.0))
                                .py(px(12.0))
                                .children(body),
                        ),
                )
                .into_any_element(),
        )
    }

    /// One transcript turn (web `renderTurns`' `.turn`): role label + time,
    /// the text (newlines preserved; ``` fences as monospace blocks — see
    /// [`split_fenced_code`]), then its tool chips. A chip with a result
    /// preview is a disclosure (web `<details class="tool-detail">`):
    /// clicking toggles the result open underneath.
    pub(super) fn render_peek_turn(
        &self,
        ti: usize,
        turn: &TranscriptTurn,
        open_tools: &HashSet<(usize, usize)>,
        theme: &Theme,
        code_family: &SharedString,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let is_user = turn.role == "user";
        let time = format_turn_time(turn.timestamp.as_deref());
        let text_blocks: Vec<gpui::AnyElement> = turn
            .text
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .map(split_fenced_code)
            .unwrap_or_default()
            .into_iter()
            .map(|(is_code, chunk)| {
                if is_code {
                    div()
                        .w_full()
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(5.0))
                        .bg(theme.element_hover.opacity(0.6))
                        .font_family(code_family.clone())
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text)
                        .child(SharedString::from(chunk))
                        .into_any_element()
                } else {
                    div()
                        .w_full()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text)
                        .child(SharedString::from(chunk))
                        .into_any_element()
                }
            })
            .collect();
        let tools: Vec<gpui::AnyElement> = turn
            .tools
            .iter()
            .enumerate()
            .map(|(tj, tool)| {
                let open = open_tools.contains(&(ti, tj));
                let has_result = tool
                    .result_preview
                    .as_deref()
                    .is_some_and(|r| !r.is_empty());
                let summary = if tool.input_preview.is_empty() {
                    tool.name.clone()
                } else {
                    format!("{} {}", tool.name, tool.input_preview)
                };
                let chip = div()
                    .id(SharedString::from(format!("overview-peek-tool-{ti}-{tj}")))
                    .max_w_full()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .px(px(7.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.element_hover.opacity(0.4))
                    .font_family(code_family.clone())
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .when(has_result, |el| {
                        el.cursor_pointer()
                            .hover(|el| el.text_color(theme.text))
                            .child(SharedString::from(if open { "▾" } else { "▸" }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(peek) = this.subagent_peek.as_mut()
                                    && !peek.open_tools.remove(&(ti, tj))
                                {
                                    peek.open_tools.insert((ti, tj));
                                }
                                cx.notify();
                            }))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(summary)),
                    );
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(chip)
                    .when(open, |el| {
                        el.when_some(tool.result_preview.clone(), |el, result| {
                            el.child(
                                div()
                                    .ml(px(10.0))
                                    .px(px(8.0))
                                    .py(px(5.0))
                                    .rounded(px(4.0))
                                    .border_l_2()
                                    .border_color(theme.border)
                                    .font_family(code_family.clone())
                                    .text_size(crate::typography::ui_rems(10.5))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(result)),
                            )
                        })
                    })
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(crate::typography::ui_rems(10.5))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(if is_user {
                                theme.accent
                            } else {
                                theme.text_muted
                            })
                            .child(SharedString::from(turn_role_label(&turn.role))),
                    )
                    .when(!time.is_empty(), |el| {
                        el.child(
                            div()
                                .text_color(theme.text_muted.opacity(0.6))
                                .child(SharedString::from(time)),
                        )
                    }),
            )
            .children(text_blocks)
            .when(!tools.is_empty(), |el| {
                el.child(div().flex().flex_col().gap(px(4.0)).children(tools))
            })
            .into_any_element()
    }
}
