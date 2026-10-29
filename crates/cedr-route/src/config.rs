//! `routes.toml`: harnesses, readings sources and routes (docs/route.md, Config).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result, io_err, parse_err};

pub const CONFIG_VERSION: i64 = 1;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: i64,
    #[serde(default)]
    pub evaluation: Option<Evaluation>,
    #[serde(default)]
    pub thresholds: Thresholds,
    #[serde(default)]
    pub orchestrator: Option<Orchestrator>,
    #[serde(default)]
    pub harnesses: BTreeMap<String, Harness>,
    #[serde(default)]
    pub model_windows: BTreeMap<String, ModelWindow>,
    #[serde(default)]
    pub readings: Vec<ReadingsSource>,
    #[serde(default)]
    pub routes: BTreeMap<String, Route>,
    #[serde(default)]
    pub sync: Option<Sync>,
    #[serde(default)]
    pub machine: Option<Machine>,
}

/// What this machine has, whatever the routes assume (docs/route.md, Machine checks). Local only.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    /// Harnesses this machine's person does not have; unknown names only warn.
    #[serde(default)]
    pub skip: Vec<String>,
    /// How the app names this machine; default the host name up to its first dot.
    pub name: Option<String>,
}

/// Sync with the cedr.dev app (docs/route.md, Sync).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sync {
    pub org: String,
    #[serde(default)]
    pub routes: RoutesFrom,
    pub url: Option<String>,
    /// Sync in the background after picks, verdicts and status line captures.
    #[serde(default = "default_true")]
    pub auto: bool,
}

pub const DEFAULT_API_URL: &str = "https://api.cedr.dev/v1";

impl Sync {
    pub fn url(&self) -> &str {
        self.url
            .as_deref()
            .unwrap_or(DEFAULT_API_URL)
            .trim_end_matches('/')
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutesFrom {
    #[default]
    Local,
    Org,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evaluation {
    pub last: Option<String>,
    #[serde(default = "default_stale_days")]
    pub stale_days: i64,
    #[serde(default)]
    pub models: Vec<String>,
}

fn default_stale_days() -> i64 {
    35
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Thresholds {
    pub fresh_seconds: i64,
    pub session_conserve: f64,
    pub weekly_conserve: f64,
    pub out: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            fresh_seconds: 600,
            session_conserve: 85.0,
            weekly_conserve: 85.0,
            out: 90.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Orchestrator {
    pub harness: String,
    pub model: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Harness {
    #[serde(default)]
    pub expect: Vec<String>,
    pub effort_option: Option<String>,
    pub t3: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub brief_prefix: String,
    /// Model ids to resolve against when no readings source lists any.
    #[serde(default)]
    pub models: Vec<String>,
    /// How the harness is paid for: a plan with limit windows, or an API key billed per token.
    #[serde(default)]
    pub billing: Billing,
    /// The program this harness runs; a machine without it has the harness absent.
    pub command: Option<String>,
    /// The built-in reader the plan's usage should come from, for the stale machine file check.
    pub reader: Option<ReaderKind>,
}

/// The built-in readers a harness can name as its usage source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ReaderKind {
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "muse")]
    Muse,
    #[serde(rename = "claude-statusline")]
    ClaudeStatusline,
}

impl ReaderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReaderKind::Codex => "codex",
            ReaderKind::Muse => "muse",
            ReaderKind::ClaudeStatusline => "claude-statusline",
        }
    }
}

impl ReadingsSource {
    /// The built-in reader kind and the harness it reads, for the stale machine file check.
    pub fn built_in(&self) -> Option<(ReaderKind, &str)> {
        match self {
            ReadingsSource::Codex { harness, .. } => Some((ReaderKind::Codex, harness)),
            ReadingsSource::Muse { harness, .. } => Some((ReaderKind::Muse, harness)),
            ReadingsSource::ClaudeStatusline { harness } => {
                Some((ReaderKind::ClaudeStatusline, harness))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Billing {
    #[default]
    Plan,
    Api,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelWindow {
    pub harness: String,
    pub model: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "lowercase")]
pub enum ReadingsSource {
    Http {
        url: String,
        #[serde(default = "default_cache_seconds")]
        cache_seconds: i64,
    },
    File {
        path: PathBuf,
    },
    Command {
        command: Vec<String>,
        #[serde(default = "default_cache_seconds")]
        cache_seconds: i64,
    },
    /// Plan usage and models from `codex app-server`.
    Codex {
        #[serde(default = "default_codex")]
        harness: String,
        #[serde(default = "default_cache_seconds")]
        cache_seconds: i64,
    },
    /// What `cedr route statusline` captured from Claude Code.
    #[serde(rename = "claude-statusline")]
    ClaudeStatusline {
        #[serde(default = "default_claude")]
        harness: String,
    },
    /// Plan usage from `muse usage --json`.
    Muse {
        #[serde(default = "default_muse")]
        harness: String,
        #[serde(default = "default_cache_seconds")]
        cache_seconds: i64,
    },
}

fn default_muse() -> String {
    "muse".to_string()
}

fn default_codex() -> String {
    "codex".to_string()
}

fn default_claude() -> String {
    "claude".to_string()
}

fn default_cache_seconds() -> i64 {
    60
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    #[serde(default)]
    pub work: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default)]
    pub council: bool,
    /// Councils only: the fewest merged reports that can pass (docs/council.md).
    pub quorum: Option<usize>,
    /// Councils only: the least severe finding that blocks; default P1.
    pub block: Option<crate::council::Severity>,
    pub candidates: Vec<Candidate>,
}

impl Route {
    /// The council's quorum: as configured, else 2, never more than its candidates.
    pub fn quorum(&self) -> usize {
        self.quorum.unwrap_or(2).min(self.candidates.len())
    }

    /// The least severe finding that blocks the council: as configured, else P1.
    pub fn block(&self) -> crate::council::Severity {
        self.block.unwrap_or(crate::council::Severity::P1)
    }
}

fn default_true() -> bool {
    true
}

fn default_role() -> String {
    "general".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub target: String,
    #[serde(default)]
    pub effort: String,
    pub tier: Option<i64>,
}

/// A validated config, the file it came from, and the rules hash shown in route tags.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub path: PathBuf,
    pub rules: String,
}

impl Config {
    /// Parses and validates config text; `path` only labels errors.
    pub fn parse(text: &str, path: &Path) -> Result<Config> {
        let config: Config = toml::from_str(text).map_err(|e| parse_err(path, e))?;
        // A file that takes its harnesses from the org's shared routes names them in its readers
        // before they are loaded; `load` checks those readers again against the merged config.
        let overlay = config
            .sync
            .as_ref()
            .is_some_and(|s| s.routes == RoutesFrom::Org);
        config.validate(!overlay).map_err(|m| parse_err(path, m))?;
        Ok(config)
    }

    fn validate(&self, check_readers: bool) -> std::result::Result<(), String> {
        if self.version != CONFIG_VERSION {
            return Err(format!(
                "unsupported version {}; this cedr reads version {CONFIG_VERSION}",
                self.version
            ));
        }
        for (id, route) in &self.routes {
            if route.candidates.is_empty() {
                return Err(format!("route {id} has no candidates"));
            }
            match (route.council, route.quorum) {
                (false, Some(_)) => return Err(format!("route {id}: quorum is for councils only")),
                _ if !route.council && route.block.is_some() => {
                    return Err(format!("route {id}: block is for councils only"));
                }
                (true, Some(q)) if q == 0 || q > route.candidates.len() => {
                    return Err(format!(
                        "route {id}: quorum {q} must be between 1 and its {} candidates",
                        route.candidates.len()
                    ));
                }
                _ => {}
            }
            for c in &route.candidates {
                if c.target == "self" {
                    if c.tier.is_some() {
                        return Err(format!(
                            "route {id}: self ends the walk, so it takes no tier"
                        ));
                    }
                    continue;
                }
                match c.target.split_once('/') {
                    Some((h, m)) if !m.is_empty() && self.harnesses.contains_key(h) => {}
                    _ => {
                        return Err(format!(
                            "route {id}: target {:?} is not self or <defined harness>/<model>",
                            c.target
                        ));
                    }
                }
            }
        }
        for (id, w) in &self.model_windows {
            if !self.harnesses.contains_key(&w.harness) {
                return Err(format!(
                    "model_windows.{id}: unknown harness {:?}",
                    w.harness
                ));
            }
        }
        for (id, h) in &self.harnesses {
            if let Some(command) = &h.command {
                let ok = if Path::new(command).is_absolute() {
                    !command.chars().any(char::is_control)
                } else {
                    !command.is_empty()
                        && !command.contains(['/', '\\'])
                        && !command.chars().any(|c| c.is_whitespace() || c.is_control())
                };
                if !ok {
                    return Err(format!(
                        "harnesses.{id}.command {command:?} must be a program name or an absolute path, without arguments"
                    ));
                }
            }
        }
        if let Some(name) = self.machine.as_ref().and_then(|m| m.name.as_ref())
            && (name.trim().is_empty()
                || name.chars().count() > 64
                || name.chars().any(char::is_control))
        {
            return Err(format!(
                "machine.name {name:?} must be 1 to 64 characters on one line"
            ));
        }
        if let Some(o) = &self.orchestrator
            && !self.harnesses.contains_key(&o.harness)
        {
            return Err(format!("orchestrator: unknown harness {:?}", o.harness));
        }
        if let Some(sync) = &self.sync
            && !is_slug(&sync.org)
        {
            return Err(format!(
                "sync.org {:?} is not an organization slug (lowercase letters, digits and hyphens)",
                sync.org
            ));
        }
        for source in self.readings.iter().filter(|_| check_readers) {
            if let ReadingsSource::Codex { harness, .. }
            | ReadingsSource::Muse { harness, .. }
            | ReadingsSource::ClaudeStatusline { harness } = source
                && !self.harnesses.contains_key(harness)
            {
                return Err(format!("readings: unknown harness {harness:?}"));
            }
        }
        Ok(())
    }

    /// The harness id a readings feed's name refers to, by id or alias.
    pub fn harness_for(&self, name: &str) -> Option<&str> {
        if let Some((id, _)) = self.harnesses.get_key_value(name) {
            return Some(id);
        }
        self.harnesses
            .iter()
            .find(|(_, h)| h.aliases.iter().any(|a| a == name))
            .map(|(id, _)| id.as_str())
    }
}

/// The first 7 hex characters of SHA-256 over a config file's bytes, shown in route tags.
pub fn rules_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..7]
        .to_string()
}

/// Where `cedr route sync` keeps an org's shared routes.
pub fn org_routes_path(org: &str) -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config")
        .join("cedr")
        .join("org-routes")
        .join(format!("{org}.toml"))
}

/// Parses shared routes: a full config without `[[readings]]`, `[sync]` or `[machine]`, which stay on
/// each machine. A shared file must never make members' machines run a command or fetch a URL, and
/// what a machine has is for that machine to say.
pub fn parse_shared(text: &str, path: &Path) -> Result<Config> {
    let config = Config::parse(text, path)?;
    if !config.readings.is_empty() || config.sync.is_some() || config.machine.is_some() {
        return Err(parse_err(
            path,
            "shared routes cannot contain [[readings]], [sync] or [machine]; those stay in each machine's routes.toml",
        ));
    }
    // A path would let whoever edits the shared routes learn which files exist on members' machines.
    if let Some((id, _)) = config.harnesses.iter().find(|(_, h)| {
        h.command
            .as_deref()
            .is_some_and(|c| Path::new(c).is_absolute())
    }) {
        return Err(parse_err(
            path,
            format!(
                "shared routes may name harnesses.{id}.command only as a program name, not a path"
            ),
        ));
    }
    Ok(config)
}

/// Reads and validates the file at `path` alone, without the org's shared routes (for `sync`,
/// which must work before anything is pulled).
pub fn load_local(path: &Path) -> Result<Config> {
    let bytes = std::fs::read(path).map_err(io_err(path))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| parse_err(path, e))?;
    Config::parse(text, path)
}

/// An organization slug: lowercase letters, digits and hyphens, so it is safe as a file name.
pub fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Reads and validates the config at `path`. With `[sync] routes = "org"`, everything but its
/// `[[readings]]`, `[sync]` and `[machine]` comes from the org's pulled shared routes, whose hash is
/// the rules hash.
pub fn load(path: &Path) -> Result<Loaded> {
    let bytes = std::fs::read(path).map_err(io_err(path))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| parse_err(path, e))?;
    let config = Config::parse(text, path)?;
    let Some(sync) = config.sync.clone().filter(|s| s.routes == RoutesFrom::Org) else {
        return Ok(Loaded {
            config,
            path: path.to_path_buf(),
            rules: rules_hash(&bytes),
        });
    };
    let org_path = org_routes_path(&sync.org);
    let org_bytes = std::fs::read(&org_path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::Config(format!(
            "{} uses the shared routes of {}, which are not pulled yet: run `cedr route sync`",
            path.display(),
            sync.org
        )),
        _ => io_err(&org_path)(e),
    })?;
    let org_text = std::str::from_utf8(&org_bytes).map_err(|e| parse_err(&org_path, e))?;
    let mut merged = parse_shared(org_text, &org_path)?;
    merged.readings = config.readings;
    merged.sync = config.sync;
    merged.machine = config.machine;
    merged.validate(true).map_err(|m| parse_err(path, m))?;
    Ok(Loaded {
        config: merged,
        path: path.to_path_buf(),
        rules: rules_hash(&org_bytes),
    })
}

/// The config file to use: the explicit path, `$CEDR_ROUTES`, the nearest `.cedr/routes.toml`
/// from `cwd` upwards, then the user config.
pub fn locate(explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os("CEDR_ROUTES").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    for dir in cwd.ancestors() {
        let candidate = dir.join(".cedr").join("routes.toml");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    let user = xdg_dir("XDG_CONFIG_HOME", ".config")
        .join("cedr")
        .join("routes.toml");
    if user.is_file() {
        return Ok(user);
    }
    Err(Error::Config(format!(
        "no routes.toml found: pass --config, set CEDR_ROUTES, add .cedr/routes.toml, or create {}",
        user.display()
    )))
}

/// `$VAR` when set to an absolute path, else `~/<fallback>` (the XDG base directory rule).
pub fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    if let Some(v) = std::env::var_os(var) {
        let p = PathBuf::from(v);
        if p.is_absolute() {
            return p;
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> std::result::Result<Config, String> {
        Config::parse(text, Path::new("routes.toml")).map_err(|e| e.to_string())
    }

    #[test]
    fn rejects_targets_on_undefined_harnesses() {
        let err = parse(
            "version = 1\n[harnesses.codex]\n[routes.r]\ncandidates = [{ target = \"claude/opus\" }]\n",
        )
        .unwrap_err();
        assert!(
            err.contains("not self or <defined harness>/<model>"),
            "{err}"
        );
    }

    #[test]
    fn self_cannot_join_a_tier() {
        let err = parse("version = 1\n[harnesses.codex]\n[routes.r]\ncandidates = [{ target = \"codex/sol\", tier = 1 }, { target = \"self\", tier = 1 }]\n")
            .unwrap_err();
        assert!(err.contains("self ends the walk"), "{err}");
    }

    #[test]
    fn rejects_unknown_fields_and_versions() {
        assert!(
            parse("version = 2\n")
                .unwrap_err()
                .contains("unsupported version 2")
        );
        assert!(parse("version = 1\n[thresholds]\nouts = 90\n").is_err());
    }

    #[test]
    fn finds_harnesses_by_alias() {
        let c = parse("version = 1\n[harnesses.claude]\naliases = [\"claudeAgent\"]\n").unwrap();
        assert_eq!(c.harness_for("claudeAgent"), Some("claude"));
        assert_eq!(c.harness_for("claude"), Some("claude"));
        assert_eq!(c.harness_for("codex"), None);
    }
}
