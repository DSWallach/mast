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
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return vec![];
    };
    let root = home.join("Library/Application Support/Claude");
    let mut out = Vec::new();
    for kind in ["claude-code-sessions", "local-agent-mode-sessions"] {
        if let Ok(accounts) = std::fs::read_dir(root.join(kind)) {
            for account in accounts.flatten() {
                if let Ok(orgs) = std::fs::read_dir(account.path()) {
                    for org in orgs.flatten() {
                        let p = org.path().join("scheduled-tasks.json");
                        if p.is_file() {
                            out.push(p)
                        }
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// Match transcript provenance to a registry definition only when unambiguous.
pub(crate) fn imported_identity(
    file: &str,
    name: &str,
    device: &str,
    session: &str,
    scope: &str,
) -> String {
    let mut sources = Vec::new();
    for path in registries() {
        let Ok(bytes) = read(&path, 2 * 1024 * 1024) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        for row in value["scheduledTasks"].as_array().into_iter().flatten() {
            if row["id"].as_str() == Some(name) && row["filePath"].as_str() == Some(file) {
                sources.push(format!("{}#{name}", path.display()));
            }
        }
    }
    let source = if sources.len() == 1 {
        sources.remove(0)
    } else if sources.len() > 1 {
        format!("ambiguous:{device}:{file}:{session}")
    } else {
        format!("legacy:{device}:{file}")
    };
    task_id(scope, device, &source)
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
                    grace_minutes: if id.contains("stall-watch") { 30 } else { 360 },
                },
                warnings,
            });
        }
    }
    Ok(result)
}
