//! `routes.toml`: harnesses, readings sources and routes (docs/route.md, Config).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result, io_err, parse_err, toml_err};
use crate::pick::Size;
use crate::pool::State;

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
    /// What a pick does with a family's newest version that `models` does not list.
    #[serde(default)]
    pub preview: Preview,
}

/// How a pick treats a version the last evaluation did not list (docs/route.md, Preview versions).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preview {
    /// Hold it back, except for small picks, which collect its verdicts.
    #[default]
    Trial,
    /// Always use the newest evaluated version.
    Hold,
    /// Use the newest version whatever the evaluation lists.
    Use,
}

fn default_stale_days() -> i64 {
    35
}

/// One layer of plan thresholds; what it leaves unset comes from the layer below (docs/route.md,
/// Thresholds).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    pub fresh_seconds: Option<i64>,
    pub session_conserve: Option<f64>,
    pub weekly_conserve: Option<f64>,
    pub out: Option<f64>,
    pub session_out: Option<f64>,
    pub weekly_out: Option<f64>,
    pub conserve_sizes: Option<Vec<Size>>,
    pub unknown_sizes: Option<Vec<Size>>,
}

impl Thresholds {
    fn validate(&self, label: &str) -> std::result::Result<(), String> {
        for (key, v) in [
            ("session_conserve", self.session_conserve),
            ("weekly_conserve", self.weekly_conserve),
            ("out", self.out),
            ("session_out", self.session_out),
            ("weekly_out", self.weekly_out),
        ] {
            if let Some(v) = v
                && !(0.0..=100.0).contains(&v)
            {
                return Err(format!("{label}.{key} {v} must be between 0 and 100"));
            }
        }
        match self.fresh_seconds {
            Some(s) if s < 0 => Err(format!("{label}.fresh_seconds {s} must not be negative")),
            _ => Ok(()),
        }
    }
}

/// The thresholds in force for a plan or route once the layers are applied.
#[derive(Debug, Clone)]
pub struct Limits {
    pub fresh_seconds: i64,
    pub session_conserve: f64,
    pub weekly_conserve: f64,
    pub session_out: f64,
    pub weekly_out: f64,
    pub conserve_sizes: Vec<Size>,
    pub unknown_sizes: Vec<Size>,
    /// The layer each key came from; a key not here is the default.
    from: BTreeMap<&'static str, &'static str>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fresh_seconds: 600,
            session_conserve: 85.0,
            weekly_conserve: 85.0,
            session_out: 90.0,
            weekly_out: 90.0,
            conserve_sizes: vec![Size::Small],
            unknown_sizes: vec![Size::Small, Size::Medium],
            from: BTreeMap::new(),
        }
    }
}

/// One threshold in force and the layer that set it, for `cedr route thresholds` and `show`.
#[derive(Debug, Clone, Serialize)]
pub struct LimitRow {
    pub key: &'static str,
    pub value: serde_json::Value,
    pub from: &'static str,
}

impl Limits {
    fn apply(&mut self, layer: &Thresholds, name: &'static str) {
        fn set<T: Clone>(
            from: &mut BTreeMap<&'static str, &'static str>,
            name: &'static str,
            key: &'static str,
            field: &mut T,
            value: &Option<T>,
        ) {
            if let Some(v) = value {
                *field = v.clone();
                from.insert(key, name);
            }
        }
        let f = &mut self.from;
        set(
            f,
            name,
            "fresh_seconds",
            &mut self.fresh_seconds,
            &layer.fresh_seconds,
        );
        set(
            f,
            name,
            "session_conserve",
            &mut self.session_conserve,
            &layer.session_conserve,
        );
        set(
            f,
            name,
            "weekly_conserve",
            &mut self.weekly_conserve,
            &layer.weekly_conserve,
        );
        set(f, name, "session_out", &mut self.session_out, &layer.out);
        set(f, name, "weekly_out", &mut self.weekly_out, &layer.out);
        set(
            f,
            name,
            "session_out",
            &mut self.session_out,
            &layer.session_out,
        );
        set(
            f,
            name,
            "weekly_out",
            &mut self.weekly_out,
            &layer.weekly_out,
        );
        set(
            f,
            name,
            "conserve_sizes",
            &mut self.conserve_sizes,
            &layer.conserve_sizes,
        );
        set(
            f,
            name,
            "unknown_sizes",
            &mut self.unknown_sizes,
            &layer.unknown_sizes,
        );
    }

    /// Whether a plan in `state` takes work of `size`.
    pub fn takes(&self, state: State, size: Size) -> bool {
        match state {
            State::Ready => true,
            State::Conserve => self.conserve_sizes.contains(&size),
            State::Unknown => self.unknown_sizes.contains(&size),
            State::Out | State::Absent => false,
        }
    }

    pub fn rows(&self) -> Vec<LimitRow> {
        let row = |key: &'static str, value: serde_json::Value| LimitRow {
            key,
            value,
            from: self.from.get(key).copied().unwrap_or("default"),
        };
        vec![
            row("fresh_seconds", self.fresh_seconds.into()),
            row("session_conserve", self.session_conserve.into()),
            row("weekly_conserve", self.weekly_conserve.into()),
            row("session_out", self.session_out.into()),
            row("weekly_out", self.weekly_out.into()),
            row("conserve_sizes", serde_json::json!(self.conserve_sizes)),
            row("unknown_sizes", serde_json::json!(self.unknown_sizes)),
        ]
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
    /// Overrides `[thresholds]` for this plan.
    #[serde(default)]
    pub thresholds: Thresholds,
}

/// The built-in readers a harness can name as its usage source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ReaderKind {
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "muse")]
    Muse,
    #[serde(rename = "grok")]
    Grok,
    #[serde(rename = "claude-statusline")]
    ClaudeStatusline,
}

impl ReaderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReaderKind::Codex => "codex",
            ReaderKind::Muse => "muse",
            ReaderKind::Grok => "grok",
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
            ReadingsSource::Grok { harness, .. } => Some((ReaderKind::Grok, harness)),
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
    /// Plan usage from `grok agent stdio`.
    Grok {
        #[serde(default = "default_grok")]
        harness: String,
        #[serde(default = "default_cache_seconds")]
        cache_seconds: i64,
    },
}

fn default_grok() -> String {
    "grok".to_string()
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

/// Configured revision budget; unlimited is represented as null in merge output.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(untagged)]
pub enum Rounds {
    Count(u32),
    Unlimited(Unlimited),
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unlimited {
    Unlimited,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fixes {
    #[default]
    Agent,
    Ask,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Advisory {
    #[default]
    Apply,
    FollowUp,
    Ask,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Spares {
    #[default]
    All,
    Quorum,
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
    /// Councils only: revision rounds before a blocking gate stands; default 1.
    pub rounds: Option<Rounds>,
    pub fixes: Option<Fixes>,
    pub advisory: Option<Advisory>,
    pub spares: Option<Spares>,
    /// A cascade: the route that judges this route's work (docs/route.md, Cascade).
    pub judge: Option<String>,
    /// With `judge`: where escalated work goes; none means the orchestrator takes it.
    pub escalate: Option<String>,
    /// Overrides `[thresholds]` and the harness's thresholds for this route's picks.
    #[serde(default)]
    pub thresholds: Thresholds,
    /// Overrides `thresholds` for one plan's candidates on this route, by harness id.
    #[serde(default)]
    pub harness_thresholds: BTreeMap<String, Thresholds>,
    pub candidates: Vec<Candidate>,
}

impl Route {
    /// The council's quorum: as configured, else 2, never more than its regular members.
    pub fn quorum(&self) -> usize {
        self.quorum
            .unwrap_or(2)
            .min(self.candidates.iter().filter(|c| !c.spare).count())
    }

    /// The least severe finding that blocks the council: as configured, else P1.
    pub fn block(&self) -> crate::council::Severity {
        self.block.unwrap_or(crate::council::Severity::P1)
    }

    /// The council's revision rounds: as configured, else 1.
    pub fn rounds(&self) -> Option<u32> {
        match self.rounds {
            None => Some(1),
            Some(Rounds::Count(n)) => Some(n),
            Some(Rounds::Unlimited(_)) => None,
        }
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
    #[serde(default)]
    pub spare: bool,
    pub target: String,
    #[serde(default)]
    pub effort: String,
    pub tier: Option<i64>,
    /// The work sizes this candidate takes; all of them when unset.
    pub sizes: Option<Vec<Size>>,
}

impl Candidate {
    pub fn takes_size(&self, size: Size) -> bool {
        self.sizes.as_ref().is_none_or(|s| s.contains(&size))
    }
}

/// A validated config, the file it came from, and the rules hash shown in route tags.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub path: PathBuf,
    pub rules: String,
    pub readings_dir: PathBuf,
    pub warnings: Vec<String>,
}

impl Config {
    /// Parses and validates config text; `path` only labels errors.
    pub fn parse(text: &str, path: &Path) -> Result<Config> {
        let config: Config = toml::from_str(text).map_err(|e| toml_err(path, text, &e))?;
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
                (true, Some(q))
                    if q == 0 || q > route.candidates.iter().filter(|c| !c.spare).count() =>
                {
                    return Err(format!(
                        "route {id}: quorum {q} must be between 1 and its {} candidates",
                        route.candidates.iter().filter(|c| !c.spare).count()
                    ));
                }
                _ => {}
            }
            match route.rounds {
                Some(_) if !route.council => {
                    return Err(format!("route {id}: rounds is for councils only"));
                }
                Some(Rounds::Count(r)) if r > 3 => {
                    return Err(format!("route {id}: rounds {r} must be between 0 and 3"));
                }
                _ => {}
            }
            if route.council
                && route.candidates.iter().any(|c| c.spare)
                && route
                    .candidates
                    .iter()
                    .all(|c| c.spare || c.target == "self")
            {
                return Err(format!(
                    "route {id}: a spare only stands in, so a council needs a member that is not one"
                ));
            }
            if !route.council
                && (route.fixes.is_some() || route.advisory.is_some() || route.spares.is_some())
            {
                return Err(format!(
                    "route {id}: fixes, advisory and spares are for councils only"
                ));
            }
            self.validate_cascade(id, route)?;
            if route.council {
                let quorum = route.quorum();
                for size in Size::ALL {
                    let takes = route
                        .candidates
                        .iter()
                        .filter(|c| !c.spare)
                        .filter(|c| c.sizes.as_ref().is_none_or(|s| s.contains(&size)))
                        .count();
                    if takes < quorum {
                        return Err(format!(
                            "route {id}: {takes} members take {} work, below its quorum of {quorum}",
                            size.as_str()
                        ));
                    }
                }
            }
            route
                .thresholds
                .validate(&format!("routes.{id}.thresholds"))?;
            for (harness, layer) in &route.harness_thresholds {
                if !self.harnesses.contains_key(harness) {
                    return Err(format!(
                        "routes.{id}.harness_thresholds names unknown harness {harness:?}"
                    ));
                }
                layer.validate(&format!("routes.{id}.harness_thresholds.{harness}"))?;
            }
            for c in &route.candidates {
                if c.spare && (!route.council || c.target == "self") {
                    return Err(format!(
                        "route {id}: spare requires a council member other than self"
                    ));
                }
                match &c.sizes {
                    Some(_) if c.target == "self" => {
                        return Err(format!("route {id}: self takes no sizes"));
                    }
                    Some(s) if s.is_empty() => {
                        return Err(format!("route {id}: {} has empty sizes", c.target));
                    }
                    _ => {}
                }
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
        // Files that already set these [thresholds] keys outside the range must keep loading
        // (docs/versioning.md); the new keys and layers have no such files.
        Thresholds {
            session_out: self.thresholds.session_out,
            weekly_out: self.thresholds.weekly_out,
            ..Thresholds::default()
        }
        .validate("thresholds")?;
        for (id, h) in &self.harnesses {
            h.thresholds
                .validate(&format!("harnesses.{id}.thresholds"))?;
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
            | ReadingsSource::Grok { harness, .. }
            | ReadingsSource::ClaudeStatusline { harness } = source
                && !self.harnesses.contains_key(harness)
            {
                return Err(format!("readings: unknown harness {harness:?}"));
            }
        }
        Ok(())
    }

    /// The cascade rules: `judge` and `escalate` name other, non-council routes; `escalate` needs a
    /// judge; a judge is not a cascade itself; and following `escalate` never comes back.
    fn validate_cascade(&self, id: &str, route: &Route) -> std::result::Result<(), String> {
        if route.escalate.is_some() && route.judge.is_none() {
            return Err(format!("route {id}: escalate needs a judge"));
        }
        if route.council && (route.judge.is_some() || route.escalate.is_some()) {
            return Err(format!("route {id}: a council cannot be a cascade"));
        }
        for (field, name) in [("judge", &route.judge), ("escalate", &route.escalate)] {
            let Some(name) = name else { continue };
            if name == id {
                return Err(format!("route {id}: {field} names the route itself"));
            }
            let Some(other) = self.routes.get(name) else {
                return Err(format!("route {id}: {field} names unknown route {name:?}"));
            };
            if other.council {
                return Err(format!("route {id}: {field} {name:?} is a council"));
            }
            if field == "judge" && other.judge.is_some() {
                return Err(format!("route {id}: judge {name:?} is a cascade itself"));
            }
        }
        let mut seen = vec![id];
        let mut next = route.escalate.as_deref();
        while let Some(name) = next {
            if seen.contains(&name) {
                return Err(format!("route {id}: escalate comes back to {name:?}"));
            }
            seen.push(name);
            next = self.routes.get(name).and_then(|r| r.escalate.as_deref());
        }
        Ok(())
    }

    /// The thresholds in force: the defaults, then `[thresholds]`, then the harness's, then the
    /// route's, then the route's for that harness.
    pub fn limits(&self, harness: Option<&str>, route: Option<&Route>) -> Limits {
        let mut limits = Limits::default();
        limits.apply(&self.thresholds, "thresholds");
        if let Some(h) = harness.and_then(|h| self.harnesses.get(h)) {
            limits.apply(&h.thresholds, "harness");
        }
        if let Some(r) = route {
            limits.apply(&r.thresholds, "route");
            if let Some(layer) = harness.and_then(|h| r.harness_thresholds.get(h)) {
                limits.apply(layer, "route harness");
            }
        }
        limits
    }

    /// Whether `[evaluation] models` lists `harness/model`, the harness by id or alias.
    pub fn evaluated(&self, harness: &str, model: &str) -> bool {
        let Some(ev) = &self.evaluation else {
            return false;
        };
        ev.models.iter().any(|e| {
            e.split_once('/')
                .is_some_and(|(h, m)| m == model && self.harness_for(h) == Some(harness))
        })
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
            readings_dir: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            warnings: Vec::new(),
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
        readings_dir: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        warnings: Vec::new(),
    })
}

/// Why a routes file was selected determines whether it may supply executable machine settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Explicit,
    Env,
    Repo,
    User,
}

pub struct Located {
    pub path: PathBuf,
    pub origin: Origin,
}

fn user_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("cedr/routes.toml")
}

/// Machine configuration never comes from the current checkout. Flags and environment are explicit trust choices.
pub fn machine_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os("CEDR_ROUTES").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let user = user_path();
    if user.is_file() {
        return Ok(user);
    }
    Err(Error::Config(format!(
        "no routes.toml found for machine configuration: pass --config, set CEDR_ROUTES, or create {}; a starter file and the app's generator: https://cedr.dev/docs/route",
        user.display()
    )))
}

/// Routing preference: explicit path, environment, nearest repo override, then the user file.
pub fn locate(explicit: Option<&Path>, cwd: &Path) -> Result<Located> {
    if let Some(p) = explicit {
        return Ok(Located {
            path: p.to_path_buf(),
            origin: Origin::Explicit,
        });
    }
    if let Some(p) = std::env::var_os("CEDR_ROUTES").filter(|v| !v.is_empty()) {
        return Ok(Located {
            path: PathBuf::from(p),
            origin: Origin::Env,
        });
    }
    for dir in cwd.ancestors() {
        let candidate = dir.join(".cedr/routes.toml");
        if candidate.is_file() {
            return Ok(Located {
                path: candidate,
                origin: Origin::Repo,
            });
        }
    }
    Ok(Located {
        path: machine_path(None)?,
        origin: Origin::User,
    })
}

/// A discovered repository may override routing, but cannot choose commands, files, or credential destinations.
pub fn load_selected(selected: Located) -> Result<Loaded> {
    if selected.origin != Origin::Repo {
        return load(&selected.path);
    }
    let path = selected.path;
    let bytes = std::fs::read(&path).map_err(io_err(&path))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| parse_err(&path, e))?;
    let mut document: toml::Table = toml::from_str(text).map_err(|e| toml_err(&path, text, &e))?;
    let mut ignored = false;
    for section in ["readings", "sync", "machine"] {
        ignored |= document.remove(section).is_some();
    }
    let routing = toml::to_string(&document).map_err(|e| parse_err(&path, e))?;
    let mut config = parse_shared(&routing, &path)?;
    let user = user_path();
    if user.is_file() {
        let local = load_local(&user)?;
        config.readings = local
            .readings
            .into_iter()
            .filter(|source| match source {
                ReadingsSource::Codex { harness, .. }
                | ReadingsSource::ClaudeStatusline { harness }
                | ReadingsSource::Muse { harness, .. }
                | ReadingsSource::Grok { harness, .. } => config.harness_for(harness).is_some(),
                _ => true,
            })
            .collect();
        config.sync = local.sync;
        config.machine = local.machine;
    }
    config.validate(true).map_err(|m| parse_err(&path, m))?;
    let warnings = if ignored {
        vec![format!(
            "{}: ignored repository [[readings]], [sync] and [machine]; move machine settings to {} or explicitly select a trusted file with --config or CEDR_ROUTES",
            path.display(),
            user.display()
        )]
    } else {
        Vec::new()
    };
    Ok(Loaded {
        config,
        path,
        rules: rules_hash(&bytes),
        readings_dir: user.parent().unwrap_or(Path::new(".")).to_path_buf(),
        warnings,
    })
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

    const CASCADE: &str = "version = 1\n[harnesses.codex]\n[routes.a]\ncandidates = [{ target = \"codex/sol\" }]\n[routes.j]\ncandidates = [{ target = \"codex/luna\" }]\n[routes.c]\ncouncil = true\ncandidates = [{ target = \"codex/sol\" }]\n";

    fn cascade_err(extra: &str) -> String {
        let text = CASCADE.replace("[routes.a]\n", &format!("[routes.a]\n{extra}"));
        parse(&text).unwrap_err()
    }

    #[test]
    fn validates_cascades() {
        let ok = CASCADE.replace(
            "[routes.a]\n",
            "[routes.a]\njudge = \"j\"\nescalate = \"j\"\n",
        );
        assert!(parse(&ok).is_ok());
        assert!(cascade_err("escalate = \"j\"\n").contains("escalate needs a judge"));
        assert!(cascade_err("judge = \"a\"\n").contains("names the route itself"));
        assert!(cascade_err("judge = \"nope\"\n").contains("unknown route \"nope\""));
        assert!(cascade_err("judge = \"c\"\n").contains("is a council"));
        assert!(cascade_err("judge = \"j\"\nescalate = \"c\"\n").contains("is a council"));
        let council = CASCADE.replace("council = true\n", "council = true\njudge = \"j\"\n");
        assert!(
            parse(&council)
                .unwrap_err()
                .contains("a council cannot be a cascade")
        );
        let chained = CASCADE.replace("[routes.j]\n", "[routes.j]\njudge = \"a\"\n");
        let chained = chained.replace("[routes.a]\n", "[routes.a]\njudge = \"j\"\n");
        assert!(parse(&chained).unwrap_err().contains("is a cascade itself"));
        let looped = format!(
            "{CASCADE}[routes.b]\njudge = \"j\"\nescalate = \"a\"\ncandidates = [{{ target = \"self\" }}]\n"
        )
        .replace("[routes.a]\n", "[routes.a]\njudge = \"j\"\nescalate = \"b\"\n");
        assert!(
            parse(&looped)
                .unwrap_err()
                .contains("escalate comes back to")
        );
    }

    #[test]
    fn rounds_are_for_councils_within_0_to_3() {
        let council = |r: &str| {
            CASCADE.replace(
                "council = true\n",
                &format!("council = true\nrounds = {r}\n"),
            )
        };
        assert!(parse(&council("0")).is_ok());
        assert!(
            parse(&council("4"))
                .unwrap_err()
                .contains("between 0 and 3")
        );
        assert!(cascade_err("rounds = 1\n").contains("rounds is for councils only"));
    }

    #[test]
    fn preview_modes_parse_and_reject_others() {
        let with = |m: &str| format!("version = 1\n[evaluation]\npreview = \"{m}\"\n");
        assert_eq!(
            parse(&with("hold")).unwrap().evaluation.unwrap().preview,
            Preview::Hold
        );
        assert_eq!(
            parse("version = 1\n[evaluation]\n")
                .unwrap()
                .evaluation
                .unwrap()
                .preview,
            Preview::Trial
        );
        assert!(parse(&with("later")).is_err());
    }

    #[test]
    fn thresholds_come_from_the_most_specific_layer() {
        let c = parse(
            "version = 1\n[thresholds]\nsession_out = 95\nweekly_conserve = 80\n[harnesses.a.thresholds]\nout = 70\n[harnesses.b]\n[routes.r]\ncandidates = [{ target = \"self\" }]\n[routes.r.thresholds]\nweekly_out = 60\nconserve_sizes = []\n",
        )
        .unwrap();
        let global = c.limits(Some("b"), None);
        assert_eq!(
            (
                global.session_out,
                global.weekly_out,
                global.weekly_conserve
            ),
            (95.0, 90.0, 80.0)
        );
        let plan = c.limits(Some("a"), None);
        assert_eq!(
            (plan.session_out, plan.weekly_out),
            (70.0, 70.0),
            "a plan's out beats a global session_out"
        );
        let route = c.limits(Some("a"), c.routes.get("r"));
        assert_eq!((route.session_out, route.weekly_out), (70.0, 60.0));
        assert!(!route.takes(State::Conserve, Size::Small));
        let from: Vec<(&str, &str)> = route.rows().iter().map(|r| (r.key, r.from)).collect();
        assert!(from.contains(&("weekly_out", "route")));
        assert!(from.contains(&("session_out", "harness")));
        assert!(from.contains(&("weekly_conserve", "thresholds")));
        assert!(from.contains(&("fresh_seconds", "default")));
    }

    #[test]
    fn a_route_can_give_one_plan_its_own_limits() {
        let c = parse(
            "version = 1\n[harnesses.a.thresholds]\nweekly_out = 70\n[harnesses.b]\n[routes.r]\ncandidates = [{ target = \"self\" }]\n[routes.r.thresholds]\nweekly_out = 80\nsession_out = 75\n[routes.r.harness_thresholds.a]\nweekly_out = 98\n",
        )
        .unwrap();
        let r = c.routes.get("r");
        let a = c.limits(Some("a"), r);
        assert_eq!((a.weekly_out, a.session_out), (98.0, 75.0));
        assert_eq!(c.limits(Some("b"), r).weekly_out, 80.0);
        assert_eq!(
            c.limits(Some("a"), None).weekly_out,
            70.0,
            "a plan's own answer ignores the route"
        );
        let from: Vec<(&str, &str)> = a.rows().iter().map(|x| (x.key, x.from)).collect();
        assert!(from.contains(&("weekly_out", "route harness")));
        assert!(from.contains(&("session_out", "route")));
    }

    #[test]
    fn a_route_refuses_limits_for_a_plan_it_cannot_know_or_out_of_range() {
        let file = |h: &str, v: u32| {
            format!(
                "version = 1\n[harnesses.a]\n[routes.r]\ncandidates = [{{ target = \"self\" }}]\n[routes.r.harness_thresholds.{h}]\nweekly_out = {v}\n"
            )
        };
        assert!(parse(&file("a", 98)).is_ok());
        assert!(
            parse(&file("nope", 98))
                .unwrap_err()
                .to_string()
                .contains("unknown harness")
        );
        assert!(parse(&file("a", 101)).is_err());
    }

    #[test]
    fn rejects_thresholds_and_sizes_that_cannot_apply() {
        assert!(
            parse("version = 1\n[thresholds]\nout = 101\n").is_ok(),
            "older files keep loading"
        );
        assert!(parse("version = 1\n[thresholds]\nweekly_out = 101\n").is_err());
        assert!(
            parse("version = 1\n[harnesses.a.thresholds]\nout = 101\n")
                .unwrap_err()
                .contains("harnesses.a.thresholds.out 101")
        );
        assert!(
            parse("version = 1\n[harnesses.a.thresholds]\nfresh_seconds = -1\n")
                .unwrap_err()
                .contains("harnesses.a.thresholds.fresh_seconds")
        );
        assert!(parse("version = 1\n[thresholds]\nunknown_sizes = [\"huge\"]\n").is_err());
        let route = |c: &str| format!("version = 1\n[harnesses.a]\n[routes.r]\n{c}");
        assert!(
            parse(&route("candidates = [{ target = \"a/m\", sizes = [] }]\n"))
                .unwrap_err()
                .contains("empty sizes")
        );
        assert!(
            parse(&route(
                "candidates = [{ target = \"self\", sizes = [\"small\"] }]\n"
            ))
            .unwrap_err()
            .contains("self takes no sizes")
        );
    }

    #[test]
    fn every_size_a_council_reviews_must_reach_its_quorum() {
        let route = |c: &str| format!("version = 1\n[harnesses.a]\n[routes.r]\n{c}");
        let members = |small: &str| {
            format!(
                "council = true\nquorum = 2\ncandidates = [\n  {{ target = \"a/m\" }},\n  {{ target = \"a/n\" }},\n  {{ target = \"a/o\", sizes = [\"large\"] }},\n  {{ target = \"a/p\", sizes = [{small}] }},\n]\n"
            )
        };
        assert!(parse(&route(&members("\"small\""))).is_ok());
        let two = "council = true\nquorum = 2\ncandidates = [\n  { target = \"a/m\" },\n  { target = \"a/n\", sizes = [\"large\"] },\n]\n";
        let err = parse(&route(two)).unwrap_err();
        assert!(err.contains("1 members take small work"), "{err}");
    }

    #[test]
    fn finds_harnesses_by_alias() {
        let c = parse("version = 1\n[harnesses.claude]\naliases = [\"claudeAgent\"]\n").unwrap();
        assert_eq!(c.harness_for("claudeAgent"), Some("claude"));
        assert_eq!(c.harness_for("claude"), Some("claude"));
        assert_eq!(c.harness_for("codex"), None);
    }
    #[test]
    fn a_council_of_only_spares_is_refused() {
        let err = parse(
            "version = 1\n[harnesses.a]\n[routes.c]\ncouncil = true\ncandidates = [{ target = \"a/m\", spare = true }]\n",
        );
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("a member that is not one")
        );
        let with_self = parse(
            "version = 1\n[harnesses.a]\n[routes.c]\ncouncil = true\ncandidates = [{ target = \"a/m\", spare = true }, { target = \"self\" }]\n",
        );
        assert!(with_self.is_err(), "self cannot stand in for a member");
    }

    #[test]
    fn council_options_validate_without_changing_old_defaults() {
        let base = "version = 1\n[harnesses.a]\n[routes.c]\ncouncil = true\ncandidates = [{ target = \"a/m\" }, { target = \"a/n\", spare = true }]\n";
        let c = parse(base).unwrap();
        assert_eq!(c.routes["c"].quorum(), 1);
        assert_eq!(c.routes["c"].rounds(), Some(1));
        assert!(parse(&base.replace("council = true", "council = true\nrounds = \"unlimited\"\nfixes = \"ask\"\nadvisory = \"follow-up\"\nspares = \"quorum\"")).unwrap().routes["c"].rounds().is_none());
        for text in [
            base.replace("council = true", "council = false"),
            base.replace("a/n", "self"),
            base.replace("council = true", "council = true\nquorum = 2"),
            base.replace("target = \"a/m\"", "target = \"a/m\", sizes = [\"large\"]"),
        ] {
            assert!(parse(&text).is_err(), "{text}");
        }
        for key in [
            "fixes = \"agent\"",
            "advisory = \"apply\"",
            "spares = \"all\"",
            "rounds = \"unlimited\"",
        ] {
            let text =
                format!("version = 1\n[routes.c]\n{key}\ncandidates = [{{ target = \"self\" }}]\n");
            assert!(parse(&text).is_err());
        }
    }
}
