#![cfg(unix)]

use std::{
    fs,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn interrupting_snapshot_loading_terminates_textconv_and_returns_signal_status() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let command = |program: &str| {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", temp.path())
            .env("XDG_STATE_HOME", temp.path().join("state"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(&repo);
        command
    };
    let git = |args: &[&str]| {
        let output = command("git").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    fs::write(repo.join("sample.txt"), "before\n").unwrap();
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-qm",
        "initial",
    ]);
    fs::write(repo.join("sample.txt"), "after\n").unwrap();
    fs::write(repo.join(".gitattributes"), "sample.txt diff=waiting\n").unwrap();
    let helper = temp.path().join("textconv.sh");
    fs::write(&helper, "touch \"$HOME/helper-ready\"\nsleep 10\n").unwrap();
    git(&[
        "config",
        "diff.waiting.textconv",
        &format!("sh '{}'", helper.display()),
    ]);
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let ready = temp.path().join("helper-ready");
        let _ = fs::remove_file(&ready);
        let mut child = command(env!("CARGO_BIN_EXE_diffo"))
            .args(["review", "status", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !ready.exists() {
            if start.elapsed() > Duration::from_secs(5) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Textconv helper did not start");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // SAFETY: The child is alive and owned by this test; only it receives the signal.
        assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, signal) }, 0);
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(128 + signal));
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}
