//! Check drafts against GitHub's own diff, then post one batched review.
//! Reached only from the human-clicked `SubmitReview` RPC, which the engine
//! refuses on every transport an agent or remote client can use.
use super::*;
use mast_proto::{
    PrepareReviewParams, PrepareReviewReply, ReviewSide, SubmitReviewComment, SubmitReviewOutcome,
    SubmitReviewParams, SubmitReviewReply, UnplacedDraft, review_marker,
};

/// Where the engine keeps a review target's marker nonce and the draft ids of the attempt in
/// flight. Called only while the per-PR submit lock is held.
pub trait SubmitLedger: Send + Sync {
    /// The target's nonce, created and made durable if it has none.
    fn ensure_nonce(&self, target: &str) -> Result<String, String>;
    /// Whether a recorded review already belongs to `nonce`.
    fn nonce_recorded(&self, target: &str, nonce: &str) -> bool;
    /// Compare-and-set: replace the nonce only if it still equals `expected`; returns the one now in effect.
    fn renew_nonce(&self, target: &str, expected: &str) -> Result<String, String>;
    /// Durably save the draft ids about to be posted under `nonce`.
    fn save_attempt(&self, target: &str, nonce: &str, ids: &[String]) -> Result<(), String>;
    fn attempt_ids(&self, target: &str, nonce: &str) -> Option<Vec<String>>;
}

const PAGE: usize = 100;
const MAX_PAGES: usize = 10;
const MAX_FILE_PAGES: usize = 30;
const MAX_COMMENT_PAGES: usize = 30;

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

/// Line number -> (text, index of the hunk it sits in).
#[derive(Default)]
struct PatchLines {
    right: HashMap<u32, (String, u32)>,
    left: HashMap<u32, (String, u32)>,
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
    let mut hunk = 0u32;
    for line in patch.split('\n') {
        if line.starts_with("@@ ") {
            if let Some((o, n)) = hunk_start(line) {
                (old, new, in_hunk) = (o, n, true);
                hunk += 1;
            }
            continue;
        }
        if !in_hunk {
            continue;
        }
        let text = |rest: &str| rest.trim_end_matches('\r').trim_end().to_owned();
        match line.chars().next() {
            Some('+') => {
                lines.right.insert(new, (text(&line[1..]), hunk));
                new += 1;
            }
            Some('-') => {
                lines.left.insert(old, (text(&line[1..]), hunk));
                old += 1;
            }
            Some(' ') => {
                lines.right.insert(new, (text(&line[1..]), hunk));
                lines.left.insert(old, (text(&line[1..]), hunk));
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

/// Why GitHub's diff at the PR head cannot take this comment, as the engine knows it. The
/// UI turns "changed locally" into "not pushed" when the checkout is ahead of the PR.
pub const REASON_NO_DIFF: &str = "no diff on GitHub";
pub const REASON_CHANGED: &str = "changed locally";
pub const REASON_SPANS_HUNKS: &str = "spans hunks";

fn unplaced_reason(comment: &SubmitReviewComment, files: &HashMap<String, Option<PatchLines>>) -> Option<&'static str> {
    let Some(Some(lines)) = files.get(&comment.path) else {
        return Some(REASON_NO_DIFF);
    };
    let map = |side: ReviewSide| match side {
        ReviewSide::Left => &lines.left,
        ReviewSide::Right => &lines.right,
    };
    let agrees = |side: ReviewSide, line: u32, text: &str| map(side).get(&line).is_some_and(|(found, _)| found == norm(text));
    if !agrees(comment.side, comment.line, &comment.line_text) {
        return Some(REASON_CHANGED);
    }
    let start = comment.start_line.filter(|start| *start < comment.line);
    if let Some(start) = start {
        let start_side = comment.start_side.unwrap_or(comment.side);
        let Some((_, start_hunk)) = map(start_side).get(&start) else {
            return Some(REASON_CHANGED);
        };
        if !agrees(start_side, start, &comment.start_text) {
            return Some(REASON_CHANGED);
        }
        if map(comment.side).get(&comment.line).map(|(_, hunk)| hunk) != Some(start_hunk) {
            return Some(REASON_SPANS_HUNKS);
        }
    }
    // The drafted lines plus two either side must read the same on GitHub.
    if comment.context.iter().any(|ctx| !agrees(comment.side, ctx.line, &ctx.text)) {
        return Some(REASON_CHANGED);
    }
    None
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

fn reply(outcome: SubmitReviewOutcome, review: &Value, fallback_url: &str, sent: Vec<String>, exact: bool) -> SubmitReviewReply {
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
        sent_exact: exact,
    }
}

fn plain(outcome: SubmitReviewOutcome, url: String) -> SubmitReviewReply {
    SubmitReviewReply { outcome, review_id: None, url, local_record_error: None, sent_draft_ids: Vec::new(), sent_exact: false }
}

fn is_pending_conflict(error: &str) -> bool {
    error.to_ascii_lowercase().contains("pending review")
}

/// Which of `params.comments` an existing review already carries. Pages through all of the
/// review's comments; a failed or runaway read is an error, never "no match".
async fn already_sent(gh: &dyn GhRunner, base: &str, review_id: u64, params: &SubmitReviewParams) -> Result<Vec<String>, String> {
    let mut existing: Vec<Value> = Vec::new();
    for page in 1..=MAX_COMMENT_PAGES {
        let reply = gh
            .rest("GET", &format!("{base}/reviews/{review_id}/comments?per_page={PAGE}&page={page}"), None)
            .await?;
        let list = reply.as_array().ok_or("GitHub returned an unexpected review comment list")?;
        existing.extend(list.iter().cloned());
        if list.len() < PAGE {
            return Ok(match_sent(&existing, params));
        }
    }
    Err("GitHub returned too many comments on this review to check them".into())
}

fn match_sent(existing: &[Value], params: &SubmitReviewParams) -> Vec<String> {
    params
        .comments
        .iter()
        .filter(|comment| {
            existing.iter().any(|found| {
                // `line` is null when the comment is outdated; `original_line` still names it.
                let line = found
                    .get("line")
                    .filter(|line| !line.is_null())
                    .or_else(|| found.get("original_line"))
                    .and_then(Value::as_u64);
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
    ledger: &dyn SubmitLedger,
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
                // The ids saved before the POST say what that review carries. A text match is only
                // the fallback when nothing was saved (an older attempt), never the authority.
                let (sent, exact) = match ledger.attempt_ids(&params.target, nonce) {
                    Some(saved) => (saved, true),
                    None => match review.get("id").and_then(Value::as_u64) {
                        Some(id) => (already_sent(gh, &base, id, params).await?, false),
                        None => (Vec::new(), false),
                    },
                };
                return Ok(reply(SubmitReviewOutcome::AlreadyPosted, review, &url, sent, exact));
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
    // Durable before the POST: if the response is lost, the marker on GitHub plus these ids
    // are enough to finish the job, even after a restart.
    let ids: Vec<String> = params.comments.iter().map(|c| c.draft_id.clone()).collect();
    ledger
        .save_attempt(&params.target, nonce, &ids)
        .map_err(|e| format!("could not save the review attempt: {e}"))?;
    match gh.rest("POST", &format!("{base}/reviews"), Some(&payload(params, nonce))).await {
        Ok(review) => {
            if review.get("id").and_then(Value::as_u64).is_none() {
                return Err("GitHub accepted the review but returned no review id".into());
            }
            let sent = params.comments.iter().map(|c| c.draft_id.clone()).collect();
            Ok(reply(SubmitReviewOutcome::Posted, &review, &url, sent, true))
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

    /// Post the review (or find the one an earlier attempt already posted), then drop the
    /// cached threads. At most one submit per PR runs at a time, and the nonce is read, checked
    /// against recorded reviews and renewed (compare-and-set) only while holding that lock.
    /// Returns the reply and the nonce it ran under.
    pub async fn submit_review(
        &self,
        params: &SubmitReviewParams,
        ledger: &dyn SubmitLedger,
    ) -> Result<(SubmitReviewReply, String), String> {
        valid_pr(&params.owner, &params.repo, params.number)?;
        let lock = self.submit_lock(&params.owner, &params.repo, params.number);
        let Ok(_guard) = lock.try_lock_owned() else {
            return Err("Already posting a review to this pull request".into());
        };
        let mut nonce = ledger.ensure_nonce(&params.target)?;
        // A nonce that already belongs to a recorded review must not match that review again,
        // or the next drafts could never be posted. A check never renews.
        if !params.check_only && ledger.nonce_recorded(&params.target, &nonce) {
            nonce = ledger.renew_nonce(&params.target, &nonce)?;
        }
        let reply = submit(self.inner.gh.as_ref(), params, &nonce, ledger).await?;
        if matches!(reply.outcome, SubmitReviewOutcome::Posted | SubmitReviewOutcome::AlreadyPosted) {
            self.invalidate_review_threads(&params.owner, &params.repo, params.number).await;
        }
        Ok((reply, nonce))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::ReviewEvent;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    #[derive(Default)]
    struct LedgerState {
        nonce: Option<String>,
        recorded: std::collections::HashSet<String>,
        attempt: Option<(String, Vec<String>)>,
        renews: usize,
    }

    /// In-memory stand-in for the registry; it outlives a "restarted" PrDash like the real one.
    #[derive(Default)]
    struct MemLedger(StdMutex<LedgerState>);

    impl MemLedger {
        fn nonce(&self) -> Option<String> {
            self.0.lock().unwrap().nonce.clone()
        }
        fn renews(&self) -> usize {
            self.0.lock().unwrap().renews
        }
        fn record(&self, nonce: &str) {
            self.0.lock().unwrap().recorded.insert(nonce.into());
        }
    }

    impl SubmitLedger for MemLedger {
        fn ensure_nonce(&self, _: &str) -> Result<String, String> {
            Ok(self.0.lock().unwrap().nonce.get_or_insert_with(|| "n0".into()).clone())
        }
        fn nonce_recorded(&self, _: &str, nonce: &str) -> bool {
            self.0.lock().unwrap().recorded.contains(nonce)
        }
        fn renew_nonce(&self, _: &str, expected: &str) -> Result<String, String> {
            let mut state = self.0.lock().unwrap();
            if state.nonce.as_deref() != Some(expected) {
                return Ok(state.nonce.clone().unwrap_or_default());
            }
            state.renews += 1;
            let next = format!("n{}", state.renews);
            state.nonce = Some(next.clone());
            state.attempt = None;
            Ok(next)
        }
        fn save_attempt(&self, _: &str, nonce: &str, ids: &[String]) -> Result<(), String> {
            self.0.lock().unwrap().attempt = Some((nonce.into(), ids.to_vec()));
            Ok(())
        }
        fn attempt_ids(&self, _: &str, nonce: &str) -> Option<Vec<String>> {
            self.0.lock().unwrap().attempt.as_ref().filter(|(n, _)| n == nonce).map(|(_, ids)| ids.clone())
        }
    }

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
        comments_fail: StdMutex<Option<String>>,
        head: StdMutex<String>,
        mode: StdMutex<PostMode>,
        delay: Duration,
        calls: StdMutex<Vec<(String, String, Option<Value>)>>,
    }

    const PATCH: &str = "@@ -10,3 +10,4 @@ fn a() {\n keep one\n-old line\n+new line\n+added line\n keep two\n@@ -38,2 +39,2 @@ fn b() {\n second\n-gone\n+here\n";

    impl FakeRest {
        fn new(mode: PostMode) -> Arc<Self> {
            Arc::new(Self {
                reviews: StdMutex::new(Vec::new()),
                review_comments: StdMutex::new(Vec::new()),
                comments_fail: StdMutex::new(None),
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
                    if self.comments_fail.lock().unwrap().is_some() {
                        return Err(self.comments_fail.lock().unwrap().clone().unwrap());
                    }
                    let page: usize = path.rsplit("page=").next().and_then(|p| p.parse().ok()).unwrap_or(1);
                    let all = self.review_comments.lock().unwrap().clone();
                    return Ok(Value::Array(all.into_iter().skip((page - 1) * PAGE).take(PAGE).collect()));
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
            context: Vec::new(),
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
        assert_eq!(lines.right[&10], ("keep one".to_owned(), 1));
        assert_eq!(lines.right[&11].0, "new line");
        assert_eq!(lines.right[&13].0, "keep two");
        assert_eq!(lines.left[&11].0, "old line");
        assert_eq!(lines.left[&12].0, "keep two");
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
        assert_eq!(reasons["changed"], REASON_CHANGED);
        assert_eq!(reasons["past"], REASON_CHANGED);
        assert_eq!(reasons["nofile"], REASON_NO_DIFF);
        assert_eq!(reasons["binary"], REASON_NO_DIFF);
    }

    #[tokio::test]
    async fn posts_one_batched_review_pinned_to_the_head_with_marker() {
        let gh = FakeRest::new(PostMode::Ok);
        let reply = submit(gh.as_ref(), &params(two()), "nonce-1", &MemLedger::default()).await.unwrap();
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
        assert!(submit(gh.as_ref(), &unknown, "n", &MemLedger::default()).await.unwrap_err().contains("head commit is unknown"));
        *gh.head.lock().unwrap() = "def456".into();
        let error = submit(gh.as_ref(), &params(two()), "n", &MemLedger::default()).await.unwrap_err();
        assert!(error.contains("new commits"));
        assert!(gh.posts().is_empty());
    }

    #[tokio::test]
    async fn network_error_is_reported() {
        let gh = FakeRest::new(PostMode::Fail("could not run gh: connection reset"));
        let error = submit(gh.as_ref(), &params(two()), "n", &MemLedger::default()).await.unwrap_err();
        assert!(error.contains("connection reset"));
    }

    #[tokio::test]
    async fn lost_response_then_check_reposts_nothing_even_when_the_matcher_misses_a_comment() {
        let gh = FakeRest::new(PostMode::Lost("gh timed out"));
        let ledger = MemLedger::default();
        assert_eq!(submit(gh.as_ref(), &params(two()), "n0", &ledger).await.unwrap_err(), "gh timed out");
        assert_eq!(gh.posts().len(), 1);
        // The review landed, but GitHub's comment list only shows d1 (d2's text doesn't match).
        *gh.review_comments.lock().unwrap() = vec![
            serde_json::json!({"path": "src/a.rs", "line": 11, "body": "```suggestion\nbody d1\n```"}),
        ];
        let mut check = params(two());
        check.check_only = true;
        let reply = submit(gh.as_ref(), &check, "n0", &ledger).await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::AlreadyPosted);
        assert_eq!(reply.review_id, Some(991));
        assert_eq!(reply.sent_draft_ids, ["d1", "d2"], "the ids saved before the POST decide what was sent");
        assert!(reply.sent_exact);
        assert_eq!(gh.gets("/comments"), 0, "the text match is not consulted when ids were saved");
        // A plain retry sends nothing either.
        let again = submit(gh.as_ref(), &params(two()), "n0", &ledger).await.unwrap();
        assert_eq!(again.outcome, SubmitReviewOutcome::AlreadyPosted);
        assert_eq!(gh.posts().len(), 1);
        // A different nonce is a different review.
        *gh.mode.lock().unwrap() = PostMode::Ok;
        assert_eq!(submit(gh.as_ref(), &params(two()), "fresh", &MemLedger::default()).await.unwrap().outcome, SubmitReviewOutcome::Posted);
    }

    #[tokio::test]
    async fn check_only_after_a_partial_record_never_renews_the_nonce() {
        let gh = FakeRest::new(PostMode::Ok);
        let dash = PrDash::new(gh.clone(), PrDashConfig::default());
        let ledger = MemLedger::default();
        ledger.ensure_nonce("t").unwrap();
        ledger.record("n0"); // a partial match recorded a review under n0 and kept the nonce
        let mut check = params(two());
        check.check_only = true;
        let (reply, nonce) = dash.submit_review(&check, &ledger).await.unwrap();
        assert_eq!((reply.outcome, nonce.as_str()), (SubmitReviewOutcome::NotFound, "n0"));
        assert_eq!((ledger.renews(), ledger.nonce().as_deref()), (0, Some("n0")));
        // A real submit retires the used nonce and posts the rest under a new one.
        let (reply, nonce) = dash.submit_review(&params(two()), &ledger).await.unwrap();
        assert_eq!((reply.outcome, nonce.as_str()), (SubmitReviewOutcome::Posted, "n1"));
        assert_eq!(ledger.renews(), 1);
    }

    #[tokio::test]
    async fn concurrent_submits_post_once_and_the_loser_leaves_the_nonce_alone() {
        let gh = FakeRest::slow(Duration::from_millis(60));
        let dash = PrDash::new(gh.clone(), PrDashConfig::default());
        let ledger = Arc::new(MemLedger::default());
        ledger.ensure_nonce("t").unwrap();
        ledger.record("n0"); // the next submit must renew inside the lock
        let first = tokio::spawn({
            let (dash, ledger) = (dash.clone(), ledger.clone());
            async move { dash.submit_review(&params(two()), ledger.as_ref()).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second = dash.submit_review(&params(two()), ledger.as_ref()).await.unwrap_err();
        assert!(second.contains("Already posting"));
        let (reply, nonce) = first.await.unwrap().unwrap();
        assert_eq!((reply.outcome, nonce.as_str()), (SubmitReviewOutcome::Posted, "n1"));
        assert_eq!(gh.posts().len(), 1);
        assert_eq!((ledger.renews(), ledger.nonce().as_deref()), (1, Some("n1")), "one renewal, by the in-flight submit");
    }

    fn landed_review(gh: &FakeRest, comments: Vec<Value>) {
        gh.reviews.lock().unwrap().push(serde_json::json!({"id": 55, "state": "COMMENTED", "body": "x <!-- mast-review:n -->",
            "html_url": "https://github.com/o/r/pull/7#pullrequestreview-55"}));
        *gh.review_comments.lock().unwrap() = comments;
    }

    #[tokio::test]
    async fn marker_match_treats_null_line_as_absent_and_reads_every_page() {
        let gh = FakeRest::new(PostMode::Ok);
        let mut found: Vec<Value> = (0..150).map(|i| serde_json::json!({"path": "elsewhere.rs", "line": i + 1, "body": "filler"})).collect();
        // Outdated comments carry `line: null`; `original_line` names them. The second
        // draft's match sits on page two.
        found.insert(3, serde_json::json!({"path": "src/a.rs", "line": null, "original_line": 11, "body": "```suggestion\nbody d1\n```"}));
        found.push(serde_json::json!({"path": "src/a.rs", "line": 12, "original_line": 12, "body": "```suggestion\nbody d2\n```"}));
        landed_review(&gh, found);
        let reply = submit(gh.as_ref(), &params(two()), "n", &MemLedger::default()).await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::AlreadyPosted);
        assert_eq!(reply.sent_draft_ids, ["d1", "d2"]);
        assert_eq!(gh.gets("/comments?per_page=100&page=2"), 1);
    }

    #[tokio::test]
    async fn a_failed_comment_read_is_an_error_not_a_no_match() {
        let gh = FakeRest::new(PostMode::Ok);
        landed_review(&gh, vec![]);
        *gh.comments_fail.lock().unwrap() = Some("HTTP 502".into());
        assert_eq!(submit(gh.as_ref(), &params(two()), "n", &MemLedger::default()).await.unwrap_err(), "HTTP 502");
        assert!(gh.posts().is_empty());
    }

    #[tokio::test]
    async fn a_partial_match_reports_only_what_matched() {
        let gh = FakeRest::new(PostMode::Ok);
        landed_review(&gh, vec![serde_json::json!({"path": "src/a.rs", "line": 12, "body": "```suggestion\nbody d2\n```"})]);
        let reply = submit(gh.as_ref(), &params(two()), "n", &MemLedger::default()).await.unwrap();
        assert_eq!(reply.sent_draft_ids, ["d2"], "d1 stays a local draft");
    }

    #[tokio::test]
    async fn context_lines_must_agree_with_githubs_patch() {
        let gh = FakeRest::new(PostMode::Ok);
        let ctx = |line, text: &str| mast_proto::ContextLine { line, text: text.into() };
        let mut same = comment("same", 11, "new line", None);
        same.context = vec![ctx(10, "keep one"), ctx(11, "new line"), ctx(12, "added line"), ctx(13, "keep two")];
        let mut drifted = comment("drifted", 11, "new line", None);
        drifted.context = vec![ctx(10, "keep one edited locally"), ctx(11, "new line")];
        let mut beyond = comment("beyond", 11, "new line", None);
        beyond.context = vec![ctx(14, "a line GitHub's hunk does not have")];
        let prepared = prepare(
            gh.as_ref(),
            &PrepareReviewParams { owner: "o".into(), repo: "r".into(), number: 7, comments: vec![same, drifted, beyond] },
        )
        .await
        .unwrap();
        assert_eq!(prepared.placed, ["same"]);
        assert!(prepared.unplaced.iter().all(|u| u.reason == REASON_CHANGED));
        assert_eq!(prepared.unplaced.len(), 2);
    }

    #[tokio::test]
    async fn a_range_across_hunks_is_unplaceable() {
        let gh = FakeRest::new(PostMode::Ok);
        let reply = prepare(
            gh.as_ref(),
            &PrepareReviewParams {
                owner: "o".into(), repo: "r".into(), number: 7,
                comments: vec![comment("two-hunks", 40, "here", Some((11, "new line"))), comment("one-hunk", 12, "added line", Some((11, "new line")))],
            },
        )
        .await
        .unwrap();
        // Line 40 is in a second hunk of the same file.
        assert_eq!(reply.placed, ["one-hunk"]);
        assert_eq!(reply.unplaced[0].reason, REASON_SPANS_HUNKS);
    }

    #[tokio::test]
    async fn restart_after_a_lost_response_finds_the_review_with_the_persisted_nonce() {
        let gh = FakeRest::new(PostMode::Lost("gh timed out"));
        let first = PrDash::new(gh.clone(), PrDashConfig::default());
        let ledger = MemLedger::default();
        assert!(first.submit_review(&params(two()), &ledger).await.is_err());
        // A new engine process: no in-memory state, only the nonce the registry kept.
        let restarted = PrDash::new(gh.clone(), PrDashConfig::default());
        let (reply, _) = restarted.submit_review(&params(two()), &ledger).await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::AlreadyPosted);
        assert_eq!(gh.posts().len(), 1);
    }

    #[tokio::test]
    async fn check_only_never_posts() {
        let gh = FakeRest::new(PostMode::Ok);
        let mut check = params(vec![]);
        check.check_only = true;
        check.commit_id.clear();
        assert_eq!(submit(gh.as_ref(), &check, "n", &MemLedger::default()).await.unwrap().outcome, SubmitReviewOutcome::NotFound);
        gh.reviews.lock().unwrap().push(serde_json::json!({"id": 4, "state": "COMMENTED", "body": "x <!-- mast-review:n -->"}));
        assert_eq!(submit(gh.as_ref(), &check, "n", &MemLedger::default()).await.unwrap().outcome, SubmitReviewOutcome::AlreadyPosted);
        assert!(gh.posts().is_empty());
    }

    #[tokio::test]
    async fn pending_browser_review_is_detected_before_posting() {
        let gh = FakeRest::new(PostMode::Ok);
        gh.reviews.lock().unwrap().push(serde_json::json!({"id": 7, "state": "PENDING", "body": ""}));
        let reply = submit(gh.as_ref(), &params(two()), "n", &MemLedger::default()).await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::PendingConflict);
        assert_eq!(reply.url, "https://github.com/o/r/pull/7");
        assert!(gh.posts().is_empty());
    }

    #[tokio::test]
    async fn pending_rejection_from_github_maps_to_conflict() {
        let gh = FakeRest::new(PostMode::Fail("Unprocessable Entity: User can only have one pending review per pull request (HTTP 422)"));
        let reply = submit(gh.as_ref(), &params(two()), "n", &MemLedger::default()).await.unwrap();
        assert_eq!(reply.outcome, SubmitReviewOutcome::PendingConflict);
    }

    #[tokio::test]
    async fn empty_reviews_and_bad_targets_are_refused_without_a_call() {
        let gh = FakeRest::new(PostMode::Ok);
        let mut empty = params(vec![]);
        empty.body = "  ".into();
        assert!(submit(gh.as_ref(), &empty, "n", &MemLedger::default()).await.is_err());
        let mut bad = params(two());
        bad.owner = "o/../x".into();
        assert!(submit(gh.as_ref(), &bad, "n", &MemLedger::default()).await.is_err());
        assert!(gh.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn success_invalidates_cached_threads() {
        let gh = FakeRest::new(PostMode::Ok);
        let dash = PrDash::new(gh.clone(), PrDashConfig::default());
        dash.review_threads("o", "r", 7).await.unwrap();
        dash.submit_review(&params(two()), &MemLedger::default()).await.unwrap();
        assert!(!dash.inner.threads.lock().await.contains_key("o/r#7"));
    }

    #[tokio::test]
    async fn a_second_submit_while_one_is_in_flight_is_refused() {
        let gh = FakeRest::slow(Duration::from_millis(60));
        let dash = PrDash::new(gh.clone(), PrDashConfig::default());
        let ledger = Arc::new(MemLedger::default());
        let first = tokio::spawn({
            let dash = dash.clone();
            let ledger = ledger.clone();
            async move { dash.submit_review(&params(two()), ledger.as_ref()).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second = dash.submit_review(&params(two()), ledger.as_ref()).await.unwrap_err();
        assert!(second.contains("Already posting"));
        assert_eq!(first.await.unwrap().unwrap().0.outcome, SubmitReviewOutcome::Posted);
        assert_eq!(gh.posts().len(), 1);
        // Once the first finished the PR is free again; the marker stops a repeat.
        assert_eq!(dash.submit_review(&params(two()), ledger.as_ref()).await.unwrap().0.outcome, SubmitReviewOutcome::AlreadyPosted);
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
