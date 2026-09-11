use tree_sitter::Language;
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_zig() -> *const ();
    fn tree_sitter_javascript() -> *const ();
    fn tree_sitter_typescript() -> *const ();
    fn tree_sitter_tsx() -> *const ();
    fn tree_sitter_rust() -> *const ();
    fn tree_sitter_c() -> *const ();
    fn tree_sitter_cpp() -> *const ();
    fn tree_sitter_python() -> *const ();
    fn tree_sitter_gn() -> *const ();
}

#[derive(Clone, Copy, Debug)]
pub struct Grammar {
    pub name: &'static str,
    pub query: &'static str,
    language_fn: unsafe extern "C" fn() -> *const (),
}

impl Grammar {
    pub fn language(self) -> Language {
        // SAFETY: Every entry points to a Tree-sitter-generated, statically
        // linked grammar function compiled by build.rs.
        Language::new(unsafe { LanguageFn::from_raw(self.language_fn) })
    }
}

const JAVASCRIPT_QUERY: &str = concat!(
    include_str!("syntax_queries/javascript_highlights.scm"),
    "\n",
    include_str!("syntax_queries/javascript_highlights_params.scm")
);
const TSX_QUERY: &str = concat!(
    include_str!("syntax_queries/typescript_highlights.scm"),
    "\n",
    include_str!("syntax_queries/javascript_highlights_jsx.scm")
);
const CPP_QUERY: &str = concat!(
    include_str!("syntax_queries/c_highlights.scm"),
    "\n",
    include_str!("syntax_queries/cpp_highlights.scm")
);

pub fn find(language: &str) -> Option<Grammar> {
    let (name, query, language_fn): (_, _, unsafe extern "C" fn() -> *const ()) = match language {
        "zig" => (
            "zig",
            include_str!("syntax_queries/zig_highlights.scm"),
            tree_sitter_zig,
        ),
        "javascript" => ("javascript", JAVASCRIPT_QUERY, tree_sitter_javascript),
        "typescript" => (
            "typescript",
            include_str!("syntax_queries/typescript_highlights.scm"),
            tree_sitter_typescript,
        ),
        "tsx" => ("tsx", TSX_QUERY, tree_sitter_tsx),
        "rust" => (
            "rust",
            include_str!("syntax_queries/rust_highlights.scm"),
            tree_sitter_rust,
        ),
        "c" => (
            "c",
            include_str!("syntax_queries/c_highlights.scm"),
            tree_sitter_c,
        ),
        "cpp" => ("cpp", CPP_QUERY, tree_sitter_cpp),
        "python" => (
            "python",
            include_str!("syntax_queries/python_highlights.scm"),
            tree_sitter_python,
        ),
        "gn" => (
            "gn",
            include_str!("syntax_queries/gn_highlights.scm"),
            tree_sitter_gn,
        ),
        _ => return None,
    };
    Some(Grammar {
        name,
        query,
        language_fn,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_bundled_grammars_and_parses_empty_sources() {
        for name in [
            "zig",
            "javascript",
            "typescript",
            "tsx",
            "rust",
            "c",
            "cpp",
            "python",
            "gn",
        ] {
            let grammar = find(name).unwrap();
            assert_eq!(grammar.name, name);
            assert!(!grammar.query.is_empty());
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&grammar.language()).unwrap();
            assert!(parser.parse("", None).is_some());
        }
        assert!(find("markdown").is_none());
    }
}
