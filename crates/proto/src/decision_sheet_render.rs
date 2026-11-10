//! HTML rendering for validated decision sheets.
use crate::decision_sheet::{DecisionSheet, Status};
use std::fmt::Write;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn status(s: &Status) -> String {
    let (class, label) = match s.state.as_str() {
        "decided" => ("mk-decided", "✓"),
        "rejected" => ("mk-removed", "✗"),
        _ => ("mk-open", "?"),
    };
    format!(
        "<span class=\"mk-chip {class}\">{label}{}</span>",
        s.date
            .as_deref()
            .map(|d| format!(" {}", esc(d)))
            .unwrap_or_default()
    )
}
fn compact_status(s: &Status) -> String {
    let (class, label) = match s.state.as_str() {
        "decided" => ("mk-decided", "✓"),
        "rejected" => ("mk-removed", "✗"),
        _ => ("mk-open", "?"),
    };
    format!("<span class=\"mk-chip {class}\">{label}</span>")
}
fn child_status(child: &Status, parent: &Status) -> String {
    if child.state == parent.state {
        String::new()
    } else {
        compact_status(child)
    }
}
fn count_label(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}
fn section(out: &mut String, name: &str) {
    write!(out, "<h2 class=\"mk-section\">{}</h2>", esc(name)).unwrap();
}
fn marker(m: &str) -> &'static str {
    match m {
        "+" => "mk-added",
        "~" => "mk-changed",
        "−" => "mk-removed",
        _ => "mk-context",
    }
}

pub fn render(sheet: &DecisionSheet) -> String {
    let mut out = format!("<div class=\"mk-sheet\"><h1>{}</h1>", esc(&sheet.title));
    let mut statuses: Vec<&Status> = Vec::new();
    for m in &sheet.models {
        statuses.push(&m.status);
        statuses.extend(m.fields.iter().map(|f| &f.status));
    }
    for e in &sheet.endpoints {
        statuses.push(&e.status);
        statuses.extend(e.request.iter().chain(&e.response).map(|l| &l.status));
        statuses.extend(e.callers.iter().map(|c| &c.status));
    }
    for r in &sheet.rules {
        statuses.extend(r.rows.iter().map(|x| &x.status));
    }
    statuses.extend(sheet.validations.iter().map(|x| &x.status));
    statuses.extend(sheet.permissions.iter().map(|x| &x.status));
    for f in &sheet.flows {
        statuses.push(&f.status);
        statuses.extend(f.steps.iter().map(|x| &x.status));
        for step in &f.steps {
            statuses.extend(step.parallel.iter().map(|x| &x.status));
        }
    }
    statuses.extend(sheet.ui_states.iter().map(|x| &x.status));
    statuses.extend(sheet.out_of_scope.iter().map(|x| &x.status));
    write!(out, "<div class=\"mk-chips\"><span class=\"mk-chip mk-added\">{}</span><span class=\"mk-chip mk-added\">{}</span><span class=\"mk-chip mk-changed\">{}</span><span class=\"mk-chip mk-removed\">{} rejected</span><span class=\"mk-chip mk-open\">{} open</span></div>", count_label(sheet.models.len(), "model", "models"), count_label(sheet.models.iter().map(|m| m.fields.len()).sum::<usize>(), "field", "fields"), count_label(sheet.endpoints.len(), "API shape", "API shapes"), statuses.iter().filter(|s| s.state == "rejected").count(), statuses.iter().filter(|s| s.state == "open").count()).unwrap();
    out.push_str("<div class=\"mk-legend\"><span><b class=\"mk-yes\">+</b> new</span><span><b>~</b> changed</span><span><b class=\"mk-no\">−</b> removed / rejected</span><span><span class=\"mk-chip mk-decided\">✓</span> decided</span><span><span class=\"mk-chip mk-open\">?</span> open</span></div>");
    if !sheet.flows.is_empty() {
        section(&mut out, "Flows · what each action touches");
        out.push_str("<div class=\"mk-legend\"><span class=\"mk-step mk-ui\">UI</span><span class=\"mk-step mk-api\">API</span><span class=\"mk-step mk-db\">DB write</span><span class=\"mk-step mk-event\">event</span><span class=\"mk-step mk-effect\">side effect</span><span class=\"mk-step mk-none\">does not happen</span></div>");
        for f in &sheet.flows {
            let lane = if f.steps.len() >= 4 {
                "mk-lane mk-lane-dense"
            } else {
                "mk-lane"
            };
            write!(out, "<div class=\"mk-flow\"><div class=\"mk-card-header\">{} {}</div><div class=\"{}\">", esc(&f.title), status(&f.status), lane).unwrap();
            for (i, s) in f.steps.iter().enumerate() {
                if i > 0 {
                    out.push_str("<div class=\"mk-arrow\">→</div>");
                }
                if !s.parallel.is_empty() {
                    out.push_str("<div class=\"mk-fan\">");
                }
                write!(out, "<div class=\"mk-step mk-{}\"><div class=\"mk-step-kind\">{}</div>{}<div class=\"mk-mono\">{}</div>{}</div>", s.kind, esc(&s.kind.to_uppercase()), esc(&s.label), s.params.iter().map(|x| esc(x)).collect::<Vec<_>>().join("<br>"), child_status(&s.status, &f.status)).unwrap();
                for p in &s.parallel {
                    write!(out, "<div class=\"mk-step mk-{}\"><div class=\"mk-step-kind\">{}</div>{}<div class=\"mk-mono\">{}</div>{}</div>", p.kind, esc(&p.kind.to_uppercase()), esc(&p.label), p.params.iter().map(|x| esc(x)).collect::<Vec<_>>().join("<br>"), child_status(&p.status, &f.status)).unwrap();
                }
                if !s.parallel.is_empty() {
                    out.push_str("</div>");
                }
            }
            out.push_str("</div></div>");
        }
    }
    if !sheet.models.is_empty() {
        section(&mut out, "Data model");
        out.push_str("<div class=\"mk-grid\">");
        for m in &sheet.models {
            write!(out,"<div class=\"mk-card\"><div class=\"mk-card-header\">{} {}</div><table class=\"mk-table\"><tr><th></th><th>field</th><th>type</th><th>null / default</th><th class=\"mk-notes\">constraint</th><th></th></tr>",esc(&m.name),status(&m.status)).unwrap();
            for f in &m.fields {
                write!(out,"<tr><td class=\"mk-marker {}\">{}</td><td class=\"mk-mono\">{}</td><td class=\"mk-mono\">{}</td><td>{}{}</td><td class=\"mk-notes\">{}</td><td>{}</td></tr>",marker(&f.marker),esc(&f.marker),esc(&f.name),esc(&f.field_type),if f.null { "nullable" } else { "required" }, f.default.as_ref().map(|x| format!(" / {}",esc(x))).unwrap_or_default(),f.constraint.as_deref().map(esc).unwrap_or_else(|| "—".into()),compact_status(&f.status)).unwrap();
            }
            out.push_str("</table></div>");
        }
        out.push_str("</div>");
    }
    for e in &sheet.endpoints {
        section(&mut out, &format!("API contract · {}", e.name));
        write!(out,"<div class=\"mk-grid\"><div class=\"mk-card\"><div class=\"mk-card-header\">{} {}</div></div>",esc(&e.name),status(&e.status)).unwrap();
        for (name, lines) in [("request", &e.request), ("response", &e.response)] {
            write!(out,"<div class=\"mk-card\"><div class=\"mk-card-header\">{name}</div><div class=\"mk-diff\">").unwrap();
            for l in lines {
                write!(
                    out,
                    "<div class=\"{}\">{} {} {}</div>",
                    marker(&l.marker),
                    esc(&l.marker),
                    esc(&l.text),
                    child_status(&l.status, &e.status)
                )
                .unwrap();
            }
            out.push_str("</div></div>");
        }
        if !e.callers.is_empty() {
            out.push_str("<div class=\"mk-card\"><div class=\"mk-card-header\">existing callers</div><table class=\"mk-table\">");
            for c in &e.callers {
                write!(out, "<tr><td class=\"mk-mono\">{}</td><td class=\"mk-notes\">{}</td><td>{}</td></tr>", esc(&c.name), esc(&c.effect), compact_status(&c.status)).unwrap();
            }
            out.push_str("</table></div>");
        }
        out.push_str("</div>");
    }
    if !sheet.rules.is_empty() {
        section(&mut out, "Rules");
        for r in &sheet.rules {
            out.push_str("<div class=\"mk-card\"><table class=\"mk-table\"><tr><th>state</th>");
            let has_action = r.rows.iter().any(|row| row.action.is_some());
            if has_action {
                out.push_str("<th>action</th>");
            }
            for c in &r.columns {
                write!(out, "<th>{}</th>", esc(c)).unwrap();
            }
            out.push_str("<th></th></tr>");
            for row in &r.rows {
                write!(out, "<tr><td>{}</td>", esc(&row.state)).unwrap();
                if has_action {
                    write!(
                        out,
                        "<td>{}</td>",
                        row.action.as_deref().map(esc).unwrap_or_else(|| "—".into())
                    )
                    .unwrap();
                }
                for c in &row.cells {
                    let class = match c.as_str() {
                        "✓" => "mk-yes",
                        "—" => "mk-no",
                        _ => "mk-question",
                    };
                    write!(out, "<td class=\"{class}\">{}</td>", esc(c)).unwrap();
                }
                write!(out, "<td>{}</td></tr>", compact_status(&row.status)).unwrap();
            }
            out.push_str("</table></div>");
        }
    }
    if !sheet.validations.is_empty() {
        section(&mut out, "Validations");
        out.push_str("<div class=\"mk-card\"><table class=\"mk-table\"><tr><th>rule</th><th>enforced</th><th>result</th><th></th></tr>");
        for v in &sheet.validations {
            write!(out, "<tr><td>{}</td><td>", esc(&v.rule)).unwrap();
            for e in &v.enforced {
                write!(out, "<span class=\"mk-chip mk-neutral\">{}</span>", esc(e)).unwrap();
            }
            write!(
                out,
                "</td><td class=\"mk-mono\">{}</td><td>{}</td></tr>",
                esc(&v.result),
                compact_status(&v.status)
            )
            .unwrap();
        }
        out.push_str("</table></div>");
    }
    if !sheet.permissions.is_empty() {
        section(&mut out, "Permissions");
        let columns: std::collections::BTreeSet<&str> = sheet
            .permissions
            .iter()
            .flat_map(|p| p.access.keys().map(String::as_str))
            .collect();
        out.push_str("<div class=\"mk-card\"><table class=\"mk-table\"><tr><th>subject</th>");
        for column in &columns {
            write!(out, "<th>{}</th>", esc(column)).unwrap();
        }
        out.push_str("<th></th></tr>");
        for p in &sheet.permissions {
            write!(out, "<tr><td class=\"mk-mono\">{}</td>", esc(&p.subject)).unwrap();
            for column in &columns {
                write!(
                    out,
                    "<td>{}</td>",
                    p.access
                        .get(*column)
                        .map(|s| esc(s))
                        .unwrap_or_else(|| "—".into())
                )
                .unwrap();
            }
            write!(out, "<td>{}</td></tr>", compact_status(&p.status)).unwrap();
        }
        out.push_str("</table></div>");
    }
    if !sheet.ui_states.is_empty() {
        section(&mut out, "UI states");
        out.push_str("<div class=\"mk-screenshots\">");
        for s in &sheet.ui_states {
            out.push_str("<div class=\"mk-screenshot\"><div class=\"mk-screenshot-placeholder\">");
            if let Some(image) = &s.image {
                write!(out, "<a href=\"{}\">Open screenshot</a>", esc(image)).unwrap();
            } else {
                out.push_str("screenshot");
            }
            out.push_str("</div>");
            write!(
                out,
                "<div class=\"mk-caption\">{} {}</div></div>",
                esc(&s.label),
                compact_status(&s.status)
            )
            .unwrap();
        }
        out.push_str("</div>");
    }
    if !sheet.out_of_scope.is_empty() {
        section(&mut out, "Out of scope");
        out.push_str("<div class=\"mk-chips\">");
        for s in &sheet.out_of_scope {
            write!(
                out,
                "<span class=\"mk-chip mk-removed\">{}</span> {}",
                esc(&s.label),
                compact_status(&s.status)
            )
            .unwrap();
        }
        out.push_str("</div>");
    }
    out.push_str("</div>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_references_are_safe_links() {
        let source = r#"{"kind":"decision-sheet","title":"T","ui_states":[{"id":"screen","label":"Screen","image":"mast-artifact:abc","status":{"state":"open","date":null}}]}"#;
        let sheet = DecisionSheet::parse(source).unwrap();
        assert!(render(&sheet).contains("href=\"mast-artifact:abc\""));
        assert!(
            DecisionSheet::parse(&source.replace("mast-artifact:abc", "javascript:alert(1)"))
                .is_err()
        );
    }
    #[test]
    fn child_statuses_are_compact_and_only_show_changes() {
        let source = r#"{"kind":"decision-sheet","title":"T","flows":[{"id":"f","title":"Flow","status":{"state":"decided","date":"2026-11-09"},"steps":[{"id":"a","kind":"ui","label":"same","params":[],"status":{"state":"decided","date":"2026-11-09"}},{"id":"b","kind":"api","label":"open child","params":[],"status":{"state":"open","date":null}},{"id":"c","kind":"db","label":"rejected child","params":[],"status":{"state":"rejected","date":"2026-11-09"}}]}]}"#;
        let html = render(&DecisionSheet::parse(source).unwrap());
        assert_eq!(html.matches("2026-11-09").count(), 1);
        assert!(html.contains(
            "open child<div class=\"mk-mono\"></div><span class=\"mk-chip mk-open\">?</span>"
        ));
        assert!(html.contains(
            "rejected child<div class=\"mk-mono\"></div><span class=\"mk-chip mk-removed\">✗</span>"
        ));
        assert_eq!(count_label(1, "model", "models"), "1 model");
        assert_eq!(count_label(1, "field", "fields"), "1 field");
        assert_eq!(count_label(1, "API shape", "API shapes"), "1 API shape");
        assert_eq!(count_label(2, "model", "models"), "2 models");
    }
    #[test]
    fn decided_child_in_open_flow_is_visible() {
        let open = Status {
            state: "open".into(),
            date: None,
        };
        let decided = Status {
            state: "decided".into(),
            date: Some("2026-11-09".into()),
        };
        assert_eq!(
            child_status(&decided, &open),
            "<span class=\"mk-chip mk-decided\">✓</span>"
        );
        assert!(child_status(&decided, &decided).is_empty());
    }
    #[test]
    fn sample_render_and_counts() {
        let sheet = DecisionSheet::parse(include_str!(
            "../../../docs/artifact-kit/decision-sheet-example.json"
        ))
        .unwrap();
        let html = render(&sheet);
        if let Ok(path) = std::env::var("MAST_DECISION_SHEET_RENDER_PATH") {
            std::fs::write(path, &html).unwrap();
        }
        assert_eq!(
            html,
            include_str!("../tests/snapshots/decision-sheet.fragment.html")
        );
        assert!(html.contains("7 open"));
        assert!(html.contains("5 rejected"));
        for title in [
            "Flows",
            "Data model",
            "API contract",
            "Rules",
            "Validations",
            "Permissions",
            "UI states",
            "Out of scope",
        ] {
            assert!(html.contains(title), "{title}");
        }
        assert!(!html.contains("style=\""));
    }
}
