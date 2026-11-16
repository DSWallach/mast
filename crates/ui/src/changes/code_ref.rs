//! Hunk reference staging.
use super::*;

pub(super) fn hunk_selection(
    file: &FileDiff,
    hunk: &Hunk,
) -> Option<(String, mast_proto::CodeRefRange, String, bool)> {
    let old_side = hunk.lines.iter().all(|line| line.new_no.is_none());
    let path = if old_side {
        file.old_path.clone().unwrap_or_else(|| file.path.clone())
    } else {
        file.path.clone()
    };
    let lines: Vec<_> = hunk
        .lines
        .iter()
        .filter(|line| {
            if old_side {
                line.old_no.is_some()
            } else {
                line.new_no.is_some()
            }
        })
        .collect();
    let number = |line: &DiffLine| if old_side { line.old_no } else { line.new_no };
    let range = mast_proto::CodeRefRange {
        start_line: number(lines.first()?)?,
        end_line: number(lines.last()?)?,
    };
    let snippet = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Some((path, range, snippet, old_side))
}

impl Changes {
    pub(super) fn add_hunk_to_chat(
        &mut self,
        path: String,
        range: mast_proto::CodeRefRange,
        expected: String,
        old_side: bool,
        cx: &mut Context<Self>,
    ) {
        let key = self.owner_chat_id_owned().unwrap_or_default();
        let state = self.state.read(cx);
        let Some(member) = state.selected_execution_member(&key) else {
            return;
        };
        let Some(context) = crate::files::client::FilesRequestContext::for_chat(state, &key) else {
            return;
        };
        let Some(engine) = state.engine().cloned() else {
            return;
        };
        let commit = match self.scope {
            DiffScope::Commit => self.commit.as_ref().and_then(|commit| {
                if old_side {
                    commit.parent_shas.first().cloned()
                } else {
                    Some(commit.sha.clone())
                }
            }),
            DiffScope::Range if !old_side => self
                .owner
                .as_ref()
                .and_then(|owner| owner.range.as_ref())
                .and_then(|range| range.head_sha.clone()),
            DiffScope::WorkingTree if old_side => member.head_sha.clone(),
            _ => None,
        };
        let lookup_head = old_side && self.scope == DiffScope::WorkingTree && commit.is_none();
        if commit.is_none()
            && !lookup_head
            && (old_side || matches!(self.scope, DiffScope::Commit | DiffScope::Range))
        {
            return;
        }
        let request = mast_proto::ReadWorkspaceFileRequest {
            target: context.target.clone(),
            path: path.clone(),
        };
        let client =
            crate::files::client::WorkspaceFilesClient::new(engine.clone(), context.clone());
        let state_handle = self.state.clone();
        state_handle.update(cx, |state, cx| {
            state.begin_code_ref_load(&key);
            cx.notify();
        });
        cx.spawn(async move |this, cx| {
            let identity = crate::code_ref::repository_identity(&engine, &context).await;
            let commit = if lookup_head {
                let params = serde_json::json!({ "cwd": context.cwd.clone(), "targetDeviceId": context.target_device_id.clone(), "cursor": 0, "limit": 1 });
                engine.client().call(mast_rpc::methods::LIST_GIT_HISTORY, params).await.ok()
                    .and_then(|value| serde_json::from_value::<mast_proto::GitHistoryPage>(value).ok())
                    .and_then(|page| page.head_sha)
            } else { commit };
            let snapshot = if let Some(commit) = commit {
                Some((mast_proto::CodeRefRevision::Commit { commit }, expected.clone()))
            } else if lookup_head { None } else {
                match client.read_file(request).await {
                    Ok(file) if !file.truncated => file.text.and_then(|contents| {
                        let snippet = crate::code_ref::snippet(&contents, range)?;
                        (snippet == expected).then(|| (mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: crate::code_ref::content_hash(&contents) }, snippet))
                    }),
                    _ => None,
                }
            };
            let _ = state_handle.update(cx, |state, cx| { state.end_code_ref_load(&key); cx.notify(); });
            let _ = this.update(cx, |this, cx| {
                let current = crate::files::client::FilesRequestContext::for_chat(this.state.read(cx), &key);
                if current.as_ref() != Some(&context) { return; }
                if let (Some(identity), Some((at, snippet))) = (identity, snapshot) {
                    let reference = crate::code_ref::StagedCodeRef {
                        id: uuid::Uuid::new_v4().to_string(),
                        reference: mast_proto::CodeRef { version: 1,
                            location: mast_proto::CodeRefLocation { repo_id: identity.repo_id, worktree_id: Some(identity.checkout_id).filter(|id| !id.is_empty()), path },
                            kind: mast_proto::CodeRefKind::Hunk, range: Some(range), symbol: None, at,
                        }, snippet,
                    };
                    this.state.update(cx, |state, cx| { state.add_code_ref(&key, reference); cx.notify(); });
                } else { this.error = Some("Could not capture the hunk at its revision.".into()); cx.notify(); }
            });
        }).detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_anchors_new_lines_and_deleted_file_preimage() {
        let diff = parse_unified_diff(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n one\n-old\n+new\n",
        );
        let (path, range, snippet, old) = hunk_selection(&diff[0], &diff[0].hunks[0]).unwrap();
        assert_eq!(
            (
                path.as_str(),
                range.start_line,
                range.end_line,
                snippet.as_str(),
                old
            ),
            ("a.rs", 1, 2, "one\nnew", false)
        );
        let diff = parse_unified_diff(
            "diff --git a/a.rs b/a.rs\ndeleted file mode 100644\n--- a/a.rs\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-one\n-two\n",
        );
        let (_, range, snippet, old) = hunk_selection(&diff[0], &diff[0].hunks[0]).unwrap();
        assert_eq!(
            (range.start_line, range.end_line, snippet.as_str(), old),
            (1, 2, "one\ntwo", true)
        );
    }
}
