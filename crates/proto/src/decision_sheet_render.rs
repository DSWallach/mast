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
    }
    for r in &sheet.rules {
        statuses.extend(r.rows.iter().map(|x| &x.status));
    }
    statuses.extend(sheet.validations.iter().map(|x| &x.status));
    statuses.extend(sheet.permissions.iter().map(|x| &x.status));
    for f in &sheet.flows {
        statuses.push(&f.status);
        statuses.extend(f.steps.iter().map(|x| &x.status));
    }
    statuses.extend(sheet.ui_states.iter().map(|x| &x.status));
    statuses.extend(sheet.out_of_scope.iter().map(|x| &x.status));
    write!(out, "<div class=\"mk-chips\"><span class=\"mk-chip mk-added\">{} models</span><span class=\"mk-chip mk-added\">{} fields</span><span class=\"mk-chip mk-changed\">{} API shapes</span><span class=\"mk-chip mk-removed\">{} rejected</span><span class=\"mk-chip mk-open\">{} open</span></div>", sheet.models.len(), sheet.models.iter().map(|m| m.fields.len()).sum::<usize>(), sheet.endpoints.len(), statuses.iter().filter(|s| s.state == "rejected").count(), statuses.iter().filter(|s| s.state == "open").count()).unwrap();
    out.push_str("<div class=\"mk-legend\"><span><b class=\"mk-yes\">+</b> new</span><span><b>~</b> changed</span><span><b class=\"mk-no\">−</b> removed / rejected</span><span><span class=\"mk-chip mk-decided\">✓</span> decided</span><span><span class=\"mk-chip mk-open\">?</span> open</span></div>");
    if !sheet.flows.is_empty() {
        section(&mut out, "Flows · what each action touches");
        for f in &sheet.flows {
            write!(out, "<div class=\"mk-flow\"><div class=\"mk-card-header\">{} {}</div><div class=\"mk-lane\">", esc(&f.title), status(&f.status)).unwrap();
            for (i, s) in f.steps.iter().enumerate() {
                if i > 0 {
                    out.push_str("<div class=\"mk-arrow\">→</div>");
                }
                write!(out, "<div class=\"mk-step mk-{}\"><div class=\"mk-step-kind\">{}</div>{}<div class=\"mk-mono\">{}</div>{}</div>", s.kind, esc(&s.kind.to_uppercase()), esc(&s.label), s.params.iter().map(|x| esc(x)).collect::<Vec<_>>().join("<br>"), status(&s.status)).unwrap();
            }
            out.push_str("</div></div>");
        }
    }
    if !sheet.models.is_empty() {
        section(&mut out, "Data model");
        out.push_str("<div class=\"mk-grid\">");
        for m in &sheet.models {
            write!(out,"<div class=\"mk-card\"><div class=\"mk-card-header\">{} {}</div><table class=\"mk-table\"><tr><th></th><th>field</th><th>type</th><th>null / default</th><th>constraint</th><th></th></tr>",esc(&m.name),status(&m.status)).unwrap();
            for f in &m.fields {
                write!(out,"<tr><td class=\"mk-marker {}\">{}</td><td class=\"mk-mono\">{}</td><td class=\"mk-mono\">{}</td><td>{}{}</td><td>{}</td><td>{}</td></tr>",marker(&f.marker),esc(&f.marker),esc(&f.name),esc(&f.field_type),if f.null { "nullable" } else { "required" }, f.default.as_ref().map(|x| format!(" / {}",esc(x))).unwrap_or_default(),f.constraint.as_deref().map(esc).unwrap_or_else(|| "—".into()),status(&f.status)).unwrap();
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
                    status(&l.status)
                )
                .unwrap();
            }
            out.push_str("</div></div>");
        }
        out.push_str("</div>");
    }
    if !sheet.rules.is_empty() {
        section(&mut out, "Rules");
        for r in &sheet.rules {
            out.push_str("<div class=\"mk-card\"><table class=\"mk-table\"><tr><th>state</th>");
            for c in &r.columns {
                write!(out, "<th>{}</th>", esc(c)).unwrap();
            }
            out.push_str("<th></th></tr>");
            for row in &r.rows {
                write!(out, "<tr><td>{}</td>", esc(&row.state)).unwrap();
                for c in &row.cells {
                    let class = match c.as_str() {
                        "✓" => "mk-yes",
                        "—" => "mk-no",
                        _ => "mk-question",
                    };
                    write!(out, "<td class=\"{class}\">{}</td>", esc(c)).unwrap();
                }
                write!(out, "<td>{}</td></tr>", status(&row.status)).unwrap();
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
                status(&v.status)
            )
            .unwrap();
        }
        out.push_str("</table></div>");
    }
    if !sheet.permissions.is_empty() {
        section(&mut out, "Permissions");
        out.push_str("<div class=\"mk-card\"><table class=\"mk-table\"><tr><th>subject</th><th>access</th><th></th></tr>");
        for p in &sheet.permissions {
            write!(
                out,
                "<tr><td class=\"mk-mono\">{}</td><td>{}</td><td>{}</td></tr>",
                esc(&p.subject),
                p.access
                    .iter()
                    .map(|(k, v)| format!("{}: {}", esc(k), esc(v)))
                    .collect::<Vec<_>>()
                    .join(" · "),
                status(&p.status)
            )
            .unwrap();
        }
        out.push_str("</table></div>");
    }
    if !sheet.ui_states.is_empty() {
        section(&mut out, "UI states");
        out.push_str("<div class=\"mk-screenshots\">");
        for s in &sheet.ui_states {
            out.push_str("<div class=\"mk-screenshot\"><div class=\"mk-screenshot-placeholder\">screenshot</div>");
            write!(
                out,
                "<div class=\"mk-caption\">{} {}</div></div>",
                esc(&s.label),
                status(&s.status)
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
                status(&s.status)
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
    fn sample_render_and_counts() {
        let sheet = DecisionSheet::parse(include_str!(
            "../../../docs/artifact-kit/decision-sheet-example.json"
        ))
        .unwrap();
        let html = render(&sheet);
        assert_eq!(
            html,
            include_str!("../tests/snapshots/decision-sheet.fragment.html")
        );
        assert!(html.contains("7 open"));
        assert!(html.contains("3 rejected"));
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
        if let Ok(path) = std::env::var("MAST_DECISION_SHEET_RENDER_PATH") {
            std::fs::write(path, html).unwrap();
        }
    }
}
