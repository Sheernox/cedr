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
    // Nothing here needs the hosted app, so a missing key is not a failure.
    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let checks = value["checks"].as_array().unwrap();
    let mut ids: Vec<&str> = checks.iter().map(|c| c["id"].as_str().unwrap()).collect();
    ids.dedup();
    // Every check sits beside the others of its area.
    assert_eq!(
        ids,
        [
            "cli.version",
            "key.present",
            "key.valid",
            "org.member",
            "route.config",
            "route.readers",
            "route.orchestrator",
            "route.targets",
            "route.statusline",
            "route.sync",
            "mcp.claude",
            "mcp.codex",
            "mcp.grok",
            "monitor.config",
            "monitor.sources",
            "monitor.push",
            "monitor.schedule",
            "sync.config",
            "sync.check",
            "sync.shadowed",
            "sync.rules",
            "guard.lock",
            "bench.budget",
            "bench.push",
            "bench.suite",
        ]
    );
    assert_eq!(checks[1]["state"], "skip");
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

/// Runs doctor in `home` with an empty PATH and returns one check's state and message.
fn doctor_check(home: &std::path::Path, id: &str) -> (String, String) {
    let root = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cedr"))
        .args(["doctor", "--format", "json"])
        .arg(root.path())
        .env_clear()
        .env("CEDR_TELEMETRY", "0")
        .env("CEDR_NO_AUTO_UPDATE", "1")
        .env("HOME", home)
        .env("PATH", "")
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
}

#[test]
fn doctor_lists_only_the_stale_readers_a_pick_warns_about_and_asks_for_t3_code() {
    let home = tempfile::tempdir().unwrap();
    let cfg = home.path().join(".config/cedr");
    fs::create_dir_all(&cfg).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    fs::write(
        cfg.join("feed.json"),
        format!(
            r#"{{"harnesses": [{{"harness": "codex", "fetchedAt": "{now}", "windows": []}}]}}"#
        ),
    )
    .unwrap();
    // grok is not installed; codex has no built-in reader here, but a feed covers it; muse neither.
    let routes = "version = 1\n[harnesses.grok]\ncommand = \"grok\"\nreader = \"grok\"\n[harnesses.codex]\nreader = \"codex\"\n[harnesses.muse]\nreader = \"muse\"\n[[readings]]\nkind = \"file\"\npath = \"feed.json\"\n";
    fs::write(cfg.join("routes.toml"), routes).unwrap();
    assert_eq!(
        doctor_check(home.path(), "route.readers"),
        (
            "warn".to_string(),
            "absent harnesses: [grok]; stale readers: [muse]".to_string()
        )
    );
    // T3 Code on the machine, unread: doctor says so.
    let caches = home.path().join(".t3/caches");
    fs::create_dir_all(&caches).unwrap();
    let (_, message) = doctor_check(home.path(), "route.readers");
    assert!(
        message.ends_with("T3 Code runs on this machine but no readings source reads it"),
        "{message}"
    );
    // Read first, it covers muse and Claude, which then needs neither a status line nor its cache.
    fs::write(
        cfg.join("routes.toml"),
        routes.replace(
            "[[readings]]",
            "[harnesses.claude]\nt3 = \"claudeAgent\"\n[[readings]]\nkind = \"t3\"\n[[readings]]",
        ),
    )
    .unwrap();
    let windows = format!(
        r#"{{"checkedAt": "{now}", "windows": [{{"id": "weekly", "kind": "weekly", "usedPercent": 5}}]}}"#
    );
    for (instance, driver) in [("muse", "muse"), ("claudeAgent", "claudeAgent")] {
        fs::write(
            caches.join(format!("{instance}.json")),
            format!(r#"{{"driver": "{driver}", "enabled": true, "usageLimits": {windows}}}"#),
        )
        .unwrap();
    }
    assert_eq!(
        doctor_check(home.path(), "route.readers"),
        (
            "warn".to_string(),
            "absent harnesses: [grok]; stale readers: []".to_string()
        )
    );
    assert_eq!(
        doctor_check(home.path(), "route.statusline"),
        ("ok".to_string(), "readings come from T3 Code".to_string())
    );
}

#[test]
fn doctor_names_targets_no_model_matches_and_sync_skills_agents_also_load_elsewhere() {
    let home = tempfile::tempdir().unwrap();
    let cfg = home.path().join(".config/cedr");
    fs::create_dir_all(&cfg).unwrap();
    fs::write(
        cfg.join("routes.toml"),
        "version = 1\n[harnesses.codex]\n[routes.r]\ncandidates = [{ target = \"codex/sol\" }, { target = \"codex/gpt-5.5\" }, { target = \"self\" }]\n[[readings]]\nkind = \"t3\"\n",
    )
    .unwrap();
    let caches = home.path().join(".t3/caches");
    fs::create_dir_all(&caches).unwrap();
    fs::write(
        caches.join("codex.json"),
        r#"{"driver": "codex", "enabled": true, "models": [{"slug": "gpt-6.1-sol"}],
            "skills": [{"name": "council", "scope": "user"}, {"name": "other", "scope": "user"}]}"#,
    )
    .unwrap();
    assert_eq!(
        doctor_check(home.path(), "route.targets"),
        (
            "warn".to_string(),
            "targets no offered model matches: [codex/gpt-5.5]".to_string()
        )
    );
    // A present harness with no model list resolves none of its targets either.
    fs::write(
        cfg.join("routes.toml"),
        "version = 1\n[harnesses.codex]\n[harnesses.claude]\n[harnesses.gone]\ncommand = \"gone\"\n[harnesses.muse]\n[harnesses.cx]\n[[readings]]\nkind = \"codex\"\nharness = \"cx\"\n[routes.r]\ncandidates = [{ target = \"codex/sol\" }, { target = \"codex/gpt-5.5\" }, { target = \"claude/opus\" }, { target = \"gone/x\" }, { target = \"cx/sol\" }]\n[routes.off]\nenabled = false\ncandidates = [{ target = \"muse/x\" }]\n[[readings]]\nkind = \"t3\"\n",
    )
    .unwrap();
    assert_eq!(
        doctor_check(home.path(), "route.targets").1,
        "harnesses with no model list: [claude]; targets no offered model matches: [codex/gpt-5.5]"
    );
    fs::write(
        cfg.join("routes.toml"),
        "version = 1\n[harnesses.claude]\nmodels = [\"claude-opus-5-5\"]\n[routes.r]\ncandidates = [{ target = \"claude/opus\" }]\n",
    )
    .unwrap();
    assert_eq!(doctor_check(home.path(), "route.targets").0, "ok");
    // An effort off the ladder passes as written: a warning unless the model offers it.
    fs::write(
        caches.join("codex.json"),
        r#"{"driver": "codex", "enabled": true, "models": [{"slug": "gpt-6.1-sol", "capabilities": {"optionDescriptors": [
            {"id": "reasoningEffort", "options": [{"id": "high"}, {"id": "turbo"}]}]}}],
            "skills": [{"name": "council", "scope": "user"}, {"name": "other", "scope": "user"}]}"#,
    )
    .unwrap();
    fs::write(
        cfg.join("routes.toml"),
        "version = 1\n[harnesses.codex]\n[harnesses.claude]\nmodels = [\"claude-opus-5-5\"]\n[routes.r]\ncandidates = [{ target = \"codex/sol\", effort = \"turbo\" }, { target = \"codex/sol\", effort = \"hgih\" }, { target = \"claude/opus\", effort = \"ultrathink\" }, { target = \"claude/opus\", effort = \"fast\" }]\n[[readings]]\nkind = \"t3\"\n",
    )
    .unwrap();
    assert_eq!(
        doctor_check(home.path(), "route.targets"),
        (
            "warn".to_string(),
            "efforts off the ladder that no model reading offers: [r claude/opus=fast, r codex/sol=hgih]".to_string()
        )
    );
    // Absent providers and self have no model options to judge. Account readers are deferred.
    fs::write(cfg.join("routes.toml"), "version=1\n[harnesses.codex]\n[harnesses.gone]\ncommand='gone'\nmodels=['x']\n[harnesses.cx]\nmodels=['gpt-6.1-sol']\n[[readings]]\nkind='codex'\nharness='cx'\n[[readings]]\nkind='t3'\n[routes.r]\ncandidates=[{target='codex/sol',effort='turbo'},{target='gone/x',effort='hgih'},{target='self',effort='hgih'},{target='cx/sol',effort='turbo'}]\n").unwrap();
    assert_eq!(doctor_check(home.path(), "route.targets").0, "ok");
    // A repo whose Sync writes `council` to the shared skills folder Codex reads.
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join(".cedr/skills/council")).unwrap();
    fs::write(
        root.path().join(".cedr/skills/council/SKILL.md"),
        "---\nname: council\ndescription: d\n---\n",
    )
    .unwrap();
    fs::write(
        root.path().join(".cedr/sync.toml"),
        "version = 1\ntargets = [\"agents\"]\n",
    )
    .unwrap();
    let shadowed = |root: &std::path::Path| {
        let out = Command::new(env!("CARGO_BIN_EXE_cedr"))
            .args(["doctor", "--format", "json"])
            .arg(root)
            .env_clear()
            .env("CEDR_TELEMETRY", "0")
            .env("CEDR_NO_AUTO_UPDATE", "1")
            .env("HOME", home.path())
            .env("PATH", "")
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        let c = v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "sync.shadowed")
            .cloned()
            .unwrap();
        (
            c["state"].as_str().unwrap().to_string(),
            c["message"].as_str().unwrap().to_string(),
        )
    };
    assert_eq!(
        shadowed(root.path()),
        (
            "warn".to_string(),
            "agents also load these Sync skill names from elsewhere: codex: council (user)"
                .to_string()
        )
    );
    // Sync writes the shared folder Codex reads whatever its targets name.
    fs::write(
        root.path().join(".cedr/sync.toml"),
        "version = 1\ntargets = [\"claude\"]\n",
    )
    .unwrap();
    assert_eq!(shadowed(root.path()).0, "warn");
    // A Sync skill no agent also loads elsewhere is fine.
    fs::rename(
        root.path().join(".cedr/skills/council"),
        root.path().join(".cedr/skills/review"),
    )
    .unwrap();
    assert_eq!(shadowed(root.path()).0, "ok");
}

#[test]
fn a_missing_key_fails_only_when_an_org_needs_it_and_a_broken_key_always_does() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let cfg = home.path().join(".config/cedr");
    fs::create_dir_all(&cfg).unwrap();
    assert_eq!(doctor_check(home.path(), "key.present").0, "skip");
    // An org in routes.toml [sync] (or monitor.toml) needs the key.
    fs::write(
        cfg.join("routes.toml"),
        "version = 1\n[sync]\norg = \"org-fixture\"\n",
    )
    .unwrap();
    assert_eq!(
        doctor_check(home.path(), "key.present"),
        (
            "fail".to_string(),
            "no API key, but an organization is configured for the hosted app".to_string()
        )
    );
    fs::remove_file(cfg.join("routes.toml")).unwrap();
    // Without an org, a key that exists but cannot be used still fails, and is never sent.
    let key = cfg.join("api-key");
    for (bytes, mode) in [(&b"k-1"[..], 0o644), (&b"  \n"[..], 0o600)] {
        fs::write(&key, bytes).unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(mode)).unwrap();
        let (state, message) = doctor_check(home.path(), "key.present");
        assert_eq!(state, "fail", "{message}");
        assert!(message.starts_with("API key unusable"), "{message}");
        assert_eq!(doctor_check(home.path(), "key.valid").1, "no key");
    }
}

#[test]
fn doctor_skips_absent_monitor_sources_but_warns_for_empty_configured_ones() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join(".claude/projects")).unwrap();
    let cfg = home.path().join(".config/cedr");
    fs::create_dir_all(&cfg).unwrap();
    fs::write(cfg.join("monitor.toml"), "[paths]\nmuse='~/missing-muse'\n").unwrap();
    for saved in [false, true] {
        if saved {
            let db = cedr_monitor::store::Store::open(
                &home.path().join(".local/state/cedr/monitor.sqlite"),
            )
            .unwrap();
            for harness in cedr_monitor::HARNESSES {
                db.db.execute("INSERT OR REPLACE INTO sources(harness,data) VALUES (?1,?2)", (harness, serde_json::json!({"harness": harness, "state":"absent", "files":0,"malformed":0}).to_string())).unwrap();
            }
        }
        let out = Command::new(env!("CARGO_BIN_EXE_cedr"))
            .args(["doctor", "--format", "json"])
            .arg(root.path())
            .env_clear()
            .env("HOME", home.path())
            .env("PATH", "")
            .env("CEDR_TELEMETRY", "0")
            .env("CEDR_NO_AUTO_UPDATE", "1")
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        for check in v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["id"] == "monitor.sources")
        {
            let message = check["message"].as_str().unwrap();
            let present = message.starts_with("claude:") || message.starts_with("muse:");
            assert_eq!(
                check["state"],
                if present { "warn" } else { "skip" },
                "{check}"
            );
        }
    }
}
