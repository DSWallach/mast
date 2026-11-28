//! A transient Shell-level notice with an optional action: one at a time, a
//! new one replaces the old, and it dismisses itself after a few seconds or on
//! click. Small and reusable; the work outline's own toast is a separate thing.
use std::time::Duration;

use super::*;

/// How long a notice stays before it dismisses itself.
const NOTICE_LIFETIME: Duration = Duration::from_secs(6);

#[derive(Clone)]
pub(super) struct ShellNotice {
    id: u64,
    text: SharedString,
    /// A label and the chat it opens.
    action: Option<(SharedString, String)>,
}

impl Shell {
    /// Show `text`, replacing any notice already up. `action` is a label and
    /// the id of a chat the button opens.
    pub(super) fn show_notice(&mut self, text: impl Into<SharedString>, action: Option<(String, String)>, cx: &mut Context<Self>) {
        self.notice_serial += 1;
        let id = self.notice_serial;
        self.notice = Some(ShellNotice {
            id,
            text: text.into(),
            action: action.map(|(label, chat)| (SharedString::from(label), chat)),
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE_LIFETIME).await;
            this.update(cx, |this, cx| {
                if this.notice.as_ref().is_some_and(|notice| notice.id == id) {
                    this.notice = None;
                    cx.notify();
                }
            }).ok();
        }).detach();
    }

    pub(super) fn render_notice(&mut self, viewport: gpui::Size<Pixels>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notice = self.notice.clone()?;
        let theme = Theme::of(cx).for_popup();
        let mut strip = popover::popover_card(&theme)
            .id("shell-notice")
            .debug_selector(|| "shell-notice".into())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .px(px(14.0))
            .py(px(8.0))
            .child(div().text_color(theme.text).child(notice.text.clone()));
        if let Some((label, chat)) = notice.action {
            strip = strip.child(
                div()
                    .id("shell-notice-action")
                    .debug_selector(|| "shell-notice-action".into())
                    .cursor_pointer()
                    .text_color(theme.accent)
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.notice = None;
                        this.open_chat(chat.clone(), cx);
                    })),
            );
        }
        strip = strip.child(
            div()
                .id("shell-notice-dismiss")
                .debug_selector(|| "shell-notice-dismiss".into())
                .cursor_pointer()
                .text_color(theme.text_muted)
                .child("Dismiss")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.notice = None;
                    cx.notify();
                })),
        );
        Some(
            div()
                .absolute()
                .bottom(px(24.0))
                .w(viewport.width)
                .flex()
                .justify_center()
                .child(strip)
                .into_any_element(),
        )
    }
}
