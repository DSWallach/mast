use super::*;

impl Changes {
    // ---- rendering ----

    pub(super) fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(parsed) = &self.parsed else {
            return gpui::Empty.into_any_element();
        };
        let files = parsed.files.clone();
        let origins = parsed.origins.clone();
        let submodules = parsed.submodules.clone();
        let parsed_key = parsed.key.clone();
        let Some(row) = self.rows.get(ix).copied() else {
            return gpui::Empty.into_any_element();
        };
        let theme = Theme::of(cx).clone();
        if let DiffRow::SubmoduleHeader { group } = row {
            let group_ix = group as usize;
            let Some(group) = submodules.get(group_ix) else {
                return gpui::Empty.into_any_element();
            };
            let collapsed = self
                .submodule_folds
                .get(&group.repository_path)
                .copied()
                .unwrap_or(false);
            return self.render_submodule_header(group_ix, group, collapsed, &theme, cx);
        }
        let Some(file_ix) = row.file() else {
            return gpui::Empty.into_any_element();
        };
        let highlight = files
            .get(file_ix)
            .zip(origins.get(file_ix))
            .and_then(|(file, origin)| self.request_highlight(file, origin, &parsed_key, cx));
        let hunk_index = match row {
            DiffRow::Line { hunk, .. } | DiffRow::SplitLine { hunk, .. } => Some(hunk),
            _ => None,
        };
        let word_spans = hunk_index.and_then(|hunk| files.get(file_ix)?.hunks.get(hunk as usize)
            .and_then(|data| self.request_word_spans((parsed_key.clone(), file_ix as u32, hunk), data, cx)));
        let horizontal = &self.parsed.as_ref().unwrap().horizontal[file_ix];
        let code_width = match files.get(file_ix) {
            Some(file) if !self.wrap_lines => {
                let mut metrics = horizontal.metrics(
                file,
                highlight.as_ref(),
                &theme,
                window.text_system(),
                crate::typography::generation(cx),
                );
                if let Some(expand_context::Entry::Ready { max_columns, max_line, .. }) = self.expanded_context.entries.get(&(parsed_key.clone(), file_ix as u32)) {
                    let advance = diff_ch_advance(window.text_system(), &theme, crate::typography::generation(cx));
                    metrics.max_text_width = metrics.max_text_width.max(*max_columns as f32 * advance);
                    let digits = (*max_line).max(1).ilog10() + 1;
                    metrics.max_gutter_width = metrics.max_gutter_width.max((digits as f32 * 6.6 + 14.0).max(GUTTER_WIDTH));
                }
                DiffCodeWidth::Scrollable(metrics)
            },
            _ => DiffCodeWidth::Wrapped,
        };
        let code_scroll = DiffCodeScrollContext {
            handle: horizontal.scroll.clone(),
            prefix: SharedString::from(format!("changes-code-row-{ix}")),
        };
        let file_gutter_width = files.get(file_ix).map(|file| {
            let max_line = match self.expanded_context.entries.get(&(parsed_key.clone(), file_ix as u32)) {
                Some(expand_context::Entry::Ready { max_line, .. }) => file.max_line.max(*max_line),
                _ => file.max_line,
            };
            let digits = max_line.max(1).ilog10() + 1;
            (digits as f32 * 6.6 + 14.0).max(GUTTER_WIDTH)
        }).unwrap_or(GUTTER_WIDTH);
        match row {
            DiffRow::SubmoduleHeader { .. } => gpui::Empty.into_any_element(),
            DiffRow::ContextControl { file, gap } => {
                let entry = self.expanded_context.entries.get(&(parsed_key.clone(), file));
                let loading = matches!(entry, Some(expand_context::Entry::Loading));
                let failed = matches!(entry, Some(expand_context::Entry::Failed));
                let footer = files.get(file as usize).is_some_and(|diff| gap as usize == diff.hunks.len());
                let remaining = match entry {
                    Some(expand_context::Entry::Ready { gaps, shown, .. }) => gaps.get(gap as usize).zip(shown.get(gap as usize)).map(|(gap, shown)| gap.lines.len().saturating_sub(*shown)),
                    _ => files.get(file as usize).and_then(|diff| expand_context::known_gap_size(diff, gap as usize)),
                };
                let label = if loading { "Loading…".to_owned() } else if failed { "Context unavailable".to_owned() } else if let Some(count) = remaining { format!("{} {} more {}", if footer { "↓" } else { "↑" }, count.min(20), if count == 1 { "line" } else { "lines" }) } else { "↓ More lines".to_owned() };
                div().w_full().h(px(HUNK_HEADER_HEIGHT)).flex().justify_center().items_center()
                    .text_color(theme.text_muted).text_size(px(10.0))
                    .child(div().id(("expand-context", ix)).cursor_pointer().role(gpui::Role::Button)
                        .aria_label("Expand context; Option-click to expand all")
                        .tooltip(|_, cx| cx.new(|_| DiffHeaderTooltip("Option-click to expand all".into())).into()).child(label)
                        .on_click(cx.listener(move |this, _, window, cx| this.expand_context(file, gap, window.modifiers().alt, cx))))
                    .into_any_element()
            }
            DiffRow::ContextLine { file, gap, line } => {
                let Some(expand_context::Entry::Ready { gaps, .. }) = self.expanded_context.entries.get(&(parsed_key.clone(), file)) else { return gpui::Empty.into_any_element(); };
                let Some(line) = gaps.get(gap as usize).and_then(|gap| gap.lines.get(line as usize)) else { return gpui::Empty.into_any_element(); };
                let gutter = file_gutter_width;
                if self.mode == DiffMode::Unified {
                    diff_line_row(line, &[], &[], &theme, gutter, code_width, Some(code_scroll.slot("unified")))
                } else {
                    let runs = line_runs(line, None, &theme);
                    let left = split_line_cell(line, line.old_no, runs.clone(), &theme, gutter, code_width, Some(code_scroll.slot("old"))).into_any_element();
                    let right = split_line_cell(line, line.new_no, runs, &theme, gutter, code_width, Some(code_scroll.slot("new"))).into_any_element();
                    split_row(left, right, self.wrap_lines, &theme).into_any_element()
                }
            }
            DiffRow::FileHeader { file } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let fold = self.folds.get(&file_diff.path).copied().unwrap_or_default();
                self.render_file_header(
                    file as usize,
                    file_diff,
                    origins
                        .get(file as usize)
                        .map(|origin| origin.nesting_depth)
                        .unwrap_or(0),
                    &fold,
                    FileHeaderPresentation::Row,
                    &theme,
                    cx,
                )
            }
            DiffRow::Notice { file, notice } => files
                .get(file as usize)
                .and_then(|f| file_notices(f).into_iter().nth(notice as usize))
                .map(|text| notice_row(text, &theme))
                .unwrap_or_else(|| gpui::Empty.into_any_element()),
            DiffRow::HunkHeader { file, hunk } => {
                let Some(file_diff) = files.get(file as usize) else { return gpui::Empty.into_any_element(); };
                let Some(hunk_diff) = file_diff.hunks.get(hunk as usize) else { return gpui::Empty.into_any_element(); };
                let Some(identity) = self.parsed.as_ref().and_then(|parsed| parsed.hunk_identities.get(file as usize)).and_then(|items| items.get(hunk as usize)) else { return gpui::Empty.into_any_element(); };
                let viewed = self.review_state.as_ref().is_some_and(|state| state.viewed_hunks.contains(identity));
                let seen = self.seen_hunks.contains(identity);
                let label = if self.review_closed { "PR closed" } else if self.review_error.is_some() { "Review unavailable" } else if viewed { "Viewed" } else if seen { "· Mark viewed" } else { "Mark viewed" };
                // Latest Turn's deleted-only preimage in the checkout's own repository is a tree snapshot, which CodeRef
                // cannot yet name. Expanded submodule hunks anchor to the submodule's own commits, so they never hit that.
                let nested_repository = origins.get(file as usize).is_some_and(|origin| origin.repository_path.is_some());
                let show_reference = nested_repository || !(self.scope == DiffScope::LatestTurn && code_ref::deleted_only_hunk(&hunk_diff.header));
                let symbol = highlight.as_ref().and_then(|highlight| highlight.hunk_symbols.get(hunk as usize)).and_then(Option::as_deref);
                let comment_anchor = hunk_diff.lines.iter().find_map(|line| line.new_no.map(|number| (CommentSide::New, number)).or_else(|| line.old_no.map(|number| (CommentSide::Old, number))));
                let comment_path = file_diff.path.clone();
                div().relative().child(hunk_header_row(&hunk_diff.header, symbol, &theme))
                    .when_some(comment_anchor.filter(|_| self.adder_block(file, hunk, CommentSide::New) != Some(super::rereview::REMOVED_COMMENT_MESSAGE)), |row, (side, line)| row.child(
                        crate::components::button(SharedString::from(format!("changes-hunk-comment-{file}-{hunk}")), "Comment", crate::components::ButtonVariant::Secondary, crate::components::ButtonSize::Sm, &theme)
                            .absolute().right(px(210.0)).top(px(Theme::SPACE_XS / 2.0))
                            .on_click(cx.listener(move |this, _, window, cx| this.open_draft(comment_path.clone(), side, line, window, cx)))))
                    .when(show_reference, |row| row.child(
                        crate::components::button(SharedString::from(format!("changes-hunk-chat-{file}-{hunk}")), "Add hunk to chat", crate::components::ButtonVariant::Secondary, crate::components::ButtonSize::Sm, &theme)
                            .absolute().right(px(100.0)).top(px(Theme::SPACE_XS / 2.0))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_indexed_hunk_to_chat(file as usize, hunk as usize, cx);
                            }))))
                    .when((self.review_state.is_some() || self.review_closed || self.review_error.is_some()) && !self.rereview_active(), |row| row.child(
                        crate::components::button(SharedString::from(format!("changes-hunk-viewed-{file}-{hunk}")), label, crate::components::ButtonVariant::Secondary, crate::components::ButtonSize::Sm, &theme)
                        .absolute().right(px(Theme::SPACE_SM)).top(px(Theme::SPACE_XS / 2.0))
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_review_hunk(file as usize, hunk as usize, cx)))))
                    .into_any_element()
            },
            DiffRow::Line {
                file,
                hunk,
                line,
                flat: _,
            } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let line_index = line;
                let Some(line) = file_diff
                    .hunks
                    .get(hunk as usize)
                    .and_then(|h| h.lines.get(line as usize))
                else {
                    return gpui::Empty.into_any_element();
                };
                let spans = highlight
                    .as_deref()
                    .map(|highlights| highlights.spans(line))
                    .unwrap_or(&[]);
                let gutter_px = file_gutter_width;
                let row = diff_line_row(
                    line,
                    spans,
                    word_spans.as_ref().and_then(|spans| spans.get(line_index as usize)).map(Vec::as_slice).unwrap_or(&[]),
                    &theme,
                    gutter_px,
                    code_width,
                    Some(code_scroll.slot("unified")),
                );
                let Some((side, line_no)) = line_anchor(line) else {
                    return row;
                };
                let path = file_diff.path.clone();
                let source_side = match side { CommentSide::Old => SourceSide::Old, CommentSide::New => SourceSide::New };
                let hovered = self.hovering(&path, (side, line_no));
                let move_path = path.clone();
                let leave_path = path.clone();
                let select_path = path.clone();
                let selected = self.explanation_unified_rows.map_or_else(|| self.explanation_line_selected(&path, source_side, line_no), |(start, end, _)| self.explanation_selection.as_ref().is_some_and(|s| s.path == path) && (start.min(end)..=start.max(end)).contains(&ix));
                let selection_end = self.explanation_unified_rows.map_or_else(|| self.explanation_selection.as_ref().is_some_and(|s| s.path == path && s.side == source_side && s.extent == line_no), |(_, end, _)| end == ix && self.explanation_selection.as_ref().is_some_and(|s| s.path == path));
                div()
                    .id(("diff-line", ix))
                    .w_full()
                    .relative()
                    .when(selected, |el| el.bg(theme.element_active))
                    .child(row)
                    .when(selection_end, |el| el.child(self.render_explanation(&theme, cx)))
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                        window.focus(&this.diff_focus, cx);
                        this.select_unified_explanation_line(&select_path, source_side, line_no, ix, event.modifiers.shift, cx);
                    }))
                    .when(hovered, |el| {
                        el.child(positioned_adder(
                            comment_adder_left(side, gutter_px),
                            render_comment_adder(&path, side, line_no, &theme, self.adder_block(file, hunk, side), cx),
                        ))
                    })
                    .on_mouse_move(cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                        if event.dragging() && this.explanation_selection.as_ref().is_some_and(|s| s.path == move_path) {
                            this.select_unified_explanation_line(&move_path, source_side, line_no, ix, true, cx);
                        }
                        this.set_hover(&move_path, Some((side, line_no)), cx);
                    }))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if !*hovered {
                            this.clear_hover_at(&leave_path, (side, line_no), cx);
                        }
                    }))
                    .into_any_element()
            }
            DiffRow::SplitLine {
                file,
                hunk,
                left,
                right,
            } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let Some(lines) = file_diff.hunks.get(hunk as usize).map(|h| &h.lines) else {
                    return gpui::Empty.into_any_element();
                };
                let gutter_px = file_gutter_width;
                // Same slot on both sides = a context row: one line, drawn in
                // both columns.
                let mirrored = left.is_some() && left == right;
                let (left_slot, right_slot) = (left, right);
                let left = left.and_then(|slot| lines.get(slot as usize));
                let right = right.and_then(|slot| lines.get(slot as usize));
                // `\ No newline at end of file` is not code on one side — it
                // is a note about the row, so it spans both columns. Pairing
                // never puts a marker opposite code, so either side having one
                // means the whole row is the marker.
                if let Some(line) = [left, right]
                    .into_iter()
                    .flatten()
                    .find(|line| line.kind == LineKind::Meta)
                {
                    return meta_line_row(&line.text, &theme, 2.0 * (ACCENT_BAR_WIDTH + gutter_px));
                }
                // Refcounted, not cloned per listener: a split row wires up to
                // four of them, and this runs for every row in the viewport
                // plus the list's overdraw, every frame.
                let path: SharedString = file_diff.path.clone().into();
                // A mirrored row's columns carry the same text and the same
                // spans, so the runs are built once and shared — context is
                // most of a diff, so this is most of the rows.
                let shared_runs = mirrored
                    .then(|| left.map(|line| line_runs(line, highlight.as_deref(), &theme)))
                    .flatten();
                let cell = |line: Option<&DiffLine>, slot: Option<u32>, old: bool| {
                    line.map(|line| {
                        let runs = shared_runs
                            .clone()
                            .unwrap_or_else(|| line_runs(line, highlight.as_deref(), &theme));
                        let ranges = slot.and_then(|slot| word_spans.as_ref()?.get(slot as usize)).map(Vec::as_slice).unwrap_or(&[]);
                        let runs = word_diff::tint_runs(runs, ranges, word_tint(line, &theme));
                        let number = if old { line.old_no } else { line.new_no };
                        split_line_cell(
                            line,
                            number,
                            runs,
                            &theme,
                            gutter_px,
                            code_width,
                            Some(code_scroll.slot(if old { "old" } else { "new" })),
                        )
                    })
                };
                // The left column is inert. It shows the pre-change file, and
                // a deleted line is not there to be changed — a note on it
                // would cite a line the agent cannot edit. Everything is cited
                // against the new file, so only the right column takes a `+`.
                // Cards for old-side notes still render (they are pushed by
                // the row, not the column), so switching layouts never hides
                // one that is already staged.
                let left_number = left.and_then(|line| line.old_no);
                let left_path = path.clone();
                let left_move_path = path.clone();
                let left = cell(left, left_slot, true)
                    .map(|cell| cell.id(("split-old", ix)).relative()
                        .when(left_number.is_some_and(|number| self.explanation_line_selected(&path, SourceSide::Old, number)), |el| el.bg(theme.element_active))
                        .when(left_number.is_some_and(|number| self.explanation_selection.as_ref().is_some_and(|s| s.path == path.as_ref() && s.side == SourceSide::Old && s.extent == number)), |el| el.child(self.render_explanation(&theme, cx)))
                        .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            window.focus(&this.diff_focus, cx);
                            if let Some(number) = left_number { this.select_explanation_line(&left_path, SourceSide::Old, number, event.modifiers.shift, cx); }
                        }))
                        .on_mouse_move(cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                            if let Some(number) = left_number && event.dragging() && this.explanation_selection.as_ref().is_some_and(|s| s.path == left_move_path.as_ref() && s.side == SourceSide::Old) {
                                this.select_explanation_line(&left_move_path, SourceSide::Old, number, true, cx);
                            }
                        }))
                        .into_any_element())
                    .unwrap_or_else(|| split_filler().into_any_element());
                let right = match (cell(right, right_slot, false), right.and_then(line_anchor)) {
                    (Some(cell), Some(anchor)) => {
                        let (side, line_no) = anchor;
                        let (move_path, leave_path) = (path.clone(), path.clone());
                        let select_path = path.clone();
                        cell.id(("split-new", ix)).relative()
                            .when(self.explanation_line_selected(&path, SourceSide::New, line_no), |el| el.bg(theme.element_active))
                            .when(self.explanation_selection.as_ref().is_some_and(|s| s.path == path.as_ref() && s.side == SourceSide::New && s.extent == line_no), |el| el.child(self.render_explanation(&theme, cx)))
                            .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                window.focus(&this.diff_focus, cx);
                                this.select_explanation_line(&select_path, SourceSide::New, line_no, event.modifiers.shift, cx);
                            }))
                            .when(self.hovering(&path, anchor), |el| {
                                el.relative().child(positioned_adder(
                                    split_adder_left(gutter_px),
                                    render_comment_adder(&path, side, line_no, &theme, self.adder_block(file, hunk, side), cx),
                                ))
                            })
                            .on_mouse_move(cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                                if event.dragging() && this.explanation_selection.as_ref().is_some_and(|s| s.path == move_path.as_ref() && s.side == SourceSide::New) {
                                    this.select_explanation_line(&move_path, SourceSide::New, line_no, true, cx);
                                }
                                this.set_hover(&move_path, Some(anchor), cx);
                            }))
                            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                if !*hovered {
                                    this.clear_hover_at(&leave_path, anchor, cx);
                                }
                            }))
                            .into_any_element()
                    }
                    (Some(cell), None) => cell.into_any_element(),
                    (None, _) => split_filler().into_any_element(),
                };
                split_row(left, right, self.wrap_lines, &theme).into_any_element()
            }
            DiffRow::CommentCard { file, card } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let comment = self.comment_index.get(&file_diff.path).and_then(|comments| comments.get(card as usize)).cloned();
                match comment.as_ref() {
                    Some(comment) if self.review_suggestions.contains_key(&comment.id) => self.render_suggestion_card(comment, &theme, cx),
                    Some(comment) if self.is_local_draft(&comment.id) => self.render_local_draft_card(comment, &theme, cx),
                    Some(comment) => crate::comment_ui::render_comment_card(
                        comment,
                        &theme,
                        cx,
                        Self::edit_comment,
                        Self::remove_comment,
                        None,
                    ),
                    None => gpui::Empty.into_any_element(),
                }
            }
            DiffRow::CommentDraft { file } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                match self
                    .draft
                    .as_ref()
                    .filter(|draft| draft.path == file_diff.path)
                {
                    // Header cites the same path the staged card and the
                    // prompt bullet will.
                    Some(draft) => crate::comment_ui::render_comment_draft(
                        draft_cite_path(draft),
                        draft.line,
                        draft.input.clone(),
                        draft.editing_id.is_some(),
                        &theme,
                        cx,
                        Self::cancel_draft,
                        Self::commit_draft,
                        None,
                    ),
                    None => gpui::Empty.into_any_element(),
                }
            }
            DiffRow::ReviewThread { thread, height, .. } => {
                self.render_review_thread(thread as usize, height, &theme, cx)
            }
            DiffRow::BodyPad { .. } => div().w_full().h(px(BODY_BOTTOM_PAD)).into_any_element(),
            DiffRow::FoldingBody { file } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let fold = self.folds.get(&file_diff.path).copied().unwrap_or_default();
                let (from, to) = (fold.from, fold.to);
                // Only the revealable slice is built — the tween never pays
                // for lines it cannot show.
                let cap = from.max(to).min(FOLD_TWEEN_MAX_PX);
                let body = render_file_body_upto(
                    file_diff,
                    highlight,
                    &theme,
                    cap,
                    self.mode,
                    code_width,
                    Some(DiffCodeScrollContext {
                        handle: code_scroll.handle.clone(),
                        prefix: SharedString::from(format!(
                            "changes-fold-code-{file}-{}",
                            fold.epoch
                        )),
                    }),
                );
                let clipped = div().w_full().overflow_hidden().child(body);
                if fold.animating() {
                    clipped
                        .with_animation(
                            SharedString::from(format!("fold-{}-{}", file_diff.path, fold.epoch)),
                            COLLAPSE.animation(),
                            move |el, t| el.h(px(motion::lerp(from, to, t))),
                        )
                        .into_any_element()
                } else {
                    // Post-tween, pre-settle: hold the full target height so
                    // the settle splice swaps rows without any reflow (the
                    // capped slice always covers what the viewport can see —
                    // tweens start from a clicked, on-screen header).
                    clipped.h(px(to)).into_any_element()
                }
            }
        }
    }

    pub(super) fn render_submodule_header(
        &mut self,
        group_ix: usize,
        group: &SubmoduleGroup,
        collapsed: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let repository_path = group.repository_path.clone();
        let revision = match (&group.old_revision, &group.new_revision) {
            (Some(old), Some(new)) => Some(format!(
                "{} → {}",
                &old[..old.len().min(7)],
                &new[..new.len().min(7)]
            )),
            _ => None,
        };
        let left_pad = Theme::SPACE_MD + group.nesting_depth as f32 * Theme::SPACE_LG;
        div()
            .id(("submodule-hdr", group_ix))
            .w_full()
            .h(px(FILE_HEADER_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .pl(px(left_pad))
            .pr(px(Theme::SPACE_MD))
            .bg(theme.ink(0.025))
            .cursor_pointer()
            .when(true, |element| crate::components::changes_diff_header(element, theme, theme.ink(0.05)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_submodule(group_ix, cx);
            }))
            .child(
                crate::icons::icon(if collapsed {
                    crate::icons::ALT_ARROW_RIGHT
                } else {
                    crate::icons::ALT_ARROW_DOWN
                })
                .size(px(13.0))
                .text_color(theme.text_muted.opacity(0.7)),
            )
            .child(
                crate::file_icons::icon(
                    crate::file_icons::FileIconIdentity::directory(&repository_path, false),
                    theme.appearance,
                )
                .size(px(14.0))
                .flex_none(),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .child(SharedString::from(repository_path)),
            )
            .when_some(revision, |el, revision| {
                el.child(
                    div()
                        .flex_none()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(10.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(revision)),
                )
            })
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(format!(
                        "{} {}",
                        group.file_count,
                        if group.file_count == 1 {
                            "file"
                        } else {
                            "files"
                        }
                    ))),
            )
            .when(group.truncated, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(9.0))
                        .text_color(theme.warning.opacity(0.8))
                        .child("PARTIAL"),
                )
            })
            .when(group.dirty, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(9.0))
                        .px(px(5.0))
                        .py(px(1.0))
                        .when(true, |el| crate::components::changes_warning_badge(el, theme, crate::components::ChangesWarning::Dirty))
                        .child("DIRTY"),
                )
            })
            .child(
                div()
                    .flex_none()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(11.0))
                    .text_color(add_color(theme))
                    .child(SharedString::from(format!("+{}", group.additions))),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(11.0))
                    .text_color(del_color(theme))
                    .child(SharedString::from(format!("−{}", group.deletions))),
            )
            .into_any_element()
    }

    pub(super) fn render_file_header(
        &mut self,
        ix: usize,
        file: &FileDiff,
        nesting_depth: usize,
        fold: &FileFold,
        presentation: FileHeaderPresentation,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let collapsed = fold.collapsed;
        let path = file.path.clone();
        let adds = file.additions;
        let dels = file.deletions;
        let sticky = presentation == FileHeaderPresentation::Sticky;
        let sticky_paint = sticky.then(|| sticky_file_header_paint(theme));
        let rest_bg = if let Some(paint) = sticky_paint {
            paint.rest_bg
        } else {
            theme.ink(0.025)
        };
        let hover_bg = if let Some(paint) = sticky_paint {
            paint.hover_bg
        } else {
            theme.ink(0.05)
        };

        // Chevron (zeron checkout-diff-sidebar): chevron-right closed,
        // chevron-down open; gpui divs have no rotation transform at the
        // pinned rev, so the glyph swap crossfades over the same 200 ms.
        let chevron_icon = if collapsed {
            crate::icons::ALT_ARROW_RIGHT
        } else {
            crate::icons::ALT_ARROW_DOWN
        };
        let chevron = div().flex_none().size(px(14.0)).child(
            crate::icons::icon(chevron_icon)
                .size(px(13.0))
                .text_color(theme.text_muted.opacity(0.7)),
        );
        let chevron: AnyElement = if fold.animating() {
            chevron
                .with_animation(
                    SharedString::from(format!(
                        "chev-{}-{path}-{}",
                        presentation.key_prefix(),
                        fold.epoch
                    )),
                    CHEVRON.animation(),
                    |el, t| el.opacity(0.25 + 0.75 * t),
                )
                .into_any_element()
        } else {
            chevron.into_any_element()
        };

        // Header row: chevron + path + right-aligned
        // +N / −N counts on a slightly raised wash. The header carries the
        // section separator (the per-file wrapper it used to hang on is
        // gone — rows are flat now).
        div()
            .id(presentation.element_id(ix))
            .w_full()
            .h(px(FILE_HEADER_HEIGHT))
            .when(
                presentation == FileHeaderPresentation::Row && ix > 0,
                |el| el.border_t_1().border_color(crate::theme::hairline(0.04)),
            )
            .when(sticky, |el| {
                el.border_b_1()
                    .border_color(sticky_paint.expect("sticky paint").border)
                    .block_mouse_except_scroll()
            })
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .pl(px(Theme::SPACE_MD + nesting_depth as f32 * 16.0))
            .pr(px(Theme::SPACE_MD))
            .bg(rest_bg)
            .cursor_pointer()
            .when(true, |element| crate::components::changes_diff_header(element, theme, hover_bg))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.toggle_fold(ix, cx);
                window.focus(&this.diff_focus, cx);
                cx.notify();
            }))
            .child(chevron)
            .child(
                crate::file_icons::icon(
                    crate::file_icons::FileIconIdentity::file(&file.path),
                    theme.appearance,
                )
                .size(px(14.0))
                .flex_none(),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .overflow_hidden()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(12.0))
                    .text_color(theme.text_faint)
                    .child(div().truncate().child(SharedString::from(file.path.rsplit_once('/').map_or(String::new(), |(dir, _)| format!("{dir}/")))))
                    .child(div().flex_none().font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme.text).child(SharedString::from(file.path.rsplit('/').next().unwrap_or(&file.path).to_owned()))),
            )
            .when(collapsed && is_large_file(file), |el| {
                el.child(div().flex_none().text_size(px(10.0)).text_color(theme.text_muted).child("Load diff"))
            })
            .when(file.binary, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(10.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from("BIN")),
                )
            })
            .when(adds > 0 || !file.binary, |el| {
                el.child(
                    div()
                        .flex_none()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.0))
                        .text_color(add_color(theme))
                        .child(SharedString::from(format!("+{adds}"))),
                )
            })
            .when(dels > 0 || !file.binary, |el| {
                el.child(
                    div()
                        .flex_none()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.0))
                        .text_color(del_color(theme))
                        .child(SharedString::from(format!("−{dels}"))),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_sticky_file_header(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let scroll_top = self.list.logical_scroll_top();
        let sticky = sticky_file_header(
            &self.row_ranges,
            &self.section_headers,
            scroll_top.item_ix,
            scroll_top.offset_in_item.as_f32(),
        )?;
        debug_assert_eq!(
            self.rows.get(sticky.header_row),
            Some(&DiffRow::FileHeader {
                file: sticky.file_ix as u32,
            })
        );
        let files = self.parsed.as_ref()?.files.clone();
        let origins = self.parsed.as_ref()?.origins.clone();
        let file = files.get(sticky.file_ix)?;
        let fold = self.folds.get(&file.path).copied().unwrap_or_default();
        let next_header_y = sticky.next_header_row.and_then(|row| {
            let bounds = self.list.bounds_for_item(row)?;
            let viewport = self.list.viewport_bounds();
            Some((bounds.origin.y - viewport.origin.y).as_f32())
        });
        let top_offset = sticky_header_push_offset(next_header_y);
        let header = self.render_file_header(
            sticky.file_ix,
            file,
            origins
                .get(sticky.file_ix)
                .map(|origin| origin.nesting_depth)
                .unwrap_or(0),
            &fold,
            FileHeaderPresentation::Sticky,
            theme,
            cx,
        );
        let paint = sticky_file_header_paint(theme);
        // The sticky floats over diff rows, but it belongs to the same content
        // plane. Tint the blur with `theme.bg`; `glass_overlay` is deliberately
        // reserved for elevated menus/cards and produced the wrong hue here.
        let header = if let Some(tint) = paint.frost_tint {
            div().w_full().bg(tint).child(header).into_any_element()
        } else {
            header
        };
        // Frosted is a pass-through when the resolved surface is opaque.
        let header = crate::frost::frosted(0.0, STICKY_FILE_HEADER_BLUR, header);

        Some(
            div()
                .absolute()
                .top(px(top_offset))
                .left_0()
                .w_full()
                .child(header)
                .into_any_element(),
        )
    }

}
