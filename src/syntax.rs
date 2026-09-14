use crate::{
    syntax_grammars,
    theme::{Ansi, Color, ThemeTokens},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightMode {
    TreeSitter,
    Disabled,
}

impl HighlightMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::TreeSitter => "tree_sitter",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxToken {
    Keyword,
    String,
    Comment,
    Type,
    Function,
    Number,
    Operator,
    Plain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub token: SyntaxToken,
}

pub fn mode_for_language(language: Option<&str>) -> HighlightMode {
    if language.and_then(syntax_grammars::find).is_some() {
        HighlightMode::TreeSitter
    } else {
        HighlightMode::Disabled
    }
}

pub fn render_highlighted_line(
    ansi: Ansi,
    tokens: ThemeTokens,
    text: &str,
    spans: &[HighlightSpan],
) -> String {
    if !ansi.enabled || spans.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for span in spans {
        if span.end_byte <= cursor
            || span.start_byte >= text.len()
            || span.end_byte > text.len()
            || span.start_byte >= span.end_byte
        {
            continue;
        }
        let start = cursor.max(span.start_byte);
        // Public callers may supply arbitrary byte offsets. Never slice UTF-8
        // inside a code point, even if a malformed span came from a caller.
        if !text.is_char_boundary(start) || !text.is_char_boundary(span.end_byte) {
            continue;
        }
        out.push_str(&text[cursor..start]);
        out.push_str(&ansi.fg(color_for_token(tokens, span.token)));
        out.push_str(&text[start..span.end_byte]);
        out.push_str(ansi.reset());
        cursor = span.end_byte;
    }
    out.push_str(&text[cursor..]);
    out
}

fn color_for_token(tokens: ThemeTokens, token: SyntaxToken) -> Color {
    match token {
        SyntaxToken::Keyword => tokens.syntax_keyword,
        SyntaxToken::String => tokens.syntax_string,
        SyntaxToken::Comment => tokens.syntax_comment,
        SyntaxToken::Type => tokens.syntax_type,
        SyntaxToken::Function => tokens.syntax_function,
        SyntaxToken::Number => tokens.syntax_number,
        SyntaxToken::Operator => tokens.syntax_operator,
        SyntaxToken::Plain => tokens.syntax_plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::catppuccin_mocha;

    #[test]
    fn language_modes() {
        for name in [
            "zig",
            "typescript",
            "tsx",
            "javascript",
            "rust",
            "c",
            "cpp",
            "python",
            "gn",
        ] {
            assert_eq!(mode_for_language(Some(name)), HighlightMode::TreeSitter);
        }
        assert_eq!(mode_for_language(Some("markdown")), HighlightMode::Disabled);
        assert_eq!(mode_for_language(None), HighlightMode::Disabled);
        assert_eq!(HighlightMode::TreeSitter.label(), "tree_sitter");
        assert_eq!(HighlightMode::Disabled.label(), "disabled");
    }

    #[test]
    fn renders_token_colors_and_leaves_disabled_output_unchanged() {
        let spans = [HighlightSpan {
            start_byte: 0,
            end_byte: 5,
            token: SyntaxToken::Keyword,
        }];
        let text = "const value = 1";
        let ansi = Ansi {
            enabled: true,
            true_color: true,
        };
        let rendered = render_highlighted_line(ansi, catppuccin_mocha(), text, &spans);
        assert!(rendered.contains("\x1b[38;2;"));
        assert!(rendered.contains("const\x1b[0m value = 1"));
        assert_eq!(
            render_highlighted_line(
                Ansi {
                    enabled: false,
                    ..ansi
                },
                catppuccin_mocha(),
                text,
                &spans
            ),
            text
        );
    }

    #[test]
    fn invalid_utf8_offsets_and_overlaps_preserve_text() {
        let ansi = Ansi {
            enabled: true,
            true_color: true,
        };
        let spans = [
            HighlightSpan {
                start_byte: 1,
                end_byte: 2,
                token: SyntaxToken::String,
            },
            HighlightSpan {
                start_byte: 0,
                end_byte: 3,
                token: SyntaxToken::String,
            },
            HighlightSpan {
                start_byte: 0,
                end_byte: 4,
                token: SyntaxToken::Comment,
            },
            HighlightSpan {
                start_byte: 4,
                end_byte: 99,
                token: SyntaxToken::Number,
            },
        ];
        let rendered = render_highlighted_line(ansi, catppuccin_mocha(), "你ab", &spans);
        let expected = format!(
            "{}你{}{}a{}b",
            ansi.fg(catppuccin_mocha().syntax_string),
            ansi.reset(),
            ansi.fg(catppuccin_mocha().syntax_comment),
            ansi.reset()
        );
        assert_eq!(rendered, expected);
    }
}
