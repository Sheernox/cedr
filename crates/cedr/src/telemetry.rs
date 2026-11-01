//! Anonymous usage events (docs/telemetry.md): one queued line per command, sent in the background
//! by the hidden `cedr telemetry flush`. Nothing here may change a command's output, exit code or
//! speed beyond a file append and, at most once an hour, starting the flush.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use clap::{Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::settings::{self, Settings};

const ENDPOINT: &str = "https://api.cedr.dev/v1/telemetry";
const QUEUE: &str = "telemetry.jsonl";
/// The queue being sent; new events keep going to the queue meanwhile.
const SENDING: &str = "telemetry.sending";
const QUEUE_MAX: usize = 1000;
/// Well over 1,000 events of a few hundred bytes each.
const QUEUE_BYTES: u64 = 512 * 1024;
/// A flush stops sending after this long; the lock outlives it, so no second flush takes over.
const FLUSH_BUDGET: Duration = Duration::from_secs(60);
const FLUSH_LOCK_STALE: Duration = Duration::from_secs(600);
const BATCH: usize = 200;
const MAX_AGE: chrono::TimeDelta = chrono::TimeDelta::days(7);
const FLUSH_EVERY: Duration = Duration::from_secs(3600);
const COUNT_MAX: u64 = 100_000;

const NOTICE: &str = "\
cedr: cedr sends anonymous usage data (command names, exit codes, timing, version, OS,
cedr: which coding agents are installed, and a few counts) to improve cedr. It never sends
cedr: arguments, paths, repo names or code. Turn it off: cedr telemetry off (or CEDR_TELEMETRY=0).
cedr: cedr also updates itself once a day: cedr update --auto off turns that off.
cedr: Details: https://cedr.dev/docs/cli/telemetry";

/// Counts the running command reported, keyed by the names docs/telemetry.md lists.
static COUNTS: Mutex<BTreeMap<&'static str, u64>> = Mutex::new(BTreeMap::new());

/// Reports a count for this command's event; only keys from docs/telemetry.md, Events.
pub(crate) fn count(key: &'static str, n: usize) {
    if let Ok(mut counts) = COUNTS.lock() {
        counts.insert(key, (n as u64).min(COUNT_MAX));
    }
}

/// Commands that record no event: shell setup, telemetry itself, the per-prompt status line and
/// the hidden background commands.
pub(crate) fn silent(command: &str, background: bool) -> bool {
    background
        || command == "route statusline"
        || command == "completions"
        || command.split(' ').next() == Some("telemetry")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Off {
    Config,
    Telemetry,
    DoNotTrack,
}

impl Off {
    fn reason(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Telemetry => "CEDR_TELEMETRY",
            Self::DoNotTrack => "DO_NOT_TRACK",
        }
    }
}

/// Why telemetry is off, or `None` when it is on. The environment wins over the settings file.
pub(crate) fn off(settings: &Settings) -> Option<Off> {
    let env_off = std::env::var("CEDR_TELEMETRY").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off"
        )
    });
    if env_off {
        Some(Off::Telemetry)
    } else if settings::env_set("DO_NOT_TRACK") {
        Some(Off::DoNotTrack)
    } else if !settings.telemetry() {
        Some(Off::Config)
    } else {
        None
    }
}

/// Records the finished command and starts a flush when one is due. Never fails the caller.
pub(crate) fn record(command: &str, exit: u8, elapsed: Duration) {
    let settings = Settings::load();
    if off(&settings).is_some() {
        return;
    }
    let ci = settings::in_ci();
    let install = if ci {
        settings::new_uuid()
    } else if settings.notice_shown() && settings.install_id().is_some() {
        settings.install_id().map(str::to_string)
    } else {
        // An agent starts `cedr mcp` and keeps its stderr, so the person would never see the
        // notice there; it waits for a command they run themselves.
        if command == "mcp" && !settings.notice_shown() {
            return;
        }
        first_run(settings.notice_shown())
    };
    let Some(install) = install else {
        return;
    };
    let counts = COUNTS
        .lock()
        .map(|c| c.iter().map(|(k, v)| (k.to_string(), json!(v))).collect())
        .unwrap_or_default();
    let event = event_line(&install, ci, command, exit, elapsed, counts);
    if append(&settings::state_file(QUEUE), &event).is_err() {
        return;
    }
    let stamp = settings::state_file("telemetry-flush");
    // CI flushes every time: the runner's state does not outlive the job.
    let due = settings::stamp_due(&stamp, SystemTime::now(), FLUSH_EVERY) || ci;
    if due && let Ok(exe) = std::env::current_exe() {
        let _ = cedr_route::sync::detached_start(
            &exe,
            &[
                std::ffi::OsStr::new("telemetry"),
                std::ffi::OsStr::new("flush"),
            ],
        );
    }
}

/// Shows the notice if it was not shown, and creates the install ID. The settings are read again
/// just before saving, so a `cedr telemetry off` that ran meanwhile is never overwritten. An ID or
/// notice that cannot be saved would be new on every run, so then nothing is recorded.
fn first_run(notice_shown: bool) -> Option<String> {
    if !notice_shown {
        eprintln!("{NOTICE}");
    }
    let mut fresh = Settings::load();
    if off(&fresh).is_some() {
        return None;
    }
    fresh.set_notice_shown();
    let id = match fresh.install_id() {
        Some(id) => id.to_string(),
        None => {
            let id = settings::new_uuid()?;
            fresh.set_install_id(Some(&id));
            id
        }
    };
    fresh.save().ok()?;
    Some(id)
}

fn event_line(
    install: &str,
    ci: bool,
    command: &str,
    exit: u8,
    elapsed: Duration,
    counts: serde_json::Map<String, Value>,
) -> Value {
    let mut event = json!({
        "install": install,
        "ci": ci,
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "version": env!("CARGO_PKG_VERSION"),
        "command": command,
        "exit": exit,
        "ms": u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
    });
    if !counts.is_empty() {
        event["counts"] = Value::Object(counts);
    }
    event
}

/// Appends an event, unless the queue is already past what a flush keeps (a machine that has been
/// offline for long): then the event is dropped, so the file never grows without bound.
fn append(path: &Path, line: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > QUEUE_BYTES) {
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // One write per line keeps concurrent appends from interleaving.
    file.write_all(format!("{line}\n").as_bytes())
}

#[derive(Subcommand)]
pub(crate) enum TelemetryCommand {
    /// Show whether usage data is sent, and what turned it off
    Status {
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Send anonymous usage data (the default)
    On,
    /// Stop sending usage data, and delete the install ID and queued events
    Off,
    /// Send queued events (started in the background)
    #[command(hide = true)]
    Flush,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Format {
    Text,
    Json,
}

pub(crate) fn run(command: TelemetryCommand) -> Result<ExitCode, String> {
    let mut settings = Settings::load();
    match command {
        TelemetryCommand::Status { format } => {
            let off = off(&settings);
            let queued = std::fs::read_to_string(settings::state_file(QUEUE))
                .map(|t| t.lines().count())
                .unwrap_or(0);
            let install = off.is_none().then(|| settings.install_id()).flatten();
            match format {
                Format::Json => println!(
                    "{}",
                    json!({
                        "enabled": off.is_none(),
                        "reason": off.map(Off::reason),
                        "install": install,
                        "queued": queued,
                        "ci": settings::in_ci(),
                    })
                ),
                Format::Text => {
                    match off {
                        None => println!(
                            "telemetry: on (install {})",
                            install.unwrap_or("not yet created")
                        ),
                        Some(Off::Config) => println!("telemetry: off (cedr telemetry off)"),
                        Some(o) => println!("telemetry: off ({} is set)", o.reason()),
                    }
                    println!("queued events: {queued}");
                    println!("what is sent: https://cedr.dev/docs/cli/telemetry");
                }
            }
        }
        TelemetryCommand::On => {
            eprintln!("{NOTICE}");
            settings.set_bool("telemetry", true);
            settings.set_install_id(settings::new_uuid().as_deref());
            settings.set_notice_shown();
            settings.save()?;
            println!("telemetry: on");
            if let Some(o) = off(&settings) {
                println!("telemetry: but {} is set, which keeps it off", o.reason());
            }
        }
        TelemetryCommand::Off => {
            settings.set_bool("telemetry", false);
            settings.set_install_id(None);
            settings.set_notice_shown();
            settings.save()?;
            for name in [QUEUE, SENDING] {
                match std::fs::remove_file(settings::state_file(name)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("telemetry: removing queued events: {e}")),
                }
            }
            println!("telemetry: off; the install ID and queued events are deleted");
        }
        TelemetryCommand::Flush => {
            if off(&settings).is_none() {
                flush(&endpoint()?);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn endpoint() -> Result<String, String> {
    match std::env::var("CEDR_TELEMETRY_URL") {
        Ok(url) if !url.is_empty() => crate::net::checked_base(&url),
        _ => Ok(ENDPOINT.into()),
    }
}

/// Sends the queue: moves it aside to `sending` so new events keep queuing, sends each install's
/// events in batches, then leaves in `sending` only what was not delivered, for the next flush. A
/// batch the app refuses as invalid (400, 413) is dropped, since resending it can never succeed; any
/// other failure, the time budget or `cedr telemetry off` stops sending until the next flush.
fn flush(endpoint: &str) {
    let Ok(Some(_lock)) = cedr_route::sync::state_lock("telemetry.lock", FLUSH_LOCK_STALE) else {
        return;
    };
    let queue = settings::state_file(QUEUE);
    let sending = settings::state_file(SENDING);
    // A flush that did not finish left its events in `sending`: this one sends those, and the
    // queue waits for the next, so neither file is ever overwritten.
    if !sending.exists() && std::fs::rename(&queue, &sending).is_err() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(&sending) else {
        return;
    };
    let events = pending(&text, chrono::Utc::now());
    let Ok(client) = crate::net::client(Duration::from_secs(5)) else {
        return;
    };
    let agents = agents();
    let started = std::time::Instant::now();
    let mut unsent: Vec<&Value> = Vec::new();
    let mut stopped = false;
    for ((install, ci), group) in group(&events) {
        for chunk in group.chunks(BATCH) {
            stopped =
                stopped || started.elapsed() > FLUSH_BUDGET || off(&Settings::load()).is_some();
            if stopped {
                unsent.extend(chunk.iter().copied());
                continue;
            }
            let body = batch(&install, ci, &agents, chunk);
            let status = client
                .post(endpoint)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string())
                .send()
                .map(|r| r.status().as_u16());
            if !matches!(status, Ok(s) if (200..300).contains(&s) || s == 400 || s == 413) {
                stopped = true;
                unsent.extend(chunk.iter().copied());
            }
        }
    }
    // `cedr telemetry off` ran meanwhile: what it could not delete while this flush held it goes now.
    if off(&Settings::load()).is_some() {
        let _ = std::fs::remove_file(&sending);
        let _ = std::fs::remove_file(&queue);
        return;
    }
    // A command that opened the queue just before the rename appends to `sending` after it was
    // read; those lines are kept.
    let late = std::fs::read_to_string(&sending)
        .ok()
        .and_then(|now| now.get(text.len()..).map(str::to_string))
        .unwrap_or_default();
    let keep: String = unsent.iter().map(|e| format!("{e}\n")).collect::<String>() + &late;
    if keep.is_empty() {
        let _ = std::fs::remove_file(&sending);
    } else {
        let _ = settings::write_atomic(&sending, keep.as_bytes());
    }
}

/// Queued events still worth sending: well-formed, at most 7 days old, and the newest 1,000.
fn pending(text: &str, now: chrono::DateTime<chrono::Utc>) -> Vec<Value> {
    let mut events: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| {
            e["install"].is_string()
                && e["at"]
                    .as_str()
                    .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                    .is_some_and(|at| now.signed_duration_since(at) <= MAX_AGE)
        })
        .collect();
    let excess = events.len().saturating_sub(QUEUE_MAX);
    events.drain(..excess);
    events
}

/// Events grouped by install and CI flag, in queue order.
fn group(events: &[Value]) -> Vec<((String, bool), Vec<&Value>)> {
    let mut out: Vec<((String, bool), Vec<&Value>)> = Vec::new();
    for e in events {
        let key = (
            e["install"].as_str().unwrap_or_default().to_string(),
            e["ci"].as_bool().unwrap_or(false),
        );
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.push(e),
            None => out.push((key, vec![e])),
        }
    }
    out
}

fn batch(install: &str, ci: bool, agents: &[&str], events: &[&Value]) -> Value {
    let events: Vec<Value> = events
        .iter()
        .map(|e| {
            let mut e = (*e).clone();
            if let Some(o) = e.as_object_mut() {
                o.remove("install");
                o.remove("ci");
            }
            e
        })
        .collect();
    json!({
        "install": install,
        "ci": ci,
        "os": match std::env::consts::OS {
            "linux" => "linux",
            "macos" => "macos",
            "windows" => "windows",
            _ => "other",
        },
        "arch": match std::env::consts::ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            _ => "other",
        },
        "agents": agents,
        "events": events,
    })
}

/// Agents whose home directory exists here (docs/telemetry.md, Sending). Nothing is read or run.
fn agents() -> Vec<&'static str> {
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let data = cedr_route::config::xdg_dir("XDG_DATA_HOME", ".local/share");
    let env_or = |var: &str, fallback: std::path::PathBuf| {
        std::env::var_os(var)
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or(fallback)
    };
    [
        ("claude", env_or("CLAUDE_CONFIG_DIR", home.join(".claude"))),
        ("codex", env_or("CODEX_HOME", home.join(".codex"))),
        ("gemini", home.join(".gemini")),
        ("copilot", env_or("COPILOT_HOME", home.join(".copilot"))),
        ("opencode", data.join("opencode")),
        ("muse", data.join("muse")),
        ("antigravity", home.join(".gemini/antigravity")),
        ("grok", env_or("GROK_HOME", home.join(".grok"))),
        ("cursor", home.join(".cursor")),
    ]
    .into_iter()
    .filter(|(_, dir)| dir.is_dir())
    .map(|(name, _)| name)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_carry_only_the_contract_fields() {
        let mut counts = serde_json::Map::new();
        counts.insert("route_routes".into(), json!(12));
        let e = event_line(
            "id",
            false,
            "route pick",
            1,
            Duration::from_millis(41),
            counts,
        );
        let mut keys: Vec<_> = e.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "at", "ci", "command", "counts", "exit", "install", "ms", "version"
            ]
        );
        let b = batch("id", false, &["claude"], &[&e]);
        let sent: Vec<_> = b["events"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert!(!sent.contains(&"install".to_string()) && !sent.contains(&"ci".to_string()));
        assert_eq!(b["install"], "id");
    }

    #[test]
    fn silent_commands_record_nothing() {
        for c in [
            "completions",
            "route statusline",
            "telemetry status",
            "telemetry",
        ] {
            assert!(silent(c, false), "{c}");
        }
        assert!(silent("update", true));
        assert!(!silent("route pick", false) && !silent("update", false));
    }

    #[test]
    fn the_queue_drops_old_and_malformed_events_and_keeps_the_newest_thousand() {
        let now = chrono::Utc::now();
        let at = |days: i64| {
            (now - chrono::TimeDelta::days(days)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };
        let mut text = String::new();
        for i in 0..1005 {
            text.push_str(&format!(
                "{{\"install\":\"a\",\"at\":\"{}\",\"exit\":{i}}}\n",
                at(0)
            ));
        }
        text.push_str(&format!("{{\"install\":\"a\",\"at\":\"{}\"}}\n", at(8)));
        text.push_str("not json\n");
        let kept = pending(&text, now);
        assert_eq!(kept.len(), 1000);
        assert_eq!(kept[0]["exit"], 5);
    }

    #[test]
    fn batches_group_by_install_in_queue_order() {
        let events = [
            json!({"install": "a", "ci": false}),
            json!({"install": "b", "ci": true}),
            json!({"install": "a", "ci": false}),
        ];
        let groups = group(&events);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, ("a".to_string(), false));
        assert_eq!(groups[0].1.len(), 2);
    }
}
