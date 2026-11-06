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
    let stats: Value = serde_json::from_slice(
        &s.cedr(&[
            "route",
            "stats",
            "--config",
            &config().display().to_string(),
            "--format",
            "json",
        ])
        .stdout,
    )
    .unwrap();
    assert_eq!(
        stats,
        json!([{"route": "review.code", "target": "codex/gpt-6.1-sol", "picks": 1, "accepted": 1, "reworked": 0,
                "redone": 0, "rejected": 0, "verdicts": 1, "accepted_pct": 100.0, "provisional": true, "preview": false, "median_minutes": 0.0, "judged": null, "escalated": null}])
    );
    let text = stdout(&s.cedr(&[
        "route",
        "stats",
        "--config",
        &config().display().to_string(),
    ]));
    let (header, row) = text.split_once('\n').unwrap();
    // Every value after the target ends under the end of its column's name.
    let ends = |line: &str| -> Vec<usize> {
        let mut at = 0;
        line.split_whitespace()
            .map(|w| {
                at = line[at..].find(w).unwrap() + at + w.len();
                at
            })
            .collect()
    };
    let (header, row) = (ends(header), ends(row));
    assert_eq!(header.len(), 11, "{text}");
    assert_eq!(header[2..], row[2..11], "{text}");
    assert!(
        row.len() == 12 && text.ends_with(" (provisional)\n"),
        "{text}"
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
        "route_show",
        "route_models",
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
    // Exercise the new MCP paths against one recorded council run.
    send(
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"council_plan","arguments":{"route":"council","members":["codex/sol","claude/opus"],"models":{"codex":["gpt-6.1-sol"],"claude":["claude-opus-5-5"]}}}}),
    );
    let result = recv(5);
    let plan: Value =
        serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let run = plan["run"].as_str().unwrap();
    send(
        json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"council_merge","arguments":{"route":"council","run":run,"previous":{"gate":"block","block":"P1","groups":[]},"reports":[{"member":"a","score":8,"spare_for":"c","findings":[]},{"member":"b","findings":[]}]}}}),
    );
    let result = recv(6);
    let merge: Value =
        serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(merge["gate"], "pass");
    assert_eq!(merge["scores"]["a"], 8.0);
    assert_eq!(merge["spares"]["a"], "c");
    assert_eq!(merge["stalled"], false);
    assert!(merge["id"].as_str().unwrap().starts_with("c-"));
    send(
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"council_stats","arguments":{"route":"council"}}}),
    );
    let result = recv(7);
    let stats: Value =
        serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(stats[0]["pass"], 1);
    send(
        json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"route_history","arguments":{"limit":1}}}),
    );
    let result = recv(8);
    let h: Value =
        serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(h[0]["id"], merge["id"]);
    send(
        json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"route_stats","arguments":{"route":"council"}}}),
    );
    let result = recv(9);
    let stats: Value =
        serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(stats.as_array().unwrap().len(), 2);
    assert!(stats.as_array().unwrap().iter().all(|r| r["picks"] == 1));
    send(
        json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"route_pick","arguments":{"route":"review.code","judges":"missing","escalates":"missing"}}}),
    );
    assert_eq!(recv(10)["result"]["isError"], true);
    drop(stdin);
    child.wait().unwrap();
}

#[test]
fn a_cascade_names_its_judge_in_the_pick_and_the_list() {
    let s = Scratch::new(10.0, 20.0);
    let out = stdout(&s.pick("impl", &["--format", "t3"]));
    assert!(
        out.ends_with("  cascade: judge review.code, escalate self\n"),
        "{out}"
    );
    let cfg = config().display().to_string();
    let list = stdout(&s.cedr(&["route", "list", "--config", &cfg]));
    assert!(
        list.contains("codex/sol > self | judge review.code -> self\n"),
        "{list}"
    );
}

#[test]
fn cascade_links_validate_draft_and_destination_and_inherit_run() {
    let s = Scratch::new(10.0, 20.0);
    let cfg = s.dir.path().join("routes.toml");
    let source = std::fs::read_to_string(config()).unwrap().replace(
        "judge = \"review.code\"",
        "judge = \"review.code\"\nescalate = \"hard\"",
    );
    std::fs::write(
        &cfg,
        format!("{source}\n[routes.hard]\ncandidates = [{{target = \"codex/sol\"}}]\n"),
    )
    .unwrap();
    let cfg = cfg.display().to_string();
    let readings = s.readings();
    let pick = |route: &str, extra: &[&str]| {
        let mut args = vec![
            "route",
            "pick",
            route,
            "--config",
            &cfg,
            "--readings",
            &readings,
            "--format",
            "json",
        ];
        args.extend_from_slice(extra);
        s.cedr(&args)
    };
    let draft: Value = serde_json::from_slice(&pick("impl", &[]).stdout).unwrap();
    let id = draft["delegates"][0]["id"].as_str().unwrap();
    let run = draft["run"].as_str().unwrap();
    assert_eq!(run.len(), 14);
    assert!(run.starts_with("r-"));
    assert!(
        run[2..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    for (route, flag) in [("review.code", "--judges"), ("hard", "--escalates")] {
        let o = pick(route, &[flag, id]);
        assert_eq!(
            o.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        let linked: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(linked["run"], run);
        assert_eq!(pick(route, &[flag, "p-unknown"]).status.code(), Some(2));
        assert_eq!(pick("impl", &[flag, id]).status.code(), Some(2));
    }
    assert_eq!(
        pick("review.code", &["--judges", id, "--escalates", id])
            .status
            .code(),
        Some(2)
    );
    let ordinary: Value = serde_json::from_slice(&pick("review.code", &[]).stdout).unwrap();
    assert!(ordinary["run"].is_null());
    let ordinary_id = ordinary["delegates"][0]["id"].as_str().unwrap();
    for flag in ["--judges", "--escalates"] {
        assert_eq!(
            pick("review.code", &[flag, ordinary_id]).status.code(),
            Some(2)
        );
    }
    // A real cascade without an escalation route cannot be used as an escalation link.
    let self_draft: Value =
        serde_json::from_slice(&s.pick("impl", &["--format", "json"]).stdout).unwrap();
    assert_eq!(
        s.pick(
            "review.code",
            &[
                "--escalates",
                self_draft["delegates"][0]["id"].as_str().unwrap()
            ]
        )
        .status
        .code(),
        Some(2)
    );
    let rows: Value = serde_json::from_slice(
        &s.cedr(&[
            "route",
            "history",
            "--route",
            "review.code",
            "--limit",
            "1",
            "--format",
            "json",
        ])
        .stdout,
    )
    .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    let stats: Value = serde_json::from_slice(
        &s.cedr(&[
            "route", "stats", "--config", &cfg, "--route", "impl", "--format", "json",
        ])
        .stdout,
    )
    .unwrap();
    assert_eq!(stats[0]["judged"], 1);
    assert_eq!(stats[0]["escalated"], 1);
    assert_eq!(
        s.cedr(&["route", "stats", "--config", &cfg, "--since", "yesterday"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        s.cedr(&["route", "history", "--limit", "501"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn import_emits_valid_config_and_lists_converted_tiers() {
    let s = Scratch::new(10.0, 20.0);
    let source = config().parent().unwrap().join("import.toml");
    let o = s.cedr(&["route", "import", &source.display().to_string()]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let output = s.dir.path().join("routes.toml");
    std::fs::write(&output, &o.stdout).unwrap();
    let list = s.cedr(&["route", "list", "--config", &output.display().to_string()]);
    assert_eq!(list.status.code(), Some(0));
    assert!(stdout(&list).contains("off claude/sonnet > self"));
    std::fs::write(&output, "unsupported = true").unwrap();
    let bad = s.cedr(&["route", "import", &output.display().to_string()]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unsupported"));
}

/// A scratch copy of the fixture routes, since these commands write to the file.
fn editable(s: &Scratch) -> String {
    let path = s.dir.path().join("routes.toml");
    std::fs::copy(config(), &path).unwrap();
    path.display().to_string()
}

#[test]
fn a_threshold_edit_changes_the_next_pick_and_is_reversible() {
    let s = Scratch::new(40.0, 20.0);
    let cfg = editable(&s);
    let readings = s.readings();
    let pick = || {
        s.cedr(&[
            "route",
            "pick",
            "review.code",
            "--config",
            &cfg,
            "--readings",
            &readings,
            "--no-record",
            "--format",
            "json",
        ])
    };
    let target = |o: &std::process::Output| {
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        v["delegates"][0]["target"].as_str().map(str::to_string)
    };
    assert_eq!(target(&pick()).as_deref(), Some("claude/claude-sonnet-5-5"));

    let set = s.cedr(&[
        "route",
        "thresholds",
        "--harness",
        "claude",
        "--set",
        "weekly_conserve=10",
        "--config",
        &cfg,
    ]);
    assert_eq!(
        set.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert!(
        stdout(&set).contains("thresholds changed (rules "),
        "{}",
        stdout(&set)
    );
    assert_eq!(target(&pick()).as_deref(), Some("codex/gpt-6.1-sol"));

    let shown = s.cedr(&[
        "route",
        "thresholds",
        "--harness",
        "claude",
        "--config",
        &cfg,
        "--format",
        "json",
    ]);
    let rows: Value = serde_json::from_slice(&shown.stdout).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "weekly_conserve")
        .unwrap();
    assert_eq!(
        (row["value"].as_f64(), row["from"].as_str()),
        (Some(10.0), Some("harness"))
    );

    s.cedr(&[
        "route",
        "thresholds",
        "--harness",
        "claude",
        "--unset",
        "weekly_conserve",
        "--config",
        &cfg,
    ]);
    assert_eq!(target(&pick()).as_deref(), Some("claude/claude-sonnet-5-5"));

    let refused = s.cedr(&["route", "thresholds", "--set", "out=101", "--config", &cfg]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("between 0 and 100"));
}

#[test]
fn show_reports_each_size_and_candidate_edits_and_disabling_reach_it() {
    let s = Scratch::new(40.0, 20.0);
    let cfg = editable(&s);
    let readings = s.readings();
    let show = || {
        let o = s.cedr(&[
            "route",
            "show",
            "impl",
            "--config",
            &cfg,
            "--readings",
            &readings,
            "--format",
            "json",
        ]);
        assert_eq!(
            o.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice::<Value>(&o.stdout).unwrap()
    };
    let v = show();
    assert_eq!(v["picks"].as_array().unwrap().len(), 3);
    assert_eq!(v["picks"][2]["targets"][0], "codex/gpt-6.1-sol");
    assert_eq!(v["candidates"][0]["model"], "gpt-6.1-sol");
    assert_eq!(
        v["candidates"][0]["takes"],
        json!(["small", "medium", "large"])
    );
    assert_eq!(v["candidates"][1]["target"], "self");

    let edit = s.cedr(&[
        "route",
        "add",
        "impl",
        "claude/sonnet",
        "--sizes",
        "large",
        "--at",
        "1",
        "--config",
        &cfg,
    ]);
    assert_eq!(
        edit.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&edit.stderr)
    );
    let v = show();
    assert_eq!(
        v["picks"][0]["targets"][0], "codex/gpt-6.1-sol",
        "large only: small work skips it"
    );
    assert_eq!(v["picks"][0]["reason"], "primary");
    assert_eq!(v["picks"][2]["targets"][0], "claude/claude-sonnet-5-5");

    s.cedr(&["route", "remove", "impl", "claude/sonnet", "--config", &cfg]);
    assert_eq!(show()["candidates"].as_array().unwrap().len(), 2);

    assert_eq!(
        s.cedr(&["route", "disable", "impl", "--config", &cfg])
            .status
            .code(),
        Some(0)
    );
    let v = show();
    assert_eq!(
        (v["enabled"].as_bool(), v["picks"][1]["reason"].as_str()),
        (Some(false), Some("disabled"))
    );
    s.cedr(&["route", "enable", "impl", "--config", &cfg]);
    assert_eq!(show()["enabled"], true);
    assert_eq!(
        s.cedr(&["route", "disable", "nope", "--config", &cfg])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn models_lists_what_each_harness_offers() {
    let s = Scratch::new(40.0, 20.0);
    let cfg = config().display().to_string();
    let readings = s.readings();
    let o = s.cedr(&[
        "route",
        "models",
        "--config",
        &cfg,
        "--readings",
        &readings,
        "--format",
        "json",
    ]);
    let rows: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(
        rows.as_array()
            .unwrap()
            .contains(&json!({"harness": "codex", "models": ["gpt-6.1-sol", "gpt-6-sol"]})),
        "{rows}"
    );
}

/// A routes file in the scratch directory, for tests that need their own readings sources.
fn routes_file(s: &Scratch, text: &str) -> String {
    let path = s.dir.path().join("own-routes.toml");
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

#[test]
fn claude_reads_claude_codes_usage_cache_without_a_status_line() {
    let s = Scratch::new(10.0, 20.0);
    let cfg = routes_file(
        &s,
        "version = 1\n[harnesses.claude]\nexpect = [\"five_hour\", \"seven_day\"]\nmodels = [\"claude-opus-5-5\", \"claude-fable-5-1\"]\n[routes.r]\ncandidates = [{ target = \"claude/fable\" }]\n[[readings]]\nkind = \"claude-statusline\"\n",
    );
    // A broken status line capture must not hide a healthy cache.
    let captures = s.dir.path().join("state/cedr/readings");
    std::fs::create_dir_all(&captures).unwrap();
    std::fs::write(captures.join("claude.json"), "{cut").unwrap();
    let now = chrono::Utc::now();
    let later = (now + chrono::Duration::hours(3)).to_rfc3339();
    let claude = s.dir.path().join("claude");
    std::fs::create_dir_all(&claude).unwrap();
    let cache = json!({"oauthAccount": {"accountUuid": "acct-a"}, "cachedUsageUtilization": {
        "accountUuid": "acct-a", "fetchedAtMs": now.timestamp_millis(), "utilization": {
        "five_hour": {"utilization": 39, "resets_at": later},
        "seven_day": {"utilization": 45, "resets_at": later},
        "limits": [{"kind": "weekly_scoped", "percent": 2, "resets_at": later,
                    "scope": {"model": {"display_name": "Fable"}}}]}}});
    std::fs::write(claude.join(".claude.json"), cache.to_string()).unwrap();
    let o = s.cedr(&["route", "pools", "--config", &cfg, "--format", "json"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let pools: Value = serde_json::from_slice(&o.stdout).unwrap();
    let rows: Vec<(&str, &str)> = pools
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["family"].as_str().unwrap(),
                r["summary"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("", "claude s39% w45%"),
            ("fable", "claude s39% w45% fable2%")
        ]
    );
}

#[test]
fn t3_code_readings_come_first_and_the_claude_reader_fills_in_when_they_are_stale() {
    let s = Scratch::new(10.0, 20.0);
    let cfg = routes_file(
        &s,
        "version = 1\n[harnesses.claude]\nexpect = [\"five_hour\", \"seven_day\"]\nt3 = \"claudeAgent\"\n[routes.r]\ncandidates = [{ target = \"claude/fable\" }]\n[[readings]]\nkind = \"t3\"\n[[readings]]\nkind = \"claude-statusline\"\n",
    );
    let now = chrono::Utc::now();
    let later = (now + chrono::Duration::hours(3)).to_rfc3339();
    let caches = s.dir.path().join("t3/caches");
    std::fs::create_dir_all(&caches).unwrap();
    let t3 = |checked: chrono::DateTime<chrono::Utc>| {
        json!({"driver": "claudeAgent", "enabled": true,
               "models": [{"slug": "claude-opus-5-5"}, {"slug": "claude-fable-5-1"}],
               "usageLimits": {"checkedAt": checked.to_rfc3339(), "windows": [
                   {"id": "five_hour", "kind": "session", "usedPercent": 8, "resetsAt": later},
                   {"id": "seven_day", "kind": "weekly", "usedPercent": 49, "resetsAt": later},
                   {"id": "seven_day_fable", "kind": "weekly", "usedPercent": 3, "resetsAt": later}]}})
        .to_string()
    };
    std::fs::write(caches.join("claudeAgent.json"), t3(now)).unwrap();
    // A broken capture shows whether the claude-statusline reader ran: it warns when it does.
    let captures = s.dir.path().join("state/cedr/readings");
    std::fs::create_dir_all(&captures).unwrap();
    std::fs::write(captures.join("claude.json"), "{cut").unwrap();
    let pools = |s: &Scratch| {
        let o = s.cedr(&["route", "pools", "--config", &cfg, "--format", "json"]);
        assert_eq!(
            o.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        let rows: Value = serde_json::from_slice(&o.stdout).unwrap();
        let rows: Vec<(String, String)> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["family"].as_str().unwrap().into(),
                    r["summary"].as_str().unwrap().into(),
                )
            })
            .collect();
        (rows, String::from_utf8_lossy(&o.stderr).into_owned())
    };
    let (rows, stderr) = pools(&s);
    assert_eq!(
        rows,
        [
            ("".to_string(), "claude s8% w49%".to_string()),
            ("fable".to_string(), "claude s8% w49% fable3%".to_string())
        ]
    );
    assert!(
        !stderr.contains("claude"),
        "the covered claude reader stays quiet: {stderr}"
    );
    // T3 Code stopped refreshing: the next source supplies the reading.
    std::fs::write(
        caches.join("claudeAgent.json"),
        t3(now - chrono::Duration::hours(1)),
    )
    .unwrap();
    let claude = s.dir.path().join("claude");
    std::fs::create_dir_all(&claude).unwrap();
    let cache = json!({"oauthAccount": {"accountUuid": "acct-a"}, "cachedUsageUtilization": {
        "accountUuid": "acct-a", "fetchedAtMs": now.timestamp_millis(), "utilization": {
        "five_hour": {"utilization": 39, "resets_at": later},
        "seven_day": {"utilization": 45, "resets_at": later}}}});
    std::fs::write(claude.join(".claude.json"), cache.to_string()).unwrap();
    let (rows, _) = pools(&s);
    assert_eq!(rows[0], ("".to_string(), "claude s39% w45%".to_string()));
}

#[test]
fn several_claude_harnesses_never_share_one_usage_cache() {
    let s = Scratch::new(10.0, 20.0);
    let cfg = routes_file(
        &s,
        "version = 1\n[harnesses.claude]\n[harnesses.work]\n[[readings]]\nkind = \"claude-statusline\"\n[[readings]]\nkind = \"claude-statusline\"\nharness = \"work\"\n",
    );
    let now = chrono::Utc::now();
    let claude = s.dir.path().join("claude");
    std::fs::create_dir_all(&claude).unwrap();
    let cache = json!({"oauthAccount": {"accountUuid": "acct-a"}, "cachedUsageUtilization": {
        "accountUuid": "acct-a", "fetchedAtMs": now.timestamp_millis(), "utilization": {
        "five_hour": {"utilization": 39, "resets_at": (now + chrono::Duration::hours(3)).to_rfc3339()}}}});
    std::fs::write(claude.join(".claude.json"), cache.to_string()).unwrap();
    let out = stdout(&s.cedr(&["route", "pools", "--config", &cfg]));
    assert!(
        out.contains("claude no reading") && out.contains("work no reading"),
        "{out}"
    );
}

#[test]
fn the_newer_of_the_status_line_capture_and_the_usage_cache_wins() {
    let s = Scratch::new(10.0, 20.0);
    let cfg = routes_file(
        &s,
        "version = 1\n[harnesses.claude]\nexpect = [\"five_hour\", \"seven_day\"]\n[routes.r]\ncandidates = [{ target = \"claude/fable\" }]\n[[readings]]\nkind = \"claude-statusline\"\n",
    );
    let now = chrono::Utc::now();
    let later = (now + chrono::Duration::hours(3)).to_rfc3339();
    let earlier = now - chrono::Duration::minutes(5);
    let captures = s.dir.path().join("state/cedr/readings");
    let claude = s.dir.path().join("claude");
    std::fs::create_dir_all(&captures).unwrap();
    std::fs::create_dir_all(&claude).unwrap();
    let write = |capture_at: chrono::DateTime<chrono::Utc>,
                 cache_at: chrono::DateTime<chrono::Utc>| {
        let capture = json!({"harnesses": [{"harness": "claude", "fetchedAt": capture_at.to_rfc3339(), "windows": [
            {"id": "five_hour", "kind": "session", "percent": 10, "resetsAt": later},
            {"id": "seven_day", "kind": "weekly", "percent": 11, "resetsAt": later}]}]});
        std::fs::write(captures.join("claude.json"), capture.to_string()).unwrap();
        let cache = json!({"oauthAccount": {"accountUuid": "acct-a"}, "cachedUsageUtilization": {
            "accountUuid": "acct-a", "fetchedAtMs": cache_at.timestamp_millis(), "utilization": {
            "five_hour": {"utilization": 39, "resets_at": later},
            "seven_day": {"utilization": 45, "resets_at": later},
            "limits": [{"kind": "weekly_scoped", "percent": 2, "resets_at": later,
                        "scope": {"model": {"display_name": "Fable"}}}]}}});
        std::fs::write(claude.join(".claude.json"), cache.to_string()).unwrap();
        stdout(&s.cedr(&["route", "pools", "--config", &cfg, "--format", "json"]))
    };
    // The capture wins its own windows and keeps the cache's model window.
    let out = write(now, earlier);
    assert!(out.contains(r#""summary": "claude s10% w11%""#), "{out}");
    assert!(
        out.contains(r#""summary": "claude s10% w11% fable2%""#),
        "{out}"
    );
    assert!(write(earlier, now).contains(r#""summary": "claude s39% w45%""#));
    // A stale cache's model window no longer counts as read.
    let out = write(now, now - chrono::Duration::minutes(20));
    assert!(
        out.contains(r#""summary": "claude s10% w11% fable?%""#),
        "{out}"
    );

    // A broken cache beside a capture keeps the capture and says the model windows are gone.
    write(now, now);
    std::fs::write(claude.join(".claude.json"), "{cut").unwrap();
    let o = s.cedr(&["route", "pools", "--config", &cfg]);
    assert!(stdout(&o).contains("claude s10% w11%"), "{}", stdout(&o));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("model windows are missing"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn a_built_in_reader_for_a_harness_an_earlier_source_read_stays_quiet() {
    let s = Scratch::new(10.0, 20.0);
    let at = chrono::Utc::now().to_rfc3339();
    std::fs::write(
        s.dir.path().join("muse.json"),
        json!({"harnesses": [{"harness": "muse", "fetchedAt": at, "windows": [
            {"id": "weekly", "kind": "weekly", "percent": 52}]}]})
        .to_string(),
    )
    .unwrap();
    let cfg = routes_file(
        &s,
        "version = 1\n[harnesses.muse]\nexpect = [\"weekly\"]\n[[readings]]\nkind = \"file\"\npath = \"muse.json\"\n[[readings]]\nkind = \"muse\"\n",
    );
    // No muse on PATH: the built-in reader would fail if it ran.
    let o = s
        .command(&["route", "pools", "--config", &cfg])
        .env("PATH", s.dir.path())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("muse w52%"), "{}", stdout(&o));
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
}

#[test]
fn a_machine_orchestrator_applies_over_shared_routes() {
    let s = Scratch::new(10.0, 20.0);
    let shared = std::fs::read_to_string(config()).unwrap().replace(
        "[orchestrator]\nharness = \"claude\"\nmodel = \"opus\"\n",
        "",
    );
    let org = s.dir.path().join("config/cedr/org-routes");
    std::fs::create_dir_all(&org).unwrap();
    std::fs::write(org.join("acme.toml"), shared).unwrap();
    let local = "version = 1\n[orchestrator]\nharness = \"claude\"\nmodel = \"opus\"\n[sync]\norg = \"acme\"\nroutes = \"org\"\nauto = false\n";
    let cfg = routes_file(&s, local);
    let readings = s.readings();
    let pick = |cfg: &str| {
        s.cedr(&[
            "route",
            "pick",
            "review.code",
            "--config",
            cfg,
            "--readings",
            &readings,
            "--format",
            "json",
        ])
    };
    let o = pick(&cfg);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["orchestrator"], "claude s5% w20%");

    let dropped = routes_file(
        &s,
        &local.replace("harness = \"claude\"", "harness = \"gone\""),
    );
    let o = pick(&dropped);
    assert_eq!(o.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v["orchestrator"].is_null());
    assert!(
        String::from_utf8_lossy(&o.stderr)
            .contains("orchestrator harness \"gone\" is not in the shared routes"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );

    // The machine's own beats one the shared routes set, which stays the fallback.
    let theirs = std::fs::read_to_string(config()).unwrap().replace(
        "[orchestrator]\nharness = \"claude\"",
        "[orchestrator]\nharness = \"codex\"",
    );
    std::fs::write(org.join("acme.toml"), theirs).unwrap();
    let v: Value = serde_json::from_slice(&pick(&routes_file(&s, local)).stdout).unwrap();
    assert_eq!(v["orchestrator"], "claude s5% w20%");
    let gone = local.replace("harness = \"claude\"", "harness = \"gone\"");
    let v: Value = serde_json::from_slice(&pick(&routes_file(&s, &gone)).stdout).unwrap();
    assert_eq!(v["orchestrator"], "codex w10%");
}

#[test]
fn a_repository_routes_file_takes_the_user_orchestrator_or_says_why_not() {
    let s = Scratch::new(10.0, 20.0);
    let repo = s.dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".cedr")).unwrap();
    let routing = std::fs::read_to_string(config()).unwrap().replace(
        "[orchestrator]\nharness = \"claude\"\nmodel = \"opus\"\n",
        "",
    );
    std::fs::write(repo.join(".cedr/routes.toml"), routing).unwrap();
    let user = s.dir.path().join("config/cedr");
    std::fs::create_dir_all(&user).unwrap();
    let readings = s.readings();
    let pick = |orchestrator: &str| {
        std::fs::write(
            user.join("routes.toml"),
            format!("version = 1\n[harnesses.{orchestrator}]\n[orchestrator]\nharness = \"{orchestrator}\"\nmodel = \"opus\"\n"),
        )
        .unwrap();
        s.command(&[
            "route",
            "pick",
            "review.code",
            "--readings",
            &readings,
            "--format",
            "json",
        ])
        .current_dir(&repo)
        .output()
        .unwrap()
    };
    let o = pick("claude");
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        v["orchestrator"],
        "claude s5% w20%",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = pick("gone");
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v["orchestrator"].is_null());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("orchestrator harness \"gone\" is not in"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn pools_list_a_scoped_limit_for_any_version_a_family_target_may_pick() {
    let s = Scratch::new(10.0, 20.0);
    let cfg = routes_file(
        &s,
        "version = 1\n[harnesses.claude]\nexpect = [\"five_hour\", \"seven_day\"]\nmodels = [\"claude-opus-5-5\", \"claude-opus-4-5\", \"claude-sonnet-5-5\"]\n[routes.r]\ncandidates = [{ target = \"claude/opus\" }]\n[[readings]]\nkind = \"claude-statusline\"\n",
    );
    let now = chrono::Utc::now();
    let later = (now + chrono::Duration::hours(3)).to_rfc3339();
    let claude = s.dir.path().join("claude");
    std::fs::create_dir_all(&claude).unwrap();
    let cache = json!({"oauthAccount": {"accountUuid": "acct-a"}, "cachedUsageUtilization": {
        "accountUuid": "acct-a", "fetchedAtMs": now.timestamp_millis(), "utilization": {
        "five_hour": {"utilization": 39, "resets_at": later},
        "seven_day": {"utilization": 45, "resets_at": later},
        "limits": [{"kind": "weekly_scoped", "percent": 100, "resets_at": later,
                    "scope": {"model": {"display_name": "Opus 4.5"}}}]}}});
    std::fs::write(claude.join(".claude.json"), cache.to_string()).unwrap();
    let o = s.cedr(&["route", "pools", "--config", &cfg, "--format", "json"]);
    let pools: Value = serde_json::from_slice(&o.stdout).unwrap();
    let opus = pools
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["family"] == "opus-4-5")
        .unwrap_or_else(|| panic!("{pools}"));
    assert_eq!(opus["state"], "out");
    assert_eq!(opus["summary"], "claude s39% w45% opus-4-5:100%");
}
