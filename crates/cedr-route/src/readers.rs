//! Built-in readings for Claude Code and Codex (docs/route.md, Built-in readers). Both go through
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

/// The JSON-RPC exchange: initialize, then the two reads; returns their results.
fn converse(child: &mut std::process::Child, timeout: Duration) -> Result<(Value, Value), String> {
    let deadline = Instant::now() + timeout;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
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
    let out = crate::readings::run_capture(&argv, &[("MUSE_NO_AUTO_UPDATE", "1")], timeout)?;
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
            r#"read l; echo '{"id":1,"result":{}}'
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
            r#"[ "$MUSE_NO_AUTO_UPDATE" = 1 ] || exit 9
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
