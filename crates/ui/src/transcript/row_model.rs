use super::*;

#[derive(Clone)]
pub enum RowKind {
    GeneratedImage {
        owner: String,
        path: String,
        name: String,
        mime_type: String,
    },
    /// A Codex `visualize` control envelope. Mast does not execute local
    /// HTML in the embedded browser; the row instead opens a validated file
    /// inside the current workspace. `None` is a malformed envelope, kept as
    /// a quiet unavailable card so private-use control bytes never leak.
    Visualization {
        artifact: Option<VisualizationArtifact>,
    },
    User {
        source: crate::message_source::MessageSource,
        /// Visible prompt (attachment-ref trailer already stripped). When the
        /// prompt carries file mentions this is the *projected* display text —
        /// chip labels in place of the raw Markdown links.
        text: SharedString,
        /// File-mention chips over `text`, in display-byte terms. Computed
        /// once per entry change in [`rows_for_entry`] (rows are cached by
        /// fingerprint), never per frame. Empty for ordinary prompts.
        mentions: Arc<Vec<crate::composer::SentMentionSpan>>,
        /// Image refs parsed out of the message text (message-attachments.ts):
        /// thumbnails load from the owning device via ReadAttachmentChunk.
        attachments: Arc<Vec<crate::attachments::UserImageAttachment>>,
        /// Context the prompt folded in as text, lifted back out by `badges`.
        badges: Arc<Vec<crate::badges::MessageBadge>>,
        /// Optimistic echo not yet confirmed by a doc frame.
        pending: bool,
    },
    /// One top-level markdown block of a completed message.
    Markdown {
        tree: Arc<BlockTree>,
        block_ix: usize,
    },
    /// One top-level block of a STREAMING message. Split per block like
    /// completed rows (only the tail blocks' versions change per commit, so
    /// the settled prefix is never respliced or re-rendered); rendered with
    /// the fade veil.
    LiveMarkdown {
        tree: Arc<BlockTree>,
        block_ix: usize,
    },
    ToolGroup {
        summary: SharedString,
        tools: Arc<Vec<ToolItem>>,
        auto_open: bool,
    },
    InputChip {
        /// First question's header (chat-view.tsx `InputChip`: the resolved
        /// chip shows it; unresolved shows "Awaiting your answer…" — which
        /// stays TRUE even across a run death: the composer keeps the panel
        /// up until the user answers, and the engine delivers a dead run's
        /// answer as a resumed turn).
        header: SharedString,
        resolved: bool,
    },
    ErrorChip {
        message: SharedString,
    },
    /// A `[label](mast-artifact:<id or path>)` link of a settled assistant
    /// message, shown as an artifact card (title, kind, role, revision,
    /// Open) resolved against the watched artifact snapshots at render time.
    ArtifactCard {
        label: SharedString,
        target: SharedString,
    },
}

/// A validated Codex content reference (`mast_proto::visualize`).
pub type VisualizationArtifact = mast_proto::visualize::VisualizationRef;

pub(super) enum AssistantTextFragment<'a> {
    Markdown(&'a str),
    Visualization(Option<VisualizationArtifact>),
}

#[cfg(test)]
pub(super) fn visualization_artifact(payload: &str) -> Option<VisualizationArtifact> {
    mast_proto::visualize::parse_payload(payload)
}

/// Split Codex visualization references (private-use envelopes and whole
/// `visualize{…}` lines) out of assistant prose. Invalid framed payloads
/// still become an unavailable artifact row: rendering their raw private-use
/// delimiters would be both noisy and an accidental parser escape hatch.
pub(super) fn assistant_text_fragments(
    text: &str,
    suppress_streaming_prefix: bool,
) -> Vec<AssistantTextFragment<'_>> {
    use mast_proto::visualize::Segment;
    mast_proto::visualize::split_references(text, suppress_streaming_prefix)
        .into_iter()
        .map(|segment| match segment {
            Segment::Text(text) => AssistantTextFragment::Markdown(text),
            Segment::Reference(reference) => AssistantTextFragment::Visualization(reference),
        })
        .collect()
}

/// Relative `*.fragment.html` paths a settled message mentions without a
/// content reference (links, inline code, bare paths), so each still gets a
/// rendered-visualization card. Paths are resolved later, at render time,
/// against the chat's checkout.
pub(super) fn mentioned_fragments(entry_text: &[&str], referenced: &[String]) -> Vec<VisualizationArtifact> {
    let mut out: Vec<VisualizationArtifact> = Vec::new();
    for text in entry_text {
        for path in mast_proto::visualize::fragment_mentions(text) {
            let duplicate = |candidate: &str| {
                candidate == path
                    || candidate.ends_with(&format!("/{path}"))
                    || path.ends_with(&format!("/{candidate}"))
            };
            if referenced.iter().any(|seen| duplicate(seen))
                || out.iter().any(|seen| duplicate(&seen.path))
            {
                continue;
            }
            let title = mast_proto::visualize::default_title(&path)
                .unwrap_or_else(|| "Visualization".to_owned());
            out.push(VisualizationArtifact {
                path,
                title,
                wide: false,
            });
        }
    }
    out
}

pub(super) fn generated_image_devices(owner: &str, fallback: &[String]) -> Vec<String> {
    let mut devices = Vec::new();
    for device in std::iter::once(owner).chain(fallback.iter().map(String::as_str)) {
        if !device.is_empty() && !devices.iter().any(|id| id == device) {
            devices.push(device.to_owned());
        }
    }
    devices
}

/// A transcript row: stable id + content version (diff key) + block payload.
#[derive(Clone)]
pub struct Row {
    pub id: SharedString,
    pub version: u64,
    /// First row of its message entry (gets the turn gap).
    pub turn_start: bool,
    pub kind: RowKind,
    /// The owning message entry — hover anywhere on the entry's rows reveals
    /// its timestamp strip (zeron chat-view.tsx `group`/`group-hover`).
    pub entry_id: SharedString,
    /// Epoch-ms for the 16px hover-timestamp strip UNDER this row: set on the
    /// LAST row of a completed entry (user rows always; assistant rows only
    /// once streaming ends — "the turn isn't at a time yet", chat-view.tsx).
    pub timestamp: Option<i64>,
    /// Text copied by the entry-level hover action. Present only on the last
    /// settled row, beside the timestamp; tools and transport-only metadata
    /// are deliberately excluded.
    pub copy_text: Option<SharedString>,
}

/// Absolute hover-timestamp label, e.g. "Jul 1, 3:45 PM" — the exact
/// `formatTimestamp` shape (utils.ts: short month, numeric day, hour,
/// 2-digit minutes, no leading zero on the hour). Pure over an explicit
/// timezone so tests don't depend on the host's local time.
pub fn format_timestamp<Tz: chrono::TimeZone>(ms: i64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(utc) => utc
            .with_timezone(tz)
            .format("%b %-d, %-I:%M %p")
            .to_string(),
        None => String::new(),
    }
}

pub(super) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x1_0000_01b3);
    }
    hash
}

pub(super) fn tool_fingerprint(tools: &[ToolItem], auto_open: bool) -> u64 {
    let mut acc = Vec::with_capacity(tools.len() * 8 + 1);
    for t in tools {
        acc.extend_from_slice(t.part_id.as_bytes());
        acc.push(0);
        let (label, detail) = tool_chip_content(&t.call);
        acc.extend_from_slice(label.as_bytes());
        acc.extend_from_slice(&(detail.len() as u32).to_le_bytes());
        acc.push(t.is_error as u8 | (t.resolved as u8) << 1);
        // Detail payload arriving (or growing) must re-splice the row even
        // when the resolved bit didn't change.
        match t.detail.as_deref() {
            None => acc.push(0),
            Some(ToolDetail::Output {
                lines,
                truncated_by,
            }) => {
                acc.push(1);
                acc.extend_from_slice(&(lines.len() as u32).to_le_bytes());
                acc.extend_from_slice(&(*truncated_by as u32).to_le_bytes());
                let bytes: usize = lines.iter().map(|l| l.len()).sum();
                acc.extend_from_slice(&(bytes as u32).to_le_bytes());
            }
            Some(ToolDetail::Thought {
                lines,
                truncated_by,
            }) => {
                // Byte-exact plus style bits: a live mend can restyle runs
                // without changing the flattened length, and the row must
                // still re-splice.
                acc.push(4);
                acc.extend_from_slice(&(lines.len() as u32).to_le_bytes());
                acc.extend_from_slice(&(*truncated_by as u32).to_le_bytes());
                for line in lines {
                    for run in line {
                        acc.extend_from_slice(run.text.as_bytes());
                        acc.push(
                            run.style.bold as u8
                                | (run.style.italic as u8) << 1
                                | (run.style.code as u8) << 2
                                | (run.style.strikethrough as u8) << 3
                                | (run.style.link.is_some() as u8) << 4,
                        );
                    }
                    acc.push(b'\n');
                }
            }
            Some(ToolDetail::Diff { file, .. }) => {
                acc.push(2);
                acc.extend_from_slice(file.path.as_bytes());
                acc.extend_from_slice(&file.additions.to_le_bytes());
                acc.extend_from_slice(&file.deletions.to_le_bytes());
                acc.extend_from_slice(&(file.hunks.len() as u32).to_le_bytes());
            }
            Some(ToolDetail::UnifiedPatch { files }) => {
                acc.push(5);
                for file in files.iter() {
                    acc.extend_from_slice(file.path.as_bytes());
                    acc.extend_from_slice(&file.additions.to_le_bytes());
                    acc.extend_from_slice(&file.deletions.to_le_bytes());
                    acc.extend_from_slice(&(file.hunks.len() as u32).to_le_bytes());
                    let lines = file.hunks.iter().map(|h| h.lines.len()).sum::<usize>();
                    acc.extend_from_slice(&(lines as u32).to_le_bytes());
                }
            }
            Some(ToolDetail::Stats { stats }) => {
                acc.push(3);
                for stat in stats.iter() {
                    acc.extend_from_slice(stat.path.as_bytes());
                    acc.extend_from_slice(&stat.additions.to_le_bytes());
                    acc.extend_from_slice(&stat.deletions.to_le_bytes());
                }
            }
        }
        // The invocation block is pure over `call`, which the one-line hash
        // above only covers by length — hash its bytes so an in-place call
        // update (a streaming MCP input, a growing todo list) re-splices.
        if let Some(ToolDetail::Output {
            lines,
            truncated_by,
        }) = t.invocation.as_deref()
        {
            for line in lines {
                acc.extend_from_slice(line.as_bytes());
            }
            acc.extend_from_slice(&(*truncated_by as u32).to_le_bytes());
        }
        // Sidecar refs arriving after the resolve tick must re-splice too —
        // they add the fetch affordance without changing the detail payload.
        acc.push(t.output_ref.is_some() as u8 | (t.diff_ref.is_some() as u8) << 1);
        // Subagent lifecycle mutates the chip in place (status flips, the
        // live tail grows) — hash it so the row re-splices on every change.
        acc.push(
            t.subagent_ref.is_some() as u8
                | match t.subagent_status {
                    None => 0,
                    Some(SubagentStatus::Running) => 1 << 1,
                    Some(SubagentStatus::Done) => 2 << 1,
                    Some(SubagentStatus::Failed) => 3 << 1,
                },
        );
        if let Some(tail) = &t.subagent_tail {
            acc.extend_from_slice(tail.as_bytes());
        }
    }
    acc.push(auto_open as u8);
    fnv1a(&acc)
}

/// Clipboard payload for an assistant/system entry: authored text parts in
/// document order, preserving Markdown while excluding tool traces and other
/// structured parts.
pub(super) fn assistant_copy_text(entry: &SessionMessageEntry) -> Option<SharedString> {
    let text = entry
        .parts
        .iter()
        .filter_map(|part| match part {
            // Inspect the trimmed view only to reject empty parts. Copy the
            // original bytes so indentation-based code blocks and Markdown
            // hard-break whitespace survive the clipboard round trip.
            MessagePart::Text { text, .. } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.is_empty()).then(|| text.into())
}

/// Conservative first-frame fallback for whether a prompt may need a fold
/// affordance. Once the text element has measured, `render_user_body` replaces
/// this proxy with the exact wrapped-line count, so glyph width and script no
/// longer affect eligibility heuristically.
pub fn user_message_needs_collapse(text: &str) -> bool {
    text.lines().count() > USER_COLLAPSED_LINES || text.chars().count() > USER_COLLAPSE_CHARS
}

/// Layout transitions need more time when they travel farther, otherwise a
/// 1,000px pasted log crosses the screen in the same 200ms as a six-line note
/// and reads as a snap. Keep ordinary messages close to the catalog RESIZE
/// timing, then scale to an 850ms ceiling for genuinely large pasted content.
pub fn user_resize_duration_ms(height_delta: f32) -> u64 {
    (220.0 + height_delta.max(0.0) * 0.32).min(850.0).round() as u64
}

/// Short folds keep the app's familiar decisive ease-out. Large folds use
/// ease-in-out so thousands of pixels do not disappear in the first few frames
/// of an aggressively front-loaded curve.
pub fn user_resize_spec(height_delta: f32) -> motion::MotionSpec {
    let curve = if height_delta > 500.0 {
        motion::EASE_IN_OUT
    } else {
        motion::EASE_OUT
    };
    motion::MotionSpec::new(user_resize_duration_ms(height_delta), curve)
}
