use super::*;

impl AppState {
    /// Current Work Outline load state for one Space.
    pub(crate) fn work_outline(&self, space_id: &str) -> crate::work_outline::WorkOutlineLoad {
        self.work_outlines.load(space_id)
    }

    /// Declares which Spaces the Outline presentation shows, so the
    /// `WatchWorkOutline` reconciler watches exactly those (an empty list
    /// stops every watch). Takes no `cx`: the reconcile is scheduled on the app
    /// through the runtime captured at engine attach (a no-op before attach,
    /// where `attach_engine` reconciles anyway). Use
    /// [`Self::declare_work_outline_visible`] from code that already has a `cx`.
    pub(crate) fn set_work_outline_visible(&mut self, space_ids: Vec<String>) {
        if !self.record_work_outline_visible(space_ids) {
            return;
        }
        if let Some(runtime) = self.work_outline_runtime.clone() {
            runtime
                .app
                .clone()
                .spawn(async move |cx| {
                    runtime
                        .state
                        .update(cx, |state, cx| state.reconcile_work_outline_watches(cx))
                        .ok();
                })
                .detach();
        }
    }

    /// [`Self::set_work_outline_visible`] with an immediate reconcile.
    pub(crate) fn declare_work_outline_visible(
        &mut self,
        space_ids: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.record_work_outline_visible(space_ids);
        self.reconcile_work_outline_watches(cx);
    }

    /// Normalize (sort, dedupe) and store the declared set; whether it changed.
    pub(super) fn record_work_outline_visible(&mut self, mut space_ids: Vec<String>) -> bool {
        space_ids.sort();
        space_ids.dedup();
        if self.work_outline_visible == space_ids {
            return false;
        }
        self.work_outline_visible = space_ids;
        true
    }

    /// Unresolved user-action totals per watched Space (zero totals omitted).
    /// Plumbing for the Milestone 4 notification detector: it diffs this map
    /// against its previous copy and fires on 0 -> nonzero. No notification or
    /// color is produced in Milestone 1.
    #[cfg(test)]
    pub(crate) fn work_outline_unresolved_user_actions(&self) -> HashMap<String, u32> {
        self.work_outlines.unresolved_user_action_totals()
    }

    /// Tests: put a loaded Work Outline snapshot in the store.
    #[cfg(any(test, feature = "snapshot"))]
    pub(crate) fn set_work_outline_for_test(&mut self, snapshot: mast_proto::WorkOutlineSnapshot) {
        let space_id = snapshot.space_id.clone();
        self.work_outlines.set(
            &space_id,
            crate::work_outline::WorkOutlineLoad::Ready(Arc::new(snapshot)),
        );
    }

    /// Mutation facade bound to the current engine connection.
    pub(crate) fn work_outline_actions(&self) -> crate::work_outline::WorkOutlineActions {
        #[cfg(test)]
        if let Some(actions) = &self.work_outline_actions_override {
            return actions.clone();
        }
        let actions = crate::work_outline::WorkOutlineActions::new(self.engine.clone());
        match &self.work_outline_runtime {
            Some(runtime) => actions.with_executor(runtime.app.background_executor().clone()),
            None => actions,
        }
    }

    /// Apply one `WatchWorkOutline` frame for `space_id`. Returns whether the
    /// store changed. A frame for another Space, or for a Space that is not
    /// declared visible, is dropped. Stale-revision filtering is per stream
    /// and lives in the pump (`spawn_work_outline_watch`).
    pub(crate) fn apply_work_outline_frame(
        &mut self,
        space_id: &str,
        snapshot: mast_proto::WorkOutlineSnapshot,
    ) -> bool {
        if snapshot.space_id != space_id || !self.work_outline_visible_contains(space_id) {
            return false;
        }
        self.work_outlines.set(
            space_id,
            crate::work_outline::WorkOutlineLoad::Ready(Arc::new(snapshot)),
        )
    }

    /// Apply a failed subscribe. A last good snapshot survives transient
    /// failures (the outline keeps rendering stale rather than blanking);
    /// `Unsupported` always replaces it.
    pub(crate) fn apply_work_outline_failure(&mut self, space_id: &str, message: &str) -> bool {
        if !self.work_outline_visible_contains(space_id) {
            return false;
        }
        let load = crate::work_outline::WorkOutlineLoad::from_watch_error(message);
        if matches!(load, crate::work_outline::WorkOutlineLoad::Failed(_))
            && self.work_outlines.snapshot(space_id).is_some()
        {
            return false;
        }
        self.work_outlines.set(space_id, load)
    }

    /// Whether `space_id` is wanted: declared visible by the Overview, or the
    /// selected Chat's Space (the chat header drift line needs its outline
    /// from app start, not only once the Overview exists).
    fn work_outline_visible_contains(&self, space_id: &str) -> bool {
        self.work_outline_visible
            .binary_search_by(|id| id.as_str().cmp(space_id))
            .is_ok()
            || self.selected_chat_space_id() == Some(space_id)
            || (self.sidebar_outline_sections
                && self.spaces.iter().any(|space| space.id == space_id))
    }

    /// The selected Chat's Space, when it has one.
    fn selected_chat_space_id(&self) -> Option<&str> {
        self.selected_chat_row()?.space_id.as_deref()
    }

    /// Spaces that should have a live watch: the union of the declared
    /// visible set and the selected Chat's Space, restricted to live Spaces.
    /// Is the Outline presentation on screen (any Space declared visible)?
    pub(crate) fn work_outline_is_visible(&self) -> bool {
        !self.work_outline_visible.is_empty()
    }

    pub(super) fn desired_work_outline_spaces(&self) -> HashSet<String> {
        self.work_outline_visible
            .iter()
            .map(String::as_str)
            .chain(self.selected_chat_space_id())
            .chain(
                self.spaces
                    .iter()
                    .filter(|_| self.sidebar_outline_sections)
                    .map(|space| space.id.as_str()),
            )
            .filter(|id| self.spaces.iter().any(|space| space.id == *id))
            .map(str::to_owned)
            .collect()
    }

    /// Mirror of the change-request reconciler: compute the desired Space
    /// set, drop tasks and snapshots for Spaces no longer wanted (deleted or
    /// hidden), and spawn a watch for each missing one. Re-run from the
    /// Spaces frame, `attach_engine` and `declare_work_outline_visible`.
    pub(super) fn reconcile_work_outline_watches(&mut self, cx: &mut Context<Self>) {
        self.reconcile_artifact_watches(cx);
        self.reconcile_build_job_watches(cx);
        let Some(handle) = self.engine.clone() else {
            self.work_outline_tasks.clear();
            return;
        };
        let wanted = self.desired_work_outline_spaces();
        self.work_outline_tasks
            .retain(|space_id, _| wanted.contains(space_id));
        let dropped = {
            let before = self.work_outlines.space_ids().count();
            self.work_outlines
                .retain(|space_id| wanted.contains(space_id));
            before != self.work_outlines.space_ids().count()
        };
        for space_id in wanted {
            if self.work_outline_tasks.contains_key(&space_id) {
                continue;
            }
            #[cfg(feature = "startup-bench")]
            startup_trace("outline_watch_spawn");
            let task = spawn_work_outline_watch(cx, handle.clone(), space_id.clone());
            self.work_outline_tasks.insert(space_id, task);
        }
        if dropped {
            cx.notify();
        }
    }

    /// Planning artifacts of the watched Spaces.
    pub(crate) fn artifacts(&self) -> &crate::artifacts::ArtifactStore {
        &self.artifacts
    }

    /// The artifact RPC facade (shares the Work Outline transport and its
    /// test override).
    pub(crate) fn artifact_actions(&self) -> crate::artifacts::ArtifactActions {
        crate::artifacts::ArtifactActions::new(self.work_outline_actions())
    }

    /// Apply one `WatchArtifacts` frame; whether anything changed. Frames for
    /// Spaces no longer watched are dropped.
    pub(crate) fn apply_artifacts_frame(
        &mut self,
        space_id: &str,
        snapshot: mast_proto::ArtifactsSnapshot,
    ) -> bool {
        if snapshot.space_id != space_id || !self.artifact_tasks.contains_key(space_id) {
            return false;
        }
        self.artifacts.set(
            space_id,
            crate::artifacts::ArtifactsLoad::Ready(Arc::new(snapshot)),
        )
    }

    /// Register as the engine's render-capable UI and serve render checks
    /// (`WatchArtifactRenderRequests`).
    pub(crate) fn start_artifact_render_executor(
        &mut self,
        handle: EngineHandle,
        renderer: Option<std::rc::Rc<dyn crate::artifacts::render_check::ArtifactRenderer>>,
        config: crate::artifacts::render_check::RenderConfig,
        cx: &mut Context<Self>,
    ) {
        self.artifact_render_task = Some(crate::artifacts::render_check::spawn_render_requests(
            cx, handle, renderer, config,
        ));
    }

    /// Tests: install a Space's artifacts without a watch.
    #[cfg(any(test, feature = "snapshot"))]
    pub(crate) fn set_test_artifacts(&mut self, snapshot: mast_proto::ArtifactsSnapshot) {
        let space_id = snapshot.space_id.clone();
        self.artifacts.set(
            &space_id,
            crate::artifacts::ArtifactsLoad::Ready(Arc::new(snapshot)),
        );
    }

    /// Apply a failed `WatchArtifacts` subscribe.
    pub(crate) fn apply_artifacts_failure(&mut self, space_id: &str, message: &str) -> bool {
        if !self.artifact_tasks.contains_key(space_id) {
            return false;
        }
        self.artifacts.set(
            space_id,
            crate::artifacts::ArtifactsLoad::from_watch_error(message),
        )
    }

    /// `WatchArtifacts` follows the Work Outline watch set exactly: the
    /// declared visible Spaces plus the selected Chat's Space.
    fn reconcile_artifact_watches(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.engine.clone() else {
            self.artifact_tasks.clear();
            return;
        };
        let wanted = self.desired_work_outline_spaces();
        self.artifact_tasks
            .retain(|space_id, _| wanted.contains(space_id));
        let dropped = self.artifacts.retain(|space_id| wanted.contains(space_id));
        for space_id in wanted {
            if self.artifact_tasks.contains_key(&space_id) {
                continue;
            }
            let task = spawn_artifacts_watch(cx, handle.clone(), space_id.clone());
            self.artifact_tasks.insert(space_id, task);
        }
        if dropped {
            cx.notify();
        }
    }
}
