use diffo::Error;
use diffo::diff::{self, DiffFile, DiffLine, DiffLineKind, DiffSnapshot, DiffSource, Repository};
use diffo::store::{MatchStatus, Store};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    repo: PathBuf,
    state: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let state = temp.path().join("state");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(&state).unwrap();
        let f = Self { temp, repo, state };
        f.git(&["init", "-q"]);
        f.git(&["config", "user.name", "Test"]);
        f.git(&["config", "user.email", "test@example.invalid"]);
        fs::write(f.repo.join("a.txt"), "old\ncontext\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "initial"]);
        fs::write(f.repo.join("a.txt"), "new\ncontext\n").unwrap();
        f
    }
    fn command(&self, binary: impl AsRef<Path>) -> Command {
        let mut c = Command::new(binary.as_ref());
        // Do not inherit object/index paths, hooks, tracing, or injected Git config.
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                c.env_remove(key);
            }
        }
        c.current_dir(&self.repo)
            .env("XDG_STATE_HOME", &self.state)
            .env("HOME", self.temp.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("no-git-config"))
            .env("GIT_AUTHOR_NAME", "Test Author")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .env("NO_COLOR", "1");
        c
    }
    fn git(&self, args: &[&str]) {
        let out = self.command("git").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_diffo"))
            .args(args)
            .output()
            .unwrap()
    }
    fn text(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.text(args)).unwrap()
    }
    fn store_dir(&self) -> PathBuf {
        let status = self.json(&["review", "status", "--json"]);
        self.state
            .join("diffo/repos")
            .join(status["repository_id"].as_str().unwrap())
    }
    fn add(&self) -> String {
        self.text(&[
            "comments",
            "add",
            "--file",
            "a.txt",
            "--line",
            "1",
            "--end",
            "2",
            "--body",
            "Unicode 文本\nquote \" slash \\ tab\t",
        ])
    }
}

#[test]
fn cli_comment_review_and_cleanup_lifecycle() {
    let f = Fixture::new();
    assert_eq!(f.text(&["comments", "list"]), "no comments\n");
    let initial = f.json(&["review", "status", "--json"]);
    assert_eq!(initial["schema_version"], 1);
    assert_eq!(initial["files"][0]["status"], "unreviewed");
    let added = f.add();
    assert!(added.contains("a.txt:1-2 [exact] Test Author\n"));
    let list = f.json(&["comments", "list", "--json"]);
    assert_eq!(list["comments"].as_array().unwrap().len(), 1);
    let c = &list["comments"][0];
    assert_eq!(c["side"], "old");
    assert_eq!(c["body"], "Unicode 文本\nquote \" slash \\ tab\t");
    assert_eq!(list["review_target_id"], c["review_target_id"]);
    let id = c["comment_id"].as_str().unwrap();
    assert_eq!(f.text(&["comments", "get", id]), added);
    assert_eq!(f.json(&["comments", "get", id, "ignored", "--json"]), *c);
    assert_eq!(
        f.text(&["comments", "list", "--file", "absent"]),
        "no comments\n"
    );
    assert!(
        f.text(&["review", "mark", "--file", "a.txt"])
            .starts_with("reviewed a.txt comments=1 fingerprint=")
    );
    assert_eq!(
        f.json(&["review", "status", "--json"])["files"][0]["status"],
        "reviewed"
    );
    assert!(
        f.text(&["review", "mark", "--file", "a.txt", "--unreviewed"])
            .starts_with("unreviewed")
    );
    f.text(&["review", "mark", "--file", "a.txt"]);
    fs::write(f.repo.join("a.txt"), "newer\ncontext\n").unwrap();
    assert_eq!(
        f.json(&["comments", "list", "--json"])["comments"][0]["match_status"],
        "stale"
    );
    assert_eq!(
        f.json(&["review", "status", "--json"])["files"][0]["status"],
        "unreviewed"
    );
    let path = f.store_dir().join("comments.json");
    let before = fs::read(&path).unwrap();
    let dry = f.json(&["comments", "cleanup", "--dry-run", "--json"]);
    assert_eq!(dry["removed_count"], 1);
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["all"], false);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        f.text(&["comments", "clean"]),
        "removed 1 outdated comments\n"
    );
    assert_eq!(f.text(&["comments", "list"]), "no comments\n");
}

#[test]
fn cli_lists_all_targets_but_default_cleanup_only_removes_current_target() {
    let f = Fixture::new();
    f.add();
    let path = f.store_dir().join("comments.json");
    let mut data: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut other = data["comments"][0].clone();
    other["comment_id"] = json!("other-target");
    other["review_target_id"] = json!("target_other");
    other["anchor"]["match_status"] = json!("stale");
    data["comments"].as_array_mut().unwrap().push(other);
    fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    let list = f.json(&["comments", "list", "--json"]);
    assert_eq!(list["comments"].as_array().unwrap().len(), 2);
    assert_eq!(
        f.json(&["review", "status", "--json"])["files"][0]["comment_count"],
        1
    );
    assert_eq!(
        f.text(&["comments", "clean"]),
        "removed 0 outdated comments\n"
    );
    assert_eq!(
        f.text(&["comments", "clean", "--all", "--dry-run"]),
        "would remove 2 comments\n"
    );
    assert_eq!(
        f.json(&["comments", "clean", "--all", "--file", "a.txt", "--json"])["removed_count"],
        2
    );
    assert_eq!(f.text(&["comments", "list"]), "no comments\n");
}

#[test]
fn cli_missing_comments_and_empty_review() {
    let f = Fixture::new();
    f.add();
    f.git(&["checkout", "--", "a.txt"]);
    assert_eq!(f.text(&["review", "status"]), "no changed files\n");
    assert_eq!(f.json(&["review", "status", "--json"])["files"], json!([]));
    assert_eq!(
        f.json(&["comments", "list", "--json"])["comments"][0]["match_status"],
        "missing"
    );
    assert_eq!(
        f.text(&["comments", "clean", "--dry-run"]),
        "would remove 1 outdated comments\n"
    );
}

#[test]
fn cli_argument_errors_and_help() {
    let f = Fixture::new();
    let help = f.text(&["--help"]);
    assert!(help.starts_with("diffo - terminal Git diff review\n\nUsage:\n"));
    assert!(help.contains("diffo comments clean [--all]"));
    assert_eq!(help, f.text(&["--debug-git", "-h"]));
    for args in [
        vec!["comments"],
        vec!["comments", "list", "--bad"],
        vec!["comments", "list", "--file"],
        vec!["comments", "get", "absent"],
        vec![
            "comments",
            "add",
            "--file",
            "a.txt",
            "--line",
            "4294967296",
            "--body",
            "x",
        ],
        vec![
            "comments", "add", "--file", "a.txt", "--line", "-1", "--body", "x",
        ],
        vec![
            "comments", "add", "--file", "a.txt", "--line", "999", "--body", "x",
        ],
        vec!["review"],
        vec!["review", "mark"],
        vec!["review", "mark", "--file", "absent"],
        vec!["review", "status", "--reviewed"],
        vec!["themes", "validate"],
        vec!["themes", "unknown"],
    ] {
        let out = f.run(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(
            String::from_utf8(out.stderr).unwrap(),
            "diffo: invalid arguments; use diffo --help\n",
            "{args:?}"
        );
    }
}

#[test]
fn cli_not_git_and_corruption_do_not_overwrite_data() {
    let f = Fixture::new();
    let out = f
        .command(env!("CARGO_BIN_EXE_diffo"))
        .current_dir(f.temp.path())
        .args(["comments", "list"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        out.stderr,
        b"diffo: current directory is not inside a Git repository\n"
    );
    let path = f.store_dir().join("comments.json");
    fs::write(&path, b"{broken").unwrap();
    let out = f.run(&["comments", "clean", "--all"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stderr, b"diffo: stored review data is corrupted\n");
    assert_eq!(fs::read(&path).unwrap(), b"{broken");
}

fn sample_file() -> DiffFile {
    diff::parse_patch(
        b"diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        DiffSource::Explicit,
    )
    .unwrap()
    .remove(0)
}
fn sample_line(file: &DiffFile) -> &DiffLine {
    file.hunks[0]
        .lines
        .iter()
        .find(|l| l.kind == DiffLineKind::Add)
        .unwrap()
}
fn add(store: &mut Store, target: &str, file: &DiffFile) {
    store
        .add_comment(
            "repo",
            target,
            file,
            sample_line(file),
            &file.hunks[0].header,
            0,
            "body",
            "tester",
        )
        .unwrap();
}

#[test]
fn store_schema_v1_roundtrip_preserves_extra_anchor_data() {
    let dir = tempfile::tempdir().unwrap();
    let comments = json!({"schema_version": 1, "extension": {"keep": true}, "comments": [{"comment_id":"cmt_v1","repository_id":"repo","review_target_id":"target","file_path":"a.txt","anchor":{"side":"new","start_line":1,"end_line":2,"stable_line_ids":["line_one","line_two"],"hunk_header":"@@ -1 +1 @@","context_before":["before 文本"],"context_after":["after"],"patch_fingerprint":"sha256:old","match_status":"relocated","extension":42},"body":"body\n\"text\"","author":"tester","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","extension":"keep"}]});
    let states = json!({"schema_version":1,"extension":true,"states":[{"repository_id":"repo","review_target_id":"target","file_path":"a.txt","status":"reviewed","patch_fingerprint":"sha256:old","updated_at":"now","extension":42}]});
    fs::write(
        dir.path().join("comments.json"),
        serde_json::to_vec(&comments).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("review-states.json"),
        serde_json::to_vec(&states).unwrap(),
    )
    .unwrap();
    let store = Store::at(dir.path()).unwrap();
    assert_eq!(store.comments[0].match_status, MatchStatus::Relocated);
    assert_eq!(store.comments[0].context_before, ["before 文本"]);
    assert_eq!(store.comments[0].stable_line_id, "line_one");
    store.save().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(dir.path().join("comments.json")).unwrap())
            .unwrap(),
        comments
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(dir.path().join("review-states.json")).unwrap())
            .unwrap(),
        states
    );
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn store_mutations_preserve_memory_and_disk_on_write_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::at(dir.path()).unwrap();
    let file = sample_file();
    add(&mut store, "target", &file);
    store.set_reviewed("repo", "target", &file, true).unwrap();
    let comments_before = fs::read(dir.path().join("comments.json")).unwrap();
    let states_before = fs::read(dir.path().join("review-states.json")).unwrap();
    // A non-directory destination deterministically fails even when running as root.
    let obstruction = dir.path().join("not-a-directory");
    fs::write(&obstruction, "obstruction").unwrap();
    store.repo_dir = obstruction;
    assert!(matches!(
        store.remove_all_comments(None),
        Err(Error::StorageWriteFailed)
    ));
    assert_eq!(store.comments.len(), 1);
    assert!(matches!(
        store.add_comment(
            "repo",
            "target",
            &file,
            sample_line(&file),
            "header",
            0,
            "second",
            "tester"
        ),
        Err(Error::StorageWriteFailed)
    ));
    assert_eq!(store.comments.len(), 1);
    assert!(matches!(
        store.set_reviewed("repo", "target", &file, false),
        Err(Error::StorageWriteFailed)
    ));
    assert!(store.is_reviewed(&file.path, &file.patch_fingerprint, "target"));
    assert_eq!(
        fs::read(dir.path().join("comments.json")).unwrap(),
        comments_before
    );
    assert_eq!(
        fs::read(dir.path().join("review-states.json")).unwrap(),
        states_before
    );
}

#[test]
fn store_persist_failure_cleans_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::at(dir.path()).unwrap();
    fs::create_dir(dir.path().join("comments.json")).unwrap();
    fs::write(dir.path().join("comments.json/keep"), "keep").unwrap();
    let file = sample_file();
    assert!(matches!(
        store.add_comment(
            "repo",
            "target",
            &file,
            sample_line(&file),
            "header",
            0,
            "body",
            "tester"
        ),
        Err(Error::StorageWriteFailed)
    ));
    assert!(store.comments.is_empty());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    assert_eq!(
        fs::read(dir.path().join("comments.json/keep")).unwrap(),
        b"keep"
    );
}

#[test]
fn store_refresh_and_removal_respect_target_file_and_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::at(dir.path()).unwrap();
    let mut file = sample_file();
    add(&mut store, "target_a", &file);
    add(&mut store, "target_b", &file);
    let mut other = file.clone();
    other.path = "b.txt".into();
    add(&mut store, "target_a", &other);
    store.set_reviewed("repo", "target_a", &file, true).unwrap();
    assert!(store.is_reviewed("a.txt", &file.patch_fingerprint, "target_a"));
    assert!(!store.is_reviewed("a.txt", "different", "target_a"));
    assert!(!store.is_reviewed("a.txt", &file.patch_fingerprint, "target_b"));
    file.patch_fingerprint = "changed".into();
    let mut target = diff::make_review_target(&[]);
    target.target_id = "target_a".into();
    let snapshot = DiffSnapshot {
        snapshot_id: "snapshot".into(),
        repository: Repository {
            root_path: dir.path().into(),
            repo_id: "repo".into(),
            current_branch: "main".into(),
        },
        review_target: target,
        files: vec![file],
    };
    store.refresh_match_status(&snapshot);
    assert_eq!(store.comments[0].match_status, MatchStatus::Stale);
    assert_eq!(store.comments[1].match_status, MatchStatus::Exact);
    assert_eq!(store.comments[2].match_status, MatchStatus::Missing);
    assert_eq!(store.comment_count("a.txt", "target_a"), 1);
    assert_eq!(store.comment_count("a.txt", "target_b"), 1);
    assert_eq!(store.outdated_comment_count("target_a", Some("a.txt")), 1);
    assert_eq!(
        store
            .remove_outdated_comments("target_a", Some("a.txt"))
            .unwrap(),
        1
    );
    assert_eq!(store.comments.len(), 2);
    assert_eq!(store.remove_all_comments(Some("a.txt")).unwrap(), 1);
    assert_eq!(store.comments[0].file_path, "b.txt");
    assert_eq!(store.remove_all_comments(None).unwrap(), 1);
    assert!(Store::at(dir.path()).unwrap().comments.is_empty());
}

#[test]
fn store_invalid_documents_fail_closed_and_numeric_fields_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("comments.json");
    for bytes in [
        b"not json".as_slice(),
        b"[]",
        b"{\"comments\":{}}",
        b"{\"schema_version\":2,\"comments\":[]}",
        b"{\"comments\":[1]}",
    ] {
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            Store::at(dir.path()),
            Err(Error::StorageCorrupted)
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    fs::write(&path, br#"{"schema_version":1,"comments":[{"anchor":{"start_line":-1,"end_line":4294967296}},{"anchor":{"start_line":1.9,"end_line":4294967295}}]}"#).unwrap();
    let store = Store::at(dir.path()).unwrap();
    assert_eq!(
        (store.comments[0].start_line, store.comments[0].end_line),
        (0, 0)
    );
    assert_eq!(
        (store.comments[1].start_line, store.comments[1].end_line),
        (1, u32::MAX)
    );
}

#[test]
fn cli_home_and_relative_xdg_paths_are_isolated() {
    let f = Fixture::new();
    let run = |relative: bool| {
        let mut cmd = f.command(env!("CARGO_BIN_EXE_diffo"));
        if relative {
            cmd.env("XDG_STATE_HOME", "relative-state");
        } else {
            cmd.env_remove("XDG_STATE_HOME");
        }
        let out = cmd.args(["comments", "list", "--json"]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["repository_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let repo_id = run(false);
    assert!(
        f.temp
            .path()
            .join(".local/state/diffo/repos")
            .join(&repo_id)
            .is_dir()
    );
    run(true);
    assert!(
        f.repo
            .join("relative-state/diffo/repos")
            .join(repo_id)
            .is_dir()
    );
}

#[cfg(unix)]
#[test]
fn cli_broken_pipe_exits_successfully() {
    use std::process::Stdio;
    let f = Fixture::new();
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let out = f
        .command(env!("CARGO_BIN_EXE_diffo"))
        .arg("--help")
        .stdout(Stdio::from(writer))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn cli_themes_commands_preserve_output() {
    let f = Fixture::new();
    assert_eq!(
        f.text(&["themes", "list", "ignored"]),
        "catppuccin-mocha  built-in default\nbase16/base24     supported via diffo themes validate <file>\n"
    );
    let path = f.temp.path().join("theme.yaml");
    for count in [16, 24] {
        let content: String = (0..count)
            .map(|i| format!("base{i:02x}: \"abcdef\"\n"))
            .collect();
        fs::write(&path, content).unwrap();
        assert_eq!(
            f.text(&["themes", "validate", path.to_str().unwrap()]),
            format!("valid Base{count} theme ({count} color slots)\n")
        );
    }
    fs::write(&path, "not a theme").unwrap();
    let out = f.run(&["themes", "validate", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        out.stderr,
        b"diffo: theme file does not look like Base16/Base24\n"
    );
}

#[test]
fn cli_author_precedence_and_zero_end_line() {
    let f = Fixture::new();
    for (author, user, expected) in [
        (Some("Author"), Some("User"), "Author"),
        (None, Some("User"), "User"),
        (None, None, "local"),
        (Some(""), Some("User"), ""),
    ] {
        let mut cmd = f.command(env!("CARGO_BIN_EXE_diffo"));
        cmd.env_remove("GIT_AUTHOR_NAME").env_remove("USER");
        if let Some(author) = author {
            cmd.env("GIT_AUTHOR_NAME", author);
        }
        if let Some(user) = user {
            cmd.env("USER", user);
        }
        let out = cmd
            .args([
                "comments", "add", "--file", "a.txt", "--line", "+1", "--end", "-0", "--body",
                expected,
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8(out.stdout)
                .unwrap()
                .contains(&format!("a.txt:1-1 [exact] {expected}\n"))
        );
    }
}
