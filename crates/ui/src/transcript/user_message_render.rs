use super::*;

impl Transcript {
    pub(super) fn render_user_body(
        &mut self,
        row_id: &SharedString,
        row_ix: usize,
        text: SharedString,
        mentions: Arc<Vec<crate::composer::SentMentionSpan>>,
        compact: bool,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.resolve_code_refs(&mentions, cx);
        let (text, mentions) = self.project_code_ref_resolutions(&text, &mentions);
        let fold = self.user_folds.get(row_id).copied().unwrap_or_default();
        let expanded = fold.open.unwrap_or(false);
        let line_height =
            f32::from(crate::typography::ui_rems(USER_LINE_HEIGHT).to_pixels(window.rem_size()));
        let collapsed_lines = if compact { 3 } else { USER_COLLAPSED_LINES };
        let collapsed_text_h = collapsed_lines as f32 * line_height;
        // Include the continuation line in the resize endpoints so removing
        // it on expansion does not make the bubble jump by a line.
        let collapsed_h = collapsed_text_h + line_height;
        let measured_h = self
            .user_heights
            .entry(row_id.clone())
            .or_insert_with(|| Rc::new(Cell::new(0.0)))
            .clone();
        let measured = measured_h.get();
        let collapsible = text.lines().count() > collapsed_lines
            || (measured > 0.0 && measured > collapsed_text_h + 0.5)
            || (measured == 0.0 && user_message_needs_collapse(&text));
        let full_h = measured_h.get().max(collapsed_h);
        if let Some(fold) = self.user_folds.get_mut(row_id) {
            // Wrapping can change with the window width while expanded.
            fold.user_expansion_height = (full_h - collapsed_h).max(0.0);
        }

        let hold_key = row_id.clone();
        let hold_height = measured_h.clone();
        let hold_selection: Arc<str> = format!("{row_id}:u").into();
        let focus_targets = self.reference_focus_targets(row_id, &mentions, window, cx);
        let body = div()
            .id(SharedString::from(format!("{row_id}-body")))
            // A long press toggles instead of double-click. A normal release
            // remains available for text selection, and pointer movement
            // cancels the pending toggle before a drag can select text.
            .when(collapsible, |el| {
                let down_key = hold_key.clone();
                let down_height = hold_height.clone();
                let down_selection = hold_selection.clone();
                el.on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.arm_user_hold(
                            down_key.clone(),
                            row_ix,
                            collapsed_h,
                            down_height.clone(),
                            down_selection.clone(),
                            cx,
                        );
                    }),
                )
                .on_mouse_up(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, _| {
                        this.cancel_user_hold();
                    }),
                )
                .on_mouse_up_out(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, _| {
                        this.cancel_user_hold();
                    }),
                )
                .on_mouse_move(cx.listener(|this, _, _, _| {
                    this.cancel_user_hold();
                }))
            })
            .child(user_bubble_text(
                row_id,
                text,
                mentions,
                theme,
                compact,
                measured_h.clone(),
                cx.entity_id(),
                cx.weak_entity(),
                self.code_ref_hover.clone(),
                focus_targets,
                self.link_ui(),
                self.state.clone(),
                self.chat_id.clone(),
            ));
        // Height motion uses the same ease-out curve as sidebars, tool folds,
        // and pane transitions, with duration scaled to travel distance. The
        // full text remains laid out behind the clip; only the viewport over it
        // changes, so glyph wrapping never shifts.
        let duration_ms = fold
            .duration_ms
            .max(user_resize_duration_ms(full_h - collapsed_h));
        let animating = collapsible
            && fold.epoch > 0
            && fold
                .toggled_at
                .is_some_and(|at| at.elapsed() < Duration::from_millis(duration_ms + 200))
            && !motion::reduced_motion(cx);
        let ellipsis = || div().h(px(line_height)).child("...");
        let body: AnyElement = if animating {
            let from = fold.from;
            let to = if expanded { full_h } else { collapsed_h };
            let resize = user_resize_spec(full_h - collapsed_h);
            let ellipsis_h = if expanded { 0.0 } else { line_height };
            div()
                .child(div().overflow_hidden().child(body).with_animation(
                    SharedString::from(format!("{row_id}-user-resize-{}", fold.epoch)),
                    resize.animation(),
                    move |el, t| el.h(px((motion::lerp(from, to, t) - ellipsis_h).max(0.0))),
                ))
                .when(!expanded, |el| el.child(ellipsis()))
                .into_any_element()
        } else if collapsible && !expanded {
            div()
                .child(div().h(px(collapsed_text_h)).overflow_hidden().child(body))
                .child(ellipsis())
                .into_any_element()
        } else {
            body.into_any_element()
        };
        div()
            .relative()
            .child(body)
            .when(collapsible, |el| {
                el.child(self.render_user_expander(
                    row_id,
                    row_ix,
                    expanded,
                    collapsed_h,
                    measured_h,
                    theme,
                    cx,
                ))
            })
            .into_any_element()
    }

    /// A plain text link aligned with the message's left edge, following the
    /// continuation ellipsis when collapsed. No pill, border, or button wash.
    pub(super) fn render_user_expander(
        &mut self,
        row_id: &SharedString,
        row_ix: usize,
        expanded: bool,
        collapsed_h: f32,
        measured_h: Rc<Cell<f32>>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let toggle_key = row_id.clone();
        let button = crate::components::conversation_message_expander(theme, row_id, expanded, USER_LINE_HEIGHT)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_user_fold(
                    toggle_key.clone(),
                    row_ix,
                    collapsed_h,
                    measured_h.get().max(collapsed_h),
                    motion::reduced_motion(cx),
                );
                cx.notify();
            }));
        div()
            .mt(px(USER_TOGGLE_GAP))
            .flex()
            .items_start()
            .child(button)
            .into_any_element()
    }

    pub(super) fn render_generated_image(
        &mut self,
        row_id: &SharedString,
        owner: &str,
        path: &str,
        name: &str,
        mime_type: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::attachments::AttachmentSnapshot;
        let devices = self.generated_attachment_device_ids(owner, cx);
        let state = self.attachment_state(&devices, path, Some(mime_type), cx);
        let theme = Theme::of(cx).clone();
        let frame = div()
            .id(SharedString::from(format!("{row_id}-generated")))
            .w(px(512.0))
            .max_w_full()
            .h(px(320.0))
            .max_h(px(420.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(12.0))
            .overflow_hidden()
            .bg(crate::theme::ink(0.045));
        match state {
            AttachmentSnapshot::Loaded(loaded) => {
                let dimensions =
                    crate::appshots::png_dimensions(&loaded.image.bytes).unwrap_or((512, 320));
                let scale = (512.0 / dimensions.0 as f32)
                    .min(420.0 / dimensions.1 as f32)
                    .min(1.0);
                let preview =
                    crate::attachments::PreviewImage::new(name.to_owned(), loaded.image.clone());
                frame
                    .w(px(dimensions.0 as f32 * scale))
                    .h(px(dimensions.1 as f32 * scale))
                    .role(gpui::Role::Button)
                    .aria_label("Preview generated image")
                    .tab_index(0)
                    .cursor_pointer()
                    .focus_visible(move |style| style.border_2().border_color(theme.accent))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.attachment_preview_return_focus = window.focused(cx);
                        preview.viewer.reset();
                        this.attachment_preview = Some(preview.clone());
                        window.focus(&this.attachment_preview_focus, cx);
                        cx.notify();
                    }))
                    .child(
                        gpui::img(loaded.image)
                            .size_full()
                            // The frame's overflow clip is rectangular; round the image itself.
                            .rounded(px(12.0))
                            .object_fit(gpui::ObjectFit::Contain),
                    )
                    .into_any_element()
            }
            AttachmentSnapshot::Loading => frame
                .text_color(theme.text_muted)
                .child("Loading generated image…")
                .into_any_element(),
            AttachmentSnapshot::Error { .. } => frame
                .text_color(theme.text_muted)
                .child("Generated image unavailable")
                .into_any_element(),
        }
    }

    /// The right-aligned thumbnail strip above a user bubble.
    pub(super) fn render_user_attachments(
        &mut self,
        row_id: &SharedString,
        atts: &[crate::attachments::UserImageAttachment],
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::attachments::AttachmentSnapshot;
        let glyph = Theme::of(cx).glyph;
        let device_ids = self.attachment_device_ids(cx);
        let mut strip = div()
            .w_full()
            .min_w_0()
            .flex_none()
            .flex()
            .flex_row()
            .flex_wrap()
            .justify_end()
            .items_start()
            .gap(px(8.0))
            .px(px(4.0))
            .pt(px(4.0))
            .pb(px(6.0));
        for (aix, att) in atts.iter().enumerate() {
            let state = self.attachment_state(&device_ids, &att.path, None, cx);
            // The in-flight send's progress belongs ON the thumbnail
            // (2026-08-18 user request). Two ref shapes mean "still
            // crossing": the queued flow's `pending://` (bytes ship
            // engine-side after the send; the host rewrites the ref to an
            // absolute path once they land and the run starts) and the
            // legacy echo's synthetic `pending/`. Percent sources, in order:
            // this attachment's own relay transfer (`WatchTransfers`, by the
            // uploadId its ref names — the leg that actually takes time),
            // else the send-wide staging/legacy upload percent. Neither → the
            // indeterminate spinner (staged-but-waiting, retry backoff, or
            // committed-awaiting-rewrite), so the ring never shows a number
            // that isn't a real transfer position (2026-08-20 report: the
            // staging-only percent blinked out in ~100ms and lied about the
            // slow part).
            let sending = att.path.starts_with("pending://") || att.path.starts_with("pending/");
            let upload_id = att
                .path
                .strip_prefix("pending://")
                .and_then(|rest| rest.split_once('/'))
                .map(|(id, _)| id);
            let uploading = upload_id
                .and_then(|id| self.state.read(cx).transfer_percent(id))
                .or_else(|| {
                    sending
                        .then(|| self.state.read(cx).upload_progress_percent())
                        .flatten()
                });
            if let Some(appshot) = &att.appshot {
                let has_image = matches!(&state, AttachmentSnapshot::Loaded(_));
                let theme = Theme::of(cx).clone();
                let accent = theme.accent;
                let width = 240.0;
                let mut card = div()
                    .id(SharedString::from(format!("{row_id}-appshot-{aix}")))
                    .w(px(width))
                    .max_w_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_center()
                    .rounded(px(14.0))
                    .p(px(8.0))
                    .gap(px(6.0))
                    .hover(|style| style.bg(crate::theme::ink(0.045)));
                let image_frame = div()
                    .w_full()
                    .h(px(128.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .overflow_hidden();
                card = match state {
                    AttachmentSnapshot::Loaded(image) => {
                        let preview =
                            crate::attachments::PreviewImage::new(image.name, image.image.clone());
                        card.role(gpui::Role::Button)
                            .aria_label(format!(
                                "Preview {} Appshot: {}",
                                appshot.app_name,
                                appshot.title()
                            ))
                            .tab_index(0)
                            .cursor_pointer()
                            .focus_visible(move |style| style.border_2().border_color(accent))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.attachment_preview_return_focus = window.focused(cx);
                                preview.viewer.reset();
                                this.attachment_preview = Some(preview.clone());
                                window.focus(&this.attachment_preview_focus, cx);
                                cx.notify();
                            }))
                            .child(
                                // Inherit the transcript's scroll fade. GPUI replaces
                                // rather than composes nested edge-fade scopes, so a
                                // decorative thumbnail fade would bypass the chrome fade.
                                image_frame.child(
                                    img(image.image)
                                        .w_full()
                                        .h(px(126.0))
                                        .rounded(px(5.0))
                                        .object_fit(ObjectFit::Contain),
                                ),
                            )
                    }
                    AttachmentSnapshot::Loading => card.child(
                        image_frame.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.text_muted)
                                .child(if sending {
                                    "Uploading Appshot…"
                                } else {
                                    "Loading Appshot…"
                                }),
                        ),
                    ),
                    AttachmentSnapshot::Error { .. } => card.child(
                        image_frame.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.text_muted)
                                .child(if sending {
                                    "Uploading Appshot…"
                                } else {
                                    "Appshot unavailable"
                                }),
                        ),
                    ),
                };
                card = card
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .max_w_full()
                            .child(
                                div()
                                    .size(px(24.0))
                                    .flex_none()
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(match crate::appshots::presentation_icon(appshot) {
                                        Some(icon) => img(icon)
                                            .size(px(24.0))
                                            .object_fit(ObjectFit::Contain)
                                            .into_any_element(),
                                        None => crate::icons::icon(crate::icons::MONITOR)
                                            .size(px(15.0))
                                            .text_color(theme.text_muted)
                                            .into_any_element(),
                                    }),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(format!(
                                        "{} · Appshot",
                                        appshot.app_name
                                    ))),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_center()
                            .text_size(px(12.0))
                            .text_color(theme.text)
                            .child(SharedString::from(appshot.title().to_string())),
                    );
                if sending && (has_image || uploading.is_some()) {
                    card = card.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(
                                uploading
                                    .map(|pct| format!("Uploading {pct}%"))
                                    .unwrap_or_else(|| "Uploading…".into()),
                            )),
                    );
                }
                strip = strip.child(card);
                continue;
            }
            let frame = div()
                .flex_none()
                .w(px(ATT_THUMB_W))
                .h(px(ATT_THUMB_H))
                .rounded(px(8.0))
                .overflow_hidden();
            let thumb: AnyElement = match state {
                AttachmentSnapshot::Loaded(image) => {
                    let preview = crate::attachments::PreviewImage::new(
                        image.name.clone(),
                        image.image.clone(),
                    );
                    frame
                        .id(SharedString::from(format!("{row_id}#att{aix}")))
                        .relative()
                        .border_1()
                        .border_color(crate::theme::hairline(0.11))
                        .bg(crate::theme::ink(0.035))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.attachment_preview_return_focus = window.focused(cx);
                            preview.viewer.reset();
                            this.attachment_preview = Some(preview.clone());
                            window.focus(&this.attachment_preview_focus, cx);
                            cx.notify();
                        }))
                        .child(
                            img(image.image.clone())
                                // EXPLICIT dims, not size_full: img layout
                                // honors the intrinsic aspect ratio over a
                                // percent height (gpui f8d8a90 repoint), so
                                // size_full let a tall photo grow past the
                                // frame and the rectangular overflow clip
                                // squared the bottom corners (2026-08-19).
                                .w(px(ATT_THUMB_W - 2.0))
                                .h(px(ATT_THUMB_H - 2.0))
                                // The IMG needs its own radii: the frame's
                                // rounding only clips rectangularly, so the
                                // sprite must round its own corners (7 = the
                                // frame's 8 minus its 1px border).
                                .rounded(px(7.0))
                                .object_fit(ObjectFit::Cover),
                        )
                        .when(sending, |el| {
                            // The pulse read registers this entity for frames,
                            // so the overlay stays live even once the trailer's
                            // 30s pending-send bridge has lapsed.
                            let pulse = motion::pulse_wave(motion::pulse_delta(
                                &motion::MAST_PULSE,
                                cx.entity_id(),
                                cx,
                            ));
                            let indicator: AnyElement = match uploading {
                                Some(pct) => crate::loaders::upload_progress_ring(pct, 34.0),
                                None => crate::loaders::mini_glyph_spinner(
                                    format!("att-sending-{row_id}-{aix}"),
                                    3.0,
                                    glyph,
                                    cx.entity_id(),
                                    cx,
                                )
                                .into_any_element(),
                            };
                            el.child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .rounded(px(7.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(gpui::hsla(0.0, 0.0, 0.0, 0.38 + 0.05 * pulse))
                                    .child(indicator),
                            )
                        })
                        .into_any_element()
                }
                // Errored/unavailable: the dashed "missing" thumb.
                AttachmentSnapshot::Error { .. } => frame
                    .border_1()
                    .border_dashed()
                    .border_color(crate::theme::hairline(0.14))
                    .bg(crate::theme::ink(0.025))
                    .into_any_element(),
                // Loading: the pulsing skeleton (same wash as popover skeletons).
                AttachmentSnapshot::Loading => frame
                    .border_1()
                    .border_color(crate::theme::hairline(0.08))
                    .bg(crate::theme::ink(0.055))
                    .opacity(
                        0.35 + 0.4
                            * motion::pulse_wave(motion::pulse_delta(
                                &motion::MAST_PULSE,
                                cx.entity_id(),
                                cx,
                            )),
                    )
                    .into_any_element(),
            };
            strip = strip.child(thumb);
        }
        strip.into_any_element()
    }

    // ---- rendering ----

}
