use super::*;

/// Build the block rows of one (already continuation-joined) entry.
///
/// `parse` maps `(part_key, text)` to a block tree — the entity supplies
/// incremental parsers for live parts and a cache for complete ones; tests pass
/// a plain `parse_full`.
pub fn rows_for_entry(
    entry: &SessionMessageEntry,
    pending: bool,
    parse: &mut dyn FnMut(&str, &str) -> Arc<BlockTree>,
) -> Vec<Row> {
    #[cfg(test)]
    FORBID_ROW_PREPARATION
        .with(|forbidden| assert!(!forbidden.get(), "row preparation ran on the UI thread"));
    let mut rows: Vec<Row> = Vec::new();
    let streaming = entry.status == Some(MessageStatus::Streaming);
    let entry_id: SharedString = entry.id.clone().into();

    if entry.role == MessageRole::User {
        let raw: String = entry
            .parts
            .iter()
            .filter_map(|p| match p {
                MessagePart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        // Attachment refs ride the plain text (the `withAttachments`
        // transport); split them back out for the thumbnail strip.
        let (source, body) = crate::message_source::classify(&raw);
        let parsed = crate::attachments::parse_user_message_images(body);
        // File mentions render as chips here too, not just in the composer.
        // The projection is pure over the text, so the raw-length row version
        // below stays a valid cache/diff key.
        // Lifted before the mention projection, so a comment body's own
        // Markdown never lands in the bubble.
        let (body, badges) = crate::badges::split(&parsed.text);
        let (text, mentions) = crate::code_ref::project_sent_refs(&body)
            .or_else(|| crate::composer::sent_mention_display(&body))
            .unwrap_or_else(|| (body, Vec::new()));
        let copy_text = (!text.trim().is_empty()).then(|| SharedString::from(text.clone()));
        return vec![Row {
            id: entry.id.clone().into(),
            version: (raw.len() as u64) << 1 | pending as u64,
            turn_start: true,
            kind: RowKind::User {
                source,
                text: text.into(),
                mentions: Arc::new(mentions),
                attachments: Arc::new(parsed.attachments),
                badges: Arc::new(badges),
                pending,
            },
            entry_id,
            // User rows always carry the strip (chat-view.tsx: whenever
            // `createdAt` exists — the optimistic echo included).
            timestamp: Some(entry.created_at),
            copy_text,
        }];
    }

    // Assistant/system: split parts into block rows, folding consecutive
    // ordinary tools. Agent/spawn chips flush into their own group so they
    // never share a collapse with Reads/Runs.
    let last_part_ix = entry.parts.len().saturating_sub(1);
    let mut group_ix = 0usize;
    let mut pending_group: Vec<ToolItem> = Vec::new();
    let mut group_last_part_ix = 0usize;

    let flush_group =
        |rows: &mut Vec<Row>, group: &mut Vec<ToolItem>, group_ix: &mut usize, last_ix: usize| {
            if group.is_empty() {
                return;
            }
            let tools = std::mem::take(group);
            let auto_open = streaming && last_ix == last_part_ix;
            rows.push(Row {
                id: format!("{}#g{}", entry.id, group_ix).into(),
                version: tool_fingerprint(&tools, auto_open),
                turn_start: false,
                kind: RowKind::ToolGroup {
                    summary: tool_group_summary(&tools).into(),
                    tools: Arc::new(tools),
                    auto_open,
                },
                entry_id: entry.id.clone().into(),
                timestamp: None,
                copy_text: None,
            });
            *group_ix += 1;
        };

    for (part_ix, part) in entry.parts.iter().enumerate() {
        match part {
            MessagePart::Tool {
                call,
                is_error,
                resolved,
                output,
                diff,
                output_ref,
                output_bytes,
                diff_ref,
                diff_stats,
                subagent_ref,
                subagent_status,
                subagent_tail,
                ..
            } => {
                let item = ToolItem {
                    part_id: part.id().to_owned(),
                    call: call.clone(),
                    is_error: *is_error,
                    resolved: *resolved,
                    detail: tool_detail(output.as_deref(), diff.as_ref(), diff_stats.as_deref())
                        .map(Arc::new),
                    invocation: call_block(call).map(Arc::new),
                    output_ref: output_ref.clone().map(SharedString::from),
                    output_bytes: *output_bytes,
                    diff_ref: diff_ref.clone().map(SharedString::from),
                    subagent_ref: subagent_ref.clone().map(SharedString::from),
                    subagent_status: *subagent_status,
                    subagent_tail: subagent_tail.clone().map(SharedString::from),
                    is_thought: false,
                };
                // Agent chips don't share a fold with ordinary tools: flush
                // whenever the genus flips so each group is uniform.
                if pending_group
                    .first()
                    .is_some_and(|head| is_agent_tool(head) != is_agent_tool(&item))
                {
                    flush_group(
                        &mut rows,
                        &mut pending_group,
                        &mut group_ix,
                        group_last_part_ix,
                    );
                }
                pending_group.push(item);
                group_last_part_ix = part_ix;
            }
            // Thinking rides the SAME accordion as the tools around it
            // (user request) — a thought chip in the group, not its own row.
            MessagePart::Reasoning { id: part_id, text } => {
                if text.trim().is_empty() {
                    continue;
                }
                // Live only while it is the tail of a streaming reply — once
                // text or a tool follows, the thought is finished even though
                // the entry still streams.
                let live = streaming && part_ix == last_part_ix;
                // The same parse wiring as text parts: incremental while
                // streaming, hanging inline markers mended for display, the
                // settled cache once complete.
                let tree = parse(&format!("{}#{}", entry.id, part_id), text);
                let item = thought_item(part_id, &tree, live);
                // Thoughts join ordinary tool groups; agent (spawn-link)
                // groups stay pure, exactly like the tool genus rule.
                if pending_group.first().is_some_and(is_agent_tool) {
                    flush_group(
                        &mut rows,
                        &mut pending_group,
                        &mut group_ix,
                        group_last_part_ix,
                    );
                }
                pending_group.push(item);
                group_last_part_ix = part_ix;
            }
            other => {
                flush_group(
                    &mut rows,
                    &mut pending_group,
                    &mut group_ix,
                    group_last_part_ix,
                );
                match other {
                    MessagePart::Text { id: part_id, text } => {
                        if text.trim().is_empty() {
                            continue;
                        }
                        let key = format!("{}#{}", entry.id, part_id);
                        let fragments = assistant_text_fragments(text, streaming);
                        let segmented = fragments.len() > 1
                            || matches!(
                                fragments.first(),
                                Some(AssistantTextFragment::Visualization(_))
                            );
                        for (fragment_ix, fragment) in fragments.into_iter().enumerate() {
                            match fragment {
                                AssistantTextFragment::Markdown(markdown) => {
                                    if markdown.trim().is_empty() {
                                        continue;
                                    }
                                    // Preserve the ordinary part key for a leading text
                                    // fragment, so an artifact appended during streaming
                                    // does not re-key already-settled Markdown rows.
                                    let fragment_key = if segmented && fragment_ix > 0 {
                                        format!("{key}#s{fragment_ix}")
                                    } else {
                                        key.clone()
                                    };
                                    let tree = parse(&fragment_key, markdown);
                                    // Live and completed parts split identically — one row
                                    // per top-level block, same ids, so the live→complete
                                    // handoff never changes row identity. The version is a
                                    // content hash of the block's bytes (LSB = streaming),
                                    // so a commit only splices rows whose bytes actually
                                    // changed — the settled prefix of a live reply is
                                    // untouched (and its render caches stay valid).
                                    for block_ix in 0..tree.blocks.len() {
                                        let range = &tree.blocks[block_ix].range;
                                        let end = range.end.min(markdown.len());
                                        let bytes = markdown
                                            .as_bytes()
                                            .get(range.start.min(end)..end)
                                            .unwrap_or_default();
                                        let version = (fnv1a(bytes) << 1) | streaming as u64;
                                        rows.push(Row {
                                            id: format!("{fragment_key}.{block_ix}").into(),
                                            version,
                                            turn_start: false,
                                            entry_id: entry_id.clone(),
                                            timestamp: None,
                                            copy_text: None,
                                            kind: if streaming {
                                                RowKind::LiveMarkdown {
                                                    tree: tree.clone(),
                                                    block_ix,
                                                }
                                            } else {
                                                RowKind::Markdown {
                                                    tree: tree.clone(),
                                                    block_ix,
                                                }
                                            },
                                        });
                                    }
                                }
                                AssistantTextFragment::Visualization(artifact) => {
                                    let version = artifact.as_ref().map_or(0, |artifact| {
                                        fnv1a(
                                            format!(
                                                "{}\0{}\0{}",
                                                artifact.path, artifact.title, artifact.wide
                                            )
                                            .as_bytes(),
                                        )
                                    });
                                    rows.push(Row {
                                        id: format!("{key}#v{fragment_ix}").into(),
                                        version: (version << 1) | streaming as u64,
                                        turn_start: false,
                                        entry_id: entry_id.clone(),
                                        timestamp: None,
                                        copy_text: None,
                                        kind: RowKind::Visualization { artifact },
                                    });
                                }
                            }
                        }
                    }
                    MessagePart::Image {
                        id: part_id,
                        path,
                        name,
                        mime_type,
                    } => {
                        rows.push(Row {
                            id: format!("{}#{}", entry.id, part_id).into(),
                            version: fnv1a(
                                format!("{}\0{path}\0{name}\0{mime_type}", entry.device_id)
                                    .as_bytes(),
                            ),
                            turn_start: false,
                            entry_id: entry_id.clone(),
                            timestamp: None,
                            copy_text: None,
                            kind: RowKind::GeneratedImage {
                                owner: entry.device_id.clone(),
                                path: path.clone(),
                                name: name.clone(),
                                mime_type: mime_type.clone(),
                            },
                        });
                    }
                    MessagePart::Input {
                        id: part_id,
                        questions,
                        resolved,
                        ..
                    } => {
                        // Model-generated header onto the one-line chip.
                        let header: SharedString = single_line(
                            &questions
                                .first()
                                .map(|q| q.header.clone())
                                .unwrap_or_else(|| "Question".to_string()),
                        )
                        .into();
                        rows.push(Row {
                            id: format!("{}#{}", entry.id, part_id).into(),
                            version: fnv1a(header.as_bytes()) << 1 | *resolved as u64,
                            turn_start: false,
                            kind: RowKind::InputChip {
                                header,
                                resolved: *resolved,
                            },
                            entry_id: entry_id.clone(),
                            timestamp: None,
                            copy_text: None,
                        });
                    }
                    MessagePart::Error {
                        id: part_id,
                        message,
                    } => {
                        rows.push(Row {
                            id: format!("{}#{}", entry.id, part_id).into(),
                            version: message.len() as u64,
                            turn_start: false,
                            kind: RowKind::ErrorChip {
                                // Harness-generated; the chip is one line.
                                message: single_line(message).into(),
                            },
                            entry_id: entry_id.clone(),
                            timestamp: None,
                            copy_text: None,
                        });
                    }
                    // Tools and thoughts are grouped by the outer arms;
                    // nothing reaches here.
                    MessagePart::Tool { .. } | MessagePart::Reasoning { .. } => {}
                }
            }
        }
    }
    flush_group(
        &mut rows,
        &mut pending_group,
        &mut group_ix,
        group_last_part_ix,
    );

    // Fragment files the reply only mentions (a link, inline code or a bare
    // path) or that its tools wrote still get a rendered-visualization card
    // once the reply settles.
    if !streaming {
        let mut texts = Vec::new();
        let mut referenced = Vec::new();
        for part in &entry.parts {
            match part {
                MessagePart::Text { text, .. } => {
                    referenced.extend(
                        mast_proto::visualize::references(text)
                            .into_iter()
                            .map(|reference| reference.path),
                    );
                    texts.push(text.as_str());
                }
                MessagePart::Tool {
                    call,
                    output,
                    diff_stats,
                    ..
                } => {
                    match call {
                        ToolCall::WriteFile { path, .. } | ToolCall::EditFile { path, .. } => {
                            texts.push(path.as_str())
                        }
                        ToolCall::ApplyPatch { path: Some(path) } => texts.push(path.as_str()),
                        _ => {}
                    }
                    texts.extend(diff_stats.iter().flatten().map(|stat| stat.path.as_str()));
                    texts.extend(output.as_deref());
                }
                _ => {}
            }
        }
        // Planning artifact links get a card each (in order, deduplicated).
        let mut linked: Vec<mast_proto::artifacts::ArtifactLink> = Vec::new();
        for part in &entry.parts {
            if let MessagePart::Text { text, .. } = part {
                for link in mast_proto::artifacts::artifact_links(text) {
                    if !linked.iter().any(|seen| seen.target == link.target) {
                        linked.push(link);
                    }
                }
            }
        }
        for (ix, link) in linked.into_iter().enumerate() {
            rows.push(Row {
                id: format!("{}#a{ix}", entry.id).into(),
                version: fnv1a(format!("{}\0{}", link.label, link.target).as_bytes()) << 1,
                turn_start: false,
                entry_id: entry_id.clone(),
                timestamp: None,
                copy_text: None,
                kind: RowKind::ArtifactCard {
                    label: link.label.into(),
                    target: link.target.into(),
                },
            });
        }
        for (ix, artifact) in mentioned_fragments(&texts, &referenced)
            .into_iter()
            .enumerate()
        {
            rows.push(Row {
                id: format!("{}#m{ix}", entry.id).into(),
                version: fnv1a(artifact.path.as_bytes()) << 1,
                turn_start: false,
                entry_id: entry_id.clone(),
                timestamp: None,
                copy_text: None,
                kind: RowKind::Visualization {
                    artifact: Some(artifact),
                },
            });
        }
    }

    if let Some(first) = rows.first_mut() {
        first.turn_start = true;
    }
    // Timestamp strip under the entry's LAST row once the turn has settled
    // (chat-view.tsx: "No timestamp hover mid-stream"). The version bit keeps
    // the diff key honest for last-row kinds whose own version wouldn't
    // change when streaming flips off (chips).
    if !streaming && let Some(last) = rows.last_mut() {
        last.timestamp = Some(entry.created_at);
        last.copy_text = assistant_copy_text(entry);
        last.version ^= 1 << 62;
    }
    rows
}

/// The transcript's markdown parse wiring, extracted for testability: one call
/// per text part per sync. Streaming parts keep one [`IncrementalParser`] per
/// row key and advance it with the full accumulated text (`set_text` takes the
/// O(tail) append path for the prefix-extensions the doc watch delivers);
/// completed parts hit the settled cache, adopt the live parser's tree on the
/// live→complete flip (flicker-free handoff), or do one full parse.
pub fn parse_for_row(
    streaming: bool,
    key: &str,
    text: &str,
    live_parsers: &mut HashMap<String, IncrementalParser>,
    tree_cache: &mut HashMap<String, (usize, Arc<BlockTree>)>,
) -> (Arc<BlockTree>, ParseOutcome) {
    if streaming {
        let parser = live_parsers.entry(key.to_string()).or_default();
        parser.set_text(text);
        (
            // Display tree: hanging inline markers mended so closers arriving
            // later never reflow painted text (markdown/mend.rs). Completed
            // rows below use the canonical tree — the honest settle.
            Arc::new(parser.display_tree()),
            ParseOutcome::Incremental {
                parsed_bytes: parser.last_parse_bytes(),
                stable_prefix_blocks: parser.stable_prefix_blocks(),
            },
        )
    } else {
        if let Some((len, tree)) = tree_cache.get(key)
            && *len == text.len()
        {
            return (tree.clone(), ParseOutcome::Cached);
        }
        // On the live→complete flip reuse the live parser's tree when
        // the sources match — the split rows then share the exact tree
        // the unsplit row painted, guaranteeing a flicker-free handoff.
        let (tree, outcome) = match live_parsers.remove(key) {
            Some(parser) if parser.source() == text => {
                (Arc::new(parser.tree().clone()), ParseOutcome::Handoff)
            }
            _ => (Arc::new(parse_full(text)), ParseOutcome::Full),
        };
        tree_cache.insert(key.to_string(), (text.len(), tree.clone()));
        (tree, outcome)
    }
}
