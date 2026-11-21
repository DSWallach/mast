use mast_proto::{
    AgentEvent,
    token_usage::{TokenCounts, UsageMeasurement},
};
use serde_json::Value;

fn tool_diagnostics(
    method: &str,
    item: &Value,
    fingerprints: &crate::usage_fingerprint::UsageFingerprint,
) -> Vec<AgentEvent> {
    use mast_proto::token_usage::UsageDiagnostic;
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
    let channel = match kind {
        "commandExecution" | "command_execution" => "cli",
        "mcpToolCall" | "mcp_tool_call" => "mcp",
        _ => return Vec::new(),
    };
    let id = item.get("id").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() {
        return Vec::new();
    }
    let payload = if method == "item/started" {
        if channel == "cli" {
            item.get("command")
        } else {
            item.get("arguments")
        }
    } else if channel == "cli" {
        item.get("aggregatedOutput")
            .or_else(|| item.get("aggregated_output"))
    } else {
        item.get("result").or_else(|| item.get("error"))
    };
    let Some(payload) = payload.filter(|p| !p.is_null()) else {
        return Vec::new();
    };
    let (bytes, signature) = fingerprints.measure(payload);
    let arguments = method == "item/started";
    vec![AgentEvent::UsageDiagnostic {
        diagnostic: UsageDiagnostic {
            id: id.into(),
            category: if arguments {
                "toolArguments"
            } else {
                "toolResult"
            }
            .into(),
            label: "Tool payload".into(),
            channel: Some(channel.into()),
            bytes,
            signature: Some(signature),
            stage: if arguments {
                "harnessCall"
            } else {
                "harnessResponse"
            }
            .into(),
            method: "bytesOnly".into(),
            is_error: (!arguments)
                .then(|| item.get("status").and_then(Value::as_str) == Some("failed")),
            ..Default::default()
        },
    }]
}

#[derive(Default)]
pub(super) struct UsageTracker {
    fingerprints: crate::usage_fingerprint::UsageFingerprint,
    turn: String,
    baseline: Option<TokenCounts>,
    compactions: u32,
}
fn counts(v: &Value) -> Option<TokenCounts> {
    let count = |a: &str, b: &str| v.get(a).or_else(|| v.get(b)).and_then(Value::as_u64);
    Some(TokenCounts {
        input: count("inputTokens", "input_tokens")?,
        output: count("outputTokens", "output_tokens")?,
        cache_read: count("cachedInputTokens", "cached_input_tokens"),
        cache_write: count("cacheWriteInputTokens", "cache_write_input_tokens")
            .or_else(|| count("cachedInputTokens", "cached_input_tokens").map(|_| 0)),
        reasoning: count("reasoningOutputTokens", "reasoning_output_tokens"),
    })
}
fn subtract(a: &TokenCounts, b: &TokenCounts) -> TokenCounts {
    TokenCounts {
        input: a.input.saturating_sub(b.input),
        output: a.output.saturating_sub(b.output),
        cache_read: a
            .cache_read
            .zip(b.cache_read)
            .map(|(a, b)| a.saturating_sub(b)),
        cache_write: a
            .cache_write
            .zip(b.cache_write)
            .map(|(a, b)| a.saturating_sub(b)),
        reasoning: a
            .reasoning
            .zip(b.reasoning)
            .map(|(a, b)| a.saturating_sub(b)),
    }
}
impl UsageTracker {
    pub(super) fn tool_diagnostics(&self, method: &str, item: &Value) -> Vec<AgentEvent> {
        tool_diagnostics(method, item, &self.fingerprints)
    }
    pub(super) fn observe(&mut self, params: &Value) -> Vec<AgentEvent> {
        self.measure(params).unwrap_or_default()
    }
    /// A `thread/compacted` notification as a countable diagnostic. The id
    /// is distinct for each compaction within this session.
    pub(super) fn compaction(&mut self, params: &Value) -> AgentEvent {
        use mast_proto::token_usage::UsageDiagnostic;
        self.compactions += 1;
        let turn = params
            .get("turnId")
            .or_else(|| params.get("turn_id"))
            .and_then(Value::as_str)
            .unwrap_or(&self.turn);
        AgentEvent::UsageDiagnostic {
            diagnostic: UsageDiagnostic {
                id: format!("compaction-{turn}-{}", self.compactions),
                category: "compaction".into(),
                label: "Context compaction".into(),
                channel: Some("codex".into()),
                stage: "harnessEvent".into(),
                method: "bytesOnly".into(),
                ..Default::default()
            },
        }
    }
    fn measure(&mut self, params: &Value) -> Option<Vec<AgentEvent>> {
        let usage = params
            .get("tokenUsage")
            .or_else(|| params.get("token_usage"))?;
        let turn = params
            .get("turnId")
            .or_else(|| params.get("turn_id"))?
            .as_str()?;
        let last = counts(usage.get("last")?)?;
        let total = counts(usage.get("total")?)?;
        if self.turn != turn || self.baseline.is_none() {
            self.turn = turn.into();
            // Resumed/forked threads can start with earlier usage. The first
            // notification's total minus last establishes that history.
            self.baseline = Some(subtract(&total, &last));
        }
        let baseline = self.baseline.as_ref()?;
        if total.input < baseline.input || total.output < baseline.output {
            return None;
        }
        let mut events = vec![AgentEvent::UsageMeasurement {
            measurement: UsageMeasurement {
                id: format!("provider:codex:{turn}"),
                model: None,
                scope: "turn".into(),
                counts: subtract(&total, baseline),
            },
        }];
        // `last` is one model request. Re-sent notifications repeat the same
        // cumulative total, so it keys the request and they collapse.
        if last.input > 0 || last.output > 0 {
            events.push(AgentEvent::UsageMeasurement {
                measurement: UsageMeasurement {
                    id: format!(
                        "provider:codex-request:{turn}:{}:{}",
                        total.input, total.output
                    ),
                    model: None,
                    scope: "request".into(),
                    counts: last,
                },
            });
        }
        Some(events)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn raw_tool_results_are_fingerprinted_without_copying_the_body() {
        let tracker = UsageTracker::default();
        let item = json!({"type":"commandExecution", "id":"call", "aggregatedOutput":"private result", "status":"completed"});
        let first = tracker.tool_diagnostics("item/completed", &item);
        let second = tracker.tool_diagnostics("item/completed", &item);
        let AgentEvent::UsageDiagnostic { diagnostic: a } = &first[0] else {
            panic!()
        };
        let AgentEvent::UsageDiagnostic { diagnostic: b } = &second[0] else {
            panic!()
        };
        assert!(a.signature.is_some());
        assert_eq!(a.signature, b.signature);
        assert_eq!(a.bytes, 14);
        assert!(
            !serde_json::to_string(&first)
                .unwrap()
                .contains("private result")
        );
        let independent = UsageTracker::default().tool_diagnostics("item/completed", &item);
        let AgentEvent::UsageDiagnostic { diagnostic: c } = &independent[0] else {
            panic!()
        };
        assert_ne!(a.signature, c.signature);
    }
    fn event(turn: &str, total: u64, last: u64) -> Value {
        json!({"turnId":turn,"tokenUsage":{"last":{"inputTokens":last,"outputTokens":0,"cachedInputTokens":0,"cacheWriteInputTokens":0,"reasoningOutputTokens":0},"total":{"inputTokens":total,"outputTokens":0,"cachedInputTokens":0,"cacheWriteInputTokens":0,"reasoningOutputTokens":0}}})
    }
    fn input(events: Vec<AgentEvent>) -> u64 {
        let Some(AgentEvent::UsageMeasurement { measurement }) = events.first() else {
            panic!()
        };
        measurement.counts.input
    }
    #[test]
    fn cumulative_turn_counts_exclude_resumed_history_and_dedupe() {
        let mut t = UsageTracker::default();
        assert_eq!(input(t.observe(&event("a", 110, 10))), 10);
        assert_eq!(input(t.observe(&event("a", 130, 20))), 30);
        assert_eq!(input(t.observe(&event("a", 130, 20))), 30);
        assert_eq!(input(t.observe(&event("b", 135, 5))), 5);
    }
    #[test]
    fn each_request_is_reported_once_with_its_own_counts() {
        let mut t = UsageTracker::default();
        let requests = |events: Vec<AgentEvent>| {
            events
                .into_iter()
                .filter_map(|event| match event {
                    AgentEvent::UsageMeasurement { measurement }
                        if measurement.scope == "request" =>
                    {
                        Some((measurement.id, measurement.counts.input))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let first = requests(t.observe(&event("a", 110, 10)));
        let second = requests(t.observe(&event("a", 130, 20)));
        let resent = requests(t.observe(&event("a", 130, 20)));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].1, 10);
        assert_eq!(second[0].1, 20);
        assert_ne!(first[0].0, second[0].0);
        // A repeated notification keeps the same id, so the ledger upserts it.
        assert_eq!(second, resent);
        assert!(requests(t.observe(&event("a", 130, 0))).is_empty());
    }
    #[test]
    fn child_usage_routes_to_its_tracker_and_implicit_cache_write_is_zero() {
        assert!(matches!(
            super::super::normalize::route_child_notification("thread/tokenUsage/updated"),
            super::super::normalize::ChildRoute::Subagent
        ));
        let mut child = super::super::normalize::ChildStream::default();
        let params = json!({"turnId":"child-turn","tokenUsage":{"total":{"inputTokens":50,"outputTokens":5,"cachedInputTokens":30},"last":{"inputTokens":20,"outputTokens":2,"cachedInputTokens":10}}});
        let events = child.map("child", "thread/tokenUsage/updated", &params);
        let AgentEvent::UsageMeasurement { measurement } = &events[0] else {
            panic!()
        };
        assert_eq!(measurement.counts.input, 20);
        assert_eq!(measurement.counts.cache_write, Some(0));
        assert_eq!(
            measurement
                .counts
                .selected(mast_proto::token_usage::TokenKind::UncachedInput),
            10
        );
    }
}
