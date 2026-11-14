//! Standard benchmark runner. The external API is defined in docs/bench.md.
pub mod adapter;
pub mod audit;
pub mod container;
pub mod evidence;
pub mod report;
pub mod rpc;
pub mod run;
pub mod suite;

pub type Result<T> = std::result::Result<T, String>;
pub fn sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
