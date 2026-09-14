use diffo::diff::{self, DiffFile, DiffSnapshot, DiffSource, FileStatus};
use diffo::git::{self, FileSide};
use diffo::{Error, util};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

struct Fixture {
    directory: TempDir,
}
impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            directory: tempfile::tempdir().unwrap(),
        };
        fixture.git(&["init", "-q", "-b", "main"]);
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
            ("color.ui", "always"),
            ("color.diff", "always"),
        ] {
            fixture.git(&["config", key, value]);
        }
        fixture
    }
    fn root(&self) -> &Path {
        self.directory.path()
    }
    fn git(&self, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.root())
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    fn write(&self, path: &str, content: impl AsRef<[u8]>) {
        fs::write(self.root().join(path), content).unwrap();
    }
    fn commit(&self) {
        self.git(&["add", "."]);
        self.git(&["commit", "-qm", "fixture"]);
    }
    fn snapshot(&self, args: &[&str]) -> DiffSnapshot {
        let repo = git::discover_repository_at(self.root(), false).unwrap();
        let args: Vec<_> = args.iter().map(|arg| (*arg).to_owned()).collect();
        git::load_snapshot(repo, diff::make_review_target(&args), false).unwrap()
    }
}
fn file<'a>(snapshot: &'a DiffSnapshot, path: &str) -> &'a DiffFile {
    snapshot
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("missing {path}: {:?}", snapshot.files))
}
fn side(snapshot: &DiffSnapshot, path: &str, side: FileSide) -> Option<Vec<u8>> {
    git::load_file_side(
        &snapshot.repository,
        &snapshot.review_target,
        file(snapshot, path),
        side,
        false,
    )
    .unwrap()
}

#[test]
fn working_tree_merges_in_order_and_preserves_raw_git_fingerprints() {
    let f = Fixture::new();
    for name in ["both", "staged", "unstaged"] {
        f.write(name, "head\n");
    }
    f.commit();
    f.write("both", "index\n");
    f.write("staged", "index\n");
    f.git(&["add", "."]);
    f.write("both", "worktree\n");
    f.write("unstaged", "worktree\n");
    f.write("untracked", "one\r\n\nlast");
    f.write("empty", "");
    f.write("binary", b"a\0b");
    let snapshot = f.snapshot(&[]);
    assert_eq!(
        snapshot
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["both", "unstaged", "staged", "binary", "empty", "untracked"]
    );
    let both = file(&snapshot, "both");
    assert_eq!(both.source, DiffSource::Explicit);
    assert_eq!(both.hunks.len(), 2);
    assert_eq!(both.hunks[0].lines[1].text, "worktree");
    assert_eq!(both.hunks[1].lines[1].text, "index");
    let raw_unstaged = f.git(&[
        "diff",
        "--patch",
        "--find-renames",
        "--no-ext-diff",
        "--unified=1000000",
        "--no-color",
    ]);
    let raw_staged = f.git(&[
        "diff",
        "--cached",
        "--patch",
        "--find-renames",
        "--no-ext-diff",
        "--unified=1000000",
        "--no-color",
    ]);
    let unstaged = diff::parse_patch(&raw_unstaged, DiffSource::Unstaged).unwrap();
    let staged = diff::parse_patch(&raw_staged, DiffSource::Staged).unwrap();
    let merged = [
        unstaged[0].patch_text.as_slice(),
        b"\n",
        staged[0].patch_text.as_slice(),
    ]
    .concat();
    assert_eq!(both.patch_text, merged);
    assert_eq!(
        both.patch_fingerprint,
        format!("sha256:{}", util::hash_hex(&merged))
    );
    assert!(!both.patch_text.contains(&0x1b));
    assert_eq!(side(&snapshot, "both", FileSide::Old), None);
    assert_eq!(side(&snapshot, "both", FileSide::New), None);
    assert_eq!(
        side(&snapshot, "unstaged", FileSide::Old).unwrap(),
        b"head\n"
    );
    assert_eq!(
        side(&snapshot, "unstaged", FileSide::New).unwrap(),
        b"worktree\n"
    );
    assert_eq!(side(&snapshot, "staged", FileSide::Old).unwrap(), b"head\n");
    assert_eq!(
        side(&snapshot, "staged", FileSide::New).unwrap(),
        b"index\n"
    );
    assert_eq!(side(&snapshot, "untracked", FileSide::Old), None);
    assert_eq!(
        side(&snapshot, "untracked", FileSide::New).unwrap(),
        b"one\r\n\nlast"
    );
    assert_eq!(file(&snapshot, "untracked").hunks[0].lines.len(), 3);
    assert_eq!(file(&snapshot, "empty").status, FileStatus::Added);
    assert!(file(&snapshot, "empty").hunks.is_empty());
    assert!(file(&snapshot, "binary").is_binary);
    assert_eq!(side(&snapshot, "binary", FileSide::New), None);
    let mut identity = snapshot.repository.repo_id.clone() + &snapshot.review_target.target_id;
    for file in &snapshot.files {
        identity += &file.path;
        identity += &file.patch_fingerprint;
    }
    assert_eq!(
        snapshot.snapshot_id,
        format!("snap_{}", &util::hash_hex(identity.as_bytes())[..16])
    );
}

#[test]
fn explicit_targets_and_merge_base_load_correct_source_sides() {
    let f = Fixture::new();
    f.write("x.rs", "base\n");
    f.commit();
    f.git(&["tag", "base"]);
    f.git(&["checkout", "-qb", "feature"]);
    f.write("x.rs", "feature\n");
    f.commit();
    f.git(&["checkout", "-q", "main"]);
    f.write("x.rs", "main\n");
    f.commit();
    f.write("x.rs", "index\n");
    f.git(&["add", "."]);
    f.write("x.rs", "worktree\n");
    for (args, old, new) in [
        (vec!["--cached"], "main\n", "index\n"),
        (vec!["--staged"], "main\n", "index\n"),
        (vec!["base"], "base\n", "worktree\n"),
        (vec!["base", "feature"], "base\n", "feature\n"),
        (vec!["main..feature"], "main\n", "feature\n"),
        (vec!["main...feature"], "base\n", "feature\n"),
        (vec!["..feature"], "main\n", "feature\n"),
        (vec!["base.."], "base\n", "main\n"),
        (vec!["...feature"], "base\n", "feature\n"),
        (vec!["feature..."], "base\n", "main\n"),
        (vec!["--", "x.rs"], "index\n", "worktree\n"),
        (vec!["x.rs"], "index\n", "worktree\n"),
        (vec!["--color=always", "x.rs"], "index\n", "worktree\n"),
        (vec!["base", "x.rs"], "base\n", "worktree\n"),
        (
            vec!["--color=always", "--", "x.rs"],
            "index\n",
            "worktree\n",
        ),
    ] {
        let snapshot = f.snapshot(&args);
        assert_eq!(snapshot.files.len(), 1, "{args:?}");
        assert_eq!(
            side(&snapshot, "x.rs", FileSide::Old).unwrap(),
            old.as_bytes(),
            "{args:?}"
        );
        assert_eq!(
            side(&snapshot, "x.rs", FileSide::New).unwrap(),
            new.as_bytes(),
            "{args:?}"
        );
        assert!(!file(&snapshot, "x.rs").patch_text.contains(&0x1b));
    }
    assert!(f.snapshot(&["base", "--", "missing"]).files.is_empty());
}

#[test]
fn added_deleted_renamed_copied_and_binary_files() {
    let f = Fixture::new();
    f.write("old", "rename content\n");
    f.write("original", "copy content\n");
    f.write("delete", "deleted forever\n");
    f.write("binary", b"old\0bytes");
    f.commit();
    f.git(&["tag", "base"]);
    f.git(&["mv", "old", "renamed"]);
    f.git(&["rm", "-q", "delete"]);
    f.write("copy", "copy content\n");
    f.write("added", "entirely new\n");
    f.write("binary", b"new\0bytes");
    f.commit();
    let snapshot = f.snapshot(&["base", "HEAD", "--find-copies", "--find-copies-harder"]);
    assert_eq!(file(&snapshot, "added").status, FileStatus::Added);
    assert_eq!(file(&snapshot, "delete").status, FileStatus::Deleted);
    assert!(file(&snapshot, "binary").is_binary);
    for (name, old_name, status, content) in [
        ("copy", "original", FileStatus::Copied, "copy content\n"),
        ("renamed", "old", FileStatus::Renamed, "rename content\n"),
    ] {
        let changed = file(&snapshot, name);
        assert_eq!(changed.status, status);
        assert_eq!(changed.old_path.as_deref(), Some(old_name));
        assert_eq!(
            side(&snapshot, name, FileSide::Old).unwrap(),
            content.as_bytes()
        );
        assert_eq!(
            side(&snapshot, name, FileSide::New).unwrap(),
            content.as_bytes()
        );
    }
    assert_eq!(side(&snapshot, "added", FileSide::Old), None);
    assert_eq!(side(&snapshot, "delete", FileSide::New), None);
}

#[test]
fn unborn_repository_and_non_repository_errors() {
    let outside = tempfile::tempdir().unwrap();
    assert!(matches!(
        git::discover_repository_at(outside.path(), false),
        Err(Error::NotGitRepository)
    ));
    let f = Fixture::new();
    f.write("new", "hello\n");
    let snapshot = f.snapshot(&[]);
    assert_eq!(snapshot.repository.current_branch, "HEAD");
    assert_eq!(file(&snapshot, "new").source, DiffSource::Untracked);
    f.git(&["add", "."]);
    let snapshot = f.snapshot(&["--cached"]);
    assert_eq!(side(&snapshot, "new", FileSide::Old), None);
    assert_eq!(side(&snapshot, "new", FileSide::New).unwrap(), b"hello\n");
    let target = diff::make_review_target(&["nonexistent-revision".into()]);
    assert!(matches!(
        git::load_snapshot(snapshot.repository, target, false),
        Err(Error::GitCommandFailed)
    ));
}

#[test]
fn source_and_untracked_size_limits_are_preserved() {
    let f = Fixture::new();
    f.git(&["commit", "-q", "--allow-empty", "-m", "empty"]);
    let huge = fs::File::create(f.root().join("huge")).unwrap();
    huge.set_len(20 * 1024 * 1024 + 1).unwrap();
    f.write("source", vec![b'x'; 4 * 1024 * 1024 + 1]);
    let snapshot = f.snapshot(&[]);
    assert!(file(&snapshot, "huge").is_binary);
    assert!(!file(&snapshot, "source").is_binary);
    assert_eq!(side(&snapshot, "source", FileSide::New), None);
}

#[cfg(unix)]
#[test]
fn symlinks_use_realpath_identity_and_broken_entries_are_skipped() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.git(&["commit", "-q", "--allow-empty", "-m", "empty"]);
    let links = tempfile::tempdir().unwrap();
    symlink(f.root(), links.path().join("repo")).unwrap();
    let actual = git::discover_repository_at(f.root(), false).unwrap();
    let linked = git::discover_repository_at(&links.path().join("repo"), false).unwrap();
    assert_eq!(actual.repo_id, linked.repo_id);
    assert_eq!(
        actual.repo_id,
        format!(
            "repo_{}",
            &util::hash_hex(f.root().canonicalize().unwrap().as_os_str().as_bytes())[..16]
        )
    );
    symlink("nonexistent", f.root().join("broken")).unwrap();
    assert!(f.snapshot(&[]).files.is_empty());
}

// APFS rejects invalid UTF-8 names; exercise actual raw-name filesystem I/O on Linux.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_untracked_names_keep_distinct_identities() {
    use std::os::unix::ffi::OsStrExt;
    let f = Fixture::new();
    f.git(&["commit", "-q", "--allow-empty", "-m", "empty"]);
    for byte in [0xfe, 0xff] {
        fs::write(
            f.root().join(std::ffi::OsStr::from_bytes(&[byte])),
            b"\xff\n",
        )
        .unwrap();
    }
    let snapshot = f.snapshot(&[]);
    assert_eq!(snapshot.files.len(), 2);
    assert_eq!(snapshot.files[0].path, snapshot.files[1].path);
    assert_ne!(
        snapshot.files[0].patch_fingerprint,
        snapshot.files[1].patch_fingerprint
    );
    assert_ne!(
        snapshot.files[0].hunks[0].lines[0].stable_line_id,
        snapshot.files[1].hunks[0].lines[0].stable_line_id
    );
    let mut identity =
        (snapshot.repository.repo_id.clone() + &snapshot.review_target.target_id).into_bytes();
    for (file, byte) in snapshot.files.iter().zip([0xfe, 0xff]) {
        assert_eq!(
            file.hunks[0].lines[0].stable_line_id,
            format!(
                "line_{}",
                &util::hash_hex(&[&[byte][..], b":add:null:1:\xff"].concat())[..16]
            )
        );
        assert_eq!(
            git::load_file_side(
                &snapshot.repository,
                &snapshot.review_target,
                file,
                FileSide::New,
                false
            )
            .unwrap()
            .unwrap(),
            b"\xff\n"
        );
        identity.push(byte);
        identity.extend_from_slice(file.patch_fingerprint.as_bytes());
    }
    assert_eq!(
        snapshot.snapshot_id,
        format!("snap_{}", &util::hash_hex(&identity)[..16])
    );
}
