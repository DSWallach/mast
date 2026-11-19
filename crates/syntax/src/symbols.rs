//! Definition captures from the tags queries shipped with linked grammars.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, OnceLock},
};
use tree_sitter::StreamingIterator;

use crate::{DEFAULT_MAX_SOURCE_BYTES, LanguageId, language_for_path};

/// Upper bound for the index's owned entries, including an allowance per tree node.
pub const MAX_INDEX_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SymbolKind {
    Function,
    Method,
    Type,
    Constant,
    Test,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// One-based, inclusive source lines.
    pub start_line: u32,
    pub end_line: u32,
}

/// A bounded snapshot. The owner supplies only files changed by a watch event
/// and performs file reads and calls to `apply` on a background worker.
#[derive(Clone)]
pub struct SymbolIndex {
    revision: Option<u64>,
    files: im::OrdMap<String, Arc<SymbolFile>>,
    symbol_count: usize,
    retained_bytes: usize,
    truncated: bool,
    max_files: usize,
    max_symbols: usize,
    max_symbols_per_file: usize,
}

pub struct SymbolFile {
    pub lower_path: String,
    pub lower_names: Vec<String>,
    pub symbols: Vec<Symbol>,
}

impl std::ops::Deref for SymbolFile {
    type Target = [Symbol];
    fn deref(&self) -> &Self::Target { &self.symbols }
}

pub enum FileChange<'a> {
    Changed { path: &'a str, source: &'a str },
    Removed { path: &'a str },
}

impl SymbolIndex {
    pub fn new(max_files: usize, max_symbols: usize, max_symbols_per_file: usize) -> Self {
        Self {
            revision: None,
            files: im::OrdMap::new(),
            symbol_count: 0,
            retained_bytes: 0,
            truncated: false,
            max_files,
            max_symbols,
            max_symbols_per_file,
        }
    }

    /// Replaying an already applied revision does no parsing or allocation.
    pub fn apply(&mut self, revision: u64, changes: &[FileChange<'_>]) -> usize {
        if self.revision.is_some_and(|previous| revision <= previous) {
            return 0;
        }
        let mut parsed = 0;
        for change in changes {
            let path = match change {
                FileChange::Changed { path, .. } | FileChange::Removed { path } => *path,
            };
            if path.len() > 4096 {
                continue;
            }
            if let Some(previous) = self.files.remove(path) {
                self.symbol_count -= previous.len();
                self.retained_bytes -= estimated_file_bytes(path, &previous);
            }
            if let FileChange::Changed { source, .. } = change {
                if self.files.len() >= self.max_files || self.symbol_count >= self.max_symbols {
                    self.truncated = true;
                    continue;
                }
                let limit = self
                    .max_symbols_per_file
                    .min(self.max_symbols - self.symbol_count);
                let mut symbols = extract(source, path, limit.saturating_add(1));
                if symbols.len() > limit {
                    self.truncated = true;
                    symbols.truncate(limit);
                }
                parsed += 1;
                let mut file = SymbolFile { lower_path: path.to_lowercase(), lower_names: symbols.iter().map(|symbol| symbol.name.to_lowercase()).collect(), symbols };
                file.symbols.shrink_to_fit();
                file.lower_names.shrink_to_fit();
                let mut needed = estimated_file_bytes(path, &file);
                while !file.symbols.is_empty()
                    && self.retained_bytes.saturating_add(needed) > MAX_INDEX_PAYLOAD_BYTES
                {
                    self.truncated = true;
                    file.symbols.pop();
                    file.lower_names.pop();
                    file.symbols.shrink_to_fit();
                    file.lower_names.shrink_to_fit();
                    needed = estimated_file_bytes(path, &file);
                }
                file.symbols.shrink_to_fit();
                file.lower_names.shrink_to_fit();
                needed = estimated_file_bytes(path, &file);
                if !file.symbols.is_empty() {
                    self.symbol_count += file.symbols.len();
                    self.retained_bytes += needed;
                    self.files.insert(path.to_owned(), Arc::new(file));
                }
            }
        }
        self.revision = Some(revision);
        parsed
    }

    pub fn files(&self) -> &im::OrdMap<String, Arc<SymbolFile>> {
        &self.files
    }

    pub fn symbol_count(&self) -> usize {
        self.symbol_count
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Approximation of owned payload, including vector capacity and names.
    pub fn estimated_bytes(&self) -> usize {
        self.retained_bytes
    }
}

fn estimated_file_bytes(path: &str, file: &SymbolFile) -> usize {
    // im::OrdMap's node allocation is shared by several entries. Allow 128
    // bytes per entry in addition to the path and symbol payload.
    128 + path.len() + file.lower_path.capacity()
        + file.symbols.capacity() * std::mem::size_of::<Symbol>()
        + file.lower_names.capacity() * std::mem::size_of::<String>()
        + file.lower_names.iter().map(String::capacity).sum::<usize>()
        + file.symbols
            .iter()
            .map(|symbol| symbol.name.capacity())
            .sum::<usize>()
}

/// Returns the linked grammar and its tags query, if the grammar exposes one.
fn tags(language: LanguageId) -> Option<(tree_sitter::Language, String)> {
    use LanguageId::*;
    let (grammar, query) = match language {
        Rust => (
            tree_sitter_rust::LANGUAGE.into(),
            format!("{}\n(const_item name: (identifier) @name) @definition.constant", tree_sitter_rust::TAGS_QUERY),
        ),
        JavaScript | Jsx => (
            tree_sitter_javascript::LANGUAGE.into(),
            format!("{}\n(lexical_declaration kind: \"const\" (variable_declarator name: (identifier) @name)) @definition.constant", tree_sitter_javascript::TAGS_QUERY),
        ),
        TypeScript | Tsx => (
            if language == Tsx {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            } else {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
            },
            format!(
                "{}\n{}\n(lexical_declaration kind: \"const\" (variable_declarator name: (identifier) @name)) @definition.constant",
                tree_sitter_javascript::TAGS_QUERY,
                tree_sitter_typescript::TAGS_QUERY
            ),
        ),
        Python => (
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::TAGS_QUERY.to_owned(),
        ),
        Go => (
            tree_sitter_go::LANGUAGE.into(),
            format!("{}\n(const_spec name: (identifier) @name) @definition.constant", tree_sitter_go::TAGS_QUERY),
        ),
        C => (
            tree_sitter_c::LANGUAGE.into(),
            format!("{}\n((declaration (type_qualifier) @qualifier declarator: (init_declarator declarator: (identifier) @name)) @definition.constant (#eq? @qualifier \"const\"))\n((declaration (type_qualifier) @qualifier declarator: (identifier) @name) @definition.constant (#eq? @qualifier \"const\"))", tree_sitter_c::TAGS_QUERY),
        ),
        Cpp => (
            tree_sitter_cpp::LANGUAGE.into(),
            format!("{}\n((declaration (type_qualifier) @qualifier declarator: (init_declarator declarator: (identifier) @name)) @definition.constant (#eq? @qualifier \"const\"))\n((declaration (type_qualifier) @qualifier declarator: (identifier) @name) @definition.constant (#eq? @qualifier \"const\"))", tree_sitter_cpp::TAGS_QUERY),
        ),
        CSharp => (
            tree_sitter_c_sharp::LANGUAGE.into(),
            format!("{}\n((field_declaration (modifier) @modifier (variable_declaration (variable_declarator name: (identifier) @name))) @definition.constant (#eq? @modifier \"const\"))\n((local_declaration_statement (modifier) @modifier (variable_declaration (variable_declarator name: (identifier) @name))) @definition.constant (#eq? @modifier \"const\"))", tree_sitter_c_sharp::TAGS_QUERY),
        ),
        Java => (
            tree_sitter_java::LANGUAGE.into(),
            format!("{}\n(field_declaration (modifiers \"final\") declarator: (variable_declarator name: (identifier) @name)) @definition.constant", tree_sitter_java::TAGS_QUERY),
        ),
        Swift => (
            tree_sitter_swift::LANGUAGE.into(),
            format!("{}\n(property_declaration (value_binding_pattern mutability: \"let\") name: (pattern bound_identifier: (simple_identifier) @name)) @definition.constant", tree_sitter_swift::TAGS_QUERY),
        ),
        Ruby => (
            tree_sitter_ruby::LANGUAGE.into(),
            format!("{}\n(assignment left: (constant) @name) @definition.constant", tree_sitter_ruby::TAGS_QUERY),
        ),
        Php => (
            tree_sitter_php::LANGUAGE_PHP.into(),
            format!("{}\n(const_declaration (const_element (name) @name)) @definition.constant", tree_sitter_php::TAGS_QUERY),
        ),
        Lua => (
            tree_sitter_lua::LANGUAGE.into(),
            tree_sitter_lua::TAGS_QUERY.to_owned(),
        ),
        _ => return None,
    };
    Some((grammar, query))
}

fn compiled_tags(
    language: LanguageId,
) -> Option<&'static (tree_sitter::Language, tree_sitter::Query)> {
    static QUERIES: OnceLock<HashMap<LanguageId, (tree_sitter::Language, tree_sitter::Query)>> =
        OnceLock::new();
    QUERIES
        .get_or_init(|| {
            [
                LanguageId::Rust,
                LanguageId::JavaScript,
                LanguageId::Jsx,
                LanguageId::TypeScript,
                LanguageId::Tsx,
                LanguageId::Python,
                LanguageId::Go,
                LanguageId::C,
                LanguageId::Cpp,
                LanguageId::CSharp,
                LanguageId::Java,
                LanguageId::Swift,
                LanguageId::Ruby,
                LanguageId::Php,
                LanguageId::Lua,
            ]
            .into_iter()
            .filter_map(|id| {
                let (grammar, source) = tags(id)?;
                let query = tree_sitter::Query::new(&grammar, &source).ok()?;
                Some((id, (grammar, query)))
            })
            .collect()
        })
        .get(&language)
}

pub fn supports_path(path: &str) -> bool {
    matches!(
        language_for_path(path),
        Some(
            LanguageId::Rust
                | LanguageId::JavaScript
                | LanguageId::Jsx
                | LanguageId::TypeScript
                | LanguageId::Tsx
                | LanguageId::Python
                | LanguageId::Go
                | LanguageId::C
                | LanguageId::Cpp
                | LanguageId::CSharp
                | LanguageId::Java
                | LanguageId::Swift
                | LanguageId::Ruby
                | LanguageId::Php
                | LanguageId::Lua
        )
    )
}

/// Parse identifier nodes into byte ranges for hit testing. Call on a background worker.
pub fn identifier_ranges(source: &str, path: &str) -> Vec<std::ops::Range<usize>> {
    if source.len() > DEFAULT_MAX_SOURCE_BYTES { return Vec::new(); }
    let Some(language_id) = language_for_path(path) else { return Vec::new(); };
    let Some((language, _)) = compiled_tags(language_id) else { return Vec::new(); };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(language).is_err() { return Vec::new(); }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    let mut progress = |_: &tree_sitter::ParseState| if std::time::Instant::now() >= deadline { std::ops::ControlFlow::Break(()) } else { std::ops::ControlFlow::Continue(()) };
    let bytes = source.as_bytes();
    let Some(tree) = parser.parse_with_options(&mut |offset, _| &bytes[offset..], None, Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress))) else { return Vec::new(); };
    let mut cursor = tree.walk();
    let mut ranges = Vec::new();
    loop {
        let node = cursor.node();
        let identifier = node.kind().ends_with("identifier")
            || (language_id == LanguageId::Ruby && node.kind() == "constant")
            || (language_id == LanguageId::Php && node.kind() == "name");
        if identifier && node.child_count() == 0 {
            ranges.push(node.byte_range());
        }
        if cursor.goto_first_child() { continue; }
        loop {
            if cursor.goto_next_sibling() { break; }
            if !cursor.goto_parent() { return ranges; }
        }
    }
}

/// Extract definitions from one file. The caller owns scheduling and caching.
pub fn extract(source: &str, path: &str, max_symbols: usize) -> Vec<Symbol> {
    if source.len() > DEFAULT_MAX_SOURCE_BYTES || max_symbols == 0 || source.as_bytes().split(|byte| *byte == b'\n').any(|line| line.len() > 20_000) {
        return Vec::new();
    }
    let Some(language_id) = language_for_path(path) else {
        return Vec::new();
    };
    let Some((language, query)) = compiled_tags(language_id) else {
        return Vec::new();
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(language).is_err() {
        return Vec::new();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    let mut progress = |_: &tree_sitter::ParseState| if std::time::Instant::now() >= deadline { std::ops::ControlFlow::Break(()) } else { std::ops::ControlFlow::Continue(()) };
    let options = tree_sitter::ParseOptions::new().progress_callback(&mut progress);
    let bytes = source.as_bytes();
    let Some(tree) = parser.parse_with_options(&mut |offset, _| &bytes[offset..], None, Some(options)) else {
        return Vec::new();
    };
    let names = query.capture_names();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut symbols = BTreeMap::new();
    while let Some(found) = matches.next() {
        let mut name = None;
        let mut definition = None;
        for capture in found.captures {
            let capture_name = names[capture.index as usize];
            if capture_name == "name" {
                name = Some(capture.node);
            }
            let kind = match capture_name {
                "definition.function" => Some(SymbolKind::Function),
                "definition.method" => Some(SymbolKind::Method),
                "definition.constant" => Some(SymbolKind::Constant),
                "definition.class"
                | "definition.interface"
                | "definition.type"
                | "definition.struct"
                | "definition.enum" => Some(SymbolKind::Type),
                _ => None,
            };
            if let Some(kind) = kind {
                definition = Some((capture.node, kind));
            }
        }
        if let (Some(name), Some((node, mut kind))) = (name, definition) {
            let node = definition_extent(node);
            let Ok(value) = name.utf8_text(source.as_bytes()) else {
                continue;
            };
            if value.len() > 512 {
                continue;
            }
            if matches!(kind, SymbolKind::Function | SymbolKind::Method)
                && is_test(language_id, node, value, source)
            {
                kind = SymbolKind::Test;
            }
            let symbol = Symbol {
                name: value.to_owned(),
                kind,
                start_line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
            };
            let key = (name.start_byte(), name.end_byte());
            if let Some(old) = symbols.get_mut(&key) {
                if matches!(kind, SymbolKind::Method | SymbolKind::Test) {
                    *old = symbol;
                }
            } else if symbols.len() < max_symbols {
                symbols.insert(key, symbol);
            }
        }
    }
    symbols.into_values().collect()
}

fn definition_extent(mut node: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
    if node.kind() == "function_declarator" {
        let original = node;
        while let Some(parent) = node.parent() {
            if parent.kind() == "function_definition" {
                return parent;
            }
            if matches!(
                parent.kind(),
                "translation_unit" | "declaration" | "field_declaration"
            ) {
                break;
            }
            node = parent;
        }
        original
    } else {
        node
    }
}

fn is_test(language: LanguageId, node: tree_sitter::Node<'_>, name: &str, source: &str) -> bool {
    match language {
        LanguageId::Python => name.starts_with("test_"),
        LanguageId::Go => name
            .strip_prefix("Test")
            .is_some_and(|tail| tail.starts_with(|c: char| c.is_uppercase())),
        LanguageId::Rust => {
            let mut previous = node.prev_named_sibling();
            while let Some(attribute) = previous {
                if attribute.kind() != "attribute_item" {
                    break;
                }
                if attribute
                    .utf8_text(source.as_bytes())
                    .ok()
                    .is_some_and(|text| {
                        let head = text
                            .trim_start_matches("#[")
                            .trim_start()
                            .split(['(', ']', ' '])
                            .next()
                            .unwrap_or("");
                        head == "test" || head.ends_with("::test")
                    })
                {
                    return true;
                }
                previous = attribute.prev_named_sibling();
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_come_from_syntax_nodes() {
        let source = "fn run() { let value = run(); // value\n let text = \"run\"; }";
        let ranges = identifier_ranges(source, "a.rs");
        let names: Vec<_> = ranges.iter().map(|range| &source[range.clone()]).collect();
        assert_eq!(names, ["run", "value", "run", "text"]);
        for (path, source) in [("a.rb", "LIMIT = 10"), ("a.php", "<?php const LIMIT = 10;")] {
            assert!(identifier_ranges(source, path).iter().any(|range| &source[range.clone()] == "LIMIT"), "{path}");
        }
    }

    #[test]
    fn javascript_typescript_and_go_constants_are_indexed() {
        for (path, source) in [("a.js", "const LIMIT = 10; let mutable = 2;"), ("a.ts", "const LIMIT: number = 10; let mutable = 2;"), ("a.go", "package p\nconst LIMIT = 10\nvar mutable = 2\n")] {
            let constants: Vec<_> = extract(source, path, 10).into_iter().filter(|symbol| symbol.kind == SymbolKind::Constant).map(|symbol| symbol.name).collect();
            assert_eq!(constants, ["LIMIT"], "{path}");
        }
    }

    #[test]
    fn c_cpp_and_csharp_constants_are_indexed() {
        for (path, source) in [("a.c", "const int LIMIT = 10; int mutable = 2;"), ("a.cpp", "const int LIMIT = 10; int mutable = 2;"), ("a.cs", "class C { const int LIMIT = 10; int mutable = 2; }")] {
            let constants: Vec<_> = extract(source, path, 10).into_iter().filter(|symbol| symbol.kind == SymbolKind::Constant).map(|symbol| symbol.name).collect();
            assert_eq!(constants, ["LIMIT"], "{path}");
        }
    }

    #[test]
    fn java_swift_ruby_and_php_constants_are_indexed() {
        for (path, source) in [("a.java", "class C { final int LIMIT = 10; int mutable = 2; }"), ("a.swift", "let LIMIT = 10\nvar mutable = 2"), ("a.rb", "LIMIT = 10\nmutable = 2"), ("a.php", "<?php const LIMIT = 10; $mutable = 2;")] {
            let constants: Vec<_> = extract(source, path, 10).into_iter().filter(|symbol| symbol.kind == SymbolKind::Constant).map(|symbol| symbol.name).collect();
            assert_eq!(constants, ["LIMIT"], "{path}");
        }
    }

    #[test]
    fn rust_constants_are_indexed() {
        let symbols = extract("const LIMIT: usize = 10;", "src/lib.rs", 10);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "LIMIT");
        assert_eq!(symbols[0].kind, SymbolKind::Constant);
    }

    #[test]
    fn skips_minified_source() {
        let source = format!("fn visible() {{}}{}", " ".repeat(20_001));
        assert!(extract(&source, "minified.rs", 100).is_empty());
        assert_eq!(extract("fn visible() {}\n", "normal.rs", 100).len(), 1);
    }

    #[test]
    fn linked_tags_queries_compile() {
        for language in [
            LanguageId::Rust,
            LanguageId::JavaScript,
            LanguageId::Jsx,
            LanguageId::TypeScript,
            LanguageId::Tsx,
            LanguageId::Python,
            LanguageId::Go,
            LanguageId::C,
            LanguageId::Cpp,
            LanguageId::CSharp,
            LanguageId::Java,
            LanguageId::Swift,
            LanguageId::Ruby,
            LanguageId::Php,
            LanguageId::Lua,
        ] {
            let (grammar, text) = tags(language).unwrap();
            tree_sitter::Query::new(&grammar, &text)
                .unwrap_or_else(|error| panic!("{language:?}: {error}"));
        }
    }

    #[test]
    fn extracts_definitions_without_calls_and_deduplicates_methods() {
        let symbols = extract(
            "struct Item;\nimpl Item { fn run(&self) { helper(); } }\nfn helper() {}\n",
            "a.rs",
            100,
        );
        assert_eq!(
            symbols
                .iter()
                .map(|s| (&*s.name, s.kind))
                .collect::<Vec<_>>(),
            vec![
                ("Item", SymbolKind::Type),
                ("run", SymbolKind::Method),
                ("helper", SymbolKind::Function)
            ]
        );
        assert_eq!(extract("fn f() {}", "a.rs", 0), vec![]);
        let c = extract("int add(int a) {\n  return a + 1;\n}\n", "a.c", 10);
        assert_eq!((c[0].start_line, c[0].end_line), (1, 3));
        let ts = extract("interface Item {}\nfunction run() {}\n", "a.ts", 10);
        assert_eq!(
            ts.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["Item", "run"]
        );
        let tests = extract(
            "#[test]\nfn checks() {}\n#[tokio::test]\nasync fn async_check() {}\n",
            "a.rs",
            10,
        );
        assert_eq!(
            tests.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![SymbolKind::Test, SymbolKind::Test]
        );
    }

    #[test]
    fn index_updates_only_changed_files_and_bounds_results() {
        let mut index = SymbolIndex::new(2, 3, 2);
        assert_eq!(
            index.apply(
                1,
                &[
                    FileChange::Changed {
                        path: "a.rs",
                        source: "fn a() {} fn b() {}"
                    },
                    FileChange::Changed {
                        path: "b.rs",
                        source: "fn c() {}"
                    }
                ]
            ),
            2
        );
        assert_eq!(index.symbol_count(), 3);
        assert_eq!(
            index.apply(
                1,
                &[FileChange::Changed {
                    path: "a.rs",
                    source: "fn changed() {}"
                }]
            ),
            0
        );
        assert_eq!(index.files()["a.rs"][0].name, "a");
        assert_eq!(
            index.apply(
                2,
                &[
                    FileChange::Removed { path: "b.rs" },
                    FileChange::Changed {
                        path: "a.rs",
                        source: "fn changed() {}"
                    }
                ]
            ),
            1
        );
        assert_eq!(index.symbol_count(), 1);
        assert_eq!(index.files()["a.rs"][0].name, "changed");
    }

    #[test]
    fn index_large_fixture_budget_and_idle_revision() {
        let source = (0..20)
            .map(|i| format!("fn function_{i}() {{}}\n"))
            .collect::<String>();
        let paths = (0..1000)
            .map(|i| format!("src/file_{i}.rs"))
            .collect::<Vec<_>>();
        let changes = paths
            .iter()
            .map(|path| FileChange::Changed {
                path,
                source: &source,
            })
            .collect::<Vec<_>>();
        let mut index = SymbolIndex::new(1000, 20_000, 20);
        let started = std::time::Instant::now();
        assert_eq!(index.apply(1, &changes), 1000);
        let elapsed = started.elapsed();
        let bytes = index.estimated_bytes();
        eprintln!(
            "symbol index: {} symbols, {bytes} estimated payload bytes, {elapsed:?}",
            index.symbol_count()
        );
        assert_eq!(index.symbol_count(), 20_000);
        assert!(bytes < 4 * 1024 * 1024);
        assert!(elapsed < std::time::Duration::from_secs(20));
        assert_eq!(index.apply(1, &changes), 0);
    }
}
