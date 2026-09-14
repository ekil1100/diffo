use crate::{Error, Result, util};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Add,
    Delete,
    Meta,
}
impl DiffLineKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Add => "add",
            Self::Delete => "delete",
            Self::Meta => "meta",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_lineno: Option<u32>,
    pub new_lineno: Option<u32>,
    pub text: String,
    pub stable_line_id: String,
}
#[derive(Clone, Debug)]
pub struct DiffHunk {
    pub header: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Binary,
}
impl FileStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::Binary => "B",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffSource {
    Unstaged,
    Staged,
    Untracked,
    Explicit,
}
impl DiffSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unstaged => "unstaged",
            Self::Staged => "staged",
            Self::Untracked => "untracked",
            Self::Explicit => "target",
        }
    }
}
#[derive(Clone, Debug)]
pub struct DiffFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub source: DiffSource,
    pub language: Option<String>,
    pub is_binary: bool,
    pub hunks: Vec<DiffHunk>,
    pub patch_fingerprint: String,
    pub patch_text: Vec<u8>,
}
impl DiffFile {
    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|h| 1 + h.lines.len()).sum()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewTargetKind {
    WorkingTree,
    Cached,
    Commit,
    Range,
    SymmetricRange,
}
impl ReviewTargetKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::WorkingTree => "working_tree",
            Self::Cached => "cached",
            Self::Commit => "commit",
            Self::Range => "range",
            Self::SymmetricRange => "symmetric_range",
        }
    }
}
#[derive(Clone, Debug)]
pub struct ReviewTarget {
    pub kind: ReviewTargetKind,
    pub raw_args: Vec<String>,
    pub normalized_spec: String,
    pub target_id: String,
}
#[derive(Clone, Debug)]
pub struct Repository {
    pub root_path: PathBuf,
    pub repo_id: String,
    pub current_branch: String,
}
#[derive(Clone, Debug)]
pub struct DiffSnapshot {
    pub snapshot_id: String,
    pub repository: Repository,
    pub review_target: ReviewTarget,
    pub files: Vec<DiffFile>,
}
impl DiffSnapshot {
    pub fn total_lines(&self) -> usize {
        self.files.iter().map(DiffFile::line_count).sum()
    }
}

pub fn make_review_target(args: &[String]) -> ReviewTarget {
    let normalized_spec = util::join_args(args);
    let mut kind = if args.is_empty() {
        ReviewTargetKind::WorkingTree
    } else {
        ReviewTargetKind::Commit
    };
    for arg in args {
        if arg == "--cached" || arg == "--staged" {
            kind = ReviewTargetKind::Cached;
            break;
        }
        if arg.contains("...") {
            kind = ReviewTargetKind::SymmetricRange;
            break;
        }
        if arg.contains("..") {
            kind = ReviewTargetKind::Range;
            break;
        }
    }
    let hash = util::hash_hex(format!("{}:{normalized_spec}", kind.label()).as_bytes());
    ReviewTarget {
        kind,
        raw_args: args.to_vec(),
        normalized_spec,
        target_id: format!("target_{}", &hash[..16]),
    }
}

fn find_diff_start(bytes: &[u8]) -> Option<usize> {
    if bytes.starts_with(b"diff --git ") {
        Some(0)
    } else {
        find(bytes, b"\ndiff --git ").map(|n| n + 1)
    }
}
fn find(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes.windows(needle.len()).position(|w| w == needle)
}
// Byte-specific trimming is intentional: whitespace inside patches is significant.
pub(crate) fn trim_cr(line: &[u8]) -> &[u8] {
    let end = line.iter().rposition(|&b| b != b'\r').map_or(0, |i| i + 1);
    &line[..end]
}
fn text(bytes: &[u8]) -> String {
    util::lossy_utf8(bytes)
}
fn strip_git_prefix(path: &[u8]) -> &[u8] {
    if path.starts_with(b"a/") || path.starts_with(b"b/") {
        &path[2..]
    } else {
        path
    }
}

// Do not unquote Git paths here: legacy comment identities include Git's literal spelling.
#[derive(Default)]
struct PatchPaths<'a> {
    path: Option<&'a [u8]>,
    old: Option<&'a [u8]>,
}
impl<'a> PatchPaths<'a> {
    fn update(&mut self, line: &'a [u8], in_hunk: bool) -> bool {
        if let Some(rest) = line.strip_prefix(b"diff --git ") {
            if let Some(i) = find(rest, b" b/") {
                self.old = Some(strip_git_prefix(&rest[..i]));
                self.path = Some(strip_git_prefix(&rest[i + 1..]));
            } else {
                self.old = None;
                self.path = Some(rest);
            }
        } else if let Some(value) = line
            .strip_prefix(b"rename from ")
            .or_else(|| line.strip_prefix(b"copy from "))
        {
            self.old = Some(value);
        } else if let Some(value) = line
            .strip_prefix(b"rename to ")
            .or_else(|| line.strip_prefix(b"copy to "))
        {
            self.path = Some(value);
        } else if !in_hunk && line.starts_with(b"+++ ") {
            if &line[4..] != b"/dev/null" {
                self.path = Some(strip_git_prefix(&line[4..]));
            }
        } else if !in_hunk && line.starts_with(b"--- ") {
            if &line[4..] != b"/dev/null" {
                self.old = Some(strip_git_prefix(&line[4..]));
            }
        } else {
            return false;
        }
        true
    }
}

// Recover raw names from the first section even when the public display string is lossy.
// Merged files retain the first section's metadata, just like the original implementation.
pub(crate) fn raw_file_paths(file: &DiffFile) -> (&[u8], Option<&[u8]>) {
    // Valid UTF-8 names are already lossless; avoid rescanning full-context patches.
    if !file.path.contains('\u{fffd}')
        && !file
            .old_path
            .as_deref()
            .is_some_and(|path| path.contains('\u{fffd}'))
    {
        return (
            file.path.as_bytes(),
            file.old_path.as_deref().map(str::as_bytes),
        );
    }
    let mut paths = PatchPaths::default();
    let mut in_hunk = false;
    let mut seen_diff = false;
    for raw in file.patch_text.split(|&b| b == b'\n') {
        let line = trim_cr(raw);
        if line.starts_with(b"diff --git ") {
            if seen_diff {
                break;
            }
            seen_diff = true;
        }
        paths.update(line, in_hunk);
        in_hunk |= line.starts_with(b"@@ ");
    }
    (
        paths.path.unwrap_or(file.path.as_bytes()),
        paths
            .old
            .or_else(|| file.old_path.as_deref().map(str::as_bytes)),
    )
}

pub fn parse_patch(patch: &[u8], source: DiffSource) -> Result<Vec<DiffFile>> {
    let mut files = Vec::new();
    let mut pos = 0;
    while pos < patch.len() {
        let Some(relative) = find_diff_start(&patch[pos..]) else {
            break;
        };
        let start = pos + relative;
        let end = find_diff_start(&patch[start + 1..]).map_or(patch.len(), |i| start + 1 + i);
        let mut section = &patch[start..end];
        while section.last() == Some(&b'\n') {
            section = &section[..section.len() - 1];
        }
        files.push(parse_file_patch(section, source)?);
        pos = end;
    }
    Ok(files)
}

fn parse_file_patch(patch: &[u8], source: DiffSource) -> Result<DiffFile> {
    let mut paths = PatchPaths::default();
    let mut status = FileStatus::Modified;
    let mut is_binary = false;
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let (mut old_line, mut new_line) = (0u32, 0u32);
    for raw in patch.split(|&b| b == b'\n') {
        let line = trim_cr(raw);
        if line.starts_with(b"rename from ") || line.starts_with(b"rename to ") {
            status = FileStatus::Renamed;
        }
        if line.starts_with(b"copy from ") || line.starts_with(b"copy to ") {
            status = FileStatus::Copied;
        }
        if paths.update(line, !hunks.is_empty()) {
            continue;
        }
        if line.starts_with(b"new file mode") {
            status = FileStatus::Added;
        } else if line.starts_with(b"deleted file mode") {
            status = FileStatus::Deleted;
        } else if line.starts_with(b"Binary files ") || line.starts_with(b"GIT binary patch") {
            status = FileStatus::Binary;
            is_binary = true;
        } else if line.starts_with(b"@@ ") {
            let (old_start, old_count, new_start, new_count) =
                parse_hunk_header(line).ok_or(Error::ParseFailed)?;
            old_line = old_start;
            new_line = new_start;
            hunks.push(DiffHunk {
                header: text(line),
                old_start,
                old_count,
                new_start,
                new_count,
                lines: Vec::new(),
            });
        } else if let Some(hunk) = hunks.last_mut() {
            let kind = match line.first() {
                Some(b'+') => DiffLineKind::Add,
                Some(b'-') => DiffLineKind::Delete,
                Some(b'\\') => DiffLineKind::Meta,
                _ => DiffLineKind::Context,
            };
            let content = if matches!(line.first(), Some(b'+' | b'-' | b' ')) {
                &line[1..]
            } else {
                line
            };
            let (old_lineno, new_lineno) = match kind {
                DiffLineKind::Add => {
                    let n = new_line;
                    new_line = new_line.saturating_add(1);
                    (None, Some(n))
                }
                DiffLineKind::Delete => {
                    let n = old_line;
                    old_line = old_line.saturating_add(1);
                    (Some(n), None)
                }
                DiffLineKind::Context => {
                    let pair = (Some(old_line), Some(new_line));
                    old_line = old_line.saturating_add(1);
                    new_line = new_line.saturating_add(1);
                    pair
                }
                DiffLineKind::Meta => (None, None),
            };
            let mut id_input = paths.path.unwrap_or(b"").to_vec();
            // Zig's {?d} emits decimal values or the literal "null" (std.Io.Writer).
            id_input.extend_from_slice(
                format!(
                    ":{}:{}:{}:",
                    kind.label(),
                    optional_number(old_lineno),
                    optional_number(new_lineno)
                )
                .as_bytes(),
            );
            id_input.extend_from_slice(content);
            let hash = util::hash_hex(&id_input);
            hunk.lines.push(DiffLine {
                kind,
                old_lineno,
                new_lineno,
                text: text(content),
                stable_line_id: format!("line_{}", &hash[..16]),
            });
        }
    }
    let path = text(paths.path.unwrap_or(b"(unknown)"));
    Ok(DiffFile {
        language: detect_language(&path),
        path,
        old_path: paths.old.map(text),
        status,
        source,
        is_binary,
        hunks,
        patch_fingerprint: format!("sha256:{}", util::hash_hex(patch)),
        patch_text: patch.to_vec(),
    })
}
fn optional_number(value: Option<u32>) -> String {
    value.map_or_else(|| "null".into(), |n| n.to_string())
}
fn parse_hunk_header(header: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let first = header.get(3..)?.iter().position(|&b| b == b' ')? + 3;
    let second = header[first + 1..].iter().position(|&b| b == b' ')? + first + 1;
    let old = header[3..first].strip_prefix(b"-")?;
    let new = header[first + 1..second].strip_prefix(b"+")?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    Some((old_start, old_count, new_start, new_count))
}
fn parse_range(part: &[u8]) -> Option<(u32, u32)> {
    fn number(bytes: &[u8]) -> Option<u32> {
        // Zig parseInt permits signs and internal digit separators, including unsigned -0.
        let (negative, digits) = match bytes.first()? {
            b'+' => (false, &bytes[1..]),
            b'-' => (true, &bytes[1..]),
            _ => (false, bytes),
        };
        if digits.is_empty() || digits.first() == Some(&b'_') || digits.last() == Some(&b'_') {
            return None;
        }
        let mut value = 0u32;
        for &digit in digits {
            if digit == b'_' {
                continue;
            }
            if !digit.is_ascii_digit() {
                return None;
            }
            value = value
                .checked_mul(10)?
                .checked_add(u32::from(digit - b'0'))?;
        }
        if negative && value != 0 {
            None
        } else {
            Some(value)
        }
    }
    if let Some(i) = part.iter().position(|&b| b == b',') {
        Some((number(&part[..i])?, number(&part[i + 1..])?))
    } else {
        Some((number(part)?, 1))
    }
}

pub fn detect_language(path: &str) -> Option<String> {
    let base = std::path::Path::new(path).file_name()?.to_str()?;
    let language = match base {
        "Makefile" => "make",
        "Dockerfile" => "dockerfile",
        "build.zig" => "zig",
        ".gn" => "gn",
        _ => match base
            .rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())?
            .1
        {
            "zig" => "zig",
            "tsx" => "tsx",
            "ts" | "mts" | "cts" => "typescript",
            "js" | "jsx" | "mjs" | "cjs" => "javascript",
            "py" | "pyw" => "python",
            "go" => "go",
            "rs" => "rust",
            "c" | "h" => "c",
            "cpp" | "hpp" | "cc" | "cxx" | "hxx" | "hh" => "cpp",
            "java" => "java",
            "md" => "markdown",
            "json" => "json",
            "html" => "html",
            "gn" | "gni" => "gn",
            "css" => "css",
            _ => return None,
        },
    };
    Some(language.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_all_legacy_languages() {
        for (path, language) in [
            ("src/app.ts", "typescript"),
            ("src/app.tsx", "tsx"),
            ("a.mts", "typescript"),
            ("a.cts", "typescript"),
            ("a.js", "javascript"),
            ("a.cjs", "javascript"),
            ("a.mjs", "javascript"),
            ("a.jsx", "javascript"),
            ("src/lib.rs", "rust"),
            ("a.c", "c"),
            ("a.h", "c"),
            ("a.cpp", "cpp"),
            ("a.hh", "cpp"),
            ("a.py", "python"),
            ("a.pyw", "python"),
            ("BUILD.gn", "gn"),
            ("imports.gni", "gn"),
            (".gn", "gn"),
            ("Makefile", "make"),
            ("Dockerfile", "dockerfile"),
            ("build.zig", "zig"),
            ("a.go", "go"),
            ("a.java", "java"),
            ("a.md", "markdown"),
            ("a.json", "json"),
            ("a.html", "html"),
            ("a.css", "css"),
        ] {
            assert_eq!(detect_language(path).as_deref(), Some(language));
        }
        assert_eq!(detect_language("README"), None);
        assert_eq!(detect_language(".rs"), None);
        assert_eq!(detect_language("a.rs/"), Some("rust".into()));
        #[cfg(unix)]
        assert_eq!(detect_language("dir\\Makefile"), None);
    }
    #[test]
    fn parses_unified_diff_and_header_like_content() {
        let patch = b"diff --git a/src/main.zig b/src/main.zig\nindex 1111111..2222222 100644\n--- a/src/main.zig\n+++ b/src/main.zig\n@@ -1,2 +1,3 @@\n const std = @import(\"std\");\n-old();\n+new();\n+extra();\n";
        let files = parse_patch(patch, DiffSource::Explicit).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/main.zig");
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(files[0].hunks[0].lines.len(), 4);
        assert_eq!(files[0].hunks[0].lines[2].kind, DiffLineKind::Add);
        let file = parse_patch(b"diff --git a/notes.txt b/notes.txt\n--- a/notes.txt\n+++ b/notes.txt\n@@ -1,2 +1,2 @@\n keep\n--- deleted prefix\n+++ added prefix\n", DiffSource::Explicit).unwrap().remove(0);
        assert_eq!(file.path, "notes.txt");
        assert_eq!(file.hunks[0].lines[1].text, "-- deleted prefix");
        assert_eq!(file.hunks[0].lines[2].text, "++ added prefix");
    }
    #[test]
    fn malformed_later_header_fails_and_counters_saturate() {
        assert!(matches!(
            parse_patch(
                b"diff --git a/x b/x\n@@ -1,1 +1,1 @@\n a\n@@ bogus header @@\n",
                DiffSource::Explicit
            ),
            Err(Error::ParseFailed)
        ));
        let file = parse_patch(
            b"diff --git a/x b/x\n@@ -4294967295,2 +4294967295,2 @@\n a\n b\n",
            DiffSource::Explicit,
        )
        .unwrap()
        .remove(0);
        assert_eq!(file.hunks[0].lines[1].old_lineno, Some(u32::MAX));
    }
    #[test]
    fn raw_patch_and_anchor_hashes_preserve_invalid_utf8_and_crlf() {
        let patch = b"\ndiff --git a/x b/x\r\n@@ -0,0 +1,1 @@\r\n+\xff\r\n\\ No newline at end of file\r\n\n";
        let file = parse_patch(patch, DiffSource::Untracked).unwrap().remove(0);
        assert_eq!(file.patch_text, patch[1..patch.len() - 2]);
        assert_eq!(
            file.patch_fingerprint,
            format!("sha256:{}", util::hash_hex(&patch[1..patch.len() - 2]))
        );
        assert_eq!(
            file.hunks[0].lines[0].stable_line_id,
            format!("line_{}", &util::hash_hex(b"x:add:null:1:\xff")[..16])
        );
        assert_eq!(
            file.hunks[0].lines[1].stable_line_id,
            format!(
                "line_{}",
                &util::hash_hex(b"x:meta:null:null:\\ No newline at end of file")[..16]
            )
        );
    }
    #[test]
    fn multi_file_status_and_raw_paths() {
        let files = parse_patch(b"preamble\ndiff --git a/old b/new\nrename from old\nrename to new\ndiff --git a/a b/b\ncopy from a\ncopy to b\ndiff --git a/bin b/bin\nGIT binary patch\ndiff --git a/\xff b/\xff\n@@ -0,0 +1 @@\n+hi\n", DiffSource::Explicit).unwrap();
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].status, FileStatus::Renamed);
        assert_eq!(files[1].status, FileStatus::Copied);
        assert!(files[2].is_binary);
        assert_eq!(raw_file_paths(&files[3]).0, b"\xff");
        assert_eq!(
            files[3].hunks[0].lines[0].stable_line_id,
            format!("line_{}", &util::hash_hex(b"\xff:add:null:1:hi")[..16])
        );
    }
    #[test]
    fn target_kind_order_and_identity_match_legacy() {
        for (args, kind) in [
            (vec![], ReviewTargetKind::WorkingTree),
            (vec!["--cached"], ReviewTargetKind::Cached),
            (vec!["--staged"], ReviewTargetKind::Cached),
            (vec!["HEAD"], ReviewTargetKind::Commit),
            (vec!["main..feature"], ReviewTargetKind::Range),
            (
                vec!["main...feature", "--", "src"],
                ReviewTargetKind::SymmetricRange,
            ),
            (vec!["main..feature", "--cached"], ReviewTargetKind::Range),
        ] {
            let args: Vec<_> = args.into_iter().map(String::from).collect();
            let target = make_review_target(&args);
            assert_eq!(target.kind, kind);
            assert_eq!(
                target.target_id,
                format!(
                    "target_{}",
                    &util::hash_hex(
                        format!("{}:{}", kind.label(), util::join_args(&args)).as_bytes()
                    )[..16]
                )
            );
        }
    }
}
