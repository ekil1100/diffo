use std::collections::HashMap;

use crate::{
    Error, Result,
    diff::{DiffFile, DiffLine, DiffLineKind, Repository, ReviewTarget},
    git::{self, FileSide},
    syntax, syntax_grammars,
    syntax_query::{self, SideHighlight},
    theme::{Ansi, ThemeTokens},
};

#[derive(Clone, Debug)]
pub struct SyntaxCache {
    repo: Repository,
    target: ReviewTarget,
    debug_git: bool,
    entries: HashMap<(usize, FileSide), Outcome>,
}

#[derive(Clone, Debug)]
enum Outcome {
    Ready(SideHighlight),
    Unavailable,
    TooLarge,
}

impl SyntaxCache {
    pub fn new(repo: &Repository, target: &ReviewTarget, debug_git: bool) -> Self {
        Self {
            repo: repo.clone(),
            target: target.clone(),
            debug_git,
            entries: HashMap::new(),
        }
    }

    pub fn highlight_diff_line(
        &mut self,
        ansi: Ansi,
        palette: ThemeTokens,
        file_index: usize,
        file: &DiffFile,
        line: &DiffLine,
    ) -> Result<String> {
        if !ansi.enabled {
            return Err(Error::SyntaxUnavailable);
        }
        let (side, number) = line_ref(line).ok_or(Error::SyntaxUnavailable)?;
        let highlight = self.get(file_index, file, side)?;
        if highlight.line_text(number) != Some(line.text.as_bytes()) {
            return Err(Error::SyntaxUnavailable);
        }
        Ok(syntax::render_highlighted_line(
            ansi,
            palette,
            &line.text,
            highlight.line_spans(number),
        ))
    }

    fn get(
        &mut self,
        file_index: usize,
        file: &DiffFile,
        side: FileSide,
    ) -> Result<&SideHighlight> {
        let key = (file_index, side);
        // A cache belongs to one immutable snapshot. Cache negative outcomes as
        // well, so repainting a missing/oversized side never repeats Git or I/O.
        if !self.entries.contains_key(&key) {
            let outcome = self.compute_outcome(file, side)?;
            self.entries.insert(key, outcome);
        }
        match &self.entries[&key] {
            Outcome::Ready(highlight) => Ok(highlight),
            Outcome::Unavailable => Err(Error::SyntaxUnavailable),
            Outcome::TooLarge => Err(Error::SourceTooLarge),
        }
    }

    fn compute_outcome(&self, file: &DiffFile, side: FileSide) -> Result<Outcome> {
        let Some(grammar) = file.language.as_deref().and_then(syntax_grammars::find) else {
            return Ok(Outcome::Unavailable);
        };
        let source = match git::load_file_side(&self.repo, &self.target, file, side, self.debug_git)
        {
            Ok(Some(source)) => source,
            Ok(None) | Err(Error::SyntaxUnavailable) => return Ok(Outcome::Unavailable),
            Err(Error::SourceTooLarge) => return Ok(Outcome::TooLarge),
            Err(err) => return Err(err),
        };
        match syntax_query::build(grammar.language(), grammar.query, source) {
            Ok(highlight) => Ok(Outcome::Ready(highlight)),
            Err(Error::SourceTooLarge) => Ok(Outcome::TooLarge),
            Err(Error::SyntaxUnavailable) => Ok(Outcome::Unavailable),
            Err(err) => Err(err),
        }
    }
}

fn line_ref(line: &DiffLine) -> Option<(FileSide, u32)> {
    match line.kind {
        DiffLineKind::Delete => line.old_lineno.map(|number| (FileSide::Old, number)),
        DiffLineKind::Add => line.new_lineno.map(|number| (FileSide::New, number)),
        DiffLineKind::Context => line
            .new_lineno
            .map(|number| (FileSide::New, number))
            .or_else(|| line.old_lineno.map(|number| (FileSide::Old, number))),
        DiffLineKind::Meta => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diff::{self, DiffSource, FileStatus},
        theme::catppuccin_mocha,
    };
    use std::{fs, process::Command};

    const ANSI: Ansi = Ansi {
        enabled: true,
        true_color: true,
    };

    fn line(kind: DiffLineKind, old: Option<u32>, new: Option<u32>, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old_lineno: old,
            new_lineno: new,
            text: text.into(),
            stable_line_id: String::new(),
        }
    }

    fn fixture() -> (tempfile::TempDir, Repository, ReviewTarget, DiffFile) {
        let directory = tempfile::tempdir().unwrap();
        let repo = Repository {
            root_path: directory.path().to_path_buf(),
            repo_id: "syntax-test".into(),
            current_branch: "test".into(),
        };
        let target = diff::make_review_target(&[]);
        let file = DiffFile {
            path: "sample.rs".into(),
            old_path: None,
            status: FileStatus::Added,
            source: DiffSource::Untracked,
            language: Some("rust".into()),
            is_binary: false,
            hunks: Vec::new(),
            patch_fingerprint: String::new(),
            patch_text: Vec::new(),
        };
        (directory, repo, target, file)
    }

    #[test]
    fn line_references_select_correct_sides() {
        assert_eq!(
            line_ref(&line(DiffLineKind::Add, None, Some(12), "")),
            Some((FileSide::New, 12))
        );
        assert_eq!(
            line_ref(&line(DiffLineKind::Delete, Some(7), None, "")),
            Some((FileSide::Old, 7))
        );
        assert_eq!(
            line_ref(&line(DiffLineKind::Context, Some(7), Some(12), "")),
            Some((FileSide::New, 12))
        );
        assert_eq!(
            line_ref(&line(DiffLineKind::Context, Some(7), None, "")),
            Some((FileSide::Old, 7))
        );
        assert_eq!(
            line_ref(&line(DiffLineKind::Meta, Some(7), Some(12), "")),
            None
        );
        assert_eq!(line_ref(&line(DiffLineKind::Add, None, None, "")), None);
    }

    #[test]
    fn successful_highlights_are_cached_and_mismatches_are_rejected() {
        let (_directory, repo, target, file) = fixture();
        let path = repo.root_path.join(&file.path);
        fs::write(&path, "fn main() {}\n").unwrap();
        let mut cache = SyntaxCache::new(&repo, &target, false);
        let original = line(DiffLineKind::Add, None, Some(1), "fn main() {}");
        let first = cache
            .highlight_diff_line(ANSI, catppuccin_mocha(), 0, &file, &original)
            .unwrap();
        assert!(first.contains("\x1b[38;2;"));
        fs::remove_file(&path).unwrap();
        assert_eq!(
            cache
                .highlight_diff_line(ANSI, catppuccin_mocha(), 0, &file, &original)
                .unwrap(),
            first
        );
        assert_eq!(cache.entries.len(), 1);
        let changed = line(DiffLineKind::Add, None, Some(1), "fn changed() {}");
        assert!(matches!(
            cache.highlight_diff_line(ANSI, catppuccin_mocha(), 0, &file, &changed),
            Err(Error::SyntaxUnavailable)
        ));
        let absent = line(DiffLineKind::Add, None, Some(2), "fn main() {}");
        assert!(matches!(
            cache.highlight_diff_line(ANSI, catppuccin_mocha(), 0, &file, &absent),
            Err(Error::SyntaxUnavailable)
        ));
    }

    #[test]
    fn missing_and_too_large_sources_are_negative_cached() {
        let (_directory, repo, target, file) = fixture();
        let path = repo.root_path.join(&file.path);
        let mut cache = SyntaxCache::new(&repo, &target, false);
        assert!(matches!(
            cache.get(0, &file, FileSide::New),
            Err(Error::SyntaxUnavailable)
        ));
        fs::write(&path, "fn main() {}\n").unwrap();
        assert!(matches!(
            cache.get(0, &file, FileSide::New),
            Err(Error::SyntaxUnavailable)
        ));
        assert!(cache.get(1, &file, FileSide::New).is_ok());
        fs::write(&path, vec![b' '; syntax_query::MAX_FILE_SIZE + 1]).unwrap();
        assert!(matches!(
            cache.get(2, &file, FileSide::New),
            Err(Error::SourceTooLarge)
        ));
        fs::write(&path, "fn main() {}\n").unwrap();
        assert!(matches!(
            cache.get(2, &file, FileSide::New),
            Err(Error::SourceTooLarge)
        ));
        assert_eq!(cache.entries.len(), 3);
    }

    #[test]
    fn unsupported_languages_and_disabled_ansi_do_not_load_source() {
        let (_directory, repo, target, mut file) = fixture();
        let mut cache = SyntaxCache::new(&repo, &target, false);
        let added = line(DiffLineKind::Add, None, Some(1), "fn main() {}");
        assert!(matches!(
            cache.highlight_diff_line(
                Ansi {
                    enabled: false,
                    ..ANSI
                },
                catppuccin_mocha(),
                0,
                &file,
                &added
            ),
            Err(Error::SyntaxUnavailable)
        ));
        assert!(cache.entries.is_empty());
        file.language = Some("markdown".into());
        assert!(matches!(
            cache.get(0, &file, FileSide::New),
            Err(Error::SyntaxUnavailable)
        ));
        assert!(matches!(
            cache.entries[&(0, FileSide::New)],
            Outcome::Unavailable
        ));
    }

    #[test]
    fn old_index_and_new_worktree_have_independent_caches() {
        let (_directory, repo, target, mut file) = fixture();
        for args in [
            &["init", "-q"][..],
            &["config", "core.autocrlf", "false"][..],
        ] {
            let result = Command::new("git")
                .args(args)
                .current_dir(&repo.root_path)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        let path = repo.root_path.join(&file.path);
        fs::write(&path, "fn old() {}\n").unwrap();
        let result = Command::new("git")
            .args(["add", "--", &file.path])
            .current_dir(&repo.root_path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        fs::write(&path, "fn new() {}\n").unwrap();
        file.source = DiffSource::Unstaged;
        file.status = FileStatus::Modified;
        let mut cache = SyntaxCache::new(&repo, &target, false);
        for (kind, old, new, text) in [
            (DiffLineKind::Delete, Some(1), None, "fn old() {}"),
            (DiffLineKind::Add, None, Some(1), "fn new() {}"),
        ] {
            let rendered = cache
                .highlight_diff_line(
                    ANSI,
                    catppuccin_mocha(),
                    0,
                    &file,
                    &line(kind, old, new, text),
                )
                .unwrap();
            assert!(rendered.contains("\x1b[38;2;"));
        }
        assert_eq!(cache.entries.len(), 2);
    }
}
