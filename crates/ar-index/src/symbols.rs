//! Tree-sitter-driven symbol extraction.
//!
//! Walks a parsed source file and emits one [`Symbol`] per top-level
//! definition (functions, structs, enums, traits, impl blocks, etc.).
//! Used by Milestone 2 RAG indexing as the first step before embedding
//! and storing in LanceDB.
//!
//! Currently Rust-only. Other languages (Python, TypeScript, Go) will
//! land as separate modules as their grammars get wired in.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tree_sitter::{Node, Parser, Query, QueryCursor};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Module,
    TypeAlias,
    Constant,
    Static,
    Macro,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub name: String,
    /// 1-based start line in the source file.
    pub line_start: u32,
    /// 1-based inclusive end line.
    pub line_end: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("tree-sitter language load failed")]
    Language,
    #[error("tree-sitter parse failed (no root)")]
    Parse,
    #[error("tree-sitter query failed: {0}")]
    Query(String),
}

const RUST_QUERY_SOURCE: &str = r#"
(function_item       name: (identifier) @name)             @function
(struct_item         name: (type_identifier) @name)        @struct
(enum_item           name: (type_identifier) @name)        @enum
(trait_item          name: (type_identifier) @name)        @trait
(impl_item           type: (type_identifier) @name)        @impl
(mod_item            name: (identifier) @name)             @module
(type_item           name: (type_identifier) @name)        @type_alias
(const_item          name: (identifier) @name)             @constant
(static_item         name: (identifier) @name)             @static
(macro_definition    name: (identifier) @name)             @macro
"#;

fn rust_query() -> Result<&'static Query, ExtractError> {
    static QUERY: OnceLock<Result<Query, String>> = OnceLock::new();
    let entry = QUERY.get_or_init(|| {
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        Query::new(&lang, RUST_QUERY_SOURCE).map_err(|e| e.to_string())
    });
    entry.as_ref().map_err(|e| ExtractError::Query(e.clone()))
}

/// Extract top-level symbols from a Rust source file.
pub fn extract_rust_symbols(source: &str) -> Result<Vec<Symbol>, ExtractError> {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&lang)
        .map_err(|_| ExtractError::Language)?;
    let tree = parser.parse(source, None).ok_or(ExtractError::Parse)?;
    let root = tree.root_node();

    let query = rust_query()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, source.as_bytes());

    let capture_names = query.capture_names();

    let mut out = Vec::new();
    // QueryMatches::next() is an inherent method (streaming-iterator
    // semantics — items borrow from the cursor); a `for` loop would
    // require std::iter::Iterator, which it deliberately doesn't
    // implement. Silence clippy's tempting suggestion accordingly.
    #[allow(clippy::while_let_on_iterator)]
    while let Some(m) = matches.next() {
        let mut kind: Option<SymbolKind> = None;
        let mut name: Option<String> = None;
        let mut node_for_range: Option<Node> = None;
        for cap in m.captures {
            let cap_name = capture_names[cap.index as usize];
            if cap_name == "name" {
                name = Some(
                    cap.node
                        .utf8_text(source.as_bytes())
                        .unwrap_or_default()
                        .to_string(),
                );
            } else if let Some(k) = kind_from_capture(cap_name) {
                kind = Some(k);
                node_for_range = Some(cap.node);
            }
        }
        if let (Some(kind), Some(name), Some(node)) = (kind, name, node_for_range) {
            out.push(Symbol {
                kind,
                name,
                line_start: (node.start_position().row + 1) as u32,
                line_end: (node.end_position().row + 1) as u32,
            });
        }
    }

    Ok(out)
}

fn kind_from_capture(cap_name: &str) -> Option<SymbolKind> {
    Some(match cap_name {
        "function" => SymbolKind::Function,
        "struct" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "trait" => SymbolKind::Trait,
        "impl" => SymbolKind::Impl,
        "module" => SymbolKind::Module,
        "type_alias" => SymbolKind::TypeAlias,
        "constant" => SymbolKind::Constant,
        "static" => SymbolKind::Static,
        "macro" => SymbolKind::Macro,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(symbols: &[Symbol]) -> Vec<&str> {
        symbols.iter().map(|s| s.name.as_str()).collect()
    }

    fn kinds(symbols: &[Symbol], name: &str) -> Vec<SymbolKind> {
        symbols
            .iter()
            .filter(|s| s.name == name)
            .map(|s| s.kind)
            .collect()
    }

    #[test]
    fn extracts_function_struct_and_enum() {
        let src = "\
pub fn add(a: i32, b: i32) -> i32 { a + b }

pub struct User {
    pub name: String,
}

pub enum Direction {
    Up,
    Down,
}
";
        let s = extract_rust_symbols(src).expect("ok");
        let n = names(&s);
        assert!(n.contains(&"add"));
        assert!(n.contains(&"User"));
        assert!(n.contains(&"Direction"));
        assert!(kinds(&s, "add").contains(&SymbolKind::Function));
        assert!(kinds(&s, "User").contains(&SymbolKind::Struct));
        assert!(kinds(&s, "Direction").contains(&SymbolKind::Enum));
    }

    #[test]
    fn extracts_traits_and_impls() {
        let src = "\
pub trait Greet { fn hello(&self) -> String; }

pub struct Greeter;

impl Greet for Greeter {
    fn hello(&self) -> String { String::from(\"hi\") }
}
";
        let s = extract_rust_symbols(src).expect("ok");
        let n = names(&s);
        assert!(n.contains(&"Greet"));
        assert!(n.contains(&"Greeter"));
        assert!(kinds(&s, "Greet").contains(&SymbolKind::Trait));
    }

    #[test]
    fn line_ranges_are_one_based_and_inclusive() {
        let src = "\
fn one() {}
fn two() {
    let _ = 1;
}
";
        let s = extract_rust_symbols(src).expect("ok");
        let one = s.iter().find(|x| x.name == "one").expect("one");
        assert_eq!(one.line_start, 1);
        assert_eq!(one.line_end, 1);
        let two = s.iter().find(|x| x.name == "two").expect("two");
        assert_eq!(two.line_start, 2);
        assert_eq!(two.line_end, 4);
    }

    #[test]
    fn extracts_constants_and_statics() {
        let src = "\
pub const MAX: u32 = 100;
pub static GREETING: &str = \"hi\";
";
        let s = extract_rust_symbols(src).expect("ok");
        assert!(kinds(&s, "MAX").contains(&SymbolKind::Constant));
        assert!(kinds(&s, "GREETING").contains(&SymbolKind::Static));
    }

    #[test]
    fn extracts_modules_and_type_aliases() {
        let src = "\
pub mod nested {
    pub fn inside() {}
}
pub type UserId = u64;
";
        let s = extract_rust_symbols(src).expect("ok");
        let n = names(&s);
        assert!(n.contains(&"nested"));
        assert!(n.contains(&"UserId"));
        assert!(kinds(&s, "nested").contains(&SymbolKind::Module));
        assert!(kinds(&s, "UserId").contains(&SymbolKind::TypeAlias));
    }

    #[test]
    fn empty_source_yields_zero_symbols() {
        let s = extract_rust_symbols("").expect("ok");
        assert!(s.is_empty());
    }

    #[test]
    fn malformed_source_does_not_panic() {
        // tree-sitter is error-tolerant — it parses what it can.
        let s = extract_rust_symbols("fn broken( {").expect("ok");
        // Whatever it returns, the call must not panic. We accept any
        // (potentially empty) result here.
        let _ = s;
    }
}
