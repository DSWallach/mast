//! Validated, data-driven planning sheets shared by the engine and UI.
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionSheet {
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub models: Vec<Model>,
    #[serde(default)]
    pub endpoints: Vec<Endpoint>,
    #[serde(default)]
    pub rules: Vec<Rules>,
    #[serde(default)]
    pub validations: Vec<Validation>,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    #[serde(default)]
    pub flows: Vec<Flow>,
    #[serde(default)]
    pub ui_states: Vec<UiState>,
    #[serde(default)]
    pub out_of_scope: Vec<OutOfScope>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub state: String,
    pub date: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub status: Status,
    pub fields: Vec<Field>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub id: String,
    pub name: String,
    pub marker: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub null: bool,
    pub default: Option<String>,
    pub constraint: Option<String>,
    pub status: Status,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub request: Vec<DiffLine>,
    pub response: Vec<DiffLine>,
    pub status: Status,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffLine {
    pub id: String,
    pub marker: String,
    pub text: String,
    pub status: Status,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub id: String,
    pub columns: Vec<String>,
    pub rows: Vec<RuleRow>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleRow {
    pub id: String,
    pub state: String,
    pub cells: Vec<String>,
    pub status: Status,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Validation {
    pub id: String,
    pub rule: String,
    pub enforced: Vec<String>,
    pub result: String,
    pub status: Status,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permission {
    pub id: String,
    pub subject: String,
    pub access: BTreeMap<String, String>,
    pub status: Status,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    pub id: String,
    pub title: String,
    pub steps: Vec<Step>,
    pub status: Status,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub params: Vec<String>,
    pub status: Status,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiState {
    pub id: String,
    pub label: String,
    pub image: Option<String>,
    pub status: Status,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutOfScope {
    pub id: String,
    pub label: String,
    pub status: Status,
}

impl DecisionSheet {
    pub fn parse(source: &str) -> Result<Self, String> {
        let sheet: Self = serde_json::from_str(source).map_err(|e| e.to_string())?;
        if sheet.kind != "decision-sheet" {
            return Err("kind must be decision-sheet".into());
        }
        if sheet.title.trim().is_empty() {
            return Err("title must not be empty".into());
        }
        let mut ids = HashSet::new();
        fn row(ids: &mut HashSet<String>, id: &str, status: &Status) -> Result<(), String> {
            if id.trim().is_empty() {
                return Err("row id must not be empty".into());
            }
            if !ids.insert(id.to_owned()) {
                return Err(format!("duplicate id {id:?}"));
            }
            match status.state.as_str() {
                "open" if status.date.is_none() => {}
                "decided" | "rejected"
                    if status.date.as_ref().is_some_and(|d| {
                        chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok() && d.len() == 10
                    }) => {}
                _ => return Err(format!("invalid status or date for {id:?}")),
            }
            Ok(())
        }
        for m in &sheet.models {
            row(&mut ids, &m.id, &m.status)?;
            for f in &m.fields {
                row(&mut ids, &f.id, &f.status)?;
                if !["+", "~", "−", ""].contains(&f.marker.as_str()) {
                    return Err(format!("invalid marker for {:?}", f.id));
                }
            }
        }
        for e in &sheet.endpoints {
            row(&mut ids, &e.id, &e.status)?;
            for l in e.request.iter().chain(&e.response) {
                row(&mut ids, &l.id, &l.status)?;
                if !["+", "−", ""].contains(&l.marker.as_str()) {
                    return Err(format!("invalid marker for {:?}", l.id));
                }
            }
        }
        for r in &sheet.rules {
            if r.id.trim().is_empty() || !ids.insert(r.id.clone()) {
                return Err(format!("missing or duplicate id {:?}", r.id));
            }
            for x in &r.rows {
                row(&mut ids, &x.id, &x.status)?;
                if x.cells.len() != r.columns.len()
                    || x.cells
                        .iter()
                        .any(|c| !["✓", "—", "?"].contains(&c.as_str()))
                {
                    return Err(format!("invalid cells for {:?}", x.id));
                }
            }
        }
        for v in &sheet.validations {
            row(&mut ids, &v.id, &v.status)?;
        }
        for p in &sheet.permissions {
            row(&mut ids, &p.id, &p.status)?;
        }
        for f in &sheet.flows {
            row(&mut ids, &f.id, &f.status)?;
            for s in &f.steps {
                row(&mut ids, &s.id, &s.status)?;
                if !["ui", "api", "db", "event", "effect", "none"].contains(&s.kind.as_str()) {
                    return Err(format!("invalid flow kind for {:?}", s.id));
                }
            }
        }
        for s in &sheet.ui_states {
            row(&mut ids, &s.id, &s.status)?;
            if s.image.as_ref().is_some_and(|image| {
                !image.starts_with("mast-artifact:") && !image.starts_with("zeron-artifact:")
            }) {
                return Err(format!(
                    "image for {:?} must be an artifact reference",
                    s.id
                ));
            }
        }
        for s in &sheet.out_of_scope {
            row(&mut ids, &s.id, &s.status)?;
        }
        Ok(sheet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unknown_keys_and_invalid_rows() {
        assert!(
            DecisionSheet::parse(r#"{"kind":"decision-sheet","title":"T","extra":1}"#)
                .unwrap_err()
                .contains("unknown field")
        );
        assert!(DecisionSheet::parse(r#"{"kind":"decision-sheet","title":"T","ui_states":[{"label":"x","image":null,"status":{"state":"open","date":null}}]}"#).unwrap_err().contains("missing field `id`"));
        assert!(DecisionSheet::parse(r#"{"kind":"decision-sheet","title":"T","ui_states":[{"id":"x","label":"x","image":null,"status":{"state":"maybe","date":null}}]}"#).unwrap_err().contains("invalid status"));
        assert!(DecisionSheet::parse(r#"{"kind":"decision-sheet","title":"T","ui_states":[{"id":"x","label":"x","image":null,"status":{"state":"decided","date":"2026-02-30"}}]}"#).is_err());
    }
}
