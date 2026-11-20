use super::*;

/// Find the unresolved input request the panel should serve, if any: an
/// unresolved input part on the LAST assistant entry — regardless of the
/// entry's run status. The question stays answerable until the user actually
/// answers it (user requirement): a run that died under its question (engine
/// restart reaping it) leaves an aborted entry whose answer the engine
/// delivers as a resumed turn (`RespondInput`'s dead-run fallback). A newer
/// assistant entry supersedes an unanswered question. Assistant-entry-scoped,
/// not last-entry: a steer prompt sent while the agent waits appends a USER
/// entry after the streaming assistant entry, and a last-entry-only read made
/// the QuestionPanel vanish exactly when the user typed (earlier forensics;
/// matches the original composer.tsx, which reads the live-assistant fold —
/// rebuilt from replay even after the run died).
pub fn pending_input_request(
    transcript: &[SessionMessageEntry],
) -> Option<(String, Vec<UserInputQuestion>)> {
    transcript
        .iter()
        .rev()
        .find(|entry| entry.role == MessageRole::Assistant)
        .and_then(|entry| {
            entry.parts.iter().find_map(|part| match part {
                MessagePart::Input {
                    request_id,
                    questions,
                    resolved: false,
                    ..
                } => Some((request_id.clone(), questions.clone())),
                _ => None,
            })
        })
}

/// Whether the transcript shows `request_id` explicitly resolved (here or on
/// another device) — the wizard latch's release condition.
pub fn input_request_resolved(transcript: &[SessionMessageEntry], request_id: &str) -> bool {
    transcript.iter().any(|entry| {
        entry.parts.iter().any(|part| {
            matches!(
                part,
                MessagePart::Input {
                    request_id: rid,
                    resolved: true,
                    ..
                } if rid == request_id
            )
        })
    })
}

// ---------------------------------------------------------------------------
// Question wizard (pure reducer)
// ---------------------------------------------------------------------------

/// Reducer outcome of a wizard interaction.
#[derive(Debug, Clone, PartialEq)]
pub enum WizardStep {
    Stay,
    /// Single-select landed — advance after [`AUTO_ADVANCE_MS`].
    AutoAdvance,
    /// The current page has no answer the host would accept yet.
    Incomplete,
    /// All pages answered — submit these answers.
    Done(Vec<UserInputAnswer>),
}

/// Paged question state ("1/3"): single-select auto-advances, multi-select and
/// typed answers advance explicitly, number keys 1-9 select, Back pages back.
#[derive(Debug, Clone)]
pub struct Wizard {
    pub request_id: String,
    pub questions: Vec<UserInputQuestion>,
    pub page: usize,
    picked: Vec<Vec<usize>>,
    typed: Vec<String>,
}

impl Wizard {
    pub fn new(request_id: String, questions: Vec<UserInputQuestion>) -> Self {
        let n = questions.len();
        Self {
            request_id,
            questions,
            page: 0,
            picked: vec![Vec::new(); n],
            typed: vec![String::new(); n],
        }
    }

    pub fn counter(&self) -> String {
        format!("{}/{}", self.page + 1, self.questions.len().max(1))
    }

    pub fn current(&self) -> Option<&UserInputQuestion> {
        self.questions.get(self.page)
    }

    pub fn is_picked(&self, option_ix: usize) -> bool {
        self.picked
            .get(self.page)
            .is_some_and(|p| p.contains(&option_ix))
    }

    /// Whether the current page has any picked option.
    pub fn page_has_pick(&self) -> bool {
        self.picked.get(self.page).is_some_and(|p| !p.is_empty())
    }

    /// Click/tap an option.
    pub fn select(&mut self, option_ix: usize) -> WizardStep {
        let Some(question) = self.questions.get(self.page) else {
            return WizardStep::Stay;
        };
        if option_ix >= question.options.len() {
            return WizardStep::Stay;
        }
        let multi = question.multi_select;
        let Some(picked) = self.picked.get_mut(self.page) else {
            return WizardStep::Stay;
        };
        if multi {
            match picked.iter().position(|&p| p == option_ix) {
                Some(at) => {
                    picked.remove(at);
                }
                None => picked.push(option_ix),
            }
            WizardStep::Stay
        } else {
            *picked = vec![option_ix];
            WizardStep::AutoAdvance
        }
    }

    /// Number key 1-9.
    pub fn press_number(&mut self, number: usize) -> WizardStep {
        if number == 0 {
            return WizardStep::Stay;
        }
        self.select(number - 1)
    }

    pub fn set_typed(&mut self, text: String) {
        if let Some(slot) = self.typed.get_mut(self.page) {
            *slot = text;
        }
    }

    /// Free text saved for the current page.
    pub fn typed(&self) -> &str {
        self.typed.get(self.page).map(String::as_str).unwrap_or("")
    }

    /// Whether the current question takes free text: open questions always
    /// do; option lists only when the harness allows custom answers.
    pub fn accepts_typed(&self) -> bool {
        self.current()
            .is_some_and(|q| q.options.is_empty() || q.allow_custom)
    }

    /// Whether non-empty `typed` text answers the current question: free text
    /// where it's taken, otherwise only an option's name.
    pub fn typed_is_answer(&self, typed: &str) -> bool {
        let typed = typed.trim();
        !typed.is_empty()
            && (self.accepts_typed()
                || self
                    .current()
                    .is_some_and(|q| q.options.iter().any(|o| o.eq_ignore_ascii_case(typed))))
    }

    /// Whether the current page, with `typed` in the input, has an answer
    /// `validate_input_answers` would accept.
    pub fn page_answerable(&self, typed: &str) -> bool {
        self.current().is_some_and(|q| q.options.is_empty())
            || self.page_has_pick()
            || self.typed_is_answer(typed)
    }

    /// Explicit submit / auto-advance landing.
    pub fn advance(&mut self) -> WizardStep {
        if !self.page_answerable(self.typed()) {
            return WizardStep::Incomplete;
        }
        if self.page + 1 < self.questions.len() {
            self.page += 1;
            WizardStep::Stay
        } else {
            WizardStep::Done(self.answers())
        }
    }

    /// Page back; false when already on the first page.
    pub fn back(&mut self) -> bool {
        if self.page > 0 {
            self.page -= 1;
            true
        } else {
            false
        }
    }

    /// Answers per question: free text overrides picked labels where the
    /// question takes free text (or the text names one of its options).
    pub fn answers(&self) -> Vec<UserInputAnswer> {
        self.questions
            .iter()
            .enumerate()
            .map(|(ix, q)| {
                let typed = self.typed.get(ix).map(|s| s.trim()).unwrap_or("");
                let typed_counts = q.options.is_empty()
                    || q.allow_custom
                    || q.options.iter().any(|o| o.eq_ignore_ascii_case(typed));
                let labels = if !typed.is_empty() && typed_counts {
                    vec![typed.to_string()]
                } else {
                    self.picked
                        .get(ix)
                        .map(|picked| {
                            picked
                                .iter()
                                .filter_map(|&p| q.options.get(p).cloned())
                                .collect()
                        })
                        .unwrap_or_default()
                };
                UserInputAnswer {
                    question_id: q.id.clone(),
                    labels,
                }
            })
            .collect()
    }
}

impl Composer {
    pub(super) fn wizard_select(&mut self, option_ix: usize, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        let step = wizard.select(option_ix);
        if step == WizardStep::AutoAdvance {
            // Clicking a single-select option is the answer: drop any typed
            // text so the advance doesn't pick it up as an override.
            wizard.set_typed(String::new());
            self.input.update(cx, |input, cx| input.set_text("", cx));
        }
        self.refresh_wizard_placeholder(cx);
        match step {
            WizardStep::AutoAdvance => self.schedule_auto_advance(cx),
            WizardStep::Done(answers) => self.wizard_finish(answers, cx),
            WizardStep::Stay | WizardStep::Incomplete => {}
        }
        cx.notify();
    }

    pub(super) fn refresh_wizard_placeholder(&mut self, cx: &mut Context<Self>) {
        let can_clarify = self.can_ask_on_side(cx);
        if let Some(wizard) = self.wizard.as_ref() {
            let placeholder = wizard_placeholder(wizard, can_clarify);
            self.input
                .update(cx, |input, cx| input.set_placeholder(placeholder, cx));
        }
    }

    pub(super) fn can_ask_on_side(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        state
            .selected_chat
            .as_deref()
            .is_some_and(|chat_id| state.can_ask_on_side(chat_id))
    }

    /// Open a side chat of the selected chat, asking `ask` first. While the
    /// agent's question panel is up, the side chat is told the question and
    /// its options, and the panel stays up for the answer.
    pub(super) fn ask_on_side(&mut self, ask: Option<String>, cx: &mut Context<Self>) {
        let chat_id = self
            .state
            .read(cx)
            .selected_chat
            .clone()
            .filter(|_| self.can_ask_on_side(cx));
        let Some(chat_id) = chat_id else {
            self.failure = Some("Side chats aren't available for this chat.".into());
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
            return;
        };
        let question = match self.wizard.as_ref().and_then(Wizard::current) {
            Some(pending) => Some(clarify_prompt(pending, ask.as_deref())),
            None => ask,
        };
        if let Some(wizard) = self.wizard.as_mut() {
            wizard.set_typed(String::new());
        }
        self.input.update(cx, |input, cx| input.set_text("", cx));
        cx.emit(ComposerEvent::AskOnSide { chat_id, question });
        cx.notify();
    }

    /// The question panel's Clarify: ask about the choice in a side chat,
    /// with whatever is typed as the question.
    fn wizard_clarify(&mut self, cx: &mut Context<Self>) {
        let typed = self.input.read(cx).text().trim().to_string();
        self.ask_on_side((!typed.is_empty()).then_some(typed), cx);
    }

    /// Save the shared input's text as the current page's free-text answer.
    /// Every page change goes through this, so Submit, Next, Back, Enter and
    /// auto-advance all see what was typed.
    fn wizard_capture_typed(&mut self, cx: &mut Context<Self>) {
        let typed = self.input.read(cx).text().trim().to_string();
        if let Some(wizard) = self.wizard.as_mut() {
            wizard.set_typed(typed);
        }
    }

    /// Load the current page's saved free text into the shared input.
    fn wizard_restore_typed(&mut self, cx: &mut Context<Self>) {
        let typed = self
            .wizard
            .as_ref()
            .map(|w| w.typed().to_string())
            .unwrap_or_default();
        self.input.update(cx, |input, cx| input.set_text(typed, cx));
        self.refresh_wizard_placeholder(cx);
    }

    pub(super) fn schedule_auto_advance(&mut self, cx: &mut Context<Self>) {
        self.advance_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(AUTO_ADVANCE_MS))
                .await;
            this.update(cx, |composer, cx| composer.wizard_advance(cx))
                .ok();
        }));
    }

    pub(super) fn wizard_advance(&mut self, cx: &mut Context<Self>) {
        self.wizard_capture_typed(cx);
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        match wizard.advance() {
            WizardStep::Done(answers) => self.wizard_finish(answers, cx),
            // Nothing acceptable to send yet: keep the page and its text.
            WizardStep::Incomplete => cx.notify(),
            _ => {
                self.wizard_restore_typed(cx);
                cx.notify();
            }
        }
    }

    pub(super) fn wizard_back(&mut self, cx: &mut Context<Self>) {
        self.wizard_capture_typed(cx);
        if self.wizard.as_mut().is_some_and(|wizard| wizard.back()) {
            self.wizard_restore_typed(cx);
            cx.notify();
        }
    }

    /// Take the panel down for good: its request counts as handled, and the
    /// borrowed composer input gets its own identity back.
    fn retire_wizard(&mut self, cx: &mut Context<Self>) -> Option<Wizard> {
        let wizard = self.wizard.take()?;
        self.advance_task = None;
        self.answered_requests.insert(wizard.request_id.clone());
        self.input.update(cx, |input, cx| {
            input.set_placeholder("Do anything…", cx);
            input.set_key_context(MESSAGE_COMPOSER_CONTEXT, cx);
        });
        Some(wizard)
    }

    /// Quit the question without answering: stop the run so the chat waits
    /// for the next message. Ending the turn resolves the question in the
    /// transcript; anything typed stays as the draft, and an empty input
    /// gets back the draft stashed when the question opened.
    pub(super) fn wizard_quit(&mut self, cx: &mut Context<Self>) {
        if self.retire_wizard(cx).is_some() {
            let draft = self.wizard_draft.take();
            self.input.update(cx, |input, cx| {
                if let Some(draft) = draft.filter(|_| input.is_empty()) {
                    input.set_text(draft, cx);
                }
            });
            self.interrupt_selected(cx);
            cx.notify();
        }
    }

    /// Submit RespondInput and retire the panel.
    pub(super) fn wizard_finish(&mut self, answers: Vec<UserInputAnswer>, cx: &mut Context<Self>) {
        let Some(wizard) = self.retire_wizard(cx) else {
            return;
        };
        let draft = self.wizard_draft.take().unwrap_or_default();
        self.input.update(cx, |input, cx| input.set_text(draft, cx));
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(chat_id) = self.state.read(cx).selected_chat.clone() else {
            return;
        };
        let request_id = wizard.request_id.clone();
        let command = SessionCommandPayload::RespondInput {
            request_id: request_id.clone(),
            answers,
        };
        let failure_chat = chat_id.clone();
        let params = match serde_json::to_value(&command) {
            Ok(value) => serde_json::json!({ "chatId": chat_id, "command": value }),
            Err(_) => return,
        };
        // `action_task`, NOT `send_task` — see `interrupt`.
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::QUEUE_COMMAND, params).await;
            if let Err(err) = result {
                this.update(cx, |composer, cx| {
                    composer.failure = Some(format!("Answer failed: {err}").into());
                    composer.failure_key = Some(failure_chat);
                    // The answer never left this device — put the panel back.
                    composer.answered_requests.remove(&request_id);
                    cx.notify();
                })
                .ok();
                return;
            }
            // Safety net against a dead-looking session: the command queued,
            // but the host may still REJECT it (e.g. the run's resolver is
            // gone). If the very same request is still the live pending input
            // once the host has had ample time to execute and the resolved
            // flag to sync back, the answer demonstrably didn't take —
            // un-hide the panel instead of leaving the question unanswerable.
            cx.background_executor().timer(Duration::from_secs(2)).await;
            this.update(cx, |composer, cx| {
                let transcript = composer.state.read(cx).transcript.clone();
                let still_pending = pending_input_request(&transcript)
                    .is_some_and(|(pending_id, _)| pending_id == request_id);
                if still_pending && composer.answered_requests.remove(&request_id) {
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn on_wizard_key(
        &mut self,
        event: &KeyDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // Keys bubbling out of the free-text input must not double-handle:
        // digits select options only while the input is empty, and Enter is the
        // input's own Submit action when it has focus.
        let input_focused = self.input.read(cx).focus_handle.is_focused(window);
        let input_empty = self.input.read(cx).is_empty();
        let key = event.keystroke.key.as_str();
        // A BARE digit picks an option. With a modifier held the keystroke
        // belongs to an app shortcut — ⌘1..⌘9 jump to a sidebar row — and the
        // panel must not also consume it as a selection.
        if let Ok(digit) = key.parse::<usize>()
            && (1..=9).contains(&digit)
            && !event.keystroke.modifiers.modified()
        {
            if !input_focused || input_empty {
                self.wizard_select(digit - 1, cx);
                // Consumed as a selection: stop the platform from also
                // inserting the digit into the focused free-text input.
                cx.stop_propagation();
            }
        } else if key == "enter" {
            if !input_focused {
                self.wizard_advance(cx);
                cx.stop_propagation();
            }
        } else if key == "escape" {
            if wizard_escape_goes_back(key, input_focused, input_empty) {
                self.wizard_back(cx);
            }
            cx.stop_propagation();
        }
    }

    // ---- render pieces ----

    /// The agent-asked-a-question panel (zeron question-panel.tsx), rendered in
    /// place of the composer: the same floating-pill chrome (`rounded-[26px]
    /// border-white/[0.08] bg-white/[0.03] shadow-xl`), uppercase header +
    /// "1/3" counter chip, option rows with number kbd chips, a free-text
    /// override over a hairline, and Back / Next-Submit footer.
    pub(super) fn render_wizard(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(wizard) = self.wizard.clone() else {
            return gpui::Empty.into_any_element();
        };
        let counter = wizard.counter();
        let Some(question) = wizard.current().cloned() else {
            return gpui::Empty.into_any_element();
        };
        let page = wizard.page;
        let last = page + 1 >= wizard.questions.len();
        let typed = self.input.read(cx).text().trim().to_string();
        let typed_empty = typed.is_empty() || !wizard.accepts_typed();
        let can_advance = wizard.page_answerable(&typed);
        let can_clarify = self.can_ask_on_side(cx);
        let typed_hint =
            (!typed.is_empty() && !wizard.typed_is_answer(&typed)).then_some(if can_clarify {
                "This question only takes the listed options. Enter asks about it in a side chat."
            } else {
                "This question only takes the listed options."
            });
        // The composer's own render sizes the shared input, and it doesn't
        // run while the panel is up: left at a single-line viewport, wrapped
        // text painted below the panel. Cap it here and let it scroll.
        let viewport = WIZARD_INPUT_MAX_LINES * INPUT_LINE_HEIGHT;
        self.input.update(cx, |input, cx| {
            if input.viewport_height != Some(viewport)
                || input.settled_viewport_height != Some(viewport)
                || input.resizing
                || input.overflow_top_padding != 0.0
            {
                input.viewport_height = Some(viewport);
                input.settled_viewport_height = Some(viewport);
                input.resizing = false;
                input.overflow_top_padding = 0.0;
                cx.notify();
            }
        });

        let options = question.options.iter().enumerate().map(|(ix, label)| {
            // Selection reads on the row only while no typed override exists
            // (typed answers win — zeron question-panel.tsx `isSel`).
            let picked = wizard.is_picked(ix) && typed_empty;
            div()
                .id(SharedString::from(format!("wizard-option-{ix}")))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .px(px(14.0))
                .py(px(10.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(if picked {
                    crate::theme::ink(0.16)
                } else {
                    gpui::transparent_black()
                })
                // zeron question-panel.tsx option rows: `transition-colors`.
                .bg(if picked {
                    crate::theme::ink(0.09)
                } else {
                    motion::hover_blend(
                        &format!("wizard-option-{ix}"),
                        crate::theme::ink(0.025),
                        crate::theme::ink(0.06),
                    )
                })
                .on_hover(motion::hover_listener(format!("wizard-option-{ix}")))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.wizard_select(ix, cx)))
                .role(gpui::Role::Button)
                .aria_label(SharedString::from(label.clone()))
                .aria_selected(picked)
                .debug_selector(move || format!("wizard-option-{ix}"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(crate::typography::ui_rems(13.5))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(if picked {
                            theme.text
                        } else {
                            theme.text.opacity(0.9)
                        })
                        .child(SharedString::from(label.clone())),
                )
                .when(ix < 9, |el| {
                    el.child(
                        // Number kbd chip: `size-[22px] rounded-md text-[11px]`.
                        div()
                            .flex_none()
                            .size(px(22.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .bg(if picked {
                                crate::theme::ink(0.16)
                            } else {
                                crate::theme::ink(0.05)
                            })
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(if picked {
                                theme.text
                            } else {
                                theme.text_muted.opacity(0.6)
                            })
                            .child(SharedString::from(format!("{}", ix + 1))),
                    )
                })
        });

        crate::components::conversation_wizard_surface(
            div()
                .id("question-panel")
                .debug_selector(|| "question-panel".into())
                .role(gpui::Role::Group)
                .when(window.is_a11y_active(), |el| {
                    el.aria_label(SharedString::from(question.header.clone()))
                })
                .track_focus(&self.wizard_focus)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    this.on_wizard_key(event, window, cx)
                })),
            &theme,
            COMPOSER_RADIUS,
        )
        .flex()
        .flex_col()
        .child(
            div()
                .id(("wizard-body", page))
                // Keep the footer inside the window even for large inputs
                // or many options; the body can scroll independently.
                .max_h(window.viewport_size().height * 0.6)
                .overflow_y_scroll()
                .px(px(16.0))
                .pt(px(16.0))
                .flex()
                .flex_col()
                // Header: tracked uppercase + counter chip when paged.
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(10.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text_muted.opacity(0.6))
                                .child(SharedString::from(crate::popover::tracked_upper(
                                    &question.header,
                                ))),
                        )
                        .when(wizard.questions.len() > 1, |el| {
                            el.child(
                                div()
                                    .h(px(20.0))
                                    .px(px(6.0))
                                    .flex()
                                    .items_center()
                                    .rounded(px(6.0))
                                    .bg(crate::theme::ink(0.06))
                                    .text_size(crate::typography::ui_rems(10.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme.text_muted.opacity(0.6))
                                    .child(SharedString::from(counter)),
                            )
                        }),
                )
                .child(
                    div()
                        .id(("wizard-question", page))
                        // A long command gets its own viewport so the
                        // approval choices stay close at hand.
                        .max_h(window.viewport_size().height * 0.25)
                        .flex_shrink_0()
                        .overflow_y_scroll()
                        .mt(px(6.0))
                        .text_size(crate::typography::ui_rems(15.0))
                        .line_height(px(20.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(SharedString::from(question.question.clone())),
                )
                .when(question.multi_select, |el| {
                    el.child(wizard_note(&theme, "Select one or more options.").mt(px(4.0)))
                })
                .child(
                    div()
                        .mt(px(12.0))
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .children(options),
                )
                // Free-text override over a hairline (shares the composer
                // input entity).
                .child(
                    div()
                        .mt(px(12.0))
                        .border_t_1()
                        .border_color(crate::theme::hairline(0.06))
                        .pt(px(12.0))
                        .pb(px(4.0))
                        .px(px(4.0))
                        .child(self.input.clone())
                        .when_some(typed_hint, |el, hint| {
                            el.child(wizard_note(&theme, hint).mt(px(6.0)))
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .justify_between()
                .items_center()
                .px(px(16.0))
                .pb(px(16.0))
                .pt(px(4.0))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            crate::popover::btn_ghost(&theme, "Quit", "wizard-quit")
                                .id("wizard-quit")
                                .debug_selector(|| "wizard-quit".into())
                                .on_click(cx.listener(|this, _, _, cx| this.wizard_quit(cx))),
                        )
                        .when(page > 0, |el| {
                            el.child(
                                crate::popover::btn_ghost(&theme, "Back", "wizard-back")
                                    .id("wizard-back")
                                    .on_click(cx.listener(|this, _, _, cx| this.wizard_back(cx))),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .when(can_clarify, |el| {
                            el.child(
                                crate::popover::btn_ghost(&theme, "Clarify", "wizard-clarify")
                                    .id("wizard-clarify")
                                    .debug_selector(|| "wizard-clarify".into())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.wizard_clarify(cx)),
                                    ),
                            )
                        })
                        .child(
                            crate::popover::btn_primary(
                                &theme,
                                if last { "Submit" } else { "Next" },
                            )
                            .id("wizard-submit")
                            .role(gpui::Role::Button)
                            .aria_label(if last { "Submit" } else { "Next" })
                            .debug_selector(|| "wizard-submit".into())
                            .px(px(16.0))
                            .when(!can_advance, |el| el.opacity(0.4))
                            .on_click(cx.listener(|this, _, _, cx| this.wizard_advance(cx))),
                        ),
                ),
        )
        .into_any_element()
    }
}
