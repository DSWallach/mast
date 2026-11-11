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
        mast_proto::SideChatDelivery::Queue => {
            format!("Queued {what} for when the main chat's turn ends")
        }
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
        let state = self.state.read(cx);
        let local = state.engine().is_some_and(|engine| {
            engine
                .engine_info()
                .supports(mast_proto::capabilities::SIDE_CHAT_V1)
        });
        local
            && state
                .chats
                .iter()
                .any(|chat| chat.id == chat_id && !chat.archived && !chat.is_side_chat())
            && state.chat_host_supports(chat_id, mast_proto::capabilities::SIDE_CHAT_V1)
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
                Ok(_) => {}
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
            "Queued 2 exchanges for when the main chat's turn ends"
        );
    }
}
