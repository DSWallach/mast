use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    let mut revision = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    if git(&["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|status| !status.is_empty())
    {
        revision.push_str("-dirty");
    }
    println!("cargo:rustc-env=MAST_USAGE_BUILD_REVISION={revision}");
    // Feature builds share package versions. Track commits and dirty source
    // changes so configuration cohorts can identify the code being compared.
    for source in [
        "../../crates",
        "../../apps",
        "../../Cargo.toml",
        "../../Cargo.lock",
    ] {
        println!("cargo:rerun-if-changed={source}");
    }
    for reference in [
        Some("HEAD".into()),
        Some("packed-refs".into()),
        git(&["symbolic-ref", "-q", "HEAD"]),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(path) = git(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            &reference,
        ]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
