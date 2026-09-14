use crate::diff::{DiffFile, DiffHunk, DiffLine, DiffLineKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Stacked,
    Split,
}

impl ViewMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stacked => "stacked",
            Self::Split => "split",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoldMode {
    Unfold,
    Fold,
}

impl FoldMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unfold => "unfold",
            Self::Fold => "fold",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FoldId {
    pub file_index: usize,
    pub hunk_index: usize,
    pub ordinal: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoldState {
    Collapsed,
    Expanded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoldEntry {
    pub id: FoldId,
    pub state: FoldState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    FileHeader,
    HunkHeader,
    Fold,
    StackedCode,
    SplitCode,
    FileMeta,
}

#[derive(Clone, Debug)]
pub struct VisualRow<'a> {
    pub kind: RowKind,
    pub hunk_index: Option<usize>,
    pub fold_id: Option<FoldId>,
    pub fold_line_count: usize,
    pub fold_expanded: bool,
    pub fold_lines: &'a [DiffLine],
    pub change_id: Option<usize>,
    pub line: Option<&'a DiffLine>,
    pub left: Option<&'a DiffLine>,
    pub right: Option<&'a DiffLine>,
    pub hunk_header: Option<&'a str>,
}

impl<'a> VisualRow<'a> {
    fn new(kind: RowKind) -> Self {
        Self {
            kind,
            hunk_index: None,
            fold_id: None,
            fold_line_count: 0,
            fold_expanded: false,
            fold_lines: &[],
            change_id: None,
            line: None,
            left: None,
            right: None,
            hunk_header: None,
        }
    }

    pub fn comment_line(&self) -> Option<&'a DiffLine> {
        self.line.or(self.right).or(self.left)
    }

    fn is_change(&self) -> bool {
        if let Some(line) = self.line {
            return is_change(line);
        }
        self.left
            .is_some_and(|line| line.kind == DiffLineKind::Delete)
            || self
                .right
                .is_some_and(|line| line.kind == DiffLineKind::Add)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangeSpan {
    pub id: usize,
    pub start_row: usize,
    pub end_row: usize,
}

#[derive(Clone, Debug)]
pub struct FileView<'a> {
    pub rows: Vec<VisualRow<'a>>,
    pub changes: Vec<ChangeSpan>,
    pub additions: usize,
    pub deletions: usize,
    pub line_number_width: usize,
}

const CONTEXT_RADIUS: usize = 3;
const MIN_FOLD_LINES: usize = 4;

pub fn build_file_view<'a>(
    file: &'a DiffFile,
    file_index: usize,
    mode: ViewMode,
    fold_mode: FoldMode,
    folds: &[FoldEntry],
) -> FileView<'a> {
    let mut view = FileView {
        rows: vec![VisualRow::new(RowKind::FileHeader)],
        changes: Vec::new(),
        additions: 0,
        deletions: 0,
        line_number_width: 4,
    };
    let mut max_line = 0;
    for line in file.hunks.iter().flat_map(|hunk| &hunk.lines) {
        max_line = max_line
            .max(line.old_lineno.unwrap_or(0))
            .max(line.new_lineno.unwrap_or(0));
        match line.kind {
            DiffLineKind::Add => view.additions += 1,
            DiffLineKind::Delete => view.deletions += 1,
            _ => {}
        }
    }
    view.line_number_width = max_line.to_string().len().max(4);
    if file.is_binary || file.hunks.is_empty() {
        view.rows.push(VisualRow::new(RowKind::FileMeta));
    }
    for (hunk_index, hunk) in file.hunks.iter().enumerate() {
        if hunk.lines.is_empty() {
            view.rows.push(VisualRow {
                hunk_index: Some(hunk_index),
                hunk_header: Some(&hunk.header),
                ..VisualRow::new(RowKind::HunkHeader)
            });
        }
        let visibility = context_visibility(hunk, fold_mode);
        let visible = |index: usize| visibility.as_ref().is_none_or(|v| v[index]);
        let mut i = 0;
        let mut ordinal = 0;
        while i < hunk.lines.len() {
            let line = &hunk.lines[i];
            if line.kind == DiffLineKind::Context && !visible(i) {
                let start = i;
                while i < hunk.lines.len()
                    && hunk.lines[i].kind == DiffLineKind::Context
                    && !visible(i)
                {
                    i += 1;
                }
                let id = FoldId {
                    file_index,
                    hunk_index,
                    ordinal,
                };
                ordinal += 1;
                if i - start < MIN_FOLD_LINES {
                    for line in &hunk.lines[start..i] {
                        view.rows.push(context_row(line, hunk_index, None, mode));
                    }
                } else {
                    let expanded = fold_state(folds, id) == FoldState::Expanded;
                    view.rows.push(VisualRow {
                        hunk_index: Some(hunk_index),
                        fold_id: Some(id),
                        fold_line_count: i - start,
                        fold_expanded: expanded,
                        fold_lines: &hunk.lines[start..i],
                        ..VisualRow::new(RowKind::Fold)
                    });
                    if expanded {
                        for line in &hunk.lines[start..i] {
                            view.rows
                                .push(context_row(line, hunk_index, Some(id), mode));
                        }
                    }
                }
                continue;
            }
            if mode == ViewMode::Stacked || line.kind == DiffLineKind::Meta {
                view.rows.push(VisualRow {
                    hunk_index: Some(hunk_index),
                    line: Some(line),
                    ..VisualRow::new(if line.kind == DiffLineKind::Meta {
                        RowKind::FileMeta
                    } else {
                        RowKind::StackedCode
                    })
                });
                i += 1;
            } else if line.kind == DiffLineKind::Context {
                view.rows.push(context_row(line, hunk_index, None, mode));
                i += 1;
            } else {
                // Git emits deletions followed by additions. Pair in order, leaving
                // the shorter side empty instead of borrowing the next context row.
                let delete_start = i;
                while i < hunk.lines.len() && hunk.lines[i].kind == DiffLineKind::Delete {
                    i += 1;
                }
                let add_start = i;
                while i < hunk.lines.len() && hunk.lines[i].kind == DiffLineKind::Add {
                    i += 1;
                }
                let deletes = &hunk.lines[delete_start..add_start];
                let adds = &hunk.lines[add_start..i];
                for pair in 0..deletes.len().max(adds.len()) {
                    view.rows.push(VisualRow {
                        hunk_index: Some(hunk_index),
                        left: deletes.get(pair),
                        right: adds.get(pair),
                        ..VisualRow::new(RowKind::SplitCode)
                    });
                }
            }
        }
    }
    assign_changes(&mut view);
    view
}

fn context_row(
    line: &DiffLine,
    hunk_index: usize,
    fold_id: Option<FoldId>,
    mode: ViewMode,
) -> VisualRow<'_> {
    let mut row = VisualRow::new(match mode {
        ViewMode::Stacked => RowKind::StackedCode,
        ViewMode::Split => RowKind::SplitCode,
    });
    row.hunk_index = Some(hunk_index);
    row.fold_id = fold_id;
    match mode {
        ViewMode::Stacked => row.line = Some(line),
        ViewMode::Split => {
            row.left = Some(line);
            row.right = Some(line);
        }
    }
    row
}

fn is_change(line: &DiffLine) -> bool {
    matches!(line.kind, DiffLineKind::Add | DiffLineKind::Delete)
}

fn context_visibility(hunk: &DiffHunk, mode: FoldMode) -> Option<Vec<bool>> {
    if mode == FoldMode::Unfold
        || !hunk.lines.iter().any(is_change)
        || !hunk
            .lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Context)
    {
        return None;
    }
    let mut visible = vec![false; hunk.lines.len()];
    for (i, line) in hunk.lines.iter().enumerate() {
        if is_change(line) {
            let start = i.saturating_sub(CONTEXT_RADIUS);
            let end = hunk.lines.len().min(i + CONTEXT_RADIUS + 1);
            for (visible, line) in visible[start..end].iter_mut().zip(&hunk.lines[start..end]) {
                if line.kind == DiffLineKind::Context {
                    *visible = true;
                }
            }
        }
    }
    Some(visible)
}

pub fn fold_state(folds: &[FoldEntry], id: FoldId) -> FoldState {
    folds
        .iter()
        .find(|entry| entry.id == id)
        .map_or(FoldState::Collapsed, |entry| entry.state)
}

fn assign_changes(view: &mut FileView<'_>) {
    let mut start = None;
    for (i, row) in view.rows.iter_mut().enumerate() {
        if row.is_change() {
            start.get_or_insert(i);
            row.change_id = Some(view.changes.len());
        } else if let Some(start_row) = start.take() {
            view.changes.push(ChangeSpan {
                id: view.changes.len(),
                start_row,
                end_row: i - 1,
            });
        }
    }
    if let Some(start_row) = start {
        view.changes.push(ChangeSpan {
            id: view.changes.len(),
            start_row,
            end_row: view.rows.len() - 1,
        });
    }
}

pub fn next_change(view: &FileView<'_>, cursor_row: usize) -> Option<usize> {
    view.changes
        .iter()
        .find(|change| change.start_row > cursor_row)
        .map(|change| change.start_row)
}

pub fn previous_change(view: &FileView<'_>, cursor_row: usize) -> Option<usize> {
    view.changes
        .iter()
        .rev()
        .find(|change| change.start_row < cursor_row)
        .map(|change| change.start_row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffSource, parse_patch};

    fn file(body: &str) -> DiffFile {
        let patch = format!(
            "diff --git a/a.zig b/a.zig\n--- a/a.zig\n+++ b/a.zig\n@@ -1,18 +1,18 @@\n{body}"
        );
        parse_patch(patch.as_bytes(), DiffSource::Explicit)
            .unwrap()
            .remove(0)
    }

    const LONG_CONTEXT: &str = " one\n two\n three\n four\n five\n six\n six-a\n six-b\n six-c\n six-d\n-old();\n+new();\n seven\n eight\n nine\n ten\n eleven\n twelve\n";

    #[test]
    fn line_number_width_includes_both_sides_and_hidden_context() {
        let patch = format!(
            "diff --git a/a.zig b/a.zig\n--- a/a.zig\n+++ b/a.zig\n@@ -100000,18 +1,18 @@\n{LONG_CONTEXT}"
        );
        let file = parse_patch(patch.as_bytes(), DiffSource::Explicit)
            .unwrap()
            .remove(0);
        for mode in [ViewMode::Stacked, ViewMode::Split] {
            for fold_mode in [FoldMode::Fold, FoldMode::Unfold] {
                assert_eq!(
                    build_file_view(&file, 0, mode, fold_mode, &[]).line_number_width,
                    6
                );
            }
        }
        assert_eq!(
            build_file_view(
                &self::file("+short\n"),
                0,
                ViewMode::Stacked,
                FoldMode::Unfold,
                &[]
            )
            .line_number_width,
            4
        );
    }

    #[test]
    fn unfold_shows_long_context_without_fold_row() {
        let file = file(LONG_CONTEXT);
        for mode in [ViewMode::Stacked, ViewMode::Split] {
            let view = build_file_view(&file, 0, mode, FoldMode::Unfold, &[]);
            assert!(view.rows.iter().all(|row| row.kind != RowKind::Fold));
            assert!(
                view.rows
                    .iter()
                    .any(|row| row.comment_line().is_some_and(|line| line.text == "six-a"))
            );
            assert_eq!((view.additions, view.deletions), (1, 1));
        }
    }

    #[test]
    fn stacked_and_split_fold_distant_context_keep_nearby_context() {
        let file = file(LONG_CONTEXT);
        for mode in [ViewMode::Stacked, ViewMode::Split] {
            let view = build_file_view(&file, 0, mode, FoldMode::Fold, &[]);
            let folds: Vec<_> = view
                .rows
                .iter()
                .filter(|row| row.kind == RowKind::Fold)
                .collect();
            assert_eq!(folds.len(), 1);
            assert_eq!(folds[0].fold_line_count, 7);
            assert!(folds[0].comment_line().is_none());
            assert!(view.rows.iter().any(|row| row.fold_id.is_none()
                && row.comment_line().is_some_and(|line| line.text == "six-b")));
            assert!(
                !view
                    .rows
                    .iter()
                    .any(|row| row.comment_line().is_some_and(|line| line.text == "six-a"))
            );
            // Three distant lines after the change do not meet the minimum of four.
            assert!(
                view.rows
                    .iter()
                    .any(|row| row.comment_line().is_some_and(|line| line.text == "twelve"))
            );
        }
    }

    #[test]
    fn fold_keeps_context_when_hunk_has_no_changes() {
        let file = file(" one\n two\n three\n four\n five\n");
        for mode in [ViewMode::Stacked, ViewMode::Split] {
            let view = build_file_view(&file, 0, mode, FoldMode::Fold, &[]);
            assert_eq!(view.rows.len(), 6);
            assert!(view.rows.iter().all(|row| row.kind != RowKind::Fold));
            assert!(view.changes.is_empty());
        }
    }

    #[test]
    fn split_pairs_deletes_and_adds_and_prefers_new_comment_line() {
        let file = file(" const std = @import(\"std\");\n-old();\n+new();\n+extra();\n");
        let view = build_file_view(&file, 0, ViewMode::Split, FoldMode::Unfold, &[]);
        let paired = &view.rows[2];
        assert_eq!(paired.left.unwrap().text, "old();");
        assert_eq!(paired.right.unwrap().text, "new();");
        assert_eq!(paired.comment_line().unwrap().text, "new();");
        assert!(view.rows[3].left.is_none());
        assert_eq!(view.rows[3].right.unwrap().text, "extra();");
        assert_eq!((view.additions, view.deletions), (2, 1));
        assert_eq!(
            view.changes,
            [ChangeSpan {
                id: 0,
                start_row: 2,
                end_row: 3
            }]
        );
    }

    #[test]
    fn split_handles_unpaired_deletes_adds_and_meta() {
        let file =
            file("+first\n context\n-one\n-two\n+replacement\n\\ No newline at end of file\n");
        let view = build_file_view(&file, 0, ViewMode::Split, FoldMode::Fold, &[]);
        assert!(view.rows[1].left.is_none());
        assert_eq!(view.rows[1].right.unwrap().text, "first");
        assert!(std::ptr::eq(
            view.rows[2].left.unwrap(),
            view.rows[2].right.unwrap()
        ));
        assert_eq!(view.rows[4].comment_line().unwrap().text, "two");
        assert!(view.rows[4].right.is_none());
        assert_eq!(view.rows[5].kind, RowKind::FileMeta);
        assert!(view.rows[5].change_id.is_none());
    }

    #[test]
    fn change_navigation_returns_next_and_previous_starts() {
        let file = file(" one\n-old();\n+new();\n two\n-old2();\n+new2();\n");
        for mode in [ViewMode::Stacked, ViewMode::Split] {
            let view = build_file_view(&file, 0, mode, FoldMode::Unfold, &[]);
            let first = next_change(&view, 0).unwrap();
            let second = next_change(&view, first).unwrap();
            assert!(second > first);
            assert_eq!(previous_change(&view, second), Some(first));
            assert_eq!(previous_change(&view, first), None);
            assert_eq!(next_change(&view, second), None);
            assert_eq!(previous_change(&view, usize::MAX), Some(second));
            for change in &view.changes {
                assert!(
                    view.rows[change.start_row..=change.end_row]
                        .iter()
                        .all(|row| row.change_id == Some(change.id))
                );
            }
        }
    }

    #[test]
    fn expanded_fold_includes_context_and_stable_ids_across_layouts() {
        let file = file(LONG_CONTEXT);
        let id = FoldId {
            file_index: 3,
            hunk_index: 0,
            ordinal: 0,
        };
        for mode in [ViewMode::Stacked, ViewMode::Split] {
            let view = build_file_view(
                &file,
                3,
                mode,
                FoldMode::Fold,
                &[FoldEntry {
                    id,
                    state: FoldState::Expanded,
                }],
            );
            let fold = &view.rows[1];
            assert!(fold.fold_expanded);
            assert_eq!(fold.fold_id, Some(id));
            let expanded: Vec<_> = view
                .rows
                .iter()
                .filter(|row| row.kind != RowKind::Fold && row.fold_id == Some(id))
                .collect();
            assert_eq!(expanded.len(), 7);
            assert_eq!(expanded[0].comment_line().unwrap().text, "one");
        }
        assert_eq!(fold_state(&[], id), FoldState::Collapsed);
    }

    #[test]
    fn minimum_fold_size_and_context_radius_are_exact() {
        for distant in [0, 1, 3, 4, 8] {
            let body = format!(
                "{}-old\n+new\n{}",
                " context\n".repeat(distant + CONTEXT_RADIUS),
                " context\n".repeat(distant + CONTEXT_RADIUS)
            );
            let file = file(&body);
            let view = build_file_view(&file, 0, ViewMode::Stacked, FoldMode::Fold, &[]);
            let folds: Vec<_> = view
                .rows
                .iter()
                .filter(|row| row.kind == RowKind::Fold)
                .collect();
            assert_eq!(folds.len(), if distant >= MIN_FOLD_LINES { 2 } else { 0 });
            for (ordinal, fold) in folds.iter().enumerate() {
                assert_eq!(fold.fold_line_count, distant);
                assert_eq!(fold.fold_id.unwrap().ordinal, ordinal);
            }
        }
    }

    #[test]
    fn file_and_empty_hunk_metadata_are_retained() {
        let mut file = file("");
        let view = build_file_view(&file, 0, ViewMode::Stacked, FoldMode::Fold, &[]);
        assert_eq!(view.rows[0].kind, RowKind::FileHeader);
        assert_eq!(view.rows[1].kind, RowKind::HunkHeader);
        assert_eq!(view.rows[1].hunk_header, Some("@@ -1,18 +1,18 @@"));
        file.hunks.clear();
        for binary in [false, true] {
            file.is_binary = binary;
            let view = build_file_view(&file, 0, ViewMode::Split, FoldMode::Fold, &[]);
            assert_eq!(view.rows.len(), 2);
            assert_eq!(view.rows[1].kind, RowKind::FileMeta);
        }
    }
}
