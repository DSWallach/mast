use super::*;
use super::git_history::*;

#[tokio::test]
#[allow(clippy::disallowed_methods)]
async fn checkout_identity_from_disk_matches_git_and_spawns_nothing() {
    let git = |cwd: &Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main");
    std::fs::create_dir_all(main.join("sub/deep")).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "x"]);
    let linked = dir.path().join("linked");
    git(&main, &["worktree", "add", "-q", "--detach", linked.to_str().unwrap()]);
    std::fs::create_dir_all(linked.join("sub")).unwrap();
    for p in [main.clone(), main.join("sub/deep"), linked.clone(), linked.join("sub")] {
        let want_root = PathBuf::from(git(&p, &["rev-parse", "--show-toplevel"]));
        let want_git = PathBuf::from(git(&p, &["rev-parse", "--path-format=absolute", "--git-dir"]));
        let (root, git_dir) = checkout_layout_from_fs(&p).expect("fast path resolves");
        assert_eq!(root, std::fs::canonicalize(want_root).unwrap(), "{p:?}");
        assert_eq!(git_dir, std::fs::canonicalize(want_git).unwrap(), "{p:?}");
    }
    // Not a checkout, or inside a git dir: leave it to git.
    assert!(checkout_layout_from_fs(dir.path()).is_none());
    assert!(checkout_layout_from_fs(&main.join(".git")).is_none());
    assert!(checkout_layout_from_fs(&main.join(".git/objects")).is_none());
    assert!(checkout_layout_from_fs(&dir.path().join("missing")).is_none());
}

#[test]
fn worktree_branches_are_created_as_mast_and_both_prefixes_are_ours() {
    assert_eq!(
        worktree_branch_from_title("Fix login flow"),
        "mast/fix-login-flow"
    );
    assert!(is_owned_branch("mast/swift-otter"));
    assert!(is_owned_branch("zeron/swift-otter"));
    assert!(!is_owned_branch("feature/zeron/x"));
    assert!(is_owned_branch_of("zeron/swift-otter", "swift-otter"));
    assert!(is_owned_branch_of("mast/swift-otter", "swift-otter"));
    assert!(!is_owned_branch_of("mast/other", "swift-otter"));
}

#[test]
fn broad_workspace_roots_stop_at_home_but_keep_projects_and_volumes() {
    let home = Path::new("/Users/example");
    assert!(is_broad_workspace_root_with_home(Path::new("/"), home));
    assert!(is_broad_workspace_root_with_home(Path::new("/Users"), home));
    assert!(is_broad_workspace_root_with_home(home, home));
    assert!(is_broad_workspace_root_with_home(
        Path::new("relative/project"),
        home
    ));
    assert!(!is_broad_workspace_root_with_home(
        Path::new("/Users/example/Projects/app"),
        home
    ));
    assert!(!is_broad_workspace_root_with_home(
        Path::new("/Volumes/work/app"),
        home
    ));
}

#[test]
fn automatic_access_blocks_privacy_managed_home_subtrees_only() {
    let home = Path::new("/Users/example");
    for path in [
        "/Users/example/Desktop",
        "/Users/example/Documents/project",
        "/Users/example/Downloads/repo",
        "/Users/example/Movies/clips",
        "/Users/example/Music/library",
        "/Users/example/Pictures/Photos Library.photoslibrary",
        "/Users/example/Library/Application Support/app",
    ] {
        assert!(is_automatic_access_blocked_with_home(Path::new(path), home));
    }
    assert!(!is_automatic_access_blocked_with_home(
        Path::new("/Users/example/Projects/app"),
        home
    ));
    assert!(!is_automatic_access_blocked_with_home(
        Path::new("/Volumes/work/app"),
        home
    ));
}

#[test]
fn resolved_broad_root_catches_dot_alias() {
    let home = home_dir();
    assert!(is_broad_workspace_root_resolved(&home.join(".")));
}

#[test]
fn home_picker_recognizes_privacy_managed_children_before_metadata_probes() {
    let home = Path::new("/Users/example");
    for name in [
        "Desktop",
        "Documents",
        "Downloads",
        "Movies",
        "Music",
        "Pictures",
        "Library",
    ] {
        assert!(is_protected_home_child_with_home(home, name, home));
    }
    assert!(!is_protected_home_child_with_home(home, "Projects", home));
    assert!(!is_protected_home_child_with_home(
        Path::new("/Users/example/Projects"),
        "Documents",
        home
    ));
}

fn history_commit(sha: String, parent_sha: Option<String>) -> GitHistoryCommit {
    GitHistoryCommit {
        subject: sha.clone(),
        sha,
        parent_shas: parent_sha.into_iter().collect(),
        author_name: "Test".into(),
        author_email: "test@example.com".into(),
        authored_at: "2026-08-20T12:00:00Z".into(),
        refs: Vec::new(),
    }
}

fn score(query: &str, candidate: &str) -> Option<u32> {
    let mut matcher = nucleo_matcher::Matcher::new({
        let mut config = nucleo_matcher::Config::DEFAULT;
        config.set_match_paths();
        config
    });
    nucleo_matcher::pattern::Pattern::parse(
        query,
        nucleo_matcher::pattern::CaseMatching::Smart,
        nucleo_matcher::pattern::Normalization::Smart,
    )
    .score(
        nucleo_matcher::Utf32String::from(candidate).slice(..),
        &mut matcher,
    )
}

#[test]
fn parses_common_github_remote_forms() {
    let expected = Some(("openai".to_string(), "codex".to_string()));
    assert_eq!(
        parse_github_remote("https://github.com/openai/codex.git"),
        expected
    );
    assert_eq!(
        parse_github_remote("git@github.com:openai/codex.git"),
        expected
    );
    assert_eq!(parse_github_remote("https://gitlab.com/openai/codex"), None);
}

#[tokio::test]
async fn github_avatar_downloads_once_into_the_local_cache() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let bytes = b"\xff\xd8\xffavatar".to_vec();
    let served = bytes.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await.unwrap();
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    served.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(&served).await.unwrap();
    });

    let data = tempfile::tempdir().unwrap();
    let repos =
        Repos::with_worktrees_root(data.path(), "device", data.path().join("worktrees"));
    let url = format!("http://{address}/avatar.jpg");
    let first = repos.cache_github_avatar(&url).await.expect("downloaded");
    server.await.unwrap();
    assert_eq!(tokio::fs::read(&first).await.unwrap(), bytes);
    assert_eq!(
        repos.cache_github_avatar(&url).await.as_deref(),
        Some(first.as_str())
    );
}

#[test]
fn linux_drives_take_media_and_mnt_mounts_system_first() {
    let mounts = "\
sysfs /sys sysfs rw 0 0
/dev/nvme0n1p2 / ext4 rw 0 0
tmpfs /run tmpfs rw 0 0
/dev/sda1 /media/wing/T7\\040Shield exfat rw 0 0
/dev/sdb1 /run/media/wing/Backup ext4 rw 0 0
/dev/sdc1 /mnt/scratch ext4 rw 0 0
/dev/nvme0n1p2 /var/lib/docker ext4 rw 0 0
";
    let drives = finish_drives(linux_drives(mounts));
    let rows: Vec<(&str, &str)> = drives
        .iter()
        .map(|d| (d.name.as_str(), d.path.as_str()))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("System", "/"),
            ("Backup", "/run/media/wing/Backup"),
            ("scratch", "/mnt/scratch"),
            ("T7 Shield", "/media/wing/T7 Shield"),
        ]
    );
}

#[test]
fn linux_drives_take_custom_top_level_block_mounts_not_system_splits() {
    let mounts = "\
/dev/nvme0n1p2 / ext4 rw 0 0
/dev/sdb1 /disk2 ext4 rw 0 0
/dev/mapper/vault /tank btrfs rw 0 0
/dev/nvme0n1p1 /boot/efi vfat rw 0 0
/dev/sdd1 /boot ext4 rw 0 0
/dev/sde1 /home ext4 rw 0 0
/dev/sdf1 /data/disks/a ext4 rw 0 0
/dev/loop3 /snap/core22/1234 squashfs ro 0 0
/dev/loop9 /disk3 ext4 rw 0 0
";
    let drives = finish_drives(linux_drives(mounts));
    let rows: Vec<&str> = drives.iter().map(|d| d.path.as_str()).collect();
    // /disk2 and /tank are user drive roots; a loop-mounted ext4 image at
    // a custom root counts too (it's squashfs snaps that are noise). The
    // system's own split partitions and deep mounts stay out.
    assert_eq!(rows, vec!["/", "/disk2", "/disk3", "/tank"]);
}

#[test]
fn finish_drives_dedupes_by_mount_point_keeping_the_first_name() {
    let drives = finish_drives(vec![
        DriveEntry {
            name: "Macintosh HD".into(),
            path: "/".into(),
        },
        DriveEntry {
            name: "System".into(),
            path: "/".into(),
        },
    ]);
    assert_eq!(drives.len(), 1);
    assert_eq!(drives[0].name, "Macintosh HD");
}

#[test]
fn mount_point_unescape_handles_octal_and_lone_backslash() {
    assert_eq!(unescape_mount_point("/media/a\\040b"), "/media/a b");
    assert_eq!(unescape_mount_point("/media/tab\\011x"), "/media/tab\tx");
    assert_eq!(unescape_mount_point("/media/plain"), "/media/plain");
    assert_eq!(unescape_mount_point("/media/tail\\"), "/media/tail\\");
}

#[tokio::test]
async fn list_drives_resolves_on_this_platform() {
    let data = tempfile::tempdir().unwrap();
    let repos =
        Repos::with_worktrees_root(data.path(), "device", data.path().join("worktrees"));
    // Content is machine-dependent; the call must succeed and dedupe.
    let drives = repos.list_drives().await.unwrap();
    let mut paths: Vec<&String> = drives.iter().map(|d| &d.path).collect();
    paths.dedup();
    assert_eq!(paths.len(), drives.len());
}

#[test]
fn fuzzy_score_matches_a_path_subsequence() {
    assert!(score("cmp rs", "crates/ui/src/composer.rs").is_some());
    assert!(score("composer crates", "crates/ui/src/composer.rs").is_some());
    assert!(score("xyzq", "crates/ui/src/composer.rs").is_none());
}

#[test]
fn git_history_matches_unicode_case_insensitively() {
    let mut candidate = history_commit("a1b2c3d4".into(), None);
    candidate.subject = "RÉPARER la recherche".into();

    assert!(git_history_matches("réparer", &candidate));
}

#[test]
fn history_compaction_handles_a_twenty_thousand_commit_gap() {
    const DEPTH: usize = 20_000;
    let commits = (0..DEPTH)
        .rev()
        .map(|index| {
            history_commit(
                format!("c{index:05}"),
                (index > 0).then(|| format!("c{:05}", index - 1)),
            )
        })
        .collect::<Vec<_>>();
    let newest = format!("c{:05}", DEPTH - 1);
    let oldest = "c00000".to_string();
    let visible = HashSet::from([newest.clone(), oldest.clone()]);

    let compact = compact_history_commits(&commits, &visible);

    assert_eq!(compact.len(), 2);
    assert_eq!(compact[0].sha, newest);
    assert_eq!(compact[0].parent_shas, vec![oldest.clone()]);
    assert_eq!(compact[1].sha, oldest);
    assert!(compact[1].parent_shas.is_empty());
}

#[test]
fn search_files_obeys_gitignore_and_returns_directories() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/composer.rs"), "").unwrap();
    std::fs::write(root.path().join(".secret"), "").unwrap();
    std::fs::write(root.path().join(".gitignore"), "ignored\n").unwrap();
    std::fs::create_dir(root.path().join("ignored")).unwrap();
    std::fs::write(root.path().join("ignored/nope.rs"), "").unwrap();

    let matches = search_files_blocking(root.path(), "src", &[]).unwrap();
    assert!(
        matches
            .iter()
            .any(|entry| entry.path == "src" && entry.is_dir)
    );
    assert!(matches.iter().any(|entry| entry.path == "src/composer.rs"));
    assert!(
        !matches
            .iter()
            .any(|entry| entry.path.starts_with("ignored"))
    );
    assert!(
        search_files_blocking(root.path(), "secret", &[])
            .unwrap()
            .iter()
            .any(|entry| entry.path == ".secret")
    );
    assert!(!matches.iter().any(|entry| entry.path.starts_with(".git/")));
}

#[test]
fn empty_search_features_recent_paths_before_shallow_entries() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("README.md"), "").unwrap();
    std::fs::write(root.path().join("src/deep.rs"), "").unwrap();

    let matches = search_files_blocking(root.path(), "", &["src/deep.rs".into()]).unwrap();
    assert_eq!(
        matches.first().map(|entry| entry.path.as_str()),
        Some("src/deep.rs")
    );
    assert!(matches.iter().any(|entry| entry.path == "README.md"));
}

#[test]
fn search_files_prefers_filename_matches() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("composer/docs")).unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("composer/docs/readme.md"), "").unwrap();
    std::fs::write(root.path().join("src/composer.rs"), "").unwrap();

    let matches = search_files_blocking(root.path(), "composer", &[]).unwrap();
    let composer = matches
        .iter()
        .position(|entry| entry.path == "src/composer.rs")
        .unwrap();
    let path_only = matches
        .iter()
        .position(|entry| entry.path == "composer/docs/readme.md")
        .unwrap();
    assert!(composer < path_only);
}

#[test]
fn cached_search_reuses_one_walk_within_the_ttl() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("alpha.rs"), "").unwrap();
    let cache: FileIndexCache = std::sync::Mutex::new(HashMap::new());

    let first = search_files_cached(&cache, root.path(), "alpha", &[], || false).unwrap();
    assert_eq!(first.first().map(|m| m.path.as_str()), Some("alpha.rs"));
    std::fs::write(root.path().join("beta.rs"), "").unwrap();
    let second = search_files_cached(&cache, root.path(), "beta", &[], || false).unwrap();
    assert!(second.is_empty(), "{second:?}");
}

#[test]
fn cancelled_search_stops_before_walking() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("README.md"), "").unwrap();
    let cancelled = AtomicBool::new(true);

    let err = search_files_blocking_with_cancel(root.path(), "", &[], || {
        cancelled.load(Ordering::Relaxed)
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("cancelled"));
}

#[tokio::test]
async fn workspace_checkout_rejects_sibling_paths() {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let sibling = tempfile::tempdir().unwrap();
    let repos =
        Repos::with_worktrees_root(data.path(), "device", data.path().join("worktrees"));

    assert_eq!(
        repos.workspace_checkout(root.path(), root.path()).await,
        std::fs::canonicalize(root.path()).ok()
    );
    assert!(
        repos
            .workspace_checkout(root.path(), sibling.path())
            .await
            .is_none()
    );
}

fn git_in(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[tokio::test]
async fn unknown_checkout_lookups_share_one_worktree_list() {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let wt_parent = tempfile::tempdir().unwrap();
    let missing = wt_parent.path().join("gone");
    git_in(root.path(), &["init", "-q"]);
    git_in(root.path(), &["commit", "-q", "--allow-empty", "-m", "init"]);
    let linked = wt_parent.path().join("linked");
    git_in(
        root.path(),
        &["worktree", "add", "-q", "-b", "side", linked.to_str().unwrap()],
    );
    let repos =
        Repos::with_worktrees_root(data.path(), "device", data.path().join("worktrees"));
    let refreshes = |r: &Repos| r.inner.checkout_set_refreshes.load(Ordering::Relaxed);

    // 40 concurrent lookups of a path that does not exist never reach git;
    // a canonical-but-unknown path costs exactly one shared refresh.
    let stray = tempfile::tempdir().unwrap();
    let stray_path = std::fs::canonicalize(stray.path()).unwrap();
    let lookups = (0..40).map(|_| {
        let repos = repos.clone();
        let root = root.path().to_path_buf();
        let stray_path = stray_path.clone();
        let missing = missing.clone();
        async move {
            assert!(repos.workspace_checkout(&root, &missing).await.is_none());
            repos.workspace_checkout_canonical(&root, stray_path).await
        }
    });
    for found in futures::future::join_all(lookups).await {
        assert!(found.is_none());
    }
    assert_eq!(refreshes(&repos), 1);

    // Hits are served from the cache, including the linked worktree.
    let before = refreshes(&repos);
    assert_eq!(
        repos.workspace_checkout(root.path(), &linked).await,
        std::fs::canonicalize(&linked).ok()
    );
    assert_eq!(
        repos.workspace_checkout(root.path(), root.path()).await,
        std::fs::canonicalize(root.path()).ok()
    );
    assert_eq!(refreshes(&repos), before);
}

#[tokio::test]
async fn concurrent_searches_of_one_checkout_do_not_cancel_each_other() {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("alpha.rs"), "").unwrap();
    std::fs::write(root.path().join("beta.rs"), "").unwrap();
    let repos =
        Repos::with_worktrees_root(data.path(), "device", data.path().join("worktrees"));

    let alpha = repos.search_files(root.path().into(), "alpha".into(), Vec::new());
    let beta = repos.search_files(root.path().into(), "beta".into(), Vec::new());
    let (alpha, beta) = tokio::join!(alpha, beta);

    assert_eq!(alpha.unwrap()[0].path, "alpha.rs");
    assert_eq!(beta.unwrap()[0].path, "beta.rs");
}
