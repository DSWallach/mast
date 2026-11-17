//! Full item creation from the persistent Overview toolbar.
use super::work_outline_ui::WorkOutlineModal;
use super::*;
use crate::work_outline::actions::{OperationSlot, instruction_error_text};
use mast_proto::{CreateWorkItemParams, WorkItemStatus, WorkOutlinePlacement};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Space,
    Parent,
}

pub(super) struct CreateItemDialog {
    title: Entity<ComposerInput>,
    description: Entity<ComposerInput>,
    space: Option<String>,
    parent: Option<String>,
    status: WorkItemStatus,
    first: bool,
    picker: Option<Picker>,
    op: OperationSlot,
    saving: bool,
    pending_operation: Option<String>,
    error: Option<String>,
    _events: Vec<Subscription>,
}

impl CreateItemDialog {
    pub(super) fn focus(&self, cx: &App) -> FocusHandle {
        self.title.read(cx).focus_handle(cx)
    }
    pub(super) fn saving(&self) -> bool {
        self.saving
    }
}

impl Shell {
    pub(crate) fn open_create_work_item(
        &mut self,
        space: Option<String>,
        parent: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(&self.work_outline_modal, Some(WorkOutlineModal::CreateItem(dialog)) if dialog.saving)
        {
            return;
        }
        let state = self.state.read(cx);
        let space = space
            .filter(|id| state.space_row(id).is_some())
            .or_else(|| {
                self.settings
                    .space_filter
                    .clone()
                    .filter(|id| state.space_row(id).is_some())
            })
            .or_else(|| {
                state
                    .selected_space
                    .clone()
                    .filter(|id| state.space_row(id).is_some())
            })
            .or_else(|| state.spaces.first().map(|space| space.id.clone()));
        let parent = parent.filter(|id| {
            space.as_deref().is_some_and(|space| {
                state
                    .work_outline(space)
                    .snapshot()
                    .is_some_and(|snapshot| {
                        snapshot
                            .item(id)
                            .is_some_and(|item| item.archived_at.is_none())
                    })
            })
        });
        let title = cx.new(|cx| ComposerInput::new("Item title", cx).with_text_metrics(13.0, 20.0));
        let description = cx.new(|cx| {
            ComposerInput::new("Description (optional)", cx)
                .with_text_metrics(12.5, 18.0)
                .with_auto_grow()
        });
        let submit = cx.subscribe_in(&title, window, |this, _, event, window, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_create_work_item(window, cx);
            }
            cx.notify();
        });
        let edits = cx.subscribe(&description, |_, _, _, cx| cx.notify());
        self.begin_work_outline_modal(
            WorkOutlineModal::CreateItem(CreateItemDialog {
                title,
                description,
                space,
                parent,
                status: WorkItemStatus::Pending,
                first: false,
                picker: None,
                op: OperationSlot::default(),
                saving: false,
                pending_operation: None,
                error: None,
                _events: vec![submit, edits],
            }),
            cx,
        );
    }

    fn edit_create_item(
        &mut self,
        edit: impl FnOnce(&mut CreateItemDialog),
        cx: &mut Context<Self>,
    ) {
        if let Some(WorkOutlineModal::CreateItem(dialog)) = &mut self.work_outline_modal
            && !dialog.saving
        {
            edit(dialog);
            dialog.error = None;
            cx.notify();
        }
    }

    pub(super) fn dismiss_create_item_picker(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(WorkOutlineModal::CreateItem(dialog)) = &mut self.work_outline_modal
            && dialog.picker.take().is_some()
        {
            cx.notify();
            return true;
        }
        false
    }

    fn submit_create_work_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let actions = state.work_outline_actions();
        let Some(WorkOutlineModal::CreateItem(dialog)) = &mut self.work_outline_modal else {
            return;
        };
        if dialog.saving {
            return;
        }
        let title = dialog.title.read(cx).text().trim().to_owned();
        let description = dialog.description.read(cx).text().trim().to_owned();
        let invalid = if title.is_empty() {
            Some("Enter an item title")
        } else if title.chars().count() > mast_proto::limits::MAX_TITLE_CHARS {
            Some("The title is limited to 256 characters")
        } else if description.chars().count() > mast_proto::limits::MAX_DESCRIPTION_CHARS {
            Some("The description is limited to 8,000 characters")
        } else if dialog
            .space
            .as_deref()
            .is_none_or(|space| state.space_row(space).is_none())
        {
            Some("Choose a Space first")
        } else if dialog.space.as_deref().is_none_or(|space| {
            state
                .work_outline(space)
                .snapshot()
                .is_none_or(|snapshot| !snapshot.available)
        }) {
            Some("This Space's outline is not available yet")
        } else if dialog.parent.as_deref().is_some_and(|id| {
            state
                .work_outline(dialog.space.as_deref().unwrap())
                .snapshot()
                .is_none_or(|snapshot| {
                    snapshot
                        .item(id)
                        .is_none_or(|item| item.archived_at.is_some())
                })
        }) {
            Some("Choose an available parent work item")
        } else {
            None
        };
        if let Some(error) = invalid {
            dialog.error = Some(error.into());
            cx.notify();
            return;
        }
        let mut params = CreateWorkItemParams {
            space_id: dialog.space.clone().unwrap(),
            parent_work_item_id: dialog.parent.clone(),
            placement: if dialog.first {
                WorkOutlinePlacement::First
            } else {
                WorkOutlinePlacement::Last
            },
            title,
            description: (!description.is_empty()).then_some(description),
            status: Some(dialog.status),
            operation_id: String::new(),
        };
        params.operation_id = dialog.op.id_for(&serde_json::to_string(&params).unwrap());
        let task = actions.create_work_item(params.clone());
        let operation = params.operation_id.clone();
        dialog.pending_operation = Some(operation.clone());
        dialog.saving = true;
        dialog.picker = None;
        dialog.error = None;
        dialog.title.update(cx, |input, cx| {
            input.read_only = true;
            cx.notify();
        });
        dialog.description.update(cx, |input, cx| {
            input.read_only = true;
            cx.notify();
        });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.and_then(|reply| reply.into_create_work_item());
            this.update_in(cx, |this, window, cx| {
                // A late response must never close or edit a newer dialog.
                let Some(WorkOutlineModal::CreateItem(dialog)) = &mut this.work_outline_modal
                else {
                    return;
                };
                if dialog.pending_operation.as_deref() != Some(operation.as_str()) {
                    return;
                }
                dialog.saving = false;
                match result {
                    Ok(created) => {
                        this.state.update(cx, |state, cx| {
                            let draft = crate::work_outline::action_log::create_entry(
                                &created.work_item_id,
                                &params.title,
                                None,
                            );
                            state.action_log.record(&params.space_id, draft);
                            cx.notify();
                        });
                        this.resolve_work_outline_modal(true, window, cx);
                    }
                    Err(error) => {
                        dialog.error = Some(instruction_error_text(&error));
                        dialog.title.update(cx, |input, cx| {
                            input.read_only = false;
                            cx.notify();
                        });
                        dialog.description.update(cx, |input, cx| {
                            input.read_only = false;
                            cx.notify();
                        });
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}

pub(super) fn create_item_card(
    dialog: &CreateItemDialog,
    shell: &Shell,
    viewport: gpui::Size<Pixels>,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let state = shell.state.read(cx);
    let space_label = dialog
        .space
        .as_deref()
        .and_then(|id| state.space_row(id))
        .map(|space| space.display_name().to_owned())
        .unwrap_or_else(|| "Choose a Space".into());
    let snapshot = dialog
        .space
        .as_deref()
        .and_then(|id| state.work_outline(id).snapshot().cloned());
    let parent_label = dialog
        .parent
        .as_deref()
        .and_then(|id| snapshot.as_ref()?.item(id))
        .map(|item| item.title.clone())
        .unwrap_or_else(|| "Space root (no parent)".into());
    let label = |text: &'static str| {
        div()
            .flex_none()
            .mt(px(10.0))
            .mb(px(4.0))
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted)
            .child(text)
    };
    let picker_button = |id: &'static str, value: String, picker| {
        popover::btn_ghost(theme, &format!("{value} ▾"), id)
            .id(id)
            .flex_none()
            .debug_selector(move || id.into())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.edit_create_item(
                    |dialog| {
                        dialog.picker = if dialog.picker == Some(picker) {
                            None
                        } else {
                            Some(picker)
                        }
                    },
                    cx,
                );
            }))
    };
    let choices: Vec<(String, String)> = match dialog.picker {
        Some(Picker::Space) => state
            .spaces
            .iter()
            .map(|space| (space.id.clone(), space.display_name().to_owned()))
            .collect(),
        Some(Picker::Parent) => std::iter::once((String::new(), "Space root (no parent)".into()))
            .chain(snapshot.iter().flat_map(|snapshot| {
                snapshot
                    .items
                    .iter()
                    .filter(|item| item.archived_at.is_none())
                    .map(|item| (item.id.clone(), item_path(snapshot, item)))
            }))
            .collect(),
        None => Vec::new(),
    };
    let picker = dialog.picker;
    let rows = choices.into_iter().map(|(id, text)| {
        let selected = match picker {
            Some(Picker::Space) => dialog.space.as_deref() == Some(&id),
            _ => dialog.parent.as_deref().unwrap_or("") == id,
        };
        let selector = format!("create-item-choice:{id}");
        popover::menu_row(theme, selected, selector.clone())
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .child(div().min_w_0().truncate().child(text))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.edit_create_item(
                    |dialog| {
                        if picker == Some(Picker::Space) {
                            dialog.space = Some(id.clone());
                            dialog.parent = None;
                        } else {
                            dialog.parent = (!id.is_empty()).then_some(id.clone());
                        }
                        dialog.picker = None;
                    },
                    cx,
                )
            }))
    });
    let status = div().flex_none().flex().flex_wrap().gap(px(6.0)).children(
        [
            (WorkItemStatus::Pending, "Pending"),
            (WorkItemStatus::Active, "Active"),
            (WorkItemStatus::Completed, "Completed"),
        ]
        .into_iter()
        .map(|(status, text)| {
            crate::components::outline_toolbar_button(
                text,
                text.into(),
                dialog.status == status,
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.edit_create_item(|dialog| dialog.status = status, cx)
            }))
        }),
    );
    popover::dialog_card(theme)
        .w(px((f32::from(viewport.width) - 32.0).clamp(0.0, 520.0)))
        .max_h(px(f32::from(viewport.height) * 0.85))
        .overflow_hidden()
        .debug_selector(|| "create-work-item-dialog".into())
        .child(popover::dialog_title(theme, "Create work item").flex_none())
        .child(
            div()
                .id("create-item-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .child(label("Title"))
                .child(
                    popover::dialog_field(theme, dialog.title.clone().into_any_element())
                        .flex_none()
                        .debug_selector(|| "create-item-title".into()),
                )
                .child(label("Space"))
                .child(picker_button(
                    "create-item-space",
                    space_label,
                    Picker::Space,
                ))
                .child(label("Parent work item"))
                .child(picker_button(
                    "create-item-parent",
                    parent_label,
                    Picker::Parent,
                ))
                .when(picker.is_some(), |el| {
                    el.child(
                        div()
                            .id("create-item-choices")
                            .flex_none()
                            .max_h(px(150.0))
                            .overflow_y_scroll()
                            .children(rows),
                    )
                })
                .when(state.spaces.is_empty(), |el| {
                    el.child(popover::dialog_body(
                        theme,
                        "Add a Space before creating work items.",
                    ))
                })
                .child(label("Description"))
                .child(
                    popover::dialog_field(theme, dialog.description.clone().into_any_element())
                        .flex_none(),
                )
                .child(label("Status"))
                .child(status)
                .child(label("Placement"))
                .child(
                    div().flex_none().flex().gap(px(6.0)).children(
                        [(true, "First"), (false, "Last")]
                            .into_iter()
                            .map(|(first, text)| {
                                crate::components::outline_toolbar_button(
                                    text,
                                    text.into(),
                                    dialog.first == first,
                                    theme,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.edit_create_item(|dialog| dialog.first = first, cx)
                                    },
                                ))
                            }),
                    ),
                )
                .children(dialog.error.as_ref().map(|error| {
                    div()
                        .mt(px(8.0))
                        .text_color(theme.danger)
                        .child(error.clone())
                })),
        )
        .child(
            div()
                .mt(px(14.0))
                .flex_none()
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    popover::btn_ghost(theme, "Cancel", "create-item-cancel")
                        .id("create-item-cancel")
                        .debug_selector(|| "create-item-cancel".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.cancel_work_outline_modal(window, cx);
                        })),
                )
                .child(
                    popover::btn_primary(
                        theme,
                        if dialog.saving {
                            "Creating…"
                        } else {
                            "Create item"
                        },
                    )
                    .id("create-item-submit")
                    .debug_selector(|| "create-item-submit".into())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.submit_create_work_item(window, cx)),
                    ),
                ),
        )
        .into_any_element()
}

fn item_path(
    snapshot: &mast_proto::WorkOutlineSnapshot,
    item: &mast_proto::WorkItemView,
) -> String {
    let mut path = vec![item.title.as_str()];
    let mut parent = item.parent_work_item_id.as_deref();
    let mut seen = std::collections::HashSet::from([item.id.as_str()]);
    while let Some(item) = parent.and_then(|id| snapshot.item(id)) {
        if !seen.insert(item.id.as_str()) {
            break;
        }
        path.push(&item.title);
        parent = item.parent_work_item_id.as_deref();
    }
    path.reverse();
    path.join(" › ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::store::{fixture_ids::LAUNCH, fixture_snapshot};
    use std::sync::{Arc, Mutex};

    fn seed(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.spaces = ["s1", "s2"].into_iter().map(|id| serde_json::from_value(serde_json::json!({
                    "id":id, "deviceId":"local", "path":format!("/{id}"), "createdAt":"2026-11-01T00:00:00Z"
                })).unwrap()).collect();
                state.selected_space = Some("s1".into());
                state.set_work_outline_for_test(fixture_snapshot("s1"));
                state.set_work_outline_for_test(fixture_snapshot("s2"));
                cx.notify();
            });
            cx.notify();
        });
        redraw(cx);
    }
    fn redraw(cx: &mut gpui::VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    }
    fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
        let at = cx.debug_bounds(selector).expect(selector).center();
        cx.simulate_click(at, gpui::Modifiers::default());
        redraw(cx);
    }

    #[gpui::test]
    fn toolbar_opens_full_dialog_and_space_change_resets_parent(cx: &mut gpui::TestAppContext) {
        let (_dir, shell, cx) = super::super::overview_ui::tests::setup(cx);
        seed(&shell, cx);
        click(cx, "outline-new-item");
        assert!(cx.debug_bounds("create-work-item-dialog").is_some());
        shell.read_with(cx, |shell, cx| {
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat"))
        });
        click(cx, "create-item-parent");
        click(
            cx,
            Box::leak(format!("create-item-choice:{LAUNCH}").into_boxed_str()),
        );
        shell.read_with(cx, |shell, _| {
            let Some(WorkOutlineModal::CreateItem(dialog)) = &shell.work_outline_modal else {
                panic!()
            };
            assert_eq!(dialog.parent.as_deref(), Some(LAUNCH));
        });
        click(cx, "create-item-space");
        click(cx, "create-item-choice:s2");
        shell.read_with(cx, |shell, _| {
            let Some(WorkOutlineModal::CreateItem(dialog)) = &shell.work_outline_modal else {
                panic!()
            };
            assert_eq!(dialog.space.as_deref(), Some("s2"));
            assert!(dialog.parent.is_none());
        });
        for (width, height) in [(360.0, 500.0), (1200.0, 900.0)] {
            cx.simulate_resize(gpui::size(px(width), px(height)));
            redraw(cx);
            let card = cx.debug_bounds("create-work-item-dialog").unwrap();
            let submit = cx.debug_bounds("create-item-submit").unwrap();
            assert!(card.left() >= px(0.0) && card.right() <= px(width));
            assert!(submit.bottom() <= card.bottom());
        }
        click(cx, "create-item-cancel");
        shell.read_with(cx, |shell, _| assert!(shell.work_outline_modal.is_none()));
    }

    #[gpui::test]
    fn submission_preserves_options_and_retries_without_duplicate_creation(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_dir, shell, cx) = super::super::overview_ui::tests::setup(cx);
        seed(&shell, cx);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sink = calls.clone();
        let actions = crate::work_outline::actions::WorkOutlineActions::scripted(Arc::new(
            move |_, params| {
                let mut calls = sink.lock().unwrap();
                calls.push(params);
                if calls.len() == 1 {
                    Err(mast_rpc::RpcError::Failed(
                        "work-outline/invalid: try again".into(),
                    ))
                } else {
                    Ok(serde_json::json!({"workItemId":"created"}))
                }
            },
        ));
        shell.update_in(cx, |shell, window, cx| {
            shell.state.update(cx, |state, _| {
                state.work_outline_actions_override = Some(actions)
            });
            shell.open_create_work_item(Some("s1".into()), Some(LAUNCH.into()), window, cx);
            shell.submit_create_work_item(window, cx);
            let Some(WorkOutlineModal::CreateItem(dialog)) = &mut shell.work_outline_modal else {
                panic!()
            };
            assert!(dialog.error.is_some(), "blank title is rejected locally");
            dialog
                .title
                .update(cx, |input, cx| input.set_text("New child", cx));
            dialog
                .description
                .update(cx, |input, cx| input.set_text("Details", cx));
            dialog.status = WorkItemStatus::Active;
            dialog.first = true;
            shell.submit_create_work_item(window, cx);
            shell.submit_create_work_item(window, cx);
            assert!(shell.cancel_work_outline_modal(window, cx));
            assert!(
                matches!(
                    &shell.work_outline_modal,
                    Some(WorkOutlineModal::CreateItem(_))
                ),
                "in-flight dialog cannot be discarded"
            );
        });
        cx.run_until_parked();
        shell.read_with(cx, |shell, cx| {
            let Some(WorkOutlineModal::CreateItem(dialog)) = &shell.work_outline_modal else {
                panic!()
            };
            assert!(!dialog.saving);
            assert!(dialog.error.is_some());
            assert_eq!(dialog.title.read(cx).text(), "New child");
        });
        shell.update_in(cx, |shell, window, cx| {
            shell.submit_create_work_item(window, cx)
        });
        cx.run_until_parked();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0], calls[1],
            "retry uses the same operation and fields"
        );
        assert_eq!(calls[0]["spaceId"], "s1");
        assert_eq!(calls[0]["parentWorkItemId"], LAUNCH);
        assert_eq!(calls[0]["description"], "Details");
        assert_eq!(calls[0]["status"], "active");
        assert_eq!(calls[0]["placement"]["type"], "first");
        shell.read_with(cx, |shell, _| assert!(shell.work_outline_modal.is_none()));
    }
}
