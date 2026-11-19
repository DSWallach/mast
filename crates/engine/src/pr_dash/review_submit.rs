//! Check drafts against GitHub's own diff, then post one batched review.
//! Reached only from the human-clicked `SubmitReview` RPC, which the engine
//! refuses on every transport an agent or remote client can use.
use super::*;
use mast_proto::{
    PrepareReviewParams, PrepareReviewReply, ReviewSide, SubmitReviewComment, SubmitReviewOutcome,
    SubmitReviewParams, SubmitReviewReply, UnplacedDraft, review_marker,
};

const PAGE: usize = 100;
const MAX_PAGES: usize = 10;
const MAX_FILE_PAGES: usize = 30;

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
pub(super) fn payload(params: &SubmitReviewParams, nonce: &str) -> Value {
    let marker = review_marker(nonce);
    let body = match params.body.trim() {
        "" => marker,
        text => format!("{text}\n\n{marker}"),
    };
    serde_json::json!({
        "body": body,
        "event": params.event.github_event(),
        "comments": params.comments.iter().map(comment_json).collect::<Vec<_>>(),
        "commit_id": params.commit_id,
    })
}

fn valid_part(part: &str) -> bool {
    !part.is_empty()
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn valid_pr(owner: &str, repo: &str, number: u64) -> Result<(), String> {
    if valid_part(owner) && valid_part(repo) && number != 0 {
        Ok(())
    } else {
        Err("Invalid pull request".into())
    }
}

fn pr_base(owner: &str, repo: &str, number: u64) -> String {
    format!("repos/{owner}/{repo}/pulls/{number}")
}

fn pr_url(owner: &str, repo: &str, number: u64) -> String {
    format!("https://github.com/{owner}/{repo}/pull/{number}")
}

// ---------------------------------------------------------------------------
// Placement against GitHub's committed diff
// ---------------------------------------------------------------------------

#[derive(Default)]
struct PatchLines {
    right: HashMap<u32, String>,
    left: HashMap<u32, String>,
}

fn hunk_start(header: &str) -> Option<(u32, u32)> {
    // `@@ -12,5 +14,7 @@ context`
    let mut parts = header.strip_prefix("@@ ")?.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let number = |range: &str| range.split(',').next()?.parse::<u32>().ok();
    Some((number(old)?, number(new)?))
}

fn parse_patch(patch: &str) -> PatchLines {
    let mut lines = PatchLines::default();
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    for line in patch.split('\n') {
        if line.starts_with("@@ ") {
            if let Some((o, n)) = hunk_start(line) {
                (old, new, in_hunk) = (o, n, true);
            }
            continue;
        }
        if !in_hunk {
            continue;
        }
        let text = |rest: &str| rest.trim_end_matches('\r').trim_end().to_owned();
        match line.chars().next() {
            Some('+') => {
                lines.right.insert(new, text(&line[1..]));
                new += 1;
            }
            Some('-') => {
                lines.left.insert(old, text(&line[1..]));
                old += 1;
            }
            Some(' ') => {
                lines.right.insert(new, text(&line[1..]));
                lines.left.insert(old, text(&line[1..]));
                old += 1;
                new += 1;
            }
            _ => {}
        }
    }
    lines
}

async fn pull_head(gh: &dyn GhRunner, base: &str) -> Result<String, String> {
    let pull = gh.rest("GET", base, None).await?;
    pull.pointer("/head/sha")
        .and_then(Value::as_str)
        .filter(|sha| !sha.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "GitHub did not report the pull request's head commit".into())
}

async fn pull_files(gh: &dyn GhRunner, base: &str) -> Result<HashMap<String, Option<PatchLines>>, String> {
    let mut files = HashMap::new();
    for page in 1..=MAX_FILE_PAGES {
        let reply = gh
            .rest("GET", &format!("{base}/files?per_page={PAGE}&page={page}"), None)
            .await?;
        let list = reply.as_array().ok_or("GitHub returned an unexpected file list")?;
        for file in list {
            let Some(name) = file.get("filename").and_then(Value::as_str) else {
                continue;
            };
            let parsed = file.get("patch").and_then(Value::as_str).map(parse_patch);
            if let Some(previous) = file.get("previous_filename").and_then(Value::as_str)
                && let Some(parsed) = &parsed
            {
                // A LEFT-side comment on a renamed file names the old path.
                files.insert(previous.to_owned(), Some(PatchLines { right: HashMap::new(), left: parsed.left.clone() }));
            }
            files.insert(name.to_owned(), parsed);
        }
        if list.len() < PAGE {
            break;
        }
    }
    Ok(files)
}

fn norm(text: &str) -> &str {
    text.trim_end_matches('\r').trim_end()
}

fn unplaced_reason(comment: &SubmitReviewComment, files: &HashMap<String, Option<PatchLines>>) -> Option<&'static str> {
    let Some(entry) = files.get(&comment.path) else {
        return Some("this file is not in the pull request's diff");
    };
    let Some(lines) = entry else {
        return Some("GitHub has no diff for this file");
    };
    let check = |side: ReviewSide, line: u32, text: &str| -> Option<&'static str> {
        let map = match side {
            ReviewSide::Left => &lines.left,
            ReviewSide::Right => &lines.right,
        };
        match map.get(&line) {
            None => Some("this line is not in the pull request's diff"),
            Some(found) if found != norm(text) => Some("this line differs from the pull request's committed code"),
            Some(_) => None,
        }
    };
    if let Some(reason) = check(comment.side, comment.line, &comment.line_text) {
        return Some(reason);
    }
    match comment.start_line.filter(|start| *start < comment.line) {
        Some(start) => check(comment.start_side.unwrap_or(comment.side), start, &comment.start_text),
        None => None,
    }
}

/// Validate every draft against the PR's diff at its current head on GitHub,
/// never against a working tree.
pub(super) async fn prepare(gh: &dyn GhRunner, params: &PrepareReviewParams) -> Result<PrepareReviewReply, String> {
    valid_pr(&params.owner, &params.repo, params.number)?;
    let base = pr_base(&params.owner, &params.repo, params.number);
    let head_sha = pull_head(gh, &base).await?;
    let files = pull_files(gh, &base).await?;
    let mut placed = Vec::new();
    let mut unplaced = Vec::new();
    for comment in &params.comments {
        match unplaced_reason(comment, &files) {
            None => placed.push(comment.draft_id.clone()),
            Some(reason) => unplaced.push(UnplacedDraft { draft_id: comment.draft_id.clone(), reason: reason.into() }),
        }
    }
    Ok(PrepareReviewReply { head_sha, placed, unplaced })
}

// ---------------------------------------------------------------------------
// Submit
// ---------------------------------------------------------------------------

fn reply(outcome: SubmitReviewOutcome, review: &Value, fallback_url: &str, sent: Vec<String>) -> SubmitReviewReply {
    SubmitReviewReply {
        outcome,
        review_id: review.get("id").and_then(Value::as_u64),
        url: review
            .get("html_url")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .unwrap_or(fallback_url)
            .to_owned(),
        local_record_error: None,
        sent_draft_ids: sent,
    }
}

fn plain(outcome: SubmitReviewOutcome, url: String) -> SubmitReviewReply {
    SubmitReviewReply { outcome, review_id: None, url, local_record_error: None, sent_draft_ids: Vec::new() }
}

fn is_pending_conflict(error: &str) -> bool {
    error.to_ascii_lowercase().contains("pending review")
}

/// Which of `params.comments` an existing review already carries (best effort:
/// a failure here only leaves the drafts in place).
async fn already_sent(gh: &dyn GhRunner, base: &str, review_id: u64, params: &SubmitReviewParams) -> Vec<String> {
    let Ok(Value::Array(existing)) = gh
        .rest("GET", &format!("{base}/reviews/{review_id}/comments?per_page={PAGE}"), None)
        .await
    else {
        return Vec::new();
    };
    params
        .comments
        .iter()
        .filter(|comment| {
            existing.iter().any(|found| {
                let line = found.get("line").or_else(|| found.get("original_line")).and_then(Value::as_u64);
                found.get("path").and_then(Value::as_str) == Some(comment.path.as_str())
                    && line == Some(u64::from(comment.line))
                    && found.get("body").and_then(Value::as_str).map(str::trim) == Some(comment.body.trim())
            })
        })
        .map(|comment| comment.draft_id.clone())
        .collect()
}

pub(super) async fn submit(
    gh: &dyn GhRunner,
    params: &SubmitReviewParams,
    nonce: &str,
) -> Result<SubmitReviewReply, String> {
    valid_pr(&params.owner, &params.repo, params.number)?;
    if nonce.trim().is_empty() {
        return Err("A review marker is required".into());
    }
    if !params.check_only {
        if params.commit_id.trim().is_empty() {
            return Err("The pull request's head commit is unknown; reopen Submit review".into());
        }
        if params.comments.is_empty() && params.body.trim().is_empty() {
            return Err("Add a summary or at least one comment before submitting".into());
        }
    }
    let base = pr_base(&params.owner, &params.repo, params.number);
    let url = pr_url(&params.owner, &params.repo, params.number);
    let marker = review_marker(nonce);

    // Every submit, the first included, looks for a review that already carries
    // this PR's marker before it sends anything. GitHub only lists a PENDING
    // review to its author, so a pending entry is the reviewer's own.
    let mut has_pending = false;
    for page in 1..=MAX_PAGES {
        let reviews = gh
            .rest("GET", &format!("{base}/reviews?per_page={PAGE}&page={page}"), None)
            .await?;
        let list = reviews.as_array().ok_or("GitHub returned an unexpected review list")?;
        for review in list {
            if review
                .get("body")
                .and_then(Value::as_str)
                .is_some_and(|body| body.contains(&marker))
            {
                let sent = match review.get("id").and_then(Value::as_u64) {
                    Some(id) => already_sent(gh, &base, id, params).await,
                    None => Vec::new(),
                };
                return Ok(reply(SubmitReviewOutcome::AlreadyPosted, review, &url, sent));
            }
            if review.get("state").and_then(Value::as_str) == Some("PENDING") {
                has_pending = true;
            }
        }
        if list.len() < PAGE {
            break;
        }
    }
    if has_pending {
        return Ok(plain(SubmitReviewOutcome::PendingConflict, url));
    }
    if params.check_only {
        return Ok(plain(SubmitReviewOutcome::NotFound, url));
    }
    let head = pull_head(gh, &base).await?;
    if !head.eq_ignore_ascii_case(params.commit_id.trim()) {
        return Err("The pull request has new commits on GitHub. Reopen Submit review to re-check where your comments land.".into());
    }
    match gh.rest("POST", &format!("{base}/reviews"), Some(&payload(params, nonce))).await {
        Ok(review) => {
            if review.get("id").and_then(Value::as_u64).is_none() {
                return Err("GitHub accepted the review but returned no review id".into());
            }
            let sent = params.comments.iter().map(|c| c.draft_id.clone()).collect();
            Ok(reply(SubmitReviewOutcome::Posted, &review, &url, sent))
        }
        Err(error) if is_pending_conflict(&error) => Ok(plain(SubmitReviewOutcome::PendingConflict, url)),
        Err(error) => Err(error),
    }
}

impl PrDash {
    fn submit_lock(&self, owner: &str, repo: &str, number: u64) -> Arc<tokio::sync::Mutex<()>> {
        let key = format!("{}/{}#{number}", owner.to_ascii_lowercase(), repo.to_ascii_lowercase());
        self.inner
            .submit_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key)
            .or_default()
            .clone()
    }

    /// Read-only: where would these drafts land on the PR's head on GitHub?
    pub async fn prepare_review(&self, params: &PrepareReviewParams) -> Result<PrepareReviewReply, String> {
        prepare(self.inner.gh.as_ref(), params).await
    }

    /// Post the review (or find the one an earlier attempt already posted), then
    /// drop the cached threads. At most one submit per PR runs at a time.
    pub async fn submit_review(&self, params: &SubmitReviewParams, nonce: &str) -> Result<SubmitReviewReply, String> {
        valid_pr(&params.owner, &params.repo, params.number)?;
        let lock = self.submit_lock(&params.owner, &params.repo, params.number);
        let Ok(_guard) = lock.try_lock_owned() else {
            return Err("Already posting a review to this pull request".into());
        };
        let reply = submit(self.inner.gh.as_ref(), params, nonce).await;
        if matches!(&reply, Ok(r) if matches!(r.outcome, SubmitReviewOutcome::Posted | SubmitReviewOutcome::AlreadyPosted)) {
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
    use std::time::Duration;

    #[derive(Clone)]
    enum PostMode {
        Ok,
        /// The POST lands on GitHub but its response is lost.
        Lost(&'static str),
        Fail(&'static str),
    }

    /// Recorded GitHub responses; every call is logged, nothing leaves the process.
    struct FakeRest {
        reviews: StdMutex<Vec<Value>>,
        review_comments: StdMutex<Vec<Value>>,
        head: StdMutex<String>,
        mode: StdMutex<PostMode>,
        delay: Duration,
        calls: StdMutex<Vec<(String, String, Option<Value>)>>,
    }

    const PATCH: &str = "@@ -10,3 +10,4 @@ fn a() {\n keep one\n-old line\n+new line\n+added line\n keep two\n";

    impl FakeRest {
        fn new(mode: PostMode) -> Arc<Self> {
            Arc::new(Self {
                reviews: StdMutex::new(Vec::new()),
                review_comments: StdMutex::new(Vec::new()),
                head: StdMutex::new("abc123".into()),
                mode: StdMutex::new(mode),
                delay: Duration::ZERO,
                calls: StdMutex::new(Vec::new()),
            })
        }
        fn slow(delay: Duration) -> Arc<Self> {
            let mut fake = Arc::try_unwrap(Self::new(PostMode::Ok)).ok().unwrap();
            fake.delay = delay;
            Arc::new(fake)
        }
        fn posts(&self) -> Vec<Value> {
            self.calls.lock().unwrap().iter().filter(|c| c.0 == "POST").filter_map(|c| c.2.clone()).collect()
        }
        fn gets(&self, needle: &str) -> usize {
            self.calls.lock().unwrap().iter().filter(|c| c.0 == "GET" && c.1.contains(needle)).count()
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
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            let root = "repos/o/r/pulls/7";
            if method == "GET" {
                if path == root {
                    return Ok(serde_json::json!({"head": {"sha": self.head.lock().unwrap().clone()}}));
                }
                if path.starts_with(&format!("{root}/files")) {
                    return Ok(serde_json::json!([
                        {"filename": "src/a.rs", "patch": PATCH},
                        {"filename": "big.bin"},
                        {"filename": "src/new.rs", "previous_filename": "src/old.rs", "patch": "@@ -1 +1 @@\n-before\n+after\n"}
                    ]));
                }
                if path.contains("/comments") {
                    return Ok(Value::Array(self.review_comments.lock().unwrap().clone()));
                }
                if path.starts_with(&format!("{root}/reviews")) {
                    return Ok(Value::Array(self.reviews.lock().unwrap().clone()));
                }
                return Err(format!("unexpected GET {path}"));
            }
            let mode = self.mode.lock().unwrap().clone();
            let landed = |this: &Self| {
                let review = serde_json::json!({"id": 991, "state": "COMMENTED", "body": body.and_then(|b| b.get("body")).cloned().unwrap_or_default(),
                    "html_url": "https://github.com/o/r/pull/7#pullrequestreview-991"});
                this.reviews.lock().unwrap().push(review.clone());
                review
            };
            match mode {
                PostMode::Ok => Ok(landed(self)),
                PostMode::Lost(error) => {
                    landed(self);
                    Err(error.into())
                }
                PostMode::Fail(error) => Err(error.into()),
            }
        }
    }

    fn comment(id: &str, line: u32, text: &str, start: Option<(u32, &str)>) -> SubmitReviewComment {
        SubmitReviewComment {
            draft_id: id.into(),
            path: "src/a.rs".into(),
            line,
            side: ReviewSide::Right,
            start_line: start.map(|s| s.0),
            start_side: start.map(|_| ReviewSide::Right),
            body: format!("```suggestion\nbody {id}\n```"),
            line_text: text.into(),
            start_text: start.map(|s| s.1.to_owned()).unwrap_or_default(),
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
            commit_id: "abc123".into(),
            check_only: false,
        }
    }

    fn two() -> Vec<SubmitReviewComment> {
        vec![comment("d1", 11, "new line", None), comment("d2", 12, "added line", Some((11, "new line")))]
    }

    #[test]
    fn patch_lines_are_numbered_per_side() {
        let lines = parse_patch(PATCH);
        assert_eq!(lines.right[&10], "keep one");
        assert_eq!(lines.right[&11], "new line");
        assert_eq!(lines.right[&13], "keep two");
        assert_eq!(lines.left[&11], "old line");
        assert_eq!(lines.left[&12], "keep two");
    }

    #[tokio::test]
    async fn prepare_places_against_githubs_diff_and_reports_the_head() {
        let gh = FakeRest::new(PostMode::Ok);
        let mut bad_text = comment("changed", 11, "locally edited line", None);
        bad_text.body = "x".into();
        let mut renamed = comment("left", 1, "before", None);
        renamed.path = "src/old.rs".into();
        renamed.side = ReviewSide::Left;
        let mut missing = comment("nofile", 1, "x", None);
        missing.path = "src/other.rs".into();
        let mut binary = comment("binary", 1, "x", None);
        binary.path = "big.bin".into();
        let prepared = prepare(
            gh.as_ref(),
            &PrepareReviewParams {
                owner: "o".into(),
                repo: "r".into(),
                number: 7,
                comments: vec![comment("ok", 11, "new line", None), comment("range", 12, "added line", Some((11, "new line"))), bad_text,
                    comment("past", 99, "x", None), renamed, missing, binary],
            },
        )
        .await
        .unwrap();
        assert_eq!(prepared.head_sha, "abc123");
        assert_eq!(prepared.placed, ["ok", "range", "left"]);
        let reasons: HashMap<_, _> = prepared.unplaced.iter().map(|u| (u.draft_id.as_str(), u.reason.as_str())).collect();
        assert!(reasons["changed"].contains("differs"));
        assert!(reasons["past"].contains("not in the pull request's diff"));
        assert!(reasons["nofile"].contains("not in the pull request's diff"));
        assert!(reasons["binary"].contains("no diff"));
    }

    #[tokio::test]
    async fn posts_one_batched_review_pinned_to_the_head_with_marker() {
        let gh = FakeRest::new(PostMode::Ok);
        let reply = submit(gh.as_ref(), &params(two()), "nonce-1").await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::Posted);
        assert_eq!(reply.review_id, Some(991));
        assert_eq!(reply.sent_draft_ids, ["d1", "d2"]);
        let posts = gh.posts();
        assert_eq!(posts.len(), 1);
        let body = &posts[0];
        assert_eq!(body["event"], "REQUEST_CHANGES");
        assert_eq!(body["commit_id"], "abc123");
        assert!(body["body"].as_str().unwrap().ends_with("<!-- mast-review:nonce-1 -->"));
        let comments = body["comments"].as_array().unwrap();
        assert_eq!(comments[0]["line"], 11);
        assert!(comments[0].get("start_line").is_none());
        assert_eq!(comments[1]["start_line"], 11);
        assert_eq!(comments[1]["start_side"], "RIGHT");
        assert!(comments[0].get("line_text").is_none(), "placement text is not sent to GitHub");
        assert!(comments[0]["body"].as_str().unwrap().contains("```suggestion"));
    }

    #[tokio::test]
    async fn refuses_without_a_head_or_when_the_head_moved() {
        let gh = FakeRest::new(PostMode::Ok);
        let mut unknown = params(two());
        unknown.commit_id.clear();
        assert!(submit(gh.as_ref(), &unknown, "n").await.unwrap_err().contains("head commit is unknown"));
        *gh.head.lock().unwrap() = "def456".into();
        let error = submit(gh.as_ref(), &params(two()), "n").await.unwrap_err();
        assert!(error.contains("new commits"));
        assert!(gh.posts().is_empty());
    }

    #[tokio::test]
    async fn network_error_is_reported() {
        let gh = FakeRest::new(PostMode::Fail("could not run gh: connection reset"));
        let error = submit(gh.as_ref(), &params(two()), "n").await.unwrap_err();
        assert!(error.contains("connection reset"));
    }

    #[tokio::test]
    async fn timeout_then_retry_finds_the_marker_and_does_not_post_again() {
        let gh = FakeRest::new(PostMode::Lost("gh timed out"));
        assert_eq!(submit(gh.as_ref(), &params(two()), "stable").await.unwrap_err(), "gh timed out");
        assert_eq!(gh.posts().len(), 1);
        // The review landed. A later attempt (even after a restart, with the persisted nonce) sends nothing.
        *gh.review_comments.lock().unwrap() = vec![
            serde_json::json!({"path": "src/a.rs", "line": 11, "body": "```suggestion\nbody d1\n```"}),
        ];
        let reply = submit(gh.as_ref(), &params(two()), "stable").await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::AlreadyPosted);
        assert_eq!(reply.review_id, Some(991));
        assert_eq!(reply.sent_draft_ids, ["d1"], "only drafts the review really carries are reported");
        assert_eq!(gh.posts().len(), 1);
        // A different nonce is a different review.
        *gh.mode.lock().unwrap() = PostMode::Ok;
        let other = submit(gh.as_ref(), &params(two()), "fresh").await.unwrap();
        assert_eq!(other.outcome, SubmitReviewOutcome::Posted);
    }

    #[tokio::test]
    async fn check_only_never_posts() {
        let gh = FakeRest::new(PostMode::Ok);
        let mut check = params(vec![]);
        check.check_only = true;
        check.commit_id.clear();
        assert_eq!(submit(gh.as_ref(), &check, "n").await.unwrap().outcome, SubmitReviewOutcome::NotFound);
        gh.reviews.lock().unwrap().push(serde_json::json!({"id": 4, "state": "COMMENTED", "body": "x <!-- mast-review:n -->"}));
        assert_eq!(submit(gh.as_ref(), &check, "n").await.unwrap().outcome, SubmitReviewOutcome::AlreadyPosted);
        assert!(gh.posts().is_empty());
    }

    #[tokio::test]
    async fn pending_browser_review_is_detected_before_posting() {
        let gh = FakeRest::new(PostMode::Ok);
        gh.reviews.lock().unwrap().push(serde_json::json!({"id": 7, "state": "PENDING", "body": ""}));
        let reply = submit(gh.as_ref(), &params(two()), "n").await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::PendingConflict);
        assert_eq!(reply.url, "https://github.com/o/r/pull/7");
        assert!(gh.posts().is_empty());
    }

    #[tokio::test]
    async fn pending_rejection_from_github_maps_to_conflict() {
        let gh = FakeRest::new(PostMode::Fail("Unprocessable Entity: User can only have one pending review per pull request (HTTP 422)"));
        let reply = submit(gh.as_ref(), &params(two()), "n").await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::PendingConflict);
    }

    #[tokio::test]
    async fn empty_reviews_and_bad_targets_are_refused_without_a_call() {
        let gh = FakeRest::new(PostMode::Ok);
        let mut empty = params(vec![]);
        empty.body = "  ".into();
        assert!(submit(gh.as_ref(), &empty, "n").await.is_err());
        let mut bad = params(two());
        bad.owner = "o/../x".into();
        assert!(submit(gh.as_ref(), &bad, "n").await.is_err());
        assert!(gh.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn success_invalidates_cached_threads() {
        let gh = FakeRest::new(PostMode::Ok);
        let dash = PrDash::new(gh.clone(), PrDashConfig::default());
        dash.review_threads("o", "r", 7).await.unwrap();
        dash.submit_review(&params(two()), "n").await.unwrap();
        assert!(!dash.inner.threads.lock().await.contains_key("o/r#7"));
    }

    #[tokio::test]
    async fn a_second_submit_while_one_is_in_flight_is_refused() {
        let gh = FakeRest::slow(Duration::from_millis(60));
        let dash = PrDash::new(gh.clone(), PrDashConfig::default());
        let first = tokio::spawn({
            let dash = dash.clone();
            async move { dash.submit_review(&params(two()), "n").await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second = dash.submit_review(&params(two()), "n").await.unwrap_err();
        assert!(second.contains("Already posting"));
        assert_eq!(first.await.unwrap().unwrap().outcome, SubmitReviewOutcome::Posted);
        assert_eq!(gh.posts().len(), 1);
        // Once the first finished the PR is free again; the marker stops a repeat.
        assert_eq!(dash.submit_review(&params(two()), "n").await.unwrap().outcome, SubmitReviewOutcome::AlreadyPosted);
        assert_eq!(gh.gets("/reviews?"), 2);
    }

    #[tokio::test]
    async fn a_timed_out_gh_child_is_killed() {
        let dir = std::env::temp_dir().join(format!("mast-gh-kill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("pid");
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg(format!("echo $$ > {}; exec sleep 30", pid_file.display()));
        let error = run_command(command, None, Duration::from_millis(400)).await.unwrap_err();
        assert_eq!(error, "gh timed out");
        let pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
        let mut alive = true;
        for _ in 0..40 {
            alive = std::process::Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success();
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!alive, "the child outlived its timeout");
        std::fs::remove_dir_all(&dir).ok();
    }
}
