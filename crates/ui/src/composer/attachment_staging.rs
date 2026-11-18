use super::*;

impl Composer {
    pub(super) fn render_skill_chips(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let skills = self.staged_skills();
        if skills.is_empty() {
            return None;
        }
        let mut row = div()
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .overflow_hidden()
            .px(px(STRIP_PAD_X))
            .pt(px(STRIP_PAD_TOP));
        for (ix, name) in skills.iter().enumerate() {
            let remove = name.clone();
            let label: SharedString = format!("/{name}").into();
            row = row.child(
                crate::components::conversation_skill_chip(theme, ix)
                    .tooltip(|_, cx| {
                        cx.new(|_| {
                            crate::workspace_chip::WorkspaceChipTooltip(
                                "Attached skill — sent as a \"load these skills\" line \
                                 before your message. Skills stay loaded for the rest \
                                 of the chat."
                                    .into(),
                            )
                        })
                        .into()
                    })
                    .tooltip_show_delay(Duration::from_millis(350))
                    .child(
                        crate::icons::icon(crate::icons::COMMAND)
                            .size(px(12.0))
                            .text_color(theme.text_muted.opacity(0.7)),
                    )
                    .child(label)
                    .child(
                        crate::components::conversation_skill_remove(theme, ix)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove_skill(&remove, cx);
                            })),
                    ),
            );
        }
        Some(row)
    }

    pub fn stage_appshot(&mut self, appshot: CapturedAppshot, cx: &mut Context<Self>) {
        self.stage_appshot_for(self.current_key.clone(), appshot, cx);
    }

    pub fn stage_appshot_for(
        &mut self,
        key: String,
        appshot: CapturedAppshot,
        cx: &mut Context<Self>,
    ) -> bool {
        let staged_bytes = self
            .appshots
            .get(&key)
            .into_iter()
            .flatten()
            .map(|shot| shot.screenshot.bytes().len() as u64)
            .sum::<u64>();
        let saved_bytes = self
            .queue_edit_draft
            .as_ref()
            .filter(|_| key == self.current_key)
            .map(|(_, _, shots)| {
                shots
                    .iter()
                    .map(|shot| shot.screenshot.bytes().len() as u64)
                    .sum::<u64>()
            })
            .unwrap_or_default();
        let staged_bytes = staged_bytes.saturating_add(saved_bytes);
        let incoming = appshot.screenshot.bytes().len() as u64;
        if incoming > attachments::MAX_ATTACHMENT_BYTES
            || staged_bytes.saturating_add(incoming) > appshots::MAX_STAGED_APPSHOT_BYTES
        {
            self.failure = Some(
                "Remove an Appshot before adding another (96 MB staged Appshot limit).".into(),
            );
            self.failure_key = Some(key);
            cx.notify();
            return false;
        }
        self.appshot_entrances
            .retain(|_, start| start.elapsed().as_secs_f32() < motion::speed_scale());
        if !motion::reduced_motion(cx) {
            self.appshot_entrances
                .insert(appshot.id.clone(), Instant::now());
        }
        // A capture completing while a queue edit is being saved belongs to
        // the displaced draft; it must not be lost or change the in-flight edit.
        if self.queue_edit_finishing && key == self.current_key {
            if let Some((_, _, saved_appshots)) = &mut self.queue_edit_draft {
                saved_appshots.push(appshot);
            } else {
                self.appshots.entry(key).or_default().push(appshot);
            }
        } else {
            self.appshots.entry(key).or_default().push(appshot);
        }
        self.failure = None;
        self.failure_key = None;
        cx.notify();
        true
    }

    pub fn show_appshot_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.failure = Some(message.into());
        self.failure_key = Some(self.current_key.clone());
        cx.notify();
    }

    pub(super) fn add_staged(&mut self, staged: Vec<StagedAttachment>, cx: &mut Context<Self>) {
        if self.queue_edit_finishing {
            return;
        }
        if staged.is_empty() {
            return;
        }
        self.attachments
            .entry(self.current_key.clone())
            .or_default()
            .extend(staged);
        self.focus_pending = true;
        cx.notify();
    }

    /// Add dropped or pasted paths, staging images and mentioning other paths.
    pub(crate) fn add_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.queue_edit_finishing {
            self.failure =
                Some("Wait for the queued message to finish saving before adding files.".into());
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
            return;
        }
        let (roots, remote_device) = {
            let state = self.state.read(cx);
            let chat = state.selected_chat_row();
            let space = chat
                .and_then(|chat| state.space_for_chat(chat))
                .or_else(|| state.selected_space_row());
            let device = chat
                .map(|chat| chat.device_id.clone())
                .or_else(|| state.effective_device_id());
            let remote = device
                .as_deref()
                .filter(|id| state.local_device_id.as_deref() != Some(*id))
                .map(|id| state.device_name(id).unwrap_or(id).to_owned());
            let mut roots = Vec::new();
            if let Some(cwd) = chat.and_then(|chat| chat.cwd.as_deref()) {
                roots.push(PathBuf::from(cwd));
            }
            if let Some(space) = space {
                roots.push(PathBuf::from(&space.path));
            }
            (roots, remote)
        };
        let mut staged = Vec::new();
        let mut failures = Vec::new();
        for path in paths {
            let name = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned();
            let metadata = match std::fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    failures.push(format!("{name}: {error}"));
                    continue;
                }
            };
            if metadata.is_file() && attachments::format_by_extension(&path).is_some() {
                match attachments::stage_file(&path) {
                    Ok(attachment) => staged.push(attachment),
                    Err(message) => failures.push(message.to_string()),
                }
                continue;
            }
            if !metadata.is_file() && !metadata.is_dir() {
                failures.push(format!("{name}: not a regular file or folder"));
                continue;
            }
            let access = if metadata.is_dir() {
                std::fs::read_dir(&path).map(|_| ())
            } else {
                std::fs::File::open(&path).map(|_| ())
            };
            if let Err(error) = access {
                failures.push(format!("{name}: {error}"));
                continue;
            }
            let path = match path.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    failures.push(format!("{name}: {error}"));
                    continue;
                }
            };
            // A local path cannot identify a file on a different device, even
            // when the two devices happen to use the same directory names.
            if let Some(device) = &remote_device {
                failures.push(format!("{name} is on this Mac; the chat runs on {device}"));
                continue;
            }
            let relative = roots
                .iter()
                .filter_map(|root| root.canonicalize().ok())
                .find_map(|root| {
                    path.strip_prefix(root)
                        .ok()
                        .filter(|p| !p.as_os_str().is_empty())
                        .map(PathBuf::from)
                });
            let reference = relative.as_deref().unwrap_or(&path);
            let Some(reference) = reference.to_str() else {
                failures.push(format!("{name}: path is not valid UTF-8"));
                continue;
            };
            let inserted = self.input.update(cx, |input, cx| {
                input.insert_dropped_mention(reference, metadata.is_dir(), cx)
            });
            if inserted {
                if !metadata.is_dir() && relative.is_some() { self.stage_workspace_file(reference, cx); }
                self.reset_mention(None, cx);
                self.reset_slash(None, cx);
                self.focus_pending = true;
            } else {
                failures.push(format!("{name}: cannot insert this path in the composer"));
            }
        }
        self.add_staged(staged, cx);
        if !failures.is_empty() {
            self.failure = Some(failures.join("\n").into());
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
        }
    }

    /// Add a file-tree or file-tab drop through the existing file-mention
    /// pipeline. This keeps the reference workspace-relative and therefore
    /// valid for local and remote sessions alike.
    pub(crate) fn add_workspace_path(
        &mut self,
        path: &str,
        is_directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let inserted = self.input.update(cx, |input, cx| {
            input.insert_dropped_mention(path, is_directory, cx)
        });
        if inserted {
            if !is_directory { self.stage_workspace_file(path, cx); }
            self.reset_mention(None, cx);
            self.reset_slash(None, cx);
            let focus = self.input.read(cx).focus_handle.clone();
            window.focus(&focus, cx);
            cx.notify();
        }
    }

    pub(super) fn remove_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.queue_edit_finishing {
            return;
        }
        if let Some(list) = self.attachments.get_mut(&self.current_key) {
            list.retain(|a| a.id != id);
            if list.is_empty() {
                self.attachments.remove(&self.current_key);
            }
        }
        cx.notify();
    }

    pub(super) fn restore_failed_appshots(
        &mut self,
        sent: &[CapturedAppshot],
        failed_key: &str,
        restore_key: &str,
    ) {
        if sent.is_empty() {
            return;
        }
        let mut merged = sent.to_vec();
        for key in [failed_key, restore_key] {
            for shot in self.appshots.remove(key).unwrap_or_default() {
                if !merged.iter().any(|existing| existing.id == shot.id) {
                    merged.push(shot);
                }
            }
        }
        self.appshots.insert(restore_key.to_string(), merged);
    }

    pub(super) fn remove_appshot(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.queue_edit_finishing {
            return;
        }
        if let Some(list) = self.appshots.get_mut(&self.current_key) {
            list.retain(|appshot| appshot.id != id);
            if list.is_empty() {
                self.appshots.remove(&self.current_key);
            }
        }
        cx.notify();
    }

    /// Drop a deleted chat's per-chat composer state — staged attachments hold
    /// raw image bytes, and a deleted chat's stage could never be sent again.
    pub fn purge_chat(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        self.attachments.remove(chat_id);
        self.appshots.remove(chat_id);
        self.skills.remove(chat_id);
        self.state.update(cx, |state, _| {
            state.purge_review_comments(chat_id);
        });
    }

    /// Staged in `AppState` because the changes pane writes them.
    pub(super) fn staged_comments(&self, cx: &App) -> Vec<crate::comments::ReviewComment> {
        self.state
            .read(cx)
            .review_comments(&self.current_key)
            .to_vec()
    }

    pub(super) fn render_comments_chip(&self, theme: &Theme, cx: &App) -> Option<gpui::Div> {
        let count = self.staged_comments(cx).len();
        if count == 0 {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_row()
                .px(px(STRIP_PAD_X))
                .pt(px(STRIP_PAD_TOP))
                .child(crate::badges::render(
                    "composer-comments",
                    &crate::badges::MessageBadge {
                        icon: crate::icons::CHAT_ROUND_LINE,
                        label: crate::comments::chip_label(count).into(),
                        // The staged set is already on screen in the changes
                        // pane, so a hover card would only repeat it.
                        details: Vec::new(),
                    },
                    theme,
                )),
        )
    }

    /// The staged-thumbnail strip (attachment-ui.tsx AttachmentStrip):
    /// `flex flex-wrap gap-2 px-4 pt-3`, 56px rounded thumbs, a remove button
    /// revealed on hover, click opens the full-size preview.
    pub(super) fn render_attachment_strip(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let staged = self.staged();
        if staged.is_empty() {
            return None;
        }
        let mut strip = crate::components::conversation_attachment_strip(STRIP_GAP, STRIP_PAD_X, STRIP_PAD_TOP);
        for (ix, att) in staged.iter().enumerate() {
            let group: SharedString = format!("composer-att-{}", att.id).into();
            let preview = attachments::PreviewImage::new(att.name.clone(), att.image.clone());
            let remove_id = att.id.clone();
            strip = strip.child(
                div()
                    .group(group.clone())
                    .flex_none()
                    .relative()
                    .child(
                        crate::components::conversation_attachment_thumb(ix, STRIP_THUMB)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                preview.viewer.reset();
                                this.preview = Some(preview.clone());
                                this.preview_focus_pending = true;
                                cx.notify();
                            }))
                            .child(
                                img(att.image.clone())
                                    // EXPLICIT dims, not size_full: img layout
                                    // honors the image's intrinsic aspect
                                    // ratio over a percent height (gpui
                                    // f8d8a90 repoint), so size_full let a
                                    // tall photo grow past the frame — the
                                    // rectangular overflow clip then squared
                                    // the bottom corners (2026-08-19 report).
                                    // 56−2 = frame minus its 1px borders.
                                    .w(px(STRIP_THUMB - 2.0))
                                    .h(px(STRIP_THUMB - 2.0))
                                    // Own radii — the frame's rounding only
                                    // clips rectangularly (7 = 8 - border).
                                    .rounded(px(7.0))
                                    .object_fit(ObjectFit::Cover),
                            ),
                    )
                    // Own layer: inside the frosted pill everything shares one
                    // draw order and images render last, so without it the
                    // thumbnail paints OVER this button (user report).
                    .child(crate::frost::layered(
                        crate::components::conversation_attachment_remove(theme, ix, group)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                // The button overhangs the thumbnail, whose
                                // hitbox is right underneath — don't let the
                                // same click also open the preview.
                                cx.stop_propagation();
                                this.remove_attachment(&remove_id, cx);
                            })),
                    )),
            );
        }
        Some(strip)
    }

    pub(super) fn render_appshot_strip(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let appshots = self.staged_appshots();
        if appshots.is_empty() {
            return None;
        }
        let mut strip = crate::components::conversation_appshot_strip(STRIP_GAP, STRIP_PAD_X, STRIP_PAD_TOP);
        let max_image_width = self.last_available_width.unwrap_or(COMPOSER_MAX_WIDTH)
            - 2.0 * Theme::SPACE_LG
            - 2.0
            - 2.0 * STRIP_PAD_X
            - 2.0 * APPSHOT_IMAGE_INSET;
        for (ix, appshot) in appshots.iter().enumerate() {
            let group: SharedString = format!("composer-appshot-{}", appshot.id).into();
            let preview = crate::attachments::PreviewImage::new(
                appshot.screenshot.name.clone(),
                appshot.screenshot.image.clone(),
            );
            let preview_on_key = preview.clone();
            let preview_on_a11y = preview.clone();
            let composer_for_preview = cx.entity().downgrade();
            let remove_id = appshot.id.clone();
            let remove_on_key_id = remove_id.clone();
            let remove_on_a11y_id = remove_id.clone();
            let composer_for_remove = cx.entity().downgrade();
            let source: SharedString = appshot
                .window_title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| appshot.app_name.clone())
                .into();
            let preview_label: SharedString = format!("Preview {source}").into();
            let remove_label: SharedString = format!("Remove {source}").into();
            let preview_aria = preview_label.clone();
            let remove_aria = remove_label.clone();
            let (image_width, image_height) =
                appshot_contained_size(appshot.screenshot_dimensions, max_image_width);
            let tile_width = (image_width + 2.0 * APPSHOT_IMAGE_INSET).max(APPSHOT_TILE_MIN_WIDTH);
            let mut card = crate::components::conversation_appshot_card(ix, group.clone(), tile_width, APPSHOT_TILE_HEIGHT)
                .tooltip(move |_, cx| {
                    cx.new(|_| AppshotActionTooltip(preview_label.clone()))
                        .into()
                })
                .role(gpui::Role::Button)
                .aria_label(preview_aria)
                .tab_index(0)
                .focus_visible(|style| {
                    style
                        .bg(crate::theme::ink(0.06))
                        .border_1()
                        .border_color(theme.accent)
                })
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        cx.stop_propagation();
                        preview_on_key.viewer.reset();
                        this.preview = Some(preview_on_key.clone());
                        this.preview_focus_pending = true;
                        cx.notify();
                    }
                }))
                .on_a11y_action(gpui::AccessibleAction::Click, move |_, _, cx| {
                    composer_for_preview
                        .update(cx, |this, cx| {
                            preview_on_a11y.viewer.reset();
                            this.preview = Some(preview_on_a11y.clone());
                            this.preview_focus_pending = true;
                            cx.notify();
                        })
                        .ok();
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    preview.viewer.reset();
                    this.preview = Some(preview.clone());
                    this.preview_focus_pending = true;
                    cx.notify();
                }))
                .child(
                    div()
                        .id(("composer-appshot-preview", ix))
                        .w(px(tile_width))
                        .h(px(APPSHOT_PREVIEW_HEIGHT))
                        .relative()
                        .flex_none()
                        .flex()
                        .items_end()
                        .justify_center()
                        .overflow_hidden()
                        .rounded(px(12.0))
                        .child(
                            div()
                                .w(px(image_width))
                                .h(px(image_height))
                                .flex_none()
                                .overflow_hidden()
                                .rounded(px(4.0))
                                .shadow_sm()
                                .child(crate::edge_fade::edge_faded(
                                    44.0,
                                    false,
                                    true,
                                    img(appshot.screenshot.image.clone())
                                        .w(px(image_width))
                                        .h(px(image_height))
                                        .object_fit(ObjectFit::Contain),
                                )),
                        ),
                )
                .child(
                    div()
                        .mt(px(20.0))
                        .max_w(px(tile_width - 20.0))
                        .truncate()
                        .text_center()
                        .text_size(px(12.5))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(source),
                );
            if let Some(icon) = &appshot.app_icon {
                card = card.child(crate::frost::layered(
                    div()
                        .absolute()
                        .top(px(APPSHOT_PREVIEW_HEIGHT - 22.0))
                        .left(px((tile_width - 28.0) / 2.0))
                        .size(px(28.0))
                        .rounded(px(7.0))
                        .bg(theme.bg)
                        .border_1()
                        .border_color(theme.border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            img(icon.clone())
                                .size(px(24.0))
                                .rounded(px(5.0))
                                .object_fit(ObjectFit::Contain),
                        ),
                ));
            }
            card = card.child(crate::frost::layered(
                crate::components::conversation_appshot_remove(theme, ix, group)
                    .tooltip(move |_, cx| {
                        cx.new(|_| AppshotActionTooltip(remove_label.clone()))
                            .into()
                    })
                    .role(gpui::Role::Button)
                    .aria_label(remove_aria)
                    .tab_index(0)
                    .focus_visible(|style| style.opacity(1.0).border_1().border_color(theme.accent))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            cx.stop_propagation();
                            this.remove_appshot(&remove_on_key_id, cx);
                        }
                    }))
                    .on_a11y_action(gpui::AccessibleAction::Click, move |_, _, cx| {
                        composer_for_remove
                            .update(cx, |this, cx| {
                                this.remove_appshot(&remove_on_a11y_id, cx);
                            })
                            .ok();
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.remove_appshot(&remove_id, cx);
                    })),
            ));
            // Entity-owned timestamps prevent the entrance replaying on route remount.
            if let Some(start) = self.appshot_entrances.get(&appshot.id) {
                let raw = (start.elapsed().as_secs_f32() / (0.24 * motion::speed_scale()))
                    .clamp(0.0, 1.0);
                if raw < 1.0 && !motion::reduced_motion(cx) {
                    let progress =
                        motion::MotionSpec::new(240, motion::EASE_OUT_EXPO).progress(raw);
                    card = card.opacity(progress).top(px(8.0 * (1.0 - progress)));
                    window.request_animation_frame();
                }
            }
            strip = strip.child(card);
        }
        Some(strip.into_any_element())
    }

    /// Paperclip: the native image picker (the original's hidden
    /// `<input type=file accept=image/* multiple>`).
    pub(super) fn open_file_picker(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        self.picker_task = Some(cx.spawn(async move |this, cx| {
            let result = rx.await;
            this.update(cx, |composer, cx| {
                if let Ok(Ok(Some(paths))) = result {
                    composer.add_paths(paths, cx);
                }
                // Both Attach and Cancel return to the draft.
                composer.focus_pending = true;
                cx.notify();
            })
            .ok();
        }));
    }

}
