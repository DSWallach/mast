use mast_proto::{
    AgentEvent,
    token_usage::{TokenCounts, UsageMeasurement},
};
use serde_json::Value;

pub(super) fn tool_diagnostics(method: &str, item: &Value) -> Vec<AgentEvent> {
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
    let bytes = payload
        .as_str()
        .map(|s| s.len())
        .unwrap_or_else(|| payload.to_string().len()) as u64;
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
    turn: String,
    baseline: Option<TokenCounts>,
}
fn counts(v: &Value) -> Option<TokenCounts> {
    let count = |a: &str, b: &str| v.get(a).or_else(|| v.get(b)).and_then(Value::as_u64);
    Some(TokenCounts {
        input: count("inputTokens", "input_tokens")?,
        output: count("outputTokens", "output_tokens")?,
        cache_read: count("cachedInputTokens", "cached_input_tokens"),
        cache_write: count("cacheWriteInputTokens", "cache_write_input_tokens"),
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
    pub(super) fn observe(&mut self, params: &Value) -> Option<AgentEvent> {
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
        Some(AgentEvent::UsageMeasurement {
            measurement: UsageMeasurement {
                id: format!("provider:codex:{turn}"),
                model: None,
                scope: "turn".into(),
                counts: subtract(&total, baseline),
            },
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn event(turn: &str, total: u64, last: u64) -> Value {
        json!({"turnId":turn,"tokenUsage":{"last":{"inputTokens":last,"outputTokens":0,"cachedInputTokens":0,"cacheWriteInputTokens":0,"reasoningOutputTokens":0},"total":{"inputTokens":total,"outputTokens":0,"cachedInputTokens":0,"cacheWriteInputTokens":0,"reasoningOutputTokens":0}}})
    }
    fn input(event: Option<AgentEvent>) -> u64 {
        let AgentEvent::UsageMeasurement { measurement } = event.unwrap() else {
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
}
