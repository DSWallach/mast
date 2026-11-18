//! Read-only Claude Desktop schedule discovery. Never edits source schedules.
use super::{evaluator, fail};
use crate::EngineError;
use mast_proto::*;
use std::{
    io::Read,
    path::{Path, PathBuf},
};

fn read(path: &Path, limit: u64) -> Result<Vec<u8>, EngineError> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(fail("Import file is too large"));
    }
    Ok(bytes)
}
fn registries() -> Vec<PathBuf> {
    discover_registries().0
}

fn discover_registries() -> (Vec<PathBuf>, bool) {
    fn directories(path: &Path, complete: &mut bool) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        match std::fs::read_dir(path) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => match entry.path().metadata() {
                            Ok(metadata) if metadata.is_dir() => paths.push(entry.path()),
                            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                                *complete = false;
                            }
                            _ => {}
                        },
                        Err(_) => *complete = false,
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => *complete = false,
        }
        paths
    }

    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return (Vec::new(), false);
    };
    let root = home.join("Library/Application Support/Claude");
    let mut paths = Vec::new();
    let mut complete = true;
    for kind in ["claude-code-sessions", "local-agent-mode-sessions"] {
        for account in directories(&root.join(kind), &mut complete) {
            for org in directories(&account, &mut complete) {
                let path = org.join("scheduled-tasks.json");
                match path.metadata() {
                    Ok(metadata) if metadata.is_file() => paths.push(path),
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => complete = false,
                    _ => {}
                }
            }
        }
    }
    paths.sort();
    (paths, complete)
}

/// The `filePath` of the registry row an import source (`…json#id`) names,
/// read from that registry now. `None` when the registry or row is gone.
pub(crate) fn registry_file(import_source: &str) -> Option<String> {
    let (path, id) = import_source.rsplit_once('#')?;
    let bytes = read(Path::new(path), 2 * 1024 * 1024).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value["scheduledTasks"]
        .as_array()?
        .iter()
        .find(|row| row["id"].as_str() == Some(id))?["filePath"]
        .as_str()
        .map(str::to_owned)
}

/// Match transcript provenance to a registry definition only when unambiguous.
pub(crate) fn imported_identity(
    file: &str,
    name: &str,
    device: &str,
    session: &str,
    scope: &str,
) -> String {
    let (mut sources, _) = desktop_sources(file, name);
    let source = if sources.len() == 1 {
        sources.remove(0)
    } else if sources.len() > 1 {
        format!("ambiguous:{device}:{file}:{session}")
    } else {
        format!("legacy:{device}:{file}")
    };
    task_id(scope, device, &source)
}

/// Exact source matches and whether the search was conclusive. Missing or
/// unreadable registries cannot prove a definition has been retired.
pub(super) fn desktop_sources(file: &str, name: &str) -> (Vec<String>, bool) {
    let mut sources = Vec::new();
    let (paths, complete) = discover_registries();
    let mut all_read = complete && !paths.is_empty();
    for path in paths {
        let rows = read(&path, 2 * 1024 * 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let Some(rows) = rows.as_ref().and_then(|value| value["scheduledTasks"].as_array()) else {
            all_read = false;
            continue;
        };
        for row in rows {
            if row["id"].as_str() == Some(name) && row["filePath"].as_str() == Some(file) {
                sources.push(format!("{}#{name}", path.display()));
            }
        }
    }
    (sources, all_read)
}

pub(crate) fn task_id(scope: &str, device: &str, source: &str) -> String {
    crate::work_outline_launch::scheduled_id(&format!("{scope}\0{device}\0{source}"), "import")
}

pub(crate) fn verify_paused(source: &str) -> Result<(), EngineError> {
    let (file, id) = source
        .rsplit_once('#')
        .ok_or_else(|| fail("Import source cannot be verified"))?;
    let value: serde_json::Value =
        serde_json::from_slice(&read(Path::new(file), 2 * 1024 * 1024)?).map_err(fail)?;
    let row = value["scheduledTasks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["id"].as_str() == Some(id))
        .ok_or_else(|| fail("Source task is missing; its paused state cannot be verified"))?;
    if row["enabled"].as_bool() != Some(false) {
        return Err(fail(
            "Pause this schedule in Claude before enabling it in Mast",
        ));
    }
    Ok(())
}
pub fn preview(
    path: Option<&str>,
    timezone: &str,
    space_id: Option<String>,
) -> Result<Vec<ScheduledImportCandidate>, EngineError> {
    let paths = path
        .map(|p| vec![PathBuf::from(p)])
        .unwrap_or_else(registries);
    let mut result = Vec::new();
    for path in paths {
        let value: serde_json::Value =
            serde_json::from_slice(&read(&path, 2 * 1024 * 1024)?).map_err(fail)?;
        let rows = value
            .get("scheduledTasks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| fail("Unrecognized Claude schedule registry"))?;
        for row in rows {
            let Some(id) = row["id"].as_str() else {
                continue;
            };
            let Some(file) = row["filePath"].as_str() else {
                continue;
            };
            let schedule = if let Some(cron) = row["cronExpression"].as_str() {
                TaskSchedule::Cron {
                    expression: cron.into(),
                    timezone: timezone.into(),
                }
            } else if let Some(at) = row["fireAt"].as_i64() {
                TaskSchedule::Once { at }
            } else {
                continue;
            };
            evaluator::validate(&schedule).map_err(fail)?;
            // A task that repeats within the hour only catches up its latest miss briefly.
            let sub_hourly = matches!(&schedule, TaskSchedule::Cron { expression, .. }
                if expression.split_whitespace().next().is_some_and(|m| m == "*" || m.contains(['/', ',', '-'])));
            let prompt = String::from_utf8(read(Path::new(file), 128 * 1024)?).map_err(fail)?;
            if prompt
                .lines()
                .take(12)
                .any(|line| line.to_uppercase().contains("RETIRED"))
            {
                continue;
            }
            let mut warnings=vec!["Imported paused. Confirm timezone, model, tools and permissions before cutover.".into(),"Pause this task in Claude and confirm no source run is active before enabling Mast.".into()];
            if row.get("approvedPermissions").is_some()
                || row["permissionMode"]
                    .as_str()
                    .is_some_and(|s| s != "default")
            {
                warnings.push(
                    "Source approvals are not transferred; Mast will ask for permissions.".into(),
                );
            }
            if row["useWorktree"].as_bool() == Some(true) {
                warnings.push("Source uses isolated worktrees; choose a suitable working directory before enabling.".into());
            }
            result.push(ScheduledImportCandidate {
                source: format!("{}#{id}", path.display()),
                source_enabled: row["enabled"].as_bool().unwrap_or(false),
                draft: ScheduledTaskDraft {
                    name: row["displayName"].as_str().unwrap_or(id).into(),
                    prompt,
                    cwd: row["cwd"].as_str().unwrap_or("").into(),
                    space_id: space_id.clone(),
                    config: ChatConfig { approval_rules: true,
                        harness: HarnessId::ClaudeCode,
                        model: row["model"].as_str().map(str::to_owned),
                        reasoning: None,
                        model_options: Default::default(),
                        sandbox: SandboxLevel::WorkspaceWrite,
                        auto_approve: false,
                    },
                    schedule,
                    enabled: false,
                    cutover_confirmed: false,
                    work_item_id: None,
                    output: Default::default(),
                    after_success: Default::default(),
                    grace_minutes: if sub_hourly { 30 } else { 360 },
                },
                warnings,
            });
        }
    }
    Ok(result)
}
