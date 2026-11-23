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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use mast_engine::peer_link::{PAIRING_PREFIX, PEER_PAIR_METHOD, PeerRegistry};
use mast_engine::peer_outbound::{Links, SavedPeer, save_links};
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::{Harness, HarnessError, RunControls};
use mast_mcp::{Mast, Tools};
use mast_proto::{AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode};
use mast_rpc::methods;
use serde_json::{Value, json};

const SPACE_A: &str = "space-a";
const SPACE_B: &str = "space-b";
const MANAGER: &str = "mgr";

/// Completes every run at once and remembers the prompts it was given.
struct RecordingClaude {
    prompts: Arc<Mutex<Vec<String>>>,
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
    async fn run(&self, request: RunRequest, _controls: RunControls) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.prompts.lock().unwrap().push(request.prompt.clone());
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::ClaudeCode,
                model: "fake".into(),
                tools: vec![],
                cwd: request.cwd,
                session_id: "sess-fake".into(),
                assistant_message_id: "a-1".into(),
                chat_id: None,
                space_id: None,
            }),
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

fn engine(root: &Path, name: &str, prompts: Arc<Mutex<Vec<String>>>) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(RecordingClaude { prompts }));
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
        let a = engine(&root, "a", manager_prompts.clone());
        let worker_prompts = Arc::new(Mutex::new(Vec::new()));
        let b = engine(&root, "b", worker_prompts.clone());

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
        let rig = Rig { _tmp: tmp, a, b, a_data, a_repo, b_repo, registry, manager_prompts, worker_prompts, listener, url, token };
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
async fn a_report_waits_on_the_peer_until_it_is_reachable_again() {
    let rig = Rig::start(false).await;
    let tools = rig.manager_tools();
    let pushed = rig.push_feature(&tools).await;
    let worker = rig.start_worker(&tools, &pushed).await;
    let (worker_tools, _lease) = rig.worker_tools(&worker);
    worker_tools.call("report_to_parent", json!({ "text": "STATUS: done\nRESULT: queued while the link was down" })).await.unwrap();

    // B's listener goes away before A ever connected; A starts collecting and cannot reach it.
    let port: u16 = rig.url.rsplit(':').next().unwrap().parse().unwrap();
    rig.listener.as_ref().unwrap().abort();
    rig.save_link();
    tokio::time::sleep(Duration::from_secs(7)).await;
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
