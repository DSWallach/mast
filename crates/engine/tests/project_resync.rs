//! Documentation re-sync (Release 1) against a real offline engine: the
//! aligned no-op, review before any write, decline, approve, stale approval,
//! and a retried apply landing exactly once. The model step is a counting
//! fake; nothing here starts a harness or touches a real home directory.

#![allow(clippy::disallowed_methods)] // spawn sites here are tests/build scripts (post-mortem P3)
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use mast_engine::project_resync::ModelStep;
use mast_engine::{EngineCore, HarnessRegistry};
use mast_proto::{ArtifactView, HarnessId, derive_id, roles};
use mast_rpc::{RpcClient, methods};
use serde_json::{Value, json};

const SPACE: &str = "space-1";

fn git(dir: &Path, args: &[&str]) {
    let output = mast_harness::git_command().arg("-C").arg(dir).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

struct Rig {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    core: EngineCore,
    client: RpcClient,
    space_path: PathBuf,
    project: String,
    plan_id: String,
    calls: Arc<AtomicUsize>,
    /// What the fake model answers with next.
    answer: Arc<std::sync::Mutex<String>>,
}

fn no_change() -> String {
    json!({"summary": "", "evidence": [], "questions": [], "newPlan": null}).to_string()
}

fn proposal(new_plan: &str) -> String {
    json!({"summary": "Two Cargo slots, not one", "evidence": ["David answered Q-3 on revision 1"],
           "questions": [], "newPlan": new_plan})
    .to_string()
}

async fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let space_path = root.join("project");
    std::fs::create_dir_all(&space_path).unwrap();
    git(&space_path, &["init", "-q", "-b", "main"]);
    git(&space_path, &["config", "user.email", "t@example.com"]);
    git(&space_path, &["config", "user.name", "t"]);
    std::fs::write(space_path.join("README.md"), "readme\n").unwrap();
    git(&space_path, &["add", "README.md"]);
    git(&space_path, &["commit", "-q", "-m", "root"]);
    let calls = Arc::new(AtomicUsize::new(0));
    let answer = Arc::new(std::sync::Mutex::new(no_change()));
    let (core, client) = connect(&root, &calls, &answer);
    client
        .call(methods::MUTATE, json!({"op": "createSpace", "spaceId": SPACE, "deviceId": core.device_id, "path": space_path.to_string_lossy()}))
        .await
        .unwrap();
    client
        .call(methods::WORK_OUTLINE_MUTATE, json!({"op": "createWorkItem", "spaceId": SPACE, "title": "Project", "operationId": "resync-project"}))
        .await
        .unwrap();
    let project = derive_id("resync-project", roles::ITEM);
    client
        .call(methods::WORK_OUTLINE_MUTATE, json!({"op": "updateWorkItem", "workItemId": project, "changes": {"isProject": true}, "operationId": "resync-project-flag"}))
        .await
        .unwrap();
    std::fs::write(space_path.join("plan.md"), "# Plan\n\nOne Cargo slot.\n").unwrap();
    let registered: Value = client
        .call(methods::ARTIFACT_REGISTER, json!({"spaceId": SPACE, "workItemId": project, "path": "plan.md", "role": "plan"}))
        .await
        .unwrap();
    let plan_id = registered["artifactId"].as_str().unwrap().to_owned();
    client.call(methods::ARTIFACT_PIN, json!({"artifactId": plan_id, "pinned": true})).await.unwrap();
    Rig { _tmp: tmp, root, core, client, space_path, project, plan_id, calls, answer }
}

/// An engine over `root/data` whose re-sync model step is the counting fake.
fn connect(root: &Path, calls: &Arc<AtomicUsize>, answer: &Arc<std::sync::Mutex<String>>) -> (EngineCore, RpcClient) {
    let core = EngineCore::assemble(&root.join("data"), Arc::new(HarnessRegistry::new()), HarnessId::Mock, None)
        .expect("engine core assembles");
    let model: ModelStep = {
        let (calls, answer) = (calls.clone(), answer.clone());
        Arc::new(move |_prompt: String| {
            calls.fetch_add(1, Ordering::SeqCst);
            let reply = answer.lock().unwrap().clone();
            Box::pin(async move { Ok(reply) })
        })
    };
    let service = Arc::try_unwrap(core.rpc_service()).ok().expect("sole owner").with_resync_model(model);
    let client = mast_rpc::memory_client(Arc::new(service));
    (core, client)
}

/// Stop the engine, rewrite the saved job the way an interrupted run leaves
/// it, and start a new engine over the same data.
async fn restart_with_job(rig: Rig, edit: impl FnOnce(&mut Value)) -> Rig {
    let Rig { _tmp, root, core, client, space_path, project, plan_id, calls, answer } = rig;
    let saved: Value = client
        .call(methods::READ_PROJECT_DOCUMENT_INVENTORY, json!({"spaceId": SPACE, "projectId": project}))
        .await
        .unwrap();
    let mut row = saved["inventory"].clone();
    edit(&mut row);
    let row: mast_proto::project_document_inventory::DocumentationInventory = serde_json::from_value(row).unwrap();
    core.workspace.flush();
    core.shutdown().await;
    drop(client);
    drop(core);
    let org = std::env::var("MAST_ORG_ID").unwrap_or_else(|_| mast_engine::DEFAULT_ORG_ID.into());
    let user = std::env::var("MAST_USER_ID").unwrap_or_else(|_| mast_engine::DEFAULT_USER_ID.into());
    let profile = mast_engine::EngineProfile::development(&root.join("data"), &org, &user);
    let storage = mast_sync::DocsStore::open(profile.store_root()).unwrap();
    let bytes = storage.load_snapshot(mast_doc::REGISTRY_DOC_ID).unwrap().unwrap();
    let mut doc = mast_doc::RegistryDoc::from_bytes(&bytes, "dev-a").unwrap();
    doc.put_project_document_inventory(SPACE, &row).unwrap();
    storage.save_snapshot(mast_doc::REGISTRY_DOC_ID, &doc.to_bytes().unwrap()).unwrap();
    drop(storage);
    let (core, client) = connect(&root, &calls, &answer);
    Rig { _tmp, root, core, client, space_path, project, plan_id, calls, answer }
}

impl Rig {
    async fn inventory(&self) {
        self.client
            .call(methods::PROJECT_DOCUMENT_INVENTORY, json!({"spaceId": SPACE, "projectId": self.project, "scan": true}))
            .await
            .unwrap();
    }

    async fn resync(&self, action: &str, proposal: Option<&str>) -> Result<Value, String> {
        self.client
            .call(methods::PROJECT_RESYNC, json!({"spaceId": SPACE, "projectId": self.project, "action": action, "proposalHash": proposal}))
            .await
            .map_err(|e| e.to_string())
    }

    fn plan(&self) -> ArtifactView {
        self.core.workspace.artifact(&self.plan_id).expect("plan artifact")
    }

    fn plan_text(&self) -> String {
        std::fs::read_to_string(self.plan().stored_path).unwrap()
    }

    fn set_answer(&self, text: String) {
        *self.answer.lock().unwrap() = text;
    }

    async fn edit_plan(&self, content: &str) -> Value {
        self.client
            .call(methods::ARTIFACT_WRITE_CONTENT, json!({"artifactId": self.plan_id, "content": content, "baseRevision": self.plan().current_revision}))
            .await
            .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn aligned_project_is_a_no_op_the_second_time() {
    let rig = rig().await;
    rig.inventory().await;
    let first = rig.resync("start", None).await.unwrap();
    assert_eq!(first["job"]["status"], "aligned");
    assert_eq!(first["coverageComplete"], true);
    assert_eq!(rig.calls.load(Ordering::SeqCst), 1);
    let second = rig.resync("start", None).await.unwrap();
    assert_eq!(second["job"]["status"], "aligned");
    assert_eq!(rig.calls.load(Ordering::SeqCst), 1, "unchanged inputs make no model call");
    assert_eq!(rig.plan().current_revision, 1, "and no revision");
    assert_eq!(rig.plan_text(), "# Plan\n\nOne Cargo slot.\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proposal_is_reviewed_before_anything_is_written_and_decline_changes_nothing() {
    let rig = rig().await;
    rig.inventory().await;
    rig.set_answer(proposal("# Plan\n\nTwo Cargo slots.\n"));
    let ready = rig.resync("start", None).await.unwrap();
    assert_eq!(ready["job"]["status"], "readyForReview");
    assert!(ready["diff"].as_array().is_some_and(|lines| lines.iter().any(|l| l["kind"] == "added")));
    assert_eq!(rig.plan().current_revision, 1, "nothing is written before approval");
    assert_eq!(rig.plan_text(), "# Plan\n\nOne Cargo slot.\n");
    let hash = ready["job"]["proposalHash"].as_str().unwrap().to_owned();

    let declined = rig.resync("decline", Some(&hash)).await.unwrap();
    assert_eq!(declined["job"]["status"], "declined");
    assert_eq!(rig.plan().current_revision, 1);
    assert_eq!(rig.plan_text(), "# Plan\n\nOne Cargo slot.\n");
    // The same proposal is not raised again.
    let again = rig.resync("start", None).await.unwrap();
    assert_eq!(again["job"]["status"], "declined");
    assert_eq!(rig.plan().current_revision, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_publishes_one_revision_and_a_rerun_writes_nothing() {
    let rig = rig().await;
    rig.inventory().await;
    rig.set_answer(proposal("# Plan\n\nTwo Cargo slots.\n"));
    let ready = rig.resync("start", None).await.unwrap();
    let hash = ready["job"]["proposalHash"].as_str().unwrap().to_owned();
    let applied = rig.resync("approve", Some(&hash)).await.unwrap();
    assert_eq!(applied["job"]["status"], "applied");
    assert_eq!(applied["job"]["appliedRevision"], 2);
    assert_eq!(rig.plan().current_revision, 2);
    assert_eq!(rig.plan_text(), "# Plan\n\nTwo Cargo slots.\n");
    assert_eq!(rig.plan().id, rig.plan_id, "the same artifact, not a new one");

    // A second approval of the same proposal is refused and writes nothing.
    assert!(rig.resync("approve", Some(&hash)).await.is_err());
    assert_eq!(rig.plan().current_revision, 2);
    // Re-running after approval: aligned, no model call, no write.
    let calls = rig.calls.load(Ordering::SeqCst);
    let rerun = rig.resync("start", None).await.unwrap();
    assert_eq!(rerun["job"]["status"], "aligned");
    assert_eq!(rig.calls.load(Ordering::SeqCst), calls);
    assert_eq!(rig.plan().current_revision, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_after_the_plan_changed_lands_nothing() {
    let rig = rig().await;
    rig.inventory().await;
    rig.set_answer(proposal("# Plan\n\nTwo Cargo slots.\n"));
    let ready = rig.resync("start", None).await.unwrap();
    let hash = ready["job"]["proposalHash"].as_str().unwrap().to_owned();
    // Someone edits the plan after review.
    rig.edit_plan("# Plan\n\nOne Cargo slot, edited.\n").await;
    assert_eq!(rig.plan().current_revision, 2);
    let outcome = rig.resync("approve", Some(&hash)).await.unwrap();
    assert_eq!(outcome["job"]["status"], "failed");
    assert_eq!(rig.plan().current_revision, 2, "the stale proposal wrote no revision");
    assert_eq!(rig.plan_text(), "# Plan\n\nOne Cargo slot, edited.\n", "the concurrent edit survives");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_for_a_different_proposal_is_refused() {
    let rig = rig().await;
    rig.inventory().await;
    rig.set_answer(proposal("# Plan\n\nTwo Cargo slots.\n"));
    rig.resync("start", None).await.unwrap();
    assert!(rig.resync("approve", Some("deadbeef")).await.is_err());
    assert!(rig.resync("approve", Some("../../etc")).await.is_err());
    assert_eq!(rig.plan().current_revision, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn model_output_that_breaks_the_contract_writes_nothing() {
    let rig = rig().await;
    rig.inventory().await;
    for bad in ["not json".to_owned(), json!({"summary": "s", "evidence": [], "newPlan": "# x"}).to_string(), proposal(&"x".repeat(1024 * 1024 + 1))] {
        rig.set_answer(bad);
        let outcome = rig.resync("start", None).await.unwrap();
        assert_eq!(outcome["job"]["status"], "failed");
        assert_eq!(rig.plan().current_revision, 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_double_start_runs_one_model_call() {
    let rig = rig().await;
    rig.inventory().await;
    rig.set_answer(proposal("# Plan\n\nTwo Cargo slots.\n"));
    let (a, b) = tokio::join!(rig.resync("start", None), rig.resync("start", None));
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(rig.calls.load(Ordering::SeqCst), 1);
    let status = rig.resync("status", None).await.unwrap();
    assert_eq!(status["job"]["status"], "readyForReview");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retried_write_with_the_same_operation_lands_once() {
    let rig = rig().await;
    let params = mast_proto::ArtifactWriteContentParams {
        artifact_id: rig.plan_id.clone(),
        content: "# Plan\n\nTwo Cargo slots.\n".into(),
        base_revision: Some(1),
        note: None,
        target_device_id: None,
    };
    let by = mast_proto::WorkOutlineActor::default();
    let first = rig.core.artifacts.write_content_once(&params, by.clone(), "resync-op").unwrap();
    // The reply was lost; the retry names the same operation and the old base.
    let retry = rig.core.artifacts.write_content_once(&params, by.clone(), "resync-op").unwrap();
    assert_eq!(first.revision, 2);
    assert_eq!(retry.revision, 2);
    assert_eq!(rig.plan().current_revision, 2);
    // A different operation on the stale base is refused as stale.
    let stale = rig.core.artifacts.write_content_once(&params, by, "another-op").unwrap_err();
    assert_eq!(stale.code, mast_proto::ArtifactErrorCode::Stale);
    assert_eq!(rig.plan().current_revision, 2);
    let _ = &rig.space_path;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_apply_interrupted_after_the_write_resumes_without_a_second_revision() {
    let rig = rig().await;
    rig.inventory().await;
    rig.set_answer(proposal("# Plan\n\nTwo Cargo slots.\n"));
    let ready = rig.resync("start", None).await.unwrap();
    let hash = ready["job"]["proposalHash"].as_str().unwrap().to_owned();
    // The write landed, then the engine stopped before the receipt was saved.
    let params = mast_proto::ArtifactWriteContentParams {
        artifact_id: rig.plan_id.clone(),
        content: "# Plan\n\nTwo Cargo slots.\n".into(),
        base_revision: Some(1),
        note: None,
        target_device_id: None,
    };
    let by = mast_proto::WorkOutlineActor::default();
    rig.core.artifacts.write_content_once(&params, by, &mast_engine::project_resync::apply_operation_id(&hash)).unwrap();
    let rig = restart_with_job(rig, |row| row["job"]["status"] = json!("applying")).await;
    let calls = rig.calls.load(Ordering::SeqCst);
    for expected in ["applied", "aligned"] {
        let resumed = rig.resync("start", None).await.unwrap();
        assert_eq!(resumed["job"]["status"], expected);
        assert_eq!(rig.plan().current_revision, 2, "exactly one new revision, however often it resumes");
    }
    assert_eq!(rig.calls.load(Ordering::SeqCst), calls, "resuming makes no model call");
    assert_eq!(rig.plan_text(), "# Plan\n\nTwo Cargo slots.\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_left_running_by_a_restart_starts_again() {
    let rig = rig().await;
    rig.inventory().await;
    rig.set_answer(proposal("# Plan\n\nTwo Cargo slots.\n"));
    rig.resync("start", None).await.unwrap();
    assert_eq!(rig.calls.load(Ordering::SeqCst), 1);
    let rig = restart_with_job(rig, |row| row["job"]["status"] = json!("checking")).await;
    let again = rig.resync("start", None).await.unwrap();
    assert_eq!(again["job"]["status"], "readyForReview");
    assert_eq!(rig.calls.load(Ordering::SeqCst), 2, "the interrupted check ran again");
    assert_eq!(rig.plan().current_revision, 1);
}
