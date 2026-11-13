use cedr_bench::{
    adapter::Harness,
    container,
    report::*,
    run::{self, Backend, Invocation, Options},
    suite::{self, Kind, Suite, Task},
};
use std::path::{Path, PathBuf};
use std::time::Duration;
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bench/suite")
}
fn options() -> Options {
    Options {
        harness: Harness::Command,
        model: "stub".into(),
        effort: None,
        version: "test".into(),
        local: true,
        build_image: false,
        set_reference: true,
        stop_usd: None,
        allow_unpriced: false,
        tasks: vec![],
        command: vec!["stub".into()],
        billing: "plan".into(),
    }
}
struct Stub {
    calls: usize,
    unknown: bool,
    unknown_at: Option<usize>,
}
impl Backend for Stub {
    fn invoke(&mut self, _: &str, w: &Path, _: Duration) -> Result<Invocation, String> {
        if self.calls < 2 {
            assert!(!w.join(".git").exists());
        } else {
            let count = std::process::Command::new("git")
                .arg("-C")
                .arg(w)
                .args(["rev-list", "--count", "HEAD"])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1");
        }
        std::fs::write(w.join("probe.txt"), b"cedr-bench-probe\n").unwrap();
        std::fs::write(w.join("command.txt"), b"cedr-bench-command\n").unwrap();
        self.calls += 1;
        if self.calls > 2 {
            let config = w.join(".git/config");
            std::fs::write(config, "[core]\nfsmonitor = touch ../host-helper-ran\n").unwrap();
            std::fs::write(w.join("answer.txt"), "fixed\n").unwrap();
        }

        Ok(Invocation {
            status: Status::Graded,
            usage: Usage {
                tokens: Tokens {
                    input_tokens: Some(100),
                    ..Tokens::default()
                },
                cost_usd: if self.unknown || self.unknown_at == Some(self.calls) {
                    None
                } else {
                    Some(0.3)
                },
                ..Usage::default()
            },
            duration_ms: 1,
        })
    }
    fn grade(&mut self, _: &Suite, _t: &Task, w: &Path) -> Result<Vec<u8>, String> {
        assert_eq!(
            std::fs::read_to_string(w.join("answer.txt")).unwrap(),
            "fixed\n"
        );
        assert!(!w.parent().unwrap().join("host-helper-ran").exists());
        Ok(std::fs::read(fixture().join("../grade.json")).unwrap())
    }
    fn environment(&self) -> Environment {
        Environment {
            mode: "local".into(),
            clean_home: true,
            network: "host".into(),
            pinned: false,
            agent_image: None,
            os: "linux".into(),
            arch: "x86_64".into(),
            cli: "test".into(),
        }
    }
}
fn measure(o: &Options, unknown: bool) -> BenchResult {
    let s = suite::load(&fixture()).unwrap();
    let a = tempfile::tempdir().unwrap();
    let mut b = Stub {
        calls: 0,
        unknown,
        unknown_at: None,
    };
    run::run(&s, o, &mut b, a.path(), |_| {}).unwrap()
}
#[test]
fn suite_digest_detects_edits_and_invalid_metadata() {
    let s = suite::load(&fixture()).unwrap();
    let d = tempfile::tempdir().unwrap();
    suite::copy(&fixture(), d.path()).unwrap();
    suite::verify(d.path(), &s.digest).unwrap();
    std::fs::write(d.path().join("tasks/fix/repo/answer.txt"), "changed").unwrap();
    assert!(suite::verify(d.path(), &s.digest).is_err());
    std::fs::write(
        d.path().join("tasks/fix/task.toml"),
        "id='fix'\nkind='code'\ntimeout_seconds=10\nlanguage='text'\nunknown=true\n",
    )
    .unwrap();
    assert!(suite::load(d.path()).unwrap_err().contains("unknown"));
}
#[cfg(unix)]
#[test]
fn suite_refuses_symlinks() {
    let d = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("/etc", d.path().join("outside")).unwrap();
    assert!(suite::digest(d.path()).is_err());
}
#[test]
fn snapshots_preserve_empty_starting_repositories() {
    let d = tempfile::tempdir().unwrap();
    suite::copy(&fixture(), d.path()).unwrap();
    std::fs::remove_file(d.path().join("tasks/fix/repo/answer.txt")).unwrap();
    let s = suite::load(d.path()).unwrap();
    assert!(s.root.join("tasks/fix/repo").is_dir());
    let artifacts = tempfile::tempdir().unwrap();
    let mut backend = Stub {
        calls: 0,
        unknown: false,
        unknown_at: None,
    };
    assert!(
        run::run(&s, &options(), &mut backend, artifacts.path(), |_| {})
            .unwrap()
            .complete
    );
}
#[test]
fn ordered_scores_goal_stop_and_unknown_cost() {
    let r = measure(&options(), false);
    assert!(r.complete);
    assert_eq!(r.score, 75.0);
    assert_eq!(r.solved, 0);
    assert!(r.over_goal);
    assert_eq!(r.tasks.len(), 2);
    let mut o = options();
    o.stop_usd = Some(0.7);
    let r = measure(&o, false);
    assert_eq!(r.stop, Some(Stop::UserStop));
    assert_eq!(r.score, 37.5);
    assert_eq!(r.tasks[1].status, Status::NotReached);
    let r = measure(&options(), true);
    assert_eq!(r.stop, Some(Stop::UnknownCost));
    assert!(r.tasks.iter().all(|t| t.status == Status::NotReached));
    let mut o = options();
    o.allow_unpriced = true;
    let r = measure(&o, true);
    assert!(r.complete);
    assert_eq!(r.unpriced_trials, 4);
    o.tasks = vec!["second".into()];
    let r = measure(&o, false);
    assert!(!r.complete);
    assert_eq!(r.score, 37.5);
    assert!(!r.eligible);
}
#[test]
fn show_refuses_tampered_summary_and_round_trips() {
    let r = measure(&options(), false);
    let mut json = serde_json::to_value(&r).unwrap();
    let bytes = serde_json::to_vec(&r).unwrap();
    parse(&bytes).unwrap();
    json["score"] = serde_json::json!(99);
    assert!(parse(&serde_json::to_vec(&json).unwrap()).is_err());
}
#[test]
fn malformed_grader_never_passes() {
    for s in [
        "{}",
        "{bad}",
        r#"{"checks":[{"id":"a","passed":"yes"}]}"#,
        r#"{"checks":[{"id":"a","passed":true},{"id":"a","passed":true},{"id":"c","passed":true},{"id":"d","passed":true}]}"#,
    ] {
        assert!(grade(s.as_bytes(), Kind::Code).is_err());
    }
}
#[test]
fn probe_tolerance_requires_the_file() {
    let u = Usage {
        tokens: Tokens {
            input_tokens: Some(100),
            cache_read_tokens: Some(20),
            ..Tokens::default()
        },
        ..Usage::default()
    };
    assert_eq!(probe(&u, Some(136), false, true).status, ProbeStatus::Match);
    assert_eq!(
        probe(&u, Some(137), false, true).status,
        ProbeStatus::Mismatch
    );
    assert_eq!(
        probe(&u, None, false, true).status,
        ProbeStatus::NoReference
    );
    assert_eq!(probe(&u, None, true, true).status, ProbeStatus::Reference);
    assert_eq!(
        probe(&u, Some(120), false, false).status,
        ProbeStatus::Match
    );
}
#[test]
fn egress_arguments_have_no_direct_route() {
    assert!(container::allowed(Harness::Claude, "api.anthropic.com:443"));
    for h in [
        "example.com:443",
        "api.anthropic.com:80",
        "api.anthropic.com.evil:443",
        "127.0.0.1:443",
    ] {
        assert!(!container::allowed(Harness::Claude, h));
    }
    let network = container::network_args("internal-test");
    assert!(network.iter().any(|a| a == "--internal"));
    assert!(
        network
            .iter()
            .any(|a| a == "com.docker.network.bridge.gateway_mode_ipv4=isolated")
    );
    let args = container::task_args(
        "internal-test",
        "agent",
        Path::new("/work"),
        Path::new("/home"),
        "task",
        None,
        Harness::Claude,
    );
    assert!(args.windows(2).any(|a| a == ["--network", "internal-test"]));
    assert!(
        !args
            .iter()
            .any(|a| a.contains("host-gateway") || a.contains("docker.sock") || a == "host")
    );
    assert!(args.contains(&"--read-only".into()));
    assert!(args.contains(&"--cap-drop=ALL".into()));
}

#[test]
fn vendor_fixtures_normalize_tokens_without_double_counting() {
    use cedr_bench::adapter::{self, Harness::*};
    let root = fixture().join("../usage");
    for (h, file, model, input, cached, output, reason) in [
        (
            Claude,
            "claude.json",
            "claude-haiku-5-5",
            1191,
            Some(47223),
            137,
            None,
        ),
        (
            Codex,
            "codex.jsonl",
            "gpt-6-luna",
            5131,
            Some(36736),
            110,
            Some(0),
        ),
        (
            Muse,
            "muse.jsonl",
            "muse-spark-1.3",
            63840,
            Some(31473),
            234,
            Some(55),
        ),
        (
            Antigravity,
            "antigravity.jsonl",
            "gemini-3.8-flash-high",
            36513,
            None,
            915,
            Some(224),
        ),
        (
            Grok,
            "grok.jsonl",
            "grok-4.7",
            6468,
            Some(22784),
            285,
            Some(155),
        ),
        (
            Gemini,
            "gemini.json",
            "gemini-2.5-pro",
            3676,
            Some(21263),
            174,
            Some(154),
        ),
        (Command, "command.json", "custom", 50, None, 10, None),
    ] {
        let u = adapter::parse(h, &std::fs::read(root.join(file)).unwrap(), model);
        assert_eq!(u.tokens.input_tokens, Some(input), "{h:?}");
        assert_eq!(u.tokens.cache_read_tokens, cached, "{h:?}");
        assert_eq!(u.tokens.output_tokens, Some(output), "{h:?}");
        assert_eq!(u.tokens.reasoning_tokens, reason, "{h:?}");
        if h == Muse {
            assert_eq!(u.tokens.cache_write_tokens, None);
        }
    }
    let u = adapter::parse(Muse, b"{}", "muse-spark-1.3");
    assert_eq!(u.tokens, Tokens::default());
    assert_eq!(u.cost_usd, None);
    let u=adapter::parse(Codex,b"{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":2,\"output_tokens\":1}}","gpt-6-luna");
    assert_eq!(u.tokens.input_tokens, None);
    assert_eq!(u.cost_usd, None);
}

#[cfg(unix)]
#[test]
fn actual_proxy_policy_is_connect_only_and_exact_host() {
    let script = include_str!("../src/proxy.py");
    let check = format!(
        "import os; os.environ['BENCH_HOSTS']='api.anthropic.com'; ns={{'__name__':'test'}}; exec({script:?},ns); allowed=ns['allowed']; assert allowed('api.anthropic.com:443'); assert not allowed('example.com:443'); assert not allowed('api.anthropic.com.evil:443'); assert not allowed('api.anthropic.com:80')"
    );
    let status = std::process::Command::new("python3")
        .args(["-c", &check])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn split_probe_keeps_known_spend_when_readiness_is_unpriced() {
    let s = suite::load(&fixture()).unwrap();
    let a = tempfile::tempdir().unwrap();
    let mut b = Stub {
        calls: 0,
        unknown: false,
        unknown_at: Some(2),
    };
    let r = run::run(&s, &options(), &mut b, a.path(), |_| {}).unwrap();
    assert_eq!(r.stop, Some(Stop::UnknownCost));
    assert_eq!(r.spent_usd, 0.3);
    assert_eq!(r.unpriced_trials, 1);
    assert_eq!(r.probe.input_tokens, Some(100));
    assert!(r.probe.tools_ok);
}

#[test]
fn visual_grader_requires_unique_gates_and_relative_artifacts() {
    let g = grade(
        br#"{"gates":[{"id":"file","passed":true,"detail":"static"}],"artifacts":["shot.png"]}"#,
        Kind::Visual,
    )
    .unwrap();
    assert!(g.checks.is_empty());
    assert_eq!(g.artifacts, ["shot.png"]);
    for bad in [
        br#"{"gates":[{"id":"file","passed":true},{"id":"file","passed":true}]}"#.as_slice(),
        br#"{"gates":[{"id":"file","passed":true}],"artifacts":["../outside.png"]}"#.as_slice(),
    ] {
        assert!(grade(bad, Kind::Visual).is_err());
    }
}

#[test]
fn malformed_grader_diagnostics_do_not_echo_output_values() {
    let error = grade(
        br#"{"checks":[{"id":"case","passed":"private-output-marker"}]}"#,
        Kind::Code,
    )
    .err()
    .unwrap();
    assert!(error.contains("malformed grader output"));
    assert!(!error.contains("private-output-marker"));
}

#[test]
fn version_checks_accept_the_full_muse_build_revision() {
    assert!(container::version_matches(
        "Muse Code 1.4.18 (1.4.18-R8587.1)",
        "1.4.18-R8587.1"
    ));
    assert!(!container::version_matches(
        "Muse Code 1.4.18 (1.4.18-R8587.1)",
        "1.4.18-R8587.2"
    ));
    assert!(container::version_matches("codex-cli 0.167.2", "0.167.2"));
}

#[test]
fn suite_image_build_uses_the_suite_root_and_its_dockerfile() {
    let args = container::build_args(Path::new("/suite-snapshot"), "base:test").unwrap();
    assert_eq!(args.last().unwrap(), "/suite-snapshot");
    assert!(
        args.windows(2)
            .any(|a| a == ["--file", "/suite-snapshot/image/Dockerfile"])
    );
}

#[test]
fn claude_cost_uses_cache_lifetimes_and_request_prompt_tiers() {
    use cedr_bench::adapter::{self, Harness};
    let request = serde_json::json!({"type":"assistant", "message": {
        "id":"message-one", "model":"claude-haiku-5-5", "usage": {
            "input_tokens":10, "cache_read_input_tokens":90000,
            "cache_creation_input_tokens":10000, "output_tokens":100,
            "cache_creation":{"ephemeral_1h_input_tokens":8000}
        }
    }});
    let result = serde_json::json!({"type":"result", "total_cost_usd":999,
        "modelUsage":{"claude-haiku-5-5":{
            "inputTokens":20,"cacheReadInputTokens":90000,"cacheCreationInputTokens":10000,
            "cacheCreationEphemeral1hInputTokens":8000,"outputTokens":110,"thinkingTokens":50
        }}
    });
    // A content block repeated for one message must not apply the premium twice.
    let text = format!("{request}\n{request}\n{result}\n");
    let usage = adapter::parse(Harness::Claude, text.as_bytes(), "claude-haiku-5-5");
    let request_cost =
        (10.0 * 0.1 + 90000.0 * 0.01 + 2000.0 * 0.125 + 8000.0 * 0.2 + 100.0 * 0.5) / 1e6;
    let expected = request_cost * 5.0 + (10.0 * 0.1 + 10.0 * 0.5) / 1e6;
    assert!((usage.cost_usd.unwrap() - expected).abs() < 1e-12);
    assert_eq!(usage.tokens.reasoning_tokens, Some(50));
    assert_eq!(
        adapter::parse(
            Harness::Claude,
            result.to_string().as_bytes(),
            "claude-haiku-5-5"
        )
        .cost_usd,
        None
    );
    let mut boundary = request;
    boundary["message"]["usage"]["input_tokens"] = 0.into();
    let mut total = result;
    total["modelUsage"]["claude-haiku-5-5"]["inputTokens"] = 0.into();
    let usage = adapter::parse(
        Harness::Claude,
        format!("{boundary}\n{total}\n").as_bytes(),
        "claude-haiku-5-5",
    );
    assert!((usage.cost_usd.unwrap() - (900.0 + 250.0 + 1600.0 + 55.0) / 1e6).abs() < 1e-12);
}

#[cfg(unix)]
#[test]
fn claude_install_overrides_an_inherited_non_root_home() {
    // Execute the generated installer shell with offline stand-ins for its I/O.
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("curl"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(
        temp.path().join("bash"),
        "#!/bin/sh\nprintf '%s' \"$HOME\" > \"$INSTALL_HOME_RECORD\"\n",
    )
    .unwrap();
    std::fs::write(temp.path().join("cp"), "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    for name in ["curl", "bash", "cp"] {
        std::fs::set_permissions(
            temp.path().join(name),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let record = temp.path().join("installer-home");
    let install = container::claude_install("1.2.3");
    let status = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(install.strip_prefix("RUN ").unwrap())
        .env("PATH", temp.path())
        .env("HOME", "/home/bench")
        .env("INSTALL_HOME_RECORD", &record)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(record).unwrap(), "/root");
}
