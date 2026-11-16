//! Open pinned code snapshots in the existing file pane.
use super::*;

impl Shell {
    pub(super) fn activate_pinned_code_ref(
        &mut self,
        activation: &crate::markdown::render::LinkActivation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<crate::markdown::render::LinkOutcome> {
        let reference = crate::code_ref::decode_pinned_link(&activation.target.original)?;
        let root = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == self.active_chat)?
            .cwd
            .clone()?;
        let link = crate::workspace_links::resolve_workspace_file_link(
            &reference.reference.location.path,
            &root,
        )?;
        self.add_file_surface_at(link.path.clone(), None, None, window, cx);
        let lookup = (self.panel_key(cx), link.path);
        if let Some(file) = self
            .file_surface_keys
            .get(&lookup)
            .and_then(|id| self.file_surfaces.get(id))
            .cloned()
        {
            file.update(cx, |file, cx| file.show_pinned_reference(reference, cx));
            Some(crate::markdown::render::LinkOutcome::Internal)
        } else {
            Some(crate::markdown::render::LinkOutcome::Rejected)
        }
    }
}
