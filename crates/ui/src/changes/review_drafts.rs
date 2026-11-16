//! Indices for review annotations, rebuilt when the review revision changes.
use super::{Changes, Context, methods};
use crate::comments::CommentSide;
use mast_proto::{ReviewSide, ReviewThread};

impl Changes {
    pub(super) fn load_review_threads(&mut self, target: &str, cx: &mut Context<Self>) {
        self.review_threads_task = None;
        self.review_threads.clear();
        self.review_thread_index = ThreadIndex::default();
        self.review_threads_error = None;
        let Some(pr) = target
            .strip_prefix("pr:")
            .and_then(mast_proto::PullRequestRef::parse_external_id)
        else {
            return;
        };
        let target = target.to_owned();
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::GET_REVIEW_THREADS,
                serde_json::json!({"owner": pr.owner, "repo": pr.repo, "number": pr.number}),
                cx,
            )
        });
        self.review_threads_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|value| {
                serde_json::from_value::<Vec<ReviewThread>>(value)
                    .map_err(|error| error.to_string())
            });
            this.update(cx, |this, cx| {
                if this.review_target.as_deref() != Some(&target) {
                    return;
                }
                this.review_threads_task = None;
                match result {
                    Ok(threads) => {
                        this.review_thread_index = ThreadIndex::new(&threads);
                        this.review_threads = threads;
                        cx.notify();
                    }
                    Err(error) => {
                        this.review_threads_error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }
}
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct ThreadIndex {
    by_anchor: HashMap<String, HashMap<(CommentSide, u32), Vec<usize>>>,
}

impl ThreadIndex {
    pub(super) fn new(threads: &[ReviewThread]) -> Self {
        let mut index = Self::default();
        for (ix, thread) in threads.iter().enumerate() {
            let (Some(side), Some(line)) = (thread.side, thread.line) else {
                continue;
            };
            let side = match side {
                ReviewSide::Left => CommentSide::Old,
                ReviewSide::Right => CommentSide::New,
            };
            index
                .by_anchor
                .entry(thread.path.clone())
                .or_default()
                .entry((side, line))
                .or_default()
                .push(ix);
        }
        index
    }

    pub(super) fn at(&self, path: &str, side: CommentSide, line: u32) -> &[usize] {
        self.by_anchor
            .get(path)
            .and_then(|file| file.get(&(side, line)))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn many_threads_have_direct_anchor_lookups() {
        let threads: Vec<_> = (0..5000)
            .map(|i| ReviewThread {
                id: i.to_string(),
                path: "src/a.rs".into(),
                line: Some(i + 1),
                side: Some(ReviewSide::Right),
                outdated: false,
                resolved: false,
                comments: vec![],
            })
            .collect();
        let index = ThreadIndex::new(&threads);
        assert_eq!(index.by_anchor["src/a.rs"].len(), 5000);
        for i in 0..5000 {
            assert_eq!(index.at("src/a.rs", CommentSide::New, i + 1), &[i as usize]);
        }
        assert!(index.at("src/other.rs", CommentSide::New, 1).is_empty());
    }
}
