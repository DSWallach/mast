//! Turn a [`SmallWorld`] into real git repos and real engine stores inside a
//! scratch root, and hash the result logically.
//!
//! Everything is created through the engine's own APIs (workspace host, work
//! outline mutations) and through real git run via the logging shim. Nothing is
//! written outside the scratch root.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mast_engine::build_jobs::{BuildJobs, BuildJobsConfig, HostProfile, NewJob};
use mast_engine::{EngineCore, EngineProfile, HarnessRegistry};
use mast_proto::{
    ArtifactRegisterParams, ChatConfig, HarnessId, SandboxLevel, ScheduledChat, ScheduledTaskDraft,
    ScheduledTasksRequest, SidebarPinChange, SidebarSectionChange, TaskSchedule,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::environment::ScratchDirectories;
use crate::world::{SmallWorld, WorktreeState};

/// Where a world's repositories live under `repos/`.
pub fn main_repo(scratch: &ScratchDirectories, world: &SmallWorld) -> PathBuf {
    scratch.repositories().join(&world.spaces[0].repository)
}

pub fn worktree_path(scratch: &ScratchDirectories, path: &str) -> PathBuf {
    scratch.repositories().join("worktrees").join(path)
}

fn other(message: impl Into<String>) -> std::io::Error {
    std::io::Error::other(message.into())
}

fn git(
    scratch: &ScratchDirectories,
    cwd: &Path,
    args: &[&str],
    at: i64,
) -> std::io::Result<String> {
    let date = format!("@{} +0000", at / 1000);
    let output = scratch.git(
        cwd,
        args,
        &[("GIT_AUTHOR_DATE", &date), ("GIT_COMMITTER_DATE", &date)],
    )?;
    if !output.status.success() {
        return Err(other(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Create the repo, a few real commits, branches and linked worktrees (some
/// dirty, some deleted).
pub fn materialize_repos(scratch: &ScratchDirectories, world: &SmallWorld) -> std::io::Result<()> {
    let repo = main_repo(scratch, world);
    std::fs::create_dir_all(&repo)?;
    let hour = 3_600_000;
    let at = |step: i64| world.clock_ms - 100 * hour + step * hour;
    git(scratch, &repo, &["init", "-q", "-b", "main"], at(0))?;
    let commits = [
        ("README.md", "# Fixture project\n"),
        ("src/lib.rs", "pub fn answer() -> u32 {\n    42\n}\n"),
        ("src/layout.rs", "pub const WIDTH: u32 = 400;\n"),
        ("docs/notes.md", "Notes for the fixture world.\n"),
    ];
    for (step, (path, body)) in commits.iter().enumerate() {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().ok_or_else(|| other("no parent"))?)?;
        std::fs::write(&file, body)?;
        git(scratch, &repo, &["add", path], at(step as i64 + 1))?;
        let message = format!("Add {path}");
        git(
            scratch,
            &repo,
            &["commit", "-q", "-m", &message],
            at(step as i64 + 1),
        )?;
    }
    for branch in ["feature/search", "fix/stale-chat"] {
        git(scratch, &repo, &["branch", branch], at(10))?;
    }
    std::fs::create_dir_all(scratch.repositories().join("worktrees"))?;
    for (index, worktree) in world.worktrees.iter().enumerate() {
        let path = worktree_path(scratch, &worktree.path);
        let path_text = path.to_str().ok_or_else(|| other("non-UTF-8 path"))?;
        git(
            scratch,
            &repo,
            &["worktree", "add", "-q", "-b", &worktree.branch, path_text],
            at(20),
        )?;
        let step = 30 + index as i64;
        match worktree.state {
            WorktreeState::Clean => {}
            WorktreeState::Dirty => {
                std::fs::write(path.join("README.md"), "# Fixture project\n\nEdited.\n")?;
                std::fs::write(path.join("scratch.txt"), "untracked\n")?;
            }
            WorktreeState::Deleted => {
                // The directory is inside the scratch root this code created.
                std::fs::remove_dir_all(&path)?;
            }
        }
        let _ = step;
    }
    Ok(())
}

/// The `builds` preset's engine data: a real build queue on this device with one queued job for
/// the first chat (it has a log) and one already cancelled for the second. Nothing ticks the
/// queue, so no command ever runs. The engine the app embeds next serves this queue.
pub fn attach_build_queue(
    data: &Path,
    world: &SmallWorld,
    core: &EngineCore,
) -> Result<(), String> {
    let root = data.join("build-queue-fixture");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let config = BuildJobsConfig::new(root.clone(), vec![HostProfile::local(root.clone())]);
    let jobs = BuildJobs::builder(config)
        .open()
        .map_err(|e| e.to_string())?;
    let enqueue = |chat: usize, label: &str| {
        let chat = &world.chats[chat];
        jobs.enqueue(NewJob {
            op_id: format!("{}:{label}", chat.id),
            chat_id: chat.id.clone(),
            space_id: chat.space_id.clone(),
            device_id: core.device_id.clone(),
            worktree_key: chat.worktree.clone(),
            cwd: root.to_string_lossy().into_owned(),
            argv: ["cargo", "test", "-p", label].map(String::from).to_vec(),
            label: Some(format!("cargo test -p {label}")),
            ..Default::default()
        })
        .map(|enqueued| enqueued.job)
        .map_err(|e| e.to_string())
    };
    let waiting = enqueue(0, "fixture-ui")?;
    std::fs::write(
        &waiting.log_path,
        "fixture log: waiting for a build slot\nfixture log: 1 job ahead\n",
    )
    .map_err(|e| e.to_string())?;
    let dropped = enqueue(1, "fixture-engine")?;
    jobs.cancel(&dropped.id, None).map_err(|e| e.to_string())?;
    mast_engine::driver_hooks::attach_build_jobs(&core.device_id, jobs);
    Ok(())
}

pub fn open_core(data: &Path) -> Result<EngineCore, String> {
    let profile = EngineProfile::local(data).map_err(|e| e.to_string())?;
    EngineCore::assemble_with_profile(
        profile,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .map_err(|e| e.to_string())
}

async fn outline(client: &mast_rpc::RpcClient, params: Value) -> Result<Value, String> {
    client
        .call(mast_rpc::methods::WORK_OUTLINE_MUTATE, params)
        .await
        .map_err(|e| e.to_string())
}

/// Create spaces, chats, transcripts and the outline in the engine store under
/// `scratch.data()`. Returns after the store is flushed and the engine shut down.
pub async fn materialize_engine(
    scratch: &ScratchDirectories,
    world: &SmallWorld,
) -> Result<(), String> {
    let core = open_core(scratch.data())?;
    let repo = main_repo(scratch, world);
    let space = &world.spaces[0];
    core.workspace
        .create_space(
            &space.id,
            &core.device_id,
            &repo.to_string_lossy(),
            Some(space.name.clone()),
            true,
        )
        .map_err(|e| e.to_string())?;
    for chat in &world.chats {
        let cwd = worktree_path(scratch, &chat.worktree);
        core.workspace
            .create_chat(
                &chat.id,
                Some(&chat.space_id),
                None,
                None,
                Some(cwd.to_string_lossy().into_owned()),
            )
            .map_err(|e| e.to_string())?;
        core.workspace
            .rename_chat(&chat.id, &chat.title)
            .map_err(|e| e.to_string())?;
        let handle = core.doc_host.open(&chat.id).map_err(|e| e.to_string())?;
        for entry in &chat.entries {
            let entry: mast_doc::SessionMessageEntry =
                serde_json::from_value(entry.clone()).map_err(|e| e.to_string())?;
            handle
                .doc()
                .push_message(&entry)
                .map_err(|e| e.to_string())?;
        }
    }
    let client = mast_rpc::memory_client(core.rpc_service());
    for item in &world.items {
        let mut params = json!({
            "op": "createWorkItem", "spaceId": item.space_id, "title": item.title,
            "status": item.status, "operationId": item.operation_id,
        });
        if let Some(parent) = item.parent {
            params["parentWorkItemId"] = json!(world.items[parent].id);
        }
        outline(&client, params).await?;
    }
    for (index, (chat, item)) in [(0usize, 3usize), (1, 4), (2, 8)].into_iter().enumerate() {
        outline(
            &client,
            json!({"op": "assignChatToWorkItem", "chatId": world.chats[chat].id,
                   "workItemId": world.items[item].id,
                   "operationId": format!("{}-assign-{index}", world.items[0].operation_id)}),
        )
        .await?;
    }
    for (index, request) in world.requests.iter().enumerate() {
        let operation = format!("{}-request-{index}", world.items[0].operation_id);
        let created = outline(
            &client,
            json!({"op": "createUserActionRequest", "workItemId": world.items[request.item].id,
                   "origin": {"kind": "chat", "chatId": world.chats[request.chat].id},
                   "text": request.text, "info": request.detail,
                   "dedupeKey": operation, "operationId": operation}),
        )
        .await?;
        if request.answered {
            outline(
                &client,
                json!({"op": "resolveUserActionRequest",
                       "userActionRequestId": created["userActionRequestId"],
                       "note": "Use a new worktree.", "operationId": format!("{operation}-resolve")}),
            )
            .await?;
        }
    }
    materialize_feature_data(scratch, world, &core, &client).await?;
    core.shutdown().await;
    Ok(())
}

async fn materialize_feature_data(
    scratch: &ScratchDirectories,
    world: &SmallWorld,
    core: &EngineCore,
    client: &mast_rpc::RpcClient,
) -> Result<(), String> {
    let space = &world.spaces[0];
    let repo = main_repo(scratch, world);
    let cwd = repo.to_string_lossy().into_owned();
    for task in &world.scheduled {
        let draft = ScheduledTaskDraft {
            name: task.title.into(),
            prompt: "Write a short report about the fixture project.".into(),
            cwd: cwd.clone(),
            space_id: Some(space.id.clone()),
            config: ChatConfig {
                harness: HarnessId::Mock,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::ReadOnly,
                auto_approve: false,
                approval_rules: false,
            },
            schedule: TaskSchedule::Cron {
                expression: "0 9 * * 1".into(),
                timezone: "UTC".into(),
            },
            enabled: false,
            grace_minutes: 0,
            cutover_confirmed: false,
            work_item_id: None,
            output: Default::default(),
            after_success: Default::default(),
            chat_mode: Default::default(),
            refresh: Default::default(),
            write_to_notion: Some(false),
        };
        let request = ScheduledTasksRequest::Save {
            id: task.id.clone(),
            expected_revision: 0,
            draft,
            operation_id: format!("{}-create", task.id),
        };
        client.call(mast_rpc::methods::SCHEDULED_TASKS, json!(request))
            .await.map_err(|e| e.to_string())?;
        let run_id = format!("{}-run", task.id);
        core.workspace.create_chat(&run_id, Some(&space.id), None, None, Some(cwd.clone()))
            .map_err(|e| e.to_string())?;
        core.workspace.rename_chat(&run_id, task.title).map_err(|e| e.to_string())?;
        core.workspace.set_chat_scheduled_task_info(&run_id, &ScheduledChat {
            task_id: task.id.clone(),
            task_name: task.title.into(),
            run_id: run_id.clone(),
            session_created_at: Some(world.clock_ms),
            ..Default::default()
        }).map_err(|e| e.to_string())?;
        if task.pinned {
            core.workspace.change_sidebar_pin(&SidebarPinChange::Section {
                change: SidebarSectionChange::PinScheduled {
                    task_id: task.id.clone(),
                    pinned: true,
                },
            }).map_err(|e| e.to_string())?;
        }
    }
    for (index, artifact) in world.artifacts.iter().enumerate() {
        let path = repo.join(format!("fixture-artifact-{index}.md"));
        std::fs::write(&path, artifact.body).map_err(|e| e.to_string())?;
        core.artifacts.register(&ArtifactRegisterParams {
            space_id: space.id.clone(),
            work_item_id: Some(world.items[artifact.item].id.clone()),
            path: path.to_string_lossy().into_owned(),
            title: Some(artifact.title.into()),
            ..Default::default()
        }).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Build the whole world: repos first (chats point at worktrees), then stores.
pub async fn materialize(scratch: &ScratchDirectories, world: &SmallWorld) -> Result<(), String> {
    materialize_repos(scratch, world).map_err(|e| e.to_string())?;
    materialize_engine(scratch, world).await
}

fn relative(root: &Path, path: &str) -> String {
    path.strip_prefix(root.to_str().unwrap_or_default())
        .unwrap_or(path)
        .to_owned()
}

/// Hash of the world's logical content: engine rows, transcripts, outline and
/// git refs/status. Paths are taken relative to the scratch root and wall-clock
/// stamps are excluded, so the same seed hashes the same in any scratch root.
pub fn snapshot_hash(
    scratch: &ScratchDirectories,
    world: &SmallWorld,
    core: &EngineCore,
) -> Result<String, String> {
    let lines = snapshot_lines(scratch, world, core)?;
    let mut hasher = Sha256::new();
    for line in &lines {
        hasher.update(line.as_bytes());
        hasher.update(*b"\n");
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The logical lines [`snapshot_hash`] hashes (also useful to diff two worlds).
pub fn snapshot_lines(
    scratch: &ScratchDirectories,
    world: &SmallWorld,
    core: &EngineCore,
) -> Result<Vec<String>, String> {
    let root = scratch.root();
    let mut lines: Vec<String> = Vec::new();
    for space in &world.spaces {
        let row = core
            .workspace
            .space(&space.id)
            .map_err(|e| e.to_string())?
            .ok_or("space missing")?;
        lines.push(format!(
            "space {} {:?} {}",
            space.id,
            row.name,
            relative(root, &row.path)
        ));
    }
    let mut chats = core.workspace.read_chats().map_err(|e| e.to_string())?;
    chats.sort_by(|a, b| a.id.cmp(&b.id));
    for chat in chats {
        lines.push(format!(
            "chat {} {:?} {} archived={}",
            chat.id,
            chat.title,
            relative(root, &chat.cwd.clone().unwrap_or_default()),
            chat.archived
        ));
        let handle = core.doc_host.open(&chat.id).map_err(|e| e.to_string())?;
        for entry in handle.doc().read_entries().map_err(|e| e.to_string())? {
            lines.push(format!(
                "entry {}",
                serde_json::to_string(&entry).map_err(|e| e.to_string())?
            ));
        }
    }
    let service = mast_engine::work_outline::WorkOutlineService::new(core.workspace.clone());
    let snapshot = service.snapshot(&world.spaces[0].id);
    for item in &snapshot.items {
        lines.push(format!(
            "item {} parent={:?} {:?} {:?}/{:?}",
            item.id, item.parent_work_item_id, item.title, item.status, item.effective_status
        ));
    }
    for assignment in &snapshot.assignments {
        // Not the whole view: it carries the device id and a wall-clock stamp.
        lines.push(format!(
            "assignment {} {:?} {:?}",
            assignment.chat_id, assignment.work_item_id, assignment.state
        ));
    }
    let mut requests = snapshot.user_action_requests.clone();
    requests.sort_by(|a, b| a.id.cmp(&b.id));
    for request in requests {
        lines.push(format!(
            "request {} {:?} open={}",
            request.id,
            request.text,
            request.is_open()
        ));
    }
    let repo = main_repo(scratch, world);
    for (cwd, args) in [
        (
            &repo,
            vec!["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        (&repo, vec!["worktree", "list", "--porcelain"]),
    ] {
        let out = git(scratch, cwd, &args, world.clock_ms).map_err(|e| e.to_string())?;
        lines.push(out.replace(root.to_str().unwrap_or_default(), "<root>"));
    }
    for worktree in &world.worktrees {
        let path = worktree_path(scratch, &worktree.path);
        if path.is_dir() {
            lines.push(format!(
                "status {} {}",
                worktree.path,
                git(scratch, &path, &["status", "--porcelain"], world.clock_ms)
                    .map_err(|e| e.to_string())?
            ));
        } else {
            lines.push(format!("status {} deleted", worktree.path));
        }
    }
    Ok(lines)
}

/// Read the world back out of the stores as UI rows. Legacy interactive sessions can select the first chat.
pub fn load_rows(
    world: &SmallWorld,
    core: &EngineCore,
    select_first_chat: bool,
) -> Result<mast_ui::snapshot::WorldRows, String> {
    let mut spaces = Vec::new();
    for space in &world.spaces {
        spaces.push(
            core.workspace
                .space(&space.id)
                .map_err(|e| e.to_string())?
                .ok_or("space missing")?,
        );
    }
    let mut chats = core.workspace.read_chats().map_err(|e| e.to_string())?;
    // Deterministic order: the world's own chat order.
    chats.sort_by_key(|chat| world.chats.iter().position(|c| c.id == chat.id));
    let selected = if select_first_chat {
        world.chats.first().map(|chat| chat.id.clone())
    } else {
        None
    };
    let mut transcript = Vec::new();
    if let Some(id) = &selected {
        let handle = core.doc_host.open(id).map_err(|e| e.to_string())?;
        for entry in handle.doc().read_entries().map_err(|e| e.to_string())? {
            transcript.push(serde_json::to_value(entry).map_err(|e| e.to_string())?);
        }
    }
    let service = mast_engine::work_outline::WorkOutlineService::new(core.workspace.clone());
    Ok(mast_ui::snapshot::WorldRows {
        spaces,
        chats,
        outline: service.snapshot(&world.spaces[0].id),
        selected_chat: selected,
        transcript,
    })
}
