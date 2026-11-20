use super::*;

impl Shell {
    pub(in crate::shell) fn sidebar_transfer_extra_gap(&self, group: &str) -> f32 {
        self.sidebar_session_transfer
            .as_ref()
            .or_else(|| {
                self.sidebar_session_return
                    .as_ref()
                    .map(|state| &state.transfer)
            })
            .and_then(|drag| drag.section_gaps.get(group))
            .map_or(0.0, |gap| {
                if self.reduced_motion {
                    gap.to
                } else {
                    gap.current()
                }
            })
    }

    pub(super) fn render_sidebar_gap_row(
        &mut self,
        row: AnyElement,
        id: &str,
        group: &str,
        index: usize,
    ) -> AnyElement {
        let returning = self.sidebar_session_transfer.is_none();
        let transfer = if let Some(drag) = self.sidebar_session_transfer.as_mut() {
            Some(drag)
        } else {
            self.sidebar_session_return
                .as_mut()
                .map(|returning| &mut returning.transfer)
        };
        let Some(drag) = transfer else {
            return row;
        };
        // Pin-to-pin keeps the existing sibling-slide implementation.
        let target = if returning || (group == "pinned" && drag.source_group == "pinned") {
            0.0
        } else {
            drag.preview
                .as_ref()
                .filter(|gap| gap.group == group)
                .map_or(0.0, |gap| {
                    sidebar_gap_offset(
                        index,
                        (drag.source_group == group).then_some(drag.source_index),
                        gap.index,
                        drag.row_height + SIDEBAR_LIST_GAP,
                    )
                })
        };
        let slide = drag
            .siblings
            .entry(id.to_owned())
            .or_insert_with(|| SidebarSessionSlide {
                from: 0.0,
                to: 0.0,
                epoch: drag.slide.epoch,
                started: std::time::Instant::now(),
            });
        slide.retarget(target);
        let (from, to, epoch) = (slide.from, slide.to, slide.epoch);
        let frame = div().relative().child(row);
        if !returning || self.reduced_motion {
            frame.top(px(to)).into_any_element()
        } else {
            frame
                .with_animation(
                    SharedString::from(format!("session-gap-{id}-{epoch}")),
                    TAB_SLIDE.animation(),
                    move |el, t| el.top(px(motion::lerp(from, to, t))),
                )
                .into_any_element()
        }
    }

    pub(in crate::shell) fn begin_sidebar_session_transfer(
        &mut self,
        payload: &SidebarSessionDrag,
        cursor_offset: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.chat_status_hover = None;
        self.cancel_pinned_session_drag(cx);
        self.sidebar_session_return = None;
        self.pinned_session_drag_generation = self.pinned_session_drag_generation.wrapping_add(1);
        let top = f32::from(
            window.mouse_position().y - cursor_offset.y - self.sidebar_scroll.bounds().top(),
        ) - f32::from(self.sidebar_scroll.offset().y);
        self.sidebar_session_transfer = Some(SidebarSessionTransfer {
            payload: payload.clone(),
            origin: std::rc::Rc::new(std::cell::Cell::new(
                window.mouse_position() - cursor_offset,
            )),
            cursor_offset,
            pointer: window.mouse_position(),
            viewport: None,
            preview: None,
            source_group: String::new(),
            source_index: 0,
            row_height: 0.0,
            source_collapse: SidebarSessionSlide {
                from: 0.0,
                to: 0.0,
                epoch: 0,
                started: std::time::Instant::now(),
            },
            collapsed_height: 0.0,
            section_gaps: Default::default(),
            siblings: Default::default(),
            slide: SidebarSessionSlide {
                from: top,
                to: top,
                epoch: self.pinned_session_drag_generation << 32,
                started: std::time::Instant::now(),
            },
        });
        {
            let origin = self
                .sidebar_session_transfer
                .as_ref()
                .unwrap()
                .origin
                .clone();
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(
                            SIDEBAR_DRAG_SCROLL_FRAME_MS,
                        ))
                        .await;
                    let keep_running = this
                        .update(cx, |shell, cx| {
                            let Some(transfer) = shell.sidebar_session_transfer.as_ref() else {
                                return false;
                            };
                            if !cx.has_active_drag()
                                || !std::rc::Rc::ptr_eq(&origin, &transfer.origin)
                            {
                                return false;
                            }
                            // Layout also animates while the pointer is stationary.
                            cx.notify();
                            // Pinned drags already have their own edge-scroll loop.
                            if transfer
                                .payload
                                .visible_ids
                                .contains(&transfer.payload.chat_id)
                            {
                                return true;
                            }
                            let Some(viewport) = transfer.viewport else {
                                return true;
                            };
                            if !viewport.contains(&transfer.pointer) {
                                return true;
                            }
                            let delta = pinned_drag_scroll_delta(
                                f32::from(transfer.pointer.y),
                                f32::from(viewport.top()),
                                f32::from(viewport.bottom()),
                            );
                            let offset = shell.sidebar_scroll.offset();
                            let scroll_top = -f32::from(offset.y);
                            let max_scroll = f32::from(shell.sidebar_scroll.max_offset().y);
                            let next = (scroll_top + delta).clamp(0.0, max_scroll);
                            if next != scroll_top {
                                shell
                                    .sidebar_scroll
                                    .set_offset(gpui::point(offset.x, px(-next)));
                                cx.notify();
                            }
                            true
                        })
                        .unwrap_or(false);
                    if !keep_running {
                        break;
                    }
                }
            })
            .detach();
        }
        cx.notify();
    }

    pub(in crate::shell) fn sidebar_session_transfer_is_valid(
        &self,
        payload: &SidebarSessionDrag,
        cx: &App,
    ) -> bool {
        if payload.filter != self.settings.space_filter
            || self.active_sidebar_pin_profile_key(cx).as_deref() != Some(&payload.profile_key)
        {
            return false;
        }
        let state = self.state.read(cx);
        let visible: HashSet<String> = state
            .sidebar_chats(Utc::now(), payload.filter.as_deref())
            .into_iter()
            .map(|(_, chat)| chat.id.clone())
            .collect();
        if !visible.contains(&payload.chat_id) {
            return false;
        }
        let pins = self.sidebar_pins_for_profile(&payload.profile_key, cx);
        let current: Vec<_> = pins.into_iter().filter(|id| visible.contains(id)).collect();
        current == *payload.visible_ids
    }

    pub(in crate::shell) fn cancel_sidebar_session_transfer(&mut self, cx: &mut Context<Self>) {
        self.chat_status_hover = None;
        self.cancel_pinned_session_drag(cx);
        if let Some(mut transfer) = self.sidebar_session_transfer.take() {
            transfer.preview = None;
            if !self.reduced_motion && self.sidebar_session_transfer_is_valid(&transfer.payload, cx)
            {
                self.sidebar_session_return = Some(SidebarSessionReturn {
                    transfer,
                    epoch: self.pinned_session_drag_generation,
                    started: std::time::Instant::now(),
                });
                let epoch = self.pinned_session_drag_generation;
                cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(
                                SIDEBAR_DRAG_SCROLL_FRAME_MS,
                            ))
                            .await;
                        let keep_running = this
                            .update(cx, |shell, cx| {
                                let Some(returning) = shell.sidebar_session_return.as_ref() else {
                                    return false;
                                };
                                if returning.epoch != epoch {
                                    return false;
                                }
                                if returning.started.elapsed() >= TAB_SLIDE.total()
                                    && returning.transfer.slide.started.elapsed()
                                        >= TAB_SLIDE.total()
                                    && returning.transfer.source_collapse.started.elapsed()
                                        >= TAB_SLIDE.total()
                                    && returning
                                        .transfer
                                        .section_gaps
                                        .values()
                                        .all(|gap| gap.started.elapsed() >= TAB_SLIDE.total())
                                {
                                    shell.sidebar_session_return = None;
                                    cx.notify();
                                    return false;
                                }
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if !keep_running {
                            break;
                        }
                    }
                })
                .detach();
                self.pinned_session_drag_generation =
                    self.pinned_session_drag_generation.wrapping_add(1);
            }
            cx.notify();
        }
    }

    pub(in crate::shell) fn render_moving_sidebar_session(
        &mut self,
        row: AnyElement,
        height: f32,
        theme: &Theme,
    ) -> AnyElement {
        let viewport = self.sidebar_scroll.bounds();
        let scroll_top = -f32::from(self.sidebar_scroll.offset().y);
        let returning = self.sidebar_session_transfer.is_none();
        let transfer = if let Some(transfer) = self.sidebar_session_transfer.as_mut() {
            transfer
        } else {
            &mut self.sidebar_session_return.as_mut().unwrap().transfer
        };
        let origin = f32::from(transfer.origin.get().y - viewport.top()) + scroll_top;
        let target = if returning {
            origin
        } else {
            f32::from(transfer.pointer.y - transfer.cursor_offset.y - viewport.top()) + scroll_top
        };
        if returning {
            transfer.slide.retarget(target);
        } else {
            transfer.slide.from = target;
            transfer.slide.to = target;
            transfer.slide.started = std::time::Instant::now();
        }
        let from = transfer.slide.from;
        let to = transfer.slide.to;
        let epoch = transfer.slide.epoch;
        let frame = div()
            .absolute()
            .left(px(Theme::SPACE_SM))
            .right(px(Theme::SPACE_SM))
            .h(px(height))
            .rounded(px(8.0))
            .when(!returning, |el| el.bg(theme.surface_raised).shadow_md())
            .child(row);
        if self.reduced_motion {
            frame.top(px(to)).into_any_element()
        } else {
            frame
                .with_animation(
                    ("sidebar-session-slide", epoch),
                    TAB_SLIDE.animation(),
                    move |el, t| el.top(px(motion::lerp(from, to, t))),
                )
                .into_any_element()
        }
    }

    pub(in crate::shell) fn finish_sidebar_session_transfer(
        &mut self,
        payload: &SidebarSessionDrag,
        target: SidebarSessionDrop,
        cx: &mut Context<Self>,
    ) {
        self.chat_status_hover = None;
        let matches_drag = self.sidebar_session_transfer.as_ref().is_some_and(|drag| {
            drag.payload.chat_id == payload.chat_id
                && drag.payload.profile_key == payload.profile_key
                && drag.payload.filter == payload.filter
                && drag.payload.visible_ids == payload.visible_ids
        });
        if !matches_drag || !self.sidebar_session_transfer_is_valid(payload, cx) {
            self.cancel_sidebar_session_transfer(cx);
            return;
        }
        if let SidebarSessionDrop::Section(id) = &target
            && !self
                .active_sidebar_sections(cx)
                .iter()
                .any(|section| &section.id == id)
        {
            self.cancel_sidebar_session_transfer(cx);
            return;
        }
        let sections = self.active_sidebar_sections(cx);
        let managed_source = sections.iter().any(|section| {
            section
                .id
                .starts_with(mast_proto::OUTLINE_SIDEBAR_SECTION_PREFIX)
                && section.session_ids.contains(&payload.chat_id)
        });
        let managed_target = matches!(&target, SidebarSessionDrop::Section(id)
            if id.starts_with(mast_proto::OUTLINE_SIDEBAR_SECTION_PREFIX));
        if managed_target || (managed_source && !matches!(target, SidebarSessionDrop::Pinned(_))) {
            self.sidebar_notice = Some("Move grouped sessions in Work Outline.".into());
            self.cancel_sidebar_session_transfer(cx);
            cx.notify();
            return;
        }
        let saved = self.raw_sidebar_pins(cx);
        let next = sidebar_session_drop_pins(
            &saved,
            &payload.visible_ids,
            &payload.chat_id,
            target.clone(),
        );
        let sections = self.active_sidebar_sections(cx);
        let source_section = sections
            .iter()
            .find(|section| section.session_ids.contains(&payload.chat_id))
            .map(|section| section.id.as_str());
        let target_section = match &target {
            SidebarSessionDrop::Section(id) => Some(id.as_str()),
            _ => None,
        };
        if saved == next && source_section == target_section {
            // A no-op drop still animates home; ordinary groups retain recency order.
            self.cancel_sidebar_session_transfer(cx);
            return;
        }
        // Validate and accept before ending the preview. Rejected drops use
        // the same animated return path as dropping outside a destination.
        let change = if let Some(index) = next.iter().position(|id| id == &payload.chat_id) {
            let after = index.checked_sub(1).and_then(|i| next.get(i)).cloned();
            let before = next.get(index + 1).cloned();
            if saved.contains(&payload.chat_id) {
                mast_proto::SidebarPinChange::Move {
                    session_id: payload.chat_id.clone(),
                    after,
                    before,
                }
            } else {
                mast_proto::SidebarPinChange::Pin {
                    session_id: payload.chat_id.clone(),
                    after,
                    before,
                }
            }
        } else {
            mast_proto::SidebarPinChange::Unpin {
                session_id: payload.chat_id.clone(),
            }
        };
        if self.state.read(cx).workspace_scope != Some(WorkspaceScope::Local)
            && !matches!(target, SidebarSessionDrop::Pinned(_))
        {
            if !self.change_sidebar_section(
                mast_proto::SidebarSectionChange::Assign {
                    session_id: payload.chat_id.clone(),
                    section_id: target_section.map(str::to_owned),
                },
                cx,
            ) {
                self.cancel_sidebar_session_transfer(cx);
                return;
            }
        } else {
            if saved != next
                && !self.apply_sidebar_pin_change(payload.profile_key.clone(), change, cx)
            {
                self.cancel_sidebar_session_transfer(cx);
                return;
            }
            if !matches!(target, SidebarSessionDrop::Pinned(_))
                || self.state.read(cx).workspace_scope == Some(WorkspaceScope::Local)
                || saved == next
            {
                self.assign_sidebar_section(
                    &payload.chat_id,
                    match &target {
                        SidebarSessionDrop::Section(id) => Some(id.as_str()),
                        _ => None,
                    },
                    cx,
                );
            }
        }
        self.sidebar_session_transfer = None;
        self.cancel_pinned_session_drag(cx);
        if matches!(target, SidebarSessionDrop::Pinned(_)) {
            self.pinned_open = true;
        } else {
            self.sessions_open = true;
        }
        // Membership changes preserve each group's live activity sort.
        // Drag previews already animated this move. Establish a fresh layout
        // baseline so the automatic resort glide does not replay it on release.
        self.sidebar_prev_order.clear();
        self.sidebar_resort.clear();
        self.sidebar_new_keys.clear();
        cx.notify();
    }
    pub(in crate::shell) fn update_pinned_session_drag(
        &mut self,
        payload: &SidebarSessionDrag,
        over: usize,
        cx: &mut Context<Self>,
    ) {
        if !self.pinned_open
            || self.active_sidebar_pin_profile_key(cx).as_deref() != Some(&payload.profile_key)
        {
            self.cancel_pinned_session_drag(cx);
            return;
        }
        let Some(from) = payload
            .visible_ids
            .iter()
            .position(|id| id == &payload.chat_id)
        else {
            self.cancel_pinned_session_drag(cx);
            return;
        };
        if over >= payload.visible_ids.len() {
            self.cancel_pinned_session_drag(cx);
            return;
        }
        match &mut self.pinned_session_drag {
            Some(drag)
                if drag.chat_id == payload.chat_id
                    && drag.filter == payload.filter
                    && drag.profile_key == payload.profile_key
                    && drag.visible_ids.as_ref() == payload.visible_ids.as_ref()
                    && drag.over != over =>
            {
                drag.prev_over = drag.over;
                drag.over = over;
                drag.epoch = drag.epoch.wrapping_add(1);
                cx.notify();
            }
            Some(drag)
                if drag.chat_id == payload.chat_id
                    && drag.filter == payload.filter
                    && drag.profile_key == payload.profile_key
                    && drag.visible_ids.as_ref() == payload.visible_ids.as_ref() => {}
            _ => {
                self.pinned_session_drag_generation =
                    self.pinned_session_drag_generation.wrapping_add(1);
                self.pinned_session_drag = Some(PinnedSessionDragState {
                    chat_id: payload.chat_id.clone(),
                    visible_ids: payload.visible_ids.clone(),
                    from,
                    over,
                    prev_over: from,
                    epoch: 0,
                    filter: payload.filter.clone(),
                    profile_key: payload.profile_key.clone(),
                    pointer_y: None,
                    viewport_top: 0.0,
                    viewport_bottom: 0.0,
                    generation: self.pinned_session_drag_generation,
                    autoscroll_active: false,
                });
                cx.notify();
            }
        }
    }

    pub(in crate::shell) fn track_pinned_session_drag_pointer(
        &mut self,
        payload: SidebarSessionDrag,
        pointer_y: f32,
        viewport_top: f32,
        viewport_bottom: f32,
        cx: &mut Context<Self>,
    ) {
        if !payload.visible_ids.contains(&payload.chat_id) {
            return;
        }
        let scroll_top = -f32::from(self.sidebar_scroll.offset().y);
        let rel_y = pinned_session_pointer_y(pointer_y, viewport_top, scroll_top);
        let inside_pinned_section =
            row_drop_index(rel_y, &self.sidebar_pinned_heights, false).is_some();
        let Some(over) = row_drop_index(rel_y, &self.sidebar_pinned_heights, true) else {
            if let Some(drag) = self.pinned_session_drag.as_mut() {
                drag.pointer_y = None;
                drag.autoscroll_active = false;
            }
            return;
        };

        self.update_pinned_session_drag(&payload, over, cx);
        if !inside_pinned_section {
            if let Some(drag) = self.pinned_session_drag.as_mut() {
                drag.pointer_y = None;
                drag.autoscroll_active = false;
            }
            return;
        }
        let delta = pinned_drag_scroll_delta(pointer_y, viewport_top, viewport_bottom);
        let Some(drag) = self.pinned_session_drag.as_mut() else {
            return;
        };
        drag.pointer_y = Some(pointer_y);
        drag.viewport_top = viewport_top;
        drag.viewport_bottom = viewport_bottom;
        let should_start = delta != 0.0 && !drag.autoscroll_active;
        let generation = drag.generation;
        if should_start {
            drag.autoscroll_active = true;
            self.start_pinned_session_autoscroll(generation, cx);
        }
    }

    pub(super) fn start_pinned_session_autoscroll(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(
                        super::SIDEBAR_DRAG_SCROLL_FRAME_MS,
                    ))
                    .await;
                let keep_running = this
                    .update(cx, |shell, cx| {
                        shell.pinned_session_autoscroll_tick(generation, cx)
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn pinned_session_autoscroll_tick(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(drag) = self.pinned_session_drag.as_ref() else {
            return false;
        };
        if drag.generation != generation || !cx.has_active_drag() {
            if let Some(drag) = self.pinned_session_drag.as_mut() {
                drag.autoscroll_active = false;
            }
            return false;
        }
        let Some(pointer_y) = drag.pointer_y else {
            if let Some(drag) = self.pinned_session_drag.as_mut() {
                drag.autoscroll_active = false;
            }
            return false;
        };
        let viewport_top = drag.viewport_top;
        let viewport_bottom = drag.viewport_bottom;
        let delta = pinned_drag_scroll_delta(pointer_y, viewport_top, viewport_bottom);
        let scroll_top = -f32::from(self.sidebar_scroll.offset().y);
        let max_scroll = f32::from(self.sidebar_scroll.max_offset().y);
        let Some(next_scroll) = pinned_drag_scroll_step(
            true,
            generation,
            drag.generation,
            scroll_top,
            max_scroll,
            delta,
        ) else {
            if let Some(drag) = self.pinned_session_drag.as_mut() {
                drag.autoscroll_active = false;
            }
            return false;
        };

        let rel_y = pinned_session_pointer_y(pointer_y, viewport_top, next_scroll);
        let Some(over) = row_drop_index(rel_y, &self.sidebar_pinned_heights, false) else {
            if let Some(drag) = self.pinned_session_drag.as_mut() {
                drag.autoscroll_active = false;
            }
            return false;
        };

        let offset = self.sidebar_scroll.offset();
        self.sidebar_scroll
            .set_offset(gpui::point(offset.x, px(-next_scroll)));
        if let Some(drag) = self.pinned_session_drag.as_mut()
            && drag.over != over
        {
            drag.prev_over = drag.over;
            drag.over = over;
            drag.epoch = drag.epoch.wrapping_add(1);
        }
        cx.notify();
        true
    }

    pub(in crate::shell) fn pinned_session_drag_is_valid(&self, cx: &App) -> bool {
        let Some(drag) = self.pinned_session_drag.as_ref() else {
            return true;
        };
        if !self.pinned_open
            || drag.filter != self.settings.space_filter
            || self.active_sidebar_pin_profile_key(cx).as_deref() != Some(&drag.profile_key)
        {
            return false;
        }
        let current_pins = self.sidebar_pins_for_profile(&drag.profile_key, cx);
        let visible_ids: HashSet<String> = self
            .state
            .read(cx)
            .overview_chats(Utc::now())
            .into_iter()
            .filter(|(_, chat)| match &drag.filter {
                Some(space_id) => chat.space_id.as_deref() == Some(space_id.as_str()),
                None => true,
            })
            .map(|(_, chat)| chat.id.clone())
            .collect();
        let current_ids = current_pins
            .iter()
            .filter(|id| visible_ids.contains(id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        pinned_drag_snapshot_is_valid(&drag.chat_id, drag.visible_ids.as_ref(), &current_ids)
    }

    pub(in crate::shell) fn cancel_pinned_session_drag(&mut self, cx: &mut Context<Self>) {
        if self.pinned_session_drag.take().is_some() {
            self.pinned_session_drag_generation =
                self.pinned_session_drag_generation.wrapping_add(1);
            cx.notify();
        }
    }
}
