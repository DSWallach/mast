use super::*;

impl AppState {
    /// The captured execution binding of a Chat, from its Space's outline
    /// snapshot (`None` = unbound or the snapshot is not loaded).
    pub(crate) fn chat_execution_binding(
        &self,
        chat_id: &str,
    ) -> Option<&mast_proto::ChatExecutionBindingView> {
        let space_id = self
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)?
            .space_id
            .as_deref()?;
        self.work_outlines
            .snapshot(space_id)?
            .chat_execution_binding(chat_id)
    }

    /// The pills of a Chat's Changes pane and branch indicator: its bound
    /// execution members (primary first, branch/HEAD from the latest
    /// validation reply), then, only when the chat has diff targets, its own
    /// checkout and one member per resolvable target. Empty for a Chat with
    /// neither a binding nor a target.
    pub(crate) fn chat_execution_members(
        &self,
        chat_id: &str,
    ) -> Vec<crate::work_outline::execution::ExecutionMember> {
        use crate::work_outline::execution::ExecutionMember;
        let mut members = match self.chat_execution_binding(chat_id) {
            Some(binding) => {
                let entry = self
                    .chat_execution
                    .get(chat_id)
                    .filter(|entry| entry.binding_key == execution_binding_key(binding));
                crate::work_outline::execution::execution_members(
                    binding,
                    entry.and_then(|entry| entry.reply.as_ref()),
                )
            }
            None => Vec::new(),
        };
        let targets = self.chat_diff_targets(chat_id);
        let ref_targets = self.chat_ref_targets(chat_id);
        if !targets.is_empty() || !ref_targets.is_empty() {
            members.extend(
                self.chats
                    .iter()
                    .find(|chat| chat.id == chat_id)
                    .and_then(ExecutionMember::own_checkout),
            );
            for target in targets {
                // A target that is also a bound member's checkout is that
                // member (no duplicate pill).
                if !members
                    .iter()
                    .any(|member| member.checkout_path == target.path)
                {
                    members.push(ExecutionMember::from_diff_target(target));
                }
            }
            members.extend(ref_targets.iter().map(ExecutionMember::from_ref_target));
        }
        members
    }

    /// The member the branch indicator and Changes pane show. Precedence: the
    /// pill picked in this session, else the engine-selected diff target,
    /// else the default (primary, else first: the own checkout of an unbound
    /// Chat). `None` for a Chat with no members.
    pub(crate) fn selected_execution_member(
        &self,
        chat_id: &str,
    ) -> Option<crate::work_outline::execution::ExecutionMember> {
        let members = self.chat_execution_members(chat_id);
        let picked = self
            .chat_execution
            .get(chat_id)
            .and_then(|entry| entry.selected.as_deref());
        picked
            .and_then(|repo| members.iter().find(|member| member.repo_id == repo))
            .or_else(|| {
                let selected = self
                    .chat_diff_targets(chat_id)
                    .iter()
                    .find(|target| target.selected)?;
                members.iter().find(|member| {
                    member.kind != crate::work_outline::execution::MemberKind::Ref
                        && member.checkout_path == selected.path
                })
            })
            .or_else(|| {
                let selected = self
                    .chat_ref_targets(chat_id)
                    .iter()
                    .find(|target| target.selected)?;
                let id = crate::work_outline::execution::ref_target_member_id(&selected.id);
                members.iter().find(|member| member.repo_id == id)
            })
            .or_else(|| crate::work_outline::execution::default_member(&members))
            .cloned()
    }

    pub(crate) fn chat_execution_state(
        &self,
        chat_id: &str,
    ) -> Option<&crate::work_outline::execution::ChatExecutionState> {
        self.chat_execution.get(chat_id)
    }

    /// Pick the repository a Chat's branch indicator and Changes pane show.
    ///
    /// The pick is applied in memory at once (the pane re-targets without
    /// waiting) and, for a Chat with diff targets, persisted through
    /// `SetChatDiffTarget`: a target pill writes `select {path}`, the own or a
    /// binding pill `clearSelection`. A stale in-memory pick must not fight
    /// the persisted selection: [`Self::apply_chat_diff_targets`] drops it when
    /// a frame selects a different target, and a failed write drops it here
    /// (the engine's selection is the truth).
    pub(crate) fn select_chat_execution_member(
        &mut self,
        chat_id: &str,
        repo_id: &str,
        cx: &mut Context<Self>,
    ) {
        use crate::work_outline::execution::MemberKind;
        let effective = self
            .selected_execution_member(chat_id)
            .map(|member| member.repo_id);
        if self.pick_chat_execution_member_locally(chat_id, repo_id) {
            cx.notify();
        }
        if effective.as_deref() == Some(repo_id)
            || (self.chat_diff_targets(chat_id).is_empty()
                && self.chat_ref_targets(chat_id).is_empty())
        {
            return;
        }
        let Some(member) = self
            .chat_execution_members(chat_id)
            .into_iter()
            .find(|member| member.repo_id == repo_id)
        else {
            return;
        };
        if member.kind == MemberKind::Ref {
            let ref_id = member
                .repo_id
                .strip_prefix("ref:")
                .unwrap_or(&member.repo_id)
                .to_owned();
            self.write_chat_diff_target(
                mast_proto::SetChatDiffTargetParams {
                    chat_id: chat_id.to_owned(),
                    action: mast_proto::SetChatDiffTargetAction::SelectRef,
                    ref_id: Some(ref_id),
                    ..Default::default()
                },
                cx,
            )
            .detach();
            return;
        }
        let (action, path) = match member.kind {
            MemberKind::Ref => return,
            MemberKind::Target => (
                mast_proto::SetChatDiffTargetAction::Select,
                Some(member.checkout_path),
            ),
            MemberKind::Own | MemberKind::Binding => {
                (mast_proto::SetChatDiffTargetAction::ClearSelection, None)
            }
        };
        self.set_chat_diff_target(chat_id, action, path, cx)
            .detach();
    }

    /// The in-memory half of [`Self::select_chat_execution_member`]. Returns
    /// whether the pick changed.
    pub(crate) fn pick_chat_execution_member_locally(
        &mut self,
        chat_id: &str,
        repo_id: &str,
    ) -> bool {
        let entry = self.chat_execution.entry(chat_id.to_owned()).or_default();
        if entry.selected.as_deref() == Some(repo_id) {
            return false;
        }
        entry.selected = Some(repo_id.to_owned());
        true
    }

    /// Selection is engine-owned: a session-local pill pick affects the header
    /// only after its selection has round-tripped through the engine.
    pub(crate) fn display_chat_branch<'a>(&'a self, chat: &'a Chat) -> Option<&'a str> {
        let target = self.selected_chat_diff_target(&chat.id);
        if target.is_none() && chat.source_context.is_none() {
            return None;
        }
        crate::change_requests::display_checkout(chat, target)
            .branch
            .filter(|branch| !branch.trim().is_empty())
    }

    pub(crate) fn selected_checkout_key(
        &self,
        chat_id: &str,
    ) -> Option<(String, String, Option<String>)> {
        self.selected_chat_diff_target(chat_id)
            .map(|target| {
                (target.path.clone(), target.checkout_id.clone(), target.branch.clone())
            })
    }

    /// The explicitly selected checkout target, if any.
    pub(crate) fn selected_chat_diff_target(
        &self,
        chat_id: &str,
    ) -> Option<&mast_proto::ChatDiffTargetView> {
        self.chat_diff_targets(chat_id)
            .iter()
            .find(|target| target.selected)
    }

    /// Resolved diff targets of a Chat (empty when none).
    pub(crate) fn chat_diff_targets(&self, chat_id: &str) -> &[mast_proto::ChatDiffTargetView] {
        self.chat_diff_targets
            .get(chat_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Resolved branch / PR range targets of a Chat (empty when none).
    pub(crate) fn chat_ref_targets(&self, chat_id: &str) -> &[mast_proto::ChatRefTargetView] {
        self.chat_ref_targets
            .get(chat_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Apply one `WatchChatDiffTargets` frame (the full list). Returns
    /// whether anything changed. In-memory picks that the frame supersedes
    /// are dropped: a frame that selects a target the Chat was not on (the
    /// agent bound a worktree mid-turn, another window picked, or our own
    /// write landing) wins over a pill picked earlier; a pick of a target
    /// that vanished goes too. A frame without a selection keeps the pick: it
    /// is what a `clearSelection` write (own or binding pill) looks like.
    pub(crate) fn apply_chat_diff_targets(
        &mut self,
        frame: Vec<mast_proto::ChatDiffTargetsView>,
    ) -> bool {
        let mut next: HashMap<String, Vec<mast_proto::ChatDiffTargetView>> = HashMap::new();
        let mut next_refs: HashMap<String, Vec<mast_proto::ChatRefTargetView>> = HashMap::new();
        for view in frame {
            if !view.ref_targets.is_empty() {
                next_refs.insert(view.chat_id.clone(), view.ref_targets);
            }
            if !view.targets.is_empty() {
                next.insert(view.chat_id, view.targets);
            }
        }
        if next == self.chat_diff_targets && next_refs == self.chat_ref_targets {
            return false;
        }
        // What a frame selects: a checkout target's path or a ref target's
        // id (at most one of the two).
        let selected_key = |targets: &HashMap<String, Vec<mast_proto::ChatDiffTargetView>>,
                            refs: &HashMap<String, Vec<mast_proto::ChatRefTargetView>>,
                            chat_id: &str| {
            targets
                .get(chat_id)
                .and_then(|targets| targets.iter().find(|target| target.selected))
                .map(|target| format!("path:{}", target.path))
                .or_else(|| {
                    refs.get(chat_id)
                        .and_then(|refs| refs.iter().find(|target| target.selected))
                        .map(|target| format!("ref:{}", target.id))
                })
        };
        for (chat_id, entry) in &mut self.chat_execution {
            let Some(pick) = entry.selected.as_deref() else {
                continue;
            };
            let now = selected_key(&next, &next_refs, chat_id);
            let reselected = now.is_some()
                && now != selected_key(&self.chat_diff_targets, &self.chat_ref_targets, chat_id);
            let vanished = pick
                .strip_prefix("target:")
                .map(|checkout_id| {
                    !next
                        .get(chat_id)
                        .is_some_and(|targets| targets.iter().any(|t| t.checkout_id == checkout_id))
                })
                .or_else(|| {
                    pick.strip_prefix("ref:").map(|id| {
                        !next_refs
                            .get(chat_id)
                            .is_some_and(|refs| refs.iter().any(|t| t.id == id))
                    })
                })
                .unwrap_or(false);
            if reselected || vanished {
                entry.selected = None;
            }
        }
        self.chat_diff_targets = next;
        self.chat_ref_targets = next_refs;
        self.board_status_revision = self.board_status_revision.wrapping_add(1);
        true
    }

    /// Tests: put a `WatchChatDiffTargets` frame in the store.
    #[cfg(test)]
    pub(crate) fn set_chat_diff_targets_for_test(
        &mut self,
        frame: Vec<mast_proto::ChatDiffTargetsView>,
    ) {
        self.apply_chat_diff_targets(frame);
    }

    /// Forget the pill picked in this session so the persisted (engine)
    /// selection shows. Called when a write that changes the persisted
    /// selection is issued from the "Diff checkout..." picker.
    pub(crate) fn clear_chat_diff_member_pick(&mut self, chat_id: &str) {
        if let Some(entry) = self.chat_execution.get_mut(chat_id) {
            entry.selected = None;
        }
    }

    /// The last failed diff target write of a Chat.
    pub(crate) fn chat_diff_target_error(&self, chat_id: &str) -> Option<&str> {
        self.chat_diff_target_errors
            .get(chat_id)
            .map(String::as_str)
    }

    pub(crate) fn dismiss_chat_diff_target_error(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        if self.chat_diff_target_errors.remove(chat_id).is_some() {
            cx.notify();
        }
    }

    /// One diff target RPC: the engine, or the test transport.
    /// The UI is attached to another process's engine over the local port. That connection is
    /// not the app's own, so the engine refuses human-only methods (posting a review) on it.
    pub(crate) fn engine_is_remote(&self) -> bool {
        self.engine
            .as_ref()
            .is_some_and(|engine| matches!(engine.mode(), crate::state::EngineMode::Remote { .. }))
    }

    pub(crate) fn chat_diff_rpc(
        &self,
        method: &'static str,
        params: serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<serde_json::Value, String>> {
        #[cfg(test)]
        if let Some(fake) = self.chat_diff_rpc_override.clone() {
            return Task::ready(fake(method, params));
        }
        let Some(engine) = self.engine.clone() else {
            return Task::ready(Err("Mast is not connected to its engine.".to_owned()));
        };
        cx.spawn(async move |_, _| {
            engine
                .client()
                .call(method, params)
                .await
                .map_err(|error| error.to_string())
        })
    }

    /// `ListChatDiffCheckouts {chatId}`: the "Diff checkout..." candidates in
    /// engine order (the picker ranks them).
    pub(crate) fn list_chat_diff_checkouts(
        &self,
        chat_id: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<mast_proto::DiffCheckoutCandidate>, String>> {
        let params = serde_json::to_value(mast_proto::ListChatDiffCheckoutsParams {
            chat_id: chat_id.to_owned(),
        })
        .unwrap_or_default();
        let task = self.chat_diff_rpc(methods::LIST_CHAT_DIFF_CHECKOUTS, params, cx);
        cx.spawn(async move |_, _| {
            let value = task.await?;
            serde_json::from_value::<mast_proto::ListChatDiffCheckoutsReply>(value)
                .map(|reply| reply.candidates)
                .map_err(|error| format!("Malformed checkout list from the engine: {error}"))
        })
    }

    /// `SetChatDiffTarget`. A failure is kept in
    /// [`Self::chat_diff_target_error`] (and returned) and drops the in-memory
    /// pick, so the pane shows what the engine still has selected.
    pub(crate) fn set_chat_diff_target(
        &mut self,
        chat_id: &str,
        action: mast_proto::SetChatDiffTargetAction,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        self.write_chat_diff_target(
            mast_proto::SetChatDiffTargetParams {
                chat_id: chat_id.to_owned(),
                action,
                path,
                ..Default::default()
            },
            cx,
        )
    }

    /// `SetChatDiffTarget {chatId, action: bindRef, input, baseBranch?,
    /// repoPath?}`: resolve a PR number / URL or a branch name, fetch its
    /// refs read-only and select the new range target. A failure is kept in
    /// [`Self::chat_diff_target_error`] like every diff target write.
    pub(crate) fn bind_chat_ref_target(
        &mut self,
        chat_id: &str,
        input: String,
        repo_path: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        self.write_chat_diff_target(
            mast_proto::SetChatDiffTargetParams {
                chat_id: chat_id.to_owned(),
                action: mast_proto::SetChatDiffTargetAction::BindRef,
                input: Some(input),
                repo_path,
                ..Default::default()
            },
            cx,
        )
    }

    /// `ListChatRefDiffCandidates {chatId}`: linked PRs first, then branches
    /// matching the chat's ticket, in engine order.
    pub(crate) fn list_chat_ref_candidates(
        &self,
        chat_id: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<mast_proto::RefDiffCandidate>, String>> {
        let params = serde_json::to_value(mast_proto::ListChatDiffCheckoutsParams {
            chat_id: chat_id.to_owned(),
        })
        .unwrap_or_default();
        let task = self.chat_diff_rpc(methods::LIST_CHAT_REF_DIFF_CANDIDATES, params, cx);
        cx.spawn(async move |_, _| {
            let value = task.await?;
            serde_json::from_value::<mast_proto::ListChatRefDiffCandidatesReply>(value)
                .map(|reply| reply.candidates)
                .map_err(|error| format!("Malformed branch list from the engine: {error}"))
        })
    }

    fn write_chat_diff_target(
        &mut self,
        params: mast_proto::SetChatDiffTargetParams,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let chat_id = params.chat_id.clone();
        let params = serde_json::to_value(params).unwrap_or_default();
        self.chat_diff_target_errors.remove(&chat_id);
        let task = self.chat_diff_rpc(methods::SET_CHAT_DIFF_TARGET, params, cx);
        cx.spawn(async move |this, cx| {
            let result = task.await.map(|_| ());
            if let Err(message) = &result {
                let message = message.clone();
                this.update(cx, |state, cx| {
                    tracing::warn!(chat_id, %message, "SetChatDiffTarget failed");
                    state.clear_chat_diff_member_pick(&chat_id);
                    state.chat_diff_target_errors.insert(chat_id, message);
                    cx.notify();
                })
                .ok();
            }
            result
        })
    }

    /// Fetch the bound Chat's per-member branch/HEAD and checkout identities
    /// (`ValidateExecutionConfiguration {chatId}`; the engine forwards to the
    /// owning device). `force` refetches (a surface opening); otherwise only
    /// a Chat without a reply for its current binding fetches. Unbound Chats
    /// do nothing.
    pub(crate) fn refresh_chat_execution(
        &mut self,
        chat_id: &str,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(binding) = self.chat_execution_binding(chat_id) else {
            return;
        };
        let key = execution_binding_key(binding);
        let entry = self.chat_execution.entry(chat_id.to_owned()).or_default();
        let current = entry.binding_key == key;
        if current && entry.loading {
            return;
        }
        if current && !force && (entry.reply.is_some() || entry.error.is_some()) {
            return;
        }
        if !current {
            entry.reply = None;
            entry.error = None;
            entry.binding_key = key.clone();
        }
        entry.loading = true;
        let task = self.work_outline_actions().validate_execution(
            mast_proto::ValidateExecutionConfigurationParams {
                chat_id: Some(chat_id.to_owned()),
                ..Default::default()
            },
        );
        let chat_id = chat_id.to_owned();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |state, cx| {
                let Some(entry) = state.chat_execution.get_mut(&chat_id) else {
                    return;
                };
                if entry.binding_key != key {
                    return;
                }
                entry.loading = false;
                match result {
                    Ok(reply) => {
                        entry.reply = Some(reply);
                        entry.error = None;
                    }
                    Err(error) => {
                        entry.error =
                            Some(crate::work_outline::execution::execution_error_text(&error));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

}
