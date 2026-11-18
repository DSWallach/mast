//! Ownership and event ordering for one ACP session.
use super::*;

const STARVE_GRACE: Duration = Duration::from_secs(2);
const BUSY_RECENT: Duration = Duration::from_secs(3);
const CANCEL_FLUSH: Duration = Duration::from_secs(2);

#[cfg(unix)]
type SignalTarget = i32;
#[cfg(windows)]
type SignalTarget = std::sync::Arc<crate::windows_process::Job>;

#[derive(PartialEq, Eq)]
enum LoopControl {
    Continue,
    Stop,
}

struct AcpSessionState {
    child_exit: Option<Option<std::process::ExitStatus>>,
    exit_drain_deadline: Option<tokio::time::Instant>,
    escalation_deadline: Option<tokio::time::Instant>,
    escalation_target: Option<SignalTarget>,
    interrupted: bool,
    interrupt_sent: bool,
    prompt_seq: u64,
    current_prompt_id: Option<String>,
    completed_prompts: VecDeque<String>,
    prompt_stall_deadline: Option<tokio::time::Instant>,
    turn: Option<BoxFuture<'static, Result<Value, HarnessError>>>,
    queued_steers: VecDeque<String>,
    steering_call: Option<(String, BoxFuture<'static, Result<Value, HarnessError>>)>,
    steer_backlog: VecDeque<String>,
    client: RpcClient,
    session_id: String,
    prompt_complete_extension: bool,
    prompt_stall: Option<Duration>,
    done_current: bool,
    open_tools: std::collections::HashSet<String>,
    last_update_at: tokio::time::Instant,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    subagents: SubagentObserver,
    request_input: std::sync::Arc<RequestInputFn>,
    assistant_message_id: String,
    starve_deadline: Option<tokio::time::Instant>,
    done_after_interrupt: bool,
    steering_open: bool,
    cancel_flush_deadline: Option<tokio::time::Instant>,
}

impl AcpSessionState {
    async fn settle_prompt(
        &mut self,
        res: Result<Value, HarnessError>,
        incoming: &mut mpsc::Receiver<Incoming>,
        auth_method: Option<&str>,
        agent_name: &str,
    ) -> LoopControl {
        self.turn = None;
        if res.is_err() && self.client.is_closed() {
            return LoopControl::Stop;
        }
        self.starve_deadline = None;
        self.prompt_stall_deadline = None;
        if let Some(id) = self.current_prompt_id.take() {
            self.completed_prompts.push_back(id);
            while self.completed_prompts.len() > 32 {
                self.completed_prompts.pop_front();
            }
        }
        // Settle an in-flight `_session/steering` call BEFORE closing
        // the turn: its response rides the same stdout as the prompt
        // response, so by now it is (nearly always) already parsed —
        // the select just hadn't polled it yet. Deciding it here keeps
        // the ordering deterministic: an injection that landed in this
        // turn emits its Steered boundary now, ahead of the drained
        // tail and the Done (a Steered AFTER Done re-armed the
        // consumer with no next turn — the stranded-Working bug); a
        // rejected/unsettled call redelivers as the next turn. The
        // timeout guards the flooded-incoming edge (reader blocked on
        // a full channel never parses the response): past it the call
        // is abandoned and the steer redelivered.
        if let Some((text, mut fut)) = self.steering_call.take() {
            let outcome = match tokio::time::timeout(
                Duration::from_millis(1000),
                &mut fut,
            )
            .await
            {
                Ok(Ok(resp)) => resp
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("injected")
                    .to_owned(),
                Ok(Err(_)) | Err(_) => "promptRequired".to_owned(),
            };
            if self.interrupted {
                // Winding down; abandoned like any queued steer.
            } else if outcome != "promptRequired" {
                let (prev, next) = rotate(&mut self.assistant_message_id);
                if !send(
                    &self.event_tx,
                    AgentEvent::Steered {
                        assistant_message_id: Some(prev),
                        next_assistant_message_id: Some(next),
                    },
                )
                .await
                {
                    return LoopControl::Stop;
                }
            } else {
                self.queued_steers.push_back(text);
            }
            // Followers waiting on the settled call have no live turn
            // to inject into anymore: boundary delivery.
            while let Some(next_text) = self.steer_backlog.pop_front() {
                self.queued_steers.push_back(next_text);
            }
        }
        // Updates streamed before the prompt response are already
        // queued in stdout order — fold them into the turn before
        // closing it (responses bypass the incoming queue).
        let mut consumer_gone = false;
        while let Ok(inc) = incoming.try_recv() {
            match inc {
                Incoming::Notification { method, params } => {
                    let events =
                        session_update_events(&method, &params, &self.session_id, &mut self.subagents);
                    for ev in events {
                        if !send(&self.event_tx, ev).await {
                            consumer_gone = true;
                            break;
                        }
                    }
                }
                Incoming::Request { id, method, params } => {
                    for ev in handle_server_request_live(
                        &self.client,
                        id,
                        &method,
                        &params,
                        &self.request_input,
                        &self.session_id,
                    ) {
                        if !send(&self.event_tx, ev).await {
                            consumer_gone = true;
                            break;
                        }
                    }
                }
                _ => {}
            }
            if consumer_gone {
                break;
            }
        }
        if consumer_gone {
            return LoopControl::Stop;
        }
        let (prev, _next) = rotate(&mut self.assistant_message_id);
        if !send(
            &self.event_tx,
            AgentEvent::AssistantMessageCompleted { assistant_message_id: prev },
        )
        .await
        {
            return LoopControl::Stop;
        }
        // Per-turn token usage, when the adapter settles the prompt
        // with it (claude-agent-acp and codex-acp both do).
        if let Some(usage) = usage_from_response(&res)
            && !send(&self.event_tx, usage).await
        {
            return LoopControl::Stop;
        }
        let (status, mut error) = stop_outcome(&res, self.interrupted);
        if !self.interrupted && auth_method.is_some() && res.as_ref().is_err_and(is_auth_required) {
            error = Some(format!("{agent_name} isn't signed in. Use Settings → Agents → Sign in."));
        }
        self.done_current = true;
        if self.interrupted {
            self.done_after_interrupt = true;
        }
        if !send(
            &self.event_tx,
            AgentEvent::Done {
                status,
                result: None,
                error,
                session_id: Some(self.session_id.clone()),
            },
        )
        .await
        {
            return LoopControl::Stop;
        }
        if self.interrupted || res.is_err() {
            return LoopControl::Stop;
        }
        // Persistent session: a queued steer becomes the next turn;
        // otherwise stay alive for the mailbox — the caller owns
        // teardown (mirrors the codex harness).
        if let Some(text) = self.queued_steers.pop_front() {
            let (prev, next) = rotate(&mut self.assistant_message_id);
            if !send(
                &self.event_tx,
                AgentEvent::Steered {
                    assistant_message_id: Some(prev),
                    next_assistant_message_id: Some(next),
                },
            )
            .await
            {
                return LoopControl::Stop;
            }
            self.begin_prompt(text);
        } else if !self.steering_open {
            return LoopControl::Stop;
        }
        LoopControl::Continue
    }

    async fn accept_incoming(&mut self, inc: Option<Incoming>) -> LoopControl {
        match inc {
            Some(Incoming::Notification { method, params }) => {
                if params.get("sessionId").and_then(Value::as_str).is_some_and(|id| id != self.session_id) {
                    return LoopControl::Continue;
                }
                self.last_update_at = tokio::time::Instant::now();
                // Wire traffic is a sign of life for the prompt-stall
                // watchdog — EXCEPT session boilerplate: opencode emits
                // available_commands_update right after session/new on
                // every session, including ones whose provider is down
                // (where it then retries the provider stream forever
                // with nothing further on the wire — verified live,
                // 1.18.18). One such frame must not disarm the watchdog
                // for the whole turn; only turn progress counts.
                let boilerplate = method == "session/update"
                    && matches!(
                        params
                            .get("update")
                            .and_then(|u| u.get("sessionUpdate"))
                            .and_then(Value::as_str),
                        Some("available_commands_update")
                            | Some("config_option_update")
                            | Some("current_mode_update")
                    );
                if !boilerplate {
                    self.prompt_stall_deadline = None;
                }
                // `_x.ai/session/prompt_complete` — the AUTHORITATIVE
                // turn end for agents advertising it (grok): the
                // `session/prompt` RPC can hang after the turn really
                // finished. Settle through the SAME response arm by
                // swapping the in-flight future for the synthesized
                // response; the abandoned RPC future drains in the
                // background and its late result is discarded. Guards:
                // session match, an outstanding prompt, and an exact
                // prompt-id match (a stale replay of an already-settled
                // prompt must never settle a newer turn; grok echoes the
                // `_meta.promptId` we mint — verified live, 1.0.4).
                if self.prompt_complete_extension
                    && method == "_x.ai/session/prompt_complete"
                    && !self.interrupted
                    && self.turn.is_some()
                    && params.get("sessionId").and_then(Value::as_str)
                        == Some(self.session_id.as_str())
                {
                    let pid = params
                        .get("promptId")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let stale = pid.as_deref().is_some_and(|p| {
                        self.completed_prompts.iter().any(|c| c == p)
                    }) || (pid.is_some() && pid != self.current_prompt_id);
                    if !stale {
                        let stop = params
                            .get("stopReason")
                            .and_then(Value::as_str)
                            .unwrap_or("end_turn")
                            .to_owned();
                        if let Some(old) = self.turn.take() {
                            tokio::spawn(async move {
                                let _ = old.await;
                            });
                        }
                        self.turn = Some(Box::pin(async move {
                            Ok(json!({ "stopReason": stop }))
                        }));
                    }
                }
                // Other notifications (other sessions, agent noise) are
                // tolerated by design.
                let events =
                    session_update_events(&method, &params, &self.session_id, &mut self.subagents);
                for ev in events {
                    track_open_tools(&ev, &mut self.open_tools);
                    if !send(&self.event_tx, ev).await {
                        return LoopControl::Stop;
                    }
                }
            }
            Some(Incoming::Request { id, method, params }) => {
                self.prompt_stall_deadline = None;
                for ev in handle_server_request_live(
                    &self.client,
                    id,
                    &method,
                    &params,
                    &self.request_input,
                    &self.session_id,
                ) {
                    if !send(&self.event_tx, ev).await {
                        return LoopControl::Stop;
                    }
                }
            }
            Some(Incoming::Eof) | None => {
                // The turn ends via a request RESPONSE, which races EOF
                // through a different channel than notifications: an agent
                // exiting right after its final response must read as a
                // clean finish, not a crash. The response (if any) is
                // already resolved by the reader before it sends Eof.
                // Only a RESOLVED response is a clean finish; a request
                // failed by the reader's EOF cleanup falls through to the
                // crash-message bookkeeping below (stderr tail intact).
                if let Some(mut fut) = self.turn.take()
                    && let Ok(res @ Ok(_)) =
                        tokio::time::timeout(Duration::from_millis(50), &mut fut).await
                {
                    let (prev, _next) = rotate(&mut self.assistant_message_id);
                    let _ = send(
                        &self.event_tx,
                        AgentEvent::AssistantMessageCompleted { assistant_message_id: prev },
                    )
                    .await;
                    if let Some(usage) = usage_from_response(&res) {
                        let _ = send(&self.event_tx, usage).await;
                    }
                    let (status, error) = stop_outcome(&res, self.interrupted);
                    self.done_current = true;
                    if self.interrupted {
                        self.done_after_interrupt = true;
                    }
                    let _ = send(
                        &self.event_tx,
                        AgentEvent::Done {
                            status,
                            result: None,
                            error,
                            session_id: Some(self.session_id.clone()),
                        },
                    )
                    .await;
                }
                return LoopControl::Stop;
            }
        }
        LoopControl::Continue
    }

    async fn settle_steering(
        &mut self,
        res: Result<Value, HarnessError>,
        incoming: &mut mpsc::Receiver<Incoming>,
    ) -> LoopControl {
        let (text, _) = self.steering_call.take().expect("guarded by if");
        let outcome = match &res {
            Ok(resp) => resp
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or("injected")
                .to_owned(),
            Err(e) => {
                tracing::debug!(
                    target: "mast_harness::acp",
                    "_session/steering failed (redelivering): {e}"
                );
                // Failed calls redeliver like a lost turn-end race.
                "promptRequired".to_owned()
            }
        };
        if self.interrupted {
            // The run is winding down; the steer is abandoned like
            // any queued steer at interrupt.
        } else if outcome != "promptRequired" {
            // Injected into a live turn → a Steered boundary. But if
            // the turn ended while the call was in flight, the
            // injection was consumed by THAT turn — its output
            // already streamed and the turn's Done already closed the
            // segment. Emitting Steered after that Done re-armed the
            // consumer (parked session → Working) with no next turn
            // and no Done ever coming — the stranded-Working /
            // eternal-timer bug. Post-turn: nothing left to do.
            if self.turn.is_some() {
                // The injection proves the turn is LIVE: any settle
                // deadline armed by an earlier noRunningTurn reply
                // is no longer valid.
                self.starve_deadline = None;
                // Pre-injection updates can still sit in `incoming`
                // (responses bypass that queue): drain them into the
                // CURRENT segment first, or text the agent streamed
                // before the injection landed folds after the split —
                // the transcript attributes it to the reply-to-steer.
                let mut consumer_gone = false;
                while let Ok(inc) = incoming.try_recv() {
                    match inc {
                        Incoming::Notification { method, params } => {
                            let events =
                                session_update_events(&method, &params, &self.session_id, &mut self.subagents);
                            for ev in events {
                                if !send(&self.event_tx, ev).await {
                                    consumer_gone = true;
                                    break;
                                }
                            }
                        }
                        Incoming::Request { id, method, params } => {
                            for ev in handle_server_request_live(
                                &self.client,
                                id,
                                &method,
                                &params,
                                &self.request_input,
                        &self.session_id,
                            ) {
                                if !send(&self.event_tx, ev).await {
                                    consumer_gone = true;
                                    break;
                                }
                            }
                        }
                        _ => {}
                    }
                    if consumer_gone {
                        break;
                    }
                }
                if consumer_gone {
                    return LoopControl::Stop;
                }
                let (prev, next) = rotate(&mut self.assistant_message_id);
                if !send(
                    &self.event_tx,
                    AgentEvent::Steered {
                        assistant_message_id: Some(prev),
                        next_assistant_message_id: Some(next),
                    },
                )
                .await
                {
                    return LoopControl::Stop;
                }
            }
        } else if self.turn.is_some() {
            // Raced the turn end: redeliver at the boundary the
            // loop is about to hit. `noRunningTurn` is stronger —
            // the adapter says nothing is running while our prompt
            // is still outstanding: the starved-turn signature. Arm
            // the grace deadline; if the prompt's response does not
            // land first, the recovery arm below settles the dead
            // turn and promotes this steer.
            if res
                .as_ref()
                .ok()
                .and_then(|r| r.get("reason"))
                .and_then(Value::as_str)
                == Some("noRunningTurn")
            {
                tracing::warn!(
                    target: "mast_harness::acp",
                    "steering answered noRunningTurn with a prompt \
                     outstanding; arming starved-turn recovery"
                );
                self.starve_deadline =
                    Some(tokio::time::Instant::now() + STARVE_GRACE);
            }
            self.queued_steers.push_back(text);
        } else {
            // The turn ended while the call was in flight and its
            // boundary already passed — the steer becomes the next
            // turn directly.
            let (prev, next) = rotate(&mut self.assistant_message_id);
            if !send(
                &self.event_tx,
                AgentEvent::Steered {
                    assistant_message_id: Some(prev),
                    next_assistant_message_id: Some(next),
                },
            )
            .await
            {
                return LoopControl::Stop;
            }
            self.begin_prompt(text);
        }
        while let Some(next_text) = self.steer_backlog.pop_front() {
            if self.turn.is_some() && !self.interrupted {
                let fut = steering_call_future(&self.client, &self.session_id, &next_text);
                self.steering_call = Some((next_text, fut));
                break;
            }
            // No live turn to inject into: boundary delivery.
            self.queued_steers.push_back(next_text);
        }
        LoopControl::Continue
    }

    async fn flush_cancel(&mut self) -> LoopControl {
        self.cancel_flush_deadline = None;
        if self.turn.is_none()
            && let Some(text) = self.queued_steers.pop_front()
        {
            let (prev, next) = rotate(&mut self.assistant_message_id);
            if !send(
                &self.event_tx,
                AgentEvent::Steered {
                    assistant_message_id: Some(prev),
                    next_assistant_message_id: Some(next),
                },
            )
            .await
            {
                return LoopControl::Stop;
            }
            self.begin_prompt(text);
        } else if self.turn.is_none() && !self.steering_open {
            // Mailbox closed while the flush waited: nothing left.
            return LoopControl::Stop;
        }
        LoopControl::Continue
    }

    async fn recover_starved_prompt(&mut self) -> LoopControl {
        self.starve_deadline = None;
        tracing::warn!(
            target: "mast_harness::acp",
            "prompt response missing past turn-end evidence; settling \
             the dead turn (and promoting any queued steer)"
        );
        // Drop the dead future: a response that somehow arrives later
        // resolves a closed channel harmlessly.
        self.turn = None;
        let (prev, _next) = rotate(&mut self.assistant_message_id);
        if !send(
            &self.event_tx,
            AgentEvent::AssistantMessageCompleted { assistant_message_id: prev },
        )
        .await
        {
            return LoopControl::Stop;
        }
        self.done_current = true;
        if !send(
            &self.event_tx,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(self.session_id.clone()),
            },
        )
        .await
        {
            return LoopControl::Stop;
        }
        if let Some(text) = self.queued_steers.pop_front() {
            let (prev, next) = rotate(&mut self.assistant_message_id);
            if !send(
                &self.event_tx,
                AgentEvent::Steered {
                    assistant_message_id: Some(prev),
                    next_assistant_message_id: Some(next),
                },
            )
            .await
            {
                return LoopControl::Stop;
            }
            self.begin_prompt(text);
        } else if !self.steering_open {
            // Mirror the normal turn-settled exit: mailbox closed
            // and nothing left to run — the session is over.
            return LoopControl::Stop;
        }
        LoopControl::Continue
    }

    async fn accept_steer(
        &mut self,
        steer: Option<crate::SteerMessage>,
        prompt_transform: fn(Option<ReasoningLevel>, &str) -> String,
        reasoning: Option<ReasoningLevel>,
        steer_ext: bool,
    ) -> LoopControl {
        match steer {
            Some(msg) => {
                // Same transform as the initial prompt: Claude's
                // Ultrathink prefix rides every steer too.
                let text = prompt_transform(reasoning, &msg.prompt);
                if self.turn.is_none() && self.cancel_flush_deadline.is_some() {
                    // A busy-session cancel is already in flight: this
                    // steer lines up behind it and dispatches at flush.
                    self.queued_steers.push_back(text);
                } else if self.turn.is_none()
                    && (!self.open_tools.is_empty()
                        || self.last_update_at.elapsed() < BUSY_RECENT)
                {
                    // Mid self-continued turn (see BUSY_RECENT above):
                    // cancel it rather than prompt into the starve.
                    //
                    tracing::info!(
                        target: "mast_harness::acp",
                        "steer into a self-continuing session; cancelling \
                         the unowned turn before prompting"
                    );
                    self.client.notify(
                        "session/cancel",
                        Some(json!({ "sessionId": self.session_id })),
                    );
                    self.queued_steers.push_back(text);
                    self.cancel_flush_deadline =
                        Some(tokio::time::Instant::now() + CANCEL_FLUSH);
                } else if self.turn.is_none() {
                    // Idle between turns: a steer is simply the next turn.
                    let (prev, next) = rotate(&mut self.assistant_message_id);
                    if !send(
                        &self.event_tx,
                        AgentEvent::Steered {
                            assistant_message_id: Some(prev),
                            next_assistant_message_id: Some(next),
                        },
                    )
                    .await
                    {
                        return LoopControl::Stop;
                    }
                    self.begin_prompt(text);
                } else if steer_ext {
                    // Mid-turn injection via the `_session/steering`
                    // extension: start the call, resolved by its own
                    // select branch. One call in flight at a time;
                    // followers wait in the backlog.
                    if self.steering_call.is_some() {
                        self.steer_backlog.push_back(text);
                    } else {
                        let fut = steering_call_future(&self.client, &self.session_id, &text);
                        self.steering_call = Some((text, fut));
                    }
                } else {
                    // No extension (Grok today): turn-boundary delivery.
                    self.queued_steers.push_back(text);
                }
            }
            None => {
                self.steering_open = false;
                if self.turn.is_none() && self.queued_steers.is_empty() {
                    return LoopControl::Stop;
                }
            }
        }
        LoopControl::Continue
    }

    fn observe_child_exit(
        &mut self,
        status: std::io::Result<std::process::ExitStatus>,
        child: &Child,
    ) {
        self.escalation_deadline = None;
        self.child_exit = Some(status.ok());
        child.request_group_shutdown();
        // Descendants may hold stdout open after the adapter exits.
        self.exit_drain_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(200));
    }

    fn interrupt(
        &mut self,
        has_turn: bool,
        child: &Child,
        interrupt_grace: Duration,
    ) -> LoopControl {
        self.interrupt_sent = true;
        self.interrupted = true;
        if has_turn {
            self.client.notify("session/cancel", Some(json!({ "sessionId": self.session_id })));
            if let Some(pid) = crate::process::signal_target(child) {
                self.escalation_target = Some(pid);
                self.escalation_deadline = Some(tokio::time::Instant::now() + interrupt_grace);
            }
            LoopControl::Continue
        } else {
            LoopControl::Stop
        }
    }

    fn begin_prompt(&mut self, text: String) {
        self.done_current = false;
        self.open_tools.clear();
        self.last_update_at = tokio::time::Instant::now();
        self.prompt_seq += 1;
        self.current_prompt_id = self
            .prompt_complete_extension
            .then(|| format!("zeron-p{}", self.prompt_seq));
        self.prompt_stall_deadline = self.prompt_stall.map(|d| tokio::time::Instant::now() + d);
        self.turn = Some(prompt_turn(
            self.client.clone(),
            self.session_id.clone(),
            text,
            self.current_prompt_id.clone(),
        ));
    }

    fn escalate_interrupt(
        &mut self,
        escalation_signal: &mut Signal,
        kill_grace: Duration,
    ) {
        // Once escalation starts, a late response no longer owns this turn.
        self.turn = None;
        if let Some(target) = &self.escalation_target {
            send_signal(target, *escalation_signal);
        }
        self.escalation_deadline = match *escalation_signal {
            Signal::Term => {
                *escalation_signal = Signal::Kill;
                Some(tokio::time::Instant::now() + kill_grace)
            }
            Signal::Kill => None,
        };
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_session(
        &mut self,
        child: &mut Child,
        agent_name: &str,
        stderr_tail: &crate::StderrTail,
        kill_grace: Duration,
    ) {
        // A vanished Devin process cannot send subagent_completed. Settle every
        // open nested transcript before the parent's terminal Done.
        let subagent_status = if self.interrupted {
            DoneStatus::Interrupted
        } else {
            DoneStatus::Errored
        };
        for event in self.subagents.finish_open(subagent_status) {
            if !send(&self.event_tx, event).await {
                break;
            }
        }

        // Terminal bookkeeping: never end the stream without a Done unless the
        // consumer already hung up.
        if !self.event_tx.is_closed() {
            if self.interrupted && !self.done_after_interrupt {
                let _ = self.event_tx
                    .send(Ok(AgentEvent::Done {
                        status: DoneStatus::Interrupted,
                        result: None,
                        error: None,
                        session_id: Some(self.session_id.clone()),
                    }))
                    .await;
            } else if !self.interrupted && !self.done_current {
                // A child killed mid-turn must not read as a silent success.
                let status = match self.child_exit {
                    Some(status) => status,
                    None => tokio::time::timeout(Duration::from_millis(200), child.wait())
                        .await
                        .ok()
                        .and_then(Result::ok),
                };
                child.request_group_shutdown();
                stderr_tail.wait_closed().await;
                let _ = self.event_tx
                    .send(Ok(AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(crate::crash_message(agent_name, status, stderr_tail)),
                        session_id: Some(self.session_id.clone()),
                    }))
                    .await;
            }
        }

        child.shutdown(kill_grace).await;
    }

    async fn expire_prompt_stall(
        &mut self,
        agent_name: &str,
        stall_hint: &str,
    ) {
        let _ = send(
            &self.event_tx,
            AgentEvent::Error {
                code: None, resets_at: None, message: format!(
                    "{agent_name} did not respond to the prompt at all \
                     (no wire activity for {}s). {}",
                    self.prompt_stall.map(|d| d.as_secs()).unwrap_or(0),
                    stall_hint,
                ),
            },
        )
        .await;
        self.done_current = true;
        let _ = send(
            &self.event_tx,
            AgentEvent::Done {
                status: DoneStatus::Errored,
                result: None,
                error: Some(format!(
                    "{agent_name} is unresponsive — the run was closed."
                )),
                session_id: Some(self.session_id.clone()),
            },
        )
        .await;
    }
}

struct EstablishedSession {
    session_id: String,
    steer_ext: bool,
    init_commands: Vec<SlashCommand>,
}

#[allow(clippy::too_many_arguments)]
async fn establish_session(
    client: &RpcClient,
    incoming: &mut mpsc::Receiver<Incoming>,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    request: &RunRequest,
    harness: HarnessId,
    agent_name: &'static str,
    auth_method: Option<&'static str>,
    effort_in_model_id: bool,
    effort_values: fn(Option<ReasoningLevel>, Option<&str>) -> Vec<&'static str>,
) -> Result<EstablishedSession, HarnessError> {
    let init = client
        .request("initialize", initialize_params(harness))
        .await?;
    let steer_ext = steering_supported(&init);
    let init_commands = scan_available_commands(&init);

    let session_params = json!({ "cwd": request.cwd, "mcpServers": [] });
    let (session_id, mut session_response) = if let Some(resume) = &request.resume {
        let mut load = session_params.clone();
        load["sessionId"] = Value::String(resume.clone());
        match request_draining(client, incoming, "session/load", load).await {
            Ok(resp) => (resume.clone(), resp),
            Err(e) if auth_method.is_some() && is_auth_required(&e) => {
                return Err(HarnessError::Protocol(format!(
                    "{agent_name} isn't signed in. Use Settings → Agents → Sign in."
                )));
            }
            // A missing/foreign session falls back to a fresh one.
            Err(e) => {
                tracing::debug!(
                    target: "mast_harness::acp",
                    "session/load failed (starting fresh): {e}"
                );
                let _ = send(event_tx, AgentEvent::Error {
                    code: None, resets_at: None, message: format!("{agent_name} could not restore session {resume}; starting a new session without the previous context: {e}"),
                }).await;
                let new = new_session(
                    client,
                    incoming,
                    session_params.clone(),
                    agent_name,
                    auth_method.is_some(),
                )
                .await?;
                (
                    new.get("sessionId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    new,
                )
            }
        }
    } else {
        let new = new_session(
            client,
            incoming,
            session_params,
            agent_name,
            auth_method.is_some(),
        )
        .await?;
        (
            new.get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            new,
        )
    };
    if session_id.is_empty() {
        return Err(HarnessError::Protocol(
            "session/new returned no sessionId".into(),
        ));
    }
    if harness == HarnessId::Devin
        && let Some(model) = request.model.as_deref()
    {
        devin_models::wait_for_model(
            client,
            incoming,
            &session_id,
            &mut session_response,
            model,
        )
        .await?;
    }
    // ACP has had two model-selection surfaces. Newer config-option agents
    // use category=model below; Grok Build currently advertises only the
    // first-class `models` state and requires `session/set_model`. Other
    // ACP clients follow the same split. Unlike the best-effort auxiliary options,
    // an explicit model switch is strict: prompting with a different
    // model than the picker shows is worse than surfacing the RPC error.
    let requested_model: Option<String> = match request.model.as_deref() {
        Some(model) if effort_in_model_id => Some(effort_variant_id(
            &session_response,
            model,
            request.reasoning,
        )),
        model => model.map(str::to_owned),
    };
    if harness == HarnessId::Antigravity {
        validate_config_model_selection(
            &session_response,
            requested_model.as_deref(),
            &request.model_options,
        )?;
    }
    if let Some(model) =
        first_class_model_change(&session_response, requested_model.as_deref())?
    {
        request_draining(
            client,
            incoming,
            "session/set_model",
            json!({
                "sessionId": session_id,
                "modelId": model,
            }),
        )
        .await
        .map_err(|error| {
            HarnessError::Protocol(format!("agent rejected model switch to {model}: {error}"))
        })?;
    }
    // Apply the run's model + effort + model options through the
    // session's advertised config options. Best-effort for effort and
    // traits: a rejected auxiliary set is logged and the agent default
    // runs.
    let efforts = effort_values(request.reasoning, request.model.as_deref());
    let session_commands = scan_available_commands(&session_response);
    let init_commands = if session_commands.is_empty() {
        init_commands
    } else {
        session_commands
    };
    let options_snapshot = session_response;
    for (config_id, payload) in config_option_sets(
        &options_snapshot,
        requested_model.as_deref(),
        &efforts,
        &request.model_options,
    ) {
        let mut params = serde_json::Map::new();
        params.insert("sessionId".into(), session_id.clone().into());
        params.insert("configId".into(), config_id.clone().into());
        if let Some(payload) = payload.as_object() {
            for (k, v) in payload {
                params.insert(k.clone(), v.clone());
            }
        }
        if let Err(e) = request_draining(
            client,
            incoming,
            "session/set_config_option",
            Value::Object(params),
        )
        .await
        {
            if matches!(harness, HarnessId::Antigravity | HarnessId::Devin)
                && requested_model.is_some()
                && is_model_config_option(&options_snapshot, &config_id)
            {
                return Err(HarnessError::Protocol(format!(
                    "agent rejected requested model {}: {e}",
                    requested_model.as_deref().unwrap_or_default()
                )));
            }
            tracing::debug!(
                target: "mast_harness::acp",
                "session/set_config_option {config_id}={payload} rejected (agent default runs): {e}"
            );
        }
    }
    Ok(EstablishedSession {
        session_id,
        steer_ext,
        init_commands,
    })
}

/// The per-run event loop: one task multiplexing agent messages, the pending
/// turn, the steering mailbox, the interrupt token, and consumer liveness.
pub(super) async fn run_session(session: Session) {
    let Session {
        // Locals drop in reverse binding order: reap the child before cleanup.
        scratch: _scratch,
        mut child,
        client,
        mut incoming,
        event_tx,
        controls,
        request,
        harness,
        agent_name,
        prompt_complete_extension,
        prompt_stall,
        stall_hint,
        effort_in_model_id,
        auth_method,
        sessions_root,
        prompt_transform,
        effort_values,
        interrupt_grace,
        kill_grace,
        handshake_timeout,
        stderr_tail,
    } = session;
    let RunControls {
        request_input,
        mut steering,
        interrupt,
    } = controls;
    let request_input = std::sync::Arc::new(request_input);

    // ---- handshake + session (interruptible) ------------------------------
    let setup = establish_session(
        &client,
        &mut incoming,
        &event_tx,
        &request,
        harness,
        agent_name,
        auth_method,
        effort_in_model_id,
        effort_values,
    );
    let EstablishedSession { session_id, steer_ext, init_commands } = tokio::select! {
        res = tokio::time::timeout(handshake_timeout, setup) => {
            let res = res.unwrap_or_else(|_| {
                // A hung handshake (agent waiting on a login it can never
                // get, a wedged adapter) used to spin "Working" forever —
                // the false "thinking for 2+ minutes then nothing" class of
                // report. Bound it and say what was reached.
                Err(HarnessError::Protocol(format!(
                    "{agent_name} did not complete the ACP handshake within {}s \
                     (the agent may be waiting for a login — try running it once \
                     in a terminal)",
                    handshake_timeout.as_secs()
                )))
            });
            match res {
                Ok(v) => v,
                Err(e) => {
                    // A child that dies before the handshake used to surface only
                    // the RPC-side symptom ("transport closed") — its exit status
                    // and stderr, both already in hand, were dropped, leaving
                    // startup crashes undiagnosable (user report). When the child
                    // is already gone, give the reader task a beat to drain the
                    // pipe, then append the crash text; the Done carrying it is
                    // journaled, so the cause survives for later inspection. A
                    // still-live child (the timeout) contributes its stderr tail.
                    let error = match child.try_wait() {
                        Ok(Some(status)) => {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            format!(
                                "{e}; {}",
                                crate::crash_message(agent_name, Some(status), &stderr_tail)
                            )
                        }
                        _ => match stderr_tail.snapshot() {
                            Some(tail) => format!("{e}; stderr: {tail}"),
                            None => e.to_string(),
                        },
                    };
                    tracing::warn!(target: "mast_harness::acp", %error, "agent setup failed");
                    let _ = event_tx
                        .send(Ok(AgentEvent::Done {
                            status: DoneStatus::Errored,
                            result: None,
                            error: Some(error),
                            session_id: None,
                        }))
                        .await;
                    child.shutdown(kill_grace).await;
                    return;
                }
            }
        },
        _ = interrupt.cancelled() => {
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: None,
                }))
                .await;
            child.shutdown(kill_grace).await;
            return;
        }
    };

    let assistant_message_id = new_message_id();
    if !send(
        &event_tx,
        AgentEvent::SessionStarted {
            harness,
            model: request.model.clone().unwrap_or_default(),
            tools: Vec::new(),
            cwd: request.cwd.clone(),
            session_id: session_id.clone(),
            assistant_message_id: assistant_message_id.clone(),
            chat_id: None,
            space_id: None,
        },
    )
    .await
    {
        child.shutdown(kill_grace).await;
        return;
    }
    if !init_commands.is_empty()
        && !send(
            &event_tx,
            AgentEvent::AvailableCommands {
                commands: init_commands,
            },
        )
        .await
    {
        child.shutdown(kill_grace).await;
        return;
    }

    // Subagent correlation + transcript tails: Devin carries nested updates
    // on ACP itself; everything else gets the Grok tracker (inert without
    // Grok's subagent lifecycle extension).
    let subagents = if harness == HarnessId::Devin {
        SubagentObserver::Devin(DevinTracker::default())
    } else {
        SubagentObserver::Grok(SubagentTracker::new(
            session_id.clone(),
            event_tx.clone(),
            sessions_root,
        ))
    };

    // ---- main loop --------------------------------------------------------
    // Prompt-completion settlement state (the prompt-complete extension):
    // one prompt is outstanding at a time, identified by `current_prompt_id`;
    // settled ids are remembered so a STALE `prompt_complete` (a late replay
    // of an already-settled prompt) can never settle a newer turn.
    let prompt_seq: u64 = 1;
    let current_prompt_id = prompt_complete_extension.then(|| format!("zeron-p{prompt_seq}"));
    let completed_prompts: VecDeque<String> = VecDeque::new();
    // `MAST_ACP_PROMPT_STALL_MS` overrides the spec's bound; 0 disables.
    let prompt_stall: Option<Duration> = match mast_proto::env::var("MAST_ACP_PROMPT_STALL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        Some(0) => None,
        Some(ms) => Some(Duration::from_millis(ms)),
        None => prompt_stall,
    };
    let prompt_stall_deadline: Option<tokio::time::Instant> =
        prompt_stall.map(|d| tokio::time::Instant::now() + d);
    let turn: Option<BoxFuture<'static, Result<Value, HarnessError>>> = Some({
        prompt_turn(
            client.clone(),
            session_id.clone(),
            prompt_transform(request.reasoning, &request.prompt),
            current_prompt_id.clone(),
        )
    });
    // Steers waiting for the turn boundary (agents without the extension, or
    // extension steers that lost the turn-end race).
    let queued_steers: VecDeque<String> = VecDeque::new();
    // The in-flight `_session/steering` call (text + response future), plus
    // followers awaiting their turn. Polled from the main select so the loop
    // keeps draining `incoming` while the agent responds — awaiting inline
    // deadlocks against a full incoming channel when the agent floods
    // updates (the reader blocks on the channel and never parses the
    // steering response).
    let steering_call: Option<(String, BoxFuture<'static, Result<Value, HarnessError>>)> = None;
    let steer_backlog: VecDeque<String> = VecDeque::new();
    let steering_open = true;
    let done_current = false;
    let done_after_interrupt = false;
    let mut escalation_signal = Signal::Term;
    // Starved-turn recovery (2026-08-12 stuck-Working incident): a
    // `session/prompt` sent while the agent runs a SELF-CONTINUED turn (a
    // background-task re-invocation no prompt started) starves —
    // claude-agent-acp does not track turns it did not start, so the merged
    // turn's result is never attributed to the pending prompt (reproduced
    // against 0.66.0; the prompt's TEXT still reaches the model, queued by
    // the CLI). The tell is protocol evidence, not timing: a steering call
    // answered `promptRequired`/`noRunningTurn` while OUR prompt is
    // outstanding means the adapter has no turn that could ever settle it.
    // A short grace covers the true turn-end race (its response lands within
    // milliseconds); past it, the dead prompt is closed out with a Done and
    // the queued steer is promoted to a fresh turn.
    let starve_deadline: Option<tokio::time::Instant> = None;
    // Silence is not a turn boundary: completed tools, text, and usage may
    // all precede a slow model request. Keep the prompt future alive until
    // its response (or an authoritative completion extension) arrives.
    // MAST_ACP_QUIET_SETTLE_MS is intentionally no longer honored (#296).
    let last_update_at = tokio::time::Instant::now();
    let open_tools: std::collections::HashSet<String> = std::collections::HashSet::new();
    // PREVENTION, ahead of all the recovery above: never send a
    // `session/prompt` into a session that is visibly mid SELF-CONTINUED
    // turn — that prompt's reply is what the adapter drops (the verified
    // starve). Visibly busy = an open tool call, or stream traffic within
    // BUSY_RECENT, with no prompt of ours outstanding. The discipline is
    // Zed's, verified against the real adapter: `session/cancel` the
    // unowned turn, give it CANCEL_FLUSH to die and drain, then prompt.
    // This makes the interactive path starve-free; the settle layers below
    // remain for the notification race a client cannot see coming.
    let cancel_flush_deadline: Option<tokio::time::Instant> = None;

    let mut state = AcpSessionState {
        prompt_seq,
        current_prompt_id,
        completed_prompts,
        prompt_stall_deadline,
        turn,
        queued_steers,
        steering_call,
        steer_backlog,
        client,
        session_id,
        prompt_complete_extension,
        prompt_stall,
        done_current,
        open_tools,
        last_update_at,
        event_tx,
        subagents,
        request_input,
        assistant_message_id,
        starve_deadline,
        done_after_interrupt,
        steering_open,
        cancel_flush_deadline,
        child_exit: None,
        exit_drain_deadline: None,
        escalation_deadline: None,
        escalation_target: None,
        interrupted: false,
        interrupt_sent: false,
    };
    'main: loop {
        tokio::select! {
            status = child.wait(), if state.child_exit.is_none() => {
                state.observe_child_exit(status, &child);
            },
            _ = tokio::time::sleep_until(state.exit_drain_deadline.unwrap_or_else(tokio::time::Instant::now)),
                if state.exit_drain_deadline.is_some() => break 'main,

            res = async { state.turn.as_mut().expect("guarded by if").await }, if state.turn.is_some() => {
                if state.settle_prompt(res, &mut incoming, auth_method, agent_name).await == LoopControl::Stop {
                    break 'main;
                }
            },

            inc = incoming.recv() => {
                if state.accept_incoming(inc).await == LoopControl::Stop {
                    break 'main;
                }
            },

            res = async { state.steering_call.as_mut().expect("guarded by if").1.as_mut().await },
                if state.steering_call.is_some() =>
            {
                if state.settle_steering(res, &mut incoming).await == LoopControl::Stop {
                    break 'main;
                }
            },

            // Busy-session cancel flushed (see BUSY_RECENT/CANCEL_FLUSH
            // above): the unowned self-continued turn had its cancel and a
            // drain window; the queued steer becomes a fresh prompt on a
            // now-idle agent.
            _ = tokio::time::sleep_until(
                state.cancel_flush_deadline.unwrap_or_else(tokio::time::Instant::now)
            ), if state.cancel_flush_deadline.is_some() && !state.interrupted => {
                if state.flush_cancel().await == LoopControl::Stop {
                    break 'main;
                }
            },

            // Explicit noRunningTurn from the steering extension proves
            // the adapter has no running turn. After a grace for its racing
            // response, recover the stranded prompt. Silence, tool results,
            // and usage updates must never arm this recovery.
            _ = tokio::time::sleep_until(
                state.starve_deadline.unwrap_or_else(tokio::time::Instant::now)
            ), if state.starve_deadline.is_some() && state.turn.is_some() && !state.interrupted => {
                if state.recover_starved_prompt().await == LoopControl::Stop {
                    break 'main;
                }
            },

            steer = steering.recv(), if state.steering_open && !state.interrupted => {
                if state.accept_steer(steer, prompt_transform, request.reasoning, steer_ext).await == LoopControl::Stop {
                    break 'main;
                }
            },

            _ = interrupt.cancelled(), if !state.interrupt_sent => {
                let has_turn = state.turn.is_some();
                if state.interrupt(has_turn, &child, interrupt_grace)
                    == LoopControl::Stop
                {
                    break 'main;
                }
            },

            // Prompt-stall watchdog: a prompt was sent and NOTHING has come
            // back on the wire at all — no queue bookkeeping, no updates, no
            // requests. Healthy grok acknowledges within milliseconds; total
            // silence past the bound is a wedged agent (stale shared leader,
            // launch-time update check). Surface a visible error instead of
            // indefinite Working.
            _ = tokio::time::sleep_until(
                state.prompt_stall_deadline.unwrap_or_else(tokio::time::Instant::now),
            ), if state.prompt_stall_deadline.is_some() && state.turn.is_some() && !state.interrupted => {
                state.expire_prompt_stall(
                    agent_name, stall_hint,
                ).await;
                break 'main;
            },

            // Keep escalation in the owner task: it cannot outlive child.wait()
            // or signal a pid after the child has been reaped.
            _ = tokio::time::sleep_until(state.escalation_deadline.unwrap_or_else(tokio::time::Instant::now)),
                if state.escalation_deadline.is_some() => {
                state.escalate_interrupt(&mut escalation_signal, kill_grace);
            },

            _ = state.event_tx.closed() => break 'main,
        }
    }

    state.finish_session(
        &mut child, agent_name, &stderr_tail, kill_grace,
    ).await;
}
