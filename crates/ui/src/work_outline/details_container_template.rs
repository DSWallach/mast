//! Container templates in Work Item Details: the **Template** section.
//!
//! An item can carry rules every DIRECT child follows (the engine creates the
//! whole structure for a new child). The rules are edited only here: a small
//! tree editor ("Each child" = the root, then nested nodes) with a title, a
//! project toggle and a worker-destination toggle per node, and a list of
//! instructions per node. Save sends `setContainerTemplate` (existing ids
//! kept so renames follow; new nodes and instructions carry empty ids the
//! engine assigns); Turn off sends `template: None`.
//!
//! Also here: "Doesn't follow the template" + **Apply child template\u{2026}** on a child
//! of a container whose rules it doesn't follow, and **Apply to all
//! children…** on the container when some child doesn't.
//!
//! The pure model ([`TemplateEditor`], [`section_state`]) is separate from the
//! rendering, like `details_template_review.rs`. A child module of
//! `details_panel`, so it uses the surface's private state directly.
use std::collections::HashMap;

use gpui::{AnyElement, Context, Entity, SharedString, div, prelude::*, px};
use mast_proto::{
    ApplyContainerTemplateParams, ContainerTemplate, RuleInstruction, RuleNode,
    SetContainerTemplateParams, WorkItemView, WorkOutlineMutation, WorkOutlineSnapshot,
};

use super::super::actions::new_operation_id;
use super::{DetailsEvent, WorkItemDetails};
use crate::composer::ComposerInput;
use crate::theme::Theme;

/// One instruction of a node in the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EditorInstruction {
    /// Local identity (the text input's key); never sent.
    pub key: u64,
    /// The rule instruction's id; empty for one added in this session.
    pub id: String,
    pub text: String,
}

/// One node of the rules in the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EditorNode {
    pub key: u64,
    /// Empty for a node added in this session.
    pub id: String,
    pub title: String,
    /// Kept as is (the editor has no field for it).
    pub description: Option<String>,
    pub is_project: bool,
    pub is_worker_destination: bool,
    pub instructions: Vec<EditorInstruction>,
    pub children: Vec<EditorNode>,
}

/// The editable copy of a container's rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TemplateEditor {
    /// The revision being edited (0 = new rules).
    pub revision: u32,
    pub root: EditorNode,
    next_key: u64,
}

impl TemplateEditor {
    /// An editor over `template`, or over an empty root when there are no
    /// rules yet.
    pub(crate) fn new(template: Option<&ContainerTemplate>) -> Self {
        let mut editor = Self {
            revision: template.map_or(0, |template| template.revision),
            root: EditorNode {
                key: 0,
                id: String::new(),
                title: String::new(),
                description: None,
                is_project: false,
                is_worker_destination: false,
                instructions: Vec::new(),
                children: Vec::new(),
            },
            next_key: 1,
        };
        if let Some(template) = template {
            editor.root = editor.import(&template.root);
        }
        editor
    }

    fn fresh_key(&mut self) -> u64 {
        let key = self.next_key;
        self.next_key += 1;
        key
    }

    fn import(&mut self, node: &RuleNode) -> EditorNode {
        EditorNode {
            key: self.fresh_key(),
            id: node.id.clone(),
            title: node.title.clone(),
            description: node.description.clone(),
            is_project: node.is_project,
            is_worker_destination: node.is_worker_destination,
            instructions: node
                .instructions
                .iter()
                .map(|instruction| EditorInstruction {
                    key: self.fresh_key(),
                    id: instruction.id.clone(),
                    text: instruction.text.clone(),
                })
                .collect(),
            children: node.children.iter().map(|child| self.import(child)).collect(),
        }
    }

    /// The rules to send: ids kept, new ones empty, titles and instruction
    /// texts trimmed, blank instructions dropped. Checked with
    /// [`RuleNode::check`]; the message is user-presentable.
    pub(crate) fn build(&self) -> Result<ContainerTemplate, String> {
        fn export(node: &EditorNode) -> RuleNode {
            RuleNode {
                id: node.id.clone(),
                title: node.title.trim().to_owned(),
                description: node.description.clone(),
                is_project: node.is_project,
                is_worker_destination: node.is_worker_destination,
                instructions: node
                    .instructions
                    .iter()
                    .filter(|instruction| !instruction.text.trim().is_empty())
                    .map(|instruction| RuleInstruction {
                        id: instruction.id.clone(),
                        text: instruction.text.trim().to_owned(),
                    })
                    .collect(),
                children: node.children.iter().map(export).collect(),
            }
        }
        let root = export(&self.root);
        root.check()?;
        Ok(ContainerTemplate {
            revision: self.revision,
            root,
        })
    }

    pub(crate) fn node_mut(&mut self, key: u64) -> Option<&mut EditorNode> {
        fn find(node: &mut EditorNode, key: u64) -> Option<&mut EditorNode> {
            if node.key == key {
                return Some(node);
            }
            node.children.iter_mut().find_map(|child| find(child, key))
        }
        find(&mut self.root, key)
    }

    /// Every node, preorder.
    pub(crate) fn nodes(&self) -> Vec<&EditorNode> {
        fn walk<'a>(node: &'a EditorNode, out: &mut Vec<&'a EditorNode>) {
            out.push(node);
            for child in &node.children {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }

    /// Add an empty child under `parent`; its key.
    pub(crate) fn add_child(&mut self, parent: u64) -> Option<u64> {
        let key = self.fresh_key();
        let node = self.node_mut(parent)?;
        node.children.push(EditorNode {
            key,
            id: String::new(),
            title: String::new(),
            description: None,
            is_project: false,
            is_worker_destination: false,
            instructions: Vec::new(),
            children: Vec::new(),
        });
        Some(key)
    }

    /// Remove a node (never the root) with its subtree.
    pub(crate) fn remove_node(&mut self, key: u64) -> bool {
        fn remove(node: &mut EditorNode, key: u64) -> bool {
            if let Some(ix) = node.children.iter().position(|child| child.key == key) {
                node.children.remove(ix);
                return true;
            }
            node.children.iter_mut().any(|child| remove(child, key))
        }
        remove(&mut self.root, key)
    }

    /// Move a node one place among its siblings.
    pub(crate) fn move_node(&mut self, key: u64, up: bool) -> bool {
        fn walk(node: &mut EditorNode, key: u64, up: bool) -> bool {
            if let Some(ix) = node.children.iter().position(|child| child.key == key) {
                let to = if up { ix.checked_sub(1) } else { Some(ix + 1) };
                return match to.filter(|to| *to < node.children.len()) {
                    Some(to) => {
                        node.children.swap(ix, to);
                        true
                    }
                    None => false,
                };
            }
            node.children.iter_mut().any(|child| walk(child, key, up))
        }
        walk(&mut self.root, key, up)
    }

    pub(crate) fn set_project(&mut self, key: u64, on: bool) {
        if let Some(node) = self.node_mut(key) {
            node.is_project = on;
        }
    }

    /// At most one worker destination, never the root: turning one on clears
    /// every other.
    pub(crate) fn set_worker_destination(&mut self, key: u64, on: bool) {
        if key == self.root.key {
            return;
        }
        fn clear(node: &mut EditorNode) {
            node.is_worker_destination = false;
            node.children.iter_mut().for_each(clear);
        }
        if on {
            clear(&mut self.root);
        }
        if let Some(node) = self.node_mut(key) {
            node.is_worker_destination = on;
        }
    }

    /// Add an empty instruction to `node`; its key.
    pub(crate) fn add_instruction(&mut self, node: u64) -> Option<u64> {
        let key = self.fresh_key();
        self.node_mut(node)?.instructions.push(EditorInstruction {
            key,
            id: String::new(),
            text: String::new(),
        });
        Some(key)
    }

    pub(crate) fn remove_instruction(&mut self, key: u64) {
        fn walk(node: &mut EditorNode, key: u64) {
            node.instructions.retain(|instruction| instruction.key != key);
            node.children.iter_mut().for_each(|child| walk(child, key));
        }
        walk(&mut self.root, key);
    }

    pub(crate) fn set_title(&mut self, key: u64, title: String) {
        if let Some(node) = self.node_mut(key) {
            node.title = title;
        }
    }

    pub(crate) fn set_instruction_text(&mut self, key: u64, text: String) {
        fn walk(node: &mut EditorNode, key: u64, text: &str) {
            for instruction in &mut node.instructions {
                if instruction.key == key {
                    instruction.text = text.to_owned();
                }
            }
            node.children.iter_mut().for_each(|child| walk(child, key, text));
        }
        walk(&mut self.root, key, &text);
    }
}

/// Whether `child` follows the rules of `parent`.
pub(crate) fn follows_template(parent: &WorkItemView, child: &WorkItemView) -> bool {
    match (&parent.container_template, &child.template_node_id) {
        (Some(template), Some(node)) => *node == template.root.id,
        _ => false,
    }
}

/// What the Template section shows for one item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SectionState {
    /// The item carries rules.
    pub has_rules: bool,
    /// Live direct children / those following (only with rules).
    pub children: usize,
    pub following: usize,
    /// The item's parent has rules the item doesn't follow.
    pub doesnt_follow: bool,
}

impl SectionState {
    /// "Apply to all children…": rules and at least one child not following.
    pub(crate) fn can_apply_all(&self) -> bool {
        self.has_rules && self.following < self.children
    }
}

pub(crate) fn section_state(snapshot: &WorkOutlineSnapshot, item: &WorkItemView) -> SectionState {
    let mut state = SectionState {
        has_rules: item.container_template.is_some(),
        ..Default::default()
    };
    if state.has_rules {
        for child in item
            .child_ids
            .iter()
            .filter_map(|id| snapshot.item(id))
            .filter(|child| child.archived_at.is_none())
        {
            state.children += 1;
            state.following += usize::from(follows_template(item, child));
        }
    }
    state.doesnt_follow = item
        .parent_work_item_id
        .as_deref()
        .and_then(|id| snapshot.item(id))
        .is_some_and(|parent| parent.container_template.is_some() && !follows_template(parent, item));
    state
}

/// The open editor and its text inputs (keyed by node / instruction key).
#[derive(Default)]
pub(crate) struct ContainerTemplateState {
    pub editor: Option<TemplateEditor>,
    pub inputs: HashMap<u64, Entity<ComposerInput>>,
    pub error: Option<String>,
}

impl WorkItemDetails {
    /// "Set up child template…" / "Edit": open the editor on the current rules.
    pub(crate) fn begin_container_template(&mut self, cx: &mut Context<Self>) {
        let editor = TemplateEditor::new(
            self.item()
                .and_then(|item| item.container_template.as_ref()),
        );
        self.container_template.inputs.clear();
        self.container_template.error = None;
        self.container_template.editor = Some(editor);
        self.ensure_template_inputs(cx);
        cx.notify();
    }

    pub(crate) fn close_container_template(&mut self, cx: &mut Context<Self>) {
        self.container_template.editor = None;
        self.container_template.inputs.clear();
        self.container_template.error = None;
        cx.notify();
    }

    /// Give every node title and instruction of the editor a text input.
    fn ensure_template_inputs(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.container_template.editor.as_ref() else {
            return;
        };
        let mut wanted: Vec<(u64, String, bool)> = Vec::new();
        for node in editor.nodes() {
            if node.key != editor.root.key {
                wanted.push((node.key, node.title.clone(), true));
            }
            for instruction in &node.instructions {
                wanted.push((instruction.key, instruction.text.clone(), false));
            }
        }
        for (key, text, single_line) in wanted {
            if self.container_template.inputs.contains_key(&key) {
                continue;
            }
            let input = cx.new(|cx| {
                let input = ComposerInput::with_context(
                    if single_line { "Title" } else { "Instruction" },
                    crate::composer::PALETTE_SEARCH_CONTEXT,
                    cx,
                )
                .with_text_metrics(12.0, 18.0);
                if single_line { input.with_single_line() } else { input }
            });
            input.update(cx, |input, cx| input.set_text(text, cx));
            self.container_template.inputs.insert(key, input);
        }
    }

    /// Copy what the inputs hold into the model (before Save).
    fn sync_template_texts(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.container_template.editor.as_mut() else {
            return;
        };
        for (key, input) in &self.container_template.inputs {
            let text = input.read(cx).text().to_owned();
            editor.set_title(*key, text.clone());
            editor.set_instruction_text(*key, text);
        }
    }

    /// Apply a structural edit, then make sure new rows have inputs.
    fn edit_template(&mut self, edit: impl FnOnce(&mut TemplateEditor), cx: &mut Context<Self>) {
        self.sync_template_texts(cx);
        if let Some(editor) = self.container_template.editor.as_mut() {
            edit(editor);
        }
        self.ensure_template_inputs(cx);
        cx.notify();
    }

    /// Save: validate, then send `setContainerTemplate`.
    pub(crate) fn save_container_template(&mut self, cx: &mut Context<Self>) {
        self.sync_template_texts(cx);
        let Some(editor) = self.container_template.editor.as_ref() else {
            return;
        };
        match editor.build() {
            Ok(template) => self.send_container_template(Some(template), cx),
            Err(message) => {
                self.container_template.error = Some(message);
                cx.notify();
            }
        }
    }

    /// Turn off: send `template: None`.
    pub(crate) fn turn_off_container_template(&mut self, cx: &mut Context<Self>) {
        self.send_container_template(None, cx);
    }

    fn send_container_template(
        &mut self,
        template: Option<ContainerTemplate>,
        cx: &mut Context<Self>,
    ) {
        let key = "container-template-save";
        if !self.in_flight.insert(key.to_owned()) {
            return;
        }
        self.container_template.error = None;
        let off = template.is_none();
        let task = self.state.read(cx).work_outline_actions().mutate(
            WorkOutlineMutation::SetContainerTemplate(SetContainerTemplateParams {
                operation_id: new_operation_id(),
                space_id: self.space_id.clone(),
                work_item_id: self.item_id.clone(),
                template,
            }),
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.in_flight.remove(key);
                match result {
                    Ok(_) => {
                        this.close_container_template(cx);
                        cx.emit(DetailsEvent::Notice {
                            text: if off { "Child template turned off" } else { "Child template saved" }.into(),
                            error: false,
                        });
                    }
                    Err(error) => this.container_template.error = Some(error.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Send `applyContainerTemplate` for `work_item_id` (this item = every
    /// direct child; a child = just that child).
    pub(crate) fn apply_container_template(&mut self, work_item_id: String, cx: &mut Context<Self>) {
        let key = format!("container-template-apply:{work_item_id}");
        if !self.in_flight.insert(key.clone()) {
            return;
        }
        self.error = None;
        let task = self.state.read(cx).work_outline_actions().mutate(
            WorkOutlineMutation::ApplyContainerTemplate(ApplyContainerTemplateParams {
                operation_id: new_operation_id(),
                space_id: self.space_id.clone(),
                work_item_id,
            }),
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.in_flight.remove(&key);
                match result {
                    Ok(_) => cx.emit(DetailsEvent::Notice {
                        text: "Child template applied".into(),
                        error: false,
                    }),
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The **Template** section: summary and actions, or the open editor.
    pub(super) fn render_container_template_section(
        &self,
        item: &WorkItemView,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let state = section_state(snapshot, item);
        let mut header = div().flex().flex_wrap().items_center().gap(px(6.0)).child(
            div()
                .text_size(crate::typography::ui_rems(11.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text_muted)
                .child("Child template"),
        );
        let editing = self.container_template.editor.is_some();
        if !editing {
            header = header.child(
                crate::work_outline::hints::details_button(
                    "work-item-container-template-edit",
                    if state.has_rules { "Edit" } else { "Set up child template\u{2026}" },
                    theme,
                )
                .debug_selector(|| "work-item-container-template-edit".into())
                .on_click(cx.listener(|this, _, _, cx| this.begin_container_template(cx))),
            );
            if state.can_apply_all() {
                let id = item.id.clone();
                header = header.child(
                    crate::work_outline::hints::details_button(
                        "work-item-container-template-apply-all",
                        "Apply to all children\u{2026}",
                        theme,
                    )
                    .debug_selector(|| "work-item-container-template-apply-all".into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.apply_container_template(id.clone(), cx)
                    })),
                );
            }
        }
        let mut section = div().flex().flex_col().gap(px(4.0)).child(header);
        let proposals = super::super::required_actions::proposals_for(snapshot, Some(&item.id))
            .filter(|proposal| proposal.target.scope == mast_proto::work_outline_user_actions::ProposalTargetScope::ChildTemplate
                && proposal.target.container_work_item_id.as_deref() == Some(item.id.as_str())
                && super::super::required_actions::is_pending(proposal))
            .cloned()
            .collect::<Vec<_>>();
        if !proposals.is_empty() {
            section = section.child(
                crate::work_outline::hints::details_button("work-item-template-proposals", if self.template_proposals_open { "Hide proposed instructions" } else { "Review proposed instructions" }, theme)
                    .debug_selector(|| "work-item-template-proposals".into())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.template_proposals_open = !this.template_proposals_open;
                        cx.notify();
                    })),
            );
            if self.template_proposals_open {
                section = section.child(
                    div().debug_selector(|| "work-item-template-proposals-inline".into())
                        .flex().flex_col().gap(px(4.0))
                        .children(self.render_pending_proposal_groups(proposals, snapshot, theme, cx)),
                );
            }
        }
        if state.has_rules && !editing {
            section = section.child(
                div()
                    .debug_selector(|| "work-item-container-template-summary".into())
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!(
                        "Applies to {} direct children \u{b7} {} follow",
                        state.children, state.following
                    ))),
            );
        }
        if state.doesnt_follow {
            let id = item.id.clone();
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .debug_selector(|| "work-item-template-doesnt-follow".into())
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.text_muted)
                            .child("Doesn\u{2019}t follow the child template"),
                    )
                    .child(
                        crate::work_outline::hints::details_button(
                            "work-item-template-apply",
                            "Apply child template\u{2026}",
                            theme,
                        )
                        .debug_selector(|| "work-item-template-apply".into())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.apply_container_template(id.clone(), cx)
                        })),
                    ),
            );
        }
        if let Some(editor) = self.container_template.editor.as_ref() {
            section = section.child(self.render_template_node(&editor.root, true, 0, theme, cx));
            if let Some(error) = &self.container_template.error {
                section = section.child(
                    div()
                        .debug_selector(|| "work-item-container-template-error".into())
                        .text_size(crate::typography::ui_rems(10.5))
                        .text_color(theme.warning)
                        .child(SharedString::from(error.clone())),
                );
            }
            let mut buttons = div().flex().gap(px(6.0)).child(
                crate::work_outline::hints::details_button("work-item-container-template-save", "Save", theme)
                    .debug_selector(|| "work-item-container-template-save".into())
                    .on_click(cx.listener(|this, _, _, cx| this.save_container_template(cx))),
            );
            if state.has_rules {
                buttons = buttons.child(
                    crate::work_outline::hints::details_button(
                        "work-item-container-template-off",
                        "Turn off",
                        theme,
                    )
                    .debug_selector(|| "work-item-container-template-off".into())
                    .on_click(cx.listener(|this, _, _, cx| this.turn_off_container_template(cx))),
                );
            }
            section = section.child(buttons.child(
                crate::work_outline::hints::details_button("work-item-container-template-cancel", "Cancel", theme)
                    .on_click(cx.listener(|this, _, _, cx| this.close_container_template(cx))),
            ));
        }
        section
    }

    /// One node row of the editor and, indented, its instructions and children.
    fn render_template_node(
        &self,
        node: &EditorNode,
        is_root: bool,
        depth: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = node.key as usize;
        let node_key = node.key;
        let mut head = div().flex().flex_wrap().items_center().gap(px(4.0));
        head = if is_root {
            head.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text)
                    .child("Each child"),
            )
        } else {
            match self.container_template.inputs.get(&node.key) {
                Some(input) => head.child(
                    div()
                        .flex_1()
                        .min_w(px(80.0))
                        .child(crate::components::outline_input_frame(input, theme)),
                ),
                None => head,
            }
        };
        let project = node.is_project;
        head = head.child(
            crate::work_outline::hints::details_button(
                ("work-item-ct-project", key),
                if project { "\u{2713} Project" } else { "Project" },
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.edit_template(|editor| editor.set_project(node_key, !project), cx)
            })),
        );
        if !is_root {
            let destination = node.is_worker_destination;
            head = head
                .child(
                    crate::work_outline::hints::details_button(
                        ("work-item-ct-destination", key),
                        if destination {
                            "\u{2713} Worker destination"
                        } else {
                            "Worker destination"
                        },
                        theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit_template(
                            |editor| editor.set_worker_destination(node_key, !destination),
                            cx,
                        )
                    })),
                )
                .child(
                    crate::work_outline::hints::details_button(("work-item-ct-up", key), "\u{2191}", theme).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.edit_template(|editor| { let _ = editor.move_node(node_key, true); }, cx)
                        }),
                    ),
                )
                .child(
                    crate::work_outline::hints::details_button(("work-item-ct-down", key), "\u{2193}", theme).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.edit_template(|editor| { let _ = editor.move_node(node_key, false); }, cx)
                        }),
                    ),
                )
                .child(
                    crate::work_outline::hints::details_button(("work-item-ct-remove", key), "Remove", theme).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.edit_template(|editor| { let _ = editor.remove_node(node_key); }, cx)
                        }),
                    ),
                );
        }
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .when(depth > 0, |el| {
                el.ml(px(10.0))
                    .pl(px(6.0))
                    .border_l_1()
                    .border_color(theme.border)
            })
            .child(head);
        for instruction in &node.instructions {
            let instruction_key = instruction.key;
            let Some(input) = self.container_template.inputs.get(&instruction.key) else {
                continue;
            };
            body = body.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(4.0))
                    .child(div().flex_1().min_w_0().child(crate::components::outline_input_frame(input, theme)))
                    .child(
                        crate::work_outline::hints::details_button(
                            ("work-item-ct-instruction-remove", instruction.key as usize),
                            "Remove",
                            theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit_template(
                                |editor| editor.remove_instruction(instruction_key),
                                cx,
                            )
                        })),
                    ),
            );
        }
        body = body.child(
            div()
                .flex()
                .gap(px(4.0))
                .child(
                    crate::work_outline::hints::details_button(
                        ("work-item-ct-add-instruction", key),
                        "Add instruction",
                        theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit_template(|editor| { let _ = editor.add_instruction(node_key); }, cx)
                    })),
                )
                .child(
                    crate::work_outline::hints::details_button(("work-item-ct-add-child", key), "Add item", theme)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit_template(|editor| { let _ = editor.add_child(node_key); }, cx)
                        })),
                ),
        );
        for child in &node.children {
            body = body.child(self.render_template_node(child, false, depth + 1, theme, cx));
        }
        body.into_any_element()
    }
}

#[cfg(test)]
#[path = "details_container_template_tests.rs"]
mod tests;
