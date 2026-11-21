//! Print the per-turn token analysis from a usage ledger without touching it:
//! the ledger is copied with `VACUUM INTO` from a read-only connection first.
//!
//!   cargo run -p mast-engine --example token_usage_report -- <store-root> [days]
//!
//! `<store-root>` holds `token-usage.sqlite3`, e.g. `~/.mast/profiles/local`.

use mast_engine::token_usage::{AttributionReport, AttributionRow, Columns, ScheduledRun};
use mast_proto::token_usage::{TokenUsageQuery, TurnGroup};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let root = std::path::PathBuf::from(args.next().expect("store root"));
    let days: Option<i64> = args.next().map(|d| d.parse()).transpose()?;
    let copy = tempfile::tempdir()?;
    rusqlite::Connection::open_with_flags(
        root.join("token-usage.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?
    .execute(
        "VACUUM INTO ?1",
        [copy.path().join("token-usage.sqlite3").to_string_lossy()],
    )?;
    let runs = scheduled_runs(&root, copy.path())?;
    let usage = mast_engine::token_usage::TokenUsage::open(copy.path())?;
    let now = chrono::Utc::now().timestamp_millis();
    let since = days.map(|d| now - d * 86_400_000);
    let report = usage.attribution_report(since, &runs)?;
    print_attribution(&report, runs.len());
    let snapshot = usage.snapshot(&TokenUsageQuery {
        since,
        ..Default::default()
    })?;
    let reported = report.total.input + report.total.output;
    println!(
        "\ncheck: attribution total {reported} == engine query total {}: {}",
        snapshot.total,
        if reported == snapshot.total {
            "OK"
        } else {
            "MISMATCH"
        }
    );
    if reported != snapshot.total {
        return Err("attribution total differs from the engine query total".into());
    }
    let analysis = snapshot.turn_analysis.unwrap_or_default();
    println!(
        "{} accounted tokens over {} turns ({})",
        snapshot.total,
        snapshot.runs,
        days.map_or("all recorded".into(), |d| format!("last {d} days"))
    );
    println!("\n== Reported fields by harness ==");
    for h in &analysis.harnesses {
        println!(
            "{:<12} turns={} cacheRead={} cacheWrite={} reasoning={} requests={} start={} legacyOnly={}\n  {}",
            h.harness,
            h.turns,
            h.cache_read_turns,
            h.cache_write_turns,
            h.reasoning_turns,
            h.request_count_turns,
            h.session_start_turns,
            h.legacy_only_turns,
            h.semantics
        );
    }
    print_groups(
        "Workflows (harness · agent type · start)",
        &analysis.workflows,
    );
    print_groups("Chat age", &analysis.chat_ages);
    println!("\n== Largest turns ==");
    for t in analysis.turns.iter().take(15) {
        let c = &t.counts;
        println!(
            "{} {:<10} {:<18} {:<10} in={} read={} write={} out={} req={} sub={} peak={} tools={} toolBytes={} start={} ageH={} scope={} chat={}",
            t.at,
            t.harness.as_deref().unwrap_or("?"),
            t.agent_type,
            t.status,
            c.input,
            opt(c.cache_read),
            opt(c.cache_write),
            c.output,
            opt(t.requests),
            opt(t.subagent_requests),
            opt(t.peak_context),
            t.tool_calls,
            t.tool_result_bytes,
            t.session_start.as_deref().unwrap_or("?"),
            t.chat_age_ms
                .map_or("?".into(), |a| (a / 3_600_000).to_string()),
            t.scope,
            t.chat_id
        );
    }
    Ok(())
}

/// Run history from a read-only `VACUUM INTO` copy of the scheduler's store.
/// A missing store means no task attribution, not an error.
fn scheduled_runs(
    root: &std::path::Path,
    scratch: &std::path::Path,
) -> Result<Vec<ScheduledRun>, Box<dyn std::error::Error>> {
    let source = root.join("scheduled-tasks.sqlite3");
    if !source.exists() {
        return Ok(Vec::new());
    }
    let copy = scratch.join("scheduled-tasks.sqlite3");
    rusqlite::Connection::open_with_flags(&source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?
        .execute("VACUUM INTO ?1", [copy.to_string_lossy()])?;
    let db =
        rusqlite::Connection::open_with_flags(copy, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = db.prepare("SELECT body FROM runs")?;
    let bodies = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(bodies
        .iter()
        .filter_map(|body| {
            let run: serde_json::Value = serde_json::from_str(body).ok()?;
            Some(ScheduledRun {
                chat_id: run["chatId"].as_str()?.to_owned(),
                task: run["snapshot"]["name"].as_str()?.to_owned(),
                started_at: run["createdAt"].as_i64()?,
                finished_at: run["finishedAt"].as_i64(),
            })
        })
        .collect())
}

fn m(tokens: u64) -> String {
    format!("{:.1}M", tokens as f64 / 1e6)
}

fn columns(c: &Columns) -> String {
    format!(
        "turns={:<5} uncached={:<9} cacheRead={:<10} cacheWrite={:<9} output={:<8} (input {}{})",
        c.turns,
        m(c.uncached_input),
        m(c.cache_read),
        m(c.cache_write),
        m(c.output),
        m(c.input),
        if c.cache_unreported_input > 0 {
            format!(", cache unreported on {}", m(c.cache_unreported_input))
        } else {
            String::new()
        }
    )
}

fn print_rows(title: &str, rows: &[AttributionRow], limit: usize) {
    println!("\n== {title} ==");
    for row in rows.iter().take(limit) {
        let label = if row.detail.is_empty() {
            row.key.clone()
        } else {
            format!("{} [{}]", row.key, row.detail)
        };
        println!("{label:<60} {}", columns(&row.columns));
    }
}

fn print_attribution(report: &AttributionReport, runs: usize) {
    println!("== Attribution (one source per turn; {runs} scheduled runs joined) ==");
    println!("{:<60} {}", "TOTAL", columns(&report.total));
    print_rows("Category (follow-ups split out)", &report.categories, 20);
    print_rows(
        "Activity (follow-ups counted as Conversation)",
        &report.activities,
        20,
    );
    println!("\n== Scheduled tasks ==");
    for t in &report.scheduled_tasks {
        println!(
            "{:<40} runChats={:<3} run: {}",
            t.task,
            t.run_chats,
            columns(&t.run)
        );
        if t.follow_up.turns > 0 {
            println!("{:<40} {:<13} followUp: {}", "", "", columns(&t.follow_up));
        }
    }
    print_rows("Work items", &report.work_items, 20);
    print_rows("Work-item chats", &report.chats, 25);
    print_rows(
        "Models (a turn counts once under each model it used)",
        &report.models,
        20,
    );
    print_rows("Agent type and trigger", &report.triggers, 20);
    println!("\n== Reconciliation: all rows vs accounted ==");
    for s in &report.reconciliation {
        println!(
            "{:<24} rows={:<6} allRows={:<10} accounted={:<10} notCounted={}",
            s.scope,
            s.rows,
            m(s.all_rows_tokens),
            m(s.accounted_tokens),
            m(s.all_rows_tokens - s.accounted_tokens.min(s.all_rows_tokens))
        );
    }
    println!("\n== Reconciliation by activity: plain sum of provider rows vs accounted ==");
    for a in &report.activity_reconciliation {
        println!(
            "{:<24} plainSum={:<10} (input minus cache read {:<8}) accounted={}",
            a.activity,
            m(a.all_provider_rows_tokens),
            m(a.all_provider_rows_uncounted_input),
            m(a.accounted_tokens)
        );
    }
    for note in &report.notes {
        println!("note: {note}");
    }
}

fn print_groups(title: &str, groups: &[TurnGroup]) {
    println!("\n== {title} ==");
    for g in groups {
        let share = |part: u64| {
            (g.cache_known_input > 0)
                .then(|| format!("{:.1}%", part as f64 * 100.0 / g.cache_known_input as f64))
                .unwrap_or_else(|| "?".into())
        };
        println!(
            "{:<55} turns={:<4} input={:<11} out={:<9} median={:<9} p90={:<10} cacheRead={} cacheWrite={} (on {}/{} turns) perRequest={} requests={} peakMax={} toolBytes={}",
            g.label,
            g.turns,
            g.input,
            g.output,
            g.median_input,
            g.p90_input,
            share(g.cache_read),
            share(g.cache_write),
            g.cache_known_turns,
            g.turns,
            (g.requests > 0)
                .then(|| (g.request_known_input / g.requests).to_string())
                .unwrap_or_else(|| "?".into()),
            g.requests,
            g.peak_context_max,
            g.tool_result_bytes
        );
    }
}

fn opt(n: Option<u64>) -> String {
    n.map_or("?".into(), |n| n.to_string())
}
