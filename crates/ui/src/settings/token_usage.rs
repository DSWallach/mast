//! Settings token accounting. Provider totals and diagnostic payload bytes never
//! share an axis: the latter are optimization clues, not billed token costs.
use crate::{popover, settings::widgets, state::AppState, theme::Theme};
use gpui::{Context, Entity, EventEmitter, SharedString, Task, Window, div, prelude::*};
use mast_proto::token_usage::*;

#[derive(Clone)]
pub struct OpenUsageChat(pub String);
pub struct TokenUsagePage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    query: TokenUsageQuery,
    target: Option<String>,
    days: Option<i64>,
    tab: usize,
    snapshot: Option<TokenUsageSnapshot>,
    error: Option<String>,
    loading: bool,
    task: Option<Task<()>>,
    generation: u64,
    refresh_task: Option<Task<()>>,
    polling: bool,
}
impl EventEmitter<OpenUsageChat> for TokenUsagePage {}
impl TokenUsagePage {
    #[cfg(feature = "snapshot")]
    pub(crate) fn fixture(state: Entity<AppState>, tab: usize, cx: &mut Context<Self>) -> Self {
        let mut page = Self::new(state, cx);
        page.error = None;
        page.task = None;
        page.loading = false;
        page.tab = tab;
        page.polling = false;
        let now = chrono::Utc::now().timestamp_millis();
        let groups = vec![
            UsageGroup {
                label: "Conversation".into(),
                tokens: 720000,
                runs: 12,
            },
            UsageGroup {
                label: "PR review".into(),
                tokens: 210000,
                runs: 4,
            },
            UsageGroup {
                label: "Title generation".into(),
                tokens: 8000,
                runs: 16,
            },
        ];
        page.snapshot = Some(TokenUsageSnapshot {
            total: 938000,
            runs: 32,
            models: vec!["large-model".into(), "small-model".into()],
            activities: groups.clone(),
            roles: vec![UsageGroup {
                label: "main + subagents".into(),
                tokens: 938000,
                runs: 32,
            }],
            days: vec![UsageGroup {
                label: "2026-11-16".into(),
                tokens: 938000,
                runs: 32,
            }],
            details: vec![UsageRun {
                id: "run".into(),
                chat_id: "example".into(),
                model: "large-model".into(),
                activity: "PR review".into(),
                role: "main + subagents".into(),
                at: now,
                tokens: 52000,
                status: "completed".into(),
                configuration: "config-revision-1".into(),
                context: Default::default(),
            }],
            diagnostics: vec![
                DiagnosticGroup {
                    category: "toolResult".into(),
                    label: "github/search".into(),
                    channel: Some("mcp".into()),
                    stage: "harnessResponse".into(),
                    method: "bytesOnly".into(),
                    bytes: 82400,
                    observations: 14,
                    example_chat_id: "example".into(),
                    ..Default::default()
                },
                DiagnosticGroup {
                    category: "toolResult".into(),
                    label: "rg".into(),
                    channel: Some("cli".into()),
                    stage: "harnessResponse".into(),
                    method: "bytesOnly".into(),
                    bytes: 12300,
                    observations: 9,
                    example_chat_id: "example".into(),
                    ..Default::default()
                },
                DiagnosticGroup {
                    category: "instruction".into(),
                    label: "Mast development / review gate".into(),
                    revision: Some("instruction-revision-2".into()),
                    stage: "submittedToHarness".into(),
                    method: "bytesOnly".into(),
                    bytes: 7800,
                    observations: 12,
                    example_chat_id: "example".into(),
                    ..Default::default()
                },
            ],
            coverage: vec![
                "Example fixture. Provider counts and diagnostic bytes are separate.".into(),
            ],
            recording_since: Some(now - 7 * 86400000),
            updated_at: now,
            ..Default::default()
        });
        page
    }
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut page = Self {
            state,
            scroll: Default::default(),
            query: Default::default(),
            target: None,
            days: Some(7),
            tab: 0,
            snapshot: None,
            error: None,
            loading: false,
            task: None,
            refresh_task: None,
            polling: true,
            generation: 0,
        };
        page.load(cx);
        page
    }
    fn load(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.query.since = self
            .days
            .map(|days| chrono::Utc::now().timestamp_millis() - days * 86_400_000);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.error = Some("Connect to an engine to view token usage.".into());
            self.loading = false;
            return;
        };
        let mut params = serde_json::to_value(&self.query).expect("usage query");
        if let Some(target) = &self.target {
            params["targetDeviceId"] = target.clone().into();
        }
        self.loading = true;
        self.error = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(mast_rpc::methods::TOKEN_USAGE, params)
                .await
                .map_err(|e| e.to_string())
                .and_then(|v| {
                    serde_json::from_value::<TokenUsageSnapshot>(v).map_err(|e| e.to_string())
                });
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(snapshot) => this.snapshot = Some(snapshot),
                    Err(error) => {
                        this.snapshot = None;
                        this.error = Some(error);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
    fn reset(&mut self, cx: &mut Context<Self>) {
        self.query.offset = 0;
        self.snapshot = None;
        self.load(cx);
    }
    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}
impl popover::ScrollRailHost for TokenUsagePage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }
    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}
fn choice(theme: &Theme, id: String, label: String, selected: bool) -> gpui::Stateful<gpui::Div> {
    crate::components::settings_autosave_choice(theme, SharedString::from(id), selected)
        .child(label)
}
fn rows(theme: &Theme, groups: &[UsageGroup], total: u64) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(groups.iter().map(|g| {
            let share = if total == 0 {
                0.0
            } else {
                g.tokens as f64 / total as f64 * 100.0
            };
            widgets::card_row(theme, false)
                .child(
                    div().flex_1().min_w_0().child(g.label.clone()).child(
                        div().h_1().rounded_full().bg(theme.border).child(
                            div()
                                .h_1()
                                .rounded_full()
                                .bg(theme.accent)
                                .w(gpui::relative((share / 100.0) as f32)),
                        ),
                    ),
                )
                .child(format!(
                    "{} tokens · {:.1}% · {} runs",
                    g.tokens, share, g.runs
                ))
        }))
}
impl Render for TokenUsagePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.polling && self.refresh_task.is_none() {
            self.refresh_task = Some(cx.spawn(async move |this, cx| {
                gpui::Timer::after(std::time::Duration::from_secs(15)).await;
                this.update(cx, |this, cx| {
                    this.refresh_task = None;
                    if !this.loading {
                        this.load(cx);
                    }
                })
                .ok();
            }));
        }
        let theme = Theme::of(cx).clone();
        let mut content=widgets::page_column()
            .child(widgets::page_header(&theme,"Token usage",None))
            .child(widgets::page_subtitle(&theme,"Find expensive activities, tools, and instructions. Compare provider usage with observed payload overhead."));
        let state = self.state.read(cx);
        let devices = state.devices.clone();
        let local = state.local_device_id.clone();
        let target = self.target.clone().or(local.clone());
        let offline = target.as_deref().is_some_and(|id| {
            !state.is_local_device(id) && !state.device_online(id, chrono::Utc::now())
        });
        let device_choices = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                choice(
                    &theme,
                    "usage-device-local".into(),
                    "This device".into(),
                    self.target.is_none(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.target = None;
                    this.reset(cx);
                })),
            )
            .children(
                devices
                    .into_iter()
                    .filter(|d| Some(&d.id) != local.as_ref())
                    .map(|d| {
                        let id = d.id.clone();
                        choice(
                            &theme,
                            format!("usage-device-{id}"),
                            d.name,
                            self.target.as_deref() == Some(id.as_str()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.target = Some(id.clone());
                            this.reset(cx);
                        }))
                    }),
            );
        content = content
            .child(widgets::field_label(&theme, "Device"))
            .child(device_choices);
        if offline {
            content = content.child(widgets::warning_strip(
                &theme,
                "This device is offline. Its usage cannot be refreshed.",
            ));
        }
        let dates = div().flex().flex_wrap().gap_2().children(
            [
                (Some(1), "24 hours"),
                (Some(7), "7 days"),
                (Some(30), "30 days"),
                (None, "All recorded"),
            ]
            .into_iter()
            .map(|(days, label)| {
                choice(
                    &theme,
                    format!("usage-date-{label}"),
                    label.into(),
                    self.days == days,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.days = days;
                    this.reset(cx);
                }))
            }),
        );
        content = content
            .child(widgets::field_label(&theme, "Time range"))
            .child(dates);
        let mut models = vec![None];
        if let Some(s) = &self.snapshot {
            models.extend(s.models.iter().cloned().map(Some));
        }
        // Keep the active model selectable while a request is loading.
        if self.query.model.is_some() && !models.contains(&self.query.model) {
            models.push(self.query.model.clone());
        }
        content = content.child(widgets::field_label(&theme, "Model")).child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(models.into_iter().enumerate().map(|(i, model)| {
                    choice(
                        &theme,
                        format!("usage-model-{i}"),
                        model.clone().unwrap_or("All models".into()),
                        self.query.model == model,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.query.model = model.clone();
                        this.reset(cx);
                    }))
                })),
        );
        content = content
            .child(widgets::field_label(&theme, "Token type"))
            .child(div().flex().flex_wrap().gap_2().children(
                TokenKind::ALL.into_iter().enumerate().map(|(i, kind)| {
                    choice(
                        &theme,
                        format!("usage-kind-{i}"),
                        kind.label().into(),
                        self.query.token_kind == kind,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.query.token_kind = kind;
                        this.reset(cx);
                    }))
                }),
            ));
        content = content.child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(
                    [
                        "Overview",
                        "Tools",
                        "Instructions & rules",
                        "Trends & comparisons",
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(tab, label)| {
                        choice(
                            &theme,
                            format!("usage-tab-{tab}"),
                            label.into(),
                            self.tab == tab,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tab = tab;
                            cx.notify();
                        }))
                    }),
                )
                .child(
                    choice(&theme, "usage-refresh".into(), "Refresh".into(), false)
                        .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                ),
        );
        if self.loading {
            content = content.child(widgets::page_subtitle(&theme, "Loading usage…"));
        }
        if let Some(error) = &self.error {
            content = content.child(widgets::error_strip(&theme, error.clone()));
        }
        if let Some(snapshot) = &self.snapshot {
            content = content.child(widgets::section_card(&theme).child(
                widgets::card_row(&theme, true).child(format!(
                    "{} tokens · {} recorded runs",
                    snapshot.total, snapshot.runs
                )),
            ));
            if snapshot.unknown_breakdown > 0 {
                content=content.child(widgets::warning_strip(&theme,format!("{} measurements have no reported breakdown for this token type. Missing values are excluded, not measured zeroes.",snapshot.unknown_breakdown)));
            }
            if snapshot.runs == 0 {
                content=content.child(widgets::page_subtitle(&theme,if snapshot.recording_since.is_some(){"No recorded runs match these filters."}else{"No usage recorded yet. New agent runs will appear here when the harness reports usage."}));
            }
            match self.tab {
                0 => {
                    content = content.child(widgets::field_label(
                        &theme,
                        "Activities · most tokens first",
                    ));
                    for (i, g) in snapshot.activities.iter().enumerate() {
                        let activity = g.label.clone();
                        content = content.child(
                            widgets::card_row(&theme, i == 0)
                                .id(SharedString::from(format!("usage-activity-{i}")))
                                .cursor_pointer()
                                .child(rows(&theme, std::slice::from_ref(g), snapshot.total))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.query.activity = Some(activity.clone());
                                    this.reset(cx);
                                })),
                        );
                    }
                    if self.query.activity.is_some() {
                        content = content.child(
                            choice(
                                &theme,
                                "usage-all-activities".into(),
                                "Show all activities".into(),
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.query.activity = None;
                                this.reset(cx);
                            })),
                        )
                    }
                    content = content
                        .child(widgets::field_label(&theme, "Execution role"))
                        .child(rows(&theme, &snapshot.roles, snapshot.total));
                    content = content.child(widgets::field_label(
                        &theme,
                        "Contributing runs · newest first",
                    ));
                    for (i, run) in snapshot.details.iter().enumerate() {
                        let chat = run.chat_id.clone();
                        content = content.child(
                            widgets::card_row(&theme, i == 0)
                                .id(SharedString::from(format!("usage-run-{i}")))
                                .cursor_pointer()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(format!("{} · {}", run.activity, run.model))
                                        .child(widgets::page_subtitle(
                                            &theme,
                                            format!(
                                                "{} · {} · {}",
                                                format_time(run.at),
                                                run.status,
                                                run.role
                                            ),
                                        )),
                                )
                                .child(format!("{} tokens →", run.tokens))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(OpenUsageChat(chat.clone()))
                                })),
                        );
                    }
                    content = content.child(
                        div()
                            .flex()
                            .gap_2()
                            .when(self.query.offset > 0, |d| {
                                d.child(
                                    choice(&theme, "usage-prev".into(), "Previous".into(), false)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.query.offset =
                                                this.query.offset.saturating_sub(50);
                                            this.load(cx);
                                        })),
                                )
                            })
                            .when(snapshot.has_more, |d| {
                                d.child(
                                    choice(&theme, "usage-next".into(), "Next".into(), false)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.query.offset =
                                                this.query.offset.saturating_add(50);
                                            this.load(cx);
                                        })),
                                )
                            }),
                    );
                }
                1 | 2 => {
                    let tools = self.tab == 1;
                    content=content.child(widgets::page_subtitle(&theme,if tools{"Observed calls and payload bytes. Original harness responses and retained transcript results are separate stages; neither establishes what was billed or repeatedly included in context."}else{"Instruction bytes submitted to the harness, grouped by source revision. Hidden global/repository rules and actual repeated request inclusion are not reported by these harnesses."}));
                    content=content.child(widgets::page_subtitle(&theme,"Model filters select turns containing that model. Diagnostic bytes are turn-level and independent of token-type selection; they are not attributed to a specific model in mixed-model turns."));
                    let groups = snapshot
                        .diagnostics
                        .iter()
                        .filter(|d| {
                            if tools {
                                d.category.starts_with("tool") || d.category == "retainedToolResult"
                            } else {
                                d.category.starts_with("instruction") || d.category == "skill"
                            }
                        })
                        .collect::<Vec<_>>();
                    if groups.is_empty() {
                        content = content.child(widgets::page_subtitle(
                            &theme,
                            "No diagnostics recorded for these filters.",
                        ));
                    }
                    for (i, d) in groups.iter().enumerate() {
                        let chat = d.example_chat_id.clone();
                        content=content.child(widgets::card_row(&theme,i==0).id(SharedString::from(format!("usage-diagnostic-{i}"))).cursor_pointer().child(div().flex_1().min_w_0().child(format!("{} · {}",d.channel.as_deref().unwrap_or("instructions"),d.label)).child(widgets::page_subtitle(&theme,format!("{} · {} · {} observations · {} errors · {} repeats · {} ms",d.category,d.stage,d.observations,d.errors,d.repeated,d.elapsed_ms))).when_some(d.revision.clone(),|div,r|div.child(widgets::page_subtitle(&theme,format!("Revision {}",&r[..r.len().min(12)]))))).child(format!("{} bytes →",d.bytes)).on_click(cx.listener(move|_,_,_,cx|cx.emit(OpenUsageChat(chat.clone())))));
                    }
                }
                _ => {
                    content = content
                        .child(widgets::field_label(&theme, "Daily usage · UTC"))
                        .child(rows(&theme, &snapshot.days, snapshot.total));
                    content=content.child(widgets::page_subtitle(&theme,"Compare the same task and model under different configurations. Lower usage alone does not prove better results. Run details retain a configuration fingerprint; hidden prompt changes and cache warmness can affect comparisons."));
                    if !snapshot.context_samples.is_empty() {
                        content = content.child(widgets::field_label(
                            &theme,
                            "Latest context occupancy · not cumulative usage",
                        ));
                        for sample in snapshot.context_samples.iter().take(5) {
                            content = content.child(widgets::page_subtitle(
                                &theme,
                                format!(
                                    "{} · {} · {} / {} context tokens",
                                    format_time(sample.at),
                                    sample.role,
                                    sample
                                        .tokens
                                        .map(|n| n.to_string())
                                        .unwrap_or_else(|| "unknown".into()),
                                    sample
                                        .window
                                        .map(|n| n.to_string())
                                        .unwrap_or_else(|| "unknown".into())
                                ),
                            ));
                        }
                    }
                    for (i, cohort) in snapshot.configurations.iter().enumerate() {
                        let average = if cohort.completed == 0 {
                            "No completed samples".into()
                        } else {
                            format!(
                                "{} tokens per completed run",
                                cohort.completed_tokens / cohort.completed
                            )
                        };
                        content=content.child(widgets::card_row(&theme,i==0).child(div().flex_1().min_w_0().child(format!("{} · {}",cohort.model,cohort.activity)).child(widgets::page_subtitle(&theme,format!("{} · {} completed · {} failed/cancelled · {} partial",average,cohort.completed,cohort.failed,cohort.partial))).child(widgets::page_subtitle(&theme,format!("Configuration {}",cohort.fingerprint)))).child(format!("{} tokens",cohort.tokens)));
                    }
                    for (i, run) in snapshot.details.iter().enumerate() {
                        content = content.child(
                            widgets::card_row(&theme, i == 0)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(format!(
                                            "{} · {} · {}",
                                            format_time(run.at),
                                            run.model,
                                            run.status
                                        ))
                                        .child(widgets::page_subtitle(
                                            &theme,
                                            format!(
                                                "Configuration {}",
                                                &run.configuration
                                                    [..run.configuration.len().min(12)]
                                            ),
                                        )),
                                )
                                .child(format!("{} tokens", run.tokens)),
                        );
                    }
                }
            }
            content = content.child(widgets::field_label(&theme, "Coverage"));
            for note in &snapshot.coverage {
                content = content.child(widgets::page_subtitle(&theme, note.clone()));
            }
            content = content.child(widgets::page_subtitle(
                &theme,
                format!(
                    "Recording since {} · Updated {}",
                    snapshot
                        .recording_since
                        .map(format_time)
                        .unwrap_or_else(|| "no measurements".into()),
                    format_time(snapshot.updated_at)
                ),
            ));
        }
        let rail = popover::rail(self, "token-usage-scrollbar", &theme, cx);
        div()
            .id("token-usage-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("token-usage-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(content),
            )
            .children(rail)
    }
}
fn format_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "Unknown time".into())
}
