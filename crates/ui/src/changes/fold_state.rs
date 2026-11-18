use super::*;

impl Changes {
    pub(super) fn replace_file_body(&mut self, file_ix: usize, new_body: Vec<DiffRow>) {
        let new_body = if let Some(parsed) = &self.parsed {
            expand_context::add_rows(new_body, &parsed.files, &parsed.key, &self.expanded_context).0
        } else { new_body };
        let new_body = self.add_thread_rows(file_ix, new_body);
        if self.explanation_selection.take().is_some() {
            self.explanation_unified_rows = None;
            self.explanation_hunk = None;
            self.cancel_explanation();
        }
        self.seen_candidates.clear();
        let Some(range) = self.row_ranges.get(file_ix).and_then(Clone::clone) else {
            return;
        };
        let body = range.start + 1..range.end;
        let delta = new_body.len() as isize - body.len() as isize;
        // Only splice the rows that moved: `ListState::splice` clamps the
        // scroll anchor to the range start when the anchored row is inside it,
        // so replacing a whole body jumped the pane to the top of the file.
        let (prefix, suffix) = {
            let old = &self.rows[body.clone()];
            let prefix = old
                .iter()
                .zip(&new_body)
                .take_while(|(a, b)| a == b)
                .count();
            let suffix = old[prefix..]
                .iter()
                .rev()
                .zip(new_body[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            (prefix, suffix)
        };
        if delta == 0 && prefix + suffix >= body.len() {
            return;
        }
        let changed = body.start + prefix..body.end - suffix;
        let mid: Vec<DiffRow> = new_body[prefix..new_body.len() - suffix].to_vec();
        self.list.splice(changed.clone(), mid.len());
        self.rows.splice(changed, mid);
        self.row_ranges[file_ix] = Some(range.start..(range.end as isize + delta) as usize);
        for (ix, range) in self.row_ranges.iter_mut().enumerate() {
            if ix == file_ix {
                continue;
            }
            let Some(range) = range else {
                continue;
            };
            if range.start >= body.end {
                *range =
                    (range.start as isize + delta) as usize..(range.end as isize + delta) as usize;
            }
        }
        for header in &mut self.section_headers {
            if *header >= body.end {
                *header = (*header as isize + delta) as usize;
            }
        }
    }

    pub(super) fn toggle_fold(&mut self, file_ix: usize, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed else {
            return;
        };
        let Some(file) = parsed.files.get(file_ix) else {
            return;
        };
        if self.wrap_lines {
            let collapsed = !self
                .folds
                .get(&file.path)
                .is_some_and(|fold| fold.collapsed);
            let body = if collapsed {
                Vec::new()
            } else {
                body_rows(
                    file_ix as u32,
                    file,
                    &self.comments_for(&file.path, cx),
                    self.draft_anchor_in(&file.path),
                    self.mode,
                )
            };
            let fold = self.folds.entry(file.path.clone()).or_default();
            fold.collapsed = collapsed;
            fold.toggled_at = None;
            self.replace_file_body(file_ix, body);
            cx.notify();
            return;
        }
        let ui_scale = crate::typography::effective_rem_px(cx) / 16.0;
        let expanded_height = body_height_with_card_heights(
            file,
            &self.comments_for(&file.path, cx),
            self.draft_anchor_in(&file.path),
            self.mode,
            diff_line_height(Theme::of(cx)),
            |comment| {
                let height = comments::card_height(&comment.body);
                if self.review_suggestions.contains_key(&comment.id) || self.is_local_draft(&comment.id) {
                    height * ui_scale
                } else {
                    height
                }
            },
        );
        let fold = self.folds.entry(file.path.clone()).or_default();
        let currently_collapsed = fold.collapsed;
        fold.from = if currently_collapsed {
            0.0
        } else {
            expanded_height
        };
        fold.to = if currently_collapsed {
            expanded_height
        } else {
            0.0
        };
        fold.collapsed = !currently_collapsed;
        fold.epoch += 1;
        fold.toggled_at = Some(std::time::Instant::now());
        // The body tweens as ONE clipped stand-in row; the settle sweep
        // swaps it for steady rows (all lines, or none) once the window
        // elapses.
        self.replace_file_body(
            file_ix,
            vec![DiffRow::FoldingBody {
                file: file_ix as u32,
            }],
        );
        self.ensure_fold_settle(cx);
    }

    pub(super) fn toggle_submodule(&mut self, group_ix: usize, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed else {
            return;
        };
        let Some(group) = parsed.submodules.get(group_ix) else {
            return;
        };
        let collapsed = !self
            .submodule_folds
            .get(&group.repository_path)
            .copied()
            .unwrap_or(false);
        self.submodule_folds
            .insert(group.repository_path.clone(), collapsed);
        self.reflatten(cx);
    }

    /// Keep a sweep alive while any [`DiffRow::FoldingBody`] stand-ins
    /// remain; each tick settles the ones whose tween window has elapsed.
    pub(super) fn ensure_fold_settle(&mut self, cx: &mut Context<Self>) {
        if self.fold_settle.is_some() {
            return;
        }
        self.fold_settle = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FOLD_TWEEN_WINDOW).await;
                let more = this
                    .update(cx, |changes, cx| changes.settle_folds(cx))
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
            this.update(cx, |changes, _| changes.fold_settle = None)
                .ok();
        }));
    }

    /// Replace every settled folding stand-in with its steady-state rows.
    /// Returns whether any stand-ins are still mid-tween.
    pub(super) fn settle_folds(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(parsed) = &self.parsed else {
            return false;
        };
        let files = parsed.files.clone();
        let mut pending = false;
        for file_ix in (0..self.row_ranges.len()).rev() {
            let Some(range) = &self.row_ranges[file_ix] else {
                continue;
            };
            let folding = self.rows.get(range.start + 1)
                == Some(&DiffRow::FoldingBody {
                    file: file_ix as u32,
                });
            if !folding {
                continue;
            }
            let Some(file) = files.get(file_ix) else {
                continue;
            };
            let fold = self.folds.get(&file.path).copied().unwrap_or_default();
            if fold.animating() {
                pending = true;
                continue;
            }
            let body = if fold.collapsed {
                Vec::new()
            } else {
                body_rows(
                    file_ix as u32,
                    file,
                    &self.comments_for(&file.path, cx),
                    self.draft_anchor_in(&file.path),
                    self.mode,
                )
            };
            self.replace_file_body(file_ix, body);
        }
        cx.notify();
        pending
    }

    /// Every parsed file currently folded shut?
    pub(super) fn all_collapsed(&self) -> bool {
        let Some(parsed) = &self.parsed else {
            return false;
        };
        let visible_files = self
            .row_ranges
            .iter()
            .enumerate()
            .filter_map(|(ix, range)| range.as_ref().map(|_| ix))
            .collect::<Vec<_>>();
        let files_collapsed = visible_files.is_empty()
            || visible_files.into_iter().all(|ix| {
                parsed.files.get(ix).is_some_and(|file| {
                    self.folds
                        .get(&file.path)
                        .is_some_and(|fold| fold.collapsed)
                })
            });
        let groups_collapsed = parsed.submodules.is_empty()
            || parsed.submodules.iter().all(|group| {
                self.submodule_folds
                    .get(&group.repository_path)
                    .copied()
                    .unwrap_or(false)
            });
        files_collapsed && groups_collapsed
    }

    /// Collapse every file section, or expand them all when everything is
    /// already shut (the toolbar's fold button, t3code parity). Steady-state
    /// writes — no per-row tween arming, the whole list just snaps. List
    /// splices run bottom-up over the OLD ranges (each is O(log n)), then
    /// the row model rebuilds wholesale; the scroll anchor rides the
    /// splices, landing on the nearest file header when its body vanishes.
    pub(super) fn toggle_collapse_all(&mut self, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed else {
            return;
        };
        let collapse = !self.all_collapsed();
        let files = parsed.files.clone();
        let groups = parsed.submodules.clone();
        for file in files.iter() {
            let fold = self.folds.entry(file.path.clone()).or_default();
            fold.collapsed = collapse;
            fold.toggled_at = None;
        }
        for group in groups.iter() {
            self.submodule_folds
                .insert(group.repository_path.clone(), collapse);
        }
        if !groups.is_empty() {
            self.reflatten(cx);
            return;
        }
        let staged = self.staged_comments(cx);
        let draft = self.draft_anchor();
        for file_ix in (0..self.row_ranges.len().min(files.len())).rev() {
            let Some(range) = &self.row_ranges[file_ix] else {
                continue;
            };
            let body = range.start + 1..range.end;
            let new_len = if collapse {
                0
            } else {
                let file = &files[file_ix];
                let comments: Vec<ReviewComment> = staged
                    .iter()
                    .filter(|comment| !comment.is_file() && comment.path == file.path)
                    .cloned()
                    .collect();
                let body = body_rows(
                    file_ix as u32,
                    file,
                    &comments,
                    self.draft_anchor_in(&file.path),
                    self.mode,
                );
                expand_context::add_rows(body, &files, &parsed.key, &self.expanded_context).0.len()
            };
            if body.len() != new_len {
                self.list.splice(body, new_len);
            }
        }
        let (rows, mut ranges) = flatten_rows(
            &files,
            &staged,
            draft
                .as_ref()
                .map(|(path, side, line)| (path.as_str(), *side, *line)),
            self.mode,
            |_| collapse,
        );
        let (rows, indices) = expand_context::add_rows(rows, &files, &parsed.key, &self.expanded_context);
        for range in &mut ranges { *range = indices[range.start]..indices[range.end]; }
        self.rows = rows;
        self.row_ranges = ranges.into_iter().map(Some).collect();
        self.section_headers = self
            .row_ranges
            .iter()
            .filter_map(|range| range.as_ref().map(|range| range.start))
            .collect();
        cx.notify();
    }

    /// Swap unified ⇄ split (toolbar toggle). The parse is untouched — only
    /// the flattening changes — so this rebuilds the row model and re-anchors
    /// the scroll onto whichever file was under the viewport's top edge (row
    /// indices do not survive the re-pairing).
    pub(super) fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        self.mode = self.mode.toggled();
        self.reset_horizontal_scroll();
        let split = self.mode.is_split();
        crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
            settings.diff_split = split;
        });
        // A draft's `+` sits in a column that may not exist after the swap.
        self.hover = None;
        self.reflatten(cx);
    }

    pub(super) fn toggle_wrap(&mut self, cx: &mut Context<Self>) {
        self.wrap_lines = !self.wrap_lines;
        self.reset_horizontal_scroll();
        let wrap = self.wrap_lines;
        crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
            settings.diff_wrap = wrap;
        });

        // A folding stand-in has an analytic fixed-line height. Settle it
        // before switching to variable-height rows; steady rows can then be
        // measured by the virtual list at the current pane width.
        let folding = self
            .rows
            .iter()
            .any(|row| matches!(row, DiffRow::FoldingBody { .. }));
        if folding {
            self.fold_settle = None;
            for fold in self.folds.values_mut() {
                fold.toggled_at = None;
            }
            self.reflatten(cx);
        } else {
            self.list.remeasure();
            cx.notify();
        }
    }

    pub(super) fn reflatten(&mut self, cx: &mut Context<Self>) {
        self.seen_candidates.clear();
        if self.explanation_selection.take().is_some() {
            self.explanation_unified_rows = None;
            self.explanation_hunk = None;
            self.cancel_explanation();
        }
        let Some(parsed) = &self.parsed else {
            cx.notify();
            return;
        };
        let files = parsed.files.clone();
        let groups = parsed.submodules.clone();
        let layout = parsed.layout.clone();
        let top = self.list.logical_scroll_top().item_ix;
        let anchor_row = self.rows.get(top).copied();
        let anchor_file = self
            .row_ranges
            .iter()
            .position(|range| range.as_ref().is_some_and(|range| range.contains(&top)));
        let anchor_group = self.rows.get(top).and_then(|row| match row {
            DiffRow::SubmoduleHeader { group } => Some(*group as usize),
            _ => None,
        });
        let collapsed: Vec<bool> = files
            .iter()
            .map(|file| {
                self.folds
                    .get(&file.path)
                    .is_some_and(|fold| fold.collapsed)
            })
            .collect();
        let staged = self.staged_comments(cx);
        let draft = self.draft_anchor();
        let group_collapsed = groups
            .iter()
            .map(|group| {
                self.submodule_folds
                    .get(&group.repository_path)
                    .copied()
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        let (rows, mut ranges, mut section_headers) = flatten_diff_tree(
            &files,
            &groups,
            &layout,
            &staged,
            draft
                .as_ref()
                .map(|(path, side, line)| (path.as_str(), *side, *line)),
            self.mode,
            |ix| collapsed.get(ix).copied().unwrap_or(false),
            |ix| group_collapsed.get(ix).copied().unwrap_or(false),
        );
        let (rows, indices) = expand_context::add_rows(rows, &files, &parsed.key, &self.expanded_context);
        for range in ranges.iter_mut().flatten() { *range = indices[range.start]..indices[range.end]; }
        for index in &mut section_headers { *index = indices[*index]; }
        let row_height = px(diff_line_height(Theme::of(cx)));
        self.list.reset_with_uniform_height(rows.len(), row_height);
        self.rows = rows;
        self.row_ranges = ranges;
        self.section_headers = section_headers;
        self.refresh_thread_rows(cx);
        let anchor = anchor_row.and_then(|row| self.rows.iter().position(|candidate| *candidate == row)).or_else(|| anchor_file
            .and_then(|ix| self.row_ranges.get(ix))
            .and_then(|range| range.as_ref())
            .map(|range| range.start))
            .or_else(|| {
                anchor_group.and_then(|group| {
                    self.rows.iter().position(|row| {
                        *row == DiffRow::SubmoduleHeader {
                            group: group as u32,
                        }
                    })
                })
            });
        if let Some(start) = anchor {
            self.list.scroll_to_reveal_item(start);
        }
        cx.notify();
    }

}
