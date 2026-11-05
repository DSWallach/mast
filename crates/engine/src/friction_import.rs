//! Parsing for Markdown friction logs.

use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use mast_proto::{FrictionApproval, FrictionKind, FrictionSeverity};

#[derive(Debug, PartialEq, Eq)]
pub struct ImportedFriction {
    pub summary: String,
    pub what_happened: String,
    pub workaround: Option<String>,
    pub suggestion: Option<String>,
    pub approval: Option<FrictionApproval>,
    pub kind: FrictionKind,
    pub severity: Option<FrictionSeverity>,
    pub created_at: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SkippedFriction {
    pub line: usize,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct FrictionParseResult {
    pub reports: Vec<ImportedFriction>,
    pub skipped: Vec<SkippedFriction>,
}

fn optional(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && !["none", "n/a"].contains(&value.to_ascii_lowercase().as_str()))
        .then(|| value.to_owned())
}

fn parse_entry(header: &str, body: &str) -> Result<ImportedFriction, String> {
    let (date, rest) = header.split_once(' ').ok_or("missing date or summary")?;
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| "invalid date")?;
    let (date_time, summary) = match rest.split_once(' ') {
        Some((time, summary)) if time.len() == 5 && time.as_bytes().get(2) == Some(&b':') => {
            let date_time =
                NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M")
                    .map_err(|_| "invalid time")?;
            (date_time, summary.trim())
        }
        _ => (date.and_hms_opt(0, 0, 0).unwrap(), rest.trim()),
    };
    if summary.is_empty() || summary.chars().count() > 140 {
        return Err("summary must contain 1 to 140 characters".into());
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if let Some((name, value)) = line
            .strip_prefix("- ")
            .and_then(|line| line.split_once(':'))
        {
            fields.push((
                name.trim_matches('*').trim().to_ascii_lowercase(),
                value.trim().to_owned(),
            ));
        } else if let Some((_, value)) = fields.last_mut() {
            if !line.is_empty() {
                value.push('\n');
                value.push_str(line);
            }
        } else if !line.is_empty() {
            fields.push((String::new(), line.to_owned()));
        }
    }
    let mut happened = Vec::new();
    let mut workaround = None;
    let mut suggestion = None;
    let mut command = None;
    let mut why = None;
    let mut kind = FrictionKind::Workflow;
    let mut severity = None;
    for (name, value) in fields {
        match name.as_str() {
            "friction" | "what happened" => {
                if let Some(value) = optional(&value) {
                    happened.push(value);
                }
            }
            "workaround" | "workaround used" => workaround = optional(&value),
            "suggested improvement" | "suggested rule/tooling improvement" | "suggestion" => {
                suggestion = optional(&value)
            }
            "approval requested" => command = optional(&value),
            "why" | "why approval was needed" => why = optional(&value),
            "kind" => {
                kind = match value.trim().to_ascii_lowercase().as_str() {
                    "bug" => FrictionKind::Bug,
                    "tool_failure" | "tool failure" => FrictionKind::ToolFailure,
                    "permission" => FrictionKind::Permission,
                    "environment" => FrictionKind::Environment,
                    "workflow" => FrictionKind::Workflow,
                    "docs" => FrictionKind::Docs,
                    _ => {
                        happened.push(format!("Kind: {value}"));
                        FrictionKind::Workflow
                    }
                }
            }
            "severity" => {
                severity = match value.trim().to_ascii_lowercase().as_str() {
                    "low" => Some(FrictionSeverity::Low),
                    "medium" => Some(FrictionSeverity::Medium),
                    "high" => Some(FrictionSeverity::High),
                    _ => {
                        happened.push(format!("Severity: {value}"));
                        None
                    }
                }
            }
            _ => {
                if !value.is_empty() {
                    happened.push(if name.is_empty() {
                        value
                    } else {
                        format!("{name}: {value}")
                    });
                }
            }
        }
    }
    if happened.is_empty() {
        return Err("missing friction details".into());
    }
    let clean = |s: &str| mast_harness::redact::redact_secrets(s);
    Ok(ImportedFriction {
        summary: clean(summary),
        what_happened: clean(&happened.join("\n")),
        workaround: workaround.map(|v| clean(&v)),
        suggestion: suggestion.map(|v| clean(&v)),
        approval: command.map(|command| FrictionApproval {
            command: clean(&command),
            why: clean(why.as_deref().unwrap_or("")),
            answer: None,
        }),
        kind,
        severity,
        created_at: Utc.from_utc_datetime(&date_time).timestamp_millis(),
    })
}

/// Parse dated level-two entries and retain a line number for each rejected entry.
pub fn parse_friction_markdown(markdown: &str) -> FrictionParseResult {
    let mut result = FrictionParseResult::default();
    let mut current: Option<(usize, String, String)> = None;
    let finish =
        |entry: (usize, String, String), result: &mut FrictionParseResult| match parse_entry(
            &entry.1, &entry.2,
        ) {
            Ok(report) => result.reports.push(report),
            Err(reason) => result.skipped.push(SkippedFriction {
                line: entry.0,
                reason,
            }),
        };
    for (index, line) in markdown.lines().enumerate() {
        if let Some(header) = line.strip_prefix("## ") {
            if let Some(entry) = current.take() {
                finish(entry, &mut result);
            }
            current = Some((index + 1, header.to_owned(), String::new()));
        } else if let Some((_, _, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some(entry) = current {
        finish(entry, &mut result);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_entries_and_skips_malformed_entries() {
        let result = parse_friction_markdown(include_str!("../tests/fixtures/friction-log.md"));
        assert_eq!(result.reports.len(), 2);
        assert_eq!(result.skipped.len(), 2);
        let report = &result.reports[0];
        assert_eq!(report.summary, "Build approval");
        assert_eq!(
            report.what_happened,
            "Build was blocked by a missing permission.\nadditional context: See the local build instructions."
        );
        assert_eq!(report.workaround.as_deref(), Some("Retry after approval"));
        assert_eq!(report.approval.as_ref().unwrap().command, "cargo build");
        assert_eq!(result.reports[1].kind, FrictionKind::Docs);
        assert_eq!(result.reports[1].severity, Some(FrictionSeverity::Low));
        assert!(!result.reports[1].what_happened.contains("synthetic-secret"));
        assert_eq!(result.skipped[0].reason, "invalid date");
    }
}
