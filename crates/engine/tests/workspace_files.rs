//! End-to-end coverage for the workspace file RPC surface over the real
//! in-memory transport.

#![allow(clippy::disallowed_methods)] // spawn sites here are tests/build scripts (post-mortem P3)
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mast_engine::{EngineCore, HarnessRegistry};
use mast_proto::{
    WorkspaceDirectoryPage, WorkspaceFileChanges, WorkspaceFileText, WriteWorkspaceFileOutcome,
};
use mast_rpc::methods;

async fn git(cwd: &Path, args: &[&str]) {
    let output = mast_harness::git_command_async()
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .await
        .expect("git spawns");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_history_follows_renames_and_opens_the_historical_diff() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo).await;
    git(&repo, &["mv", "src/lib.rs", "src/renamed.rs"]).await;
    git(&repo, &["commit", "-m", "rename file"]).await;
    let core = assemble(&temp.path().join("data"), "device-file-history");
    core.workspace.create_space("space-file-history", &core.device_id, &repo.to_string_lossy(), None, true).unwrap();
    core.workspace.create_chat("chat-file-history", Some("space-file-history"), None, None, None).unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    let params = serde_json::json!({"chatId":"chat-file-history", "path":"src/renamed.rs"});
    let history = client.call(methods::LIST_WORKSPACE_FILE_HISTORY, params.clone()).await.unwrap();
    assert_eq!(history["commits"].as_array().unwrap().len(), 2);
    assert_eq!(history["commits"][0]["path"], "src/renamed.rs");
    assert_eq!(history["commits"][0]["previousPath"], "src/lib.rs");
    assert_eq!(history["commits"][1]["path"], "src/lib.rs");
    let cached = client.call(methods::LIST_WORKSPACE_FILE_HISTORY, params).await.unwrap();
    assert_eq!(history, cached);
    let old = &history["commits"][1];
    let diff = client.call(methods::GET_WORKSPACE_FILE_HISTORY_DIFF, serde_json::json!({"chatId":"chat-file-history", "path":old["path"], "commit":old["commit"]["sha"]})).await.unwrap();
    assert!(diff.as_str().unwrap().contains("+pub fn answer"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_history_handles_merge_commits() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo).await;
    git(&repo, &["checkout", "-b", "topic"]).await;
    std::fs::write(repo.join("src/lib.rs"), "pub fn topic() {}\n").unwrap();
    git(&repo, &["add", "src/lib.rs"]).await;
    git(&repo, &["commit", "-m", "topic edit"]).await;
    git(&repo, &["checkout", "main"]).await;
    std::fs::write(repo.join("other.txt"), "main\n").unwrap();
    git(&repo, &["add", "other.txt"]).await;
    git(&repo, &["commit", "-m", "main edit"]).await;
    git(&repo, &["merge", "--no-ff", "topic", "-m", "merge topic"]).await;
    let core = assemble(&temp.path().join("data"), "device-merge-history");
    core.workspace.create_space("space-merge", &core.device_id, &repo.to_string_lossy(), None, true).unwrap();
    core.workspace.create_chat("chat-merge", Some("space-merge"), None, None, None).unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    let history = client.call(methods::LIST_WORKSPACE_FILE_HISTORY, serde_json::json!({"chatId":"chat-merge", "path":"src/lib.rs"})).await.unwrap();
    let commits = history["commits"].as_array().unwrap();
    assert!(commits.iter().any(|entry| entry["commit"]["subject"] == "topic edit"));
    assert!(commits.iter().any(|entry| entry["commit"]["subject"] == "initial"));
    assert!(commits.iter().all(|entry| entry["path"] == "src/lib.rs"));
}

async fn init_repo(path: &Path) {
    std::fs::create_dir_all(path.join("src/nested")).expect("repo tree");
    git(path, &["init", "-b", "main"]).await;
    std::fs::write(path.join("README.md"), "hello\n").expect("readme");
    std::fs::write(path.join("src/lib.rs"), "pub fn answer() -> u8 { 42 }\n").expect("source");
    std::fs::write(path.join("src/nested/mod.rs"), "pub mod child;\n").expect("nested source");
    git(path, &["add", "."]).await;
    git(path, &["commit", "-m", "initial"]).await;
}

fn assemble(data_dir: &Path, device_id: &str) -> EngineCore {
    std::fs::create_dir_all(data_dir).expect("data dir");
    std::fs::write(data_dir.join("device-id"), device_id).expect("device id");
    EngineCore::assemble(
        data_dir,
        Arc::new(HarnessRegistry::new()),
        mast_proto::HarnessId::Mock,
        None,
    )
    .expect("engine assembles")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symbol_index_tracks_workspace_changes() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo).await;
    std::fs::write(repo.join("many.rs"), (0..250).map(|i| format!("fn symbol_{i}() {{}}\n")).collect::<String>()).unwrap();
    let core = assemble(&temp.path().join("data"), "device-symbols");
    core.workspace.create_space("space-symbols", &core.device_id, &repo.to_string_lossy(), None, true).unwrap();
    core.workspace.create_chat("chat-symbols", Some("space-symbols"), None, None, None).unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    let mut stream = client.subscribe(methods::WATCH_WORKSPACE_SYMBOLS, serde_json::json!({"chatId":"chat-symbols"})).await.unwrap();
    let initial = tokio::time::timeout(Duration::from_secs(30), stream.recv()).await.unwrap().unwrap();
    assert!(initial.get("files").is_none(), "status frames must never carry the whole index");
    assert!(serde_json::to_vec(&initial).unwrap().len() < 256);
    let params = |query: &str| serde_json::json!({"chatId":"chat-symbols", "query":query, "limit":200});
    let initial_matches = client.call(methods::QUERY_WORKSPACE_SYMBOLS, params("answer")).await.unwrap();
    assert_eq!(initial_matches[0]["symbol"]["name"], "answer");
    let bounded = client.call(methods::QUERY_WORKSPACE_SYMBOLS, serde_json::json!({"chatId":"chat-symbols", "query":"", "limit":10000})).await.unwrap();
    assert_eq!(bounded.as_array().unwrap().len(), 200);
    client.call(methods::REFRESH_WORKSPACE_SYMBOLS, serde_json::json!({"chatId":"chat-symbols", "force":false})).await.unwrap();
    let checked = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = stream.recv().await.unwrap();
            if frame["updating"] == false { break frame; }
        }
    }).await.unwrap();
    assert_eq!(checked["revision"], initial["revision"], "unchanged open must not parse files");
    std::fs::write(repo.join("src/lib.rs"), "pub fn revised() {}\n").unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = stream.recv().await.unwrap();
            assert!(frame.get("files").is_none());
            if frame["updating"] == false {
                let matches = client.call(methods::QUERY_WORKSPACE_SYMBOLS, params("revised")).await.unwrap();
                if !matches.as_array().unwrap().is_empty() { break; }
            }
        }
    }).await.unwrap();
    std::fs::write(repo.join("created.rs"), "fn created() {}\n").unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = stream.recv().await.unwrap();
            if frame["updating"] == false {
                let matches = client.call(methods::QUERY_WORKSPACE_SYMBOLS, params("created")).await.unwrap();
                if !matches.as_array().unwrap().is_empty() { break; }
            }
        }
    }).await.unwrap();
    std::fs::remove_file(repo.join("created.rs")).unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = stream.recv().await.unwrap();
            if frame["updating"] == false {
                let matches = client.call(methods::QUERY_WORKSPACE_SYMBOLS, params("created")).await.unwrap();
                if matches.as_array().unwrap().is_empty() { break; }
            }
        }
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symbol_index_evicts_the_least_recently_used_checkout() {
    let temp = tempfile::tempdir().unwrap();
    let core = assemble(&temp.path().join("data"), "device-symbol-lru");
    let mut receivers = Vec::new();
    for number in 0..3 {
        let repo = temp.path().join(format!("repo-{number}"));
        init_repo(&repo).await;
        let space = format!("space-{number}");
        let chat = format!("chat-{number}");
        core.workspace.create_space(&space, &core.device_id, &repo.to_string_lossy(), None, true).unwrap();
        core.workspace.create_chat(&chat, Some(&space), None, None, None).unwrap();
        let target: mast_proto::WorkspaceTarget = serde_json::from_value(serde_json::json!({"chatId": chat})).unwrap();
        let mut receiver = core.workspace_files.subscribe_symbols(target).await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), async { while receiver.borrow().is_none() { receiver.changed().await.unwrap(); } }).await.unwrap();
        receivers.push(receiver);
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        while receivers[0].changed().await.is_ok() {}
    }).await.expect("oldest checkout should be evicted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_index_tracks_file_changes() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo).await;
    let core = assemble(&temp.path().join("data"), "device-index");
    core.workspace
        .create_space(
            "space-index",
            &core.device_id,
            &repo.to_string_lossy(),
            None,
            true,
        )
        .unwrap();
    core.workspace
        .create_chat("chat-index", Some("space-index"), None, None, None)
        .unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    let mut stream = client
        .subscribe(
            methods::WATCH_WORKSPACE_SEARCH_FILES,
            serde_json::json!({"chatId":"chat-index"}),
        )
        .await
        .unwrap();
    let initial = tokio::time::timeout(Duration::from_secs(30), stream.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        initial["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "README.md")
    );
    let mut content = client
        .subscribe(
            methods::SEARCH_WORKSPACE_CONTENT,
            serde_json::json!({"chatId": "chat-index", "query": "hello"}),
        )
        .await
        .unwrap();
    let mut saw_progress = false;
    let mut saw_match = false;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = content.recv().await.unwrap();
            match frame["type"].as_str() {
                Some("progress") => saw_progress = true,
                Some("match") => saw_match |= frame["path"] == "README.md",
                Some("complete") => break,
                Some("error") => panic!("content search failed: {frame}"),
                _ => panic!("unexpected content frame: {frame}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(saw_progress && saw_match);
    std::fs::write(repo.join("created.rs"), "created").unwrap();
    let created = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = stream.recv().await.unwrap();
            if frame["files"]
                .as_array()
                .is_some_and(|paths| paths.iter().any(|path| path == "created.rs"))
            {
                break frame;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        created["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "created.rs")
    );
    std::fs::rename(repo.join("created.rs"), repo.join("renamed.rs")).unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = stream.recv().await.unwrap();
            let paths = frame["files"].as_array().unwrap();
            if paths.iter().any(|path| path == "renamed.rs")
                && !paths.iter().any(|path| path == "created.rs")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    std::fs::remove_file(repo.join("renamed.rs")).unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = stream.recv().await.unwrap();
            if !frame["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|path| path == "renamed.rs")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_index_refreshes_on_subscription_when_watcher_exceeds_budget() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo).await;
    for index in 0..=mast_engine::workspace_files::MAX_WATCH_DIRS {
        std::fs::create_dir(repo.join(format!("directory-{index}"))).unwrap();
    }
    let core = assemble(&temp.path().join("data"), "device-index-repair");
    core.workspace
        .create_space(
            "space-index",
            &core.device_id,
            &repo.to_string_lossy(),
            None,
            true,
        )
        .unwrap();
    core.workspace
        .create_chat("chat-index", Some("space-index"), None, None, None)
        .unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    let mut stream = client
        .subscribe(
            methods::WATCH_WORKSPACE_SEARCH_FILES,
            serde_json::json!({"chatId":"chat-index"}),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), stream.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    std::fs::write(repo.join("created.rs"), "created").unwrap();
    // Reopening the picker requests a refresh even without filesystem events.
    let mut reopened = client
        .subscribe(
            methods::WATCH_WORKSPACE_SEARCH_FILES,
            serde_json::json!({"chatId":"chat-index"}),
        )
        .await
        .unwrap();
    let cached = reopened.recv().await.unwrap();
    assert!(
        !cached["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "created.rs")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), reopened.recv())
            .await
            .is_err(),
        "an open during the cooldown must not immediately re-walk"
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let frame = reopened.recv().await.unwrap();
            if frame["files"]
                .as_array()
                .is_some_and(|paths| paths.iter().any(|path| path == "created.rs"))
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    std::fs::write(repo.join("not-requested.rs"), "later").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(6), reopened.recv())
            .await
            .is_err(),
        "a refresh request must not start a recurring re-walk timer"
    );
    // The existing picker shares the refreshed index too.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let frame = stream.recv().await.unwrap();
            if frame["files"]
                .as_array()
                .is_some_and(|paths| paths.iter().any(|path| path == "created.rs"))
            {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_file_rpcs_list_search_read_write_and_watch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    init_repo(&repo).await;
    let core = assemble(&temp.path().join("data"), "device-files");
    core.workspace
        .create_space(
            "space-files",
            &core.device_id,
            &repo.to_string_lossy(),
            None,
            true,
        )
        .expect("space");
    core.workspace
        .create_chat("chat-files", Some("space-files"), None, None, None)
        .expect("chat");
    let client = mast_rpc::memory_client(core.rpc_service());

    let root = client
        .call(
            methods::LIST_WORKSPACE_DIRECTORY,
            serde_json::json!({ "chatId": "chat-files" }),
        )
        .await
        .expect("list root");
    let root: WorkspaceDirectoryPage = serde_json::from_value(root).expect("typed root");
    assert_eq!(root.entries[0].path, "src");
    assert!(root.entries.iter().any(|entry| entry.path == "README.md"));
    assert!(!root.entries.iter().any(|entry| entry.path == ".git"));

    let nested = client
        .call(
            methods::LIST_WORKSPACE_DIRECTORY,
            serde_json::json!({ "chatId": "chat-files", "directory": "src" }),
        )
        .await
        .expect("list src");
    let nested: WorkspaceDirectoryPage = serde_json::from_value(nested).expect("typed src");
    assert!(
        nested
            .entries
            .iter()
            .any(|entry| entry.path == "src/lib.rs")
    );

    let matches = client
        .call(
            methods::SEARCH_WORKSPACE_FILES,
            serde_json::json!({ "chatId": "chat-files", "query": "lib" }),
        )
        .await
        .expect("search");
    assert_eq!(matches[0]["path"], "src/lib.rs");

    let read = client
        .call(
            methods::READ_WORKSPACE_FILE,
            serde_json::json!({ "chatId": "chat-files", "path": "README.md" }),
        )
        .await
        .expect("read");
    let read: WorkspaceFileText = serde_json::from_value(read).expect("typed read");
    assert_eq!(read.text.as_deref(), Some("hello\n"));
    let original_hash = read.content_hash.expect("content hash");
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><rect width="20" height="20"/></svg>"#;
    std::fs::write(repo.join("example.svg"), svg).unwrap();
    let image = client.call(methods::READ_WORKSPACE_IMAGE, serde_json::json!({ "chatId": "chat-files", "path": "example.svg", "expectedCheckoutId": read.checkout_id, "offset": 0 })).await.expect("workspace image");
    assert_eq!(image["mimeType"], "image/svg+xml");
    assert_eq!(image["done"], true);
    assert_eq!(image["size"], svg.len());
    for (path, checkout) in [
        ("example.svg", "wrong-checkout"),
        ("../example.svg", read.checkout_id.as_str()),
        (".git/config", read.checkout_id.as_str()),
    ] {
        assert!(client.call(methods::READ_WORKSPACE_IMAGE, serde_json::json!({ "chatId": "chat-files", "path": path, "expectedCheckoutId": checkout, "offset": 0 })).await.is_err());
    }

    let mut watch = client
        .subscribe(
            methods::WATCH_WORKSPACE_FILES,
            serde_json::json!({ "chatId": "chat-files" }),
        )
        .await
        .expect("watch subscribe");
    let baseline = tokio::time::timeout(Duration::from_secs(3), watch.recv())
        .await
        .expect("baseline timeout")
        .expect("watch alive");
    let baseline: WorkspaceFileChanges = serde_json::from_value(baseline).expect("baseline shape");
    assert!(baseline.resync_required);

    let written = client
        .call(
            methods::WRITE_WORKSPACE_FILE,
            serde_json::json!({
                "chatId": "chat-files",
                "path": "README.md",
                "text": "updated\n",
                "expectedCheckoutId": read.checkout_id,
                "expectedContentHash": original_hash,
                "encoding": "utf8",
                "lineEnding": "lf",
            }),
        )
        .await
        .expect("write");
    let written: WriteWorkspaceFileOutcome =
        serde_json::from_value(written).expect("written shape");
    let new_hash = match written {
        WriteWorkspaceFileOutcome::Written { file } => file.content_hash,
        outcome => panic!("unexpected write outcome: {outcome:?}"),
    };
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "updated\n"
    );

    let conflict = client
        .call(
            methods::WRITE_WORKSPACE_FILE,
            serde_json::json!({
                "chatId": "chat-files",
                "path": "README.md",
                "text": "stale overwrite\n",
                "expectedCheckoutId": read.checkout_id,
                "expectedContentHash": "stale",
                "encoding": "utf8",
                "lineEnding": "lf",
            }),
        )
        .await
        .expect("conflict response");
    assert_eq!(conflict["status"], "conflict");
    assert_eq!(conflict["currentContentHash"], new_hash);
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "updated\n"
    );

    let concurrent_read = client
        .call(
            methods::READ_WORKSPACE_FILE,
            serde_json::json!({ "chatId": "chat-files", "path": "README.md" }),
        )
        .await
        .expect("read before concurrent writers");
    let concurrent_hash = concurrent_read["contentHash"]
        .as_str()
        .expect("concurrent hash")
        .to_string();
    let first_write = client.call(
        methods::WRITE_WORKSPACE_FILE,
        serde_json::json!({
            "chatId": "chat-files",
            "path": "README.md",
            "text": "concurrent\n",
            "expectedCheckoutId": read.checkout_id,
                "expectedContentHash": concurrent_hash.clone(),
            "encoding": "utf8",
            "lineEnding": "lf",
        }),
    );
    let second_write = client.call(
        methods::WRITE_WORKSPACE_FILE,
        serde_json::json!({
            "chatId": "chat-files",
            "path": "README.md",
            "text": "concurrent\n",
            "expectedCheckoutId": read.checkout_id,
                "expectedContentHash": concurrent_hash,
            "encoding": "utf8",
            "lineEnding": "lf",
        }),
    );
    let (first_write, second_write) = tokio::join!(first_write, second_write);
    let statuses = [
        first_write.expect("first concurrent writer")["status"]
            .as_str()
            .expect("first status")
            .to_string(),
        second_write.expect("second concurrent writer")["status"]
            .as_str()
            .expect("second status")
            .to_string(),
    ];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| status.as_str() == "written")
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| status.as_str() == "conflict")
            .count(),
        1
    );

    std::fs::write(repo.join("created.txt"), "created\n").expect("external create");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let item = tokio::time::timeout_at(deadline, watch.recv())
            .await
            .expect("watch change timeout")
            .expect("watch alive");
        let item: WorkspaceFileChanges = serde_json::from_value(item).expect("change shape");
        if item
            .changes
            .iter()
            .any(|change| change.path == "created.txt")
        {
            break;
        }
    }

    assert!(
        client
            .call(
                methods::READ_WORKSPACE_FILE,
                serde_json::json!({ "chatId": "chat-files", "path": "../outside" }),
            )
            .await
            .is_err()
    );
    drop(watch);
    core.shutdown().await;
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_file_rpcs_preserve_plain_folder_search_support() {
    let temp = tempfile::tempdir().expect("tempdir");
    let folder = temp.path().join("plain-folder");
    std::fs::create_dir_all(&folder).expect("plain folder");
    std::fs::write(folder.join("notes.txt"), "plain\n").expect("plain file");
    let core = assemble(&temp.path().join("plain-data"), "device-plain");
    core.workspace
        .create_space(
            "space-plain",
            &core.device_id,
            &folder.to_string_lossy(),
            None,
            false,
        )
        .expect("plain space");
    let client = mast_rpc::memory_client(core.rpc_service());

    let legacy = client
        .call(
            methods::SEARCH_FILES,
            serde_json::json!({ "spaceId": "space-plain", "query": "notes" }),
        )
        .await
        .expect("legacy SearchFiles on plain folder");
    assert_eq!(legacy[0]["path"], "notes.txt");
    let listing = client
        .call(
            methods::LIST_WORKSPACE_DIRECTORY,
            serde_json::json!({ "spaceId": "space-plain" }),
        )
        .await
        .expect("workspace listing on plain folder");
    assert!(
        listing["entries"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| entry["path"] == "notes.txt"))
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_rejects_changed_checkout_even_when_contents_match() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo).await;
    git(
        &repo,
        &["worktree", "add", "-b", "other", worktree.to_str().unwrap()],
    )
    .await;
    let core = assemble(&temp.path().join("data"), "device-files");
    core.workspace
        .create_space(
            "space-files",
            &core.device_id,
            repo.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    core.workspace
        .create_chat("chat-files", Some("space-files"), None, None, None)
        .unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    let read = client
        .call(
            methods::READ_WORKSPACE_FILE,
            serde_json::json!({
                "chatId": "chat-files", "path": "README.md"
            }),
        )
        .await
        .unwrap();
    assert!(!read["checkoutId"].as_str().unwrap().is_empty());
    core.workspace
        .set_chat_cwd("chat-files", worktree.to_str().unwrap())
        .unwrap();
    let other = client
        .call(
            methods::READ_WORKSPACE_FILE,
            serde_json::json!({
                "chatId": "chat-files", "path": "README.md"
            }),
        )
        .await
        .unwrap();
    assert_eq!(read["contentHash"], other["contentHash"]);
    assert_ne!(read["checkoutId"], other["checkoutId"]);
    let mut request = serde_json::json!({
        "chatId": "chat-files", "path": "README.md", "text": "pending original edit\n",
        "expectedContentHash": read["contentHash"], "expectedCheckoutId": read["checkoutId"],
        "encoding": "utf8", "lineEnding": "lf"
    });
    let error = client
        .call(methods::WRITE_WORKSPACE_FILE, request.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Workspace changed"), "{error}");
    request
        .as_object_mut()
        .unwrap()
        .remove("expectedCheckoutId");
    assert!(
        client
            .call(methods::WRITE_WORKSPACE_FILE, request.clone())
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "hello\n"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "hello\n"
    );
    // Returning to the original checkout allows the preserved edit to be saved.
    core.workspace
        .set_chat_cwd("chat-files", repo.to_str().unwrap())
        .unwrap();
    request["expectedCheckoutId"] = read["checkoutId"].clone();
    let written = client
        .call(methods::WRITE_WORKSPACE_FILE, request)
        .await
        .unwrap();
    assert_eq!(written["status"], "written");
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "pending original edit\n"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "hello\n"
    );
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
