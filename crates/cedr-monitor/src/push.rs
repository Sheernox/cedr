//! Batch complete hours; commit the watermark only after all batches have succeeded.
use crate::{Result, store::Store};
use cedr_route::{
    config::{RoutesFrom, Sync},
    machine,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};
pub const BATCH: usize = 5000;
/// Shapes one hourly row for the wire: the contract carries counts, so a counter the harness did not report goes as 0
/// (nullable fields keep null), and the per-row ceiling protects the app's integer bounds. Local counts stay exact.
fn wire_row(row: &mut Value) {
    if let Some(row) = row.as_object_mut() {
        for (key, value) in row {
            if NULLABLE.contains(&key.as_str()) {
                continue;
            }
            if value.is_null() {
                *value = json!(0);
            } else if let Some(n) = value.as_i64() {
                *value = json!(n.min(10_000_000_000));
            }
        }
    }
}

/// Row fields the contract allows to be null (and the model, a string); every other counter is sent as a number.
const NULLABLE: [&str; 5] = [
    "effort",
    "project",
    "contextPeak",
    "reportedCostUsd",
    "model",
];
pub fn send(
    db: &Store,
    sync: &Sync,
    key: &str,
    full: bool,
    start: Option<i64>,
    config: Option<&Path>,
) -> Result<Value> {
    let path = cedr_route::config::xdg_dir("XDG_STATE_HOME", ".local/state")
        .join("cedr/monitor-push.json");
    let cfg = if let Some(path) = config {
        cedr_route::config::load_local(path)?
    } else {
        cedr_route::config::Config::parse("version=1", Path::new("monitor.toml"))?
    };
    let machine = machine::machine_id()?;
    let name = machine::machine_name(&cfg);
    send_with(db, sync, full, start, &path, &machine, &name, |body| {
        Ok(cedr_route::sync::send_monitor(sync, key, body)?)
    })
}
/// Parses `--start`: `all` (0, everything in the store), `now`, a date (UTC midnight) or an RFC 3339 time.
pub fn parse_start(s: &str) -> Result<i64> {
    match s {
        "all" => Ok(0),
        "now" => Ok(chrono::Utc::now().timestamp()),
        _ => chrono::DateTime::parse_from_rfc3339(s)
            .map(|t| t.timestamp())
            .or_else(|_| {
                chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map(|d| {
                    d.and_hms_opt(0, 0, 0)
                        .unwrap_or_default()
                        .and_utc()
                        .timestamp()
                })
            })
            .map_err(|_| "--start takes all, now, YYYY-MM-DD or an RFC 3339 time".into()),
    }
}
/// Sends usage at or after the org's start (`chosen` from `--start`, else the one saved by an earlier push, else
/// everything) and saves the start first, so later scheduled, automatic and `--full` pushes keep to it. A start
/// moved earlier resends from it; one moved later stops sending older usage but cannot remove what was already sent.
/// `defaulted` in the reply is true when no start had been chosen and everything was sent.
#[allow(clippy::too_many_arguments)]
pub fn send_with(
    db: &Store,
    sync: &Sync,
    full: bool,
    chosen: Option<i64>,
    path: &Path,
    machine: &str,
    name: &str,
    mut post: impl FnMut(&Value) -> Result<Value>,
) -> Result<Value> {
    let mut watermarks: BTreeMap<String, i64> = match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(e.into()),
    };
    let start_path = path.with_file_name("monitor-start.json");
    let mut starts: BTreeMap<String, i64> = match std::fs::read(&start_path) {
        Ok(b) => serde_json::from_slice(&b)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(e.into()),
    };
    let identity = crate::digest(&format!("{}:{}", sync.org, sync.url()));
    let saved = starts.get(&identity).copied();
    // A start moved later on a store that has pushed takes effect at the next whole hour: the app replaces whole
    // hourly rows, so cutting into an hour it already has would remove usage it was sent.
    let narrowed =
        chosen.filter(|s| watermarks.contains_key(&identity) && saved.is_none_or(|o| *s > o));
    let cutoff = narrowed
        .map(|s| (s + 3599).div_euclid(3600) * 3600)
        .or(chosen)
        .or(saved)
        .unwrap_or(0);
    // The start is saved before anything is sent, so an interrupted push cannot fall back to everything. Moving it
    // earlier first forgets the push time, so every later attempt sends from the new start until one succeeds.
    if chosen.is_some_and(|s| saved.is_some_and(|o| s < o)) {
        watermarks.remove(&identity);
        write_atomic(path, &watermarks)?;
    }
    let previous = watermarks.get(&identity).copied().unwrap_or(0);
    if saved != Some(cutoff) {
        starts.insert(identity.clone(), cutoff);
        write_atomic(&start_path, &starts)?;
    }
    let at = chrono::Utc::now().timestamp();
    let mut rows = crate::report::hourly(
        db,
        if full {
            cutoff
        } else {
            cutoff.max(previous.saturating_sub(7200).div_euclid(3600) * 3600)
        },
    )?;
    // Hours whose attribution changed (sources::REREADS) go again in full, once per org. Repairs before the start
    // stay pending, so moving the start earlier later still repairs them.
    // One row per old key (a reread moves many responses from the same key), with the newest repair it covers.
    let mut q = db.db.prepare("SELECT max(id),hour,harness,model,effort,project,session,sidechain FROM repairs WHERE hour>=?2 AND NOT EXISTS(SELECT 1 FROM repairs_sent s WHERE s.org=?1 AND s.id=repairs.id) GROUP BY hour,harness,model,effort,project,session,sidechain ORDER BY hour")?;
    let repairs = q
        .query_map(params![identity, cutoff.div_euclid(3600) * 3600], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, json!({"harness":r.get::<_,String>(2)?,"model":r.get::<_,String>(3)?,"effort":r.get::<_,Option<String>>(4)?,"project":r.get::<_,Option<String>>(5)?,"session":r.get::<_,String>(6)?,"sidechain":r.get::<_,bool>(7)?})))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let sent_from = rows
        .iter()
        .filter_map(|r| r["hour"].as_str().map(String::from))
        .min();
    let key = |r: &Value| {
        [
            "harness",
            "model",
            "effort",
            "project",
            "session",
            "sidechain",
        ]
        .map(|k| r[k].to_string())
        .join("|")
    };
    let last_repair = repairs.iter().map(|r| r.0).max();
    let mut hours = repairs.iter().map(|r| r.1).collect::<Vec<_>>();
    hours.dedup();
    for hour in hours {
        let stamp = crate::report::stamp(hour);
        if sent_from.as_ref().is_some_and(|f| stamp >= *f) {
            continue;
        }
        rows.extend(crate::report::hourly_range(
            db,
            hour.max(cutoff),
            hour + 3600,
        )?);
    }
    let present = rows
        .iter()
        .map(|r| (r["hour"].to_string(), key(r)))
        .collect::<std::collections::HashSet<_>>();
    for (_, hour, old) in &repairs {
        let stamp = crate::report::stamp(*hour);
        if !present.contains(&(json!(stamp).to_string(), key(old))) {
            let mut zero = old.clone();
            zero["hour"] = json!(stamp);
            for k in [
                "responses",
                "input",
                "cacheWrite5m",
                "cacheWrite1h",
                "cacheRead",
                "output",
                "reasoning",
                "webSearches",
                "webFetches",
                "toolCalls",
                "toolErrors",
                "apiErrors",
                "compactions",
                "wallMs",
                "wallSamples",
                "activeMs",
            ] {
                zero[k] = json!(0);
            }
            zero["contextPeak"] = Value::Null;
            zero["reportedCostUsd"] = Value::Null;
            rows.push(zero);
        }
    }
    for row in &mut rows {
        wire_row(row);
    }
    let mut q = db
        .db
        .prepare("SELECT data FROM readings WHERE at>=? ORDER BY at DESC LIMIT 500")?;
    let readings = q
        .query_map(
            [crate::report::stamp(if full {
                cutoff
            } else {
                cutoff.max(previous)
            })],
            |r| r.get::<_, String>(0),
        )?
        .map(|r| Ok(serde_json::from_str::<Value>(&r?)?))
        .collect::<Result<Vec<_>>>()?;
    let mut accepted = 0u64;
    let mut skipped = 0u64;
    let sources = db
        .source_values()?
        .into_iter()
        .map(|mut v| {
            if let Some(o) = v.as_object_mut() {
                o.remove("message");
            }
            v
        })
        .collect::<Vec<_>>();
    let mut batches = Vec::new();
    let mut start = 0;
    while start < rows.len() {
        let mut end = start;
        let mut bytes = 0;
        while end < rows.len() && end - start < BATCH {
            let size = serde_json::to_vec(&rows[end])?.len();
            if end > start && bytes + size > 3 * 1024 * 1024 {
                break;
            }
            bytes += size;
            end += 1;
        }
        batches.push(&rows[start..end]);
        start = end;
    }
    if batches.is_empty() {
        batches.push(&[]);
    }
    for batch in batches {
        let body = json!({"org":sync.org,"machine":machine,"name":name,"at":crate::report::stamp(at),"cli":env!("CARGO_PKG_VERSION"),"rows":batch,"limits":readings,"sources":sources});
        if serde_json::to_vec(&body)?.len() > 4 * 1024 * 1024 {
            return Err("monitor batch exceeds 4 MiB".into());
        }
        let reply = post(&body)?;
        let a = reply["accepted"]
            .as_u64()
            .ok_or("invalid monitor accepted count")?;
        let s = reply["skipped"]
            .as_u64()
            .ok_or("invalid monitor skipped count")?;
        if a + s != batch.len() as u64 {
            return Err("monitor API did not acknowledge every row".into());
        }
        accepted += a;
        skipped += s;
    }
    // Repairs are marked sent before the push time moves on; a crash between the two only resends whole hours.
    db.db.execute_batch("BEGIN IMMEDIATE")?;
    if let Some(last) = last_repair {
        db.db.execute(
            "INSERT OR IGNORE INTO repairs_sent SELECT ?1,id FROM repairs WHERE hour>=?2 AND id<=?3",
            params![identity, cutoff.div_euclid(3600) * 3600, last],
        )?;
    }
    db.db.execute_batch("COMMIT")?;
    watermarks.insert(identity, at);
    write_atomic(path, &watermarks)?;
    Ok(
        json!({"accepted":accepted,"skipped":skipped,"defaulted":chosen.is_none() && saved.is_none()}),
    )
}
fn write_atomic(path: &Path, value: &BTreeMap<String, i64>) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(value)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}
pub fn settings(
    org: Option<&str>,
    routes: Option<&Path>,
    c: &crate::config::Config,
) -> Result<Sync> {
    let mut sync = if org.is_some() && routes.is_none() {
        Sync {
            org: org.unwrap_or_default().into(),
            auto: false,
            url: None,
            routes: RoutesFrom::Local,
        }
    } else if let Some(path) = routes {
        cedr_route::config::load_local(path)?
            .sync
            .ok_or("routes config has no sync organization")?
    } else if let Some(org) = &c.org {
        Sync {
            org: org.clone(),
            auto: false,
            url: c.url.clone(),
            routes: RoutesFrom::Local,
        }
    } else {
        // Only the person's own routes file: Monitor is not per repo, and a `.cedr/routes.toml` in whatever checkout
        // the command runs from must never choose where the API key is sent.
        let path = cedr_route::config::xdg_dir("XDG_CONFIG_HOME", ".config")
            .join("cedr")
            .join("routes.toml");
        cedr_route::config::load_local(&path)
            .ok()
            .and_then(|c| c.sync)
            .ok_or("no organization: pass --org or set org in ~/.config/cedr/monitor.toml")?
    };
    if let Some(org) = org {
        sync.org = org.into();
    }
    if let Some(url) = &c.url {
        sync.url = Some(url.clone());
    }
    if !cedr_route::config::is_slug(&sync.org) {
        return Err("invalid organization slug".into());
    }
    Ok(sync)
}
/// Statusline callers never wait for a push. A create-new stamp serializes automatic starts.
pub fn auto_start() -> Result<()> {
    let Some(_lock) = cedr_route::sync::monitor_auto_lock()? else {
        return Ok(());
    };
    let c = crate::config::Config::load()?;
    if !c.auto || settings(None, None, &c).is_err() {
        return Ok(());
    }
    let path =
        cedr_route::config::xdg_dir("XDG_STATE_HOME", ".local/state").join("cedr/monitor-auto");
    std::fs::create_dir_all(path.parent().ok_or("missing state directory")?)?;
    if let Ok(m) = std::fs::metadata(&path) {
        if m.modified()?.elapsed().unwrap_or_default().as_secs() < 1800 {
            return Ok(());
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    cedr_route::sync::detached_start(
        &std::env::current_exe()?,
        &[
            std::ffi::OsStr::new("monitor"),
            std::ffi::OsStr::new("push"),
            std::ffi::OsStr::new("--quiet"),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Row;
    #[test]
    fn batches_and_atomic_watermark() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        db.db.execute_batch("BEGIN").unwrap();
        for i in 0..5001 {
            db.upsert(&Row {
                id: i.to_string(),
                harness: "codex".into(),
                session: format!("s-{i}"),
                model: "gpt-6.1-sol".into(),
                at: 1793354400,
                ..Default::default()
            })
            .unwrap();
        }
        db.db.execute_batch("COMMIT").unwrap();
        let sync = Sync {
            org: "fixture".into(),
            url: None,
            auto: false,
            routes: RoutesFrom::Local,
        };
        let path = d.path().join("push.json");
        let mut calls = 0;
        let result = send_with(
            &db,
            &sync,
            false,
            None,
            &path,
            "m-fixture",
            "fixture",
            |b| {
                calls += 1;
                if calls == 2 {
                    return Err("synthetic failure".into());
                }
                assert_eq!(b["rows"].as_array().unwrap().len(), 5000);
                Ok(json!({"accepted":5000,"skipped":0}))
            },
        );
        assert!(result.is_err());
        assert!(!path.exists());
        let mut sizes = vec![];
        send_with(
            &db,
            &sync,
            false,
            None,
            &path,
            "m-fixture",
            "fixture",
            |b| {
                let n = b["rows"].as_array().unwrap().len();
                sizes.push(n);
                Ok(json!({"accepted":n,"skipped":0}))
            },
        )
        .unwrap();
        assert_eq!(sizes, [5000, 1]);
        assert!(path.exists());
    }
    #[test]
    fn a_start_is_kept_by_later_pushes_and_widening_it_resends() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        // One response each at 10:00, 11:30 and 12:00 UTC.
        for (id, at) in [
            ("old", 1793354400),
            ("mid", 1793359800),
            ("new", 1793361600),
        ] {
            db.upsert(&Row {
                id: id.into(),
                harness: "codex".into(),
                session: "s-1".into(),
                model: "gpt-6.1-sol".into(),
                at,
                ..Default::default()
            })
            .unwrap();
        }
        let sync = Sync {
            org: "fixture".into(),
            url: None,
            auto: false,
            routes: RoutesFrom::Local,
        };
        let path = d.path().join("push.json");
        let push = |full: bool, start: Option<i64>| {
            let mut sent = 0;
            let reply = send_with(
                &db,
                &sync,
                full,
                start,
                &path,
                "m-fixture",
                "fixture",
                |b| {
                    sent += b["rows"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|r| r["responses"].as_i64().unwrap())
                        .sum::<i64>();
                    let n = b["rows"].as_array().unwrap().len();
                    Ok(json!({"accepted":n,"skipped":0}))
                },
            )
            .unwrap();
            (sent, reply["defaulted"].as_bool().unwrap())
        };
        // Future-only from 11:00: the 11:30 and 12:00 responses, never the 10:00 one, even with --full.
        assert_eq!(push(false, Some(1793358000)), (2, false));
        assert_eq!(push(true, None), (2, false));
        // Moving the start back to everything resends from it once.
        assert_eq!(push(false, Some(0)), (3, false));
        let fresh = tempfile::tempdir().unwrap();
        let path = fresh.path().join("push.json");
        let reply = send_with(
            &db,
            &sync,
            false,
            None,
            &path,
            "m-fixture",
            "fixture",
            |b| Ok(json!({"accepted":b["rows"].as_array().unwrap().len(),"skipped":0})),
        )
        .unwrap();
        assert_eq!(reply["defaulted"], true);
        assert_eq!(parse_start("2026-10-31").unwrap(), 1793404800);
        assert!(parse_start("yesterday").is_err());
    }
    #[test]
    fn hours_a_reread_moved_are_resent_once_with_zero_rows_for_vanished_keys() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        // 10:00 UTC, long before the last push: the response now belongs to s-child; the app has it under s-parent.
        db.upsert(&Row {
            id: "r".into(),
            harness: "codex".into(),
            session: "s-child".into(),
            model: "gpt-6.1-sol".into(),
            at: 1793354400,
            input: Some(7),
            ..Default::default()
        })
        .unwrap();
        db.db.execute("INSERT INTO repairs(hour,harness,model,effort,project,session,sidechain) VALUES(1793354400,'codex','gpt-6.1-sol',NULL,NULL,'s-parent',0)", []).unwrap();
        let sync = Sync {
            org: "fixture".into(),
            url: None,
            auto: false,
            routes: RoutesFrom::Local,
        };
        let path = d.path().join("push.json");
        std::fs::write(
            &path,
            serde_json::to_vec(
                &json!({crate::digest(&format!("{}:{}", sync.org, sync.url())): 1793440800}),
            )
            .unwrap(),
        )
        .unwrap();
        let push = || {
            let mut rows = vec![];
            send_with(
                &db,
                &sync,
                false,
                None,
                &path,
                "m-fixture",
                "fixture",
                |b| {
                    rows.extend(b["rows"].as_array().unwrap().iter().map(|r| {
                        (
                            r["session"].as_str().unwrap().to_string(),
                            r["responses"].as_i64().unwrap(),
                            r["input"].as_i64().unwrap(),
                        )
                    }));
                    Ok(json!({"accepted":b["rows"].as_array().unwrap().len(),"skipped":0}))
                },
            )
            .unwrap();
            rows.sort();
            rows
        };
        assert_eq!(
            push(),
            [
                ("s-child".to_string(), 1, 7),
                ("s-parent".to_string(), 0, 0)
            ]
        );
        assert_eq!(push(), []);
    }
    fn sync() -> Sync {
        Sync {
            org: "fixture".into(),
            url: None,
            auto: false,
            routes: RoutesFrom::Local,
        }
    }
    fn response(db: &Store, id: &str, session: &str, at: i64) {
        db.upsert(&Row {
            id: id.into(),
            harness: "codex".into(),
            session: session.into(),
            model: "gpt-6.1-sol".into(),
            at,
            input: Some(7),
            ..Default::default()
        })
        .unwrap();
    }
    /// (session, responses, activeMs) of every row a push sends.
    fn sent(db: &Store, path: &Path, chosen: Option<i64>) -> Vec<(String, i64, i64)> {
        let mut rows = vec![];
        send_with(
            db,
            &sync(),
            false,
            chosen,
            path,
            "m-fixture",
            "fixture",
            |b| {
                let batch = b["rows"].as_array().unwrap();
                rows.extend(batch.iter().map(|r| {
                    (
                        r["session"].as_str().unwrap().to_string(),
                        r["responses"].as_i64().unwrap(),
                        r["activeMs"].as_i64().unwrap(),
                    )
                }));
                Ok(json!({"accepted":batch.len(),"skipped":0}))
            },
        )
        .unwrap();
        rows.sort();
        rows
    }
    #[test]
    fn a_moved_response_records_its_old_key_in_the_same_write() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        response(&db, "r", "s-parent", 1793354400);
        response(&db, "r", "s-parent", 1793354400);
        response(&db, "r", "s-child", 1793354400);
        let moved: Vec<String> = db
            .db
            .prepare("SELECT session FROM repairs")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(moved, ["s-parent"]);
    }
    #[test]
    fn repairs_before_the_start_wait_until_it_moves_back() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        response(&db, "r", "s-parent", 1793354400);
        response(&db, "r", "s-child", 1793354400);
        let path = d.path().join("push.json");
        // Start at 11:00: the 10:00 repair is neither sent nor consumed.
        assert_eq!(sent(&db, &path, Some(1793358000)), []);
        assert_eq!(
            sent(&db, &path, Some(0)),
            [("s-child".into(), 1, 0), ("s-parent".into(), 0, 0)]
        );
        assert_eq!(sent(&db, &path, None), []);
    }
    #[test]
    fn the_start_survives_a_failed_first_push() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        response(&db, "old", "s-1", 1793354400);
        let path = d.path().join("push.json");
        let failed = send_with(
            &db,
            &sync(),
            false,
            Some(1793358000),
            &path,
            "m-fixture",
            "fixture",
            |_| Err("offline".into()),
        );
        assert!(failed.is_err());
        assert_eq!(sent(&db, &path, None), []);
    }
    #[test]
    fn a_later_start_after_pushing_never_cuts_into_an_hour_the_app_has() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        // 10:10 and 10:40 UTC, both already sent.
        response(&db, "a", "s-1", 1793354400 + 600);
        response(&db, "b", "s-1", 1793354400 + 2400);
        let path = d.path().join("push.json");
        assert_eq!(sent(&db, &path, None), [("s-1".into(), 2, 300_000)]);
        // A 10:30 start becomes 11:00, so the 10:00 row is not replaced by half of itself.
        assert_eq!(sent(&db, &path, Some(1793354400 + 1800)), []);
        let starts: BTreeMap<String, i64> =
            serde_json::from_slice(&std::fs::read(d.path().join("monitor-start.json")).unwrap())
                .unwrap();
        assert_eq!(starts.values().copied().collect::<Vec<_>>(), [1793358000]);
    }
    #[test]
    fn many_moves_from_one_key_send_one_zero_row() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        for i in 0..50 {
            let id = format!("r{i}");
            response(&db, &id, "s-parent", 1793354400 + i);
            response(&db, &id, &format!("s-child{}", i % 2), 1793354400 + i);
        }
        let rows = sent(&db, &d.path().join("push.json"), None);
        assert_eq!(rows.iter().filter(|r| r.0 == "s-parent").count(), 1);
        assert_eq!(rows.iter().map(|r| r.1).sum::<i64>(), 50);
        assert_eq!(
            sent(&db, &d.path().join("push.json"), None)
                .iter()
                .filter(|r| r.0 == "s-parent")
                .count(),
            0
        );
    }
    #[test]
    fn a_mid_hour_start_counts_no_active_time_before_it() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        // 10:29 and 10:31 UTC; the start is 10:30.
        response(&db, "a", "s-1", 1793356140);
        response(&db, "b", "s-1", 1793356260);
        assert_eq!(
            sent(&db, &d.path().join("push.json"), Some(1793356200)),
            [("s-1".into(), 1, 0)]
        );
    }
    #[test]
    fn local_http_wire_format() {
        use std::io::{BufRead, Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            let (stream, _) = server.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("POST /v1/monitor/usage "));
            let mut len = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if line.to_ascii_lowercase().starts_with("content-length:") {
                    len = line
                        .split(':')
                        .nth(1)
                        .unwrap()
                        .trim()
                        .parse::<usize>()
                        .unwrap();
                }
            }
            let mut bytes = vec![0; len];
            reader.read_exact(&mut bytes).unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["org"], "fixture");
            let reply = r#"{"accepted":0,"skipped":0,"historyDays":90}"#;
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.len(),
                reply
            )
            .unwrap();
        });
        let sync = Sync {
            org: "fixture".into(),
            url: Some(format!("http://127.0.0.1:{port}/v1")),
            auto: false,
            routes: RoutesFrom::Local,
        };
        let v = cedr_route::sync::send_monitor(
            &sync,
            "synthetic-key",
            &json!({"org":"fixture","rows":[]}),
        )
        .unwrap();
        assert_eq!(v["historyDays"], 90);
        worker.join().unwrap();
    }

    #[test]
    fn wire_rows_carry_counts_and_keep_nullable_fields() {
        let mut row = json!({"model": "m", "effort": null, "project": null, "contextPeak": null, "reportedCostUsd": null,
            "input": null, "output": 20_000_000_000_i64, "responses": 3});
        wire_row(&mut row);
        assert_eq!(
            row,
            json!({"model": "m", "effort": null, "project": null, "contextPeak": null, "reportedCostUsd": null,
            "input": 0, "output": 10_000_000_000_i64, "responses": 3})
        );
    }
}
