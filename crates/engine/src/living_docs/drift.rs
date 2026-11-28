//! Tier A structural drift: read a Markdown document against the repository,
//! spending no model tokens.
//!
//! Only inline code spans and link destinations are read; fenced blocks are
//! examples. A false alarm costs more than a miss, so each check is narrow:
//!
//! - a **tool id** is a snake_case span starting with one of [`TOOL_PREFIXES`],
//!   or any `mcp__mast-agent__<tool>` span;
//! - a **CLI command** is a span starting `mast <word>`;
//! - a **file reference** is a relative link, or a span under a top-level
//!   directory of the repo, ending in a known extension, optionally with
//!   `:line`, `:line-line` or `#Lline`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Families of Mast agent tools whose names are not also used for ordinary
/// identifiers. Broader prefixes (`artifact_`, `review_`, `mast_`, `peer_`)
/// would flag widget ids, test names, crate names and modules.
pub const TOOL_PREFIXES: &[&str] = &[
    "work_outline_",
    "work_queue_",
    "work_item_external_run_",
    "artifact_get_",
    "artifact_render_",
    "peer_push_",
    "peer_fetch_",
    "peer_command_",
    "context_read_",
];

/// Endings that name a field or counter, not a tool.
const NOT_TOOL_SUFFIXES: &[&str] = &["_", "_id", "_ids", "_at", "_ms", "_count", "_generation"];

/// Server prefix under which agents see Mast's tools.
const MCP_TOOL_PREFIX: &str = "mcp__mast-agent__";

/// Directories whose contents are built or installed, not tracked.
const GENERATED_DIRS: &[&str] = &["build", "target", "node_modules"];

const FILE_EXTENSIONS: &[&str] = &[
    "rs", "md", "toml", "json", "ts", "tsx", "js", "mjs", "cjs", "py", "sh", "swift", "yml",
    "yaml", "css", "html", "sql", "txt", "lock", "plist", "service", "svg", "png", "jpg",
];

/// The names that exist in code, supplied by the caller. An empty set turns
/// its check off.
#[derive(Debug, Clone, Default)]
pub struct KnownSymbols {
    pub tool_names: BTreeSet<String>,
    /// `headless`, or `daemon install` to also check a command's own
    /// subcommands. Include hidden commands.
    pub cli_subcommands: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DriftKind {
    MissingFile,
    LinePastEof,
    UnknownTool,
    UnknownCliSubcommand,
    #[default]
    #[serde(other)]
    Other,
}

/// One reference that no longer matches the code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DriftFinding {
    pub kind: DriftKind,
    /// The reference as written, or the missing tool or command.
    pub symbol: String,
    /// 1-based line of its first appearance in the document.
    pub doc_line: usize,
}

/// The tree a document's references are resolved in. Paths are relative to the
/// repository root, normalized, with no `..`.
pub trait RepoTree {
    fn is_dir(&self, path: &str) -> bool;
    /// Lines in the file at `path`; `None` when there is no file there.
    fn line_count(&self, path: &str) -> Option<usize>;
}

/// A checked-out repository.
pub struct FsRepoTree {
    root: PathBuf,
}

impl FsRepoTree {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl RepoTree for FsRepoTree {
    fn is_dir(&self, path: &str) -> bool {
        self.root.join(path).is_dir()
    }

    fn line_count(&self, path: &str) -> Option<usize> {
        let path = self.root.join(path);
        if !path.is_file() {
            return None;
        }
        // A file that exists but cannot be read must not look missing or short.
        let Ok(bytes) = std::fs::read(path) else {
            return Some(usize::MAX);
        };
        let newlines = bytes.iter().filter(|byte| **byte == b'\n').count();
        Some(newlines + usize::from(bytes.last().is_some_and(|last| *last != b'\n')))
    }
}

/// Check one Markdown document. `doc_path` is its repo path, or `None` for an
/// artifact, whose references are relative to the root. Each missing symbol is
/// reported once, in document order.
pub fn check_markdown(
    text: &str,
    doc_path: Option<&str>,
    known: &KnownSymbols,
    tree: &dyn RepoTree,
) -> Vec<DriftFinding> {
    let doc_dir = doc_path
        .and_then(|path| path.rsplit_once('/'))
        .map_or("", |(dir, _)| dir);
    let mut references = Vec::new();
    for (first_line, block) in blocks(text) {
        scan_block(first_line, &block, &mut references);
    }
    references.sort_by_key(|reference| reference.line);
    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();
    for reference in references {
        let found = match reference.source {
            Source::Code => check_tool(&reference.text, known)
                .or_else(|| check_command(&reference.text, known))
                .or_else(|| check_file(&reference.text, false, doc_dir, tree)),
            Source::Link => check_file(&reference.text, true, doc_dir, tree),
        };
        if let Some((kind, symbol)) = found
            && seen.insert((kind, symbol.clone()))
        {
            findings.push(DriftFinding {
                kind,
                symbol,
                doc_line: reference.line,
            });
        }
    }
    findings
}

enum Source {
    Code,
    Link,
}

struct Reference {
    source: Source,
    text: String,
    line: usize,
}

/// The prose blocks outside fenced code, each with its first line number. A
/// stray backtick cannot pair across a blank line, list item, heading or table
/// row.
fn blocks(text: &str) -> Vec<(usize, String)> {
    let mut blocks = Vec::new();
    let mut current: Option<(usize, String)> = None;
    let mut fence: Option<(char, usize)> = None;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start_matches([' ', '\t', '>']);
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'));
        let run = marker.map_or(0, |marker| {
            trimmed.chars().take_while(|c| *c == marker).count()
        });
        if let Some((open, length)) = fence {
            if marker == Some(open) && run >= length && trimmed[run..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        // A backtick fence's info string holds no backticks; ```a``` is a span.
        if run >= 3 && !(marker == Some('`') && trimmed[run..].contains('`')) {
            fence = marker.map(|marker| (marker, run));
            blocks.extend(current.take());
            continue;
        }
        if trimmed.is_empty() {
            blocks.extend(current.take());
            continue;
        }
        if starts_block(trimmed) {
            blocks.extend(current.take());
        }
        match &mut current {
            Some((_, block)) => {
                block.push('\n');
                block.push_str(line);
            }
            None => current = Some((index + 1, line.to_owned())),
        }
    }
    blocks.extend(current);
    blocks
}

fn starts_block(line: &str) -> bool {
    let numbered = line.trim_start_matches(|c: char| c.is_ascii_digit());
    line.starts_with(['#', '|'])
        || ["- ", "* ", "+ "]
            .iter()
            .any(|marker| line.starts_with(marker))
        || (numbered.len() < line.len() && numbered.starts_with(". "))
}

fn scan_block(first_line: usize, block: &str, out: &mut Vec<Reference>) {
    let line_at = |offset: usize| first_line + block[..offset].matches('\n').count();
    let bytes = block.as_bytes();
    let run_at = |start: usize| {
        bytes[start..]
            .iter()
            .take_while(|byte| **byte == b'`')
            .count()
    };
    // The block with code spans blanked, so links inside code are not links.
    let mut prose = bytes.to_vec();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index += 2;
            continue;
        }
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let open = run_at(index);
        let mut close = index + open;
        let found = loop {
            if close >= bytes.len() {
                break false;
            }
            if bytes[close] != b'`' {
                close += 1;
            } else if run_at(close) == open {
                break true;
            } else {
                close += run_at(close);
            }
        };
        if !found {
            index += open;
            continue;
        }
        out.push(Reference {
            source: Source::Code,
            text: block[index + open..close]
                .replace('\n', " ")
                .trim()
                .to_owned(),
            line: line_at(index),
        });
        prose[index..close + open].fill(b' ');
        index = close + open;
    }
    let mut from = 0;
    while let Some(found) = prose[from..].windows(2).position(|pair| pair == b"](") {
        let start = from + found + 2;
        let rest = &prose[start..];
        let destination = if rest.first() == Some(&b'<') {
            rest[1..]
                .iter()
                .position(|byte| *byte == b'>')
                .map(|end| &rest[1..=end])
        } else {
            let mut depth = 0usize;
            rest.iter()
                .position(|byte| match byte {
                    b'(' => {
                        depth += 1;
                        false
                    }
                    b')' if depth > 0 => {
                        depth -= 1;
                        false
                    }
                    b')' => true,
                    byte => byte.is_ascii_whitespace(),
                })
                .map(|end| &rest[..end])
        };
        if let Some(Ok(text)) = destination.map(std::str::from_utf8) {
            out.push(Reference {
                source: Source::Link,
                text: text.to_owned(),
                line: line_at(start),
            });
        }
        from = start;
    }
}

fn check_tool(span: &str, known: &KnownSymbols) -> Option<(DriftKind, String)> {
    if known.tool_names.is_empty() {
        return None;
    }
    let served = span.strip_prefix(MCP_TOOL_PREFIX);
    let name = served.unwrap_or(span);
    let snake_case = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    let looks_like_tool = served.is_some()
        || (TOOL_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
            && !NOT_TOOL_SUFFIXES
                .iter()
                .any(|suffix| name.ends_with(suffix)));
    (snake_case && looks_like_tool && !name.ends_with('_') && !known.tool_names.contains(name))
        .then(|| (DriftKind::UnknownTool, name.to_owned()))
}

fn check_command(span: &str, known: &KnownSymbols) -> Option<(DriftKind, String)> {
    if known.cli_subcommands.is_empty() {
        return None;
    }
    let mut words = span.split_whitespace();
    if words.next() != Some("mast") {
        return None;
    }
    let is_command = |word: &&str| {
        word.starts_with(|c: char| c.is_ascii_lowercase())
            && word
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    let first = words.next().filter(is_command)?;
    let nested = format!("{first} ");
    let has_nested = known
        .cli_subcommands
        .iter()
        .any(|name| name.starts_with(&nested));
    let unknown = |symbol: &str| Some((DriftKind::UnknownCliSubcommand, format!("mast {symbol}")));
    if !has_nested {
        return if known.cli_subcommands.contains(first) {
            None
        } else {
            unknown(first)
        };
    }
    let second = words.next().filter(is_command)?;
    let both = format!("{first} {second}");
    if known.cli_subcommands.contains(&both) {
        None
    } else {
        unknown(&both)
    }
}

fn check_file(
    reference: &str,
    is_link: bool,
    doc_dir: &str,
    tree: &dyn RepoTree,
) -> Option<(DriftKind, String)> {
    // Placeholders, globs, commands, URLs and paths outside the repo.
    if reference.contains("://")
        || reference.contains("::")
        || reference.contains("...")
        || reference.contains(|c: char| c.is_whitespace() || "<>*{}$…|\"'`()[]=,;?%\\".contains(c))
        || reference.starts_with(['/', '~', '#', '-', '@'])
    {
        return None;
    }
    let (path, line) = split_line(reference);
    if path.contains([':', '#']) && !(is_link && path.contains('#')) {
        return None;
    }
    // A link may end in a heading anchor.
    let path = path.split('#').next().unwrap_or_default();
    let extension = path.rsplit('/').next()?.rsplit_once('.')?.1;
    if !FILE_EXTENSIONS.contains(&extension) {
        return None;
    }
    if !is_link {
        let anchored = path.starts_with("./") || path.starts_with("../");
        let first = path.split('/').next().unwrap_or_default();
        if !path.contains('/') || (!anchored && (first.starts_with('.') || !tree.is_dir(first))) {
            return None;
        }
    }
    // Relative to the document (Markdown) or to the root (planning artifacts).
    let candidates: Vec<String> = [doc_dir, ""]
        .iter()
        .filter_map(|base| normalize(base, path))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let lengths: Vec<usize> = candidates
        .iter()
        .filter_map(|candidate| tree.line_count(candidate))
        .collect();
    if lengths.is_empty() {
        // Build output is absent from a clean tree without having drifted.
        if path
            .split('/')
            .any(|segment| GENERATED_DIRS.contains(&segment))
        {
            return None;
        }
        return Some((DriftKind::MissingFile, reference.to_owned()));
    }
    let line = line?;
    lengths
        .iter()
        .all(|length| line > *length)
        .then(|| (DriftKind::LinePastEof, reference.to_owned()))
}

/// `path`, and the last line a `:12`, `:12-20`, `:12:3` or `#L12-L20` suffix
/// names.
fn split_line(reference: &str) -> (&str, Option<usize>) {
    let number = |text: &str| text.strip_prefix('L').unwrap_or(text).parse::<usize>().ok();
    let last = |range: &str| {
        let mut ends = range.split('-').map(number);
        let first = ends.next()??;
        ends.try_fold(first, |highest, end| Some(highest.max(end?)))
    };
    if let Some((path, anchor)) = reference.split_once("#L") {
        return match last(anchor) {
            Some(line) => (path, Some(line)),
            None => (reference, None),
        };
    }
    let Some((path, suffix)) = reference.split_once(':') else {
        return (reference, None);
    };
    let mut parts = suffix.split(':');
    match parts.next().and_then(last) {
        Some(line) if parts.all(|column| column.parse::<usize>().is_ok()) => (path, Some(line)),
        _ => (reference, None),
    }
}

/// `base/path` with `.` and `..` resolved; `None` when it leaves the root.
fn normalize(base: &str, path: &str) -> Option<String> {
    let mut segments = Vec::new();
    for segment in base.split('/').chain(path.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            segment => segments.push(segment),
        }
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Tree(HashMap<&'static str, usize>);

    impl RepoTree for Tree {
        fn is_dir(&self, path: &str) -> bool {
            self.0
                .keys()
                .any(|file| file.starts_with(&format!("{path}/")))
        }
        fn line_count(&self, path: &str) -> Option<usize> {
            self.0.get(path).copied()
        }
    }

    fn tree() -> Tree {
        Tree(HashMap::from([
            ("crates/engine/src/lib.rs", 100),
            ("docs/mcp.md", 10),
            ("docs/internal/peer-link.md", 10),
            ("README.md", 5),
        ]))
    }

    fn known() -> KnownSymbols {
        let set = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
        KnownSymbols {
            tool_names: set(&["work_outline_read_current", "read_chat"]),
            cli_subcommands: set(&["headless", "mcp", "daemon install", "daemon status"]),
        }
    }

    fn check(text: &str) -> Vec<(DriftKind, String)> {
        check_markdown(text, Some("docs/mcp.md"), &known(), &tree())
            .into_iter()
            .map(|finding| (finding.kind, finding.symbol))
            .collect()
    }

    #[test]
    fn a_removed_file_is_flagged_in_spans_and_links() {
        assert_eq!(
            check(
                "See `crates/engine/src/gone.rs` and [guide](internal/gone.md#setup \"measured\")."
            ),
            [
                (
                    DriftKind::MissingFile,
                    "crates/engine/src/gone.rs".to_owned()
                ),
                (DriftKind::MissingFile, "internal/gone.md#setup".to_owned()),
            ]
        );
    }

    #[test]
    fn a_line_past_the_end_is_flagged() {
        for reference in [
            "crates/engine/src/lib.rs:101",
            "crates/engine/src/lib.rs:90-120",
        ] {
            assert_eq!(
                check(&format!("`{reference}`")),
                [(DriftKind::LinePastEof, reference.to_owned())]
            );
        }
        assert_eq!(check("[x](../crates/engine/src/lib.rs#L400)").len(), 1);
        assert!(check("`crates/engine/src/lib.rs:100` `crates/engine/src/lib.rs:4:9`").is_empty());
    }

    #[test]
    fn unknown_tools_and_commands_are_flagged_once_with_their_line() {
        let text = "Intro.\n\nCall `work_outline_vanish`,\nthen `mcp__mast-agent__old_tool` and `work_outline_vanish`.\n\
                    \n- `mast serve --now`\n- `mast daemon reload`";
        let findings = check_markdown(text, None, &known(), &tree());
        let got: Vec<(&str, usize)> = findings
            .iter()
            .map(|f| (f.symbol.as_str(), f.doc_line))
            .collect();
        assert_eq!(
            got,
            [
                ("work_outline_vanish", 3),
                ("old_tool", 4),
                ("mast serve", 6),
                ("mast daemon reload", 7)
            ]
        );
    }

    #[test]
    fn ordinary_code_and_prose_are_not_flagged() {
        let text = r#"
Prose names work_outline_vanish and crates/engine/src/gone.rs without code spans.
Fields `work_item_id`, `owed_since_commit`, `read_only`, `mast_proto`, `artifact_card`,
`work_outline_inputs_generation`, the `work_outline_*` family and `work_outline_workflow_pack_`.
Known: `work_outline_read_current`, `mcp__mast-agent__read_chat`, `mcp__mast-agent__<tool>`.
Commands `mast`, `mast headless`, `mast mcp --peer NAME`, `mast daemon <command>`,
`mast daemon install`, `mast --version`, `cargo test -p mast-engine`.
Paths `src/main.rs`, `application/json`, `a/b.rs`, `~/.mast/settings.json`, `/etc/hosts.toml`,
`docs/changelog.d/<id>.md`, `crates/*/Cargo.toml`, `crates/engine/src/{a,b}.rs`, `docs/.../x.md`,
`.mast/artifacts/x.md`, `docs/build/out.png`, [out](../target/report.md), `crates/engine/src/lib.rs::run`, `docs/changelog.d`, `crates/engine/`.
Links [web](https://example.test/gone.md), [artifact](mast-artifact:abc), [anchor](#gone),
[ok](internal/peer-link.md), [root](docs/internal/peer-link.md), [up](../README.md),
[outside](../../elsewhere.md), [local](file:///tmp/gone.rs), `[in code](gone.md)`.

```sh
mast removed-command   # `work_outline_vanish` `crates/gone.rs`
[gone](gone.md)
```

~~~
`work_outline_vanish`
~~~
"#;
        assert_eq!(check(text), []);
    }

    #[test]
    fn empty_known_sets_turn_those_checks_off() {
        let findings = check_markdown(
            "`work_outline_vanish` `mast vanish`",
            None,
            &KnownSymbols::default(),
            &tree(),
        );
        assert!(findings.is_empty());
    }

    #[test]
    fn a_checked_out_tree_counts_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "one\ntwo").unwrap();
        std::fs::write(dir.path().join("b.rs"), "one\n").unwrap();
        let tree = FsRepoTree::new(dir.path());
        assert_eq!(tree.line_count("a.rs"), Some(2));
        assert_eq!(tree.line_count("b.rs"), Some(1));
        assert_eq!(tree.line_count("c.rs"), None);
    }
}
