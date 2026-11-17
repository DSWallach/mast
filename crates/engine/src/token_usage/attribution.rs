use super::*;

pub(crate) fn launch_agent_type(
    activity: &str,
    worker: bool,
    orchestrator: bool,
    review: bool,
) -> &'static str {
    match activity {
        "Title generation" => "title-generator",
        _ if review || activity == "PR review" => "pr-review",
        _ if worker => "worker",
        _ if orchestrator => "orchestrator",
        _ => "main",
    }
}

pub(super) fn spawn_agent_type(call: &ToolCall) -> Option<&str> {
    if !call.is_subagent_spawn() {
        return None;
    }
    let input = match call {
        ToolCall::Unknown { input, .. } | ToolCall::Mcp { input, .. } => input.as_ref()?,
        _ => return None,
    };
    ["subagent_type", "agent_type", "agentType"]
        .into_iter()
        .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|label| {
            !label.is_empty() && label.len() <= 80 && !label.chars().any(char::is_control)
        })
}

pub(super) fn tag(
    db: &Connection,
    turn: &str,
    source: &str,
    kind: &str,
    agent: &str,
    role: &str,
) -> Result<(), EngineError> {
    db.execute(
        "INSERT INTO usage_agents SELECT ?1,?2,?3,?4,?5 WHERE ?3!='measurement' OR EXISTS(SELECT 1 FROM measurements WHERE turn_id=?1 AND source=?2)
         ON CONFLICT(turn_id,source,kind) DO UPDATE SET agent_type=CASE WHEN excluded.agent_type='Unknown agent type' THEN usage_agents.agent_type ELSE excluded.agent_type END",
        params![turn, source, kind, agent, role],
    ).map_err(fail)?;
    Ok(())
}

impl TokenUsage {
    pub fn set_attribution(&self, chat: &str, agent_type: String, parent_chat_id: Option<String>) {
        if self.2.is_some() {
            let chat = chat.to_owned();
            self.dispatch(move |usage| usage.set_attribution(&chat, agent_type, parent_chat_id));
            return;
        }
        let mut inner = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let Some(active) = inner.active.get_mut(chat) else {
            return;
        };
        active.context.agent_type = Some(agent_type.clone());
        active.context.parent_chat_id = parent_chat_id;
        let turn = format!("{}:{}", active.run, active.turn);
        let context = serde_json::to_string(&active.context).unwrap_or_default();
        let result = (|| -> rusqlite::Result<()> {
            let tx = inner.db.transaction()?;
            tx.execute(
                "UPDATE turns SET context=?2 WHERE id=?1",
                params![turn, context],
            )?;
            tx.execute(
                "UPDATE usage_agents SET agent_type=?2 WHERE turn_id=?1 AND role!='subagent'",
                params![turn, agent_type],
            )?;
            tx.commit()
        })();
        if let Err(error) = result {
            inner.capture_failures += 1;
            tracing::warn!(%error, "usage attribution write failed");
        }
    }

    pub fn promote_orchestrator(&self, chat: &str) {
        if self.2.is_some() {
            let chat = chat.to_owned();
            self.dispatch(move |usage| usage.promote_orchestrator(&chat));
            return;
        }
        let mut inner = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let Some(active) = inner.active.get_mut(chat) else {
            return;
        };
        if active.context.agent_type.as_deref() != Some("main") {
            return;
        }
        active.context.agent_type = Some("orchestrator".into());
        let turn = format!(
            "{}:{}",
            active.run,
            if active.has_turn {
                active.turn
            } else {
                active.turn.saturating_sub(1)
            }
        );
        let context = serde_json::to_string(&active.context).unwrap_or_default();
        let result = (|| -> rusqlite::Result<()> {
            let tx = inner.db.transaction()?;
            tx.execute(
                "UPDATE turns SET context=?2 WHERE id=?1",
                params![turn, context],
            )?;
            tx.execute("UPDATE usage_agents SET agent_type='orchestrator' WHERE turn_id=?1 AND agent_type='main' AND role!='subagent'", [&turn])?;
            tx.commit()
        })();
        if let Err(error) = result {
            inner.capture_failures += 1;
            tracing::warn!(%error, "orchestrator attribution write failed");
        }
    }
}
