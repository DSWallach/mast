// Container templates: the editor model (build keeps ids, new ones empty, one
// worker destination), the "follows" predicate, and the Template section's
// visibility rules. Pure; nothing reaches the engine.
use super::*;
use crate::work_outline::store::{fixture_ids::*, fixture_snapshot};

fn rule(id: &str, title: &str) -> RuleNode {
    RuleNode {
        id: id.into(),
        title: title.into(),
        ..Default::default()
    }
}

fn rules() -> ContainerTemplate {
    let mut planning = rule("n-plan", "Planning");
    planning.instructions = vec![RuleInstruction {
        id: "i-1".into(),
        text: "Write a plan".into(),
    }];
    let mut tickets = rule("n-tix", "Tickets");
    tickets.is_worker_destination = true;
    let mut root = rule("n-root", "");
    root.is_project = true;
    root.children = vec![planning, tickets];
    ContainerTemplate { revision: 3, root }
}

fn node_key(editor: &TemplateEditor, title: &str) -> u64 {
    editor.nodes().into_iter().find(|n| n.title == title).unwrap().key
}

#[test]
fn building_unedited_rules_round_trips_with_the_same_ids() {
    let editor = TemplateEditor::new(Some(&rules()));
    assert_eq!(editor.build().unwrap(), rules());
}

#[test]
fn new_nodes_and_instructions_have_empty_ids_and_old_ones_keep_theirs() {
    let mut editor = TemplateEditor::new(Some(&rules()));
    let plan = node_key(&editor, "Planning");
    let docs = editor.add_child(plan).unwrap();
    editor.set_title(docs, " Docs ".into());
    let added = editor.add_instruction(docs).unwrap();
    editor.set_instruction_text(added, "Keep it short".into());
    let blank = editor.add_instruction(docs).unwrap();
    editor.set_instruction_text(blank, "   ".into());
    editor.set_title(plan, "Planning!".into());
    let built = editor.build().unwrap();
    assert_eq!(built.revision, 3);
    let planning = &built.root.children[0];
    assert_eq!((planning.id.as_str(), planning.title.as_str()), ("n-plan", "Planning!"));
    assert_eq!(planning.instructions[0].id, "i-1");
    let docs = &planning.children[0];
    assert_eq!((docs.id.as_str(), docs.title.as_str()), ("", "Docs"));
    assert_eq!(docs.instructions.len(), 1, "blank instructions are dropped");
    assert_eq!(docs.instructions[0].id, "");
}

#[test]
fn an_empty_editor_builds_an_empty_root_at_revision_zero() {
    let built = TemplateEditor::new(None).build().unwrap();
    assert_eq!(built.revision, 0);
    assert_eq!(built.root, RuleNode::default());
}

#[test]
fn only_one_worker_destination_and_never_the_root() {
    let mut editor = TemplateEditor::new(Some(&rules()));
    let plan = node_key(&editor, "Planning");
    editor.set_worker_destination(plan, true);
    let built = editor.build().unwrap();
    let flags: Vec<bool> = built.root.children.iter().map(|n| n.is_worker_destination).collect();
    assert_eq!(flags, vec![true, false]);
    let root = editor.root.key;
    editor.set_worker_destination(root, true);
    assert!(!editor.root.is_worker_destination);
}

#[test]
fn build_surfaces_the_checks_message_and_nodes_move_and_remove() {
    let mut editor = TemplateEditor::new(Some(&rules()));
    let plan = node_key(&editor, "Planning");
    let child = editor.add_child(plan).unwrap();
    assert_eq!(editor.build().unwrap_err(), "Every template item needs a title");
    assert!(editor.remove_node(child));
    assert!(!editor.remove_node(editor.root.key));
    assert!(editor.move_node(plan, false));
    assert!(!editor.move_node(plan, false));
    assert_eq!(editor.build().unwrap().root.children[1].title, "Planning");
}

fn follows_fixture() -> WorkOutlineSnapshot {
    let mut snapshot = fixture_snapshot("s1");
    for item in &mut snapshot.items {
        if item.id == LAUNCH {
            item.container_template = Some(rules());
        }
        if item.id == API {
            item.template_node_id = Some("n-root".into());
        }
    }
    snapshot
}

#[test]
fn a_child_follows_iff_its_node_is_the_parents_root() {
    let mut snapshot = follows_fixture();
    let parent = snapshot.item(LAUNCH).unwrap().clone();
    assert!(follows_template(&parent, snapshot.item(API).unwrap()));
    assert!(!follows_template(&parent, snapshot.item(UI).unwrap()));
    snapshot.items.iter_mut().find(|i| i.id == UI).unwrap().template_node_id = Some("other".into());
    assert!(!follows_template(&parent, snapshot.item(UI).unwrap()));
}

#[test]
fn the_template_section_shows_what_each_item_needs() {
    let snapshot = follows_fixture();
    // The container: rules, 1 of 2 children follow, so Apply to all.
    let container = section_state(&snapshot, snapshot.item(LAUNCH).unwrap());
    assert!(container.has_rules && !container.doesnt_follow);
    assert_eq!((container.children, container.following), (2, 1));
    assert!(container.can_apply_all());
    // A following child: nothing to apply.
    let api = section_state(&snapshot, snapshot.item(API).unwrap());
    assert!(!api.has_rules && !api.doesnt_follow && !api.can_apply_all());
    // A child that doesn't follow: the notice and Apply.
    let ui = section_state(&snapshot, snapshot.item(UI).unwrap());
    assert!(ui.doesnt_follow);
    // Everyone follows: no Apply to all.
    let mut all = snapshot.clone();
    all.items.iter_mut().find(|i| i.id == UI).unwrap().template_node_id = Some("n-root".into());
    assert!(!section_state(&all, all.item(LAUNCH).unwrap()).can_apply_all());
    // No rules anywhere: nothing shows.
    let plain = fixture_snapshot("s1");
    assert_eq!(
        section_state(&plain, plain.item(LAUNCH).unwrap()),
        SectionState::default()
    );
    assert!(!section_state(&plain, plain.item(UI).unwrap()).doesnt_follow);
}
