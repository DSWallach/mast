//! Linear rows and operations: per-Space setting, createWorkItemFromLinear
//! (one batch, idempotent, title format), project links, modified-field
//! marking and clearing, agent refusal, and Space deletion.

use super::*;
use crate::registry::tests::space;
use mast_proto::linear::LinearIssueLink;
use mast_proto::work_outline::{
    CreateWorkItemParams, FieldPatch, RemoveWorkItemExternalReferenceParams, UpdateWorkItemParams,
    WorkItemChanges, WorkOutlineActorKind, WorkOutlineErrorCode, WorkOutlineMutateReply,
    WorkOutlineMutation, WorkOutlinePlacement, WorkOutlineSnapshot,
};
use mast_proto::{LinearProjectRef, WorkOutlineError};

const SPACE: &str = "sp-1";
const OFF_SPACE: &str = "sp-2";
const NOW: i64 = 1_700_000_000_000;

fn actor(kind: WorkOutlineActorKind) -> WorkOutlineActor {
    WorkOutlineActor {
        kind,
        device_id: "dev-a".into(),
        chat_id: None,
        message_id: None,
    }
}

fn doc() -> RegistryDoc {
    let mut doc = RegistryDoc::new("dev-a");
    doc.upsert_space(&space(SPACE, "dev-a", "/tmp/one"))
        .unwrap();
    doc.upsert_space(&space(OFF_SPACE, "dev-a", "/tmp/two"))
        .unwrap();
    doc
}

fn apply_as(
    doc: &mut RegistryDoc,
    kind: WorkOutlineActorKind,
    mutation: WorkOutlineMutation,
) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
    doc.apply_work_outline_mutation(&mutation, &actor(kind), NOW)
}

fn apply(
    doc: &mut RegistryDoc,
    mutation: WorkOutlineMutation,
) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
    apply_as(doc, WorkOutlineActorKind::User, mutation)
}

fn enable(doc: &mut RegistryDoc, space_id: &str, op: &str, enabled: bool) {
    apply(
        doc,
        WorkOutlineMutation::SetSpaceLinearEnabled(SetSpaceLinearEnabledParams {
            space_id: space_id.into(),
            enabled,
            operation_id: op.into(),
        }),
    )
    .unwrap();
}

fn issue(identifier: &str, title: &str, description: Option<&str>) -> LinearIssueLink {
    LinearIssueLink {
        id: format!("uuid-{identifier}"),
        identifier: identifier.into(),
        title: title.into(),
        description: description.map(str::to_owned),
        url: format!("https://linear.app/acme/issue/{identifier}"),
    }
}

fn from_linear(
    doc: &mut RegistryDoc,
    space_id: &str,
    op: &str,
    parent: Option<&str>,
    issue: LinearIssueLink,
) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItemFromLinear(CreateWorkItemFromLinearParams {
            space_id: space_id.into(),
            parent_work_item_id: parent.map(str::to_owned),
            placement: WorkOutlinePlacement::Last,
            issue,
            operation_id: op.into(),
        }),
    )
}

fn plain_item(doc: &mut RegistryDoc, op: &str, title: &str, description: Option<&str>) -> String {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            title: title.into(),
            description: description.map(str::to_owned),
            status: None,
            operation_id: op.into(),
        }),
    )
    .unwrap()
    .work_item_id
    .unwrap()
}

fn update(
    doc: &mut RegistryDoc,
    kind: WorkOutlineActorKind,
    op: &str,
    id: &str,
    changes: WorkItemChanges,
) {
    apply_as(
        doc,
        kind,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: id.into(),
            changes,
            operation_id: op.into(),
        }),
    )
    .unwrap();
}

fn link_issue(doc: &mut RegistryDoc, op: &str, id: &str, identifier: &str) -> String {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItemExternalReference(
            CreateWorkItemExternalReferenceParams {
                work_item_id: id.into(),
                provider: LINEAR_PROVIDER.into(),
                reference_type: LINEAR_ISSUE_REFERENCE.into(),
                external_id: identifier.into(),
                url: format!("https://linear.app/acme/issue/{identifier}"),
                cached_title: None,
                operation_id: op.into(),
            },
        ),
    )
    .unwrap()
    .reference_id
    .unwrap()
}

fn snap(doc: &RegistryDoc) -> WorkOutlineSnapshot {
    doc.work_outline_snapshot(SPACE, true)
}

fn modified(doc: &RegistryDoc, id: &str) -> Vec<String> {
    snap(doc)
        .item(id)
        .unwrap()
        .external_sync_modified_fields
        .clone()
}

#[test]
fn worker_automation_defaults_off_and_persists_per_space() {
    use mast_proto::work_outline::{MergedPrCompletion, SetWorkerAutomationParams};
    let mut doc = doc();
    let defaults = snap(&doc).space_settings;
    assert!(!defaults.worker_worktrees);
    assert_eq!(defaults.merged_pr_completion, None);
    assert_eq!(defaults.archive_workers_after_days, None);
    apply(&mut doc, WorkOutlineMutation::SetWorkerAutomation(SetWorkerAutomationParams {
        space_id: SPACE.into(), field: None, worker_worktrees: true,
        merged_pr_completion: Some(MergedPrCompletion::Ask),
        archive_workers_after_days: Some(3), operation_id: "worker-settings".into(),
    })).unwrap();
    let settings = snap(&doc).space_settings;
    assert!(settings.worker_worktrees);
    assert_eq!(settings.merged_pr_completion, Some(MergedPrCompletion::Ask));
    assert_eq!(settings.archive_workers_after_days, Some(3));
    assert_eq!(doc.work_outline_snapshot(OFF_SPACE, true).space_settings, defaults);
    apply(&mut doc, WorkOutlineMutation::SetWorkerAutomation(SetWorkerAutomationParams {
        space_id: SPACE.into(), field: Some(mast_proto::work_outline::WorkerAutomationField::Worktrees),
        worker_worktrees: false, merged_pr_completion: None, archive_workers_after_days: None,
        operation_id: "worker-worktrees-off".into(),
    })).unwrap();
    let settings = snap(&doc).space_settings;
    assert!(!settings.worker_worktrees);
    assert_eq!(settings.merged_pr_completion, Some(MergedPrCompletion::Ask));
    assert_eq!(settings.archive_workers_after_days, Some(3));
}

#[test]
fn worker_context_window_defaults_to_200k_for_existing_spaces_and_zero_turns_it_off() {
    use mast_proto::work_outline::SetWorkerAutomationParams;
    let mut doc = doc();
    // A Space without a settings row, and one whose row predates the field.
    assert_eq!(snap(&doc).space_settings.worker_context_window, Some(200_000));
    apply(&mut doc, WorkOutlineMutation::SetWorkerAutomation(SetWorkerAutomationParams {
        space_id: SPACE.into(), field: None, worker_worktrees: true,
        merged_pr_completion: None, archive_workers_after_days: None,
        operation_id: "older-row".into(),
    })).unwrap();
    assert_eq!(snap(&doc).space_settings.worker_context_window, Some(200_000));
    let row = |value: serde_json::Value| -> RegistryRow {
        serde_json::from_value(json!({
            "kind": KIND_WORK_OUTLINE_SPACE_SETTINGS, "id": SPACE,
            "fields": { "workerContextWindow": value },
        })).unwrap()
    };
    assert_eq!(decode_space_settings(Some(&row(json!(128_000)))).worker_context_window, Some(128_000));
    assert_eq!(decode_space_settings(Some(&row(json!(0)))).worker_context_window, None);
    assert_eq!(decode_space_settings(Some(&row(json!("junk")))).worker_context_window, Some(200_000));
}

#[test]
fn worker_context_window_setter_stores_default_as_null_and_no_cap_as_zero() {
    use mast_proto::work_outline::SetWorkerContextWindowParams;
    let set = |doc: &mut RegistryDoc, tokens: Option<u64>, key: &str| {
        apply(doc, WorkOutlineMutation::SetWorkerContextWindow(SetWorkerContextWindowParams {
            space_id: SPACE.into(), tokens, operation_id: key.into(),
        }))
    };
    let window = |doc: &RegistryDoc| snap(doc).space_settings.worker_context_window;
    let mut doc = doc();

    // Default on a Space without a row writes nothing.
    set(&mut doc, None, "default-first").unwrap();
    assert_eq!(window(&doc), Some(200_000));

    set(&mut doc, Some(0), "no-cap").unwrap();
    assert_eq!(window(&doc), None);
    // Replaying the same operation is harmless.
    set(&mut doc, Some(0), "no-cap").unwrap();
    assert_eq!(window(&doc), None);

    set(&mut doc, Some(128_000), "size").unwrap();
    assert_eq!(window(&doc), Some(128_000));

    set(&mut doc, None, "default-again").unwrap();
    assert_eq!(window(&doc), Some(200_000));
    // The other settings of the row are untouched.
    assert!(!snap(&doc).space_settings.worker_worktrees);

    // Only the offered sizes, never an arbitrary count.
    assert_eq!(set(&mut doc, Some(150_000), "odd").unwrap_err().code, WorkOutlineErrorCode::Invalid);
    assert_eq!(window(&doc), Some(200_000));
    let unknown_space = apply(&mut doc, WorkOutlineMutation::SetWorkerContextWindow(SetWorkerContextWindowParams {
        space_id: "no-such-space".into(), tokens: Some(0), operation_id: "other".into(),
    }));
    assert_eq!(unknown_space.unwrap_err().code, WorkOutlineErrorCode::NotFound);
}

#[test]
fn worker_context_window_setter_is_user_only() {
    use mast_proto::work_outline::SetWorkerContextWindowParams;
    let mut doc = doc();
    for kind in [WorkOutlineActorKind::Agent, WorkOutlineActorKind::System] {
        let refused = apply_as(&mut doc, kind, WorkOutlineMutation::SetWorkerContextWindow(SetWorkerContextWindowParams {
            space_id: SPACE.into(), tokens: Some(0), operation_id: format!("agent-{kind:?}"),
        }));
        assert_eq!(
            refused.unwrap_err().code,
            WorkOutlineErrorCode::Unauthorized,
            "{kind:?} must not change the window"
        );
    }
    assert_eq!(snap(&doc).space_settings.worker_context_window, Some(200_000));
}

#[test]
fn worker_call_nudge_setter_stores_default_as_null_and_off_as_zero() {
    use mast_proto::work_outline::SetWorkerCallNudgeParams;
    let set = |doc: &mut RegistryDoc, calls: Option<u32>, key: &str| {
        apply(doc, WorkOutlineMutation::SetWorkerCallNudge(SetWorkerCallNudgeParams {
            space_id: SPACE.into(), calls, operation_id: key.into(),
        }))
    };
    let nudge = |doc: &RegistryDoc| snap(doc).space_settings.worker_call_nudge;
    let mut doc = doc();
    set(&mut doc, None, "default-first").unwrap();
    assert_eq!(nudge(&doc), 150);
    set(&mut doc, Some(0), "off").unwrap();
    assert_eq!(nudge(&doc), 0);
    set(&mut doc, Some(250), "count").unwrap();
    assert_eq!(nudge(&doc), 250);
    set(&mut doc, None, "default-again").unwrap();
    assert_eq!(nudge(&doc), 150);
    assert_eq!(set(&mut doc, Some(7), "odd").unwrap_err().code, WorkOutlineErrorCode::Invalid);
    assert_eq!(nudge(&doc), 150);
    for kind in [WorkOutlineActorKind::Agent, WorkOutlineActorKind::System] {
        let refused = apply_as(&mut doc, kind, WorkOutlineMutation::SetWorkerCallNudge(SetWorkerCallNudgeParams {
            space_id: SPACE.into(), calls: Some(0), operation_id: format!("agent-{kind:?}"),
        }));
        assert_eq!(refused.unwrap_err().code, WorkOutlineErrorCode::Unauthorized, "{kind:?}");
    }
    assert_eq!(nudge(&doc), 150);
}

#[test]
fn role_default_model_setter_stores_empty_as_null_and_is_user_only() {
    use mast_proto::work_outline::{DefaultModelRole, SetRoleDefaultModelParams};
    let set = |doc: &mut RegistryDoc, role, model: Option<&str>, key: &str| {
        apply(doc, WorkOutlineMutation::SetRoleDefaultModel(SetRoleDefaultModelParams {
            space_id: SPACE.into(), role, model: model.map(str::to_owned), operation_id: key.into(),
        }))
    };
    let models = |doc: &RegistryDoc| {
        let s = snap(doc).space_settings;
        (s.manager_default_model, s.worker_default_model)
    };
    let mut doc = doc();
    assert_eq!(models(&doc), (None, None));
    let before = doc.batches_written();
    set(&mut doc, DefaultModelRole::Worker, None, "noop").unwrap();
    assert_eq!(doc.batches_written(), before, "clearing an unset role writes nothing");
    set(&mut doc, DefaultModelRole::Worker, Some(" claude-haiku-4-5-20251001 "), "w").unwrap();
    assert_eq!(models(&doc), (None, Some("claude-haiku-4-5-20251001".into())));
    set(&mut doc, DefaultModelRole::Manager, Some("claude-sonnet-5-5"), "m").unwrap();
    set(&mut doc, DefaultModelRole::Worker, Some("  "), "clear").unwrap();
    assert_eq!(models(&doc), (Some("claude-sonnet-5-5".into()), None));
    assert_eq!(
        set(&mut doc, DefaultModelRole::Worker, Some("bad\nid"), "bad").unwrap_err().code,
        WorkOutlineErrorCode::Invalid
    );
    for kind in [WorkOutlineActorKind::Agent, WorkOutlineActorKind::System] {
        let refused = apply_as(&mut doc, kind, WorkOutlineMutation::SetRoleDefaultModel(SetRoleDefaultModelParams {
            space_id: SPACE.into(), role: DefaultModelRole::Worker, model: Some("claude-opus-5-5".into()),
            operation_id: format!("agent-{kind:?}"),
        }));
        assert_eq!(refused.unwrap_err().code, WorkOutlineErrorCode::Unauthorized, "{kind:?}");
    }
    assert_eq!(models(&doc), (Some("claude-sonnet-5-5".into()), None));
}

#[test]
fn spaces_default_to_linear_off_and_the_setting_is_lww() {
    let mut doc = doc();
    assert!(!snap(&doc).space_settings.linear_enabled);
    assert!(!doc.space_linear_enabled(SPACE));
    // Turning off what is already off writes nothing.
    let before = doc.batches_written();
    enable(&mut doc, SPACE, "off-0", false);
    assert_eq!(doc.batches_written(), before);
    enable(&mut doc, SPACE, "on-1", true);
    assert!(snap(&doc).space_settings.linear_enabled);
    assert_eq!(doc.linear_enabled_space_ids(), vec![SPACE.to_owned()]);
    assert!(
        !doc.work_outline_snapshot(OFF_SPACE, true)
            .space_settings
            .linear_enabled
    );
    let row = doc
        .overlay_row(KIND_WORK_OUTLINE_SPACE_SETTINGS, SPACE)
        .unwrap();
    assert_eq!(row.fields["spaceId"], json!(SPACE));
    enable(&mut doc, SPACE, "off-2", false);
    assert!(!snap(&doc).space_settings.linear_enabled);
    assert!(doc.linear_enabled_space_ids().is_empty());
    // Unknown Space.
    let err = apply(
        &mut doc,
        WorkOutlineMutation::SetSpaceLinearEnabled(SetSpaceLinearEnabledParams {
            space_id: "nope".into(),
            enabled: true,
            operation_id: "x".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::NotFound);
}

#[test]
fn create_from_linear_writes_item_and_reference_in_one_batch_and_replays() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    let before = doc.batches_written();
    let reply = from_linear(
        &mut doc,
        SPACE,
        "op-1",
        None,
        issue("ENG-123", "Fix login", Some("Steps\n")),
    )
    .unwrap();
    assert_eq!(doc.batches_written(), before + 1, "one batch");
    let id = reply.work_item_id.clone().unwrap();
    assert_eq!(id, derive_id("op-1", roles::ITEM));
    let s = snap(&doc);
    let item = s.item(&id).unwrap();
    assert_eq!(item.title, "ENG-123 · Fix login");
    assert_eq!(item.description.as_deref(), Some("Steps"));
    assert_eq!(item.status, WorkItemStatus::Pending);
    assert!(item.external_sync_modified_fields.is_empty());
    let reference = s.linear_issue_reference(&id).unwrap();
    assert_eq!(Some(&reference.id), reply.reference_id.as_ref());
    assert_eq!(reference.external_id, "ENG-123");
    assert_eq!(reference.url, "https://linear.app/acme/issue/ENG-123");
    assert_eq!(reference.cached_title.as_deref(), Some("Fix login"));
    // Replay: same ids, no write.
    let written = doc.batches_written();
    let again = from_linear(
        &mut doc,
        SPACE,
        "op-1",
        None,
        issue("ENG-123", "Fix login", Some("Steps\n")),
    )
    .unwrap();
    assert_eq!(again, reply);
    assert_eq!(doc.batches_written(), written);
    // Same operation id, other inputs: conflict.
    let err = from_linear(&mut doc, SPACE, "op-1", None, issue("ENG-9", "x", None)).unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::OperationConflict);
    // Under a parent.
    let child = from_linear(
        &mut doc,
        SPACE,
        "op-2",
        Some(&id),
        issue("ENG-7", "Sub", None),
    )
    .unwrap()
    .work_item_id
    .unwrap();
    let s = snap(&doc);
    assert_eq!(
        s.item(&child).unwrap().parent_work_item_id.as_deref(),
        Some(id.as_str())
    );
    assert_eq!(s.item(&child).unwrap().description, None);
}

#[test]
fn linear_operations_are_refused_in_a_space_with_linear_off() {
    let mut doc = doc();
    let err =
        from_linear(&mut doc, OFF_SPACE, "op-1", None, issue("ENG-1", "x", None)).unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::Invalid);
    assert_eq!(err.message, LINEAR_DISABLED_MESSAGE);
    let item = plain_item(&mut doc, "plain", "Plain", None);
    let err = apply(
        &mut doc,
        WorkOutlineMutation::LinkLinearProject(LinkLinearProjectParams {
            work_item_id: item.clone(),
            project: LinearProjectRef {
                id: "p-1".into(),
                name: "Auth".into(),
                url: "https://linear.app/acme/project/auth".into(),
            },
            operation_id: "link".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.message, LINEAR_DISABLED_MESSAGE);
    assert!(snap(&doc).external_references.is_empty());
}

#[test]
fn agents_cannot_run_linear_operations() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    let mut agent = actor(WorkOutlineActorKind::Agent);
    agent.chat_id = Some("chat-1".into());
    let err = doc
        .apply_work_outline_mutation(
            &WorkOutlineMutation::SetSpaceLinearEnabled(SetSpaceLinearEnabledParams {
                space_id: SPACE.into(),
                enabled: false,
                operation_id: "agent-off".into(),
            }),
            &agent,
            NOW,
        )
        .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::Unauthorized);
    assert!(doc.space_linear_enabled(SPACE));
}

#[test]
fn link_linear_project_adds_a_project_reference() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    let item = plain_item(&mut doc, "plain", "Plain", None);
    let reply = apply(
        &mut doc,
        WorkOutlineMutation::LinkLinearProject(LinkLinearProjectParams {
            work_item_id: item.clone(),
            project: LinearProjectRef {
                id: "p-1".into(),
                name: "Auth".into(),
                url: "https://linear.app/acme/project/auth".into(),
            },
            operation_id: "link".into(),
        }),
    )
    .unwrap();
    let s = snap(&doc);
    let reference = s.linear_project_reference(&item).unwrap();
    assert_eq!(Some(&reference.id), reply.reference_id.as_ref());
    assert_eq!(reference.external_id, "p-1");
    assert_eq!(reference.cached_title.as_deref(), Some("Auth"));
    assert_eq!(
        s.nearest_linear_reference(&item, LINEAR_PROJECT_REFERENCE)
            .map(|r| r.id.clone()),
        reply.reference_id
    );
}

#[test]
fn user_edits_mark_fields_and_system_edits_do_not() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    let id = from_linear(
        &mut doc,
        SPACE,
        "op-1",
        None,
        issue("ENG-1", "One", Some("Body")),
    )
    .unwrap()
    .work_item_id
    .unwrap();
    // The sync writer (system actor) changes text without marking.
    update(
        &mut doc,
        WorkOutlineActorKind::System,
        "sync-1",
        &id,
        WorkItemChanges {
            title: Some("ENG-1 · One v2".into()),
            description: FieldPatch::Set("Body v2".into()),
            ..Default::default()
        },
    );
    assert!(modified(&doc, &id).is_empty());
    // A user status change marks nothing.
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        "status",
        &id,
        WorkItemChanges {
            status: Some(WorkItemStatus::Active),
            ..Default::default()
        },
    );
    assert!(modified(&doc, &id).is_empty());
    // Setting the same title is not an edit.
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        "same",
        &id,
        WorkItemChanges {
            title: Some("ENG-1 · One v2".into()),
            ..Default::default()
        },
    );
    assert!(modified(&doc, &id).is_empty());
    // A user title edit marks title, in the same op as the title.
    let before = doc.batches_written();
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        "rename",
        &id,
        WorkItemChanges {
            title: Some("My own title".into()),
            ..Default::default()
        },
    );
    assert_eq!(doc.batches_written(), before + 1);
    assert_eq!(modified(&doc, &id), vec!["title".to_owned()]);
    let row = doc.overlay_row(KIND_WORK_ITEMS, &id).unwrap();
    assert_eq!(
        row.clocks.get("title"),
        row.clocks.get(MODIFIED_FIELDS_FIELD)
    );
    // Then the description (cleared counts as an edit).
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        "clear",
        &id,
        WorkItemChanges {
            description: FieldPatch::Clear,
            ..Default::default()
        },
    );
    assert_eq!(
        modified(&doc, &id),
        vec!["description".to_owned(), "title".to_owned()]
    );
    // Unlinking the issue clears the marks.
    let reference = snap(&doc).linear_issue_reference(&id).unwrap().id.clone();
    apply(
        &mut doc,
        WorkOutlineMutation::RemoveWorkItemExternalReference(
            RemoveWorkItemExternalReferenceParams {
                reference_id: reference,
                operation_id: "unlink".into(),
            },
        ),
    )
    .unwrap();
    assert!(modified(&doc, &id).is_empty());
    assert!(snap(&doc).linear_issue_reference(&id).is_none());
}

#[test]
fn edits_of_unlinked_items_mark_nothing() {
    let mut doc = doc();
    let id = plain_item(&mut doc, "plain", "Plain", None);
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        "rename",
        &id,
        WorkItemChanges {
            title: Some("Renamed".into()),
            ..Default::default()
        },
    );
    assert!(modified(&doc, &id).is_empty());
}

#[test]
fn linking_an_issue_keeps_an_existing_description() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    let with_text = plain_item(&mut doc, "a", "Mine", Some("My notes"));
    let without = plain_item(&mut doc, "b", "Bare", None);
    link_issue(&mut doc, "link-a", &with_text, "ENG-1");
    link_issue(&mut doc, "link-b", &without, "ENG-2");
    assert_eq!(modified(&doc, &with_text), vec!["description".to_owned()]);
    assert!(modified(&doc, &without).is_empty());
    // A second issue reference: removing one keeps the marks.
    let second = link_issue(&mut doc, "link-a2", &with_text, "ENG-3");
    apply(
        &mut doc,
        WorkOutlineMutation::RemoveWorkItemExternalReference(
            RemoveWorkItemExternalReferenceParams {
                reference_id: second,
                operation_id: "unlink-a2".into(),
            },
        ),
    )
    .unwrap();
    assert_eq!(modified(&doc, &with_text), vec!["description".to_owned()]);
}

#[test]
fn space_deletion_removes_the_settings_row() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    doc.delete_space(SPACE).unwrap();
    assert_eq!(
        doc.row_state(KIND_WORK_OUTLINE_SPACE_SETTINGS, SPACE),
        RowState::Tombstoned
    );
    assert!(doc.linear_enabled_space_ids().is_empty());
}

#[test]
fn system_writes_never_overwrite_a_field_the_user_took_over() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    let id = from_linear(
        &mut doc,
        SPACE,
        "op-1",
        None,
        issue("ENG-1", "One", Some("Body")),
    )
    .unwrap()
    .work_item_id
    .unwrap();
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        "rename",
        &id,
        WorkItemChanges {
            title: Some("Mine".into()),
            ..Default::default()
        },
    );
    // A sync that read the item before the rename still sends a title.
    update(
        &mut doc,
        WorkOutlineActorKind::System,
        "sync",
        &id,
        WorkItemChanges {
            title: Some("ENG-1 · One v2".into()),
            description: FieldPatch::Set("Body v2".into()),
            ..Default::default()
        },
    );
    let s = snap(&doc);
    let item = s.item(&id).unwrap();
    assert_eq!(item.title, "Mine");
    assert_eq!(item.description.as_deref(), Some("Body v2"));
    assert_eq!(item.status, WorkItemStatus::Pending);
}

#[test]
fn space_linear_account_defaults_and_updates_independently() {
    let mut doc = doc();
    assert_eq!(snap(&doc).space_settings.linear_account_id, None);
    let set = |account_id, operation_id: &str| {
        WorkOutlineMutation::SetSpaceLinearAccount(SetSpaceLinearAccountParams {
            space_id: SPACE.into(),
            account_id,
            operation_id: operation_id.into(),
        })
    };
    apply(&mut doc, set(Some("personal".into()), "account-1")).unwrap();
    assert_eq!(
        snap(&doc).space_settings.linear_account_id.as_deref(),
        Some("personal")
    );
    assert_eq!(
        doc.work_outline_snapshot(OFF_SPACE, true)
            .space_settings
            .linear_account_id,
        None
    );
    let batches = doc.batches_written();
    apply(&mut doc, set(Some("personal".into()), "account-2")).unwrap();
    assert_eq!(doc.batches_written(), batches);
    apply(&mut doc, set(None, "account-3")).unwrap();
    assert_eq!(snap(&doc).space_settings.linear_account_id, None);
}

#[test]
fn manager_read_only_setter_is_user_only_project_only_and_replays_an_operation_once() {
    use mast_proto::work_outline::SetManagerReadOnlyParams;
    let mut doc = doc();
    let item = plain_item(&mut doc, "item-op", "Project", None);
    let set = |doc: &mut RegistryDoc, kind, read_only: bool, op: &str| {
        apply_as(doc, kind, WorkOutlineMutation::SetManagerReadOnly(SetManagerReadOnlyParams {
            work_item_id: item.clone(), read_only, operation_id: op.into(),
        }))
    };
    let setting = |doc: &RegistryDoc| mast_proto::StandingRole::manager_runs_read_only(
        snap(doc).standing_role(mast_proto::StandingRoleKind::Manager, &item));
    let user = WorkOutlineActorKind::User;

    assert_eq!(set(&mut doc, user, false, "not-yet").unwrap_err().code, WorkOutlineErrorCode::Invalid, "not a project");
    apply(&mut doc, WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
        work_item_id: item.clone(),
        changes: WorkItemChanges { is_project: Some(true), ..Default::default() },
        operation_id: "mark".into(),
    })).unwrap();
    assert!(setting(&doc), "a project that never chose is read-only");
    assert!(snap(&doc).standing_roles.is_empty());

    for kind in [WorkOutlineActorKind::Agent, WorkOutlineActorKind::System] {
        assert_eq!(set(&mut doc, kind, false, "agent").unwrap_err().code, WorkOutlineErrorCode::Unauthorized, "{kind:?}");
    }
    assert!(setting(&doc));

    set(&mut doc, user, false, "off-1").unwrap();
    assert!(!setting(&doc), "off before any Manager exists keeps a placeholder record");
    assert_eq!(snap(&doc).standing_roles[0].current_chat_id, None);
    set(&mut doc, user, true, "on-1").unwrap();
    assert!(setting(&doc));
    // A retry of an earlier operation applies nothing, even after a later change.
    set(&mut doc, user, false, "off-1").unwrap();
    assert!(setting(&doc));
    set(&mut doc, user, false, "off-2").unwrap();
    assert!(!setting(&doc));
}

#[test]
fn manager_worker_model_setter_keeps_read_only_replays_once_and_clears_to_default() {
    use mast_proto::work_outline::{SetManagerReadOnlyParams, SetManagerWorkerModelParams};
    use mast_proto::{StandingRoleKind, StandingRoleModel};
    let mut doc = doc();
    let item = plain_item(&mut doc, "item-wm", "Project", None);
    let model = |name: &str| StandingRoleModel { harness: "claude-code".into(), model: Some(name.into()) };
    let set = |doc: &mut RegistryDoc, kind, worker_model: Option<StandingRoleModel>, op: &str| {
        apply_as(doc, kind, WorkOutlineMutation::SetManagerWorkerModel(SetManagerWorkerModelParams {
            work_item_id: item.clone(), worker_model, operation_id: op.into(),
        }))
    };
    let current = |doc: &RegistryDoc| snap(doc).standing_role(StandingRoleKind::Manager, &item).and_then(|r| r.worker_model.clone());
    let user = WorkOutlineActorKind::User;

    assert_eq!(set(&mut doc, user, Some(model("a")), "early").unwrap_err().code, WorkOutlineErrorCode::Invalid, "not a project");
    apply(&mut doc, WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
        work_item_id: item.clone(),
        changes: WorkItemChanges { is_project: Some(true), ..Default::default() },
        operation_id: "mark".into(),
    })).unwrap();
    for kind in [WorkOutlineActorKind::Agent, WorkOutlineActorKind::System] {
        assert_eq!(set(&mut doc, kind, Some(model("a")), "agent").unwrap_err().code, WorkOutlineErrorCode::Unauthorized);
    }
    let empty = StandingRoleModel { harness: String::new(), model: None };
    assert_eq!(set(&mut doc, user, Some(empty), "bad").unwrap_err().code, WorkOutlineErrorCode::Invalid);
    let typo = StandingRoleModel { harness: "claude_code".into(), model: None };
    assert_eq!(set(&mut doc, user, Some(typo), "typo").unwrap_err().code, WorkOutlineErrorCode::Invalid, "unknown harness");
    assert_eq!(current(&doc), None);

    apply(&mut doc, WorkOutlineMutation::SetManagerReadOnly(SetManagerReadOnlyParams {
        work_item_id: item.clone(), read_only: false, operation_id: "ro".into(),
    })).unwrap();
    set(&mut doc, user, Some(model("opus")), "wm-1").unwrap();
    assert_eq!(current(&doc), Some(model("opus")));
    assert_eq!(snap(&doc).standing_role(StandingRoleKind::Manager, &item).unwrap().read_only, Some(false), "read-only survives");
    set(&mut doc, user, None, "wm-2").unwrap();
    assert_eq!(current(&doc), None, "cleared to the built-in default");
    set(&mut doc, user, Some(model("opus")), "wm-1").unwrap();
    assert_eq!(current(&doc), None, "a retried operation applies once");
}

#[test]
fn manager_fallback_chain_setter_validates_replays_once_and_keeps_other_settings() {
    use mast_proto::work_outline::{SetManagerFallbackChainParams, SetManagerWorkerModelParams};
    use mast_proto::{StandingRoleKind, StandingRoleModel};
    let mut doc = doc();
    let item = plain_item(&mut doc, "item-fc", "Project", None);
    let entry = |harness: &str, model: Option<&str>| StandingRoleModel { harness: harness.into(), model: model.map(str::to_owned) };
    let set = |doc: &mut RegistryDoc, kind, chain: Vec<StandingRoleModel>, op: &str| {
        apply_as(doc, kind, WorkOutlineMutation::SetManagerFallbackChain(SetManagerFallbackChainParams {
            work_item_id: item.clone(), fallback_chain: chain, operation_id: op.into(),
        }))
    };
    let current = |doc: &RegistryDoc| snap(doc).standing_role(StandingRoleKind::Manager, &item).map(|r| r.fallback_chain.clone());
    let user = WorkOutlineActorKind::User;

    assert_eq!(set(&mut doc, user, vec![entry("codex", None)], "early").unwrap_err().code, WorkOutlineErrorCode::Invalid, "not a project");
    apply(&mut doc, WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
        work_item_id: item.clone(),
        changes: WorkItemChanges { is_project: Some(true), ..Default::default() },
        operation_id: "mark".into(),
    })).unwrap();
    for kind in [WorkOutlineActorKind::Agent, WorkOutlineActorKind::System] {
        assert_eq!(set(&mut doc, kind, vec![entry("codex", None)], "agent").unwrap_err().code, WorkOutlineErrorCode::Unauthorized);
    }
    assert_eq!(set(&mut doc, user, vec![entry("", None)], "empty").unwrap_err().code, WorkOutlineErrorCode::Invalid);
    assert_eq!(set(&mut doc, user, vec![entry("claude_code", None)], "typo").unwrap_err().code, WorkOutlineErrorCode::Invalid, "unknown harness");
    let nine: Vec<_> = (0..9).map(|_| entry("codex", None)).collect();
    assert_eq!(set(&mut doc, user, nine, "nine").unwrap_err().code, WorkOutlineErrorCode::Invalid, "more than 8 entries");
    assert_eq!(current(&doc), None);

    apply(&mut doc, WorkOutlineMutation::SetManagerWorkerModel(SetManagerWorkerModelParams {
        work_item_id: item.clone(), worker_model: Some(entry("claude-code", Some("opus"))), operation_id: "wm".into(),
    })).unwrap();
    let chain = vec![entry("codex", None), entry("claude-code", Some("claude-sonnet-5-5"))];
    set(&mut doc, user, chain.clone(), "fc-1").unwrap();
    assert_eq!(current(&doc), Some(chain.clone()));
    let role = snap(&doc).standing_role(StandingRoleKind::Manager, &item).unwrap().clone();
    assert_eq!(role.worker_model, Some(entry("claude-code", Some("opus"))), "the worker model survives");
    set(&mut doc, user, Vec::new(), "fc-2").unwrap();
    assert_eq!(current(&doc), Some(Vec::new()), "an empty chain turns the fallback off");
    set(&mut doc, user, chain, "fc-1").unwrap();
    assert_eq!(current(&doc), Some(Vec::new()), "a retried operation applies once");
}
