use diffo::{Error, cli};
use std::io::Write;

fn main() {
    let result = std::env::args_os()
        .skip(1)
        .map(|arg| arg.into_string().map_err(|_| Error::InvalidArguments))
        .collect::<diffo::Result<Vec<_>>>()
        .and_then(|args| cli::run(&args));
    if let Err(error) = result {
        if matches!(&error, Error::Io(e) if e.kind() == std::io::ErrorKind::BrokenPipe) {
            return;
        }
        let message = match &error {
            Error::Interrupted(signal) => std::process::exit(128 + signal),
            Error::NotGitRepository => "current directory is not inside a Git repository".into(),
            Error::GitCommandFailed => {
                "git command failed; run with --debug-git for details".into()
            }
            Error::DiffTooLarge => {
                "diff is too large to load (over 200MB); narrow the review range".into()
            }
            Error::InvalidArguments => "invalid arguments; use diffo --help".into(),
            Error::StorageCorrupted => "stored review data is corrupted".into(),
            Error::ThemeInvalid => "theme file does not look like Base16/Base24".into(),
            Error::StorageWriteFailed => "StorageWriteFailed".into(),
            Error::ParseFailed => "ParseFailed".into(),
            Error::SyntaxUnavailable => "SyntaxUnavailable".into(),
            Error::SourceTooLarge => "SourceTooLarge".into(),
            Error::Io(e) => match e.kind() {
                std::io::ErrorKind::NotFound => "FileNotFound".into(),
                std::io::ErrorKind::PermissionDenied => "AccessDenied".into(),
                _ => e.to_string(),
            },
            Error::Json(e) => e.to_string(),
        };
        let _ = writeln!(std::io::stderr().lock(), "diffo: {message}");
        std::process::exit(1);
    }
}
