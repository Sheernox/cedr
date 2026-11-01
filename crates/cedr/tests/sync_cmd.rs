//! Sync regression tests use fixture repos and local-only HTTP mocks.
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cedr"))
}
fn run(root: &Path, args: &[&str]) -> Output {
    binary().args(args).arg(root).output().unwrap()
}
fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        if e.path().is_dir() {
            copy(&e.path(), &to.join(e.file_name()));
        } else {
            fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}
fn fixture() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    copy(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sync/source"),
        d.path(),
    );
    d
}
#[test]
fn every_target_matches_golden_and_is_deterministic() {
    let d = fixture();
    let p = cedr_sync::plan(d.path()).unwrap();
    assert!(cedr_sync::apply(d.path(), &p, false).unwrap());
    let expected = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sync/expected");
    for f in &p.files {
        let b = fs::read(d.path().join(&f.path)).unwrap();
        assert_eq!(b, fs::read(expected.join(&f.path)).unwrap(), "{}", f.path);
    }
    let p2 = cedr_sync::plan(d.path()).unwrap();
    assert!(p2.report.is_empty(), "{}", p2.report.to_text());
    assert!(p2.files.iter().all(|f| f.diff.is_empty()));
    let first = fs::read(d.path().join(".cedr/sync.lock")).unwrap();
    assert!(cedr_sync::apply(d.path(), &p2, false).unwrap());
    assert_eq!(first, fs::read(d.path().join(".cedr/sync.lock")).unwrap());
}
#[test]
fn reports_source_drift_edits_missing_and_invalid_config() {
    let d = fixture();
    let p = cedr_sync::plan(d.path()).unwrap();
    cedr_sync::apply(d.path(), &p, false).unwrap();
    fs::write(
        d.path().join(".cedr/rules/10-style.md"),
        "Use narrower types.\n",
    )
    .unwrap();
    assert!(cedr_sync::check(d.path()).to_text().contains("CS001"));
    fs::write(d.path().join("AGENTS.md"), "hand edit\n").unwrap();
    assert!(cedr_sync::check(d.path()).to_text().contains("CS002"));
    assert!(!cedr_sync::apply(d.path(), &cedr_sync::plan(d.path()).unwrap(), false).unwrap());
    fs::remove_file(d.path().join("AGENTS.md")).unwrap();
    assert!(cedr_sync::check(d.path()).to_text().contains("CS003"));
    fs::write(d.path().join(".cedr/sync.toml"), "version=2").unwrap();
    assert!(cedr_sync::check(d.path()).to_text().contains("CS004"));
}
#[test]
fn merge_preserves_foreign_keys_order_and_toml_comments() {
    let d = fixture();
    fs::create_dir_all(d.path().join(".codex")).unwrap();
    fs::write(
        d.path().join(".codex/config.toml"),
        "# retained\nsandbox_mode = 'workspace-write'\n",
    )
    .unwrap();
    fs::write(d.path().join(".mcp.json"), "{\"z\":1,\"a\":2}\n").unwrap();
    let p = cedr_sync::plan(d.path()).unwrap();
    cedr_sync::apply(d.path(), &p, false).unwrap();
    let b = fs::read_to_string(d.path().join(".mcp.json")).unwrap();
    assert!(b.find('z').unwrap() < b.find('a').unwrap());
    assert!(
        fs::read_to_string(d.path().join(".codex/config.toml"))
            .unwrap()
            .starts_with("# retained\nsandbox_mode = 'workspace-write'")
    );
    let mut v: Value = serde_json::from_str(&b).unwrap();
    v["foreign"] = json!(true);
    fs::write(
        d.path().join(".mcp.json"),
        serde_json::to_string(&v).unwrap(),
    )
    .unwrap();
    assert!(cedr_sync::check(d.path()).is_empty());
    fs::write(
        d.path().join(".cedr/sync.toml"),
        "version=1\ntargets=['agents']\n",
    )
    .unwrap();
    fs::remove_file(d.path().join(".cedr/rules/30-claude.md")).unwrap();
    let p = cedr_sync::plan(d.path()).unwrap();
    cedr_sync::apply(d.path(), &p, false).unwrap();
    let v: Value = serde_json::from_slice(&fs::read(d.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(v, json!({"z":1,"a":2,"foreign":true}));
}
#[test]
fn refuses_unmanaged_markdown_and_init_adopts_existing_files() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    fs::write(r.join("AGENTS.md"), "Project instructions.\n").unwrap();
    fs::write(r.join("CLAUDE.md"), "Claude instructions.\n").unwrap();
    fs::create_dir_all(r.join(".claude")).unwrap();
    fs::create_dir_all(r.join(".cursor")).unwrap();
    fs::write(r.join(".mcp.json"),r#"{"mcpServers":{"local":{"command":"npx","args":["example@1.2.3"],"env":{"TOKEN":"sk-example"}}}}"#).unwrap();
    fs::write(
        r.join(".cursor/mcp.json"),
        r#"{"mcpServers":{"local":{"command":"other"}}}"#,
    )
    .unwrap();
    fs::write(
        r.join(".claude/settings.json"),
        r#"{"permissions":{"allow":["Bash(npm test)","Read(src/**)"]}}"#,
    )
    .unwrap();
    let notes = cedr_sync::import::init(r, None).unwrap();
    assert!(notes.iter().any(|s| s.contains("set variable TOKEN")));
    assert!(notes.iter().any(|s| s.contains("different launch")));
    let c = cedr_sync::load_config(r).unwrap();
    assert_eq!(c.mcp["local"].env["TOKEN"], "${TOKEN}");
    assert!(c.permissions.allow.contains(&"shell:npm test".into()));
    let p = cedr_sync::plan(r).unwrap();
    assert!(p.files.iter().all(|f| !f.diff.contains("sk-example")));
    assert!(cedr_sync::apply(r, &p, false).unwrap());
    assert!(cedr_sync::check(r).is_empty());
    let d = fixture();
    fs::write(d.path().join("AGENTS.md"), "unmanaged\n").unwrap();
    let p = cedr_sync::plan(d.path()).unwrap();
    assert!(!cedr_sync::apply(d.path(), &p, false).unwrap());
    assert!(cedr_sync::apply(d.path(), &p, true).unwrap());
}
#[test]
fn canonical_org_hash_matches_javascript_fixture() {
    let v: Value = serde_json::from_str(include_str!("../../../fixtures/sync/hash.json")).unwrap();
    assert_eq!(cedr_sync::org::policy_hash(&v["input"]), v["expected"]);
}
fn org(root: &Path) -> Value {
    let mut v = json!({"org":"org-fixture","version":7,"instructions":"Organization instructions.\n","deny":["read:.env"],"banned":["npm:example-*"],"targets":["unknown-required"],"checks":{"unrestrictedShell":false,"pinnedServers":true},"skills":[{"name":"policy","files":[{"path":"SKILL.md","content":"---\nname: policy\ndescription: Required policy\n---\nReview.\n"}]}]});
    v["hash"] = json!(cedr_sync::org::policy_hash(&v));
    fs::write(
        root.join(".cedr/org-rules.json"),
        serde_json::to_vec(&v).unwrap(),
    )
    .unwrap();
    v
}
#[test]
fn org_policy_applies_and_reports_every_check() {
    let d = fixture();
    let config = d.path().join(".cedr/sync.toml");
    let text = fs::read_to_string(&config)
        .unwrap()
        .replace("example-mcp@1.2.3", "example-mcp")
        .replace("\"shell:npm\"", "\"shell:*\"");
    fs::write(config, text).unwrap();
    org(d.path());
    let p = cedr_sync::plan(d.path()).unwrap();
    let findings = p.report.to_text();
    for code in ["CS101", "CS102", "CS103", "CS104", "CS105"] {
        assert!(findings.contains(code), "{findings}");
    }
    assert!(!p.config.mcp.contains_key("local"));
    assert!(cedr_sync::apply(d.path(), &p, false).unwrap());
    assert!(
        fs::read_to_string(d.path().join("AGENTS.md"))
            .unwrap()
            .contains("Organization instructions")
    );
    let mut v = org(d.path());
    v["instructions"] = json!("changed");
    fs::write(
        d.path().join(".cedr/org-rules.json"),
        serde_json::to_vec(&v).unwrap(),
    )
    .unwrap();
    assert!(cedr_sync::plan(d.path()).is_err());
}
#[test]
fn dry_run_writes_nothing_and_cli_check_formats_findings() {
    let d = fixture();
    let out = binary()
        .args(["sync", "--dry-run"])
        .arg(d.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("@@"));
    assert!(!d.path().join("AGENTS.md").exists());
    let out = binary()
        .args(["sync", "check", "--format", "json"])
        .arg(d.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!v["findings"].as_array().unwrap().is_empty());
    let out = run(d.path(), &["sync"]);
    assert!(out.status.success());
}
#[cfg(unix)]
#[test]
fn refuses_symlinks_in_sources_and_destinations() {
    use std::os::unix::fs::symlink;
    let d = fixture();
    symlink("/tmp", d.path().join(".claude")).unwrap();
    assert!(cedr_sync::plan(d.path()).is_err());
    let d = fixture();
    symlink("/tmp", d.path().join(".cedr/skills/review/link")).unwrap();
    assert!(cedr_sync::plan(d.path()).is_err());
}

/// A single local request, recording headers privately and asserting only nonsecret fields.
fn mock(status: u16, body: Value) -> (String, std::thread::JoinHandle<Value>) {
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let task = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut bytes = vec![];
        let mut buf = [0; 4096];
        let mut end = None;
        let mut length = 0;
        loop {
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if end.is_none()
                && let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n")
            {
                end = Some(i + 4);
                let headers = String::from_utf8_lossy(&bytes[..i]);
                length = headers
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|s| s.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
            }
            if end.is_some_and(|i| bytes.len() >= i + length) {
                break;
            }
        }
        let start = String::from_utf8_lossy(&bytes)
            .lines()
            .next()
            .unwrap()
            .to_string();
        let value = if length == 0 {
            json!({})
        } else {
            serde_json::from_slice(&bytes[end.unwrap()..end.unwrap() + length]).unwrap()
        };
        let text = body.to_string();
        write!(socket,"HTTP/1.1 {status} reply\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{text}",text.len()).unwrap();
        json!({"request":start,"body":value})
    });
    (url, task)
}
fn online(root: &Path, url: &str, sub: &str) -> Output {
    let config = root.join("routes.toml");
    fs::write(
        &config,
        format!("version=1\n[sync]\norg='org-fixture'\nurl={url:?}\n"),
    )
    .unwrap();
    binary()
        .args(["sync", sub])
        .arg(root)
        .arg("--config")
        .arg(config)
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap()
}
#[test]
fn pull_handles_200_404_and_403_and_preserves_rules_on_failure() {
    let d = fixture();
    let policy = org(d.path());
    let (url, task) = mock(200, policy.clone());
    let out = online(d.path(), &url, "pull");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        task.join().unwrap()["request"],
        "GET /v1/sync/rules?org=org-fixture HTTP/1.1"
    );
    let before = fs::read(d.path().join(".cedr/org-rules.json")).unwrap();
    let (url, task) = mock(403, json!({"error":"plan_required"}));
    let out = online(d.path(), &url, "pull");
    assert!(out.status.success());
    task.join().unwrap();
    assert_eq!(
        before,
        fs::read(d.path().join(".cedr/org-rules.json")).unwrap()
    );
    let (url, task) = mock(403, json!({"error":"not_a_member"}));
    let out = online(d.path(), &url, "pull");
    assert_eq!(out.status.code(), Some(2));
    task.join().unwrap();
    assert_eq!(
        before,
        fs::read(d.path().join(".cedr/org-rules.json")).unwrap()
    );
    let (url, task) = mock(404, json!({"error":"no_rules"}));
    let out = online(d.path(), &url, "pull");
    assert!(out.status.success());
    task.join().unwrap();
    assert!(!d.path().join(".cedr/org-rules.json").exists());
}
#[test]
fn sync_push_has_exact_body_fields_and_newer_policy_hint() {
    let d = fixture();
    org(d.path());
    let p = cedr_sync::plan(d.path()).unwrap();
    cedr_sync::apply(d.path(), &p, false).unwrap();
    let (url, task) = mock(200, json!({"ok":true,"rules":{"version":8}}));
    let out = online(d.path(), &url, "push");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("org rules v8 are available: run cedr sync pull")
    );
    let request = task.join().unwrap();
    assert_eq!(request["request"], "PUT /v1/sync/repo HTTP/1.1");
    let body = &request["body"];
    let mut keys: Vec<_> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "at", "cli", "commit", "counts", "files", "findings", "org", "repo", "rules", "targets"
        ]
    );
    assert_eq!(body["rules"]["version"], 7);
    assert!(
        body["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["state"] == "ok")
    );
    assert!(!body.to_string().contains("Organization instructions"));
    assert!(!body.to_string().contains("Bearer"));
}
#[test]
fn strict_sources_and_skill_size_fail_without_echoing_secret_values() {
    for text in [
        "version=2\ntargets=['agents']",
        "version=1\ntargets=[]",
        "version=1\ntargets=['agents','agents']",
        "version=1\ntargets=['unknown']",
        "version=1\ntargets=['agents']\nunknown='private-value'",
        "version=1\ntargets=['agents']\n[mcp.local]\ncommand='tool'\nurl='https://example.com'",
        "version=1\ntargets=['agents']\n[mcp.local]\ncommand='tool'\nenv={TOKEN='sk-private-value'}",
    ] {
        let e = cedr_sync::config::Config::parse(text).unwrap_err();
        assert!(!e.contains("private-value"));
    }
    let d = fixture();
    fs::write(
        d.path().join(".cedr/skills/review/large.bin"),
        vec![0; 1024 * 1024 + 1],
    )
    .unwrap();
    assert!(cedr_sync::plan(d.path()).is_err());
}
#[test]
fn copies_empty_and_binary_files_and_reports_binary_edits() {
    let d = fixture();
    let file = d.path().join(".cedr/skills/review/data.bin");
    fs::write(&file, [0xff, 0xfe]).unwrap();
    fs::write(d.path().join(".cedr/skills/review/empty"), []).unwrap();
    let p = cedr_sync::plan(d.path()).unwrap();
    assert!(cedr_sync::apply(d.path(), &p, false).unwrap());
    assert!(cedr_sync::check(d.path()).is_empty());
    fs::write(file, [0xfe, 0xff]).unwrap();
    let p = cedr_sync::plan(d.path()).unwrap();
    assert!(cedr_sync::apply(d.path(), &p, false).unwrap());
    assert_eq!(
        fs::read(d.path().join(".agents/skills/review/data.bin")).unwrap(),
        [0xfe, 0xff]
    );
}
#[test]
fn diffs_never_print_url_queries() {
    let d = fixture();
    let file = d.path().join(".cedr/sync.toml");
    let text = fs::read_to_string(&file).unwrap().replace(
        "https://example.com/mcp",
        "https://example.com/mcp?token=private-query",
    );
    fs::write(file, text).unwrap();
    fs::write(
        d.path().join(".cedr/rules/10-style.md"),
        "See https://example.com/docs?token=private-query\n",
    )
    .unwrap();
    let p = cedr_sync::plan(d.path()).unwrap();
    assert!(p.files.iter().all(|f| !f.diff.contains("private-query")));
}
#[test]
fn import_reports_mixed_native_execpolicy_entries() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join(".codex/rules")).unwrap();
    fs::write(d.path().join(".codex/rules/cedr.rules"),"prefix_rule(pattern = [\"git\",\"status\"], decision = \"allow\")\nprefix_rule(pattern = [\"git\",\"push\"], decision = \"prompt\")\n").unwrap();
    let notes = cedr_sync::import::init(d.path(), None).unwrap();
    assert!(
        notes
            .iter()
            .any(|s| s.contains("execpolicy rules: no translation"))
    );
    assert!(
        cedr_sync::load_config(d.path())
            .unwrap()
            .permissions
            .allow
            .contains(&"shell:git status".into())
    );
}
#[test]
fn a_repo_without_permissions_or_hooks_gets_no_empty_settings_files() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join(".cedr/rules")).unwrap();
    fs::write(d.path().join(".cedr/rules/10-tests.md"), "Run the tests.\n").unwrap();
    fs::write(
        d.path().join(".cedr/sync.toml"),
        "version = 1\ntargets = [\"agents\", \"claude\", \"cursor\", \"codex\"]\n\n[mcp.docs]\nurl = \"https://mcp.example.com/mcp\"\n",
    )
    .unwrap();
    let out = run(d.path(), &["sync"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for absent in [
        ".claude/settings.json",
        ".cursor/cli.json",
        ".cursor/hooks.json",
        ".codex/hooks.json",
        ".codex/rules/cedr.rules",
    ] {
        assert!(
            !d.path().join(absent).exists(),
            "{absent} was written with nothing in it"
        );
    }
    assert!(d.path().join(".mcp.json").exists());
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(!printed.contains("not supported by agents"), "{printed}");
    assert!(run(d.path(), &["sync", "check"]).status.success());
}
#[test]
fn servers_with_only_the_agents_target_are_reported_once() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join(".cedr")).unwrap();
    fs::write(
        d.path().join(".cedr/sync.toml"),
        "version = 1\ntargets = [\"agents\"]\n\n[mcp.docs]\nurl = \"https://mcp.example.com/mcp\"\n",
    )
    .unwrap();
    let out = run(d.path(), &["sync"]);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(all.matches("reach no agent").count(), 1, "{all}");
}
/// A repo that writes for Grok Build and for agents whose files Grok Build also reads.
fn grok_repo(targets: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join(".cedr/rules")).unwrap();
    fs::write(
        d.path().join(".cedr/sync.toml"),
        format!(
            r#"version = 1
targets = [{targets}]
[mcp.local]
command = "npx"
args = ["-y", "example-mcp@1.2.3"]
env = {{ API_KEY = "${{API_KEY}}" }}
[mcp.docs]
url = "https://example.com/mcp"
headers = {{ Authorization = "Bearer ${{DOCS_TOKEN}}" }}
[permissions]
allow = ["fetch:example.com", "mcp:docs", "mcp:local/ping", "read:src/**", "shell:*", "shell:npm test"]
deny = ["edit:private/**", "read:.env", "shell:rm -rf"]
[[hooks]]
event = "before-shell"
command = "bin/check.sh"
[[hooks]]
event = "after-edit"
command = "npm run format"
"#
        ),
    )
    .unwrap();
    for (name, text) in [
        ("10-scoped", "---\npaths: [\"src/**\"]\n---\nScoped.\n"),
        (
            "20-claude",
            "---\nagents: [\"claude\"]\n---\nClaude only.\n",
        ),
    ] {
        fs::write(d.path().join(format!(".cedr/rules/{name}.md")), text).unwrap();
    }
    d
}
#[test]
fn grok_gets_each_rule_once_and_is_told_what_else_it_reads() {
    let d = grok_repo(r#""agents", "claude", "cursor", "grok""#);
    for (name, text) in [
        (
            "30-both",
            "---\nagents: [\"claude\", \"grok\"]\n---\nBoth.\n",
        ),
        ("40-grok", "---\nagents: [\"grok\"]\n---\nGrok only.\n"),
        (
            "50-grok-scoped",
            "---\nagents: [\"grok\"]\npaths: [\"a/**\", \"b/**\"]\n---\nScoped to grok.\n",
        ),
    ] {
        fs::write(d.path().join(format!(".cedr/rules/{name}.md")), text).unwrap();
    }
    let p = cedr_sync::plan(d.path()).unwrap();
    cedr_sync::apply(d.path(), &p, false).unwrap();
    let exists = |path: &str| d.path().join(path).exists();
    // Grok Build always reads CLAUDE.md, so a second copy under .grok/rules would load the rule twice.
    assert!(
        fs::read_to_string(d.path().join("CLAUDE.md"))
            .unwrap()
            .contains("Both.")
    );
    assert!(!exists(".grok/rules/30-both.md"));
    assert!(exists(".grok/rules/40-grok.md"));
    assert!(
        fs::read_to_string(d.path().join(".grok/rules/50-grok-scoped.md"))
            .unwrap()
            .contains("## When working in a/**, b/**\n\nScoped to grok.")
    );
    // Grok Build reads `*.md` in `.cursor/rules/`, so Cursor's `.mdc` files must stay out of its way.
    assert!(!exists(".cursor/rules/10-scoped.md") && exists(".cursor/rules/10-scoped.mdc"));
    let noted = |text: &str| p.notes.iter().any(|n| n.contains(text));
    assert!(noted(
        "rule 10-scoped: Grok Build also loads .claude/rules/10-scoped.md"
    ));
    assert!(noted("rule 20-claude: Grok Build also reads CLAUDE.md"));
    assert!(!noted("rule 30-both"));
    assert!(noted("grok --trust"));
    // Without a claude target there is nothing to warn about, and the rule has its own file.
    let solo = grok_repo(r#""agents", "grok""#);
    fs::remove_file(solo.path().join(".cedr/rules/20-claude.md")).unwrap();
    let p = cedr_sync::plan(solo.path()).unwrap();
    assert!(
        !p.notes.iter().any(|n| n.starts_with("rule ")),
        "{:?}",
        p.notes
    );
}
#[test]
fn grok_files_survive_an_import_with_nothing_lost_and_a_path_hook_still_reaches_the_repo_root() {
    let d = grok_repo(r#""agents", "grok""#);
    fs::remove_file(d.path().join(".cedr/rules/20-claude.md")).unwrap();
    let before = cedr_sync::load_config(d.path()).unwrap();
    let p = cedr_sync::plan(d.path()).unwrap();
    cedr_sync::apply(d.path(), &p, false).unwrap();
    let hooks: Value =
        serde_json::from_slice(&fs::read(d.path().join(".grok/hooks/cedr.json")).unwrap()).unwrap();
    assert_eq!(
        hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "../../bin/check.sh"
    );
    assert_eq!(
        hooks["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        "npm run format"
    );
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(d.path().join(".grok/config.toml")).unwrap()).unwrap();
    assert_eq!(
        config["mcp_servers"]["docs"]["headers"]["Authorization"].as_str(),
        Some("Bearer ${DOCS_TOKEN}")
    );
    fs::remove_dir_all(d.path().join(".cedr")).unwrap();
    fs::remove_file(d.path().join("AGENTS.md")).unwrap();
    cedr_sync::import::init(d.path(), None).unwrap();
    let after = cedr_sync::load_config(d.path()).unwrap();
    assert!(after.targets.iter().any(|t| t == "grok"));
    assert_eq!(after.permissions.allow, before.permissions.allow);
    assert_eq!(after.permissions.deny, before.permissions.deny);
    assert_eq!(after.mcp["local"].env["API_KEY"], "${API_KEY}");
    assert_eq!(
        after.mcp["docs"].headers["Authorization"],
        "Bearer ${DOCS_TOKEN}"
    );
    let commands: Vec<_> = after
        .hooks
        .iter()
        .map(|h| (h.event.as_str(), h.command.as_str()))
        .collect();
    assert_eq!(
        commands,
        [
            ("before-shell", "bin/check.sh"),
            ("after-edit", "npm run format")
        ]
    );
}
#[test]
fn grok_import_reports_rules_it_cannot_translate() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join(".grok/rules")).unwrap();
    fs::write(d.path().join(".grok/rules/mine.md"), "Be brief.\n").unwrap();
    fs::write(
        d.path().join(".grok/config.toml"),
        "[permission]\nallow = [\"Bash(git *)\"]\nask = [\"Edit(a)\"]\nrules = [{ action = \"ask\", tool = \"read\", pattern = \"x\" }, { action = \"allow\", tool = \"grep\" }]\n",
    )
    .unwrap();
    let notes = cedr_sync::import::init(d.path(), None).unwrap();
    assert_eq!(
        notes,
        [
            ".grok/config.toml: permission.ask: no translation",
            ".grok/config.toml: permission.rules: no translation",
            ".grok/config.toml: permission.rules: no translation",
        ]
    );
    let c = cedr_sync::load_config(d.path()).unwrap();
    assert_eq!(c.permissions.allow, ["shell:git"]);
    assert!(c.targets.iter().any(|t| t == "grok"));
    let rule = fs::read_to_string(d.path().join(".cedr/rules/50-grok-mine.md")).unwrap();
    assert!(rule.contains("Be brief.") && rule.contains("grok"));
}
#[test]
fn a_server_grok_cannot_namespace_is_left_out_and_named() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join(".cedr")).unwrap();
    fs::write(
        d.path().join(".cedr/sync.toml"),
        "version = 1\ntargets = [\"grok\"]\n[mcp.ok-name]\nurl = \"https://a.example\"\n[mcp.1digit]\nurl = \"https://b.example\"\n[mcp.has__two]\nurl = \"https://c.example\"\n",
    )
    .unwrap();
    let p = cedr_sync::plan(d.path()).unwrap();
    cedr_sync::apply(d.path(), &p, false).unwrap();
    let config = fs::read_to_string(d.path().join(".grok/config.toml")).unwrap();
    assert!(
        config.contains("ok-name") && !config.contains("1digit") && !config.contains("has__two")
    );
    assert!(
        p.notes
            .iter()
            .any(|n| n == "mcp.1digit: not supported by grok")
    );
}
