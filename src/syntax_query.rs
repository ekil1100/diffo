use std::{ops::Range, sync::Arc};

use tree_sitter::{
    Language, Node, Parser, Point, Query, QueryCursor, QueryMatch, QueryPredicateArg,
    StreamingIterator, Tree,
};

use crate::{
    Error, Result,
    syntax::{HighlightSpan, SyntaxToken},
};

pub const MAX_FILE_SIZE: usize = 512 * 1024;
const CHUNK_LINES: usize = 128;

#[derive(Clone, Debug)]
pub struct SideHighlight {
    pub source: Vec<u8>,
    pub line_starts: Vec<usize>,
    pub spans_by_line: Vec<Vec<HighlightSpan>>,
    tree: Tree,
    query: Arc<Query>,
    highlighted_chunks: Vec<bool>,
    query_failed: bool,
}

impl SideHighlight {
    pub fn line_text(&self, line_number: u32) -> Option<&[u8]> {
        let index = line_number.checked_sub(1)? as usize;
        let start = *self.line_starts.get(index)?;
        Some(&self.source[start..line_end(&self.source, &self.line_starts, index)])
    }

    pub fn line_spans(&mut self, line_number: u32) -> &[HighlightSpan] {
        let Some(index) = line_number.checked_sub(1).map(|index| index as usize) else {
            return &[];
        };
        if index >= self.line_starts.len() || self.query_failed {
            return &[];
        }
        let chunk = index / CHUNK_LINES;
        if !self.highlighted_chunks[chunk] {
            self.highlight_chunk(chunk);
        }
        if self.query_failed {
            &[]
        } else {
            &self.spans_by_line[index]
        }
    }

    fn highlight_chunk(&mut self, chunk: usize) {
        let start = chunk * CHUNK_LINES;
        let end = (start + CHUNK_LINES).min(self.line_starts.len());
        let mut cursor = QueryCursor::new();
        // Query the full tree with an intersecting range: multiline captures and
        // ancestor-dependent predicates still see their complete syntax context.
        cursor.set_point_range(Point::new(start, 0)..Point::new(end, 0));
        {
            // Official bindings handle eq?, any-of?, match? and their negations.
            // Lua predicates remain application-defined.
            let mut matches =
                cursor.matches(&self.query, self.tree.root_node(), self.source.as_slice());
            while let Some(found) = matches.next() {
                if !general_predicates_pass(&self.query, found, &self.source) {
                    continue;
                }
                for capture in found.captures {
                    if let Some(token) =
                        token_for_capture(self.query.capture_names()[capture.index as usize])
                    {
                        append_capture_span(
                            &mut self.spans_by_line,
                            &self.line_starts,
                            &self.source,
                            capture.node,
                            token,
                            start..end,
                        );
                    }
                }
            }
        }
        if cursor.did_exceed_match_limit() {
            // Negative-cache failed queries instead of retrying them on every frame.
            self.query_failed = true;
            return;
        }
        for spans in &mut self.spans_by_line[start..end] {
            // Stable sorting retains query order for captures of identical bytes.
            spans.sort_by_key(|span| (span.start_byte, span.end_byte));
        }
        self.highlighted_chunks[chunk] = true;
    }
}

pub fn compile(language: &Language, query_source: &str) -> Result<Arc<Query>> {
    Query::new(language, query_source)
        .map(Arc::new)
        .map_err(|err| {
            eprintln!("tree-sitter query compile failed: {err}");
            Error::SyntaxUnavailable
        })
}

pub fn build(language: Language, query: Arc<Query>, source: Vec<u8>) -> Result<SideHighlight> {
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
    let line_starts = build_line_starts(&source);
    let spans_by_line = vec![Vec::new(); line_starts.len()];
    let highlighted_chunks = vec![false; line_starts.len().div_ceil(CHUNK_LINES)];
    Ok(SideHighlight {
        source,
        line_starts,
        spans_by_line,
        tree,
        query,
        highlighted_chunks,
        query_failed: false,
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
    rows: Range<usize>,
) {
    let first_row = node.start_position().row.max(rows.start);
    let last_row = node.end_position().row.min(rows.end - 1);
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

    fn build_with_query(language: Language, query: &str, source: Vec<u8>) -> Result<SideHighlight> {
        let query = compile(&language, query)?;
        build(language, query, source)
    }

    fn highlight(name: &str, source: &str) -> SideHighlight {
        let grammar = syntax_grammars::find(name).unwrap();
        build_with_query(
            grammar.language(),
            grammar.query,
            source.as_bytes().to_vec(),
        )
        .unwrap_or_else(|err| panic!("failed to highlight {name}: {err}"))
    }

    fn has_token(highlight: &mut SideHighlight, line: u32, token: SyntaxToken, text: &str) -> bool {
        let spans = highlight.line_spans(line).to_vec();
        let source = highlight.line_text(line).unwrap();
        spans.iter().any(|span| {
            span.token == token
                && source.get(span.start_byte..span.end_byte) == Some(text.as_bytes())
        })
    }

    #[test]
    fn file_switch_highlighting_only_computes_requested_lines() {
        let source: String = (0..4000).map(|i| format!("fn f_{i}() {{}}\n")).collect();
        let mut h = highlight("rust", &source);
        let computed = |h: &SideHighlight| {
            h.spans_by_line
                .iter()
                .filter(|spans| !spans.is_empty())
                .count()
        };
        assert_eq!(
            computed(&h),
            0,
            "Opening a file must not highlight offscreen lines"
        );
        assert!(has_token(&mut h, 3900, SyntaxToken::Keyword, "fn"));
        assert!((1..=128).contains(&computed(&h)));
        let first_chunk = computed(&h);
        let spans = h.line_spans(3900).to_vec();
        assert_eq!(h.line_spans(3900), spans);
        assert_eq!(computed(&h), first_chunk);
        assert!(has_token(&mut h, 1, SyntaxToken::Keyword, "fn"));
        assert!(computed(&h) > first_chunk);
        assert!(computed(&h) <= 256);
        assert!(h.line_spans(0).is_empty());
        assert!(h.line_spans(4001).is_empty());
    }

    #[test]
    fn chunked_queries_match_full_queries_across_languages_and_multiline_captures() {
        let mut cases: Vec<_> = [
            ("zig", "const value = \"架\";\n"),
            ("javascript", "const value = `first\nsecond`;\n"),
            (
                "typescript",
                "function f(\n  value: number,\n) { return value; }\n",
            ),
            ("tsx", "const view = <div>{\"架\"}</div>;\n"),
            ("rust", "fn f() { let value = \"first\nsecond\"; }\n"),
            ("c", "/* first\nsecond */\nint f(void) { return 1; }\n"),
            (
                "cpp",
                "class App { public:\n  int run() { return 1; }\n};\n",
            ),
            (
                "python",
                "def f():\n    \"\"\"first\n    second\"\"\"\n    return True\n",
            ),
            (
                "gn",
                "executable(\"foo\") {\n  sources = [ \"foo.cc\" ]\n}\n",
            ),
        ]
        .into_iter()
        .map(|(name, source)| (name, source.repeat(CHUNK_LINES + 1)))
        .collect();
        cases.push((
            "rust",
            format!(
                "/* first\n{}last */\nfn main() {{}}\n",
                "中间 🦀\r\n".repeat(CHUNK_LINES * 3)
            ),
        ));
        for (name, source) in cases {
            let mut h = highlight(name, &source);
            assert!(!h.tree.root_node().has_error(), "Invalid {name} fixture");
            let mut expected = vec![Vec::new(); h.line_starts.len()];
            let mut cursor = QueryCursor::new();
            {
                let mut matches = cursor.matches(&h.query, h.tree.root_node(), h.source.as_slice());
                while let Some(found) = matches.next() {
                    if !general_predicates_pass(&h.query, found, &h.source) {
                        continue;
                    }
                    for capture in found.captures {
                        if let Some(token) =
                            token_for_capture(h.query.capture_names()[capture.index as usize])
                        {
                            append_capture_span(
                                &mut expected,
                                &h.line_starts,
                                &h.source,
                                capture.node,
                                token,
                                0..h.line_starts.len(),
                            );
                        }
                    }
                }
            }
            assert!(!cursor.did_exceed_match_limit());
            for spans in &mut expected {
                spans.sort_by_key(|span| (span.start_byte, span.end_byte));
            }
            // Navigate backwards first, so no earlier chunk can prime later captures.
            for (index, spans) in expected.iter().enumerate().rev() {
                assert_eq!(
                    h.line_spans(index as u32 + 1),
                    spans,
                    "{name}, line {}",
                    index + 1
                );
            }
            assert_eq!(h.spans_by_line, expected, "{name}");
            assert!(h.highlighted_chunks.iter().all(|&ready| ready));
        }
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
            let mut highlighted = highlight(language, source);
            assert!(
                has_token(&mut highlighted, 1, token, text),
                "missing {text:?} in {language}: {:?}",
                highlighted.line_spans(1)
            );
        }
    }

    #[test]
    fn typescript_keeps_ordinary_keywords() {
        let mut h = highlight(
            "typescript",
            "const value: number = 1;\nfunction run() { return value; }\n",
        );
        assert!(has_token(&mut h, 1, SyntaxToken::Keyword, "const"));
        assert!(has_token(&mut h, 2, SyntaxToken::Keyword, "function"));
        assert!(has_token(&mut h, 2, SyntaxToken::Keyword, "return"));
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
        let mut h = build_with_query(
            grammar.language(),
            "((identifier) @type (#lua-match? @type \"^[A-Z_][a-zA-Z0-9_]*\"))",
            b"const MyType = lower;\n".to_vec(),
        )
        .unwrap();
        assert!(has_token(&mut h, 1, SyntaxToken::Type, "MyType"));
        assert!(!has_token(&mut h, 1, SyntaxToken::Type, "lower"));
        let mut malformed = build_with_query(
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
            let mut h =
                build_with_query(grammar.language(), &query, b"UPPER; lower; Other;".to_vec())
                    .unwrap();
            let spans = h.line_spans(1).to_vec();
            let captured: Vec<_> = spans
                .iter()
                .map(|s| std::str::from_utf8(&h.source[s.start_byte..s.end_byte]).unwrap())
                .collect();
            assert_eq!(captured, expected, "{predicate}");
        }
    }

    #[test]
    fn multiline_utf8_and_crlf_spans_stay_within_line_bytes() {
        let mut h = highlight("rust", "/* 你好\r\n世界 */\r\nlet text = \"🦀\";\n");
        assert_eq!(h.line_text(1), Some("/* 你好".as_bytes()));
        assert_eq!(h.line_text(2), Some("世界 */".as_bytes()));
        assert_eq!(h.line_text(3), Some("let text = \"🦀\";".as_bytes()));
        assert!(has_token(&mut h, 1, SyntaxToken::Comment, "/* 你好"));
        assert!(has_token(&mut h, 2, SyntaxToken::Comment, "世界 */"));
        for line in 1..=3 {
            let spans = h.line_spans(line).to_vec();
            let text = std::str::from_utf8(h.line_text(line).unwrap()).unwrap();
            for span in spans {
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
            build_with_query(
                grammar.language(),
                grammar.query,
                vec![b' '; MAX_FILE_SIZE + 1]
            ),
            Err(Error::SourceTooLarge)
        ));
        assert!(
            build_with_query(grammar.language(), grammar.query, vec![b' '; MAX_FILE_SIZE]).is_ok()
        );
    }
}
