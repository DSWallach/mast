use super::query::FILTER;
use super::*;

pub(super) const AGENT: &str = "COALESCE(a.agent_type,CASE WHEN m.role='subagent' THEN NULL ELSE json_extract(t.context,'$.agentType') END,'Unknown agent type')";

pub(super) fn populate(
    db: &Connection,
    q: &TokenUsageQuery,
    selected: &str,
    known: &str,
    result: &mut TokenUsageSnapshot,
) -> Result<(), EngineError> {
    result.insights_available = true;
    let bindings = params![q.model, q.since, q.until, q.activity, q.agent_type];
    result.available_agent_types = db.prepare(&format!(
        "SELECT DISTINCT {AGENT} FROM measurements m JOIN turns t ON t.id=m.turn_id
         LEFT JOIN usage_agents a ON a.turn_id=m.turn_id AND a.source=m.source AND a.kind='measurement'
         WHERE m.scope!='resumeBaseline' ORDER BY 1"
    )).map_err(fail)?.query_map([], |r| r.get(0)).map_err(fail)?.collect::<Result<_,_>>().map_err(fail)?;

    result.agent_types = agent_groups(
        db,
        &format!(
        "SELECT {AGENT},m.role,COALESCE(SUM({selected}),0),COUNT(DISTINCT t.id),COUNT(*),MIN(t.chat),COALESCE(SUM(CASE WHEN {known} THEN 0 ELSE 1 END),0)
         {FILTER} GROUP BY 1,2 ORDER BY 3 DESC,1,2 LIMIT 200"
    ),
        bindings,
    )?;

    // Claude aggregates intentionally suppress these rows in the billing query.
    // Keep the provider's child-request evidence visible without adding it again.
    result.observed_agents = agent_groups(db, &format!(
        "SELECT {AGENT},m.role,COALESCE(SUM({selected}),0),COUNT(DISTINCT t.id),COUNT(*),MIN(t.chat),COALESCE(SUM(CASE WHEN {known} THEN 0 ELSE 1 END),0)
         FROM measurements m JOIN turns t ON t.id=m.turn_id
         LEFT JOIN usage_agents a ON a.turn_id=m.turn_id AND a.source=m.source AND a.kind='measurement'
         WHERE m.role='subagent' AND m.scope='request'
         AND (?1 IS NULL OR m.model=?1) AND (?2 IS NULL OR t.at>=?2) AND (?3 IS NULL OR t.at<?3)
         AND (?4 IS NULL OR t.activity=?4) AND (?5 IS NULL OR {AGENT}=?5)
         GROUP BY 1,2 ORDER BY 3 DESC,1 LIMIT 200"
    ), bindings)?;

    result.efficiency = db.query_row(&format!(
        "WITH turns_selected AS (SELECT t.id,t.status,SUM({selected}) AS tokens {FILTER} GROUP BY t.id,t.status)
         SELECT COUNT(CASE WHEN status='completed' THEN 1 END),
         COUNT(CASE WHEN status='failed' THEN 1 END),COUNT(CASE WHEN status='cancelled' THEN 1 END),
         COUNT(CASE WHEN status='partial' THEN 1 END),
         COALESCE(SUM(CASE WHEN status='completed' THEN tokens ELSE 0 END),0),
         COALESCE(SUM(CASE WHEN status IN ('failed','cancelled') THEN tokens ELSE 0 END),0),
         COALESCE(SUM(CASE WHEN status='partial' THEN tokens ELSE 0 END),0) FROM turns_selected"
    ), bindings, |r| Ok(EfficiencySummary {
        completed:r.get(0)?, failed:r.get(1)?, cancelled:r.get(2)?, partial:r.get(3)?,
        completed_tokens:r.get(4)?, failed_tokens:r.get(5)?, partial_tokens:r.get(6)?,
    })).map_err(fail)?;

    let sql = "WITH observations AS (
        SELECT d.*,t.chat,t.at,COALESCE(a.agent_type,'Unknown agent type') AS agent_type,
        COALESCE(d.signature,CASE WHEN d.category IN ('instruction','instructionEnvelope') THEN d.revision END) AS identity
        FROM diagnostics d JOIN turns t ON t.id=d.turn_id
        LEFT JOIN usage_agents a ON a.turn_id=d.turn_id AND a.source=d.source AND a.kind=d.category||':'||d.stage
        WHERE (?1 IS NULL OR EXISTS(SELECT 1 FROM measurements m WHERE m.turn_id=t.id AND m.model=?1 AND m.scope!='resumeBaseline'))
        AND (?2 IS NULL OR t.at>=?2) AND (?3 IS NULL OR t.at<?3) AND (?4 IS NULL OR t.activity=?4)
        AND (?5 IS NULL OR COALESCE(a.agent_type,'Unknown agent type')=?5)
        AND (d.category IN ('toolCall','toolResult','retainedToolResult')
          OR (d.category IN ('instruction','instructionEnvelope') AND d.stage='submittedToHarness'))
        AND (d.category!='retainedToolResult' OR NOT EXISTS(
            SELECT 1 FROM diagnostics raw WHERE raw.turn_id=d.turn_id AND raw.source=d.source AND raw.category='toolResult' AND raw.stage='harnessResponse'))
        AND (d.category!='instructionEnvelope' OR NOT EXISTS(
            SELECT 1 FROM diagnostics entries WHERE entries.turn_id=d.turn_id AND entries.category='instruction' AND entries.stage='submittedToHarness'))
      ), ranked AS (
        SELECT observations.*,ROW_NUMBER() OVER(PARTITION BY chat,category,label,channel,stage,agent_type,identity ORDER BY at,turn_id,source) AS occurrence FROM observations
      )
      SELECT category,label,channel,stage,agent_type,COUNT(*),
      SUM(CASE WHEN identity IS NOT NULL AND occurrence>1 THEN 1 ELSE 0 END),SUM(bytes),
      SUM(CASE WHEN identity IS NOT NULL AND occurrence>1 THEN bytes ELSE 0 END),
      SUM(COALESCE(is_error,0)),
      SUM(CASE WHEN category IN ('toolResult','retainedToolResult') AND method='bytesOnly' AND bytes=0 AND channel IN ('cli','mcp') THEN 1 ELSE 0 END),
      chat,MIN(turn_id),MIN(at)
      FROM ranked GROUP BY chat,category,label,channel,stage,agent_type
      HAVING SUM(CASE WHEN identity IS NOT NULL AND occurrence>1 THEN 1 ELSE 0 END)>0 OR SUM(COALESCE(is_error,0))>0
        OR SUM(CASE WHEN category IN ('toolResult','retainedToolResult') AND method='bytesOnly' AND bytes=0 AND channel IN ('cli','mcp') THEN 1 ELSE 0 END)>0
      ORDER BY 9 DESC,7 DESC,10 DESC,11 DESC,category,label,chat LIMIT 200";
    result.insights = db
        .prepare(sql)
        .map_err(fail)?
        .query_map(bindings, |r| {
            Ok(EfficiencyInsight {
                category: r.get(0)?,
                label: r.get(1)?,
                channel: r.get(2)?,
                stage: r.get(3)?,
                agent_type: r.get(4)?,
                occurrences: r.get(5)?,
                repeated: r.get(6)?,
                bytes: r.get(7)?,
                repeated_bytes: r.get(8)?,
                errors: r.get(9)?,
                empty_results: r.get(10)?,
                example_chat_id: r.get(11)?,
                example_turn_id: r.get(12)?,
                example_at: r.get(13)?,
            })
        })
        .map_err(fail)?
        .collect::<Result<_, _>>()
        .map_err(fail)?;
    Ok(())
}

fn agent_groups(
    db: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<AgentUsageGroup>, EngineError> {
    db.prepare(sql)
        .map_err(fail)?
        .query_map(params, |r| {
            Ok(AgentUsageGroup {
                unknown_breakdown: r.get(6)?,
                agent_type: r.get(0)?,
                role: r.get(1)?,
                tokens: r.get(2)?,
                turns: r.get(3)?,
                observations: r.get(4)?,
                example_chat_id: r.get(5)?,
            })
        })
        .map_err(fail)?
        .collect::<Result<_, _>>()
        .map_err(fail)
}
