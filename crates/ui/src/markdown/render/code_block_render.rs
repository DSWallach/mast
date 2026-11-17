use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn render_code_block(
    language: Option<&str>,
    code: &str,
    top_ix: usize,
    ix: usize,
    opts: &RenderOptions,
    theme: &Theme,
    highlight: CodeHighlight,
) -> AnyElement {
    let panel_theme = crate::components::conversation_code_theme(theme);
    let theme = &panel_theme;
    if is_mermaid(language)
        && let Some(handler) = opts.media.as_ref().and_then(|media| media.diagram.as_ref())
    {
        let frame_id: SharedString = format!("{}-mermaid-{ix}", opts.row_key).into();
        let diagram = handler(code, frame_id.clone(), theme);
        let toggle = diagram.toggle_source.clone();
        let toggle_action = code_icon_action(
            format!("{frame_id}-source-toggle").into(),
            if diagram.show_source {
                "Show diagram"
            } else {
                "Show source"
            },
            if diagram.show_source {
                crate::icons::EYE
            } else {
                crate::icons::FILE_CODE
            },
            Rc::new(move |window, cx| toggle(window, cx)),
            theme,
        );
        if diagram.show_source {
            return render_code_block_source_with_actions(
                language,
                code,
                top_ix,
                ix,
                opts,
                theme,
                highlight,
                vec![toggle_action],
            );
        }
        let mut actions = vec![toggle_action];
        actions.extend(code_copy_button(code, ix, opts, theme));
        return crate::components::conversation_code_frame(frame_id, language, actions, diagram.body, theme)
            .into_any_element();
    }
    render_code_block_source(language, code, top_ix, ix, opts, theme, highlight)
}

pub(super) fn code_icon_action(
    id: SharedString,
    label: &'static str,
    icon_path: &'static str,
    handler: Rc<dyn Fn(&mut Window, &mut gpui::App)>,
    theme: &Theme,
) -> AnyElement {
    crate::components::conversation_code_icon_action(theme, id, icon_path, CODE_ACTION_SIZE)
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            handler(window, cx);
        })
        .tooltip(move |_, cx| cx.new(move |_| CodeBlockTooltip(label)).into())
        .into_any_element()
}

pub(super) fn code_copy_button(
    code: &str,
    ix: usize,
    opts: &RenderOptions,
    theme: &Theme,
) -> Option<AnyElement> {
    opts.copy.clone().map(|copy| {
        let copied = copy.copied_ix == Some(ix);
        let code_text: SharedString = code.to_string().into();
        let handler = copy.handler.clone();
        let fade_key = format!("{}-copy{ix}", opts.row_key);
        crate::components::conversation_code_copy_action(theme, SharedString::from(fade_key), copied, CODE_ACTION_SIZE)
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                handler(ix, code_text.clone(), window, cx);
            })
            .into_any_element()
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_code_block_source(
    language: Option<&str>,
    code: &str,
    top_ix: usize,
    ix: usize,
    opts: &RenderOptions,
    theme: &Theme,
    highlight: CodeHighlight,
) -> AnyElement {
    render_code_block_source_with_actions(
        language,
        code,
        top_ix,
        ix,
        opts,
        theme,
        highlight,
        Vec::new(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_code_block_source_with_actions(
    language: Option<&str>,
    code: &str,
    top_ix: usize,
    ix: usize,
    opts: &RenderOptions,
    theme: &Theme,
    highlight: CodeHighlight,
    mut extra_actions: Vec<AnyElement>,
) -> AnyElement {
    let mono = font(theme.font_mono.clone());
    // Per-line strings + runs through the cross-frame cache (validity: code
    // length + highlight slice identity — a fresh highlight Arc re-derives).
    let hl_key = highlight.map_or((0, 0), |h| (h.as_ptr() as usize, h.len()));
    let build = || {
        let lines: Vec<(SharedString, Vec<TextRun>)> = code
            .split('\n')
            .enumerate()
            .map(|(li, line)| {
                let spans = highlight
                    .and_then(|h| h.get(li))
                    .map(|t| &t[..])
                    .unwrap_or(&[]);
                (
                    SharedString::from(line.to_string()),
                    runs_for_syntax_line(line, spans, &mono, theme),
                )
            })
            .collect();
        Rc::new(CachedCode {
            code_len: code.len(),
            hl_key,
            lines,
        })
    };
    let cached: Rc<CachedCode> = match &opts.cache {
        Some(cache) => {
            let mut cache = cache.borrow_mut();
            cache.sync_style();
            let entry = cache
                .code
                .entry((opts.row_key.clone(), top_ix, ix))
                .or_insert_with(&build);
            if entry.code_len != code.len() || entry.hl_key != hl_key {
                *entry = build();
            }
            entry.clone()
        }
        None => build(),
    };
    // Streaming veil over appended code, tracked on the whole code text and
    // sliced per line below (paint-only run recolor — heights stay exact).
    let veil_spans = match &opts.veil {
        Some(veil) => veil.borrow_mut().advance(ix, code, opts.now),
        None => Vec::new(),
    };
    let scroll_id: SharedString = format!("{}-code{ix}", opts.row_key).into();
    let sel_wash = selection_wash(theme);
    // Mermaid source is not searchable (its diagram view has no text), so
    // find never highlights it — the count side skips it too.
    let find = opts
        .find
        .clone()
        .filter(|_| !is_mermaid(language))
        .map(|find| FindPaint {
            find,
            row_key: opts.row_key.clone(),
        });
    let code_ui = opts.code.as_ref().and_then(|code| code.get(&ix)).cloned();
    let fit_content = code_ui.as_ref().is_some_and(|ui| ui.fit_content);

    let fit_button = code_ui.as_ref().map(|ui| {
        let toggle = ui.toggle_fit.clone();
        let fade_key = format!("{}-fit{ix}", opts.row_key);
        let base = if fit_content {
            crate::theme::ink(0.09)
        } else {
            gpui::transparent_black()
        };
        let hover = crate::theme::ink(if fit_content { 0.13 } else { 0.08 });
        div()
            .id(SharedString::from(fade_key.clone()))
            .size(px(CODE_ACTION_SIZE))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(crate::motion::hover_blend(&fade_key, base, hover))
            .on_hover(crate::motion::hover_listener(fade_key))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                toggle(window, cx);
            })
            .tooltip(move |_, cx| {
                cx.new(move |_| {
                    CodeBlockTooltip(if fit_content {
                        "Use horizontal scrolling"
                    } else {
                        "Fit content"
                    })
                })
                .into()
            })
            .child(
                crate::icons::icon(crate::icons::WRAP_TEXT)
                    .size(px(13.0))
                    .text_color(theme.text_muted),
            )
    });
    let mut actions = Vec::new();
    actions.extend(fit_button.map(IntoElement::into_any_element));
    actions.append(&mut extra_actions);
    actions.extend(code_copy_button(code, ix, opts, theme));

    let lines = div()
        .map(|el| {
            if fit_content {
                el.w_full().min_w_0()
            } else {
                el.min_w_full().flex_none()
            }
        })
        .px(crate::typography::ui_rems(CODE_PADDING_X))
        .py(crate::typography::ui_rems(CODE_PADDING_Y))
        .font_family(theme.font_mono.clone())
        .text_size(px(theme.code_font_size))
        .line_height(px(theme.code_font_size * CODE_LINE_HEIGHT_RATIO))
        .map(|el| {
            if fit_content {
                el.whitespace_normal()
            } else {
                el.whitespace_nowrap()
            }
        })
        .flex()
        .flex_col()
        .children((0..cached.lines.len()).scan(0usize, move |off, li| {
            let (line, runs) = &cached.lines[li];
            let start = *off;
            *off = start + line.len() + 1; // +1 for the '\n'
            let local = slice_spans(&veil_spans, start, start + line.len());
            let runs = apply_veil(runs.clone(), &local);
            let key = code_line_selection_key(&opts.row_key, ix, li);
            Some(
                div()
                    .map(|el| {
                        if fit_content {
                            el.w_full()
                                .min_w_0()
                                .min_h(px(theme.code_font_size * CODE_LINE_HEIGHT_RATIO))
                        } else {
                            el.h(px(theme.code_font_size * CODE_LINE_HEIGHT_RATIO))
                                .flex_none()
                        }
                    })
                    .child(selectable_text_element(
                        key,
                        line.clone(),
                        runs,
                        sel_wash,
                        find.clone(),
                    )),
            )
        }));

    let body: AnyElement = if let Some(ui) = code_ui.as_ref() {
        if fit_content {
            div()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .child(lines)
                .into_any_element()
        } else {
            let mut scroller = div()
                .id(scroll_id)
                .w_full()
                .min_w_0()
                .flex()
                .overflow_x_scroll()
                .track_scroll(&ui.scroll)
                .child(lines);
            // A vertical wheel over code must keep bubbling to the transcript;
            // only a true horizontal gesture moves this local viewport.
            scroller.style().restrict_scroll_to_axis = Some(true);
            scroller.into_any_element()
        }
    } else {
        // Non-transcript previews keep their existing native scroll behavior.
        div()
            .id(scroll_id)
            .w_full()
            .overflow_x_scroll()
            .child(lines)
            .into_any_element()
    };

    let scrollbar = (!fit_content)
        .then(|| code_ui.as_ref())
        .flatten()
        .and_then(|ui| {
            let bar = ui.scrollbar.as_ref()?;
            let metrics = bar.metrics;
            let hover = bar.hover.clone();
            let press = bar.press.clone();
            let release_up = bar.release.clone();
            let release_out = bar.release.clone();
            let thumb_height = if bar.active {
                crate::popover::MENU_SCROLLBAR_HOVER_THUMB_WIDTH
            } else {
                crate::popover::MENU_SCROLLBAR_THUMB_WIDTH
            };
            Some(
                div()
                    .id(SharedString::from(format!("{}-scrollbar", ui.key)))
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .bottom(px(0.0))
                    .h(px(CODE_SCROLLBAR_HIT_HEIGHT))
                    .on_hover(move |hovered, window, cx| hover(*hovered, window, cx))
                    .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
                        press(event.position.x, window, cx);
                    })
                    .on_drag(
                        CodeScrollbarDrag {
                            key: ui.key.clone(),
                        },
                        |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.new(|_| CodeScrollbarDragGhost)
                        },
                    )
                    .on_mouse_up(gpui::MouseButton::Left, move |_, window, cx| {
                        release_up(window, cx)
                    })
                    .on_mouse_up_out(gpui::MouseButton::Left, move |_, window, cx| {
                        release_out(window, cx)
                    })
                    .child(
                        div()
                            .absolute()
                            .left(px(
                                crate::popover::MENU_SCROLLBAR_TRACK_INSET + metrics.thumb_left
                            ))
                            .bottom(px(2.0))
                            .w(px(metrics.thumb_width))
                            .h(px(thumb_height))
                            .rounded(px(thumb_height / 2.0))
                            .bg(theme
                                .text_faint
                                .opacity(if bar.active { 0.68 } else { 0.5 })),
                    ),
            )
        });

    let frame_body = div().w_full().relative().child(body).children(scrollbar);
    let mut block = crate::components::conversation_code_frame(
        format!("{}-code-frame{ix}", opts.row_key).into(),
        language,
        actions,
        frame_body.into_any_element(),
        theme,
    );
    if let Some(ui) = code_ui {
        let viewport_hover = ui.viewport_hover.clone();
        let drag_move = ui.drag_move.clone();
        let drag_key = ui.key;
        block = block
            .on_hover(move |hovered, window, cx| viewport_hover(*hovered, window, cx))
            .on_drag_move(
                move |event: &gpui::DragMoveEvent<CodeScrollbarDrag>, window, cx| {
                    let Some(drag) = event.dragged_item().downcast_ref::<CodeScrollbarDrag>()
                    else {
                        return;
                    };
                    if drag.key == drag_key {
                        drag_move(event.event.position.x, window, cx);
                    }
                },
            );
    }
    block.into_any_element()
}
