use std::path::{Path, PathBuf};

/// Failures that stop a command. The CLI maps every variant to exit code 2.
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
    Source(String),
    #[error("{0}")]
    Registry(String),
    #[error("{0}")]
    Package(String),
    #[error("{0}")]
    Introspect(String),
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

/// A TOML error as `line L, column C: <reason>`, without the source excerpt the parser adds and
/// with any URL in the reason cut to scheme and host, so a token in the file never reaches output.
pub(crate) fn toml_err(path: &Path, text: &str, e: &toml::de::Error) -> Error {
    let reason = redact_urls(e.message());
    let message = match e.span() {
        Some(span) => {
            let before = &text[..span.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            format!("line {line}, column {column}: {reason}")
        }
        None => reason,
    };
    parse_err(path, message)
}

/// Every `scheme://...` in `s` cut to `scheme://host`.
fn redact_urls(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ')' | ']' | '>'))
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        let host = authority
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .rsplit('@')
            .next()
            .unwrap_or("");
        out.push_str(host);
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod toml_tests {
    use super::*;

    #[test]
    fn toml_errors_drop_the_excerpt_and_url_secrets() {
        let text = "version = 1\nurl = \"https://u:p{}h.example/x?token=s3cret\"\nbad =\n";
        let text = text.replace("{}", &'@'.to_string());
        let e = toml::from_str::<toml::Table>(&text).unwrap_err();
        let shown = toml_err(Path::new("f.toml"), &text, &e).to_string();
        assert!(shown.starts_with("f.toml: line 3, column"), "{shown}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert_eq!(
            redact_urls(
                "see \"https://u:p{}h.example/x?t=1\" now"
                    .replace("{}", &'@'.to_string())
                    .as_str()
            ),
            "see \"https://h.example\" now"
        );
    }
}
