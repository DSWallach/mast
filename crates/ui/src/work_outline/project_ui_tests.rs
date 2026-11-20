// Work Item Details "Project" section: the toggle and the worker destination
// send `updateWorkItem` with the new fields, and neither is an undo step.
// Included from `overview.rs`'s test module so they share its fixtures.
use super::*;

#[gpui::test]
fn documentation_inventory_entry_is_available_on_projects(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = instruction_fixture::snapshot("s1");
    snapshot.items.iter_mut().find(|item| item.id == HOUSEKEEPING).unwrap().is_project = true;
    let (overview, _, vcx) = instruction_overview(cx, dir.path(), snapshot, reply_ok);
    let _details = open_details(&overview, vcx, HOUSEKEEPING);
    assert!(vcx.debug_bounds("project-document-inventory-section").is_some());
    assert!(vcx.debug_bounds("project-document-inventory").is_some());
}

#[gpui::test]
fn documentation_inventory_button_reads_then_starts_project_task(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let _guard = runtime.enter();
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = instruction_fixture::snapshot("s1");
    snapshot.items.iter_mut().find(|item| item.id == HOUSEKEEPING).unwrap().is_project = true;
    snapshot.execution_space = Some(mast_proto::work_outline_execution::ExecutionSpace { space_id: "s1".into(), path: "/tmp".into(), device_id: "device".into() });
    let (overview, _, vcx) = instruction_overview(cx, dir.path(), snapshot, reply_ok);
    let (out, mut requests) = tokio::sync::mpsc::channel::<String>(64);
    let (replies, inbound) = tokio::sync::mpsc::channel::<String>(64);
    let state = overview.update(vcx, |overview, _| overview.state.clone());
    state.update(vcx, |state, _| state.set_test_engine(crate::state::EngineHandle::from_test_client(mast_rpc::RpcClient::new(out, inbound))));
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    let at = vcx.debug_bounds("project-document-inventory").unwrap().center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    let frame = requests.try_recv().expect("initial read request");
    let request: serde_json::Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(request["method"], mast_rpc::methods::READ_PROJECT_DOCUMENT_INVENTORY);
    runtime.block_on(async {
        replies.send(serde_json::json!({"id": request["id"], "ok": {"inventory": {"taskId":"x", "observedAt":0}}}).to_string()).await.unwrap();
        while replies.capacity() < replies.max_capacity() { tokio::task::yield_now().await; }
    });
    vcx.run_until_parked();
    assert!(requests.try_recv().is_err(), "malformed read must not start a scan");
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    let completed = serde_json::json!({"projectId": HOUSEKEEPING, "taskId":"task", "destinationId":"destination", "ownerDeviceId":"device", "observedAt":1, "artifacts":[], "pending":[]});
    let frame = requests.try_recv().expect("completed inventory read");
    let request: serde_json::Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(request["method"], mast_rpc::methods::READ_PROJECT_DOCUMENT_INVENTORY);
    runtime.block_on(async {
        replies.send(serde_json::json!({"id": request["id"], "ok": {"inventory": completed}}).to_string()).await.unwrap();
        while replies.capacity() < replies.max_capacity() { tokio::task::yield_now().await; }
    });
    vcx.run_until_parked();
    assert!(requests.try_recv().is_err(), "completed inventory must open without a scan");
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    let reservation = serde_json::json!({"projectId": HOUSEKEEPING, "taskId":"task", "destinationId":"destination", "ownerDeviceId":"device", "observedAt":0, "artifacts":[], "pending":["Creation pending"]});
    for (index, initial) in [serde_json::json!({"inventory": reservation}), serde_json::json!({"inventory": null})].into_iter().enumerate() {
    for (method, result) in [
        (mast_rpc::methods::READ_PROJECT_DOCUMENT_INVENTORY, initial),
        (mast_rpc::methods::PROJECT_DOCUMENT_INVENTORY, serde_json::json!({"taskId":API, "observedAt":1, "artifacts":[], "pending":["Reconciliation pending"]})),
    ] {
        let request = loop {
            runtime.block_on(async { tokio::task::yield_now().await });
            let frame = requests.try_recv().unwrap_or_else(|_| panic!("missing {method} request"));
            let request: serde_json::Value = serde_json::from_str(&frame).unwrap();
            if request["method"] == method { break request; }
            runtime.block_on(replies.send(serde_json::json!({"id": request["id"], "err": "unavailable"}).to_string())).unwrap();
            vcx.run_until_parked();
        };
        assert_eq!(request["method"], method);
        assert_eq!(request["params"]["projectId"], HOUSEKEEPING);
        if method == mast_rpc::methods::PROJECT_DOCUMENT_INVENTORY {
            assert_eq!(request["params"]["scan"], true);
            assert_eq!(request["params"]["targetDeviceId"], "device");
        }
        runtime.block_on(async {
            replies.send(serde_json::json!({"id": request["id"], "ok": result}).to_string()).await.unwrap();
            while replies.capacity() < replies.max_capacity() { tokio::task::yield_now().await; }
        });
        vcx.run_until_parked();
    }
    assert!(vcx.debug_bounds("project-document-task").is_some());
    if index == 0 {
        vcx.simulate_click(at, gpui::Modifiers::default());
        vcx.run_until_parked();
    }
    }
    vcx.update(|window, cx| details.update(cx, |d, cx| d.begin_title_draft("Unsaved project name", window, cx)));
    let at = vcx.debug_bounds("project-document-task").expect("task link").center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    let request = details.update(vcx, |d, _| d.captured_confirm.take()).expect("asks before discarding the draft");
    vcx.update(|window, cx| (request.on_decision)(false, window, cx));
    assert!(details.read_with(vcx, |d, cx| d.has_unsaved_draft(cx)));
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    let request = details.update(vcx, |d, _| d.captured_confirm.take()).expect("asks again");
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    assert_eq!(overview.read_with(vcx, |o, cx| o.work_item_details.as_ref().map(|d| d.read(cx).item_id.clone())).as_deref(), Some(API));
}

#[gpui::test]
fn the_project_toggle_and_worker_destination_send_updates_and_are_not_undoable(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    let at = vcx
        .debug_bounds("work-item-project-toggle")
        .expect("project toggle")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    let last = calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last["op"], "updateWorkItem");
    assert_eq!(last["changes"]["isProject"], true);

    details.update(vcx, |d, cx| {
        d.set_worker_destination(Some("tickets".into()), cx)
    });
    vcx.run_until_parked();
    let last = calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last["changes"]["workerDestination"], "tickets");
    details.update(vcx, |d, cx| d.set_worker_destination(None, cx));
    vcx.run_until_parked();
    let last = calls.lock().unwrap().last().cloned().unwrap();
    assert!(last["changes"]["workerDestination"].is_null());
    assert!(last["changes"].get("workerDestination").is_some());

    // Project settings never land on the undo stack.
    let snapshot = instruction_fixture::snapshot("s1");
    let changes = mast_proto::WorkItemChanges {
        is_project: Some(true),
        ..Default::default()
    };
    assert!(
        crate::work_outline::action_log::update_entry(&snapshot, HOUSEKEEPING, &changes, None)
            .is_none()
    );
}
