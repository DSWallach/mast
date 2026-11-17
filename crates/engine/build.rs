fn main() {
    // Explicit invocation metadata invalidates shared Cargo artifacts across worktrees.
    // Without it, leave provenance unknown rather than reuse another checkout's SHA.
    println!("cargo:rerun-if-env-changed=MAST_USAGE_BUILD_REVISION");
    println!("cargo:rerun-if-changed=build.rs");
    let revision = std::env::var("MAST_USAGE_BUILD_REVISION")
        .ok()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 80
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=MAST_USAGE_BUILD_REVISION={revision}");
}
