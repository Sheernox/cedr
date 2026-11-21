//! Run the Council CLI in a scratch repository with fake vendor CLIs.
#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::{Scratch, stdout};
use serde_json::{Value, json};

const CLEAN: &str = "No findings.\nScore: 9/10\nEnd of review.";
const RUN_CONFIG: &str = r#"
version = 1

[harnesses.claude]
models = ["claude-test"]
billing = "api"

[harnesses.codex]
models = ["gpt-test"]
billing = "api"

[harnesses.muse]
models = ["muse-test"]
billing = "api"

[routes.council]
council = true
candidates = [
  { target = "claude/claude-test", effort = "high" },
  { target = "codex/gpt-test", effort = "high" },
]

[routes.spare]
council = true
candidates = [
  { target = "codex/gpt-test" },
  { target = "muse/muse-test" },
  { target = "claude/claude-test", spare = true },
]

[routes.ci]
council = true
quorum = 1
candidates = [
  { target = "codex/gpt-test" },
  { target = "claude/claude-test", spare = true },
]
"#;

struct Run {
    scratch: Scratch,
    root: PathBuf,
    tmp: PathBuf,
    bin: PathBuf,
    out: PathBuf,
}

impl Run {
    fn new() -> Self {
        let scratch = Scratch::new(0.0, 0.0);
        let root = scratch.dir.path().join("repo");
        let tmp = scratch.dir.path().join("tmp");
        let bin = scratch.dir.path().join("bin");
        let out = scratch.dir.path().join("reports");
        for path in [&root, &tmp, &bin] {
            fs::create_dir(path).unwrap();
        }
        fs::write(scratch.dir.path().join("routes.toml"), RUN_CONFIG).unwrap();
        let run = Self {
            scratch,
            root,
            tmp,
            bin,
            out,
        };
        run.git(&["init", "-q", "-b", "main"]);
        fs::write(run.root.join("data,here.txt"), "base\n").unwrap();
        run.commit("base");
        run.git(&["switch", "-qc", "change"]);
        fs::write(run.root.join("data,here.txt"), "reviewed version\n").unwrap();
        run.commit("change");
        fs::write(run.root.join("data,here.txt"), "dirty checkout\n").unwrap();
        fs::write(run.root.join("untracked.txt"), "private file\n").unwrap();
        run
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(&self.root)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "--", "data,here.txt"]);
        self.git(&["commit", "-qm", message]);
    }

    fn command(&self, route: &str, flags: &[&str]) -> Command {
        let dir = self.scratch.dir.path();
        let mut path = vec![self.bin.clone()];
        path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let mut cmd = self.scratch.command(&["council", "run", route, "--config"]);
        cmd.arg(dir.join("routes.toml"))
            .args(flags)
            .current_dir(&self.root)
            .env("HOME", dir.join("home"))
            .env("USERPROFILE", dir.join("home"))
            .env("CODEX_HOME", dir.join("codex"))
            .env("XDG_DATA_HOME", dir.join("data"))
            .env("TMPDIR", &self.tmp)
            .env("PATH", std::env::join_paths(path).unwrap())
            .env("CASE_DIR", dir)
            .env("CEDR_TELEMETRY", "0")
            .env("CEDR_NO_AUTO_UPDATE", "1")
            .env("ANTHROPIC_API_KEY", "fake-anthropic-key")
            .env("CODEX_API_KEY", "fake-codex-key");
        cmd
    }

    fn run(&self, route: &str, flags: &[&str]) -> Output {
        self.command(route, flags)
            .arg("--out")
            .arg(&self.out)
            .output()
            .unwrap()
    }

    fn agent(&self, name: &str, reply: &str, before: &str, after: &str) {
        let (flag, mode, key, event) = match name {
            "claude" => (
                "--permission-mode",
                "plan",
                "ANTHROPIC_API_KEY",
                json!({"type":"result", "subtype":"success", "result":reply}).to_string(),
            ),
            "codex" => (
                "--sandbox",
                "read-only",
                "CODEX_API_KEY",
                format!(
                    "{}\n{}",
                    json!({"type":"item.completed","item":{"type":"agent_message","text":reply}}),
                    json!({"type":"turn.completed", "usage":{}})
                ),
            ),
            _ => unreachable!(),
        };
        let program = self.bin.join(name);
        fs::write(
            &program,
            format!(
                "#!/bin/sh\nset -eu\n                 case \" $* \" in *' {flag} {mode} '*) ;; *) exit 42 ;; esac\n                 test \x22${key}\x22 = fake-{key_name}-key\n                 test \"$(cat data,here.txt)\" = 'reviewed version'\n                 test ! -e untracked.txt && test ! -e .git\n                 cat > \"$CASE_DIR/{name}.prompt\"\n                 pwd > \"$CASE_DIR/{name}.cwd\"\n                 {before}\n                 printf '%s\\n' '{}'\n                 {after}\n",
                event.replace('\'', "'\\''"),
                key_name = if name == "claude" { "anthropic" } else { "codex" },
            ),
        )
        .unwrap();
        fs::set_permissions(program, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.scratch.dir.path().join(rel)).unwrap()
    }

    fn history(&self) -> Vec<Value> {
        self.read("state/cedr/route-history.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn check_copies_removed(&self) {
        let claude = self.read("claude.cwd");
        let codex = self.read("codex.cwd");
        assert_ne!(claude, codex);
        for dir in [claude, codex] {
            assert!(dir.trim().starts_with(self.tmp.to_str().unwrap()));
            assert!(!std::path::Path::new(dir.trim()).exists());
        }
        assert_eq!(fs::read_dir(&self.tmp).unwrap().count(), 0);
        assert_eq!(
            fs::read_to_string(self.root.join("data,here.txt")).unwrap(),
            "dirty checkout\n"
        );
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_gate(out: &Output, code: i32, gate: &str) -> Value {
    assert_eq!(out.status.code(), Some(code), "{}", stderr(out));
    let gate_json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(gate_json["gate"], gate);
    assert_eq!(gate_json["strict"], true);
    gate_json
}

#[test]
fn two_members_run_in_parallel_and_record_one_strict_gate() {
    let run = Run::new();
    let fifo = run.scratch.dir.path().join("ready");
    assert!(Command::new("mkfifo").arg(fifo).status().unwrap().success());
    run.agent(
        "claude",
        CLEAN,
        "printf 'ready\\n' > \"$CASE_DIR/ready\"",
        "",
    );
    run.agent(
        "codex",
        CLEAN,
        "read ready < \"$CASE_DIR/ready\"; test \"$ready\" = ready",
        "",
    );
    let out = run.run("council", &["--format", "json", "--timeout", "5"]);
    let gate = assert_gate(&out, 0, "pass");
    assert_eq!(
        gate["reviewers"],
        json!(["claude/claude-test", "codex/gpt-test"])
    );
    assert_eq!(run.read("reports/claude-claude-test.md"), CLEAN);
    assert_eq!(run.read("reports/codex-gpt-test.md"), CLEAN);

    let prompt = run.read("claude.prompt");
    assert_eq!(prompt, run.read("codex.prompt"));
    for phrase in [
        "# Run a Cedr Council",
        "<file>:<line>",
        "End of review.",
        "-base",
        "+reviewed version",
    ] {
        assert!(prompt.contains(phrase), "{phrase} missing from {prompt}");
    }
    assert!(!prompt.contains("dirty checkout"));

    let records = run.history();
    assert_eq!(records.len(), 3);
    let run_id = &records[0]["run"];
    assert!(run_id.as_str().unwrap().starts_with("r-"));
    assert!(records.iter().all(|record| &record["run"] == run_id));
    assert_eq!(records[2]["id"], gate["id"]);
    run.check_copies_removed();
}

#[test]
fn run_and_merge_share_machine_formats_and_keep_the_gate() {
    for format in ["github", "sarif", "gitlab"] {
        let run = Run::new();
        run.agent(
            "claude",
            "data,here.txt:1 - P1 - rejects 100% of inputs",
            "",
            "",
        );
        run.agent("codex", CLEAN, "", "");
        let json_path = run.scratch.dir.path().join("merged.json");
        let out = run.run(
            "council",
            &[
                "--format",
                format,
                "--diff",
                "main..HEAD",
                "--report-json",
                json_path.to_str().unwrap(),
            ],
        );
        assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
        let merged = run
            .scratch
            .command(&[
                "council",
                "merge",
                "--no-record",
                "--strict",
                "--format",
                format,
            ])
            .arg(format!(
                "claude/claude-test={}",
                run.out.join("claude-claude-test.md").display()
            ))
            .arg(format!(
                "codex/gpt-test={}",
                run.out.join("codex-gpt-test.md").display()
            ))
            .output()
            .unwrap();
        assert_eq!(merged.status.code(), Some(1), "{}", stderr(&merged));
        assert_eq!(out.stdout, merged.stdout, "{format}");
        let json: Value = serde_json::from_slice(&fs::read(json_path).unwrap()).unwrap();
        assert_eq!(json["gate"], "block");
        assert_eq!(json["groups"][0]["line"], 1);
        assert!(stderr(&out).contains(run.out.join("revision-brief.md").to_str().unwrap()));
        assert!(
            run.read("reports/revision-brief.md")
                .contains("rejects 100% of inputs")
        );
    }
}

#[test]
fn ci_reviews_a_pinned_head_and_replaces_a_member_without_a_key() {
    let run = Run::new();
    run.git(&["branch", "reviewed"]);
    run.commit("new head");
    run.agent("claude", CLEAN, "", "");
    run.agent("codex", CLEAN, "exit 42", "");
    let out = run
        .command(
            "ci",
            &[
                "--head",
                "reviewed",
                "--require-api-keys",
                "--format",
                "json",
            ],
        )
        .env_remove("OPENAI_API_KEY")
        .arg("--out")
        .arg(&run.out)
        .output()
        .unwrap();
    let gate = assert_gate(&out, 0, "pass");
    assert_eq!(gate["reviewers"], json!(["claude/claude-test"]));
    assert!(stderr(&out).contains("skipped codex: OPENAI_API_KEY is missing"));
    assert_eq!(
        gate["spares"],
        json!({"claude/claude-test":"codex/gpt-test"})
    );
    assert!(run.read("claude.prompt").contains("+reviewed version"));
    assert!(!run.scratch.dir.path().join("codex.prompt").exists());
}

#[test]
fn ci_replaces_a_member_whose_cli_is_missing() {
    let run = Run::new();
    run.agent("claude", CLEAN, "", "");
    let path = std::env::var_os("PATH").unwrap();
    for name in ["git", "cat"] {
        let binary = std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
            .unwrap();
        std::os::unix::fs::symlink(binary, run.bin.join(name)).unwrap();
    }
    let out = run
        .command("ci", &["--require-api-keys", "--format", "json"])
        .env("PATH", &run.bin)
        .env("OPENAI_API_KEY", "fake-openai-key")
        .arg("--out")
        .arg(&run.out)
        .output()
        .unwrap();
    let gate = assert_gate(&out, 0, "pass");
    assert!(stderr(&out).contains("skipped codex: codex is not installed on PATH"));
    assert_eq!(
        gate["spares"],
        json!({"claude/claude-test":"codex/gpt-test"})
    );
}

#[test]
fn a_timed_out_reviewer_cannot_complete_the_gate() {
    let run = Run::new();
    run.agent("claude", CLEAN, "", "");
    run.agent("codex", CLEAN, "", "exec sleep 60");
    let out = run.run("council", &["--timeout", "1", "--format", "json"]);
    let gate = assert_gate(&out, 3, "incomplete");
    assert_eq!(gate["reviewers"], json!(["claude/claude-test"]));
    assert_eq!(gate["incomplete_reports"], json!(["codex/gpt-test"]));
    assert!(stderr(&out).contains("codex/gpt-test timed out after 1 seconds"));
    assert_eq!(run.read("reports/codex-gpt-test.md"), CLEAN);
    run.check_copies_removed();
}

#[test]
fn a_spare_replaces_a_harness_without_an_adapter() {
    let run = Run::new();
    run.agent("claude", CLEAN, "", "");
    run.agent("codex", CLEAN, "", "");
    let out = run.run("spare", &["--format", "json"]);
    let gate = assert_gate(&out, 0, "pass");
    assert!(stderr(&out).contains("skipped muse: no headless Council adapter"));
    assert!(stderr(&out).contains("claude/claude-test is the spare for muse/muse-test"));
    assert_eq!(
        gate["spares"],
        json!({"claude/claude-test":"muse/muse-test"})
    );
    assert_eq!(fs::read_dir(&run.out).unwrap().count(), 2);
}

#[test]
fn an_explicit_unsupported_member_has_no_replacement_and_records_incomplete() {
    let run = Run::new();
    let out = run.run("spare", &["--member", "muse/muse-test"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stdout(&out).starts_with("council: incomplete (0 of quorum 2 reviewed"));
    let records = run.history();
    assert_eq!(records.len(), 1);
    assert!(records[0]["run"].as_str().unwrap().starts_with("r-"));
    assert_eq!(fs::read_dir(&run.out).unwrap().count(), 0);
}

#[test]
fn ctrl_c_removes_the_export_after_stopping_the_review() {
    let run = Run::new();
    run.agent("claude", CLEAN, "exec sleep 60", "");
    let mut child = run
        .command(
            "council",
            &["--member", "claude/claude-test", "--timeout", "5"],
        )
        .args(["--out"])
        .arg(&run.out)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let marker = run.scratch.dir.path().join("claude.cwd");
    let start = Instant::now();
    while !marker.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "Council exited before the reviewer started"
        );
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "reviewer did not start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: the child handle names the live Council process started by this test.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
        0
    );
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("Council run interrupted"));
    assert_eq!(fs::read_dir(&run.tmp).unwrap().count(), 0);
}
