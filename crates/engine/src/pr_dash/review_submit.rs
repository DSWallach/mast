//! Post one batched review to GitHub. Reached only from the human-clicked
//! `SubmitReview` RPC; no agent tool or forwardable method calls this.
use super::*;
use mast_proto::{
    ReviewSide, SubmitReviewComment, SubmitReviewOutcome, SubmitReviewParams, SubmitReviewReply,
    review_marker,
};

const PAGE: usize = 100;
const MAX_PAGES: usize = 10;

fn side(side: ReviewSide) -> &'static str {
    match side {
        ReviewSide::Left => "LEFT",
        ReviewSide::Right => "RIGHT",
    }
}

fn comment_json(comment: &SubmitReviewComment) -> Value {
    let mut value = serde_json::json!({
        "path": comment.path,
        "line": comment.line,
        "side": side(comment.side),
        "body": comment.body,
    });
    if let Some(start) = comment.start_line.filter(|start| *start < comment.line) {
        value["start_line"] = start.into();
        value["start_side"] = side(comment.start_side.unwrap_or(comment.side)).into();
    }
    value
}

/// The `POST …/reviews` body. The marker rides at the end of the review body.
pub(super) fn payload(params: &SubmitReviewParams) -> Value {
    let marker = review_marker(&params.attempt_id);
    let body = match params.body.trim() {
        "" => marker,
        text => format!("{text}\n\n{marker}"),
    };
    let mut value = serde_json::json!({
        "body": body,
        "event": params.event.github_event(),
        "comments": params.comments.iter().map(comment_json).collect::<Vec<_>>(),
    });
    if let Some(commit) = params.commit_id.as_deref().filter(|c| !c.is_empty()) {
        value["commit_id"] = commit.into();
    }
    value
}

fn valid_part(part: &str) -> bool {
    !part.is_empty()
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn reply(outcome: SubmitReviewOutcome, review: &Value, fallback_url: &str) -> SubmitReviewReply {
    SubmitReviewReply {
        outcome,
        review_id: review.get("id").and_then(Value::as_u64),
        url: review
            .get("html_url")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .unwrap_or(fallback_url)
            .to_owned(),
    }
}

fn is_pending_conflict(error: &str) -> bool {
    error.to_ascii_lowercase().contains("pending review")
}

pub(super) async fn submit(
    gh: &dyn GhRunner,
    params: &SubmitReviewParams,
) -> Result<SubmitReviewReply, String> {
    if !valid_part(&params.owner) || !valid_part(&params.repo) || params.number == 0 {
        return Err("Invalid pull request".into());
    }
    if params.attempt_id.trim().is_empty() {
        return Err("A review attempt id is required".into());
    }
    if params.comments.is_empty() && params.body.trim().is_empty() {
        return Err("Add a summary or at least one comment before submitting".into());
    }
    let base = format!("repos/{}/{}/pulls/{}", params.owner, params.repo, params.number);
    let pr_url = format!("https://github.com/{}/{}/pull/{}", params.owner, params.repo, params.number);
    let marker = review_marker(&params.attempt_id);

    // GitHub only lists a PENDING review to its author, so any pending entry
    // here is the reviewer's own (started in the browser).
    let mut has_pending = false;
    for page in 1..=MAX_PAGES {
        let reviews = gh
            .rest("GET", &format!("{base}/reviews?per_page={PAGE}&page={page}"), None)
            .await?;
        let list = reviews.as_array().ok_or("GitHub returned an unexpected review list")?;
        for review in list {
            let state = review.get("state").and_then(Value::as_str).unwrap_or_default();
            if state == "PENDING" {
                has_pending = true;
            } else if review
                .get("body")
                .and_then(Value::as_str)
                .is_some_and(|body| body.contains(&marker))
            {
                return Ok(reply(SubmitReviewOutcome::AlreadyPosted, review, &pr_url));
            }
        }
        if list.len() < PAGE {
            break;
        }
    }
    if has_pending {
        return Ok(SubmitReviewReply {
            outcome: SubmitReviewOutcome::PendingConflict,
            review_id: None,
            url: pr_url,
        });
    }
    match gh.rest("POST", &format!("{base}/reviews"), Some(&payload(params))).await {
        Ok(review) => {
            if review.get("id").and_then(Value::as_u64).is_none() {
                return Err("GitHub accepted the review but returned no review id".into());
            }
            Ok(reply(SubmitReviewOutcome::Posted, &review, &pr_url))
        }
        Err(error) if is_pending_conflict(&error) => Ok(SubmitReviewReply {
            outcome: SubmitReviewOutcome::PendingConflict,
            review_id: None,
            url: pr_url,
        }),
        Err(error) => Err(error),
    }
}

impl PrDash {
    /// Post the review, then drop the cached threads so the next read is fresh.
    pub async fn submit_review(&self, params: &SubmitReviewParams) -> Result<SubmitReviewReply, String> {
        let reply = submit(self.inner.gh.as_ref(), params).await;
        if matches!(&reply, Ok(r) if r.outcome != SubmitReviewOutcome::PendingConflict) {
            self.invalidate_review_threads(&params.owner, &params.repo, params.number).await;
        }
        reply
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::ReviewEvent;
    use std::sync::Mutex as StdMutex;

    /// Recorded GitHub responses; every call is logged, nothing leaves the process.
    struct FakeRest {
        reviews: StdMutex<Vec<Value>>,
        post: StdMutex<Option<Result<Value, String>>>,
        calls: StdMutex<Vec<(String, String, Option<Value>)>>,
    }

    impl FakeRest {
        fn new(reviews: Vec<Value>, post: Result<Value, String>) -> Arc<Self> {
            Arc::new(Self {
                reviews: StdMutex::new(reviews),
                post: StdMutex::new(Some(post)),
                calls: StdMutex::new(Vec::new()),
            })
        }
        fn posts(&self) -> Vec<Value> {
            self.calls.lock().unwrap().iter().filter(|c| c.0 == "POST").filter_map(|c| c.2.clone()).collect()
        }
    }

    #[async_trait::async_trait]
    impl GhRunner for FakeRest {
        async fn current_user(&self) -> Result<String, String> {
            Ok("reviewer".into())
        }
        async fn graphql(&self, _: &str, _: &[(&str, &str)]) -> Result<Value, String> {
            Ok(serde_json::json!({"data":{"repository":{"pullRequest":{"reviewThreads":{"pageInfo":{"hasNextPage":false},"nodes":[]}}}}}))
        }
        async fn rest(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
            self.calls.lock().unwrap().push((method.into(), path.into(), body.cloned()));
            if method == "GET" {
                return Ok(Value::Array(self.reviews.lock().unwrap().clone()));
            }
            self.post.lock().unwrap().take().unwrap_or_else(|| Err("unexpected second POST".into()))
        }
    }

    fn comment(id: &str, start: Option<u32>) -> SubmitReviewComment {
        SubmitReviewComment {
            draft_id: id.into(),
            path: "src/a.rs".into(),
            line: 12,
            side: ReviewSide::Right,
            start_line: start,
            start_side: start.map(|_| ReviewSide::Right),
            body: "```suggestion\nfn a() {}\n```".into(),
        }
    }

    fn params(comments: Vec<SubmitReviewComment>) -> SubmitReviewParams {
        SubmitReviewParams {
            target: "pr:o/r#7".into(),
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
            event: ReviewEvent::RequestChanges,
            body: "Needs work".into(),
            comments,
            commit_id: Some("abc123".into()),
            attempt_id: "attempt-1".into(),
        }
    }

    fn recorded_created() -> Value {
        serde_json::json!({"id": 991, "state": "CHANGES_REQUESTED", "html_url": "https://github.com/o/r/pull/7#pullrequestreview-991"})
    }

    #[tokio::test]
    async fn posts_one_batched_review_with_ranges_and_marker() {
        let gh = FakeRest::new(vec![], Ok(recorded_created()));
        let reply = submit(gh.as_ref(), &params(vec![comment("d1", None), comment("d2", Some(10))])).await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::Posted);
        assert_eq!(reply.review_id, Some(991));
        assert!(reply.url.ends_with("pullrequestreview-991"));
        let posts = gh.posts();
        assert_eq!(posts.len(), 1);
        let body = &posts[0];
        assert_eq!(body["event"], "REQUEST_CHANGES");
        assert_eq!(body["commit_id"], "abc123");
        assert!(body["body"].as_str().unwrap().ends_with("<!-- mast-review:attempt-1 -->"));
        let comments = body["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0]["line"], 12);
        assert_eq!(comments[0]["side"], "RIGHT");
        assert!(comments[0].get("start_line").is_none());
        assert_eq!(comments[1]["start_line"], 10);
        assert_eq!(comments[1]["start_side"], "RIGHT");
        assert!(comments[0]["body"].as_str().unwrap().contains("```suggestion"));
    }

    #[tokio::test]
    async fn network_error_is_reported_and_not_swallowed() {
        let gh = FakeRest::new(vec![], Err("could not run gh: connection reset".into()));
        let error = submit(gh.as_ref(), &params(vec![comment("d1", None)])).await.unwrap_err();
        assert!(error.contains("connection reset"));
    }

    #[tokio::test]
    async fn lost_response_retry_does_not_double_post() {
        // The first POST landed on GitHub but its response was lost: the retry
        // finds the marker in an existing review and sends nothing.
        let existing = serde_json::json!({"id": 55, "state": "COMMENTED", "body": "Needs work\n\n<!-- mast-review:attempt-1 -->",
            "html_url": "https://github.com/o/r/pull/7#pullrequestreview-55"});
        let gh = FakeRest::new(vec![existing], Err("must not be called".into()));
        let reply = submit(gh.as_ref(), &params(vec![comment("d1", None)])).await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::AlreadyPosted);
        assert_eq!(reply.review_id, Some(55));
        assert!(gh.posts().is_empty());
    }

    #[tokio::test]
    async fn another_attempts_marker_does_not_block_a_new_review() {
        let other = serde_json::json!({"id": 5, "state": "COMMENTED", "body": "x <!-- mast-review:attempt-0 -->"});
        let gh = FakeRest::new(vec![other], Ok(recorded_created()));
        let reply = submit(gh.as_ref(), &params(vec![comment("d1", None)])).await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::Posted);
    }

    #[tokio::test]
    async fn pending_browser_review_is_detected_before_posting() {
        let pending = serde_json::json!({"id": 7, "state": "PENDING", "body": ""});
        let gh = FakeRest::new(vec![pending], Err("must not be called".into()));
        let reply = submit(gh.as_ref(), &params(vec![comment("d1", None)])).await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::PendingConflict);
        assert_eq!(reply.url, "https://github.com/o/r/pull/7");
        assert!(gh.posts().is_empty());
    }

    #[tokio::test]
    async fn pending_rejection_from_github_maps_to_conflict() {
        let gh = FakeRest::new(vec![], Err("Unprocessable Entity: User can only have one pending review per pull request (HTTP 422)".into()));
        let reply = submit(gh.as_ref(), &params(vec![comment("d1", None)])).await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::PendingConflict);
    }

    #[tokio::test]
    async fn empty_reviews_and_bad_targets_are_refused_without_a_call() {
        let gh = FakeRest::new(vec![], Ok(recorded_created()));
        let mut empty = params(vec![]);
        empty.body = "  ".into();
        assert!(submit(gh.as_ref(), &empty).await.is_err());
        let mut bad = params(vec![comment("d1", None)]);
        bad.owner = "o/../x".into();
        assert!(submit(gh.as_ref(), &bad).await.is_err());
        assert!(gh.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn success_invalidates_cached_threads() {
        let gh = FakeRest::new(vec![], Ok(recorded_created()));
        let dash = PrDash::new(gh.clone(), PrDashConfig::default());
        dash.review_threads("o", "r", 7).await.unwrap();
        dash.submit_review(&params(vec![comment("d1", None)])).await.unwrap();
        assert!(!dash.inner.threads.lock().await.contains_key("o/r#7"));
    }
}
