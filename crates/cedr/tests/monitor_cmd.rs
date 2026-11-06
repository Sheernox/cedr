//! Monitor subprocesses use isolated synthetic homes and never need network access.
mod common;
use std::process::Command;
#[test]
fn empty_sources_and_json_report() {
    let d = tempfile::tempdir().unwrap();
    for args in [
        vec!["monitor", "sources", "--format", "json"],
        vec!["monitor", "--since", "7d", "--format", "json"],
        vec!["monitor", "report", "--by", "day", "--format", "json"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_cedr"))
            .args(&args)
            .env("HOME", d.path())
            .env("XDG_STATE_HOME", d.path().join("state"))
            .env("XDG_CONFIG_HOME", d.path().join("config"))
            .env("XDG_DATA_HOME", d.path().join("data"))
            .env("CLAUDE_CONFIG_DIR", d.path().join("claude"))
            .env("CODEX_HOME", d.path().join("codex"))
            .env("COPILOT_HOME", d.path().join("copilot"))
            .env("OPENCODE_DB", d.path().join("missing.db"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        if args[1] == "sources" {
            assert!(
                v.as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["harness"] == "cursor" && r["state"] == "unsupported")
            );
        } else {
            assert!(v["groups"].as_array().unwrap().is_empty());
        }
    }
}
#[test]
fn a_push_with_nowhere_to_send_fails_before_reading_any_log() {
    let d = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cedr"))
        .args(["monitor", "push", "--quiet"])
        .current_dir(d.path())
        .env("HOME", d.path())
        .env("XDG_STATE_HOME", d.path().join("state"))
        .env("XDG_CONFIG_HOME", d.path().join("config"))
        .env("XDG_DATA_HOME", d.path().join("data"))
        .env_remove("CEDR_API_KEY")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no organization"));
    assert!(
        !d.path().join("state/cedr/monitor.sqlite").exists(),
        "collected logs before finding there was nowhere to send them"
    );
}
#[test]
fn limits_include_claude_codes_usage_cache_without_a_status_line() {
    let d = tempfile::tempdir().unwrap();
    let claude = d.path().join("claude");
    std::fs::create_dir_all(&claude).unwrap();
    let now = chrono::Utc::now();
    let cache = serde_json::json!({"oauthAccount": {"accountUuid": "acct-a"}, "cachedUsageUtilization": {
        "accountUuid": "acct-a", "fetchedAtMs": now.timestamp_millis(), "utilization": {
        "five_hour": {"utilization": 39, "resets_at": (now + chrono::Duration::hours(3)).to_rfc3339()}}}});
    std::fs::write(claude.join(".claude.json"), cache.to_string()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cedr"))
        .args(["monitor", "limits", "--format", "json"])
        .env("HOME", d.path())
        .env("PATH", "")
        .env("XDG_STATE_HOME", d.path().join("state"))
        .env("XDG_CONFIG_HOME", d.path().join("config"))
        .env("XDG_DATA_HOME", d.path().join("data"))
        .env("CLAUDE_CONFIG_DIR", &claude)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v.to_string().contains(r#""harness":"claude""#), "{v}");
    assert!(v.to_string().contains(r#""percent":39.0"#), "{v}");
}

#[test]
fn limits_text_rounds_elapsed_share_but_json_keeps_number() {
    let scratch = common::Scratch::new(20.0, 40.0);
    let db =
        cedr_monitor::store::Store::open(&scratch.dir.path().join("state/cedr/monitor.sqlite"))
            .unwrap();
    let now = chrono::Utc::now();
    db.db.execute("INSERT INTO readings(harness,window,at,data) VALUES(?1,?2,?3,?4)", [
        "test".to_string(), "window".to_string(), now.to_rfc3339(), serde_json::json!({"harness":"test","window":"window","percent":4,"windowMinutes":300,"resetsAt":(now + chrono::Duration::minutes(207)).to_rfc3339()}).to_string()
    ]).unwrap();
    let run = |format| {
        scratch
            .command(&["monitor", "limits", "--format", format])
            .env("HOME", scratch.dir.path())
            .env("CODEX_HOME", scratch.dir.path().join("codex"))
            .env("COPILOT_HOME", scratch.dir.path().join("copilot"))
            .env("GROK_HOME", scratch.dir.path().join("grok"))
            .env("OPENCODE_DB", scratch.dir.path().join("missing.db"))
            .env("PATH", scratch.dir.path().join("empty-bin"))
            .output()
            .unwrap()
    };
    let text = run("text");
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    assert!(
        common::stdout(&text).contains("elapsed=31%"),
        "{}",
        common::stdout(&text)
    );
    let out = run("json");
    assert!(out.status.success());
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["harness"] == "test")
        .unwrap();
    assert!((row["elapsedShare"].as_f64().unwrap() - 0.31).abs() < 0.001);
    assert!(row["runsOutAt"].is_null());
}
