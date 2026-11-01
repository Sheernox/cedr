//! Cedr Guard core: discovery of the skills and MCP servers an AI coding agent
//! loads, lint rules over that setup, the `.cedr/guard.lock` pin file, tool surface
//! extraction from npm packages, and surface diffs. The `cedr` binary is a thin
//! layer over this crate; every JSON shape here is fixed by `docs/design.md`.

pub mod canonical;
pub mod config;
pub mod diff;
pub mod discovery;
pub mod error;
pub mod finding;
mod hidden;
mod introspect;
pub mod lint;
pub mod lock;
mod npm;
pub mod outdated;
mod patterns;
pub mod remote;
pub mod risk;
pub mod skill;
pub mod source;
pub mod surface;
pub mod verify;

pub use error::{Error, Result};
