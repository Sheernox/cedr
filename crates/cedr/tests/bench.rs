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
    let script = "import pathlib,json;pathlib.Path('probe.txt').write_text('cedr-bench-probe\\n');pathlib.Path('command.txt').write_text('cedr-bench-command\\n');print(json.dumps({'input_tokens':100,'output_tokens':1,'cost_usd':0}))";
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
fn show_checks_consistency_and_export_preserves_objects_and_shot_layout() {
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
    std::fs::write(&program,r#"#!/usr/bin/env python3
import json, os, pathlib, sys
if "--version" in sys.argv:
    print("test");sys.exit(0)
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
        std::fs::read_to_string(host_data.join("history.txt")).unwrap(),
        "host"
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_cannot_satisfy_the_probe_readiness_check() {
    let s = Scratch::new(0.0, 0.0);
    let program = "import pathlib,json;p=pathlib.Path;p('target').write_text('cedr-bench-probe\\n');p('probe.txt').symlink_to('target');p('command.txt').write_text('cedr-bench-command\\n');print(json.dumps({'input_tokens':100,'output_tokens':1,'cost_usd':0}))";
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
