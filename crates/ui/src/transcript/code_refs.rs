//! Resolution of references in sent user messages.
use super::*;

fn key(reference: &crate::code_ref::StagedCodeRef) -> String {
    let value = serde_json::to_string(&reference.reference).unwrap_or_default();
    crate::code_ref::content_hash(&format!("{value}\n{}", reference.snippet))
}

impl Transcript {
    pub(super) fn resolve_code_refs(
        &mut self,
        spans: &[crate::composer::SentMentionSpan],
        cx: &mut Context<Self>,
    ) {
        let Some(chat_id) = self.chat_id.as_deref() else {
            return;
        };
        let state = self.state.read(cx);
        let Some(context) = crate::files::client::FilesRequestContext::for_chat(state, chat_id)
        else {
            return;
        };
        let Some(engine) = state.engine().cloned() else {
            return;
        };
        for span in spans {
            let Some(reference) = span.code_ref.clone() else {
                continue;
            };
            let Some(range) = reference.reference.range else {
                continue;
            };
            let id = key(&reference);
            if self.code_ref_resolutions.contains_key(&id) {
                continue;
            }
            self.code_ref_resolutions.insert(id.clone(), None);
            let request = mast_proto::ReadWorkspaceFileRequest {
                target: context.target.clone(),
                path: reference.reference.location.path.clone(),
            };
            let client =
                crate::files::client::WorkspaceFilesClient::new(engine.clone(), context.clone());
            cx.spawn(async move |this, cx| {
                let result = client.read_file(request).await;
                let resolution = result.ok().and_then(|file| {
                    if file.truncated
                        || reference
                            .reference
                            .location
                            .worktree_id
                            .as_ref()
                            .is_some_and(|id| id != &file.checkout_id)
                    {
                        return None;
                    }
                    Some(crate::code_ref::reanchor(
                        file.text.as_deref()?,
                        range,
                        &reference.snippet,
                    ))
                });
                let _ = this.update(cx, |this, cx| {
                    this.code_ref_resolutions.insert(id, resolution);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    pub(super) fn project_code_ref_resolutions(
        &self,
        text: &str,
        spans: &[crate::composer::SentMentionSpan],
    ) -> (SharedString, Arc<Vec<crate::composer::SentMentionSpan>>) {
        let mut display = String::new();
        let mut projected = Vec::new();
        let mut offset = 0;
        for span in spans {
            display.push_str(&text[offset..span.range.start]);
            let start = display.len();
            let mut span = span.clone();
            span.resolution = span.code_ref.as_ref().and_then(|reference| {
                self.code_ref_resolutions
                    .get(&key(reference))
                    .copied()
                    .flatten()
            });
            match span.resolution {
                Some(crate::code_ref::Resolution::Moved(range)) => {
                    let mut reference = span.code_ref.clone().unwrap();
                    reference.reference.range = Some(range);
                    display.push_str(&format!("{} moved", reference.label()));
                    span.path =
                        format!("{}:{}", reference.reference.location.path, range.start_line)
                            .into();
                }
                Some(crate::code_ref::Resolution::Changed) => {
                    display.push_str(&text[span.range.clone()]);
                    display.push_str(" changed");
                }
                _ => display.push_str(&text[span.range.clone()]),
            }
            offset = span.range.end;
            span.range = start..display.len();
            projected.push(span);
        }
        display.push_str(&text[offset..]);
        (display.into(), Arc::new(projected))
    }
}

pub(super) fn activate(
    reference: crate::code_ref::StagedCodeRef,
    state: Entity<AppState>,
    chat_id: String,
    link_ui: Option<render::LinkUi>,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let state = state.read(cx);
    let Some(context) = crate::files::client::FilesRequestContext::for_chat(state, &chat_id) else {
        return;
    };
    let Some(engine) = state.engine().cloned() else {
        return;
    };
    let Some(range) = reference.reference.range else {
        return;
    };
    let request = mast_proto::ReadWorkspaceFileRequest {
        target: context.target.clone(),
        path: reference.reference.location.path.clone(),
    };
    let client = crate::files::client::WorkspaceFilesClient::new(engine, context);
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let result = client.read_file(request).await;
        let resolution = match result {
            Ok(file)
                if !file.truncated
                    && !reference
                        .reference
                        .location
                        .worktree_id
                        .as_ref()
                        .is_some_and(|id| id != &file.checkout_id) =>
            {
                file.text
                    .as_deref()
                    .map(|contents| crate::code_ref::reanchor(contents, range, &reference.snippet))
            }
            Err(crate::files::client::FilesClientError::Request(message))
                if message.to_lowercase().contains("no such file")
                    || message.to_lowercase().contains("not found") =>
            {
                Some(crate::code_ref::Resolution::Changed)
            }
            _ => None,
        };
        let Some(resolution) = resolution else {
            return;
        };
        let target = match resolution {
            crate::code_ref::Resolution::Unchanged(range)
            | crate::code_ref::Resolution::Moved(range) => {
                format!("{}:{}", reference.reference.location.path, range.start_line)
            }
            crate::code_ref::Resolution::Changed => crate::code_ref::pinned_link(&reference),
        };
        let _ = handle.update(cx, |_, window, cx| {
            render::activate_link(
                crate::markdown::links::LinkTarget::new("", &target),
                crate::markdown::links::LinkAction::Primary,
                link_ui.as_ref(),
                window,
                cx,
            )
        });
    })
    .detach();
}
