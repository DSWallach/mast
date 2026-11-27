//! Tier-capped worker context packets and heading-boundary slices.
//!
//! Everything here is pure: no workspace, no I/O. Callers hand in text and ids
//! and get back a packet, a slice or a verdict, so each rule is testable on its
//! own and a different transport can reuse it.
//!
//! * [`build_packet`] assembles a [`WorkerContextPacket`] in strict
//!   static-to-dynamic order so consecutive tasks share the longest possible
//!   prompt-cache prefix: role prompt and tool schemas, then frozen spec
//!   contracts, then project-memory rules that intersect the task's owned
//!   paths, then the task slice. A packet over the tier's token cap is refused
//!   with [`PacketOverBudget`] so a Lead or Manager splits the ticket first.
//! * [`worker_instruction_entries`] drops Manager and Lead orchestration
//!   entries from a leaf worker's view of its Work Outline instructions.
//! * [`read_slice`] cuts a Markdown document at heading boundaries for the
//!   `context_read_slice` tool and returns a replay id that stays the same for
//!   the same bytes.
//! * [`worker_read_chat_window`] is the `read_chat` fence: a worker may not
//!   read a sibling, and may read only a short window of its own recorded
//!   handoff predecessor.

use std::collections::BTreeSet;

use mast_proto::work_outline_instructions::{
    InstructionRevisionEntry, IntegrityError, render_instruction_envelope,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Name of the `context_read_slice` tool.
pub const TOOL_NAME: &str = "context_read_slice";

/// Instruction-entry marker: an entry whose text starts with this line is
/// Manager/Lead orchestration and is not shown to a leaf worker.
pub const ORCHESTRATION_MARKER: &str = "[orchestration]";

/// Largest window of its recorded predecessor's transcript a worker may read.
pub const PREDECESSOR_READ_LIMIT: usize = 6;

/// A Tier-3 packet's memory section is capped at this many lines.
pub const TIER3_MEMORY_LINE_CAP: usize = 200;

/// Default and bounds of `maxTokens` for one slice.
pub const DEFAULT_SLICE_TOKENS: u32 = 1_500;
pub const MIN_SLICE_TOKENS: u32 = 100;
pub const MAX_SLICE_TOKENS: u32 = 6_000;

/// Longest `reason` accepted, in characters.
pub const MAX_REASON_CHARS: usize = 200;

/// The four lines every Tier-1 packet carries: read first, parallel tool
/// calls, an interleaved hypothesis test, and a non-derivable checkpoint.
pub const TIER1_SCAFFOLD: [&str; 4] = [
    "Read the contracts and verified project-memory rules below BEFORE your first tool call.",
    "Issue independent read and search tool calls in parallel (3 or more calls per turn).",
    "After each tool output, say whether your hypothesis held before you edit code.",
    "Checkpoint only what the repo cannot show: a rule or decision, the command that verifies it, a blocker. Never copy git status or raw logs.",
];

/// Layer headers. They are part of the cached prefix, so they never change
/// per task.
const LAYER_HEADERS: [&str; 4] = [
    "[1. ROLE PROMPT & TOOL SCHEMAS]",
    "[2. FROZEN SPEC & SUBSYSTEM CONTRACTS]",
    "[3. VERIFIED PROJECT MEMORY RULES]",
    "[4. TASK SLICE]",
];

/// Rough token count: one token per four bytes, rounded up. A Tier-1 packet of
/// 3,000 tokens is about 12 KB.
pub fn estimate_tokens(text: &str) -> u32 {
    u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX)
}

// ---------------------------------------------------------------------------
// Tiers
// ---------------------------------------------------------------------------

/// Model tier a packet is compiled for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelTier {
    /// Small models: 3,000-token packets plus the steering scaffold.
    Tier1,
    /// Mid models: 6,000-token packets.
    Tier2,
    /// Frontier models: 6,000-token packets, no scaffold, open salience
    /// judgment under the 200-line memory cap.
    Tier3,
}

impl ModelTier {
    /// Largest packet this tier launches with.
    pub fn packet_cap_tokens(self) -> u32 {
        match self {
            Self::Tier1 => 3_000,
            Self::Tier2 | Self::Tier3 => 6_000,
        }
    }

    /// Fraction of the context window at which old large tool outputs are
    /// replaced by replay ids (P11).
    pub fn soft_wrap_fraction(self) -> f32 {
        match self {
            Self::Tier1 => 0.45,
            Self::Tier2 | Self::Tier3 => 0.65,
        }
    }

    /// Fraction at which the session is swapped for a checkpointed one (P11).
    pub fn hard_swap_fraction(self) -> f32 {
        match self {
            Self::Tier1 => 0.60,
            Self::Tier2 | Self::Tier3 => 0.78,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Tier1 => "Tier 1",
            Self::Tier2 => "Tier 2",
            Self::Tier3 => "Tier 3",
        }
    }
}

// ---------------------------------------------------------------------------
// Project-memory rules
// ---------------------------------------------------------------------------

/// One verified rule from `project-memory.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRule {
    pub id: String,
    pub scope_glob: String,
    pub rule: String,
    #[serde(default)]
    pub verified_by: Option<String>,
}

impl MemoryRule {
    fn line(&self) -> String {
        match &self.verified_by {
            Some(by) => format!("- {} [{}]: {} (verified by {by})", self.id, self.scope_glob, self.rule),
            None => format!("- {} [{}]: {}", self.id, self.scope_glob, self.rule),
        }
    }
}

/// Parse `- <RULE-ID> [<scope_glob>]: <rule> (verified by <ref>)` lines.
/// Other lines are ignored.
pub fn parse_memory_rules(markdown: &str) -> Vec<MemoryRule> {
    markdown
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("- ")?;
            let (id, rest) = rest.split_once(" [")?;
            let (glob, rest) = rest.split_once("]:")?;
            let id = id.trim();
            let glob = glob.trim();
            if id.is_empty() || id.contains(char::is_whitespace) || glob.is_empty() {
                return None;
            }
            let rest = rest.trim();
            let (rule, verified_by) = match rest.rsplit_once("(verified by ") {
                Some((rule, tail)) if tail.ends_with(')') => (
                    rule.trim(),
                    Some(tail.trim_end_matches(')').trim().to_owned()),
                ),
                _ => (rest, None),
            };
            (!rule.is_empty()).then(|| MemoryRule {
                id: id.to_owned(),
                scope_glob: glob.to_owned(),
                rule: rule.to_owned(),
                verified_by,
            })
        })
        .collect()
}

/// Rules whose `scope_glob` intersects any of `owned_paths`. An empty
/// `owned_paths` selects only rules whose glob is `**`.
pub fn rules_for_paths<'a>(rules: &'a [MemoryRule], owned_paths: &[String]) -> Vec<&'a MemoryRule> {
    let owned: Vec<String> = owned_paths.iter().flat_map(|p| expand_braces(p)).collect();
    rules
        .iter()
        .filter(|rule| {
            expand_braces(&rule.scope_glob).iter().any(|glob| {
                glob == "**"
                    || owned.iter().any(|path| globs_intersect(glob, path))
            })
        })
        .collect()
}

/// Expand one level of `{a,b}` alternation (nested braces are expanded
/// recursively).
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let mut depth = 0usize;
    let mut close = None;
    for (offset, ch) in pattern[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return vec![pattern.to_owned()];
    };
    let (head, tail) = (&pattern[..open], &pattern[close + 1..]);
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = open + 1;
    for (offset, ch) in pattern[open + 1..close].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&pattern[start..open + 1 + offset]);
                start = open + 2 + offset;
            }
            _ => {}
        }
    }
    parts.push(&pattern[start..close]);
    parts
        .into_iter()
        .flat_map(|part| expand_braces(&format!("{head}{part}{tail}")))
        .collect()
}

/// `*` matches within a path segment, `**` across segments, `?` one character.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = p[2..].strip_prefix(b"/").unwrap_or(&p[2..]);
                (0..=t.len()).any(|skip| go(rest, &t[skip..]))
            }
            Some(b'*') => {
                let rest = &p[1..];
                (0..=t.len())
                    .take_while(|&skip| skip == 0 || t[skip - 1] != b'/')
                    .any(|skip| go(rest, &t[skip..]))
            }
            Some(b'?') => t.first().is_some_and(|&c| c != b'/') && go(&p[1..], &t[1..]),
            Some(&c) => t.first() == Some(&c) && go(&p[1..], &t[1..]),
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

/// Literal directory prefix of a glob (everything before its first wildcard
/// segment), without a trailing slash.
fn literal_prefix(glob: &str) -> &str {
    let end = glob.find(['*', '?']).unwrap_or(glob.len());
    let head = &glob[..end];
    if end == glob.len() {
        return head.trim_end_matches('/');
    }
    head.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn globs_intersect(a: &str, b: &str) -> bool {
    if glob_match(a, b) || glob_match(b, a) {
        return true;
    }
    let (pa, pb) = (literal_prefix(a), literal_prefix(b));
    let nested = |outer: &str, inner: &str| {
        outer.is_empty() || inner == outer || inner.starts_with(&format!("{outer}/"))
    };
    let wild = |g: &str| g.contains(['*', '?']);
    // A wildcard glob reaches everything under its literal directory; two
    // plain paths intersect only when equal (handled above) or one is a
    // directory holding the other.
    (wild(a) && nested(pa, pb)) || (wild(b) && nested(pb, pa)) || (!wild(a) && !wild(b) && (nested(a, b) || nested(b, a)))
}

// ---------------------------------------------------------------------------
// Instruction altitude filter
// ---------------------------------------------------------------------------

/// A leaf worker's view of its effective Work Outline instructions: the
/// built-in Manager role entry and any entry marked [`ORCHESTRATION_MARKER`]
/// are removed, and the rest are renumbered. The stored revision, its hash and
/// the delivered envelope are not touched; this is a view for packets.
pub fn worker_instruction_entries(entries: &[InstructionRevisionEntry]) -> Vec<InstructionRevisionEntry> {
    let mut kept: Vec<InstructionRevisionEntry> = entries
        .iter()
        .filter(|entry| {
            entry.instruction_id != mast_proto::MANAGER_ROLE_INSTRUCTION_ID
                && entry.text.trim_start().lines().next().map(str::trim) != Some(ORCHESTRATION_MARKER)
        })
        .cloned()
        .collect();
    for (index, entry) in kept.iter_mut().enumerate() {
        entry.order = index as u32;
    }
    kept
}

// ---------------------------------------------------------------------------
// Packet
// ---------------------------------------------------------------------------

/// What a packet is compiled from.
#[derive(Debug, Clone)]
pub struct PacketInputs<'a> {
    pub tier: ModelTier,
    /// Role system prompt (static per role).
    pub role_prompt: &'a str,
    /// Compact MCP tool schemas for the role (static per role).
    pub tool_schemas: &'a str,
    /// The caller's effective instructions; the leaf worker view is rendered.
    pub instructions: &'a [InstructionRevisionEntry],
    /// Frozen spec and subsystem contracts (shared across a wave).
    pub contracts: &'a str,
    /// All project-memory rules; those intersecting `owned_paths` are kept.
    pub memory_rules: &'a [MemoryRule],
    pub owned_paths: &'a [String],
    /// The task's own slice (the only per-task part).
    pub task_slice: &'a str,
}

/// A packet in cache order. Layers 1 to 3 are identical for two tasks of the
/// same wave and tier whose owned paths select the same rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerContextPacket {
    pub tier: ModelTier,
    /// The four layers, most static first.
    pub layers: [String; 4],
    pub memory_rule_ids: Vec<String>,
    pub estimated_tokens: u32,
    pub cap_tokens: u32,
}

impl WorkerContextPacket {
    /// The full packet text: the layers in order, each under its header.
    pub fn render(&self) -> String {
        render_layers(&self.layers)
    }

    /// Hash of the first `count` layers (1 to 4) as rendered: equal hashes
    /// mean a shared prompt-cache prefix.
    pub fn prefix_hash(&self, count: usize) -> String {
        let count = count.clamp(1, 4);
        let text = render_layers(&self.layers[..count]);
        hex(&Sha256::digest(text.as_bytes()))[..16].to_owned()
    }
}

fn render_layers(layers: &[String]) -> String {
    let mut out = String::new();
    for (index, layer) in layers.iter().enumerate() {
        out.push_str(LAYER_HEADERS[index]);
        out.push('\n');
        out.push_str(layer.trim_end());
        out.push_str("\n\n");
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

/// The packet is over its tier's cap. The ticket must be split finer first;
/// Tier 2 is for tasks that cannot be split.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PacketOverBudget {
    pub tier: ModelTier,
    pub estimated_tokens: u32,
    pub cap_tokens: u32,
    /// Estimated tokens of each layer, in order.
    pub layer_tokens: [u32; 4],
}

impl std::fmt::Display for PacketOverBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PacketOverBudget: the packet is about {} tokens, over the {} cap of {} (layers: role {}, contracts {}, memory {}, task {}). ",
            self.estimated_tokens,
            self.tier.label(),
            self.cap_tokens,
            self.layer_tokens[0],
            self.layer_tokens[1],
            self.layer_tokens[2],
            self.layer_tokens[3],
        )?;
        if self.tier == ModelTier::Tier1 {
            f.write_str("The Lead or Manager must split the ticket finer first; promote it to Tier 2 only when the task is irreducible.")
        } else {
            f.write_str("The Lead or Manager must split the ticket finer; there is no larger tier.")
        }
    }
}

impl std::error::Error for PacketOverBudget {}

/// Why a packet could not be built.
#[derive(Debug)]
pub enum PacketError {
    OverBudget(PacketOverBudget),
    Instructions(IntegrityError),
}

impl std::fmt::Display for PacketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OverBudget(over) => over.fmt(f),
            Self::Instructions(error) => write!(f, "instructions could not be rendered: {error:?}"),
        }
    }
}

impl std::error::Error for PacketError {}

/// Compile a packet, or refuse it with [`PacketOverBudget`].
pub fn build_packet(inputs: &PacketInputs<'_>) -> Result<WorkerContextPacket, PacketError> {
    let mut layer1 = String::new();
    layer1.push_str(inputs.role_prompt.trim());
    let schemas = inputs.tool_schemas.trim();
    if !schemas.is_empty() {
        layer1.push_str("\n\n");
        layer1.push_str(schemas);
    }
    let visible = worker_instruction_entries(inputs.instructions);
    if !visible.is_empty() {
        let envelope = render_instruction_envelope(1, &visible).map_err(PacketError::Instructions)?;
        layer1.push_str("\n\n");
        layer1.push_str(envelope.trim_end());
    }
    if inputs.tier == ModelTier::Tier1 {
        layer1.push_str("\n\nOPERATING PROTOCOL:");
        for line in TIER1_SCAFFOLD {
            layer1.push_str("\n- ");
            layer1.push_str(line);
        }
    }

    let selected = rules_for_paths(inputs.memory_rules, inputs.owned_paths);
    let mut memory_lines: Vec<String> = selected.iter().map(|rule| rule.line()).collect();
    if inputs.tier == ModelTier::Tier3 && memory_lines.len() > TIER3_MEMORY_LINE_CAP {
        memory_lines.truncate(TIER3_MEMORY_LINE_CAP);
    }
    let memory_rule_ids: Vec<String> = selected.iter().take(memory_lines.len()).map(|rule| rule.id.clone()).collect();
    let layer3 = if memory_lines.is_empty() {
        "None apply to the owned paths.".to_owned()
    } else {
        memory_lines.join("\n")
    };

    let layers = [
        layer1,
        inputs.contracts.trim().to_owned(),
        layer3,
        inputs.task_slice.trim().to_owned(),
    ];
    let packet = WorkerContextPacket {
        tier: inputs.tier,
        estimated_tokens: 0,
        cap_tokens: inputs.tier.packet_cap_tokens(),
        memory_rule_ids,
        layers,
    };
    let layer_tokens = [
        estimate_tokens(&packet.layers[0]),
        estimate_tokens(&packet.layers[1]),
        estimate_tokens(&packet.layers[2]),
        estimate_tokens(&packet.layers[3]),
    ];
    let estimated_tokens = estimate_tokens(&packet.render());
    if estimated_tokens > packet.cap_tokens {
        return Err(PacketError::OverBudget(PacketOverBudget {
            tier: inputs.tier,
            estimated_tokens,
            cap_tokens: packet.cap_tokens,
            layer_tokens,
        }));
    }
    Ok(WorkerContextPacket { estimated_tokens, ..packet })
}

// ---------------------------------------------------------------------------
// Markdown sections
// ---------------------------------------------------------------------------

/// One heading and the bytes it governs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub level: usize,
    pub title: String,
    /// Unique, lowercase, dash-separated anchor.
    pub slug: String,
    /// Byte offset of the heading line.
    pub start: usize,
    /// End of the section including its subsections.
    pub end: usize,
}

fn slugify(title: &str) -> String {
    let mut slug = String::new();
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            slug.extend(ch.to_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
}

/// Headings of `text`, ignoring `#` lines inside fenced code blocks.
pub fn sections(text: &str) -> Vec<Section> {
    let mut found: Vec<Section> = Vec::new();
    let mut in_fence: Option<(char, usize)> = None;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let fence = trimmed
            .chars()
            .next()
            .filter(|c| *c == '`' || *c == '~')
            .map(|c| (c, trimmed.chars().take_while(|x| *x == c).count()))
            .filter(|(_, run)| *run >= 3);
        match (in_fence, fence) {
            (None, Some(open)) => in_fence = Some(open),
            (Some((c, run)), Some((d, n))) if c == d && n >= run => in_fence = None,
            (None, None) => {
                let level = trimmed.chars().take_while(|c| *c == '#').count();
                if (1..=6).contains(&level) && line.len() - trimmed.len() < 4 {
                    let title = trimmed[level..].trim();
                    if trimmed[level..].starts_with([' ', '\t']) && !title.is_empty() {
                        let title = title.trim_end_matches('#').trim().to_owned();
                        found.push(Section {
                            level,
                            slug: String::new(),
                            title,
                            start: offset,
                            end: text.len(),
                        });
                    }
                }
            }
            _ => {}
        }
        offset += line.len();
    }
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for section in &mut found {
        let base = slugify(&section.title);
        let count = seen.entry(base.clone()).or_insert(0);
        section.slug = if *count == 0 { base } else { format!("{base}-{count}") };
        *count += 1;
    }
    for index in 0..found.len() {
        let level = found[index].level;
        if let Some(next) = found[index + 1..].iter().find(|s| s.level <= level) {
            found[index].end = next.start;
        }
    }
    found
}

/// Why a slice could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SliceError {
    /// No heading matches. `available` lists the first few anchors.
    UnknownAnchor { anchor: String, available: Vec<String> },
    /// More than one heading starts with the anchor.
    AmbiguousAnchor { anchor: String, matches: Vec<String> },
}

impl std::fmt::Display for SliceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownAnchor { anchor, available } => {
                write!(f, "no heading matches {anchor:?}; anchors: {}", available.join(", "))
            }
            Self::AmbiguousAnchor { anchor, matches } => {
                write!(f, "{anchor:?} matches several headings: {}", matches.join(", "))
            }
        }
    }
}

impl std::error::Error for SliceError {}

/// A heading-bounded piece of a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Slice {
    pub text: String,
    /// The section the slice starts at, when one was asked for.
    pub anchor: Option<String>,
    pub truncated: bool,
    /// The heading to ask for next when the slice stopped before it.
    pub next_anchor: Option<String>,
    /// True when one section alone was larger than the budget and was cut at a
    /// paragraph or line boundary instead.
    pub cut_inside_section: bool,
    pub estimated_tokens: u32,
    pub total_tokens: u32,
    /// Same document bytes and same slice give the same id, so a caller can
    /// drop an old large output and refer to it by this id.
    pub replay_id: String,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Cut `text` at heading boundaries to at most `max_tokens` (clamped to
/// [`MIN_SLICE_TOKENS`]..=[`MAX_SLICE_TOKENS`], default
/// [`DEFAULT_SLICE_TOKENS`]). With `anchor`, start at that heading (its slug,
/// its title, or a unique slug prefix such as `w3-t4`) and stay inside its
/// section. `source_key` names the document (for example `plan:<artifact>@<rev>`)
/// and goes into the replay id.
pub fn read_slice(
    text: &str,
    source_key: &str,
    anchor: Option<&str>,
    max_tokens: Option<u32>,
) -> Result<Slice, SliceError> {
    let budget = max_tokens
        .unwrap_or(DEFAULT_SLICE_TOKENS)
        .clamp(MIN_SLICE_TOKENS, MAX_SLICE_TOKENS) as usize
        * 4;
    let all = sections(text);
    let (range_start, range_end, resolved) = match anchor.map(str::trim).filter(|a| !a.is_empty()) {
        None => (0, text.len(), None),
        Some(wanted) => {
            let slug = slugify(wanted.trim_start_matches('#'));
            let exact: Vec<&Section> = all.iter().filter(|s| s.slug == slug).collect();
            let candidates = if exact.is_empty() {
                all.iter().filter(|s| s.slug.starts_with(&slug)).collect::<Vec<_>>()
            } else {
                exact
            };
            match candidates.as_slice() {
                [] => {
                    return Err(SliceError::UnknownAnchor {
                        anchor: wanted.to_owned(),
                        available: all.iter().take(20).map(|s| s.slug.clone()).collect(),
                    });
                }
                [one] => (one.start, one.end, Some(one.slug.clone())),
                many => {
                    return Err(SliceError::AmbiguousAnchor {
                        anchor: wanted.to_owned(),
                        matches: many.iter().take(10).map(|s| s.slug.clone()).collect(),
                    });
                }
            }
        }
    };
    // Block boundaries: the range start and every heading inside it.
    let mut cuts: Vec<usize> = vec![range_start];
    cuts.extend(
        all.iter()
            .map(|s| s.start)
            .filter(|start| *start > range_start && *start < range_end),
    );
    cuts.push(range_end);
    cuts.dedup();

    let mut end = range_start;
    let mut cut_inside_section = false;
    for window in cuts.windows(2) {
        if window[1] - range_start <= budget {
            end = window[1];
            continue;
        }
        if end == range_start {
            end = clean_cut(text, range_start, range_start + budget);
            cut_inside_section = true;
        }
        break;
    }
    let truncated = end < range_end;
    let next_anchor = truncated
        .then(|| all.iter().find(|s| s.start >= end && s.start < range_end).map(|s| s.slug.clone()))
        .flatten();
    let body = text[range_start..end].trim_end().to_owned();
    let digest = Sha256::digest(format!("{source_key}\0{}\0{body}", resolved.as_deref().unwrap_or("")).as_bytes());
    Ok(Slice {
        estimated_tokens: estimate_tokens(&body),
        total_tokens: estimate_tokens(&text[range_start..range_end]),
        replay_id: format!("cs_{}", &hex(&digest)[..12]),
        text: body,
        anchor: resolved,
        truncated,
        next_anchor,
        cut_inside_section,
    })
}

/// Largest cut in `text[start..limit]` that ends a paragraph, else a line,
/// else a character.
fn clean_cut(text: &str, start: usize, limit: usize) -> usize {
    let mut limit = limit.min(text.len());
    while !text.is_char_boundary(limit) {
        limit -= 1;
    }
    let window = &text[start..limit];
    if let Some(at) = window.rfind("\n\n") {
        return start + at + 1;
    }
    if let Some(at) = window.rfind('\n') {
        return start + at + 1;
    }
    limit
}

// ---------------------------------------------------------------------------
// Plan extraction
// ---------------------------------------------------------------------------

/// A task reference such as `W3-T4` found in a work item's title: the first
/// word that starts with a letter, holds a dash and contains a digit.
pub fn task_ref(title: &str) -> Option<String> {
    title
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .find(|word| {
            word.contains('-')
                && word.chars().next().is_some_and(char::is_alphabetic)
                && word.chars().any(|c| c.is_ascii_digit())
        })
        .map(str::to_owned)
}

/// The pieces of a long plan a worker needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanExtract {
    /// Sections about contracts or invariants, in plan order.
    pub contracts: String,
    /// The section whose heading names the task, with its subsections; empty
    /// when none does.
    pub task_slice: String,
    /// Heading of the task section.
    pub task_anchor: Option<String>,
    /// `anchor: title` for every heading, for navigating with
    /// `context_read_slice`.
    pub outline: Vec<String>,
}

/// Cut a plan down to the contracts and the task's own section.
pub fn extract_plan(plan: &str, task_ref: Option<&str>) -> PlanExtract {
    let all = sections(plan);
    let task = task_ref.and_then(|wanted| {
        let wanted = wanted.to_lowercase();
        all.iter()
            .filter(|s| s.title.to_lowercase().contains(&wanted))
            .min_by_key(|s| (s.level, s.start))
    });
    let mut contracts = String::new();
    let mut last_end = 0usize;
    for section in all.iter().filter(|s| s.level <= 3) {
        let title = section.title.to_lowercase();
        let inside_task = task.is_some_and(|t| section.start >= t.start && section.start < t.end);
        if section.start < last_end || inside_task || !(title.contains("contract") || title.contains("invariant")) {
            continue;
        }
        contracts.push_str(plan[section.start..section.end].trim_end());
        contracts.push_str("\n\n");
        last_end = section.end;
    }
    PlanExtract {
        contracts: contracts.trim_end().to_owned(),
        task_slice: task.map(|t| plan[t.start..t.end].trim_end().to_owned()).unwrap_or_default(),
        task_anchor: task.map(|t| t.slug.clone()),
        outline: all.iter().map(|s| format!("{}: {}", s.slug, s.title)).collect(),
    }
}

// ---------------------------------------------------------------------------
// read_chat fence
// ---------------------------------------------------------------------------

/// How the chat a worker wants to read relates to the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRelation {
    Own,
    Parent,
    Child,
    /// The chat the worker's work item recorded as its handoff predecessor.
    Predecessor,
    /// Another worker of the same parent.
    Sibling,
    Other,
}

/// Facts about the two chats, enough to classify them.
#[derive(Debug, Clone, Copy)]
pub struct ReadChatFacts<'a> {
    pub caller_chat_id: &'a str,
    pub caller_parent_id: Option<&'a str>,
    pub target_chat_id: &'a str,
    pub target_parent_id: Option<&'a str>,
    /// Handoff predecessors recorded for the caller's own work item.
    pub recorded_predecessors: &'a [String],
}

pub fn classify_chat(facts: &ReadChatFacts<'_>) -> ChatRelation {
    if facts.target_chat_id == facts.caller_chat_id {
        ChatRelation::Own
    } else if facts.recorded_predecessors.iter().any(|id| id == facts.target_chat_id) {
        ChatRelation::Predecessor
    } else if facts.caller_parent_id == Some(facts.target_chat_id) {
        ChatRelation::Parent
    } else if facts.target_parent_id == Some(facts.caller_chat_id) {
        ChatRelation::Child
    } else if facts.caller_parent_id.is_some() && facts.caller_parent_id == facts.target_parent_id {
        ChatRelation::Sibling
    } else {
        ChatRelation::Other
    }
}

/// A `read_chat` the worker fence refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadChatRefused(pub ChatRelation);

impl std::fmt::Display for ReadChatRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            ChatRelation::Sibling => f.write_str(
                "a worker cannot read a sibling worker's chat; ask your parent, or read the chat of your recorded handoff predecessor",
            ),
            _ => f.write_str(
                "a worker can read its own chat, its parent, its children and its recorded handoff predecessor",
            ),
        }
    }
}

impl std::error::Error for ReadChatRefused {}

/// The window a worker may read: unchanged for its own chat, its parent and
/// its children; at most [`PREDECESSOR_READ_LIMIT`] messages of its recorded
/// predecessor; refused for a sibling or anyone else.
pub fn worker_read_chat_window(
    relation: ChatRelation,
    limit: Option<usize>,
) -> Result<Option<usize>, ReadChatRefused> {
    match relation {
        ChatRelation::Own | ChatRelation::Parent | ChatRelation::Child => Ok(limit),
        ChatRelation::Predecessor => Ok(Some(limit.unwrap_or(PREDECESSOR_READ_LIMIT).min(PREDECESSOR_READ_LIMIT))),
        ChatRelation::Sibling | ChatRelation::Other => Err(ReadChatRefused(relation)),
    }
}

// ---------------------------------------------------------------------------
// context_read_slice
// ---------------------------------------------------------------------------

/// Where a slice is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SliceTarget {
    /// The work item's pinned plan, optionally from one heading.
    Plan { anchor: Option<String> },
}

impl SliceTarget {
    /// `plan` or `plan#<anchor>`.
    pub fn parse(target: &str) -> Result<Self, String> {
        let target = target.trim();
        let (kind, anchor) = match target.split_once('#') {
            Some((kind, anchor)) => (kind.trim(), Some(anchor.trim().to_owned()).filter(|a| !a.is_empty())),
            None => (target, None),
        };
        match kind {
            "plan" => Ok(Self::Plan { anchor }),
            _ => Err(format!("unknown target {target:?}: use plan or plan#<heading-anchor>")),
        }
    }
}

/// Arguments of `context_read_slice`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextReadSliceArgs {
    pub target: String,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    pub reason: String,
}

impl ContextReadSliceArgs {
    pub fn validate(&self) -> Result<SliceTarget, String> {
        let reason = self.reason.trim();
        if reason.is_empty() {
            return Err("reason is required: say what you need the slice for".to_owned());
        }
        if reason.chars().count() > MAX_REASON_CHARS {
            return Err(format!("reason is at most {MAX_REASON_CHARS} characters"));
        }
        SliceTarget::parse(&self.target)
    }
}

/// One `context_read_slice` call, for the `context_slice_pull` usage record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSlicePull {
    pub chat_id: String,
    pub target: String,
    pub reason: String,
    pub requested_tokens: u32,
    pub returned_tokens: u32,
    pub truncated: bool,
    pub replay_id: String,
}

/// Where pulls are recorded (`token-usage.sqlite3`, kind `context_slice_pull`).
pub trait SlicePullSink: Send + Sync {
    fn record(&self, pull: &ContextSlicePull);
}

/// The reply of `context_read_slice`.
pub fn slice_reply(slice: &Slice, target: &str) -> Value {
    json!({
        "target": target,
        "replayId": slice.replay_id,
        "anchor": slice.anchor,
        "text": slice.text,
        "tokens": slice.estimated_tokens,
        "totalTokens": slice.total_tokens,
        "truncated": slice.truncated,
        "nextAnchor": slice.next_anchor,
        "cutInsideSection": slice.cut_inside_section,
    })
}

/// The tool definition. Its serialized form stays under 550 bytes.
pub fn tool_definition() -> Value {
    json!({
        "name": TOOL_NAME,
        "description": "Read part of your plan without loading all of it, cut at headings. Pass plan or plan#<anchor>; the reply has nextAnchor when more follows and a replayId you can cite after dropping the text.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "target": { "type": "string", "description": "plan or plan#<heading-anchor>" },
                "maxTokens": { "type": "integer", "description": "100-6000, default 1500" },
                "reason": { "type": "string", "description": "Why you need it" }
            },
            "required": ["target", "reason"]
        }
    })
}

/// Anchors of every heading in `text`, as a set (used to check a caller's
/// anchor without cutting).
pub fn anchors(text: &str) -> BTreeSet<String> {
    sections(text).into_iter().map(|s| s.slug).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::work_outline_instructions::{InstructionProvenance, InstructionSourceKind};

    fn entry(id: &str, text: &str, order: u32) -> InstructionRevisionEntry {
        InstructionRevisionEntry {
            instruction_id: id.to_owned(),
            source_kind: InstructionSourceKind::WorkItem,
            scope_id: "item".to_owned(),
            scope_title: "Item".to_owned(),
            text: text.to_owned(),
            order,
            provenance: InstructionProvenance::default(),
        }
    }

    fn inputs<'a>(tier: ModelTier, task: &'a str, rules: &'a [MemoryRule], owned: &'a [String]) -> PacketInputs<'a> {
        PacketInputs {
            tier,
            role_prompt: "You are a worker.",
            tool_schemas: "tools: whoami",
            instructions: &[],
            contracts: "CONTRACT: keep ids stable.",
            memory_rules: rules,
            owned_paths: owned,
            task_slice: task,
        }
    }

    fn rule(id: &str, glob: &str) -> MemoryRule {
        MemoryRule { id: id.into(), scope_glob: glob.into(), rule: format!("rule {id}"), verified_by: Some("abc123".into()) }
    }

    #[test]
    fn layers_are_static_to_dynamic_and_share_a_prefix_across_tasks() {
        let rules = [rule("R1", "crates/engine/**")];
        let owned = ["crates/engine/src/a.rs".to_owned()];
        let one = build_packet(&inputs(ModelTier::Tier2, "TASK one", &rules, &owned)).unwrap();
        let two = build_packet(&inputs(ModelTier::Tier2, "TASK two, longer", &rules, &owned)).unwrap();
        let text = one.render();
        let positions: Vec<usize> = LAYER_HEADERS.iter().map(|h| text.find(h).unwrap()).collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{text}");
        assert_eq!(one.prefix_hash(3), two.prefix_hash(3), "layers 1-3 are byte-identical");
        assert_ne!(one.prefix_hash(4), two.prefix_hash(4));
        assert!(two.render().starts_with(&text[..text.find(LAYER_HEADERS[3]).unwrap()]));
    }

    #[test]
    fn only_rules_that_intersect_owned_paths_are_included() {
        let rules = [
            rule("R1", "crates/engine/**"),
            rule("R2", "crates/ui/**"),
            rule("R3", "crates/engine/src/{artifacts,briefing}.rs"),
            rule("R4", "docs/*.md"),
        ];
        let owned = ["crates/engine/src/{context_slice.rs,artifacts.rs}".to_owned()];
        let packet = build_packet(&inputs(ModelTier::Tier2, "t", &rules, &owned)).unwrap();
        assert_eq!(packet.memory_rule_ids, ["R1", "R3"]);
        let docs = ["docs/mcp.md".to_owned()];
        let packet = build_packet(&inputs(ModelTier::Tier2, "t", &rules, &docs)).unwrap();
        assert_eq!(packet.memory_rule_ids, ["R4"]);
    }

    #[test]
    fn memory_rules_parse_from_markdown() {
        let parsed = parse_memory_rules(
            "# Memory\n- RULE-1 [crates/engine/**]: never block the runtime (verified by abc123)\n- RULE-2 [docs/**]: no marketing\nnot a rule\n",
        );
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "RULE-1");
        assert_eq!(parsed[0].rule, "never block the runtime");
        assert_eq!(parsed[0].verified_by.as_deref(), Some("abc123"));
        assert_eq!(parsed[1].verified_by, None);
        assert_eq!(parsed[0].line(), "- RULE-1 [crates/engine/**]: never block the runtime (verified by abc123)");
    }

    #[test]
    fn tier1_carries_the_four_line_scaffold_and_tier3_does_not() {
        let tier1 = build_packet(&inputs(ModelTier::Tier1, "t", &[], &[])).unwrap();
        for line in TIER1_SCAFFOLD {
            assert!(tier1.layers[0].contains(line), "{line}");
        }
        assert_eq!(TIER1_SCAFFOLD.len(), 4);
        // The scaffold sits in layer 1, so it is part of the cached prefix.
        assert!(!tier1.layers[3].contains("hypothesis"));
        let tier3 = build_packet(&inputs(ModelTier::Tier3, "t", &[], &[])).unwrap();
        assert!(!tier3.render().contains("OPERATING PROTOCOL"));
    }

    #[test]
    fn tier3_memory_is_capped_at_200_lines() {
        let rules: Vec<MemoryRule> = (0..250).map(|n| rule(&format!("R{n}"), "**")).collect();
        let mut input = inputs(ModelTier::Tier3, "t", &rules, &[]);
        input.contracts = "";
        let packet = build_packet(&input);
        // 200 short rules fit in 6,000 tokens; the cap bounds the section.
        let packet = match packet {
            Ok(packet) => packet,
            Err(PacketError::OverBudget(over)) => panic!("{over}"),
            Err(other) => panic!("{other}"),
        };
        assert_eq!(packet.layers[2].lines().count(), TIER3_MEMORY_LINE_CAP);
        assert_eq!(packet.memory_rule_ids.len(), TIER3_MEMORY_LINE_CAP);
    }

    #[test]
    fn tier1_over_3000_tokens_is_refused_and_tier2_takes_the_same_packet() {
        let big = "x".repeat(12_100);
        match build_packet(&inputs(ModelTier::Tier1, &big, &[], &[])) {
            Err(PacketError::OverBudget(over)) => {
                assert_eq!(over.cap_tokens, 3_000);
                assert!(over.estimated_tokens > 3_000);
                assert_eq!(over.tier, ModelTier::Tier1);
                let message = over.to_string();
                assert!(message.starts_with("PacketOverBudget:"), "{message}");
                assert!(message.contains("split the ticket finer first"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        let packet = build_packet(&inputs(ModelTier::Tier2, &big, &[], &[])).unwrap();
        assert!(packet.estimated_tokens > 3_000 && packet.estimated_tokens <= 6_000);
        let huge = "x".repeat(25_000);
        let Err(PacketError::OverBudget(over)) = build_packet(&inputs(ModelTier::Tier2, &huge, &[], &[])) else {
            panic!("tier 2 cap not enforced");
        };
        assert!(over.to_string().contains("no larger tier"));
    }

    #[test]
    fn a_packet_at_the_cap_is_accepted() {
        // 3,000 tokens is 12,000 bytes of rendered packet.
        let mut task = String::new();
        loop {
            let packet = build_packet(&inputs(ModelTier::Tier1, &task, &[], &[]));
            match packet {
                Ok(p) if p.estimated_tokens >= 2_999 => break,
                Ok(_) => task.push_str("word "),
                Err(error) => panic!("{error}"),
            }
        }
    }

    #[test]
    fn leaf_view_drops_manager_and_orchestration_entries() {
        let entries = vec![
            entry(mast_proto::MANAGER_ROLE_INSTRUCTION_ID, "You coordinate workers.", 0),
            entry("a", "Run df -h before builds.", 1),
            entry("b", "[orchestration]\nBatch three tasks at a time.", 2),
            entry("c", "Keep commits small.", 3),
        ];
        let view = worker_instruction_entries(&entries);
        let ids: Vec<&str> = view.iter().map(|e| e.instruction_id.as_str()).collect();
        assert_eq!(ids, ["a", "c"]);
        assert_eq!(view.iter().map(|e| e.order).collect::<Vec<_>>(), [0, 1]);

        let mut input = inputs(ModelTier::Tier2, "t", &[], &[]);
        input.instructions = &entries;
        let text = build_packet(&input).unwrap().render();
        assert!(text.contains("Run df -h before builds."));
        assert!(text.contains("Keep commits small."));
        assert!(!text.contains("Batch three tasks"));
        assert!(!text.contains("You coordinate workers"));
        assert!(text.contains("<mast_work_outline_instructions "));
    }

    const DOC: &str = "# Plan\nintro\n\n## Contracts\nC1\n\n### Sub\nsub text\n\n## Task W3-T4\nbody of the task\n\n```\n# not a heading\n```\n\n## Task W3-T5\nother\n";

    #[test]
    fn headings_ignore_fenced_code_and_get_unique_slugs() {
        let found = sections("# A\n```\n# B\n```\n# A\n");
        assert_eq!(found.iter().map(|s| s.slug.as_str()).collect::<Vec<_>>(), ["a", "a-1"]);
        let doc = sections(DOC);
        assert_eq!(doc.iter().map(|s| s.slug.as_str()).collect::<Vec<_>>(), ["plan", "contracts", "sub", "task-w3-t4", "task-w3-t5"]);
        let contracts = &doc[1];
        assert_eq!(&DOC[contracts.start..contracts.end], "## Contracts\nC1\n\n### Sub\nsub text\n\n");
    }

    #[test]
    fn slice_cuts_at_heading_boundaries() {
        let mut doc = String::from("# Top\n");
        for n in 0..6 {
            doc.push_str(&format!("## Part {n}\n{}\n\n", "word ".repeat(100)));
        }
        let slice = read_slice(&doc, "plan:a@1", None, Some(MIN_SLICE_TOKENS * 3)).unwrap();
        assert!(slice.truncated);
        assert!(!slice.cut_inside_section);
        assert!(slice.text.ends_with("word"), "ends on a section end: {:?}", &slice.text[slice.text.len() - 20..]);
        assert!(slice.estimated_tokens <= MIN_SLICE_TOKENS * 3);
        let next = slice.next_anchor.clone().unwrap();
        assert!(next.starts_with("part-"), "{next}");
        // Following the anchor continues where the cut stopped.
        let more = read_slice(&doc, "plan:a@1", Some(&next), Some(300)).unwrap();
        assert!(more.text.starts_with(&format!("## Part {}", &next[5..])));
        // Nothing is cut when it fits.
        let all = read_slice(&doc, "plan:a@1", None, Some(MAX_SLICE_TOKENS)).unwrap();
        assert!(!all.truncated && all.next_anchor.is_none());
        assert_eq!(all.text, doc.trim_end());
    }

    #[test]
    fn slice_by_anchor_stays_inside_the_section() {
        let slice = read_slice(DOC, "k", Some("contracts"), None).unwrap();
        assert!(slice.text.starts_with("## Contracts") && slice.text.contains("sub text"));
        assert!(!slice.text.contains("W3-T4") && !slice.truncated);
        assert_eq!(slice.anchor.as_deref(), Some("contracts"));
        // A unique slug prefix and the title both resolve.
        let by_prefix = read_slice(DOC, "k", Some("task-w3-t4"), None).unwrap();
        assert!(by_prefix.text.contains("body of the task") && !by_prefix.text.contains("other"));
        assert_eq!(read_slice(DOC, "k", Some("Task W3-T4"), None).unwrap().text, by_prefix.text);
        assert!(by_prefix.text.contains("# not a heading"), "fence kept whole");
        assert!(matches!(read_slice(DOC, "k", Some("task"), None), Err(SliceError::AmbiguousAnchor { .. })));
        let Err(SliceError::UnknownAnchor { available, .. }) = read_slice(DOC, "k", Some("nope"), None) else {
            panic!("unknown anchor accepted");
        };
        assert!(available.contains(&"contracts".to_owned()));
    }

    #[test]
    fn an_oversized_section_is_cut_at_a_paragraph_not_mid_word() {
        let para = format!("{}\n\n", "alpha beta ".repeat(60));
        let doc = format!("# Big\n{}", para.repeat(10));
        let slice = read_slice(&doc, "k", None, Some(MIN_SLICE_TOKENS)).unwrap();
        assert!(slice.truncated && slice.cut_inside_section);
        assert!(slice.next_anchor.is_none());
        assert!(slice.text.ends_with("beta"), "{:?}", slice.text);
        assert!(slice.estimated_tokens <= MIN_SLICE_TOKENS);
        // Multibyte text never splits a character.
        let doc = "é".repeat(2_000);
        let slice = read_slice(&doc, "k", None, Some(MIN_SLICE_TOKENS)).unwrap();
        assert!(slice.truncated && slice.text.chars().all(|c| c == 'é'));
    }

    #[test]
    fn replay_ids_are_stable_for_the_same_bytes_and_differ_otherwise() {
        let a = read_slice(DOC, "plan:a@1", Some("contracts"), None).unwrap();
        let b = read_slice(DOC, "plan:a@1", Some("contracts"), Some(2_000)).unwrap();
        assert_eq!(a.replay_id, b.replay_id);
        assert!(a.replay_id.starts_with("cs_") && a.replay_id.len() == 15);
        let other_rev = read_slice(DOC, "plan:a@2", Some("contracts"), None).unwrap();
        assert_ne!(a.replay_id, other_rev.replay_id);
        let other_section = read_slice(DOC, "plan:a@1", Some("task-w3-t4"), None).unwrap();
        assert_ne!(a.replay_id, other_section.replay_id);
    }

    #[test]
    fn plan_extract_finds_contracts_and_the_task_section() {
        assert_eq!(task_ref("W3-T4 · ContextPackBuilder"), Some("W3-T4".to_owned()));
        assert_eq!(task_ref("Fix the build"), None);
        let extract = extract_plan(DOC, Some("W3-T4"));
        assert!(extract.contracts.starts_with("## Contracts") && extract.contracts.contains("sub text"));
        assert!(extract.task_slice.starts_with("## Task W3-T4"));
        assert!(!extract.task_slice.contains("other"));
        assert_eq!(extract.task_anchor.as_deref(), Some("task-w3-t4"));
        assert!(extract.outline.iter().any(|l| l.starts_with("task-w3-t5:")));
        let none = extract_plan(DOC, Some("W9-T9"));
        assert!(none.task_slice.is_empty() && none.task_anchor.is_none());
    }

    #[test]
    fn read_chat_fence_refuses_siblings_and_windows_the_predecessor() {
        let predecessors = ["old".to_owned()];
        let facts = |target: &'static str, target_parent: Option<&'static str>| ReadChatFacts {
            caller_chat_id: "me",
            caller_parent_id: Some("boss"),
            target_chat_id: target,
            target_parent_id: target_parent,
            recorded_predecessors: &predecessors,
        };
        assert_eq!(classify_chat(&facts("me", Some("boss"))), ChatRelation::Own);
        assert_eq!(classify_chat(&facts("boss", None)), ChatRelation::Parent);
        assert_eq!(classify_chat(&facts("kid", Some("me"))), ChatRelation::Child);
        assert_eq!(classify_chat(&facts("sib", Some("boss"))), ChatRelation::Sibling);
        assert_eq!(classify_chat(&facts("old", Some("boss"))), ChatRelation::Predecessor, "a predecessor that is also a sibling stays readable");
        assert_eq!(classify_chat(&facts("far", Some("elsewhere"))), ChatRelation::Other);

        assert!(worker_read_chat_window(ChatRelation::Sibling, Some(1)).is_err());
        assert!(worker_read_chat_window(ChatRelation::Other, None).is_err());
        assert_eq!(worker_read_chat_window(ChatRelation::Predecessor, Some(500)), Ok(Some(6)));
        assert_eq!(worker_read_chat_window(ChatRelation::Predecessor, None), Ok(Some(6)));
        assert_eq!(worker_read_chat_window(ChatRelation::Predecessor, Some(2)), Ok(Some(2)));
        assert_eq!(worker_read_chat_window(ChatRelation::Own, Some(500)), Ok(Some(500)));
        assert!(worker_read_chat_window(ChatRelation::Sibling, None).unwrap_err().to_string().contains("sibling"));
    }

    #[test]
    fn tool_definition_is_under_550_bytes_and_args_validate() {
        let size = serde_json::to_string(&tool_definition()).unwrap().len();
        assert!(size < 550, "{size} bytes");
        let args: ContextReadSliceArgs =
            serde_json::from_value(json!({"target": "plan#task-w3-t4", "maxTokens": 800, "reason": "need the doc rules"})).unwrap();
        assert_eq!(args.validate(), Ok(SliceTarget::Plan { anchor: Some("task-w3-t4".into()) }));
        let no_reason = ContextReadSliceArgs { target: "plan".into(), max_tokens: None, reason: " ".into() };
        assert!(no_reason.validate().unwrap_err().contains("reason"));
        let wrong = ContextReadSliceArgs { target: "file:/etc/passwd".into(), max_tokens: None, reason: "x".into() };
        assert!(wrong.validate().unwrap_err().contains("unknown target"));
    }
}
