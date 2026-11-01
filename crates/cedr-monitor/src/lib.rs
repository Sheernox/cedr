//! Local, counts-only coding-agent usage collection and reporting.
pub mod config;
pub mod cursors;
pub mod limits;
pub mod pricing;
pub mod push;
pub mod report;
pub mod schedule;
pub mod sources;
pub mod store;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const HARNESSES: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "copilot",
    "opencode",
    "muse",
    "antigravity",
    "grok",
    "cursor",
];

pub fn digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn collect() -> Result<store::Store> {
    let config = config::Config::load()?;
    let mut store = store::Store::open(&store::path())?;
    sources::collect(&config, &mut store)?;
    Ok(store)
}
