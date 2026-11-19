//! Revision-scoped inputs to the scope map.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::super::{DiffTreeNode, FileDiff, FileStatus};
use super::layout::{Entry, Folder};

#[derive(Clone, Debug)]
pub(super) struct File {
    pub index: usize,
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
    pub status: FileStatus,
    pub risk: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::changes) struct Totals {
    pub(in crate::changes) files: usize,
    pub(in crate::changes) additions: u64,
    pub(in crate::changes) deletions: u64,
}

#[derive(Clone, Debug)]
pub(super) struct Data {
    pub root: Folder,
    pub files: Vec<File>,
    pub folders: Arc<HashMap<String, Totals>>,
    pub totals: Totals,
}

/// The parsed diff key owns the expensive tree and rollups. Callers invalidate
/// this when walkthrough notes change for the same diff.
#[derive(Default)]
pub(in crate::changes) struct Cache {
    entry: Option<(String, Data)>,
}

impl Cache {
    pub(super) fn get(&self) -> Option<&Data> {
        self.entry.as_ref().map(|(_, data)| data)
    }
    pub(super) fn get_or_build(&mut self, key: &str, build: impl FnOnce() -> Data) -> &Data {
        if self.entry.as_ref().is_none_or(|(cached, _)| cached != key) {
            self.entry = Some((key.to_owned(), build()));
        }
        &self.entry.as_ref().expect("cache was populated").1
    }

    pub(in crate::changes) fn invalidate(&mut self) {
        self.entry = None;
    }
}

#[derive(Default)]
struct Builder {
    files: Vec<Entry>,
    folders: BTreeMap<String, Builder>,
}

impl Builder {
    fn insert(&mut self, path: &str, entry: Entry) {
        if let Some((directory, rest)) = path.split_once('/') {
            self.folders
                .entry(directory.into())
                .or_default()
                .insert(rest, entry);
        } else {
            self.files.push(entry);
        }
    }

    fn finish(self, path: &str) -> Folder {
        Folder {
            path: path.to_owned(),
            files: self.files,
            folders: self
                .folders
                .into_iter()
                .map(|(name, child)| {
                    let path = if path.is_empty() {
                        name
                    } else {
                        format!("{path}/{name}")
                    };
                    child.finish(&path)
                })
                .collect(),
        }
    }
}

impl Totals {
    fn add(&mut self, file: &File) {
        self.files += 1;
        self.additions += u64::from(file.additions);
        self.deletions += u64::from(file.deletions);
    }
}

impl Data {
    pub(super) fn new(
        files: &[FileDiff],
        layout: &[DiffTreeNode],
        risks: &HashMap<String, String>,
    ) -> Self {
        fn indices(nodes: &[DiffTreeNode], output: &mut Vec<usize>) {
            for node in nodes {
                match node {
                    DiffTreeNode::File(index) => output.push(*index as usize),
                    DiffTreeNode::Submodule { children, .. } => indices(children, output),
                }
            }
        }
        let mut order = Vec::new();
        indices(layout, &mut order);
        let mut builder = Builder::default();
        let mut result = Self {
            root: Folder::default(),
            files: Vec::with_capacity(order.len()),
            folders: Arc::new(HashMap::new()),
            totals: Totals::default(),
        };
        for index in order {
            let Some(source) = files.get(index) else {
                continue;
            };
            let file = File {
                index,
                path: source.path.clone(),
                additions: source.additions,
                deletions: source.deletions,
                status: source.status,
                risk: risks
                    .get(&source.path)
                    .filter(|s| !s.trim().is_empty())
                    .cloned(),
            };
            builder.insert(
                &file.path,
                Entry {
                    id: index,
                    weight: u64::from(file.additions) + u64::from(file.deletions),
                },
            );
            result.totals.add(&file);
            for (position, _) in file.path.match_indices('/') {
                Arc::make_mut(&mut result.folders)
                    .entry(file.path[..=position].to_owned())
                    .or_default()
                    .add(&file);
            }
            result.files.push(file);
        }
        result.root = builder.finish("");
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_nested_rollups_and_ignores_empty_risks() {
        let mut a = FileDiff::new("src/a.rs".into(), None);
        a.additions = 3;
        let mut b = FileDiff::new("src/nested/b.rs".into(), None);
        b.deletions = 2;
        let risks = HashMap::from([
            ("src/a.rs".into(), "check this".into()),
            ("src/nested/b.rs".into(), " ".into()),
        ]);
        let data = Data::new(
            &[a, b],
            &[DiffTreeNode::File(0), DiffTreeNode::File(1)],
            &risks,
        );
        assert_eq!(
            data.totals,
            Totals {
                files: 2,
                additions: 3,
                deletions: 2,
            }
        );
        assert_eq!(data.folders["src/"], data.totals);
        assert_eq!(data.folders["src/nested/"].deletions, 2);
        assert_eq!(data.files[0].risk.as_deref(), Some("check this"));
        assert_eq!(data.files[1].risk, None);
    }

    #[test]
    fn cache_reuses_a_revision_and_rebuilds_after_invalidation() {
        let mut cache = Cache::default();
        let mut builds = 0;
        for key in ["a", "a", "b"] {
            cache.get_or_build(key, || {
                builds += 1;
                Data::new(&[], &[], &HashMap::new())
            });
        }
        assert_eq!(builds, 2);
        cache.invalidate();
        cache.get_or_build("b", || {
            builds += 1;
            Data::new(&[], &[], &HashMap::new())
        });
        assert_eq!(builds, 3);
    }
}
