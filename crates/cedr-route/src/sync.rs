//! `cedr route sync`: push local pick history to the cedr.dev app and pull the org's shared routes
//! (docs/route.md, Sync). The API key is read here and only ever sent as a bearer header.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use crate::config::{self, RoutesFrom, Sync, xdg_dir};
use crate::error::{Error, Result, io_err};
use crate::machine;
use crate::pick::Size;
use crate::readings::{MAX_BYTES, read_limited};
use crate::session::Session;

pub const BATCH: usize = 500;
const TIMEOUT: Duration = Duration::from_secs(15);

fn sync_err(step: &str, message: impl std::fmt::Display) -> Error {
    Error::Config(format!("sync {step}: {message}"))
}

/// The API key from `$CEDR_API_KEY`, else `$XDG_CONFIG_HOME/cedr/api-key`, which must be private.
pub fn api_key() -> Result<String> {
    if let Some(k) = std::env::var("CEDR_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
    {
        return Ok(k.trim().to_string());
    }
    let path = xdg_dir("XDG_CONFIG_HOME", ".config")
        .join("cedr")
        .join("api-key");
    let meta = std::fs::metadata(&path).map_err(|_| {
        Error::Config(format!(
            "no API key: set CEDR_API_KEY or save a key from the app's API keys page to {} (mode 600)",
            path.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(Error::Config(format!(
                "{} is readable by others; run chmod 600 on it",
                path.display()
            )));
        }
    }
    let key = std::fs::read_to_string(&path).map_err(io_err(&path))?;
    Ok(key.trim().to_string())
}

/// Plain HTTP is allowed only to a development app on this machine.
fn is_local(url: &reqwest::Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

fn allowed(url: &reqwest::Url) -> bool {
    url.scheme() == "https" || (url.scheme() == "http" && is_local(url))
}

fn client(url: &str) -> Result<reqwest::blocking::Client> {
    let parsed = reqwest::Url::parse(url).map_err(|e| sync_err("url", e))?;
    if !allowed(&parsed) {
        return Err(sync_err("url", "the API URL must be https://"));
    }
    // Every redirect hop meets the same rule, so a local app cannot hand the history to plain HTTP elsewhere.
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 {
            attempt.error("too many redirects")
        } else if allowed(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("refused a redirect to a URL that is not https://")
        }
    });
    reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .redirect(policy)
        .build()
        .map_err(|e| sync_err("client", e))
}

/// A failed request with its causes (a refused redirect says why), never its URL, which may carry the org.
fn request_error(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut parts = vec![e.to_string()];
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = cause.source();
    }
    parts.join(": ")
}

/// The API's error from a failed reply, for messages, with the key removed should the API echo it.
fn reply_error(step: &str, response: reqwest::blocking::Response, key: &str) -> Error {
    let status = response.status();
    let body = read_limited(response, 4096).unwrap_or_default();
    let reply = serde_json::from_slice::<Value>(&body).unwrap_or_default();
    let detail = [reply.get("error"), reply.get("message")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(": ");
    let detail = if key.is_empty() {
        detail
    } else {
        detail.replace(key, "[redacted]")
    };
    let tail = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };
    sync_err(step, format!("HTTP {status}{tail}"))
}

fn state_file(name: &str) -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state")
        .join("cedr")
        .join(name)
}

fn state_path() -> PathBuf {
    state_file("route-sync.json")
}

fn read_offsets(path: &Path) -> BTreeMap<String, u64> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_offsets(path: &Path, offsets: &BTreeMap<String, u64>) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec(offsets).expect("offsets serialize"),
    )
    .map_err(io_err(&tmp))?;
    std::fs::rename(&tmp, path).map_err(io_err(path))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pushed {
    pub accepted: u64,
    pub skipped: u64,
}

/// Complete history lines from `offset`: (end offset of each line, parsed record). A torn last line
/// (a write in progress) waits for the next sync; an unreadable complete line is skipped.
fn pending(history: &Path, offset: u64) -> Result<Vec<(u64, Value)>> {
    let mut file = match std::fs::File::open(history) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(history)(e)),
    };
    let len = file.metadata().map_err(io_err(history))?.len();
    // A history file replaced by a shorter one starts over rather than skipping its records.
    let start = if offset > len { 0 } else { offset };
    file.seek(SeekFrom::Start(start)).map_err(io_err(history))?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(io_err(history))?;
    let mut out = Vec::new();
    let mut at = start;
    for line in text.split_inclusive('\n') {
        if !line.ends_with('\n') {
            break;
        }
        at += line.len() as u64;
        if let Ok(v) = serde_json::from_str::<Value>(line.trim_end()) {
            out.push((at, v));
        }
    }
    Ok(out)
}

/// Sends every record not yet accepted, in order, advancing the saved offset after each batch.
pub fn push(sync: &Sync, key: &str, history: &Path) -> Result<Pushed> {
    let state = state_path();
    let id = format!("{} {}", sync.url(), sync.org);
    let mut offsets = read_offsets(&state);
    let records = pending(history, offsets.get(&id).copied().unwrap_or(0))?;
    let http = client(sync.url())?;
    let mut total = Pushed::default();
    for batch in records.chunks(BATCH) {
        let body =
            json!({"org": sync.org, "records": batch.iter().map(|(_, v)| v).collect::<Vec<_>>()});
        let response = http
            .post(format!("{}/route/history", sync.url()))
            .bearer_auth(key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .map_err(|e| sync_err("push", request_error(e)))?;
        if !response.status().is_success() {
            return Err(reply_error("push", response, key));
        }
        let reply: Value = serde_json::from_slice(
            &read_limited(response, MAX_BYTES).map_err(|e| sync_err("push", e))?,
        )
        .map_err(|e| sync_err("push", format!("unexpected reply ({e})")))?;
        // The offset moves only when the API accounts for every record sent; otherwise the batch is
        // retried next time rather than silently dropped.
        let (Some(accepted), Some(skipped)) =
            (reply["accepted"].as_u64(), reply["skipped"].as_u64())
        else {
            return Err(sync_err(
                "push",
                "the reply has no accepted and skipped counts",
            ));
        };
        if accepted + skipped != batch.len() as u64 {
            return Err(sync_err(
                "push",
                format!(
                    "the API accounted for {} of {} records",
                    accepted + skipped,
                    batch.len()
                ),
            ));
        }
        total.accepted += accepted;
        total.skipped += skipped;
        offsets.insert(id.clone(), batch.last().expect("batches are not empty").0);
        write_offsets(&state, &offsets)?;
    }
    Ok(total)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Pulled {
    /// Shared routes written to the path, with their rules hash, update time and author.
    Routes {
        path: PathBuf,
        rules: String,
        updated_at: String,
        updated_by: String,
    },
    /// The org has no shared routes; any old local copy was removed.
    None,
    /// The org's plan does not include shared routes; any old local copy was removed.
    NotOnPlan,
}

/// Fetches the org's shared routes, validates them, and stores them for `[sync] routes = "org"`.
pub fn pull(sync: &Sync, key: &str) -> Result<Pulled> {
    let path = config::org_routes_path(&sync.org);
    let mut url = reqwest::Url::parse(&format!("{}/route/routes", sync.url()))
        .map_err(|e| sync_err("url", e))?;
    url.query_pairs_mut().append_pair("org", &sync.org);
    let response = client(sync.url())?
        .get(url)
        .bearer_auth(key)
        .send()
        .map_err(|e| sync_err("pull", request_error(e)))?;
    let status = response.status().as_u16();
    if status == 404 || status == 403 {
        // Only "no saved routes" and "not on this plan" mean the org stopped sharing; any other refusal (not a
        // member, a read-only or blocked key) is an error, and the local copy stays.
        let body = read_limited(response, 4096).unwrap_or_default();
        let reply = serde_json::from_slice::<Value>(&body).unwrap_or_default();
        let code = reply
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let outcome = match (status, code) {
            (404, _) => Pulled::None,
            (403, "plan_required") => Pulled::NotOnPlan,
            _ => {
                let message = reply
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .replace(key, "[redacted]");
                return Err(sync_err(
                    "pull",
                    format!("HTTP {status}: {code}: {message}"),
                ));
            }
        };
        // Routing with a copy the org no longer shares would silently diverge from the team.
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&path)(e)),
        }
        return Ok(outcome);
    }
    if !response.status().is_success() {
        return Err(reply_error("pull", response, key));
    }
    let reply: Value = serde_json::from_slice(
        &read_limited(response, MAX_BYTES).map_err(|e| sync_err("pull", e))?,
    )
    .map_err(|e| sync_err("pull", format!("unexpected reply ({e})")))?;
    let text = reply["routes"]
        .as_str()
        .ok_or_else(|| sync_err("pull", "the reply has no routes text"))?;
    config::parse_shared(text, &path)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(io_err(&tmp))?;
    std::fs::rename(&tmp, &path).map_err(io_err(&path))?;
    let field = |k: &str| reply[k].as_str().unwrap_or("").to_string();
    Ok(Pulled::Routes {
        path,
        rules: config::rules_hash(text.as_bytes()),
        updated_at: field("updatedAt"),
        updated_by: field("updatedBy"),
    })
}

/// This machine's current view for the app (docs/route.md, Sync, Machine snapshot): pools, what a
/// `medium` pick of each route would choose now, and stale readers. Nothing is recorded.
pub fn snapshot(session: &Session, sync: &Sync) -> Result<Value> {
    let ctx = session.context();
    let mut next = Vec::new();
    for id in ctx.config.routes.keys() {
        let p = ctx.pick(id, Size::Medium, &[])?;
        let reason = match (p.council, p.delegates.first()) {
            (true, Some(_)) => "council".to_string(),
            (false, Some(d)) => d.reason.clone(),
            (_, None) => p.reason.clone().unwrap_or_default(),
        };
        next.push(json!({
            "route": clip(id, 64),
            "targets": p.delegates.iter().take(16).map(|d| clip(&d.target, 128)).collect::<Vec<_>>(),
            "reason": clip(&reason, 512),
        }));
    }
    let mut pools = ctx.pools();
    pools.truncate(SNAPSHOT_ROWS);
    for row in &mut pools {
        row.summary = clip(&row.summary, 256);
    }
    next.truncate(SNAPSHOT_ROWS);
    let mut snapshot = json!({
        "org": sync.org,
        "machine": machine::machine_id()?,
        "name": clip(&machine::machine_name(ctx.config), 64),
        "at": session.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "cli": env!("CARGO_PKG_VERSION"),
        "rules": ctx.rules,
        "routes": match sync.routes { RoutesFrom::Local => "local", RoutesFrom::Org => "org" },
        "pools": pools,
        "next": next,
        "stale": session.stale.iter().take(16).collect::<Vec<_>>(),
    });
    // Keep Route's wire order stable when Sync enables insertion-ordered JSON merging.
    snapshot.sort_all_objects();
    Ok(snapshot)
}

/// The most pool rows and routes a snapshot carries; the app refuses larger ones.
const SNAPSHOT_ROWS: usize = 64;

/// At most `max` characters, so a long summary or reason never gets the whole snapshot refused.
fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Sends the machine snapshot; the app keeps only the latest one per machine.
pub fn send_machine(sync: &Sync, key: &str, body: &Value) -> Result<()> {
    put_json(sync, key, "/route/machine", "snapshot", body)
}

/// Sends a repo's Guard inventory (docs/design.md, Push); the app keeps only the latest push
/// per organization and repo.
pub fn send_guard(sync: &Sync, key: &str, body: &Value) -> Result<()> {
    put_json(sync, key, "/guard/repo", "push", body)
}

fn put_json(sync: &Sync, key: &str, path: &str, step: &str, body: &Value) -> Result<()> {
    let response = client(sync.url())?
        .put(format!("{}{path}", sync.url()))
        .bearer_auth(key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .map_err(|e| sync_err(step, request_error(e)))?;
    if !response.status().is_success() {
        return Err(reply_error(step, response, key));
    }
    Ok(())
}

/// How often picks, verdicts and status line captures may start a background sync.
pub const AUTO_EVERY: Duration = Duration::from_secs(600);
/// A lock older than this belongs to a sync that died; the next one takes it over.
const LOCK_STALE: Duration = Duration::from_secs(120);

/// Held while a sync runs, so two never move the same offsets; released on drop. The file holds
/// a nonce, so a sync whose lock was taken over as stale never removes its successor's.
pub struct Lock {
    path: PathBuf,
    nonce: String,
}

impl Drop for Lock {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).is_ok_and(|n| n == self.nonce) {
            // Gone already is fine: nothing else depends on this file once its holder is done.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The sync lock, or none while another sync holds it.
pub fn lock() -> Result<Option<Lock>> {
    lock_at(&state_file("route-sync.lock"), SystemTime::now())
}

fn lock_at(path: &Path, now: SystemTime) -> Result<Option<Lock>> {
    lock_stale_after(path, now, LOCK_STALE)
}

fn lock_stale_after(path: &Path, now: SystemTime, stale_after: Duration) -> Result<Option<Lock>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    let nonce = crate::pick::new_id();
    for attempt in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(nonce.as_bytes()).map_err(io_err(path))?;
                return Ok(Some(Lock {
                    path: path.to_path_buf(),
                    nonce,
                }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(path)
                    .and_then(|m| m.modified())
                    .map(|t| now.duration_since(t).unwrap_or_default() > stale_after)
                    .unwrap_or(true);
                if !stale || attempt == 1 {
                    return Ok(None);
                }
                // Renaming is atomic: of two syncs taking over the same stale lock, one moves it
                // and the other finds it gone, so neither removes a lock the other just made.
                let moved = path.with_extension(format!("stale-{nonce}"));
                match std::fs::rename(path, &moved) {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&moved);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_err(path)(e)),
                }
            }
            Err(e) => return Err(io_err(path)(e)),
        }
    }
    Ok(None)
}

/// Where a background sync leaves its last error for the next manual run to show.
pub fn log_path() -> PathBuf {
    state_file("route-sync.log")
}

/// How far a stamp may sit in the future and still count as recent: file times come from a coarser
/// clock than `SystemTime::now`, so a stamp written just now can read slightly ahead of it.
const STAMP_SKEW: Duration = Duration::from_secs(5);

/// Whether a background sync is due; marks it started when it is, so a burst of picks starts one.
/// A stamp further in the future (the clock moved back) counts as due rather than blocking for hours.
fn auto_due(stamp: &Path, now: SystemTime) -> Result<bool> {
    let recent = std::fs::metadata(stamp)
        .and_then(|m| m.modified())
        .is_ok_and(|t| match now.duration_since(t) {
            Ok(age) => age < AUTO_EVERY,
            Err(ahead) => ahead.duration() < STAMP_SKEW,
        });
    if recent {
        return Ok(false);
    }
    if let Some(dir) = stamp.parent() {
        std::fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    std::fs::write(stamp, b"").map_err(io_err(stamp))?;
    Ok(true)
}

/// Whether this machine may start a background sync now (one stat while not due, so the status
/// line pays nothing more); marks it started when it may.
pub fn auto_now() -> Result<bool> {
    auto_due(&state_file("route-sync-auto"), SystemTime::now())
}

/// Starts `<exe> route sync --quiet --config <config>` in the background. The child is not waited
/// for here: a thread reaps it, so a long-lived MCP server leaves no zombies behind.
pub fn auto_start(exe: &Path, config: &Path) -> Result<()> {
    detached_start(
        exe,
        &[
            std::ffi::OsStr::new("route"),
            std::ffi::OsStr::new("sync"),
            std::ffi::OsStr::new("--quiet"),
            std::ffi::OsStr::new("--config"),
            config.as_os_str(),
        ],
    )
}

/// Starts background CLI work with closed streams and reaps its process without waiting here.
pub fn detached_start(exe: &Path, args: &[&std::ffi::OsStr]) -> Result<()> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| sync_err("background", e))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Serializes monitor automatic-start decisions independently of its longer-running pushes.
pub fn monitor_auto_lock() -> Result<Option<Lock>> {
    lock_at(&state_file("monitor-auto.lock"), SystemTime::now())
}

/// Validates a destination without making a request, using Route's transport policy.
pub fn validate_url(url: &str) -> Result<()> {
    client(url).map(|_| ())
}

/// A nonce-protected lock on `$XDG_STATE_HOME/cedr/<name>`, for other background CLI work (telemetry
/// flushes, self-updates) that must run one at a time per machine. `stale_after` must exceed the
/// longest the holder can run, or a second holder takes it over while the first still works.
pub fn state_lock(name: &str, stale_after: Duration) -> Result<Option<Lock>> {
    lock_stale_after(&state_file(name), SystemTime::now(), stale_after)
}

/// An independent nonce-protected lock for monitor pushes.
pub fn monitor_lock() -> Result<Option<Lock>> {
    lock_at(&state_file("monitor-push.lock"), SystemTime::now())
}

/// Sends counts-only monitor batches using the same key, URL and error rules as Route.
pub fn send_monitor(sync: &Sync, key: &str, body: &Value) -> Result<Value> {
    let response = client(sync.url())?
        .post(format!("{}/monitor/usage", sync.url()))
        .bearer_auth(key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .map_err(|e| sync_err("monitor", request_error(e)))?;
    if !response.status().is_success() {
        return Err(reply_error("monitor", response, key));
    }
    let bytes = read_limited(response, 4096).map_err(|e| sync_err("monitor reply", e))?;
    serde_json::from_slice(&bytes).map_err(|_| sync_err("monitor reply", "invalid JSON"))
}

/// Organization rules response. Only the contract's specific errors are nonfatal.
pub enum SyncRules {
    Rules(Value),
    None,
    PlanRequired,
}
/// Reads organization rules using the same key, client and URL rules as Route.
pub fn get_sync_rules(sync: &Sync, key: &str) -> Result<SyncRules> {
    let mut url = reqwest::Url::parse(&format!("{}/sync/rules", sync.url()))
        .map_err(|_| sync_err("pull", "invalid API URL"))?;
    url.query_pairs_mut().append_pair("org", &sync.org);
    let response = client(sync.url())?
        .get(url)
        .bearer_auth(key)
        .send()
        .map_err(|e| sync_err("pull", request_error(e)))?;
    let status = response.status();
    if status.is_success() {
        return read_limited(response, 8 * 1024 * 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .ok_or(())
            .map(SyncRules::Rules)
            .map_err(|_| sync_err("pull", "invalid JSON response"));
    }
    if status.as_u16() == 404 || status.as_u16() == 403 {
        let v: Value = read_limited(response, 8 * 1024 * 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .ok_or(())
            .map_err(|_| sync_err("pull", format!("HTTP {status}")))?;
        if status.as_u16() == 404 && v["error"] == "no_rules" {
            return Ok(SyncRules::None);
        }
        if status.as_u16() == 403 && v["error"] == "plan_required" {
            return Ok(SyncRules::PlanRequired);
        }
        return Err(sync_err("pull", format!("HTTP {status}")));
    }
    Err(reply_error("pull", response, key))
}
/// Reports a repo without sending source contents or environment values.
pub fn send_sync_repo(sync: &Sync, key: &str, body: &Value) -> Result<Value> {
    let response = client(sync.url())?
        .put(format!("{}/sync/repo", sync.url()))
        .bearer_auth(key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .map_err(|e| sync_err("push", request_error(e)))?;
    if !response.status().is_success() {
        return Err(reply_error("push", response, key));
    }
    read_limited(response, 1024 * 1024)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .ok_or(())
        .map_err(|_| sync_err("push", "invalid JSON response"))
}
/// Account metadata for Doctor; callers must never print the response wholesale.
pub fn get_me(sync: &Sync, key: &str) -> Result<Value> {
    let response = client(sync.url())?
        .get(format!("{}/me", sync.url()))
        .bearer_auth(key)
        .send()
        .map_err(|e| sync_err("account", request_error(e)))?;
    if !response.status().is_success() {
        return Err(sync_err("account", format!("HTTP {}", response.status())));
    }
    read_limited(response, 1024 * 1024)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .ok_or(())
        .map_err(|_| sync_err("account", "invalid JSON response"))
}

/// Latest CLI release for diagnostics. An explicit loopback API supports offline test fixtures.
pub fn doctor_release(sync: &Sync) -> Result<Value> {
    let local = reqwest::Url::parse(sync.url())
        .ok()
        .is_some_and(|u| is_local(&u));
    let url = if local {
        format!("{}/releases/latest", sync.url())
    } else {
        "https://api.github.com/repos/Sheernox/cedr/releases/latest".into()
    };
    let response = client(&url)?
        .get(&url)
        .header(reqwest::header::USER_AGENT, "cedr")
        .send()
        .map_err(|e| sync_err("release", request_error(e)))?;
    if !response.status().is_success() {
        return Err(sync_err("release", "release lookup unavailable"));
    }
    let bytes =
        read_limited(response, 256 * 1024).map_err(|_| sync_err("release", "invalid response"))?;
    serde_json::from_slice(&bytes).map_err(|_| sync_err("release", "invalid JSON response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn background_syncs_start_at_most_once_a_window() {
        let dir = tempfile::tempdir().unwrap();
        let stamp = dir.path().join("auto");
        let now = SystemTime::now();
        assert!(auto_due(&stamp, now).unwrap());
        assert!(!auto_due(&stamp, now).unwrap());
        assert!(auto_due(&stamp, now + AUTO_EVERY + Duration::from_secs(1)).unwrap());
        assert!(
            auto_due(&stamp, now - Duration::from_secs(3600)).unwrap(),
            "a clock moved back never blocks background syncs"
        );
    }

    #[test]
    fn a_stamp_just_ahead_of_the_clock_still_counts_as_recent() {
        let dir = tempfile::tempdir().unwrap();
        let stamp = dir.path().join("auto");
        let now = SystemTime::now();
        std::fs::File::create(&stamp)
            .unwrap()
            .set_modified(now + Duration::from_millis(20))
            .unwrap();
        assert!(!auto_due(&stamp, now).unwrap());
    }

    #[test]
    fn a_sync_whose_lock_was_taken_over_leaves_its_successor_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync.lock");
        let now = SystemTime::now();
        let old = lock_at(&path, now).unwrap().unwrap();
        assert!(lock_at(&path, now).unwrap().is_none(), "held");
        let later = now + LOCK_STALE + Duration::from_secs(1);
        let new = lock_at(&path, later).unwrap().unwrap();
        drop(old);
        // Judged at the real time, since the successor's file was written just now.
        assert!(
            lock_at(&path, now).unwrap().is_none(),
            "the successor still holds it"
        );
        drop(new);
        assert!(lock_at(&path, now).unwrap().is_some());
    }

    #[test]
    fn snapshot_fields_are_clipped_by_characters() {
        assert_eq!(clip("ab\u{1F600}cd", 3), "ab\u{1F600}");
    }

    #[test]
    fn pending_returns_complete_lines_after_the_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        std::fs::write(
            &path,
            "{\"id\":\"a\"}\n{\"id\":\"b\"}\nnot json\n{\"id\":\"c\"}\n{\"id\":\"torn",
        )
        .unwrap();
        let all = pending(&path, 0).unwrap();
        let ids: Vec<&str> = all.iter().map(|(_, v)| v["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["a", "b", "c"], "the torn line waits; junk is skipped");
        let after_a = all[0].0;
        let rest: Vec<String> = pending(&path, after_a)
            .unwrap()
            .iter()
            .map(|(_, v)| v["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(rest, ["b", "c"]);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\"}\n")
            .unwrap();
        assert_eq!(pending(&path, all[2].0).unwrap()[0].1["id"], "torn");
    }

    #[test]
    fn council_records_pass_through_pending_without_losing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        let record = json!({"type":"council","id":"c-0123456789ab","at":"2026-10-29T00:00:00Z","route":null,"run":null,"round":1,"rounds":1,"gate":"pass","next":"done","quorum":2,"block":"P1","reviewers":["a","b"],"groups":{"P0":0,"P1":0,"P2":0,"P3":0},"findings":{"a":0,"b":0}});
        let line = format!("{record}\n");
        std::fs::write(&path, &line).unwrap();
        let records = pending(&path, 0).unwrap();
        assert_eq!(records, vec![(line.len() as u64, record)]);
        assert!(pending(&path, records[0].0).unwrap().is_empty());
    }

    #[test]
    fn a_shorter_history_starts_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        std::fs::write(&path, "{\"id\":\"a\"}\n").unwrap();
        assert_eq!(pending(&path, 10_000).unwrap().len(), 1);
    }

    #[test]
    fn only_local_apps_may_use_plain_http() {
        assert!(client("http://api.cedr.dev/v1").is_err());
        assert!(client("http://localhost:4321/api/v1").is_ok());
        assert!(client("https://api.cedr.dev/v1").is_ok());
    }
}
