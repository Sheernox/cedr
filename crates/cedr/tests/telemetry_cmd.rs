//! Telemetry (docs/telemetry.md) against a loopback endpoint: the notice, the queue, what a flush
//! sends, and every off switch. Each run gets an empty home and environment.
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

fn cedr(home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_cedr"));
    c.args(args)
        .current_dir(home)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "")
        .env("CEDR_NO_AUTO_UPDATE", "1")
        // Nothing listens here, so a flush a test does not expect fails without leaving the machine.
        .env("CEDR_TELEMETRY_URL", "http://127.0.0.1:9/v1/telemetry");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn queue(home: &Path) -> Vec<Value> {
    std::fs::read_to_string(home.join(".local/state/cedr/telemetry.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn settings(home: &Path) -> toml::Table {
    std::fs::read_to_string(home.join(".config/cedr/cli.toml"))
        .unwrap()
        .parse()
        .unwrap()
}

/// Accepts one request and answers 200; returns the request line and the JSON body.
fn accept_one(listener: &TcpListener) -> (String, Value) {
    listener.set_nonblocking(false).unwrap();
    let (socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        if header.trim().is_empty() {
            break;
        }
        if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let reply = r#"{"accepted":1,"skipped":0}"#;
    write!(
        &socket,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    )
    .unwrap();
    (line, serde_json::from_slice(&body).unwrap())
}

#[test]
fn the_first_run_shows_the_notice_and_the_flush_sends_only_contract_fields() {
    let home = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/telemetry", listener.local_addr().unwrap());
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    let out = cedr(home.path(), &["lint", "."], &[("CEDR_TELEMETRY_URL", &url)]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cedr telemetry off"), "{stderr}");
    let s = settings(home.path());
    let install = s["install_id"].as_str().unwrap().to_string();
    assert_eq!(s["notice"].as_integer(), Some(1));

    let (line, body) = accept_one(&listener);
    assert!(line.starts_with("POST /v1/telemetry "), "{line}");
    assert_eq!(body["install"], install.as_str());
    assert_eq!(body["ci"], false);
    assert_eq!(body["agents"], serde_json::json!(["claude"]));
    let event = &body["events"][0];
    assert_eq!(event["command"], "lint");
    assert_eq!(event["version"], env!("CARGO_PKG_VERSION"));
    let mut keys: Vec<_> = event.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["at", "command", "exit", "ms", "version"]);
    let text = body.to_string();
    assert!(
        !text.contains(&home.path().display().to_string()),
        "no paths: {text}"
    );

    let again = cedr(home.path(), &["lint", "."], &[]);
    assert!(!String::from_utf8_lossy(&again.stderr).contains("telemetry off"));
    assert_eq!(
        settings(home.path())["install_id"].as_str(),
        Some(install.as_str())
    );
}

#[test]
fn ci_runs_print_no_notice_and_never_reuse_an_install_id() {
    let home = tempfile::tempdir().unwrap();
    let out = cedr(home.path(), &["lint", "."], &[("GITHUB_ACTIONS", "true")]);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("telemetry"));
    assert!(!home.path().join(".config/cedr/cli.toml").exists());
    // The flush (always started in CI) may already have taken the queue; either way it is a CI event.
    let state = home.path().join(".local/state/cedr");
    let queued = std::fs::read_to_string(state.join("telemetry.jsonl"))
        .or_else(|_| std::fs::read_to_string(state.join("telemetry.sending")))
        .unwrap_or_default();
    if let Some(line) = queued.lines().next() {
        let event: Value = serde_json::from_str(line).unwrap();
        assert_eq!(event["ci"], true);
    }
}

#[test]
fn off_deletes_the_id_and_queue_and_records_nothing_after() {
    let home = tempfile::tempdir().unwrap();
    cedr(home.path(), &["lint", "."], &[]);
    assert!(settings(home.path()).contains_key("install_id"));
    let out = cedr(home.path(), &["telemetry", "off"], &[]);
    assert!(out.status.success());
    let s = settings(home.path());
    assert_eq!(s["telemetry"].as_bool(), Some(false));
    assert!(!s.contains_key("install_id"));
    assert!(queue(home.path()).is_empty());
    cedr(home.path(), &["lint", "."], &[]);
    assert!(queue(home.path()).is_empty());

    let status = cedr(
        home.path(),
        &["telemetry", "status", "--format", "json"],
        &[],
    );
    let v: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(v["enabled"], false);
    assert_eq!(v["reason"], "config");
}

#[test]
fn environment_opt_outs_write_nothing() {
    for (var, value) in [("DO_NOT_TRACK", "1"), ("CEDR_TELEMETRY", "off")] {
        let home = tempfile::tempdir().unwrap();
        let out = cedr(home.path(), &["lint", "."], &[(var, value)]);
        assert!(!String::from_utf8_lossy(&out.stderr).contains("telemetry"));
        assert!(!home.path().join(".config").exists(), "{var}");
        assert!(queue(home.path()).is_empty(), "{var}");
        let status = cedr(
            home.path(),
            &["telemetry", "status", "--format", "json"],
            &[(var, value)],
        );
        let v: Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(v["reason"], var);
    }
}

#[test]
fn completions_record_nothing() {
    let home = tempfile::tempdir().unwrap();
    cedr(home.path(), &["completions", "bash"], &[]);
    assert!(!home.path().join(".config").exists());
    assert!(queue(home.path()).is_empty());
}

#[test]
fn a_failed_flush_keeps_each_event_once_and_off_removes_what_is_pending() {
    let home = tempfile::tempdir().unwrap();
    let state = home.path().join(".local/state/cedr");
    cedr(home.path(), &["lint", "."], &[]);
    // The first run starts a flush to an endpoint nothing listens on; give it time to fail.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !state.join("telemetry.sending").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    while state.join("telemetry.lock").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let sending = std::fs::read_to_string(state.join("telemetry.sending")).unwrap();
    assert_eq!(
        sending.lines().count(),
        1,
        "the unsent event stays for the next flush"
    );
    assert!(
        queue(home.path()).is_empty(),
        "and is not also put back in the queue"
    );

    cedr(home.path(), &["telemetry", "off"], &[]);
    assert!(!state.join("telemetry.sending").exists());
    assert!(!state.join("telemetry.jsonl").exists());
}

#[test]
fn telemetry_on_shows_the_notice() {
    let home = tempfile::tempdir().unwrap();
    let out = cedr(home.path(), &["telemetry", "on"], &[]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("cedr telemetry off"));
}
