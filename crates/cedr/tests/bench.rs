//! `cedr bench` end to end against a scratch repo, with scripts standing in for the agents: which
//! trials run, what each step can see, what the budget stops, and what the exit codes say.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Repo {
    dir: tempfile::TempDir,
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Repo {
    fn new() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo { dir };
        let root = repo.root();
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("AGENTS.md"), "base rules\n").unwrap();
        repo.commit("base");
        repo
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn commit(&self, message: &str) {
        git(&self.root(), &["add", "-A"]);
        git(&self.root(), &["commit", "-q", "-m", message]);
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    fn machine_routes(&self, text: &str) -> PathBuf {
        let path = self.dir.path().join("config/cedr/routes.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    /// A program on the fake PATH. Its body is a shell script.
    fn program(&self, name: &str, body: &str) {
        let bin = self.dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let path = bin.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn suite(&self, text: &str) -> String {
        self.write(".cedr/bench/s.toml", text).display().to_string()
    }

    /// `cedr` with scratch state, in the repo, with the fake programs first on PATH.
    fn cedr(&self, args: &[&str]) -> Command {
        let scratch = self.dir.path();
        let mut c = Command::new(env!("CARGO_BIN_EXE_cedr"));
        c.args(args)
            .current_dir(self.root())
            .env("XDG_STATE_HOME", scratch.join("state"))
            .env("XDG_CACHE_HOME", scratch.join("cache"))
            .env("XDG_CONFIG_HOME", scratch.join("config"))
            .env("XDG_DATA_HOME", scratch.join("data"))
            .env("TMPDIR", {
                let tmp = scratch.join("tmp");
                fs::create_dir_all(&tmp).unwrap();
                tmp
            })
            .env("CEDR_TELEMETRY", "0")
            .env("CEDR_NO_AUTO_UPDATE", "1")
            .env_remove("CEDR_API_KEY")
            .env_remove("CEDR_ROUTES")
            .env_remove("GITHUB_ACTIONS")
            .env_remove("GITHUB_REPOSITORY")
            .env_remove("GITHUB_SERVER_URL")
            .env_remove("GITHUB_RUN_ID")
            .env_remove("GITHUB_REF")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    scratch.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            );
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cedr(args).output().unwrap()
    }
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

fn stdout_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// A `command` agent that writes `done` when its model is "good", at $0.10 a trial.
const AGENT: &str = r#"if [ "$1" = good ]; then echo fixed > done; fi
echo '{"cost_usd": 0.1, "input_tokens": 10, "output_tokens": 5}'"#;

fn command_suite(trials: u32) -> String {
    format!(
        r#"version = 1
trials = {trials}
[baseline]
harness = "command"
model = "bad"
command = ["agent", "{{model}}"]
[candidate]
harness = "command"
model = "good"
command = ["agent", "{{model}}"]
[[tasks]]
id = "write-done"
prompt = "write the file"
test = "test -f done"
"#
    )
}

fn run_args(suite: &str) -> Vec<&str> {
    vec![
        "bench",
        "run",
        suite,
        "--allow-unpriced",
        "--format",
        "json",
    ]
}

#[test]
fn tests_decide_each_trial_and_the_report_counts_them_per_variant() {
    let repo = Repo::new();
    repo.program("agent", AGENT);
    let suite = repo.suite(&command_suite(2));
    let out = repo.run(&run_args(&suite));
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let r = stdout_json(&out);
    assert_eq!(r["complete"], true);
    assert_eq!(r["summary"]["baseline"]["passed"], 0);
    assert_eq!(r["summary"]["baseline"]["scored"], 2);
    assert_eq!(r["summary"]["candidate"]["passed"], 2);
    assert_eq!(r["pass_rate_delta"], 1.0);
    assert_eq!(r["separated"], false);
    assert_eq!(r["spent_usd"], 0.4);
    let order: Vec<&str> = r["trials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["variant"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["baseline", "candidate", "baseline", "candidate"]);
    assert_eq!(r["trials"][0]["status"], "failed");
    assert_eq!(r["trials"][0]["test_exit"], 1);
    // The trial's work stays in its clone.
    assert!(!repo.root().join("done").exists());
    let leftovers: Vec<_> = fs::read_dir(repo.dir.path().join("tmp")).unwrap().collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn fail_on_decides_the_exit_code_of_a_complete_run() {
    let repo = Repo::new();
    repo.program("agent", AGENT);
    // Swap the models: now the candidate is the one that fails.
    let suite = repo.suite(
        &command_suite(1)
            .replace("model = \"bad\"", "model = \"x\"")
            .replace("model = \"good\"", "model = \"bad\"")
            .replace("model = \"x\"", "model = \"good\""),
    );
    let mut args = run_args(&suite);
    assert_eq!(code(&repo.run(&args)), 0);
    args.extend(["--fail-on", "lower"]);
    assert_eq!(code(&repo.run(&args)), 1);
    let last = args.len() - 1;
    args[last] = "separated";
    assert_eq!(code(&repo.run(&args)), 0, "one trial cannot separate them");
}

#[test]
fn the_budget_stops_the_run_evenly_and_an_incomplete_run_never_passes() {
    let repo = Repo::new();
    repo.program("agent", AGENT);
    let suite = repo.suite(&command_suite(10));
    let mut args = run_args(&suite);
    args.extend(["--budget-usd", "0.25"]);
    let out = repo.run(&args);
    assert_eq!(code(&out), 3);
    let r = stdout_json(&out);
    assert_eq!(r["complete"], false);
    assert_eq!(r["stop"], "budget");
    // 0.1 + 0.1 spent, and a third would reach 0.3: the dearest-trial check stops it first.
    assert_eq!(r["trials"].as_array().unwrap().len(), 2);
    assert_eq!(r["summary"]["baseline"]["scored"], 1);
    assert_eq!(r["summary"]["candidate"]["scored"], 1);
    assert_eq!(r["budget_usd"], 0.25);
}

#[test]
fn a_budget_set_in_settings_applies_and_a_bad_flag_is_refused() {
    let repo = Repo::new();
    repo.program("agent", AGENT);
    let suite = repo.suite(&command_suite(10));
    assert_eq!(
        stdout_json(&repo.run(&["bench", "budget", "--format", "json"])),
        serde_json::json!({"budget_usd": 2.0, "source": "default"})
    );
    assert_eq!(code(&repo.run(&["bench", "budget", "0.15"])), 0);
    assert_eq!(
        stdout_json(&repo.run(&["bench", "budget", "--format", "json"]))["source"],
        "settings"
    );
    let r = stdout_json(&repo.run(&run_args(&suite)));
    assert_eq!(r["budget_usd"], 0.15);
    assert_eq!(r["trials"].as_array().unwrap().len(), 1);

    // The flag wins over settings; a nonsense flag or value is a usage error, not a default.
    let mut args = run_args(&suite);
    args.extend(["--budget-usd", "0.35", "--no-record"]);
    assert_eq!(stdout_json(&repo.run(&args))["budget_usd"], 0.35);
    let mut args = run_args(&suite);
    args.extend(["--budget-usd=-1"]);
    assert_eq!(code(&repo.run(&args)), 2);
    assert_eq!(code(&repo.run(&["bench", "budget", "--", "-1"])), 2);

    assert_eq!(code(&repo.run(&["bench", "budget", "--unset"])), 0);
    assert_eq!(
        stdout_json(&repo.run(&["bench", "budget", "--format", "json"]))["source"],
        "default"
    );
}

#[test]
fn unpriced_variants_need_consent_and_a_missing_harness_stops_before_any_trial() {
    let repo = Repo::new();
    repo.program("agent", AGENT);
    let suite = repo.suite(&command_suite(1));
    let out = repo.run(&["bench", "run", &suite]);
    assert_eq!(code(&out), 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("--allow-unpriced"));

    let missing = repo.suite(&command_suite(1).replace("\"agent\"", "\"no-such-agent\""));
    let out = repo.run(&run_args(&missing));
    assert_eq!(code(&out), 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("no-such-agent is not installed"));
}

#[test]
fn test_files_replace_what_the_agent_did_to_the_tests() {
    let repo = Repo::new();
    repo.program(
        "agent",
        r#"mkdir -p tests; printf 'exit 0\n' > tests/check.sh; echo '{"cost_usd": 0}'"#,
    );
    repo.write(".cedr/bench/hidden/tests/check.sh", "exit 7\n");
    let suite = repo.suite(&command_suite(1).replace(
        "test = \"test -f done\"",
        "test = \"sh tests/check.sh\"\ntest_files = \".cedr/bench/hidden\"",
    ));
    let r = stdout_json(&repo.run(&run_args(&suite)));
    assert_eq!(r["trials"][0]["status"], "failed");
    assert_eq!(r["trials"][0]["test_exit"], 7);
}

#[test]
fn test_commands_lose_secrets_the_agent_keeps_its_own_and_neither_gets_cedrs() {
    let repo = Repo::new();
    let record = repo.dir.path().join("seen");
    fs::create_dir_all(&record).unwrap();
    let dump = |file: &str| {
        format!(
            "echo \"key=${{VENDOR_API_KEY:-}} aws=${{AWS_SECRET_ACCESS_KEY:-}} cedr=${{CEDR_API_KEY:-}}\" > {}/{file}",
            record.display()
        )
    };
    repo.program(
        "agent",
        &format!("{}\necho '{{\"cost_usd\": 0}}'", dump("agent")),
    );
    let suite = repo.suite(&command_suite(1).replace(
        "test = \"test -f done\"",
        &format!("setup = '{}'\ntest = '{}'", dump("setup"), dump("test")),
    ));
    let out = repo
        .cedr(&run_args(&suite))
        .env("VENDOR_API_KEY", "vendor-secret")
        .env("AWS_SECRET_ACCESS_KEY", "aws-secret")
        .env("CEDR_API_KEY", "cedr-secret")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let seen = |file: &str| fs::read_to_string(record.join(file)).unwrap();
    assert_eq!(
        seen("agent").trim(),
        "key=vendor-secret aws=aws-secret cedr="
    );
    assert_eq!(seen("setup").trim(), "key= aws= cedr=");
    assert_eq!(seen("test").trim(), "key= aws= cedr=");
}

#[test]
fn refs_decide_what_each_variant_starts_from_and_results_record_the_setup() {
    let repo = Repo::new();
    git(&repo.root(), &["branch", "before"]);
    repo.write("AGENTS.md", "new rules\n");
    repo.write("feature.txt", "x\n");
    repo.commit("candidate change");
    repo.program(
        "agent",
        r#"echo '{"cost_usd": 0, "models": ["resolved-1"]}'"#,
    );
    let suite = repo.suite(
        &command_suite(1)
            .replace("model = \"bad\"", "model = \"m\"\nref = \"before\"")
            .replace("model = \"good\"", "model = \"m\"\nref = \"main\"")
            .replace("test -f done", "test -f feature.txt"),
    );
    let r = stdout_json(&repo.run(&run_args(&suite)));
    assert_eq!(r["trials"][0]["status"], "failed");
    assert_eq!(r["trials"][1]["status"], "passed");
    let (b, c) = (&r["variants"]["baseline"], &r["variants"]["candidate"]);
    assert_ne!(b["commit"], c["commit"]);
    assert_ne!(b["config_sha256"], c["config_sha256"], "AGENTS.md changed");
    assert_eq!(b["ref"], "before");
    assert_eq!(b["resolved_models"][0], "resolved-1");
}

#[test]
fn identical_variants_and_a_bad_ref_are_refused_before_anything_runs() {
    let repo = Repo::new();
    repo.program("agent", "touch \"$HOME/never\"");
    let same = repo.suite(&command_suite(1).replace("model = \"good\"", "model = \"bad\""));
    let out = repo.run(&run_args(&same));
    assert_eq!(code(&out), 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("the same"));

    let bad =
        repo.suite(&command_suite(1).replace("model = \"bad\"", "model = \"bad\"\nref = \"nope\""));
    let out = repo.run(&run_args(&bad));
    assert_eq!(code(&out), 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("nope"));
}

#[test]
fn plan_shows_the_work_and_starts_nothing() {
    let repo = Repo::new();
    let marker = repo.dir.path().join("ran");
    repo.program("agent", &format!("touch {}", marker.display()));
    let suite = repo.suite(&command_suite(2));
    let out = repo.run(&["bench", "plan", &suite, "--format", "json"]);
    assert_eq!(code(&out), 0);
    let plan = stdout_json(&out);
    assert_eq!(plan["total_trials"], 4);
    assert_eq!(plan["budget_usd"], 2.0);
    assert_eq!(plan["tasks"][0]["test"], "test -f done");
    assert!(
        plan["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("unpriced"))
    );
    assert!(!marker.exists());
    let text = repo.run(&["bench", "plan", &suite]);
    assert!(String::from_utf8_lossy(&text.stdout).contains("4 trials"));
    // The one suite in .cedr/bench/ is found without naming it.
    assert_eq!(code(&repo.run(&["bench", "plan"])), 0);
}

#[test]
fn init_writes_a_suite_that_plans_and_never_overwrites() {
    let repo = Repo::new();
    git(&repo.root(), &["branch", "-f", "side"]);
    let out = repo.run(&["bench", "init"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let written = repo.root().join(".cedr/bench/suite.toml");
    let starter = fs::read_to_string(&written).unwrap();
    let next = String::from_utf8_lossy(&out.stderr);
    for step in [
        "prompt and test",
        "HEAD differs from main",
        "another model",
        "cedr bench plan",
    ] {
        assert!(next.contains(step), "{next}");
    }
    // The starter's variants are the same commit on a fresh repo, so only a real change can run it.
    git(&repo.root(), &["checkout", "-q", "-b", "change"]);
    repo.write("AGENTS.md", "new rules\n");
    repo.commit("change");
    let plan = repo.run(&["bench", "plan", "--format", "json"]);
    assert_eq!(code(&plan), 0, "{}", String::from_utf8_lossy(&plan.stderr));
    assert_eq!(stdout_json(&plan)["total_trials"], 6);

    fs::write(&written, "mine").unwrap();
    assert_eq!(code(&repo.run(&["bench", "init"])), 1);
    assert_eq!(fs::read_to_string(&written).unwrap(), "mine");
    assert!(starter.contains("A suite is code"));
    assert_eq!(code(&repo.run(&["bench", "init", "--name", "../../x"])), 2);
}

#[test]
fn doctor_reports_the_cap_in_force_and_any_suite_that_would_not_plan() {
    let repo = Repo::new();
    let check = |id: &str| -> (String, String) {
        let root = repo.root().display().to_string();
        let report = stdout_json(&repo.run(&["doctor", &root, "--format", "json"]));
        let found = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("no {id} in {report}"))
            .clone();
        (
            found["state"].as_str().unwrap().to_string(),
            found["message"].as_str().unwrap().to_string(),
        )
    };
    assert_eq!(check("bench.push").0, "skip");
    repo.machine_routes("version=1\n[sync]\norg='acme'\n");
    assert_eq!(
        check("bench.push"),
        ("ok".into(), "results sent automatically to acme".into())
    );
    repo.machine_routes("version=1\n[sync]\norg='acme'\nauto=false\n");
    let disabled = check("bench.push");
    assert_eq!(disabled.0, "skip");
    assert!(disabled.1.contains("auto = false") && disabled.1.contains("acme"));
    repo.machine_routes("invalid = [");
    assert_eq!(check("bench.push").0, "warn");
    repo.machine_routes("version=1\n");
    assert_eq!(check("bench.suite").0, "skip");
    assert_eq!(
        check("bench.budget"),
        ("ok".into(), "$2.00 (default)".into())
    );
    repo.suite("version = 1\nnot_a_key = true\n");
    assert_eq!(check("bench.suite").0, "fail");
    repo.program("agent", AGENT);
    repo.suite(&command_suite(1));
    assert_eq!(check("bench.suite").0, "warn", "the suite is unpriced");
}

#[test]
fn a_saved_run_reads_back_with_the_same_report_and_exit_code() {
    let repo = Repo::new();
    repo.program("agent", AGENT);
    let suite = repo.suite(&command_suite(10));
    let mut args = run_args(&suite);
    args.extend(["--budget-usd", "0.25"]);
    let first = stdout_json(&repo.run(&args));
    let id = first["id"].as_str().unwrap();

    let shown = repo.run(&["bench", "show", id, "--format", "json"]);
    assert_eq!(code(&shown), 3, "still incomplete");
    assert_eq!(stdout_json(&shown), first);

    let rows = stdout_json(&repo.run(&["bench", "history", "--format", "json"]));
    assert_eq!(rows[0]["id"], first["id"]);
    assert_eq!(rows[0]["complete"], false);
    assert_eq!(rows[0]["baseline"]["scored"], 1);

    // A corrupt file is skipped with a note, and never hides the others or breaks the command.
    fs::write(
        repo.dir.path().join("state/cedr/bench/b-zzzz.json"),
        "{not json",
    )
    .unwrap();
    let out = repo.run(&["bench", "history", "--format", "json"]);
    assert_eq!(code(&out), 0);
    assert_eq!(stdout_json(&out)[0]["id"], first["id"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("skipped"));
    assert_eq!(code(&repo.run(&["bench", "show", "b-zzzz"])), 2);
}

#[test]
fn claude_gets_its_flags_the_prompt_on_stdin_and_what_is_left_of_the_budget() {
    let repo = Repo::new();
    let seen = repo.dir.path().join("argv");
    repo.program(
        "claude",
        &format!(
            r#"if [ "$1" = --version ]; then echo '9.9.9 (Claude Code)'; exit 0; fi
echo "$*" >> {argv}
cat >> {argv}
echo '{{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.5,"modelUsage":{{"claude-x-1":{{"inputTokens":1,"outputTokens":2}}}}}}'"#,
            argv = seen.display()
        ),
    );
    let suite = repo.suite(
        r#"version = 1
trials = 1
[baseline]
harness = "claude"
model = "sonnet"
effort = "low"
[candidate]
harness = "claude"
model = "opus"
permissions = "full"
[[tasks]]
id = "t"
prompt = "--do-the-work"
test = "true"
"#,
    );
    let out = repo.run(&[
        "bench",
        "run",
        &suite,
        "--format",
        "json",
        "--budget-usd",
        "2",
    ]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let r = stdout_json(&out);
    assert_eq!(r["spent_usd"], 1.0);
    assert_eq!(
        r["variants"]["baseline"]["harness_version"],
        "9.9.9 (Claude Code)"
    );
    assert_eq!(
        r["variants"]["baseline"]["resolved_models"][0],
        "claude-x-1"
    );
    let argv = fs::read_to_string(&seen).unwrap();
    let mut calls = argv.split("--do-the-work");
    let (first, second) = (calls.next().unwrap(), calls.next().unwrap());
    assert!(first.contains("--model sonnet"), "{first}");
    assert!(first.contains("--effort low"), "{first}");
    assert!(first.contains("--permission-mode acceptEdits"), "{first}");
    assert!(first.contains("--max-budget-usd 2.0000"), "{first}");
    assert!(
        second.contains("--permission-mode bypassPermissions"),
        "{second}"
    );
    assert!(second.contains("--max-budget-usd 1.5000"), "{second}");
}

#[test]
fn an_agent_that_errors_or_cannot_set_up_leaves_the_run_incomplete() {
    let repo = Repo::new();
    repo.program("agent", "exit 9");
    let suite = repo.suite(&command_suite(1));
    let out = repo.run(&run_args(&suite));
    assert_eq!(code(&out), 3);
    let r = stdout_json(&out);
    assert_eq!(r["trials"][0]["status"], "agent_error");
    assert_eq!(r["summary"]["baseline"]["unscored"], 1);
    assert_eq!(r["pass_rate_delta"], Value::Null);

    repo.program("agent", AGENT);
    let setup = repo.suite(&command_suite(1).replace(
        "test = \"test -f done\"",
        "setup = \"exit 1\"\ntest = \"true\"",
    ));
    let r = stdout_json(&repo.run(&run_args(&setup)));
    assert_eq!(r["trials"][0]["status"], "setup_failed");
    assert_eq!(r["spent_usd"], 0.0);
}

#[test]
fn an_agent_past_its_timeout_counts_against_it_and_does_not_stop_the_run() {
    let repo = Repo::new();
    repo.program("agent", "sleep 30");
    let suite =
        repo.suite(&command_suite(1).replace("trials = 1", "trials = 1\ntimeout_seconds = 1"));
    let out = repo.run(&run_args(&suite));
    assert_eq!(code(&out), 0, "a timeout is scored, so the run is complete");
    let r = stdout_json(&out);
    assert_eq!(r["trials"][0]["status"], "timeout");
    assert_eq!(r["summary"]["baseline"]["scored"], 1);
    assert_eq!(r["summary"]["baseline"]["passed"], 0);
    assert_eq!(r["unpriced_trials"], 2);
}

#[test]
fn the_apps_cap_is_pulled_into_settings_and_an_org_without_one_clears_it() {
    use std::io::{Read, Write};
    let repo = Repo::new();
    let serve = |reply: &'static str| {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
            request.lines().next().unwrap().to_string()
        });
        (url, task)
    };
    let pull = |url: &str| {
        let config = repo.dir.path().join("routes.toml");
        fs::write(
            &config,
            format!("version=1\n[sync]\norg='acme'\nurl={url:?}\n"),
        )
        .unwrap();
        let config = config.display().to_string();
        repo.cedr(&[
            "bench", "budget", "--pull", "--config", &config, "--format", "json",
        ])
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap()
    };

    let (url, task) = serve(r#"{"org":"acme","budgetUsd":7.5}"#);
    let out = pull(&url);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        task.join().unwrap(),
        "GET /v1/bench/settings?org=acme HTTP/1.1"
    );
    assert_eq!(
        stdout_json(&out),
        serde_json::json!({"budget_usd": 7.5, "source": "settings"})
    );

    // A reply for another organization or a nonsense cap changes nothing.
    for reply in [
        r#"{"org":"other","budgetUsd":900}"#,
        r#"{"org":"acme","budgetUsd":-1}"#,
    ] {
        let (url, task) = serve(reply);
        assert_eq!(code(&pull(&url)), 2, "{reply}");
        task.join().unwrap();
        assert_eq!(
            stdout_json(&repo.run(&["bench", "budget", "--format", "json"]))["budget_usd"],
            7.5
        );
    }

    let (url, task) = serve(r#"{"org":"acme","budgetUsd":null}"#);
    let out = pull(&url);
    task.join().unwrap();
    assert_eq!(stdout_json(&out)["source"], "default");
}

#[test]
fn a_priced_harness_that_reports_no_cost_stops_the_run_instead_of_spending_blind() {
    let repo = Repo::new();
    repo.program(
        "claude",
        r#"if [ "$1" = --version ]; then echo 1; exit 0; fi
echo '{"type":"result","subtype":"success","is_error":false}'"#,
    );
    let suite = repo.suite(
        r#"version = 1
trials = 3
[baseline]
harness = "claude"
model = "a"
[candidate]
harness = "claude"
model = "b"
[[tasks]]
id = "t"
prompt = "p"
test = "true"
"#,
    );
    let out = repo.run(&[
        "bench",
        "run",
        &suite,
        "--format",
        "json",
        "--budget-usd",
        "0.01",
    ]);
    assert_eq!(code(&out), 3);
    let r = stdout_json(&out);
    assert_eq!(r["stop"], "unknown_cost");
    assert_eq!(r["trials"].as_array().unwrap().len(), 1);
    assert_eq!(r["unpriced_trials"], 1);
    assert!(
        String::from_utf8_lossy(
            &repo
                .run(&["bench", "show", r["id"].as_str().unwrap()])
                .stdout
        )
        .contains("incomplete (unknown cost)")
    );

    // The user can agree to it, and then every trial runs.
    let out = repo.run(&[
        "bench",
        "run",
        &suite,
        "--allow-unpriced",
        "--format",
        "json",
    ]);
    assert_eq!(code(&out), 0);
    assert_eq!(stdout_json(&out)["trials"].as_array().unwrap().len(), 6);
}

#[test]
fn an_agent_that_never_reads_its_prompt_cannot_take_the_run_down() {
    let repo = Repo::new();
    repo.program(
        "claude",
        r#"if [ "$1" = --version ]; then echo 1; exit 0; fi
echo '{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.01}'"#,
    );
    // Larger than a pipe's buffer, so a writer would still be writing when the agent exits.
    repo.write(".cedr/bench/prompt.md", &"p".repeat(1024 * 1024));
    let suite = repo.suite(
        r#"version = 1
trials = 1
[baseline]
harness = "claude"
model = "a"
[candidate]
harness = "claude"
model = "b"
[[tasks]]
id = "t"
prompt_file = ".cedr/bench/prompt.md"
test = "true"
"#,
    );
    let out = repo.run(&["bench", "run", &suite, "--format", "json"]);
    assert_eq!(out.status.code(), Some(0), "killed by a signal when None");
    assert_eq!(stdout_json(&out)["summary"]["candidate"]["passed"], 1);
}

#[test]
fn a_settings_file_that_cannot_be_read_is_an_error_rather_than_the_default_cap() {
    let repo = Repo::new();
    // A directory at the file's path fails to read for every user, root included.
    fs::create_dir_all(repo.dir.path().join("config/cedr/cli.toml")).unwrap();
    let out = repo.run(&["bench", "budget"]);
    assert_eq!(code(&out), 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("cli.toml"));
}

#[test]
fn an_interrupt_kills_the_agent_and_its_children_and_keeps_what_was_measured() {
    let repo = Repo::new();
    let marker = repo.dir.path().join("agent.pid");
    repo.program(
        "agent",
        &format!(
            "sleep 60 &\necho $! > {}.tmp\nmv {0}.tmp {0}\nwait",
            marker.display()
        ),
    );
    let suite = repo.suite(&command_suite(1));
    let child = repo
        .cedr(&run_args(&suite))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&marker);
    send("-INT", child.id());
    let out = child.wait_with_output().unwrap();

    assert_eq!(code(&out), 3);
    let r = stdout_json(&out);
    assert_eq!(r["stop"], "interrupted");
    assert_eq!(r["complete"], false);
    assert_eq!(r["trials"].as_array().unwrap().len(), 1);
    assert_eq!(r["trials"][0]["status"], "interrupted");
    let helper = fs::read_to_string(&marker).unwrap().trim().to_string();
    let alive = Command::new("kill")
        .args(["-0", &helper])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the agent's child outlived the run");
    let saved = repo.run(&["bench", "history", "--format", "json"]);
    assert_eq!(stdout_json(&saved)[0]["stop"], "interrupted");
    let left: Vec<_> = fs::read_dir(repo.dir.path().join("tmp")).unwrap().collect();
    assert!(left.is_empty(), "the clone was left behind: {left:?}");
}

#[test]
fn a_timed_out_agent_has_an_unknown_cost_even_after_printing_one() {
    let repo = Repo::new();
    repo.program(
        "codex",
        r#"if [ "$1" = --version ]; then echo 1; exit 0; fi
echo '{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":10}}'
sleep 30"#,
    );
    let suite = repo.suite(
        r#"version = 1
trials = 2
timeout_seconds = 1
[baseline]
harness = "codex"
model = "gpt-6.1-sol"
effort = "low"
[candidate]
harness = "codex"
model = "gpt-6.1-sol"
effort = "high"
[[tasks]]
id = "t"
prompt = "p"
test = "true"
"#,
    );
    let out = repo.run(&["bench", "run", &suite, "--format", "json"]);
    assert_eq!(code(&out), 3);
    let r = stdout_json(&out);
    assert_eq!(r["stop"], "unknown_cost");
    assert_eq!(r["trials"].as_array().unwrap().len(), 1);
    assert_eq!(r["trials"][0]["cost_usd"], Value::Null);
    assert_eq!(r["unpriced_trials"], 1);
}

#[test]
fn a_prompt_file_that_cannot_be_read_stops_the_run_before_any_spend() {
    let repo = Repo::new();
    let marker = repo.dir.path().join("ran");
    repo.program("agent", &format!("touch {}", marker.display()));
    fs::write(repo.root().join("bad.md"), [0xff_u8, 0xfe]).unwrap();
    let suite = repo.suite(&format!(
        "{}\n[[tasks]]\nid = \"second\"\nprompt_file = \"bad.md\"\ntest = \"true\"\n",
        command_suite(1)
    ));
    assert_eq!(code(&repo.run(&["bench", "plan", &suite])), 2);
    assert_eq!(code(&repo.run(&run_args(&suite))), 2);
    assert!(!marker.exists(), "an agent started before the refusal");
}

#[test]
fn a_shallow_clone_is_refused_before_any_trial() {
    let repo = Repo::new();
    repo.write("x", "x\n");
    repo.commit("second");
    let shallow = repo.dir.path().join("shallow");
    git(
        repo.dir.path(),
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &format!("file://{}", repo.root().display()),
            shallow.to_str().unwrap(),
        ],
    );
    fs::create_dir_all(shallow.join(".cedr/bench")).unwrap();
    let suite = shallow.join(".cedr/bench/s.toml");
    fs::write(&suite, command_suite(1)).unwrap();
    let out = repo
        .cedr(&["bench", "plan", suite.to_str().unwrap()])
        .current_dir(&shallow)
        .output()
        .unwrap();
    assert_eq!(code(&out), 2);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("fetch-depth: 0"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_relative_command_path_is_from_the_repo_root_wherever_cedr_runs() {
    let repo = Repo::new();
    let script = repo.write("tools/agent.sh", &format!("#!/bin/sh\n{AGENT}\n"));
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    repo.write("sub/keep", "");
    repo.commit("tools");
    let suite = repo.suite(&command_suite(1).replace("\"agent\"", "\"./tools/agent.sh\""));
    let out = repo
        .cedr(&run_args(&suite))
        .current_dir(repo.root().join("sub"))
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let r = stdout_json(&out);
    assert_eq!(r["trials"][1]["status"], "passed");
}

/// Waits for an agent's marker file, which it writes once it is running.
fn wait_for(marker: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the agent never started"
        );
        std::thread::yield_now();
    }
}

fn send(signal: &str, pid: u32) {
    let sent = Command::new("kill")
        .args([signal, &pid.to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
}

#[test]
fn an_interrupt_is_the_stop_reason_even_when_the_trial_cost_is_unknown() {
    let repo = Repo::new();
    let marker = repo.dir.path().join("running");
    repo.program(
        "codex",
        &format!(
            "if [ \"$1\" = --version ]; then echo 1; exit 0; fi\ntouch {}\nsleep 60",
            marker.display()
        ),
    );
    let suite = repo.suite(
        r#"version = 1
trials = 2
[baseline]
harness = "codex"
model = "gpt-6.1-sol"
effort = "low"
[candidate]
harness = "codex"
model = "gpt-6.1-sol"
effort = "high"
[[tasks]]
id = "t"
prompt = "p"
test = "true"
"#,
    );
    let child = repo
        .cedr(&["bench", "run", &suite, "--format", "json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&marker);
    send("-TERM", child.id());
    let out = child.wait_with_output().unwrap();
    assert_eq!(code(&out), 3);
    assert_eq!(stdout_json(&out)["stop"], "interrupted");
}

#[test]
fn an_interrupt_saves_the_run_when_its_output_has_gone_away() {
    let repo = Repo::new();
    let marker = repo.dir.path().join("running");
    repo.program("agent", &format!("touch {}\nsleep 60", marker.display()));
    let suite = repo.suite(&command_suite(1));
    let mut child = repo
        .cedr(&run_args(&suite))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    drop(child.stderr.take());
    wait_for(&marker);
    send("-INT", child.id());
    assert_eq!(child.wait().unwrap().code(), Some(3));
    let saved = repo.run(&["bench", "history", "--format", "json"]);
    assert_eq!(stdout_json(&saved)[0]["stop"], "interrupted");
}

#[test]
fn a_hangup_ignored_by_the_parent_does_not_stop_the_run() {
    let repo = Repo::new();
    let marker = repo.dir.path().join("running");
    let go = repo.dir.path().join("go");
    repo.program(
        "agent",
        &format!(
            "touch {}\nwhile [ ! -e {} ]; do sleep 0.05; done\necho fixed > done",
            marker.display(),
            go.display()
        ),
    );
    let suite = repo.suite(&command_suite(1));
    // The shell ignores the signal and execs cedr, which is what nohup does.
    let mut args = vec![
        "-c".to_string(),
        "trap '' HUP; exec \"$@\"".to_string(),
        "sh".to_string(),
    ];
    args.push(env!("CARGO_BIN_EXE_cedr").to_string());
    args.extend(run_args(&suite).iter().map(ToString::to_string));
    let cedr = repo.cedr(&[]);
    let env = cedr
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key, value)));
    let child = Command::new("sh")
        .args(&args)
        .current_dir(repo.root())
        .envs(env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&marker);
    send("-HUP", child.id());
    fs::write(&go, "").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(code(&out), 0);
    assert_eq!(stdout_json(&out)["stop"], Value::Null);
}

#[test]
fn an_agent_that_removes_its_clone_has_an_unknown_cost() {
    let repo = Repo::new();
    repo.program("agent", "cd .. && rm -rf repo");
    let suite = repo.suite(&command_suite(1));
    let out = repo.run(&run_args(&suite));
    assert_eq!(code(&out), 3);
    let r = stdout_json(&out);
    assert_eq!(r["trials"][0]["status"], "agent_error");
    assert_eq!(r["unpriced_trials"], 2);
}

/// A loopback API with bounded accepts, so a missing request fails instead of hanging the suite.
struct BenchApi {
    listener: std::net::TcpListener,
    url: String,
}

impl BenchApi {
    fn new() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        Self { listener, url }
    }

    fn respond(&self, replies: Vec<(u16, bool)>) -> std::thread::JoinHandle<Vec<Value>> {
        self.respond_with_retry_after(replies.into_iter().map(|(s, d)| (s, d, None)).collect())
    }

    fn respond_with_retry_after(
        &self,
        replies: Vec<(u16, bool, Option<&'static str>)>,
    ) -> std::thread::JoinHandle<Vec<Value>> {
        use std::io::{Read, Write};
        let listener = self.listener.try_clone().unwrap();
        std::thread::spawn(move || {
            replies.into_iter().map(|(status, duplicate, retry_after)| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(std::time::Instant::now() < deadline, "missing Bench request");
                            std::thread::yield_now();
                        }
                        Err(e) => panic!("accept: {e}"),
                    }
                };
                stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
                let mut request = Vec::new();
                let mut byte = [0_u8];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                let headers = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(headers.starts_with("post /v1/bench/runs http/1.1\r\n"), "{headers}");
                assert!(headers.contains("authorization: bearer test-key\r\n"));
                assert!(headers.contains("content-type: application/json\r\n"));
                let length: usize = headers.lines().find_map(|l| l.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                let mut bytes = vec![0; length];
                stream.read_exact(&mut bytes).unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                let reply = if status == 200 {
                    serde_json::json!({"ok": true, "id": body["result"]["id"], "duplicate": duplicate})
                } else {
                    serde_json::json!({"error": match status {
                        400 => "bad_request", 403 => "not_a_member", 413 => "too_large", _ => "unavailable",
                    }, "message": "test-key"})
                }.to_string();
                let retry_after = retry_after.map(|v| format!("Retry-After: {v}\r\n")).unwrap_or_default();
                write!(stream, "HTTP/1.1 {status} reply\r\n{retry_after}Content-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
                body
            }).collect()
        })
    }

    fn assert_no_request(&self) {
        assert!(
            matches!(self.listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "unexpected request"
        );
    }

    fn config(&self, repo: &Repo, extra: &str) -> String {
        repo.machine_routes(&format!(
            "version=1\n[sync]\norg='acme'\nurl={:?}\n{extra}\n",
            self.url
        ))
        .display()
        .to_string()
    }
}

fn seed_result(repo: &Repo) -> Value {
    repo.program("agent", AGENT);
    let suite = repo.suite(&command_suite(1));
    let mut args = run_args(&suite);
    args.push("--no-record");
    let out = repo.run(&args);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    stdout_json(&out)
}

fn store_result(repo: &Repo, id: &str, result: &Value) {
    let mut result = result.clone();
    result["id"] = id.into();
    let dir = repo.dir.path().join("state/cedr/bench");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
}

#[test]
fn push_tracks_unsent_ids_per_org_and_explicit_ids_can_be_duplicates() {
    let repo = Repo::new();
    let result = seed_result(&repo);
    for id in ["b-3", "b-1", "b-2"] {
        store_result(&repo, id, &result);
    }
    let mut other = result.clone();
    other["source"]["repo"] = "other/repo".into();
    store_result(&repo, "b-4-other", &other);
    let mut legacy = result.clone();
    legacy.as_object_mut().unwrap().remove("source");
    store_result(&repo, "b-5-legacy", &legacy);
    fs::write(
        repo.dir.path().join("state/cedr/bench/b-6-corrupt.json"),
        "{not json",
    )
    .unwrap();
    fs::create_dir(repo.dir.path().join("state/cedr/bench/b-7-unreadable.json")).unwrap();
    let state = repo.dir.path().join("state/cedr");
    fs::write(state.join("bench-pushed-acme.txt"), "b-1\n").unwrap();
    let api = BenchApi::new();
    let config = api.config(&repo, "");
    let send = |extra: &[&str]| {
        let mut args = vec!["bench", "push", "--config", &config, "--format", "json"];
        args.extend_from_slice(extra);
        repo.cedr(&args)
            .env("CEDR_API_KEY", "test-key")
            .output()
            .unwrap()
    };
    let task = api.respond(vec![(200, false), (200, false)]);
    let out = send(&[]);
    assert_eq!(code(&out), 0);
    let bodies = task.join().unwrap();
    assert_eq!(
        bodies
            .iter()
            .map(|b| b["result"]["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["b-2", "b-3"]
    );
    assert_eq!(
        stdout_json(&out),
        serde_json::json!({"org":"acme","pushed":["b-2","b-3"],"duplicates":[],"failed":[],"skipped":4})
    );
    assert_eq!(
        fs::read_to_string(state.join("bench-pushed-acme.txt")).unwrap(),
        "b-1\nb-2\nb-3\n"
    );
    let out = send(&[]);
    assert_eq!(
        code(&out),
        0,
        "other repos and older results are not failures"
    );
    assert_eq!(stdout_json(&out)["pushed"], serde_json::json!([]));
    assert_eq!(stdout_json(&out)["skipped"], 4);
    let out = repo
        .cedr(&["bench", "push", "--config", &config])
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "pushed 0 to acme\n4 saved results from other repos left alone; name them or pass --all\n"
    );
    api.assert_no_request();
    let task = api.respond(vec![(200, true)]);
    let out = send(&["b-2"]);
    task.join().unwrap();
    assert_eq!(stdout_json(&out)["duplicates"], serde_json::json!(["b-2"]));
    assert_eq!(stdout_json(&out)["pushed"], serde_json::json!(["b-2"]));
    let task = api.respond(vec![(200, false); 3]);
    let out = send(&["--org", "other"]);
    assert_eq!(code(&out), 0);
    assert!(task.join().unwrap().iter().all(|b| b["org"] == "other"));
    assert_eq!(
        fs::read_to_string(state.join("bench-pushed-other.txt")).unwrap(),
        "b-1\nb-2\nb-3\n"
    );
    assert_eq!(
        fs::read_to_string(state.join("bench-pushed-acme.txt")).unwrap(),
        "b-1\nb-2\nb-3\nb-2\n"
    );
    let out = send(&["--all", "--repo", "override"]);
    assert_eq!(code(&out), 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used with"));
    api.assert_no_request();
    let task = api.respond(vec![(200, false)]);
    let out = send(&["--all"]);
    assert_eq!(
        code(&out),
        2,
        "--all still reports a legacy result with no repo"
    );
    assert_eq!(task.join().unwrap()[0]["repo"], "other/repo");
    assert_eq!(
        stdout_json(&out)["pushed"],
        serde_json::json!(["b-4-other"])
    );
    assert_eq!(stdout_json(&out)["skipped"], 0);
    assert_eq!(stdout_json(&out)["failed"][0]["id"], "b-5-legacy");
    assert_eq!(stdout_json(&out)["failed"][1]["id"], "b-6-corrupt");
    assert_eq!(stdout_json(&out)["failed"][2]["id"], "b-7-unreadable");
    let out = send(&["b-6-corrupt"]);
    assert_eq!(code(&out), 2);
    assert_eq!(stdout_json(&out)["failed"][0]["id"], "b-6-corrupt");
    api.assert_no_request();
}

#[test]
fn auto_sends_only_the_new_run_even_when_incomplete_with_the_exact_body() {
    let repo = Repo::new();
    let result = seed_result(&repo);
    store_result(&repo, "b-old", &result);
    let api = BenchApi::new();
    api.config(&repo, &format!("[machine]\nname={:?}", "é".repeat(64)));
    let suite = repo.suite(&command_suite(10));
    let mut args = run_args(&suite);
    args.extend(["--budget-usd", "0.25", "--repo", "acme/web"]);
    let task = api.respond(vec![(200, false)]);
    let out = repo
        .cedr(&args)
        .env("CEDR_API_KEY", "test-key")
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_SERVER_URL", "https://github.com")
        .env("GITHUB_REPOSITORY", "from/env")
        .env("GITHUB_RUN_ID", "123")
        .env("GITHUB_REF", "refs/pull/12/merge")
        .output()
        .unwrap();
    assert_eq!(code(&out), 3);
    let mut saved = stdout_json(&out);
    let id = saved["id"].as_str().unwrap().to_string();
    let source = saved.as_object_mut().unwrap().remove("source").unwrap();
    assert_eq!(source["repo"], "acme/web");
    let bodies = task.join().unwrap();
    let machine = fs::read_to_string(repo.dir.path().join("state/cedr/machine-id")).unwrap();
    assert_eq!(
        bodies,
        vec![serde_json::json!({
            "org": "acme", "cli": env!("CARGO_PKG_VERSION"), "repo": "acme/web",
            "machine": {"id": machine.trim(), "name": "é".repeat(64)},
            "ci": {"provider":"github", "pr":12, "run_url":"https://github.com/from/env/actions/runs/123"},
            "result": saved,
        })]
    );
    assert!(bodies[0]["result"].get("source").is_none());
    assert_eq!(
        fs::read_to_string(repo.dir.path().join("state/cedr/bench-pushed-acme.txt")).unwrap(),
        format!("{id}\n")
    );
    let shown = repo.run(&["bench", "show", &id, "--format", "json"]);
    assert_eq!(out.stdout, shown.stdout);
    assert!(String::from_utf8_lossy(&out.stderr).contains(&format!("pushed {id} to acme")));
    api.assert_no_request();
}

#[test]
fn failed_auto_sends_preserve_complete_regression_and_incomplete_reports_and_exit_codes() {
    let repo = Repo::new();
    repo.program("agent", AGENT);
    let api = BenchApi::new();
    api.config(&repo, "");
    let suite = repo.suite(
        &command_suite(2)
            .replace("model = \"bad\"", "model = \"x\"")
            .replace("model = \"good\"", "model = \"bad\"")
            .replace("model = \"x\"", "model = \"good\""),
    );
    for (status, expected, extra, error) in [
        (400, 0, vec![], "bad_request"),
        (403, 1, vec!["--fail-on", "lower"], "not_a_member"),
        (413, 3, vec!["--budget-usd", "0.15"], "too_large"),
    ] {
        let task = api.respond(vec![(status, false)]);
        let mut args = run_args(&suite);
        args.extend(extra.iter());
        let out = repo
            .cedr(&args)
            .env("CEDR_API_KEY", "test-key")
            .output()
            .unwrap();
        task.join().unwrap();
        assert_eq!(code(&out), expected);
        let saved = stdout_json(&out);
        let id = saved["id"].as_str().unwrap();
        let mut show_args = vec!["bench", "show", id, "--format", "json"];
        if expected == 1 {
            show_args.extend(["--fail-on", "lower"]);
        }
        let shown = repo.run(&show_args);
        assert_eq!(code(&shown), expected);
        assert_eq!(out.stdout, shown.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("was not sent")
                && stderr.contains(error)
                && stderr.contains("cedr bench push"),
            "{stderr}"
        );
        assert!(!stderr.contains("test-key"));
        assert_eq!(
            stderr
                .lines()
                .filter(|l| l.contains("was not sent"))
                .count(),
            1
        );
    }
    assert!(
        !repo
            .dir
            .path()
            .join("state/cedr/bench-pushed-acme.txt")
            .exists()
    );
}

#[test]
fn automatic_sending_respects_opt_outs_and_silent_unconfigured_states() {
    let repo = Repo::new();
    repo.program("agent", AGENT);
    let suite = repo.suite(&command_suite(1));
    let api = BenchApi::new();
    let config = repo.dir.path().join("config/cedr/routes.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let cases = [
        (None, None, false),
        (Some("version=1\n".to_string()), None, false),
        (
            Some(format!(
                "version=1\n[sync]\norg='acme'\nurl={:?}\nauto=false\n",
                api.url
            )),
            None,
            false,
        ),
        (
            Some(format!(
                "version=1\n[sync]\norg='acme'\nurl={:?}\n",
                api.url
            )),
            Some("--no-push"),
            false,
        ),
        (
            Some(format!(
                "version=1\n[sync]\norg='acme'\nurl={:?}\n",
                api.url
            )),
            Some("--no-record"),
            false,
        ),
        (Some("broken = [".to_string()), None, true),
    ];
    let mut no_push_id = String::new();
    for (text, flag, warns) in cases {
        if let Some(text) = text {
            fs::write(&config, text).unwrap();
        }
        let before = stdout_json(&repo.run(&["bench", "history", "--format", "json"]))
            .as_array()
            .unwrap()
            .len();
        let mut args = run_args(&suite);
        if let Some(flag) = flag {
            args.push(flag);
        }
        let out = repo
            .cedr(&args)
            .env("CEDR_API_KEY", "test-key")
            .output()
            .unwrap();
        assert_eq!(code(&out), 0);
        assert_eq!(stdout_json(&out)["complete"], true);
        if flag == Some("--no-push") {
            no_push_id = stdout_json(&out)["id"].as_str().unwrap().to_string();
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(stderr.contains("was not sent"), warns, "{stderr}");
        let after = stdout_json(&repo.run(&["bench", "history", "--format", "json"]))
            .as_array()
            .unwrap()
            .len();
        assert_eq!(after - before, usize::from(flag != Some("--no-record")));
        api.assert_no_request();
    }
    api.config(&repo, "");
    let out = repo.run(&run_args(&suite));
    assert_eq!(code(&out), 0, "no key must not fail the run");
    assert!(String::from_utf8_lossy(&out.stderr).contains("was not sent: no API key"));
    api.assert_no_request();
    // --no-push postpones sending: a later bare push still includes that saved result.
    let due = stdout_json(&repo.run(&["bench", "history", "--format", "json"]))
        .as_array()
        .unwrap()
        .len();
    let task = api.respond(vec![(200, false); due]);
    let out = repo
        .cedr(&["bench", "push", "--format", "json"])
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0);
    assert!(
        stdout_json(&out)["pushed"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(no_push_id))
    );
    assert_eq!(task.join().unwrap().len(), due);
}

#[test]
fn push_continues_after_local_and_server_failures_and_legacy_results_can_be_sent() {
    let repo = Repo::new();
    let result = seed_result(&repo);
    let mut old = result.clone();
    old.as_object_mut().unwrap().remove("source");
    store_result(&repo, "b-1-old", &old);
    let shown = repo.run(&["bench", "show", "b-1-old", "--format", "json"]);
    assert_eq!(code(&shown), 0);
    assert_eq!(stdout_json(&shown)["version"], 1);
    assert!(stdout_json(&shown)["source"].is_null());
    let mut big = result.clone();
    big["variants"]["baseline"]["resolved_models"] = serde_json::json!(vec!["m".repeat(256); 1100]);
    store_result(&repo, "b-2-big", &big);
    store_result(&repo, "b-3-denied", &result);
    store_result(&repo, "b-4-ok", &result);
    let api = BenchApi::new();
    let config = api.config(&repo, "");
    let task = api.respond(vec![(403, false), (200, false)]);
    let out = repo
        .cedr(&[
            "bench",
            "push",
            "b-1-old",
            "b-2-big",
            "b-3-denied",
            "b-4-ok",
            "--config",
            &config,
            "--format",
            "json",
        ])
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    let bodies = task.join().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(code(&out), 2);
    let report = stdout_json(&out);
    assert_eq!(report["pushed"], serde_json::json!(["b-4-ok"]));
    assert_eq!(report["failed"][0]["id"], "b-1-old");
    assert!(
        report["failed"][0]["error"]
            .as_str()
            .unwrap()
            .contains("pass --repo")
    );
    assert!(
        report["failed"][1]["error"]
            .as_str()
            .unwrap()
            .contains("b-2-big: body over 256 KiB (262144 bytes)")
    );
    assert!(
        report["failed"][2]["error"]
            .as_str()
            .unwrap()
            .contains("not_a_member")
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("test-key"));
    assert_eq!(
        fs::read_to_string(repo.dir.path().join("state/cedr/bench-pushed-acme.txt")).unwrap(),
        "b-4-ok\n"
    );
    api.assert_no_request();
    let out = repo
        .cedr(&["bench", "push", "b-1-old", "--config", &config])
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    assert_eq!(code(&out), 2);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with("b-1-old: no repo name saved; pass --repo\n"),
        "{text}"
    );
    assert!(text.ends_with("pushed 0 to acme\n"));
    api.assert_no_request();
    let task = api.respond(vec![(200, true)]);
    let out = repo
        .cedr(&[
            "bench",
            "push",
            "b-1-old",
            "--config",
            &config,
            "--repo",
            "override/repo",
        ])
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0);
    assert_eq!(task.join().unwrap()[0]["repo"], "override/repo");
    assert!(String::from_utf8_lossy(&out.stdout).contains("pushed 1 to acme (1 duplicates)"));
}

#[test]
fn dry_run_prints_the_bodies_without_a_key_request_or_sent_marker() {
    let repo = Repo::new();
    let result = seed_result(&repo);
    store_result(&repo, "b-dry", &result);
    let api = BenchApi::new();
    let config = api.config(&repo, "");
    let out = repo.run(&[
        "bench",
        "push",
        "b-dry",
        "--config",
        &config,
        "--repo",
        "preview/repo",
        "--dry-run",
        "--format",
        "json",
    ]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let bodies = stdout_json(&out);
    assert_eq!(bodies.as_array().unwrap().len(), 1);
    assert_eq!(bodies[0]["result"]["id"], "b-dry");
    assert_eq!(bodies[0]["repo"], "preview/repo");
    assert!(bodies[0]["result"].get("source").is_none());
    assert!(
        !repo
            .dir
            .path()
            .join("state/cedr/bench-pushed-acme.txt")
            .exists()
    );
    api.assert_no_request();
}

#[test]
fn repo_precedence_sanitizes_remotes_and_ci_metadata_only_describes_github_https_runs() {
    let repo = Repo::new();
    repo.program("agent", AGENT);
    let suite = repo.suite(&command_suite(1));
    git(
        &repo.root(),
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/nested/acme/web.git",
        ],
    );
    let run = |extra: &[&str], github: bool, env_repo: &str, server: &str, reference: &str| {
        let mut args = run_args(&suite);
        args.push("--no-record");
        args.extend_from_slice(extra);
        repo.cedr(&args)
            .env("GITHUB_ACTIONS", if github { "true" } else { "false" })
            .env("GITHUB_REPOSITORY", env_repo)
            .env("GITHUB_SERVER_URL", server)
            .env("GITHUB_RUN_ID", "456")
            .env("GITHUB_REF", reference)
            .output()
            .unwrap()
    };
    let out = run(
        &["--repo", "explicit/name"],
        true,
        "env/name",
        "https://github.com",
        "refs/pull/42/merge",
    );
    assert_eq!(
        stdout_json(&out)["source"],
        serde_json::json!({"repo":"explicit/name", "root_commit": stdout_json(&out)["variants"]["baseline"]["commit"], "ci":{"provider":"github", "pr":42, "run_url":"https://github.com/env/name/actions/runs/456"}})
    );
    let out = run(
        &[],
        true,
        "env/name",
        "https://github.com",
        "refs/heads/main",
    );
    assert_eq!(stdout_json(&out)["source"]["repo"], "env/name");
    assert!(stdout_json(&out)["source"]["ci"]["pr"].is_null());
    let out = run(
        &[],
        true,
        "env/name",
        "http://github.com",
        "refs/pull/42/merge",
    );
    assert!(stdout_json(&out)["source"]["ci"].is_null());
    for github in [true, false] {
        let out = run(
            &[],
            github,
            "bad name",
            "https://github.com",
            "refs/pull/42/merge",
        );
        assert_eq!(stdout_json(&out)["source"]["repo"], "acme/web");
        assert!(!String::from_utf8_lossy(&out.stdout).contains("example.invalid"));
    }
    git(
        &repo.root(),
        &[
            "remote",
            "set-url",
            "origin",
            "git@example.invalid:scp/project.git",
        ],
    );
    assert_eq!(
        stdout_json(&run(&[], false, "ignored/name", "", ""))["source"]["repo"],
        "scp/project"
    );
    for remote in [
        "/home/alice/web",
        "file:///home/alice/web.git",
        "../sibling",
        "./sibling",
        "sibling/web",
        "C:\\Users\\alice\\web",
        "https://example.invalid/acme/../web.git",
        "https://example.invalid/acme/%2e%2e/web.git",
        "git@example.invalid:../acme/web.git",
    ] {
        git(&repo.root(), &["remote", "set-url", "origin", remote]);
        let out = run(&[], false, "", "", "");
        assert_eq!(stdout_json(&out)["source"]["repo"], "repo", "{remote}");
    }
    git(
        &repo.root(),
        &["remote", "set-url", "origin", "invalid name"],
    );
    assert_eq!(
        stdout_json(&run(&[], false, "", "", ""))["source"]["repo"],
        "repo"
    );
    let out = repo
        .cedr(&run_args(&suite))
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_REPOSITORY", "env/name")
        .output()
        .unwrap();
    assert!(stdout_json(&out)["source"]["ci"].is_null());
    let marker = repo.dir.path().join("ran");
    repo.program("agent", &format!("touch {}", marker.display()));
    let out = run(
        &["--repo", "bad name"],
        true,
        "valid/name",
        "https://github.com",
        "",
    );
    assert_eq!(code(&out), 2);
    assert!(!marker.exists());
    // If every inferred candidate is invalid, the run still completes with no repo name.
    repo.program("agent", AGENT);
    let invalid_root = repo.root().with_file_name("bad name");
    fs::rename(repo.root(), &invalid_root).unwrap();
    let suite = invalid_root.join(".cedr/bench/s.toml");
    let out = repo
        .cedr(&run_args(suite.to_str().unwrap()))
        .current_dir(invalid_root)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0);
    assert!(stdout_json(&out)["source"]["repo"].is_null());
}

#[test]
fn bare_push_uses_run_name_precedence_and_requires_repo_context() {
    let repo = Repo::new();
    let result = seed_result(&repo);
    for (id, name) in [
        ("b-flag", "explicit/name"),
        ("b-actions", "actions/name"),
        ("b-remote", "remote/name"),
        ("b-directory", "repo"),
    ] {
        let mut result = result.clone();
        result["source"]["repo"] = name.into();
        store_result(&repo, id, &result);
    }
    git(
        &repo.root(),
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/remote/name.git",
        ],
    );
    let api = BenchApi::new();
    let config = api.config(&repo, "");
    let args = [
        "bench",
        "push",
        "--config",
        &config,
        "--dry-run",
        "--format",
        "json",
    ];
    // Match from a subdirectory using the git top level, with the same precedence as run.
    let sub = repo.root().join("nested");
    fs::create_dir(&sub).unwrap();
    for (flag, github, expected) in [
        (Some("explicit/name"), true, "b-flag"),
        (None, true, "b-actions"),
        (None, false, "b-remote"),
    ] {
        let mut command = repo.cedr(&args);
        command
            .current_dir(&sub)
            .env("GITHUB_ACTIONS", if github { "true" } else { "false" })
            .env("GITHUB_REPOSITORY", "actions/name");
        if let Some(flag) = flag {
            command.args(["--repo", flag]);
        }
        let out = command.output().unwrap();
        assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
        let bodies = stdout_json(&out);
        assert_eq!(bodies.as_array().unwrap().len(), 1);
        assert_eq!(bodies[0]["result"]["id"], expected);
    }
    git(
        &repo.root(),
        &["remote", "set-url", "origin", "/home/alice/web"],
    );
    let out = repo.cedr(&args).current_dir(&sub).output().unwrap();
    assert_eq!(stdout_json(&out)[0]["result"]["id"], "b-directory");
    let outside = repo.dir.path();
    let out = repo.cedr(&args).current_dir(outside).output().unwrap();
    assert_eq!(code(&out), 2);
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains("run it inside the repo, name ids or pass --all"),
        "{error}"
    );
    // A name alone can collide across organizations, so an explicit one cannot bypass root matching.
    let out = repo
        .cedr(&args)
        .current_dir(outside)
        .args(["--repo", "explicit/name"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 2);
    let out = repo
        .cedr(&args)
        .current_dir(outside)
        .arg("--all")
        .output()
        .unwrap();
    assert_eq!(stdout_json(&out).as_array().unwrap().len(), 4);
    api.assert_no_request();
}

#[test]
fn uploads_clean_paths_controls_empty_values_and_lengths_without_changing_saved_results() {
    let repo = Repo::new();
    let mut result = seed_result(&repo);
    result["suite"]["name"] = format!("\u{1b}{}\n", "é".repeat(140)).into();
    let models = serde_json::json!([
        "~/models/home.gguf/",
        "./models/here.gguf",
        "../models/parent.gguf",
        "C:\\Users\\alice\\drive.gguf",
        "/home/alice/absolute.gguf",
        "",
        "\n\t",
        "vendor/model",
        "a:mini",
        format!("\u{0}{}\n", "é".repeat(270)),
        "file:///home/alice/file.gguf/",
        r"\\server\share\unc.gguf\",
        r".\models\here",
        r"..\models\llama",
        "/",
        r"C:\",
    ]);
    let cleaned_models = serde_json::json!([
        "home.gguf",
        "here.gguf",
        "parent.gguf",
        "drive.gguf",
        "absolute.gguf",
        "vendor/model",
        "a:mini",
        "é".repeat(256),
        "file.gguf",
        "unc.gguf",
        "here",
        "llama",
    ]);
    let baseline = &mut result["variants"]["baseline"];
    baseline["model"] = "\n/home/alice/models/x.gguf\t".into();
    baseline["effort"] = "\n\t".into();
    baseline["ref"] = format!("\u{0}{}\n", "é".repeat(270)).into();
    baseline["harness_version"] = format!("\u{1b}[31m{}\n", "v".repeat(600)).into();
    baseline["resolved_models"] = models.clone();
    let candidate = &mut result["variants"]["candidate"];
    candidate["model"] = format!("\t{}\n", "é".repeat(270)).into();
    candidate["effort"] = format!("\t{}\n", "é".repeat(140)).into();
    candidate["ref"] = "\nmain\t".into();
    candidate["harness_version"] = "\n\u{1b}".into();
    candidate["resolved_models"] = models.clone();
    for trial in result["trials"].as_array_mut().unwrap() {
        trial["models"] = models.clone();
    }
    result["id"] = "b-clean".into();
    store_result(&repo, "b-clean", &result);
    let path = repo.dir.path().join("state/cedr/bench/b-clean.json");
    let original_bytes = fs::read(&path).unwrap();
    let original_report = repo.run(&["bench", "show", "b-clean", "--format", "json"]);
    assert_eq!(code(&original_report), 0);
    assert_eq!(stdout_json(&original_report), result);
    let api = BenchApi::new();
    let config = api.config(&repo, "");
    let task = api.respond(vec![(200, false)]);
    let out = repo
        .cedr(&["bench", "push", "b-clean", "--config", &config])
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let bodies = task.join().unwrap();
    let sent = &bodies[0]["result"];
    assert_eq!(sent["suite"]["name"], "é".repeat(128));
    let baseline = &sent["variants"]["baseline"];
    assert_eq!(baseline["model"], "x.gguf");
    assert!(baseline["effort"].is_null());
    assert_eq!(baseline["ref"], "é".repeat(256));
    assert_eq!(
        baseline["harness_version"],
        format!("[31m{}", "v".repeat(508))
    );
    let candidate = &sent["variants"]["candidate"];
    assert_eq!(candidate["model"], "é".repeat(256));
    assert_eq!(candidate["effort"], "é".repeat(128));
    assert_eq!(candidate["ref"], "main");
    assert!(candidate["harness_version"].is_null());
    assert_eq!(baseline["resolved_models"], cleaned_models);
    assert_eq!(candidate["resolved_models"], cleaned_models);
    for trial in sent["trials"].as_array().unwrap() {
        assert_eq!(trial["models"], cleaned_models);
    }
    assert!(!bodies[0].to_string().contains("alice"));
    assert_eq!(fs::read(path).unwrap(), original_bytes);
    assert_eq!(
        repo.run(&["bench", "show", "b-clean", "--format", "json"])
            .stdout,
        original_report.stdout
    );
}

#[test]
fn automatic_sending_uses_machine_config_regardless_of_the_working_directory() {
    let repo = Repo::new();
    repo.program("agent", AGENT);
    let suite = repo.suite(&command_suite(1));
    let api = BenchApi::new();
    api.config(&repo, "");
    // A repository override cannot change or suppress the user's automatic upload destination.
    let outside = repo.dir.path().join("outside");
    fs::create_dir_all(outside.join(".cedr")).unwrap();
    fs::write(
        outside.join(".cedr/routes.toml"),
        "version=1\n[sync]\norg='other'\nauto=false\n",
    )
    .unwrap();
    let task = api.respond(vec![(200, false)]);
    let out = repo
        .cedr(&run_args(&suite))
        .current_dir(outside)
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let bodies = task.join().unwrap();
    assert_eq!(bodies[0]["org"], "acme");
    assert_eq!(bodies[0]["result"]["id"], stdout_json(&out)["id"]);
    assert_eq!(bodies[0]["repo"], "repo");
}

#[test]
fn a_tracked_file_named_head_does_not_hide_the_root_commit() {
    let repo = Repo::new();
    fs::write(repo.root().join("HEAD"), "not a ref\n").unwrap();
    git(&repo.root(), &["add", "HEAD"]);
    git(&repo.root(), &["commit", "-qm", "add a file named HEAD"]);
    let result = seed_result(&repo);
    assert!(result["source"]["root_commit"].is_string());
}

#[test]
fn bare_push_requires_matching_names_and_roots_including_from_a_linked_worktree() {
    let repo = Repo::new();
    let result = seed_result(&repo);
    let root = result["source"]["root_commit"].as_str().unwrap();
    assert_eq!(result["variants"]["baseline"]["commit"], root);
    store_result(&repo, "b-1-same-directory", &result);
    let other = Repo::new();
    git(&other.root(), &["checkout", "-q", "--orphan", "unrelated"]);
    other.write("AGENTS.md", "an independent repository\n");
    other.commit("independent root");
    let unrelated = seed_result(&other);
    assert_eq!(unrelated["source"]["repo"], result["source"]["repo"]);
    assert_ne!(unrelated["source"]["root_commit"], root);
    store_result(&repo, "b-2-other-directory", &unrelated);
    let mut legacy = result.clone();
    legacy["source"]
        .as_object_mut()
        .unwrap()
        .remove("root_commit");
    store_result(&repo, "b-3-no-root", &legacy);
    let shown = repo.run(&["bench", "show", "b-3-no-root", "--format", "json"]);
    assert_eq!(code(&shown), 0);
    assert!(stdout_json(&shown)["source"]["root_commit"].is_null());
    let api = BenchApi::new();
    let config = api.config(&repo, "");
    let args = ["bench", "push", "--config", &config, "--format", "json"];
    let task = api.respond(vec![(200, false)]);
    let out = repo
        .cedr(&args)
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0);
    assert_eq!(
        stdout_json(&out)["pushed"],
        serde_json::json!(["b-1-same-directory"])
    );
    assert_eq!(stdout_json(&out)["skipped"], 2);
    assert!(task.join().unwrap()[0]["result"].get("source").is_none());
    let out = repo
        .cedr(&args)
        .args(["--repo", "repo", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(
        stdout_json(&out),
        serde_json::json!([]),
        "--repo still checks the current root"
    );
    let out = repo
        .cedr(&args)
        .current_dir(repo.dir.path())
        .args(["--repo", "repo", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 2, "outside git, a name alone selects nothing");
    let out = repo
        .cedr(&args)
        .args(["--all", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(
        stdout_json(&out).as_array().unwrap().len(),
        2,
        "--all reaches the legacy result"
    );
    let out = repo
        .cedr(&args)
        .args(["b-3-no-root", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(stdout_json(&out)[0]["result"]["id"], "b-3-no-root");

    git(
        &repo.root(),
        &["remote", "add", "origin", "https://github.com/Acme/API.git"],
    );
    let network = seed_result(&repo);
    store_result(&repo, "b-4-same-network", &network);
    git(
        &other.root(),
        &[
            "remote",
            "add",
            "origin",
            "https://gitlab.example.invalid/acme/api.git",
        ],
    );
    let other_network = seed_result(&other);
    store_result(&repo, "b-5-other-network", &other_network);
    let mut fork = network.clone();
    fork["source"]["repo"] = "fork/api".into();
    store_result(&repo, "b-6-fork", &fork);
    // A linked worktree shares the history and origin even when its directory name differs.
    git(
        &repo.root(),
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/api.git",
        ],
    );
    let worktree = repo.dir.path().join("linked");
    git(
        &repo.root(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            worktree.to_str().unwrap(),
        ],
    );
    let task = api.respond(vec![(200, false)]);
    let out = repo
        .cedr(&args)
        .current_dir(&worktree)
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        stdout_json(&out)["pushed"],
        serde_json::json!(["b-4-same-network"])
    );
    assert_eq!(stdout_json(&out)["skipped"], 4);
    let bodies = task.join().unwrap();
    assert_eq!(
        bodies[0]["repo"], "Acme/API",
        "matching ignores ASCII case and preserves the saved name"
    );
    assert!(bodies[0]["result"].get("source").is_none());
    api.assert_no_request();
}

#[test]
fn bench_push_retries_busy_organizations_and_stops_after_three_attempts() {
    let repo = Repo::new();
    let result = seed_result(&repo);
    store_result(&repo, "b-retry", &result);
    let api = BenchApi::new();
    let config = api.config(&repo, "");
    let args = [
        "bench", "push", "b-retry", "--config", &config, "--format", "json",
    ];
    let task = api.respond_with_retry_after(vec![(503, false, Some("1")), (200, false, None)]);
    let out = repo
        .cedr(&args)
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let bodies = task.join().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0], bodies[1]);
    assert_eq!(stdout_json(&out)["pushed"], serde_json::json!(["b-retry"]));
    let marker = repo.dir.path().join("state/cedr/bench-pushed-acme.txt");
    let before = fs::read(&marker).unwrap();
    let task = api.respond_with_retry_after(vec![
        (503, false, None),
        (503, false, Some("not-seconds")),
        (503, false, Some("1")),
    ]);
    let out = repo
        .cedr(&args)
        .env("CEDR_API_KEY", "test-key")
        .output()
        .unwrap();
    assert_eq!(code(&out), 2);
    let bodies = task.join().unwrap();
    assert_eq!(bodies.len(), 3);
    assert!(bodies.iter().all(|b| b == &bodies[0]));
    assert!(
        stdout_json(&out)["failed"][0]["error"]
            .as_str()
            .unwrap()
            .contains("503")
    );
    assert_eq!(
        fs::read(marker).unwrap(),
        before,
        "failed retries never mark the result sent"
    );
    api.assert_no_request();
}

#[test]
fn cleaned_required_fields_use_unknown_without_changing_saved_results() {
    let repo = Repo::new();
    let result = seed_result(&repo);
    let api = BenchApi::new();
    let config = api.config(&repo, "");
    for model in ["~/models/foo/", "/", r"C:\"] {
        let mut result = result.clone();
        result["suite"]["name"] = "\n\t".into();
        result["variants"]["baseline"]["model"] = model.into();
        result["variants"]["baseline"]["ref"] = "\n\u{1b}".into();
        store_result(&repo, "b-empty", &result);
        let path = repo.dir.path().join("state/cedr/bench/b-empty.json");
        let before = fs::read(&path).unwrap();
        let task = api.respond(vec![(200, false)]);
        let out = repo
            .cedr(&["bench", "push", "b-empty", "--config", &config])
            .env("CEDR_API_KEY", "test-key")
            .output()
            .unwrap();
        assert_eq!(code(&out), 0);
        let bodies = task.join().unwrap();
        let sent = &bodies[0]["result"];
        assert_eq!(sent["suite"]["name"], "unknown");
        assert_eq!(sent["variants"]["baseline"]["ref"], "unknown");
        assert_eq!(
            sent["variants"]["baseline"]["model"],
            if model == "~/models/foo/" {
                "foo"
            } else {
                "unknown"
            }
        );
        assert_eq!(fs::read(path).unwrap(), before);
    }
}
