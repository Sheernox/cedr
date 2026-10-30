//! Cedr Route: pick the harness, model and effort for each kind of work from rules, balanced by how
//! much of each plan is left (docs/route.md). The library never starts an agent and never touches
//! credentials; it reads a config and supplied readings, and records picks and verdicts locally.

pub mod config;
pub mod council;
pub mod error;
pub mod history;
pub mod import;
pub mod machine;
pub mod pick;
pub mod pool;
pub mod readers;
pub mod readings;
pub mod session;
pub mod sync;

pub use error::{Error, Result};

/// Tests that write a program and then run one must not overlap: a fork in one test inherits
/// another's still-open write handle, and running that file then fails with "Text file busy".
#[cfg(test)]
pub(crate) fn spawn_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
