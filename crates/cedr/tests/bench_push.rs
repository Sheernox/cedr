//! Explicit uploads use only a loopback server and scratch state; no agent invocation is needed.
mod common;
use common::{Scratch, stdout};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;

const ID: &str = "b-20261113T000000Z-abcdef";

fn saved(s: &Scratch, url: &str) -> String {
    let root = s.dir.path().join("state/cedr/bench");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bench/result.json"),
        root.join(format!("{ID}.json")),
    )
    .unwrap();
    let config = s.dir.path().join("push.toml");
    std::fs::write(&config,format!("version = 1\n[machine]\nname = 'fixture-machine'\n[sync]\nurl = '{url}'\norg = 'org-000000000001'\n")).unwrap();
    config.display().to_string()
}

#[test]
fn dry_run_omits_all_local_evidence_and_does_not_mark_results_sent() {
    let s = Scratch::new(0.0, 0.0);
    let config = saved(&s, "http://127.0.0.1:1/v1");
    let args = [
        "bench",
        "push",
        "--config",
        &config,
        "--dry-run",
        "--format",
        "json",
    ];
    let first = s.cedr(&args);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let body: Value = serde_json::from_str(&stdout(&first)).unwrap();
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["result"]["config"]["model"], "[local model]");
    assert_eq!(
        body[0]["result"]["config"]["resolved_models"],
        json!([
            "[local model]",
            "[local model]",
            "[local model]",
            "[local model]",
            "[local model]",
            "[local model]",
            "[local model]",
            "[local model]",
            "[local model]",
            "[local model]",
            "[local model]",
            "[local model]",
            "provider/model",
            "a:mini"
        ])
    );
    assert_eq!(
        body[0]["result"]["tasks"][0]["checks"][0],
        json!({"id":"case","passed":true})
    );
    assert_eq!(
        body[0]["result"]["tasks"][0]["tamper_signals"],
        json!([{"kind":"process_introspection"}])
    );
    assert_eq!(
        body[0]["result"]["tasks"][1]["gates"],
        json!([{"id":"file","passed":true}])
    );
    let text = stdout(&first);
    for forbidden in [
        "local-only",
        "artifacts",
        "patch_sha256",
        "eligible",
        "seed",
        "detail",
        "note",
        "error",
    ] {
        assert!(!text.contains(forbidden), "leaked {forbidden}");
    }
    assert_eq!(stdout(&s.cedr(&args)), text);
    let path = s.dir.path().join(format!("state/cedr/bench/{ID}.json"));
    assert_eq!(
        s.cedr(&[
            "bench",
            "push",
            path.to_str().unwrap(),
            "--config",
            &config,
            "--dry-run",
        ])
        .status
        .code(),
        Some(2)
    );
    let explicit = s.cedr(&[
        "bench",
        "push",
        "../../missing",
        "--config",
        &config,
        "--dry-run",
        "--format",
        "json",
    ]);
    assert_eq!(explicit.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&explicit.stderr).contains("local-only"));

    let path = s.dir.path().join(format!("state/cedr/bench/{ID}.json"));
    let mut large: cedr_bench::report::BenchResult =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    large.id = "b-20261113T000000Z-ffffee".into();
    let task = large.tasks[0].clone();
    large.tasks = (0..257)
        .map(|i| {
            let mut t = task.clone();
            t.id = format!("task-{i}");
            t
        })
        .collect();
    large.summarize();
    std::fs::write(
        s.dir
            .path()
            .join(format!("state/cedr/bench/{}.json", large.id)),
        serde_json::to_vec(&large).unwrap(),
    )
    .unwrap();
    assert_eq!(s.cedr(&args).status.code(), Some(2));
    let selected = s.cedr(&["bench", "push", ID, "--config", &config, "--dry-run"]);
    assert!(selected.status.success());
    assert_eq!(stdout(&selected), text);
}

#[test]
fn outgoing_unicode_text_fits_the_servers_utf16_limits_without_changing_local_results() {
    let s = Scratch::new(0.0, 0.0);
    let config = saved(&s, "http://127.0.0.1:1/v1");
    let path = s.dir.path().join(format!("state/cedr/bench/{ID}.json"));
    let mut r: cedr_bench::report::BenchResult =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    r.config.model = "🧪".repeat(200);
    r.config.resolved_models = vec![r.config.model.clone()];
    r.config.harness_version = "🧪".repeat(300);
    r.config.effort = Some("🧪".repeat(100));
    r.environment.os = "🧪".repeat(50);
    std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
    let settings = std::fs::read_to_string(&config)
        .unwrap()
        .replace("fixture-machine", &"🧪".repeat(64));
    std::fs::write(&config, settings).unwrap();
    let result = s.cedr(&["bench", "push", ID, "--config", &config, "--dry-run"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bodies: Value = serde_json::from_str(&stdout(&result)).unwrap();
    let body = &bodies[0];
    assert_eq!(body["machine"]["name"], "🧪".repeat(64));
    assert_eq!(body["result"]["config"]["model"], "🧪".repeat(128));
    assert_eq!(
        body["result"]["config"]["resolved_models"],
        json!(["🧪".repeat(128)])
    );
    assert_eq!(
        body["result"]["config"]["harness_version"],
        "🧪".repeat(256)
    );
    assert_eq!(body["result"]["config"]["effort"], "🧪".repeat(64));
    assert_eq!(body["result"]["environment"]["os"], "🧪".repeat(32));
    let local: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(local["config"]["model"], "🧪".repeat(200));
}

#[test]
fn pushing_recorded_measurements_does_not_require_retained_artifact_files() {
    let s = Scratch::new(0.0, 0.0);
    let config = saved(&s, "http://127.0.0.1:1/v1");
    let path = s.dir.path().join(format!("state/cedr/bench/{ID}.json"));
    let mut r: cedr_bench::report::BenchResult =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    r.config.harness = "codex".into();
    r.config.model = "FILE:///local-only/model".into();
    r.environment.mode = "container".into();
    r.environment.pinned = true;
    r.environment.clean_home = true;
    r.environment.network = "allowlist".into();
    r.probe.status = cedr_bench::report::ProbeStatus::Match;
    r.probe.reference = Some(100);
    r.tamper_review.as_mut().unwrap().verdict = cedr_bench::report::TamperVerdict::Clean;
    r.tasks[0].patch_sha256 = Some("a".repeat(64));
    r.summarize();
    assert!(r.eligible);
    std::fs::write(path, serde_json::to_vec(&r).unwrap()).unwrap();
    assert_eq!(s.cedr(&["bench", "show", ID]).status.code(), Some(2));
    let pushed = s.cedr(&["bench", "push", ID, "--config", &config, "--dry-run"]);
    assert!(
        pushed.status.success(),
        "{}",
        String::from_utf8_lossy(&pushed.stderr)
    );
    let body: Value = serde_json::from_str(&stdout(&pushed)).unwrap();
    assert_eq!(body[0]["result"]["config"]["model"], "[local model]");
}

/// Read a complete HTTP request, including a body which may arrive after the headers.
fn request(stream: &mut std::net::TcpStream) -> Value {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    loop {
        let mut buf = [0; 4096];
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            assert!(headers.starts_with("POST /v1/bench/runs HTTP/1.1"));
            let length = headers
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|n| n.parse::<usize>().ok())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}

#[test]
fn only_matching_replies_mark_sent_per_org_and_explicit_ids_resend() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for (index, org) in [
            "org-000000000001",
            "org-000000000001",
            "org-000000000001",
            "org-000000000001",
            "org-000000000002",
            "org-000000000003",
        ]
        .iter()
        .enumerate()
        {
            let (mut stream, _) = listener.accept().unwrap();
            let body = request(&mut stream);
            assert_eq!(body["org"], *org);
            let mut reply = json!({"ok":true,"id":if index==0{"b-wrong"}else{ID},"duplicate":index==3,"retained":index!=5});
            if index == 1 {
                reply.as_object_mut().unwrap().remove("retained");
            }
            let reply = reply.to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",reply.len(),reply).unwrap();
        }
    });
    let s = Scratch::new(0.0, 0.0);
    let config = saved(&s, &url);
    let invoke = |extra: &[&str]| {
        let mut args = vec!["bench", "push", "--config", &config, "--format", "json"];
        args.extend_from_slice(extra);
        s.command(&args)
            .env("CEDR_API_KEY", "test-only")
            .output()
            .unwrap()
    };
    let bad = invoke(&[]);
    assert_eq!(bad.status.code(), Some(2));
    let incomplete_receipt = invoke(&[]);
    assert_eq!(incomplete_receipt.status.code(), Some(2));
    let good = invoke(&[]);
    assert!(
        good.status.success(),
        "{}",
        String::from_utf8_lossy(&good.stderr)
    );
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&good)).unwrap()["pushed"],
        json!([ID])
    );
    let none = invoke(&[]);
    assert!(none.status.success());
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&none)).unwrap()["pushed"],
        json!([])
    );
    let duplicate = invoke(&[ID]);
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&duplicate)).unwrap()["duplicates"],
        json!([ID])
    );
    assert!(invoke(&["--org", "org-000000000002"]).status.success());
    let not_retained = invoke(&["--org", "org-000000000003"]);
    assert!(not_retained.status.success());
    let reply: Value = serde_json::from_str(&stdout(&not_retained)).unwrap();
    assert_eq!(reply["pushed"], json!([]));
    assert_eq!(reply["not_retained"], json!([ID]));
    assert!(String::from_utf8_lossy(&not_retained.stderr).contains("not retained"));
    assert!(invoke(&["--org", "org-000000000003"]).status.success());
    server.join().unwrap();
}
