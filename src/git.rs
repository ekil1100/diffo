use crate::diff::{
    self, DiffFile, DiffSnapshot, DiffSource, Repository, ReviewTarget, ReviewTargetKind,
};
use crate::{Error, Result, util};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FULL_CONTEXT: &str = "--unified=1000000";
const MAX_DIFF_BYTES: usize = 200 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 20 * 1024 * 1024;
const MAX_UNTRACKED_BYTES: usize = 20 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileSide {
    Old,
    New,
}

#[cfg(unix)]
fn os_bytes(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(bytes).to_owned()
}
#[cfg(not(unix))]
fn os_bytes(bytes: &[u8]) -> OsString {
    String::from_utf8_lossy(bytes).into_owned().into()
}

#[derive(Clone, Copy)]
struct GitRunner<'a> {
    root: &'a Path,
    debug: bool,
}
impl GitRunner<'_> {
    fn run(self, args: &[&str]) -> Result<Vec<u8>> {
        self.run_os(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }
    fn run_os(self, args: &[OsString]) -> Result<Vec<u8>> {
        let mut command = Command::new("git");
        command.arg("-C").arg(self.root);
        command.args(args);
        run_command(command, self.debug, MAX_DIFF_BYTES, MAX_STDERR_BYTES)
    }
}

// Retain at most each limit plus one byte, including for untracked/source files.
fn read_bounded(reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}
struct CommandSignals {
    received: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(unix)]
    registrations: Vec<signal_hook::SigId>,
}
impl CommandSignals {
    fn new() -> io::Result<Self> {
        #[allow(unused_mut)]
        let mut signals = Self {
            received: Default::default(),
            #[cfg(unix)]
            registrations: Vec::new(),
        };
        #[cfg(unix)]
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            signals
                .registrations
                .push(signal_hook::flag::register_usize(
                    signal,
                    std::sync::Arc::clone(&signals.received),
                    signal as usize,
                )?);
        }
        Ok(signals)
    }
    fn received(&self) -> usize {
        self.received.load(std::sync::atomic::Ordering::Relaxed)
    }
}
impl Drop for CommandSignals {
    fn drop(&mut self) {
        #[cfg(unix)]
        for id in self.registrations.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

fn run_command(
    mut command: Command,
    debug: bool,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut command = process_wrap::std::CommandWrap::from(command);
    // Textconv helpers can retain Git's pipes after Git exits. Terminate the
    // whole subprocess tree on a limit/read error, before waiting for either pipe.
    #[cfg(unix)]
    command.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(process_wrap::std::JobObject);
    // A new process group does not receive the parent's terminal signals.
    // Observe cancellation here so loading a snapshot remains interruptible too.
    let signals = CommandSignals::new()?;
    let mut child = command.spawn().map_err(|_| Error::GitCommandFailed)?;
    let stdout = child.stdout().take().expect("piped stdout");
    let stderr = child.stderr().take().expect("piped stderr");
    let mut output = [Vec::new(), Vec::new()];
    let mut failure = None;
    std::thread::scope(|scope| {
        let (sender, receiver) = std::sync::mpsc::channel();
        let out_sender = sender.clone();
        scope.spawn(move || {
            let result = read_bounded(stdout, stdout_limit);
            let _ = out_sender.send((0, stdout_limit, result));
        });
        scope.spawn(move || {
            let result = read_bounded(stderr, stderr_limit);
            let _ = sender.send((1, stderr_limit, result));
        });
        let mut completed = 0;
        while completed < 2 {
            if failure.is_none() && signals.received() != 0 {
                failure = Some(Error::Interrupted(signals.received() as i32));
                let _ = child.start_kill();
            }
            let (index, limit, result) =
                match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(result) => result,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                };
            completed += 1;
            let error = match result {
                Ok(bytes) if bytes.len() > limit => Some(Error::DiffTooLarge),
                Ok(bytes) => {
                    output[index] = bytes;
                    None
                }
                Err(_) => Some(Error::GitCommandFailed),
            };
            if failure.is_none() && error.is_some() {
                failure = error;
                // An already-exited process may report ESRCH; its pipes are
                // closed in that case. Always reap it below, even on failure.
                let _ = child.start_kill();
            }
        }
    });
    let status = child.wait();
    if let Some(error) = failure {
        return Err(error);
    }
    let status = status.map_err(|_| Error::GitCommandFailed)?;
    let [out, err] = output;
    if !status.success() {
        if debug {
            let _ = io::stderr().write_all(&err);
        }
        return Err(Error::GitCommandFailed);
    }
    Ok(out)
}
fn trim_newlines(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|&b| b != b'\r' && b != b'\n')
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|&b| b != b'\r' && b != b'\n')
        .map_or(start, |i| i + 1);
    &bytes[start..end]
}

pub fn discover_repository(debug: bool) -> Result<Repository> {
    discover_repository_at(Path::new("."), debug)
}

// Also useful to callers that must not change the process-wide current directory.
pub fn discover_repository_at(directory: &Path, debug: bool) -> Result<Repository> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(directory)
        .args(["rev-parse", "--show-toplevel"]);
    let root = run_command(command, debug, 10 * 1024 * 1024, 10 * 1024 * 1024).map_err(
        |error| match error {
            Error::Interrupted(_) => error,
            _ => Error::NotGitRepository,
        },
    )?;
    let root = trim_newlines(&root);
    if root.is_empty() {
        return Err(Error::NotGitRepository);
    }
    let root_path = PathBuf::from(os_bytes(root));
    let runner = GitRunner {
        root: &root_path,
        debug,
    };
    let branch = match runner.run(&["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(value) => value,
        Err(Error::GitCommandFailed) => b"HEAD\n".to_vec(),
        Err(error) => return Err(error),
    };
    let real_root = root_path
        .canonicalize()
        .unwrap_or_else(|_| root_path.clone());
    let hash = util::hash_hex(real_root.as_os_str().as_encoded_bytes());
    Ok(Repository {
        root_path,
        repo_id: format!("repo_{}", &hash[..16]),
        current_branch: util::lossy_utf8(trim_newlines(&branch)),
    })
}

fn diff_arguments(raw_args: &[String], cached: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "diff",
        "--patch",
        "--find-renames",
        "--no-ext-diff",
        "--no-color",
        FULL_CONTEXT,
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    if cached {
        args.push("--cached".into());
    }
    // Never append options after a bare pathspec: Git rejects that argument order.
    // Ignore color overrides before '--', while leaving literal pathspecs untouched.
    let mut before_pathspec = true;
    for arg in raw_args {
        if arg == "--" {
            before_pathspec = false;
        }
        if before_pathspec && (arg == "--color" || arg.starts_with("--color=")) {
            continue;
        }
        args.push(arg.into());
    }
    args
}

pub fn load_snapshot(repo: Repository, target: ReviewTarget, debug: bool) -> Result<DiffSnapshot> {
    let runner = GitRunner {
        root: &repo.root_path,
        debug,
    };
    let mut files = Vec::new();
    if target.kind == ReviewTargetKind::WorkingTree {
        append_patch(
            &mut files,
            runner,
            &diff_arguments(&[], false),
            DiffSource::Unstaged,
        )?;
        append_patch(
            &mut files,
            runner,
            &diff_arguments(&[], true),
            DiffSource::Staged,
        )?;
        append_untracked_files(&mut files, runner)?;
    } else {
        append_patch(
            &mut files,
            runner,
            &diff_arguments(&target.raw_args, false),
            DiffSource::Explicit,
        )?;
    }
    let mut hash_input = repo.repo_id.as_bytes().to_vec();
    hash_input.extend_from_slice(target.target_id.as_bytes());
    for file in &files {
        hash_input.extend_from_slice(diff::raw_file_paths(file).0);
        hash_input.extend_from_slice(file.patch_fingerprint.as_bytes());
    }
    let hash = util::hash_hex(&hash_input);
    Ok(DiffSnapshot {
        snapshot_id: format!("snap_{}", &hash[..16]),
        repository: repo,
        review_target: target,
        files,
    })
}
fn append_patch(
    files: &mut Vec<DiffFile>,
    runner: GitRunner<'_>,
    args: &[OsString],
    source: DiffSource,
) -> Result<()> {
    let patch = runner.run_os(args)?;
    for file in diff::parse_patch(&patch, source)? {
        merge_or_append_file(files, file);
    }
    Ok(())
}
fn merge_or_append_file(files: &mut Vec<DiffFile>, mut incoming: DiffFile) {
    if let Some(existing) = files
        .iter_mut()
        .find(|file| diff::raw_file_paths(file).0 == diff::raw_file_paths(&incoming).0)
    {
        existing.patch_text.push(b'\n');
        existing.patch_text.extend_from_slice(&incoming.patch_text);
        existing.patch_fingerprint = format!("sha256:{}", util::hash_hex(&existing.patch_text));
        existing.hunks.append(&mut incoming.hunks);
        if existing.source != incoming.source {
            existing.source = DiffSource::Explicit;
        }
    } else {
        files.push(incoming);
    }
}
fn append_untracked_files(files: &mut Vec<DiffFile>, runner: GitRunner<'_>) -> Result<()> {
    let paths = runner.run(&["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut patches = Vec::new();
    for path in paths.split(|&b| b == 0).filter(|p| !p.is_empty()) {
        let Ok(file) = File::open(runner.root.join(os_bytes(path))) else {
            continue;
        };
        let Ok(content) = read_bounded(file, MAX_UNTRACKED_BYTES) else {
            continue;
        };
        append_untracked_patch(&mut patches, path, &content);
    }
    for file in diff::parse_patch(&patches, DiffSource::Untracked)? {
        merge_or_append_file(files, file);
    }
    Ok(())
}
fn append_untracked_patch(patches: &mut Vec<u8>, path: &[u8], content: &[u8]) {
    patches.extend_from_slice(b"diff --git a/");
    patches.extend_from_slice(path);
    patches.extend_from_slice(b" b/");
    patches.extend_from_slice(path);
    patches.extend_from_slice(b"\nnew file mode 100644\nindex 0000000..0000000\n");
    if content.len() > MAX_UNTRACKED_BYTES || content.contains(&0) {
        patches.extend_from_slice(b"Binary files /dev/null and b/");
        patches.extend_from_slice(path);
        patches.extend_from_slice(b" differ\n");
        return;
    }
    patches.extend_from_slice(b"--- /dev/null\n+++ b/");
    patches.extend_from_slice(path);
    patches.push(b'\n');
    let count = count_patch_lines(content);
    if count == 0 {
        return;
    }
    patches.extend_from_slice(format!("@@ -0,0 +1,{count} @@\n").as_bytes());
    for line in content.split(|&b| b == b'\n').take(count) {
        patches.push(b'+');
        patches.extend_from_slice(diff::trim_cr(line));
        patches.push(b'\n');
    }
}
fn count_patch_lines(content: &[u8]) -> usize {
    if content.is_empty() {
        0
    } else {
        content.iter().filter(|&&b| b == b'\n').count()
            + usize::from(content.last() != Some(&b'\n'))
    }
}

pub fn load_file_side(
    repo: &Repository,
    target: &ReviewTarget,
    file: &DiffFile,
    side: FileSide,
    debug: bool,
) -> Result<Option<Vec<u8>>> {
    if file.is_binary {
        return Ok(None);
    }
    let runner = GitRunner {
        root: &repo.root_path,
        debug,
    };
    if target.kind != ReviewTargetKind::WorkingTree {
        return load_explicit_file_side(runner, target, file, side);
    }
    let (path, old_path) = diff::raw_file_paths(file);
    let old_path = old_path.unwrap_or(path);
    match (file.source, side) {
        (DiffSource::Unstaged, FileSide::Old) => read_blob(runner, "", old_path),
        (DiffSource::Unstaged | DiffSource::Untracked, FileSide::New) => {
            read_worktree_file(runner.root, path)
        }
        (DiffSource::Staged, FileSide::Old) => read_blob(runner, "HEAD", old_path),
        (DiffSource::Staged, FileSide::New) => read_blob(runner, "", path),
        (DiffSource::Untracked, FileSide::Old) | (DiffSource::Explicit, _) => Ok(None),
    }
}
fn load_explicit_file_side(
    runner: GitRunner<'_>,
    target: &ReviewTarget,
    file: &DiffFile,
    side: FileSide,
) -> Result<Option<Vec<u8>>> {
    let (path, old_path) = diff::raw_file_paths(file);
    let old_path = old_path.unwrap_or(path);
    if target.kind == ReviewTargetKind::Cached {
        return match side {
            FileSide::Old => read_blob(runner, "HEAD", old_path),
            FileSide::New => read_blob(runner, "", path),
        };
    }
    let revisions = collect_revision_args(runner, &target.raw_args);
    match target.kind {
        ReviewTargetKind::Commit => match side {
            FileSide::Old => read_blob(runner, revisions.first().copied().unwrap_or(""), old_path),
            FileSide::New => {
                if let Some(revision) = revisions.get(1) {
                    read_blob(runner, revision, path)
                } else {
                    read_worktree_file(runner.root, path)
                }
            }
        },
        ReviewTargetKind::Range | ReviewTargetKind::SymmetricRange => {
            let Some((old, new)) = range_revisions(&target.raw_args) else {
                return Ok(None);
            };
            let (old, new) = (revision_or_head(old), revision_or_head(new));
            match side {
                FileSide::New => read_blob(runner, new, path),
                FileSide::Old if target.kind == ReviewTargetKind::SymmetricRange => {
                    let base = runner.run(&["merge-base", old, new])?;
                    read_blob(
                        runner,
                        &String::from_utf8_lossy(trim_newlines(&base)),
                        old_path,
                    )
                }
                FileSide::Old => read_blob(runner, old, old_path),
            }
        }
        _ => Ok(None),
    }
}
fn collect_revision_args<'a>(runner: GitRunner<'_>, args: &'a [String]) -> Vec<&'a str> {
    args.iter()
        .take_while(|arg| arg.as_str() != "--")
        .filter(|arg| !arg.starts_with('-') && !arg.contains(".."))
        .filter(|arg| {
            runner
                .run(&[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{arg}^{{commit}}"),
                ])
                .is_ok()
        })
        .map(String::as_str)
        .collect()
}
fn range_revisions(args: &[String]) -> Option<(&str, &str)> {
    for arg in args {
        if let Some(range) = arg.split_once("...").or_else(|| arg.split_once("..")) {
            return Some(range);
        }
    }
    None
}
fn revision_or_head(revision: &str) -> &str {
    if revision.is_empty() {
        "HEAD"
    } else {
        revision
    }
}
fn read_blob(runner: GitRunner<'_>, revision: &str, path: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut spec = revision.as_bytes().to_vec();
    spec.push(b':');
    spec.extend_from_slice(path);
    match runner.run_os(&["show".into(), os_bytes(&spec)]) {
        Ok(content) => Ok(Some(content)),
        Err(Error::GitCommandFailed) => Ok(None),
        Err(error) => Err(error),
    }
}
fn read_worktree_file(root: &Path, path: &[u8]) -> Result<Option<Vec<u8>>> {
    let result =
        File::open(root.join(os_bytes(path))).and_then(|file| read_bounded(file, MAX_SOURCE_BYTES));
    match result {
        Ok(content) if content.len() <= MAX_SOURCE_BYTES => Ok(Some(content)),
        Ok(_) => Ok(None),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(None)
        }
        Err(_) => Err(Error::GitCommandFailed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranges_include_omitted_endpoints() {
        for (spec, old, new) in [
            ("main..feature", "main", "feature"),
            ("main...feature", "main", "feature"),
            ("..feature", "HEAD", "feature"),
            ("main...", "main", "HEAD"),
        ] {
            let args = [spec.into()];
            let (actual_old, actual_new) = range_revisions(&args).unwrap();
            assert_eq!(
                (revision_or_head(actual_old), revision_or_head(actual_new)),
                (old, new)
            );
        }
    }
    #[test]
    fn synthetic_patch_exact_bytes() {
        let mut patch = Vec::new();
        append_untracked_patch(&mut patch, b"new.txt", b"one\r\n\xff\r\n");
        assert_eq!(patch, b"diff --git a/new.txt b/new.txt\nnew file mode 100644\nindex 0000000..0000000\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+one\n+\xff\n");
        patch.clear();
        append_untracked_patch(&mut patch, b"empty", b"");
        assert_eq!(patch, b"diff --git a/empty b/empty\nnew file mode 100644\nindex 0000000..0000000\n--- /dev/null\n+++ b/empty\n");
        patch.clear();
        append_untracked_patch(&mut patch, b"binary", b"\0");
        assert_eq!(patch, b"diff --git a/binary b/binary\nnew file mode 100644\nindex 0000000..0000000\nBinary files /dev/null and b/binary differ\n");
        for (bytes, count) in [
            (&b""[..], 0),
            (&b"\n"[..], 1),
            (&b"a"[..], 1),
            (&b"a\n"[..], 1),
            (&b"a\n\n"[..], 2),
        ] {
            assert_eq!(count_patch_lines(bytes), count);
        }
    }
    #[test]
    fn merged_fingerprint_is_single_lf_join_with_first_metadata() {
        let first = b"diff --git a/x b/x\n@@ -1 +1 @@\n-old\n+new";
        let second = b"diff --git a/x b/x\nnew file mode 100644\n@@ -0,0 +1 @@\n+old";
        let mut files = diff::parse_patch(first, DiffSource::Unstaged).unwrap();
        let incoming = diff::parse_patch(second, DiffSource::Staged)
            .unwrap()
            .remove(0);
        merge_or_append_file(&mut files, incoming);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].source, DiffSource::Explicit);
        assert_eq!(files[0].status, diff::FileStatus::Modified);
        assert_eq!(files[0].hunks.len(), 2);
        let joined = [first.as_slice(), b"\n", second.as_slice()].concat();
        assert_eq!(files[0].patch_text, joined);
        assert_eq!(
            files[0].patch_fingerprint,
            format!("sha256:{}", util::hash_hex(&joined))
        );
    }
    #[test]
    fn bounded_reader_detects_oversize_without_consuming_whole_stream() {
        let bytes = vec![b'x'; 100];
        assert_eq!(read_bounded(bytes.as_slice(), 10).unwrap().len(), 11);
        assert_eq!(read_bounded(bytes.as_slice(), 100).unwrap(), bytes);
    }
    #[test]
    fn git_output_limits_apply_to_both_pipes() {
        let mut command = Command::new("git");
        command.arg("--version");
        assert!(matches!(
            run_command(command, false, 1, 100),
            Err(Error::DiffTooLarge)
        ));
        let mut command = Command::new("git");
        command.arg("diffo-nonexistent-subcommand");
        assert!(matches!(
            run_command(command, false, 100, 1),
            Err(Error::DiffTooLarge)
        ));
    }
    #[test]
    fn oversized_pipe_terminates_long_lived_descendants() {
        // A helper holds both pipes open after reaching the cap. Waiting for the
        // other reader before terminating the process tree would block here.
        for pipe in ["stdout", "stderr"] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--ignored",
                    "--exact",
                    "git::tests::oversized_pipe_helper",
                    "--nocapture",
                ])
                .env("DIFFO_TEST_PIPE", pipe)
                .env("DIFFO_TEST_DESCENDANT", "0");
            let start = std::time::Instant::now();
            assert!(matches!(
                run_command(command, false, 64 * 1024, 64 * 1024),
                Err(Error::DiffTooLarge)
            ));
            assert!(start.elapsed() < std::time::Duration::from_secs(5));
        }
    }
    #[test]
    #[ignore = "Subprocess helper invoked by oversized_pipe_terminates_long_lived_descendants"]
    fn oversized_pipe_helper() {
        let Ok(pipe) = std::env::var("DIFFO_TEST_PIPE") else {
            return;
        };
        if std::env::var("DIFFO_TEST_DESCENDANT").as_deref() == Ok("0") {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "git::tests::oversized_pipe_helper",
                    "--nocapture",
                ])
                .env("DIFFO_TEST_DESCENDANT", "1")
                .spawn()
                .unwrap();
            child.wait().unwrap();
        } else {
            let bytes = vec![b'x'; 128 * 1024];
            let _ = if pipe == "stdout" {
                io::stdout().write_all(&bytes)
            } else {
                io::stderr().write_all(&bytes)
            };
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
    }
    #[test]
    fn revision_collection_skips_options_and_pathspecs() {
        let directory = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "initial",
            ],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(directory.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let runner = GitRunner {
            root: directory.path(),
            debug: false,
        };
        let args = ["--find-renames", "HEAD", "src/main.zig", "--", "HEAD"].map(String::from);
        assert_eq!(collect_revision_args(runner, &args), ["HEAD"]);
    }
}
