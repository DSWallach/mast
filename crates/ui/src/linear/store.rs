//! Linear state shared by every Linear surface, held in `AppState::linear`:
//! the connection status and a per-identifier issue cache (chips, details,
//! panel rows). Fetches go through `AppState::work_outline_actions()`, so
//! tests answer them with scripted replies. Updates notify `AppState`, which
//! redraws the observers. Callers only fetch for Linear-enabled Spaces.
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{App, Entity};
use mast_proto::linear::{
    LinearAccount, LinearConnectionStatus, LinearIssueSummary, LinearProjectDetail, LinearProjectSummary,
};

use super::client::LinearUiError;
use crate::state::AppState;

/// How long a fetched issue is shown without asking the host again (the
/// host caches for about as long).
pub(crate) const ISSUE_TTL: Duration = Duration::from_secs(5 * 60);
/// How long a failed issue fetch waits before the next automatic try.
pub(crate) const ISSUE_RETRY: Duration = Duration::from_secs(60);

/// What an issue chip shows. `Loading` only stands for "nothing to show
/// yet": a refresh of an entry that already has data keeps showing that data
/// (`LinearStore::in_flight` tracks the fetch), so a pull that returns the
/// same data never changes the rendered text.
#[derive(Debug, Clone)]
pub(crate) enum IssueEntry {
    Loading,
    Ready {
        issue: LinearIssueSummary,
        at: Instant,
    },
    Failed {
        error: LinearUiError,
        at: Instant,
    },
}

#[derive(Debug, Default)]
pub(crate) struct LinearStore {
    /// The last `LinearStatus` / `SetApiKey` / `ClearApiKey` answer.
    pub status: Option<Result<LinearConnectionStatus, LinearUiError>>,
    pub status_loading: bool,
    pub accounts: Option<Vec<LinearAccount>>,
    pub accounts_loading: bool,
    accounts_refresh_pending: bool,
    revisions: HashMap<String, u64>,
    issues: HashMap<String, IssueEntry>,
    /// Identifiers with a fetch under way.
    in_flight: HashSet<String>,
    /// The last "My projects" list (matches pasted project URLs).
    pub projects: HashMap<String, Vec<LinearProjectSummary>>,
    /// Project details by project id (the canonical plan panel).
    project_details: HashMap<String, ProjectDetailEntry>,
}

/// What the plan's Linear panel shows for one project. Data is kept across
/// refreshes and failures (stale-while-revalidate, like the issue cache).
#[derive(Debug, Clone)]
pub(crate) enum ProjectDetailEntry {
    Loading,
    Ready {
        detail: Box<LinearProjectDetail>,
        at: Instant,
    },
    Failed {
        error: LinearUiError,
        at: Instant,
    },
}

pub(crate) fn account_scope(snapshot: &mast_proto::WorkOutlineSnapshot) -> &str {
    snapshot.space_settings.linear_account_id.as_deref().unwrap_or("default")
}

fn scope_for_space(state: &AppState, space_id: &str) -> String {
    state.work_outline(space_id).snapshot().map(|snapshot| account_scope(snapshot)).unwrap_or("default").to_owned()
}

fn cache_key(scope: &str, id: &str) -> String {
    format!("{scope}\0{id}")
}

impl LinearStore {
    pub(crate) fn revision(&self, scope: &str) -> u64 {
        *self.revisions.get(scope).unwrap_or(&0)
    }

    pub(crate) fn invalidate_account(&mut self, scope: &str) {
        *self.revisions.entry(scope.to_owned()).or_default() += 1;
        let prefix = format!("{scope}\0");
        self.issues.retain(|key, _| !key.starts_with(&prefix));
        self.in_flight.retain(|key| !key.starts_with(&prefix));
        self.project_details.retain(|key, _| !key.starts_with(&prefix));
        self.projects.remove(scope);
    }

    /// Order-independent hash of the issue cache (which identifiers have an
    /// entry, its kind and when it was last replaced) and the fetches under
    /// way: what the ticket chips read.
    pub(crate) fn issues_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut total = 0u64;
        for (identifier, entry) in &self.issues {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            identifier.hash(&mut h);
            match entry {
                IssueEntry::Loading => 0u8.hash(&mut h),
                IssueEntry::Ready { at, .. } => (1u8, at).hash(&mut h),
                IssueEntry::Failed { at, .. } => (2u8, at).hash(&mut h),
            }
            total ^= h.finish();
        }
        total ^ (self.in_flight.len() as u64).rotate_left(17)
    }

    pub(crate) fn issue(&self, identifier: &str) -> Option<&IssueEntry> {
        self.issue_in_scope("default", identifier)
    }

    pub(crate) fn issue_in_scope(&self, scope: &str, identifier: &str) -> Option<&IssueEntry> {
        self.issues.get(&cache_key(scope, &identifier.to_uppercase()))
    }

    #[cfg(test)]
    pub(crate) fn ready_issue(&self, identifier: &str) -> Option<&LinearIssueSummary> {
        match self.issue(identifier)? {
            IssueEntry::Ready { issue, .. } => Some(issue),
            _ => None,
        }
    }

    pub(crate) fn project_detail(&self, scope: &str, project_id: &str) -> Option<&ProjectDetailEntry> {
        self.project_details.get(&cache_key(scope, project_id))
    }

    fn project_needs_fetch(&self, scope: &str, project_id: &str, now: Instant) -> bool {
        match self.project_detail(scope, project_id) {
            None => true,
            Some(ProjectDetailEntry::Loading) => false,
            Some(ProjectDetailEntry::Ready { at, .. }) => now.duration_since(*at) >= ISSUE_TTL,
            Some(ProjectDetailEntry::Failed { at, .. }) => now.duration_since(*at) >= ISSUE_RETRY,
        }
    }

    /// Whether a key is configured, when known.
    pub(crate) fn configured(&self) -> Option<bool> {
        match &self.status {
            Some(Ok(status)) => Some(status.configured),
            Some(Err(error)) if error.is(mast_proto::linear::LinearErrorCode::MissingKey) => {
                Some(false)
            }
            _ => None,
        }
    }

    /// Remember issues another call already fetched (panel lists).
    pub(crate) fn remember_issues<'a>(
        &mut self,
        scope: &str,
        issues: impl IntoIterator<Item = &'a LinearIssueSummary>,
    ) {
        let now = Instant::now();
        for issue in issues {
            self.issues.insert(
                cache_key(scope, &issue.identifier.to_uppercase()),
                IssueEntry::Ready {
                    issue: issue.clone(),
                    at: now,
                },
            );
        }
    }

    fn needs_fetch(&self, scope: &str, identifier: &str, now: Instant) -> bool {
        match self.issue_in_scope(scope, identifier) {
            None => true,
            Some(IssueEntry::Loading) => false,
            Some(IssueEntry::Ready { at, .. }) => now.duration_since(*at) >= ISSUE_TTL,
            Some(IssueEntry::Failed { at, .. }) => now.duration_since(*at) >= ISSUE_RETRY,
        }
    }
}

/// Fetch account metadata once, or after an account change.
pub(crate) fn refresh_accounts(state: &Entity<AppState>, cx: &mut App) {
    let task = state.update(cx, |state, _| {
        if state.linear.accounts_loading {
            state.linear.accounts_refresh_pending = true;
            return None;
        }
        state.linear.accounts_loading = true;
        Some(state.work_outline_actions().linear_list_accounts())
    });
    let Some(task) = task else { return; };
    let state = state.clone();
    cx.spawn(async move |cx| {
        let result = task.await;
        let retry = state.update(cx, |state, cx| {
            state.linear.accounts_loading = false;
            state.linear.accounts = Some(result.unwrap_or_default());
            let retry = std::mem::take(&mut state.linear.accounts_refresh_pending);
            cx.notify();
            retry
        });
        if retry { let _ = cx.update(|cx| refresh_accounts(&state, cx)); }
    }).detach();
}

pub(crate) fn ensure_accounts(state: &Entity<AppState>, cx: &mut App) {
    let store = &state.read(cx).linear;
    if store.accounts.is_none() && !store.accounts_loading { refresh_accounts(state, cx); }
}

/// Fetch `LinearStatus` unless an answer or a fetch is already there.
pub(crate) fn ensure_status(state: &Entity<AppState>, cx: &mut App) {
    let store = &state.read(cx).linear;
    if store.status.is_some() || store.status_loading {
        return;
    }
    refresh_status(state, false, cx);
}

/// Re-run `LinearStatus` (`force`: re-verify the key with Linear).
pub(crate) fn refresh_status(state: &Entity<AppState>, force: bool, cx: &mut App) {
    let task = state.update(cx, |state, _| {
        state.linear.status_loading = true;
        state.work_outline_actions().linear_status(force)
    });
    let state = state.clone();
    cx.spawn(async move |cx| {
        let result = task.await;
        state.update(cx, |state, cx| {
            state.linear.status_loading = false;
            state.linear.status = Some(result);
            cx.notify();
        });
    })
    .detach();
}

/// Record a status answer (`SetApiKey` / `ClearApiKey` reply).
pub(crate) fn set_status(
    state: &Entity<AppState>,
    status: Result<LinearConnectionStatus, LinearUiError>,
    cx: &mut App,
) {
    state.update(cx, |state, cx| {
        state.linear.status = Some(status);
        state.linear.status_loading = false;
        cx.notify();
    });
}

/// Fetch issue `identifier` when it is not cached (or `force`). Marks it
/// loading without notifying, so it is safe to call while rendering. An
/// entry that already has data keeps it during the fetch, and keeps it
/// (retrying after `ISSUE_RETRY`) when the fetch fails.
pub(crate) fn request_issue(state: &Entity<AppState>, space_id: &str, identifier: &str, force: bool, cx: &mut App) {
    let key = identifier.trim().to_uppercase();
    if key.is_empty() {
        return;
    }
    let now = Instant::now();
    let scope = scope_for_space(state.read(cx), space_id);
    let cache_key = cache_key(&scope, &key);
    let revision = state.read(cx).linear.revision(&scope);
    let Some(task) = state.update(cx, |state, _| {
        let store = &state.linear;
        let loading = store.in_flight.contains(&cache_key)
            || matches!(store.issue_in_scope(&scope, &key), Some(IssueEntry::Loading));
        if loading || (!force && !store.needs_fetch(&scope, &key, now)) {
            return None;
        }
        let store = &mut state.linear;
        store.in_flight.insert(cache_key.clone());
        if store.issue_in_scope(&scope, &key).is_none() {
            store.issues.insert(cache_key.clone(), IssueEntry::Loading);
        }
        Some(state.work_outline_actions().linear_issue(space_id, &scope, &key, force))
    }) else {
        return;
    };
    let state = state.clone();
    cx.spawn(async move |cx| {
        let result = task.await;
        state.update(cx, |state, cx| {
            let at = Instant::now();
            let store = &mut state.linear;
            if store.revision(&scope) != revision { return; }
            store.in_flight.remove(&cache_key);
            let entry = match result {
                Ok(issue) => IssueEntry::Ready { issue, at },
                Err(error) => match store.issues.remove(&cache_key) {
                    // Keep showing the last good issue; the next try waits
                    // `ISSUE_RETRY`.
                    Some(IssueEntry::Ready { issue, .. }) => IssueEntry::Ready {
                        issue,
                        at: at.checked_sub(ISSUE_TTL - ISSUE_RETRY).unwrap_or(at),
                    },
                    _ => IssueEntry::Failed { error, at },
                },
            };
            store.issues.insert(cache_key, entry);
            cx.notify();
        });
    })
    .detach();
}

/// Show linked project `project_id`'s detail: the host's stored copy at once
/// (never asks Linear), then the cached/fresh read behind it. Marks it
/// loading without notifying, so it is safe to call while rendering. Data
/// already shown survives a failed refresh. Callers only call this for
/// Linear-enabled Spaces; Linear is only ever read.
pub(crate) fn request_project_detail(
    state: &Entity<AppState>,
    space_id: &str,
    project_id: &str,
    force: bool,
    cx: &mut App,
) {
    use super::client::ListRead;
    let key = project_id.trim().to_owned();
    if key.is_empty() {
        return;
    }
    let now = Instant::now();
    let scope = scope_for_space(state.read(cx), space_id);
    let cache_key = cache_key(&scope, &key);
    let revision = state.read(cx).linear.revision(&scope);
    let Some((stored, fresh)) = state.update(cx, |state, _| {
        let store = &state.linear;
        let loading = matches!(
            store.project_detail(&scope, &key),
            Some(ProjectDetailEntry::Loading)
        );
        if loading || (!force && !store.project_needs_fetch(&scope, &key, now)) {
            return None;
        }
        let first = store.project_detail(&scope, &key).is_none();
        let actions = state.work_outline_actions();
        let stored = first.then(|| actions.linear_project_detail(space_id, &scope, &key, ListRead::Stored));
        let read = if force {
            ListRead::Force
        } else {
            ListRead::Cached
        };
        let fresh = actions.linear_project_detail(space_id, &scope, &key, read);
        if first {
            state
                .linear
                .project_details
                .insert(cache_key.clone(), ProjectDetailEntry::Loading);
        }
        Some((stored, fresh))
    }) else {
        return;
    };
    let state = state.clone();
    cx.spawn(async move |cx| {
        if let Some(stored) = stored
            && let Ok(detail) = stored.await
        {
            state.update(cx, |state, cx| {
                // Only fills an entry that has nothing yet; a later fresh
                // answer replaces it.
                if state.linear.revision(&scope) == revision && matches!(
                    state.linear.project_details.get(&cache_key),
                    None | Some(ProjectDetailEntry::Loading)
                ) {
                    state.linear.project_details.insert(
                        cache_key.clone(),
                        ProjectDetailEntry::Ready {
                            detail: Box::new(detail),
                            // Old enough to be refreshed by the next pass.
                            at: Instant::now(),
                        },
                    );
                    cx.notify();
                }
            });
        }
        let result = fresh.await;
        state.update(cx, |state, cx| {
            if state.linear.revision(&scope) != revision { return; }
            let at = Instant::now();
            let entry = match result {
                Ok(detail) => ProjectDetailEntry::Ready {
                    detail: Box::new(detail),
                    at,
                },
                Err(error) => match state.linear.project_details.remove(&cache_key) {
                    Some(ProjectDetailEntry::Ready { detail, .. }) => ProjectDetailEntry::Ready {
                        detail,
                        at: at.checked_sub(ISSUE_TTL - ISSUE_RETRY).unwrap_or(at),
                    },
                    _ => ProjectDetailEntry::Failed { error, at },
                },
            };
            state.linear.project_details.insert(cache_key, entry);
            cx.notify();
        });
    })
    .detach();
}

/// `LinearRefreshLinkedItems` for a Space, then re-fetch `identifiers`
/// (bypassing the cache). `on_done` gets the result.
pub(crate) fn refresh_linked(
    state: &Entity<AppState>,
    space_id: &str,
    identifiers: Vec<String>,
    cx: &mut App,
    on_done: impl FnOnce(Result<u32, LinearUiError>, &mut App) + 'static,
) {
    let task = state
        .read(cx)
        .work_outline_actions()
        .linear_refresh_linked_items(space_id);
    for identifier in &identifiers {
        request_issue(state, space_id, identifier, true, cx);
    }
    cx.spawn(async move |cx| {
        let result = task.await.map(|reply| reply.updated);
        cx.update(|cx| on_done(result, cx));
    })
    .detach();
}

#[cfg(test)]
mod account_tests {
    use super::*;

    #[test]
    fn issues_are_isolated_by_account_and_invalidated_on_key_change() {
        let mut store = LinearStore::default();
        let default_issue = crate::linear::fixture::issue("ENG-1", "Default");
        let personal_issue = crate::linear::fixture::issue("ENG-1", "Personal");
        store.remember_issues("default", [&default_issue]);
        store.remember_issues("personal", [&personal_issue]);
        assert!(matches!(store.issue_in_scope("default", "ENG-1"), Some(IssueEntry::Ready { issue, .. }) if issue.title == "Default"));
        assert!(matches!(store.issue_in_scope("personal", "ENG-1"), Some(IssueEntry::Ready { issue, .. }) if issue.title == "Personal"));
        store.invalidate_account("personal");
        assert!(store.issue_in_scope("personal", "ENG-1").is_none());
        assert!(store.issue_in_scope("default", "ENG-1").is_some());
    }
}
