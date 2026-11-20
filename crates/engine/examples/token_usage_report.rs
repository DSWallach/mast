//! Print the per-turn token analysis from a usage ledger without touching it:
//! the ledger is copied with `VACUUM INTO` from a read-only connection first.
//!
//!   cargo run -p mast-engine --example token_usage_report -- <store-root> [days]
//!
//! `<store-root>` holds `token-usage.sqlite3`, e.g. `~/.mast/profiles/local`.

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
    let usage = mast_engine::token_usage::TokenUsage::open(copy.path())?;
    let now = chrono::Utc::now().timestamp_millis();
    let snapshot = usage.snapshot(&TokenUsageQuery {
        since: days.map(|d| now - d * 86_400_000),
        ..Default::default()
    })?;
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
