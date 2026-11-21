//! Attribution report: who spent the tokens, with cache fields kept apart.
//! Rows come from the same `FILTER` as every other query, so each turn is
//! counted once under the ledger's source precedence.
use super::query::FILTER;
use super::*;
use serde::Serialize;
use std::collections::BTreeMap;

const SHOWN_CHATS: usize = 25;
const UNKNOWN_MODEL: &str = "Unknown model";

/// A scheduled run's chat and span, from the scheduler's run history. The
/// ledger stores no task name for turns recorded before `UsageContext` carried
/// one, so the report joins on the chat id.
#[derive(Debug, Clone)]
pub struct ScheduledRun {
    pub chat_id: String,
    pub task: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

/// Token columns that are never summed into one number. `input` includes
/// cache reads and writes; `uncached_input` is only counted where both cache
/// fields were reported, and the rest lands in `cache_unreported_input`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Columns {
    pub turns: u64,
    pub input: u64,
    pub uncached_input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub cache_unreported_input: u64,
}

impl Columns {
    fn add_counts(&mut self, other: &Columns) {
        self.turns += other.turns;
        self.input += other.input;
        self.uncached_input += other.uncached_input;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.output += other.output;
        self.cache_unreported_input += other.cache_unreported_input;
    }
    fn add_measurement(&mut self, input: u64, output: u64, read: Option<u64>, write: Option<u64>) {
        self.input += input;
        self.output += output;
        self.cache_read += read.unwrap_or(0);
        self.cache_write += write.unwrap_or(0);
        match (read, write) {
            (Some(r), Some(w)) => self.uncached_input += input.saturating_sub(r + w),
            _ => self.cache_unreported_input += input,
        }
    }
    /// Cache reads as a share of input, over the input whose cache fields were
    /// reported. `None` when no turn reported them.
    pub fn cache_read_share(&self) -> Option<f64> {
        let known = self.input.saturating_sub(self.cache_unreported_input);
        (known > 0).then(|| self.cache_read as f64 / known as f64)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionRow {
    pub key: String,
    pub detail: String,
    pub columns: Columns,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskRow {
    pub task: String,
    pub run_chats: u64,
    pub run: Columns,
    pub follow_up: Columns,
}

/// One measurement scope: everything the ledger holds against what the
/// precedence rule accounts for. The difference is superseded or baseline
/// data that a plain sum of rows would double-count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeReconciliation {
    pub scope: String,
    pub rows: u64,
    pub all_rows_tokens: u64,
    pub accounted_tokens: u64,
}

/// What summing every provider row reports for an activity, next to what the
/// precedence rule accounts for. The first is the number a plain sum gives.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityReconciliation {
    pub activity: String,
    pub all_provider_rows_tokens: u64,
    pub all_provider_rows_uncounted_input: u64,
    pub accounted_tokens: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionReport {
    pub total: Columns,
    /// Activity as the ledger stored it, but with scheduled-run turns after
    /// the run finished split out as `Scheduled-run follow-up`.
    pub categories: Vec<AttributionRow>,
    /// Follow-ups rolled into Conversation, which is how they behave.
    pub activities: Vec<AttributionRow>,
    pub scheduled_tasks: Vec<ScheduledTaskRow>,
    pub work_items: Vec<AttributionRow>,
    pub chats: Vec<AttributionRow>,
    pub models: Vec<AttributionRow>,
    /// Agent type and what started the turn. Turns recorded before triggers
    /// were captured land in `Trigger not recorded`.
    pub triggers: Vec<AttributionRow>,
    pub reconciliation: Vec<ScopeReconciliation>,
    pub activity_reconciliation: Vec<ActivityReconciliation>,
    pub notes: Vec<String>,
}

struct TurnRow {
    chat: String,
    at: i64,
    activity: String,
    context: UsageContext,
    columns: Columns,
    models: Vec<(String, Columns)>,
}

impl TokenUsage {
    pub fn attribution_report(
        &self,
        since: Option<i64>,
        runs: &[ScheduledRun],
    ) -> Result<AttributionReport, EngineError> {
        self.flush()?;
        let connection = Connection::open_with_flags(
            self.1.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(fail)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(2))
            .map_err(fail)?;
        let db = connection.unchecked_transaction().map_err(fail)?;
        let sql = format!(
            "SELECT t.id,t.chat,t.at,t.activity,t.context,m.model,m.scope,m.input,m.output,m.cache_read,m.cache_write {FILTER} ORDER BY t.id,m.source"
        );
        let mut statement = db.prepare(&sql).map_err(fail)?;
        let mut turns: BTreeMap<String, TurnRow> = BTreeMap::new();
        let mut accounted_by_scope: BTreeMap<String, u64> = BTreeMap::new();
        let mut rows = statement
            .query(params![
                None::<String>,
                since,
                None::<i64>,
                None::<String>,
                None::<String>
            ])
            .map_err(fail)?;
        while let Some(r) = rows.next().map_err(fail)? {
            let id: String = r.get(0).map_err(fail)?;
            let (model, scope): (String, String) =
                (r.get(5).map_err(fail)?, r.get(6).map_err(fail)?);
            let (input, output): (u64, u64) = (r.get(7).map_err(fail)?, r.get(8).map_err(fail)?);
            let (read, write): (Option<u64>, Option<u64>) =
                (r.get(9).map_err(fail)?, r.get(10).map_err(fail)?);
            *accounted_by_scope.entry(scope).or_default() += input + output;
            let turn = match turns.entry(id) {
                std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::btree_map::Entry::Vacant(e) => e.insert(TurnRow {
                    chat: r.get(1).map_err(fail)?,
                    at: r.get(2).map_err(fail)?,
                    activity: r.get(3).map_err(fail)?,
                    context: serde_json::from_str(&r.get::<_, String>(4).map_err(fail)?)
                        .unwrap_or_default(),
                    columns: Columns {
                        turns: 1,
                        ..Default::default()
                    },
                    models: Vec::new(),
                }),
            };
            turn.columns.add_measurement(input, output, read, write);
            match turn.models.iter_mut().find(|(m, _)| *m == model) {
                Some((_, c)) => c.add_measurement(input, output, read, write),
                None => {
                    let mut c = Columns {
                        turns: 1,
                        ..Default::default()
                    };
                    c.add_measurement(input, output, read, write);
                    turn.models.push((model, c));
                }
            }
        }
        let mut reconciliation = Vec::new();
        {
            let mut statement = db
                .prepare(
                    "SELECT m.scope,COUNT(*),SUM(m.input+m.output) FROM measurements m JOIN turns t ON t.id=m.turn_id
                     WHERE (?1 IS NULL OR t.at>=?1) GROUP BY m.scope ORDER BY m.scope",
                )
                .map_err(fail)?;
            let mut rows = statement.query(params![since]).map_err(fail)?;
            while let Some(r) = rows.next().map_err(fail)? {
                let scope: String = r.get(0).map_err(fail)?;
                reconciliation.push(ScopeReconciliation {
                    accounted_tokens: accounted_by_scope.get(&scope).copied().unwrap_or(0),
                    scope,
                    rows: r.get(1).map_err(fail)?,
                    all_rows_tokens: r.get(2).map_err(fail)?,
                });
            }
        }
        let mut all_provider: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        {
            let mut statement = db
                .prepare(
                    "SELECT t.activity,SUM(m.input+m.output),SUM(m.input-COALESCE(m.cache_read,0))
                     FROM measurements m JOIN turns t ON t.id=m.turn_id
                     WHERE substr(m.source,1,9)='provider:' AND (?1 IS NULL OR t.at>=?1) GROUP BY t.activity",
                )
                .map_err(fail)?;
            let mut rows = statement.query(params![since]).map_err(fail)?;
            while let Some(r) = rows.next().map_err(fail)? {
                all_provider.insert(
                    r.get(0).map_err(fail)?,
                    (r.get(1).map_err(fail)?, r.get(2).map_err(fail)?),
                );
            }
        }
        Ok(build(
            turns.into_values().collect(),
            runs,
            reconciliation,
            all_provider,
        ))
    }
}

fn build(
    turns: Vec<TurnRow>,
    runs: &[ScheduledRun],
    reconciliation: Vec<ScopeReconciliation>,
    all_provider: BTreeMap<String, (u64, u64)>,
) -> AttributionReport {
    let run_by_chat: BTreeMap<&str, &ScheduledRun> =
        runs.iter().map(|r| (r.chat_id.as_str(), r)).collect();
    let mut report = AttributionReport {
        reconciliation,
        ..Default::default()
    };
    let mut accounted_by_activity: BTreeMap<&str, u64> = BTreeMap::new();
    let mut categories: BTreeMap<String, Columns> = BTreeMap::new();
    let mut activities: BTreeMap<String, Columns> = BTreeMap::new();
    let mut tasks: BTreeMap<String, (Columns, Columns, u64)> = BTreeMap::new();
    let mut task_chats: BTreeMap<(String, String), ()> = BTreeMap::new();
    let mut work_items: BTreeMap<String, Columns> = BTreeMap::new();
    let mut chats: BTreeMap<String, (String, Columns)> = BTreeMap::new();
    let mut models: BTreeMap<(String, String), Columns> = BTreeMap::new();
    let mut triggers: BTreeMap<String, Columns> = BTreeMap::new();
    let mut chat_models: BTreeMap<&str, BTreeMap<&str, u64>> = BTreeMap::new();
    for turn in &turns {
        for (model, c) in &turn.models {
            if model != UNKNOWN_MODEL {
                *chat_models
                    .entry(&turn.chat)
                    .or_default()
                    .entry(model)
                    .or_default() += c.input;
            }
        }
    }
    for turn in &turns {
        report.total.add_counts(&turn.columns);
        *accounted_by_activity.entry(&turn.activity).or_default() +=
            turn.columns.input + turn.columns.output;
        let follow_up = turn.activity == "Scheduled task"
            && run_by_chat
                .get(turn.chat.as_str())
                .is_some_and(|run| turn.at > run.finished_at.unwrap_or(run.started_at));
        let category = match (turn.activity.as_str(), follow_up) {
            ("Scheduled task", true) => "Scheduled-run follow-up".to_owned(),
            ("Scheduled task", false) if !run_by_chat.contains_key(turn.chat.as_str()) => {
                "Scheduled task (no run history)".to_owned()
            }
            (other, _) => other.to_owned(),
        };
        categories
            .entry(category.clone())
            .or_default()
            .add_counts(&turn.columns);
        let activity = if follow_up {
            "Conversation"
        } else {
            turn.activity.as_str()
        };
        activities
            .entry(activity.to_owned())
            .or_default()
            .add_counts(&turn.columns);
        if turn.activity == "Scheduled task" {
            let name = run_by_chat
                .get(turn.chat.as_str())
                .map(|r| r.task.clone())
                .or_else(|| turn.context.scheduled_task.clone())
                .unwrap_or_else(|| "Unknown task".into());
            let entry = tasks.entry(name.clone()).or_default();
            if follow_up {
                entry.1.add_counts(&turn.columns);
            } else {
                entry.0.add_counts(&turn.columns);
            }
            if task_chats.insert((name, turn.chat.clone()), ()).is_none() {
                entry.2 += 1;
            }
        }
        triggers
            .entry(format!(
                "{} · {}",
                turn.context
                    .agent_type
                    .as_deref()
                    .unwrap_or("unknown agent type"),
                turn.context
                    .trigger
                    .as_deref()
                    .unwrap_or("Trigger not recorded")
            ))
            .or_default()
            .add_counts(&turn.columns);
        if let Some(item) = &turn.context.work_item_id {
            work_items
                .entry(item.clone())
                .or_default()
                .add_counts(&turn.columns);
        }
        if turn.activity == "Work-item automation" {
            let entry = chats.entry(turn.chat.clone()).or_insert_with(|| {
                (
                    format!(
                        "{} · work item {}",
                        turn.context
                            .agent_type
                            .as_deref()
                            .unwrap_or("unknown agent type"),
                        turn.context.work_item_id.as_deref().unwrap_or("none")
                    ),
                    Columns::default(),
                )
            });
            entry.1.add_counts(&turn.columns);
        }
        for (model, c) in &turn.models {
            let explanation = if model == UNKNOWN_MODEL {
                unknown_explanation(turn, chat_models.get(turn.chat.as_str()))
            } else {
                String::new()
            };
            models
                .entry((model.clone(), explanation))
                .or_default()
                .add_counts(c);
        }
    }
    report.categories = rows(categories.into_iter().map(|(k, c)| (k, String::new(), c)));
    report.activities = rows(activities.into_iter().map(|(k, c)| (k, String::new(), c)));
    report.scheduled_tasks = tasks
        .into_iter()
        .map(|(task, (run, follow_up, run_chats))| ScheduledTaskRow {
            task,
            run_chats,
            run,
            follow_up,
        })
        .collect();
    report.scheduled_tasks.sort_by(|a, b| {
        let total = |t: &ScheduledTaskRow| t.run.input + t.follow_up.input;
        total(b).cmp(&total(a)).then(a.task.cmp(&b.task))
    });
    report.work_items = rows(work_items.into_iter().map(|(k, c)| (k, String::new(), c)));
    report.chats = rows(chats.into_iter().map(|(k, (d, c))| (k, d, c)));
    report.chats.truncate(SHOWN_CHATS);
    report.models = rows(models.into_iter().map(|((k, d), c)| (k, d, c)));
    report.triggers = rows(triggers.into_iter().map(|(k, c)| (k, String::new(), c)));
    report.activity_reconciliation = all_provider
        .into_iter()
        .map(
            |(activity, (tokens, uncounted_input))| ActivityReconciliation {
                accounted_tokens: accounted_by_activity
                    .get(activity.as_str())
                    .copied()
                    .unwrap_or(0),
                activity,
                all_provider_rows_tokens: tokens,
                all_provider_rows_uncounted_input: uncounted_input,
            },
        )
        .collect();
    report.notes = vec![
        "Input includes cache reads and cache writes. Uncached input is input minus both, and is only counted for turns that reported both. Never add the columns together.".into(),
        "One source per turn: provider turn totals replace their request rows, resume baselines are excluded, and legacy counts are used only when a turn has nothing richer. A plain sum of every row counts the same requests more than once.".into(),
        "Scheduled-run follow-up is any turn in a scheduled run's chat that started after the run finished. The ledger stores no prompt, so the timing is the evidence that it is a later message and not that a person wrote it.".into(),
    ];
    report
}

fn rows(items: impl Iterator<Item = (String, String, Columns)>) -> Vec<AttributionRow> {
    let mut rows = items
        .map(|(key, detail, columns)| AttributionRow {
            key,
            detail,
            columns,
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        b.columns
            .input
            .cmp(&a.columns.input)
            .then(a.key.cmp(&b.key))
            .then(a.detail.cmp(&b.detail))
    });
    rows
}

/// Why a turn has no model label, and the best evidence for which one it was.
fn unknown_explanation(turn: &TurnRow, siblings: Option<&BTreeMap<&str, u64>>) -> String {
    let harness = turn.context.harness.as_deref().unwrap_or("unknown harness");
    let hint = match (
        turn.context
            .configured_model
            .as_deref()
            .filter(|m| !m.is_empty()),
        siblings.and_then(|m| m.iter().max_by_key(|(_, n)| **n)),
    ) {
        (Some(configured), _) => format!("configured model {configured}"),
        (None, Some((sibling, _))) => {
            format!("no configured model; this chat's other turns use {sibling}")
        }
        (None, None) => "no configured model and no other turn in this chat names one".into(),
    };
    format!("{harness} reported no model; {hint}")
}
