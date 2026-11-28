//! The land step's call into the ledger: read what a landed commit changed
//! from git, then apply it with [`LivingDocStore::on_landed`].

use std::path::Path;
use std::process::Output;
use std::sync::Arc;

use super::{DocStatusChange, LivingDocStore};
use crate::merge_queue::LandHook;

fn git(dir: &Path, args: &[&str]) -> Result<Output, String> {
    let mut command = mast_harness::git_command();
    command
        .current_dir(dir)
        .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false"])
        .args(args);
    for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE"] {
        command.env_remove(name);
    }
    command.env("LC_ALL", "C").env("GIT_TERMINAL_PROMPT", "0");
    let _permit = crate::git_gate::acquire_blocking("living_docs:git");
    command
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))
}

fn git_ok(dir: &Path, args: &[&str]) -> Result<Output, String> {
    let out = git(dir, args)?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn nul_list(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

/// The blob id of `path` at `rev`; `None` when the file does not exist there.
/// Any other failure is an error, never a missing file.
fn blob_at(dir: &Path, rev: &str, path: &str) -> Result<Option<String>, String> {
    let spec = format!("{rev}:{path}");
    let out = git(dir, &["rev-parse", "--verify", "--quiet", &spec])?;
    if out.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        ));
    }
    if out.status.code() == Some(1) && out.stderr.is_empty() {
        return Ok(None);
    }
    Err(format!(
        "git rev-parse {spec}: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// Apply the commit `landed` (first parent = what it landed on) to
/// `project`'s rows. `git_dir` is any directory git can run in: the
/// repository's common git directory or a worktree.
pub fn apply_landed_commit(
    store: &LivingDocStore,
    project: &str,
    git_dir: &Path,
    landed: &str,
) -> Result<Vec<DocStatusChange>, String> {
    let parent_rev = format!("{landed}^1");
    let has_parent = git(git_dir, &["rev-parse", "--verify", "--quiet", &parent_rev])?
        .status
        .success();
    let changed = if has_parent {
        let out = git_ok(
            git_dir,
            &["diff", "--no-renames", "--name-only", "-z", &parent_rev, landed],
        )?;
        nul_list(&out.stdout)
    } else {
        let out = git_ok(git_dir, &["ls-tree", "-r", "--name-only", "-z", landed])?;
        nul_list(&out.stdout)
    };
    store.on_landed(
        project,
        landed,
        &changed,
        |path| {
            if has_parent {
                blob_at(git_dir, &parent_rev, path)
            } else {
                Ok(None)
            }
        },
        |path| blob_at(git_dir, landed, path),
    )
}

/// A [`MergeQueue`](crate::merge_queue::MergeQueue) land hook that keeps
/// `store` current. A failure is logged, never raised: the land has happened.
pub fn merge_queue_hook(store: Arc<LivingDocStore>) -> LandHook {
    Arc::new(move |request, landed| {
        if let Err(error) = apply_landed_commit(
            &store,
            &landed.project_id,
            Path::new(&request.repo),
            &landed.landed_sha,
        ) {
            tracing::warn!(
                project = %landed.project_id,
                commit = %landed.landed_sha,
                %error,
                "living documents were not updated for a landed commit"
            );
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::living_docs::{
        DocDriftStatus, DriftKind, FsRepoTree, KnownSymbols, default_seed_rules, seed_rows,
    };

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = git_ok(dir, args).unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn commit(repo: &Path, files: &[(&str, &str)], message: &str) -> String {
        for (path, text) in files {
            let full = repo.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        run(repo, &["add", "-A"]);
        run(
            repo,
            &[
                "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                "-c", "commit.gpgsign=false", "commit", "-q", "-m", message,
            ],
        );
        run(repo, &["rev-parse", "HEAD"])
    }

    /// A repository with `docs/mcp.md` and one MCP tool file, ledger seeded.
    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, LivingDocStore) {
        let repo = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "-q"]);
        let first = commit(
            repo.path(),
            &[
                ("docs/mcp.md", "# MCP\n"),
                ("crates/mcp/src/tools/echo.rs", "// echo\n"),
            ],
            "start",
        );
        let data = tempfile::tempdir().unwrap();
        let store = LivingDocStore::open(data.path()).unwrap();
        store
            .update("p", |ledger| {
                seed_rows(&mut ledger.rows, default_seed_rules(), "item", |path| {
                    blob_at(repo.path(), &first, path).ok().flatten()
                });
            })
            .unwrap();
        (repo, data, store)
    }

    fn status(store: &LivingDocStore, doc: &str) -> DocDriftStatus {
        store
            .ledger("p")
            .rows
            .into_iter()
            .find(|row| row.doc_id == doc)
            .unwrap()
            .drift_status
    }

    #[test]
    fn landing_mcp_tool_code_without_its_doc_owes_the_doc() {
        let (repo, _data, store) = fixture();
        assert_eq!(status(&store, "repo:docs/mcp.md"), DocDriftStatus::Fresh);
        let landed = commit(repo.path(), &[("crates/mcp/src/tools/echo.rs", "// echo 2\n")], "tool");
        let changes = apply_landed_commit(&store, "p", repo.path(), &landed).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(status(&store, "repo:docs/mcp.md"), DocDriftStatus::OwedUpdate);
        let row = &store.ledger("p").rows;
        let row = row.iter().find(|r| r.doc_id == "repo:docs/mcp.md").unwrap();
        assert_eq!(row.owed_since_commit.as_deref(), Some(landed.as_str()));
    }

    #[test]
    fn landing_the_code_with_its_doc_keeps_the_doc_fresh() {
        let (repo, _data, store) = fixture();
        let landed = commit(
            repo.path(),
            &[
                ("crates/mcp/src/tools/echo.rs", "// echo 2\n"),
                ("docs/mcp.md", "# MCP\nechoes\n"),
            ],
            "tool and doc",
        );
        apply_landed_commit(&store, "p", repo.path(), &landed).unwrap();
        assert_eq!(status(&store, "repo:docs/mcp.md"), DocDriftStatus::Fresh);
    }

    #[test]
    fn a_later_doc_update_clears_the_debt() {
        let (repo, _data, store) = fixture();
        let code = commit(repo.path(), &[("crates/mcp/src/tools/echo.rs", "// 2\n")], "tool");
        apply_landed_commit(&store, "p", repo.path(), &code).unwrap();
        let doc = commit(repo.path(), &[("docs/mcp.md", "# MCP\nnow\n")], "doc");
        apply_landed_commit(&store, "p", repo.path(), &doc).unwrap();
        assert_eq!(status(&store, "repo:docs/mcp.md"), DocDriftStatus::Fresh);
    }

    #[test]
    fn renaming_a_documented_cli_subcommand_is_tier_a_drift() {
        let (repo, _data, store) = fixture();
        let doc = "# CLI\n\nRun `mast daemon install` to set up.\n";
        let landed = commit(repo.path(), &[("docs/mcp.md", doc)], "doc");
        apply_landed_commit(&store, "p", repo.path(), &landed).unwrap();
        let known = |subs: &[&str]| KnownSymbols {
            tool_names: ["echo".to_owned()].into(),
            cli_subcommands: subs.iter().map(|s| (*s).to_owned()).collect(),
        };
        let tree = FsRepoTree::new(repo.path());
        let read = |id: &str| std::fs::read_to_string(repo.path().join(id.strip_prefix("repo:")?)).ok();
        // Before the rename the documented command exists.
        store
            .check_drift("p", &landed, &known(&["daemon", "daemon install", "daemon uninstall"]), &tree, read)
            .unwrap();
        assert_eq!(status(&store, "repo:docs/mcp.md"), DocDriftStatus::Fresh);
        // The subcommand is renamed in code; the doc still says the old name.
        let changes = store
            .check_drift("p", &landed, &known(&["daemon", "daemon setup", "daemon uninstall"]), &tree, read)
            .unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(status(&store, "repo:docs/mcp.md"), DocDriftStatus::DriftDetected);
        let findings = &store.ledger("p").findings["repo:docs/mcp.md"];
        assert!(findings.iter().any(|f| f.kind == DriftKind::UnknownCliSubcommand
            && f.symbol == "mast daemon install"));
    }
}
