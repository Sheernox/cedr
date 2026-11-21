//! Exercise onboarding with isolated local state and no vendor CLI calls.
mod common;

use common::{Scratch, stdout};
use serde_json::Value;

#[test]
fn route_init_writes_valid_starters_for_one_two_and_four_harnesses() {
    for harnesses in [
        &["claude"][..],
        &["codex", "claude"][..],
        &["muse", "grok", "codex", "claude"][..],
    ] {
        let s = Scratch::new(0.0, 0.0);
        let mut args = vec!["route", "init", "--format", "json"];
        for h in harnesses {
            args.extend(["--harness", h]);
        }
        let out = s.cedr(&args);
        assert!(out.status.success(), "{:?}", out);
        let info: Value = serde_json::from_str(&stdout(&out)).unwrap();
        let path = s.dir.path().join("config/cedr/routes.toml");
        assert_eq!(info["written"], true);
        assert_eq!(info["path"], path.to_str().unwrap());
        assert_eq!(
            info["orchestrator"],
            serde_json::json!({"harness": "claude", "model": "opus"})
        );
        let config = cedr_route::config::load_local(&path).unwrap();
        assert_eq!(config.harnesses.len(), harnesses.len());
        assert_eq!(
            config.routes["review.code"].candidates.len(),
            harnesses.len(),
        );
        let tiers = cedr_route::pick::tiers(&config.routes["code.impl"].candidates);
        assert_eq!(tiers.len(), 3);
        assert_eq!(tiers[0].len(), harnesses.len());
        assert_eq!(tiers[1].len(), harnesses.len());
        assert!(tiers[0].iter().all(|c| c.sizes.is_some()));
        assert!(tiers[1].iter().all(|c| c.sizes.is_none()));
        assert_eq!(tiers[2][0].target, "self");
        assert_eq!(
            config.routes.contains_key("review.council"),
            harnesses.len() > 1
        );
        for h in harnesses {
            assert_eq!(config.harnesses[*h].command.as_deref(), Some(*h));
            assert!(config.harnesses[*h].reader.is_some());
        }
    }
}

#[test]
fn route_init_previews_a_custom_destination_without_creating_it() {
    let s = Scratch::new(0.0, 0.0);
    let path = s.dir.path().join("custom/routes.toml");
    let out = s.cedr(&[
        "route",
        "init",
        "--config",
        path.to_str().unwrap(),
        "--harness",
        "antigravity",
        "--orchestrator",
        "antigravity/flash",
        "--dry-run",
    ]);
    assert!(out.status.success(), "{:?}", out);
    let cfg = cedr_route::config::Config::parse(&stdout(&out), &path).unwrap();
    assert_eq!(cfg.orchestrator.unwrap().model, "flash");
    assert!(cfg.harnesses["antigravity"].reader.is_none());
    assert!(!path.parent().unwrap().exists());
}

#[test]
fn route_init_refuses_to_replace_a_file() {
    let s = Scratch::new(0.0, 0.0);
    let path = s.dir.path().join("routes.toml");
    std::fs::write(&path, "keep this").unwrap();
    let out = s.cedr(&["route", "init", "--config", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(path.to_str().unwrap()) && err.contains("cedr route add"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep this");
}

#[test]
fn route_init_detects_the_managed_claude_instance_without_a_cli() {
    use sha2::{Digest, Sha256};

    let s = Scratch::new(0.0, 0.0);
    let profile: String = Sha256::digest(b"claudeAgent")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    std::fs::create_dir_all(
        s.dir
            .path()
            .join("t3/userdata/providers/claudeAgent")
            .join(profile),
    )
    .unwrap();
    let out = s
        .command(&["route", "init", "--format", "json"])
        .env("PATH", s.dir.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let cfg =
        cedr_route::config::load_local(&s.dir.path().join("config/cedr/routes.toml")).unwrap();
    assert_eq!(cfg.harnesses["claude"].t3.as_deref(), Some("claudeAgent"));
}

#[cfg(unix)]
fn fake_agent(s: &Scratch, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = s.dir.path().join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
fn run_init(s: &Scratch, args: &[&str]) -> std::process::Output {
    let mut command = s.command(&["init", "--format", "json"]);
    command
        .args(args)
        .current_dir(s.dir.path())
        .env("PATH", s.dir.path())
        .env("HOME", s.dir.path())
        .env("XDG_DATA_HOME", s.dir.path().join("data"))
        .output()
        .unwrap()
}

#[cfg(unix)]
fn step<'a>(payload: &'a Value, id: &str) -> &'a Value {
    payload["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap()
}

#[cfg(unix)]
#[test]
fn init_without_a_terminal_previews_detected_agents_without_writing() {
    let s = Scratch::new(0.0, 0.0);
    fake_agent(
        &s,
        "claude",
        r#"if [ "$2" = list ]; then echo 'cedr-tools: running'; else echo unexpected > "$HOME/add"; fi"#,
    );
    fake_agent(
        &s,
        "opencode",
        r#"test "$*" = --version || exit 1
echo 1.17.10"#,
    );
    let out = run_init(&s, &[]);
    assert!(out.status.success(), "{:?}", out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("No terminal and no --yes"));
    let payload: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(step(&payload, "route.init")["status"], "planned");
    assert_eq!(step(&payload, "mcp.claude")["status"], "planned");
    assert_eq!(
        step(&payload, "mcp.claude")["command"],
        serde_json::json!(["claude", "mcp", "add", "cedr", "--", "cedr", "mcp"])
    );
    let opencode = step(&payload, "mcp.opencode");
    assert_eq!(opencode["status"], "skipped");
    assert!(opencode["command"].is_null());
    assert!(
        opencode["description"]
            .as_str()
            .unwrap()
            .contains(r#""command":["cedr","mcp"]"#)
    );
    assert!(!s.dir.path().join("add").exists());
    assert!(!s.dir.path().join("config").exists());
    assert!(!s.dir.path().join("state").exists());
}

#[cfg(unix)]
#[test]
fn init_preserves_existing_configuration_and_continues_on_registration_failure() {
    let s = Scratch::new(0.0, 0.0);
    let root = s.dir.path();
    std::fs::create_dir(root.join(".cedr")).unwrap();
    let routes = root.join(".cedr/routes.toml");
    std::fs::write(&routes, "version = 1\n").unwrap();
    let schedule = cedr_monitor::schedule::files(std::env::consts::OS, "cedr", 60).unwrap();
    let timer = root.join(&schedule.last().unwrap().relative);
    std::fs::create_dir_all(timer.parent().unwrap()).unwrap();
    std::fs::write(&timer, "existing timer").unwrap();
    fake_agent(
        &s,
        "claude",
        r#"if [ "$2" = list ]; then echo 'cedr: cedr mcp'; else echo unexpected > "$HOME/claude-add"; fi"#,
    );
    fake_agent(
        &s,
        "codex",
        r#"if [ "$2" = list ]; then exit 1; else echo unexpected > "$HOME/codex-add"; fi"#,
    );
    fake_agent(
        &s,
        "grok",
        r#"
if [ "$2" = add ]; then printf '%s\n' "$*" > "$HOME/grok-add"
elif [ -f "$HOME/grok-add" ]; then echo 'cedr cedr mcp'
fi"#,
    );

    fake_agent(
        &s,
        "gemini",
        r#"test "$*" = 'mcp add --scope user cedr cedr mcp' || exit 1
printf '%s\n' "$*" > "$HOME/gemini-add""#,
    );
    fake_agent(&s, "cursor", r#"echo unexpected > "$HOME/cursor-cli""#);
    fake_agent(
        &s,
        "opencode",
        r#"case "$*" in
  --version) echo 2.0.0 ;;
  'mcp add cedr --global -- cedr mcp') printf '%s\n' "$*" > "$HOME/opencode-add" ;;
  *) exit 1 ;;
esac"#,
    );

    let out = run_init(&s, &["--yes"]);
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    let payload: Value = serde_json::from_str(&stdout(&out)).unwrap();
    for id in ["route.init", "mcp.claude", "monitor.schedule"] {
        assert_eq!(step(&payload, id)["status"], "already", "{id}");
    }
    assert_eq!(step(&payload, "mcp.codex")["status"], "failed");
    assert!(step(&payload, "mcp.codex")["error"].is_string());
    assert_eq!(step(&payload, "mcp.grok")["status"], "applied");
    assert_eq!(
        std::fs::read_to_string(root.join("grok-add")).unwrap(),
        "mcp add cedr -- cedr mcp\n"
    );
    assert_eq!(std::fs::read_to_string(routes).unwrap(), "version = 1\n");
    assert_eq!(std::fs::read_to_string(timer).unwrap(), "existing timer");
    assert!(!root.join("claude-add").exists());
    assert!(!root.join("codex-add").exists());
    assert_eq!(step(&payload, "mcp.gemini")["status"], "applied");
    assert_eq!(
        std::fs::read_to_string(root.join("gemini-add")).unwrap(),
        "mcp add --scope user cedr cedr mcp\n"
    );
    assert_eq!(step(&payload, "mcp.opencode")["status"], "applied");
    assert_eq!(
        std::fs::read_to_string(root.join("opencode-add")).unwrap(),
        "mcp add cedr --global -- cedr mcp\n"
    );
    let cursor = step(&payload, "mcp.cursor");
    assert_eq!(cursor["status"], "skipped");
    assert!(cursor["command"].is_null());
    assert!(
        cursor["description"]
            .as_str()
            .unwrap()
            .contains("~/.cursor/mcp.json")
    );
    assert!(!root.join("cursor-cli").exists());
    assert!(!root.join(".cursor").exists());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Doctor:"));
}

fn run_status(s: &Scratch, root: &std::path::Path) -> std::process::Output {
    let home = s.dir.path();
    s.command(&["status", "--format", "json"])
        .current_dir(root)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("CODEX_HOME", home.join("codex"))
        .env("COPILOT_HOME", home.join("copilot"))
        .env("GROK_HOME", home.join("grok"))
        .env_remove("OPENCODE_DB")
        .env("CEDR_TELEMETRY", "1")
        .output()
        .unwrap()
}

#[test]
fn status_uses_null_for_absent_sources_and_writes_no_configuration() {
    let s = Scratch::new(0.0, 0.0);
    let out = run_status(&s, s.dir.path());
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&out)).unwrap(),
        serde_json::json!({"limits": null, "spend": null, "pendingPicks": null, "sync": null, "guard": null})
    );
    assert!(!s.dir.path().join("config").exists());
    assert!(!s.dir.path().join("cache").exists());
}

#[test]
fn status_collects_local_usage_and_finds_pending_picks_and_repo_state() {
    use chrono::{Duration, Utc};
    use serde_json::json;
    use std::fmt::Write;

    let s = Scratch::new(0.0, 0.0);
    let root = s.dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::create_dir(root.join(".cedr")).unwrap();
    std::fs::create_dir(root.join("nested")).unwrap();
    std::fs::write(
        root.join(".cedr/sync.toml"),
        "version = 1\ntargets = [\"agents\"]\n",
    )
    .unwrap();
    std::fs::write(root.join(".cedr/guard.lock"), "version = 1\n").unwrap();

    let now = Utc::now();
    std::fs::create_dir_all(root.join("claude/projects/demo")).unwrap();
    let response = |id: &str, at: chrono::DateTime<Utc>| {
        json!({
            "type": "assistant", "timestamp": at.to_rfc3339(), "sessionId": "session",
            "message": {
                "id": id, "model": "claude-opus-5-5",
                "usage": {"input_tokens": 1_000_000, "output_tokens": 0}
            }
        })
    };
    std::fs::write(
        root.join("claude/projects/demo/log.jsonl"),
        format!(
            "{}\n{}\n",
            response("today", now),
            response("earlier", now - Duration::days(2))
        ),
    )
    .unwrap();

    let state = root.join("state/cedr");
    std::fs::create_dir_all(state.join("readings")).unwrap();
    std::fs::write(
        state.join("readings/claude.json"),
        json!({"harnesses":[{"harness":"claude","fetchedAt":now.to_rfc3339(),"windows":[
            {"id":"weekly","kind":"weekly","percent":82,"resetsAt":(now+Duration::hours(2)).to_rfc3339()},
            {"id":"session","kind":"session","percent":10,"resetsAt":(now+Duration::hours(1)).to_rfc3339()},
            {"id":"expired","kind":"session","percent":99,"resetsAt":(now-Duration::hours(1)).to_rfc3339()}
        ]}]}).to_string(),
    ).unwrap();

    let mut history = String::new();
    for n in 0..7 {
        writeln!(
            history,
            "{}",
            json!({
                "type":"pick", "id":format!("p-{n}"),"at":(now+Duration::seconds(n)).to_rfc3339(),
                "route":"review.code","rules":"test","target":"codex/gpt-6-sol",
                "effort":"high","reason":"test","state":"ready","size":"medium",
            })
        )
        .unwrap();
    }
    writeln!(
        history,
        "{}",
        json!({"type":"verdict","id":"p-6","at":now.to_rfc3339(),"verdict":"accepted"})
    )
    .unwrap();
    std::fs::write(state.join("route-history.jsonl"), history).unwrap();

    let out = run_status(&s, &root.join("nested"));
    assert!(out.status.success(), "{:?}", out);
    let value: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(value["limits"].as_array().unwrap().len(), 1);
    assert_eq!(value["limits"][0]["window"], "weekly");
    assert_eq!(value["limits"][0]["percent"], 82);
    assert_eq!(value["spend"]["todayUsd"], 4.0);
    assert_eq!(value["spend"]["last7DaysUsd"], 8.0);
    let picks = value["pendingPicks"].as_array().unwrap();
    assert_eq!(picks.len(), 5);
    assert_eq!(picks[0]["id"], "p-5");
    assert_eq!(picks[4]["id"], "p-1");
    assert_eq!(value["sync"], json!({"state":"drift","findings":1}));
    assert_eq!(
        value["guard"],
        json!({"state":"clean","findings":0,"error":null})
    );
}
