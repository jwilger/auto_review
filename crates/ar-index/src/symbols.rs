//! Tree-sitter-driven symbol extraction.
//!
//! Walks a parsed source file and emits one [`Symbol`] per top-level
//! definition (functions, structs, enums, traits, impl blocks, etc.).
//! Used by Milestone 2 RAG indexing as the first step before embedding
//! and storing in `LanceDB`.
//!
//! Currently Rust-only. Other languages (Python, TypeScript, Go) will
//! land as separate modules as their grammars get wired in.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator as _};

/// Tree-sitter query capturing Go definitions and their names.
const GO_QUERY_SOURCE: &str = "
(function_declaration name: (identifier) @name)              @function
(method_declaration   name: (field_identifier) @name)        @function
(type_declaration (type_spec name: (type_identifier) @name)) @struct
(const_declaration (const_spec name: (identifier) @name))    @constant
(var_declaration   (var_spec   name: (identifier) @name))    @static
";

/// Tree-sitter query capturing Python definitions and their names.
const PYTHON_QUERY_SOURCE: &str = "
(function_definition  name: (identifier) @name)         @function
(class_definition     name: (identifier) @name)         @struct
(decorated_definition (function_definition name: (identifier) @name)) @function
(decorated_definition (class_definition    name: (identifier) @name)) @struct
";

/// Tree-sitter query capturing Rust definitions and their names.
const RUST_QUERY_SOURCE: &str = "
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
";

/// Tree-sitter query capturing TypeScript/TSX definitions and their names.
const TS_QUERY_SOURCE: &str = "
(function_declaration  name: (identifier) @name)            @function
(class_declaration     name: (type_identifier) @name)       @struct
(interface_declaration name: (type_identifier) @name)       @trait
(enum_declaration      name: (identifier) @name)            @enum
(type_alias_declaration name: (type_identifier) @name)      @type_alias
(method_definition     name: (property_identifier) @name)   @function
";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Constant,
    Enum,
    Function,
    Impl,
    Macro,
    Module,
    Static,
    Struct,
    Trait,
    TypeAlias,
}

/// One extracted definition: its kind, name and line range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-review (rag_context tests) builds Symbol with a struct literal; \
              #[non_exhaustive] would break that construction"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the serialized key order, flattened into IndexedSymbol and \
              EmbeddedSymbol; reordering would change that serialization"
)]
pub struct Symbol {
    /// What kind of definition this is.
    pub kind: SymbolKind,
    /// The defined identifier as written in the source.
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

/// Extract top-level symbols from a Go source file. Method definitions
/// map to [`SymbolKind::Function`]; `type` declarations (whether
/// struct, interface, or alias) all map to [`SymbolKind::Struct`] for
/// cross-language uniformity. `const` decls are Constants, `var` decls
/// are Statics.
///
/// # Errors
///
/// Returns an [`ExtractError`] when the grammar, parse or query fails.
#[inline]
#[expect(
    clippy::module_name_repetitions,
    reason = "re-exported at the crate root as ar_index::extract_go_symbols, the name alongside \
              its sibling per-language extractors; renaming one would break that family"
)]
pub fn extract_go_symbols(source: &str) -> Result<Vec<Symbol>, ExtractError> {
    extract_with(source, &tree_sitter_go::LANGUAGE.into(), go_query()?)
}

/// Extract top-level symbols from a Python source file. `class` defs map
/// to [`SymbolKind::Struct`] for cross-language uniformity (the
/// downstream embedder doesn't care about Python-vs-Rust nomenclature).
///
/// # Errors
///
/// Returns an [`ExtractError`] when the grammar, parse or query fails.
#[inline]
#[expect(
    clippy::module_name_repetitions,
    reason = "re-exported at the crate root as ar_index::extract_python_symbols, the name \
              alongside its sibling per-language extractors; renaming one would break that family"
)]
pub fn extract_python_symbols(source: &str) -> Result<Vec<Symbol>, ExtractError> {
    extract_with(
        source,
        &tree_sitter_python::LANGUAGE.into(),
        python_query()?,
    )
}

/// Extract top-level symbols from a Rust source file.
///
/// # Errors
///
/// Returns an [`ExtractError`] when the grammar, parse or query fails.
#[inline]
#[expect(
    clippy::module_name_repetitions,
    reason = "re-exported at the crate root as ar_index::extract_rust_symbols, the name alongside \
              its sibling per-language extractors; renaming one would break that family"
)]
pub fn extract_rust_symbols(source: &str) -> Result<Vec<Symbol>, ExtractError> {
    extract_with(source, &tree_sitter_rust::LANGUAGE.into(), rust_query()?)
}

/// Pick the right extractor for a path's extension and run it.
/// Returns `Ok(vec![])` for paths in unsupported languages so a
/// repo-wide walk doesn't fail on the first JSON file it hits.
///
/// # Errors
///
/// Returns an [`ExtractError`] when the chosen extractor fails.
#[inline]
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the path is lowercased into `lower` first, so these suffix checks already match \
              extensions case-insensitively"
)]
pub fn extract_symbols_for_path(path: &str, source: &str) -> Result<Vec<Symbol>, ExtractError> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".rs") {
        extract_rust_symbols(source)
    } else if lower.ends_with(".py") {
        extract_python_symbols(source)
    } else if lower.ends_with(".go") {
        extract_go_symbols(source)
    } else if lower.ends_with(".tsx") {
        extract_tsx_symbols(source)
    } else if lower.ends_with(".ts")
        || lower.ends_with(".js")
        || lower.ends_with(".jsx")
        || lower.ends_with(".cjs")
        || lower.ends_with(".mjs")
    {
        // tree-sitter-typescript also parses JS sources (TS is a superset).
        extract_typescript_symbols(source)
    } else {
        Ok(Vec::new())
    }
}

/// Extract top-level symbols from a TSX (`.tsx`) source file. Same
/// semantics as [`extract_typescript_symbols`] but with the TSX
/// grammar so JSX expressions parse correctly.
///
/// # Errors
///
/// Returns an [`ExtractError`] when the grammar, parse or query fails.
#[inline]
#[expect(
    clippy::module_name_repetitions,
    reason = "re-exported at the crate root as ar_index::extract_tsx_symbols, the name alongside \
              its sibling per-language extractors; renaming one would break that family"
)]
pub fn extract_tsx_symbols(source: &str) -> Result<Vec<Symbol>, ExtractError> {
    extract_with(
        source,
        &tree_sitter_typescript::LANGUAGE_TSX.into(),
        tsx_query()?,
    )
}

/// Extract top-level symbols from a TypeScript (`.ts`) source file.
/// Interfaces map to [`SymbolKind::Trait`] for cross-language uniformity
/// (they're the closest analog to Rust's trait concept).
///
/// # Errors
///
/// Returns an [`ExtractError`] when the grammar, parse or query fails.
#[inline]
#[expect(
    clippy::module_name_repetitions,
    reason = "re-exported at the crate root as ar_index::extract_typescript_symbols, the name \
              alongside its sibling per-language extractors; renaming one would break that family"
)]
pub fn extract_typescript_symbols(source: &str) -> Result<Vec<Symbol>, ExtractError> {
    extract_with(
        source,
        &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ts_query()?,
    )
}

/// Parse `source` with `lang` and turn every `query` match that has both
/// a `@name` capture and a kind capture into a [`Symbol`].
#[expect(
    clippy::map_err_ignore,
    reason = "ExtractError::Language carries no payload and its Display text is fixed; the \
              grammar version mismatch detail is dropped to keep that error variant unchanged"
)]
#[expect(
    clippy::indexing_slicing,
    reason = "tree-sitter capture indices always index the capture_names of the query that \
              produced them"
)]
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "tree-sitter capture indices are u32 that widen losslessly to usize, and row numbers \
              of a parsed source file stay far below u32::MAX lines"
)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "a tree-sitter row is a 0-based line index of an in-memory source, so row + 1 cannot \
              overflow usize"
)]
fn extract_with(source: &str, lang: &Language, query: &Query) -> Result<Vec<Symbol>, ExtractError> {
    let mut parser = Parser::new();
    parser
        .set_language(lang)
        .map_err(|_| ExtractError::Language)?;
    let tree = parser.parse(source, None).ok_or(ExtractError::Parse)?;
    let root = tree.root_node();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, source.as_bytes());
    let capture_names = query.capture_names();

    let mut out = Vec::new();
    while let Some(query_match) = matches.next() {
        let mut kind: Option<SymbolKind> = None;
        let mut name: Option<String> = None;
        let mut node_for_range: Option<Node> = None;
        for cap in query_match.captures() {
            let cap_name = capture_names[cap.index as usize];
            if cap_name == "name" {
                name = Some(
                    cap.node
                        .utf8_text(source.as_bytes())
                        .unwrap_or_default()
                        .to_owned(),
                );
                continue;
            }
            if let Some(found) = kind_from_capture(cap_name) {
                kind = Some(found);
                node_for_range = Some(cap.node);
            }
        }
        if let (Some(found_kind), Some(found_name), Some(node)) = (kind, name, node_for_range) {
            out.push(Symbol {
                kind: found_kind,
                name: found_name,
                line_start: (node.start_position().row + 1) as u32,
                line_end: (node.end_position().row + 1) as u32,
            });
        }
    }
    Ok(out)
}

/// Lazily compiled [`GO_QUERY_SOURCE`], shared by every Go extraction.
fn go_query() -> Result<&'static Query, ExtractError> {
    static QUERY: OnceLock<Result<Query, String>> = OnceLock::new();
    let entry = QUERY.get_or_init(|| {
        let lang: Language = tree_sitter_go::LANGUAGE.into();
        Query::new(&lang, GO_QUERY_SOURCE).map_err(|error| error.to_string())
    });
    entry
        .as_ref()
        .map_err(|message| ExtractError::Query(message.clone()))
}

/// Symbol kind named by a query capture, or `None` for captures (such as
/// `@name`) that do not name a kind.
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

/// Lazily compiled [`PYTHON_QUERY_SOURCE`], shared by every Python extraction.
fn python_query() -> Result<&'static Query, ExtractError> {
    static QUERY: OnceLock<Result<Query, String>> = OnceLock::new();
    let entry = QUERY.get_or_init(|| {
        let lang: Language = tree_sitter_python::LANGUAGE.into();
        Query::new(&lang, PYTHON_QUERY_SOURCE).map_err(|error| error.to_string())
    });
    entry
        .as_ref()
        .map_err(|message| ExtractError::Query(message.clone()))
}

/// Lazily compiled [`RUST_QUERY_SOURCE`], shared by every Rust extraction.
fn rust_query() -> Result<&'static Query, ExtractError> {
    static QUERY: OnceLock<Result<Query, String>> = OnceLock::new();
    let entry = QUERY.get_or_init(|| {
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        Query::new(&lang, RUST_QUERY_SOURCE).map_err(|error| error.to_string())
    });
    entry
        .as_ref()
        .map_err(|message| ExtractError::Query(message.clone()))
}

/// [`TS_QUERY_SOURCE`] compiled for the TypeScript grammar.
fn ts_query() -> Result<&'static Query, ExtractError> {
    static QUERY: OnceLock<Result<Query, String>> = OnceLock::new();
    let entry = QUERY.get_or_init(|| {
        let lang: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        Query::new(&lang, TS_QUERY_SOURCE).map_err(|error| error.to_string())
    });
    entry
        .as_ref()
        .map_err(|message| ExtractError::Query(message.clone()))
}

/// [`TS_QUERY_SOURCE`] compiled for the TSX grammar.
fn tsx_query() -> Result<&'static Query, ExtractError> {
    static QUERY: OnceLock<Result<Query, String>> = OnceLock::new();
    let entry = QUERY.get_or_init(|| {
        let lang: Language = tree_sitter_typescript::LANGUAGE_TSX.into();
        Query::new(&lang, TS_QUERY_SOURCE).map_err(|error| error.to_string())
    });
    entry
        .as_ref()
        .map_err(|message| ExtractError::Query(message.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(symbols: &[Symbol]) -> Vec<&str> {
        symbols.iter().map(|symbol| symbol.name.as_str()).collect()
    }

    fn kinds(symbols: &[Symbol], name: &str) -> Vec<SymbolKind> {
        symbols
            .iter()
            .filter(|symbol| symbol.name == name)
            .map(|symbol| symbol.kind)
            .collect()
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
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
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
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
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
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
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
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
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
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
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn empty_source_yields_zero_symbols() {
        let s = extract_rust_symbols("").expect("ok");
        assert!(s.is_empty());
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn extracts_python_functions_and_classes() {
        let src = "\
def add(a, b):
    return a + b

class User:
    def __init__(self, name):
        self.name = name

@dataclass
class Config:
    debug: bool = False
";
        let s = extract_python_symbols(src).expect("ok");
        let n = names(&s);
        assert!(n.contains(&"add"));
        assert!(n.contains(&"User"));
        assert!(n.contains(&"Config"));
        assert!(n.contains(&"__init__"));
        assert!(kinds(&s, "add").contains(&SymbolKind::Function));
        assert!(kinds(&s, "User").contains(&SymbolKind::Struct));
        // Decorated class should also map to Struct kind.
        assert!(kinds(&s, "Config").contains(&SymbolKind::Struct));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn python_decorated_functions_count() {
        let src = "\
@property
def value(self):
    return self._value

@staticmethod
def helper():
    pass
";
        let s = extract_python_symbols(src).expect("ok");
        let n = names(&s);
        assert!(n.contains(&"value"));
        assert!(n.contains(&"helper"));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn extracts_typescript_function_class_interface_enum() {
        let src = "\
export function add(a: number, b: number): number {
    return a + b;
}

export class User {
    constructor(public name: string) {}
}

export interface Greeter {
    hello(): string;
}

export enum Direction {
    Up,
    Down,
}

export type UserId = string;
";
        let s = extract_typescript_symbols(src).expect("ok");
        let n = names(&s);
        assert!(n.contains(&"add"));
        assert!(n.contains(&"User"));
        assert!(n.contains(&"Greeter"));
        assert!(n.contains(&"Direction"));
        assert!(n.contains(&"UserId"));
        assert!(kinds(&s, "add").contains(&SymbolKind::Function));
        assert!(kinds(&s, "User").contains(&SymbolKind::Struct));
        assert!(kinds(&s, "Greeter").contains(&SymbolKind::Trait));
        assert!(kinds(&s, "Direction").contains(&SymbolKind::Enum));
        assert!(kinds(&s, "UserId").contains(&SymbolKind::TypeAlias));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn extracts_tsx_with_jsx_expressions() {
        let src = "\
import * as React from 'react';

export function Greeting({ name }: { name: string }) {
    return <div>Hello, {name}!</div>;
}

export class App extends React.Component {
    render() {
        return <Greeting name=\"world\" />;
    }
}
";
        let s = extract_tsx_symbols(src).expect("ok");
        let n = names(&s);
        assert!(n.contains(&"Greeting"));
        assert!(n.contains(&"App"));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn extracts_go_functions_methods_types() {
        let src = "\
package main

func Add(a int, b int) int {
    return a + b
}

type User struct {
    Name string
}

func (u *User) Greet() string {
    return \"hi\"
}

const Pi = 3.14

var DefaultName = \"world\"
";
        let s = extract_go_symbols(src).expect("ok");
        let n = names(&s);
        assert!(n.contains(&"Add"));
        assert!(n.contains(&"User"));
        assert!(n.contains(&"Greet"));
        assert!(n.contains(&"Pi"));
        assert!(n.contains(&"DefaultName"));
        assert!(kinds(&s, "Add").contains(&SymbolKind::Function));
        assert!(kinds(&s, "User").contains(&SymbolKind::Struct));
        assert!(kinds(&s, "Greet").contains(&SymbolKind::Function));
        assert!(kinds(&s, "Pi").contains(&SymbolKind::Constant));
        assert!(kinds(&s, "DefaultName").contains(&SymbolKind::Static));
    }

    #[test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn go_empty_source_yields_zero_symbols() {
        let s = extract_go_symbols("").expect("ok");
        assert!(s.is_empty());
    }

    #[test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    fn extract_symbols_for_path_dispatches_by_extension() {
        let rust = "fn x() {}";
        let py = "def x(): pass";
        let ts = "function x() {}";
        let tsx = "const X = () => <div/>;";
        let unknown = "anything goes";

        assert!(!extract_symbols_for_path("src/main.rs", rust)
            .unwrap()
            .is_empty());
        assert!(!extract_symbols_for_path("foo.py", py).unwrap().is_empty());
        assert!(!extract_symbols_for_path("a.ts", ts).unwrap().is_empty());
        // .tsx parses JSX
        let _: Vec<Symbol> = extract_symbols_for_path("a.tsx", tsx).unwrap();
        // .js/.jsx/.mjs/.cjs route to TypeScript grammar (TS is a JS superset)
        let _: Vec<Symbol> = extract_symbols_for_path("a.js", ts).unwrap();
        let _: Vec<Symbol> = extract_symbols_for_path("a.jsx", ts).unwrap();
        let _: Vec<Symbol> = extract_symbols_for_path("a.mjs", ts).unwrap();
        let _: Vec<Symbol> = extract_symbols_for_path("a.cjs", ts).unwrap();
        // Unknown extension → empty, not error
        assert!(extract_symbols_for_path("data.json", unknown)
            .unwrap()
            .is_empty());
        assert!(extract_symbols_for_path("README", "").unwrap().is_empty());
    }

    #[test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    fn extract_symbols_for_path_is_case_insensitive_on_extension() {
        let rust = "fn x() {}";
        assert!(!extract_symbols_for_path("Foo.RS", rust).unwrap().is_empty());
    }

    #[test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn typescript_empty_source_yields_zero_symbols() {
        let s = extract_typescript_symbols("").expect("ok");
        assert!(s.is_empty());
    }

    #[test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn python_empty_source_yields_zero_symbols() {
        let s = extract_python_symbols("").expect("ok");
        assert!(s.is_empty());
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn malformed_source_does_not_panic() {
        // tree-sitter is error-tolerant — it parses what it can.
        let s = extract_rust_symbols("fn broken( {").expect("ok");
        // Whatever it returns, the call must not panic. We accept any
        // (potentially empty) result here.
        let _: Vec<Symbol> = s;
    }
}
