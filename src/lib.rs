pub mod cli;
pub mod diff;
pub mod git;
pub mod inline_diff;
pub mod store;
pub mod syntax;
pub mod syntax_cache;
pub mod syntax_grammars;
pub mod syntax_query;
pub mod theme;
pub mod tui;
pub mod tui_text;
pub mod tui_view;
pub mod util;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("current directory is not inside a Git repository")]
    NotGitRepository,
    #[error("git command failed; run with --debug-git for details")]
    GitCommandFailed,
    #[error("diff is too large to load (over 200MB); narrow the review range")]
    DiffTooLarge,
    #[error("interrupted by signal {0}")]
    Interrupted(i32),
    #[error("invalid arguments; use diffo --help")]
    InvalidArguments,
    #[error("stored review data is corrupted")]
    StorageCorrupted,
    #[error("could not write stored review data")]
    StorageWriteFailed,
    #[error("theme file does not look like Base16/Base24")]
    ThemeInvalid,
    #[error("could not parse Git diff")]
    ParseFailed,
    #[error("syntax highlighting is unavailable")]
    SyntaxUnavailable,
    #[error("source is too large for syntax highlighting")]
    SourceTooLarge,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
