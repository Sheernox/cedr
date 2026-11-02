//! Bench: run a suite of tasks against a baseline and a candidate, let tests decide each trial, and
//! report the counts (docs/bench.md). This crate holds the suite, harness adapters, trial runner and
//! report; the `cedr` binary adds the commands and settings.

pub mod exec;
pub mod harness;
pub mod report;
pub mod run;
pub mod suite;

/// A failure before or between trials, as opposed to a trial's own outcome. Names the file and key.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

pub(crate) fn io_error(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.display().to_string(),
        source,
    }
}
