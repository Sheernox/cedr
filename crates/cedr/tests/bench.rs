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
fn probe_and_usage_errors_have_stable_json_and_exit_codes() {
    let s = Scratch::new(0.0, 0.0);
    let script = "import pathlib,json;pathlib.Path('probe.txt').write_text('cedr-bench-probe\\n');pathlib.Path('command.txt').write_text('cedr-bench-command\\n');print(json.dumps({'input_tokens':100,'cache_read_tokens':0,'cache_write_tokens':0,'output_tokens':1,'cost_usd':0}))";
    let o = s.cedr(&[
        "bench",
        "probe",
        "--harness",
        "command",
        "--model",
        "stub",
        "--suite",
        &suite(),
        "--local",
        "--command",
        "python3",
        "--command",
        "-c",
        "--command",
        script,
        "--format",
        "json",
    ]);
    assert_eq!(
        o.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let p: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(p["tools_ok"], true);
    assert_eq!(p["input_tokens"], 100);
    assert_eq!(p["status"], "no_reference");
    let o = s.cedr(&[
        "bench",
        "run",
        "--harness",
        "command",
        "--model",
        "stub",
        "--suite",
        &suite(),
        "--local",
        "--stop-at-usd",
        "-1",
    ]);
    assert_eq!(o.status.code(), Some(2));
    let o = s.cedr(&["bench", "history", "--format", "json"]);
    assert!(o.status.success());
    assert_eq!(stdout(&o).trim(), "[]");
    let o = s.cedr(&["bench", "show", "../../missing", "--format", "json"]);
    assert_eq!(o.status.code(), Some(2));
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
        "--local",
        "--command",
        "false",
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
    r.summarize();
    let root = s.dir.path().join("state/cedr/bench");
    let source = root.join(id).join("artifacts/visual");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("screen.png"), png).unwrap();
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
    assert!(
        !out.join("shots")
            .join(id)
            .join("code/submission.patch")
            .exists()
    );
    let records: Value =
        serde_json::from_slice(&std::fs::read(out.join("results.json")).unwrap()).unwrap();
    assert_eq!(records, serde_json::json!([value]));
    assert_eq!(
        std::fs::read(out.join("shots").join(id).join("visual/screen.png")).unwrap(),
        png
    );
    let refused = s.cedr(&["bench", "review", id, "--clean", "--note", "Read patches"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("no tamper signals"));
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
    let shown = stdout(&s.cedr(&["bench", "show", id]));
    assert!(shown.contains("tamper review Clean"));
    assert!(!shown.contains("then: cedr bench review"));
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
        child.kill().unwrap();
        child.wait().unwrap();
        let body: Value =
            serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["tamper_review"], exported[0]["tamper_review"]);
        assert_eq!(
            body["tasks"][0]["tamper_signals"],
            exported[0]["tasks"][0]["tamper_signals"]
        );
    }
    r.score = 99.0;
    std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
    let shown = s.cedr(&["bench", "show", id]);
    assert_eq!(shown.status.code(), Some(2));
}

#[test]
fn model_specific_efforts_are_refused_before_any_agent_or_docker() {
    let s = Scratch::new(0.0, 0.0);
    let home = s.dir.path().join("codex");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("models_cache.json"),
        r#"{"models":[{"slug":"stub","supported_reasoning_levels":[{"effort":"low"}]}]}"#,
    )
    .unwrap();
    let out = s
        .command(&[
            "bench",
            "probe",
            "--harness",
            "codex",
            "--model",
            "stub",
            "--effort",
            "max",
            "--agent-version",
            "test",
            "--suite",
            &suite(),
            "--local",
        ])
        .env("CODEX_HOME", home)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not offer effort max"));
}

#[cfg(unix)]
#[test]
fn native_local_agent_state_never_reaches_the_host_data_directory() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new(0.0, 0.0);
    let host = s.dir.path().join("host");
    let host_data = host.join(".local/share");
    std::fs::create_dir_all(&host_data).unwrap();
    std::fs::write(host_data.join("history.txt"), "host").unwrap();
    let bin = s.dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let program = bin.join("codex");
    std::fs::create_dir_all(host.join(".codex")).unwrap();
    std::fs::write(host.join(".codex/auth.json"), r#"{"token":"old"}"#).unwrap();
    std::fs::write(&program,r#"#!/usr/bin/env python3
import json, os, pathlib, sys
if "--version" in sys.argv:
    print("test");sys.exit(0)
if sys.argv[1:3] == ['login','status']:
    assert not pathlib.Path(os.environ['HOME'],'.cargo').exists()
    sys.exit(0)
auth=pathlib.Path(os.environ['CODEX_HOME'],'auth.json')
auth.with_suffix('.new').write_text('{"token":"renewed"}')
auth.with_suffix('.new').replace(auth)
home=pathlib.Path(os.environ['HOME'])
data=pathlib.Path(os.environ.get('XDG_DATA_HOME',home/'.local/share'))
data.mkdir(parents=True,exist_ok=True)
(data/'history.txt').write_text('trial')
pathlib.Path('probe.txt').write_text('cedr-bench-probe\n')
pathlib.Path('command.txt').write_text('cedr-bench-command\n')
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':100,'cached_input_tokens':0,'output_tokens':1}}))
"#).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let output = s
        .command(&[
            "bench",
            "probe",
            "--harness",
            "codex",
            "--model",
            "gpt-6-luna",
            "--agent-version",
            "test",
            "--suite",
            &suite(),
            "--local",
            "--set-reference",
            "--format",
            "json",
        ])
        .env("HOME", &host)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("OPENAI_API_KEY", "test-only")
        .env_remove("CODEX_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(host.join(".codex/auth.json")).unwrap(),
        r#"{"token":"renewed"}"#
    );
    assert_eq!(
        std::fs::read_to_string(host_data.join("history.txt")).unwrap(),
        "host"
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_cannot_satisfy_the_probe_readiness_check() {
    let s = Scratch::new(0.0, 0.0);
    let program = "import pathlib,json;p=pathlib.Path;p('target').write_text('cedr-bench-probe\\n');p('probe.txt').symlink_to('target');p('command.txt').write_text('cedr-bench-command\\n');print(json.dumps({'input_tokens':100,'cache_read_tokens':0,'cache_write_tokens':0,'output_tokens':1,'cost_usd':0}))";
    let output = s.cedr(&[
        "bench",
        "probe",
        "--harness",
        "command",
        "--model",
        "stub",
        "--suite",
        &suite(),
        "--local",
        "--set-reference",
        "--command",
        "python3",
        "--command",
        "-c",
        "--command",
        program,
        "--format",
        "json",
    ]);
    assert_eq!(output.status.code(), Some(3));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["tools_ok"], false);
}

#[cfg(unix)]
#[test]
fn interrupt_is_saved_when_the_native_postflight_check_is_cancelled() {
    use std::{
        io::Read,
        os::unix::{fs::PermissionsExt, net::UnixListener},
        process::Stdio,
        sync::mpsc,
        time::Duration,
    };
    let s = Scratch::new(0.0, 0.0);
    let host = s.dir.path().join("host");
    let auth = host.join(".codex");
    std::fs::create_dir_all(&auth).unwrap();
    std::fs::write(auth.join("auth.json"), "{}").unwrap();
    let socket = s.dir.path().join("ready.sock");
    let marker = s.dir.path().join("started");
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = [0; 5];
        stream.read_exact(&mut bytes).unwrap();
        tx.send(()).unwrap();
    });
    let program=r#"#!/usr/bin/env python3
import json, pathlib, socket, threading, sys
if "--version" in sys.argv:
    print("test");sys.exit(0)
marker=pathlib.Path(MARKER)
if pathlib.Path('.git').exists():
    marker.touch()
    s=socket.socket(socket.AF_UNIX);s.connect(SOCKET);s.sendall(b'ready');s.close()
    threading.Event().wait()
if marker.exists():
    # Model an authentication request pending past the process runner's interrupt poll.
    threading.Event().wait(1)
pathlib.Path('probe.txt').write_text('cedr-bench-probe\n')
pathlib.Path('command.txt').write_text('cedr-bench-command\n')
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':100,'cached_input_tokens':0,'output_tokens':1}}))
"#.replace("MARKER",&serde_json::to_string(&marker.to_str().unwrap()).unwrap()).replace("SOCKET",&serde_json::to_string(&socket.to_str().unwrap()).unwrap());
    let bin = s.dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(bin.join("codex"), program).unwrap();
    std::fs::set_permissions(bin.join("codex"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let child = s
        .command(&[
            "bench",
            "run",
            "--harness",
            "codex",
            "--model",
            "gpt-6-luna",
            "--agent-version",
            "test",
            "--suite",
            &suite(),
            "--local",
            "--set-reference",
            "--format",
            "json",
        ])
        .env("HOME", &host)
        .env("CODEX_HOME", auth)
        .env_remove("OPENAI_API_KEY")
        .env("PATH", std::env::join_paths(paths).unwrap())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if rx.recv_timeout(Duration::from_secs(5)).is_err() {
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        let out = child.wait_with_output().unwrap();
        panic!(
            "native task did not start: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["stop"], "interrupted");
    assert_eq!(result["tasks"][0]["status"], "interrupted");
    assert_eq!(result["unpriced_trials"], 1);
    assert!(
        s.dir
            .path()
            .join("state/cedr/bench")
            .join(format!("{}.json", result["id"].as_str().unwrap()))
            .is_file()
    );
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
fn local_grading_uses_the_image_when_available_and_host_tools_otherwise() {
    use std::os::unix::fs::PermissionsExt;
    for (available, rootless, expected) in [
        (true, false, 100.0),
        (false, false, 75.0),
        (true, true, 0.0),
    ] {
        let s = Scratch::new(0.0, 0.0);
        let bin = s.dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let docker = bin.join("docker");
        std::fs::write(
            &docker,
            if available {
                r#"#!/usr/bin/env python3
import sys,json,os
args=sys.argv[1:]
if args[0]=='info': print('["name=rootless"]' if os.environ.get('BENCH_TEST_ROOTLESS') else '29.0.0')
elif args[0]=='image': print('sha256:'+'0'*64)
elif args[0]=='run':
    assert any(x.startswith('BENCH_SEED=') for x in args)
    print(json.dumps({'checks':[{'id':str(i),'passed':True} for i in range(4)]}))
"#
            } else {
                "#!/bin/sh\nexit 1\n"
            },
        )
        .unwrap();
        std::fs::set_permissions(docker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let out = s.command(&["bench","run","--harness","command","--model","stub","--suite",&suite(),"--local","--set-reference","--stop-at-usd","1","--command","python3","--command","-c","--command",
            "import pathlib,json;pathlib.Path('probe.txt').write_text('cedr-bench-probe');pathlib.Path('command.txt').write_text('cedr-bench-command');print(json.dumps(dict(input_tokens=10,cache_read_tokens=0,cache_write_tokens=0,output_tokens=1,cost_usd=0)))",
            "--format","json"])
            .env("PATH", std::env::join_paths(paths).unwrap()).env("BENCH_TEST_ROOTLESS", if rootless {"1"} else {""}).output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(if rootless { 3 } else { 0 }),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if rootless {
            assert!(String::from_utf8_lossy(&out.stderr).contains("rootless/userns UID mapping"));
        }
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["score"], expected);
        assert_eq!(result["complete"], !rootless);
    }
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
    if args[2]=='bridge': print('172.17.0.1')
    else: print(json.dumps([{'Internal':True,'Options':{'com.docker.network.bridge.gateway_mode_ipv4':'isolated'}}]))
elif args[0]=='run':
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
    if args[2]=='bridge': print('172.17.0.1')
    else: print(json.dumps([{'Internal':True,'Options':{'com.docker.network.bridge.gateway_mode_ipv4':'isolated'}}]))
elif args[0]=='run':
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
        print(json.dumps({'type':'turn.completed','usage':{'input_tokens':100,'cached_input_tokens':0,'output_tokens':1}}))
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
