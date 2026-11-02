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
