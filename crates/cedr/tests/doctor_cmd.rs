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
        "route.orchestrator",
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
#[cfg(unix)]
#[test]
fn doctor_says_why_claude_has_no_reading_and_when_no_orchestrator_is_set() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let bin = home.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("claude"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    let cfg = home.path().join(".config/cedr");
    fs::create_dir_all(&cfg).unwrap();
    let routes = "version = 1\n[harnesses.claude]\nexpect = [\"five_hour\", \"seven_day\"]\n[[readings]]\nkind = \"claude-statusline\"\n";
    fs::write(cfg.join("routes.toml"), routes).unwrap();
    let check_with = |id: &str, path: &std::path::Path| {
        let out = Command::new(env!("CARGO_BIN_EXE_cedr"))
            .args(["doctor", "--format", "json"])
            .arg(root.path())
            .env_clear()
            .env("CEDR_TELEMETRY", "0")
            .env("CEDR_NO_AUTO_UPDATE", "1")
            .env("HOME", home.path())
            .env("PATH", path)
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        let c = v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .cloned()
            .unwrap();
        (
            c["state"].as_str().unwrap().to_string(),
            c["message"].as_str().unwrap().to_string(),
        )
    };
    let check = |id: &str| check_with(id, &bin);
    let (state, message) = check("route.statusline");
    assert_eq!(state, "warn");
    assert!(
        message.contains("has not cached its plan usage"),
        "{message}"
    );
    assert_eq!(check("route.orchestrator").0, "warn");

    fs::write(
        home.path().join(".claude.json"),
        r#"{"oauthAccount": {"accountUuid": "acct-a"}, "cachedUsageUtilization": {"accountUuid": "acct-a", "fetchedAtMs": 1793750400000, "utilization": {
            "five_hour": {"utilization": 1, "resets_at": "2026-11-06T01:00:00+00:00"}}}}"#,
    )
    .unwrap();
    let (state, message) = check("route.statusline");
    assert_eq!(state, "ok");
    assert!(message.contains("usage cache"), "{message}");
    // Claude that T3 Code runs from a provider profile, with nothing on PATH, is not skipped.
    let profile = home
        .path()
        .join(".t3/userdata/providers/claudeAgent")
        .join("c857d09db23e6822e3600bc06ad8d58f92ed62bc8efd81c753f77048662cb97d");
    fs::create_dir_all(&profile).unwrap();
    fs::write(
        cfg.join("routes.toml"),
        routes.replace(
            "[harnesses.claude]\n",
            "[harnesses.claude]\ncommand = \"claude\"\n",
        ),
    )
    .unwrap();
    assert_eq!(
        check_with("route.statusline", std::path::Path::new("")).0,
        "ok"
    );
    // A second Claude source means the cache serves neither.
    fs::write(
        cfg.join("routes.toml"),
        format!("{routes}[harnesses.work]\n[[readings]]\nkind = \"claude-statusline\"\nharness = \"work\"\n"),
    )
    .unwrap();
    let (state, message) = check("route.statusline");
    assert_eq!(state, "warn");
    assert!(message.contains("only with one"), "{message}");
    fs::write(cfg.join("routes.toml"), routes).unwrap();
    fs::write(
        cfg.join("routes.toml"),
        format!("{routes}[orchestrator]\nharness = \"claude\"\nmodel = \"opus\"\n"),
    )
    .unwrap();
    assert_eq!(
        check("route.orchestrator"),
        ("ok".to_string(), "claude/opus".to_string())
    );
}
