use tree_sitter::{
    Language, Node, Parser, Query, QueryCursor, QueryMatch, QueryPredicateArg, StreamingIterator,
};

use crate::{
    Error, Result,
    syntax::{HighlightSpan, SyntaxToken},
};

pub const MAX_FILE_SIZE: usize = 512 * 1024;

#[derive(Clone, Debug)]
pub struct SideHighlight {
    pub source: Vec<u8>,
    pub line_starts: Vec<usize>,
    pub spans_by_line: Vec<Vec<HighlightSpan>>,
}

impl SideHighlight {
    pub fn line_text(&self, line_number: u32) -> Option<&[u8]> {
        let index = line_number.checked_sub(1)? as usize;
        let start = *self.line_starts.get(index)?;
        Some(&self.source[start..line_end(&self.source, &self.line_starts, index)])
    }

    pub fn line_spans(&self, line_number: u32) -> &[HighlightSpan] {
        line_number
            .checked_sub(1)
            .and_then(|index| self.spans_by_line.get(index as usize))
            .map_or(&[], Vec::as_slice)
    }
}

pub fn build(language: Language, query_source: &str, source: Vec<u8>) -> Result<SideHighlight> {
    if source.len() > MAX_FILE_SIZE {
        return Err(Error::SourceTooLarge);
    }
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|_| Error::SyntaxUnavailable)?;
    let tree = parser
        .parse(&source, None)
        .ok_or(Error::SyntaxUnavailable)?;
    let query = Query::new(&language, query_source).map_err(|err| {
        eprintln!("tree-sitter query compile failed: {err}");
        Error::SyntaxUnavailable
    })?;
    let line_starts = build_line_starts(&source);
    let mut spans_by_line = vec![Vec::new(); line_starts.len()];
    let mut cursor = QueryCursor::new();
    {
        // The official bindings evaluate eq?, any-of?, match? and their
        // negations against source bytes. Lua predicates remain application-defined.
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_slice());
        while let Some(found) = matches.next() {
            if !general_predicates_pass(&query, found, &source) {
                continue;
            }
            for capture in found.captures {
                if let Some(token) =
                    token_for_capture(query.capture_names()[capture.index as usize])
                {
                    append_capture_span(
                        &mut spans_by_line,
                        &line_starts,
                        &source,
                        capture.node,
                        token,
                    );
                }
            }
        }
    }
    if cursor.did_exceed_match_limit() {
        return Err(Error::SyntaxUnavailable);
    }
    for spans in &mut spans_by_line {
        // Stable sorting retains query order when two captures cover identical bytes.
        spans.sort_by_key(|span| (span.start_byte, span.end_byte));
    }
    Ok(SideHighlight {
        source,
        line_starts,
        spans_by_line,
    })
}

fn build_line_starts(source: &[u8]) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, &byte) in source.iter().enumerate() {
        if byte == b'\n' && index + 1 < source.len() {
            starts.push(index + 1);
        }
    }
    starts
}

fn line_end(source: &[u8], starts: &[usize], index: usize) -> usize {
    let mut end = starts.get(index + 1).copied().unwrap_or(source.len());
    if end > starts[index] && source[end - 1] == b'\n' {
        end -= 1;
    }
    while end > starts[index] && source[end - 1] == b'\r' {
        end -= 1;
    }
    end
}

fn append_capture_span(
    lines: &mut [Vec<HighlightSpan>],
    starts: &[usize],
    source: &[u8],
    node: Node<'_>,
    token: SyntaxToken,
) {
    let first_row = node.start_position().row;
    let last_row = node.end_position().row.min(lines.len() - 1);
    if node.end_byte() <= node.start_byte() || first_row >= lines.len() {
        return;
    }
    for row in first_row..=last_row {
        let start = node.start_byte().max(starts[row]);
        let end = node.end_byte().min(line_end(source, starts, row));
        if end > start {
            lines[row].push(HighlightSpan {
                start_byte: start - starts[row],
                end_byte: end - starts[row],
                token,
            });
        }
    }
}

fn token_for_capture(name: &str) -> Option<SyntaxToken> {
    Some(if name.starts_with("keyword") {
        SyntaxToken::Keyword
    } else if name.starts_with("string") || name == "character" {
        SyntaxToken::String
    } else if name.starts_with("comment") {
        SyntaxToken::Comment
    } else if name.starts_with("type") || name == "constructor" {
        SyntaxToken::Type
    } else if name.starts_with("function") || name.starts_with("method") {
        SyntaxToken::Function
    } else if name.starts_with("number")
        || name.starts_with("constant.numeric")
        || name == "float"
        || name == "integer"
    {
        SyntaxToken::Number
    } else if name.starts_with("constant") || name == "boolean" || name == "null" {
        SyntaxToken::Keyword
    } else if name.starts_with("operator") || name.starts_with("punctuation") {
        SyntaxToken::Operator
    } else if name.starts_with("property") || name.starts_with("attribute") || name == "tag" {
        SyntaxToken::Function
    } else if name.starts_with("escape") {
        SyntaxToken::String
    } else {
        return None;
    })
}

fn general_predicates_pass(query: &Query, found: &QueryMatch<'_, '_>, source: &[u8]) -> bool {
    query
        .general_predicates(found.pattern_index)
        .iter()
        .all(|predicate| {
            // As before, property directives (set!/is?/is-not?) and unknown
            // directives do not filter captures; no local-scope model is maintained.
            if predicate.operator.as_ref() != "lua-match?" {
                return true;
            }
            let [
                QueryPredicateArg::Capture(id),
                QueryPredicateArg::String(pattern),
            ] = predicate.args.as_ref()
            else {
                return false;
            };
            found
                .captures
                .iter()
                .find(|capture| capture.index == *id)
                .and_then(|capture| source.get(capture.node.byte_range()))
                .is_some_and(|value| match_supported_pattern(value, pattern))
        })
}

fn match_supported_pattern(value: &[u8], pattern: &str) -> bool {
    // Preserve the original conservative Lua-pattern subset, rather than
    // silently interpreting Lua syntax as regular expressions.
    match pattern {
        "^[A-Z]" => value.first().is_some_and(u8::is_ascii_uppercase),
        "^[A-Z_][a-zA-Z0-9_]*" => {
            value
                .first()
                .is_some_and(|byte| byte.is_ascii_uppercase() || *byte == b'_')
                && value[1..]
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        }
        "^[A-Z][A-Z_0-9]+$" => {
            value.len() >= 2
                && value[0].is_ascii_uppercase()
                && value[1..]
                    .iter()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
        }
        "^[A-Z][A-Z\\d_]+$" | "^[A-Z][A-Z\\d_]*$" | "^[A-Z][A-Z_]*$" => {
            value.first().is_some_and(u8::is_ascii_uppercase)
                && value[1..]
                    .iter()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
        }
        "^[a-z][^.]*$" => {
            value.first().is_some_and(u8::is_ascii_lowercase) && !value.contains(&b'.')
        }
        _ => {
            if let Some(alternatives) = pattern
                .strip_prefix("^(")
                .and_then(|p| p.strip_suffix(")$"))
            {
                alternatives
                    .split('|')
                    .any(|alternative| value == alternative.as_bytes())
            } else if let Some(prefix) = pattern.strip_prefix('^') {
                value.starts_with(prefix.as_bytes())
            } else {
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax_grammars;

    fn highlight(name: &str, source: &str) -> SideHighlight {
        let grammar = syntax_grammars::find(name).unwrap();
        build(
            grammar.language(),
            grammar.query,
            source.as_bytes().to_vec(),
        )
        .unwrap_or_else(|err| panic!("failed to highlight {name}: {err}"))
    }

    fn has_token(highlight: &SideHighlight, line: u32, token: SyntaxToken, text: &str) -> bool {
        let source = highlight.line_text(line).unwrap();
        highlight.line_spans(line).iter().any(|span| {
            span.token == token
                && source.get(span.start_byte..span.end_byte) == Some(text.as_bytes())
        })
    }

    #[test]
    fn capture_mapping() {
        for (name, token) in [
            ("keyword.return", SyntaxToken::Keyword),
            ("function.call", SyntaxToken::Function),
            ("comment.documentation", SyntaxToken::Comment),
            ("constant.numeric", SyntaxToken::Number),
            ("punctuation.bracket", SyntaxToken::Operator),
            ("constructor", SyntaxToken::Type),
            ("escape", SyntaxToken::String),
            ("boolean", SyntaxToken::Keyword),
        ] {
            assert_eq!(token_for_capture(name), Some(token));
        }
        assert_eq!(token_for_capture("variable"), None);
    }

    #[test]
    fn all_nine_bundled_queries_produce_real_token_spans() {
        for (language, source, token, text) in [
            ("zig", "const value = 1;\n", SyntaxToken::Keyword, "const"),
            (
                "javascript",
                "const value = 1;\n",
                SyntaxToken::Keyword,
                "const",
            ),
            (
                "typescript",
                "const value: number = 1;\n",
                SyntaxToken::Keyword,
                "const",
            ),
            (
                "tsx",
                "const view = <div>{value}</div>;\n",
                SyntaxToken::Function,
                "div",
            ),
            (
                "rust",
                "fn main() { let value: usize = 1; }\n",
                SyntaxToken::Keyword,
                "fn",
            ),
            (
                "c",
                "int main(void) { return 0; }\n",
                SyntaxToken::Keyword,
                "return",
            ),
            (
                "cpp",
                "class App { public: int run(); };\n",
                SyntaxToken::Keyword,
                "class",
            ),
            (
                "python",
                "def run() -> bool:\n    return True\n",
                SyntaxToken::Keyword,
                "def",
            ),
            (
                "gn",
                "executable(\"foo\") {\n  sources = [ \"foo.cc\" ]\n}\n",
                SyntaxToken::Function,
                "executable",
            ),
        ] {
            let highlighted = highlight(language, source);
            assert!(
                has_token(&highlighted, 1, token, text),
                "missing {text:?} in {language}: {:?}",
                highlighted.line_spans(1)
            );
        }
    }

    #[test]
    fn typescript_keeps_ordinary_keywords() {
        let h = highlight(
            "typescript",
            "const value: number = 1;\nfunction run() { return value; }\n",
        );
        assert!(has_token(&h, 1, SyntaxToken::Keyword, "const"));
        assert!(has_token(&h, 2, SyntaxToken::Keyword, "function"));
        assert!(has_token(&h, 2, SyntaxToken::Keyword, "return"));
    }

    #[test]
    fn conservative_lua_patterns() {
        for (value, pattern, expected) in [
            ("MyType", "^[A-Z_][a-zA-Z0-9_]*", true),
            ("myType", "^[A-Z_][a-zA-Z0-9_]*", false),
            ("//! doc", "^//!", true),
            ("// doc", "^//!", false),
            ("MyType", "^[A-Z]", true),
            ("MAX_VALUE", "^[A-Z][A-Z\\d_]+$", true),
            (
                "console",
                "^(arguments|module|console|window|document)$",
                true,
            ),
            (
                "unknown",
                "^(arguments|module|console|window|document)$",
                false,
            ),
            ("abc", "%w+", false),
            ("", "^[A-Z_][a-zA-Z0-9_]*", false),
        ] {
            assert_eq!(
                match_supported_pattern(value.as_bytes(), pattern),
                expected,
                "{value:?}: {pattern}"
            );
        }
    }

    #[test]
    fn lua_predicates_filter_actual_captures() {
        let grammar = syntax_grammars::find("zig").unwrap();
        let h = build(
            grammar.language(),
            "((identifier) @type (#lua-match? @type \"^[A-Z_][a-zA-Z0-9_]*\"))",
            b"const MyType = lower;\n".to_vec(),
        )
        .unwrap();
        assert!(has_token(&h, 1, SyntaxToken::Type, "MyType"));
        assert!(!has_token(&h, 1, SyntaxToken::Type, "lower"));
        let malformed = build(
            grammar.language(),
            "((identifier) @type (#lua-match? @type))",
            b"const MyType = lower;".to_vec(),
        )
        .unwrap();
        assert!(malformed.line_spans(1).is_empty());
    }

    #[test]
    fn builtin_predicates_filter_matches_and_directives_do_not() {
        let grammar = syntax_grammars::find("javascript").unwrap();
        for (predicate, expected) in [
            ("#eq? @type \"UPPER\"", vec!["UPPER"]),
            ("#not-eq? @type \"UPPER\"", vec!["lower", "Other"]),
            ("#any-of? @type \"UPPER\" \"Other\"", vec!["UPPER", "Other"]),
            ("#match? @type \"^[A-Z]+$\"", vec!["UPPER"]),
            ("#not-match? @type \"^[A-Z]\"", vec!["lower"]),
            ("#set! priority 95", vec!["UPPER", "lower", "Other"]),
            ("#is-not? local", vec!["UPPER", "lower", "Other"]),
            ("#future-directive! @type", vec!["UPPER", "lower", "Other"]),
        ] {
            let query = format!("((identifier) @type ({predicate}))");
            let h = build(grammar.language(), &query, b"UPPER; lower; Other;".to_vec()).unwrap();
            let captured: Vec<_> = h
                .line_spans(1)
                .iter()
                .map(|s| std::str::from_utf8(&h.source[s.start_byte..s.end_byte]).unwrap())
                .collect();
            assert_eq!(captured, expected, "{predicate}");
        }
    }

    #[test]
    fn multiline_utf8_and_crlf_spans_stay_within_line_bytes() {
        let h = highlight("rust", "/* 你好\r\n世界 */\r\nlet text = \"🦀\";\n");
        assert_eq!(h.line_text(1), Some("/* 你好".as_bytes()));
        assert_eq!(h.line_text(2), Some("世界 */".as_bytes()));
        assert_eq!(h.line_text(3), Some("let text = \"🦀\";".as_bytes()));
        assert!(has_token(&h, 1, SyntaxToken::Comment, "/* 你好"));
        assert!(has_token(&h, 2, SyntaxToken::Comment, "世界 */"));
        for line in 1..=3 {
            let text = std::str::from_utf8(h.line_text(line).unwrap()).unwrap();
            for span in h.line_spans(line) {
                assert!(text.get(span.start_byte..span.end_byte).is_some());
            }
        }
        assert!(h.line_text(0).is_none());
        assert!(h.line_text(4).is_none());
        assert!(h.line_spans(0).is_empty());
    }

    #[test]
    fn line_boundaries_empty_sources_and_size_limit() {
        for (source, expected) in [
            ("", vec![""]),
            ("\n", vec![""]),
            ("a\n", vec!["a"]),
            ("a\n\n", vec!["a", ""]),
            ("a\r\nb", vec!["a", "b"]),
        ] {
            let h = highlight("zig", source);
            for (index, line) in expected.iter().enumerate() {
                assert_eq!(h.line_text(index as u32 + 1), Some(line.as_bytes()));
            }
            assert_eq!(h.line_starts.len(), expected.len());
        }
        let grammar = syntax_grammars::find("zig").unwrap();
        assert!(matches!(
            build(
                grammar.language(),
                grammar.query,
                vec![b' '; MAX_FILE_SIZE + 1]
            ),
            Err(Error::SourceTooLarge)
        ));
        assert!(build(grammar.language(), grammar.query, vec![b' '; MAX_FILE_SIZE]).is_ok());
    }
}
