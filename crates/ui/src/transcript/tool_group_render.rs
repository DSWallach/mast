use super::*;

impl Transcript {
    pub(super) fn render_tool_group(
        &mut self,
        row_id: &SharedString,
        tools: &Arc<Vec<ToolItem>>,
        summary: &SharedString,
        auto_open: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut fold = self.folds.get(row_id).copied().unwrap_or_default();
        // Agent/spawn chips never fold: they are their own row, always open,
        // no "Called N tools" header — a running subagent stays visible.
        let collapses = tool_group_collapses(tools);
        let arrival_pending = !cx.reduce_motion()
            && self.tool_group_reveals.get(row_id).is_some_and(|reveal| {
                reveal.starts.iter().flatten().any(|start| {
                    Instant::now()
                        .checked_duration_since(*start)
                        .unwrap_or_default()
                        < TOOL_CONNECTOR_REVEAL.total()
                })
            });
        let effective_auto_open = auto_open || arrival_pending;
        let open = !collapses || fold.open.unwrap_or(effective_auto_open);
        if collapses {
            let reveal = self.tool_group_reveals.entry(row_id.clone()).or_default();
            if reveal
                .rendered_open
                .is_some_and(|previous| previous != open)
            {
                fold.from = reveal.rendered_height;
                fold.toggled_at = Some(Instant::now());
                fold.disclosure_at = fold.toggled_at;
                self.folds.insert(row_id.clone(), fold);
            }
            reveal.rendered_open = Some(open);
        }
        let active = collapses && auto_open;

        // A settled collapsed group has no visible body. Do not construct or
        // format thousands of hidden chips merely to clip them to zero height.
        // Keep the body during closing so the existing fold animation survives.
        let body_visible = open
            || (!cx.reduce_motion()
                && fold
                    .toggled_at
                    .is_some_and(|at| at.elapsed() < TOOL_FOLD.total()));
        let tools = if body_visible { tools.as_slice() } else { &[] };
        // Chips render their EFFECTIVE detail: the precomputed doc-resident
        // one, upgraded in place by a fetched sidecar blob (chat2-sync A3).
        // Resolved per paint (a HashMap probe per chip) so fetched content
        // needs no row rebuild — arrival is a cx.notify, like a fold toggle.
        let details: Vec<Option<Arc<ToolDetail>>> = tools
            .iter()
            .map(|tool| {
                // Spawn chips never expand — the subagent doc is the record
                // of what the tool did, and an inline body would only repeat
                // it. The whole chip is the "open that doc" click instead.
                if is_spawn_link(tool) {
                    return None;
                }
                // Among fetched blobs, the most recently REQUESTED one wins —
                // a tool can carry both a diff and an output ref, and the
                // user's last click decides which upgrade is showing.
                let mut best: Option<(u64, Arc<ToolDetail>)> = None;
                for blob_ref in [&tool.diff_ref, &tool.output_ref].into_iter().flatten() {
                    if let Some(BlobFetch::Ready(detail)) = self.blob_details.get(blob_ref) {
                        let order = self.blob_fetch_order.get(blob_ref).copied().unwrap_or(0);
                        if best.as_ref().is_none_or(|(o, _)| order > *o) {
                            best = Some((order, detail.clone()));
                        }
                    }
                }
                best.map(|(_, d)| d).or_else(|| tool.detail.clone())
            })
            .collect();
        // Full-invocation blocks — with them, EVERY chip expands: the click
        // always answers "what exactly was this call?", output or not.
        let invocations: Vec<Option<Arc<ToolDetail>>> = tools
            .iter()
            .map(|tool| tool.invocation.clone().filter(|_| !is_spawn_link(tool)))
            .collect();
        // Fetch affordance under each open detail whose full payload is still
        // sidecar-only: `(ref, label)`. Diff offered first (the richer
        // upgrade), then the output — a fetched ref hands the affordance to
        // the NEXT unfetched one instead of retiring it (both must stay
        // reachable when a tool has both).
        let affordances: Vec<Option<ChipAffordance>> = tools
            .iter()
            .map(|tool| {
                // The currently-displayed ref (same recency rule as
                // `details` above): its affordance is spent; any OTHER
                // Ready ref stays offered as a no-fetch toggle.
                let shown: Option<&SharedString> = {
                    let mut best: Option<(u64, &SharedString)> = None;
                    for blob_ref in [&tool.diff_ref, &tool.output_ref].into_iter().flatten() {
                        if matches!(self.blob_details.get(blob_ref), Some(BlobFetch::Ready(_))) {
                            let order = self.blob_fetch_order.get(blob_ref).copied().unwrap_or(0);
                            if best.is_none_or(|(o, _)| order > o) {
                                best = Some((order, blob_ref));
                            }
                        }
                    }
                    best.map(|(_, r)| r)
                };
                let candidates = [
                    (tool.diff_ref.as_ref(), "diff", None),
                    (tool.output_ref.as_ref(), "output", tool.output_bytes),
                ];
                for (blob_ref, what, bytes) in candidates {
                    let Some(blob_ref) = blob_ref else { continue };
                    let (label, failure) = match self.blob_details.get(blob_ref) {
                        Some(BlobFetch::Ready(_)) => {
                            if shown == Some(blob_ref) {
                                continue;
                            }
                            (format!("Show full {what}"), None)
                        }
                        Some(BlobFetch::Loading(_)) => (format!("Loading full {what}…"), None),
                        Some(BlobFetch::Failed(error)) => (
                            format!("Couldn't load full {what} — retry"),
                            Some(error.clone()),
                        ),
                        None => match bytes {
                            Some(b) => (format!("Show full {what} ({})", format_kb(b)), None),
                            None => (format!("Show full {what}"), None),
                        },
                    };
                    return Some(ChipAffordance {
                        blob_ref: blob_ref.clone(),
                        label: SharedString::from(label),
                        failure,
                    });
                }
                None
            })
            .collect();
        // Which chips have their detail block open (render-local, analytic —
        // the FINAL state; a mid-tween detail already counts as its target).
        let detail_folds: Vec<FoldState> = details
            .iter()
            .zip(&invocations)
            .enumerate()
            .map(|(ix, (detail, invocation))| {
                if detail.is_none() && invocation.is_none() {
                    return FoldState::default();
                }
                self.tool_details
                    .get(&SharedString::from(format!("{row_id}#d{ix}")))
                    .copied()
                    .unwrap_or_default()
            })
            .collect();
        let detail_opens: Vec<bool> = details
            .iter()
            .zip(&invocations)
            .zip(&detail_folds)
            .zip(tools.iter())
            .map(|(((detail, invocation), fold), tool)| {
                // A STREAMING thought chip defaults open (the live thinking
                // is the point); settled chips default closed. A user toggle
                // overrides either way.
                let default_open = tool.is_thought && !tool.resolved;
                (detail.is_some() || invocation.is_some()) && fold.open.unwrap_or(default_open)
            })
            .collect();
        let detail_highlights: Vec<Option<Arc<crate::changes::DiffHighlights>>> = details
            .iter()
            .enumerate()
            .map(|(ix, detail)| {
                detail
                    .as_deref()
                    .filter(|_| detail_opens[ix])
                    .and_then(|detail| self.tool_diff_highlight_for(row_id, ix, detail, cx))
            })
            .collect();
        let base_row_height = if collapses {
            TOOL_TREE_ROW_HEIGHT
        } else {
            CHIP_HEIGHT
        };
        let mut motion_active = false;
        let row_heights: Vec<f32> = details
            .iter()
            .zip(&invocations)
            .zip(&affordances)
            .zip(&detail_opens)
            .zip(&detail_folds)
            .map(|((((detail, invocation), affordance), open), fold)| {
                let target = if *open {
                    base_row_height
                        + invocation.as_deref().map_or(0.0, detail_height)
                        + detail.as_deref().map_or(0.0, detail_height)
                        + if affordance.is_some() {
                            BLOB_AFFORDANCE_HEIGHT
                        } else {
                            0.0
                        }
                } else {
                    base_row_height
                };
                if !cx.reduce_motion()
                    && let Some(at) = fold.toggled_at {
                        let t = TOOL_FOLD
                            .curve
                            .eval(at.elapsed().as_secs_f32() / TOOL_FOLD.total().as_secs_f32());
                        if t < 1.0 {
                            motion_active = true;
                        }
                        return motion::lerp(
                            fold.from + base_row_height - CHIP_CARD_HEIGHT,
                            target,
                            t,
                        );
                    }
                target
            })
            .collect();
        let reduce_motion = cx.reduce_motion();
        let now = Instant::now();
        let reveal_progress: Vec<f32> = (0..tools.len())
            .map(|ix| {
                let start = self
                    .tool_group_reveals
                    .get(row_id)
                    .and_then(|reveal| reveal.starts.get(ix))
                    .copied()
                    .flatten();
                tool_row_reveal_progress(start, now, reduce_motion)
            })
            .collect();
        let connector_progress: Vec<f32> = (0..tools.len())
            .map(|ix| {
                let start = self
                    .tool_group_reveals
                    .get(row_id)
                    .and_then(|reveal| reveal.starts.get(ix))
                    .copied()
                    .flatten();
                tool_connector_reveal_progress(start, now, reduce_motion)
            })
            .collect();
        let header_reveal = tool_row_reveal_progress(
            self.tool_group_reveals
                .get(row_id)
                .and_then(|reveal| reveal.header_started_at),
            now,
            reduce_motion,
        );
        if header_reveal < 1.0
            || reveal_progress.iter().any(|progress| *progress < 1.0)
            || connector_progress.iter().any(|progress| *progress < 1.0)
        {
            motion_active = true;
        }
        let revealed_height = CHIPS_TOP_PAD
            + row_heights
                .iter()
                .zip(&reveal_progress)
                .map(|(height, progress)| height * progress)
                .sum::<f32>();
        let viewport_height = revealed_height;
        let target = if open { viewport_height } else { 0.0 };
        let shimmer_phase = if active && !reduce_motion {
            motion::pulse_lease(cx.entity_id(), cx);
            self.tool_group_reveals
                .get(row_id)
                .and_then(|reveal| reveal.shimmer_started_at)
                .map(|start| tool_title_shimmer_phase(start, now))
        } else {
            None
        };
        let disclosure_progress = if reduce_motion {
            if open { 1.0 } else { 0.0 }
        } else {
            tool_disclosure_progress(open, fold, now)
        };

        let toggle_id = row_id.clone();
        // A quiet summary sits above the activity rail; its chevron occupies
        // the same gutter as the rounded task-tree elbows below it.
        let header = crate::components::conversation_tool_group_header(
            theme, SharedString::from(format!("{row_id}-hdr")), TOOL_GROUP_HEADER_HEIGHT,
            TOOL_LABEL_SIZE, TOOL_LABEL_LINE_HEIGHT,
        )
            // Quiet even when children failed: agents routinely have failed
            // probes mid-work, and a red HEADER read as "this whole step
            // broke" (user report). Failures still show on the individual
            // chips (destructive tint, zeron tool-chip.tsx) and in the
            // summary's "· N failed" count.
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_fold(toggle_id.clone(), viewport_height, effective_auto_open);
                cx.notify();
            }))
            .child(
                div()
                    // Keep the title adjacent to its disclosure affordance.
                    .w(px(22.0))
                    .h(px(18.0))
                    .flex_none()
                    .relative()
                    .child(
                        crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                            .absolute()
                            .left(px(ACTIVITY_TRUNK_X - 7.0))
                            .top(px(2.0))
                            .size(px(14.0))
                            .with_transformation(gpui::Transformation::rotate(gpui::radians(
                                -std::f32::consts::FRAC_PI_2 * (1.0 - disclosure_progress),
                            )))
                            .text_color(theme.text_muted),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .h(px(TOOL_LABEL_LINE_HEIGHT))
                    .flex()
                    .items_center()
                    .truncate()
                    .child(tool_group_title(summary.clone(), shimmer_phase, theme)),
            );

        let chips = div()
            .pt(px(CHIPS_TOP_PAD))
            .flex()
            .flex_col()
            .gap(px(CHIP_GAP))
            .children(tools.iter().enumerate().map(|(ix, tool)| {
                let reveal = reveal_progress[ix];
                let connector_reveal = connector_progress[ix];
                let content_reveal = tool_connector_parts(connector_reveal, ix > 0).1;
                let continuation_reveal =
                    tool_connector_continuation(connector_progress.get(ix + 1).copied());
                let row_height = row_heights[ix];
                // Spawn chips are LINKS, not accordions: the click opens the
                // subagent's transcript as a right-pane tab (the shell hosts
                // the surface — the chip only announces which doc it indexes).
                if let Some(doc_id) = tool.subagent_ref.clone().filter(|_| is_spawn_link(tool)) {
                    let chat_id = self.chat_id.clone().unwrap_or_default();
                    let title = subagent_tab_title(&tool.call);
                    let frozen = matches!(
                        tool.subagent_status,
                        Some(SubagentStatus::Done) | Some(SubagentStatus::Failed)
                    );
                    return subagent_chip(
                        tool,
                        SharedString::from(format!("{row_id}#s{ix}")),
                        cx.listener(move |_, _, _, cx| {
                            cx.emit(TranscriptEvent::OpenSubagent {
                                chat_id: chat_id.clone(),
                                doc_id: doc_id.to_string(),
                                title: title.to_string(),
                                frozen,
                            });
                        }),
                        collapses,
                        theme,
                        cx.entity_id(),
                        cx,
                    );
                }
                let detail = details[ix].clone();
                let invocation = invocations[ix].clone();
                if detail.is_none() && invocation.is_none() {
                    return reveal_tool_row(
                        tool_chip(
                            tool,
                            collapses,
                            ix > 0,
                            ix + 1 < tools.len(),
                            content_reveal,
                            connector_reveal,
                            continuation_reveal,
                            theme,
                            cx.entity_id(),
                            cx,
                        ),
                        row_height,
                        reveal,
                    );
                }
                let affordance = affordances[ix].clone();
                let open = detail_opens[ix];
                let dfold = detail_folds[ix];
                let key = SharedString::from(format!("{row_id}#d{ix}"));
                // Ordinary tools expand into muted text along the same column.
                // Subagent fallbacks retain their card; explicit heights keep
                // the row and group fold animations in sync.
                let animating = dfold.epoch > 0
                    && dfold
                        .toggled_at
                        .is_some_and(|at| at.elapsed() < FOLD_TWEEN_WINDOW);
                let toggle_key = key.clone();
                // Edit chips should disclose the actual change in one click.
                // The replicated row usually carries only stats + a sidecar
                // ref, so opening the chip starts that diff fetch immediately
                // instead of making the stats panel ask for a second click.
                let diff_on_open = edit_diff_ref_on_open(tool);
                let mut card = div()
                    .my(px((base_row_height - CHIP_CARD_HEIGHT) / 2.0))
                    .when(collapses, |el| el.ml(px(ACTIVITY_TEXT_GAP)))
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .when(!collapses, |card| {
                        card.rounded(px(9.0))
                            .border_1()
                            .border_color(crate::theme::hairline(0.07))
                            .bg(crate::theme::ink(0.03))
                    })
                    .child(
                        div()
                            .id(key.clone())
                            .h(px(if collapses {
                                CHIP_CARD_HEIGHT
                            } else {
                                CHIP_HEADER_HEIGHT
                            }))
                            .flex_none()
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                let entry =
                                    this.tool_details.entry(toggle_key.clone()).or_default();
                                let currently_open = entry.open.unwrap_or(open);
                                let opening = !currently_open;
                                entry.from = row_height - base_row_height + CHIP_CARD_HEIGHT;
                                entry.open = Some(opening);
                                entry.epoch += 1;
                                entry.toggled_at = Some(Instant::now());
                                if opening && let Some(blob_ref) = diff_on_open.clone() {
                                    this.spawn_blob_fetch(blob_ref, cx);
                                }
                                cx.notify();
                            }))
                            .child(chip_header(tool, open, theme, cx.entity_id(), cx)),
                    );
                // The body stays mounted while the close tween shrinks over it.
                // Invocation first (what was asked), then output/diff (what
                // came back), separated by a small gap.
                if open || animating {
                    let mut panel = div()
                        .flex_none()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .overflow_hidden();
                    if let Some(invocation) = invocation.as_deref() {
                        panel = panel
                            .child(
                                div()
                                    .h(px(DETAIL_SEPARATOR))
                                    .flex_none()
                                    .when(!collapses, |line| line.bg(crate::theme::hairline(0.06))),
                            )
                            .child(detail_body(
                                invocation,
                                None,
                                format!("{key}-invocation").into(),
                                theme,
                            ));
                    }
                    if let Some(detail) = detail.as_deref() {
                        panel = panel
                            .child(
                                div()
                                    .h(px(DETAIL_SEPARATOR))
                                    .flex_none()
                                    .when(!collapses, |line| line.bg(crate::theme::hairline(0.06))),
                            )
                            .child(detail_body(
                                detail,
                                detail_highlights[ix].clone(),
                                format!("{key}-detail").into(),
                                theme,
                            ));
                    }
                    if let Some(ChipAffordance {
                        blob_ref,
                        label,
                        failure,
                    }) = affordance
                    {
                        let loading = matches!(
                            self.blob_details.get(&blob_ref),
                            Some(BlobFetch::Loading(_))
                        );
                        let mut row = div()
                            .id(SharedString::from(format!("{key}-blob")))
                            .h(px(BLOB_AFFORDANCE_HEIGHT))
                            .flex_none()
                            .flex()
                            .items_center()
                            .occlude()
                            .text_size(px(TOOL_TEXT_SIZE))
                            .text_color(theme.text_faint)
                            .child(label);
                        if let Some(error) = failure {
                            row = row.tooltip(move |_, cx| {
                                cx.new(|_| BlobFetchErrorTooltip(error.clone())).into()
                            });
                        }
                        if !loading {
                            row = row
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme.text_muted))
                                .on_mouse_down(MouseButton::Left, |_, window, _| {
                                    window.prevent_default();
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.spawn_blob_fetch(blob_ref.clone(), cx);
                                    cx.notify();
                                }));
                        }
                        panel = panel.child(row);
                    }
                    card = card.child(panel);
                }
                let card = card.h(px(row_height - base_row_height + CHIP_CARD_HEIGHT));
                let card = div().min_w_0().flex_1().child(card);
                let row = div()
                    .w_full()
                    .flex_none()
                    .flex()
                    .flex_row()
                    // Stretch the line alongside the expanded text.
                    .when(collapses, |row| {
                        row.child(activity_rail(
                            tool,
                            ix > 0,
                            ix + 1 < tools.len(),
                            connector_reveal,
                            continuation_reveal,
                            base_row_height,
                            theme,
                        ))
                    })
                    .child(card.when(collapses && content_reveal < 1.0, |card| {
                        card.relative()
                            .top(px(4.0 * (1.0 - content_reveal)))
                            .opacity(content_reveal)
                    }))
                    .into_any_element();
                reveal_tool_row(row, row_height, reveal)
            }));

        let chips = chips.into_any_element();

        // Evaluate the group on the same clock as its disclosure. This also
        // gives completion a gentle close after the last arrival finishes,
        // without restarting an element animation when the list remounts it.
        let body_height = if !reduce_motion {
            fold.toggled_at
                .map(|at| {
                    let t = TOOL_FOLD.curve.eval(
                        now.saturating_duration_since(at).as_secs_f32()
                            / TOOL_FOLD.total().as_secs_f32(),
                    );
                    if t < 1.0 {
                        motion_active = true;
                    }
                    motion::lerp(fold.from, target, t)
                })
                .unwrap_or(target)
        } else {
            target
        };
        if let Some(reveal) = self.tool_group_reveals.get_mut(row_id) {
            reveal.rendered_height = body_height;
        }
        let body: AnyElement = if !collapses {
            chips.into_any_element()
        } else {
            div()
                .overflow_hidden()
                .h(px(body_height))
                .child(chips)
                .into_any_element()
        };

        let view = cx.entity_id();
        div()
            .relative()
            .flex()
            .flex_col()
            // Tool summaries and cards are code-adjacent chrome. Detail bodies
            // retain their explicit mono/diff typography below this boundary.
            .font_family(theme.font_sans_fixed.clone())
            .when(collapses, |el| {
                el.child(reveal_tool_row(
                    header.into_any_element(),
                    TOOL_GROUP_HEADER_HEIGHT,
                    header_reveal,
                ))
            })
            .child(body)
            .when(motion_active, |group| {
                group.child(
                    canvas(
                        |_, _, _| (),
                        move |_, _, window, _| {
                            window.on_next_frame(move |_, cx| cx.notify(view));
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
            })
            .into_any_element()
    }
}
