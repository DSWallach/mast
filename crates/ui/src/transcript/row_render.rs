use super::*;

impl Transcript {
    pub(super) fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(test)]
        RENDERED_ROW_COUNT.with(|count| count.set(count.get() + 1));
        let Some(row) = self.rows.get(ix).cloned() else {
            return gpui::Empty.into_any_element();
        };
        self.rendered_rows.insert(row.id.clone());
        let theme = Theme::of(cx).clone();
        let identity = {
            let state = self.state.read(cx);
            let chat = self.chat_id.as_deref()
                .and_then(|id| state.chats.iter().find(|chat| chat.id == id))
                .or_else(|| self.chat_id.is_none().then(|| state.selected_chat_row()).flatten());
            let config = chat.and_then(|chat| chat.config.as_ref());
            theme.agent_identity_color(config.map(|config| config.harness), config.and_then(|config| config.model.as_deref()).or_else(|| chat.and_then(|chat| chat.actual_model.as_deref())))
        };
        let workspace_root = {
            let state = self.state.read(cx);
            self.chat_id
                .as_deref()
                .and_then(|chat_id| state.chats.iter().find(|chat| chat.id == chat_id))
                .or_else(|| state.selected_chat_row())
                .and_then(|chat| chat.cwd.as_deref())
                .map(SharedString::from)
        };
        // The viewport spans the full window (under the titlebar): the first
        // row's gap adds the titlebar's height so a top-scrolled transcript
        // rests below the chrome it fades under. The right pane already pads
        // for the titlebar — an override instance's first row keeps only the
        // ordinary turn gap, or the content sits double-chrome low.
        // (`top_inset` defaults to the titlebar inset; the overview's chat
        // panel sets a small one — its header doesn't overlay the list.)
        let top_gap = if ix == 0 {
            Self::first_row_top_gap(self.doc_override.is_some(), self.top_inset)
        } else {
            top_gap_for(ix.checked_sub(1).and_then(|i| self.rows.get(i)), &row)
        };
        // The last row must clear the composer/status stack the transcript
        // scrolls under PLUS the fade band above it, or the timestamp strip
        // (the row's lowest content) renders half-faded (or hidden) when the
        // transcript is pinned to the bottom.
        let is_last = ix + 1 == self.rows.len();
        let bottom_pad = if is_last {
            self.bottom_clearance + Theme::TRANSCRIPT_FADE_BAND + 8.0
        } else {
            0.0
        };
        // Live-run loader rides under the LAST row's content (above its
        // clearance pad), so it sits right beneath the working reply.
        let trailer = (ix + 1 == self.rows.len())
            .then(|| self.render_working_trailer(cx))
            .flatten();

        let inner: AnyElement = match &row.kind {
            RowKind::User {
                source,
                text,
                mentions,
                attachments,
                badges,
                pending,
            } => {
                let attachments = attachments.clone();
                let badges = badges.clone();
                let text = text.clone();
                let mentions = mentions.clone();
                let pending = *pending;
                let compact = !matches!(source, crate::message_source::MessageSource::Human);
                // Attachment thumbnails ride ABOVE the bubble, right-aligned
                // (chat-view.tsx RowView: UserAttachmentStrip then the text
                // HStack); image-only sends show no bubble at all.
                let mut column = div().w_full().flex().flex_col();
                if !attachments.is_empty() {
                    column = column.child(self.render_user_attachments(
                        &row.id,
                        &attachments,
                        window,
                        cx,
                    ));
                }
                if !badges.is_empty() {
                    column = column.child(
                        div()
                            .w_full()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .justify_end()
                            .items_center()
                            .gap(px(6.0))
                            .pb(px(6.0))
                            .children(badges.iter().enumerate().map(|(bix, badge)| {
                                crate::badges::render(
                                    SharedString::from(format!("{}#badge{bix}", row.id)),
                                    badge,
                                    &theme,
                                )
                            })),
                    );
                }
                if compact {
                    let (label, chat_id) = match source {
                        crate::message_source::MessageSource::OtherChat { chat_id, title } => (format!("From {title}"), Some(chat_id.clone())),
                        crate::message_source::MessageSource::Admin { chat_id, title } => (format!("Admin chat · {title}"), Some(chat_id.clone())),
                        crate::message_source::MessageSource::HandOff { chat_id } => {
                            let title = self.state.read(cx).chats.iter().find(|chat| chat.id == *chat_id)
                                .and_then(|chat| chat.title.clone()).unwrap_or_else(|| "new chat".into());
                            (format!("Handed off to {title}"), Some(chat_id.clone()))
                        },
                        crate::message_source::MessageSource::System { label } => (format!("{label} · Mast"), None),
                        crate::message_source::MessageSource::Human => unreachable!(),
                    };
                    let resolved = chat_id.and_then(|prefix| {
                        let matches: Vec<_> = self.state.read(cx).chats.iter().filter(|chat| chat.id.starts_with(&prefix)).map(|chat| chat.id.clone()).collect();
                        (matches.len() == 1).then(|| matches[0].clone())
                    });
                    let label = if let Some(at) = row.timestamp.and_then(chrono::DateTime::from_timestamp_millis) {
                        format!("{label} · {}", crate::state::format_time_ago(at, chrono::Utc::now()))
                    } else { label };
                    let mut header = div().id(SharedString::from(format!("{}-source", row.id))).debug_selector({ let selector = format!("{}-source", row.id); move || selector.clone() }).flex().items_center().gap(px(5.0)).text_size(crate::typography::ui_rems(12.0)).text_color(theme.text_muted)
                        .child(crate::icons::icon(crate::icons::CHAT_ROUND_LINE).size(px(12.0)))
                        .child(SharedString::from(label));
                    if let Some(chat_id) = resolved {
                        header = header.cursor_pointer().on_click(cx.listener(move |_, _, _, cx| cx.emit(TranscriptEvent::OpenChat(chat_id.clone()))));
                    }
                    column = column.child(div().w_full().border_l_1().border_color(theme.accent).pl(px(10.0)).text_size(crate::typography::ui_rems(13.0)).line_height(crate::typography::ui_rems(USER_LINE_HEIGHT)).text_color(theme.text_muted)
                        .child(header)
                        .child(self.render_user_body(&row.id, ix, text, mentions, true, &theme, window, cx)));
                } else if !text.is_empty() {
                    // `min_w_0` is load-bearing: gpui text answers min/max-content
                    // probes with its UNWRAPPED width, so without it the bubble's
                    // automatic min-size is the full single-line width — the flex
                    // item can't shrink, `justify_end` pushes the overflow off the
                    // left edge, and long prompts render as one clipped line
                    // instead of wrapping inside the 80% column cap.
                    let bubble_theme = crate::components::user_message_theme(&theme, identity);
                    let content = self.render_user_body(&row.id, ix, text, mentions, false, &bubble_theme, window, cx);
                    let bubble = crate::components::conversation_user_bubble(
                        &theme, identity, self.content_width, USER_LINE_HEIGHT, pending, content,
                    );
                    column = column.child(div().w_full().flex().justify_end().child(bubble));
                }
                column.into_any_element()
            }
            RowKind::Markdown { tree, block_ix } => {
                let theme = crate::components::agent_message_theme(&theme, identity);
                let Some(top) = tree.blocks.get(*block_ix) else {
                    return gpui::Empty.into_any_element();
                };
                let code = self.code_uis_for(&row.id, &top.block, *block_ix, cx);
                let opts = RenderOptions {
                    tasks: None,
                    media: None,
                    row_key: row.id.clone(),
                    veil: None,
                    cache: (!render_cache_disabled()).then(|| self.render_cache.clone()),
                    now: Instant::now(),
                    copy: Some(self.copy_ui_for(&row.id, cx)),
                    link: self.link_ui(),
                    workspace_root: workspace_root.clone(),
                    code,
                    find: None,
                    question: None,
                };
                let highlight = self.code_highlight_for(&row.id, tree, Some(*block_ix), cx);
                
                render::render_block(
                    &top.block,
                    *block_ix,
                    *block_ix,
                    &opts,
                    &theme,
                    window,
                    highlight
                        .get(block_ix)
                        .and_then(|o| o.as_deref())
                        .map(|document| document.lines.as_slice()),
                )
            }
            RowKind::LiveMarkdown { tree, block_ix } => {
                let theme = crate::components::agent_message_theme(&theme, identity);
                let Some(top) = tree.blocks.get(*block_ix) else {
                    return gpui::Empty.into_any_element();
                };
                let code = self.code_uis_for(&row.id, &top.block, *block_ix, cx);
                // Per-appended-chunk fade veil (opacity only — layout commits
                // instantly). Reduced motion renders with no veil at all.
                // Baseline rows (text already streamed when the transcript
                // attached) start seeded: the existing reply must not fade in
                // on a session switch — only fresh appends animate.
                let seed_history = !self.veils.contains_key(&row.id)
                    && self.historical_markdown.contains_key(&row.id);
                let veil = (!motion::reduced_motion(cx)).then(|| {
                    self.veils
                        .entry(row.id.clone())
                        .or_insert_with(|| {
                            if seed_history || self.veil_baseline.contains(&row.id) {
                                Rc::new(RefCell::new(RowVeil::seeded()))
                            } else {
                                Rc::default()
                            }
                        })
                        .clone()
                });
                let opts = RenderOptions {
                    tasks: None,
                    media: None,
                    row_key: row.id.clone(),
                    veil: veil.clone(),
                    cache: (!render_cache_disabled()).then(|| self.render_cache.clone()),
                    now: Instant::now(),
                    copy: Some(self.copy_ui_for(&row.id, cx)),
                    link: self.link_ui(),
                    workspace_root: workspace_root.clone(),
                    code,
                    find: None,
                    question: None,
                };
                if seed_history && let Some(veil) = &veil {
                    let historical = &self.historical_markdown[&row.id];
                    if let RowKind::LiveMarkdown { tree, block_ix } = &historical.kind
                        && let Some(top) = tree.blocks.get(*block_ix)
                    {
                        // Use the renderer's own nested element keys and text
                        // flattening, but never cache/paint the historical tree.
                        let seed_opts = RenderOptions {
                            cache: None,
                            link: None,
                            ..opts.clone()
                        };
                        let _ = render::render_block(
                            &top.block, *block_ix, *block_ix, &seed_opts, &theme, window, None,
                        );
                        veil.borrow_mut().finish_seeding();
                    }
                }
                let highlight = self.code_highlight_for(&row.id, tree, Some(*block_ix), cx);
                let timer = frame_stats_enabled().then(Instant::now);
                let el = render::render_block(
                    &top.block,
                    *block_ix,
                    *block_ix,
                    &opts,
                    &theme,
                    window,
                    highlight
                        .get(block_ix)
                        .and_then(|o| o.as_deref())
                        .map(|document| document.lines.as_slice()),
                );
                if let Some(start) = timer {
                    record_live_frame_us(start.elapsed().as_micros() as u64);
                }
                // The attach pass for this row is done (every element rendered
                // above seeded its baseline synchronously): elements appearing
                // from the NEXT pass on are newly streamed and fade normally.
                if let Some(veil) = &veil {
                    veil.borrow_mut().finish_seeding();
                }
                // Share the loaders' bounded clock. A display-frame callback
                // here would pin the transcript to 60/120Hz for the whole
                // stream, bypassing the clock even with no loader mounted.
                if veil.is_some_and(|v| v.borrow().is_fading()) {
                    motion::pulse_lease(cx.entity_id(), cx);
                }
                el
            }
            RowKind::ToolGroup {
                tools,
                auto_open,
                summary,
            } => {
                let panel_theme = crate::components::conversation_tool_theme(&theme);
                let body = self.render_tool_group(&row.id, tools, summary, *auto_open, &panel_theme, cx);
                crate::components::conversation_tool_panel(&panel_theme, identity, body)
            },
            RowKind::InputChip { header, resolved } => {
                crate::components::conversation_question_chip(header.clone(), *resolved, &theme)
            }
            RowKind::GeneratedImage {
                owner,
                path,
                name,
                mime_type,
            } => self.render_generated_image(&row.id, owner, path, name, mime_type, cx),
            RowKind::Visualization { artifact } => {
                // A visualization registered as an artifact opens as one.
                let registered = artifact.as_ref().and_then(|artifact| {
                    self.state
                        .read(cx)
                        .artifacts()
                        .artifact_by_path(&artifact.path, workspace_root.as_deref(), None)
                        .map(|registered| registered.id.clone())
                });
                visualization_card(
                    &row.id,
                    artifact.as_ref(),
                    workspace_root.as_deref(),
                    registered,
                    self.link_ui(),
                    &theme,
                )
            }
            RowKind::ArtifactCard { label, target } => {
                let card = crate::artifacts::cards::card_model(
                    label,
                    target,
                    self.state.read(cx).artifacts(),
                    workspace_root.as_deref(),
                );
                let thumbnail = card.preview.as_ref().and_then(|artifact| crate::artifacts::preview_thumb::thumbnail(artifact, self.state.clone(), &theme, window, cx));
                let footer = card.preview.as_ref().map(|_| crate::artifacts::preview_thumb::card_footer(&row.id, &theme, crate::settings::current(cx).open_diagrams_automatically, cx));
                crate::artifacts::cards::artifact_card(&row.id, card, self.link_ui(), &theme, thumbnail, footer)
            }
            RowKind::ErrorChip { message } => {
                let chat = self.chat_id.clone().filter(|_| self.doc_override.is_none());
                let action = chat.clone().filter(|chat_id| {
                    mast_harness::is_authentication_failure(message)
                        && self.state.read(cx).chats.iter().find(|chat| chat.id == *chat_id)
                            .and_then(|chat| chat.config.as_ref())
                            .is_some_and(|config| crate::managed_accounts::supports_accounts(config.harness))
                });
                let retry = chat.filter(|chat_id| {
                    !mast_harness::is_authentication_failure(message)
                        && retryable_error_message(message)
                        && !self.rows[ix + 1..].iter().any(|row| matches!(row.kind, RowKind::User { .. } | RowKind::ErrorChip { .. }))
                        && self.state.read(cx).sessions.iter().any(|session| session.chat_id == *chat_id && session.status == mast_proto::SessionStatus::Errored)
                });
                let error_entry_id = row.entry_id.to_string();
                div().child(crate::components::conversation_error_chip(&theme, message.clone()))
                    .when_some(action, |el, chat_id| el.child(
                        div().id(SharedString::from(format!("{}-sign-in-again", row.id)))
                            .debug_selector({ let selector = format!("{}-sign-in-again", row.id); move || selector.clone() })
                            .cursor_pointer().text_color(theme.accent)
                            .child("Sign in again…")
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(TranscriptEvent::SignInAgain(chat_id.clone()))))
                    )).when_some(retry, |el, chat_id| el.child(
                        div().id(SharedString::from(format!("{}-retry", row.id)))
                            .debug_selector({ let selector = format!("{}-retry", row.id); move || selector.clone() })
                            .cursor_pointer().text_color(theme.accent)
                            .child("Retry")
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(TranscriptEvent::RetryErroredTurn { chat_id: chat_id.clone(), error_entry_id: error_entry_id.clone() })))
                    )).into_any_element()
            },
        };

        let is_first_entry_row = ix == 0 || self.rows.get(ix - 1).is_some_and(|previous| previous.entry_id != row.entry_id);
        let is_markdown = |candidate: &Row| matches!(candidate.kind, RowKind::Markdown { .. } | RowKind::LiveMarkdown { .. });
        let adjacent = |candidate: &Row| is_markdown(candidate) && candidate.entry_id == row.entry_id;
        let continues = is_markdown(&row) && ix.checked_sub(1).and_then(|i| self.rows.get(i)).is_some_and(adjacent);
        let continued_by_next = is_markdown(&row) && self.rows.get(ix + 1).is_some_and(adjacent);
        let inner = if matches!(row.kind, RowKind::Markdown { .. } | RowKind::LiveMarkdown { .. }) {
            let model = self.state.read(cx).selected_chat_row().and_then(|chat| chat.config.as_ref()).and_then(|config| config.model.clone());
            let first_block = matches!(&row.kind, RowKind::Markdown { block_ix, .. } | RowKind::LiveMarkdown { block_ix, .. } if *block_ix == 0);
            crate::components::agent_message_bubble(&theme, identity, is_first_entry_row, first_block, continues, continued_by_next, top_gap, model, inner)
        } else { inner };

        // Hover-revealed metadata strip: a RESERVED 32px lane under the
        // entry's last row. Timestamp, copy action, and copied feedback only
        // flip visibility/content, so none of them shifts the virtualizer.
        // User entries align end (under the bubble), assistant entries start.
        // Both read timestamp first, then the copy action.
        let is_user_row = matches!(row.kind, RowKind::User { .. });
        let hovered = self
            .hovered_entry
            .as_ref()
            .is_some_and(|(_, entry)| entry == &row.entry_id);
        let copied_message = self.copied_message.as_ref() == Some(&row.entry_id);
        let copy_text = row.copy_text.clone();
        let copy_entry_id = row.entry_id.clone();
        let strip = row.timestamp.map(|ms| {
            let timestamp = div()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted.opacity(0.55))
                .child(SharedString::from(format_timestamp(ms, &chrono::Local)));
            let copy = copy_text.map(|text| {
                let entry_id = copy_entry_id.clone();
                crate::components::conversation_message_copy_button(&theme, &entry_id, copied_message)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy_message(entry_id.clone(), text.clone(), cx)
                    }))
            });
            let metadata = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(Theme::SPACE_SM));
            let metadata = metadata.child(timestamp).children(copy);
            div()
                .h(px(Theme::SPACE_SM + Theme::SPACE_MD * 2.0))
                .pt(px(Theme::SPACE_SM))
                .w_full()
                .flex()
                .items_center()
                // No horizontal inset: the original's `px-1` netted out flush
                // because its message text was inset by the same amount (group
                // padding 4 + inner VStack 4 = 8 = group 4 + px-1 4). Here the
                // markdown text / user bubble sit AT the content column edges,
                // so the label must too — assistant label's left edge on the
                // text's first-character x, user label's right edge on the
                // bubble's right edge (user-reported 4px drift).
                .when(is_user_row, |el| el.justify_end())
                .when(hovered, |el| {
                    el.child(motion::fade_quick(
                        SharedString::from(format!("meta-{}", row.id)),
                        metadata,
                    ))
                })
        });
        let entry_id = row.entry_id.clone();
        let row_id = row.id.clone();
        let a11y_active = window.is_a11y_active();
        let row_kind = &row.kind;
        div()
            .id(row.id.clone())
            .map(|el| with_accessible_text(el, row_kind, a11y_active))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    let next = Some((row_id.clone(), entry_id.clone()));
                    if this.hovered_entry != next {
                        let entry_changed = this
                            .hovered_entry
                            .as_ref()
                            .is_none_or(|(_, entry)| entry != &entry_id);
                        this.hovered_entry = next;
                        if entry_changed {
                            cx.notify();
                        }
                    }
                } else if this
                    .hovered_entry
                    .as_ref()
                    .is_some_and(|(row, _)| row == &row_id)
                {
                    // Only the row that OWNS the current reveal may clear it —
                    // a stale leave from an earlier row must not blank the
                    // strip the newly entered row just lit.
                    this.hovered_entry = None;
                    cx.notify();
                }
            }))
            .w_full()
            .flex()
            .justify_center()
            .pt(px(crate::components::agent_message_row_gap(&theme, continues, top_gap)))
            .pb(px(bottom_pad))
            // Keep side gutters as the configurable column shrinks to fit.
            .px(px(48.0))
            .child(
                crate::components::conversation_column(&theme, self.content_width)
                    .child(inner)
                    .children(strip)
                    .children(trailer),
            )
            .into_any_element()
    }

}
