//! Guard coverage for every client and read-only reporting at the CLI boundary.
use cedr_core::{
    discovery::{self, Launch},
    lock, verify,
};
use serde_json::Value;
use std::{fs, path::Path, process::Command};

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            copy(&e.path(), &to.join(e.file_name()));
        } else {
            fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}

#[test]
fn every_agent_discovers_locks_and_verifies_with_existing_keys() {
    let root = tempfile::tempdir().unwrap();
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/repo-all-agents"),
        root.path(),
    );
    let ws = discovery::discover(root.path()).unwrap();
    assert_eq!(ws.skills.len(), 7);
    assert_eq!(ws.instructions, [".github/copilot-instructions.md"]);
    assert_eq!(ws.servers.len(), 11);
    for path in [
        ".mcp.json",
        ".codex/config.toml",
        ".cursor/mcp.json",
        ".gemini/settings.json",
        ".vscode/mcp.json",
        ".github/mcp.json",
        "opencode.json",
        ".agents/mcp_config.json",
        ".grok/config.toml",
    ] {
        assert!(ws.servers.iter().any(|s| s.config_path == path), "{path}");
    }
    let local = ws.servers.iter().find(|s| s.name == "local").unwrap();
    match &local.launch {
        Launch::Stdio { command, args, env } => {
            assert_eq!(command, "npx");
            assert_eq!(args, &["-y", "example-mcp@1.0.0"]);
            assert!(env.contains_key("TOKEN"));
        }
        _ => panic!("expected local launch"),
    }
    let outcome = lock::lock(
        root.path(),
        lock::LockOptions {
            offline: true,
            introspect: false,
            remote: false,
        },
    )
    .unwrap();
    assert_eq!(outcome.lock.mcp.len(), 11);
    // Grok Build loads `.mcp.json` too, but a name in both files stays one entry per client's config.
    fs::write(
        root.path().join(".grok/config.toml"),
        "[mcp_servers.claude]\ncommand = \"other-mcp\"\n",
    )
    .unwrap();
    let both = discovery::discover(root.path()).unwrap();
    let clients: Vec<_> = both
        .servers
        .iter()
        .filter(|s| s.name == "claude")
        .map(|s| s.client.as_str())
        .collect();
    assert_eq!(clients, ["claude", "grok"]);
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/repo-all-agents/.grok/config.toml"),
        root.path().join(".grok/config.toml"),
    )
    .unwrap();
    assert!(
        outcome
            .lock
            .mcp
            .windows(2)
            .all(|p| (p[0].client.as_str(), &p[0].name) < (p[1].client.as_str(), &p[1].name))
    );
    assert_eq!(lock::read_lock(root.path()).unwrap().0, outcome.lock);
    assert!(verify::verify(root.path()).unwrap().report.is_empty());
    fs::write(
        root.path().join(".agents/mcp_config.json"),
        r#"{"mcpServers":{"antigravity":{"serverUrl":"https://example.com/changed"}}}"#,
    )
    .unwrap();
    assert!(!verify::verify(root.path()).unwrap().report.is_empty());
}

#[test]
fn outdated_usage_and_empty_lock_exit_codes() {
    let root = tempfile::tempdir().unwrap();
    let run = |flags: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_cedr"))
            .arg("outdated")
            .arg(root.path())
            .args(flags)
            .output()
            .unwrap()
    };
    assert_eq!(run(&[]).status.code(), Some(2));
    lock::lock(root.path(), lock::LockOptions::default()).unwrap();
    for threshold in ["high", "medium", "low", "never"] {
        let o = run(&["--format", "json", "--fail-on", threshold]);
        assert!(o.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&o.stdout).unwrap(),
            serde_json::json!({"version":1,"updates":[]})
        );
    }
    assert_eq!(run(&["--fail-on", "none"]).status.code(), Some(2));
}

#[test]
fn push_updates_field_is_opt_in_against_local_app() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};
    for updates in [false, true] {
        let root = tempfile::tempdir().unwrap();
        lock::lock(root.path(), lock::LockOptions::default()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let config = root.path().join("routes.toml");
        fs::write(
            &config,
            format!("version = 1\n[sync]\nurl = \"http://{addr}\"\norg = \"example\"\n"),
        )
        .unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::park_timeout(Duration::from_millis(2))
                    }
                    Err(e) => panic!("mock app accept: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = Vec::new();
            let (head, length) = loop {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length = headers
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(|s| s.parse::<usize>().unwrap())
                        })
                        .unwrap();
                    break (end + 4, length);
                }
            };
            while bytes.len() < head + length {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
            }
            let body: Value = serde_json::from_slice(&bytes[head..head + length]).unwrap();
            let reply = r#"{"ok":true}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
            body
        });
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cedr"));
        cmd.arg("push")
            .arg(root.path())
            .args(["--repo", "example", "--config"])
            .arg(config)
            .env("CEDR_API_KEY", "synthetic-test-key");
        if updates {
            cmd.arg("--updates");
        }
        let output = cmd.output().unwrap();
        let body = server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if updates {
            assert_eq!(body["updates"], serde_json::json!([]));
        } else {
            assert!(body.get("updates").is_none());
        }
    }
}

#[cfg(unix)]
#[test]
fn new_discovery_paths_keep_containment_and_skill_link_rules() {
    use std::os::unix::fs::symlink;
    let outside = tempfile::tempdir().unwrap();
    fs::write(
        outside.path().join("SKILL.md"),
        "---\nname: external\ndescription: external\n---\n",
    )
    .unwrap();
    fs::write(outside.path().join("config.json"), "{}").unwrap();
    for base in [
        ".cursor/skills",
        ".gemini/skills",
        ".github/skills",
        ".opencode/skills",
        ".grok/skills",
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(base)).unwrap();
        symlink(outside.path(), root.path().join(base).join("external")).unwrap();
        let ws = discovery::discover(root.path()).unwrap();
        assert!(ws.skills.is_empty());
        assert_eq!(ws.skipped, [format!("{base}/external")]);
    }
    for path in [
        ".github/copilot-instructions.md",
        ".vscode/mcp.json",
        ".github/mcp.json",
        "opencode.json",
        ".agents/mcp_config.json",
        ".grok/config.toml",
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(path).parent().unwrap()).unwrap();
        symlink(outside.path().join("config.json"), root.path().join(path)).unwrap();
        assert!(
            discovery::discover(root.path())
                .unwrap_err()
                .to_string()
                .contains("escapes the scanned root"),
            "{path}"
        );
    }
}

#[test]
fn copilot_duplicate_server_key_is_an_actionable_error() {
    let root = tempfile::tempdir().unwrap();
    for path in [".vscode/mcp.json", ".github/mcp.json"] {
        fs::create_dir_all(root.path().join(path).parent().unwrap()).unwrap();
        let key = if path.starts_with(".vscode") {
            "servers"
        } else {
            "mcpServers"
        };
        fs::write(
            root.path().join(path),
            serde_json::json!({key:{"duplicate":{"command":"example-mcp"}}}).to_string(),
        )
        .unwrap();
    }
    let err = discovery::discover(root.path()).unwrap_err().to_string();
    assert!(err.contains("duplicate MCP server copilot.duplicate"));
    assert!(err.contains(".vscode/mcp.json") && err.contains(".github/mcp.json"));
}
