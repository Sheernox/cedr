//! Monitor subprocesses use isolated synthetic homes and never need network access.
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
