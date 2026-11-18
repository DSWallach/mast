//! The results contract of Background runs: the text appended to the prompt,
//! and the check that the finished run really published what it promised.
use crate::WorkspaceHost;
use mast_proto::*;

/// The task's prompt followed by the contract. Deterministic in the run, so a
/// launch retried after a crash queues the same message. Runs without a
/// promised artifact (Interactive, or launched before the contract) get the
/// prompt unchanged.
pub(crate) fn prompt_with_contract(run: &ScheduledRun) -> String {
    let prompt = &run.snapshot.prompt;
    let Some(results) = &run.results else {
        return prompt.clone();
    };
    let prefix = ScheduledRunResults::dedupe_prefix(&run.task_id);
    format!(
        "{prompt}\n\n---\n\
Mast results contract (unattended run {run_id}). Nobody is watching this chat, so everything the user needs must land on this work item.\n\
1. Results: publish exactly one artifact with artifact_publish, once, after your work is done. Use title \"{title}\", role \"{role}\", a markdown file, idempotencyKey \"scheduled-run-{run_id}\", and a one-line note. Do not set pinAsPlan; this is not the item's plan. Start the document with the line `Run: {run_id}`. A second publish from this run is rejected, and a run that ends without a new revision under that title and role is marked failed.\n\
2. Needs you: raise each thing the user must act on with work_outline_create_user_action_request. Set dedupeKey to \"{prefix}<topic>\", where <topic> is a short kebab-case name for the subject, the same on every run, so a recurring problem updates one request instead of piling up. Call work_outline_resolve_user_action_request once it no longer applies. Never ask in chat.\n\
3. Status: with work_outline_set_status, set this item active while the work it tracks is open. Set it completed only when your instructions say the work is finished for good; other tasks and plans may share this item.\n",
        run_id = run.id,
        title = results.title,
        role = results.role,
    )
}

/// The task's results artifact on this item. The first publish fixes its
/// title; afterwards the existing artifact (found by role, which does not
/// depend on the task name) keeps its identity and revision history when the
/// task is renamed.
pub(crate) fn resolve(
    workspace: &WorkspaceHost,
    space_id: &str,
    work_item_id: &str,
    task_id: &str,
    task_name: &str,
) -> ScheduledRunResults {
    let role = ScheduledRunResults::role_for(task_id);
    workspace
        .artifacts_of_space(space_id)
        .into_iter()
        .filter(|a| {
            a.work_item_id.as_deref() == Some(work_item_id)
                && a.role
                    .as_deref()
                    .is_some_and(|r| ScheduledRunResults::normalize_role(r) == role)
        })
        .min_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)))
        .map(|a| ScheduledRunResults {
            artifact_id: a.id,
            title: a.title,
            role: role.clone(),
            revision: None,
        })
        .unwrap_or_else(|| ScheduledRunResults::new(space_id, work_item_id, task_id, task_name))
}

/// The revision this run's chat published, or why there is none. Only
/// revisions made by the run's own chat count: a user, another task or an
/// earlier run publishing under the same title never satisfies it.
pub(super) fn published_revision(
    workspace: &WorkspaceHost,
    run: &ScheduledRun,
    results: &ScheduledRunResults,
) -> Result<u32, String> {
    let chat = run.chat_id.as_deref().unwrap_or_default();
    let Some(artifact) = workspace.artifact(&results.artifact_id) else {
        return Err(format!(
            "The run finished without publishing its results artifact “{}”.",
            results.title
        ));
    };
    artifact
        .revisions
        .iter()
        .filter(|revision| {
            revision.by.kind == WorkOutlineActorKind::Agent
                && revision.by.chat_id.as_deref() == Some(chat)
        })
        .map(|revision| revision.id)
        .max()
        .ok_or_else(|| {
            format!(
                "The run finished without a new revision of its results artifact “{}”; \
                 the latest revision ({}) was not published by this run. \
                 An unchanged document records no revision, so each run's document must include its run id.",
                results.title, artifact.current_revision
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(results: Option<ScheduledRunResults>) -> ScheduledRun {
        ScheduledRun {
            id: "run-1".into(),
            task_id: "task-1".into(),
            scheduled_for: 0,
            created_at: 0,
            finished_at: None,
            manual: false,
            state: ScheduledRunState::Launching,
            detail: None,
            chat_id: Some("chat-1".into()),
            task_revision: 1,
            snapshot: ScheduledTaskDraft {
                name: "Slack".into(),
                prompt: "Summarize Slack.".into(),
                cwd: "/".into(),
                space_id: None,
                config: ChatConfig {
                    approval_rules: false,
                    harness: HarnessId::Mock,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    sandbox: SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                },
                schedule: TaskSchedule::Cron {
                    expression: "0 8 * * 1-5".into(),
                    timezone: "America/New_York".into(),
                },
                enabled: false,
                grace_minutes: 0,
                cutover_confirmed: false,
                work_item_id: None,
                output: ScheduledTaskOutput::Background,
                after_success: Default::default(),
            },
            awaiting_request_id: None,
            awaiting_since: None,
            results,
        }
    }

    #[test]
    fn the_contract_names_the_artifact_and_stable_keys_and_is_deterministic() {
        let results = ScheduledRunResults::new("space", "item", "task-1", "Slack");
        let text = prompt_with_contract(&run(Some(results.clone())));
        assert!(text.starts_with("Summarize Slack.\n\n---\n"));
        for needle in [
            "title \"Results: Slack\"",
            &format!("role \"{}\"", results.role),
            "Run: run-1",
            "scheduled-run-run-1",
            &format!(
                "\"{}<topic>\"",
                ScheduledRunResults::dedupe_prefix("task-1")
            ),
            "work_outline_set_status",
        ] {
            assert!(text.contains(needle), "{needle}");
        }
        assert_eq!(text, prompt_with_contract(&run(Some(results))));
    }

    #[test]
    fn runs_without_a_promise_keep_their_prompt() {
        assert_eq!(prompt_with_contract(&run(None)), "Summarize Slack.");
    }
}
