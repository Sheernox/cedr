use std::path::{Path, PathBuf};

/// Failures that stop a route command. The CLI maps every variant to exit code 2.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Usage(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Adapts an `io::Error` into [`Error::Io`] carrying the path it concerns.
pub(crate) fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

pub(crate) fn parse_err(path: &Path, message: impl std::fmt::Display) -> Error {
    Error::Parse {
        path: path.to_path_buf(),
        message: message.to_string(),
    }
}
