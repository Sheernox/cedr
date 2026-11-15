//! Command boundaries use scratch XDG locations and a stub program; no vendor or Docker is needed.
mod common;
use common::{Scratch, stdout};
use serde_json::Value;
use std::path::Path;
fn suite() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/bench/suite")
        .display()
        .to_string()
}
#[test]
fn usage_errors_and_history_have_stable_exit_codes() {
    let s = Scratch::new(0.0, 0.0);
    let o = s.cedr(&["bench", "history", "--format", "json"]);
    assert!(o.status.success());
    assert_eq!(stdout(&o).trim(), "[]");
    let o = s.cedr(&["bench", "show", "../../missing", "--format", "json"]);
    assert_eq!(o.status.code(), Some(2));
    for harness in ["command", "antigravity"] {
        let o = s.cedr(&[
            "bench",
            "probe",
            "--harness",
            harness,
            "--model",
            "stub",
            "--suite",
            &suite(),
        ]);
        assert_eq!(o.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&o.stderr).contains("Compare"));
    }
}
#[test]
fn cached_suite_drift_is_refused_before_any_network_or_agent() {
    let s = Scratch::new(0.0, 0.0);
    let root = s.dir.path().join("cache/cedr/bench");
    std::fs::create_dir_all(&root).unwrap();
    cedr_bench::suite::copy(Path::new(&suite()), &root.join("edition-1")).unwrap();
    std::fs::write(
        root.join("edition-1.json"),
        r#"{"sha256":"wrong","commit":null}"#,
    )
    .unwrap();
    let o = s.cedr(&[
        "bench",
        "probe",
        "--edition",
        "1",
        "--harness",
        "command",
        "--model",
        "stub",
    ]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("digest changed"));
}

#[test]
fn show_export_and_review_preserve_validated_results_and_local_patches() {
    use cedr_bench::report::*;
    use cedr_bench::suite::Kind;
    let s = Scratch::new(0.0, 0.0);
    let id = "b-20261113T000000Z-abcdef";
    let mut task = TaskResult::pending("code", Kind::Code);
    task.status = Status::Graded;
    task.checks = ["case", "edge", "regression", "constraint"]
        .iter()
        .map(|id| Check {
            id: id.to_string(),
            passed: true,
        })
        .collect();
    task.score = Some(1.0);
    task.solved = true;
    task.cost_usd = Some(0.0);
    task.patch_sha256 = Some(cedr_bench::sha(b"local audit patch content"));
    let mut visual = TaskResult::pending("visual", Kind::Visual);
    visual.status = Status::Graded;
    visual.cost_usd = Some(0.0);
    visual.gates.push(Gate {
        id: "file".into(),
        passed: true,
        detail: None,
    });
    let png = b"\x89PNG\r\n\x1a\n";
    visual.artifacts.push(Artifact {
        path: "visual/screen.png".into(),
        sha256: cedr_bench::sha(png),
        bytes: png.len() as u64,
    });
    let mut r = BenchResult {
        version: 1,
        kind: "bench".into(),
        id: id.into(),
        edition: EditionInfo {
            number: 1,
            suite_sha256: "a".repeat(64),
            commit: None,
            image: "".into(),
        },
        config: Config {
            harness: "command".into(),
            harness_version: "custom".into(),
            model: "stub".into(),
            effort: None,
            resolved_models: vec![],
        },
        environment: Environment {
            mode: "local".into(),
            clean_home: false,
            network: "host".into(),
            pinned: false,
            agent_image: None,
            verified: Vec::new(),
            limits: None,
            os: "linux".into(),
            arch: "x86_64".into(),
            cli: "test".into(),
        },
        probe: probe(
            &Usage {
                tokens: Tokens {
                    input_tokens: Some(100),
                    cache_read_tokens: Some(0),
                    cache_write_tokens: Some(0),
                    ..Tokens::default()
                },
                cost_usd: Some(0.0),
                ..Usage::default()
            },
            None,
            true,
            true,
        ),
        billing: "plan".into(),
        started_at: "2026-11-13T00:00:00Z".into(),
        finished_at: "2026-11-13T00:00:01Z".into(),
        goal_usd: 10.0,
        stop_usd: None,
        spent_usd: 0.0,
        over_goal: false,
        unpriced_trials: 0,
        complete: true,
        stop: None,
        score: 100.0,
        solved: 1,
        code_tasks: 1,
        eligible: false,
        tasks: vec![task, visual],
        tamper_review: None,
    };
    for task in &mut r.tasks {
        task.patch_sha256 = Some(cedr_bench::sha(b"local audit patch content"));
    }
    r.summarize();
    let root = s.dir.path().join("state/cedr/bench");
    let source = root.join(id).join("artifacts/visual");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("screen.png"), png).unwrap();
    for task in &r.tasks {
        let dir = root.join(id).join("artifacts").join(&task.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("submission.patch"), b"local audit patch content").unwrap();
    }
    let path = root.join(format!("{id}.json"));
    let value = serde_json::to_value(&r).unwrap();
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let shown = s.cedr(&["bench", "show", id, "--format", "json"]);
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&shown.stdout).unwrap(),
        value
    );
    let out = s.dir.path().join("export");
    let audit = root.join(id).join("artifacts/code/submission.patch");
    std::fs::create_dir_all(audit.parent().unwrap()).unwrap();
    std::fs::write(&audit, "local audit patch content").unwrap();
    let exported = s.cedr(&["bench", "export", id, "--out", out.to_str().unwrap()]);
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let records: Value =
        serde_json::from_slice(&std::fs::read(out.join("results.json")).unwrap()).unwrap();
    assert_eq!(records, serde_json::json!([value]));
    assert_eq!(
        std::fs::read(out.join("shots").join(id).join("visual/screen.png")).unwrap(),
        png
    );
    let mut files = Vec::new();
    fn exported_files(root: &std::path::Path, at: &std::path::Path, files: &mut Vec<String>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                exported_files(root, &path, files);
            } else {
                files.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned(),
                );
            }
        }
    }
    exported_files(&out, &out, &mut files);
    files.sort();
    assert_eq!(
        files,
        [
            "results.json".to_owned(),
            format!("shots/{id}/visual/screen.png")
        ]
    );
    let refused = s.cedr(&["bench", "review", id, "--clean", "--note", "Read patches"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("no tamper signals"));
    r.probe.status = ProbeStatus::Match;
    r.config.harness = "codex".into();
    r.environment.mode = "container".into();
    r.environment.pinned = true;
    r.environment.clean_home = true;
    r.environment.network = "allowlist".into();
    r.tasks[0].tamper_signals.push(TamperSignal {
        path: "answer.py".into(),
        line: 4,
        kind: "descriptor_access".into(),
    });
    r.summarize();
    assert!(!r.eligible);
    std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
    let shown = stdout(&s.cedr(&["bench", "show", id]));
    assert!(shown.contains("answer.py:4: descriptor_access"));
    assert!(shown.contains(audit.to_str().unwrap()));
    assert!(shown.contains("cedr bench review"));
    let before = std::fs::read(&path).unwrap();
    for args in [
        vec!["bench", "review", id, "--clean"],
        vec!["bench", "review", id, "--note", "text"],
        vec![
            "bench",
            "review",
            id,
            "--clean",
            "--tampered",
            "--note",
            "text",
        ],
        vec!["bench", "review", id, "--clean", "--note", ""],
        vec!["bench", "review", id, "--clean", "--note", "two\nlines"],
    ] {
        assert_eq!(s.cedr(&args).status.code(), Some(2));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    for contents in [None, Some("changed patch")] {
        if let Some(contents) = contents {
            std::fs::write(&audit, contents).unwrap();
        } else {
            std::fs::remove_file(&audit).unwrap();
        }
        let refused = s.cedr(&["bench", "review", id, "--clean", "--note", "Read patches"]);
        assert_eq!(refused.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&refused.stderr).contains("submission.patch"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    std::fs::write(&audit, "local audit patch content").unwrap();
    for (flag, verdict, eligible) in [
        ("--clean", "clean", true),
        ("--tampered", "tampered", false),
        ("--clean", "clean", true),
    ] {
        let reviewed = s.cedr(&[
            "bench",
            "review",
            id,
            flag,
            "--note",
            "Read the saved patch",
        ]);
        assert!(
            reviewed.status.success(),
            "{}",
            String::from_utf8_lossy(&reviewed.stderr)
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["tamper_review"]["verdict"], verdict);
        assert_eq!(saved["tamper_review"]["note"], "Read the saved patch");
        chrono::DateTime::parse_from_rfc3339(saved["tamper_review"]["time"].as_str().unwrap())
            .unwrap();
        assert_eq!(saved["eligible"], eligible);
        assert_eq!(saved["score"], 100.0);
        assert_eq!(saved["tasks"][0]["tamper_signals"][0]["line"], 4);
    }
    let mut r: BenchResult = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut missing_patch = r.clone();
    missing_patch.tasks[0].patch_sha256 = None;
    missing_patch.tasks[0].status = Status::SetupFailed;
    missing_patch.tasks[0].checks.clear();
    missing_patch.tasks[0].score = Some(0.0);
    missing_patch.tasks[0].solved = false;
    missing_patch.summarize();
    std::fs::write(&path, serde_json::to_vec(&missing_patch).unwrap()).unwrap();
    let refused = s.cedr(&["bench", "review", id, "--clean", "--note", "Read patches"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("run must be run again"));
    std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
    // A flagged task must retain its patch for show, history and clean review.
    std::fs::remove_file(&audit).unwrap();
    assert_eq!(s.cedr(&["bench", "show", id]).status.code(), Some(2));
    assert_eq!(
        stdout(&s.cedr(&["bench", "history", "--format", "json"])).trim(),
        "[]"
    );
    std::fs::write(&audit, "local audit patch content").unwrap();
    let destination = s.dir.path().join("reviewed-export");
    assert!(
        s.cedr(&[
            "bench",
            "export",
            id,
            "--out",
            destination.to_str().unwrap()
        ])
        .status
        .success()
    );
    let exported: Value =
        serde_json::from_slice(&std::fs::read(destination.join("results.json")).unwrap()).unwrap();
    assert_eq!(exported[0]["tamper_review"]["verdict"], "clean");
    assert_eq!(
        exported[0]["tasks"][0]["tamper_signals"],
        serde_json::json!([{"kind":"descriptor_access"}])
    );
    let shown = stdout(&s.cedr(&["bench", "show", id]));
    assert!(shown.contains("tamper review Clean"));
    assert!(!shown.contains("then: cedr bench review"));
    let mut incomplete: BenchResult =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    incomplete.tasks[0].cost_usd = None;
    incomplete.summarize();
    std::fs::write(&path, serde_json::to_vec(&incomplete).unwrap()).unwrap();
    let shown = stdout(&s.cedr(&["bench", "show", id]));
    assert!(shown.contains("at least $0.0000, 1 trials unpriced (cost incomplete)"));
    assert!(shown.contains("over_goal=unknown"));
    let history = stdout(&s.cedr(&["bench", "history"]));
    assert!(history.contains("cost incomplete"));
    let destination = s.dir.path().join("incomplete-export");
    assert!(
        s.cedr(&[
            "bench",
            "export",
            id,
            "--out",
            destination.to_str().unwrap()
        ])
        .status
        .success()
    );
    let unknown: Value =
        serde_json::from_slice(&std::fs::read(destination.join("results.json")).unwrap()).unwrap();
    assert_eq!(unknown[0]["unpriced_trials"], 1);
    assert_eq!(unknown[0]["over_goal"], true);
    {
        use serde_json::json;
        use std::io::{BufRead, BufReader, Write};
        use std::process::Stdio;
        let config = common::config();
        let mut child = s
            .command(&["mcp", "--config", config.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut receive = |id: u64| loop {
            let row: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            if row["id"] == id {
                break row;
            }
        };
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}})).unwrap();
        receive(1);
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"bench_show","arguments":{"id":id}}})).unwrap();
        let reply = receive(2);
        assert!(
            reply["result"]["content"][1]["text"]
                .as_str()
                .unwrap()
                .contains("Cost incomplete")
        );
        child.kill().unwrap();
        child.wait().unwrap();
        let body: Value =
            serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["tamper_review"], exported[0]["tamper_review"]);
        assert_eq!(
            body["tasks"][0]["tamper_signals"],
            serde_json::to_value(&r.tasks[0].tamper_signals).unwrap()
        );
    }
    let mut mismatch = r.clone();
    mismatch.tasks[0].tamper_signals[0].kind = "evidence_mismatch".into();
    mismatch.summarize();
    std::fs::write(&path, serde_json::to_vec(&mismatch).unwrap()).unwrap();
    let refused = s.cedr(&["bench", "review", id, "--clean", "--note", "Read patches"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("evidence_mismatch"));
    r.score = 99.0;
    std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
    let shown = s.cedr(&["bench", "show", id]);
    assert_eq!(shown.status.code(), Some(2));
}

#[test]
fn old_suite_file_commands_name_compare_and_history_ignores_old_results() {
    let s = Scratch::new(0.0, 0.0);
    let out = s.cedr(&["bench", "run", ".cedr/bench/suite.toml", "--no-push"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cedr compare run"));
    let results = s.dir.path().join("state/cedr/bench");
    std::fs::create_dir_all(&results).unwrap();
    std::fs::write(
        results.join("b-old.json"),
        r#"{"version":1,"kind":"compare"}"#,
    )
    .unwrap();
    std::fs::write(results.join("unrelated.json"), "not a result").unwrap();
    let out = s.cedr(&["bench", "history", "--format", "json"]);
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), "[]");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("skipped"));
}

#[test]
fn compare_refuses_a_legacy_cap_even_with_an_explicit_override() {
    let s = Scratch::new(0.0, 0.0);
    let config = s.dir.path().join("config/cedr");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("cli.toml"),
        "telemetry=false\nauto_update=false\nbench_budget_usd=0.5\n",
    )
    .unwrap();
    let out = s.cedr(&["compare", "run", "missing.toml", "--budget-usd", "2"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("bench_budget_usd was renamed to compare_budget_usd")
    );
}

#[cfg(unix)]
#[test]
fn container_effort_uses_the_layer_catalog_without_calling_the_host_agent() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new(0.0, 0.0);
    let bin = s.dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let docker = bin.join("docker");
    std::fs::write(&docker, r#"#!/usr/bin/env python3
import sys,json,pathlib,os
args=sys.argv[1:]
ready=pathlib.Path(os.environ['BENCH_TEST_NETWORK_READY'])
if args[0]=='version': print('29.0.0')
elif args[0]=='info': print('[]')
elif args[:2]==['image','inspect']: print('sha256:'+'0'*64)
elif args[:2]==['network','connect']: ready.write_text('connected')
elif args[:2]==['network','inspect']:
    if args[2]=='bridge': sys.exit('must inspect the task network gateway')
    else: print(json.dumps([{'Internal':True,'IPAM':{'Config':[{'Gateway':'172.31.0.1'}]},'Options':{'com.docker.network.bridge.gateway_mode_ipv4':'isolated'}}]))
elif args[0]=='run':
    if args[-1].startswith('import socket,errno'): assert '172.31.0.1' in args[-1]
    if args[-1].startswith('import socket,errno') and not ready.exists(): sys.exit(23)
    if '--version' in args: print('test')
    elif '--input-format' in args:
        print(json.dumps({'response':{'response':{'models':[{'value':'claude-haiku-5-5','supportedEffortLevels':['high']}]}}}))
    elif '--model' in args:
        workspace=next(a.split('src=')[1].split(',')[0] for a in args if 'dst=/workspace' in a)
        pathlib.Path(workspace,'probe.txt').write_text('cedr-bench-probe')
        pathlib.Path(workspace,'command.txt').write_text('cedr-bench-command')
        print(json.dumps({'type':'result','modelUsage':{'claude-haiku-5-5':{'inputTokens':100,'cacheReadInputTokens':0,'cacheCreationInputTokens':0,'outputTokens':1}}}))
"#).unwrap();
    std::fs::set_permissions(docker, std::fs::Permissions::from_mode(0o755)).unwrap();
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\necho host-agent-must-not-run >&2\nexit 17\n",
    )
    .unwrap();
    std::fs::set_permissions(claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let out = s
        .command(&[
            "bench",
            "probe",
            "--harness",
            "claude",
            "--model",
            "claude-haiku-5-5",
            "--agent-version",
            "test",
            "--effort",
            "high",
            "--suite",
            &suite(),
            "--set-reference",
            "--format",
            "json",
        ])
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("ANTHROPIC_API_KEY", "fake-test-key")
        .env(
            "BENCH_TEST_NETWORK_READY",
            s.dir.path().join("proxy-connected"),
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let probe: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(probe["status"], "reference");
    assert_eq!(probe["tools_ok"], true);
}

#[cfg(unix)]
#[test]
fn codex_discovery_cannot_leave_a_container_running_into_the_probe() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new(0.0, 0.0);
    let bin = s.dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let busy = s.dir.path().join("discovery-container");
    let docker = bin.join("docker");
    std::fs::write(&docker, r#"#!/usr/bin/env python3
import sys,json,pathlib,os
args=sys.argv[1:];busy=pathlib.Path(os.environ['BENCH_TEST_BUSY'])
if args[0]=='rm':
    if busy.exists():
        busy.write_text('removing');sys.exit(1)
elif args[0]=='ps':
    if busy.exists():
        print('discovery');busy.unlink()
elif args[0]=='version': print('29.0.0')
elif args[0]=='info': print('[]')
elif args[:2]==['image','inspect']: print('sha256:'+'0'*64)
elif args[:2]==['network','inspect']:
    if args[2]=='bridge': sys.exit('must inspect the task network gateway')
    else: print(json.dumps([{'Internal':True,'IPAM':{'Config':[{'Gateway':'172.31.0.1'}]},'Options':{'com.docker.network.bridge.gateway_mode_ipv4':'isolated'}}]))
elif args[0]=='run':
    if args[-1].startswith('import socket,errno'): assert '172.31.0.1' in args[-1]
    if busy.exists(): sys.exit(125)
    if '--version' in args: print('test')
    elif 'app-server' in args:
        busy.write_text('running')
        for line in sys.stdin:
            request=json.loads(line)
            if 'id' not in request: continue
            result={} if request['method']=='initialize' else {'data':[{'model':'gpt-6-luna','supportedReasoningEfforts':[{'reasoningEffort':'low'}]}]}
            print(json.dumps({'id':request['id'],'result':result}),flush=True)
    elif '-m' in args:
        workspace=next(a.split('src=')[1].split(',')[0] for a in args if 'dst=/workspace' in a)
        pathlib.Path(workspace,'probe.txt').write_text('cedr-bench-probe')
        pathlib.Path(workspace,'command.txt').write_text('cedr-bench-command')
        print(json.dumps({'type':'turn.completed','usage':{'input_tokens':100,'cached_input_tokens':0,'cache_write_input_tokens':0,'output_tokens':1}}))
"#).unwrap();
    std::fs::set_permissions(docker, std::fs::Permissions::from_mode(0o755)).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 17\n").unwrap();
    std::fs::set_permissions(codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let out = s
        .command(&[
            "bench",
            "probe",
            "--harness",
            "codex",
            "--model",
            "gpt-6-luna",
            "--agent-version",
            "test",
            "--effort",
            "low",
            "--suite",
            &suite(),
            "--set-reference",
            "--format",
            "json",
        ])
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("BENCH_TEST_BUSY", &busy)
        .env("OPENAI_API_KEY", "fake-test-key")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!busy.exists());
    let probe: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(probe["tools_ok"], true);
}
