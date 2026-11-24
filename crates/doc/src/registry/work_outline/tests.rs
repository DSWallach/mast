//! Work Outline registry tests: write rules R1-R8, placement, materialization
//! properties, convergence through a simulated room, Space suppression, and
//! the large-outline budget.

use std::collections::HashMap;

use super::*;
use crate::registry::tests::{chat, server_round, space};
use mast_proto::work_outline::WorkItemChatAssignmentState;

const SPACE: &str = "sp-1";
const OTHER_SPACE: &str = "sp-2";

fn actor(device: &str) -> WorkOutlineActor {
    WorkOutlineActor {
        kind: WorkOutlineActorKind::User,
        device_id: device.into(),
        chat_id: None,
        message_id: None,
    }
}

fn doc_with_spaces(device: &str) -> RegistryDoc {
    let mut doc = RegistryDoc::new(device);
    doc.upsert_space(&space(SPACE, device, "/tmp/one")).unwrap();
    doc.upsert_space(&space(OTHER_SPACE, device, "/tmp/two"))
        .unwrap();
    doc
}

fn apply(doc: &mut RegistryDoc, mutation: WorkOutlineMutation) -> Outcome<WorkOutlineMutateReply> {
    let device = doc.device_id().to_owned();
    doc.apply_work_outline_mutation(&mutation, &actor(&device), 1_700_000_000_000)
}

fn create_in(
    doc: &mut RegistryDoc,
    space_id: &str,
    op: &str,
    parent: Option<&str>,
    placement: WorkOutlinePlacement,
    title: &str,
) -> String {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: space_id.into(),
            parent_work_item_id: parent.map(str::to_owned),
            placement,
            title: title.into(),
            description: None,
            status: None,
            operation_id: op.into(),
        }),
    )
    .unwrap()
    .work_item_id
    .unwrap()
}

fn create(doc: &mut RegistryDoc, op: &str, parent: Option<&str>, title: &str) -> String {
    create_in(doc, SPACE, op, parent, WorkOutlinePlacement::Last, title)
}

fn move_to(
    doc: &mut RegistryDoc,
    op: &str,
    id: &str,
    parent: Option<&str>,
    placement: WorkOutlinePlacement,
) -> Outcome<WorkOutlineMutateReply> {
    apply(
        doc,
        WorkOutlineMutation::MoveWorkItem(MoveWorkItemParams {
            work_item_id: id.into(),
            destination_parent_work_item_id: parent.map(str::to_owned),
            placement,
            operation_id: op.into(),
        }),
    )
}

fn before(id: &str) -> WorkOutlinePlacement {
    WorkOutlinePlacement::Before {
        sibling_work_item_id: id.into(),
    }
}

fn after(id: &str) -> WorkOutlinePlacement {
    WorkOutlinePlacement::After {
        sibling_work_item_id: id.into(),
    }
}

fn snap(doc: &RegistryDoc) -> WorkOutlineSnapshot {
    doc.work_outline_snapshot(SPACE, true)
}

#[test]
fn waiting_fields_are_written_and_cleared() {
    let mut doc = doc_with_spaces("dev-a");
    let id = create(&mut doc, "create-waiting", None, "Observe");
    let update = |operation_id: &str, waiting_until, waiting_reason| {
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: id.clone(),
            changes: mast_proto::WorkItemChanges {
                waiting_until,
                waiting_reason,
                ..Default::default()
            },
            operation_id: operation_id.into(),
        })
    };
    apply(&mut doc, update("set-waiting", FieldPatch::Set(1_800_000_000_000), FieldPatch::Set("Observation".into()))).unwrap();
    let snapshot = snap(&doc);
    let item = snapshot.item(&id).unwrap();
    assert_eq!(item.waiting_until, Some(1_800_000_000_000));
    assert_eq!(item.waiting_reason.as_deref(), Some("Observation"));
    let row = doc.overlay_row(KIND_WORK_ITEMS, &id).unwrap();
    assert!(row.clocks.contains_key("waitingUntil"));
    assert!(row.clocks.contains_key("waitingReason"));
    apply(&mut doc, update("clear-waiting", FieldPatch::Clear, FieldPatch::Clear)).unwrap();
    let snapshot = snap(&doc);
    let item = snapshot.item(&id).unwrap();
    assert_eq!(item.waiting_until, None);
    assert_eq!(item.waiting_reason, None);
}

/// Titles of the effective children of `parent` (`None` = roots), in order.
fn titles(snapshot: &WorkOutlineSnapshot, parent: Option<&str>) -> Vec<String> {
    let ids = match parent {
        None => snapshot.root_work_item_ids.clone(),
        Some(id) => snapshot.item(id).unwrap().child_ids.clone(),
    };
    ids.iter()
        .map(|id| snapshot.item(id).unwrap().title.clone())
        .collect()
}

fn pending_kinds(doc: &RegistryDoc) -> BTreeSet<String> {
    doc.pending
        .iter()
        .flat_map(|b| b.ops.iter().map(|op| op.kind.clone()))
        .collect()
}

fn item_row(
    id: &str,
    space_id: &str,
    parent: Option<&str>,
    parent_clock: &str,
    key: Option<&str>,
) -> RegistryRow {
    let mut fields = BTreeMap::from([
        ("spaceId".to_owned(), json!(space_id)),
        ("title".to_owned(), json!(format!("T {id}"))),
    ]);
    let mut clocks = BTreeMap::from([
        ("spaceId".to_owned(), "0000000000001-000000-seed".to_owned()),
        ("title".to_owned(), "0000000000001-000000-seed".to_owned()),
        ("parentWorkItemId".to_owned(), parent_clock.to_owned()),
    ]);
    if let Some(parent) = parent {
        fields.insert("parentWorkItemId".into(), json!(parent));
    }
    if let Some(key) = key {
        fields.insert("orderKey".into(), json!(key));
        clocks.insert("orderKey".into(), parent_clock.to_owned());
    }
    RegistryRow {
        kind: KIND_WORK_ITEMS.into(),
        id: id.into(),
        seq: 0,
        deleted: false,
        del_hlc: None,
        fields,
        clocks,
    }
}

// ── mechanism: R1, R4, chunking ─────────────────────────────────────────────

#[test]
fn row_state_distinguishes_absent_live_and_tombstoned() {
    let mut doc = doc_with_spaces("dev-a");
    assert_eq!(doc.row_state(KIND_WORK_ITEMS, "nope"), RowState::Absent);
    let id = create(&mut doc, "op-1", None, "A");
    assert_eq!(doc.row_state(KIND_WORK_ITEMS, &id), RowState::Live);
    doc.delete_row_ops(&[(KIND_WORK_ITEMS, id.as_str())]);
    assert_eq!(doc.row_state(KIND_WORK_ITEMS, &id), RowState::Tombstoned);
    // A retried create of the deleted id never revives it.
    let err = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            title: "A".into(),
            description: None,
            status: None,
            operation_id: "op-1".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::NotFound);
    assert_eq!(doc.row_state(KIND_WORK_ITEMS, &id), RowState::Tombstoned);
}

fn small_op(i: usize) -> RowOp {
    RowOp {
        kind: "chats".into(),
        id: format!("c-{i}"),
        op: OpKind::Upsert,
        set: Some(BTreeMap::from([("title".to_owned(), json!("x"))])),
        hlc: encode_hlc(1, 0, "dev-a"),
        clocks: None,
    }
}

#[test]
fn chunking_keeps_400_op_batches_for_small_ops() {
    let mut doc = RegistryDoc::new("dev-a");
    doc.enqueue_ops((0..400).map(small_op).collect());
    assert_eq!(doc.pending_len(), 1);
    let mut doc = RegistryDoc::new("dev-a");
    doc.enqueue_ops((0..801).map(small_op).collect());
    let sizes: Vec<usize> = doc.pending.iter().map(|b| b.ops.len()).collect();
    assert_eq!(sizes, vec![400, 400, 1]);
}

#[test]
fn chunking_is_byte_aware_for_large_ops() {
    let mut doc = RegistryDoc::new("dev-a");
    let big = "x".repeat(12_000);
    let ops: Vec<RowOp> = (0..400)
        .map(|i| {
            let mut op = small_op(i);
            op.set = Some(BTreeMap::from([("title".to_owned(), json!(big))]));
            op
        })
        .collect();
    let total = ops.len();
    doc.enqueue_ops(ops);
    assert!(doc.pending_len() > 1, "400 x 12 KB must not be one frame");
    for batch in &doc.pending {
        let units: usize = batch.ops.iter().map(op_utf16_units).sum();
        assert!(units <= MAX_BATCH_UTF16_UNITS, "batch of {units} units");
    }
    // Order is preserved across chunks.
    let ids: Vec<String> = doc
        .pending
        .iter()
        .flat_map(|b| b.ops.iter().map(|op| op.id.clone()))
        .collect();
    assert_eq!(ids.len(), total);
    assert_eq!(ids[0], "c-0");
    assert_eq!(ids[total - 1], "c-399");
}

#[test]
fn oversize_ops_are_refused_before_the_queue() {
    let mut doc = doc_with_spaces("dev-a");
    let before = doc.pending_len();
    let mut op = small_op(0);
    op.set = Some(BTreeMap::from([(
        "title".to_owned(),
        json!("y".repeat(16_000)),
    )]));
    assert!(doc.write_ops(vec![small_op(1), op]).is_err());
    assert_eq!(
        doc.pending_len(),
        before,
        "nothing enqueued, not even the small op"
    );

    // 8,000 quote characters are within the visible limit but serialize
    // to 16,000+ units: the op gate catches what the field check cannot.
    let err = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            title: "Big".into(),
            description: Some("\"".repeat(8_000)),
            status: None,
            operation_id: "op-big".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::LimitExceeded);
    assert!(
        err.message.contains("Description is too long to save")
            && err.message.contains("units after encoding"),
        "field-level message, got: {}",
        err.message
    );
    assert_eq!(doc.pending_len(), before);
    assert!(snap(&doc).items.is_empty());
}

// ── acceptance 1: three-level outline ───────────────────────────────────────

#[test]
fn three_level_create_rename_archive_move_reorder() {
    let mut doc = doc_with_spaces("dev-a");
    let a = create(&mut doc, "op-a", None, "A");
    let b = create(&mut doc, "op-b", None, "B");
    let a1 = create(&mut doc, "op-a1", Some(&a), "A1");
    let a2 = create(&mut doc, "op-a2", Some(&a), "A2");
    let a1x = create(&mut doc, "op-a1x", Some(&a1), "A1x");
    let first = create_in(
        &mut doc,
        SPACE,
        "op-a0",
        Some(&a),
        WorkOutlinePlacement::First,
        "A0",
    );
    let mid = create_in(&mut doc, SPACE, "op-mid", Some(&a), after(&a1), "A1.5");
    let s = snap(&doc);
    assert_eq!(titles(&s, None), ["A", "B"]);
    assert_eq!(titles(&s, Some(&a)), ["A0", "A1", "A1.5", "A2"]);
    assert_eq!(titles(&s, Some(&a1)), ["A1x"]);
    assert_eq!(s.item(&a1x).unwrap().depth, 2);
    assert!(s.repairs.is_empty(), "{:?}", s.repairs);
    // Pre-order with depth.
    let order: Vec<&str> = s.items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(order, ["A", "A0", "A1", "A1x", "A1.5", "A2", "B"]);

    // Rename.
    apply(
        &mut doc,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: a1.clone(),
            changes: mast_proto::WorkItemChanges {
                title: Some("A-one".into()),
                status: Some(WorkItemStatus::Active),
                ..Default::default()
            },
            operation_id: "op-rename".into(),
        }),
    )
    .unwrap();
    // Reorder within A, then move a level-3 item to level 1.
    move_to(&mut doc, "op-r", &a2, Some(&a), before(&first)).unwrap();
    move_to(&mut doc, "op-m", &a1x, None, before(&a)).unwrap();
    let s = snap(&doc);
    assert_eq!(titles(&s, None), ["A1x", "A", "B"]);
    assert_eq!(titles(&s, Some(&a)), ["A2", "A0", "A-one", "A1.5"]);
    assert_eq!(s.item(&a1).unwrap().status, WorkItemStatus::Active);
    assert_eq!(s.item(&a).unwrap().rollup.active_count, 1);
    assert_eq!(s.item(&a).unwrap().rollup.pending_count, 4);
    // Group P: every placement op wrote both fields with one clock.
    let row = doc.overlay_row(KIND_WORK_ITEMS, &a1x).unwrap();
    assert_eq!(row.clocks["parentWorkItemId"], row.clocks["orderKey"]);
    // Immutable fields were written once, by the create.
    let created = doc.overlay_row(KIND_WORK_ITEMS, &a1).unwrap();
    assert!(created.clocks["spaceId"] < created.clocks["title"]);

    // Archive with cascade, then restore.
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: a.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "op-arch".into(),
        }),
    )
    .unwrap();
    let s = snap(&doc);
    assert_eq!(
        s.item(&a).unwrap().archive_cause,
        Some(WorkItemArchiveCause::SelfArchived)
    );
    for id in [&first, &a1, &a2, &mid] {
        assert_eq!(
            s.item(id).unwrap().archive_cause,
            Some(WorkItemArchiveCause::Cascade)
        );
    }
    assert!(s.item(&b).unwrap().archived_at.is_none());
    assert_eq!(
        s.item(&a).unwrap().rollup.pending_count,
        0,
        "archived not counted"
    );
    apply(
        &mut doc,
        WorkOutlineMutation::RestoreWorkItem(RestoreWorkItemParams {
            work_item_id: a.clone(),
            operation_id: "op-restore".into(),
        }),
    )
    .unwrap();
    let s = snap(&doc);
    assert!(s.items.iter().all(|i| i.archived_at.is_none()));
}

#[test]
fn archive_moving_descendants_and_restore_respects_self_archived() {
    let mut doc = doc_with_spaces("dev-a");
    let a = create(&mut doc, "op-a", None, "A");
    let dest = create(&mut doc, "op-d", None, "D");
    let d0 = create(&mut doc, "op-d0", Some(&dest), "D0");
    let c1 = create(&mut doc, "op-c1", Some(&a), "C1");
    let c2 = create(&mut doc, "op-c2", Some(&a), "C2");
    let g = create(&mut doc, "op-g", Some(&c1), "G");
    // Moving under the archived item's own descendant is a cycle.
    let err = apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: a.clone(),
            descendant_archive_policy: DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: Some(g.clone()),
            },
            operation_id: "op-bad".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::Cycle);
    let pending = doc.pending_len();
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: a.clone(),
            descendant_archive_policy: DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: Some(dest.clone()),
            },
            operation_id: "op-arch".into(),
        }),
    )
    .unwrap();
    assert_eq!(
        doc.pending_len(),
        pending + 1,
        "one logical mutation, one batch"
    );
    let s = snap(&doc);
    assert_eq!(titles(&s, Some(&dest)), ["D0", "C1", "C2"]);
    assert!(s.item(&a).unwrap().child_ids.is_empty());
    assert!(s.item(&c1).unwrap().archived_at.is_none());
    assert_eq!(
        s.item(&g).unwrap().parent_work_item_id.as_deref(),
        Some(c1.as_str())
    );
    let _ = d0;

    // Self-archived descendants stay archived when an ancestor is restored.
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: c1.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "op-c1".into(),
        }),
    )
    .unwrap();
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: dest.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "op-dest".into(),
        }),
    )
    .unwrap();
    apply(
        &mut doc,
        WorkOutlineMutation::RestoreWorkItem(RestoreWorkItemParams {
            work_item_id: dest.clone(),
            operation_id: "op-rd".into(),
        }),
    )
    .unwrap();
    let s = snap(&doc);
    assert!(s.item(&dest).unwrap().archived_at.is_none());
    assert!(s.item(&c2).unwrap().archived_at.is_none());
    assert_eq!(
        s.item(&c1).unwrap().archive_cause,
        Some(WorkItemArchiveCause::SelfArchived)
    );
    assert!(
        s.item(&g).unwrap().archived_at.is_some(),
        "under a self-archived item"
    );
    // Creating under an archived parent is refused.
    let err = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: Some(c1.clone()),
            placement: WorkOutlinePlacement::Last,
            title: "x".into(),
            description: None,
            status: None,
            operation_id: "op-x".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::Archived);
}

#[test]
fn move_validation_cross_space_self_and_descendant() {
    let mut doc = doc_with_spaces("dev-a");
    let a = create(&mut doc, "op-a", None, "A");
    let b = create(&mut doc, "op-b", Some(&a), "B");
    let other = create_in(
        &mut doc,
        OTHER_SPACE,
        "op-o",
        None,
        WorkOutlinePlacement::Last,
        "O",
    );
    let code = |r: Outcome<WorkOutlineMutateReply>| r.unwrap_err().code;
    assert_eq!(
        code(move_to(
            &mut doc,
            "m1",
            &a,
            Some(&b),
            WorkOutlinePlacement::Last
        )),
        WorkOutlineErrorCode::Cycle
    );
    assert_eq!(
        code(move_to(
            &mut doc,
            "m2",
            &a,
            Some(&a),
            WorkOutlinePlacement::Last
        )),
        WorkOutlineErrorCode::Cycle
    );
    assert_eq!(
        code(move_to(
            &mut doc,
            "m3",
            &a,
            Some(&other),
            WorkOutlinePlacement::Last
        )),
        WorkOutlineErrorCode::CrossSpace
    );
    assert_eq!(
        code(move_to(&mut doc, "m4", &a, None, before("nope"))),
        WorkOutlineErrorCode::NotFound
    );
    assert_eq!(
        code(move_to(&mut doc, "m5", &a, None, before(&a))),
        WorkOutlineErrorCode::Invalid
    );
    // A no-op reorder writes nothing.
    let pending = doc.pending_len();
    move_to(&mut doc, "m6", &a, None, WorkOutlinePlacement::Last).unwrap();
    assert_eq!(doc.pending_len(), pending);
}

// ── placement: tie groups, cap, length overflow ─────────────────────────────

#[test]
fn tie_groups_are_rekeyed_in_the_same_batch() {
    let mut doc = doc_with_spaces("dev-a");
    // Three siblings with equal keys (two devices minted the same key).
    let rows = vec![
        item_row("w1", SPACE, None, "0000000000010-000000-x", Some("a0")),
        item_row("w2", SPACE, None, "0000000000010-000000-x", Some("a0")),
        item_row("w3", SPACE, None, "0000000000010-000000-x", Some("a0")),
        item_row("w4", SPACE, None, "0000000000010-000000-x", Some("a2")),
    ];
    let _ = doc.apply_rows(1, rows);
    let pending = doc.pending_len();
    let new = create_in(&mut doc, SPACE, "op-n", None, after("w1"), "N");
    assert_eq!(doc.pending_len(), pending + 1);
    let s = snap(&doc);
    assert_eq!(s.root_work_item_ids, ["w1", new.as_str(), "w2", "w3", "w4"]);
    let keys: Vec<&str> = s.items.iter().map(|i| i.order_key.as_str()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        5,
        "all keys distinct after re-keying: {keys:?}"
    );
    assert_eq!(
        doc.overlay_row(KIND_WORK_ITEMS, "w4").unwrap().fields["orderKey"],
        json!("a2")
    );
}

#[test]
fn invalid_keys_sort_last_and_length_overflow_rebalances() {
    // `place` alone: a gap between two keys at the 64-char cap.
    let lo = format!("a0{}", "0".repeat(61)) + "1";
    let hi = format!("a0{}", "0".repeat(61)) + "2";
    assert_eq!(lo.len(), 64);
    let siblings = vec![
        ("x".to_owned(), Some(lo.clone())),
        ("y".to_owned(), Some(hi.clone())),
    ];
    let placed = place(&siblings, 1, 1).unwrap();
    assert_eq!(placed.rekeys.len(), 2, "whole group rebalanced");
    let x = &placed.rekeys[0].1;
    let y = &placed.rekeys[1].1;
    assert!(x < &placed.keys[0] && placed.keys[0] < *y);
    assert!(x.len() <= 3 && y.len() <= 3);

    let mut doc = doc_with_spaces("dev-a");
    let rows = vec![
        item_row("w1", SPACE, None, "0000000000010-000000-x", Some("a0")),
        item_row(
            "w2",
            SPACE,
            None,
            "0000000000010-000000-x",
            Some("not valid!"),
        ),
        item_row("w0", SPACE, None, "0000000000010-000000-x", None),
    ];
    let _ = doc.apply_rows(1, rows);
    let s = snap(&doc);
    assert_eq!(s.root_work_item_ids, ["w1", "w0", "w2"]);
    assert!(s.repairs.contains(&WorkOutlineRepair::BadOrderKey {
        work_item_id: "w2".into()
    }));
    apply(
        &mut doc,
        WorkOutlineMutation::RepairWorkOutline(RepairWorkOutlineParams {
            space_id: SPACE.into(),
            operation_id: "op-rep".into(),
        }),
    )
    .unwrap();
    let repaired = snap(&doc);
    assert_eq!(repaired.root_work_item_ids, s.root_work_item_ids);
    assert!(repaired.repairs.is_empty(), "{:?}", repaired.repairs);
}

#[test]
fn children_cap_is_enforced() {
    let mut doc = doc_with_spaces("dev-a");
    let keys = order_keys_between(None, None, limits::MAX_CHILDREN_PER_PARENT).unwrap();
    let rows: Vec<RegistryRow> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            item_row(
                &format!("w{i:05}"),
                SPACE,
                None,
                "0000000000010-000000-x",
                Some(k),
            )
        })
        .collect();
    let _ = doc.apply_rows(1, rows);
    let err = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: None,
            placement: WorkOutlinePlacement::First,
            title: "one too many".into(),
            description: None,
            status: None,
            operation_id: "op-cap".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::LimitExceeded);
}

// ── idempotency ─────────────────────────────────────────────────────────────

#[test]
fn retries_with_the_same_operation_id_never_duplicate() {
    let mut doc = doc_with_spaces("dev-a");
    let params = CreateWorkItemParams {
        space_id: SPACE.into(),
        parent_work_item_id: None,
        placement: WorkOutlinePlacement::Last,
        title: "Once".into(),
        description: Some("d".into()),
        status: None,
        operation_id: "op-retry".into(),
    };
    let first = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItem(params.clone()),
    )
    .unwrap();
    let pending = doc.pending_len();
    let second = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItem(params.clone()),
    )
    .unwrap();
    assert_eq!(first, second);
    assert_eq!(
        first.work_item_id.as_deref(),
        Some(derive_id("op-retry", roles::ITEM).as_str())
    );
    assert_eq!(doc.pending_len(), pending, "replay writes nothing");
    assert_eq!(snap(&doc).items.len(), 1);
    // Same operation id, different inputs.
    let mut other = params;
    other.title = "Twice".into();
    let err = apply(&mut doc, WorkOutlineMutation::CreateWorkItem(other)).unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::OperationConflict);

    // References and assignments are keyed so a replay converges too.
    let item = first.work_item_id.unwrap();
    let reference = CreateWorkItemExternalReferenceParams {
        work_item_id: item.clone(),
        provider: "github".into(),
        reference_type: "pullRequest".into(),
        external_id: "acme/repo#1".into(),
        url: "https://github.com/acme/repo/pull/1".into(),
        cached_title: Some("Fix".into()),
        operation_id: "op-ref".into(),
    };
    for _ in 0..2 {
        apply(
            &mut doc,
            WorkOutlineMutation::CreateWorkItemExternalReference(reference.clone()),
        )
        .unwrap();
    }
    let s = snap(&doc);
    assert_eq!(s.external_references.len(), 1);
    assert_eq!(s.item(&item).unwrap().external_reference_ids.len(), 1);
    // Remove, then an explicit re-add revives the content-derived row.
    let id = s.external_references[0].id.clone();
    apply(
        &mut doc,
        WorkOutlineMutation::RemoveWorkItemExternalReference(
            RemoveWorkItemExternalReferenceParams {
                reference_id: id.clone(),
                operation_id: "op-rm".into(),
            },
        ),
    )
    .unwrap();
    assert!(snap(&doc).external_references.is_empty());
    let mut readd = reference;
    readd.operation_id = "op-readd".into();
    apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItemExternalReference(readd),
    )
    .unwrap();
    assert_eq!(snap(&doc).external_references.len(), 1);
}

// ── acceptance 3 and 4: Chats ───────────────────────────────────────────────

fn space_chat(id: &str, space_id: Option<&str>) -> mast_proto::Chat {
    let mut chat = chat(id, "dev-a");
    chat.space_id = space_id.map(str::to_owned);
    chat.parent_chat_id = None;
    chat
}

fn assign(
    doc: &mut RegistryDoc,
    op: &str,
    chat_id: &str,
    item: &str,
) -> Outcome<WorkOutlineMutateReply> {
    apply(
        doc,
        WorkOutlineMutation::AssignChatToWorkItem(AssignChatToWorkItemParams {
            chat_id: chat_id.into(),
            work_item_id: item.into(),
            operation_id: op.into(),
        }),
    )
}

fn unassign(doc: &mut RegistryDoc, op: &str, chat_id: &str) {
    apply(
        doc,
        WorkOutlineMutation::RemoveChatFromWorkItem(RemoveChatFromWorkItemParams {
            chat_id: chat_id.into(),
            operation_id: op.into(),
        }),
    )
    .unwrap();
}

#[test]
fn assignment_generation_survives_retries_and_changes_on_reassignment() {
    let mut doc = doc_with_spaces("dev-a");
    doc.upsert_chat(&space_chat("c1", Some(SPACE))).unwrap();
    let a = create(&mut doc, "gen-a", None, "A");
    let b = create(&mut doc, "gen-b", None, "B");
    let generation = |doc: &RegistryDoc| {
        snap(doc).assignments.into_iter().find(|a| a.chat_id == "c1").unwrap().generation.unwrap()
    };
    assign(&mut doc, "gen-first", "c1", &a).unwrap();
    let first = generation(&doc);
    assign(&mut doc, "gen-retry", "c1", &a).unwrap();
    assert_eq!(generation(&doc), first);
    assign(&mut doc, "gen-move", "c1", &b).unwrap();
    assert_ne!(generation(&doc), first);
    assign(&mut doc, "gen-return", "c1", &a).unwrap();
    assert_ne!(generation(&doc), first);
    unassign(&mut doc, "gen-remove", "c1");
    assign(&mut doc, "gen-again", "c1", &a).unwrap();
    assert_ne!(generation(&doc), first);
}

#[test]
fn assignment_generation_changes_on_return_from_another_device_and_survives_restart() {
    let mut first_device = doc_with_spaces("dev-a");
    first_device.upsert_chat(&space_chat("c1", Some(SPACE))).unwrap();
    let a = create(&mut first_device, "device-a", None, "A");
    let b = create(&mut first_device, "device-b", None, "B");
    assign(&mut first_device, "same-operation", "c1", &a).unwrap();
    let get_generation = |doc: &RegistryDoc| {
        snap(doc).assignments.into_iter().find(|a| a.chat_id == "c1").unwrap().generation.unwrap()
    };
    let old_generation = get_generation(&first_device);
    assign(&mut first_device, "retry", "c1", &a).unwrap();
    assert_eq!(get_generation(&first_device), old_generation);
    assign(&mut first_device, "move", "c1", &b).unwrap();
    let mut second_device = RegistryDoc::new("dev-b");
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut first_device, &mut second_device]);
    // The same operation ID may be reused on another device.
    assign(&mut second_device, "same-operation", "c1", &a).unwrap();
    let current = get_generation(&second_device);
    assert_ne!(current, old_generation);
    let restarted = RegistryDoc::from_bytes(&second_device.to_bytes().unwrap(), "dev-b").unwrap();
    assert_eq!(get_generation(&restarted), current);
}

#[test]
fn legacy_assignment_update_does_not_reuse_an_old_generation() {
    let mut doc = doc_with_spaces("dev-a");
    doc.upsert_chat(&space_chat("c1", Some(SPACE))).unwrap();
    let a = create(&mut doc, "legacy-a", None, "A");
    let b = create(&mut doc, "legacy-b", None, "B");
    assign(&mut doc, "legacy-first", "c1", &a).unwrap();
    assert!(snap(&doc).assignments[0].generation.is_some());
    // An older writer changes the assignment group without the new field.
    doc.commit(vec![Planned::update(
        KIND_WORK_ITEM_CHAT_ASSIGNMENTS,
        "c1",
        BTreeMap::from([
            ("workItemId".to_owned(), json!(b)),
            ("assignedAt".to_owned(), json!(1234)),
        ]),
    )], &[]).unwrap();
    assert_eq!(snap(&doc).assignments[0].generation, None);
}

#[test]
fn association_never_touches_chat_space_or_session_rows() {
    let mut doc = doc_with_spaces("dev-a");
    doc.upsert_chat(&space_chat("c1", Some(SPACE))).unwrap();
    doc.upsert_chat(&space_chat("c2", Some(SPACE))).unwrap();
    let a = create(&mut doc, "op-a", None, "A");
    let b = create(&mut doc, "op-b", None, "B");
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut doc]);
    let chat_rows = |doc: &RegistryDoc| {
        ["c1", "c2"]
            .map(|id| serde_json::to_vec(&doc.overlay_row(KIND_CHATS, id).unwrap()).unwrap())
    };
    let space_row = |doc: &RegistryDoc| {
        serde_json::to_vec(&doc.overlay_row(KIND_SPACES, SPACE).unwrap()).unwrap()
    };
    let chats_before = chat_rows(&doc);
    let space_before = space_row(&doc);

    assign(&mut doc, "o1", "c1", &a).unwrap();
    assign(&mut doc, "o2", "c2", &a).unwrap();
    let s = snap(&doc);
    assert_eq!(s.item(&a).unwrap().chat_ids, ["c1", "c2"]);
    assert_eq!(s.item(&a).unwrap().rollup.associated_chat_count, 2);
    assert!(s.unassociated_chat_ids.is_empty());
    assign(&mut doc, "o3", "c1", &b).unwrap(); // move
    unassign(&mut doc, "o4", "c2");
    let s = snap(&doc);
    assert_eq!(s.item(&b).unwrap().chat_ids, ["c1"]);
    assert!(s.item(&a).unwrap().chat_ids.is_empty());
    assert_eq!(s.unassociated_chat_ids, ["c2"]);
    let c2 = s.assignments.iter().find(|a| a.chat_id == "c2").unwrap();
    assert_eq!(c2.state, WorkItemChatAssignmentState::Unassociated);
    assert_eq!(c2.work_item_id, None);

    assert_eq!(
        pending_kinds(&doc),
        BTreeSet::from([KIND_WORK_ITEM_CHAT_ASSIGNMENTS.to_owned()])
    );
    server_round(&mut server, &mut seq, &mut [&mut doc]);
    assert_eq!(chat_rows(&doc), chats_before, "chat rows byte-identical");
    assert_eq!(space_row(&doc), space_before);
    // Group S was written as one op each time.
    let row = doc
        .overlay_row(KIND_WORK_ITEM_CHAT_ASSIGNMENTS, "c1")
        .unwrap();
    assert_eq!(row.clocks["workItemId"], row.clocks["assignedAt"]);
    assert_eq!(row.clocks["workItemId"], row.clocks["assignedBy"]);
    assert!(
        row.clocks["spaceId"] < row.clocks["workItemId"],
        "spaceId written once"
    );
}

#[test]
fn projectless_archived_child_dangling_and_cross_space_chats() {
    let mut doc = doc_with_spaces("dev-a");
    let a = create(&mut doc, "op-a", None, "A");
    let other = create_in(
        &mut doc,
        OTHER_SPACE,
        "op-o",
        None,
        WorkOutlinePlacement::Last,
        "O",
    );
    doc.upsert_chat(&space_chat("projectless", None)).unwrap();
    let mut archived = space_chat("archived", Some(SPACE));
    archived.archived = true;
    doc.upsert_chat(&archived).unwrap();
    let mut child = space_chat("child", Some(SPACE));
    child.parent_chat_id = Some("archived".into());
    doc.upsert_chat(&child).unwrap();
    doc.upsert_chat(&space_chat("gone", Some(SPACE))).unwrap();
    doc.upsert_chat(&space_chat("elsewhere", Some(OTHER_SPACE)))
        .unwrap();

    assert_eq!(
        assign(&mut doc, "p", "projectless", &a).unwrap_err().code,
        WorkOutlineErrorCode::Projectless
    );
    assert_eq!(
        assign(&mut doc, "x", "elsewhere", &a).unwrap_err().code,
        WorkOutlineErrorCode::CrossSpace
    );
    assert_eq!(
        assign(&mut doc, "n", "missing-chat", &a).unwrap_err().code,
        WorkOutlineErrorCode::NotFound
    );
    // Archived and child Chats are listed, never hidden.
    let s = snap(&doc);
    assert_eq!(s.unassociated_chat_ids, ["archived", "child", "gone"]);
    assign(&mut doc, "a1", "archived", &a).unwrap();
    assign(&mut doc, "a2", "gone", &a).unwrap();
    doc.delete_chat("gone").unwrap();
    let s = snap(&doc);
    // The deleted Chat stays inspectable as a dangling assignment.
    assert_eq!(s.item(&a).unwrap().chat_ids, ["archived", "gone"]);
    assert_eq!(s.item(&a).unwrap().rollup.associated_chat_count, 1);
    let gone = s.assignments.iter().find(|x| x.chat_id == "gone").unwrap();
    assert_eq!(gone.state, WorkItemChatAssignmentState::DanglingChat);
    assert!(s.repairs.contains(&WorkOutlineRepair::DanglingChat {
        chat_id: "gone".into()
    }));
    assert_eq!(s.unassociated_chat_ids, ["child"]);
    // Explicit removal of the dangling assignment.
    unassign(&mut doc, "rm", "gone");
    assert!(snap(&doc).item(&a).unwrap().chat_ids == ["archived"]);

    // An assignment written by a misbehaving peer is quarantined, not lost.
    let _ = doc.apply_rows(
        99,
        vec![
            RegistryRow {
                kind: KIND_WORK_ITEM_CHAT_ASSIGNMENTS.into(),
                id: "child".into(),
                seq: 99,
                deleted: false,
                del_hlc: None,
                fields: BTreeMap::from([
                    ("spaceId".to_owned(), json!(SPACE)),
                    ("workItemId".to_owned(), json!(other)),
                ]),
                clocks: BTreeMap::new(),
            },
            RegistryRow {
                kind: KIND_WORK_ITEM_CHAT_ASSIGNMENTS.into(),
                id: "elsewhere".into(),
                seq: 99,
                deleted: false,
                del_hlc: None,
                fields: BTreeMap::from([
                    ("spaceId".to_owned(), json!(SPACE)),
                    ("workItemId".to_owned(), json!(a)),
                ]),
                clocks: BTreeMap::new(),
            },
        ],
    );
    let s = snap(&doc);
    let state = |id: &str| {
        s.assignments
            .iter()
            .find(|x| x.chat_id == id)
            .unwrap()
            .state
    };
    assert_eq!(
        state("child"),
        WorkItemChatAssignmentState::DanglingWorkItem
    );
    assert_eq!(
        state("elsewhere"),
        WorkItemChatAssignmentState::SpaceMismatch
    );
    assert_eq!(s.unassociated_chat_ids, ["child"]);
    assert!(
        !s.item(&a)
            .unwrap()
            .chat_ids
            .contains(&"elsewhere".to_owned())
    );
}

// ── acceptance 2: offline concurrency through the room ──────────────────────

fn synced_pair() -> (
    RegistryDoc,
    RegistryDoc,
    HashMap<(String, String), RegistryRow>,
    u64,
) {
    let mut a = doc_with_spaces("dev-a");
    let mut b = RegistryDoc::new("dev-b");
    let mut server = HashMap::new();
    let mut seq = 0;
    for (op, title) in [("op-p", "P"), ("op-q", "Q"), ("op-r", "R")] {
        create(&mut a, op, None, title);
    }
    let p = derive_id("op-p", roles::ITEM);
    for (op, title) in [("op-x", "X"), ("op-y", "Y"), ("op-z", "Z")] {
        create(&mut a, op, Some(&p), title);
    }
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    assert_eq!(snap(&a), snap(&b));
    (a, b, server, seq)
}

fn assert_converged(a: &RegistryDoc, b: &RegistryDoc) -> WorkOutlineSnapshot {
    let (sa, sb) = (snap(a), snap(b));
    assert_eq!(sa, sb);
    assert_eq!(sa.items.len(), 6, "no item lost");
    for item in &sa.items {
        let row = a.overlay_row(KIND_WORK_ITEMS, &item.id).unwrap();
        assert_eq!(
            row.clocks.get("parentWorkItemId"),
            row.clocks.get("orderKey"),
            "whole placement, never torn"
        );
    }
    sa
}

#[test]
fn concurrent_move_vs_reorder_vs_move_converge() {
    let id = |op: &str| derive_id(op, roles::ITEM);
    // Move (A) vs reorder (B) of the same item.
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    move_to(
        &mut a,
        "m1",
        &id("op-x"),
        Some(&id("op-q")),
        WorkOutlinePlacement::Last,
    )
    .unwrap();
    move_to(
        &mut b,
        "m2",
        &id("op-x"),
        Some(&id("op-p")),
        after(&id("op-z")),
    )
    .unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let s = assert_converged(&a, &b);
    let x = s.item(&id("op-x")).unwrap();
    assert!(
        (x.parent_work_item_id == Some(id("op-q")))
            || (x.parent_work_item_id == Some(id("op-p"))
                && s.item(&id("op-p")).unwrap().child_ids.last() == Some(&id("op-x")))
    );

    // Move vs move to different parents.
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    move_to(
        &mut a,
        "m1",
        &id("op-y"),
        Some(&id("op-q")),
        WorkOutlinePlacement::First,
    )
    .unwrap();
    move_to(
        &mut b,
        "m2",
        &id("op-y"),
        Some(&id("op-r")),
        WorkOutlinePlacement::First,
    )
    .unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    assert_converged(&a, &b);

    // Crossing moves create a raw cycle: repaired deterministically.
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    move_to(
        &mut a,
        "m1",
        &id("op-q"),
        Some(&id("op-r")),
        WorkOutlinePlacement::Last,
    )
    .unwrap();
    move_to(
        &mut b,
        "m2",
        &id("op-r"),
        Some(&id("op-q")),
        WorkOutlinePlacement::Last,
    )
    .unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let s = assert_converged(&a, &b);
    assert!(s.repairs.iter().any(
        |r| matches!(r, WorkOutlineRepair::CycleBroken { members, .. } if members.len() == 2)
    ));
    // The next move under the cycle persists the repair in its own batch,
    // so the user's move is never undone by the derived repair.
    let pending = a.pending_len();
    move_to(
        &mut a,
        "m3",
        &id("op-x"),
        Some(&id("op-q")),
        WorkOutlinePlacement::Last,
    )
    .unwrap();
    assert_eq!(a.pending_len(), pending + 1);
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let s = assert_converged(&a, &b);
    assert!(
        !s.repairs
            .iter()
            .any(|r| matches!(r, WorkOutlineRepair::CycleBroken { .. }))
    );
    let x = s.item(&id("op-x")).unwrap();
    assert_eq!(x.parent_work_item_id, Some(id("op-q")));
}

fn tie_rekey_vs_offline_move(a_pushes_first: bool) {
    let mut a = doc_with_spaces("dev-a");
    let mut b = RegistryDoc::new("dev-b");
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    // Three tied roots and a destination, all with old clocks.
    let old = "0000000000010-000000-x";
    let rows = vec![
        item_row("s1", SPACE, None, old, Some("a0")),
        item_row("s2", SPACE, None, old, Some("a0")),
        item_row("s3", SPACE, None, old, Some("a0")),
        item_row("q", SPACE, None, old, Some("a5")),
    ];
    for row in &rows {
        server.insert((row.kind.clone(), row.id.clone()), row.clone());
    }
    let _ = a.apply_rows(seq, rows.clone());
    let _ = b.apply_rows(seq, rows);
    assert_eq!(snap(&a), snap(&b));

    // B moves s2 under q while offline; A inserts after s1, which re-keys the
    // tie group (including s2) from A's stale view of s2's parent.
    move_to(&mut b, "m-b", "s2", Some("q"), WorkOutlinePlacement::Last).unwrap();
    let new = create_in(&mut a, SPACE, "op-n", None, after("s1"), "N");
    let rekey = a
        .pending
        .iter()
        .flat_map(|batch| batch.ops.iter())
        .find(|op| op.id == "s2")
        .expect("s2 is re-keyed by A");
    assert_eq!(
        rekey
            .clocks
            .as_ref()
            .and_then(|c| c.get("parentWorkItemId")),
        Some(&old.to_owned()),
        "re-key keeps the sibling's parent at its existing clock"
    );

    // Pushes reach the server in either order; the move wins both ways.
    // (the server applies batches in slice order).
    if a_pushes_first {
        server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    } else {
        server_round(&mut server, &mut seq, &mut [&mut b, &mut a]);
    }
    let (sa, sb) = (snap(&a), snap(&b));
    assert_eq!(sa, sb, "identical materialization");
    assert_eq!(
        sa.item("s2").unwrap().parent_work_item_id.as_deref(),
        Some("q")
    );
    assert_eq!(titles(&sa, Some("q")), ["T s2"]);
    assert!(sa.root_work_item_ids.contains(&new));
    assert!(!sa.root_work_item_ids.iter().any(|id| id == "s2"));
}

#[test]
fn tie_rekey_never_reverts_a_concurrent_offline_move_of_the_sibling() {
    tie_rekey_vs_offline_move(true);
    tie_rekey_vs_offline_move(false);
}

#[test]
fn sweep_and_space_cascade_deletes_beat_a_fast_clock_writers_rows() {
    // B's wall clock runs an hour fast; its row carries a far-future clock.
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    b.clock.last_ms = Utc::now().timestamp_millis() + 3_600_000;
    let late = create(&mut b, "op-late", None, "Late");
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    // A (normal clock) deletes the Space: the cascade must observe the fast
    // clocks, or its tombstones would lose to B's rows.
    a.delete_space(SPACE).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    for doc in [&a, &b] {
        assert_eq!(doc.row_state(KIND_WORK_ITEMS, &late), RowState::Tombstoned);
    }

    // Same for the orphan sweep: a row from a fast device in a Space that is
    // already gone.
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    b.clock.last_ms = Utc::now().timestamp_millis() + 3_600_000;
    a.delete_space(SPACE).unwrap();
    let late = create(&mut b, "op-late2", None, "Late");
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    assert_eq!(a.row_state(KIND_WORK_ITEMS, &late), RowState::Live);
    assert_eq!(a.sweep_work_outline_orphans(), 1);
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    for doc in [&a, &b] {
        assert_eq!(doc.row_state(KIND_WORK_ITEMS, &late), RowState::Tombstoned);
    }
}

#[test]
fn sweep_with_nothing_to_do_writes_nothing() {
    let (mut a, _b, _server, _seq) = synced_pair();
    let pending = a.pending_len();
    assert_eq!(a.sweep_work_outline_orphans(), 0);
    assert_eq!(a.pending_len(), pending);
}

#[test]
fn slow_clock_device_edit_is_not_lost() {
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    let x = derive_id("op-x", roles::ITEM);
    // A's wall clock runs an hour fast.
    a.clock.last_ms = Utc::now().timestamp_millis() + 3_600_000;
    apply(
        &mut a,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: x.clone(),
            changes: mast_proto::WorkItemChanges {
                title: Some("from fast A".into()),
                ..Default::default()
            },
            operation_id: "u1".into(),
        }),
    )
    .unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    // B (normal clock) edits after seeing A's write: it must win.
    apply(
        &mut b,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: x.clone(),
            changes: mast_proto::WorkItemChanges {
                title: Some("later from B".into()),
                ..Default::default()
            },
            operation_id: "u2".into(),
        }),
    )
    .unwrap();
    move_to(&mut b, "m", &x, None, WorkOutlinePlacement::First).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let s = assert_converged(&a, &b);
    assert_eq!(s.item(&x).unwrap().title, "later from B");
    assert_eq!(s.root_work_item_ids.first(), Some(&x));
}

// ── acceptance 29: tombstones, restart, Space deletion ──────────────────────

#[test]
fn restart_round_trip_preserves_pending_outline_rows() {
    let mut doc = doc_with_spaces("dev-a");
    let a = create(&mut doc, "op-a", None, "A");
    create(&mut doc, "op-b", Some(&a), "B");
    doc.upsert_chat(&space_chat("c1", Some(SPACE))).unwrap();
    assign(&mut doc, "o", "c1", &a).unwrap();
    let before = snap(&doc);
    let restored = RegistryDoc::from_bytes(&doc.to_bytes().unwrap(), "dev-a").unwrap();
    assert_eq!(snap(&restored), before);
    assert_eq!(restored.pending_len(), doc.pending_len());
    // And after the pending ops are acked, again.
    let mut restored = restored;
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut restored]);
    let again = RegistryDoc::from_bytes(&restored.to_bytes().unwrap(), "dev-a").unwrap();
    assert_eq!(again.pending_len(), 0);
    assert_eq!(snap(&again), before);
}

#[test]
fn space_deletion_suppresses_cascades_and_sweeps_late_writes() {
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    a.upsert_chat(&space_chat("c1", Some(SPACE))).unwrap();
    let p = derive_id("op-p", roles::ITEM);
    assign(&mut a, "o", "c1", &p).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    // B, offline, keeps writing into the Space while A deletes it.
    create(&mut b, "op-late", Some(&p), "Late");
    a.delete_space(SPACE).unwrap();
    assert!(!snap(&a).available, "suppressed before the server sees it");
    assert!(snap(&a).items.is_empty());
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    for doc in [&a, &b] {
        let s = snap(doc);
        assert!(!s.available && s.items.is_empty() && s.assignments.is_empty());
    }
    // The cascade tombstoned what A knew; B's late row survives as an
    // orphan until the sweep (synced replicas only) removes it.
    assert_eq!(a.row_state(KIND_WORK_ITEMS, &p), RowState::Tombstoned);
    assert_eq!(
        a.row_state(KIND_WORK_ITEM_CHAT_ASSIGNMENTS, "c1"),
        RowState::Tombstoned
    );
    let late = derive_id("op-late", roles::ITEM);
    assert_eq!(a.row_state(KIND_WORK_ITEMS, &late), RowState::Live);
    assert_eq!(a.sweep_work_outline_orphans(), 1);
    assert_eq!(a.sweep_work_outline_orphans(), 0, "idempotent");
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    assert_eq!(b.row_state(KIND_WORK_ITEMS, &late), RowState::Tombstoned);
    // Writes into the deleted Space are refused.
    let err = apply(
        &mut b,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            title: "x".into(),
            description: None,
            status: None,
            operation_id: "op-dead".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::NotFound);
    // Recreating the folder mints a new Space id and adopts nothing.
    a.upsert_space(&space("sp-new", "dev-a", "/tmp/one"))
        .unwrap();
    let fresh = a.work_outline_snapshot("sp-new", true);
    assert!(fresh.available && fresh.items.is_empty());
    // The other Space was untouched.
    assert!(a.work_outline_snapshot(OTHER_SPACE, true).available);
}

#[test]
fn lenient_rows_degrade_fields_not_rows() {
    let mut doc = doc_with_spaces("dev-a");
    let mut row = item_row("w1", SPACE, None, "0000000000010-000000-x", Some("a0"));
    row.fields.insert("status".into(), json!("blocked"));
    row.fields.remove("title");
    row.fields.insert("archiveCause".into(), json!("merged"));
    row.fields.insert("archivedAt".into(), json!(5));
    row.fields.insert("futureField".into(), json!({"x": 1}));
    let mut bad = item_row("w2", SPACE, None, "0000000000010-000000-x", Some("a1"));
    bad.fields.insert("spaceId".into(), json!(42));
    let _ = doc.apply_rows(1, vec![row, bad]);
    let s = snap(&doc);
    let item = s.item("w1").unwrap();
    assert_eq!(item.status, WorkItemStatus::Pending);
    assert_eq!(item.title, "Untitled");
    assert_eq!(item.archive_cause, Some(WorkItemArchiveCause::SelfArchived));
    assert!(s.repairs.contains(&WorkOutlineRepair::UnknownStatus {
        work_item_id: "w1".into()
    }));
    assert!(s.repairs.contains(&WorkOutlineRepair::MissingTitle {
        work_item_id: "w1".into()
    }));
    assert!(s.repairs.contains(&WorkOutlineRepair::Malformed {
        row_kind: KIND_WORK_ITEMS.into(),
        id: "w2".into()
    }));
}

// ── materialization properties ──────────────────────────────────────────────

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn random_rows(rng: &mut Rng) -> Vec<RegistryRow> {
    let n = 1 + rng.below(24) as usize;
    let keys = ["a0", "a1", "a1", "a2", "Zz", "bad key", "a0V"];
    (0..n)
        .map(|i| {
            let space_id = if rng.below(6) == 0 {
                OTHER_SPACE
            } else {
                SPACE
            };
            let parent = match rng.below(10) {
                0 | 1 => None,
                2 => Some(format!("w{i}")), // self-parent
                3 => Some("missing".to_owned()),
                _ => Some(format!("w{}", rng.below(n as u64))),
            };
            let clock = encode_hlc(1 + rng.below(50) as i64, 0, "seed");
            let key = (rng.below(8) != 0).then(|| keys[rng.below(keys.len() as u64) as usize]);
            item_row(&format!("w{i}"), space_id, parent.as_deref(), &clock, key)
        })
        .collect()
}

fn inputs(rows: Vec<RegistryRow>) -> WorkOutlineInputs {
    WorkOutlineInputs {
        space_live: true,
        synced: true,
        items: rows,
        // The Space `doc_with_spaces` creates (the implicit execution root).
        execution_space: Some(mast_proto::work_outline_execution::ExecutionSpace {
            space_id: SPACE.into(),
            path: "/tmp/one".into(),
            device_id: "dev-a".into(),
        }),
        ..Default::default()
    }
}

fn check_tree_invariants(s: &WorkOutlineSnapshot, rows: &[RegistryRow]) {
    let in_space: BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.fields["spaceId"] == json!(SPACE))
        .map(|r| r.id.as_str())
        .collect();
    let listed: Vec<&str> = s.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(listed.len(), in_space.len(), "every item exactly once");
    assert_eq!(listed.iter().copied().collect::<BTreeSet<_>>(), in_space);
    for item in &s.items {
        // Acyclic: reach a root within n steps.
        let mut cur = item.parent_work_item_id.clone();
        let mut steps = 0;
        while let Some(p) = cur {
            assert!(in_space.contains(p.as_str()));
            steps += 1;
            assert!(steps <= s.items.len(), "effective cycle at {}", item.id);
            cur = s.item(&p).unwrap().parent_work_item_id.clone();
        }
        for child in &item.child_ids {
            assert_eq!(
                s.item(child).unwrap().parent_work_item_id.as_deref(),
                Some(item.id.as_str())
            );
        }
    }
    for repair in &s.repairs {
        if let WorkOutlineRepair::CycleBroken {
            victim, members, ..
        } = repair
        {
            let clock = |id: &str| {
                rows.iter()
                    .find(|r| r.id == id)
                    .and_then(|r| r.clocks.get("parentWorkItemId").cloned())
            };
            let best = members
                .iter()
                .max_by(|a, b| (clock(a), *a).cmp(&(clock(b), *b)))
                .unwrap();
            assert_eq!(best, victim);
        }
    }
}

#[test]
fn random_parent_graphs_materialize_to_a_valid_deterministic_tree() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..400 {
        let rows = random_rows(&mut rng);
        let s = materialize_work_outline(SPACE, &inputs(rows.clone()));
        check_tree_invariants(&s, &rows);
        // Determinism under input permutation.
        let mut shuffled = rows.clone();
        for i in (1..shuffled.len()).rev() {
            let j = rng.below(i as u64 + 1) as usize;
            shuffled.swap(i, j);
        }
        assert_eq!(materialize_work_outline(SPACE, &inputs(shuffled)), s);
        // Repair idempotence: persisting the derived repairs keeps the tree
        // and clears the cycle/key markers; repairing again writes nothing.
        let mut doc = doc_with_spaces("dev-a");
        let _ = doc.apply_rows(1, rows.clone());
        assert_eq!(snap(&doc), s);
        doc.repair_work_outline(&RepairWorkOutlineParams {
            space_id: SPACE.into(),
            operation_id: "op".into(),
        })
        .unwrap();
        let repaired = snap(&doc);
        for item in &s.items {
            let after = repaired.item(&item.id).unwrap();
            assert_eq!(after.parent_work_item_id, item.parent_work_item_id);
            assert_eq!(after.child_ids, item.child_ids);
        }
        assert_eq!(repaired.root_work_item_ids, s.root_work_item_ids);
        assert!(!repaired.repairs.iter().any(|r| matches!(
            r,
            WorkOutlineRepair::CycleBroken { .. } | WorkOutlineRepair::BadOrderKey { .. }
        )));
        let pending = doc.pending_len();
        doc.repair_work_outline(&RepairWorkOutlineParams {
            space_id: SPACE.into(),
            operation_id: "op2".into(),
        })
        .unwrap();
        assert_eq!(doc.pending_len(), pending);
    }
}

#[test]
fn cycle_shapes_self_two_nested_and_disjoint() {
    let c = |ms: i64| encode_hlc(ms, 0, "seed");
    let rows = vec![
        // self-parent
        item_row("s", SPACE, Some("s"), &c(5), Some("a0")),
        // 2-cycle: newest parent edge is on "u"
        item_row("t", SPACE, Some("u"), &c(3), Some("a1")),
        item_row("u", SPACE, Some("t"), &c(9), Some("a2")),
        // 3-cycle with a tail hanging into it
        item_row("x", SPACE, Some("y"), &c(4), Some("a3")),
        item_row("y", SPACE, Some("z"), &c(4), Some("a4")),
        item_row("z", SPACE, Some("x"), &c(4), Some("a5")),
        item_row("tail", SPACE, Some("y"), &c(99), Some("a6")),
        // cross-Space parent and a missing one
        item_row("o", OTHER_SPACE, None, &c(1), Some("a0")),
        item_row("k", SPACE, Some("o"), &c(1), Some("a7")),
        item_row("m", SPACE, Some("nope"), &c(1), Some("a8")),
    ];
    let s = materialize_work_outline(SPACE, &inputs(rows.clone()));
    check_tree_invariants(&s, &rows);
    let broken: Vec<(&str, usize)> = s
        .repairs
        .iter()
        .filter_map(|r| match r {
            WorkOutlineRepair::CycleBroken {
                victim, members, ..
            } => Some((victim.as_str(), members.len())),
            _ => None,
        })
        .collect();
    // Equal clocks tie-break on the greatest id.
    assert_eq!(broken, [("s", 1), ("u", 2), ("z", 3)]);
    assert!(s.repairs.contains(&WorkOutlineRepair::CrossSpaceParent {
        work_item_id: "k".into(),
        parent_work_item_id: "o".into()
    }));
    assert!(s.repairs.contains(&WorkOutlineRepair::ParentMissing {
        work_item_id: "m".into(),
        parent_work_item_id: "nope".into()
    }));
    assert_eq!(
        s.item("tail").unwrap().parent_work_item_id.as_deref(),
        Some("y")
    );
    assert_eq!(
        s.item("z").unwrap().raw_parent_work_item_id.as_deref(),
        Some("x")
    );
    assert_eq!(s.item("z").unwrap().parent_work_item_id, None);
}

#[test]
fn unavailable_space_suppresses_everything() {
    let rows = vec![item_row(
        "w",
        SPACE,
        None,
        "0000000000001-000000-x",
        Some("a0"),
    )];
    let mut input = inputs(rows);
    input.space_live = false;
    input.synced = false;
    assert_eq!(
        materialize_work_outline(SPACE, &input),
        WorkOutlineSnapshot::unavailable(SPACE, false, 0)
    );
}

// ── acceptance 30: large outline ────────────────────────────────────────────

#[test]
fn ten_thousand_items_materialize_within_budget() {
    let mut rng = Rng(42);
    let n = 10_000usize;
    let keys = order_keys_between(None, None, 64).unwrap();
    let mut ops: Vec<RowOp> = Vec::with_capacity(n + 2_000);
    for i in 0..n {
        let parent = (i > 50).then(|| format!("w{:05}", rng.below(i as u64)));
        let mut set = BTreeMap::from([
            ("spaceId".to_owned(), json!(SPACE)),
            ("title".to_owned(), json!(format!("Item {i}"))),
            ("orderKey".to_owned(), json!(keys[i % keys.len()])),
            ("parentWorkItemId".to_owned(), opt_value(parent.as_deref())),
        ]);
        set.insert("status".into(), json!("active"));
        ops.push(RowOp {
            kind: KIND_WORK_ITEMS.into(),
            id: format!("w{i:05}"),
            op: OpKind::Upsert,
            set: Some(set),
            hlc: encode_hlc(1, 0, "seed"),
            clocks: None,
        });
    }
    let mut doc = doc_with_spaces("dev-a");
    for i in 0..2_000 {
        doc.upsert_chat(&space_chat(&format!("c{i:04}"), Some(SPACE)))
            .unwrap();
        if i % 2 == 0 {
            ops.push(RowOp {
                kind: KIND_WORK_ITEM_CHAT_ASSIGNMENTS.into(),
                id: format!("c{i:04}"),
                op: OpKind::Upsert,
                set: Some(BTreeMap::from([
                    ("spaceId".to_owned(), json!(SPACE)),
                    ("workItemId".to_owned(), json!(format!("w{:05}", i * 3))),
                ])),
                hlc: encode_hlc(1, 0, "seed"),
                clocks: None,
            });
        }
    }
    // All of it still pending (offline): the overlay must stay linear.
    doc.write_ops(ops).unwrap();
    let started = std::time::Instant::now();
    let input = doc.work_outline_inputs(SPACE, true);
    let cloned = started.elapsed();
    let s = materialize_work_outline(SPACE, &input);
    let total = started.elapsed();
    assert_eq!(s.items.len(), n);
    assert_eq!(s.unassociated_chat_ids.len(), 1_000);
    let root_total: u32 = s
        .root_work_item_ids
        .iter()
        .map(|id| s.item(id).unwrap().rollup.active_count)
        .sum();
    assert_eq!(root_total as usize, n, "ancestor totals cover every item");
    // Generous for unoptimized test builds; release is far faster.
    assert!(
        total.as_secs_f64() < 5.0,
        "clone {cloned:?}, total {total:?}"
    );
    // A write against the large outline also stays bounded.
    let started = std::time::Instant::now();
    create(&mut doc, "op-big", Some("w00010"), "one more");
    assert!(
        started.elapsed().as_secs_f64() < 5.0,
        "{:?}",
        started.elapsed()
    );
}

// ── undo: the inverse operations the UI's action log sends ──────────────────

/// Undo is an ordinary mutation: the inverse of a move (`First` or `After`
/// the previous sibling, under the old parent) puts an item back exactly, a
/// field patch restores a title, and restore reverses a cascading archive.
/// See `ui::work_outline::action_log`.
#[test]
fn undo_inverse_operations_restore_order_parent_fields_and_archive() {
    let mut doc = doc_with_spaces("d1");
    let a = create(&mut doc, "op-a", None, "A");
    let b = create(&mut doc, "op-b", None, "B");
    let c = create(&mut doc, "op-c", None, "C");
    let base = titles(&snap(&doc), None);
    assert_eq!(base, ["A", "B", "C"]);

    // Reorder C to the front; the inverse is After(previous sibling = B).
    move_to(&mut doc, "op-1", &c, None, WorkOutlinePlacement::First).unwrap();
    assert_eq!(titles(&snap(&doc), None), ["C", "A", "B"]);
    move_to(&mut doc, "op-1-undo", &c, None, after(&b)).unwrap();
    assert_eq!(titles(&snap(&doc), None), base);

    // Reparent A's next sibling under A (last); the inverse names the old
    // parent and the previous sibling (A).
    move_to(&mut doc, "op-2", &b, Some(&a), WorkOutlinePlacement::Last).unwrap();
    assert_eq!(titles(&snap(&doc), Some(&a)), ["B"]);
    move_to(&mut doc, "op-2-undo", &b, None, after(&a)).unwrap();
    assert_eq!(titles(&snap(&doc), None), base);
    assert!(titles(&snap(&doc), Some(&a)).is_empty());

    // First child of a parent: the inverse placement is First.
    move_to(&mut doc, "op-3", &a, Some(&c), WorkOutlinePlacement::Last).unwrap();
    move_to(&mut doc, "op-3-undo", &a, None, WorkOutlinePlacement::First).unwrap();
    assert_eq!(titles(&snap(&doc), None), base);

    // A patch with the old values restores them, and nothing else changes.
    let before = snap(&doc).item(&b).unwrap().clone();
    for (op, title, status) in [
        ("op-4", "B2", WorkItemStatus::Completed),
        ("op-4-undo", "B", before.status),
    ] {
        apply(
            &mut doc,
            WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
                work_item_id: b.clone(),
                changes: mast_proto::WorkItemChanges {
                    title: Some(title.into()),
                    status: Some(status),
                    ..Default::default()
                },
                operation_id: op.into(),
            }),
        )
        .unwrap();
    }
    let after_undo = snap(&doc).item(&b).unwrap().clone();
    assert_eq!(after_undo.title, before.title);
    assert_eq!(after_undo.status, before.status);

    // Archive with descendants, then restore: the subtree is live again.
    let child = create(&mut doc, "op-child", Some(&a), "A child");
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: a.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "op-5".into(),
        }),
    )
    .unwrap();
    assert!(snap(&doc).item(&child).unwrap().archived_at.is_some());
    apply(
        &mut doc,
        WorkOutlineMutation::RestoreWorkItem(RestoreWorkItemParams {
            work_item_id: a.clone(),
            operation_id: "op-5-undo".into(),
        }),
    )
    .unwrap();
    let s = snap(&doc);
    assert!(s.items.iter().all(|i| i.archived_at.is_none()));
    assert_eq!(titles(&s, Some(&a)), ["A child"]);
}

fn doc_with_worktree_space(device: &str) -> RegistryDoc {
    let mut doc = RegistryDoc::new(device);
    for (id, path) in [
        (SPACE, "/tmp/repo"),
        (OTHER_SPACE, "/tmp/repo/.worktrees/x"),
    ] {
        doc.create_space_with_pr_prompts(
            &space(id, device, path),
            &mast_proto::PrReviewPrompts::neutral(),
        )
        .unwrap();
    }
    doc
}

#[test]
fn fold_space_moves_chats_and_deletes_the_emptied_space() {
    let mut doc = doc_with_worktree_space("dev-a");
    let mut moved = chat("chat-1", "dev-a");
    moved.space_id = Some(OTHER_SPACE.into());
    moved.cwd = Some("/tmp/repo/.worktrees/x".into());
    let mut stays = chat("chat-2", "dev-a");
    stays.space_id = Some(SPACE.into());
    doc.upsert_chat(&moved).unwrap();
    doc.upsert_chat(&stays).unwrap();

    assert_eq!(
        doc.fold_space(OTHER_SPACE, SPACE).unwrap(),
        Some(vec!["chat-1".to_string()])
    );
    let spaces: Vec<String> = doc
        .read_spaces()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(spaces, [SPACE]);
    let chat = doc.chat("chat-1").unwrap().expect("chat survives the fold");
    assert_eq!(chat.space_id.as_deref(), Some(SPACE));
    assert_eq!(chat.cwd.as_deref(), Some("/tmp/repo/.worktrees/x"));
    assert_eq!(doc.read_chats().unwrap().len(), 2);
}

#[test]
fn fold_space_refuses_peer_chats_outline_rows_and_missing_targets() {
    let mut doc = doc_with_worktree_space("dev-a");
    let mut peer = chat("chat-peer", "dev-b");
    peer.space_id = Some(OTHER_SPACE.into());
    doc.upsert_chat(&peer).unwrap();
    assert!(!doc.space_foldable(OTHER_SPACE).unwrap());
    assert_eq!(doc.fold_space(OTHER_SPACE, SPACE).unwrap(), None);

    let mut doc = doc_with_worktree_space("dev-a");
    create_in(
        &mut doc,
        OTHER_SPACE,
        "op-wt",
        None,
        WorkOutlinePlacement::Last,
        "Worktree item",
    );
    assert!(!doc.space_foldable(OTHER_SPACE).unwrap());
    assert_eq!(doc.fold_space(OTHER_SPACE, SPACE).unwrap(), None);

    // A live run's agent grant is pinned to its Space: wait for it to settle.
    let mut doc = doc_with_worktree_space("dev-a");
    let mut running = chat("chat-running", "dev-a");
    running.space_id = Some(OTHER_SPACE.into());
    doc.upsert_chat(&running).unwrap();
    for status in [SessionStatus::Working, SessionStatus::AwaitingInput] {
        doc.upsert_session(&Session {
            last_completed_turn: None,
            awaiting_input: None,
            last_turn_tool_failures: None,
            todo_progress: None,
            chat_id: "chat-running".into(),
            device_id: "dev-a".into(),
            status,
            started_at: None,
            updated_at: Utc::now(),
        })
        .unwrap();
        assert!(!doc.space_foldable(OTHER_SPACE).unwrap());
    }
    doc.upsert_session(&Session {
        last_completed_turn: None,
        awaiting_input: None,
        last_turn_tool_failures: None,
        todo_progress: None,
        chat_id: "chat-running".into(),
        device_id: "dev-a".into(),
        status: SessionStatus::Idle,
        started_at: None,
        updated_at: Utc::now(),
    })
    .unwrap();
    assert!(doc.space_foldable(OTHER_SPACE).unwrap());

    let mut doc = doc_with_worktree_space("dev-a");
    assert!(doc.space_foldable(OTHER_SPACE).unwrap());
    assert_eq!(doc.fold_space(OTHER_SPACE, "sp-missing").unwrap(), None);
    assert_eq!(doc.fold_space(OTHER_SPACE, OTHER_SPACE).unwrap(), None);
    assert_eq!(doc.read_spaces().unwrap().len(), 2);
}

#[test]
fn scheduled_run_lists_and_counts_exclude_superseded_and_retired_chats() {
    let mut doc = doc_with_spaces("dev-a");
    let item = create(&mut doc, "item", None, "Routine");
    for (id, at, retired) in [("older", 1, false), ("latest", 2, false), ("retired", 3, true)] {
        let mut chat = space_chat(id, Some(SPACE));
        chat.scheduled_task_info = Some(mast_proto::ScheduledChat {
            task_id: if retired { "retired" } else { "task" }.into(),
            task_name: "Routine".into(), run_id: id.into(), session_created_at: Some(at), retired,
            ..Default::default()
        });
        doc.upsert_chat(&chat).unwrap();
        assign(&mut doc, &format!("assign-{id}"), id, &item).unwrap();
    }
    let outline = snap(&doc);
    let view = outline.item(&item).unwrap();
    // Lookups still find every run's item; lists and counts skip the hidden ones.
    assert_eq!(view.chat_ids, ["latest", "older", "retired"]);
    assert_eq!(outline.hidden_run_chat_ids, ["older", "retired"]);
    assert_eq!(outline.listed_chat_ids(view).collect::<Vec<_>>(), ["latest"]);
    assert_eq!(view.rollup.associated_chat_count, 1);
    assert!(outline.unassociated_chat_ids.is_empty());
}

#[test]
fn unassigned_hidden_runs_stay_unassociated_but_are_marked() {
    let mut doc = doc_with_spaces("dev-a");
    for (id, at) in [("older", 1), ("latest", 2)] {
        let mut chat = space_chat(id, Some(SPACE));
        chat.scheduled_task_info = Some(mast_proto::ScheduledChat {
            task_id: "task".into(),
            task_name: "Routine".into(),
            run_id: id.into(),
            session_created_at: Some(at),
            ..Default::default()
        });
        doc.upsert_chat(&chat).unwrap();
    }
    let outline = snap(&doc);
    assert_eq!(outline.unassociated_chat_ids, ["latest", "older"]);
    assert!(outline.is_hidden_run("older") && !outline.is_hidden_run("latest"));
}

fn mark_project(doc: &mut RegistryDoc, op: &str, id: &str) {
    apply(
        doc,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: id.into(),
            changes: mast_proto::WorkItemChanges {
                is_project: Some(true),
                ..Default::default()
            },
            operation_id: op.into(),
        }),
    )
    .unwrap();
}

#[test]
fn final_handback_intent_replays_original_and_rejects_stale_assignment() {
    let mut doc = doc_with_spaces("dev-a");
    doc.upsert_chat(&space_chat("parent", Some(SPACE))).unwrap();
    let mut worker = space_chat("worker", Some(SPACE));
    worker.parent_chat_id = Some("parent".into());
    doc.upsert_chat(&worker).unwrap();
    let project = create(&mut doc, "handback-project", None, "Project");
    mark_project(&mut doc, "handback-mark-project", &project);
    let a = create(&mut doc, "handback-a", Some(&project), "A");
    let b = create(&mut doc, "handback-b", Some(&project), "B");
    assign(&mut doc, "handback-assign", "worker", &a).unwrap();
    let generation = snap(&doc).assignments.iter().find(|a| a.chat_id == "worker").unwrap().generation.clone().unwrap();
    assert!(doc.prepare_final_handback("wrong-space", "worker", OTHER_SPACE, &generation, "Done", false).is_err());
    let first = doc.prepare_final_handback("handback-op", "worker", SPACE, &generation, "Done", false).unwrap();
    assert_eq!(first.parent_chat_id, "parent");
    assert_eq!(first.project_id, project);
    doc.archive_final_handback(&first);
    assert!(doc.chat_archived_by_final_handback("worker"));
    assert!(doc.chat_retention_stamps()["worker"].archived_at.is_some());
    // A later manual archive owns the archived state even when its value
    // is unchanged. Imported work must not clear a manual decision.
    doc.set_chat_archived("worker", true).unwrap();
    assert!(!doc.chat_archived_by_final_handback("worker"));
    assert!(doc.final_handback_archived(&first));
    assign(&mut doc, "handback-move", "worker", &b).unwrap();
    assert_eq!(doc.prepare_final_handback("handback-op", "worker", SPACE, &generation, "Done", false).unwrap(), first);
    assert!(doc.prepare_final_handback("new-op", "worker", SPACE, &generation, "Done", false).is_err());
    assert!(doc.prepare_final_handback("handback-op", "worker", SPACE, &generation, "Changed", false).is_err());
    let mut restarted = RegistryDoc::from_bytes(&doc.to_bytes().unwrap(), "dev-a").unwrap();
    assert_eq!(restarted.prepare_final_handback("handback-op", "worker", SPACE, &generation, "Done", false).unwrap(), first);
}

fn queue(
    doc: &mut RegistryDoc,
    op: &str,
    id: &str,
    change: mast_proto::QueueChange,
) -> Outcome<WorkOutlineMutateReply> {
    apply(
        doc,
        WorkOutlineMutation::SetQueuePosition(mast_proto::SetQueuePositionParams {
            work_item_id: id.into(),
            change,
            operation_id: op.into(),
        }),
    )
}

fn queue_place(placement: WorkOutlinePlacement) -> mast_proto::QueueChange {
    mast_proto::QueueChange::Place { placement }
}

fn no_pull_requests(_url: &str) -> mast_proto::work_status::PullRequestFacts {
    Default::default()
}

fn no_chats(_chat: &str) -> mast_proto::ChatActivity {
    Default::default()
}

fn queue_titles(doc: &RegistryDoc, project: &str) -> Vec<String> {
    snap(doc)
        .work_queue(project, 0, &no_pull_requests, &no_chats)
        .into_iter()
        .map(|entry| entry.title)
        .collect()
}

#[test]
fn queue_orders_adds_moves_and_removes_without_moving_items() {
    let mut doc = doc_with_spaces("dev-a");
    let project = create(&mut doc, "q-project", None, "Project");
    mark_project(&mut doc, "q-mark", &project);
    let a = create(&mut doc, "q-a", Some(&project), "A");
    let b = create(&mut doc, "q-b", Some(&project), "B");
    let c = create(&mut doc, "q-c", Some(&project), "C");

    queue(&mut doc, "q1", &a, queue_place(WorkOutlinePlacement::Last)).unwrap();
    queue(&mut doc, "q2", &b, queue_place(WorkOutlinePlacement::Last)).unwrap();
    queue(&mut doc, "q3", &c, queue_place(before(&a))).unwrap();
    assert_eq!(queue_titles(&doc, &project), ["C", "A", "B"]);

    queue(&mut doc, "q4", &c, queue_place(after(&b))).unwrap();
    assert_eq!(queue_titles(&doc, &project), ["A", "B", "C"]);

    // Queuing leaves the tree alone.
    assert_eq!(snap(&doc).item(&project).unwrap().child_ids, [a.clone(), b.clone(), c.clone()]);

    queue(&mut doc, "q5", &b, mast_proto::QueueChange::Remove).unwrap();
    assert_eq!(queue_titles(&doc, &project), ["A", "C"]);
    assert_eq!(snap(&doc).item(&b).unwrap().queue_order_key, None);
    // Removing an item that is not queued is a no-op.
    queue(&mut doc, "q6", &b, mast_proto::QueueChange::Remove).unwrap();
}

#[test]
fn queue_rejects_items_outside_a_project_and_foreign_anchors() {
    let mut doc = doc_with_spaces("dev-a");
    let loose = create(&mut doc, "q-loose", None, "Loose");
    assert!(queue(&mut doc, "q-no-project", &loose, queue_place(WorkOutlinePlacement::Last)).is_err());

    let p1 = create(&mut doc, "q-p1", None, "P1");
    let p2 = create(&mut doc, "q-p2", None, "P2");
    mark_project(&mut doc, "q-m1", &p1);
    mark_project(&mut doc, "q-m2", &p2);
    let a = create(&mut doc, "q-a1", Some(&p1), "A");
    let x = create(&mut doc, "q-x2", Some(&p2), "X");
    queue(&mut doc, "q-in-p1", &a, queue_place(WorkOutlinePlacement::Last)).unwrap();
    assert!(queue(&mut doc, "q-foreign", &x, queue_place(before(&a))).is_err());
    assert!(queue(&mut doc, "q-self", &a, queue_place(before(&a))).is_err());
    assert!(queue(&mut doc, "q-project", &p1, queue_place(WorkOutlinePlacement::Last)).is_err());
    assert_eq!(queue_titles(&doc, &p2), Vec::<String>::new());
}

#[test]
fn queue_prerequisites_and_completion_are_validated() {
    use mast_proto::{QueueChange, QueueCompletion};
    let set = |ids: &[&String]| QueueChange::SetPrerequisites {
        work_item_ids: ids.iter().map(|id| (*id).clone()).collect(),
    };
    let mut doc = doc_with_spaces("dev-a");
    let project = create(&mut doc, "qp-project", None, "Project");
    mark_project(&mut doc, "qp-mark", &project);
    let a = create(&mut doc, "qp-a", Some(&project), "A");
    let b = create(&mut doc, "qp-b", Some(&project), "B");
    let c = create(&mut doc, "qp-c", Some(&project), "C");
    let other = create(&mut doc, "qp-other", None, "Other");
    mark_project(&mut doc, "qp-mark-other", &other);
    let foreign = create(&mut doc, "qp-foreign", Some(&other), "Foreign");
    queue(&mut doc, "qp1", &a, queue_place(WorkOutlinePlacement::Last)).unwrap();
    queue(&mut doc, "qp2", &b, queue_place(WorkOutlinePlacement::Last)).unwrap();

    queue(&mut doc, "qp3", &b, set(&[&a])).unwrap();
    assert_eq!(snap(&doc).item(&b).unwrap().queue_prerequisites, [a.clone()]);
    // The same set again, and a duplicate id, change nothing.
    queue(&mut doc, "qp4", &b, set(&[&a, &a])).unwrap();
    assert_eq!(snap(&doc).item(&b).unwrap().queue_prerequisites, [a.clone()]);
    // A prerequisite need not be queued, but it must be an item of the project.
    queue(&mut doc, "qp5", &b, set(&[&a, &c])).unwrap();
    assert_eq!(snap(&doc).item(&b).unwrap().queue_prerequisites, [a.clone(), c.clone()]);
    assert!(queue(&mut doc, "qp-self", &b, set(&[&b])).is_err());
    assert!(queue(&mut doc, "qp-foreign", &b, set(&[&foreign])).is_err());
    assert!(queue(&mut doc, "qp-project", &b, set(&[&project])).is_err());
    assert!(queue(&mut doc, "qp-gone", &b, set(&[&"no-such-item".to_owned()])).is_err());
    // `a` waiting on `b`, which waits on `a`, could never be offered.
    assert!(queue(&mut doc, "qp-cycle", &a, set(&[&b])).is_err());
    assert_eq!(snap(&doc).item(&a).unwrap().queue_prerequisites, Vec::<String>::new());
    // Too many.
    let many: Vec<String> = (0..9).map(|i| create(&mut doc, &format!("qp-many-{i}"), Some(&project), "M")).collect();
    assert!(queue(&mut doc, "qp-many", &a, set(&many.iter().collect::<Vec<_>>())).is_err());
    // An empty list clears them.
    queue(&mut doc, "qp6", &b, set(&[])).unwrap();
    assert!(snap(&doc).item(&b).unwrap().queue_prerequisites.is_empty());

    assert_eq!(snap(&doc).item(&a).unwrap().queue_completion, QueueCompletion::Reported);
    queue(&mut doc, "qp7", &a, QueueChange::SetCompletion { completion: QueueCompletion::Landed }).unwrap();
    assert_eq!(snap(&doc).item(&a).unwrap().queue_completion, QueueCompletion::Landed);
    queue(&mut doc, "qp8", &a, QueueChange::SetCompletion { completion: QueueCompletion::Landed }).unwrap();
    queue(&mut doc, "qp9", &a, QueueChange::SetCompletion { completion: QueueCompletion::Reported }).unwrap();
    assert_eq!(snap(&doc).item(&a).unwrap().queue_completion, QueueCompletion::Reported);
    // A project itself cannot take a completion.
    assert!(queue(&mut doc, "qp-loose", &other, QueueChange::SetCompletion { completion: QueueCompletion::Landed }).is_err());
}

#[test]
fn queue_fields_survive_unrelated_updates_and_completion_records_who_did_it() {
    use mast_proto::{QueueChange, QueueCompletion};
    let mut doc = doc_with_spaces("dev-a");
    let project = create(&mut doc, "qc-project", None, "Project");
    mark_project(&mut doc, "qc-mark", &project);
    let a = create(&mut doc, "qc-a", Some(&project), "A");
    let b = create(&mut doc, "qc-b", Some(&project), "B");
    doc.upsert_chat(&space_chat("chat-agent", Some(SPACE))).unwrap();
    assign(&mut doc, "qc-assign", "chat-agent", &a).unwrap();
    queue(&mut doc, "qc1", &b, queue_place(WorkOutlinePlacement::Last)).unwrap();
    queue(&mut doc, "qc2", &b, QueueChange::SetPrerequisites { work_item_ids: vec![a.clone()] }).unwrap();
    queue(&mut doc, "qc3", &b, QueueChange::SetCompletion { completion: QueueCompletion::Landed }).unwrap();
    let update = |doc: &mut RegistryDoc, op: &str, id: &str, changes: mast_proto::WorkItemChanges, actor_kind| {
        let chat_id = (actor_kind == WorkOutlineActorKind::Agent).then(|| "chat-agent".to_owned());
        let actor = WorkOutlineActor { kind: actor_kind, device_id: "dev-a".into(), chat_id, message_id: None };
        doc.apply_work_outline_mutation(
            &WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
                work_item_id: id.into(),
                changes,
                operation_id: op.into(),
            }),
            &actor,
            1_700_000_000_000,
        )
        .unwrap();
    };
    // A rename, which a peer that does not know the queue fields also writes,
    // leaves them alone.
    update(&mut doc, "qc4", &b, mast_proto::WorkItemChanges { title: Some("B2".into()), ..Default::default() }, WorkOutlineActorKind::User);
    let snapshot = snap(&doc);
    assert_eq!(snapshot.item(&b).unwrap().queue_prerequisites, [a.clone()]);
    assert_eq!(snapshot.item(&b).unwrap().queue_completion, QueueCompletion::Landed);
    assert!(snapshot.item(&b).unwrap().queue_order_key.is_some());

    // Completion records who did it, so an agent's completion is told from a person's.
    let complete = || mast_proto::WorkItemChanges { status: Some(WorkItemStatus::Completed), ..Default::default() };
    update(&mut doc, "qc5", &a, complete(), WorkOutlineActorKind::Agent);
    assert_eq!(snap(&doc).item(&a).unwrap().completed_by.as_ref().map(|by| by.kind), Some(WorkOutlineActorKind::Agent));
    let reopen = || mast_proto::WorkItemChanges { status: Some(WorkItemStatus::Active), ..Default::default() };
    update(&mut doc, "qc6", &a, reopen(), WorkOutlineActorKind::User);
    assert_eq!(snap(&doc).item(&a).unwrap().completed_by, None);
    update(&mut doc, "qc7", &a, complete(), WorkOutlineActorKind::User);
    assert_eq!(snap(&doc).item(&a).unwrap().completed_by.as_ref().map(|by| by.kind), Some(WorkOutlineActorKind::User));

    // A person completing an item an agent completed takes the completion over.
    update(&mut doc, "qc8", &a, reopen(), WorkOutlineActorKind::User);
    update(&mut doc, "qc9", &a, complete(), WorkOutlineActorKind::Agent);
    assert_eq!(snap(&doc).item(&a).unwrap().completed_by.as_ref().map(|by| by.kind), Some(WorkOutlineActorKind::Agent));
    update(&mut doc, "qc10", &a, complete(), WorkOutlineActorKind::User);
    assert_eq!(snap(&doc).item(&a).unwrap().completed_by.as_ref().map(|by| by.kind), Some(WorkOutlineActorKind::User));
    // An agent completing it again never takes a person's completion back.
    update(&mut doc, "qc11", &a, complete(), WorkOutlineActorKind::Agent);
    assert_eq!(snap(&doc).item(&a).unwrap().completed_by.as_ref().map(|by| by.kind), Some(WorkOutlineActorKind::User));
}

/// An older build does not know the queue fields. Its writes to an item row, a
/// retitle, a completion or an archive, never carry them, so they survive, and
/// its anonymous completion is not a person's landing.
#[test]
fn an_older_peers_writes_keep_the_queue_fields_and_its_completion_is_not_a_landing() {
    use mast_proto::{QueueChange, QueueCompletion, QueueEntryState, QueueWaitReason};
    let mut doc = doc_with_spaces("dev-a");
    let project = create(&mut doc, "op-project", None, "Project");
    mark_project(&mut doc, "op-mark", &project);
    let a = create(&mut doc, "op-a", Some(&project), "A");
    let b = create(&mut doc, "op-b", Some(&project), "B");
    queue(&mut doc, "op-q1", &a, queue_place(WorkOutlinePlacement::Last)).unwrap();
    queue(&mut doc, "op-q2", &b, queue_place(WorkOutlinePlacement::Last)).unwrap();
    queue(&mut doc, "op-q3", &b, QueueChange::SetPrerequisites { work_item_ids: vec![a.clone()] }).unwrap();
    queue(&mut doc, "op-q4", &a, QueueChange::SetCompletion { completion: QueueCompletion::Landed }).unwrap();
    let entries = |doc: &RegistryDoc| snap(doc).work_queue(&project, 0, &no_pull_requests, &no_chats);
    let states = |doc: &RegistryDoc| entries(doc).into_iter().map(|entry| (entry.title, entry.state)).collect::<Vec<_>>();
    // What an older writer sends: only the fields it knows.
    let older = |doc: &mut RegistryDoc, id: &str, fields: [(&str, Value); 2]| {
        let fields = fields.into_iter().map(|(name, value)| (name.to_owned(), value)).collect();
        doc.commit(vec![Planned::update(KIND_WORK_ITEMS, id, fields)], &[]).unwrap();
    };
    assert_eq!(
        states(&doc),
        [("A".to_owned(), QueueEntryState::Ready), ("B".to_owned(), QueueEntryState::Waiting)]
    );

    // A retitle leaves the order, the prerequisite and the completion alone.
    older(&mut doc, &a, [("title", json!("A renamed")), ("updatedAt", json!(5))]);
    older(&mut doc, &b, [("title", json!("B renamed")), ("updatedAt", json!(5))]);
    let outline = snap(&doc);
    assert_eq!(outline.item(&b).unwrap().queue_prerequisites, [a.clone()]);
    assert_eq!(outline.item(&a).unwrap().queue_completion, QueueCompletion::Landed);
    assert_eq!(queue_titles(&doc, &project), ["A renamed", "B renamed"]);

    // It completes A without saying who: the item is done, but nobody has landed it.
    older(&mut doc, &a, [("status", json!("completed")), ("updatedAt", json!(6))]);
    assert_eq!(states(&doc)[0].1, QueueEntryState::Done);
    assert_eq!(states(&doc)[1].1, QueueEntryState::Waiting);
    assert_eq!(entries(&doc)[1].waiting_on[0].reason, QueueWaitReason::NotLanded);
    assert!(snap(&doc).next_queue_offer(&project, 0, &no_pull_requests, &no_chats).is_none());

    // A person completing it on a current build takes the completion over.
    let complete = || mast_proto::WorkItemChanges { status: Some(WorkItemStatus::Completed), ..Default::default() };
    apply(
        &mut doc,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: a.clone(),
            changes: complete(),
            operation_id: "op-person".into(),
        }),
    )
    .unwrap();
    assert_eq!(states(&doc)[1].1, QueueEntryState::Ready);
    assert_eq!(
        snap(&doc).next_queue_offer(&project, 0, &no_pull_requests, &no_chats).map(|entry| entry.work_item_id),
        Some(b.clone())
    );

    // It archives and restores B: B keeps its place, prerequisite and key.
    let key = snap(&doc).item(&b).unwrap().queue_order_key.clone();
    older(&mut doc, &b, [("archivedAt", json!(7)), ("updatedAt", json!(7))]);
    assert!(states(&doc).iter().all(|(title, state)| title != "B renamed" || *state == QueueEntryState::Done));
    older(&mut doc, &b, [("archivedAt", Value::Null), ("updatedAt", json!(8))]);
    let outline = snap(&doc);
    assert_eq!(outline.item(&b).unwrap().queue_order_key, key);
    assert_eq!(outline.item(&b).unwrap().queue_prerequisites, [a.clone()]);
    assert_eq!(queue_titles(&doc, &project), ["A renamed", "B renamed"]);
    assert_eq!(states(&doc)[1].1, QueueEntryState::Ready);
}

#[test]
fn queue_entry_state_and_next_offer() {
    let mut doc = doc_with_spaces("dev-a");
    let project = create(&mut doc, "qs-project", None, "Project");
    mark_project(&mut doc, "qs-mark", &project);
    let a = create(&mut doc, "qs-a", Some(&project), "A");
    let b = create(&mut doc, "qs-b", Some(&project), "B");
    queue(&mut doc, "qs1", &a, queue_place(WorkOutlinePlacement::Last)).unwrap();
    queue(&mut doc, "qs2", &b, queue_place(WorkOutlinePlacement::Last)).unwrap();
    let offer = |doc: &RegistryDoc| {
        snap(doc)
            .next_queue_offer(&project, 0, &no_pull_requests, &no_chats)
            .map(|e| e.title)
    };
    assert_eq!(offer(&doc), Some("A".into()));

    // A completed item leaves the offer; the next ready one takes it.
    apply(
        &mut doc,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: a.clone(),
            changes: mast_proto::WorkItemChanges {
                status: Some(WorkItemStatus::Completed),
                ..Default::default()
            },
            operation_id: "qs-done".into(),
        }),
    )
    .unwrap();
    assert_eq!(offer(&doc), Some("B".into()));
    let states: Vec<_> = snap(&doc)
        .work_queue(&project, 0, &no_pull_requests, &no_chats)
        .into_iter()
        .map(|e| e.state)
        .collect();
    assert_eq!(states, [mast_proto::QueueEntryState::Done, mast_proto::QueueEntryState::Ready]);
}

fn link_pull_request(doc: &mut RegistryDoc, op: &str, item: &str, number: u32) -> String {
    let url = format!("https://github.com/acme/repo/pull/{number}");
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItemExternalReference(CreateWorkItemExternalReferenceParams {
            work_item_id: item.into(),
            provider: "github".into(),
            reference_type: "pullRequest".into(),
            external_id: format!("acme/repo#{number}"),
            url: url.clone(),
            cached_title: None,
            operation_id: op.into(),
        }),
    )
    .unwrap();
    url
}

#[test]
fn queue_never_asks_for_the_pr_of_a_completed_item() {
    let mut doc = doc_with_spaces("dev-a");
    let project = create(&mut doc, "qc-project", None, "Project");
    mark_project(&mut doc, "qc-mark", &project);
    let a = create(&mut doc, "qc-a", Some(&project), "A");
    let b = create(&mut doc, "qc-b", Some(&project), "B");
    queue(&mut doc, "qc1", &a, queue_place(WorkOutlinePlacement::Last)).unwrap();
    queue(&mut doc, "qc2", &b, queue_place(WorkOutlinePlacement::Last)).unwrap();
    let a_pr = link_pull_request(&mut doc, "qc-pr-a", &a, 1);
    let b_pr = link_pull_request(&mut doc, "qc-pr-b", &b, 2);
    apply(
        &mut doc,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: a.clone(),
            changes: mast_proto::WorkItemChanges {
                status: Some(WorkItemStatus::Completed),
                ..Default::default()
            },
            operation_id: "qc-done".into(),
        }),
    )
    .unwrap();
    let asked = std::cell::RefCell::new(Vec::new());
    let lookup = |url: &str| {
        asked.borrow_mut().push(url.to_owned());
        mast_proto::work_status::PullRequestFacts::default()
    };
    snap(&doc).work_queue(&project, 0, &lookup, &no_chats);
    assert_eq!(*asked.borrow(), [b_pr], "only the live item is looked up, not {a_pr}");
}

#[test]
fn queue_counts_a_merged_pr_as_done_and_holds_an_unanswered_one() {
    use mast_proto::QueueEntryState::{Done, Ready};
    use mast_proto::work_status::PullRequestFacts;
    let mut doc = doc_with_spaces("dev-a");
    let project = create(&mut doc, "qm-project", None, "Project");
    mark_project(&mut doc, "qm-mark", &project);
    let a = create(&mut doc, "qm-a", Some(&project), "A");
    let b = create(&mut doc, "qm-b", Some(&project), "B");
    queue(&mut doc, "qm1", &a, queue_place(WorkOutlinePlacement::Last)).unwrap();
    queue(&mut doc, "qm2", &b, queue_place(WorkOutlinePlacement::Last)).unwrap();
    let a_pr = link_pull_request(&mut doc, "qm-pr", &a, 1);
    let snapshot = snap(&doc);
    let lookup = |known: PullRequestFacts| {
        let a_pr = a_pr.clone();
        move |url: &str| if url == a_pr { known } else { PullRequestFacts::default() }
    };
    let offer = |facts| {
        snapshot
            .next_queue_offer(&project, 0, &lookup(facts), &no_chats)
            .map(|e| e.title)
    };
    let states = |facts| -> Vec<_> {
        snapshot
            .work_queue(&project, 0, &lookup(facts), &no_chats)
            .into_iter()
            .map(|e| (e.state, e.facts_pending))
            .collect()
    };

    // Cold cache: A is Ready but pending, so nothing is offered, B included.
    let unknown = PullRequestFacts::default();
    assert_eq!(states(unknown), [(Ready, true), (Ready, false)]);
    assert_eq!(offer(unknown), None);

    // The lookup answers merged: A is Done and B takes the offer.
    let merged = PullRequestFacts { state_known: true, merged: true, ..Default::default() };
    assert_eq!(states(merged), [(Done, false), (Ready, false)]);
    assert_eq!(offer(merged), Some("B".into()));

    // Open with no chat attached stays Ready and is offered.
    let open = PullRequestFacts { state_known: true, open: true, ..Default::default() };
    assert_eq!(states(open), [(Ready, false), (Ready, false)]);
    assert_eq!(offer(open), Some("A".into()));
}

#[test]
fn queue_pause_is_a_project_setting_users_toggle() {
    let mut doc = doc_with_spaces("dev-a");
    let loose = create(&mut doc, "qp-loose", None, "Loose");
    let pause = |doc: &mut RegistryDoc, op: &str, id: &str, paused| {
        apply(
            doc,
            WorkOutlineMutation::SetQueuePaused(mast_proto::SetQueuePausedParams {
                by: None,
                reason: None,
                work_item_id: id.into(),
                paused,
                operation_id: op.into(),
            }),
        )
    };
    assert!(pause(&mut doc, "qp-1", &loose, true).is_err(), "only a project can pause");
    let project = create(&mut doc, "qp-project", None, "Project");
    mark_project(&mut doc, "qp-mark", &project);
    assert!(!snap(&doc).item(&project).unwrap().queue_paused);
    pause(&mut doc, "qp-2", &project, true).unwrap();
    assert!(snap(&doc).item(&project).unwrap().queue_paused);
    pause(&mut doc, "qp-3", &project, false).unwrap();
    assert!(!snap(&doc).item(&project).unwrap().queue_paused);
}

#[test]
fn queued_item_leaves_the_queue_when_it_moves_out_of_its_project() {
    let mut doc = doc_with_spaces("dev-a");
    let p1 = create(&mut doc, "qm-p1", None, "P1");
    let p2 = create(&mut doc, "qm-p2", None, "P2");
    mark_project(&mut doc, "qm-m1", &p1);
    mark_project(&mut doc, "qm-m2", &p2);
    let a = create(&mut doc, "qm-a", Some(&p1), "A");
    queue(&mut doc, "qm-q", &a, queue_place(WorkOutlinePlacement::Last)).unwrap();

    // A move inside the project keeps the place.
    let sub = create(&mut doc, "qm-sub", Some(&p1), "Sub");
    move_to(&mut doc, "qm-in", &a, Some(&sub), WorkOutlinePlacement::Last).unwrap();
    assert_eq!(queue_titles(&doc, &p1), ["A"]);

    // A move to another project drops it instead of queuing it there.
    move_to(&mut doc, "qm-out", &a, Some(&p2), WorkOutlinePlacement::Last).unwrap();
    assert_eq!(queue_titles(&doc, &p1), Vec::<String>::new());
    assert_eq!(queue_titles(&doc, &p2), Vec::<String>::new());
    assert_eq!(snap(&doc).item(&a).unwrap().queue_order_key, None);
}

#[test]
fn unmarking_a_project_retires_its_queue_and_pause() {
    let mut doc = doc_with_spaces("dev-a");
    let project = create(&mut doc, "qu-project", None, "Project");
    mark_project(&mut doc, "qu-mark", &project);
    let a = create(&mut doc, "qu-a", Some(&project), "A");
    queue(&mut doc, "qu-q", &a, queue_place(WorkOutlinePlacement::Last)).unwrap();
    apply(
        &mut doc,
        WorkOutlineMutation::SetQueuePaused(mast_proto::SetQueuePausedParams {
            by: None,
            reason: None,
            work_item_id: project.clone(),
            paused: true,
            operation_id: "qu-pause".into(),
        }),
    )
    .unwrap();
    apply(
        &mut doc,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: project.clone(),
            changes: mast_proto::WorkItemChanges {
                is_project: Some(false),
                ..Default::default()
            },
            operation_id: "qu-unmark".into(),
        }),
    )
    .unwrap();
    let snapshot = snap(&doc);
    assert_eq!(snapshot.item(&a).unwrap().queue_order_key, None);
    assert!(!snapshot.item(&project).unwrap().queue_paused);
}

#[test]
fn agents_cannot_edit_a_queue_or_its_pause() {
    let mut doc = doc_with_spaces("dev-a");
    let project = create(&mut doc, "qa-project", None, "Project");
    mark_project(&mut doc, "qa-mark", &project);
    let a = create(&mut doc, "qa-a", Some(&project), "A");
    doc.upsert_chat(&space_chat("agent-chat", Some(SPACE))).unwrap();
    let agent = WorkOutlineActor {
        kind: WorkOutlineActorKind::Agent,
        device_id: "dev-a".into(),
        chat_id: Some("agent-chat".into()),
        message_id: None,
    };
    for mutation in [
        WorkOutlineMutation::SetQueuePosition(mast_proto::SetQueuePositionParams {
            work_item_id: a.clone(),
            change: queue_place(WorkOutlinePlacement::Last),
            operation_id: "qa-position".into(),
        }),
        WorkOutlineMutation::SetQueuePaused(mast_proto::SetQueuePausedParams {
            by: None,
            reason: None,
            work_item_id: project.clone(),
            paused: true,
            operation_id: "qa-pause".into(),
        }),
    ] {
        assert!(
            doc.apply_work_outline_mutation(&mutation, &agent, 1_700_000_000_000)
                .is_err(),
            "{} must be refused for agents",
            mutation.op_name()
        );
    }
    assert_eq!(snap(&doc).item(&a).unwrap().queue_order_key, None);
}

#[test]
fn moving_a_container_drops_queued_descendants_but_keeps_a_nested_projects_queue() {
    let mut doc = doc_with_spaces("dev-a");
    let p1 = create(&mut doc, "qc-p1", None, "P1");
    let p2 = create(&mut doc, "qc-p2", None, "P2");
    mark_project(&mut doc, "qc-m1", &p1);
    mark_project(&mut doc, "qc-m2", &p2);
    let folder = create(&mut doc, "qc-folder", Some(&p1), "Folder");
    let child = create(&mut doc, "qc-child", Some(&folder), "Child");
    let nested = create(&mut doc, "qc-nested", Some(&folder), "Nested project");
    mark_project(&mut doc, "qc-mn", &nested);
    let inner = create(&mut doc, "qc-inner", Some(&nested), "Inner");
    queue(&mut doc, "qc-q1", &child, queue_place(WorkOutlinePlacement::Last)).unwrap();
    queue(&mut doc, "qc-q2", &inner, queue_place(WorkOutlinePlacement::Last)).unwrap();

    move_to(&mut doc, "qc-move", &folder, Some(&p2), WorkOutlinePlacement::Last).unwrap();
    let snapshot = snap(&doc);
    assert_eq!(snapshot.item(&child).unwrap().queue_order_key, None);
    assert!(snapshot.item(&inner).unwrap().queue_order_key.is_some());
    assert_eq!(queue_titles(&doc, &nested), ["Inner"]);
    assert_eq!(queue_titles(&doc, &p2), Vec::<String>::new());
}

fn run_params(item: &str, tag: &str, op: &str) -> mast_proto::UpsertExternalRunParams {
    mast_proto::UpsertExternalRunParams {
        work_item_id: item.into(),
        run_tag: tag.into(),
        operation_id: op.into(),
        ..Default::default()
    }
}

fn upsert_run(
    doc: &mut RegistryDoc,
    params: mast_proto::UpsertExternalRunParams,
) -> Outcome<WorkOutlineMutateReply> {
    apply(doc, WorkOutlineMutation::UpsertExternalRun(params))
}

#[test]
fn external_run_upsert_writes_only_the_fields_supplied_and_replays_cleanly() {
    let mut doc = doc_with_spaces("dev-a");
    let item = create(&mut doc, "run-item", None, "Item");
    let first = mast_proto::UpsertExternalRunParams {
        codespace_name: Some("box-1".into()),
        provider: Some("claude".into()),
        ..run_params(&item, "tag-1", "run-op-1")
    };
    let id = upsert_run(&mut doc, first.clone()).unwrap().external_run_id.unwrap();
    assert_eq!(id, mast_proto::external_run_id(&item, "tag-1"));
    let runs = snap(&doc).external_runs;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].kind, "codespace");
    assert_eq!(runs[0].codespace_name.as_deref(), Some("box-1"));
    assert!(runs[0].expected_alive && runs[0].ended_at.is_none());

    // The same call again (same operation id) is a replay: still one run, and
    // the recorded fields are exactly as they were.
    let before = snap(&doc).external_runs;
    upsert_run(&mut doc, first).unwrap();
    assert_eq!(snap(&doc).external_runs.len(), 1);
    let after = snap(&doc).external_runs;
    assert_eq!(
        (&before[0].codespace_name, &before[0].provider, &before[0].branch),
        (&after[0].codespace_name, &after[0].provider, &after[0].branch)
    );

    // A later call with other fields keeps what the first one wrote.
    let branch = mast_proto::UpsertExternalRunParams {
        branch: Some("feature".into()),
        ..run_params(&item, "tag-1", "run-op-2")
    };
    upsert_run(&mut doc, branch).unwrap();
    let run = snap(&doc).external_runs.remove(0);
    assert_eq!(run.codespace_name.as_deref(), Some("box-1"));
    assert_eq!(run.provider.as_deref(), Some("claude"));
    assert_eq!(run.branch.as_deref(), Some("feature"));
}

#[test]
fn ending_a_run_stamps_it_and_reopening_needs_an_explicit_flag() {
    use mast_proto::ExternalRunOutcome;
    let mut doc = doc_with_spaces("dev-a");
    let item = create(&mut doc, "run-item", None, "Item");
    upsert_run(&mut doc, run_params(&item, "tag-1", "op-1")).unwrap();
    let ended = mast_proto::UpsertExternalRunParams {
        outcome: Some(ExternalRunOutcome::Done),
        ..run_params(&item, "tag-1", "op-2")
    };
    upsert_run(&mut doc, ended).unwrap();
    let run = snap(&doc).external_runs.remove(0);
    assert!(!run.expected_alive);
    assert_eq!(run.outcome, Some(ExternalRunOutcome::Done));
    assert_eq!(run.ended_at, Some(1_700_000_000_000));

    // A late call cannot quietly bring an ended run back.
    let alive = mast_proto::UpsertExternalRunParams {
        expected_alive: Some(true),
        ..run_params(&item, "tag-1", "op-3")
    };
    let error = upsert_run(&mut doc, alive.clone()).unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::Invalid);
    assert!(!snap(&doc).external_runs[0].expected_alive);

    let reopened = mast_proto::UpsertExternalRunParams { reopen: true, ..alive };
    upsert_run(&mut doc, reopened).unwrap();
    let run = snap(&doc).external_runs.remove(0);
    assert!(run.expected_alive);
    assert_eq!((run.ended_at, run.outcome), (None, None));
}

#[test]
fn external_run_upsert_validates_its_input() {
    let mut doc = doc_with_spaces("dev-a");
    let item = create(&mut doc, "run-item", None, "Item");
    assert!(upsert_run(&mut doc, run_params(&item, "", "op-1")).is_err());
    assert!(upsert_run(&mut doc, run_params(&item, &"t".repeat(201), "op-2")).is_err());
    let long_name = mast_proto::UpsertExternalRunParams {
        display_name: Some("n".repeat(201)),
        ..run_params(&item, "tag-1", "op-3")
    };
    assert_eq!(
        upsert_run(&mut doc, long_name).unwrap_err().code,
        WorkOutlineErrorCode::LimitExceeded
    );
    let missing = run_params("no-such-item", "tag-1", "op-4");
    assert_eq!(upsert_run(&mut doc, missing).unwrap_err().code, WorkOutlineErrorCode::NotFound);
    assert!(snap(&doc).external_runs.is_empty());
}

#[test]
fn two_clients_registering_the_same_run_converge_on_one_row_and_a_fresh_client_sees_it() {
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    let item = derive_id("op-p", roles::ITEM);
    let from_a = mast_proto::UpsertExternalRunParams {
        codespace_name: Some("box-1".into()),
        ..run_params(&item, "tag-1", "a-op")
    };
    let from_b = mast_proto::UpsertExternalRunParams {
        branch: Some("feature".into()),
        ..run_params(&item, "tag-1", "b-op")
    };
    upsert_run(&mut a, from_a).unwrap();
    upsert_run(&mut b, from_b).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    for doc in [&a, &b] {
        let runs = snap(doc).external_runs;
        assert_eq!(runs.len(), 1, "one row, not two");
        // Different fields written by different clients both survive.
        assert_eq!(runs[0].codespace_name.as_deref(), Some("box-1"));
        assert_eq!(runs[0].branch.as_deref(), Some("feature"));
    }
    // A client that joins later gets the server's rows in its hello reply and
    // reads the same run.
    let mut fresh = RegistryDoc::new("dev-c");
    fresh.apply_rows(seq, server.values().cloned().collect());
    assert_eq!(snap(&fresh).external_runs, snap(&a).external_runs);
    assert_eq!(snap(&fresh).external_runs.len(), 1);
}

#[test]
fn runs_stay_with_an_item_that_moves_or_is_archived_and_go_with_the_space() {
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    let q = derive_id("op-q", roles::ITEM);
    let x = derive_id("op-x", roles::ITEM);
    upsert_run(&mut a, run_params(&x, "tag-1", "run-1")).unwrap();
    // Moving the item under another parent keeps its run.
    move_to(&mut a, "move-x", &x, Some(&q), WorkOutlinePlacement::Last).unwrap();
    assert_eq!(snap(&a).external_runs.len(), 1);
    // Archiving it leaves the run readable.
    apply(
        &mut a,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: x.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "archive-x".into(),
        }),
    )
    .unwrap();
    assert_eq!(snap(&a).external_runs.len(), 1);
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let run_id = mast_proto::external_run_id(&x, "tag-1");
    assert_eq!(b.row_state(KIND_EXTERNAL_RUNS, &run_id), RowState::Live);
    // Deleting the Space removes the run row on every client.
    a.delete_space(SPACE).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    for doc in [&a, &b] {
        assert_eq!(doc.row_state(KIND_EXTERNAL_RUNS, &run_id), RowState::Tombstoned);
    }
}

#[test]
fn run_rows_without_a_space_or_for_a_missing_item_are_left_out_of_the_snapshot() {
    let mut doc = doc_with_spaces("dev-a");
    let item = create(&mut doc, "run-item", None, "Item");
    upsert_run(&mut doc, run_params(&item, "tag-1", "op-1")).unwrap();
    let raw = |id: &str, fields: serde_json::Value| RowOp {
        kind: KIND_EXTERNAL_RUNS.into(),
        id: id.into(),
        op: OpKind::Upsert,
        set: Some(fields.as_object().unwrap().clone().into_iter().collect()),
        hlc: encode_hlc(9, 0, "dev-a"),
        clocks: None,
    };
    doc.enqueue_ops(vec![
        raw("no-space", json!({"workItemId": item, "runTag": "x", "kind": "codespace"})),
        raw("no-item", json!({"spaceId": SPACE, "workItemId": "missing", "runTag": "y"})),
        raw("other-space", json!({"spaceId": OTHER_SPACE, "workItemId": item, "runTag": "z"})),
        raw("future", json!({"spaceId": SPACE, "workItemId": item, "runTag": "f", "kind": "someFutureKind"})),
    ]);
    let kinds: Vec<_> = snap(&doc).external_runs.iter().map(|r| r.kind.clone()).collect();
    assert_eq!(kinds.len(), 2, "{kinds:?}");
    assert!(kinds.contains(&"codespace".to_owned()));
    assert!(kinds.contains(&"someFutureKind".to_owned()), "unknown kinds are kept");
}

const QUEUE_NOW_MS: i64 = 100 * 60 * 1000;

/// A project queue of A then B, with one chat `w1` attached to A.
fn queue_with_a_chat() -> (RegistryDoc, String) {
    let mut doc = doc_with_spaces("dev-a");
    let project = create(&mut doc, "st-project", None, "Project");
    mark_project(&mut doc, "st-mark", &project);
    let a = create(&mut doc, "st-a", Some(&project), "A");
    let b = create(&mut doc, "st-b", Some(&project), "B");
    queue(&mut doc, "st1", &a, queue_place(WorkOutlinePlacement::Last)).unwrap();
    queue(&mut doc, "st2", &b, queue_place(WorkOutlinePlacement::Last)).unwrap();
    doc.upsert_chat(&space_chat("w1", Some(SPACE))).unwrap();
    assign(&mut doc, "st-assign", "w1", &a).unwrap();
    (doc, project)
}

/// States of the queue, and the title offered, when `w1` looks like `activity`.
fn queue_when_the_chat_is(
    doc: &RegistryDoc,
    project: &str,
    activity: mast_proto::ChatActivity,
) -> (Vec<(mast_proto::QueueEntryState, Option<mast_proto::QueueStall>)>, Option<String>) {
    let chats = move |id: &str| if id == "w1" { activity } else { Default::default() };
    let snapshot = snap(doc);
    let states = snapshot
        .work_queue(project, QUEUE_NOW_MS, &no_pull_requests, &chats)
        .into_iter()
        .map(|entry| (entry.state, entry.stall))
        .collect();
    let offer = snapshot
        .next_queue_offer(project, QUEUE_NOW_MS, &no_pull_requests, &chats)
        .map(|entry| entry.title);
    (states, offer)
}

fn worker(last_active_minutes_ago: i64) -> mast_proto::ChatActivity {
    mast_proto::ChatActivity {
        worker: true,
        last_active_ms: QUEUE_NOW_MS - last_active_minutes_ago * 60_000,
        ..Default::default()
    }
}

#[test]
fn a_worker_whose_last_turn_errored_is_stalled_and_the_next_item_is_offered() {
    use mast_proto::QueueEntryState::{Ready, Stalled};
    use mast_proto::QueueStall;
    let (doc, project) = queue_with_a_chat();
    let errored = mast_proto::ChatActivity { errored: true, ..worker(2) };
    let (states, offer) = queue_when_the_chat_is(&doc, &project, errored);
    assert_eq!(states, [(Stalled, Some(QueueStall::Errored)), (Ready, None)]);
    assert_eq!(offer.as_deref(), Some("B"));
    // An error that is a credits or usage limit says so.
    let limit = mast_proto::ChatActivity { out_of_credits: true, ..errored };
    let (states, _) = queue_when_the_chat_is(&doc, &project, limit);
    assert_eq!(states[0], (Stalled, Some(QueueStall::OutOfCredits)));
}

#[test]
fn a_worker_idle_within_the_window_is_still_running_and_nothing_new_is_offered() {
    use mast_proto::QueueEntryState::{Ready, Running};
    let (doc, project) = queue_with_a_chat();
    let (states, offer) = queue_when_the_chat_is(&doc, &project, worker(10));
    assert_eq!(states, [(Running, None), (Ready, None)]);
    assert_eq!(offer, None, "its background build may still be going");
    // The edge of the window still counts.
    let (states, _) = queue_when_the_chat_is(&doc, &project, worker(30));
    assert_eq!(states[0].0, Running);
}

#[test]
fn the_same_worker_idle_past_the_window_is_stalled_and_the_next_item_is_offered() {
    use mast_proto::QueueEntryState::{Ready, Stalled};
    use mast_proto::QueueStall;
    let (doc, project) = queue_with_a_chat();
    let (states, offer) = queue_when_the_chat_is(&doc, &project, worker(31));
    assert_eq!(states, [(Stalled, Some(QueueStall::Idle { minutes: Some(31) })), (Ready, None)]);
    assert_eq!(offer.as_deref(), Some("B"));
    // A worker with no recorded activity at all is stalled too, not running.
    let (states, _) = queue_when_the_chat_is(&doc, &project, mast_proto::ChatActivity { worker: true, ..Default::default() });
    assert_eq!(states[0], (Stalled, Some(QueueStall::Idle { minutes: None })));
}

#[test]
fn a_final_handback_makes_the_item_done() {
    use mast_proto::QueueEntryState::{Done, Ready};
    let (mut doc, project) = queue_with_a_chat();
    let a = derive_id("st-a", roles::ITEM);
    // The worker's final handback is a registry row for its current
    // assignment; the snapshot reports the item.
    let generation = snap(&doc).assignments[0].generation.clone().expect("a generation");
    record_handback(&mut doc, "handback-1", "w1", &generation);
    assert_eq!(snap(&doc).handed_back_work_item_ids, [a]);
    let (states, offer) = queue_when_the_chat_is(&doc, &project, worker(1));
    assert_eq!(states, [(Done, None), (Ready, None)]);
    assert_eq!(offer.as_deref(), Some("B"));
}

fn record_handback(doc: &mut RegistryDoc, id: &str, chat: &str, generation: &str) {
    doc.enqueue_ops(vec![RowOp {
        kind: crate::registry::final_handback::KIND_FINAL_HANDBACKS.into(),
        id: id.into(),
        op: OpKind::Upsert,
        set: Some(BTreeMap::from([
            ("spaceId".to_owned(), json!(SPACE)),
            ("workerChatId".to_owned(), json!(chat)),
            ("assignmentGeneration".to_owned(), json!(generation)),
        ])),
        hlc: encode_hlc(9, 0, "dev-a"),
        clocks: None,
    }]);
}

#[test]
fn a_handback_for_an_earlier_assignment_does_not_mark_the_chats_new_item_done() {
    use mast_proto::QueueEntryState::{Done, Running};
    let (mut doc, project) = queue_with_a_chat();
    // A handback recorded for some earlier assignment of this chat.
    record_handback(&mut doc, "handback-old", "w1", "an-earlier-generation");
    assert!(snap(&doc).handed_back_work_item_ids.is_empty());
    let (states, _) = queue_when_the_chat_is(&doc, &project, worker(1));
    assert_ne!(states[0].0, Done);
    assert_eq!(states[0].0, Running);
}

#[test]
fn a_run_ended_on_one_device_is_not_expected_alive_whatever_another_wrote_last() {
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    let item = derive_id("op-p", roles::ITEM);
    upsert_run(&mut a, run_params(&item, "tag-1", "reg-1")).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    // A ends the run while B, which has not seen that, writes expectedAlive.
    let ended = mast_proto::UpsertExternalRunParams {
        outcome: Some(mast_proto::ExternalRunOutcome::Failed),
        ..run_params(&item, "tag-1", "end-1")
    };
    upsert_run(&mut a, ended).unwrap();
    let alive = mast_proto::UpsertExternalRunParams {
        expected_alive: Some(true),
        ..run_params(&item, "tag-1", "again-1")
    };
    upsert_run(&mut b, alive).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    for doc in [&a, &b] {
        let run = snap(doc).external_runs.remove(0);
        assert!(run.ended_at.is_some());
        assert!(!run.expected_alive, "an ended run is never expected alive");
    }
}

#[test]
fn reopening_cannot_be_combined_with_ending() {
    let mut doc = doc_with_spaces("dev-a");
    let item = create(&mut doc, "run-item", None, "Item");
    upsert_run(&mut doc, run_params(&item, "tag-1", "op-1")).unwrap();
    for params in [
        mast_proto::UpsertExternalRunParams { reopen: true, expected_alive: Some(false), ..run_params(&item, "tag-1", "op-2") },
        mast_proto::UpsertExternalRunParams { reopen: true, outcome: Some(mast_proto::ExternalRunOutcome::Done), ..run_params(&item, "tag-1", "op-3") },
    ] {
        assert_eq!(upsert_run(&mut doc, params).unwrap_err().code, WorkOutlineErrorCode::Invalid);
    }
}

#[test]
fn a_planning_chat_does_not_keep_an_item_running_by_sitting_idle() {
    use mast_proto::QueueEntryState::{Ready, Running};
    let (doc, project) = queue_with_a_chat();
    let planning = mast_proto::ChatActivity { worker: false, last_active_ms: QUEUE_NOW_MS - 60_000, ..Default::default() };
    let (states, offer) = queue_when_the_chat_is(&doc, &project, planning);
    assert_eq!(states, [(Ready, None), (Ready, None)], "no worker, so still waiting to be started");
    assert_eq!(offer.as_deref(), Some("A"));
    // While it is actually in a turn, the item is being worked on.
    let busy = mast_proto::ChatActivity { working: true, ..planning };
    assert_eq!(queue_when_the_chat_is(&doc, &project, busy).0[0].0, Running);
}

#[test]
fn a_turn_in_flight_runs_however_long_ago_the_chat_was_last_seen_but_an_archived_chat_does_not() {
    use mast_proto::QueueEntryState::{Ready, Running};
    let (doc, project) = queue_with_a_chat();
    let working = mast_proto::ChatActivity { working: true, ..worker(90) };
    assert_eq!(queue_when_the_chat_is(&doc, &project, working).0[0].0, Running);
    // A worker archived without a handback is gone: the item can start afresh.
    let archived = mast_proto::ChatActivity { archived: true, ..worker(1) };
    let (states, offer) = queue_when_the_chat_is(&doc, &project, archived);
    assert_eq!(states[0], (Ready, None));
    assert_eq!(offer.as_deref(), Some("A"));
}

/// Like [`queue_when_the_chat_is`] for several chats, by id.
fn queue_when_chats_are(
    doc: &RegistryDoc,
    project: &str,
    activity: &[(&'static str, mast_proto::ChatActivity)],
) -> Vec<(mast_proto::QueueEntryState, Option<mast_proto::QueueStall>)> {
    let activity = activity.to_vec();
    let chats = move |id: &str| {
        activity
            .iter()
            .find(|(chat, _)| *chat == id)
            .map(|(_, activity)| *activity)
            .unwrap_or_default()
    };
    snap(doc)
        .work_queue(project, QUEUE_NOW_MS, &no_pull_requests, &chats)
        .into_iter()
        .map(|entry| (entry.state, entry.stall))
        .collect()
}

fn set_waiting_until(doc: &mut RegistryDoc, item: &str, until: i64, op: &str) {
    apply(
        doc,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: item.into(),
            changes: mast_proto::WorkItemChanges {
                waiting_until: FieldPatch::Set(until),
                ..Default::default()
            },
            operation_id: op.into(),
        }),
    )
    .unwrap();
}

#[test]
fn a_waiting_item_is_blocked_even_when_its_worker_is_stalled_and_a_handback_still_wins() {
    use mast_proto::QueueEntryState::{Blocked, Done};
    let (mut doc, project) = queue_with_a_chat();
    let a = derive_id("st-a", roles::ITEM);
    set_waiting_until(&mut doc, &a, QUEUE_NOW_MS + 3_600_000, "st-wait");
    let errored = mast_proto::ChatActivity { errored: true, ..worker(2) };
    let states = queue_when_chats_are(&doc, &project, &[("w1", errored)]);
    assert_eq!(states[0].0, Blocked);
    // A final handback still ends it.
    let generation = snap(&doc).assignments[0].generation.clone().expect("a generation");
    record_handback(&mut doc, "handback-wait", "w1", &generation);
    let states = queue_when_chats_are(&doc, &project, &[("w1", errored)]);
    assert_eq!(states[0].0, Done);
}

#[test]
fn a_working_planning_chat_keeps_the_item_running_beside_an_errored_worker() {
    use mast_proto::QueueEntryState::Running;
    let (mut doc, project) = queue_with_a_chat();
    let a = derive_id("st-a", roles::ITEM);
    doc.upsert_chat(&space_chat("plan", Some(SPACE))).unwrap();
    assign(&mut doc, "st-assign-plan", "plan", &a).unwrap();
    let errored = mast_proto::ChatActivity { errored: true, ..worker(2) };
    let planning = mast_proto::ChatActivity { working: true, ..Default::default() };
    let states = queue_when_chats_are(&doc, &project, &[("w1", errored), ("plan", planning)]);
    assert_eq!(states[0].0, Running);
}

#[test]
fn the_stall_reason_comes_from_the_most_recently_active_worker() {
    use mast_proto::QueueEntryState::Stalled;
    use mast_proto::QueueStall;
    let (mut doc, project) = queue_with_a_chat();
    let a = derive_id("st-a", roles::ITEM);
    doc.upsert_chat(&space_chat("w2", Some(SPACE))).unwrap();
    assign(&mut doc, "st-assign-w2", "w2", &a).unwrap();
    let older_idle = worker(90);
    let newer_errored = mast_proto::ChatActivity { errored: true, ..worker(40) };
    let states = queue_when_chats_are(&doc, &project, &[("w1", older_idle), ("w2", newer_errored)]);
    assert_eq!(states[0], (Stalled, Some(QueueStall::Errored)));
    // Swap which one is newer and the reason follows it.
    let states = queue_when_chats_are(
        &doc,
        &project,
        &[("w1", worker(40)), ("w2", mast_proto::ChatActivity { errored: true, ..worker(90) })],
    );
    assert_eq!(states[0], (Stalled, Some(QueueStall::Idle { minutes: Some(40) })));
}

#[test]
fn a_handback_survives_the_workers_chat_being_deleted() {
    let (mut doc, _project) = queue_with_a_chat();
    let a = derive_id("st-a", roles::ITEM);
    let generation = snap(&doc).assignments[0].generation.clone().expect("a generation");
    record_handback(&mut doc, "handback-gone", "w1", &generation);
    assert_eq!(snap(&doc).handed_back_work_item_ids, [a.clone()]);
    // The Chat row goes; the assignment stays as a dangling one.
    doc.delete_chat("w1").unwrap();
    assert_eq!(snap(&doc).handed_back_work_item_ids, [a], "the item is still reported done");
}

#[test]
fn a_chat_moved_to_another_item_leaves_the_handback_behind() {
    let (mut doc, _project) = queue_with_a_chat();
    let a = derive_id("st-a", roles::ITEM);
    let b = derive_id("st-b", roles::ITEM);
    let generation = snap(&doc).assignments[0].generation.clone().expect("a generation");
    record_handback(&mut doc, "handback-moved", "w1", &generation);
    assert_eq!(snap(&doc).handed_back_work_item_ids, [a.clone()]);
    // The same Chat is kept and attached to the next item: a fresh assignment.
    assign(&mut doc, "st-reassign", "w1", &b).unwrap();
    assert!(snap(&doc).handed_back_work_item_ids.is_empty(), "B is not done because A was");
    // Moving it back is yet another assignment, so the old handback stays old.
    assign(&mut doc, "st-reassign-back", "w1", &a).unwrap();
    assert!(snap(&doc).handed_back_work_item_ids.is_empty());
}

#[test]
fn a_handback_row_without_a_generation_does_not_match_a_current_assignment() {
    let (mut doc, _project) = queue_with_a_chat();
    doc.enqueue_ops(vec![RowOp {
        kind: crate::registry::final_handback::KIND_FINAL_HANDBACKS.into(),
        id: "handback-legacy".into(),
        op: OpKind::Upsert,
        set: Some(BTreeMap::from([
            ("spaceId".to_owned(), json!(SPACE)),
            ("workerChatId".to_owned(), json!("w1")),
        ])),
        hlc: encode_hlc(9, 0, "dev-a"),
        clocks: None,
    }]);
    assert!(snap(&doc).handed_back_work_item_ids.is_empty());
}

fn run_settings_mutation(
    op: &str,
    settings: mast_proto::ExternalRunSettings,
) -> WorkOutlineMutation {
    WorkOutlineMutation::SetExternalRunSettings(mast_proto::SetExternalRunSettingsParams {
        space_id: SPACE.into(),
        settings,
        operation_id: op.into(),
    })
}

#[test]
fn external_run_settings_default_then_round_trip_and_reject_unsafe_values() {
    let mut doc = doc_with_spaces("dev-a");
    let defaults = snap(&doc).space_settings.external_runs;
    assert_eq!(defaults.capacity_cap, 5);
    assert!(!defaults.probe_configured());

    let configured = mast_proto::ExternalRunSettings {
        capacity_cap: 8,
        box_limit: Some(15),
        stale_days: 3,
        run_dir: Some("/work/runs/{runTag}".into()),
        exit_file: Some("exit_code".into()),
        output_file: Some("out.log".into()),
        tmux_session: Some("run-{runTag}".into()),
    };
    apply(&mut doc, run_settings_mutation("run-settings-1", configured.clone())).unwrap();
    assert_eq!(snap(&doc).space_settings.external_runs, configured);

    for bad in [
        mast_proto::ExternalRunSettings { capacity_cap: 0, ..configured.clone() },
        mast_proto::ExternalRunSettings { stale_days: 0, ..configured.clone() },
        mast_proto::ExternalRunSettings { box_limit: Some(0), ..configured.clone() },
        mast_proto::ExternalRunSettings { run_dir: Some("relative".into()), ..configured.clone() },
        mast_proto::ExternalRunSettings { run_dir: Some("/a/$(rm -rf x)".into()), ..configured.clone() },
        mast_proto::ExternalRunSettings { run_dir: Some("/a/../b".into()), ..configured.clone() },
        mast_proto::ExternalRunSettings { exit_file: Some("../exit".into()), ..configured.clone() },
        mast_proto::ExternalRunSettings { tmux_session: Some("a b".into()), ..configured.clone() },
    ] {
        assert!(apply(&mut doc, run_settings_mutation("run-settings-bad", bad)).is_err());
    }
    assert_eq!(snap(&doc).space_settings.external_runs, configured);
}
