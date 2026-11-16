use super::*;

// Aggregates replace request observations, including child-role observations.
// Legacy counts are only used when a turn has no richer measurements.
const FILTER:&str="FROM measurements m JOIN turns t ON t.id=m.turn_id
    WHERE (?1 IS NULL OR m.model=?1) AND (?2 IS NULL OR t.at>=?2)
    AND (?3 IS NULL OR t.at<?3) AND (?4 IS NULL OR t.activity=?4)
    AND (m.scope!='legacy' OR NOT EXISTS(SELECT 1 FROM measurements x WHERE x.turn_id=m.turn_id AND x.scope!='legacy'))
    AND (m.scope IN ('turn','turnIncludingChildren','legacy') OR NOT EXISTS(SELECT 1 FROM measurements x WHERE x.turn_id=m.turn_id AND x.model=m.model AND x.scope IN ('turn','turnIncludingChildren')))";

fn selection(kind: TokenKind) -> (&'static str, &'static str) {
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

impl TokenUsage {
    pub fn snapshot(&self, q: &TokenUsageQuery) -> Result<TokenUsageSnapshot, EngineError> {
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
            "Tool schemas, hidden prompts, repeated request inclusion, exact rule token costs and causal savings are unavailable unless the harness exposes them.".into(),
            "Claude per-model cumulative results are differenced within a harness process and include children. Request observations may be partial until a result arrives. Other harnesses may report input/output only.".into(),
            "Showing at most 200 diagnostic groups and the latest 50 context snapshots. Narrow the filters to inspect other observations.".into(),
        ],..Default::default()};
        let failures = self
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .capture_failures;
        if failures > 0 {
            result.coverage.push(format!("{failures} telemetry writes failed in this engine process. Recorded totals may be incomplete."));
        }
        result.recording_since = db
            .query_row("SELECT MIN(at) FROM turns", [], |r| r.get(0))
            .map_err(fail)?;
        result.models = db
            .prepare("SELECT DISTINCT model FROM measurements ORDER BY model")
            .map_err(fail)?
            .query_map([], |r| r.get(0))
            .map_err(fail)?
            .collect::<Result<_, _>>()
            .map_err(fail)?;
        (result.total,result.runs,result.unknown_breakdown)=db.query_row(&format!("SELECT COALESCE(SUM({selected}),0),COUNT(DISTINCT t.id),COALESCE(SUM(CASE WHEN {known} THEN 0 ELSE 1 END),0) {FILTER}"),params![q.model,q.since,q.until,q.activity],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(fail)?;
        for (column, groups, order) in [
            ("t.activity", &mut result.activities, "2 DESC,1"),
            ("m.role", &mut result.roles, "2 DESC,1"),
            ("date(t.at/1000,'unixepoch')", &mut result.days, "1"),
        ] {
            let mut statement=db.prepare(&format!("SELECT {column},COALESCE(SUM({selected}),0),COUNT(DISTINCT t.id) {FILTER} GROUP BY {column} ORDER BY {order}")).map_err(fail)?;
            *groups = statement
                .query_map(params![q.model, q.since, q.until, q.activity], |r| {
                    Ok(UsageGroup {
                        label: r.get(0)?,
                        tokens: r.get(1)?,
                        runs: r.get(2)?,
                    })
                })
                .map_err(fail)?
                .collect::<Result<_, _>>()
                .map_err(fail)?;
        }
        let mut details=db.prepare(&format!("SELECT t.id,t.chat,m.model,t.activity,m.role,t.at,t.status,t.configuration,SUM({selected}),t.context {FILTER} GROUP BY t.id,m.model,m.role ORDER BY t.at DESC,t.id,m.model,m.role LIMIT 51 OFFSET ?5")).map_err(fail)?;
        result.details = details
            .query_map(
                params![
                    q.model,
                    q.since,
                    q.until,
                    q.activity,
                    q.offset.min(i64::MAX as usize) as i64
                ],
                |r| {
                    Ok(UsageRun {
                        id: r.get(0)?,
                        chat_id: r.get(1)?,
                        model: r.get(2)?,
                        activity: r.get(3)?,
                        role: r.get(4)?,
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
        let mut configurations=db.prepare(&format!("SELECT t.configuration,m.model,t.activity,COALESCE(SUM({selected}),0),COUNT(DISTINCT CASE WHEN t.status='completed' THEN t.id END),COUNT(DISTINCT CASE WHEN t.status IN ('failed','cancelled') THEN t.id END),COUNT(DISTINCT CASE WHEN t.status='partial' THEN t.id END),COALESCE(SUM(CASE WHEN t.status='completed' THEN {selected} ELSE 0 END),0),COUNT(DISTINCT t.id) {FILTER} GROUP BY t.configuration,m.model,t.activity ORDER BY 4 DESC LIMIT 200")).map_err(fail)?;
        result.configurations = configurations
            .query_map(params![q.model, q.since, q.until, q.activity], |r| {
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
                })
            })
            .map_err(fail)?
            .collect::<Result<_, _>>()
            .map_err(fail)?;
        // A mixed-model turn contributes each diagnostic once, not once/model.
        let mut diagnostics=db.prepare("SELECT d.category,d.label,d.revision,d.channel,d.stage,d.method,SUM(d.bytes),COUNT(*),SUM(COALESCE(d.is_error,0)),MIN(t.chat),COUNT(d.signature)-COUNT(DISTINCT d.signature),SUM(COALESCE(d.elapsed_ms,0)) FROM diagnostics d JOIN turns t ON t.id=d.turn_id WHERE (?1 IS NULL OR EXISTS(SELECT 1 FROM measurements m WHERE m.turn_id=t.id AND m.model=?1)) AND (?2 IS NULL OR t.at>=?2) AND (?3 IS NULL OR t.at<?3) AND (?4 IS NULL OR t.activity=?4) GROUP BY d.category,d.label,d.revision,d.channel,d.stage,d.method ORDER BY SUM(d.bytes) DESC LIMIT 200").map_err(fail)?;
        result.diagnostics = diagnostics
            .query_map(params![q.model, q.since, q.until, q.activity], |r| {
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
            })
            .map_err(fail)?
            .collect::<Result<_, _>>()
            .map_err(fail)?;
        let mut samples=db.prepare("SELECT s.at,s.tokens,s.window,s.role FROM context_samples s JOIN turns t ON t.id=s.turn_id WHERE (?1 IS NULL OR EXISTS(SELECT 1 FROM measurements m WHERE m.turn_id=t.id AND m.model=?1)) AND (?2 IS NULL OR t.at>=?2) AND (?3 IS NULL OR t.at<?3) AND (?4 IS NULL OR t.activity=?4) ORDER BY s.at DESC LIMIT 50").map_err(fail)?;
        result.context_samples = samples
            .query_map(params![q.model, q.since, q.until, q.activity], |r| {
                Ok(UsageContextSample {
                    at: r.get(0)?,
                    tokens: r.get(1)?,
                    window: r.get(2)?,
                    role: r.get(3)?,
                })
            })
            .map_err(fail)?
            .collect::<Result<_, _>>()
            .map_err(fail)?;
        Ok(result)
    }
}
