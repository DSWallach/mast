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

pub fn comparison(previous: &Report, current: &Report) -> String {
    let mut out = format!(
        "# Mast metrics comparison\n\nPrevious: {} to {}. Current: {} to {}. Deltas are current minus previous; check window lengths and coverage before comparing.\n\n",
        previous.since, previous.until, current.since, current.until
    );
    out.push_str("| Metric | Previous | Current | Delta |\n| --- | ---: | ---: | ---: |\n");
    let mut row = |label: &str, old: Option<i64>, new: Option<i64>| {
        let delta = old
            .zip(new)
            .map(|(a, b)| format!("{:+}", b - a))
            .unwrap_or_else(|| "—".into());
        writeln!(
            out,
            "| {label} | {} | {} | {delta} |",
            value(old),
            value(new)
        )
        .unwrap();
    };
    let a = &previous.totals;
    let b = &current.totals;
    row(
        "Required Actions",
        Some(request_count(a) as i64),
        Some(request_count(b) as i64),
    );
    row(
        "Prose questions without card",
        Some(a.prose_questions_without_card as i64),
        Some(b.prose_questions_without_card as i64),
    );
    row(
        "Status checks",
        Some(a.status_questions_by_day.values().sum::<u64>() as i64),
        Some(b.status_questions_by_day.values().sum::<u64>() as i64),
    );
    row(
        "Auto-continue messages",
        Some(a.auto_continue_messages as i64),
        Some(b.auto_continue_messages as i64),
    );
    row(
        "Worker-stopped wakes",
        Some(a.worker_stopped_wakes as i64),
        Some(b.worker_stopped_wakes as i64),
    );
    for role in ["coordinator", "worker", "plain"] {
        let key = format!("role:{role}");
        let old = previous.groups.get(&key).and_then(|g| g.reply_words.median);
        let new = current.groups.get(&key).and_then(|g| g.reply_words.median);
        row(&format!("{role} reply median words"), old, new);
    }
    out
}
