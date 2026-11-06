use mast_proto::CreateWorkItemInstructionParams;
use mast_proto::work_outline::{
    CreateWorkItemParams, WorkItemView, WorkOutlineActor, WorkOutlineActorKind,
    WorkOutlineErrorCode, WorkOutlineMutation, WorkOutlinePlacement, WorkOutlineSnapshot,
};
use mast_proto::work_outline_container_templates::{
    ApplyContainerTemplateParams, ContainerTemplate, RuleInstruction, RuleNode,
    SetContainerTemplateParams,
};
use mast_proto::work_outline_instructions::InstructionScope;
use mast_proto::work_outline_user_actions::{
    ApproveWorkItemInstructionProposalParams, InstructionProposalOrigin,
    InstructionProposalTarget, ProposalAction, ProposeWorkItemInstructionParams,
};

use crate::registry::RegistryDoc;
use crate::registry::tests::{chat, space};

const SPACE: &str = "sp-1";
const NOW: i64 = 1_700_000_000_000;

fn actor(kind: WorkOutlineActorKind) -> WorkOutlineActor {
    WorkOutlineActor {
        kind,
        device_id: "dev-a".into(),
        chat_id: (kind == WorkOutlineActorKind::Agent).then(|| "c1".into()),
        message_id: None,
    }
}

fn doc() -> RegistryDoc {
    let mut doc = RegistryDoc::new("dev-a");
    doc.upsert_space(&space(SPACE, "dev-a", "/tmp/one")).unwrap();
    let mut c = chat("c1", "dev-a");
    c.space_id = Some(SPACE.into());
    c.parent_chat_id = None;
    doc.upsert_chat(&c).unwrap();
    doc
}

fn run(
    doc: &mut RegistryDoc,
    kind: WorkOutlineActorKind,
    m: WorkOutlineMutation,
) -> Result<(), mast_proto::WorkOutlineError> {
    doc.apply_work_outline_mutation(&m, &actor(kind), NOW)
        .map(|_| ())
}

fn create(doc: &mut RegistryDoc, op: &str, parent: Option<&str>, title: &str) -> String {
    doc.apply_work_outline_mutation(
        &WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: parent.map(str::to_owned),
            placement: WorkOutlinePlacement::Last,
            title: title.into(),
            description: None,
            status: None,
            operation_id: op.into(),
        }),
        &actor(WorkOutlineActorKind::User),
        NOW,
    )
    .unwrap()
    .work_item_id
    .unwrap()
}

fn node(id: &str, title: &str) -> RuleNode {
    RuleNode {
        id: id.into(),
        title: title.into(),
        ..Default::default()
    }
}

fn ins(id: &str, text: &str) -> RuleInstruction {
    RuleInstruction {
        id: id.into(),
        text: text.into(),
    }
}

/// Each child is a project: Planning (instruction) + Tickets (destination).
fn projects_rules() -> ContainerTemplate {
    let mut planning = node("n-plan", "Planning");
    planning.instructions = vec![ins("i-plan", "Plan first")];
    let mut tickets = node("n-tix", "Tickets");
    tickets.is_worker_destination = true;
    let mut root = node("n-root", "");
    root.is_project = true;
    root.children = vec![planning, tickets];
    ContainerTemplate { revision: 0, root }
}

fn set_rules(doc: &mut RegistryDoc, op: &str, container: &str, t: Option<ContainerTemplate>) {
    run(
        doc,
        WorkOutlineActorKind::User,
        WorkOutlineMutation::SetContainerTemplate(SetContainerTemplateParams {
            operation_id: op.into(),
            space_id: SPACE.into(),
            work_item_id: container.into(),
            template: t,
        }),
    )
    .unwrap();
}

fn snap(doc: &RegistryDoc) -> WorkOutlineSnapshot {
    doc.work_outline_snapshot(SPACE, true)
}

fn child_titled<'a>(s: &'a WorkOutlineSnapshot, parent: &str, title: &str) -> &'a WorkItemView {
    s.items
        .iter()
        .find(|i| i.parent_work_item_id.as_deref() == Some(parent) && i.title == title)
        .unwrap_or_else(|| panic!("no {title} under {parent}"))
}

fn live_instructions(s: &WorkOutlineSnapshot, item: &str) -> Vec<String> {
    s.instructions
        .iter()
        .filter(|i| i.work_item_id.as_deref() == Some(item) && !i.archived())
        .map(|i| i.text.clone())
        .collect()
}

#[test]
fn a_new_child_gets_the_whole_structure_at_once() {
    let mut d = doc();
    let projects = create(&mut d, "op-projects", None, "Projects");
    set_rules(&mut d, "op-set", &projects, Some(projects_rules()));
    let p = create(&mut d, "op-p1", Some(&projects), "Checkout");
    let s = snap(&d);
    let project = s.item(&p).unwrap();
    assert!(project.is_project);
    assert_eq!(project.template_node_id.as_deref(), Some("n-root"));
    let planning = child_titled(&s, &p, "Planning");
    let tickets = child_titled(&s, &p, "Tickets");
    assert_eq!(planning.template_node_id.as_deref(), Some("n-plan"));
    assert_eq!(project.worker_destination_id.as_deref(), Some(tickets.id.as_str()));
    assert_eq!(live_instructions(&s, &planning.id), vec!["Plan first"]);
    // The instruction reaches the item's effective set.
    let eff = s.effective_instructions(&planning.id);
    assert!(eff.entries.iter().any(|e| e.text == "Plan first"));
}

#[test]
fn followed_rule_resolution_checks_the_template_path() {
    let mut d = doc();
    let projects = create(&mut d, "resolve-projects", None, "Projects");
    set_rules(&mut d, "resolve-rules", &projects, Some(projects_rules()));
    let project = create(&mut d, "resolve-project", Some(&projects), "Checkout");
    let planning = child_titled(&snap(&d), &project, "Planning").id.clone();
    let task = create(&mut d, "resolve-task", Some(&planning), "Task");
    assert_eq!(
        d.followed_template_rule(SPACE, &task),
        Some((projects.clone(), "n-plan".into(), planning.clone()))
    );
    assert_eq!(d.followed_template_rule(SPACE, &projects), None);
    let mut template = snap(&d).item(&projects).unwrap().container_template.clone().unwrap();
    template.root.children.remove(0);
    set_rules(&mut d, "resolve-remove", &projects, Some(template));
    assert_eq!(d.followed_template_rule(SPACE, &task), Some((projects, "n-root".into(), project)));
}

#[test]
fn replaying_the_creation_adds_nothing() {
    let mut d = doc();
    let projects = create(&mut d, "op-projects", None, "Projects");
    set_rules(&mut d, "op-set", &projects, Some(projects_rules()));
    create(&mut d, "op-p1", Some(&projects), "Checkout");
    let before = snap(&d).items.len();
    create(&mut d, "op-p1", Some(&projects), "Checkout");
    assert_eq!(snap(&d).items.len(), before);
}

#[test]
fn editing_the_rules_propagates_and_never_deletes() {
    let mut d = doc();
    let projects = create(&mut d, "op-projects", None, "Projects");
    set_rules(&mut d, "op-set", &projects, Some(projects_rules()));
    let p = create(&mut d, "op-p1", Some(&projects), "Checkout");
    let planning_id = child_titled(&snap(&d), &p, "Planning").id.clone();

    // Instruction edit, rename, new node, removed node.
    let mut t = snap(&d).item(&projects).unwrap().container_template.clone().unwrap();
    t.root.children[0].title = "Plan".into();
    t.root.children[0].instructions[0].text = "Plan carefully".into();
    t.root.children.remove(1); // Tickets removed
    let mut review = node("n-rev", "Review");
    review.instructions = vec![ins("i-rev", "Review it")];
    t.root.children.push(review);
    set_rules(&mut d, "op-set2", &projects, Some(t));

    let s = snap(&d);
    assert_eq!(s.item(&planning_id).unwrap().title, "Plan");
    assert_eq!(live_instructions(&s, &planning_id), vec!["Plan carefully"]);
    let review = child_titled(&s, &p, "Review");
    assert_eq!(live_instructions(&s, &review.id), vec!["Review it"]);
    // Tickets was removed from the rules: detached, still there, not deleted.
    let tickets = child_titled(&s, &p, "Tickets");
    assert!(tickets.template_node_id.is_none());
    assert!(tickets.archived_at.is_none());
}

#[test]
fn planning_template_propagation_does_not_write() {
    let mut d = doc();
    let projects = create(&mut d, "projects-plan", None, "Projects");
    set_rules(&mut d, "rules-plan", &projects, Some(projects_rules()));
    let child = create(&mut d, "child-plan", Some(&projects), "Checkout");
    let planning = child_titled(&snap(&d), &child, "Planning").id.clone();
    let mut template = snap(&d).item(&projects).unwrap().container_template.clone().unwrap();
    template.root.children[0].instructions[0].text = "Plan carefully".into();
    let params = SetContainerTemplateParams {
        operation_id: "update-plan".into(),
        space_id: SPACE.into(),
        work_item_id: projects.clone(),
        template: Some(template),
    };
    let plan = d.plan_container_template(&params, &actor(WorkOutlineActorKind::User), NOW).unwrap();
    assert!(!plan.is_empty());
    assert_eq!(live_instructions(&snap(&d), &planning), vec!["Plan first"]);
    d.set_container_template(&params, &actor(WorkOutlineActorKind::User), NOW).unwrap();
    assert_eq!(live_instructions(&snap(&d), &planning), vec!["Plan carefully"]);
}

#[test]
fn template_proposals_propagate_or_add_an_own_instruction() {
    use mast_proto::work_outline_user_actions::ProposalTargetScope;
    let mut d = doc();
    let projects = create(&mut d, "proposal-projects", None, "Projects");
    set_rules(&mut d, "proposal-rules", &projects, Some(projects_rules()));
    let first = create(&mut d, "proposal-first", Some(&projects), "First");
    let second = create(&mut d, "proposal-second", Some(&projects), "Second");
    let first_plan = child_titled(&snap(&d), &first, "Planning").id.clone();
    let second_plan = child_titled(&snap(&d), &second, "Planning").id.clone();
    let default_target = d.apply_work_outline_mutation(&WorkOutlineMutation::ProposeWorkItemInstruction(ProposeWorkItemInstructionParams {
        space_id: Some(SPACE.into()), target: InstructionProposalTarget {
            scope: ProposalTargetScope::ChildTemplate, work_item_id: Some(first.clone()),
            container_work_item_id: Some(projects.clone()), ..Default::default()
        }, origin: InstructionProposalOrigin::default(), text: "Review the project".into(),
        action: ProposalAction::Add, instruction_id: None, rationale: None, operation_id: "proposal-default-rule".into(),
    }), &actor(WorkOutlineActorKind::User), NOW).unwrap().proposal_id.unwrap();
    let root_id = snap(&d).item(&projects).unwrap().container_template.as_ref().unwrap().root.id.clone();
    assert_eq!(d.instruction_proposal(&default_target).unwrap().target.rule_node_id.as_deref(), Some(root_id.as_str()));
    let row = snap(&d).instructions.iter().find(|i| i.work_item_id.as_deref() == Some(&first_plan)).unwrap().clone();
    // A child row may already have drifted due to the original approval bug.
    d.commit(vec![super::super::Planned::update(super::super::KIND_WORK_ITEM_INSTRUCTIONS, &row.id,
        std::collections::BTreeMap::from([("text".into(), serde_json::json!("Drifted"))]))], &[]).unwrap();
    let propose = |d: &mut RegistryDoc, op: &str, action, instruction_id: Option<String>, target, text: &str| {
        d.apply_work_outline_mutation(&WorkOutlineMutation::ProposeWorkItemInstruction(ProposeWorkItemInstructionParams {
            space_id: Some(SPACE.into()), target, origin: InstructionProposalOrigin::default(),
            text: text.into(), action, instruction_id, rationale: None, operation_id: op.into(),
        }), &actor(WorkOutlineActorKind::User), NOW).unwrap().proposal_id.unwrap()
    };
    let approve = |d: &mut RegistryDoc, id: String, op: &str, text: &str, project_only| {
        run(d, WorkOutlineActorKind::User, WorkOutlineMutation::ApproveWorkItemInstructionProposal(
            ApproveWorkItemInstructionProposalParams { proposal_id: id, approved_text: text.into(), placement: None,
                allow_stale: false, project_only, operation_id: op.into() },
        )).unwrap();
    };
    let mut legacy = d.instruction_proposal(&default_target).unwrap().target;
    legacy.rule_node_id = None;
    d.commit(vec![super::super::Planned::update(super::super::user_actions::KIND_WORK_ITEM_INSTRUCTION_PROPOSALS, &default_target,
        std::collections::BTreeMap::from([("target".into(), serde_json::json!(legacy))]))], &[]).unwrap();
    approve(&mut d, default_target, "approve-legacy-root", "Review the project", false);
    assert!(live_instructions(&snap(&d), &second).contains(&"Review the project".to_owned()));
    let edit = propose(&mut d, "proposal-edit", ProposalAction::Edit, Some(row.id.clone()), InstructionProposalTarget::default(), "Plan carefully");
    assert_eq!(d.instruction_proposal(&edit).unwrap().target.scope, ProposalTargetScope::ChildTemplate);
    assert_eq!(d.instruction_proposal(&edit).unwrap().instruction_id.as_deref(), Some("i-plan"));
    assert_eq!(d.instruction_proposal(&edit).unwrap().base_text.as_deref(), Some("Plan first"));
    approve(&mut d, edit, "approve-edit", "Plan carefully", false);
    assert_eq!(live_instructions(&snap(&d), &first_plan), vec!["Plan carefully"]);
    assert_eq!(live_instructions(&snap(&d), &second_plan), vec!["Plan carefully"]);
    let local = propose(&mut d, "proposal-local", ProposalAction::Edit, Some(row.id.clone()), InstructionProposalTarget::default(), "Local detail");
    let before_local = snap(&d).item(&projects).unwrap().container_template.clone();
    approve(&mut d, local, "approve-local", "Local detail", true);
    assert_eq!(snap(&d).item(&projects).unwrap().container_template, before_local);
    assert_eq!(live_instructions(&snap(&d), &first_plan), vec!["Plan carefully", "Local detail"]);
    assert_eq!(live_instructions(&snap(&d), &second_plan), vec!["Plan carefully"]);
    let add = propose(&mut d, "proposal-add", ProposalAction::Add, None, InstructionProposalTarget {
        scope: ProposalTargetScope::ChildTemplate, work_item_id: Some(first.clone()),
        rule_node_path: vec!["Planning".into()], ..Default::default()
    }, "Review plans");
    approve(&mut d, add, "approve-add", "Review plans", false);
    assert!(live_instructions(&snap(&d), &second_plan).contains(&"Review plans".to_owned()));
    let remove = propose(&mut d, "proposal-remove", ProposalAction::Remove, Some(row.id), InstructionProposalTarget::default(), "");
    approve(&mut d, remove, "approve-remove", "", false);
    let s = snap(&d);
    assert!(!live_instructions(&s, &second_plan).contains(&"Plan carefully".to_owned()));
    assert!(live_instructions(&s, &first_plan).contains(&"Local detail".to_owned()));
}

#[test]
fn legacy_proposal_cannot_write_a_followed_instruction() {
    use std::collections::BTreeMap;
    use serde_json::json;
    let mut d = doc();
    let projects = create(&mut d, "legacy-projects", None, "Projects");
    set_rules(&mut d, "legacy-rules", &projects, Some(projects_rules()));
    let project = create(&mut d, "legacy-child", Some(&projects), "Project");
    let planning = child_titled(&snap(&d), &project, "Planning").id.clone();
    let row = snap(&d).instructions.iter().find(|i| i.work_item_id.as_deref() == Some(&planning)).unwrap().clone();
    for (op, action, text) in [("legacy-edit", ProposalAction::Edit, "Changed"), ("legacy-remove", ProposalAction::Remove, "")] {
        let proposal = d.apply_work_outline_mutation(&WorkOutlineMutation::ProposeWorkItemInstruction(ProposeWorkItemInstructionParams {
            space_id: Some(SPACE.into()), target: InstructionProposalTarget::default(), origin: InstructionProposalOrigin::default(),
            text: text.into(), action, instruction_id: Some(row.id.clone()), rationale: None, operation_id: op.into(),
        }), &actor(WorkOutlineActorKind::User), NOW).unwrap().proposal_id.unwrap();
        // Simulate the row shape written before template-aware proposals.
        d.commit(vec![super::super::Planned::update(super::super::user_actions::KIND_WORK_ITEM_INSTRUCTION_PROPOSALS, &proposal,
            BTreeMap::from([("target".into(), json!(InstructionProposalTarget::work_item(&planning))), ("instructionId".into(), json!(row.id))]))], &[]).unwrap();
        let result = run(&mut d, WorkOutlineActorKind::User, WorkOutlineMutation::ApproveWorkItemInstructionProposal(
            ApproveWorkItemInstructionProposalParams { proposal_id: proposal, approved_text: text.into(), placement: None,
                allow_stale: true, project_only: false, operation_id: format!("approve-{op}") }));
        assert!(result.is_err());
        assert_eq!(live_instructions(&snap(&d), &planning), vec!["Plan first"]);
    }
}

#[test]
fn removing_a_node_keeps_its_instructions_on_the_detached_item() {
    let mut d = doc();
    let projects = create(&mut d, "op-projects", None, "Projects");
    set_rules(&mut d, "op-set", &projects, Some(projects_rules()));
    let p = create(&mut d, "op-p1", Some(&projects), "Checkout");
    let planning_id = child_titled(&snap(&d), &p, "Planning").id.clone();
    let mut t = snap(&d).item(&projects).unwrap().container_template.clone().unwrap();
    t.root.children.remove(0);
    set_rules(&mut d, "op-set2", &projects, Some(t));
    let s = snap(&d);
    assert!(s.item(&planning_id).unwrap().template_node_id.is_none());
    assert_eq!(live_instructions(&s, &planning_id), vec!["Plan first"]);
    // Now a plain instruction: editable again.
    let row = s.instructions.iter().find(|i| i.work_item_id.as_deref() == Some(&planning_id)).unwrap();
    assert!(row.template_instruction_id.is_none());
}

#[test]
fn template_instructions_cannot_be_edited_but_can_be_muted() {
    let mut d = doc();
    let projects = create(&mut d, "op-projects", None, "Projects");
    set_rules(&mut d, "op-set", &projects, Some(projects_rules()));
    let p = create(&mut d, "op-p1", Some(&projects), "Checkout");
    let s = snap(&d);
    let planning = child_titled(&s, &p, "Planning").id.clone();
    let row = s
        .instructions
        .iter()
        .find(|i| i.work_item_id.as_deref() == Some(&planning))
        .unwrap()
        .clone();
    assert!(row.template_instruction_id.is_some());
    assert!(d.plan_instruction_text_update(&row, "mine", NOW).is_err());
    let edit = run(
        &mut d,
        WorkOutlineActorKind::User,
        WorkOutlineMutation::UpdateWorkItemInstruction(
            mast_proto::work_outline::UpdateWorkItemInstructionParams {
                instruction_id: row.id.clone(),
                text: "mine".into(),
                operation_id: "op-edit".into(),
            },
        ),
    );
    assert!(edit.is_err());
    // Proposals update the rules instead of writing the followed row directly.
    for (operation, action, text) in [
        ("edit", ProposalAction::Edit, "changed"),
        ("remove", ProposalAction::Remove, ""),
    ] {
        let proposal = d.apply_work_outline_mutation(
            &WorkOutlineMutation::ProposeWorkItemInstruction(ProposeWorkItemInstructionParams {
                space_id: Some(SPACE.into()),
                target: InstructionProposalTarget::default(),
                origin: InstructionProposalOrigin::default(),
                text: text.into(),
                action,
                instruction_id: Some(row.id.clone()),
                rationale: None,
                operation_id: format!("propose-{operation}"),
            }),
            &actor(WorkOutlineActorKind::User),
            NOW,
        ).unwrap().proposal_id.unwrap();
        let approval = run(
            &mut d,
            WorkOutlineActorKind::User,
            WorkOutlineMutation::ApproveWorkItemInstructionProposal(
                ApproveWorkItemInstructionProposalParams {
                    proposal_id: proposal,
                    approved_text: text.into(),
                    placement: None,
                    allow_stale: false,
                    project_only: false,
                    operation_id: format!("approve-{operation}"),
                },
            ),
        );
        assert!(approval.is_ok());
        if action == ProposalAction::Edit {
            assert_eq!(live_instructions(&snap(&d), &planning), vec!["changed"]);
        } else {
            assert!(live_instructions(&snap(&d), &planning).is_empty());
        }
    }
    // A user's own instruction can be added next to it.
    run(
        &mut d,
        WorkOutlineActorKind::User,
        WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
            space_id: SPACE.into(),
            scope: InstructionScope::WorkItem { work_item_id: planning.clone() },
            text: "mine".into(),
            placement: None,
            source: None,
            operation_id: "op-own".into(),
        }),
    )
    .unwrap();
    assert_eq!(live_instructions(&snap(&d), &planning), vec!["mine"]);
}

#[test]
fn apply_adopts_existing_items_by_title_and_creates_the_rest() {
    let mut d = doc();
    let projects = create(&mut d, "op-projects", None, "Projects");
    // Existing child with a matching "planning" item and an extra.
    let old = create(&mut d, "op-old", Some(&projects), "Old project");
    let planning = create(&mut d, "op-oldp", Some(&old), "planning");
    create(&mut d, "op-notes", Some(&old), "Notes");
    set_rules(&mut d, "op-set", &projects, Some(projects_rules()));
    // Setting the rules did not touch the existing child.
    assert!(snap(&d).item(&old).unwrap().template_node_id.is_none());
    run(
        &mut d,
        WorkOutlineActorKind::User,
        WorkOutlineMutation::ApplyContainerTemplate(ApplyContainerTemplateParams {
            operation_id: "op-apply".into(),
            space_id: SPACE.into(),
            work_item_id: projects.clone(),
        }),
    )
    .unwrap();
    let s = snap(&d);
    assert!(s.item(&old).unwrap().is_project);
    assert_eq!(s.item(&planning).unwrap().template_node_id.as_deref(), Some("n-plan"));
    assert_eq!(live_instructions(&s, &planning), vec!["Plan first"]);
    child_titled(&s, &old, "Tickets");
    child_titled(&s, &old, "Notes"); // extras kept
    assert_eq!(s.items.iter().filter(|i| i.title.eq_ignore_ascii_case("planning")).count(), 1);
}

#[test]
fn agents_cannot_set_rules_and_nesting_is_refused() {
    let mut d = doc();
    let projects = create(&mut d, "op-projects", None, "Projects");
    let denied = run(
        &mut d,
        WorkOutlineActorKind::Agent,
        WorkOutlineMutation::SetContainerTemplate(SetContainerTemplateParams {
            operation_id: "op-x".into(),
            space_id: SPACE.into(),
            work_item_id: projects.clone(),
            template: Some(projects_rules()),
        }),
    );
    assert_eq!(denied.unwrap_err().code, WorkOutlineErrorCode::Unauthorized);
    set_rules(&mut d, "op-set", &projects, Some(projects_rules()));
    let p = create(&mut d, "op-p1", Some(&projects), "Checkout");
    let nested = run(
        &mut d,
        WorkOutlineActorKind::User,
        WorkOutlineMutation::SetContainerTemplate(SetContainerTemplateParams {
            operation_id: "op-n".into(),
            space_id: SPACE.into(),
            work_item_id: p,
            template: Some(projects_rules()),
        }),
    );
    assert!(nested.is_err());
}

#[test]
fn turning_the_template_off_detaches_everything() {
    let mut d = doc();
    let projects = create(&mut d, "op-projects", None, "Projects");
    set_rules(&mut d, "op-set", &projects, Some(projects_rules()));
    let p = create(&mut d, "op-p1", Some(&projects), "Checkout");
    set_rules(&mut d, "op-off", &projects, None);
    let s = snap(&d);
    assert!(s.item(&p).unwrap().template_node_id.is_none());
    assert!(child_titled(&s, &p, "Planning").template_node_id.is_none());
    assert!(s.instructions.iter().all(|i| i.template_instruction_id.is_none()));
    assert_eq!(live_instructions(&s, &child_titled(&s, &p, "Planning").id), vec!["Plan first"]);
}
