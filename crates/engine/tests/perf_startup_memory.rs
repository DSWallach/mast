//! Headless startup RSS measurement on a synthetic local profile.
use mast_doc::{MessagePart, MessageRole, MessageStatus, SessionDoc, SessionMessageEntry};
use mast_engine::{EngineCore, EngineProfile, HarnessId, default_registry};
use mast_sync::DocsStore;
use std::{path::Path, process::Command, sync::Arc};

fn assemble(path: &Path) -> EngineCore {
    EngineCore::assemble_with_profile(
        EngineProfile::local(path).unwrap(),
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "headless startup memory benchmark"]
async fn synthetic_startup_resident_memory() {
    if let Some(path) = std::env::var_os("MAST_SYNTHETIC_PROFILE") {
        let core = assemble(Path::new(&path));
        let chats = core.workspace.read_chats().unwrap();
        assert_eq!(chats.len(), 360);
        assert_eq!(chats.iter().filter(|chat| chat.archived).count(), 110);
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let rss = Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        assert!(rss.status.success());
        eprintln!(
            "SYNTHETIC_STARTUP_RSS_KIB={}",
            String::from_utf8_lossy(&rss.stdout).trim()
        );
        core.shutdown().await;
        return;
    }
    let dir = tempfile::Builder::new()
        .prefix("mast-memory-")
        .tempdir_in("/private/tmp")
        .unwrap();
    let core = assemble(dir.path());
    let store = DocsStore::open(EngineProfile::local(dir.path()).unwrap().store_root()).unwrap();
    let mut total = 0;
    for i in 0..360 {
        let id = format!("chat-{i}");
        core.workspace
            .create_chat(&id, None, Some(&core.device_id), None, None)
            .unwrap();
        if i < 110 {
            core.workspace.set_chat_archived(&id, true).unwrap();
        }
        let doc = SessionDoc::init(&id).unwrap();
        doc.push_message(&SessionMessageEntry {
            id: "message".into(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "text".into(),
                text: format!("chat {i}: ") + &"synthetic transcript content ".repeat(800),
            }],
            created_at: 1,
            device_id: "synthetic".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
        .unwrap();
        let snapshot = doc.export_snapshot().unwrap();
        total += snapshot.len();
        store.save_snapshot(&id, &snapshot).unwrap();
    }
    eprintln!("SYNTHETIC_PROFILE chats=360 archived=110 snapshot_bytes={total}");
    drop(store);
    core.shutdown().await;
    drop(core);
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "synthetic_startup_resident_memory",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("MAST_SYNTHETIC_PROFILE", dir.path())
        .output()
        .unwrap();
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
