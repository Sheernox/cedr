//! Plan usage readings and the model lists harnesses offer (docs/route.md, Readings).
//!
//! Readings are only ever supplied: a pick never starts a provider lookup of its own. Sources are
//! files, HTTPS feeds or commands that print the readings JSON; feeds are cached briefly so repeated
//! picks cost nothing, and a failed fetch falls back to the last cache, whose readings then age into
//! `unknown` instead of being treated as unused plans.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::{Config, ReaderKind, ReadingsSource, xdg_dir};

pub const MAX_BYTES: usize = 262_144;
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub enum WindowKind {
    Session,
    Weekly,
}

#[derive(Debug, Clone)]
pub struct Window {
    /// The id before any `@account` suffix.
    pub id: String,
    pub kind: WindowKind,
    pub percent: Option<f64>,
    pub resets_at: Option<DateTime<Utc>>,
    /// The family the source says this window is limited to (docs/route.md, Pool state).
    pub model: Option<String>,
    /// When this window was read, when that is earlier than its entry's `fetched_at`.
    pub fetched_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub fetched_at: Option<DateTime<Utc>>,
    pub error: bool,
    pub windows: Vec<Window>,
}

/// Readings keyed by harness id (aliases already resolved).
#[derive(Debug, Clone, Default)]
pub struct Readings {
    pub entries: HashMap<String, Entry>,
    pub models: HashMap<String, Vec<String>>,
    /// Harnesses this machine does not have, with their pool summary (docs/route.md, Machine checks).
    pub absent: HashMap<String, String>,
    /// Each harness's models' effort options, by model id (docs/route.md, Model options).
    pub efforts: HashMap<String, HashMap<String, crate::t3::Effort>>,
    /// The subscription each plan harness is signed in with, when a source names it.
    pub plans: HashMap<String, String>,
}

/// Parses an RFC 3339 timestamp; one without an offset is unusable, never assumed UTC.
pub fn parse_ts(value: &Value) -> Option<DateTime<Utc>> {
    let s = value.as_str()?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

impl Readings {
    /// Merges one readings document: the first entry per harness wins, model lists append without
    /// duplicates. Names the config does not know are ignored; malformed parts are skipped.
    pub fn merge(&mut self, config: &Config, doc: &Value) {
        let list = doc
            .get("harnesses")
            .or_else(|| doc.get("providers"))
            .and_then(Value::as_array);
        for e in list.into_iter().flatten() {
            let name = e
                .get("harness")
                .or_else(|| e.get("provider"))
                .and_then(Value::as_str);
            let Some(id) = name.and_then(|n| config.harness_for(n)) else {
                continue;
            };
            if self.entries.contains_key(id) {
                continue;
            }
            let error = match e.get("error") {
                None | Some(Value::Null) | Some(Value::Bool(false)) => false,
                Some(Value::String(s)) => !s.is_empty(),
                Some(_) => true,
            };
            let windows = e
                .get("windows")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|w| Window {
                    id: w
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .split('@')
                        .next()
                        .unwrap_or("")
                        .to_string(),
                    kind: if w.get("kind").and_then(Value::as_str) == Some("session") {
                        WindowKind::Session
                    } else {
                        WindowKind::Weekly
                    },
                    percent: w.get("percent").and_then(Value::as_f64),
                    resets_at: w.get("resetsAt").and_then(parse_ts),
                    model: w
                        .get("model")
                        .and_then(Value::as_str)
                        .filter(|m| !m.is_empty())
                        .map(str::to_string),
                    fetched_at: w.get("fetchedAt").and_then(parse_ts),
                })
                .collect();
            self.entries.insert(
                id.to_string(),
                Entry {
                    fetched_at: e.get("fetchedAt").and_then(parse_ts),
                    error,
                    windows,
                },
            );
        }
        if let Some(models) = doc.get("models").and_then(Value::as_object) {
            self.merge_models(config, models);
        }
        self.merge_efforts(config, doc);
        for (name, plan) in doc
            .get("plans")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if let (Some(id), Some(plan)) = (config.harness_for(name), plan.as_str()) {
                let plan: String = plan.chars().take(64).collect();
                if !plan.is_empty() && !plan.chars().any(char::is_control) {
                    self.plans.entry(id.to_string()).or_insert(plan);
                }
            }
        }
    }

    /// `efforts`: `{"<harness>": {"<model id>": {"option": "<id>", "values": [...]}}}`, the first
    /// per model winning. A malformed model entry is skipped.
    fn merge_efforts(&mut self, config: &Config, doc: &Value) {
        for (name, models) in doc
            .get("efforts")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let Some(id) = config.harness_for(name) else {
                continue;
            };
            let known = self.efforts.entry(id.to_string()).or_default();
            for (model, e) in models.as_object().into_iter().flatten() {
                let option = e["option"].as_str().filter(|o| !o.is_empty());
                let values: Vec<String> = e["values"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .collect();
                if let Some(option) = option
                    && !values.is_empty()
                {
                    known.entry(model.clone()).or_insert(crate::t3::Effort {
                        option: option.to_string(),
                        values,
                    });
                }
            }
        }
    }

    fn merge_models(&mut self, config: &Config, models: &serde_json::Map<String, Value>) {
        for (name, ids) in models {
            let Some(id) = config.harness_for(name) else {
                continue;
            };
            let list = self.models.entry(id.to_string()).or_default();
            for m in ids
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !list.iter().any(|x| x == m) {
                    list.push(m.to_string());
                }
            }
        }
    }

    /// Harnesses no source listed models for fall back to the config's static `models`.
    pub fn fill_models(&mut self, config: &Config) {
        for (id, h) in &config.harnesses {
            if !h.models.is_empty() && self.models.get(id).is_none_or(|m| m.is_empty()) {
                self.models.insert(id.clone(), h.models.clone());
            }
        }
    }

    /// Replaces the model lists with an orchestrator's own `{"<harness>": [ids]}` object.
    pub fn replace_models(&mut self, config: &Config, doc: &Value) -> Result<(), String> {
        let obj = doc
            .as_object()
            .ok_or("models must be a JSON object of harness to model ids")?;
        self.models.clear();
        self.merge_models(config, obj);
        Ok(())
    }
}

/// Reads every configured source in order. Warnings (failed fetches, stale caches used instead)
/// are returned for the caller to show.
pub fn collect(config: &Config, config_dir: &Path, now: DateTime<Utc>) -> (Readings, Vec<String>) {
    let mut readings = Readings::default();
    let mut warnings = Vec::new();
    // Claude Code's usage cache belongs to the one account this process sees, so it cannot tell
    // several Claude harnesses apart; those read only their status line captures.
    let claude_cache = config
        .readings
        .iter()
        .filter(|s| matches!(s, ReadingsSource::ClaudeStatusline { .. }))
        .count()
        == 1;
    for source in &config.readings {
        if covered(config, &readings, source) {
            continue;
        }
        match fetch(config, source, config_dir, now, claude_cache) {
            Ok(Fetched { doc, warning }) => {
                readings.merge(config, &doc);
                warnings.extend(warning);
            }
            Err(w) => warnings.push(w),
        }
    }
    (readings, warnings)
}

/// A built-in reader whose harness an earlier source already read would only repeat the work and
/// warn about a plan that has a reading, since the first entry wins. Codex's reader also lists
/// models, so it still runs while no source has listed them.
fn covered(config: &Config, readings: &Readings, source: &ReadingsSource) -> bool {
    let Some((kind, harness)) = source.built_in() else {
        return false;
    };
    let Some(id) = config.harness_for(harness) else {
        return false;
    };
    readings.entries.contains_key(id)
        && (kind != ReaderKind::Codex || readings.models.get(id).is_some_and(|m| !m.is_empty()))
}

/// A source's document, and a warning when it came from an old cache after a failed fetch.
struct Fetched {
    doc: Value,
    warning: Option<String>,
}

impl From<Value> for Fetched {
    fn from(doc: Value) -> Self {
        Fetched { doc, warning: None }
    }
}

/// Reads at most `limit + 1` bytes, so an oversized or endless source is refused, not buffered.
pub fn read_limited(r: impl Read, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut body = Vec::new();
    r.take(limit as u64 + 1).read_to_end(&mut body)?;
    Ok(body)
}

fn read_file(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    read_limited(std::fs::File::open(path)?, limit)
}

/// A URL without its path or query, which may carry a token, for messages.
fn url_label(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) => format!("{}://{}", u.scheme(), u.host_str().unwrap_or("?")),
        Err(_) => "the configured URL".into(),
    }
}

/// A command's program name only; its arguments may carry a token.
fn command_label(argv: &[String]) -> String {
    argv.first()
        .and_then(|p| Path::new(p).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?".into())
}

/// Parses a readings document from bytes, refusing oversized or non-object replies.
pub fn parse_doc(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("reply exceeds {MAX_BYTES} bytes"));
    }
    let doc: Value = serde_json::from_slice(bytes).map_err(|e| format!("not JSON: {e}"))?;
    if !doc.is_object() {
        return Err("readings must be a JSON object".into());
    }
    Ok(doc)
}

fn fetch(
    config: &Config,
    source: &ReadingsSource,
    config_dir: &Path,
    now: DateTime<Utc>,
    claude_cache: bool,
) -> Result<Fetched, String> {
    match source {
        ReadingsSource::File { path } => {
            let path = config_dir.join(path);
            let bytes = read_file(&path, MAX_BYTES)
                .map_err(|e| format!("readings {}: {e}", path.display()))?;
            parse_doc(&bytes)
                .map(Fetched::from)
                .map_err(|e| format!("readings {}: {e}", path.display()))
        }
        ReadingsSource::Http { url, cache_seconds } => {
            cached(&format!("http {url}"), *cache_seconds, now, || {
                http_get(url)
                    .map_err(|e| format!("readings unavailable from {} ({e})", url_label(url)))
            })
        }
        ReadingsSource::Command {
            command,
            cache_seconds,
        } => {
            // JSON keeps argv boundaries, so ["a b"] and ["a", "b"] get different caches.
            let key = format!(
                "command {}",
                serde_json::to_string(command).expect("argv serializes")
            );
            cached(&key, *cache_seconds, now, || {
                run_command(command, COMMAND_TIMEOUT).map_err(|e| {
                    format!("readings command {} failed ({e})", command_label(command))
                })
            })
        }
        ReadingsSource::T3 => {
            // T3 Code runs plans, so a key billed per token has nothing to read there.
            let harnesses: Vec<crate::readers::T3Harness> = config
                .harnesses
                .iter()
                .filter(|(_, h)| h.billing == crate::config::Billing::Plan)
                .map(|(id, h)| crate::readers::T3Harness {
                    harness: id,
                    instance: h.t3.as_deref().unwrap_or(id),
                    effort_option: h.effort_option.as_deref(),
                })
                .collect();
            let (doc, warnings) = crate::readers::t3(&crate::t3::caches(), &harnesses, now, |h| {
                config.limits(Some(h), None).fresh_seconds
            });
            Ok(Fetched {
                doc,
                warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
            })
        }
        ReadingsSource::Codex {
            harness,
            cache_seconds,
        } => cached(&format!("codex {harness}"), *cache_seconds, now, || {
            crate::readers::codex(harness, COMMAND_TIMEOUT)
                .map_err(|e| format!("codex readings unavailable ({e})"))
        }),
        ReadingsSource::Muse {
            harness,
            cache_seconds,
        } => cached(&format!("muse {harness}"), *cache_seconds, now, || {
            crate::readers::muse(harness, COMMAND_TIMEOUT)
                .map_err(|e| format!("muse readings unavailable ({e})"))
        }),
        ReadingsSource::Grok {
            harness,
            cache_seconds,
        } => cached(&format!("grok {harness}"), *cache_seconds, now, || {
            crate::readers::grok(harness, COMMAND_TIMEOUT)
                .map_err(|e| format!("grok readings unavailable ({e})"))
        }),
        ReadingsSource::ClaudeStatusline { harness } => claude(harness, claude_cache),
    }
}

/// The newer of the status line capture and Claude Code's own usage cache. Hosts that run Claude
/// Code through its SDK (no status line) only have the cache. A capture that fails is an error only
/// without a cache; a cache that fails beside a capture is a warning, since it alone has model windows.
fn claude(harness: &str, use_cache: bool) -> Result<Fetched, String> {
    let path = crate::readers::statusline_path(harness);
    let capture = match read_file(&path, MAX_BYTES) {
        Ok(bytes) => parse_doc(&bytes)
            .map(Some)
            .map_err(|e| format!("readings {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("readings {}: {e}", path.display())),
    };
    let cache = if use_cache {
        crate::readers::claude_usage(harness)
            .map_err(|e| format!("claude readings unavailable ({e})"))
    } else {
        Ok(None)
    };
    let fetched = |d: &Value| parse_ts(&d["harnesses"][0]["fetchedAt"]);
    match (capture, cache) {
        (Ok(Some(c)), Ok(Some(u))) => {
            let (mut newer, older) = if fetched(&u) > fetched(&c) {
                (u, c)
            } else {
                (c, u)
            };
            // The capture has no model windows, so the newer document keeps the other's windows
            // it lacks rather than dropping a limit. Each keeps its own `fetchedAt`, so it ages
            // into unknown under whatever freshness threshold the pick applies.
            let older_at = older["harnesses"][0]["fetchedAt"].clone();
            let extra: Vec<Value> = older["harnesses"][0]["windows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|w| {
                    !newer["harnesses"][0]["windows"]
                        .as_array()
                        .is_some_and(|ws| ws.iter().any(|n| n["id"] == w["id"]))
                })
                .map(|w| {
                    let mut w = w.clone();
                    w["fetchedAt"] = older_at.clone();
                    w
                })
                .collect();
            if let Some(ws) = newer["harnesses"][0]["windows"].as_array_mut() {
                ws.extend(extra);
            }
            Ok(newer.into())
        }
        // Model windows come only from the cache, so losing it is worth saying.
        (Ok(Some(c)), Err(e)) => Ok(Fetched {
            doc: c,
            warning: Some(format!("{e}; model windows are missing")),
        }),
        (Ok(Some(d)), _) | (_, Ok(Some(d))) => Ok(d.into()),
        // Neither a capture nor a cache yet: no reading (`cedr doctor` says why).
        (Ok(None), Ok(None)) => Ok(json!({}).into()),
        (Err(e), _) | (_, Err(e)) => Err(e),
    }
}

fn cache_path(key: &str) -> PathBuf {
    let digest = Sha256::digest(key.as_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    xdg_dir("XDG_CACHE_HOME", ".cache")
        .join("cedr")
        .join(format!("readings-{hex}.json"))
}

/// A fresh cache, else the fetch (cached on success), else the last cache with a warning.
fn cached(
    key: &str,
    cache_seconds: i64,
    now: DateTime<Utc>,
    get: impl FnOnce() -> Result<Value, String>,
) -> Result<Fetched, String> {
    let path = cache_path(key);
    // The cache wraps one document, so twice the document cap bounds it.
    let old: Option<Value> = read_file(&path, 2 * MAX_BYTES)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let old_data = old
        .as_ref()
        .and_then(|o| o.get("data"))
        .filter(|d| d.is_object());
    if let (Some(at), Some(data)) = (
        old.as_ref()
            .and_then(|o| o.get("cachedAt"))
            .and_then(parse_ts),
        old_data,
    ) {
        let age = (now - at).num_seconds();
        if (0..cache_seconds).contains(&age) {
            return Ok(data.clone().into());
        }
    }
    match get() {
        Ok(data) => {
            // A cache that cannot be written only costs a refetch next time.
            if let Some(dir) = path.parent()
                && std::fs::create_dir_all(dir).is_ok()
            {
                let tmp = path.with_extension("tmp");
                let body = json!({"cachedAt": now.to_rfc3339(), "data": data});
                if std::fs::write(&tmp, body.to_string()).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
            Ok(data.into())
        }
        Err(e) => match old_data {
            Some(data) => Ok(Fetched {
                doc: data.clone(),
                warning: Some(format!("{e}; using the last cached readings")),
            }),
            None => Err(e),
        },
    }
}

fn http_get(url: &str) -> Result<Value, String> {
    if !url.starts_with("https://") {
        return Err("only https:// readings URLs are allowed".into());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .https_only(true)
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(crate::sync::request_error)?;
    let body = read_limited(response, MAX_BYTES)
        .map_err(|_| "readings response could not be read within the size limit".to_string())?;
    parse_doc(&body)
}

/// Runs a readings command with one deadline for both its exit and its output. A descendant that
/// keeps stdout open cannot stall the pick: the reader thread is left behind and ends when the
/// pipe closes.
fn run_command(argv: &[String], timeout: Duration) -> Result<Value, String> {
    parse_doc(&run_capture(argv, &[], timeout)?)
}

/// Runs `argv` with extra environment `env` and returns its stdout (capped), with one deadline for
/// both its exit and its output; a non-zero exit is an error.
pub(crate) fn run_capture(
    argv: &[String],
    env: &[(&str, &str)],
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    run_capture_at(argv, env, timeout, None)
}

/// Built-in account probes choose a directory outside the current repository.
pub(crate) fn run_capture_at(
    argv: &[String],
    env: &[(&str, &str)],
    timeout: Duration,
    cwd: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let (program, args) = argv.split_first().ok_or("empty command")?;
    let mut command = Command::new(program);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_limited(stdout, MAX_BYTES));
    });
    let deadline = Instant::now() + timeout;
    let timed_out = || format!("timed out after {} ms", timeout.as_millis());
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(timed_out());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let body = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| format!("{} waiting for its output", timed_out()))?
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("exited with {status}"));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    #[test]
    fn failed_http_readings_never_disclose_url_credentials() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = super::http_get(&format!(
            "https://{address}/private-path?token=SYNTHETIC_SECRET#fragment"
        ))
        .unwrap_err();
        for private in ["SYNTHETIC_SECRET", "private-path", "token=", "fragment"] {
            assert!(!error.contains(private), "{error}");
        }
        assert!(!error.is_empty());
    }
    use super::*;

    fn config() -> Config {
        Config::parse(
            "version = 1\n[harnesses.claude]\naliases = [\"claudeAgent\"]\n[harnesses.codex]\n",
            Path::new("t.toml"),
        )
        .unwrap()
    }

    #[test]
    fn the_codex_reader_runs_until_an_earlier_source_also_listed_its_models() {
        let c = config();
        let codex = ReadingsSource::Codex {
            harness: "codex".into(),
            cache_seconds: 60,
        };
        let mut r = Readings::default();
        assert!(!covered(&c, &r, &codex));
        r.merge(
            &c,
            &json!({"harnesses": [{"harness": "codex", "fetchedAt": "2026-10-29T01:00:00Z", "windows": []}]}),
        );
        assert!(!covered(&c, &r, &codex), "its model list is still to come");
        r.merge(&c, &json!({"models": {"codex": ["gpt-6.1-sol"]}}));
        assert!(covered(&c, &r, &codex));
    }

    #[test]
    fn merges_provider_named_feeds_and_strips_account_suffixes() {
        let mut r = Readings::default();
        r.merge(
            &config(),
            &json!({
                "providers": [
                    {"provider": "claudeAgent", "fetchedAt": "2026-10-29T01:00:00Z", "error": null,
                     "windows": [{"id": "five_hour@acct", "kind": "session", "percent": 5, "resetsAt": "2026-10-29T06:00:00Z"}]},
                    {"provider": "grok", "windows": []}
                ],
                "models": {"claudeAgent": ["claude-opus-5-5"], "codex": ["gpt-6.1-sol"]}
            }),
        );
        let e = &r.entries["claude"];
        assert_eq!(e.windows[0].id, "five_hour");
        assert_eq!(e.windows[0].kind, WindowKind::Session);
        assert!(!e.error);
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.models["codex"], vec!["gpt-6.1-sol"]);
    }

    #[test]
    fn static_models_fill_only_harnesses_without_a_list() {
        let c = Config::parse(
            "version = 1\n[harnesses.claude]\nmodels = [\"claude-opus-5-5\"]\n[harnesses.codex]\nmodels = [\"static\"]\n",
            Path::new("t.toml"),
        )
        .unwrap();
        let mut r = Readings::default();
        r.merge(&c, &json!({"models": {"codex": ["gpt-6.1-sol"]}}));
        r.fill_models(&c);
        assert_eq!(r.models["claude"], ["claude-opus-5-5"]);
        assert_eq!(r.models["codex"], ["gpt-6.1-sol"]);
    }

    #[test]
    fn first_source_wins_per_harness() {
        let mut r = Readings::default();
        let c = config();
        r.merge(&c, &json!({"harnesses": [{"harness": "codex", "error": "boom"}], "models": {"codex": ["a"]}}));
        r.merge(&c, &json!({"harnesses": [{"harness": "codex", "error": null}], "models": {"codex": ["a", "b"]}}));
        assert!(r.entries["codex"].error);
        assert_eq!(r.models["codex"], vec!["a", "b"]);
    }

    #[test]
    fn timestamps_without_offset_are_unusable() {
        assert!(parse_ts(&json!("2026-10-29T01:00:00")).is_none());
        assert!(parse_ts(&json!("2026-10-29T01:00:00.123+00:00")).is_some());
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    #[test]
    fn commands_return_their_document() {
        let _serial = crate::spawn_lock();
        let doc = run_command(&sh("printf '{\"harnesses\": []}'"), Duration::from_secs(5)).unwrap();
        assert_eq!(doc, json!({"harnesses": []}));
        assert!(
            run_command(&sh("exit 3"), Duration::from_secs(5))
                .unwrap_err()
                .contains("exited")
        );
    }

    #[test]
    fn a_descendant_holding_stdout_cannot_outlast_the_deadline() {
        let _serial = crate::spawn_lock();
        let started = Instant::now();
        let err =
            run_command(&sh("(sleep 5) & printf '{}'"), Duration::from_millis(300)).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn messages_never_show_url_paths_or_command_arguments() {
        assert_eq!(
            url_label("https://feed.example/limits?token=s3cret"),
            "https://feed.example"
        );
        assert_eq!(
            command_label(&["/usr/bin/probe".into(), "--key=s3cret".into()]),
            "probe"
        );
    }

    #[test]
    fn refuses_oversized_and_non_object_documents() {
        assert!(parse_doc(&vec![b' '; MAX_BYTES + 1]).is_err());
        assert!(parse_doc(b"[]").is_err());
    }
}
