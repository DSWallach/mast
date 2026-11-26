//! Runs every YAML in `scenarios/` through the batch runner, each in its own
//! sandbox. Report-only in the release run: scripts/run_release_tests.sh runs
//! this outside nextest (it needs a host sandbox) and never fails on it.
//!
//! `scripts/ui-drive run <file.yaml>` drives `scenario_launcher` the same way.
#![cfg(any(target_os = "macos", target_os = "linux"))]
#![allow(clippy::disallowed_methods)]

#[cfg(test)]
#[path = "../../../test_support/isolation.rs"]
mod test_isolation;

use std::path::PathBuf;

use mast_ui_driver::scenario;

fn account_home() -> PathBuf {
    // The test constructor replaces HOME; the driver guard must still receive
    // the actual account home so it can verify the scratch sandbox is separate.
    if let Some(home) = std::env::var_os("MAST_TEST_ORIGINAL_HOME") {
        return PathBuf::from(home);
    }
    PathBuf::from(std::env::var_os("HOME").expect("HOME"))
}

/// Inside the sandbox: play the scenario `launch` wrote to the control dir.
#[test]
#[ignore = "runs inside the driver sandbox; started by the launcher"]
fn scenario_serve() {
    if let Err(error) = scenario::serve_from_control() {
        panic!("scenario runner failed: {error}");
    }
}

fn output_dir(name: &str) -> PathBuf {
    match std::env::var_os("MAST_SCENARIO_OUT") {
        Some(dir) => PathBuf::from(dir).join(name),
        None => std::env::temp_dir().join("mast-scenarios").join(name),
    }
}

fn render() -> bool {
    std::env::var_os("MAST_SCENARIO_RENDER").is_some()
}

/// Outside the sandbox: `MAST_SCENARIO_FILE` names one scenario (the CLI's
/// `run`). Prints the report; fails when the scenario does.
#[test]
#[ignore = "requires an available host sandbox"]
fn scenario_launcher() {
    let file = PathBuf::from(std::env::var_os("MAST_SCENARIO_FILE").expect("MAST_SCENARIO_FILE"));
    let name = file.file_stem().unwrap().to_string_lossy().into_owned();
    let exe = std::env::current_exe().unwrap();
    let report = scenario::launch(&exe, &account_home(), &file, &output_dir(&name), render())
        .unwrap_or_else(|e| panic!("{e}"));
    print!("{}", report.summary());
    assert!(report.ok, "scenario {} failed", report.name);
}

/// Every scenario under `scenarios/`. Prints a result line per scenario and
/// fails if any failed (the release script treats that as a report only).
#[test]
#[ignore = "requires an available host sandbox"]
fn run_all_scenarios() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    let files = scenario::scenario_files(&dir);
    assert!(!files.is_empty(), "no scenarios in {}", dir.display());
    let exe = std::env::current_exe().unwrap();
    let mut failed = Vec::new();
    for file in &files {
        let name = file.file_stem().unwrap().to_string_lossy().into_owned();
        match scenario::launch(&exe, &account_home(), file, &output_dir(&name), render()) {
            Ok(report) => {
                print!("{}", report.summary());
                if !report.ok {
                    failed.push(report.name);
                }
            }
            Err(error) => {
                println!("FAIL {name}: {error}");
                failed.push(name);
            }
        }
    }
    println!("scenarios: {} run, {} failed", files.len(), failed.len());
    assert!(failed.is_empty(), "failed scenarios: {failed:?}");
}

/// Every shipped scenario parses (no sandbox needed).
#[test]
fn shipped_scenarios_parse() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    for file in scenario::scenario_files(&dir) {
        scenario::Scenario::load(&file).unwrap_or_else(|e| panic!("{e}"));
    }
}
