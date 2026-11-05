//! Exercise subagent scanning with isolated synthetic metadata.

#[test]
fn scans_a_session_with_subagents() {
    let directory = tempfile::tempdir().unwrap();
    let transcript = directory.path().join("session.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    let subagents = directory.path().join("session/subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    std::fs::write(
        subagents.join("agent-child.meta.json"),
        r#"{"description":"Synthetic child"}"#,
    )
    .unwrap();
    let result = mast_engine::subagent_scan::scan_subagents(&transcript).expect("scan");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].description.as_deref(), Some("Synthetic child"));
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
