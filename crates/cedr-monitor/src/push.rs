//! Batch complete hours; commit the watermark only after all batches have succeeded.
use crate::{Result, store::Store};
use cedr_route::{
    config::{RoutesFrom, Sync},
    machine,
};
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
    send_with(db, sync, full, &path, &machine, &name, |body| {
        Ok(cedr_route::sync::send_monitor(sync, key, body)?)
    })
}
pub fn send_with(
    db: &Store,
    sync: &Sync,
    full: bool,
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
    let identity = crate::digest(&format!("{}:{}", sync.org, sync.url()));
    let previous = watermarks.get(&identity).copied().unwrap_or(0);
    let at = chrono::Utc::now().timestamp();
    let mut rows = crate::report::hourly(
        db,
        if full {
            0
        } else {
            previous.saturating_sub(7200).div_euclid(3600) * 3600
        },
    )?;
    for row in &mut rows {
        wire_row(row);
    }
    let mut q = db
        .db
        .prepare("SELECT data FROM readings WHERE at>=? ORDER BY at DESC LIMIT 500")?;
    let readings = q
        .query_map(
            [crate::report::stamp(if full { 0 } else { previous })],
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
    watermarks.insert(identity, at);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(&watermarks)?)?;
    std::fs::rename(tmp, path)?;
    Ok(json!({"accepted":accepted,"skipped":skipped}))
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
        let result = send_with(&db, &sync, false, &path, "m-fixture", "fixture", |b| {
            calls += 1;
            if calls == 2 {
                return Err("synthetic failure".into());
            }
            assert_eq!(b["rows"].as_array().unwrap().len(), 5000);
            Ok(json!({"accepted":5000,"skipped":0}))
        });
        assert!(result.is_err());
        assert!(!path.exists());
        let mut sizes = vec![];
        send_with(&db, &sync, false, &path, "m-fixture", "fixture", |b| {
            let n = b["rows"].as_array().unwrap().len();
            sizes.push(n);
            Ok(json!({"accepted":n,"skipped":0}))
        })
        .unwrap();
        assert_eq!(sizes, [5000, 1]);
        assert!(path.exists());
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
