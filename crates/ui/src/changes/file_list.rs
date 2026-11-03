use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileOrigin {
    pub(super) repository_relative_path: String,
    pub(super) repository_path: Option<String>,
    pub(super) nesting_depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SubmoduleGroup {
    pub(super) repository_path: String,
    pub(super) parent_repository_path: Option<String>,
    pub(super) old_revision: Option<String>,
    pub(super) new_revision: Option<String>,
    pub(super) additions: u32,
    pub(super) deletions: u32,
    pub(super) file_count: usize,
    pub(super) truncated: bool,
    /// The parent gitlink's post-change SHA carried Git's `-dirty` suffix.
    /// Expanded children replace that SHA row, so the group must retain it.
    pub(super) dirty: bool,
    pub(super) nesting_depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DiffTreeNode {
    File(u32),
    Submodule {
        group: u32,
        children: Vec<DiffTreeNode>,
    },
}

pub(super) fn nested_display_path(repository_path: &str, path: &str) -> String {
    if path.is_empty() {
        repository_path.to_string()
    } else {
        format!(
            "{}/{}",
            repository_path.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

pub(super) fn submodule_local_path<'a>(
    repository_path: &'a str,
    parent_repository_path: Option<&str>,
) -> Option<&'a str> {
    match parent_repository_path {
        None => Some(repository_path),
        Some(parent) => repository_path.strip_prefix(parent)?.strip_prefix('/'),
    }
}

pub(super) fn gitlink_has_dirty_marker(file: &FileDiff) -> bool {
    file.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .any(|line| line.kind == LineKind::Add && line.text.trim_end().ends_with("-dirty"))
}

/// Parse root and nested patches into one file arena plus an ordered tree.
/// Expanded group nodes replace their owning gitlink file; unavailable
/// sections are deliberately ignored so the root SHA-only file remains.
pub(super) fn parse_diff_tree(
    root_patch: &str,
    submodules: &[SubmoduleDiffSummary],
) -> (
    Vec<FileDiff>,
    Vec<FileOrigin>,
    Vec<SubmoduleGroup>,
    Vec<DiffTreeNode>,
) {
    let mut files = parse_patch(root_patch);
    let mut origins = files
        .iter()
        .map(|file| FileOrigin {
            repository_relative_path: file.path.clone(),
            repository_path: None,
            nesting_depth: 0,
        })
        .collect::<Vec<_>>();
    let root_files = (0..files.len() as u32).collect::<Vec<_>>();
    let mut repository_files: HashMap<String, Vec<u32>> = HashMap::new();
    let mut groups = Vec::new();

    for section in submodules.iter().filter(|section| section.expanded) {
        let section_files = parse_patch(&section.patch);
        // An expanded section without renderable children must not replace
        // the root/parent gitlink. This occurs when a combined payload is
        // truncated before the nested patch arrives; the SHA transition is
        // still the only useful fallback in that case.
        if section_files.is_empty() {
            continue;
        }
        let depth = {
            let mut depth = 0usize;
            let mut parent = section.parent_repository_path.as_deref();
            let mut seen = std::collections::HashSet::new();
            while let Some(path) = parent {
                if !seen.insert(path) || depth >= 16 {
                    break;
                }
                depth += 1;
                parent = submodules
                    .iter()
                    .find(|candidate| candidate.repository_path == path)
                    .and_then(|candidate| candidate.parent_repository_path.as_deref());
            }
            depth
        };
        let group_ix = groups.len() as u32;
        groups.push(SubmoduleGroup {
            repository_path: section.repository_path.clone(),
            parent_repository_path: section.parent_repository_path.clone(),
            old_revision: section.old_revision.clone(),
            new_revision: section.new_revision.clone(),
            additions: section.additions,
            deletions: section.deletions,
            file_count: section.files.len(),
            truncated: section.truncated,
            dirty: false,
            nesting_depth: depth,
        });

        let mut indices = Vec::with_capacity(section_files.len());
        for mut file in section_files {
            let repository_relative_path = file.path.clone();
            file.path = nested_display_path(&section.repository_path, &file.path);
            file.old_path = file
                .old_path
                .take()
                .map(|path| nested_display_path(&section.repository_path, &path));
            indices.push(files.len() as u32);
            files.push(file);
            origins.push(FileOrigin {
                repository_relative_path,
                repository_path: Some(section.repository_path.clone()),
                nesting_depth: depth + 1,
            });
        }
        repository_files.insert(section.repository_path.clone(), indices);
        debug_assert_eq!(group_ix as usize + 1, groups.len());
    }

    // Expanded groups replace their parent gitlink rows. Preserve the one
    // extra state encoded only in those rows: Git's post-change `-dirty`
    // suffix. This works recursively because nested section files share the
    // same arena and carry their owning repository in `origins`.
    for group in &mut groups {
        let local_path = submodule_local_path(
            &group.repository_path,
            group.parent_repository_path.as_deref(),
        );
        let owner_files = match group.parent_repository_path.as_deref() {
            None => Some(root_files.as_slice()),
            Some(parent) => repository_files.get(parent).map(Vec::as_slice),
        };
        group.dirty = local_path
            .zip(owner_files)
            .and_then(|(local_path, owner_files)| {
                owner_files.iter().find_map(|file_ix| {
                    let ix = *file_ix as usize;
                    (origins.get(ix)?.repository_relative_path == local_path)
                        .then(|| files.get(ix))
                        .flatten()
                })
            })
            .is_some_and(gitlink_has_dirty_marker);
    }

    fn build_repository(
        parent: Option<&str>,
        file_indices: &[u32],
        origins: &[FileOrigin],
        groups: &[SubmoduleGroup],
        repository_files: &HashMap<String, Vec<u32>>,
        ancestors: &mut Vec<String>,
    ) -> Vec<DiffTreeNode> {
        let candidate_groups = groups
            .iter()
            .enumerate()
            .filter(|(_, group)| group.parent_repository_path.as_deref() == parent)
            .filter_map(|(ix, group)| {
                submodule_local_path(&group.repository_path, parent)
                    .map(|local| (local.to_string(), ix))
            })
            .collect::<Vec<_>>();
        let mut used = std::collections::HashSet::new();
        let mut nodes = Vec::new();
        for &file_ix in file_indices {
            let Some(origin) = origins.get(file_ix as usize) else {
                continue;
            };
            let Some(group_ix) = candidate_groups.iter().find_map(|(path, group_ix)| {
                (path == &origin.repository_relative_path).then_some(*group_ix)
            }) else {
                nodes.push(DiffTreeNode::File(file_ix));
                continue;
            };
            let group = &groups[group_ix];
            if ancestors.contains(&group.repository_path) {
                nodes.push(DiffTreeNode::File(file_ix));
                continue;
            }
            used.insert(group_ix);
            ancestors.push(group.repository_path.clone());
            let children = build_repository(
                Some(&group.repository_path),
                repository_files
                    .get(&group.repository_path)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                origins,
                groups,
                repository_files,
                ancestors,
            );
            ancestors.pop();
            nodes.push(DiffTreeNode::Submodule {
                group: group_ix as u32,
                children,
            });
        }
        // A truncated parent patch can omit the gitlink header while the
        // structured section still arrived. Keep that section reachable.
        for (_, group_ix) in candidate_groups {
            if used.contains(&group_ix) {
                continue;
            }
            let group = &groups[group_ix];
            if ancestors.contains(&group.repository_path) {
                continue;
            }
            ancestors.push(group.repository_path.clone());
            let children = build_repository(
                Some(&group.repository_path),
                repository_files
                    .get(&group.repository_path)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                origins,
                groups,
                repository_files,
                ancestors,
            );
            ancestors.pop();
            nodes.push(DiffTreeNode::Submodule {
                group: group_ix as u32,
                children,
            });
        }
        nodes
    }

    let layout = build_repository(
        None,
        &root_files,
        &origins,
        &groups,
        &repository_files,
        &mut Vec::new(),
    );
    (files, origins, groups, layout)
}

pub(super) fn logical_file_count(layout: &[DiffTreeNode]) -> usize {
    layout
        .iter()
        .map(|node| match node {
            DiffTreeNode::File(_) => 1,
            DiffTreeNode::Submodule { children, .. } => logical_file_count(children),
        })
        .sum()
}

/// Generated dependency snapshots are usually reviewed through their source manifests.
pub(super) fn is_generated_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "Cargo.lock"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "bun.lock"
            | "bun.lockb"
            | "Gemfile.lock"
            | "Pipfile.lock"
            | "poetry.lock"
            | "uv.lock"
            | "composer.lock"
            | "go.sum"
            | "Podfile.lock"
            | "mix.lock"
    ) || name.ends_with(".min.js")
        || name.ends_with(".min.css")
}

#[cfg(test)]
mod tests {
    use super::is_generated_file;

    #[test]
    fn generated_file_detection_uses_file_names() {
        assert!(is_generated_file("nested/Cargo.lock"));
        assert!(is_generated_file("web/app.min.js"));
        assert!(!is_generated_file("src/lock.rs"));
        assert!(!is_generated_file("Cargo.toml"));
    }
}

fn ordered_file_indices(nodes: &[DiffTreeNode], output: &mut Vec<usize>) {
    for node in nodes {
        match node {
            DiffTreeNode::File(index) => output.push(*index as usize),
            DiffTreeNode::Submodule { children, .. } => ordered_file_indices(children, output),
        }
    }
}

fn file_matches_filter(file: &FileDiff, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || file.path.to_lowercase().contains(&query)
        || file
            .old_path
            .as_ref()
            .is_some_and(|path| path.to_lowercase().contains(&query))
}

impl Changes {
    fn jump_to_file(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(file) = self
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.files.get(index))
        else {
            return;
        };
        if let Some(fold) = self.folds.get_mut(&file.path) {
            fold.collapsed = false;
            fold.toggled_at = None;
        }
        self.submodule_folds.clear();
        self.reflatten(cx);
        if let Some(start) = self
            .row_ranges
            .get(index)
            .and_then(|range| range.as_ref())
            .map(|range| range.start)
        {
            self.list.scroll_to(gpui::ListOffset {
                item_ix: start,
                offset_in_item: px(0.0),
            });
        }
        cx.notify();
    }

    pub(super) fn render_file_list(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(parsed) = self.parsed.as_ref() else {
            return div().into_any_element();
        };
        let query = self.file_filter.read(cx).text().to_string();
        let mut indices = Vec::new();
        ordered_file_indices(&parsed.layout, &mut indices);
        let files = parsed.files.clone();
        let entries = indices
            .into_iter()
            .filter(|index| {
                files
                    .get(*index)
                    .is_some_and(|file| file_matches_filter(file, &query))
            })
            .map(|index| {
                let file = &files[index];
                let label = match &file.old_path {
                    Some(old) if old != &file.path => format!("{old} → {}", file.path),
                    _ => file.path.clone(),
                };
                div()
                    .id(("changes-file-list-entry", index))
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .px(px(6.0))
                    .py(px(4.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.element_hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.jump_to_file(index, cx);
                        window.focus(&this.diff_focus, cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text_dim)
                            .child(SharedString::from(label)),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(add_color(theme))
                            .child(SharedString::from(format!("+{}", file.additions))),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(del_color(theme))
                            .child(SharedString::from(format!("−{}", file.deletions))),
                    )
            })
            .collect::<Vec<_>>();
        div()
            .id("changes-file-list")
            .w(px(240.0))
            .flex_none()
            .min_h_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.border)
            .child(popover::search_input_frame(
                theme,
                self.file_filter.clone().into_any_element(),
            ))
            .child(
                div()
                    .id("changes-file-list-entries")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(entries),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    #[test]
    fn filter_matches_both_rename_paths_case_insensitively() {
        let mut file = FileDiff::new("src/new.rs".into(), Some("src/old.rs".into()));
        file.status = FileStatus::Renamed;
        assert!(file_matches_filter(&file, "OLD"));
        assert!(file_matches_filter(&file, "new.rs"));
        assert!(!file_matches_filter(&file, "missing"));
    }
}

impl Changes {
    pub(super) fn diff_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.keystroke.modifiers;
        if !self.diff_focus.is_focused(window)
            || modifiers.platform
            || modifiers.control
            || modifiers.alt
        {
            return;
        }
        let forward = match event.keystroke.key.as_str() {
            "j" => true,
            "k" => false,
            _ => return,
        };
        self.step_diff(forward, modifiers.shift, cx);
        cx.stop_propagation();
        window.prevent_default();
    }

    fn step_diff(&mut self, forward: bool, files_only: bool, cx: &mut Context<Self>) {
        let Some(parsed) = self.parsed.as_ref() else {
            return;
        };
        let mut files = Vec::new();
        ordered_file_indices(&parsed.layout, &mut files);
        let top = self
            .list
            .logical_scroll_top()
            .item_ix
            .min(self.rows.len().saturating_sub(1));
        let current_file = self.rows.get(top).and_then(|row| row.file());
        if files_only {
            let position =
                current_file.and_then(|file| files.iter().position(|index| *index == file));
            let target = match (position, forward) {
                (Some(index), true) => files.get(index + 1),
                (Some(index), false) => index.checked_sub(1).and_then(|index| files.get(index)),
                (None, true) => files.first(),
                (None, false) => files.last(),
            }
            .copied();
            if let Some(index) = target {
                self.jump_to_file(index, cx);
            }
            return;
        }
        let hunks: Vec<(usize, usize)> = files
            .iter()
            .flat_map(|index| (0..parsed.files[*index].hunks.len()).map(move |hunk| (*index, hunk)))
            .collect();
        let current_hunk = self
            .rows
            .iter()
            .take(top + 1)
            .rev()
            .find_map(|row| match row {
                DiffRow::HunkHeader { file, hunk } if Some(*file as usize) == current_file => {
                    Some((*file as usize, *hunk as usize))
                }
                DiffRow::FileHeader { .. } => None,
                _ => None,
            });
        let position =
            current_hunk.and_then(|item| hunks.iter().position(|candidate| *candidate == item));
        let target = match (position, forward) {
            (Some(index), true) => hunks.get(index + 1),
            (Some(index), false) => index.checked_sub(1).and_then(|index| hunks.get(index)),
            (None, true) => current_file
                .and_then(|file| hunks.iter().find(|(index, _)| *index == file))
                .or_else(|| hunks.first()),
            (None, false) => current_file
                .and_then(|file| hunks.iter().rev().find(|(index, _)| *index < file))
                .or_else(|| hunks.last()),
        }
        .copied();
        if let Some((file, hunk)) = target {
            self.jump_to_file(file, cx);
            if let Some(row) = self.rows.iter().position(|row| {
                *row == DiffRow::HunkHeader {
                    file: file as u32,
                    hunk: hunk as u32,
                }
            }) {
                self.list.scroll_to(gpui::ListOffset {
                    item_ix: row,
                    offset_in_item: px(0.0),
                });
            }
        }
    }
}
