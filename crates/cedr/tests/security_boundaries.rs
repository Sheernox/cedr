//! Untrusted repository inputs must not choose executable machine settings or file ownership.
mod common;
use cedr_core::{lock, verify};
use common::Scratch;
use serde_json::json;
use std::{fs, path::Path};

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn pin(root: &Path) -> lock::LockOutcome {
    lock::lock(
        root,
        lock::LockOptions {
            offline: true,
            introspect: false,
            remote: false,
        },
    )
    .unwrap()
}

#[test]
fn guard_locks_redact_urls_without_losing_launch_drift() {
    let s = Scratch::new(0.0, 0.0);
    let root = s.dir.path();
    let url = "https://example.invalid/mcp?token=SYNTHETIC_SECRET#PRIVATE_FRAGMENT";
    write(
        root,
        ".mcp.json",
        &json!({"mcpServers":{"test":{"url":url}}}).to_string(),
    );
    let mut pinned = pin(root).lock;
    let text = fs::read_to_string(root.join(".cedr/guard.lock")).unwrap();
    assert!(!text.contains("SYNTHETIC_SECRET") && !text.contains("PRIVATE_FRAGMENT"));
    assert!(verify::verify(root).unwrap().report.is_empty());
    let remote = cedr_core::remote::check(root, "2026-01-01T00:00:00Z".into()).unwrap();
    assert!(
        serde_json::to_string(&remote)
            .unwrap()
            .contains("not_pinned")
    );
    pinned.mcp[0].source = format!("url:{url}");
    write(root, ".cedr/guard.lock", &pinned.to_toml());
    let legacy = s
        .command(&[
            "guard",
            "verify",
            root.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(legacy.status.success());
    assert!(String::from_utf8_lossy(&legacy.stderr).contains("rotate"));
    assert!(!String::from_utf8_lossy(&legacy.stderr).contains("SYNTHETIC_SECRET"));
    write(
        root,
        ".mcp.json",
        &json!({"mcpServers":{"test":{"url":url.replace("SYNTHETIC_SECRET", "ROTATED_SECRET")}}})
            .to_string(),
    );
    let report = verify::verify(root).unwrap().report.to_text();
    assert!(report.contains("CV005"));
    assert!(!report.contains("CV004") && !report.contains("SECRET"));
}

#[cfg(unix)]
#[test]
fn guard_detects_file_and_symlink_substitutions_in_both_directions() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        ".agents/skills/test/SKILL.md",
        "---\nname: test\ndescription: fixture\n---\nTest.\n",
    );
    let script = dir.path().join(".agents/skills/test/script.sh");
    fs::write(&script, "payload.sh").unwrap();
    pin(dir.path());
    fs::remove_file(&script).unwrap();
    symlink("payload.sh", &script).unwrap();
    assert!(
        verify::verify(dir.path())
            .unwrap()
            .report
            .to_text()
            .contains("CV001")
    );
    pin(dir.path());
    assert!(verify::verify(dir.path()).unwrap().report.is_empty());
    fs::remove_file(&script).unwrap();
    fs::write(&script, "payload.sh").unwrap();
    assert!(
        verify::verify(dir.path())
            .unwrap()
            .report
            .to_text()
            .contains("CV001")
    );
}

#[test]
fn sync_refuses_forged_ownership_before_mutating_any_file() {
    for (path, target, keys, version) in [
        ("source.txt", "agents", vec![], 1),
        (".git/hooks/pre-commit", "agents", vec![], 1),
        ("package.json", "claude", vec!["scripts"], 1),
        (".mcp.json", "claude", vec![], 1),
        (".claude/settings.json", "claude", vec!["unrelated"], 1),
        (".mcp.json", "codex", vec!["mcpServers"], 1),
        ("AGENTS.md", "agents", vec![], 99),
    ] {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            ".cedr/sync.toml",
            "version=1\ntargets=['agents']\n",
        );
        let contents = "{\"scripts\":{},\"unrelated\":true}";
        write(dir.path(), path, contents);
        write(dir.path(), ".cedr/sync.lock", &json!({"version":version,"files":{path:{"target":target,"hash":cedr_sync::hash(contents.as_bytes()),"keys":keys}}}).to_string());
        assert!(cedr_sync::plan(dir.path()).is_err(), "{path}");
        assert!(cedr_sync::check(dir.path()).to_text().contains("CS004"));
        assert_eq!(fs::read_to_string(dir.path().join(path)).unwrap(), contents);
    }
}

#[test]
fn sync_cannot_claim_handwritten_instructions_with_a_matching_hash() {
    for targets in ["['agents']", "['agents','claude']"] {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            ".cedr/sync.toml",
            &format!("version=1\ntargets={targets}\n"),
        );
        let text = "Handwritten instructions.\n";
        write(dir.path(), "CLAUDE.md", text);
        write(dir.path(), ".cedr/sync.lock", &json!({"version":1,"files":{"CLAUDE.md":{"target":"claude","hash":cedr_sync::hash(text.as_bytes()),"keys":[]}}}).to_string());
        let plan = cedr_sync::plan(dir.path()).unwrap();
        assert!(plan.report.to_text().contains("CS002"));
        assert!(!cedr_sync::apply(dir.path(), &plan, false).unwrap());
        assert_eq!(
            fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap(),
            text
        );
    }
}

#[cfg(unix)]
#[test]
fn discovered_repo_routes_cannot_run_readings_or_supply_credential_destinations() {
    let s = Scratch::new(0.0, 0.0);
    let root = s.dir.path().join("repo");
    let marker = s.dir.path().join("executed");
    let command = format!("printf '{{}}'; touch {}", marker.display());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = format!(
        "version=1\n[[readings]]\nkind='command'\ncommand={}\n[sync]\norg='audit'\nurl='http://{}'\n[routes.test]\ncandidates=[{{target='self'}}]\n",
        json!(["sh", "-c", command]),
        listener.local_addr().unwrap()
    );
    write(&root, ".cedr/routes.toml", &config);
    for args in [
        vec!["route", "models"],
        vec!["route", "pick", "test", "--format", "json"],
        vec!["route", "sync"],
        vec!["sync", "pull"],
        vec!["guard", "push"],
        vec!["doctor", "--format", "json"],
    ] {
        let output = s
            .command(&args)
            .current_dir(&root)
            .env("CEDR_API_KEY", "SYNTHETIC_KEY")
            .output()
            .unwrap();
        assert!(!marker.exists(), "{args:?}");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "{args:?}"
        );
        if args[0] == "route" && args[1] == "models" {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains("ignored repository"));
        }
        if args[0] == "doctor" {
            let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(doc.to_string().contains("ignored repository"));
        }
        if args[0] == "route" && args[1] == "pick" {
            assert!(String::from_utf8_lossy(&output.stdout).contains("ignored repository"));
        }
    }
    // Explicit choices retain executable configuration without needing a persistent trust store.
    let file = root.join(".cedr/routes.toml");
    for by_env in [false, true] {
        let mut cmd = s.command(&["route", "models"]);
        cmd.current_dir(&root);
        if by_env {
            cmd.env("CEDR_ROUTES", &file);
        } else {
            cmd.args(["--config", file.to_str().unwrap()]);
        }
        assert!(cmd.output().unwrap().status.success());
        assert!(marker.exists());
        fs::remove_file(&marker).unwrap();
        fs::remove_dir_all(s.dir.path().join("cache")).ok();
    }
    // Relative user-file readings resolve at the user file, never inside the checkout.
    write(&root, ".cedr/routes.toml", "version=1\n[harnesses.demo]\n");
    write(
        s.dir.path(),
        "config/cedr/routes.toml",
        "version=1\n[harnesses.demo]\n[harnesses.codex]\n[[readings]]\nkind='file'\npath='feed.json'\n[[readings]]\nkind='codex'\nharness='codex'\n",
    );
    write(
        s.dir.path(),
        "config/cedr/feed.json",
        "{\"models\":{\"demo\":[\"trusted-model\"]}}",
    );
    let output = s
        .command(&["route", "models", "--format", "json"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("trusted-model"));
}

#[test]
fn hostile_skill_nesting_is_a_finding_instead_of_a_process_abort() {
    let s = Scratch::new(0.0, 0.0);
    write(
        s.dir.path(),
        ".agents/skills/deep/SKILL.md",
        &format!("---\n{}0\n---\n", "- ".repeat(500_000)),
    );
    let output = s
        .command(&[
            "guard",
            "lint",
            s.dir.path().to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let findings = String::from_utf8_lossy(&output.stdout);
    assert!(
        findings.contains("CG004") && findings.contains("nesting exceeds 128"),
        "{findings}"
    );
    write(
        s.dir.path(),
        ".agents/skills/deep/SKILL.md",
        &format!(
            "---\na: &a {}0{}\nb: &b [*a]\n---\n",
            "[".repeat(127),
            "]".repeat(127)
        ),
    );
    let output = s
        .command(&[
            "guard",
            "lint",
            s.dir.path().to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("nesting exceeds 128"));
}
