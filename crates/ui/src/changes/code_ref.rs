//! Hunk reference staging.
use super::*;

/// A zero new-line count identifies deleted-only hunks without scanning their lines.
pub(super) fn deleted_only_hunk(header: &str) -> bool {
    header
        .split_whitespace()
        .nth(2)
        .and_then(|part| part.strip_prefix('+'))
        .and_then(|part| part.split_once(','))
        .and_then(|(_, count)| count.parse::<u32>().ok())
        == Some(0)
}

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

/// The nested repository and exact commit a submodule hunk was diffed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NestedHunk {
    pub(super) repo_root: String,
    pub(super) revision: String,
}

/// Resolve a hunk's nested repository, its revision on the side the hunk was
/// anchored to, and its path relative to that repository. Nested diffs always
/// compare two commits of the submodule, so both revisions are real SHAs.
pub(super) fn nested_hunk(
    origin: &FileOrigin,
    groups: &[SubmoduleGroup],
    display_path: &str,
    old_side: bool,
) -> Option<(NestedHunk, String)> {
    let repo_root = origin.repository_path.as_deref()?;
    let group = groups
        .iter()
        .find(|group| group.repository_path == repo_root)?;
    let revision = if old_side {
        group.old_revision.as_deref()
    } else {
        group.new_revision.as_deref()
    }?;
    let path = display_path
        .strip_prefix(repo_root)?
        .strip_prefix('/')?
        .to_string();
    (!revision.is_empty() && !path.is_empty()).then(|| {
        (
            NestedHunk {
                repo_root: repo_root.to_string(),
                revision: revision.to_string(),
            },
            path,
        )
    })
}

fn apply_owner_context(
    context: &mut crate::files::client::FilesRequestContext,
    owner: &ChangesTarget,
    local_device_id: Option<&str>,
) {
    if let Some(cwd) = &owner.cwd {
        // The chat's own checkout must keep checkout_path unset: the
        // workspace API accepts an explicit path only for a bound target.
        if context.cwd != *cwd {
            context.target.checkout_path = Some(cwd.clone());
        }
        context.cwd = cwd.clone();
        context.checkout_id = owner
            .checkout_id
            .clone()
            .or_else(|| context.checkout_id.clone());
    }
    context.target_device_id =
        (local_device_id != Some(owner.device_id.as_str())).then(|| owner.device_id.clone());
}

impl Changes {
    pub(super) fn add_indexed_hunk_to_chat(
        &mut self,
        file: usize,
        hunk: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(parsed) = self.parsed.as_ref() else {
            return;
        };
        let Some(selection) = parsed.files.get(file).and_then(|file| {
            file.hunks
                .get(hunk)
                .and_then(|hunk| hunk_selection(file, hunk))
        }) else {
            return;
        };
        let (path, range, snippet, old_side) = selection;
        let nested_origin = parsed
            .origins
            .get(file)
            .filter(|origin| origin.repository_path.is_some());
        let (path, nested) = match nested_origin {
            None => (path, None),
            Some(origin) => match nested_hunk(origin, &parsed.submodules, &path, old_side) {
                Some((nested, path)) => (path, Some(nested)),
                None => {
                    self.error = Some("The submodule revision for this hunk is unavailable.".into());
                    cx.notify();
                    return;
                }
            },
        };
        self.add_hunk_to_chat(path, range, snippet, old_side, nested, cx);
    }

    fn code_ref_context(
        &self,
        state: &AppState,
        key: &str,
    ) -> Option<crate::files::client::FilesRequestContext> {
        let mut context = crate::files::client::FilesRequestContext::for_chat(state, key)?;
        if let Some(owner) = &self.owner {
            apply_owner_context(&mut context, owner, state.local_device_id.as_deref());
        }
        Some(context)
    }

    pub(super) fn add_hunk_to_chat(
        &mut self,
        path: String,
        range: mast_proto::CodeRefRange,
        expected: String,
        old_side: bool,
        nested: Option<NestedHunk>,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.owner_chat_id_owned() else {
            self.error = Some("Open a chat to add this hunk.".into());
            cx.notify();
            return;
        };
        let state = self.state.read(cx);
        let Some(context) = self.code_ref_context(state, &key) else {
            return;
        };
        let Some(engine) = state.engine().cloned() else {
            return;
        };
        let commit = match self.scope {
            _ if nested.is_some() => None,
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
            _ => None,
        };
        let source_refs = match self.scope {
            _ if nested.is_some() => None,
            DiffScope::WorkingTree if old_side => {
                Some(("HEAD".to_string(), "HEAD".to_string(), false))
            }
            DiffScope::Branch if old_side => self
                .base_ref
                .clone()
                .map(|base| (base, "HEAD".to_string(), false)),
            DiffScope::Range => self
                .owner
                .as_ref()
                .and_then(|owner| owner.range.as_ref())
                .map(|range| {
                    (
                        range.base_ref.clone(),
                        range
                            .head_sha
                            .clone()
                            .unwrap_or_else(|| range.head_ref.clone()),
                        !old_side,
                    )
                }),
            _ => None,
        };
        if nested.is_none()
            && commit.is_none()
            && source_refs.is_none()
            && (old_side || matches!(self.scope, DiffScope::Commit | DiffScope::Range))
        {
            self.error = Some("The base revision for this hunk is unavailable.".into());
            cx.notify();
            return;
        }
        let request = mast_proto::ReadWorkspaceFileRequest {
            target: context.target.clone(),
            path: path.clone(),
        };
        let client =
            crate::files::client::WorkspaceFilesClient::new(engine.clone(), context.clone());
        let state_handle = self.state.clone();
        let Some(queue_permit) = crate::code_ref::try_queue_io() else {
            self.error = Some("Too many code references are loading.".into());
            cx.notify();
            return;
        };
        let accepted = state_handle.update(cx, |state, cx| {
            let accepted = state.begin_code_ref_load(&key);
            cx.notify();
            accepted
        });
        if !accepted {
            self.error = Some("Too many code references are loading.".into());
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            let _queue_permit = queue_permit;
            let _permit = crate::code_ref::acquire_io_permit().await;
            let identity = crate::code_ref::repository_identity(&engine, &context).await;
            let snapshot = if let Some(nested) = nested.as_ref() {
                // Nested diffs compare two commits of the submodule: read the
                // anchored side from that repository at its exact revision.
                let params = serde_json::json!({ "cwd": context.cwd.clone(), "checkoutId": context.checkout_id.clone(), "targetDeviceId": context.target_device_id.clone(), "repoRoot": nested.repo_root, "revision": nested.revision, "path": path.clone() });
                engine.client().call(mast_rpc::methods::RESOLVE_CODE_REF_SOURCE, params).await.ok().and_then(|value| {
                    let sha = value.get("revisionSha")?.as_str()?.to_string();
                    let contents = value.get("text")?.as_str()?;
                    let snippet = crate::code_ref::snippet(contents, range)?;
                    (snippet == expected).then_some((mast_proto::CodeRefRevision::Commit { commit: sha }, snippet))
                })
            } else if let Some((base_ref, head_ref, head)) = source_refs {
                let params = serde_json::json!({ "cwd": context.cwd.clone(), "checkoutId": context.checkout_id.clone(), "targetDeviceId": context.target_device_id.clone(), "baseRef": base_ref, "headRef": head_ref, "path": path.clone(), "head": head });
                engine.client().call(mast_rpc::methods::RESOLVE_CODE_REF_SOURCE, params).await.ok().and_then(|value| {
                    let sha = value.get("revisionSha")?.as_str()?.to_string();
                    let contents = value.get("text")?.as_str()?;
                    let snippet = crate::code_ref::snippet(contents, range)?;
                    (snippet == expected).then_some((mast_proto::CodeRefRevision::Commit { commit: sha }, snippet))
                })
            } else if let Some(commit) = commit {
                Some((mast_proto::CodeRefRevision::Commit { commit }, expected.clone()))
            } else {
                match client.read_file(request).await {
                    Ok(file) if !file.truncated => file.text.and_then(|contents| {
                        let snippet = crate::code_ref::snippet(&contents, range)?;
                        (snippet == expected).then(|| (mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: crate::code_ref::content_hash(&contents) }, snippet))
                    }),
                    _ => None,
                }
            };
            state_handle.update(cx, |state, cx| { state.end_code_ref_load(&key); cx.notify(); });
            let _ = this.update(cx, |this, cx| {
                let current = this.code_ref_context(this.state.read(cx), &key);
                if current.as_ref() != Some(&context) { return; }
                if let (Some(identity), Some((at, snippet))) = (identity, snapshot) {
                    let reference = crate::code_ref::StagedCodeRef {
                        id: uuid::Uuid::new_v4().to_string(),
                        reference: mast_proto::CodeRef { version: 1,
                            location: mast_proto::CodeRefLocation { repo_id: identity.repo_id, worktree_id: Some(identity.checkout_id).filter(|id| !id.is_empty()), repo_root: nested.as_ref().map(|nested| nested.repo_root.clone()), path },
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
    fn own_checkout_does_not_become_an_explicit_bound_target() {
        let mut context = crate::files::client::FilesRequestContext {
            target: mast_proto::WorkspaceTarget {
                chat_id: Some("chat".into()),
                space_id: None,
                checkout_path: None,
            },
            target_device_id: None,
            cwd: "/repo".into(),
            checkout_id: Some("checkout".into()),
        };
        let owner = ChangesTarget {
            chat_id: "chat".into(),
            device_id: "local".into(),
            cwd: Some("/repo".into()),
            checkout_id: Some("checkout".into()),
            branch: None,
            unconfirmed: None,
            exact_checkout: false,
            range: None,
        };
        apply_owner_context(&mut context, &owner, Some("local"));
        assert_eq!(context.target.checkout_path, None);
        assert_eq!(context.checkout_id.as_deref(), Some("checkout"));
    }

    #[test]
    fn deleted_only_detection_uses_hunk_header() {
        assert!(deleted_only_hunk("@@ -1,2 +0,0 @@"));
        assert!(!deleted_only_hunk("@@ -1,2 +1,2 @@"));
        assert!(!deleted_only_hunk("@@ -1 +1 @@"));
    }

    #[test]
    fn hunk_anchors_new_lines_and_deleted_file_preimage() {
        let diff = parse_patch(
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
        let diff = parse_patch(
            "diff --git a/a.rs b/a.rs\ndeleted file mode 100644\n--- a/a.rs\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-one\n-two\n",
        );
        let (_, range, snippet, old) = hunk_selection(&diff[0], &diff[0].hunks[0]).unwrap();
        assert_eq!(
            (range.start_line, range.end_line, snippet.as_str(), old),
            (1, 2, "one\ntwo", true)
        );
    }

    /// A parent checkout with a real submodule whose second commit edits `lib.rs`.
    struct SubmoduleFixture {
        _dir: tempfile::TempDir,
        parent: std::path::PathBuf,
        first: String,
        second: String,
        patch: String,
    }

    fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
        let output = mast_harness::git_command()
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn submodule_fixture() -> SubmoduleFixture {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin");
        let parent = dir.path().join("parent");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&parent).unwrap();
        for repo in [&origin, &parent] {
            git_in(repo, &["init", "-q", "-b", "main"]);
            git_in(repo, &["config", "user.name", "Test"]);
            git_in(repo, &["config", "user.email", "test@example.invalid"]);
        }
        std::fs::write(origin.join("lib.rs"), "one\ntwo\nthree\n").unwrap();
        git_in(&origin, &["add", "."]);
        git_in(&origin, &["commit", "-qm", "first"]);
        // `git submodule add` stalls for minutes on some hosts; a local clone plus a
        // gitlink and `.gitmodules` entry yields the same layout without the helper.
        let url = origin.to_str().unwrap();
        git_in(&parent, &["clone", "-q", url, "vendor/ui"]);
        git_in(&parent, &["config", "-f", ".gitmodules", "submodule.vendor/ui.path", "vendor/ui"]);
        git_in(&parent, &["config", "-f", ".gitmodules", "submodule.vendor/ui.url", url]);
        git_in(&parent, &["add", ".gitmodules", "vendor/ui"]);
        git_in(&parent, &["commit", "-qm", "add submodule"]);
        let nested = parent.join("vendor/ui");
        git_in(&nested, &["config", "user.name", "Test"]);
        git_in(&nested, &["config", "user.email", "test@example.invalid"]);
        let first = git_in(&nested, &["rev-parse", "HEAD"]);
        std::fs::write(nested.join("lib.rs"), "one\ntwo\nTWO\nthree\n").unwrap();
        git_in(&nested, &["commit", "-qam", "second"]);
        let second = git_in(&nested, &["rev-parse", "HEAD"]);
        let patch = git_in(&nested, &["diff", "--no-color", "--unified=3", &first, &second]);
        SubmoduleFixture { _dir: dir, parent, first, second, patch }
    }

    fn nested_tree(fixture: &SubmoduleFixture) -> (Vec<FileDiff>, Vec<FileOrigin>, Vec<SubmoduleGroup>) {
        let summary = mast_proto::SubmoduleDiffSummary {
            repository_path: "vendor/ui".into(),
            parent_repository_path: None,
            old_revision: Some(fixture.first.clone()),
            new_revision: Some(fixture.second.clone()),
            expanded: true,
            patch: format!("{}\n", fixture.patch),
            files: Vec::new(),
            additions: 1,
            deletions: 0,
            truncated: false,
        };
        let (files, origins, groups, _) = parse_diff_tree("", &[summary]);
        (files, origins, groups)
    }

    #[test]
    fn submodule_hunk_carries_nested_root_and_submodule_sha() {
        let fixture = submodule_fixture();
        let (files, origins, groups) = nested_tree(&fixture);
        assert_eq!(files.len(), 1);
        let (display_path, range, snippet, old_side) =
            hunk_selection(&files[0], &files[0].hunks[0]).unwrap();
        assert_eq!(display_path, "vendor/ui/lib.rs");
        assert!(!old_side);
        let (nested, path) = nested_hunk(&origins[0], &groups, &display_path, old_side).unwrap();
        assert_eq!(nested.repo_root, "vendor/ui");
        assert_eq!(nested.revision, fixture.second);
        assert_ne!(nested.revision, git_in(&fixture.parent, &["rev-parse", "HEAD"]));
        assert_eq!(path, "lib.rs");

        // The chip opens the workspace file inside the submodule checkout.
        let location = mast_proto::CodeRefLocation {
            repo_id: "repo".into(),
            worktree_id: None,
            repo_root: Some(nested.repo_root.clone()),
            path,
        };
        assert_eq!(location.workspace_path(), "vendor/ui/lib.rs");
        let opened = std::fs::read_to_string(fixture.parent.join(location.workspace_path())).unwrap();
        assert_eq!(crate::code_ref::snippet(&opened, range).as_deref(), Some(snippet.as_str()));
        // The anchored snippet matches the submodule's own commit, not the parent's.
        let at_revision = crate::code_ref::read_commit_file(
            &fixture.parent.join(&nested.repo_root),
            &nested.revision,
            "lib.rs",
        )
        .unwrap();
        assert_eq!(crate::code_ref::snippet(&at_revision, range).as_deref(), Some(snippet.as_str()));

        // After an edit shifts the lines, the reference re-anchors as moved.
        std::fs::write(
            fixture.parent.join("vendor/ui/lib.rs"),
            "zero\none\ntwo\nTWO\nthree\n",
        )
        .unwrap();
        let edited = std::fs::read_to_string(fixture.parent.join(location.workspace_path())).unwrap();
        assert!(matches!(
            crate::code_ref::reanchor(&edited, range, &snippet),
            crate::code_ref::Resolution::Moved(moved) if moved.start_line == range.start_line + 1
        ));
    }

    #[test]
    fn submodule_old_side_uses_the_old_revision_and_missing_revisions_fail() {
        let fixture = submodule_fixture();
        let (_, origins, mut groups) = nested_tree(&fixture);
        let (nested, path) = nested_hunk(&origins[0], &groups, "vendor/ui/lib.rs", true).unwrap();
        assert_eq!((nested.revision.as_str(), path.as_str()), (fixture.first.as_str(), "lib.rs"));
        groups[0].new_revision = None;
        assert!(nested_hunk(&origins[0], &groups, "vendor/ui/lib.rs", false).is_none());
        assert!(nested_hunk(&origins[0], &groups, "other/lib.rs", true).is_none());
    }

    #[test]
    fn reference_without_nested_field_still_reads_and_projects() {
        let old = serde_json::json!({
            "version": 1,
            "where": {"repoId": "repo", "path": "src/a.rs"},
            "kind": "hunk",
            "range": {"startLine": 2, "endLine": 3},
            "at": {"commit": "abc123"}
        });
        let reference: mast_proto::CodeRef = serde_json::from_value(old).unwrap();
        let staged = crate::code_ref::StagedCodeRef {
            id: "old".into(),
            reference,
            snippet: "x\ny".into(),
        };
        assert_eq!(staged.reference.location.workspace_path(), "src/a.rs");
        let prompt = crate::code_ref::with_code_refs("look", std::slice::from_ref(&staged));
        let (_, spans) = crate::code_ref::project_sent_refs(&prompt).unwrap();
        assert_eq!(spans[0].path.as_ref(), "src/a.rs:2");
        assert_eq!(spans[0].code_ref.as_ref().unwrap().reference, staged.reference);
    }
}
