//! Runs `cedr route` and `cedr mcp` against fixtures/route with readings written at test time
//! (fresh timestamps), and a scratch XDG state directory so history never touches the real one.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use common::{Scratch, config, stdout};
use serde_json::{Value, json};

#[test]
fn t3_format_prints_the_tag_and_the_delegate_task_target() {
    let s = Scratch::new(40.0, 20.0);
    let o = s.pick("review.code", &["--format", "t3"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let out = stdout(&o);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("[route review.code -> claude/claude-sonnet-5-5 high | balanced:20%<codex=40% | claude s5% w20%; claude s5% w20% | rules "), "{out}");
    assert_eq!(
        lines[1],
        r#"  delegate_task role=review target={"providerInstanceId": "claudeAgent", "model": "claude-sonnet-5-5", "options": {"effort": "high"}}"#
    );
}

#[test]
fn kept_work_exits_1_and_errors_exit_2() {
    let s = Scratch::new(40.0, 20.0);
    let keep = s.pick("design", &[]);
    assert_eq!(keep.status.code(), Some(1));
    assert_eq!(
        stdout(&keep),
        "keep: design (orchestrator-only route); claude s5% w20%\n"
    );
    let unknown = s.pick("nope", &[]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown route \"nope\""));
}

#[test]
fn a_council_lists_absent_members() {
    let s = Scratch::new(10.0, 95.0);
    let out = stdout(&s.pick("council", &[]));
    assert!(
        out.contains("-> codex/gpt-6.1-sol high | council |"),
        "{out}"
    );
    assert!(
        out.ends_with("absent (report, never substitute): claude=out\n"),
        "{out}"
    );
}

#[test]
fn verdicts_on_recorded_picks_show_in_stats() {
    let s = Scratch::new(10.0, 20.0);
    let pick: Value =
        serde_json::from_slice(&s.pick("review.code", &["--format", "json"]).stdout).unwrap();
    let id = pick["delegates"][0]["id"].as_str().unwrap().to_string();
    assert!(
        pick["delegates"][0]["tag"]
            .as_str()
            .unwrap()
            .ends_with(&format!("| pick {id}]"))
    );
    let ok = s.cedr(&[
        "route", "verdict", &id, "accepted", "--task", "t-1", "--note", "clean",
    ]);
    assert_eq!(
        ok.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert_eq!(
        s.cedr(&["route", "verdict", "p-000000000000", "accepted"])
            .status
            .code(),
        Some(2)
    );
    let skipped = s.pick("review.code", &["--no-record"]);
    assert_eq!(skipped.status.code(), Some(0));
    let stats: Value =
        serde_json::from_slice(&s.cedr(&["route", "stats", "--format", "json"]).stdout).unwrap();
    assert_eq!(
        stats,
        json!([{"route": "review.code", "target": "codex/gpt-6.1-sol", "picks": 1, "accepted": 1, "reworked": 0,
                "redone": 0, "rejected": 0, "verdicts": 1, "accepted_pct": 100.0, "provisional": true}])
    );
}

#[test]
fn the_nearest_repo_config_is_found_from_the_working_directory() {
    let s = Scratch::new(10.0, 20.0);
    let repo = s.dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".cedr")).unwrap();
    std::fs::create_dir_all(repo.join("src/deep")).unwrap();
    std::fs::copy(config(), repo.join(".cedr/routes.toml")).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_cedr"))
        .args(["route", "list"])
        .current_dir(repo.join("src/deep"))
        .env("XDG_CONFIG_HOME", s.dir.path().join("none"))
        .env_remove("CEDR_ROUTES")
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        stdout(&o).contains("review.code      on  codex/sol = claude/sonnet"),
        "{}",
        stdout(&o)
    );
}

#[test]
fn malformed_readings_are_an_error_not_empty_readings() {
    let s = Scratch::new(10.0, 20.0);
    std::fs::write(s.dir.path().join("readings.json"), "[]").unwrap();
    let o = s.pick("review.code", &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("readings must be a JSON object"));
}

#[test]
fn picks_carry_warnings_for_the_orchestrator() {
    let s = Scratch::new(10.0, 20.0);
    let stale = s.dir.path().join("routes.toml");
    let text = std::fs::read_to_string(config()).unwrap();
    std::fs::write(
        &stale,
        text.replacen(
            "version = 1\n",
            "version = 1\n[evaluation]\nlast = \"2020-01-01\"\n",
            1,
        ),
    )
    .unwrap();
    let readings = s.readings();
    let o = s.cedr(&[
        "route",
        "pick",
        "review.code",
        "--format",
        "json",
        "--config",
        &stale.display().to_string(),
        "--readings",
        &readings,
    ]);
    let pick: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(
        pick["warnings"][0]
            .as_str()
            .unwrap()
            .contains("route evaluation is"),
        "{pick}"
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("route evaluation is"));
}

#[test]
fn mcp_serves_picks_and_returns_errors_as_tool_errors() {
    let s = Scratch::new(10.0, 20.0);
    let mut child = Command::new(env!("CARGO_BIN_EXE_cedr"))
        .args(["mcp", "--config", &config().display().to_string()])
        .env("XDG_STATE_HOME", s.dir.path().join("state"))
        .env("XDG_CACHE_HOME", s.dir.path().join("cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut send = |v: Value| writeln!(stdin, "{v}").unwrap();
    let mut recv = |id: i64| loop {
        let v: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        if v["id"] == id {
            break v;
        }
    };
    send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}),
    );
    assert_eq!(recv(1)["result"]["serverInfo"]["name"], "cedr");
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let tools: Vec<String> = recv(2)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for t in [
        "route_pick",
        "route_manual",
        "route_verdict",
        "route_pools",
        "route_list",
    ] {
        assert!(tools.iter().any(|x| x == t), "{tools:?}");
    }
    // The server reads its configured sources; this route needs none to keep the work.
    send(
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "route_pick", "arguments": {"route": "design"}}}),
    );
    let kept = recv(3);
    let body: Value =
        serde_json::from_str(kept["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["reason"], "orchestrator-only route");
    send(
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "route_pick", "arguments": {"route": "nope"}}}),
    );
    let err = recv(4);
    assert_eq!(err["result"]["isError"], true);
    assert!(
        err["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown route")
    );
    drop(stdin);
    child.wait().unwrap();
}
