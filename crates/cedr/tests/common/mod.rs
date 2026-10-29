//! Shared scaffolding for the route, council and sync tests: the fixture config, and a scratch
//! directory holding fresh readings and the XDG state, cache and config directories.
#![allow(dead_code)] // each test binary uses a different part

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::json;
use tempfile::TempDir;

pub fn config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/route/routes.toml")
}

pub struct Scratch {
    pub dir: TempDir,
}

impl Scratch {
    /// A state directory and a readings file with codex at `codex`% and claude at `claude`% weekly.
    pub fn new(codex: f64, claude: f64) -> Scratch {
        let dir = TempDir::new().unwrap();
        let now = chrono::Utc::now();
        let at = now.to_rfc3339();
        let later = (now + chrono::Duration::hours(3)).to_rfc3339();
        let readings = json!({
            "providers": [
                {"provider": "codex", "fetchedAt": at, "error": null, "windows": [
                    {"id": "primary", "kind": "weekly", "percent": codex, "resetsAt": later}]},
                {"provider": "claudeAgent", "fetchedAt": at, "error": null, "windows": [
                    {"id": "five_hour", "kind": "session", "percent": 5, "resetsAt": later},
                    {"id": "seven_day", "kind": "weekly", "percent": claude, "resetsAt": later}]}
            ],
            "models": {"codex": ["gpt-6.1-sol", "gpt-6-sol"], "claudeAgent": ["claude-opus-5-5", "claude-sonnet-5-5"]}
        });
        std::fs::write(dir.path().join("readings.json"), readings.to_string()).unwrap();
        Scratch { dir }
    }

    pub fn readings(&self) -> String {
        self.dir.path().join("readings.json").display().to_string()
    }

    /// `cedr` with state, cache and config directories inside the scratch directory.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cedr"));
        c.args(args)
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("XDG_CACHE_HOME", self.dir.path().join("cache"))
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env_remove("CEDR_ROUTES")
            .env_remove("CEDR_API_KEY");
        c
    }

    pub fn cedr(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("cedr runs")
    }

    pub fn pick(&self, route: &str, extra: &[&str]) -> Output {
        let config = config().display().to_string();
        let readings = self.readings();
        let mut args = vec![
            "route",
            "pick",
            route,
            "--config",
            &config,
            "--readings",
            &readings,
        ];
        args.extend_from_slice(extra);
        self.cedr(&args)
    }
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
