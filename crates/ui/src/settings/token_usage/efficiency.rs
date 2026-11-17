use super::*;

impl TokenUsagePage {
    pub(super) fn agent_breakdown(
        &self,
        snapshot: &TokenUsageSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut content = div().flex().flex_col().gap_2().child(widgets::field_label(
            theme,
            "Agent types · most tokens first",
        ));
        for (i, agent) in snapshot.agent_types.iter().enumerate() {
            let name = agent.agent_type.clone();
            let suffix = if agent.role == "main + subagents" {
                "includes subagents"
            } else {
                agent.role.as_str()
            };
            content = content.child(
                widgets::card_row(theme, i == 0)
                    .id(SharedString::from(format!("usage-agent-type-{i}")))
                    .debug_selector(move || format!("usage-agent-type-{i}"))
                    .cursor_pointer()
                    .child(div().flex_1().min_w_0().child(name.clone()).child(
                        widgets::page_subtitle(theme, format!("{suffix} · {} turns", agent.turns)),
                    ))
                    .child(format!("{} tokens →", agent.tokens))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.query.agent_type = Some(name.clone());
                        this.reset(cx);
                    })),
            );
        }
        if !snapshot.observed_agents.is_empty() {
            content = content.child(widgets::field_label(theme, "Observed child agent requests"))
                .child(widgets::page_subtitle(theme, "A partial breakdown from provider-reported child requests. These tokens overlap the totals above; do not add them. Open an example to inspect the work."));
            for (i, agent) in snapshot.observed_agents.iter().enumerate() {
                let chat = agent.example_chat_id.clone();
                content = content.child(
                    widgets::card_row(theme, i == 0)
                        .id(SharedString::from(format!("usage-observed-agent-{i}")))
                        .debug_selector(move || format!("usage-observed-agent-{i}"))
                        .cursor_pointer()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(agent.agent_type.clone())
                                .child(widgets::page_subtitle(
                                    theme,
                                    format!(
                                        "{} reported requests · {} parent turns · {} missing breakdowns",
                                        agent.observations, agent.turns, agent.unknown_breakdown
                                    ),
                                )),
                        )
                        .child(if agent.unknown_breakdown == agent.observations {
                            "Token breakdown unreported →".to_owned()
                        } else if agent.unknown_breakdown > 0 {
                            format!("{} observed tokens (partial) →", agent.tokens)
                        } else {
                            format!("{} observed tokens →", agent.tokens)
                        })
                        .on_click(
                            cx.listener(move |_, _, _, cx| cx.emit(OpenUsageChat(chat.clone()))),
                        ),
                );
            }
        }
        content
    }

    pub(super) fn efficiency_view(
        &self,
        snapshot: &TokenUsageSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        if !snapshot.insights_available {
            return div().child(widgets::page_subtitle(
                theme,
                "Update Mast on the selected device to view efficiency diagnostics.",
            ));
        }
        let summary = &snapshot.efficiency;
        let average = if summary.completed == 0 {
            "No completed turns in this selection.".to_owned()
        } else {
            format!(
                "{} total tokens per completed turn, including failed and partial attempts in this selection.",
                snapshot.total / summary.completed
            )
        };
        let mut content = div().flex().flex_col().gap_2()
            .child(widgets::field_label(theme, "Completion and failed attempts"))
            .child(widgets::section_card(theme)
                .child(widgets::card_row(theme, true).child(average))
                .child(widgets::card_row(theme, false).child(format!("{} completed · {} failed · {} cancelled · {} partial turns", summary.completed, summary.failed, summary.cancelled, summary.partial)))
                .child(widgets::card_row(theme, false).child(format!("{} completed-turn tokens · {} failed/cancelled tokens · {} partial-turn tokens", summary.completed_tokens, summary.failed_tokens, summary.partial_tokens))))
            .child(widgets::page_subtitle(theme, "Completed means the agent finished a turn, not that its work was verified. Compare the same task, model and outcome; Trends & comparisons separates work items and configurations."))
            .child(widgets::field_label(theme, "Repeated work to investigate"))
            .child(widgets::page_subtitle(theme, "Repetition, errors and empty results can be necessary. These are observed calls and bytes, not measured waste or promised token savings. Input and result rows describe separate stages."));
        if snapshot.insights.is_empty() {
            content = content.child(widgets::page_subtitle(
                theme,
                "No repetition, tool errors or empty results recorded for these filters.",
            ));
        }
        for (i, insight) in snapshot.insights.iter().enumerate() {
            let chat = insight.example_chat_id.clone();
            let topic = match insight.category.as_str() {
                "toolCall" => "Repeated tool input",
                "retainedToolResult" | "toolResult" => "Tool results",
                _ => "Instruction delivery",
            };
            content = content.child(
                widgets::card_row(theme, i == 0)
                    .id(SharedString::from(format!("usage-efficiency-insight-{i}")))
                    .debug_selector(move || format!("usage-efficiency-insight-{i}"))
                    .cursor_pointer()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(format!(
                                "{topic} · {} · {}",
                                insight.agent_type, insight.label
                            )))
                            .child(widgets::page_subtitle(
                                theme,
                                format!(
                                    "{} observations · {} repeats · {} errors · {} empty results",
                                    insight.occurrences,
                                    insight.repeated,
                                    insight.errors,
                                    insight.empty_results
                                ),
                            ))
                            .child(widgets::page_subtitle(
                                theme,
                                format!(
                                    "{} · {} · example {}",
                                    insight.channel.as_deref().unwrap_or("instructions"),
                                    insight.stage,
                                    format_time(insight.example_at)
                                ),
                            )),
                    )
                    .child(format!("{} repeated bytes →", insight.repeated_bytes))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(OpenUsageChat(chat.clone())))),
            );
        }
        content
    }
}
