//! Opt-in projection and a default size cap for `mast_manage` replies.
//!
//! Watch snapshots (the work outline, chats, scheduled tasks) can run to
//! megabytes, and every later model call resends them. `view` selects part of
//! a reply; a reply that is still over the cap comes back as a short shape
//! summary that says how to ask for less. Small replies are unchanged.
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Largest reply returned by default.
const DEFAULT_MAX_BYTES: usize = 64 * 1024;
/// Largest `maxBytes` a caller may ask for.
const HARD_MAX_BYTES: usize = 512 * 1024;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ManagementView {
    /// Dot path into the reply. A segment is an object key, an array index, or
    /// the `id` of an array element (e.g. `items.ITEM_ID`).
    path: Option<String>,
    /// When the selected value is an array: keep elements whose top-level
    /// fields equal these values (e.g. `{"taskId":"T"}`).
    #[serde(rename = "match")]
    matches: Option<Map<String, Value>>,
    /// Keep only these top-level keys of the selected object or of each element.
    fields: Option<Vec<String>>,
    offset: Option<usize>,
    limit: Option<usize>,
    max_bytes: Option<usize>,
    /// Lift the size cap: the whole selected value, however large.
    #[serde(default)]
    full: bool,
}

fn step<'a>(value: &'a Value, segment: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map.get(segment),
        Value::Array(items) => segment
            .parse::<usize>()
            .ok()
            .and_then(|i| items.get(i))
            .or_else(|| {
                items
                    .iter()
                    .find(|item| item.get("id").and_then(Value::as_str) == Some(segment))
            }),
        _ => None,
    }
}

fn pick(value: &Value, fields: &[String]) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| fields.contains(key))
                .map(|(key, v)| (key.clone(), v.clone()))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn shape(value: &Value) -> Value {
    let describe = |v: &Value| match v {
        Value::Array(items) => json!({"array": items.len()}),
        Value::Object(map) => json!({"object": map.len()}),
        other => json!({"bytes": other.to_string().len()}),
    };
    match value {
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), describe(v))).collect())
        }
        other => describe(other),
    }
}

impl ManagementView {
    pub(super) fn apply(self, reply: Value) -> Value {
        let cap = self
            .max_bytes
            .unwrap_or(DEFAULT_MAX_BYTES)
            .min(HARD_MAX_BYTES);
        let requested = self.path.is_some()
            || self.matches.is_some()
            || self.fields.is_some()
            || self.offset.is_some()
            || self.limit.is_some();
        let mut selected = &reply;
        if let Some(path) = &self.path {
            for segment in path.split('.').filter(|s| !s.is_empty()) {
                match step(selected, segment) {
                    Some(next) => selected = next,
                    None => {
                        return json!({
                            "error": format!("view.path segment {segment:?} not found"),
                            "shape": shape(selected),
                        });
                    }
                }
            }
        }
        let projected = match selected {
            Value::Array(items) => {
                let kept: Vec<&Value> = items
                    .iter()
                    .filter(|item| {
                        self.matches.as_ref().is_none_or(|m| {
                            m.iter().all(|(key, want)| item.get(key) == Some(want))
                        })
                    })
                    .collect();
                let total = kept.len();
                let offset = self.offset.unwrap_or(0).min(total);
                let limit = self.limit.unwrap_or(usize::MAX);
                let page: Vec<Value> = kept
                    .into_iter()
                    .skip(offset)
                    .take(limit)
                    .map(|item| match &self.fields {
                        Some(fields) => pick(item, fields),
                        None => item.clone(),
                    })
                    .collect();
                let end = offset + page.len();
                let mut out = json!({"total": total, "offset": offset, "items": page});
                if end < total {
                    out["nextOffset"] = json!(end);
                }
                out
            }
            other => match &self.fields {
                Some(fields) => pick(other, fields),
                None => other.clone(),
            },
        };
        let bytes = projected.to_string().len();
        if bytes <= cap || self.full {
            return if requested { projected } else { reply };
        }
        // Name the largest array and a page size that fits, so one retry works.
        let page = |items: &Vec<Value>| {
            let each = (items.iter().map(|i| i.to_string().len()).sum::<usize>() / items.len().max(1)).max(1);
            (cap * 3 / 4 / each).max(1)
        };
        let mut retry = vec![];
        let mut hint = format!("Reply is {bytes} bytes, over the {cap} byte cap. ");
        // `view` JSON for a retry that pages an array at `path`, keeping the
        // caller's match, fields and starting offset when they applied to it.
        let paged = |path: Option<String>, limit: usize, carry: bool| {
            let mut view = Map::new();
            if let Some(path) = path {
                view.insert("path".into(), json!(path));
            }
            if carry {
                if let Some(m) = &self.matches {
                    view.insert("match".into(), Value::Object(m.clone()));
                }
                if let Some(f) = &self.fields {
                    view.insert("fields".into(), json!(f));
                }
            }
            view.insert("limit".into(), json!(limit));
            view.insert("offset".into(), json!(if carry { self.offset.unwrap_or(0) } else { 0 }));
            Value::Object(view)
        };
        if let Some(items) = selected.as_array() {
            // The caller already selected an array: page that same path.
            let limit = page(items);
            let view = paged(self.path.clone(), limit, true);
            hint += &format!("Page it with view: {view} (match, fields and offset kept) and follow nextOffset until it is absent. ");
            retry.push(json!({"view": view}));
        } else if let Some((key, items)) = selected.as_object().and_then(|map| {
            map.iter()
                .filter_map(|(key, v)| v.as_array().map(|items| (key, items)))
                .max_by_key(|(_, items)| items.iter().map(|i| i.to_string().len()).sum::<usize>())
        }) {
            let prefix = self.path.as_deref().map(|p| format!("{p}.")).unwrap_or_default();
            let view = paged(Some(format!("{prefix}{key}")), page(items), false);
            hint += &format!("Page the {key} array with view: {view} and follow nextOffset until it is absent. ");
            retry.push(json!({"view": view}));
        }
        retry.push(json!({"view": {"full": true}}));
        hint += "Narrow with view.match (equality on element fields) or view.fields (keys to keep), or pass view: {\"full\":true} to get everything uncapped.";
        json!({
            "truncated": true,
            "bytes": bytes,
            "maxBytes": cap,
            "shape": shape(&projected),
            "hint": hint,
            "retry": retry,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(v: Value) -> ManagementView {
        serde_json::from_value(v).unwrap()
    }

    fn snapshot() -> Value {
        let runs: Vec<Value> = (0..50)
            .map(|i| json!({"id": format!("r{i}"), "taskId": if i % 2 == 0 {"a"} else {"b"}, "detail": "x".repeat(5000)}))
            .collect();
        json!({"tasks": [{"id": "a", "name": "A"}, {"id": "b", "name": "B"}], "runs": runs})
    }

    #[test]
    fn small_replies_pass_through_unchanged() {
        let reply = json!({"tasks": [{"id": "a"}]});
        assert_eq!(ManagementView::default().apply(reply.clone()), reply);
    }

    #[test]
    fn oversized_replies_become_a_shape_summary_under_the_cap() {
        let out = ManagementView::default().apply(snapshot());
        assert_eq!(out["truncated"], true);
        assert_eq!(out["shape"]["runs"]["array"], 50);
        assert!(out.to_string().len() < 2048);
    }

    #[test]
    fn one_tasks_latest_runs_fit_under_10kb() {
        let out = view(json!({"path":"runs","match":{"taskId":"a"},"fields":["id","taskId"],"limit":3}))
            .apply(snapshot());
        assert_eq!(out["total"], 25);
        assert_eq!(out["items"].as_array().unwrap().len(), 3);
        assert_eq!(out["nextOffset"], 3);
        assert!(out.to_string().len() < 10 * 1024);
    }

    #[test]
    fn path_selects_an_element_by_id_and_fields_trim_it() {
        let out = view(json!({"path":"tasks.b","fields":["name"]})).apply(snapshot());
        assert_eq!(out, json!({"name": "B"}));
        let missing = view(json!({"path":"tasks.zzz"})).apply(snapshot());
        assert!(missing["error"].is_string());
    }

    #[test]
    fn the_envelope_names_a_retry_that_fits_and_paging_reaches_every_row() {
        let reply = snapshot();
        let envelope = ManagementView::default().apply(reply.clone());
        let retry = envelope["retry"][0]["view"].clone();
        assert_eq!(retry["path"], "runs");
        assert!(envelope["hint"].as_str().unwrap().contains("\"path\":\"runs\""));
        assert_eq!(envelope["retry"][1]["view"]["full"], true);
        // Follow the suggested view, then nextOffset, until it ends.
        let mut seen = vec![];
        let mut view = retry;
        loop {
            let page = serde_json::from_value::<ManagementView>(view.clone()).unwrap().apply(reply.clone());
            assert!(page.get("truncated").is_none(), "{page}");
            seen.extend(page["items"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_owned()));
            match page.get("nextOffset") {
                Some(next) => view["offset"] = next.clone(),
                None => break,
            }
        }
        let expected: Vec<String> = (0..50).map(|i| format!("r{i}")).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn a_retry_after_selecting_an_array_pages_that_path_and_keeps_the_filters() {
        let reply = snapshot();
        let envelope = view(json!({"path":"runs","match":{"taskId":"a"},"offset":2})).apply(reply.clone());
        assert_eq!(envelope["truncated"], true, "{envelope}");
        let retry = envelope["retry"][0]["view"].clone();
        assert_eq!(retry["path"], "runs");
        assert_eq!(retry["match"], json!({"taskId":"a"}));
        assert_eq!(retry["offset"], 2);
        let page = serde_json::from_value::<ManagementView>(retry).unwrap().apply(reply);
        assert!(page.get("truncated").is_none() && page.get("error").is_none(), "{page}");
        assert_eq!(page["items"][0]["id"], "r4");
    }

    #[test]
    fn full_returns_everything_past_the_cap() {
        let out = view(json!({"full": true})).apply(snapshot());
        assert!(out.get("truncated").is_none());
        assert_eq!(out["runs"].as_array().unwrap().len(), 50);
    }

    #[test]
    fn max_bytes_raises_the_cap_but_is_bounded() {
        let out = view(json!({"maxBytes": 400_000})).apply(snapshot());
        assert!(out.get("truncated").is_none());
        let out = view(json!({"maxBytes": 10_000_000, "path": "runs"})).apply(json!({"runs": ["x".repeat(600_000)]}));
        assert_eq!(out["truncated"], true);
    }
}
