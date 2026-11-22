//! Per-turn accounting for comparing workflows. Uses the billing FILTER so turn
//! sums reconcile with the snapshot total under the same filters.
use super::query::FILTER;
use super::*;
use std::collections::BTreeMap;

const SHOWN_TURNS: usize = 50;

pub(super) fn populate(
    db: &Connection,
    q: &TokenUsageQuery,
    result: &mut TokenUsageSnapshot,
) -> Result<(), EngineError> {
    let sql = format!(
        "WITH acc AS (
            SELECT t.id,t.chat,t.at,t.status,t.activity,t.context,
            group_concat(DISTINCT m.model) AS models,
            SUM(m.input) AS input,SUM(m.output) AS output,
            CASE WHEN COUNT(m.cache_read)=COUNT(*) THEN SUM(m.cache_read) END AS cache_read,
            CASE WHEN COUNT(m.cache_write)=COUNT(*) THEN SUM(m.cache_write) END AS cache_write,
            CASE WHEN COUNT(m.reasoning)=COUNT(*) THEN SUM(m.reasoning) END AS reasoning,
            MAX(m.scope='turnIncludingChildren') AS with_children,MAX(m.scope='turn') AS turn_scope,
            MAX(m.scope='request') AS request_scope
            {FILTER} GROUP BY t.id),
        req AS (SELECT turn_id,SUM(role='main') AS main,SUM(role='subagent') AS sub FROM measurements
            WHERE scope='request' AND (?1 IS NULL OR model=?1) AND turn_id IN (SELECT id FROM acc) GROUP BY turn_id),
        ctx AS (SELECT turn_id,MAX(tokens) AS peak FROM context_samples
            WHERE role='main' AND turn_id IN (SELECT id FROM acc) GROUP BY turn_id),
        tools AS (SELECT d.turn_id,SUM(d.category='toolCall') AS calls,
            SUM(CASE WHEN d.category='toolResult' AND d.stage='harnessResponse' THEN d.bytes
              WHEN d.category='retainedToolResult' AND NOT EXISTS(SELECT 1 FROM diagnostics raw WHERE raw.turn_id=d.turn_id
                AND raw.source=d.source AND raw.category='toolResult' AND raw.stage='harnessResponse') THEN d.bytes
              ELSE 0 END) AS bytes
            FROM diagnostics d WHERE d.turn_id IN (SELECT id FROM acc) GROUP BY d.turn_id)
        SELECT acc.id,acc.chat,acc.at,acc.status,acc.activity,acc.context,acc.models,acc.input,acc.output,
            acc.cache_read,acc.cache_write,acc.reasoning,acc.with_children,acc.turn_scope,acc.request_scope,
            COALESCE(req.main,0),COALESCE(req.sub,0),ctx.peak,COALESCE(tools.calls,0),COALESCE(tools.bytes,0)
        FROM acc LEFT JOIN req ON req.turn_id=acc.id LEFT JOIN ctx ON ctx.turn_id=acc.id
        LEFT JOIN tools ON tools.turn_id=acc.id"
    );
    let mut statement = db.prepare(&sql).map_err(fail)?;
    let profiles = statement
        .query_map(
            params![q.model, q.since, q.until, q.activity, q.agent_type],
            |r| {
                let context: UsageContext =
                    serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default();
                let at: i64 = r.get(2)?;
                let main_requests: u64 = r.get(15)?;
                // A turn without request rows predates request capture or
                // uses an adapter that never reports them: unknown, not zero.
                let requests = (main_requests > 0).then_some(main_requests);
                let scope = if r.get::<_, bool>(12)? {
                    "turnIncludingChildren"
                } else if r.get::<_, bool>(13)? {
                    "turn"
                } else if r.get::<_, bool>(14)? {
                    "request"
                } else {
                    "legacy"
                };
                Ok(TurnProfile {
                    turn_id: r.get(0)?,
                    chat_id: r.get(1)?,
                    at,
                    status: r.get(3)?,
                    activity: r.get(4)?,
                    agent_type: context
                        .agent_type
                        .clone()
                        .unwrap_or_else(|| "Unknown agent type".into()),
                    harness: context.harness.clone(),
                    models: r
                        .get::<_, String>(6)?
                        .split(',')
                        .map(str::to_owned)
                        .collect(),
                    session_start: context.session_start.clone(),
                    chat_age_ms: context
                        .chat_created_at
                        .map(|created| at - created)
                        .filter(|age| *age >= 0),
                    counts: TokenCounts {
                        input: r.get(7)?,
                        output: r.get(8)?,
                        cache_read: r.get(9)?,
                        cache_write: r.get(10)?,
                        reasoning: r.get(11)?,
                    },
                    scope: scope.into(),
                    requests,
                    subagent_requests: requests.map(|_| r.get(16)).transpose()?,
                    peak_context: r.get(17)?,
                    tool_calls: r.get(18)?,
                    tool_result_bytes: r.get(19)?,
                })
            },
        )
        .map_err(fail)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(fail)?;
    result.turn_analysis = Some(analyze(profiles));
    Ok(())
}

pub(super) fn analyze(mut profiles: Vec<TurnProfile>) -> TurnAnalysis {
    profiles.sort_by(|a, b| {
        b.counts
            .input
            .cmp(&a.counts.input)
            .then(b.at.cmp(&a.at))
            .then(a.turn_id.cmp(&b.turn_id))
    });
    let mut workflows: BTreeMap<String, Vec<&TurnProfile>> = BTreeMap::new();
    let mut ages: BTreeMap<usize, Vec<&TurnProfile>> = BTreeMap::new();
    let mut harnesses: BTreeMap<String, HarnessCoverage> = BTreeMap::new();
    for turn in &profiles {
        let harness = turn.harness.as_deref().unwrap_or("Unknown harness");
        workflows
            .entry(format!(
                "{harness} · {} · {}",
                turn.agent_type,
                start_label(turn.session_start.as_deref())
            ))
            .or_default()
            .push(turn);
        ages.entry(age_bucket(turn.chat_age_ms))
            .or_default()
            .push(turn);
        let coverage = harnesses
            .entry(harness.to_owned())
            .or_insert_with(|| HarnessCoverage {
                harness: harness.to_owned(),
                semantics: semantics(harness).into(),
                ..Default::default()
            });
        coverage.turns += 1;
        coverage.cache_read_turns += u64::from(turn.counts.cache_read.is_some());
        coverage.cache_write_turns += u64::from(turn.counts.cache_write.is_some());
        coverage.reasoning_turns += u64::from(turn.counts.reasoning.is_some());
        coverage.request_count_turns += u64::from(turn.requests.is_some());
        coverage.legacy_only_turns += u64::from(turn.scope == "legacy");
        coverage.session_start_turns += u64::from(turn.session_start.is_some());
    }
    let mut workflows = workflows
        .into_iter()
        .map(|(label, turns)| group(label, &turns))
        .collect::<Vec<_>>();
    workflows.sort_by(|a, b| b.input.cmp(&a.input).then(a.label.cmp(&b.label)));
    let chat_ages = ages
        .into_iter()
        .map(|(bucket, turns)| group(AGE_LABELS[bucket].into(), &turns))
        .collect();
    let mut harnesses = harnesses.into_values().collect::<Vec<_>>();
    harnesses.sort_by(|a, b| b.turns.cmp(&a.turns).then(a.harness.cmp(&b.harness)));
    profiles.truncate(SHOWN_TURNS);
    TurnAnalysis {
        turns: profiles,
        workflows,
        chat_ages,
        harnesses,
    }
}

fn group(label: String, turns: &[&TurnProfile]) -> TurnGroup {
    let mut inputs = turns.iter().map(|t| t.counts.input).collect::<Vec<_>>();
    inputs.sort_unstable();
    let mut group = TurnGroup {
        label,
        turns: turns.len() as u64,
        median_input: percentile(&inputs, 50),
        p90_input: percentile(&inputs, 90),
        example_chat_id: turns
            .iter()
            .max_by_key(|t| t.counts.input)
            .map(|t| t.chat_id.clone())
            .unwrap_or_default(),
        ..Default::default()
    };
    for turn in turns {
        let c = &turn.counts;
        group.input += c.input;
        group.output += c.output;
        if let Some(read) = c.cache_read {
            group.cache_known_input += c.input;
            group.cache_read += read;
            group.cache_write += c.cache_write.unwrap_or(0);
            group.cache_known_turns += 1;
        }
        if let Some(requests) = turn.requests {
            group.request_known_input += c.input;
            group.requests += requests;
            group.request_known_turns += 1;
        }
        if let Some(peak) = turn.peak_context {
            group.peak_context_max = group.peak_context_max.max(peak);
            group.peak_context_known_turns += 1;
        }
        group.tool_result_bytes += turn.tool_result_bytes;
    }
    group
}

/// Nearest-rank percentile of a sorted slice.
fn percentile(sorted: &[u64], pct: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (pct * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn start_label(start: Option<&str>) -> &'static str {
    match start {
        Some("fresh") => "new session",
        Some("resumed") => "resumed session",
        Some("continued") => "continued in process",
        _ => "start unrecorded",
    }
}

const AGE_LABELS: [&str; 6] = [
    "Chat under 1 hour old",
    "Chat 1–6 hours old",
    "Chat 6–24 hours old",
    "Chat 1–7 days old",
    "Chat over 7 days old",
    "Chat age unrecorded",
];
fn age_bucket(age: Option<i64>) -> usize {
    const HOUR: i64 = 3_600_000;
    match age {
        Some(a) if a < HOUR => 0,
        Some(a) if a < 6 * HOUR => 1,
        Some(a) if a < 24 * HOUR => 2,
        Some(a) if a < 7 * 24 * HOUR => 3,
        Some(_) => 4,
        None => 5,
    }
}

fn semantics(harness: &str) -> &'static str {
    match harness {
        "ClaudeCode" => {
            "Per-request and per-result counts from the provider. Input includes cache reads and cache writes. Result totals include subagents and replace request rows. Results do not report reasoning. The first result after a resume only sets a baseline and is excluded."
        }
        "Codex" => {
            "Cumulative thread counters converted to per-turn amounts, plus each request's own counts. Input includes cached input. Codex does not report cache writes; they are recorded as zero. Reasoning is part of output."
        }
        _ => {
            "Input and output totals only. Cache, reasoning and request counts are not reported, and it is unknown whether input includes cached tokens."
        }
    }
}
