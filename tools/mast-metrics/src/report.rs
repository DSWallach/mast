use std::fmt::Write;

use crate::metrics::{Aggregate, Report};

fn value(value: Option<i64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_else(|| "—".into())
}

pub fn markdown(report: &Report) -> String {
    let mut out = format!(
        "# Mast metrics baseline\n\nWindow: {} to {} (UTC, end exclusive).\n\n",
        report.since, report.until
    );
    let totals = &report.totals;
    let requests: u64 = totals.requests_by_kind.values().sum();
    writeln!(out, "| Metric | Count |\n| --- | ---: |\n| Required Actions raised | {requests} |\n| Prose questions without a card | {} |\n| Cards per prose question | {} |\n| Auto-continue messages observed | {} |\n| Worker-stopped wakes observed | {} |", totals.prose_questions_without_card, totals.card_to_prose_ratio.map(|v| format!("{v:.2}")).unwrap_or_else(|| "—".into()), totals.auto_continue_messages, totals.worker_stopped_wakes).unwrap();
    out.push_str("\n| Request kind | Count |\n| --- | ---: |\n");
    for (kind, count) in &totals.requests_by_kind {
        writeln!(out, "| {kind} | {count} |").unwrap();
    }
    out.push_str("\n| Resolution category | Sample | Median hours | P90 hours |\n| --- | ---: | ---: | ---: |\n");
    for (kind, dist) in &totals.resolution_millis {
        let hours = |x: Option<i64>| {
            x.map(|v| format!("{:.2}", v as f64 / 3_600_000.0))
                .unwrap_or_else(|| "—".into())
        };
        writeln!(
            out,
            "| {kind} | {} | {} | {} |",
            dist.count,
            hours(dist.median),
            hours(dist.p90)
        )
        .unwrap();
    }
    out.push_str(
        "\n| Chat role | Replies | Median words | P90 words |\n| --- | ---: | ---: | ---: |\n",
    );
    for role in ["coordinator", "worker", "plain"] {
        if let Some(group) = report.groups.get(&format!("role:{role}")) {
            let d = &group.reply_words;
            writeln!(
                out,
                "| {role} | {} | {} | {} |",
                d.count,
                value(d.median),
                value(d.p90)
            )
            .unwrap();
        }
    }
    out.push_str("\n| Status checks by day (UTC) | Count |\n| --- | ---: |\n");
    for (day, count) in &totals.status_questions_by_day {
        writeln!(out, "| {day} | {count} |").unwrap();
    }
    out.push_str("\n## Coverage\n\n");
    for note in &report.coverage {
        writeln!(out, "- {note}").unwrap();
    }
    out.push_str("\nSpace, project and role aggregates are in the JSON report.\n");
    out
}

pub fn request_count(a: &Aggregate) -> u64 {
    a.requests_by_kind.values().sum()
}
