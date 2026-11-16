//! Read-only GitHub review threads for an open review view.
use super::*;
use mast_proto::{ReviewSide, ReviewThread, ReviewThreadComment};

const QUERY: &str = "query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $repo) { pullRequest(number: $number) {
    reviewThreads(first: 100, after: $after) { pageInfo { hasNextPage endCursor }
      nodes { id path line originalLine diffSide isOutdated isResolved
        comments(first: 100) { pageInfo { hasNextPage } nodes { id body author { login } } }
      }
    }
  } }
}";

/// Fetch on demand only. Callers cache the result and do not poll while hidden.
pub(super) async fn fetch(
    gh: &dyn GhRunner,
    owner: &str,
    repo: &str,
    number: u64,
) -> Result<Vec<ReviewThread>, String> {
    let number = number.to_string();
    let mut cursor: Option<String> = None;
    let mut threads = Vec::new();
    for _ in 0..20 {
        let mut vars = vec![
            ("owner", owner),
            ("repo", repo),
            ("number", number.as_str()),
        ];
        if let Some(after) = cursor.as_deref() {
            vars.push(("after", after));
        }
        let body = gh.graphql(QUERY, &vars).await?;
        let connection = body
            .pointer("/data/repository/pullRequest/reviewThreads")
            .filter(|value| value.is_object())
            .ok_or("GitHub returned no review threads")?;
        threads.extend(parse_page(connection)?);
        if !connection
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(threads);
        }
        let next = connection
            .pointer("/pageInfo/endCursor")
            .and_then(Value::as_str)
            .filter(|next| !next.is_empty())
            .ok_or("GitHub returned no thread cursor")?;
        if cursor.as_deref() == Some(next) {
            return Err("GitHub repeated a thread cursor".into());
        }
        cursor = Some(next.to_owned());
    }
    Err("GitHub returned too many review thread pages".into())
}

fn parse_page(connection: &Value) -> Result<Vec<ReviewThread>, String> {
    let nodes = connection
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or("Missing thread nodes")?;
    nodes
        .iter()
        .map(|node| {
            let string = |key: &str| {
                node.get(key)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            };
            let comments = node.get("comments").ok_or("Missing thread comments")?;
            if comments
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Err("A review thread has more than 100 comments".into());
            }
            let comments = comments
                .get("nodes")
                .and_then(Value::as_array)
                .ok_or("Missing thread comment nodes")?
                .iter()
                .map(|comment| ReviewThreadComment {
                    id: comment
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    author: comment
                        .pointer("/author/login")
                        .and_then(Value::as_str)
                        .unwrap_or("Deleted user")
                        .to_owned(),
                    body: comment
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                })
                .collect();
            Ok(ReviewThread {
                id: string("id").ok_or("Missing thread id")?.into(),
                path: string("path").ok_or("Missing thread path")?.into(),
                line: node
                    .get("line")
                    .and_then(Value::as_u64)
                    .or_else(|| node.get("originalLine").and_then(Value::as_u64))
                    .and_then(|v| u32::try_from(v).ok()),
                side: match string("diffSide") {
                    Some("LEFT") => Some(ReviewSide::Left),
                    Some("RIGHT") => Some(ReviewSide::Right),
                    _ => None,
                },
                outdated: node
                    .get("isOutdated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                resolved: node
                    .get("isResolved")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                comments,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeGh(AtomicUsize);
    #[async_trait::async_trait]
    impl GhRunner for FakeGh {
        async fn current_user(&self) -> Result<String, String> {
            Ok("reviewer".into())
        }
        async fn graphql(&self, _: &str, _: &[(&str, &str)]) -> Result<Value, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(
                serde_json::json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
                    "pageInfo":{"hasNextPage":false}, "nodes":[]
                }}}}}),
            )
        }
    }

    #[tokio::test]
    async fn repeated_visible_reads_share_the_cache() {
        let gh = Arc::new(FakeGh(AtomicUsize::new(0)));
        let dash = PrDash::new(gh.clone(), PrDashConfig::default());
        for _ in 0..100 {
            assert!(
                dash.review_threads("owner", "repo", 1)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(gh.0.load(Ordering::SeqCst), 1);
        assert!(dash.review_threads("owner", "repo", 0).await.is_err());
        assert_eq!(gh.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn recorded_thread_page() {
        let page = serde_json::json!({"nodes":[{"id":"T1","path":"src/a.rs","line":4,"diffSide":"RIGHT","isOutdated":false,"isResolved":true,
            "comments":{"pageInfo":{"hasNextPage":false},"nodes":[{"id":"C1","body":"Please check","author":{"login":"reviewer"}}]}}]});
        let result = parse_page(&page).unwrap();
        assert_eq!(result[0].comments[0].body, "Please check");
        assert_eq!(result[0].side, Some(ReviewSide::Right));
        assert!(result[0].resolved);
    }
}
