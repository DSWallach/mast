use super::*;

/// Match a history commit using the same semantics as the history UI.
///
/// This remains the shared entry point so RPC filtering and client-side
/// filtering cannot disagree about Unicode case normalization.
pub fn git_history_matches(query: &str, commit: &GitHistoryCommit) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let normalized = query.to_ascii_lowercase();
    commit.sha.to_ascii_lowercase().starts_with(&normalized)
        || fuzzy_score(query, &format!("{} {}", commit.sha, commit.subject)).is_some()
}

/// Contract hidden commits to their nearest visible ancestors. Search results
/// remain sparse without leaving graph lanes aimed at rows that are absent.
pub(super) fn compact_history_commits(
    commits: &[GitHistoryCommit],
    visible: &HashSet<String>,
) -> Vec<GitHistoryCommit> {
    let by_sha: HashMap<_, _> = commits
        .iter()
        .map(|commit| (commit.sha.as_str(), commit))
        .collect();

    /// Resolve a hidden commit to its nearest visible ancestors without using
    /// the call stack. Completed nodes are cached across visible commits so a
    /// shared hidden branch is contracted only once.
    fn nearest_visible_parents(
        sha: &str,
        visible: &HashSet<String>,
        by_sha: &HashMap<&str, &GitHistoryCommit>,
        memo: &mut HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        if visible.contains(sha) || !by_sha.contains_key(sha) {
            return vec![sha.to_string()];
        }
        if let Some(cached) = memo.get(sha) {
            return cached.clone();
        }

        struct Frame {
            sha: String,
            next_parent: usize,
            resolved: Vec<String>,
            seen: HashSet<String>,
        }

        impl Frame {
            fn new(sha: String) -> Self {
                Self {
                    sha,
                    next_parent: 0,
                    resolved: Vec::new(),
                    seen: HashSet::new(),
                }
            }

            fn extend(&mut self, parents: &[String]) {
                for parent in parents {
                    if self.seen.insert(parent.clone()) {
                        self.resolved.push(parent.clone());
                    }
                }
            }
        }

        let mut visiting = HashSet::from([sha.to_string()]);
        let mut stack = vec![Frame::new(sha.to_string())];
        loop {
            let next_parent = {
                let frame = stack.last_mut().expect("history traversal frame");
                let parents = &by_sha[frame.sha.as_str()].parent_shas;
                (frame.next_parent < parents.len()).then(|| {
                    let parent = parents[frame.next_parent].clone();
                    frame.next_parent += 1;
                    parent
                })
            };

            let Some(parent) = next_parent else {
                let frame = stack.pop().expect("history traversal frame");
                visiting.remove(&frame.sha);
                let resolved = frame.resolved;
                memo.insert(frame.sha, resolved.clone());
                if let Some(caller) = stack.last_mut() {
                    caller.extend(&resolved);
                    continue;
                }
                return resolved;
            };

            let resolved = if visible.contains(&parent) || !by_sha.contains_key(parent.as_str()) {
                Some(vec![parent.clone()])
            } else if let Some(cached) = memo.get(&parent) {
                Some(cached.clone())
            } else if visiting.contains(&parent) {
                // Git commit graphs are acyclic, but keep malformed input from
                // looping forever just as the previous `visiting` guard did.
                Some(Vec::new())
            } else {
                None
            };

            if let Some(resolved) = resolved {
                stack
                    .last_mut()
                    .expect("history traversal frame")
                    .extend(&resolved);
            } else {
                visiting.insert(parent.clone());
                stack.push(Frame::new(parent));
            }
        }
    }

    let mut memo = HashMap::new();
    commits
        .iter()
        .filter(|commit| visible.contains(&commit.sha))
        .cloned()
        .map(|mut commit| {
            let mut seen = HashSet::new();
            commit.parent_shas = commit
                .parent_shas
                .iter()
                .flat_map(|parent| nearest_visible_parents(parent, visible, &by_sha, &mut memo))
                .filter(|parent| seen.insert(parent.clone()))
                .collect();
            commit
        })
        .collect()
}

pub(super) fn bounded_field(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[derive(serde::Deserialize)]
pub(super) struct GitHubCommitAvatar {
    sha: String,
    author: Option<GitHubAvatarUser>,
    commit: GitHubCommitMetadata,
}

#[derive(serde::Deserialize)]
pub(super) struct GitHubAvatarUser {
    avatar_url: String,
}

#[derive(serde::Deserialize)]
pub(super) struct GitHubCommitMetadata {
    author: GitHubCommitAuthor,
}

#[derive(serde::Deserialize)]
pub(super) struct GitHubCommitAuthor {
    email: String,
}

pub(super) fn parse_github_remote(remote: &str) -> Option<(String, String)> {
    let remote = remote.trim();
    let path = remote
        .strip_prefix("https://github.com/")
        .or_else(|| remote.strip_prefix("http://github.com/"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .or_else(|| remote.strip_prefix("git@github.com:"))?;
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty()
        || repo.is_empty()
        || parts.next().is_some()
        || !owner
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        || !repo.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

pub(super) fn parse_history_log(
    output: &str,
    refs_by_sha: &HashMap<String, Vec<GitHistoryRef>>,
) -> Vec<GitHistoryCommit> {
    let fields: Vec<&str> = output.split('\0').collect();
    fields
        .chunks(6)
        .filter_map(|record| {
            if record.len() != 6 {
                return None;
            }
            let sha = record[0].trim_start_matches(['\r', '\n']);
            if sha.is_empty() {
                return None;
            }
            Some(GitHistoryCommit {
                sha: sha.to_string(),
                parent_shas: record[1]
                    .split_ascii_whitespace()
                    .map(str::to_string)
                    .collect(),
                subject: bounded_field(record[2], 4_096),
                author_name: bounded_field(record[3], 512),
                author_email: bounded_field(record[4], 512),
                authored_at: bounded_field(record[5], 128),
                refs: refs_by_sha.get(sha).cloned().unwrap_or_default(),
            })
        })
        .collect()
}

pub(super) fn parse_history_refs(output: &str) -> HashMap<String, Vec<GitHistoryRef>> {
    let fields: Vec<&str> = output.split('\0').collect();
    let mut refs_by_sha: HashMap<String, Vec<GitHistoryRef>> = HashMap::new();
    for record in fields.chunks(6) {
        if record.len() != 6 {
            continue;
        }
        let full_name = record[0].trim_start_matches(['\r', '\n']);
        let object_sha = record[1];
        let object_type = record[2];
        let peeled_sha = record[3];
        let peeled_type = record[4];
        let symbolic_target = record[5];
        if full_name.is_empty() || !symbolic_target.is_empty() {
            continue;
        }
        let Some((kind, label)) = (if let Some(label) = full_name.strip_prefix("refs/heads/") {
            Some((GitHistoryRefKind::Branch, label))
        } else if let Some(label) = full_name.strip_prefix("refs/remotes/") {
            Some((GitHistoryRefKind::Remote, label))
        } else {
            full_name
                .strip_prefix("refs/tags/")
                .map(|label| (GitHistoryRefKind::Tag, label))
        }) else {
            continue;
        };
        let target_sha = if object_type == "commit" {
            object_sha
        } else if object_type == "tag" && peeled_type == "commit" {
            peeled_sha
        } else {
            continue;
        };
        refs_by_sha
            .entry(target_sha.to_string())
            .or_default()
            .push(GitHistoryRef {
                kind,
                label: bounded_field(label, 1_024),
            });
    }
    for refs in refs_by_sha.values_mut() {
        refs.sort_by(|left, right| {
            let order = |kind| match kind {
                GitHistoryRefKind::Branch => 0,
                GitHistoryRefKind::Tag => 1,
                GitHistoryRefKind::Remote => 2,
            };
            order(left.kind)
                .cmp(&order(right.kind))
                .then_with(|| left.label.cmp(&right.label))
        });
    }
    refs_by_sha
}

impl Repos {
    /// Public commit history in topological order. Only user-facing branches,
    /// remotes, and tags seed the walk, so Mast's internal refs never leak
    /// into the graph or keep otherwise-unreachable checkpoints visible.
    pub async fn history(
        &self,
        repo_path: &Path,
        cursor: usize,
        limit: usize,
    ) -> Result<GitHistoryPage, EngineError> {
        let limit = limit.clamp(1, GIT_HISTORY_MAX_LIMIT);
        let head_sha = self
            .git(&["rev-parse", "--verify", "HEAD^{commit}"], Some(repo_path))
            .await
            .ok()
            .filter(|sha| !sha.is_empty());

        let refs_out = self
            .git(
                &[
                    "for-each-ref",
                    "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(*objectname)%00%(*objecttype)%00%(symref)%00",
                    "refs/heads",
                    "refs/remotes",
                    "refs/tags",
                ],
                Some(repo_path),
            )
            .await?;
        let refs_by_sha = parse_history_refs(&refs_out);

        if head_sha.is_none() && refs_by_sha.is_empty() {
            return Ok(GitHistoryPage {
                commits: Vec::new(),
                branch_tips: Vec::new(),
                head_sha: None,
                next_cursor: None,
                total_count: Some(0),
                head_commit_count: Some(0),
                comparison: None,
            });
        }

        let skip = format!("--skip={cursor}");
        let max_count = format!("--max-count={}", limit + 1);
        let mut log_args = vec![
            "log",
            "--topo-order",
            "--no-color",
            "--no-decorate",
            "--no-show-signature",
            "--no-patch",
            skip.as_str(),
            max_count.as_str(),
            "--format=%H%x00%P%x00%s%x00%an%x00%ae%x00%aI%x00",
        ];
        if head_sha.is_some() {
            log_args.push("HEAD");
        }
        log_args.extend(["--branches", "--remotes", "--tags"]);
        let log = self.git(&log_args, Some(repo_path)).await?;
        let mut commits = parse_history_log(&log, &refs_by_sha);
        let has_next = commits.len() > limit;
        commits.truncate(limit);

        // Branch tips are deliberately independent from history pagination.
        // `--no-walk` resolves every local/remote tip while deduplicating refs
        // that point at the same commit. Include HEAD as a useful anchor for a
        // detached checkout, but do not let tags seed extra overview rows.
        let branch_tips = if cursor == 0 {
            let mut tip_args = vec![
                "log",
                "--no-walk=sorted",
                "--no-color",
                "--no-decorate",
                "--no-show-signature",
                "--no-patch",
                "--format=%H%x00%P%x00%s%x00%an%x00%ae%x00%aI%x00",
            ];
            if head_sha.is_some() {
                tip_args.push("HEAD");
            }
            tip_args.extend(["--branches", "--remotes"]);
            let tips = self.git(&tip_args, Some(repo_path)).await?;
            parse_history_log(&tips, &refs_by_sha)
        } else {
            Vec::new()
        };

        let total_count = if cursor == 0 {
            let mut count_args = vec!["rev-list", "--count"];
            if head_sha.is_some() {
                count_args.push("HEAD");
            }
            count_args.extend(["--branches", "--remotes", "--tags"]);
            self.git(&count_args, Some(repo_path))
                .await
                .ok()
                .and_then(|count| count.parse().ok())
        } else {
            None
        };
        let head_commit_count = if cursor == 0 && head_sha.is_some() {
            self.git(&["rev-list", "--count", "HEAD"], Some(repo_path))
                .await
                .ok()
                .and_then(|count| count.parse().ok())
        } else {
            None
        };
        let comparison = if cursor == 0 && head_sha.is_some() {
            self.history_comparison(repo_path).await
        } else {
            None
        };

        Ok(GitHistoryPage {
            next_cursor: has_next.then_some(cursor + commits.len()),
            commits,
            branch_tips,
            head_sha,
            total_count,
            head_commit_count,
            comparison,
        })
    }

    /// Compare the checked-out branch with the best locally available
    /// integration ref. This deliberately never talks to the network: `Fetch
    /// all` refreshes remote-tracking refs, then the next History load sees
    /// those new counts.
    async fn history_comparison(&self, repo_path: &Path) -> Option<GitHistoryComparison> {
        // Detached checkouts do not have a branch relationship to present.
        self.git(
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            Some(repo_path),
        )
        .await
        .ok()?;

        // An integration remote is more useful than the branch's push/tracking
        // ref: a feature usually tracks origin/feature, whereas the status the
        // user needs in History is its relationship to upstream/main.
        let mut candidates = Vec::new();
        for remote in ["upstream", "origin"] {
            let remote_head = format!("refs/remotes/{remote}/HEAD");
            if let Ok(base) = self
                .git(
                    &["symbolic-ref", "--quiet", "--short", &remote_head],
                    Some(repo_path),
                )
                .await
                && !base.is_empty()
            {
                candidates.push(base);
            }
        }
        // A remote HEAD is not guaranteed to have been configured locally.
        // Conventional default names keep the result useful in that case.
        candidates.extend([
            "upstream/main".to_string(),
            "origin/main".to_string(),
            "upstream/master".to_string(),
            "origin/master".to_string(),
        ]);
        // Fall back to the configured tracking ref only after integration
        // defaults. This still gives sensible data in a single-remote repo.
        if let Ok(base) = self
            .git(
                &[
                    "rev-parse",
                    "--abbrev-ref",
                    "--symbolic-full-name",
                    "@{upstream}",
                ],
                Some(repo_path),
            )
            .await
            && !base.is_empty()
        {
            candidates.push(base);
        }

        let mut seen = HashSet::new();
        for base in candidates
            .into_iter()
            .filter(|base| seen.insert(base.clone()))
        {
            let commit_ref = format!("{base}^{{commit}}");
            if self
                .git(
                    &["rev-parse", "--verify", "--quiet", &commit_ref],
                    Some(repo_path),
                )
                .await
                .is_err()
            {
                continue;
            }
            let range = format!("HEAD...{base}");
            let Ok(counts) = self
                .git(
                    &["rev-list", "--left-right", "--count", &range],
                    Some(repo_path),
                )
                .await
            else {
                continue;
            };
            let mut counts = counts.split_whitespace();
            let (Some(ahead), Some(behind)) = (counts.next(), counts.next()) else {
                continue;
            };
            let (Ok(ahead), Ok(behind)) = (ahead.parse(), behind.parse()) else {
                continue;
            };
            return Some(GitHistoryComparison {
                base,
                ahead,
                behind,
            });
        }
        None
    }

    /// Fuzzy subject / SHA search across the complete public history. Results
    /// stay in `--topo-order`; fuzzy score decides inclusion, never row order,
    /// so the client can keep rendering a meaningful commit graph.
    pub async fn search_history(
        &self,
        repo_path: &Path,
        query: &str,
        cursor: usize,
        limit: usize,
    ) -> Result<GitHistoryPage, EngineError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(GitHistoryPage {
                commits: Vec::new(),
                branch_tips: Vec::new(),
                head_sha: None,
                next_cursor: None,
                total_count: Some(0),
                head_commit_count: None,
                comparison: None,
            });
        }
        let limit = limit.clamp(1, GIT_HISTORY_MAX_LIMIT);
        let head_sha = self
            .git(&["rev-parse", "--verify", "HEAD^{commit}"], Some(repo_path))
            .await
            .ok()
            .filter(|sha| !sha.is_empty());
        let refs_out = self
            .git(
                &[
                    "for-each-ref",
                    "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(*objectname)%00%(*objecttype)%00%(symref)%00",
                    "refs/heads",
                    "refs/remotes",
                    "refs/tags",
                ],
                Some(repo_path),
            )
            .await?;
        let refs_by_sha = parse_history_refs(&refs_out);
        if head_sha.is_none() && refs_by_sha.is_empty() {
            return Ok(GitHistoryPage {
                commits: Vec::new(),
                branch_tips: Vec::new(),
                head_sha: None,
                next_cursor: None,
                total_count: Some(0),
                head_commit_count: None,
                comparison: None,
            });
        }

        let mut log_args = vec![
            "log",
            "--topo-order",
            "--no-color",
            "--no-decorate",
            "--no-show-signature",
            "--no-patch",
            "--format=%H%x00%P%x00%s%x00%an%x00%ae%x00%aI%x00",
        ];
        if head_sha.is_some() {
            log_args.push("HEAD");
        }
        log_args.extend(["--branches", "--remotes", "--tags"]);
        let log = self.git(&log_args, Some(repo_path)).await?;
        let all_commits = parse_history_log(&log, &refs_by_sha);
        let visible: HashSet<String> = all_commits
            .iter()
            .filter(|commit| git_history_matches(query, commit))
            .map(|commit| commit.sha.clone())
            .collect();
        let matches = compact_history_commits(&all_commits, &visible);
        let total_count = matches.len();
        let start = cursor.min(total_count);
        let end = start.saturating_add(limit).min(total_count);
        let commits = matches[start..end].to_vec();

        Ok(GitHistoryPage {
            commits,
            branch_tips: Vec::new(),
            head_sha,
            next_cursor: (end < total_count).then_some(end),
            total_count: Some(total_count),
            head_commit_count: None,
            comparison: None,
        })
    }

    /// Best-effort GitHub profile images for the authors in one history page.
    /// Git itself only stores names and emails, so this resolves the hosting
    /// metadata separately and caches it by both commit and normalized email.
    pub async fn history_avatar_urls(
        &self,
        repo_path: &Path,
        authors: &[(String, String)],
        cursor: usize,
        limit: usize,
    ) -> HashMap<String, String> {
        let Ok(remote) = self
            .git(&["remote", "get-url", "origin"], Some(repo_path))
            .await
        else {
            return HashMap::new();
        };
        let Some((owner, repo)) = parse_github_remote(&remote) else {
            return HashMap::new();
        };
        let repo_key = format!("{owner}/{repo}").to_ascii_lowercase();
        let per_page = limit.clamp(1, 100);
        let page = cursor / per_page + 1;
        let page_key = format!("{repo_key}|{page}|{per_page}");
        let should_fetch = self
            .inner
            .github_avatar_pages
            .lock()
            .map(|mut pages| pages.insert(page_key.clone()))
            .unwrap_or(false);

        if should_fetch {
            let url = format!("https://api.github.com/repos/{owner}/{repo}/commits");
            let mut request = self.inner.http.get(url).query(&[
                ("per_page", per_page.to_string()),
                ("page", page.to_string()),
            ]);
            if let Some(token) = std::env::var("GITHUB_TOKEN")
                .ok()
                .filter(|token| !token.is_empty())
                .or_else(|| {
                    std::env::var("GH_TOKEN")
                        .ok()
                        .filter(|token| !token.is_empty())
                })
            {
                request = request.bearer_auth(token);
            }

            let rows = match request.send().await {
                Ok(response) if response.status().is_success() => {
                    response.json::<Vec<GitHubCommitAvatar>>().await.ok()
                }
                _ => None,
            };
            if let Some(rows) = rows {
                let mut identities_by_url: HashMap<String, Vec<(String, String)>> = HashMap::new();
                for row in rows {
                    let Some(author) = row.author else {
                        continue;
                    };
                    if !author.avatar_url.starts_with("https://") {
                        continue;
                    }
                    let avatar_url = if author.avatar_url.contains('?') {
                        format!("{}&s=40", author.avatar_url)
                    } else {
                        format!("{}?s=40", author.avatar_url)
                    };
                    identities_by_url.entry(avatar_url).or_default().push((
                        row.sha.to_ascii_lowercase(),
                        row.commit.author.email.trim().to_ascii_lowercase(),
                    ));
                }
                let downloads = stream::iter(identities_by_url.into_iter().map(
                    |(url, identities)| async move {
                        self.cache_github_avatar(&url)
                            .await
                            .map(|path| (identities, path))
                    },
                ))
                .buffer_unordered(8)
                .filter_map(|download| async move { download })
                .collect::<Vec<_>>()
                .await;
                if let Ok(mut cache) = self.inner.github_avatars.lock() {
                    for (identities, path) in downloads {
                        for (sha, email) in identities {
                            cache.insert(format!("{repo_key}|sha|{sha}"), path.clone());
                            if !email.is_empty() {
                                cache.insert(format!("{repo_key}|email|{email}"), path.clone());
                            }
                        }
                    }
                }
            } else if let Ok(mut pages) = self.inner.github_avatar_pages.lock() {
                // A transient network/auth failure may be retried on refresh.
                pages.remove(&page_key);
            }
        }

        let Ok(cache) = self.inner.github_avatars.lock() else {
            return HashMap::new();
        };
        authors
            .iter()
            .filter_map(|(sha, email)| {
                let avatar = cache
                    .get(&format!("{repo_key}|sha|{}", sha.to_ascii_lowercase()))
                    .or_else(|| {
                        cache.get(&format!(
                            "{repo_key}|email|{}",
                            email.trim().to_ascii_lowercase()
                        ))
                    })?;
                Some((email.trim().to_ascii_lowercase(), avatar.clone()))
            })
            .collect()
    }

    pub(super) async fn cache_github_avatar(&self, url: &str) -> Option<String> {
        const MAX_AVATAR_BYTES: u64 = 2 * 1024 * 1024;

        let digest = Sha256::digest(url.as_bytes());
        let cache_dir = self.inner.data_dir.join("cache").join("git-avatars");
        let path = cache_dir.join(format!("{}.img", hex(&digest[..16])));
        if tokio::fs::metadata(&path)
            .await
            .ok()
            .is_some_and(|metadata| metadata.len() > 0)
        {
            return Some(path.to_string_lossy().into_owned());
        }
        tokio::fs::create_dir_all(&cache_dir).await.ok()?;
        let response = self.inner.http.get(url).send().await.ok()?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|size| size > MAX_AVATAR_BYTES)
        {
            return None;
        }
        let bytes = response.bytes().await.ok()?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_AVATAR_BYTES {
            return None;
        }
        let temporary = cache_dir.join(format!(
            ".{}.{}.tmp",
            hex(&digest[..8]),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::write(&temporary, &bytes).await.ok()?;
        if tokio::fs::rename(&temporary, &path).await.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
            if !path.is_file() {
                return None;
            }
        }
        Some(path.to_string_lossy().into_owned())
    }
}
