#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PairRanges {
    pub old: Vec<Range>,
    pub new: Vec<Range>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenKind {
    Whitespace,
    Word,
    Punct,
}

#[derive(Clone, Copy, Debug)]
struct Token {
    start: usize,
    end: usize,
    kind: TokenKind,
}

const MAX_LCS_TOKENS: usize = 240;
const MAX_LCS_CELLS: usize = 24_000;

pub fn diff_ranges(old_text: &str, new_text: &str) -> PairRanges {
    if old_text == new_text {
        return PairRanges::default();
    }
    let old = tokenize(old_text);
    let new = tokenize(new_text);
    if old.is_empty()
        || new.is_empty()
        || old.len() > MAX_LCS_TOKENS
        || new.len() > MAX_LCS_TOKENS
        || old.len() * new.len() > MAX_LCS_CELLS
    {
        return affix_ranges(old_text, new_text);
    }

    let equal = |i: usize, j: usize| {
        old_text[old[i].start..old[i].end] == new_text[new[j].start..new[j].end]
    };
    let cols = new.len() + 1;
    let mut dp = vec![0usize; (old.len() + 1) * cols];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            dp[i * cols + j] = if equal(i, j) {
                dp[(i + 1) * cols + j + 1] + 1
            } else {
                dp[(i + 1) * cols + j].max(dp[i * cols + j + 1])
            };
        }
    }
    let mut old_matched = vec![false; old.len()];
    let mut new_matched = vec![false; new.len()];
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        if equal(i, j) {
            old_matched[i] = true;
            new_matched[j] = true;
            i += 1;
            j += 1;
        } else if dp[(i + 1) * cols + j] >= dp[i * cols + j + 1] {
            // Keep the original deletion-first tie break for repeated tokens.
            i += 1;
        } else {
            j += 1;
        }
    }
    PairRanges {
        old: ranges_from_tokens(&old, &old_matched),
        new: ranges_from_tokens(&new, &new_matched),
    }
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    for (start, ch) in text.char_indices() {
        let kind = if matches!(ch, ' ' | '\t' | '\r' | '\n') {
            TokenKind::Whitespace
        } else if ch.is_ascii_alphanumeric() || ch == '_' {
            TokenKind::Word
        } else {
            TokenKind::Punct
        };
        let end = start + ch.len_utf8();
        if let Some(previous) = tokens.last_mut()
            && kind != TokenKind::Punct
            && previous.kind == kind
        {
            previous.end = end;
        } else {
            tokens.push(Token { start, end, kind });
        }
        // Once over the cap, only the fact that the cap was exceeded matters.
        if tokens.len() > MAX_LCS_TOKENS {
            break;
        }
    }
    tokens
}

fn ranges_from_tokens(tokens: &[Token], matched: &[bool]) -> Vec<Range> {
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if matched[i] {
            i += 1;
            continue;
        }
        let mut start = i;
        let mut significant = false;
        while i < tokens.len() && !matched[i] {
            significant |= tokens[i].kind != TokenKind::Whitespace;
            i += 1;
        }
        let mut end = i;
        if significant {
            while start < end && tokens[start].kind == TokenKind::Whitespace {
                start += 1;
            }
            while end > start && tokens[end - 1].kind == TokenKind::Whitespace {
                end -= 1;
            }
        }
        if start < end {
            append_range(
                &mut ranges,
                Range {
                    start: tokens[start].start,
                    end: tokens[end - 1].end,
                },
            );
        }
    }
    ranges
}

fn append_range(ranges: &mut Vec<Range>, range: Range) {
    if range.start >= range.end {
        return;
    }
    if let Some(previous) = ranges.last_mut()
        && previous.end >= range.start
    {
        previous.end = previous.end.max(range.end);
    } else {
        ranges.push(range);
    }
}

fn affix_ranges(old: &str, new: &str) -> PairRanges {
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = old[prefix..]
        .bytes()
        .rev()
        .zip(new[prefix..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    let range = |text: &str| {
        if prefix < text.len() - suffix {
            vec![Range {
                start: prefix,
                end: text.len() - suffix,
            }]
        } else {
            Vec::new()
        }
    };
    PairRanges {
        old: range(old),
        new: range(new),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts<'a>(text: &'a str, ranges: &[Range]) -> Vec<&'a str> {
        ranges
            .iter()
            .map(|range| &text[range.start..range.end])
            .collect()
    }

    #[test]
    fn token_diff_keeps_moved_common_run_unhighlighted() {
        let old = "    snapshot.files[state.active_file], store, view, state,";
        let new = "    state, snapshot.files[state.active_file],";
        let ranges = diff_ranges(old, new);
        assert_eq!(parts(old, &ranges.old), ["store, view, state,"]);
        assert_eq!(parts(new, &ranges.new), ["state,"]);
    }

    #[test]
    fn token_diff_marks_only_changed_suffix() {
        let old = "const name = oldValue;";
        let new = "const name = newValue;";
        let ranges = diff_ranges(old, new);
        assert_eq!(parts(old, &ranges.old), ["oldValue"]);
        assert_eq!(parts(new, &ranges.new), ["newValue"]);
    }

    #[test]
    fn token_diff_preserves_whitespace_only_changes() {
        let ranges = diff_ranges("call(a, b)", "call(a,b)");
        assert_eq!(parts("call(a, b)", &ranges.old), [" "]);
        assert!(ranges.new.is_empty());
        let ranges = diff_ranges(" \t", "  ");
        assert_eq!(parts(" \t", &ranges.old), [" \t"]);
        assert_eq!(parts("  ", &ranges.new), ["  "]);
    }

    #[test]
    fn multiple_edits_preserve_common_middle() {
        let old = "old(first, shared, last);";
        let new = "new(first, shared, end);";
        let ranges = diff_ranges(old, new);
        assert_eq!(parts(old, &ranges.old), ["old", "last"]);
        assert_eq!(parts(new, &ranges.new), ["new", "end"]);
    }

    #[test]
    fn identical_empty_insert_and_delete() {
        for text in ["", "a", "架😀e\u{301}"] {
            assert_eq!(diff_ranges(text, text), PairRanges::default());
        }
        assert_eq!(
            diff_ranges("", "架"),
            PairRanges {
                old: vec![],
                new: vec![Range { start: 0, end: 3 }]
            }
        );
        assert_eq!(
            diff_ranges("架", ""),
            PairRanges {
                old: vec![Range { start: 0, end: 3 }],
                new: vec![]
            }
        );
    }

    #[test]
    fn unicode_tokens_keep_byte_boundaries_and_common_characters() {
        let old = "call(架构, 😀);";
        let new = "call(架子, 😃);";
        let ranges = diff_ranges(old, new);
        assert_eq!(parts(old, &ranges.old), ["构", "😀"]);
        assert_eq!(parts(new, &ranges.new), ["子", "😃"]);
    }

    #[test]
    fn lcs_limits_use_utf8_safe_affix_ranges() {
        // 241 punctuation tokens exceed the token cap; 160 exceed the cell cap.
        for count in [160, MAX_LCS_TOKENS + 1] {
            let common = ";".repeat(count);
            let old = format!("{common}😀 middle À{common}");
            let new = format!("{common}😃 middle Ā{common}");
            let ranges = diff_ranges(&old, &new);
            assert_eq!(parts(&old, &ranges.old), ["😀 middle À"]);
            assert_eq!(parts(&new, &ranges.new), ["😃 middle Ā"]);
        }
    }

    #[test]
    fn affix_does_not_overlap_or_split_unicode() {
        for (old, new, old_parts, new_parts) in [
            ("😀", "😃", vec!["😀"], vec!["😃"]),
            ("À", "Ā", vec!["À"], vec!["Ā"]),
            ("a", "aa", vec![], vec!["a"]),
            ("架a", "架", vec!["a"], vec![]),
        ] {
            let ranges = affix_ranges(old, new);
            assert_eq!(parts(old, &ranges.old), old_parts);
            assert_eq!(parts(new, &ranges.new), new_parts);
        }
    }

    #[test]
    fn coalesce_only_overlapping_or_adjacent_nonempty_ranges() {
        let mut ranges = vec![];
        for (start, end) in [(1, 3), (2, 4), (4, 6), (9, 10), (11, 11)] {
            append_range(&mut ranges, Range { start, end });
        }
        assert_eq!(
            ranges,
            [Range { start: 1, end: 6 }, Range { start: 9, end: 10 }]
        );
    }

    #[test]
    fn generated_unicode_ranges_are_ordered_nonempty_and_in_bounds() {
        let atoms = [
            "", "a", "_", " ", "\t", "架", "😀", "\u{301}", ";", "old new",
        ];
        for a in atoms {
            for b in atoms {
                let old = format!("{a}({b}){a}");
                let new = format!("{b}({a}){b}");
                let pair = diff_ranges(&old, &new);
                for (text, ranges) in [(&old, &pair.old), (&new, &pair.new)] {
                    let mut previous = 0;
                    for range in ranges {
                        assert!(
                            previous <= range.start
                                && range.start < range.end
                                && range.end <= text.len()
                        );
                        assert!(
                            text.is_char_boundary(range.start) && text.is_char_boundary(range.end)
                        );
                        previous = range.end;
                    }
                }
            }
        }
    }
}
