use super::*;

impl Shell {
    /// Flat top-to-bottom chat ids exactly as [`Self::render_active_rows`]
    /// draws them: pins first, then the user's sort, device grouping,
    /// and local-device promotion. Jump shortcuts and session cycling read
    /// this projection so keyboard order never drifts from the screen.
    pub(in crate::shell) fn sidebar_visible_order(&self, cx: &Context<Self>) -> Vec<String> {
        let filter = self.settings.space_filter.clone();
        let profile_key = self.active_sidebar_pin_profile_key(cx);
        let saved_pins = self.active_sidebar_pins(cx);
        let frozen_pinned = self
            .pinned_session_drag
            .as_ref()
            .filter(|drag| {
                drag.filter == filter && profile_key.as_deref() == Some(&drag.profile_key)
            })
            .map(|drag| drag.visible_ids.clone());
        let pinned_order = frozen_pinned
            .as_ref()
            .map_or(saved_pins.as_slice(), |ids| ids.as_slice());
        let state = self.state.read(cx);
        let mut chats: Vec<mast_proto::Chat> = state
            .sidebar_chats_unsorted(Utc::now(), filter.as_deref())
            .into_iter()
            .map(|(_, chat)| chat.clone())
            .collect();
        #[cfg(test)]
        perf_counts::SIDEBAR_SORTS.with(|c| c.set(c.get() + 1));
        chats.sort_by(|left, right| compare_sidebar_chats(self.settings.sidebar_sort, left, right));
        let (pinned_chats, chats): (Vec<_>, Vec<_>) = chats
            .into_iter()
            .partition(|chat| pinned_order.contains(&chat.id));
        let custom_sections = self.active_sidebar_sections(cx);
        let mut custom_order = Vec::new();
        for section in &custom_sections {
            if !section.collapsed {
                custom_order.extend(
                    chats
                        .iter()
                        .filter(|chat| section.session_ids.contains(&chat.id))
                        .map(|chat| chat.id.clone()),
                );
            }
        }
        let chats: Vec<_> = chats
            .into_iter()
            .filter(|chat| {
                !custom_sections
                    .iter()
                    .any(|section| section.session_ids.contains(&chat.id))
            })
            .collect();
        let ordered = {
            let mut groups: Vec<(Option<(String, String)>, Vec<mast_proto::Chat>)> = Vec::new();
            for chat in chats {
                let key = sidebar_group_key(self.settings.sidebar_organization, &chat)
                    .map(|key| (key, String::new()));
                if let Some((_, existing)) = groups.iter_mut().find(|(group, _)| group == &key) {
                    existing.push(chat);
                } else {
                    groups.push((key, vec![chat]));
                }
            }
            if self.settings.sidebar_organization == SidebarOrganization::ByDevice {
                promote_local_device_group(&mut groups, state.local_device_id.as_deref());
            }
            move_scheduled_group_last(&mut groups);
            groups
                .into_iter()
                .flat_map(|(group, rows)| {
                    if !group.is_some_and(|(key, _)| key == SCHEDULED_GROUP_KEY) {
                        return rows;
                    }
                    if !self.scheduled_tasks_open {
                        return Vec::new();
                    }
                    let tagged = rows
                        .into_iter()
                        .map(|chat| (chat.scheduled_task.clone().unwrap_or_default(), chat))
                        .collect();
                    group_by_scheduled_task(tagged)
                        .into_iter()
                        .filter(|(task, _)| {
                            !self
                                .sidebar_collapsed_groups
                                .contains(&scheduled_subgroup_key(task))
                        })
                        .flat_map(|(_, rows)| rows)
                        .collect()
                })
                .map(|chat| chat.id)
                .collect::<Vec<_>>()
        };
        let ordered = pinned_chats
            .into_iter()
            .map(|chat| chat.id)
            .chain(custom_order.iter().cloned())
            .chain(ordered)
            .collect::<Vec<_>>();
        let mut visible = project_pinned_first(&ordered, pinned_order);
        if !self.sessions_open
            && self.settings.sidebar_organization == SidebarOrganization::InOneList
        {
            visible.retain(|id| pinned_order.contains(id) || custom_order.contains(id));
        }
        if !self.pinned_open {
            let pins: HashSet<&str> = pinned_order.iter().map(String::as_str).collect();
            visible.retain(|id| !pins.contains(id.as_str()));
        }
        visible
    }
    /// Shared metadata and visibility settings for active and archived sessions.
    pub(super) fn sidebar_chat_data(
        &self,
        status: ChatIndicator,
        chat: mast_proto::Chat,
        state: &AppState,
    ) -> ActiveChatRow {
        // Line 1 is "Space @ device" (t3code's project row);
        // project-less sessions read as their home-dir cwd `~`.
        let space = state.space_for_chat(&chat);
        let project = match (space, chat.space_id.as_deref()) {
            (Some(space), _) => space.display_name().to_string(),
            (None, None) => "~".to_string(),
            (None, Some(_)) => "?".to_string(),
        };
        let device = state
            .device_name(&chat.device_id)
            .unwrap_or("Unknown device")
            .to_string();
        let mut folder = project.clone();
        // Unknown device → no fragment, same as the archived list.
        if state.device_name(&chat.device_id).is_some() {
            folder = format!("{folder} @ {device}");
        }
        // The branch shows whenever the engine has stamped one —
        // main-checkout sessions included, not just worktrees.
        let branch = state
            .display_chat_branch(&chat)
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .map(str::to_string)
            .filter(|_| self.settings.sidebar_show_branch);
        let change_request = state
            .change_request_for_chat(&chat)
            .cloned()
            .filter(|_| self.settings.sidebar_show_pull_request);
        let organization = self.settings.sidebar_organization;
        let group = sidebar_group_key(organization, &chat).map(|key| {
            let label = if chat.scheduled_task.is_some() {
                "Scheduled tasks".to_owned()
            } else if organization == SidebarOrganization::ByDevice {
                device
            } else {
                project
            };
            (key, label)
        });
        ActiveChatRow {
            status,
            chat: chat.clone(),
            folder,
            branch,
            change_request,
            group,
        }
    }
    pub(in crate::shell) fn render_active_rows(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> SidebarSessionRows {
        #[cfg(test)]
        let measurement_started = std::time::Instant::now();
        let now = Utc::now();
        let filter = self.settings.space_filter.clone();
        let profile_key = self.active_sidebar_pin_profile_key(cx);
        let saved_pins = self.active_sidebar_pins(cx);
        let frozen_pinned = self
            .pinned_session_drag
            .as_ref()
            .filter(|drag| {
                drag.filter == filter && profile_key.as_deref() == Some(&drag.profile_key)
            })
            .map(|drag| drag.visible_ids.clone());
        let mut rows: Vec<ActiveChatRow> = {
            let state = self.state.read(cx);
            let mut chats: Vec<_> = state
                .sidebar_chats_unsorted(now, filter.as_deref())
                .into_iter()
                .map(|(status, chat)| (status, chat.clone()))
                .collect();
            #[cfg(test)]
            perf_counts::SIDEBAR_SORTS.with(|c| c.set(c.get() + 1));
            chats.sort_by(|left, right| {
                compare_sidebar_chats(self.settings.sidebar_sort, &left.1, &right.1)
            });
            chats
                .into_iter()
                .map(|(status, chat)| self.sidebar_chat_data(status, chat, state))
                .collect()
        };
        let pinned_order = frozen_pinned
            .as_ref()
            .map_or(saved_pins.as_slice(), |ids| ids.as_slice());
        let base_ids: Vec<String> = rows.iter().map(|row| row.chat.id.clone()).collect();
        let ordered_ids = project_pinned_first(&base_ids, pinned_order);
        let rank: std::collections::HashMap<&str, usize> = ordered_ids
            .iter()
            .enumerate()
            .map(|(ix, id)| (id.as_str(), ix))
            .collect();
        #[cfg(test)]
        perf_counts::SIDEBAR_SORTS.with(|c| c.set(c.get() + 1));
        rows.sort_by_key(|row| {
            rank.get(row.chat.id.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
        let active: HashSet<&str> = base_ids.iter().map(String::as_str).collect();
        let pinned_count = pinned_order
            .iter()
            .filter(|id| active.contains(id.as_str()))
            .collect::<HashSet<_>>()
            .len();
        let regular_rows = rows.split_off(pinned_count);
        let pinned_rows = rows;
        self.sidebar_pinned_heights = pinned_rows
            .iter()
            .map(|row| {
                sidebar_row_height(
                    self.settings.sidebar_compact,
                    self.settings.sidebar_show_project_label,
                    row.branch.is_some(),
                    row.change_request.is_some(),
                    crate::ui_style::current(cx).tokens,
                )
            })
            .collect();
        let visible_pinned_ids = std::sync::Arc::new(
            pinned_rows
                .iter()
                .map(|row| row.chat.id.clone())
                .collect::<Vec<_>>(),
        );

        let custom_sections = self.active_sidebar_sections(cx);
        let custom_count = custom_sections.len();
        let mut custom_groups: Vec<_> = custom_sections
            .iter()
            .map(|section| {
                (
                    Some((format!("section:{}", section.id), section.name.clone())),
                    Vec::new(),
                )
            })
            .collect();
        let mut regular_groups: Vec<(Option<(String, String)>, Vec<ActiveChatRow>)> = Vec::new();
        for row in regular_rows {
            if let Some(index) = custom_sections
                .iter()
                .position(|section| section.session_ids.contains(&row.chat.id))
            {
                custom_groups[index].1.push(row);
                continue;
            }
            if let Some((_, existing)) = regular_groups
                .iter_mut()
                .find(|(group, _)| group == &row.group)
            {
                existing.push(row);
            } else {
                regular_groups.push((row.group.clone(), vec![row]));
            }
        }
        if self.settings.sidebar_organization == SidebarOrganization::ByDevice {
            let local_device_id = self.state.read(cx).local_device_id.clone();
            promote_local_device_group(&mut regular_groups, local_device_id.as_deref());
        }
        move_scheduled_group_last(&mut regular_groups);
        let mut sections = Vec::with_capacity(regular_groups.len() + usize::from(pinned_count > 0));
        if !pinned_rows.is_empty() {
            sections.push((None, pinned_rows));
        }
        let custom_sessions = custom_groups
            .iter()
            .map(|(_, rows)| rows.len())
            .sum::<usize>();
        sections.extend(custom_groups);
        sections.extend(regular_groups);

        let returning = self.sidebar_session_transfer.is_none();
        let transfer = self.sidebar_session_transfer.as_mut().or_else(|| {
            self.sidebar_session_return
                .as_mut()
                .map(|state| &mut state.transfer)
        });
        if let Some(drag) = transfer {
            for (section_index, (group, rows)) in sections.iter().enumerate() {
                let Some(index) = rows
                    .iter()
                    .position(|row| row.chat.id == drag.payload.chat_id)
                else {
                    continue;
                };
                drag.source_group = if section_index == 0 && pinned_count > 0 {
                    "pinned".into()
                } else {
                    group
                        .as_ref()
                        .map_or_else(|| "regular".into(), |(key, _)| format!("regular:{key}"))
                };
                drag.source_index = index;
                drag.row_height = sidebar_row_height(
                    self.settings.sidebar_compact,
                    self.settings.sidebar_show_project_label,
                    rows[index].branch.is_some(),
                    rows[index].change_request.is_some(),
                    crate::ui_style::current(cx).tokens,
                );
                // Move the vacant slot to the destination instead of keeping two
                // holes. Sample layout and paint with the same reversible easing.
                let collapse = !returning
                    && drag
                        .preview
                        .as_ref()
                        .is_some_and(|gap| gap.group != drag.source_group);
                let target = if collapse {
                    drag.row_height
                        + if rows.len() > 1 {
                            SIDEBAR_LIST_GAP
                        } else {
                            0.0
                        }
                } else {
                    0.0
                };
                drag.source_collapse.retarget(target);
                let removed = if self.reduced_motion {
                    target
                } else {
                    drag.source_collapse.current()
                };
                if let Some(gap) = drag.preview.as_mut() {
                    let origin = f32::from(
                        drag.origin.get().y
                            - self.sidebar_scroll.bounds().top()
                            - self.sidebar_scroll.offset().y,
                    );
                    if gap.group != drag.source_group && gap.top > origin {
                        gap.top -= removed - drag.collapsed_height;
                    }
                }
                drag.collapsed_height = removed;
                let destination = drag
                    .preview
                    .as_ref()
                    .filter(|gap| !returning && gap.group != drag.source_group)
                    .map(|gap| gap.group.clone());
                if let Some(group) = &destination {
                    drag.section_gaps
                        .entry(group.clone())
                        .or_insert_with(|| SidebarSessionSlide {
                            from: 0.0,
                            to: 0.0,
                            epoch: 0,
                            started: std::time::Instant::now(),
                        });
                }
                for (group, gap) in &mut drag.section_gaps {
                    gap.retarget(if destination.as_ref() == Some(group) {
                        drag.row_height
                            + if group == "pinned" && pinned_count == 0 {
                                0.0
                            } else {
                                SIDEBAR_LIST_GAP
                            }
                    } else {
                        0.0
                    });
                }
                break;
            }
        }

        #[cfg(test)]
        let preparation_elapsed = measurement_started.elapsed();
        let selected = self.state.read(cx).selected_chat.clone();
        // Re-checked at render so the chips drop the FRAME a popover opens,
        // not on the next modifier event — the jumps are suppressed under it.
        let jump_hints = self.jump_hints && !self.overlay_owns_keyboard(cx);
        let keymap = self.settings.keymap.clone();
        // Flat top-to-bottom slot across groups: the same order
        // `sidebar_visible_order` hands the jump shortcuts and cycling, so a
        // chip always names the key that opens its row.
        let visible_slots: std::collections::HashMap<_, _> = if jump_hints {
            self.sidebar_visible_order(cx)
                .into_iter()
                .enumerate()
                .map(|(index, id)| (id, index))
                .collect()
        } else {
            std::collections::HashMap::new()
        };
        let mut slot = 0usize;
        let mut rendered = Vec::new();
        let mut moving_row = None;
        // Drag previews animate sibling offsets, so every row stays mounted.
        let cull = (self.sidebar_session_transfer.is_none()
            && self.sidebar_session_return.is_none()
            && self.pinned_session_drag.is_none())
        .then(|| {
            (
                -f32::from(self.sidebar_scroll.offset().y),
                f32::from(self.sidebar_scroll.bounds().size.height),
            )
        });
        self.sidebar_row_extents
            .borrow_mut()
            .retain(|id, _| active.contains(id.as_str()));
        for (group, rows) in sections {
            let pinned_group = slot < pinned_count;
            // A closed disclosure still needs row heights for its next opening
            // animation, but its clipped children need no controls or text layout.
            // Keep the children while a transition or drag is in progress.
            let closed_motion_key = if pinned_group && !self.pinned_open {
                Some("pinned".to_owned())
            } else if let Some((key, _)) = &group {
                if let Some(id) = key.strip_prefix("section:") {
                    custom_sections
                        .iter()
                        .find(|section| section.id == id)
                        .filter(|section| section.collapsed)
                        .map(|_| format!("custom:{id}"))
                } else {
                    let organization = match self.settings.sidebar_organization {
                        SidebarOrganization::ByDevice => "device",
                        SidebarOrganization::ByProject => "project",
                        SidebarOrganization::InOneList => "list",
                    };
                    let collapse_key = format!("{organization}:{key}");
                    self.sidebar_collapsed_groups
                        .contains(&collapse_key)
                        .then(|| format!("group:{collapse_key}"))
                }
            } else if !pinned_group
                && self.settings.sidebar_organization == SidebarOrganization::InOneList
                && !self.sessions_open
            {
                Some("sessions".to_owned())
            } else {
                None
            };
            let omit_closed_rows = self.sidebar_session_transfer.is_none()
                && self.sidebar_session_return.is_none()
                && self.pinned_session_drag.is_none()
                && closed_motion_key.as_ref().is_some_and(|key| {
                    !self
                        .sidebar_disclosure_motion
                        .get(key)
                        .is_some_and(|motion| motion.animating())
                });
            let drag_group = if pinned_group {
                "pinned".to_owned()
            } else {
                group
                    .as_ref()
                    .map_or_else(|| "regular".to_owned(), |(key, _)| format!("regular:{key}"))
            };
            let row_tasks: Vec<String> = rows
                .iter()
                .map(|row| row.chat.scheduled_task.clone().unwrap_or_default())
                .collect();
            let mut rendered_rows = Vec::with_capacity(rows.len());
            for (group_index, row) in rows.into_iter().enumerate() {
                if omit_closed_rows {
                    let height = sidebar_row_height(
                        self.settings.sidebar_compact,
                        self.settings.sidebar_show_project_label,
                        row.branch.is_some(),
                        row.change_request.is_some(),
                        crate::ui_style::current(cx).tokens,
                    );
                    slot += 1;
                    rendered_rows.push((
                        format!("c:{}", row.chat.id),
                        height,
                        div().h(px(height)).flex_none().into_any_element(),
                    ));
                    continue;
                }
                let ActiveChatRow {
                    status,
                    chat,
                    folder,
                    branch,
                    change_request,
                    group: _,
                } = row;
                let time_ago: SharedString =
                    format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), now).into();
                let is_selected = selected.as_deref() == Some(chat.id.as_str());
                let harness = self
                    .settings
                    .sidebar_show_harness
                    .then(|| chat.config.as_ref().map(|c| c.harness))
                    .flatten();
                let height = sidebar_row_height(
                    self.settings.sidebar_compact,
                    self.settings.sidebar_show_project_label,
                    branch.is_some(),
                    change_request.is_some(),
                    crate::ui_style::current(cx).tokens,
                );
                // Only rows a jump slot can reach wear a chip; row 10 onward
                // keeps its time-ago.
                let jump_slot = visible_slots.get(&chat.id).copied();
                let jump_label: Option<SharedString> = if jump_hints && let Some(slot) = jump_slot {
                    let combo = keymap.get(ShortcutId::JumpSession(slot));
                    (slot < JUMP_SLOTS && !combo.is_empty()).then(|| badge_combo(combo).into())
                } else {
                    None
                };
                let drag = (self.pinned_open || slot >= pinned_count)
                    .then(|| {
                        profile_key.as_ref().map(|profile_key| SidebarSessionDrag {
                            chat_id: chat.id.clone(),
                            visible_ids: visible_pinned_ids.clone(),
                            filter: filter.clone(),
                            profile_key: profile_key.clone(),
                        })
                    })
                    .flatten();
                let ordinal = slot;
                slot += 1;
                let origin = self
                    .sidebar_session_transfer
                    .as_ref()
                    .filter(|drag| drag.payload.chat_id == chat.id)
                    .map(|drag| drag.origin.clone())
                    .or_else(|| {
                        self.sidebar_session_return
                            .as_ref()
                            .filter(|returning| returning.transfer.payload.chat_id == chat.id)
                            .map(|returning| returning.transfer.origin.clone())
                    });
                let is_moving = origin.is_some();
                let removed = self
                    .sidebar_session_transfer
                    .as_ref()
                    .or_else(|| {
                        self.sidebar_session_return
                            .as_ref()
                            .map(|state| &state.transfer)
                    })
                    .filter(|drag| drag.payload.chat_id == chat.id)
                    .map_or(0.0, |drag| drag.collapsed_height);
                let slot_height = height - removed;
                let offscreen = origin.is_none()
                    && !is_selected
                    && cull.is_some_and(|(scroll_top, viewport)| {
                        let extent = self
                            .sidebar_row_extents
                            .borrow()
                            .get(&chat.id)
                            .filter(|(recorded, _, _)| *recorded == ordinal)
                            .map(|&(_, top, bottom)| (top, bottom));
                        sidebar_row_offscreen(extent, scroll_top, viewport)
                    });
                let element = if offscreen {
                    div().h(px(slot_height.max(0.0))).into_any_element()
                } else {
                    let title: SharedString = transcript::single_line(
                        &chat.title.clone().unwrap_or_else(|| "New session".into()),
                    )
                    .into();
                    if is_moving {
                        self.render_chat_row_with_chat(
                            chat.id.clone(),
                            None,
                            title,
                            time_ago,
                            folder.into(),
                            branch.map(SharedString::from),
                            change_request,
                            harness,
                            status,
                            is_selected,
                            false,
                            true,
                            None,
                            jump_label,
                            None,
                            Some(&chat),
                            theme,
                            cx,
                        )
                    } else {
                        self.cached_chat_row(
                            ChatRowArgs {
                                chat: chat.clone(),
                                id: chat.id.clone(),
                                title,
                                time_ago,
                                space_name: folder.into(),
                                branch: branch.map(SharedString::from),
                                change_request,
                                harness,
                                status,
                                selected: is_selected,
                                drag,
                                jump_label,
                            },
                            slot_height,
                            cx,
                        )
                    }
                };
                // The source slot shrinks when its vacancy moves across sections.
                // Its zero-height anchor still tracks the live activity position.
                let element = if offscreen {
                    element
                } else if let Some(origin) = origin {
                    moving_row = Some((element, height));
                    div()
                        .child(div().h(px(slot_height.max(0.0))))
                        .on_children_prepainted(move |bounds, _, _| {
                            if let Some(bounds) = bounds.first() {
                                origin.set(bounds.origin);
                            }
                        })
                        .into_any_element()
                } else {
                    self.render_sidebar_gap_row(element, &chat.id, &drag_group, group_index)
                };
                // The hit region stays at the natural slot while its content slides;
                // preview movement must not move its own insertion thresholds.
                let target_group = drag_group.clone();
                let element = div()
                    .id(SharedString::from(format!("session-slot-{}", chat.id)))
                    .debug_selector({
                        let id = chat.id.clone();
                        move || format!("session-slot-{id}")
                    })
                    .h(px(slot_height.max(0.0)))
                    .mb(px(slot_height.min(0.0)))
                    .flex_none()
                    .child(element)
                    .on_drag_move::<SidebarSessionDrag>(cx.listener(
                        move |this, event: &gpui::DragMoveEvent<SidebarSessionDrag>, _, cx| {
                            if !event.bounds.contains(&event.event.position)
                                || !this.sidebar_scroll.bounds().contains(&event.event.position)
                            {
                                return;
                            }
                            let scroll_top = -f32::from(this.sidebar_scroll.offset().y);
                            let viewport_top = this.sidebar_scroll.bounds().top();
                            let Some(drag) = this.sidebar_session_transfer.as_mut() else {
                                return;
                            };
                            let after = event.event.position.y >= event.bounds.center().y;
                            let index = group_index + usize::from(after);
                            let mut top = f32::from(
                                if after {
                                    event.bounds.bottom() + px(SIDEBAR_LIST_GAP)
                                } else {
                                    event.bounds.top()
                                } - viewport_top,
                            ) + scroll_top;
                            if drag.source_group == target_group && index > drag.source_index {
                                top -= drag.row_height + SIDEBAR_LIST_GAP - drag.collapsed_height;
                            }
                            drag.preview = Some(SidebarSessionGap {
                                group: target_group.clone(),
                                index,
                                pinned: pinned_group,
                                top,
                            });
                            cx.notify();
                        },
                    ))
                    .into_any_element();
                let element = if self.sidebar_session_transfer.is_none()
                    && self.sidebar_session_return.is_none()
                    && self.pinned_session_drag.is_none()
                    && slot_height > 0.0
                {
                    let extents = self.sidebar_row_extents.clone();
                    let scroll = self.sidebar_scroll.clone();
                    let id = chat.id.clone();
                    let shell = cx.entity_id();
                    VisibleSidebarRow {
                        height: slot_height,
                        child: element,
                        on_slot: Some(Box::new(move |bounds, cx| {
                            let top =
                                f32::from(bounds.top() - scroll.bounds().top() - scroll.offset().y);
                            let extent = (top, top + f32::from(bounds.size.height));
                            extents
                                .borrow_mut()
                                .insert(id, (ordinal, extent.0, extent.1));
                            // A re-sort can move a culled row into view; fill it next frame.
                            if offscreen
                                && !sidebar_row_offscreen(
                                    Some(extent),
                                    -f32::from(scroll.offset().y),
                                    f32::from(scroll.bounds().size.height),
                                )
                            {
                                cx.notify(shell);
                            }
                        })),
                    }
                    .into_any_element()
                } else {
                    element
                };
                rendered_rows.push((format!("c:{}", chat.id), slot_height, element));
            }

            let Some((key, label)) = group else {
                rendered.extend(rendered_rows);
                if !pinned_group {
                    let extra = self.sidebar_transfer_extra_gap(&drag_group);
                    if extra > 0.0 {
                        rendered.push((
                            format!("gap:{drag_group}"),
                            extra - SIDEBAR_LIST_GAP,
                            div()
                                .flex_none()
                                .h(px((extra - SIDEBAR_LIST_GAP).max(0.0)))
                                .mb(px((extra - SIDEBAR_LIST_GAP).min(0.0)))
                                .into_any_element(),
                        ));
                    }
                }
                continue;
            };
            if let Some(id) = key.strip_prefix("section:")
                && let Some(section) = custom_sections.iter().find(|section| section.id == id)
            {
                rendered.push(self.render_custom_sidebar_section(
                    section.clone(),
                    rendered_rows,
                    drag_group,
                    theme,
                    cx,
                ));
                continue;
            }
            if key == SCHEDULED_GROUP_KEY {
                rendered.push(self.render_scheduled_group(
                    row_tasks.into_iter().zip(rendered_rows).collect(),
                    &drag_group,
                    theme,
                    cx,
                ));
                continue;
            }
            let organization = match self.settings.sidebar_organization {
                SidebarOrganization::ByDevice => "device",
                SidebarOrganization::ByProject => "project",
                SidebarOrganization::InOneList => "list",
            };
            let collapse_key = format!("{organization}:{key}");
            let motion_key = format!("group:{collapse_key}");
            let collapsed = self.sidebar_collapsed_groups.contains(&collapse_key);
            let row_count = rendered_rows.len();
            let extra_gap = self.sidebar_transfer_extra_gap(&drag_group);
            let body_height = SIDEBAR_DISCLOSURE_BODY_INSET
                + extra_gap
                + rendered_rows
                    .iter()
                    .map(|(_, height, _)| *height)
                    .sum::<f32>()
                + SIDEBAR_LIST_GAP * row_count.saturating_sub(1) as f32;
            let body = div()
                .w_full()
                .flex()
                .flex_col()
                .pt(px(SIDEBAR_DISCLOSURE_BODY_INSET))
                .gap(px(SIDEBAR_LIST_GAP))
                .children(rendered_rows.into_iter().map(|(_, _, row)| row))
                .when(extra_gap > 0.0, |el| {
                    el.child(
                        div()
                            .flex_none()
                            .h(px((extra_gap - SIDEBAR_LIST_GAP).max(0.0)))
                            .mb(px((extra_gap - SIDEBAR_LIST_GAP).min(0.0))),
                    )
                });
            let visible_label: SharedString = if collapsed {
                format!("{label} ({row_count})").into()
            } else {
                label.into()
            };
            let chevron = self.sidebar_disclosure_chevron(&motion_key, !collapsed, theme);
            let toggle_key = collapse_key.clone();
            let toggle_motion_key = motion_key.clone();
            // Home groups are scoped to a device; other project groups refer to
            // a registered space and share its existing management menu.
            let project_id = (self.settings.sidebar_organization == SidebarOrganization::ByProject)
                .then(|| self.state.read(cx).space_row(&key).map(|_| key.clone()))
                .flatten();
            let menu = self.sidebar_group_menu_button(
                &collapse_key,
                project_id,
                SidebarGroupTarget::Device {
                    device_id: key.strip_prefix("home:").unwrap_or(&key).to_owned(),
                    key: key.clone(),
                },
                theme,
                cx,
            );
            let controls = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(menu)
                .child(chevron);
            let header =
                sidebar_disclosure_header(theme, visible_label, controls.into_any_element())
                    .id(SharedString::from(format!("sidebar-group-{collapse_key}")))
                    .group("automatic-sidebar-group")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let was_open = !this.sidebar_collapsed_groups.contains(&toggle_key);
                        this.begin_sidebar_disclosure_motion(
                            &toggle_motion_key,
                            if was_open { body_height } else { 0.0 },
                            if was_open { 0.0 } else { body_height },
                        );
                        if was_open {
                            this.sidebar_collapsed_groups.insert(toggle_key.clone());
                        } else {
                            this.sidebar_collapsed_groups.remove(&toggle_key);
                        }
                        cx.notify();
                    }));
            let body = self.render_sidebar_disclosure_body(
                &motion_key,
                !collapsed,
                body_height,
                body.into_any_element(),
            );
            let height =
                SIDEBAR_DISCLOSURE_SECTION_HEIGHT + if collapsed { 0.0 } else { body_height };
            let element = div()
                .w_full()
                .flex()
                .flex_col()
                .pt(px(SIDEBAR_SECTION_GAP))
                .child(header)
                .child(body)
                .into_any_element();
            rendered.push((format!("g:{collapse_key}"), height, element));
        }
        #[cfg(test)]
        if std::env::var_os("MAST_BENCH_SIDEBAR").is_some() {
            eprintln!(
                "BENCH sidebar | rows={} preparation_ms={:.3} construction_ms={:.3}",
                base_ids.len(),
                preparation_elapsed.as_secs_f64() * 1000.0,
                measurement_started
                    .elapsed()
                    .saturating_sub(preparation_elapsed)
                    .as_secs_f64()
                    * 1000.0
            );
        }
        SidebarSessionRows {
            custom_count,
            regular_count: base_ids
                .len()
                .saturating_sub(pinned_count + custom_sessions),
            rows: rendered,
            pinned_count,
            moving_row,
        }
    }
    pub(in crate::shell) fn render_pinned_section(
        &mut self,
        items: Vec<AnyElement>,
        body_height: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.pinned_open;
        let label = if open {
            "Pinned".into()
        } else {
            format!("Pinned ({})", items.len()).into()
        };
        let chevron = self.sidebar_disclosure_chevron("pinned", open, theme);
        let header = sidebar_disclosure_header(theme, label, chevron)
            .id("pinned-toggle")
            .debug_selector(|| "pinned-toggle".into())
            .on_drag_move::<SidebarSessionDrag>(cx.listener(
                |this, event: &gpui::DragMoveEvent<SidebarSessionDrag>, _, _| {
                    if !event.bounds.contains(&event.event.position)
                        || !this.sidebar_scroll.bounds().contains(&event.event.position)
                    {
                        return;
                    }
                    let top = f32::from(event.bounds.bottom() - this.sidebar_scroll.bounds().top())
                        + SIDEBAR_DISCLOSURE_BODY_INSET
                        - f32::from(this.sidebar_scroll.offset().y);
                    if let Some(drag) = this.sidebar_session_transfer.as_mut() {
                        drag.preview = Some(SidebarSessionGap {
                            group: "pinned".into(),
                            index: 0,
                            pinned: true,
                            top,
                        });
                    }
                },
            ))
            .on_drop::<SidebarSessionDrag>(cx.listener(|this, payload, _, cx| {
                this.finish_sidebar_session_transfer(payload, SidebarSessionDrop::Pinned(0), cx);
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                let was_open = this.pinned_open;
                this.cancel_pinned_session_drag(cx);
                this.begin_sidebar_disclosure_motion(
                    "pinned",
                    if was_open { body_height } else { 0.0 },
                    if was_open { 0.0 } else { body_height },
                );
                this.pinned_open = !was_open;
                // The disclosure owns this movement; avoid a second FLIP
                // animation on the regular sessions below it.
                this.sidebar_prev_order.clear();
                this.sidebar_resort.clear();
                this.sidebar_new_keys.clear();
                cx.notify();
            }));
        let content = div()
            .pt(px(SIDEBAR_DISCLOSURE_BODY_INSET))
            .child(Self::render_pinned_session_group(
                items,
                self.sidebar_transfer_extra_gap("pinned"),
                cx,
            ))
            .into_any_element();
        let body = self.render_sidebar_disclosure_body("pinned", open, body_height, content);
        div()
            .id("sidebar-pinned-section")
            .debug_selector(|| "sidebar-pinned-section".into())
            .flex()
            .flex_col()
            .child(header)
            .child(body)
            .into_any_element()
    }

    pub(in crate::shell) fn render_sessions_section(
        &mut self,
        content: AnyElement,
        body_height: f32,
        count: usize,
        follows_pinned: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.sidebar_shows_overview()
            || self.settings.sidebar_organization != SidebarOrganization::InOneList
        {
            return content;
        }
        let open = self.sessions_open;
        let label = if open {
            "Sessions".into()
        } else {
            format!("Sessions ({count})").into()
        };
        let chevron = self.sidebar_disclosure_chevron("sessions", open, theme);
        let header = sidebar_disclosure_header(theme, label, chevron)
            .id("sessions-toggle")
            .debug_selector(|| "sessions-toggle".into())
            .on_drag_move::<SidebarSessionDrag>(cx.listener(
                |this, event: &gpui::DragMoveEvent<SidebarSessionDrag>, _, _| {
                    if event.bounds.contains(&event.event.position) {
                        let top = f32::from(
                            event.bounds.bottom()
                                - this.sidebar_scroll.bounds().top()
                                - this.sidebar_scroll.offset().y,
                        ) + SIDEBAR_DISCLOSURE_BODY_INSET;
                        if let Some(drag) = this.sidebar_session_transfer.as_mut() {
                            drag.preview = Some(SidebarSessionGap {
                                group: "regular".into(),
                                index: 0,
                                pinned: false,
                                top,
                            });
                        }
                    }
                },
            ))
            .on_drop::<SidebarSessionDrag>(cx.listener(|this, payload, _, cx| {
                this.finish_sidebar_session_transfer(payload, SidebarSessionDrop::Regular, cx);
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.cancel_sidebar_session_transfer(cx);
                let was_open = this.sessions_open;
                this.begin_sidebar_disclosure_motion(
                    "sessions",
                    if was_open { body_height } else { 0.0 },
                    if was_open { 0.0 } else { body_height },
                );
                this.sessions_open = !was_open;
                this.sidebar_prev_order.clear();
                this.sidebar_resort.clear();
                this.sidebar_new_keys.clear();
                cx.notify();
            }));
        let content = div()
            .pt(px(SIDEBAR_DISCLOSURE_BODY_INSET))
            .child(content)
            .into_any_element();
        let body = self.render_sidebar_disclosure_body("sessions", open, body_height, content);
        div()
            .id("sidebar-sessions-section")
            .debug_selector(|| "sidebar-sessions-section".into())
            .flex()
            .flex_col()
            .pt(px(if follows_pinned {
                SIDEBAR_SECTION_GAP
            } else {
                0.0
            }))
            .child(header)
            .child(body)
            .into_any_element()
    }
    /// Archived sessions share active-row data and layout, with a restore action.
    /// The shelf starts with ten sessions and pages by 25.
    pub(in crate::shell) fn render_archived_section(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        #[cfg(test)]
        {
            self.archived_section_builds += 1;
        }
        const INITIAL: usize = 10;
        const PAGE: usize = 25;
        let now = Utc::now();
        let filter = self.settings.space_filter.clone();
        let mut rows: Vec<mast_proto::Chat> = {
            let state = self.state.read(cx);
            let covers = state.personal_assistant.coverage(&state.chats);
            state
                .chats
                .iter()
                // Spawned children stay out of the Archived section too — the
                // same top-level rule as `visible_chats`.
                .filter(|c| c.archived && c.parent_chat_id.is_none() && !c.is_side_chat() && !c.is_standing_role_predecessor())
                .filter(|c| !c.is_background_task_run() && !covers(c))
                .filter(|chat| match &filter {
                    Some(space_id) => chat.space_id.as_deref() == Some(space_id.as_str()),
                    None => true,
                })
                .cloned()
                .collect()
        };
        rows.sort_by(|left, right| compare_sidebar_chats(self.settings.sidebar_sort, left, right));
        if rows.is_empty() {
            return None;
        }
        let rows: Vec<_> = {
            let state = self.state.read(cx);
            rows.into_iter()
                .map(|chat| {
                    self.sidebar_chat_data(state.display_status_for(&chat, now), chat, state)
                })
                .collect()
        };
        let total = rows.len();
        let open = self.archived_open;
        let shown = self.archived_shown.max(INITIAL);
        let visible_count = total.min(shown);
        let has_more = total > shown;
        // "Show more" matches the row slot: compact rows are 29px, so the
        // button shrinks with them instead of towering over the list.
        let more_height = if self.settings.sidebar_compact {
            super::sidebar_row_height(
                true,
                true,
                false,
                false,
                crate::ui_style::current(cx).tokens,
            )
        } else {
            36.0
        };
        let body_height = SIDEBAR_DISCLOSURE_BODY_INSET
            + rows
                .iter()
                .take(shown)
                .map(|row| {
                    sidebar_row_height(
                        self.settings.sidebar_compact,
                        self.settings.sidebar_show_project_label,
                        row.branch.is_some(),
                        row.change_request.is_some(),
                        crate::ui_style::current(cx).tokens,
                    )
                })
                .sum::<f32>()
            + visible_count.saturating_sub(1) as f32 * SIDEBAR_LIST_GAP
            + if has_more {
                more_height + SIDEBAR_LIST_GAP
            } else {
                0.0
            };
        // Match Pinned: a muted label with a right-aligned disclosure chevron.
        // The count only shows while collapsed.
        let label: SharedString = if open {
            "Archived".into()
        } else {
            format!("Archived ({total})").into()
        };
        let chevron = self.sidebar_disclosure_chevron("archived", open, theme);
        let header = sidebar_disclosure_header(theme, label, chevron)
            .id("archived-toggle")
            .on_click(cx.listener(move |this, _, _, cx| {
                let was_open = this.archived_open;
                this.begin_sidebar_disclosure_motion(
                    "archived",
                    if was_open { body_height } else { 0.0 },
                    if was_open { 0.0 } else { body_height },
                );
                this.archived_open = !was_open;
                this.archived_shown = INITIAL;
                cx.notify();
            }));
        let section = div().flex().flex_col().child(header);
        let body = {
            let selected = self.state.read(cx).selected_chat.clone();
            let mut list = div()
                .flex()
                .flex_col()
                .pt(px(SIDEBAR_DISCLOSURE_BODY_INSET))
                .gap(px(SIDEBAR_LIST_GAP));
            for row in rows.into_iter().take(shown) {
                let chat = row.chat;
                let is_selected = selected.as_deref() == Some(chat.id.as_str());
                let harness = self
                    .settings
                    .sidebar_show_harness
                    .then(|| chat.config.as_ref().map(|c| c.harness))
                    .flatten();
                list = list.child(
                    self.render_chat_row_with_chat(
                        chat.id.clone(),
                        None,
                        transcript::single_line(
                            &chat.title.clone().unwrap_or_else(|| "New session".into()),
                        )
                        .into(),
                        format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), now)
                            .into(),
                        row.folder.into(),
                        row.branch.map(SharedString::from),
                        row.change_request,
                        harness,
                        row.status,
                        is_selected,
                        true,
                        false,
                        None,
                        None,
                        None,
                        Some(&chat),
                        theme,
                        cx,
                    ),
                );
            }
            let mut body = div().w_full().flex().flex_col().child(list);
            if has_more {
                let remaining = (total - shown).min(PAGE);
                body = body.child(
                    div()
                        .id("archived-more")
                        // Sits outside the rows' gapped column — match the
                        // list's 2px row gap or it fuses with the last row.
                        .mt(px(2.0))
                        .h(px(more_height))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.0))
                        .px(px(Theme::SPACE_SM))
                        .rounded(px(6.0))
                        .text_size(crate::typography::ui_rems(13.0))
                        .text_color(theme.text_muted.opacity(0.55))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.archived_shown = this.archived_shown.max(INITIAL) + PAGE;
                            cx.notify();
                        }))
                        .child(
                            crate::icons::icon(crate::icons::PLUS)
                                .size(px(14.0))
                                .flex_none(),
                        )
                        .child(SharedString::from(format!("Show {remaining} more"))),
                );
            }
            body.into_any_element()
        };
        let body = self.render_sidebar_disclosure_body("archived", open, body_height, body);
        // The active list already provides the spacing above Archived.
        let section = section.child(body);
        Some(section.into_any_element())
    }
    pub(in crate::shell) fn sidebar_group_menu_button(
        &self,
        id: &str,
        space_id: Option<String>,
        target: SidebarGroupTarget,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let menu_open = match &space_id {
            Some(id) => self.space_menu.get().is_some_and(|(space, _)| space == id),
            None => self
                .group_menu
                .as_ref()
                .is_some_and(|(open, _)| open == &target),
        };
        let selector = id.to_owned();
        crate::components::shell_parts::sidebar_group_menu(
            SharedString::from(format!("sidebar-group-menu-{id}")),
            theme,
            menu_open,
        )
        .debug_selector(move || format!("sidebar-group-menu-{selector}"))
        .on_click(
            cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                this.section_menu = None;
                this.close_sidebar_view_menu(cx);
                if let Some(id) = &space_id {
                    this.group_menu = None;
                    this.space_menu.open((id.clone(), event.position()));
                } else {
                    this.close_space_menu(cx);
                    this.group_menu = Some((target.clone(), event.position()));
                    window.focus(&this.group_menu_focus, cx);
                }
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .child(
            icon(icons::MORE_HORIZONTAL)
                .size(px(14.0))
                .text_color(theme.text_muted),
        )
    }
    pub(in crate::shell) fn render_scheduled_group(
        &mut self,
        rows: Vec<(String, (String, f32, AnyElement))>,
        drag_group: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (String, f32, AnyElement) {
        let total = rows.len();
        let extra_gap = self.sidebar_transfer_extra_gap(drag_group);
        let mut subgroups = Vec::new();
        let mut subgroups_height = 0.0;
        for (task, rows) in group_by_scheduled_task(rows) {
            let key = scheduled_subgroup_key(&task);
            let motion_key = format!("group:{key}");
            let open = !self.sidebar_collapsed_groups.contains(&key);
            let count = rows.len();
            let body_height = SIDEBAR_DISCLOSURE_BODY_INSET
                + rows.iter().map(|(_, height, _)| *height).sum::<f32>()
                + SIDEBAR_LIST_GAP * count.saturating_sub(1) as f32;
            let label: SharedString = if open {
                task.clone().into()
            } else {
                format!("{task} ({count})").into()
            };
            let chevron = self.sidebar_disclosure_chevron(&motion_key, open, theme);
            let toggle_motion_key = motion_key.clone();
            let menu = self.sidebar_group_menu_button(
                &key,
                None,
                SidebarGroupTarget::Scheduled {
                    task: Some(task.clone()),
                },
                theme,
                cx,
            );
            let controls = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(menu)
                .child(chevron);
            let header = sidebar_disclosure_header(theme, label, controls.into_any_element())
                .id(SharedString::from(format!("sidebar-group-{key}")))
                .group("automatic-sidebar-group")
                .on_click(cx.listener(move |this, _, _, cx| {
                    let was_open = !this.sidebar_collapsed_groups.contains(&key);
                    this.begin_sidebar_disclosure_motion(
                        &toggle_motion_key,
                        if was_open { body_height } else { 0.0 },
                        if was_open { 0.0 } else { body_height },
                    );
                    if was_open {
                        this.sidebar_collapsed_groups.insert(key.clone());
                    } else {
                        this.sidebar_collapsed_groups.remove(&key);
                    }
                    cx.notify();
                }));
            let list = div()
                .w_full()
                .flex()
                .flex_col()
                .pt(px(SIDEBAR_DISCLOSURE_BODY_INSET))
                .gap(px(SIDEBAR_LIST_GAP))
                .children(rows.into_iter().map(|(_, _, row)| row));
            let body = self.render_sidebar_disclosure_body(
                &motion_key,
                open,
                body_height,
                list.into_any_element(),
            );
            subgroups_height +=
                SIDEBAR_DISCLOSURE_HEADER_HEIGHT + if open { body_height } else { 0.0 };
            subgroups.push(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .pl(px(Theme::SPACE_SM))
                    .child(header)
                    .child(body),
            );
        }
        let open = self.scheduled_tasks_open;
        let motion_key = format!("group:{SCHEDULED_GROUP_KEY}");
        let body_height = SIDEBAR_DISCLOSURE_BODY_INSET
            + extra_gap
            + subgroups_height
            + SIDEBAR_LIST_GAP * subgroups.len().saturating_sub(1) as f32;
        let body = div()
            .w_full()
            .flex()
            .flex_col()
            .pt(px(SIDEBAR_DISCLOSURE_BODY_INSET))
            .gap(px(SIDEBAR_LIST_GAP))
            .children(subgroups)
            .when(extra_gap > 0.0, |el| {
                el.child(
                    div()
                        .flex_none()
                        .h(px((extra_gap - SIDEBAR_LIST_GAP).max(0.0)))
                        .mb(px((extra_gap - SIDEBAR_LIST_GAP).min(0.0))),
                )
            });
        let label: SharedString = if open {
            "Scheduled tasks".into()
        } else {
            format!("Scheduled tasks ({total})").into()
        };
        let chevron = self.sidebar_disclosure_chevron(&motion_key, open, theme);
        let toggle_motion_key = motion_key.clone();
        let menu = self.sidebar_group_menu_button(
            SCHEDULED_GROUP_KEY,
            None,
            SidebarGroupTarget::Scheduled { task: None },
            theme,
            cx,
        );
        let controls = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(menu)
            .child(chevron);
        let header = sidebar_disclosure_header(theme, label, controls.into_any_element())
            .id(SharedString::from(format!(
                "sidebar-group-{SCHEDULED_GROUP_KEY}"
            )))
            .group("automatic-sidebar-group")
            .on_click(cx.listener(move |this, _, _, cx| {
                let was_open = this.scheduled_tasks_open;
                this.begin_sidebar_disclosure_motion(
                    &toggle_motion_key,
                    if was_open { body_height } else { 0.0 },
                    if was_open { 0.0 } else { body_height },
                );
                this.scheduled_tasks_open = !was_open;
                cx.notify();
            }));
        let body = self.render_sidebar_disclosure_body(
            &motion_key,
            open,
            body_height,
            body.into_any_element(),
        );
        let height = SIDEBAR_DISCLOSURE_SECTION_HEIGHT + if open { body_height } else { 0.0 };
        let element = div()
            .w_full()
            .flex()
            .flex_col()
            .pt(px(SIDEBAR_SECTION_GAP))
            .child(header)
            .child(body)
            .into_any_element();
        (format!("g:{SCHEDULED_GROUP_KEY}"), height, element)
    }

    pub(in crate::shell) fn render_personal_assistant_section(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now = Utc::now();
        let (assistant_data, open_row) = {
            let state = self.state.read(cx);
            let assistant_chat = state.personal_assistant.current(&state.chats).cloned();
            let assistant_id = assistant_chat.as_ref().map(|c| c.id.as_str());
            // Keep the open session reachable even outside the current filter,
            // whether or not a personal assistant is assigned.
            let open_row = state
                .selected_chat_row()
                .filter(|open| !open.is_background_task_run())
                .filter(|open| assistant_id != Some(open.id.as_str()))
                .map(|open| {
                    self.sidebar_chat_data(state.display_status_for(open, now), open.clone(), state)
                });
            let assistant_data = assistant_chat.map(|chat| {
                let selected = state
                    .selected_chat_row()
                    .is_some_and(|selected| state.personal_assistant.covers(selected, &state.chats));
                (
                    self.sidebar_chat_data(state.display_status_for(&chat, now), chat, state),
                    selected,
                )
            });
            (assistant_data, open_row)
        };
        if assistant_data.is_none() && open_row.is_none() {
            return None;
        }
        let open_element = open_row.map(|row| {
            let chat = row.chat;
            let harness = self
                .settings
                .sidebar_show_harness
                .then(|| chat.config.as_ref().map(|c| c.harness))
                .flatten();
            self.render_chat_row(
                chat.id.clone(),
                Some("current-session"),
                transcript::single_line(chat.title.as_deref().unwrap_or("New session")).into(),
                format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), now).into(),
                row.folder.into(),
                row.branch.map(SharedString::from),
                row.change_request,
                harness,
                row.status,
                true,
                chat.archived,
                false,
                None,
                None,
                None,
                theme,
                cx,
            )
        });
        let has_assistant = assistant_data.is_some();
        let assistant_block = assistant_data.map(|(row, selected)| {
            let chat = row.chat;
            let harness = self
                .settings
                .sidebar_show_harness
                .then(|| chat.config.as_ref().map(|c| c.harness))
                .flatten();
            let element = self.render_chat_row(
                chat.id.clone(),
                None,
                transcript::single_line(
                    &chat.title.clone().unwrap_or_else(|| "New session".into()),
                )
                .into(),
                format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), now).into(),
                row.folder.into(),
                row.branch.map(SharedString::from),
                row.change_request,
                harness,
                row.status,
                selected,
                chat.archived,
                false,
                None,
                None,
                None,
                theme,
                cx,
            );
            div()
                .id("sidebar-personal-assistant")
                .debug_selector(|| "sidebar-personal-assistant".into())
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(SIDEBAR_DISCLOSURE_HEADER_HEIGHT))
                        .flex()
                        .items_center()
                        .px(px(Theme::SPACE_SM))
                        .child(crate::components::shell_parts::sidebar_section_label(
                            theme,
                            crate::personal_assistant::SECTION_LABEL.into(),
                        )),
                )
                .child(element)
        });
        Some(
            div()
                .id("sidebar-top-session")
                .flex_none()
                .flex()
                .flex_col()
                .px(px(Theme::SPACE_SM))
                .pt(px(8.0))
                .when_some(assistant_block, |el, block| el.child(block))
                .when_some(open_element, |el, row| {
                    el.child(
                        div()
                            .id("sidebar-current-session")
                            .debug_selector(|| "sidebar-current-session".into())
                            .when(!has_assistant, |el| {
                                el.child(
                                    div()
                                        .h(px(SIDEBAR_DISCLOSURE_HEADER_HEIGHT))
                                        .flex()
                                        .items_center()
                                        .px(px(Theme::SPACE_SM))
                                        .child(
                                            crate::components::shell_parts::sidebar_section_label(
                                                theme,
                                                "Active session".into(),
                                            ),
                                        ),
                                )
                            })
                            .when(has_assistant, |el| el.pt(px(SIDEBAR_LIST_GAP)))
                            .child(row),
                    )
                })
                .into_any_element(),
        )
    }
}
