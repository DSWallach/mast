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
    // Proposals created before template-aware proposals must not mutate the
    // followed row when approved, whether they request an edit or removal.
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
                    operation_id: format!("approve-{operation}"),
                },
            ),
        );
        assert!(approval.is_err());
        assert_eq!(live_instructions(&snap(&d), &planning), vec!["Plan first"]);
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
    assert_eq!(live_instructions(&snap(&d), &planning).len(), 2);
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
