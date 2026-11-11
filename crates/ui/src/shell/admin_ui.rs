//! Admin chats in the shell: the grant dialog, the "Admin · Space · 3h left"
//! badge with its Revoke button (chat header and Item tab), and the menu
//! entries. State, RPC and formatting are `crate::admin`.
use super::*;

use crate::admin::{self, DurationChoice};
use crate::shell::chat_item_ui::{ChipTone, chip_palette};
use crate::work_outline::hints::{HintKey, WithHint};

/// The open grant dialog.
struct AdminDialog {
    chat_id: String,
    choice: DurationChoice,
    /// "Skip approval for large changes" (off by default).
    skip_bulk: bool,
}

/// Shell-owned admin state (the grants themselves live in `AppState`).
#[derive(Default)]
pub(super) struct AdminUi {
    dialog: Option<AdminDialog>,
}

impl AdminUi {
    pub(super) fn is_open(&self) -> bool {
        self.dialog.is_some()
    }
}

/// A Space's display name: its name, else its folder, else "this Space".
pub(crate) fn space_display_name(state: &AppState, space_id: &str) -> String {
    state
        .space_row(space_id)
        .map(|space| space.display_name().to_owned())
        .unwrap_or_else(|| "this Space".to_owned())
}

impl Shell {
    /// The (id, name) of the Space `chat_id` lives in; `None` for a chat
    /// without a Space (admin needs one).
    pub(crate) fn admin_space_of(&self, chat_id: &str, cx: &App) -> Option<(String, String)> {
        let state = self.state.read(cx);
        let chat = state.chats.iter().find(|chat| chat.id == chat_id)?;
        let space_id = chat.space_id.clone()?;
        let name = space_display_name(state, &space_id);
        Some((space_id, name))
    }

    pub(crate) fn chat_is_admin(&self, chat_id: &str, cx: &App) -> bool {
        self.state
            .read(cx)
            .admin
            .is_admin(chat_id, admin::now_ms())
    }

    /// Open the grant dialog (also "Renew admin…"). Chats without a Space
    /// are refused.
    pub(crate) fn open_admin_dialog(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        if self.admin_space_of(chat_id, cx).is_none() {
            return;
        }
        self.admin_ui.dialog = Some(AdminDialog {
            chat_id: chat_id.to_owned(),
            choice: self.settings.admin_grant_duration,
            skip_bulk: false,
        });
        cx.notify();
    }

    pub(crate) fn close_admin_dialog(&mut self, cx: &mut Context<Self>) {
        if self.admin_ui.dialog.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn set_admin_dialog_choice(&mut self, choice: DurationChoice, cx: &mut Context<Self>) {
        if let Some(dialog) = self.admin_ui.dialog.as_mut() {
            dialog.choice = choice;
            cx.notify();
        }
    }

    pub(crate) fn set_admin_dialog_skip_bulk(&mut self, skip: bool, cx: &mut Context<Self>) {
        if let Some(dialog) = self.admin_ui.dialog.as_mut() {
            dialog.skip_bulk = skip;
            cx.notify();
        }
    }

    /// The Grant button: `GrantAdmin` with the picked duration.
    pub(crate) fn submit_admin_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.admin_ui.dialog.take() else {
            return;
        };
        self.settings.admin_grant_duration = dialog.choice;
        settings::update(settings::SavePolicy::Immediate, cx, |settings| {
            settings.admin_grant_duration = dialog.choice;
        });
        let params = admin::grant_params(&dialog.chat_id, dialog.choice, dialog.skip_bulk);
        self.state.update(cx, |state, cx| state.grant_admin(params, cx));
        cx.notify();
    }

    pub(crate) fn revoke_chat_admin(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let chat_id = chat_id.to_owned();
        self.state
            .update(cx, |state, cx| state.revoke_admin(&chat_id, cx));
    }

    /// The 1 s heartbeat hook.
    pub(super) fn admin_tick(&mut self, cx: &mut Context<Self>) {
        // Not under test: shell tests with a fake engine count every request.
        let polling = cfg!(not(test));
        self.state.update(cx, |state, cx| {
            state.admin_tick(polling, cx);
            if polling {
                state.personal_assistant_tick(cx);
            }
        });
    }

    /// The dialog overlay, when open.
    pub(super) fn render_admin_dialog(
        &mut self,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (chat_id, choice, skip_bulk) = {
            let dialog = self.admin_ui.dialog.as_ref()?;
            (dialog.chat_id.clone(), dialog.choice, dialog.skip_bulk)
        };
        let Some((_, space_name)) = self.admin_space_of(&chat_id, cx) else {
            self.admin_ui.dialog = None;
            return None;
        };
        let theme = Theme::of(cx).for_popup();
        let renew = self.chat_is_admin(&chat_id, cx);
        let mut options = div().mt(px(12.0)).flex().flex_col().gap(px(2.0));
        for option in DurationChoice::ALL {
            let selected = option == choice;
            let selector = format!("admin-dialog-duration-{}", option.key());
            options = options.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(if selected { theme.text } else { theme.text_muted })
                    .when(selected, |el| el.bg(theme.element_hover))
                    .hover(|el| el.bg(theme.element_hover.opacity(0.6)))
                    .role(gpui::Role::RadioButton)
                    .aria_label(SharedString::from(option.label()))
                    .child(
                        div()
                            .w(px(12.0))
                            .text_color(theme.accent)
                            .child(if selected { "\u{25cf}" } else { "" }),
                    )
                    .child(SharedString::from(option.label()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_admin_dialog_choice(option, cx)
                    })),
            );
        }
        let card = popover::dialog_card(&theme)
            .child(popover::dialog_title(
                &theme,
                if renew { "Renew admin" } else { "Grant admin" },
            ))
            .child(div().mt(px(6.0)).child(
                popover::dialog_body(&theme, admin::explanation(&space_name)),
            ))
            .child(options)
            .child(
                div()
                    .id("admin-dialog-skip-bulk")
                    .debug_selector(|| "admin-dialog-skip-bulk".into())
                    .mt(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .hover(|el| el.bg(theme.element_hover.opacity(0.6)))
                    .role(gpui::Role::CheckBox)
                    .aria_label(SharedString::from(admin::SKIP_BULK_LABEL))
                    .child(
                        div()
                            .w(px(12.0))
                            .text_color(theme.warning)
                            .child(if skip_bulk { "\u{2713}" } else { "\u{25cb}" }),
                    )
                    .child(admin::SKIP_BULK_LABEL)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_admin_dialog_skip_bulk(!skip_bulk, cx)
                    })),
            )
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "admin-dialog-cancel")
                            .id("admin-dialog-cancel")
                            .debug_selector(|| "admin-dialog-cancel".into())
                            .on_click(cx.listener(|this, _, _, cx| this.close_admin_dialog(cx))),
                    )
                    .child(
                        popover::btn_primary(&theme, if renew { "Renew" } else { "Grant" })
                            .id("admin-dialog-grant")
                            .debug_selector(|| "admin-dialog-grant".into())
                            .on_click(cx.listener(|this, _, _, cx| this.submit_admin_dialog(cx))),
                    ),
            )
            .into_any_element();
        Some(popover::modal("admin-dialog", viewport, card))
    }

    /// "Admin · Space · 3h left" with a Revoke button, only while the chat's
    /// grant is live. `prefix` keeps the header's and the Item tab's
    /// selectors apart.
    pub(super) fn render_admin_badge(
        &self,
        chat_id: &str,
        prefix: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now = admin::now_ms();
        let state = self.state.read(cx);
        let grant = state.admin.grant_for(chat_id, now)?;
        let name = space_display_name(state, &grant.space_id);
        let text = admin::badge_text(&name, grant, now)?;
        let tooltip = admin::badge_tooltip(grant);
        let (border, color) = chip_palette(theme, ChipTone::Warn);
        let selector = format!("{prefix}-admin-badge");
        let revoke = format!("{prefix}-admin-revoke");
        let chat_id = chat_id.to_owned();
        Some(
            crate::components::shell_parts::admin_badge(theme, selector, text.clone(), border, color)
                .child(
                    crate::components::shell_parts::admin_revoke_button(theme, revoke, text)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.revoke_chat_admin(&chat_id, cx);
                        })),
                )
                .when_some(tooltip, |el, tip| el.hint(tip, HintKey::None))
                .into_any_element(),
        )
    }

    /// The Item tab's admin block: the badge while live (with Renew), else a
    /// "Grant admin for this Space…" link. `None` for a chat without a Space.
    pub(super) fn render_item_tab_admin(
        &mut self,
        chat_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.admin_space_of(chat_id, cx)?;
        let is_admin = self.chat_is_admin(chat_id, cx);
        let badge = self.render_admin_badge(chat_id, "chat-item", theme, cx);
        let id = chat_id.to_owned();
        let link = crate::components::shell_parts::panel_link(theme, "chat-item-admin-grant".into(), theme.accent, false)
            .child(admin::grant_entry_label(is_admin))
            .on_click(cx.listener(move |this, _, _, cx| this.open_admin_dialog(&id, cx)));
        Some(
            div()
                .debug_selector(|| "chat-item-admin".into())
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.0))
                .children(badge)
                .child(link)
                .into_any_element(),
        )
    }
}
