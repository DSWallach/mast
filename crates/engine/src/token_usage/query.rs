use super::*;

// Aggregates replace request observations, including child-role observations.
// A `turn` aggregate covers every request of its thread (role), whatever model
// label the request was written with. Legacy counts are only used when a turn
// has no richer measurements.
pub(super) const FILTER:&str="FROM measurements m JOIN turns t ON t.id=m.turn_id LEFT JOIN usage_agents a ON a.turn_id=m.turn_id AND a.source=m.source AND a.kind='measurement'
    WHERE m.scope!='resumeBaseline' AND (?1 IS NULL OR m.model=?1) AND (?2 IS NULL OR t.at>=?2)
    AND (?3 IS NULL OR t.at<?3) AND (?4 IS NULL OR t.activity=?4)
    AND (?5 IS NULL OR COALESCE(a.agent_type,CASE WHEN m.role='subagent' THEN NULL ELSE json_extract(t.context,'$.agentType') END,'Unknown agent type')=?5)
    AND (m.scope!='legacy' OR NOT EXISTS(SELECT 1 FROM measurements x WHERE x.turn_id=m.turn_id AND x.scope NOT IN ('legacy','resumeBaseline')))
    AND (m.scope IN ('turn','turnIncludingChildren','legacy') OR NOT EXISTS(SELECT 1 FROM measurements x WHERE x.turn_id=m.turn_id AND (x.scope='turnIncludingChildren' OR x.model=m.model OR (x.scope='turn' AND x.role=m.role)) AND x.scope IN ('turn','turnIncludingChildren')))";

pub(super) fn selection(kind: TokenKind) -> (&'static str, &'static str) {
    match kind {
        TokenKind::All => ("m.input+m.output", "1"),
        TokenKind::Input => ("m.input", "1"),
        TokenKind::Output => ("m.output", "1"),
        TokenKind::UncachedInput => (
            "CASE WHEN m.cache_read IS NOT NULL AND m.cache_write IS NOT NULL THEN MAX(0,m.input-m.cache_read-m.cache_write) ELSE 0 END",
            "m.cache_read IS NOT NULL AND m.cache_write IS NOT NULL",
        ),
        TokenKind::CacheRead => ("COALESCE(m.cache_read,0)", "m.cache_read IS NOT NULL"),
        TokenKind::CacheWrite => ("COALESCE(m.cache_write,0)", "m.cache_write IS NOT NULL"),
        TokenKind::Reasoning => ("COALESCE(m.reasoning,0)", "m.reasoning IS NOT NULL"),
        TokenKind::NonReasoningOutput => (
            "CASE WHEN m.reasoning IS NOT NULL THEN MAX(0,m.output-m.reasoning) ELSE 0 END",
            "m.reasoning IS NOT NULL",
        ),
    }
}

/// Token totals stamped onto a completion report (see
/// [`TokenUsage::chat_digest_tokens`]). Serializes as
/// `{inputTokens, cachedInputTokens, outputTokens, turns, toolCalls}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDigestTokens {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub turns: u64,
    pub tool_calls: u64,
}

impl TokenUsage {
    /// Recorded provider input/output across the calling chat's turns on
    /// this device. Uses the ledger's supersession rules, including reported
    /// child usage, and excludes resume baselines and diagnostic estimates.
    pub(crate) fn chat_totals(&self, chat_id: &str) -> Result<Option<serde_json::Value>, EngineError> {
        self.flush()?;
        let db = Connection::open_with_flags(
            self.1.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(fail)?;
        db.busy_timeout(std::time::Duration::from_secs(2)).map_err(fail)?;
        db.query_row(
            &format!("SELECT SUM(m.input), SUM(m.output) {FILTER} AND t.chat=?6"),
            params![None::<String>, None::<i64>, None::<i64>, None::<String>, None::<String>, chat_id],
            |row| {
                let input: Option<u64> = row.get(0)?;
                let output: Option<u64> = row.get(1)?;
                Ok(input.zip(output).map(|(input, output)| {
                    serde_json::json!({ "chatId": chat_id, "input": input, "output": output })
                }))
            },
        )
        .map_err(fail)
    }

    /// The per-chat totals a parent's worker-report wake is stamped with on
    /// `report_to_parent { completion }`: input (including cached reads),
    /// cached input, output, turns and tool calls, under the same billing
    /// rules as [`Self::chat_totals`]. `None` when the chat has no measured
    /// turn on this device. Tool calls count `toolCall` diagnostics, as the
    /// per-turn analysis does.
    pub(crate) fn chat_digest_tokens(&self, chat_id: &str) -> Result<Option<ChatDigestTokens>, EngineError> {
        self.flush()?;
        let db = Connection::open_with_flags(
            self.1.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(fail)?;
        db.busy_timeout(std::time::Duration::from_secs(2)).map_err(fail)?;
        let totals = db
            .query_row(
                &format!(
                    "SELECT SUM(m.input), SUM(COALESCE(m.cache_read,0)), SUM(m.output), COUNT(DISTINCT t.id) {FILTER} AND t.chat=?6"
                ),
                params![None::<String>, None::<i64>, None::<i64>, None::<String>, None::<String>, chat_id],
                |row| {
                    let input: Option<u64> = row.get(0)?;
                    let cached: Option<u64> = row.get(1)?;
                    let output: Option<u64> = row.get(2)?;
                    let turns: u64 = row.get(3)?;
                    Ok(input.zip(output).map(|(input, output)| (input, cached.unwrap_or(0), output, turns)))
                },
            )
            .map_err(fail)?;
        let Some((input_tokens, cached_input_tokens, output_tokens, turns)) = totals else {
            return Ok(None);
        };
        let tool_calls: u64 = db
            .query_row(
                "SELECT COUNT(*) FROM diagnostics d JOIN turns t ON t.id=d.turn_id WHERE t.chat=?1 AND d.category='toolCall'",
                params![chat_id],
                |row| row.get(0),
            )
            .map_err(fail)?;
        Ok(Some(ChatDigestTokens { input_tokens, cached_input_tokens, output_tokens, turns, tool_calls }))
    }

    pub fn snapshot(&self, q: &TokenUsageQuery) -> Result<TokenUsageSnapshot, EngineError> {
        self.flush()?;
        // WAL readers never hold the collector's mutex while querying history.
        let connection = Connection::open_with_flags(
            self.1.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(fail)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(2))
            .map_err(fail)?;
        let db = connection.unchecked_transaction().map_err(fail)?;
        let (selected, known) = selection(q.token_kind);
        let mut result=TokenUsageSnapshot{updated_at:crate::now_ms(),coverage:vec![
            "Recorded on this device since rollout. Earlier history is not included.".into(),
            "Provider counts and observed payload bytes are separate. Bytes are not billed tokens.".into(),
            "Tools, instruction diagnostics and context occupancy use whole-turn selection when filtering by agent type; Efficiency candidates and provider totals use observation-level attribution.".into(),
            "Agent CLIs started inside shell tools are not independently metered. Their model token usage may be absent from this ledger.".into(),
            "Tool schemas, hidden prompts, repeated request inclusion, exact rule token costs and causal savings are unavailable unless the harness exposes them.".into(),
            "Claude role totals combine main and subagent usage; a precise Claude subagent split is unavailable. First results after Claude resume establish a baseline and use only observed current-process requests, so those turns may be incomplete. Claude per-model cumulative results are differenced within a harness process and include children. Request observations may be partial until a result arrives. Other harnesses may report input/output only.".into(),
            "Agent types use explicit launch origins and provider spawn metadata. Older records remain Unknown agent type. Claude child request observations are a partial breakdown and must not be added to parent totals.".into(),
            "Repeated calls, empty results and repeated instruction deliveries are investigation candidates. They can be necessary; observed bytes are not token savings.".into(),
            "Showing at most 200 groups per diagnostic category and the latest 50 context snapshots. Narrow the filters to inspect other observations.".into(),
        ],..Default::default()};
        let failures = self
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .capture_failures
            + self.2.as_ref().map_or(0, |writer| writer.dropped());
        if failures > 0 {
            result.coverage.push(format!("{failures} telemetry writes failed in this engine process. Recorded totals may be incomplete."));
        }
        result.recording_since = db
            .query_row("SELECT MIN(at) FROM turns", [], |r| r.get(0))
            .map_err(fail)?;
        result.models = db
            .prepare("SELECT DISTINCT model FROM measurements WHERE scope!='resumeBaseline' ORDER BY model")
            .map_err(fail)?
            .query_map([], |r| r.get(0))
            .map_err(fail)?
            .collect::<Result<_, _>>()
            .map_err(fail)?;
        (result.total,result.runs,result.unknown_breakdown)=db.query_row(&format!("SELECT COALESCE(SUM({selected}),0),COUNT(DISTINCT t.id),COALESCE(SUM(CASE WHEN {known} THEN 0 ELSE 1 END),0) {FILTER}"),params![q.model,q.since,q.until,q.activity,q.agent_type],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(fail)?;
        for (column, groups, order) in [
            ("t.activity", &mut result.activities, "2 DESC,1"),
            ("m.role", &mut result.roles, "2 DESC,1"),
            ("date(t.at/1000,'unixepoch')", &mut result.days, "1"),
        ] {
            let mut statement=db.prepare(&format!("SELECT {column},COALESCE(SUM({selected}),0),COUNT(DISTINCT t.id) {FILTER} GROUP BY {column} ORDER BY {order}")).map_err(fail)?;
            *groups = statement
                .query_map(
                    params![q.model, q.since, q.until, q.activity, q.agent_type],
                    |r| {
                        Ok(UsageGroup {
                            label: r.get(0)?,
                            tokens: r.get(1)?,
                            runs: r.get(2)?,
                        })
                    },
                )
                .map_err(fail)?
                .collect::<Result<_, _>>()
                .map_err(fail)?;
        }
        let mut details=db.prepare(&format!("SELECT t.id,t.chat,m.model,t.activity,m.role,t.at,t.status,t.configuration,SUM({selected}),t.context,COALESCE(a.agent_type,CASE WHEN m.role='subagent' THEN NULL ELSE json_extract(t.context,'$.agentType') END,'Unknown agent type') {FILTER} GROUP BY t.id,m.model,m.role,11 ORDER BY t.at DESC,t.id,m.model,m.role,11 LIMIT 51 OFFSET ?6")).map_err(fail)?;
        result.details = details
            .query_map(
                params![
                    q.model,
                    q.since,
                    q.until,
                    q.activity,
                    q.agent_type,
                    q.offset.min(i64::MAX as usize) as i64
                ],
                |r| {
                    Ok(UsageRun {
                        id: r.get(0)?,
                        chat_id: r.get(1)?,
                        model: r.get(2)?,
                        activity: r.get(3)?,
                        role: r.get(4)?,
                        agent_type: r.get(10)?,
                        at: r.get(5)?,
                        status: r.get(6)?,
                        configuration: r.get(7)?,
                        tokens: r.get(8)?,
                        context: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
                    })
                },
            )
            .map_err(fail)?
            .collect::<Result<_, _>>()
            .map_err(fail)?;
        result.has_more = result.details.len() > 50;
        result.details.truncate(50);
        let mut configurations=db.prepare(&format!("SELECT t.configuration,m.model,t.activity,COALESCE(SUM({selected}),0),COUNT(DISTINCT CASE WHEN t.status='completed' THEN t.id END),COUNT(DISTINCT CASE WHEN t.status IN ('failed','cancelled') THEN t.id END),COUNT(DISTINCT CASE WHEN t.status='partial' THEN t.id END),COALESCE(SUM(CASE WHEN t.status='completed' THEN {selected} ELSE 0 END),0),COUNT(DISTINCT t.id),COALESCE(a.agent_type,CASE WHEN m.role='subagent' THEN NULL ELSE json_extract(t.context,'$.agentType') END,'Unknown agent type'),json_extract(t.context,'$.workItemId'),json_extract(t.context,'$.spaceId'),COALESCE(SUM(CASE WHEN t.status IN ('failed','cancelled') THEN {selected} ELSE 0 END),0),json_extract(t.context,'$.buildRevision') {FILTER} GROUP BY t.configuration,m.model,t.activity,10,11,12,14 ORDER BY 4 DESC LIMIT 200")).map_err(fail)?;
        result.configurations = configurations
            .query_map(
                params![q.model, q.since, q.until, q.activity, q.agent_type],
                |r| {
                    Ok(ConfigurationUsage {
                        fingerprint: r.get(0)?,
                        model: r.get(1)?,
                        activity: r.get(2)?,
                        tokens: r.get(3)?,
                        completed: r.get(4)?,
                        failed: r.get(5)?,
                        partial: r.get(6)?,
                        completed_tokens: r.get(7)?,
                        runs: r.get(8)?,
                        agent_type: r.get(9)?,
                        work_item_id: r.get(10)?,
                        space_id: r.get(11)?,
                        failed_tokens: r.get(12)?,
                        build_revision: r.get(13)?,
                    })
                },
            )
            .map_err(fail)?
            .collect::<Result<_, _>>()
            .map_err(fail)?;
        // A mixed-model turn contributes each diagnostic once, not once/model.
        let mut diagnostics=db.prepare("WITH grouped AS (
            SELECT d.category,d.label,d.revision,d.channel,d.stage,d.method,CASE WHEN d.stage='availableOnDisk' THEN MAX(d.bytes) ELSE SUM(d.bytes) END AS bytes,COUNT(*) AS observations,SUM(COALESCE(d.is_error,0)) AS errors,MIN(t.chat) AS example_chat_id,COUNT(d.signature)-COUNT(DISTINCT d.signature) AS repeated,SUM(COALESCE(d.elapsed_ms,0)) AS elapsed_ms
            FROM diagnostics d JOIN turns t ON t.id=d.turn_id WHERE (?1 IS NULL OR EXISTS(SELECT 1 FROM measurements m WHERE m.turn_id=t.id AND m.model=?1 AND m.scope!='resumeBaseline')) AND (?2 IS NULL OR t.at>=?2) AND (?3 IS NULL OR t.at<?3) AND (?4 IS NULL OR t.activity=?4) AND (?5 IS NULL OR json_extract(t.context,'$.agentType')=?5 OR EXISTS(SELECT 1 FROM usage_agents agent WHERE agent.turn_id=t.id AND agent.agent_type=?5)) GROUP BY d.category,d.label,d.revision,d.channel,d.stage,d.method
            ) SELECT category,label,revision,channel,stage,method,bytes,observations,errors,example_chat_id,repeated,elapsed_ms FROM (SELECT grouped.*,ROW_NUMBER() OVER(PARTITION BY category ORDER BY bytes DESC,label) AS rank FROM grouped) WHERE rank<=200 ORDER BY bytes DESC,category,label").map_err(fail)?;
        result.diagnostics = diagnostics
            .query_map(
                params![q.model, q.since, q.until, q.activity, q.agent_type],
                |r| {
                    Ok(DiagnosticGroup {
                        category: r.get(0)?,
                        label: r.get(1)?,
                        revision: r.get(2)?,
                        channel: r.get(3)?,
                        stage: r.get(4)?,
                        method: r.get(5)?,
                        bytes: r.get(6)?,
                        observations: r.get(7)?,
                        errors: r.get(8)?,
                        example_chat_id: r.get(9)?,
                        repeated: r.get(10)?,
                        elapsed_ms: r.get(11)?,
                    })
                },
            )
            .map_err(fail)?
            .collect::<Result<_, _>>()
            .map_err(fail)?;
        let mut samples=db.prepare("SELECT s.at,s.tokens,s.window,s.role FROM context_samples s JOIN turns t ON t.id=s.turn_id WHERE (?1 IS NULL OR EXISTS(SELECT 1 FROM measurements m WHERE m.turn_id=t.id AND m.model=?1 AND m.scope!='resumeBaseline')) AND (?2 IS NULL OR t.at>=?2) AND (?3 IS NULL OR t.at<?3) AND (?4 IS NULL OR t.activity=?4) AND (?5 IS NULL OR json_extract(t.context,'$.agentType')=?5 OR EXISTS(SELECT 1 FROM usage_agents agent WHERE agent.turn_id=t.id AND agent.agent_type=?5)) ORDER BY s.at DESC LIMIT 50").map_err(fail)?;
        result.context_samples = samples
            .query_map(
                params![q.model, q.since, q.until, q.activity, q.agent_type],
                |r| {
                    Ok(UsageContextSample {
                        at: r.get(0)?,
                        tokens: r.get(1)?,
                        window: r.get(2)?,
                        role: r.get(3)?,
                    })
                },
            )
            .map_err(fail)?
            .collect::<Result<_, _>>()
            .map_err(fail)?;
        super::insights::populate(&db, q, selected, known, &mut result)?;
        super::turns::populate(&db, q, &mut result)?;
        Ok(result)
    }
}
