//! Definition captures from the tags queries shipped with linked grammars.

use std::collections::BTreeMap;
use tree_sitter::StreamingIterator;

use crate::{DEFAULT_MAX_SOURCE_BYTES, LanguageId, language_for_path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// One-based, inclusive source lines.
    pub start_line: u32,
    pub end_line: u32,
}

/// Returns the linked grammar and its tags query, if the grammar exposes one.
fn tags(language: LanguageId) -> Option<(tree_sitter::Language, String)> {
    use LanguageId::*;
    let (grammar, query) = match language {
        Rust => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::TAGS_QUERY.to_owned(),
        ),
        JavaScript | Jsx => (
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::TAGS_QUERY.to_owned(),
        ),
        TypeScript | Tsx => (
            if language == Tsx {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            } else {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
            },
            format!(
                "{}\n{}",
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
            tree_sitter_go::TAGS_QUERY.to_owned(),
        ),
        C => (
            tree_sitter_c::LANGUAGE.into(),
            tree_sitter_c::TAGS_QUERY.to_owned(),
        ),
        Cpp => (
            tree_sitter_cpp::LANGUAGE.into(),
            tree_sitter_cpp::TAGS_QUERY.to_owned(),
        ),
        CSharp => (
            tree_sitter_c_sharp::LANGUAGE.into(),
            tree_sitter_c_sharp::TAGS_QUERY.to_owned(),
        ),
        Java => (
            tree_sitter_java::LANGUAGE.into(),
            tree_sitter_java::TAGS_QUERY.to_owned(),
        ),
        Swift => (
            tree_sitter_swift::LANGUAGE.into(),
            tree_sitter_swift::TAGS_QUERY.to_owned(),
        ),
        Ruby => (
            tree_sitter_ruby::LANGUAGE.into(),
            tree_sitter_ruby::TAGS_QUERY.to_owned(),
        ),
        Php => (
            tree_sitter_php::LANGUAGE_PHP.into(),
            tree_sitter_php::TAGS_QUERY.to_owned(),
        ),
        Lua => (
            tree_sitter_lua::LANGUAGE.into(),
            tree_sitter_lua::TAGS_QUERY.to_owned(),
        ),
        _ => return None,
    };
    Some((grammar, query))
}

/// Extract definitions from one file. The caller owns scheduling and caching.
pub fn extract(source: &str, path: &str, max_symbols: usize) -> Vec<Symbol> {
    if source.len() > DEFAULT_MAX_SOURCE_BYTES || max_symbols == 0 {
        return Vec::new();
    }
    let Some((language, text)) = language_for_path(path).and_then(tags) else {
        return Vec::new();
    };
    let Ok(query) = tree_sitter::Query::new(&language, &text) else {
        return Vec::new();
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let names = query.capture_names();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
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
        if let (Some(name), Some((node, kind))) = (name, definition) {
            let Ok(value) = name.utf8_text(source.as_bytes()) else {
                continue;
            };
            let symbol = Symbol {
                name: value.to_owned(),
                kind,
                start_line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
            };
            let key = (name.start_byte(), name.end_byte());
            symbols
                .entry(key)
                .and_modify(|old: &mut Symbol| {
                    if kind == SymbolKind::Method {
                        *old = symbol.clone();
                    }
                })
                .or_insert(symbol);
            if symbols.len() >= max_symbols {
                break;
            }
        }
    }
    symbols.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
