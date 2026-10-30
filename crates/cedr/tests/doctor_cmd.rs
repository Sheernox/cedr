//! Doctor must work in an empty home without network or persistent writes.
use serde_json::Value;
use std::{fs, process::Command};
#[test]
fn doctor_json_is_read_only_and_has_stable_checks() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".git")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cedr"))
        .args(["doctor", "--format", "json"])
        .arg(root.path())
        .env_clear()
        .env("CEDR_TELEMETRY", "0")
        .env("CEDR_NO_AUTO_UPDATE", "1")
        .env("HOME", home.path())
        .env("PATH", "")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let checks = value["checks"].as_array().unwrap();
    for id in [
        "cli.version",
        "key.present",
        "key.valid",
        "org.member",
        "route.config",
        "route.readers",
        "route.statusline",
        "route.sync",
        "mcp.claude",
        "mcp.codex",
        "monitor.config",
        "monitor.sources",
        "monitor.push",
        "monitor.schedule",
        "sync.config",
        "sync.check",
        "sync.rules",
        "guard.lock",
    ] {
        assert!(checks.iter().any(|c| c["id"] == id), "missing {id}");
    }
    for c in checks {
        assert!(c["area"].is_string());
        assert!(c["message"].is_string());
        assert!(["ok", "warn", "fail", "skip"].contains(&c["state"].as_str().unwrap()));
        assert_eq!(c["fix"].is_null(), c["state"] == "ok");
    }
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}
#[test]
fn doctor_uses_public_org_id_from_me() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let config = home.path().join(".config/cedr");
    fs::create_dir_all(&config).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    fs::write(
        config.join("monitor.toml"),
        format!("org='org-fixture'\nurl={url:?}\n"),
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut bytes = [0; 4096];
        let n = socket.read(&mut bytes).unwrap();
        assert!(String::from_utf8_lossy(&bytes[..n]).starts_with("GET /v1/me "));
        let body = r#"{"user":{"name":"Example owner"},"organizations":[{"id":"org-fixture","slug":"different-slug"}]}"#;
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let out = Command::new(env!("CARGO_BIN_EXE_cedr"))
        .args(["doctor", "--format", "json"])
        .arg(root.path())
        .env_clear()
        .env("CEDR_TELEMETRY", "0")
        .env("CEDR_NO_AUTO_UPDATE", "1")
        .env("HOME", home.path())
        .env("PATH", "")
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let checks = v["checks"].as_array().unwrap();
    for id in ["key.valid", "org.member"] {
        assert!(checks.iter().any(|c| c["id"] == id && c["state"] == "ok"));
    }
    assert!(!String::from_utf8_lossy(&out.stdout).contains("test-key"));
}
#[test]
fn doctor_warns_for_new_release_and_org_rules_using_only_local_mocks() {
    use serde_json::json;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let cfg = home.path().join(".config/cedr");
    fs::create_dir_all(&cfg).unwrap();
    fs::create_dir(root.path().join(".cedr")).unwrap();
    fs::write(
        root.path().join(".cedr/sync.toml"),
        "version=1\ntargets=['agents']",
    )
    .unwrap();
    let mut rules = json!({"org":"org-fixture","version":7});
    rules["hash"] = json!(cedr_sync::org::policy_hash(&rules));
    fs::write(
        root.path().join(".cedr/org-rules.json"),
        serde_json::to_vec(&rules).unwrap(),
    )
    .unwrap();
    let plan = cedr_sync::plan(root.path()).unwrap();
    cedr_sync::apply(root.path(), &plan, false).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    fs::write(
        cfg.join("monitor.toml"),
        format!("org='org-fixture'\nurl={url:?}\n"),
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..3 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]);
            let body = if request.starts_with("GET /v1/me ") {
                json!({"name":"Example","organizations":[{"id":"org-fixture"}]})
            } else if request.starts_with("GET /v1/releases/latest ") {
                json!({"tag_name":"v99.0.0"})
            } else {
                assert!(request.starts_with("GET /v1/sync/rules?org=org-fixture "));
                json!({"version":8})
            };
            let body = body.to_string();
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    let out = Command::new(env!("CARGO_BIN_EXE_cedr"))
        .args(["doctor", "--format", "json"])
        .arg(root.path())
        .env_clear()
        .env("CEDR_TELEMETRY", "0")
        .env("CEDR_NO_AUTO_UPDATE", "1")
        .env("HOME", home.path())
        .env("PATH", "")
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    for id in ["cli.version", "sync.rules"] {
        assert!(
            v["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["id"] == id && c["state"] == "warn")
        );
    }
}
