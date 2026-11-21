use super::*;

impl TokenUsagePage {
    pub(super) fn turns_view(
        &self,
        snapshot: &TokenUsageSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let Some(analysis) = &snapshot.turn_analysis else {
            return div().child(widgets::page_subtitle(
                theme,
                "Update Mast on the selected device to view per-turn analysis.",
            ));
        };
        let mut content = div().flex().flex_col().gap_2()
            .child(widgets::page_subtitle(theme, "Provider-reported tokens per turn, counted the same way as the totals. Input includes cache reads and writes. Providers usually bill cache reads well below new input, so input alone overstates cost. Mast does not estimate prices. Reported fields by harness are listed at the end."))
            .child(widgets::field_label(theme, "Workflows · most input first"))
            .child(widgets::page_subtitle(theme, "Grouped by harness, agent type and how the turn started. Input per request approximates the context sent with each model call. Codex does not report cache writes, so its cache-write share reads zero. Compare similar work; a group with fewer turns can differ by task mix alone."));
        content = groups(content, "usage-workflow", &analysis.workflows, theme, cx);
        content = content
            .child(widgets::field_label(theme, "Chat age at each turn"))
            .child(widgets::page_subtitle(theme, "Older chats tend to resend more history. Turns recorded before this field existed, and title runs, have no chat age."));
        content = groups(content, "usage-chat-age", &analysis.chat_ages, theme, cx);
        content = content.child(widgets::field_label(
            theme,
            "Largest turns · most input first",
        ));
        if analysis.turns.is_empty() {
            content = content.child(widgets::page_subtitle(
                theme,
                "No turns match these filters.",
            ));
        }
        for (i, turn) in analysis.turns.iter().enumerate() {
            let chat = turn.chat_id.clone();
            let c = &turn.counts;
            let input = match (c.cache_read, c.cache_write) {
                (Some(read), Some(write)) => format!(
                    "{} input: {} cache read · {} · {} uncached",
                    compact(c.input),
                    compact(read),
                    // Codex never reports writes; the adapter records zero.
                    if turn.harness.as_deref() == Some("Codex") {
                        "cache write unreported".to_owned()
                    } else {
                        format!("{} cache write", compact(write))
                    },
                    compact(c.input.saturating_sub(read.saturating_add(write)))
                ),
                _ => format!("{} input · cache breakdown unreported", compact(c.input)),
            };
            let requests = match turn.requests {
                Some(n) => format!(
                    "{n} requests{}",
                    turn.subagent_requests
                        .filter(|s| *s > 0)
                        .map(|s| format!(" + {s} subagent"))
                        .unwrap_or_default()
                ),
                None => "requests unreported".into(),
            };
            content = content.child(
                widgets::card_row(theme, i == 0)
                    .id(SharedString::from(format!("usage-turn-{i}")))
                    .debug_selector(move || format!("usage-turn-{i}"))
                    .cursor_pointer()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(format!(
                                "{} · {} · {}",
                                turn.harness.as_deref().unwrap_or("Unknown harness"),
                                turn.models.join(", "),
                                turn.agent_type
                            )))
                            .child(widgets::page_subtitle(
                                theme,
                                format!(
                                    "{} · {} · {} · {}",
                                    format_time(turn.at),
                                    turn.status,
                                    start_label(turn.session_start.as_deref()),
                                    turn.chat_age_ms
                                        .map(|age| format!("chat {} old", duration(age)))
                                        .unwrap_or_else(|| "chat age unrecorded".into())
                                ),
                            ))
                            .child(widgets::page_subtitle(theme, format!("{input} · {} output", compact(c.output))))
                            .child(widgets::page_subtitle(
                                theme,
                                format!(
                                    "{requests} · peak context {} · {} tool calls · {} tool-result bytes · {}",
                                    turn.peak_context.map(compact).unwrap_or_else(|| "unreported".into()),
                                    turn.tool_calls,
                                    compact(turn.tool_result_bytes),
                                    scope_label(&turn.scope)
                                ),
                            )),
                    )
                    .child(format!("{} tokens →", compact(c.input.saturating_add(c.output))))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(OpenUsageChat(chat.clone())))),
            );
        }
        harnesses(content, &analysis.harnesses, theme)
    }
}

fn harnesses(mut content: gpui::Div, harnesses: &[HarnessCoverage], theme: &Theme) -> gpui::Div {
    content = content.child(widgets::field_label(theme, "Reported fields by harness"));
    for (i, h) in harnesses.iter().enumerate() {
        content = content.child(
            widgets::card_row(theme, i == 0).child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .child(format!("{} · {} turns", h.harness, h.turns))
                    .child(widgets::page_subtitle(
                        theme,
                        format!(
                            "Cache reads on {} · cache writes on {} · reasoning on {} · request counts on {} · session start on {} · input/output only on {}",
                            h.cache_read_turns,
                            h.cache_write_turns,
                            h.reasoning_turns,
                            h.request_count_turns,
                            h.session_start_turns,
                            h.legacy_only_turns
                        ),
                    ))
                    .child(widgets::page_subtitle(theme, h.semantics.clone())),
            ),
        );
    }
    content
}

fn groups(
    mut content: gpui::Div,
    prefix: &'static str,
    groups: &[TurnGroup],
    theme: &Theme,
    cx: &mut Context<TokenUsagePage>,
) -> gpui::Div {
    if groups.is_empty() {
        return content.child(widgets::page_subtitle(
            theme,
            "No turns match these filters.",
        ));
    }
    for (i, g) in groups.iter().enumerate() {
        let chat = g.example_chat_id.clone();
        let cache = if g.cache_known_turns == 0 {
            "Cache breakdown unreported".to_owned()
        } else {
            format!(
                "Cache reads {} and cache writes {} of input on {} of {} turns",
                percent(g.cache_read, g.cache_known_input),
                percent(g.cache_write, g.cache_known_input),
                g.cache_known_turns,
                g.turns
            )
        };
        let requests = if g.requests == 0 {
            "Request counts unreported".to_owned()
        } else {
            format!(
                "{} input per request over {} requests on {} turns",
                compact(g.request_known_input / g.requests),
                g.requests,
                g.request_known_turns
            )
        };
        let peak = if g.peak_context_known_turns == 0 {
            "peak context unreported".to_owned()
        } else {
            format!("peak context up to {}", compact(g.peak_context_max))
        };
        content = content.child(
            widgets::card_row(theme, i == 0)
                .id(SharedString::from(format!("{prefix}-{i}")))
                .debug_selector(move || format!("{prefix}-{i}"))
                .cursor_pointer()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().child(g.label.clone()))
                        .child(widgets::page_subtitle(
                            theme,
                            format!(
                                "{} turns · median {} · p90 {} input per turn · {} output",
                                g.turns,
                                compact(g.median_input),
                                compact(g.p90_input),
                                compact(g.output)
                            ),
                        ))
                        .child(widgets::page_subtitle(theme, cache))
                        .child(widgets::page_subtitle(
                            theme,
                            format!(
                                "{requests} · {peak} · {} tool-result bytes",
                                compact(g.tool_result_bytes)
                            ),
                        )),
                )
                .child(format!("{} input →", compact(g.input)))
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(OpenUsageChat(chat.clone())))),
        );
    }
    content
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "unknown".into();
    }
    format!("{:.1}%", part as f64 * 100.0 / whole as f64)
}

fn compact(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}K", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

fn duration(ms: i64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        ..60 => format!("{minutes} min"),
        60..2880 => format!("{} h", minutes / 60),
        _ => format!("{} days", minutes / 1440),
    }
}

fn start_label(start: Option<&str>) -> &'static str {
    match start {
        Some("fresh") => "new session",
        Some("resumed") => "resumed session",
        Some("continued") => "continued in process",
        _ => "start unrecorded",
    }
}

fn scope_label(scope: &str) -> &'static str {
    match scope {
        "turnIncludingChildren" => "turn total includes subagents",
        "turn" => "turn total",
        "request" => "sum of requests, turn total not yet reported",
        _ => "input/output only",
    }
}

#[cfg(feature = "snapshot")]
pub(super) fn fixture(now: i64) -> TurnAnalysis {
    let cached = |input: u64, read: u64, write: u64, output: u64| TokenCounts {
        input,
        output,
        cache_read: Some(read),
        cache_write: Some(write),
        reasoning: None,
    };
    let workflow =
        |label: &str, turns, input, read, write, requests, median, p90, peak| TurnGroup {
            label: label.into(),
            turns,
            input,
            output: input / 300,
            cache_known_input: input,
            cache_read: read,
            cache_write: write,
            cache_known_turns: turns,
            request_known_input: input,
            requests,
            request_known_turns: turns,
            median_input: median,
            p90_input: p90,
            peak_context_max: peak,
            peak_context_known_turns: turns,
            tool_result_bytes: input / 40,
            example_chat_id: "example".into(),
        };
    TurnAnalysis {
        turns: vec![
            TurnProfile {
                turn_id: "run:4".into(),
                chat_id: "example".into(),
                at: now - 3_600_000,
                status: "completed".into(),
                activity: "Conversation".into(),
                agent_type: "main".into(),
                harness: Some("Codex".into()),
                models: vec!["large-model".into()],
                session_start: Some("continued".into()),
                chat_age_ms: Some(30 * 3_600_000),
                counts: cached(1_548_444, 1_521_000, 0, 6_200),
                scope: "turn".into(),
                requests: Some(10),
                subagent_requests: Some(0),
                peak_context: Some(157_376),
                tool_calls: 24,
                tool_result_bytes: 96_000,
            },
            TurnProfile {
                turn_id: "review:0".into(),
                chat_id: "example".into(),
                at: now - 7_200_000,
                status: "completed".into(),
                activity: "PR review".into(),
                agent_type: "pr-review".into(),
                harness: Some("ClaudeCode".into()),
                models: vec!["large-model".into(), "small-model".into()],
                session_start: Some("fresh".into()),
                chat_age_ms: Some(120_000),
                counts: cached(440_405, 401_000, 39_000, 3_100),
                scope: "turnIncludingChildren".into(),
                requests: Some(6),
                subagent_requests: Some(9),
                peak_context: Some(80_070),
                tool_calls: 18,
                tool_result_bytes: 42_000,
            },
            TurnProfile {
                turn_id: "other:0".into(),
                chat_id: "example".into(),
                at: now - 9_000_000,
                status: "failed".into(),
                activity: "Conversation".into(),
                agent_type: "Unknown agent type".into(),
                harness: Some("Cursor".into()),
                models: vec!["Unknown model".into()],
                counts: TokenCounts {
                    input: 70_000,
                    output: 900,
                    ..Default::default()
                },
                scope: "legacy".into(),
                ..Default::default()
            },
        ],
        workflows: vec![
            workflow("Codex · main · continued in process", 90, 293_073_000, 286_148_000, 0, 1_850, 2_400_000, 9_800_000, 207_990),
            workflow("ClaudeCode · orchestrator · resumed session", 17, 21_650_000, 21_549_000, 100_000, 310, 900_000, 3_100_000, 131_761),
            TurnGroup {
                label: "Cursor · Unknown agent type · start unrecorded".into(),
                turns: 3,
                input: 210_000,
                output: 2_700,
                median_input: 70_000,
                p90_input: 72_000,
                example_chat_id: "example".into(),
                ..Default::default()
            },
        ],
        chat_ages: vec![
            workflow("Chat under 1 hour old", 40, 30_000_000, 28_500_000, 900_000, 600, 450_000, 1_900_000, 90_000),
            workflow("Chat 1–7 days old", 25, 120_000_000, 118_000_000, 80_000, 1_100, 3_900_000, 9_800_000, 207_990),
        ],
        harnesses: vec![
            HarnessCoverage {
                harness: "Codex".into(),
                turns: 121,
                cache_read_turns: 121,
                cache_write_turns: 121,
                reasoning_turns: 121,
                request_count_turns: 96,
                session_start_turns: 40,
                semantics: "Cumulative thread counters converted to per-turn amounts, plus each request's own counts. Input includes cached input. Codex does not report cache writes; they are recorded as zero. Reasoning is part of output.".into(),
                ..Default::default()
            },
            HarnessCoverage {
                harness: "Cursor".into(),
                turns: 3,
                legacy_only_turns: 3,
                semantics: "Input and output totals only. Cache, reasoning and request counts are not reported, and it is unknown whether input includes cached tokens.".into(),
                ..Default::default()
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_compact_and_unknowns_stay_unknown() {
        assert_eq!(compact(999), "999");
        assert_eq!(compact(68_700_000), "68.7M");
        assert_eq!(percent(1, 0), "unknown");
        assert_eq!(percent(97, 100), "97.0%");
        assert_eq!(duration(90 * 60_000), "1 h");
        assert_eq!(duration(3 * 86_400_000), "3 days");
        assert_eq!(start_label(None), "start unrecorded");
    }
}
