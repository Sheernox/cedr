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
    assert!(String::from_utf8_lossy(&out.stderr).contains("Doctor:"));
}
