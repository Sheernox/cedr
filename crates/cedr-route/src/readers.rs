//! Built-in readings (docs/route.md, Built-in readers). Each goes through
//! the vendor's own CLI or what it hands its user's scripts: Cedr never reads credentials or calls
//! a vendor API itself.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value, json};

use crate::config::xdg_dir;
use crate::readings::MAX_BYTES;

/// Windows at most this long are session windows; longer ones are weekly.
const SESSION_MAX_MINUTES: i64 = 300;

// Account probes must not load configuration from an untrusted checkout.
fn probe_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|p| !p.is_empty())
        .map_or_else(std::env::temp_dir, PathBuf::from)
}

fn stamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn epoch(v: &Value) -> Option<String> {
    v.as_i64()
        .and_then(|s| DateTime::<Utc>::from_timestamp(s, 0))
        .map(stamp)
}

/// Asks `codex app-server` for the plan's rate limits and the models it offers, within `timeout`.
pub fn codex(harness: &str, timeout: Duration) -> Result<Value, String> {
    codex_with("codex", harness, timeout)
}

fn codex_with(program: &str, harness: &str, timeout: Duration) -> Result<Value, String> {
    let mut child = Command::new(program)
        .arg("app-server")
        .current_dir(probe_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {program} app-server ({e})"))?;
    let result = converse(&mut child, timeout);
    let _ = child.kill();
    let _ = child.wait();
    let (limits, models) = result?;
    let mut windows = Vec::new();
    let snapshot = limits.get("rateLimits").cloned().unwrap_or(Value::Null);
    for id in ["primary", "secondary"] {
        let Some(w) = snapshot.get(id).filter(|w| w.is_object()) else {
            continue;
        };
        let minutes = w.get("windowDurationMins").and_then(Value::as_i64);
        let session = minutes.is_some_and(|m| m <= SESSION_MAX_MINUTES);
        windows.push(json!({
            "id": id,
            "kind": if session { "session" } else { "weekly" },
            "percent": w.get("usedPercent").and_then(Value::as_f64),
            "resetsAt": w.get("resetsAt").and_then(epoch),
        }));
    }
    let ids: Vec<Value> = models
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| m.get("hidden").and_then(Value::as_bool) != Some(true))
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(|id| Value::String(id.to_string()))
        .collect();
    let mut model_map = Map::new();
    model_map.insert(harness.to_string(), Value::Array(ids));
    Ok(json!({
        "harnesses": [{"harness": harness, "fetchedAt": stamp(Utc::now()), "error": null, "windows": windows}],
        "models": model_map,
    }))
}

/// Lines of a child's stdout on a channel, so a reader can wait on them with a deadline.
fn stdout_lines(stdout: std::process::ChildStdout) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut total = 0;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            total += line.len();
            // A runaway server is cut off rather than buffered without end.
            if total > MAX_BYTES * 4 || tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// The JSON-RPC exchange: initialize, then the two reads; returns their results.
fn converse(child: &mut std::process::Child, timeout: Duration) -> Result<(Value, Value), String> {
    let deadline = Instant::now() + timeout;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let rx = stdout_lines(stdout);
    let mut send = |v: Value| {
        writeln!(stdin, "{v}")
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("app-server closed its input ({e})"))
    };
    send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"clientInfo": {"name": "cedr", "version": env!("CARGO_PKG_VERSION")}}}))?;
    let (mut limits, mut models) = (None, None);
    while limits.is_none() || models.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = rx
            .recv_timeout(left)
            .map_err(|_| format!("no answer within {} ms", timeout.as_millis()))?;
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(err) = msg.get("error") {
            let what = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("error");
            return Err(format!("app-server refused request {} ({what})", msg["id"]));
        }
        match msg.get("id").and_then(Value::as_i64) {
            Some(1) => {
                send(json!({"jsonrpc": "2.0", "method": "initialized"}))?;
                send(json!({"jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read"}))?;
                send(json!({"jsonrpc": "2.0", "id": 3, "method": "model/list", "params": {}}))?;
            }
            Some(2) => limits = msg.get("result").cloned(),
            Some(3) => models = msg.get("result").cloned(),
            _ => {}
        }
    }
    Ok((limits.unwrap_or_default(), models.unwrap_or_default()))
}

/// Asks `muse usage --json` (Muse Code's own CLI and login) for the plan's allowance. Its format is
/// undocumented (schemaVersion 1, read from Muse Code 1.4.13): `allowance.window` is the 5-hour
/// session and `allowance.weekly` the week, each with `usedPercent` and `resetsAtMs`. No allowance yet
/// (before the first turn of a window) is an error, so the plan reads as unknown, never as unused.
pub fn muse(harness: &str, timeout: Duration) -> Result<Value, String> {
    muse_with("muse", harness, timeout)
}

fn muse_with(program: &str, harness: &str, timeout: Duration) -> Result<Value, String> {
    let argv = [program.to_string(), "usage".into(), "--json".into()];
    // Never let a probe replace the installed Muse Code.
    let out = crate::readings::run_capture_at(
        &argv,
        &[("MUSE_NO_AUTO_UPDATE", "1")],
        timeout,
        Some(&probe_dir()),
    )?;
    let reply: Value = serde_json::from_slice(&out).map_err(|_| {
        "muse usage printed no JSON (no usage data yet, or a newer format)".to_string()
    })?;
    if reply.get("schemaVersion").and_then(Value::as_i64) != Some(1) {
        return Err("muse usage changed its report format".into());
    }
    let allowance = reply
        .get("allowance")
        .filter(|a| a.is_object())
        .ok_or("Muse has no usage data for this window yet; it appears after the next Muse turn")?;
    let mut windows = Vec::new();
    for (id, kind) in [("window", "session"), ("weekly", "weekly")] {
        let w = allowance
            .get(id)
            .filter(|w| w.is_object())
            .ok_or(format!("muse usage has no {id} allowance"))?;
        let reset = w
            .get("resetsAtMs")
            .and_then(Value::as_i64)
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .map(stamp);
        windows.push(json!({"id": id, "kind": kind, "percent": w.get("usedPercent").and_then(Value::as_f64), "resetsAt": reset}));
    }
    Ok(
        json!({"harnesses": [{"harness": harness, "fetchedAt": stamp(Utc::now()), "error": null, "windows": windows}]}),
    )
}

/// Asks `grok agent stdio` (Grok Build's own CLI and login) for the plan's allowance. `_x.ai/billing`
/// is undocumented (read from Grok Build 1.0.70) and needs no session or model turn. The pool is
/// shared with the vendor's other products. No period yet or another format is an error, so the plan
/// reads as unknown, never as unused.
pub fn grok(harness: &str, timeout: Duration) -> Result<Value, String> {
    grok_with("grok", harness, timeout)
}

fn grok_with(program: &str, harness: &str, timeout: Duration) -> Result<Value, String> {
    let mut child = Command::new(program)
        .args(["agent", "stdio"])
        // Never let a probe replace the installed Grok Build.
        .env("GROK_DISABLE_AUTOUPDATER", "1")
        .current_dir(probe_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {program} agent stdio ({e})"))?;
    let reply = grok_billing(&mut child, timeout);
    let _ = child.kill();
    let _ = child.wait();
    grok_doc(&reply?, harness)
}

fn grok_billing(child: &mut std::process::Child, timeout: Duration) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let rx = stdout_lines(child.stdout.take().ok_or("no stdout")?);
    for request in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": 1, "clientCapabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "_x.ai/billing", "params": {}}),
    ] {
        writeln!(stdin, "{request}")
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("agent closed its input ({e})"))?;
    }
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = rx.recv_timeout(left).map_err(|e| match e {
            mpsc::RecvTimeoutError::Timeout => {
                format!("no answer within {} ms", timeout.as_millis())
            }
            mpsc::RecvTimeoutError::Disconnected => {
                "agent exited before answering; check that grok is signed in".to_string()
            }
        })?;
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if msg.get("id").and_then(Value::as_i64) != Some(2) {
            continue;
        }
        return msg
            .get("result")
            .filter(|r| r.is_object())
            .cloned()
            .ok_or_else(|| "agent refused the billing request".to_string());
    }
}

fn grok_doc(result: &Value, harness: &str) -> Result<Value, String> {
    let config = &result["config"];
    let period = config
        .get("currentPeriod")
        .filter(|p| p.is_object())
        .ok_or("Grok has no billing period yet; it appears after the plan's first use")?;
    if period["type"] != "USAGE_PERIOD_TYPE_WEEKLY" {
        return Err("grok changed its usage report format".into());
    }
    let when = |v: &Value| {
        v.as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
    };
    let percent = config["creditUsagePercent"]
        .as_f64()
        .filter(|p| p.is_finite() && *p >= 0.0);
    let (Some(percent), Some(start), Some(end)) =
        (percent, when(&period["start"]), when(&period["end"]))
    else {
        return Err("grok returned an invalid allowance".into());
    };
    if end <= start {
        return Err("grok returned an invalid allowance".into());
    }
    let window = json!({
        "id": "weekly",
        "kind": "weekly",
        "percent": percent,
        "resetsAt": stamp(end),
        "windowMinutes": (end - start).num_minutes(),
    });
    Ok(
        json!({"harnesses": [{"harness": harness, "fetchedAt": stamp(Utc::now()), "error": null, "windows": [window]}]}),
    )
}

/// Where `cedr route statusline` keeps the last capture for a harness.
pub fn statusline_path(harness: &str) -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state")
        .join("cedr")
        .join("readings")
        .join(format!("{harness}.json"))
}

/// Turns Claude Code's status line input into a readings document, or none when it carries no
/// `rate_limits` (API-key sessions, or before a session's first response).
pub fn from_statusline(input: &Value, harness: &str, now: DateTime<Utc>) -> Option<Value> {
    let limits = input.get("rate_limits")?.as_object()?;
    let mut windows = Vec::new();
    for (id, kind) in [("five_hour", "session"), ("seven_day", "weekly")] {
        let Some(w) = limits.get(id).filter(|w| w.is_object()) else {
            continue;
        };
        windows.push(json!({
            "id": id,
            "kind": kind,
            "percent": w.get("used_percentage").and_then(Value::as_f64),
            "resetsAt": w.get("resets_at").and_then(epoch),
        }));
    }
    Some(
        json!({"harnesses": [{"harness": harness, "fetchedAt": stamp(now), "error": null, "windows": windows}]}),
    )
}

/// Claude Code's own config file, where it caches the plan usage its `/usage` view shows. None
/// without a home directory.
pub fn claude_config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join(".claude.json"));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".claude.json"))
}

/// The plan usage Claude Code last cached, as a readings document. Ok(None) when there is no
/// config file or no cache in it yet; an error when either is unreadable.
pub fn claude_usage(harness: &str) -> Result<Option<Value>, String> {
    let Some(path) = claude_config_path() else {
        return Ok(None);
    };
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    // Only the two fields are kept: the rest of the file (projects, MCP server settings with their
    // tokens) is streamed past while parsing, never held, so its size does not matter.
    #[derive(serde::Deserialize)]
    struct Fields {
        #[serde(rename = "oauthAccount", default)]
        account: Value,
        #[serde(rename = "cachedUsageUtilization", default)]
        cache: Value,
    }
    let fields: Fields = serde_json::from_reader(std::io::BufReader::new(file)).map_err(|e| {
        let what = if e.is_data() {
            "is not a Claude Code config object"
        } else {
            "is not JSON"
        };
        format!("{} {what}", path.display())
    })?;
    let account = fields.account["accountUuid"].clone();
    from_usage_cache(
        &json!({"oauthAccount": {"accountUuid": account}, "cachedUsageUtilization": fields.cache}),
        harness,
    )
}

/// Turns `cachedUsageUtilization` from Claude Code's config into a readings document. Its format
/// is undocumented (read from Claude Code 2.1.318): `utilization.five_hour` and
/// `utilization.seven_day` carry `utilization` (percent) and `resets_at` (RFC 3339), and
/// `utilization.limits` lists model-scoped weekly limits (`kind` `weekly_scoped`, named by
/// `scope.model.display_name`). A cache is used only when it names the account signed in
/// (`oauthAccount.accountUuid`), so it never passes off another account's, or a signed-out, plan.
pub fn from_usage_cache(config: &Value, harness: &str) -> Result<Option<Value>, String> {
    let Some(cache) = config
        .get("cachedUsageUtilization")
        .filter(|c| !c.is_null())
    else {
        return Ok(None);
    };
    let changed = || "Claude Code changed its usage cache format".to_string();
    let cached = cache["accountUuid"]
        .as_str()
        .filter(|a| !a.is_empty())
        .ok_or_else(changed)?;
    if config["oauthAccount"]["accountUuid"].as_str() != Some(cached) {
        return Ok(None);
    }
    let fetched = cache["fetchedAtMs"]
        .as_i64()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .ok_or_else(changed)?;
    let usage = cache
        .get("utilization")
        .filter(|u| u.is_object())
        .ok_or_else(changed)?;
    let window = |id: String, kind: &str, w: &Value, percent: &str| {
        json!({
            "id": id,
            "kind": kind,
            "percent": w[percent].as_f64(),
            "resetsAt": crate::readings::parse_ts(&w["resets_at"]).map(stamp),
        })
    };
    let mut windows = Vec::new();
    for (id, kind) in [("five_hour", "session"), ("seven_day", "weekly")] {
        if let Some(w) = usage.get(id).filter(|w| w.is_object()) {
            windows.push(window(id.to_string(), kind, w, "utilization"));
        }
    }
    for l in usage["limits"].as_array().into_iter().flatten() {
        let Some(model) = l["scope"]["model"]["display_name"].as_str() else {
            continue;
        };
        if l["kind"] != "weekly_scoped" {
            continue;
        }
        // The family is spelled like model ids ("Opus 4.5" -> "opus-4-5", found in
        // "claude-opus-4-5"); the window id like the status line's ("seven_day_opus_4_5").
        let family = model
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join("-")
            .to_ascii_lowercase();
        if family.is_empty() {
            continue;
        }
        let mut w = window(
            format!("seven_day_{}", family.replace('-', "_")),
            "weekly",
            l,
            "percent",
        );
        w["model"] = Value::String(family);
        windows.push(w);
    }
    if windows.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        json!({"harnesses": [{"harness": harness, "fetchedAt": stamp(fetched), "error": null, "windows": windows}]}),
    ))
}

/// Where T3 Code keeps what it last read of each provider instance it runs: `$T3CODE_HOME/caches`
/// (default `~/.t3/caches`), one `<instance id>.json` each.
pub fn t3_caches() -> PathBuf {
    xdg_dir("T3CODE_HOME", ".t3").join("caches")
}

/// A provider cache holds a provider's models, skills and commands; one past this is not one.
const T3_MAX_BYTES: usize = 4 << 20;
/// More windows or model families than any plan has; the rest are ignored, so a hostile cache
/// cannot multiply its windows by its families.
const T3_MAX_WINDOWS: usize = 64;
const T3_MAX_FAMILIES: usize = 16;

/// What T3 Code last read of each plan, for `(harness, T3 instance id)` pairs, as a readings
/// document, plus a warning per cache that could not be read. T3 Code keeps these readings for its
/// own UI from the sessions it runs and the vendors' own CLIs and SDKs, so reading them runs nothing
/// and reads no credentials. A plan T3 Code has no current reading for (no cache, disabled, no or
/// unavailable limits, or one older than `fresh(harness)` seconds) is left out, so a later source
/// supplies it. The cache format is T3 Code's own and undocumented: the provider's `driver`,
/// `enabled`, `models[].slug` and `usageLimits` (`checkedAt`, `windows[]` with `id`, `kind`,
/// `usedPercent`, `resetsAt`, `windowDurationMins`); the rest of the file is skipped.
pub fn t3(
    dir: &Path,
    instances: &[(&str, &str)],
    now: DateTime<Utc>,
    fresh: impl Fn(&str) -> i64,
) -> (Value, Vec<String>) {
    #[derive(serde::Deserialize)]
    struct Provider {
        #[serde(default)]
        driver: String,
        #[serde(default = "enabled")]
        enabled: bool,
        #[serde(default)]
        models: Value,
        #[serde(rename = "usageLimits", default)]
        usage: Value,
    }
    fn enabled() -> bool {
        true
    }
    let mut entries = Vec::new();
    let mut models = Map::new();
    let mut warnings = Vec::new();
    for &(harness, instance) in instances {
        // The instance id comes from the routes, which may be shared: it names a file in `dir`, never a path.
        let safe = !instance.is_empty()
            && !instance.starts_with('.')
            && instance
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
        if !safe {
            continue;
        }
        let path = dir.join(format!("{instance}.json"));
        let bytes = match std::fs::File::open(&path) {
            Ok(f) => match crate::readings::read_limited(f, T3_MAX_BYTES) {
                Ok(b) if b.len() <= T3_MAX_BYTES => b,
                Ok(_) => {
                    warnings.push(format!(
                        "t3 readings: {instance} cache exceeds {T3_MAX_BYTES} bytes"
                    ));
                    continue;
                }
                Err(e) => {
                    warnings.push(format!("t3 readings: {}: {e}", path.display()));
                    continue;
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                warnings.push(format!("t3 readings: {}: {e}", path.display()));
                continue;
            }
        };
        let Ok(p) = serde_json::from_slice::<Provider>(&bytes) else {
            warnings.push(format!(
                "t3 readings: {instance} is not a T3 Code provider cache, or T3 Code changed its format"
            ));
            continue;
        };
        if !p.enabled {
            continue;
        }
        let offered: Vec<String> = p
            .models
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["slug"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if !offered.is_empty() {
            models.insert(harness.to_string(), json!(offered));
        }
        let failed = |v: &Value| !v.is_null() && v != &json!(false) && v != &json!("");
        if failed(&p.usage["unavailable"]) || failed(&p.usage["error"]) {
            continue;
        }
        let Some(checked) = crate::readings::parse_ts(&p.usage["checkedAt"]) else {
            continue;
        };
        // Judged to the millisecond, as the pool judges it, so a reading the pool calls stale never
        // covers the sources after it.
        if (now - checked).num_milliseconds() as f64 / 1000.0 > fresh(harness) as f64 {
            continue;
        }
        let windows = t3_windows(&p.driver, &p.usage["windows"], &offered);
        if windows.is_empty() {
            continue;
        }
        let fetched = checked.to_rfc3339_opts(SecondsFormat::Millis, true);
        entries.push(
            json!({"harness": harness, "fetchedAt": fetched, "error": null, "windows": windows}),
        );
    }
    (json!({"harnesses": entries, "models": models}), warnings)
}

/// T3 Code's windows for one provider in the readings format. Window ids stay T3's, except where
/// a built-in reader names the same window otherwise, so routes that expect one accept the other.
/// Windows that limit only some models carry `model`: Claude's `seven_day_<family>`, and
/// Antigravity's Argon pool and its pools for the other vendors' models it offers (`3p-*`, one
/// window per family). Antigravity's `gemini-*` windows stay shared, as a feed sends them.
fn t3_windows(driver: &str, raw: &Value, offered: &[String]) -> Vec<Value> {
    // The families of the other vendors' models Antigravity offers, for its `3p-*` windows.
    let others: std::collections::BTreeSet<&str> = offered
        .iter()
        .filter(|m| !m.contains("gemini"))
        .filter_map(|m| m.split('-').next())
        .filter(|f| !f.is_empty())
        .take(T3_MAX_FAMILIES)
        .collect();
    let mut out = Vec::new();
    for w in raw.as_array().into_iter().flatten().take(T3_MAX_WINDOWS) {
        let Some(id) = w["id"].as_str().filter(|i| !i.is_empty()) else {
            continue;
        };
        let Some(percent) = w["usedPercent"]
            .as_f64()
            .filter(|p| p.is_finite() && *p >= 0.0)
        else {
            continue;
        };
        let minutes = w["windowDurationMins"].as_i64().filter(|m| *m > 0);
        let kind = match (w["kind"].as_str(), minutes) {
            (Some("session"), _) => "session",
            (Some("weekly"), _) => "weekly",
            (_, Some(m)) if m <= SESSION_MAX_MINUTES => "session",
            _ => "weekly",
        };
        let window = |id: &str, model: Option<&str>| {
            let mut v = json!({
                "id": id,
                "kind": kind,
                "percent": percent,
                "resetsAt": crate::readings::parse_ts(&w["resetsAt"]).map(stamp),
                "windowMinutes": minutes,
            });
            if let Some(m) = model {
                v["model"] = json!(m);
            }
            v
        };
        let bare = id.split('@').next().unwrap_or(id);
        match driver {
            "claudeAgent" => {
                let family = bare
                    .strip_prefix("seven_day_")
                    .map(|f| f.replace('_', "-"))
                    .filter(|f| !f.is_empty());
                out.push(window(id, family.as_deref()));
            }
            "grok" if bare == "subscription" => out.push(window("weekly", None)),
            "antigravity" if bare.starts_with("gemini-argon") => {
                out.push(window(id, Some("argon")))
            }
            "antigravity" if bare.starts_with("3p-") => {
                // Without a model list there is no family to scope them to, and shared they
                // would hold Gemini models to the other vendors' use.
                for f in &others {
                    out.push(window(&format!("{bare}-{f}"), Some(f)));
                }
            }
            _ => out.push(window(id, None)),
        }
    }
    out
}

/// Writes a capture atomically, so a pick never reads half a file.
pub fn save_capture(path: &Path, doc: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, path)
}

/// The short status line text: `claude s23% w41%`.
pub fn statusline_text(harness: &str, doc: Option<&Value>) -> String {
    let windows: Vec<String> = doc
        .and_then(|d| d["harnesses"][0]["windows"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|w| {
            let tag = if w["kind"] == "session" { "s" } else { "w" };
            w["percent"]
                .as_f64()
                .map(|p| format!("{tag}{}%", crate::pool::round_pct(p)))
        })
        .collect();
    if windows.is_empty() {
        format!("{harness} no limits yet")
    } else {
        format!("{harness} {}", windows.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-29T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn statusline_rate_limits_become_session_and_weekly_windows() {
        let input = json!({"model": {"id": "claude-opus-5-5"}, "rate_limits": {
            "five_hour": {"used_percentage": 23.5, "resets_at": 1793800000},
            "seven_day": {"used_percentage": 41.2, "resets_at": 1794300000}}});
        let doc = from_statusline(&input, "claude", now()).unwrap();
        let e = &doc["harnesses"][0];
        assert_eq!(e["fetchedAt"], "2026-10-29T02:00:00Z");
        assert_eq!(
            e["windows"][0],
            json!({"id": "five_hour", "kind": "session", "percent": 23.5, "resetsAt": "2026-11-04T13:46:40Z"})
        );
        assert_eq!(e["windows"][1]["id"], "seven_day");
        assert_eq!(statusline_text("claude", Some(&doc)), "claude s24% w41%");
    }

    #[test]
    fn statusline_without_rate_limits_captures_nothing() {
        assert!(from_statusline(&json!({"model": {}}), "claude", now()).is_none());
        assert_eq!(statusline_text("claude", None), "claude no limits yet");
    }

    /// A T3 Code provider cache, trimmed to what the reader uses plus a field it must skip.
    fn t3_cache(
        driver: &str,
        enabled: bool,
        checked: &str,
        models: &[&str],
        windows: Value,
    ) -> String {
        json!({"driver": driver, "instanceId": driver, "enabled": enabled, "auth": {"email": "a@example.invalid"},
               "models": models.iter().map(|m| json!({"slug": m, "name": m})).collect::<Vec<_>>(),
               "usageLimits": {"checkedAt": checked, "windows": windows}})
        .to_string()
    }

    #[test]
    fn t3_code_caches_become_readings_scoped_like_the_built_in_readers() {
        let root = tempfile::tempdir().unwrap();
        let caches = root.path().join("caches");
        std::fs::create_dir(&caches).unwrap();
        let write = |name: &str, body: String| std::fs::write(caches.join(name), body).unwrap();
        write(
            "claudeAgent.json",
            t3_cache(
                "claudeAgent",
                true,
                "2026-10-29T01:58:00.120Z",
                &["claude-opus-5-5", "claude-fable-5-1"],
                json!([
                {"id": "five_hour", "kind": "session", "usedPercent": 8, "windowDurationMins": 300, "resetsAt": "2026-10-29T06:00:00.119Z"},
                {"id": "seven_day", "kind": "weekly", "usedPercent": 49, "windowDurationMins": 10080, "resetsAt": "2026-11-02T04:00:00Z"},
                {"id": "seven_day_fable", "kind": "weekly", "usedPercent": 0, "windowDurationMins": 10080}]),
            ),
        );
        write(
            "grok.json",
            t3_cache(
                "grok",
                true,
                "2026-10-29T01:59:00Z",
                &["grok-4.7"],
                json!([{"id": "subscription", "kind": "weekly", "usedPercent": 69, "resetsAt": "2026-11-01T20:13:30Z"}]),
            ),
        );
        write(
            "antigravity.json",
            t3_cache(
                "antigravity",
                true,
                "2026-10-29T01:59:00Z",
                &[
                    "gemini-4-argon-high",
                    "gemini-3.8-flash-high",
                    "claude-opus-5-5-low",
                    "gpt-oss-120b-medium",
                ],
                json!([
                {"id": "3p-5h", "kind": "session", "usedPercent": 4, "windowDurationMins": 300},
                {"id": "gemini-5h", "kind": "session", "usedPercent": 0, "windowDurationMins": 300},
                {"id": "gemini-argon-wtu-weekly", "kind": "weekly", "usedPercent": 92.01, "resetsAt": "2026-11-02T06:32:12Z"},
                {"id": "gemini-weekly", "kind": "weekly", "usedPercent": "n/a"}]),
            ),
        );
        // Stale, disabled, unavailable and malformed caches give no reading, so a later source can.
        write(
            "codex.json",
            t3_cache(
                "codex",
                true,
                "2026-10-29T01:40:00Z",
                &["gpt-6.1-sol"],
                json!([{"id": "primary", "kind": "weekly", "usedPercent": 9}]),
            ),
        );
        write(
            "muse.json",
            t3_cache(
                "muse",
                false,
                "2026-10-29T01:59:00Z",
                &["muse-wonder-1.0"],
                json!([{"id": "weekly", "kind": "weekly", "usedPercent": 9}]),
            ),
        );
        write("work.json", json!({"driver": "claudeAgent", "models": [],
            "usageLimits": {"checkedAt": "2026-10-29T01:59:00Z", "unavailable": {"reason": "signed out"}, "windows": []}}).to_string());
        write("broken.json", "{\"models\": {\"a\": 1".into());
        // An instance id from shared routes never reaches outside the caches directory.
        write(
            "../outside.json",
            t3_cache(
                "grok",
                true,
                "2026-10-29T01:59:00Z",
                &[],
                json!([{"id": "subscription", "kind": "weekly", "usedPercent": 1}]),
            ),
        );
        // Just past fresh_seconds, by half a second.
        write(
            "late.json",
            t3_cache(
                "grok",
                true,
                "2026-10-29T01:49:59.500Z",
                &[],
                json!([{"id": "subscription", "kind": "weekly", "usedPercent": 1}]),
            ),
        );
        // A hostile cache: many families times many `3p-*` windows stays bounded.
        let many: Vec<String> = (0..100).map(|i| format!("v{i}-model")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let windows: Vec<Value> = (0..100)
            .map(|i| json!({"id": format!("3p-{i}"), "kind": "weekly", "usedPercent": 1}))
            .collect();
        write(
            "many.json",
            t3_cache(
                "antigravity",
                true,
                "2026-10-29T01:59:00Z",
                &many,
                json!(windows),
            ),
        );
        let instances = [
            ("late", "late"),
            ("many", "many"),
            ("claude", "claudeAgent"),
            ("grok", "grok"),
            ("antigravity", "antigravity"),
            ("codex", "codex"),
            ("muse", "muse"),
            ("claude-work", "work"),
            ("x", "broken"),
            ("y", "../outside"),
            ("z", "missing"),
        ];
        let (doc, warnings) = t3(&caches, &instances, now(), |_| 600);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("broken is not a T3 Code provider cache"),
            "{warnings:?}"
        );
        let entries = doc["harnesses"].as_array().unwrap();
        let by = |h: &str| entries.iter().find(|e| e["harness"] == h).cloned();
        let many = by("many").unwrap()["windows"].as_array().unwrap().len();
        assert_eq!(many, T3_MAX_WINDOWS * T3_MAX_FAMILIES);
        assert!(
            by("late").is_none(),
            "600.5 seconds is past a 600-second threshold"
        );
        assert_eq!(entries.len(), 4, "{entries:?}");
        let claude = by("claude").unwrap();
        assert_eq!(claude["fetchedAt"], "2026-10-29T01:58:00.120Z");
        assert_eq!(
            claude["windows"][0],
            json!({"id": "five_hour", "kind": "session", "percent": 8.0,
            "resetsAt": "2026-10-29T06:00:00Z", "windowMinutes": 300})
        );
        assert_eq!(claude["windows"][2]["model"], "fable");
        assert_eq!(by("grok").unwrap()["windows"][0]["id"], "weekly");
        let ag: Vec<(String, Option<String>)> = by("antigravity").unwrap()["windows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| {
                (
                    w["id"].as_str().unwrap().to_string(),
                    w["model"].as_str().map(str::to_string),
                )
            })
            .collect();
        assert_eq!(
            ag,
            [
                ("3p-5h-claude".to_string(), Some("claude".to_string())),
                ("3p-5h-gpt".to_string(), Some("gpt".to_string())),
                ("gemini-5h".to_string(), None),
                (
                    "gemini-argon-wtu-weekly".to_string(),
                    Some("argon".to_string())
                ),
            ]
        );
        assert!(
            by("codex").is_none(),
            "a reading older than fresh_seconds falls through"
        );
        assert_eq!(
            doc["models"]["codex"],
            json!(["gpt-6.1-sol"]),
            "models still come through"
        );
        assert!(
            doc["models"].get("muse").is_none(),
            "a disabled instance is skipped"
        );
    }

    fn usage_cache(account: &str) -> Value {
        json!({"oauthAccount": {"accountUuid": "acct-a"}, "cachedUsageUtilization": {
            "fetchedAtMs": 1793750400000i64, "accountUuid": account, "utilization": {
                "five_hour": {"utilization": 39, "resets_at": "2026-11-06T01:00:00.098390+00:00"},
                "seven_day": {"utilization": 45.5, "resets_at": "2026-11-09T04:00:00+00:00"},
                "seven_day_opus": null,
                "limits": [
                    {"kind": "session", "percent": 39, "scope": null},
                    {"kind": "weekly_scoped", "percent": 2, "resets_at": "2026-11-09T04:00:00+00:00",
                     "scope": {"model": {"id": null, "display_name": "Fable"}}},
                    {"kind": "weekly_scoped", "percent": 7, "resets_at": "2026-11-09T04:00:00+00:00",
                     "scope": {"model": {"id": null, "display_name": " Opus (4.5) "}}}]}}})
    }

    #[test]
    fn claude_codes_usage_cache_becomes_session_weekly_and_model_windows() {
        let doc = from_usage_cache(&usage_cache("acct-a"), "claude")
            .unwrap()
            .unwrap();
        let e = &doc["harnesses"][0];
        assert_eq!(e["fetchedAt"], "2026-11-04T00:00:00Z");
        assert_eq!(
            e["windows"],
            json!([
                {"id": "five_hour", "kind": "session", "percent": 39.0, "resetsAt": "2026-11-06T01:00:00Z"},
                {"id": "seven_day", "kind": "weekly", "percent": 45.5, "resetsAt": "2026-11-09T04:00:00Z"},
                {"id": "seven_day_fable", "kind": "weekly", "percent": 2.0, "resetsAt": "2026-11-09T04:00:00Z", "model": "fable"},
                {"id": "seven_day_opus_4_5", "kind": "weekly", "percent": 7.0, "resetsAt": "2026-11-09T04:00:00Z", "model": "opus-4-5"}
            ])
        );
    }

    #[test]
    fn a_usage_cache_for_another_account_or_none_is_no_reading_and_a_new_format_is_an_error() {
        assert!(
            from_usage_cache(&usage_cache("acct-b"), "claude")
                .unwrap()
                .is_none()
        );
        assert!(
            from_usage_cache(&json!({"projects": {}}), "claude")
                .unwrap()
                .is_none()
        );
        let mut signed_out = usage_cache("acct-a");
        signed_out["oauthAccount"] = Value::Null;
        assert!(from_usage_cache(&signed_out, "claude").unwrap().is_none());
        for cache in [
            json!({"accountUuid": "acct-a", "usage": {}}),
            json!({"fetchedAtMs": 1793750400000i64, "utilization": {}}),
            json!({"accountUuid": "", "fetchedAtMs": 1793750400000i64, "utilization": {}}),
        ] {
            let err = from_usage_cache(
                &json!({"oauthAccount": {"accountUuid": "acct-a"}, "cachedUsageUtilization": cache}),
                "claude",
            )
            .unwrap_err();
            assert!(err.contains("changed its usage cache format"), "{err}");
        }
    }

    /// A stand-in `codex` that answers like `codex app-server` does.
    fn fake_codex(dir: &Path, body: &str) -> String {
        let path = dir.join("codex");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    #[test]
    fn codex_limits_and_visible_models_become_readings() {
        let _serial = crate::spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let program = fake_codex(
            dir.path(),
            r#"[ "$PWD" = "${HOME:-$USERPROFILE}" ] || exit 9
read l; echo '{"id":1,"result":{}}'
read l; read l; read l
echo '{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":9,"windowDurationMins":10080,"resetsAt":1793804404},"secondary":{"usedPercent":40,"windowDurationMins":300,"resetsAt":null}}}}'
echo '{"id":3,"result":{"data":[{"id":"gpt-6.1-sol"},{"id":"gpt-old","hidden":true}]}}'
sleep 5"#,
        );
        let doc = codex_with(&program, "codex", Duration::from_secs(5)).unwrap();
        let w = &doc["harnesses"][0]["windows"];
        assert_eq!(
            w[0],
            json!({"id": "primary", "kind": "weekly", "percent": 9.0, "resetsAt": "2026-11-04T15:00:04Z"})
        );
        assert_eq!(
            (w[1]["kind"].as_str(), w[1]["resetsAt"].is_null()),
            (Some("session"), true)
        );
        assert_eq!(doc["models"]["codex"], json!(["gpt-6.1-sol"]));
    }

    #[test]
    fn muse_allowance_becomes_session_and_weekly_windows() {
        let _serial = crate::spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let program = fake_codex(
            dir.path(),
            r#"[ "$PWD" = "${HOME:-$USERPROFILE}" ] || exit 9
[ "$MUSE_NO_AUTO_UPDATE" = 1 ] || exit 9
echo '{"schemaVersion":1,"allowance":{"window":{"usedPercent":47,"resetsAtMs":1793800000000},"weekly":{"usedPercent":53.5,"resetsAtMs":1794300000000}}}'"#,
        );
        let doc = muse_with(&program, "muse", Duration::from_secs(5)).unwrap();
        let w = &doc["harnesses"][0]["windows"];
        assert_eq!(
            w[0],
            json!({"id": "window", "kind": "session", "percent": 47.0, "resetsAt": "2026-11-04T13:46:40Z"})
        );
        assert_eq!(
            (w[1]["id"].as_str(), w[1]["percent"].as_f64()),
            (Some("weekly"), Some(53.5))
        );
    }

    fn fake_grok(dir: &Path, billing: &str) -> String {
        fake_codex(
            dir,
            &format!(
                r#"[ "$GROK_DISABLE_AUTOUPDATER" = 1 ] || exit 9
while read -r line; do
  case "$line" in
    *initialize*) echo 'not json'; echo '{{"jsonrpc":"2.0","id":1,"result":{{}}}}' ;;
    *billing*) echo '{billing}' ;;
  esac
done"#
            ),
        )
    }

    #[test]
    fn grok_reads_the_weekly_allowance() {
        let _serial = crate::spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let program = fake_grok(
            dir.path(),
            r#"{"id":2,"result":{"config":{"creditUsagePercent":1.5,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-10-28T00:00:00Z","end":"2026-11-04T00:00:00Z"}}}}"#,
        );
        let doc = grok_with(&program, "grok", Duration::from_secs(5)).unwrap();
        assert_eq!(
            doc["harnesses"][0]["windows"][0],
            json!({"id": "weekly", "kind": "weekly", "percent": 1.5,
                   "resetsAt": "2026-11-04T00:00:00Z", "windowMinutes": 10080})
        );
    }

    #[test]
    fn grok_without_a_usable_allowance_is_an_error_not_zero() {
        let _serial = crate::spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        for (billing, expected) in [
            (r#"{"id":2,"result":{"config":{}}}"#, "no billing period"),
            (
                r#"{"id":2,"result":{"config":{"creditUsagePercent":1,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_MONTHLY"}}}}"#,
                "changed its usage report",
            ),
            (
                r#"{"id":2,"result":{"config":{"creditUsagePercent":-1,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-10-28T00:00:00Z","end":"2026-11-04T00:00:00Z"}}}}"#,
                "invalid allowance",
            ),
            (r#"{"id":2,"error":{"code":-32601}}"#, "refused"),
        ] {
            let program = fake_grok(dir.path(), billing);
            let err = grok_with(&program, "grok", Duration::from_secs(5)).unwrap_err();
            assert!(err.contains(expected), "{err}");
        }
    }

    #[test]
    fn muse_without_usage_is_an_error_not_zero() {
        let _serial = crate::spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let none = fake_codex(
            dir.path(),
            r#"echo '{"schemaVersion":1,"allowance":null,"reason":"server_sent_no_usage"}'"#,
        );
        assert!(
            muse_with(&none, "muse", Duration::from_secs(5))
                .unwrap_err()
                .contains("no usage data")
        );
        let text = fake_codex(dir.path(), "echo 'No usage data (server_sent_no_usage)'");
        assert!(muse_with(&text, "muse", Duration::from_secs(5)).is_err());
    }

    #[test]
    fn a_silent_codex_times_out() {
        let _serial = crate::spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let program = fake_codex(dir.path(), "sleep 5");
        let started = Instant::now();
        let err = codex_with(&program, "codex", Duration::from_millis(300)).unwrap_err();
        assert!(err.contains("no answer"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
