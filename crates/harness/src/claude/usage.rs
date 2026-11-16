//! Metadata captured before transcript truncation. Provider usage and content
//! bytes are deliberately different event types.
use super::wire::Frame;

#[derive(Default)]
pub(super) struct UsageTracker {
    needs_resume_baseline: bool,
    previous: std::collections::BTreeMap<String, TokenCounts>,
    results: std::collections::VecDeque<String>,
}
impl UsageTracker {
    pub(super) fn establish_resume_baseline(&mut self, resumed: bool) {
        self.needs_resume_baseline = resumed;
    }

    pub(super) fn observe(&mut self, frame: &Frame) -> Vec<AgentEvent> {
        if let Frame::Result(f) = frame {
            if let Some(id) = &f.uuid {
                if self.results.contains(id) {
                    return Vec::new();
                }
                if self.results.len() >= 256 {
                    self.results.pop_front();
                }
                self.results.push_back(id.clone());
            }
        }
        let baseline_only = matches!(frame,Frame::Result(f) if !f.model_usage.is_empty())
            && self.needs_resume_baseline;
        if baseline_only {
            self.needs_resume_baseline = false;
        }
        let mut events = observe(frame);
        if matches!(frame, Frame::Result(_)) {
            for event in &mut events {
                if let AgentEvent::UsageMeasurement { measurement } = event {
                    let model = measurement.model.clone().unwrap_or_default();
                    let current = measurement.counts.clone();
                    if let Some(previous) = self.previous.get(&model) {
                        if current.input >= previous.input && current.output >= previous.output {
                            measurement.counts = TokenCounts {
                                input: current.input - previous.input,
                                output: current.output - previous.output,
                                cache_read: current
                                    .cache_read
                                    .zip(previous.cache_read)
                                    .map(|(a, b)| a.saturating_sub(b)),
                                cache_write: current
                                    .cache_write
                                    .zip(previous.cache_write)
                                    .map(|(a, b)| a.saturating_sub(b)),
                                reasoning: None,
                            };
                        }
                    }
                    self.previous.insert(model, current);
                    if baseline_only {
                        measurement.scope = "resumeBaseline".into();
                    }
                }
            }
        }
        if matches!(frame, Frame::Result(_)) {
            events.retain(|event| match event {
                AgentEvent::UsageMeasurement { measurement } => {
                    measurement.scope == "resumeBaseline"
                        || measurement.counts.input > 0
                        || measurement.counts.output > 0
                }
                _ => true,
            });
        }
        events
    }
}
use mast_proto::{
    AgentEvent,
    token_usage::{TokenCounts, UsageDiagnostic, UsageMeasurement},
};
use serde_json::Value;

pub(super) fn observe(frame: &Frame) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    match frame {
        Frame::Result(f) => {
            for (model, value) in &f.model_usage {
                let (Some(input), Some(output)) = (
                    value.get("inputTokens").and_then(Value::as_u64),
                    value.get("outputTokens").and_then(Value::as_u64),
                ) else {
                    continue;
                };
                let read = value.get("cacheReadInputTokens").and_then(Value::as_u64);
                let write = value
                    .get("cacheCreationInputTokens")
                    .and_then(Value::as_u64);
                out.push(AgentEvent::UsageMeasurement {
                    measurement: UsageMeasurement {
                        id: f
                            .uuid
                            .as_ref()
                            .map(|id| format!("provider:claude-result:{id}:{model}"))
                            .unwrap_or_else(|| format!("result:{model}")),
                        model: Some(model.clone()),
                        scope: "turnIncludingChildren".into(),
                        counts: TokenCounts {
                            input: input
                                .saturating_add(read.unwrap_or(0))
                                .saturating_add(write.unwrap_or(0)),
                            output,
                            cache_read: read,
                            cache_write: write,
                            reasoning: None,
                        },
                    },
                });
            }
        }
        Frame::Assistant(f) => {
            if !f.message.id.is_empty()
                && let Some(u) = &f.message.usage
            {
                let read = u.get("cache_read_input_tokens").and_then(Value::as_u64);
                let write = u.get("cache_creation_input_tokens").and_then(Value::as_u64);
                if let (Some(input), Some(output)) = (
                    u.get("input_tokens").and_then(Value::as_u64),
                    u.get("output_tokens").and_then(Value::as_u64),
                ) {
                    out.push(AgentEvent::UsageMeasurement {
                        measurement: UsageMeasurement {
                            id: format!("provider:claude-request:{}", f.message.id),
                            model: f.message.model.clone(),
                            scope: "request".into(),
                            counts: TokenCounts {
                                input: input
                                    .saturating_add(read.unwrap_or(0))
                                    .saturating_add(write.unwrap_or(0)),
                                output,
                                cache_read: read,
                                cache_write: write,
                                reasoning: u
                                    .pointer("/output_tokens_details/thinking_tokens")
                                    .and_then(Value::as_u64),
                            },
                        },
                    });
                }
            }
            for b in f.message.blocks().filter(|b| b.kind == "tool_use") {
                let call = super::normalize::decode_tool_use(&b.name, &b.input);
                let (channel, label) = match &call {
                    mast_proto::ToolCall::Mcp { server, tool, .. } => {
                        ("mcp", format!("{server}/{tool}"))
                    }
                    mast_proto::ToolCall::Exec { .. } => ("cli", "Shell command".into()),
                    _ => ("native", b.name),
                };
                out.push(AgentEvent::UsageDiagnostic {
                    diagnostic: UsageDiagnostic {
                        id: b.id,
                        category: "toolArguments".into(),
                        label,
                        channel: Some(channel.into()),
                        bytes: b.input.to_string().len() as u64,
                        stage: "harnessCall".into(),
                        method: "bytesOnly".into(),
                        ..Default::default()
                    },
                });
            }
        }
        Frame::User(f) => {
            for b in f.message.blocks().filter(|b| b.kind == "tool_result") {
                let bytes = if let Some(text) = b.content.as_str() {
                    text.len()
                } else {
                    b.content.to_string().len()
                };
                out.push(AgentEvent::UsageDiagnostic {
                    diagnostic: UsageDiagnostic {
                        id: b.tool_use_id,
                        category: "toolResult".into(),
                        label: "Tool result".into(),
                        bytes: bytes as u64,
                        stage: "harnessResponse".into(),
                        method: "bytesOnly".into(),
                        is_error: b.is_error,
                        ..Default::default()
                    },
                });
            }
        }
        _ => {}
    }
    if let Frame::Assistant(f) | Frame::User(f) = frame
        && let Some(parent) = &f.parent_tool_use_id
    {
        out = out
            .into_iter()
            .map(|event| AgentEvent::Subagent {
                parent_tool_use_id: parent.clone(),
                event: Box::new(event),
            })
            .collect();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn result_includes_cache_once_and_keeps_actual_models() {
        let frame = super::super::wire::parse_frame(r#"{"type":"result","modelUsage":{"small":{"inputTokens":10,"outputTokens":20,"cacheReadInputTokens":100,"cacheCreationInputTokens":5},"large":{"inputTokens":2,"outputTokens":3}}}"#).unwrap();
        let out = observe(&frame);
        assert_eq!(out.len(), 2);
        let AgentEvent::UsageMeasurement { measurement } = &out[1] else {
            panic!()
        };
        assert_eq!(measurement.model.as_deref(), Some("small"));
        assert_eq!(
            measurement
                .counts
                .selected(mast_proto::token_usage::TokenKind::All),
            135
        );
    }
    #[test]
    fn diagnostic_has_size_but_no_sensitive_body() {
        let frame = super::super::wire::parse_frame(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"call","name":"Bash","input":{"command":"echo secret-value"}}]}}"#).unwrap();
        let encoded = serde_json::to_string(&observe(&frame)).unwrap();
        assert!(!encoded.contains("secret-value"));
        assert!(encoded.contains("bytesOnly"));
    }
    #[test]
    fn live_background_fixture_cumulative_results_are_not_added_twice() {
        let mut tracker = UsageTracker::default();
        let mut input = 0;
        let mut output = 0;
        for line in
            include_str!("../../tests/fixtures/claude/live-2.1.228-background-subagent.jsonl")
                .lines()
        {
            let frame = super::super::wire::parse_frame(line).unwrap();
            if !matches!(frame, Frame::Result(_)) {
                continue;
            }
            for event in tracker.observe(&frame) {
                if let AgentEvent::UsageMeasurement { measurement } = event {
                    input += measurement.counts.input;
                    output += measurement.counts.output;
                }
            }
            assert!(tracker.observe(&frame).is_empty());
        }
        assert_eq!(input, 641 + 87489 + 15844);
        assert_eq!(output, 768);
    }

    #[test]
    fn resumed_history_seeds_a_baseline_and_inactive_models_do_not_emit_rows() {
        let mut tracker = UsageTracker::default();
        tracker.establish_resume_baseline(true);
        let first=super::super::wire::parse_frame(r#"{"type":"result","uuid":"first","modelUsage":{"active":{"inputTokens":1000,"outputTokens":500},"inactive":{"inputTokens":100,"outputTokens":20}}}"#).unwrap();
        let baselines = tracker.observe(&first);
        assert_eq!(baselines.len(), 2);
        assert!(baselines.iter().all(|event|matches!(event,AgentEvent::UsageMeasurement{measurement} if measurement.scope=="resumeBaseline")));
        let next=super::super::wire::parse_frame(r#"{"type":"result","uuid":"next","modelUsage":{"active":{"inputTokens":1010,"outputTokens":505},"inactive":{"inputTokens":100,"outputTokens":20}}}"#).unwrap();
        let events = tracker.observe(&next);
        assert_eq!(events.len(), 1);
        let AgentEvent::UsageMeasurement { measurement } = &events[0] else {
            panic!()
        };
        assert_eq!(measurement.model.as_deref(), Some("active"));
        assert_eq!(measurement.counts.input, 10);
        assert_eq!(measurement.counts.output, 5);
    }

    #[test]
    fn production_normalizer_preserves_legacy_usage_and_done_after_private_telemetry() {
        let mut normalizer = super::super::normalize::Normalizer::new();
        let frame=super::super::wire::parse_frame(r#"{"type":"result","uuid":"done","subtype":"success","usage":{"input_tokens":10,"output_tokens":20},"modelUsage":{"actual":{"inputTokens":10,"outputTokens":20}}}"#).unwrap();
        let events = normalizer.normalize(frame, false);
        assert!(matches!(
            events.first(),
            Some(AgentEvent::UsageMeasurement { .. })
        ));
        assert!(matches!(
            events[events.len() - 2],
            AgentEvent::Usage {
                input_tokens: 10,
                output_tokens: 20
            }
        ));
        assert!(matches!(events.last(), Some(AgentEvent::Done { .. })));
    }
}
