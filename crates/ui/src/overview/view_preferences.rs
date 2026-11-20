use super::*;

impl Overview {

    /// Read the persisted acknowledged/hidden-repo sets once `data_dir`
    /// exists (it's `None` until engine bootstrap finishes — retried each
    /// render until then, unlike positions' one-shot flag).
    pub(super) fn ensure_prefs_loaded(&mut self, cx: &mut Context<Self>) {
        if self.prefs_loaded {
            return;
        }
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        self.prefs_loaded = true;
        let flags =
            overview_positions::load_string_set(&data_dir, overview_positions::UI_FLAGS_FILE);
        self.restore_linear_column(&flags);
        self.restore_details_width(&flags);
        self.acknowledged_done
            .extend(overview_positions::load_string_set(
                &data_dir,
                overview_positions::ACKNOWLEDGED_DONE_FILE,
            ));
        self.hidden_repos
            .extend(overview_positions::load_string_set(
                &data_dir,
                overview_positions::HIDDEN_REPOS_FILE,
            ));
        self.work_item_filter
            .extend(work_item_filter::load_filter(&data_dir));
        if !self.work_item_filter.is_empty() {
            self.work_item_filter_needs_validation = true;
            self.rows_dirty = true;
        }
        if !self.hidden_repos.is_empty() {
            self.repo_filter_needs_validation = true;
            self.rows_dirty = true;
        }
        // A toggle made before `data_dir` existed wins over the older file.
        if !self.view_prefs_touched {
            let prefs = view_prefs_from_flags(&flags);
            self.view_mode = prefs.mode;
            self.hide_stale = prefs.hide_stale;
            self.show_archived = prefs.show_archived;
            self.active_groups = prefs.groups;
            self.rows_dirty = true;
            if !flags.contains(VIEW_OUTLINE_FLAG) {
                self.view_prefs_changed(cx);
            }
        }
    }

    /// The current view state as [`ViewPrefs`].
    pub(super) fn view_prefs(&self) -> ViewPrefs {
        ViewPrefs {
            mode: self.view_mode,
            hide_stale: self.hide_stale,
            show_archived: self.show_archived,
            groups: self.active_groups.clone(),
        }
    }

    /// Persist the view state (mode, Hide stale / Show archived, grouping
    /// dimensions + order) into `UI_FLAGS_FILE`, read-modify-write like
    /// [`Overview::persist_ui_flag`]. Call after every change to any of them.
    pub(super) fn view_prefs_changed(&mut self, cx: &Context<Self>) {
        if !self.prefs_loaded {
            self.view_prefs_touched = true;
        }
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        let mut flags =
            overview_positions::load_string_set(&data_dir, overview_positions::UI_FLAGS_FILE);
        apply_view_prefs_to_flags(&mut flags, &self.view_prefs());
        self.save_string_set(overview_positions::UI_FLAGS_FILE, &flags, cx);
    }

    pub(super) fn save_string_set(&self, file_name: &str, set: &BTreeSet<String>, cx: &Context<Self>) {
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        if let Err(err) = overview_positions::save_string_set(&data_dir, file_name, set) {
            tracing::warn!(error = %err, file = file_name, "saving overview state failed");
        }
    }

    /// Set/clear one entry of `UI_FLAGS_FILE`. Read-modify-write so every
    /// other flag in the file survives.
    pub(super) fn persist_ui_flag(&self, flag: String, on: bool, cx: &Context<Self>) {
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        let mut flags =
            overview_positions::load_string_set(&data_dir, overview_positions::UI_FLAGS_FILE);
        if on {
            flags.insert(flag);
        } else {
            flags.remove(&flag);
        }
        self.save_string_set(overview_positions::UI_FLAGS_FILE, &flags, cx);
    }

    /// Any repo-filter change: persist, and re-derive rows.
    pub(super) fn repo_filter_changed(&mut self, cx: &mut Context<Self>) {
        self.save_string_set(
            overview_positions::HIDDEN_REPOS_FILE,
            &self.hidden_repos,
            cx,
        );
        self.rows_dirty = true;
        cx.notify();
    }
}
