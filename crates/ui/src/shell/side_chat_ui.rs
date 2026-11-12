//! "Ask on the side": a side chat in the main chat's right pane, with its
//! own composer and an "Absorb into main chat" action.
use super::*;
use crate::components::side_chat_parts as parts;

/// Right-pane side chat tabs, keyed by [`RightSurface::SideChat`] handles.
#[derive(Default)]
pub(super) struct SideChatTabs {
    seq: u64,
    pub(super) tabs: std::collections::HashMap<u64, Entity<SideChatPane>>,
    subs: std::collections::HashMap<u64, Subscription>,
}

pub(crate) enum SideChatPaneEvent {
    Close,
}

/// Whether "Absorb into main chat" can run, and why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Absorb {
    /// The side chat is answering; absorb once it is done.
    Busy,
    /// Every answered exchange is already in the main chat.
    NothingNew,
    Ready(usize),
}

pub(crate) fn absorb_availability(
    entries: &[mast_doc::SessionMessageEntry],
    absorbed_through: Option<&str>,
    busy: bool,
) -> Absorb {
    if busy {
        return Absorb::Busy;
    }
    match mast_doc::side_chat::absorb_batch(entries, absorbed_through) {
        Some(batch) => Absorb::Ready(batch.exchanges.len()),
        None => Absorb::NothingNew,
    }
}

pub(crate) struct SideChatPane {
    state: Entity<AppState>,
    pub(crate) chat_id: String,
    transcript: Entity<Transcript>,
    input: Entity<ComposerInput>,
    focus_pending: bool,
    sending: bool,
    absorbing: bool,
    notice: Option<SharedString>,
    task: Option<Task<()>>,
    _input_events: Subscription,
    _state: Subscription,
}

impl gpui::EventEmitter<SideChatPaneEvent> for SideChatPane {}

impl SideChatPane {
    fn new(state: Entity<AppState>, chat_id: String, cx: &mut Context<Self>) -> Self {
        let transcript = cx.new(|cx| Transcript::for_doc(state.clone(), chat_id.clone(), true, cx));
        state.update(cx, |s, cx| s.watch_subagent_doc(chat_id.clone(), cx));
        let input = cx.new(|cx| {
            ComposerInput::new("Ask about this chat…", cx)
                .with_text_metrics(13.0, 19.0)
                .with_auto_grow()
        });
        let input_events = cx.subscribe(&input, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Submitted => this.send(cx),
            ComposerInputEvent::Edited => cx.notify(),
            _ => {}
        });
        let state_events = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            chat_id,
            transcript,
            input,
            focus_pending: true,
            sending: false,
            absorbing: false,
            notice: None,
            task: None,
            _input_events: input_events,
            _state: state_events,
        }
    }

    fn busy(&self, cx: &App) -> bool {
        matches!(
            self.state.read(cx).indicator_for(&self.chat_id, Utc::now()),
            Indicator::Working | Indicator::AwaitingInput
        )
    }

    fn absorb_state(&self, cx: &App) -> Absorb {
        let state = self.state.read(cx);
        let marker = state
            .chats
            .iter()
            .find(|chat| chat.id == self.chat_id)
            .and_then(|chat| chat.side_absorbed_through.as_deref());
        absorb_availability(state.sub_transcript(&self.chat_id), marker, self.busy(cx))
    }

    /// The side chat's host, for forwardable RPCs.
    fn host(&self, cx: &App) -> Option<String> {
        self.state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == self.chat_id)
            .map(|chat| chat.device_id.clone())
    }

    /// Questions always go through the queue: an idle side chat runs it at
    /// once, a busy one after its current answer.
    fn send(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().trim().to_owned();
        if text.is_empty() || self.sending {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let params = serde_json::json!({
            "chatId": self.chat_id,
            "messageId": uuid::Uuid::new_v4().to_string(),
            "text": text,
            "holdForTurnEnd": true,
            "targetDeviceId": self.host(cx),
        });
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.sending = true;
        self.notice = None;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::QUEUE_MESSAGE, params).await;
            this.update(cx, |pane, cx| {
                pane.sending = false;
                if let Err(err) = result {
                    pane.input.update(cx, |input, cx| input.set_text(text, cx));
                    pane.notice = Some(format!("Couldn't send: {err}").into());
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn absorb(&mut self, cx: &mut Context<Self>) {
        if self.absorbing || !matches!(self.absorb_state(cx), Absorb::Ready(_)) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let params = serde_json::json!({
            "chatId": self.chat_id,
            "targetDeviceId": self.host(cx),
        });
        self.absorbing = true;
        self.notice = None;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::ABSORB_SIDE_CHAT, params)
                .await
                .and_then(|value| {
                    serde_json::from_value::<mast_proto::AbsorbSideChatReply>(value)
                        .map_err(|e| mast_rpc::RpcError::Failed(e.to_string()))
                });
            this.update(cx, |pane, cx| {
                pane.absorbing = false;
                pane.notice = Some(match result {
                    Ok(reply) => absorbed_notice(&reply).into(),
                    Err(err) => format!("Couldn't absorb: {err}").into(),
                });
                cx.notify();
            })
            .ok();
        }));
    }
}

fn absorbed_notice(reply: &mast_proto::AbsorbSideChatReply) -> String {
    let what = if reply.exchanges == 1 {
        "1 exchange".to_owned()
    } else {
        format!("{} exchanges", reply.exchanges)
    };
    match reply.delivery {
        mast_proto::SideChatDelivery::Run => format!("Sent {what} to the main chat"),
        mast_proto::SideChatDelivery::Queue => format!("Queued {what} in the main chat"),
    }
}

impl Render for SideChatPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        if self.focus_pending {
            self.focus_pending = false;
            window.focus(&self.input.read(cx).focus_handle(cx), cx);
        }
        let absorb = self.absorb_state(cx);
        let busy = absorb == Absorb::Busy;
        let empty = self.state.read(cx).sub_transcript(&self.chat_id).is_empty() && !self.sending;
        let pane = cx.entity().downgrade();
        let header = crate::surface_chrome::toolbar(&theme)
            .debug_selector(|| "side-chat-header".into())
            .child(
                icon(icons::CHAT_ROUND_LINE)
                    .size(px(14.0))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from("Side chat")),
            )
            .child(
                parts::side_chat_absorb_button(
                    &theme,
                    if self.absorbing {
                        "Absorbing…"
                    } else {
                        "Absorb into main chat"
                    },
                    matches!(absorb, Absorb::Ready(_)) && !self.absorbing,
                )
                .on_click(cx.listener(|this, _, _, cx| this.absorb(cx))),
            )
            .child(crate::components::shell_parts::header_icon_button(
                "side-chat-close",
                icons::CLOSE,
                "Close",
                false,
                &theme,
                move |_, _, cx| {
                    pane.update(cx, |_, cx| cx.emit(SideChatPaneEvent::Close))
                        .ok();
                },
            ));
        let status: Option<SharedString> = if busy {
            Some("Answering…".into())
        } else if self.sending {
            Some("Sending…".into())
        } else {
            self.notice.clone()
        };
        let send = crate::components::conversation_send_key(&theme, theme.accent, false, busy)
            .debug_selector(|| "side-chat-send".into())
            .on_click(cx.listener(|this, _, _, cx| this.send(cx)));
        div()
            .size_full()
            .flex()
            .flex_col()
            .debug_selector(|| "side-chat-pane".into())
            .child(header)
            .child(if empty {
                parts::side_chat_empty(
                    &theme,
                    "Ask on the side",
                    "This chat knows everything the main chat does. Nothing here enters the main \
                     chat until you absorb it.",
                )
                .flex_1()
                .min_h_0()
                .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.transcript.clone())
                    .into_any_element()
            })
            .children(status.map(|text| parts::side_chat_hint(&theme, text)))
            .child(parts::side_chat_composer(
                &theme,
                self.input.clone().into_any_element(),
                send.into_any_element(),
            ))
    }
}

impl Shell {
    /// "Ask on the side…" is offered when both this engine and the chat's
    /// host understand side chats, and never on a side chat itself.
    pub(super) fn can_ask_on_side(&self, chat_id: &str, cx: &App) -> bool {
        use mast_proto::capabilities::SIDE_CHAT_V1;
        let state = self.state.read(cx);
        let local = match state.engine() {
            Some(engine) => engine.engine_info().supports(SIDE_CHAT_V1),
            None => state
                .local_device_id
                .as_deref()
                .is_some_and(|device| state.device_supports(device, SIDE_CHAT_V1)),
        };
        local
            && state
                .chats
                .iter()
                .any(|chat| chat.id == chat_id && !chat.archived && !chat.is_side_chat())
            && state.chat_host_supports(chat_id, SIDE_CHAT_V1)
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn fixture_snapshot_side_chat(&mut self, cx: &mut Context<Self>) {
        self.active_chat = "fixture-chat".into();
        self.add_side_chat_surface("fixture-side".into(), cx);
        self.chat_menu.open(ChatMenuState {
            chat_id: "fixture-chat".into(),
            position: gpui::point(px(560.0), px(64.0)),
            page: ChatMenuPage::Root,
            closeable: false,
        });
    }

    /// Create a side chat of `main_id` and open it in the main chat's right
    /// pane.
    pub(super) fn ask_on_side(&mut self, main_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        if self.state.read(cx).selected_chat.as_deref() != Some(main_id.as_str()) {
            self.open_chat(main_id.clone(), cx);
        }
        let side_id = uuid::Uuid::new_v4().to_string();
        let params =
            serde_json::json!({ "op": "createChat", "chatId": side_id, "sideOf": main_id });
        self.side_chats_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::MUTATE, params).await;
            this.update(cx, |shell, cx| match result {
                Ok(_) if shell.panel_key(cx) == main_id => shell.add_side_chat_surface(side_id, cx),
                // The user moved on before it was created: nothing will show
                // it, so it must not linger.
                Ok(_) => shell.archive_chat(side_id, cx),
                Err(err) => {
                    shell.sidebar_notice = Some(format!("Couldn't open a side chat: {err}").into());
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    pub(super) fn add_side_chat_surface(&mut self, side_id: String, cx: &mut Context<Self>) {
        self.set_surfaces_open(true, cx);
        self.side_chats.seq += 1;
        let id = self.side_chats.seq;
        let state = self.state.clone();
        let pane = cx.new(|cx| SideChatPane::new(state, side_id, cx));
        let sub = cx.subscribe(&pane, move |this, _, event, cx| match event {
            SideChatPaneEvent::Close => this.close_side_chat(id, cx),
        });
        self.side_chats.tabs.insert(id, pane);
        self.side_chats.subs.insert(id, sub);
        let key = self.panel_key(cx);
        self.right_tabs
            .entry(key)
            .or_default()
            .push(RightSurface::SideChat(id));
        self.set_right_active(RightSurface::SideChat(id), cx);
    }

    pub(super) fn side_chat_pane(&self, id: u64) -> Option<&Entity<SideChatPane>> {
        self.side_chats.tabs.get(&id)
    }

    /// Closing a side chat archives it: it lives only in this pane.
    pub(super) fn close_side_chat(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(tabs) = self
            .right_tabs
            .values_mut()
            .find(|tabs| tabs.contains(&RightSurface::SideChat(id)))
        {
            tabs.retain(|surface| *surface != RightSurface::SideChat(id));
        }
        self.drop_side_chat(id, cx);
        let key = self.panel_key(cx);
        self.panels.update(&key, |p| {
            if p.right_active == RightSurface::SideChat(id) {
                p.right_active = RightSurface::Picker;
            }
        });
        self.collapse_surfaces_if_empty(&key, cx);
        cx.notify();
    }

    /// Tear down a side chat tab's pane and archive its chat.
    pub(super) fn drop_side_chat(&mut self, id: u64, cx: &mut Context<Self>) {
        self.side_chats.subs.remove(&id);
        if let Some(pane) = self.side_chats.tabs.remove(&id) {
            let chat_id = pane.read(cx).chat_id.clone();
            self.state
                .update(cx, |s, _| s.unwatch_subagent_doc(&chat_id));
            self.archive_chat(chat_id, cx);
        }
    }

    /// Drop the side chats in `main_id`'s pane (that chat is going away).
    pub(super) fn drop_side_chats_of(&mut self, main_id: &str, cx: &mut Context<Self>) {
        let Some(tabs) = self.right_tabs.get_mut(main_id) else {
            return;
        };
        let ids: Vec<u64> = tabs
            .iter()
            .filter_map(|surface| match surface {
                RightSurface::SideChat(id) => Some(*id),
                _ => None,
            })
            .collect();
        tabs.retain(|surface| !matches!(surface, RightSurface::SideChat(_)));
        for id in ids {
            self.drop_side_chat(id, cx);
        }
    }

    /// A side pane whose tab is gone, whose main chat is archived or gone, or
    /// whose side chat was archived elsewhere is closed (and archived): it
    /// could never be reached again.
    pub(super) fn reconcile_side_chats(&mut self, cx: &mut Context<Self>) {
        if self.side_chats.tabs.is_empty() {
            return;
        }
        let state = self.state.read(cx);
        let row = |id: &str| state.chats.iter().find(|chat| chat.id == id);
        let stale: Vec<u64> = self
            .side_chats
            .tabs
            .iter()
            .filter(|(id, pane)| {
                let surface = RightSurface::SideChat(**id);
                let Some(main_id) = self
                    .right_tabs
                    .iter()
                    .find(|(_, tabs)| tabs.contains(&surface))
                    .map(|(key, _)| key.as_str())
                else {
                    return true;
                };
                let main_gone = match row(main_id) {
                    Some(main) => main.archived,
                    None => state.chats_synced,
                };
                main_gone || row(&pane.read(cx).chat_id).is_some_and(|side| side.archived)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            for tabs in self.right_tabs.values_mut() {
                tabs.retain(|surface| *surface != RightSurface::SideChat(id));
            }
            self.drop_side_chat(id, cx);
        }
    }

    /// Whether the side chat behind a tab is answering (its tab spins).
    pub(super) fn side_chat_running(&self, id: u64, cx: &App) -> bool {
        self.side_chats
            .tabs
            .get(&id)
            .is_some_and(|pane| pane.read(cx).busy(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};

    fn entry(
        id: &str,
        role: MessageRole,
        text: &str,
        status: Option<MessageStatus>,
    ) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role,
            parts: vec![MessagePart::Text {
                id: "t".into(),
                text: text.into(),
            }],
            created_at: 0,
            device_id: "d".into(),
            status,
            continuation_of: None,
        }
    }

    #[test]
    fn absorb_is_offered_only_for_new_answered_exchanges_while_idle() {
        let entries = vec![
            entry("u1", MessageRole::User, "q", None),
            entry(
                "a1",
                MessageRole::Assistant,
                "a",
                Some(MessageStatus::Complete),
            ),
        ];
        assert_eq!(absorb_availability(&entries, None, false), Absorb::Ready(1));
        assert_eq!(absorb_availability(&entries, None, true), Absorb::Busy);
        assert_eq!(
            absorb_availability(&entries, Some("a1"), false),
            Absorb::NothingNew
        );
        assert_eq!(absorb_availability(&[], None, false), Absorb::NothingNew);
    }

    #[gpui::test]
    fn a_side_chat_pane_opens_in_the_main_chat_and_closing_archives_it(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::work_outline::store::fixture_ids::{CHAT_API, CHAT_UI};
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state
                    .chats
                    .iter_mut()
                    .find(|chat| chat.id == CHAT_UI)
                    .unwrap()
                    .side_of = Some(CHAT_API.into());
            });
            // No engine here: never offered, and never on a side chat.
            assert!(!shell.can_ask_on_side(CHAT_API, cx));
            assert!(!shell.can_ask_on_side(CHAT_UI, cx));
            shell.add_side_chat_surface(CHAT_UI.into(), cx);
            let key = shell.panel_key(cx);
            assert_eq!(key, CHAT_API);
            assert_eq!(shell.right_tabs[&key], [RightSurface::SideChat(1)]);
            assert_eq!(shell.resolved_right_active(cx), RightSurface::SideChat(1));
            assert_eq!(
                shell.right_surface_rows(cx)[0].1,
                SharedString::from("Side chat")
            );
            assert!(!shell.side_chat_running(1, cx));
        });
        cx.run_until_parked();
        shell.update(cx, |shell, cx| {
            let pane = shell.side_chat_pane(1).unwrap().clone();
            pane.update(cx, |_, cx| cx.emit(SideChatPaneEvent::Close));
        });
        cx.run_until_parked();
        shell.read_with(cx, |shell, cx| {
            assert!(shell.side_chat_pane(1).is_none());
            assert!(
                shell
                    .right_tabs
                    .get(CHAT_API)
                    .is_none_or(|tabs| tabs.is_empty())
            );
            assert_ne!(shell.resolved_right_active(cx), RightSurface::SideChat(1));
            assert!(shell.mutations.iter().any(|m| m["op"] == "setChatArchived"
                && m["chatId"] == CHAT_UI
                && m["archived"] == true));
            assert!(!shell.mutations.iter().any(|m| m["chatId"] == CHAT_API));
        });
    }

    #[gpui::test]
    fn a_side_pane_that_can_no_longer_be_reached_is_closed_and_archived(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::work_outline::store::fixture_ids::{CHAT_API, CHAT_LOOSE_A, CHAT_UI};
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        let archived = |shell: &Shell, id: &str| {
            shell.mutations.iter().any(|m| {
                m["op"] == "setChatArchived" && m["chatId"] == id && m["archived"] == true
            })
        };
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                for id in [CHAT_UI, CHAT_LOOSE_A] {
                    state.chats.iter_mut().find(|chat| chat.id == id).unwrap().side_of =
                        Some(CHAT_API.into());
                }
            });
            shell.add_side_chat_surface(CHAT_UI.into(), cx);
            shell.add_side_chat_surface(CHAT_LOOSE_A.into(), cx);
            // Its tab vanished some other way.
            shell
                .right_tabs
                .get_mut(CHAT_API)
                .unwrap()
                .retain(|surface| *surface != RightSurface::SideChat(1));
            shell.state.update(cx, |_, cx| cx.notify());
        });
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| {
            assert!(shell.side_chat_pane(1).is_none());
            assert!(archived(shell, CHAT_UI));
            assert!(shell.side_chat_pane(2).is_some());
        });
        // The main chat is archived: its side panes go with it.
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.chats.iter_mut().find(|chat| chat.id == CHAT_API).unwrap().archived = true;
                cx.notify();
            });
        });
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| {
            assert!(shell.side_chat_pane(2).is_none());
            assert!(archived(shell, CHAT_LOOSE_A));
            assert!(shell.right_tabs[CHAT_API].is_empty());
        });
    }

    #[gpui::test]
    fn deleting_the_main_chat_archives_its_side_chats(cx: &mut gpui::TestAppContext) {
        use crate::work_outline::store::fixture_ids::{CHAT_API, CHAT_UI};
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.chats.iter_mut().find(|chat| chat.id == CHAT_UI).unwrap().side_of =
                    Some(CHAT_API.into());
            });
            shell.add_side_chat_surface(CHAT_UI.into(), cx);
            shell.delete_chat(CHAT_API.into(), cx);
        });
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| {
            assert!(shell.side_chat_pane(1).is_none());
            assert!(shell.mutations.iter().any(|m| m["op"] == "setChatArchived"
                && m["chatId"] == CHAT_UI));
        });
    }

    #[test]
    fn absorbed_notice_names_the_delivery() {
        let reply = |delivery, exchanges| mast_proto::AbsorbSideChatReply {
            delivery,
            exchanges,
            absorbed_through: "a".into(),
        };
        assert_eq!(
            absorbed_notice(&reply(mast_proto::SideChatDelivery::Run, 1)),
            "Sent 1 exchange to the main chat"
        );
        assert_eq!(
            absorbed_notice(&reply(mast_proto::SideChatDelivery::Queue, 2)),
            "Queued 2 exchanges in the main chat"
        );
    }
}

/// Preview state: a side chat with two answered questions (the first one
/// absorbed) and the main transcript showing the absorbed pill.
#[cfg(feature = "snapshot")]
pub(crate) fn fixture_side_chat_state(state: &mut AppState, cx: &mut Context<AppState>) {
    let entries = |value: serde_json::Value| -> Vec<mast_doc::SessionMessageEntry> {
        serde_json::from_value(value).unwrap()
    };
    let side = entries(serde_json::json!([
        {"id":"s-u1","role":"user","parts":[{"id":"t","kind":"text","text":"Why did the spacing drift in the first place?"}],"createdAt":1788900100000_i64,"deviceId":"local"},
        {"id":"s-a1","role":"assistant","parts":[{"id":"r","kind":"reasoning","text":"Checking the layout history."},{"id":"x","kind":"tool","call":{"kind":"exec","command":"git log -S gap -- crates/ui"},"resolved":true,"output":"3 commits"},{"id":"t","kind":"text","text":"The caption row used a hard-coded 6px gap while the controls used the theme's control gap, so the two drifted when the compact layout shrank the tokens."}],"createdAt":1788900101000_i64,"deviceId":"local","status":"complete"},
        {"id":"s-u2","role":"user","parts":[{"id":"t","kind":"text","text":"Does that affect the Overview too?"}],"createdAt":1788900102000_i64,"deviceId":"local"},
        {"id":"s-a2","role":"assistant","parts":[{"id":"t","kind":"text","text":"No. The Overview rows take their gap from the same token, so only the sidebar caption was off."}],"createdAt":1788900103000_i64,"deviceId":"local","status":"complete"}
    ]));
    let absorbed = mast_proto::side_chat::absorb_message(&[mast_proto::SideExchange {
        question: "Why did the spacing drift in the first place?".into(),
        answer: "The caption row used a hard-coded 6px gap while the controls used the theme's control gap.".into(),
    }]);
    let main = entries(serde_json::json!([
        {"id":"user","role":"user","parts":[{"id":"text","kind":"text","text":"Review the sidebar spacing."}],"createdAt":1788900000000_i64,"deviceId":"local"},
        {"id":"assistant","role":"assistant","parts":[{"id":"text","kind":"text","text":"I inspected the layout. The controls now align with their captions."}],"createdAt":1788900001000_i64,"deviceId":"local","status":"complete"},
        {"id":"absorbed","role":"user","parts":[{"id":"text","kind":"text","text":absorbed}],"createdAt":1788900104000_i64,"deviceId":"local"},
        {"id":"ack","role":"assistant","parts":[{"id":"text","kind":"text","text":"Noted. I'll keep the caption gap on the control token from now on."}],"createdAt":1788900105000_i64,"deviceId":"local","status":"complete"}
    ]));
    state.devices.push(
        serde_json::from_value(serde_json::json!({
            "id": "local", "name": "This Mac", "platform": "macos",
            "capabilities": [mast_proto::capabilities::SIDE_CHAT_V1],
        }))
        .unwrap(),
    );
    state.chats.push(
        serde_json::from_value(serde_json::json!({
            "id": "fixture-side", "deviceId": "local", "spaceId": "fixture-space",
            "sideOf": "fixture-chat", "sideAbsorbedThrough": "s-a1", "title": "Side chat",
            "archived": false, "createdAt": "2026-09-08T00:00:00Z"
        }))
        .unwrap(),
    );
    state
        .receive_transcript_frame(mast_doc::TranscriptFrame::Reset { reset: main }, cx)
        .unwrap();
    state.set_subagent_snapshot("fixture-side".into(), side);
}
