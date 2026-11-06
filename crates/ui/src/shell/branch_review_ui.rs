//! Resolve a worker's committed range when its menu opens.
use super::*;

impl Shell {
    pub(crate) fn existing_branch_review(
        &self,
        worker_id: &str,
        target: &mast_proto::ReviewTarget,
        cx: &App,
    ) -> Option<String> {
        let expected_id = self.branch_review_range_ids.get(worker_id)?;
        let legacy_id = expected_id.strip_prefix("review-").unwrap_or(expected_id);
        let state = self.state.read(cx);
        let space_id = state
            .chats
            .iter()
            .find(|chat| chat.id == worker_id)?
            .space_id
            .as_deref()?;
        if let Some(chat_id) = self
            .branch_review_chats
            .get(&format!("{space_id}\0{}", target.state_key()))
        {
            if !state
                .chats
                .iter()
                .any(|chat| chat.id == *chat_id && chat.archived)
            {
                return Some(chat_id.clone());
            }
        }
        state
            .chats
            .iter()
            .find(|chat| {
                chat.id != worker_id
                    && !chat.archived
                    && chat.space_id.as_deref() == Some(space_id)
                    && state.chat_ref_targets(&chat.id).iter().any(|range| {
                        (range.id == *expected_id || range.id == legacy_id)
                            && range.kind == mast_proto::RefDiffKind::Branch
                    })
            })
            .map(|chat| chat.id.clone())
    }
    pub(crate) fn load_branch_review_target(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.branch_review_targets.remove(&chat_id);
        self.branch_review_range_ids.remove(&chat_id);
        let load_id = uuid::Uuid::new_v4();
        self.branch_review_loads.insert(chat_id.clone(), load_id);
        let Some(space_id) = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.space_id.clone())
        else {
            return;
        };
        let engine = self.state.read(cx).engine().cloned();
        let task = self
            .state
            .update(cx, |state, cx| state.list_chat_diff_checkouts(&chat_id, cx));
        cx.spawn(async move |this, cx| {
            let mut candidates = task.await.unwrap_or_default();
            candidates.sort_by_key(|candidate| !candidate.selected);
            let Some(engine) = engine else {
                return;
            };
            let mut target = None;
            for candidate in candidates.into_iter().filter(|c| c.bound) {
                let Some(branch) = candidate.branch.clone() else {
                    continue;
                };
                let params = mast_proto::ResolveBranchReviewBaseParams {
                    checkout_path: candidate.path.clone(),
                    branch,
                    space_id: space_id.clone(),
                    worker_chat_id: chat_id.clone(),
                };
                let Ok(value) = serde_json::to_value(params) else {
                    continue;
                };
                let Ok(value) = engine
                    .client()
                    .call(mast_rpc::methods::RESOLVE_BRANCH_REVIEW_BASE, value)
                    .await
                else {
                    continue;
                };
                let Ok(range) =
                    serde_json::from_value::<mast_proto::ResolveBranchReviewBaseReply>(value)
                else {
                    continue;
                };
                target =
                    crate::branch_review_launch::target_for_resolved_checkout(&candidate, &range)
                        .map(|target| (target, range.review_range_id, range.existing_chat_id));
                if target.is_some() {
                    break;
                }
            }
            this.update(cx, |this, cx| {
                if this.branch_review_loads.get(&chat_id) != Some(&load_id) {
                    return;
                }
                this.branch_review_loads.remove(&chat_id);
                if let Some((target, id, existing)) = target {
                    if let Some(existing) = existing {
                        this.branch_review_chats
                            .insert(format!("{space_id}\0{}", target.state_key()), existing);
                    }
                    this.branch_review_range_ids.insert(chat_id.clone(), id);
                    this.branch_review_targets.insert(chat_id, target);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn open_branch_review_for_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let Some(target) = self.branch_review_targets.get(&chat_id).cloned() else {
            return;
        };
        if let Some(existing) = self.existing_branch_review(&chat_id, &target, cx) {
            self.open_chat(existing, cx);
            return;
        }
        let state = self.state.read(cx);
        let Some(space_id) = state
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.space_id.clone())
        else {
            return;
        };
        let item = state
            .work_outline(&space_id)
            .snapshot()
            .and_then(|snapshot| {
                crate::work_outline::instructions::assigned_chats(snapshot).remove(&chat_id)
            });
        if let Err(error) = self.open_start_chat_for_branch_review(space_id, item, target, cx) {
            self.set_sidebar_error(error);
            cx.notify();
        }
    }

    pub(crate) fn branch_review_target(&self, chat_id: &str) -> Option<&mast_proto::ReviewTarget> {
        self.branch_review_targets.get(chat_id)
    }
}
