//! Shell ownership of the review surface and its artifact revision.
use super::*;
use crate::changes::walkthrough::ReviewHost;

impl Shell {
    pub(super) fn review_artifact(&self, cx: &App) -> Option<mast_proto::ArtifactView> {
        let state = self.state.read(cx);
        let chat = state
            .chats
            .iter()
            .find(|chat| chat.id == self.active_chat)?;
        let snapshot = state.artifacts().snapshot(chat.space_id.as_deref()?)?;
        snapshot
            .for_chat(&chat.id)
            .into_iter()
            .filter(|artifact| artifact.kind == mast_proto::ArtifactKind::Review)
            .max_by_key(|artifact| artifact.updated_at)
            .cloned()
    }

    pub(super) fn review_host(&mut self, cx: &mut Context<Self>) -> Option<Entity<ReviewHost>> {
        let artifact = self.review_artifact(cx)?;
        let chat_id = self.active_chat.clone();
        if let Some((id, revision, host)) = self.review_hosts.get(&chat_id)
            && *id == artifact.id
            && *revision == artifact.current_revision
        {
            return Some(host.clone());
        }
        let shell = cx.weak_entity();
        let host =
            cx.new(|cx| ReviewHost::new(self.state.clone(), chat_id.clone(), &artifact, shell, cx));
        self.review_hosts.insert(
            chat_id,
            (artifact.id, artifact.current_revision, host.clone()),
        );
        Some(host)
    }

    pub(super) fn render_review(&mut self, expanded: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let host = self.review_host(cx);
        let label = if expanded {
            "Back to chat"
        } else {
            "Open full width"
        };
        let toolbar = crate::surface_chrome::toolbar(&theme).child(
            div()
                .id("review-placement")
                .cursor_pointer()
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.route = if expanded { Route::Chat } else { Route::Review };
                    cx.notify();
                })),
        );
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(div().flex_1().min_h_0().children(host))
            .into_any_element()
    }
}
