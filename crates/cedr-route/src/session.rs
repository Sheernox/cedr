//! Loads the config and readings a command needs, so the CLI and the MCP server read them the
//! same way. A session is per call: config edits apply on the next pick.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::config::{self, Loaded};
use crate::error::{Error, Result};
use crate::machine::{self, Stale};
use crate::pick::Context;
use crate::readings::{self, Readings};

pub struct Session {
    pub loaded: Loaded,
    pub readings: Readings,
    pub now: DateTime<Utc>,
    /// Stale evaluation, failed readings sources and machine checks, for stderr.
    pub warnings: Vec<String>,
    /// Plan harnesses whose expected reader this machine's file lacks (docs/route.md, Machine checks).
    pub stale: Vec<Stale>,
}

/// Overrides an orchestrator or test supplies instead of the configured sources.
#[derive(Default)]
pub struct Overrides {
    /// A readings document replacing every configured source.
    pub readings: Option<Value>,
    /// A `{"<harness>": [ids]}` object replacing the model lists.
    pub models: Option<Value>,
}

impl Session {
    /// Loads the config (explicit path or the usual search from `cwd`) without reading any source.
    pub fn config_only(config: Option<&Path>, cwd: &Path) -> Result<Loaded> {
        config::load(&config::locate(config, cwd)?)
    }

    pub fn open(config: Option<&Path>, cwd: &Path, overrides: Overrides) -> Result<Session> {
        let loaded = Self::config_only(config, cwd)?;
        let now = Utc::now();
        let mut warnings = Vec::new();
        let mut readings = match &overrides.readings {
            Some(doc) => {
                let mut r = Readings::default();
                r.merge(&loaded.config, doc);
                r
            }
            None => {
                let dir = loaded.path.parent().unwrap_or(Path::new("."));
                let (r, w) = readings::collect(&loaded.config, dir, now);
                warnings.extend(w);
                r
            }
        };
        if let Some(models) = &overrides.models {
            readings
                .replace_models(&loaded.config, models)
                .map_err(Error::Usage)?;
        }
        readings.fill_models(&loaded.config);
        let (absent, skip_warnings) = machine::absent(&loaded.config);
        readings.absent = absent;
        warnings.extend(skip_warnings);
        let stale = machine::stale(&loaded.config, &readings);
        warnings.extend(stale.iter().map(machine::stale_warning));
        if let Some(w) = crate::pick::stale_evaluation(&loaded.config, now) {
            warnings.push(w);
        }
        Ok(Session {
            loaded,
            readings,
            now,
            warnings,
            stale,
        })
    }

    pub fn context(&self) -> Context<'_> {
        Context {
            config: &self.loaded.config,
            rules: &self.loaded.rules,
            readings: &self.readings,
            now: self.now,
            warnings: &self.warnings,
        }
    }
}
