//! A Manager on one engine (A) runs a worker on a paired engine (B) over the
//! peer link, with no SSH: it sends a branch, starts the worker in the worktree
//! B makes for it, receives the worker's `report_to_parent` as "Worker updates"
//! in its own chat, and fetches the worker's commits back. Two real engines
//! with disposable homes, a real listener on a loopback port, real git.
//!
//! The Manager's side goes through the MCP tools (`peer_push_branch`,
//! `create_chat`, `peer_fetch_branch`); the worker's through the same MCP
//! server an agent gets on B, authenticated by an engine-minted token.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use mast_engine::peer_link::{PAIRING_PREFIX, PEER_PAIR_METHOD, PeerPostureCeiling, PeerRegistry};
use mast_engine::peer_outbound::{Links, SavedPeer, save_links};
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::{Harness, HarnessError, RunControls};
use mast_mcp::{Mast, Tools};
use mast_proto::{AgentEvent, ApprovalScope, CommandApproval, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode, UserInputQuestion};
use mast_rpc::methods;
use serde_json::{Value, json};

const SPACE_A: &str = "space-a";
const SPACE_B: &str = "space-b";
const MANAGER: &str = "mgr";

/// Completes every run at once and remembers the prompts it was given.
struct RecordingClaude {
    prompts: Arc<Mutex<Vec<String>>>,
    approval_answers: Arc<Mutex<Vec<Vec<String>>>>,
    next_msg: AtomicUsize,
}

#[async_trait]
impl Harness for RecordingClaude {
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCode
    }
    fn display_name(&self) -> &str {
        "Fake Claude"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(&self, request: RunRequest, controls: RunControls) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.prompts.lock().unwrap().push(request.prompt.clone());
        let msg_id = format!("a-{}", self.next_msg.fetch_add(1, Ordering::Relaxed) + 1);
        let start = Ok(AgentEvent::SessionStarted {
            harness: HarnessId::ClaudeCode,
            model: "fake".into(),
            tools: vec![],
            cwd: request.cwd.clone(),
            session_id: "sess-fake".into(),
            assistant_message_id: msg_id,
            chat_id: None,
            space_id: None,
        });
        if request.prompt.contains("need approval") {
            let cwd = request.cwd;
            let approval_answers = self.approval_answers.clone();
            return Ok(futures::stream::iter(vec![start])
                .chain(futures::stream::once(async move {
                    let question = UserInputQuestion {
                        id: "q-cmd".into(),
                        header: "Approve command".into(),
                        question: "Run `cargo test`?".into(),
                        options: vec!["Yes".into(), "No".into()],
                        multi_select: false,
                        allow_custom: false,
                        approval_scope: Some(ApprovalScope::Visible),
                        command_approval: Some(CommandApproval {
                            command: json!("cargo test"),
                            cwd: Some(cwd),
                            reason: None,
                            sandbox_permissions: None,
                        }),
                        permission: None,
                    };
                    let answers = (controls.request_input)(vec![question]).await.unwrap_or_default();
                    let labels: Vec<String> = answers.into_iter().flat_map(|a| a.labels).collect();
                    approval_answers.lock().unwrap().push(labels);
                    Ok(AgentEvent::Done { status: DoneStatus::Completed, result: None, error: None, session_id: Some("sess-fake".into()) })
                }))
                .boxed());
        }
        Ok(futures::stream::iter(vec![
            start,
            Ok(AgentEvent::Done { status: DoneStatus::Completed, result: None, error: None, session_id: Some("sess-fake".into()) }),
        ])
        .boxed())
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = mast_harness::git_command()
        .args(["-c", "commit.gpgsign=false", "-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?} in {}: {}", dir.display(), String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

async fn wait_until<F: FnMut() -> bool>(seconds: u64, what: &str, mut ready: F) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    while !ready() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn engine(
    root: &Path,
    name: &str,
    prompts: Arc<Mutex<Vec<String>>>,
    approval_answers: Arc<Mutex<Vec<Vec<String>>>>,
) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(RecordingClaude { prompts, approval_answers, next_msg: AtomicUsize::new(0) }));
    EngineCore::assemble(&root.join(name).join("data"), Arc::new(registry), HarnessId::ClaudeCode, None).expect("engine core assembles")
}

async fn space_and_chat(core: &EngineCore, space: &str, path: &Path, chat: Option<&str>) {
    let admin = mast_rpc::memory_client(core.rpc_service());
    admin
        .call(methods::MUTATE, json!({ "op": "createSpace", "spaceId": space, "deviceId": core.device_id, "path": path.to_string_lossy() }))
        .await
        .unwrap();
    if let Some(chat) = chat {
        admin
            .call(
                methods::MUTATE,
                json!({ "op": "createChat", "chatId": chat, "spaceId": space, "deviceId": core.device_id,
                        "config": { "harness": "claude-code", "model": null, "reasoning": null, "sandbox": "workspace-write" } }),
            )
            .await
            .unwrap();
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    a: EngineCore,
    b: EngineCore,
    a_data: PathBuf,
    a_repo: PathBuf,
    b_repo: PathBuf,
    registry: Arc<PeerRegistry>,
    manager_prompts: Arc<Mutex<Vec<String>>>,
    worker_prompts: Arc<Mutex<Vec<String>>>,
    worker_approval_answers: Arc<Mutex<Vec<Vec<String>>>>,
    listener: Option<tokio::task::JoinHandle<()>>,
    url: String,
    token: String,
}

impl Rig {
    /// Two engines, a repository on each (B's a clone of A's, so a thin bundle applies), B listening
    /// and paired. `save_link` writes B into A's saved peers, which is what makes A collect reports.
    async fn start(save_link: bool) -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let manager_prompts = Arc::new(Mutex::new(Vec::new()));
        let a = engine(&root, "a", manager_prompts.clone(), Arc::new(Mutex::new(Vec::new())));
        let worker_prompts = Arc::new(Mutex::new(Vec::new()));
        let worker_approval_answers = Arc::new(Mutex::new(Vec::new()));
        let b = engine(&root, "b", worker_prompts.clone(), worker_approval_answers.clone());

        let a_repo = root.join("a").join("project");
        std::fs::create_dir_all(&a_repo).unwrap();
        git(&a_repo, &["init", "-q"]);
        std::fs::write(a_repo.join("base.txt"), "base\n").unwrap();
        git(&a_repo, &["add", "."]);
        git(&a_repo, &["commit", "-q", "-m", "base"]);
        let b_repo = root.join("b").join("project");
        git(&root, &["clone", "-q", a_repo.to_str().unwrap(), b_repo.to_str().unwrap()]);
        space_and_chat(&a, SPACE_A, &a_repo, Some(MANAGER)).await;
        space_and_chat(&b, SPACE_B, &b_repo, Some("client-work")).await;

        let registry = PeerRegistry::open(&root.join("b").join("data")).unwrap();
        b.orchestrator_wakes.set_remote_reports(registry.clone());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let listener = Some(tokio::spawn(mast_rpc::serve_ws_listener_authed(listener, registry.authenticator(b.rpc_service()))));

        let code = registry.open_pairing().unwrap();
        let pairing = mast_rpc::connect_ws_bearer(&url, Some(&format!("{PAIRING_PREFIX}{code}"))).await.unwrap();
        let token = pairing.call(PEER_PAIR_METHOD, json!({ "label": "mac" })).await.unwrap()["token"].as_str().unwrap().to_string();

        let a_data = root.join("a").join("data");
        let rig = Rig { _tmp: tmp, a, b, a_data, a_repo, b_repo, registry, manager_prompts, worker_prompts, worker_approval_answers, listener, url, token };
        if save_link {
            rig.save_link();
        }
        rig
    }

    fn save_link(&self) {
        save_links(&self.a_data, &Links { peers: vec![SavedPeer { name: "fedora".into(), url: self.url.clone(), token: self.token.clone() }] }).unwrap();
    }

    fn manager_tools(&self) -> Tools {
        Tools::new(Arc::new(Mast::new_peer(self.url.clone(), self.token.clone())))
    }

    /// The MCP server an agent in `chat` on B gets. The lease revokes the token when dropped.
    fn worker_tools(&self, chat: &str) -> (Tools, impl Sized) {
        let lease = self.b.sessions.agent_tokens().mint(chat, "run-1", SPACE_B, &self.b.device_id);
        let tools = Tools::new(Arc::new(Mast::with_client(mast_rpc::memory_client(self.b.rpc_service()), Some(lease.token().to_owned()))));
        (tools, lease)
    }

    /// Branch `feature/x` on A with one commit past `main`, sent to B.
    async fn push_feature(&self, tools: &Tools) -> Value {
        git(&self.a_repo, &["checkout", "-q", "-b", "feature/x"]);
        std::fs::write(self.a_repo.join("feature.txt"), "feature\n").unwrap();
        git(&self.a_repo, &["add", "."]);
        git(&self.a_repo, &["commit", "-q", "-m", "feature"]);
        tools
            .call("peer_push_branch", json!({ "repo": self.a_repo, "branch": "feature/x", "project": SPACE_B, "base": "main" }))
            .await
            .unwrap()
    }

    async fn start_worker(&self, tools: &Tools, pushed: &Value) -> String {
        let created = tools
            .call(
                "create_chat",
                json!({ "project": SPACE_B, "cwd": pushed["worktreePath"], "branch": pushed["peerBranch"], "parent": MANAGER,
                        "title": "Fedora build", "harness": "claude-code", "prompt": "build it on fedora" }),
            )
            .await
            .unwrap();
        assert_eq!(created["parentChatId"], MANAGER);
        wait_until(15, "the worker's first turn to start on the peer", || self.worker_prompts.lock().unwrap().iter().any(|p| p.contains("build it on fedora"))).await;
        created["chatId"].as_str().unwrap().to_string()
    }

    fn manager_updates(&self) -> Vec<String> {
        self.manager_prompts.lock().unwrap().iter().filter(|p| p.contains("Worker updates:")).cloned().collect()
    }

    /// Deliver what is held for the Manager now (instead of waiting out the batch window) until it shows up.
    async fn wait_for_manager_update(&self, needle: &str) -> String {
        let mut found = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
        while found.is_none() {
            assert!(tokio::time::Instant::now() < deadline, "the Manager never received {needle:?}; saw {:?}", self.manager_updates());
            self.a.orchestrator_wakes.deliver_now(MANAGER).await;
            found = self.manager_updates().into_iter().find(|p| p.contains(needle));
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        found.unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_sends_a_branch_runs_a_worker_hears_its_report_and_fetches_its_commits() {
    let rig = Rig::start(true).await;
    let tools = rig.manager_tools();
    let names: Vec<String> = tools.list().into_iter().map(|t| t.name.to_string()).collect();
    assert!(names.contains(&"peer_push_branch".to_string()) && names.contains(&"peer_fetch_branch".to_string()));

    let pushed = rig.push_feature(&tools).await;
    let worktree = PathBuf::from(pushed["worktreePath"].as_str().unwrap());
    assert!(worktree.join("feature.txt").exists(), "the worker's folder holds the branch that was sent");
    assert_eq!(pushed["peerBranch"].as_str().unwrap().rsplit_once('/').unwrap().1, "x");
    assert_eq!(git(&rig.b_repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "main", "the Space's own checkout is untouched");
    let sent_head = git(&rig.a_repo, &["rev-parse", "feature/x"]);
    assert_eq!(pushed["head"], sent_head);

    let refused = tools.call("create_chat", json!({
        "project": SPACE_B, "cwd": pushed["worktreePath"], "branch": pushed["peerBranch"],
        "harness": "claude-code", "autoApprove": true,
    })).await.unwrap_err();
    assert!(refused.contains("posture ceiling"), "{refused}");

    let worker = rig.start_worker(&tools, &pushed).await;
    let visible: Vec<String> = tools.call("list_chats", json!({})).await.unwrap()["chats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(visible, [worker.clone()], "the unrelated client chat on B is invisible to the peer");
    assert!(tools.call("send_message", json!({ "chat": "client-work", "text": "hi" })).await.is_err());

    // The worker commits on B and reports through its own MCP server.
    std::fs::write(worktree.join("result.txt"), "built on fedora\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "worker result"]);
    let worker_head = git(&worktree, &["rev-parse", "HEAD"]);
    let (worker_tools, _lease) = rig.worker_tools(&worker);
    let sent = worker_tools.call("report_to_parent", json!({ "text": "STATUS: done\nRESULT: engine suite green on fedora" })).await.unwrap();
    assert_eq!(sent["sent"]["delivery"], "queuedForRemoteParent");
    assert_eq!(sent["parentChatId"], MANAGER);

    let update = rig.wait_for_manager_update("engine suite green on fedora").await;
    assert!(update.contains(&worker), "the Manager is told which chat on the peer to read: {update}");
    assert!(update.contains("fedora"), "{update}");
    // Acknowledged on B, so a later poll does not deliver it again.
    let (_, waiting) = {
        wait_until(15, "the report to be acknowledged on the peer", || rig.registry.peer_counts(&rig.registry.list()[0].id).1 == 0).await;
        rig.registry.peer_counts(&rig.registry.list()[0].id)
    };
    assert_eq!(waiting, 0);
    tokio::time::sleep(Duration::from_secs(4)).await;
    rig.a.orchestrator_wakes.deliver_now(MANAGER).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let count = rig.manager_updates().iter().filter(|p| p.contains("engine suite green on fedora")).count();
    assert_eq!(count, 1, "delivered exactly once: {:?}", rig.manager_updates());

    // The result comes home as a local branch; nothing is merged or pushed.
    let fetched = tools
        .call("peer_fetch_branch", json!({ "repo": rig.a_repo, "branch": "feature/x", "project": SPACE_B, "base": sent_head }))
        .await
        .unwrap();
    assert_eq!(fetched["newCommits"], 1);
    assert_eq!(fetched["head"], worker_head);
    assert_eq!(git(&rig.a_repo, &["rev-parse", "peer/feature/x"]), worker_head);
    assert_eq!(git(&rig.a_repo, &["show", "peer/feature/x:result.txt"]), "built on fedora");
    assert_eq!(git(&rig.a_repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "feature/x", "A's checked-out branch did not move");
    let again = tools.call("peer_fetch_branch", json!({ "repo": rig.a_repo, "branch": "feature/x", "project": SPACE_B, "base": worker_head })).await;
    assert!(again.unwrap_err().contains("no new commits"), "nothing newer to fetch");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_created_coordinator_shows_its_remote_parent_and_hears_its_own_children() {
    let rig = Rig::start(true).await;
    let tools = rig.manager_tools();
    let pushed = rig.push_feature(&tools).await;
    let coordinator = rig.start_worker(&tools, &pushed).await;
    let (coordinator_tools, _lease) = rig.worker_tools(&coordinator);

    // On B the coordinator has no parentChatId by design; whoami says where its reports go.
    let me = coordinator_tools.call("whoami", json!({})).await.unwrap();
    assert!(me["chat"]["parentChatId"].is_null(), "{me}");
    assert_eq!(me["chat"]["remoteParent"]["chatId"], MANAGER, "{me}");
    assert!(me["chat"]["remoteParentNote"].as_str().unwrap().contains("report_to_parent works"), "{me}");
    let read = coordinator_tools.call("get_chat", json!({ "chat": coordinator })).await.unwrap();
    assert_eq!(read["remoteParent"]["chatId"], MANAGER, "{read}");

    // Up: its report reaches the laptop parent through the peer link.
    let up = coordinator_tools.call("report_to_parent", json!({ "text": "STATUS: progress\nRESULT: coordinator reporting up" })).await.unwrap();
    assert_eq!(up["sent"]["delivery"], "queuedForRemoteParent");
    assert_eq!(up["parentChatId"], MANAGER);
    rig.wait_for_manager_update("coordinator reporting up").await;

    // Down: a chat the coordinator starts on B reports to the coordinator, locally.
    let child = coordinator_tools.call("create_chat", json!({
        "project": SPACE_B, "harness": "claude-code", "prompt": "child work", "idempotencyKey": "coordinator-child",
    })).await.unwrap();
    assert_eq!(child["parentChatId"], coordinator, "{child}");
    let child_id = child["chatId"].as_str().unwrap().to_owned();
    let (child_tools, _child_lease) = rig.worker_tools(&child_id);
    let down = child_tools.call("report_to_parent", json!({ "text": "STATUS: done\nRESULT: child reporting down" })).await.unwrap();
    assert_eq!(down["parentChatId"], coordinator, "{down}");
    assert_ne!(down["sent"]["delivery"], "queuedForRemoteParent", "{down}");
    // The child itself has a local parent, so it shows none of the remote kind.
    let child_me = child_tools.call("whoami", json!({})).await.unwrap();
    assert!(child_me["chat"]["remoteParent"].is_null(), "{child_me}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_report_waits_on_the_peer_until_it_is_reachable_again() {
    let mut rig = Rig::start(false).await;
    let tools = rig.manager_tools();
    let pushed = rig.push_feature(&tools).await;
    let worker = rig.start_worker(&tools, &pushed).await;
    {
        let (worker_tools, _lease) = rig.worker_tools(&worker);
        worker_tools.call("report_to_parent", json!({ "text": "STATUS: done\nRESULT: queued while the link was down" })).await.unwrap();
    }

    // B's listener goes away before A ever connected; A starts collecting and cannot reach it.
    let port: u16 = rig.url.rsplit(':').next().unwrap().parse().unwrap();
    let tunnel = rig.listener.take().unwrap();
    tunnel.abort();
    let _ = tunnel.await;
    rig.a.shutdown().await;
    rig.save_link();
    drop(rig.a);
    rig.a = engine(rig._tmp.path(), "a", rig.manager_prompts.clone(), Arc::new(Mutex::new(Vec::new())));
    wait_until(15, "the restarted collector to observe the closed tunnel", || {
        mast_engine::peer_outbound::load_status(&rig.a_data).get("fedora").is_some_and(|s| s.state == "unreachable")
    }).await;
    assert!(rig.manager_updates().is_empty());
    assert_eq!(rig.registry.peer_counts(&rig.registry.list()[0].id).1, 1, "B keeps the report");

    // B comes back (a fresh listener, as after a restart of its engine): the report is delivered.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    tokio::spawn(mast_rpc::serve_ws_listener_authed(listener, rig.registry.authenticator(rig.b.rpc_service())));
    rig.wait_for_manager_update("queued while the link was down").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_the_peer_cuts_off_the_manager_and_the_worker_has_nobody_to_report_to() {
    let rig = Rig::start(true).await;
    let tools = rig.manager_tools();
    let pushed = rig.push_feature(&tools).await;
    let worker = rig.start_worker(&tools, &pushed).await;
    assert!(tools.call("list_chats", json!({})).await.is_ok());

    assert_eq!(rig.registry.revoke("mac").unwrap(), 1);
    // The open socket is closed within a few seconds; a new one is refused.
    let mut refused = false;
    for _ in 0..100 {
        if tools.call("list_chats", json!({})).await.is_err() {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(refused, "the revoked peer was cut off");
    assert!(tools.call("peer_fetch_branch", json!({ "repo": rig.a_repo, "branch": "feature/x", "project": SPACE_B })).await.is_err());

    let (worker_tools, _lease) = rig.worker_tools(&worker);
    let error = worker_tools.call("report_to_parent", json!({ "text": "STATUS: done" })).await.unwrap_err();
    assert!(error.contains("no parent chat"), "{error}");
    assert!(rig.manager_updates().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ask_ceiling_peer_cannot_approve_a_workers_command_via_respond_to_input() {
    let rig = Rig::start(true).await;
    let tools = rig.manager_tools();
    let pushed = rig.push_feature(&tools).await;

    let created = tools
        .call(
            "create_chat",
            json!({ "project": SPACE_B, "cwd": pushed["worktreePath"], "branch": pushed["peerBranch"], "parent": MANAGER,
                    "title": "Approval gate", "harness": "claude-code", "prompt": "need approval on fedora" }),
        )
        .await
        .unwrap();
    let worker = created["chatId"].as_str().unwrap().to_string();

    async fn wait_pending_request(tools: &Tools, worker: &str, previous: Option<&str>) -> String {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let info = tools.call("get_chat", json!({ "chat": worker })).await.unwrap();
            if let Some(req_id) = info.pointer("/pendingInput/requestId").and_then(Value::as_str).filter(|id| !id.is_empty())
                && previous != Some(req_id)
            {
                // get_chat reports a peer's live request; respond_to_input answers from the transcript,
                // which records the same request a moment later. Hand it back only once both agree.
                let transcript = tools.call("read_chat", json!({ "chat": worker })).await.unwrap();
                if transcript.pointer("/pendingInput/requestId").and_then(Value::as_str) == Some(req_id) {
                    return req_id.to_string();
                }
            }
            assert!(tokio::time::Instant::now() < deadline, "timed out waiting for pendingInput (after {previous:?}): {info}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_idle(tools: &Tools, worker: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let info = tools.call("get_chat", json!({ "chat": worker })).await.unwrap();
            if info["status"] == "idle" && info["pendingInput"].is_null() {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "timed out waiting for idle: {info}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    // Wait until the worker's first command approval prompt is live.
    let first_request_id = wait_pending_request(&tools, &worker, None).await;

    // Default Ask ceiling refuses Yes (harness receives nothing), allows No.
    let refused = tools
        .call("respond_to_input", json!({ "chat": worker, "request_id": first_request_id, "answers": [{ "labels": ["Yes"] }] }))
        .await
        .unwrap_err();
    assert!(refused.contains("posture ceiling"), "{refused}");
    assert!(rig.worker_approval_answers.lock().unwrap().is_empty(), "refused Yes must never reach the harness");

    tools
        .call("respond_to_input", json!({ "chat": worker, "request_id": first_request_id, "answers": [{ "labels": ["No"] }] }))
        .await
        .unwrap();
    wait_until(15, "harness to receive first approval answer", || rig.worker_approval_answers.lock().unwrap().len() == 1).await;
    assert_eq!(rig.worker_approval_answers.lock().unwrap()[0], vec!["No"]);
    wait_idle(&tools, &worker).await;

    // Raise ceiling to Rules, trigger a second distinct prompt, and verify Yes reaches the harness.
    assert_eq!(rig.registry.set_posture("mac", PeerPostureCeiling::Rules).unwrap(), 1);
    tools.call("send_message", json!({ "chat": worker, "text": "need approval again" })).await.unwrap();
    let second_request_id = wait_pending_request(&tools, &worker, Some(&first_request_id)).await;
    assert_ne!(second_request_id, first_request_id);
    tools
        .call("respond_to_input", json!({ "chat": worker, "request_id": second_request_id, "answers": [{ "labels": ["Yes"] }] }))
        .await
        .unwrap();
    wait_until(15, "harness to receive second approval answer", || rig.worker_approval_answers.lock().unwrap().len() == 2).await;
    assert_eq!(rig.worker_approval_answers.lock().unwrap()[1], vec!["Yes"]);
    wait_idle(&tools, &worker).await;
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;

impl Rig {
    /// A project on A with a Manager (a standing-role chat), and the MCP server that
    /// Manager speaks through. Returns the Manager's chat id.
    async fn manager_world(&self) -> (String, Tools, impl Sized) {
        let admin = mast_rpc::memory_client(self.a.rpc_service());
        admin
            .call(mast_rpc::methods::WORK_OUTLINE_MUTATE, json!({ "op": "createWorkItem", "spaceId": SPACE_A, "title": "Project", "operationId": "op-project" }))
            .await
            .unwrap();
        let project = mast_proto::derive_id("op-project", mast_proto::roles::ITEM);
        admin
            .call(mast_rpc::methods::WORK_OUTLINE_MUTATE, json!({ "op": "updateWorkItem", "workItemId": project, "changes": { "isProject": true }, "operationId": "op-mark" }))
            .await
            .unwrap();
        let launch = mast_proto::CreateChatForWorkItemParams {
            work_item_id: project.clone(),
            initial_prompt: mast_proto::WorkItemPrompt { text: "Manage".into(), attachments: Vec::new() },
            config: Some(mast_proto::ChatConfig {
                approval_rules: false,
                harness: HarnessId::ClaudeCode,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: mast_proto::SandboxLevel::WorkspaceWrite,
                auto_approve: true,
            }),
            execution_override: None,
            worktree_choice: None,
            operation_id: "op-manager".into(),
            parent_chat_id: None,
            target_device_id: None,
            account_id: None,
            standing_role: Some(mast_proto::StandingRoleKind::Manager),
        };
        admin.call(methods::CREATE_CHAT_FOR_WORK_ITEM, serde_json::to_value(&launch).unwrap()).await.unwrap();
        let manager = mast_proto::derive_id("op-manager", mast_proto::roles::CHAT);
        wait_until(15, "the Manager role", || self.a.workspace.standing_role_chat(mast_proto::StandingRoleKind::Manager, &project).as_deref() == Some(manager.as_str())).await;
        let lease = self.a.sessions.agent_tokens().mint(&manager, "run-1", SPACE_A, &self.a.device_id);
        let tools = Tools::new(Arc::new(Mast::with_client(mast_rpc::memory_client(self.a.rpc_service()), Some(lease.token().to_owned()))));
        (manager, tools, lease)
    }

    /// A work item in the project (under it), with the paths its Lead's lease would cover.
    async fn lead_item(&self, title: &str, paths: &[&str]) -> String {
        let admin = mast_rpc::memory_client(self.a.rpc_service());
        let project = mast_proto::derive_id("op-project", mast_proto::roles::ITEM);
        let op = format!("item-{title}");
        admin
            .call(mast_rpc::methods::WORK_OUTLINE_MUTATE, json!({ "op": "createWorkItem", "spaceId": SPACE_A, "parentWorkItemId": project, "title": title, "operationId": op }))
            .await
            .unwrap();
        let id = mast_proto::derive_id(&op, mast_proto::roles::ITEM);
        admin
            .call(mast_rpc::methods::WORK_OUTLINE_MUTATE, json!({ "op": "updateWorkItem", "workItemId": id, "changes": { "taskSpec": { "ownedPaths": paths } }, "operationId": format!("spec-{title}") }))
            .await
            .unwrap();
        id
    }

    /// The Manager asks its own server for a Lead of `item` on B.
    async fn start_lead(&self, manager_tools: &Tools, item: &str) -> Result<String, String> {
        let created = manager_tools
            .call(
                "create_chat",
                json!({ "idempotencyKey": format!("lead-{item}"), "standingRole": "lead", "workItemId": item, "peer": "fedora",
                        "peerProject": SPACE_B, "harness": "claude-code" }),
            )
            .await?;
        Ok(created["chatId"].as_str().unwrap().to_string())
    }

    async fn wait_for_update_to(&self, manager: &str, needle: &str) -> String {
        let mut found = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
        while found.is_none() {
            assert!(tokio::time::Instant::now() < deadline, "the Manager never received {needle:?}; saw {:?}", self.manager_updates());
            self.a.orchestrator_wakes.deliver_now(manager).await;
            found = self.manager_updates().into_iter().find(|p| p.contains(needle));
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        found.unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_hosted_lead_is_read_only_reports_a_digest_and_keeps_to_its_subtree() {
    let rig = Rig::start(true).await;
    let (manager, manager_tools, _manager_lease) = rig.manager_world().await;
    let item = rig.lead_item("Slice", &["crates/a/**"]).await;
    let lead = rig.start_lead(&manager_tools, &item).await.unwrap();
    // Read-only on B whatever the Manager asked for.
    let row = rig.b.workspace.chat(&lead).unwrap().unwrap();
    assert_eq!(row.config.unwrap().sandbox, mast_proto::SandboxLevel::ReadOnly);

    // Its Workers live on B and answer to it; it reaches them and nothing else.
    let (lead_tools, _lease) = rig.worker_tools(&lead);
    let mut workers = Vec::new();
    for n in 0..mast_proto::MAX_WORKERS_PER_LEAD {
        let made = lead_tools.call("create_chat", json!({ "idempotencyKey": format!("w{n}"), "title": format!("leaf {n}") })).await.unwrap();
        assert_eq!(made["parentChatId"], lead.as_str());
        workers.push(made["chatId"].as_str().unwrap().to_string());
    }
    let seventh = lead_tools.call("create_chat", json!({ "idempotencyKey": "w-extra", "title": "one too many" })).await.unwrap_err();
    assert!(seventh.contains("already has 6 Workers"), "{seventh}");
    lead_tools.call("read_chat", json!({ "chat": workers[0] })).await.unwrap();
    assert!(lead_tools.call("read_chat", json!({ "chat": "client-work" })).await.is_err(), "a sibling chat on B is outside the Lead's subtree");
    assert!(lead_tools.call("send_message", json!({ "chat": "client-work", "text": "hi" })).await.is_err());

    // A Worker of a Worker would be depth 3.
    let (leaf_tools, _leaf_lease) = rig.worker_tools(&workers[0]);
    let deeper = leaf_tools.call("create_chat", json!({ "idempotencyKey": "deep", "title": "too deep" })).await.unwrap_err();
    assert!(deeper.contains("depth 3 is over the limit of 2"), "{deeper}");

    // Its report crosses the link as a digest; the leaf's words never do.
    let long = format!("Slice merged on fedora.\n{}", "internal detail ".repeat(3_000));
    let sent = lead_tools.call("report_to_parent", json!({ "text": long })).await.unwrap();
    assert_eq!(sent["sent"]["delivery"], "queuedForRemoteParent");
    assert_eq!(sent["sent"]["truncated"], true);
    let update = rig.wait_for_update_to(&manager, "[Lead digest, item ").await;
    assert!(update.contains("Slice merged on fedora."), "{update}");
    assert!(update.len() < 3_000, "{} bytes", update.len());
    leaf_tools.call("report_to_parent", json!({ "text": "LEAF-NOISE tests green" })).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    rig.a.orchestrator_wakes.deliver_now(&manager).await;
    assert!(rig.manager_updates().iter().all(|u| !u.contains("LEAF-NOISE")), "the Manager hears only the digest: {:?}", rig.manager_updates());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_hosted_lead_marks_a_completed_report_done() {
    let rig = Rig::start(true).await;
    let (manager, manager_tools, _manager_lease) = rig.manager_world().await;
    let item = rig.lead_item("Done slice", &[]).await;
    let lead = rig.start_lead(&manager_tools, &item).await.unwrap();
    let (lead_tools, _lease) = rig.worker_tools(&lead);
    lead_tools
        .call("report_to_parent", json!({ "text": "Slice finished.", "completion": { "testedSha": "abcdef1", "evidence": "suite green" } }))
        .await
        .unwrap();
    let update = rig.wait_for_update_to(&manager, "[Lead digest, item ").await;
    assert!(update.contains("status: Done"), "{update}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paired_devices_server_cannot_start_a_lead() {
    let rig = Rig::start(true).await;
    let tools = rig.manager_tools();
    // The paired server's token reaches B only; B starts a Lead for the Manager's engine alone.
    let refused = tools
        .call("create_chat", json!({ "project": SPACE_B, "parent": MANAGER, "standingRole": "lead", "workItemId": "item-x", "peer": "fedora", "harness": "claude-code", "prompt": "go" }))
        .await
        .unwrap_err();
    assert!(refused.contains("cannot start a Lead"), "{refused}");
    let refused = tools
        .call("create_chat", json!({ "project": SPACE_B, "parent": MANAGER, "standingRole": "lead", "harness": "claude-code" }))
        .await
        .unwrap_err();
    assert!(refused.contains("workItemId"), "{refused}");
    assert!(rig.b.workspace.read_chats().unwrap().iter().all(|chat| rig.b.workspace.peer_lead_item(&chat.id).is_none()));
}
