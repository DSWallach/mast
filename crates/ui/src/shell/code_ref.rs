//! Open pinned code snapshots in the existing file pane.
use super::*;

fn resolve_pinned_location(reference: &crate::code_ref::StagedCodeRef, root: &str) -> Option<crate::workspace_links::WorkspaceFileLink> {
    crate::workspace_links::resolve_workspace_file_link(&reference.reference.location.workspace_path(), root)
}

impl Shell {
    pub(super) fn activate_pinned_code_ref(
        &mut self,
        activation: &crate::markdown::render::LinkActivation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<crate::markdown::render::LinkOutcome> {
        let (reference, moved_to) = crate::code_ref::decode_pinned_target(&activation.target.original)?;
        let root = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == self.active_chat)?
            .cwd
            .clone()?;
        let link = resolve_pinned_location(&reference, &root)?;
        self.add_file_surface_at(link.path.clone(), None, None, window, cx);
        let lookup = (self.panel_key(cx), link.path);
        if let Some(file) = self
            .file_surface_keys
            .get(&lookup)
            .and_then(|id| self.file_surfaces.get(id))
            .cloned()
        {
            file.update(cx, |file, cx| file.show_pinned_reference(reference, moved_to, cx));
            Some(crate::markdown::render::LinkOutcome::Internal)
        } else {
            Some(crate::markdown::render::LinkOutcome::Rejected)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_location_includes_nested_repository_root() {
        let reference = crate::code_ref::StagedCodeRef {
            id: "ref".into(),
            reference: mast_proto::CodeRef {
                version: 1,
                location: mast_proto::CodeRefLocation { repo_id: "repo".into(), worktree_id: None, repo_root: Some("vendor/library".into()), path: "src/file.rs".into() },
                kind: mast_proto::CodeRefKind::Lines,
                range: Some(mast_proto::CodeRefRange { start_line: 1, end_line: 1 }), symbol: None,
                at: mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: "hash".into() },
            },
            snippet: "code".into(),
        };
        assert_eq!(resolve_pinned_location(&reference, "/workspace").unwrap().path, "vendor/library/src/file.rs");
        let mut unsafe_reference = reference;
        unsafe_reference.reference.location.repo_root = Some("../outside".into());
        assert!(resolve_pinned_location(&unsafe_reference, "/workspace").is_none());
    }
}

#[cfg(feature = "snapshot")]
impl Shell {
    pub(crate) fn fixture_changed_code_ref(&mut self, cx: &mut Context<Self>) {
        let reference = self
            .state
            .read(cx)
            .code_refs("fixture-chat")
            .first()
            .cloned();
        if let Some(reference) = reference {
            self.transcript.update(cx, |transcript, cx| {
                transcript.fixture_changed_code_ref(&reference, cx)
            });
        }
    }
}
