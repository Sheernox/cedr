//! `cedr council plan` exit codes and `cedr council merge` over text and JSON reports, and the
//! status line capture the claude-statusline reader depends on.

mod common;

use std::io::Write;
use std::process::Stdio;

use common::{Scratch, config, stdout};
use serde_json::{Value, json};

#[test]
fn a_plan_below_quorum_exits_3_and_still_names_the_members() {
    let s = Scratch::new(10.0, 95.0);
    let cfg = config().display().to_string();
    let readings = s.readings();
    let o = s.cedr(&[
        "council",
        "plan",
        "council",
        "--config",
        &cfg,
        "--readings",
        &readings,
        "--format",
        "json",
    ]);
    assert_eq!(
        o.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let plan: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        (plan["quorum"].as_u64(), plan["quorum_reachable"].as_bool()),
        (Some(2), Some(false))
    );
    assert_eq!(plan["delegates"][0]["harness"], "codex");
    let ok = Scratch::new(10.0, 20.0);
    let r = ok.readings();
    assert_eq!(
        ok.cedr(&[
            "council",
            "plan",
            "council",
            "--config",
            &cfg,
            "--readings",
            &r
        ])
        .status
        .code(),
        Some(0)
    );
    let not = ok.cedr(&[
        "council",
        "plan",
        "review.code",
        "--config",
        &cfg,
        "--readings",
        &r,
    ]);
    assert_eq!(not.status.code(), Some(2));
}

#[test]
fn merge_reads_text_and_json_reports_and_blocks_on_p1() {
    let s = Scratch::new(10.0, 20.0);
    let dir = s.dir.path();
    std::fs::write(
        dir.join("codex.txt"),
        "Findings:\n- `src/a.rs:12` - **P1** - reads until EOF - endless stdin\n- `src/b.rs:3` - **P3** - naming\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("claude.json"),
        json!({"member": "claude/opus", "findings": [{"file": "src/a.rs", "line": 10, "severity": "P2", "claim": "unbounded read"}]}).to_string(),
    )
    .unwrap();
    let codex = format!("codex/sol={}", dir.join("codex.txt").display());
    let claude = dir.join("claude.json").display().to_string();
    let o = s.cedr(&["council", "merge", &codex, &claude]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let out = stdout(&o);
    assert!(
        out.starts_with(
            "council: block (2 of quorum 2 reviewed, P1 and above block: claude/opus, codex/sol)\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("P1 src/a.rs:10 [claude/opus, codex/sol] reads until EOF\n"),
        "{out}"
    );
    assert!(
        out.contains("  claude/opus P2 :10 unbounded read\n"),
        "{out}"
    );
}

#[test]
fn merge_without_quorum_is_incomplete_and_text_needs_a_member() {
    let s = Scratch::new(10.0, 20.0);
    let path = s.dir.path().join("clean.json");
    std::fs::write(
        &path,
        json!({"member": "codex/sol", "findings": []}).to_string(),
    )
    .unwrap();
    let p = path.display().to_string();
    let cfg = config().display().to_string();
    assert_eq!(
        s.cedr(&[
            "council", "merge", &p, "--route", "council", "--config", &cfg
        ])
        .status
        .code(),
        Some(3)
    );
    assert_eq!(
        s.cedr(&["council", "merge", &p, "--quorum", "1"])
            .status
            .code(),
        Some(0)
    );
    let text = s.dir.path().join("r.txt");
    std::fs::write(&text, "src/a.rs:1 - P3 - x\n").unwrap();
    let o = s.cedr(&["council", "merge", &text.display().to_string()]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("needs <member>=<path>"));
}

#[test]
fn the_status_line_captures_rate_limits_and_can_wrap_another_command() {
    let s = Scratch::new(10.0, 20.0);
    let input = json!({"model": {"id": "claude-opus-5-5"}, "rate_limits": {
        "five_hour": {"used_percentage": 23.5, "resets_at": 4102444800i64},
        "seven_day": {"used_percentage": 41.2, "resets_at": 4102444800i64}}})
    .to_string();
    let run = |args: &[&str]| {
        let mut child = s
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let own = run(&["route", "statusline"]);
    assert_eq!(stdout(&own), "claude s24% w41%\n");
    let saved: Value = serde_json::from_slice(
        &std::fs::read(s.dir.path().join("state/cedr/readings/claude.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["harnesses"][0]["windows"][1]["id"], "seven_day");
    let failing = run(&[
        "route",
        "statusline",
        "--",
        "sh",
        "-c",
        "cat >/dev/null; exit 1",
    ]);
    assert_eq!(
        stdout(&failing),
        "claude s24% w41%\n",
        "a failing command falls back to Cedr's line"
    );
    let wrapped = run(&["route", "statusline", "--", "sh", "-c", "wc -c | tr -d ' '"]);
    assert_eq!(
        stdout(&wrapped).trim(),
        input.len().to_string(),
        "the wrapped command gets the same input"
    );
}

#[test]
fn a_truncated_json_report_is_an_error_even_with_a_member() {
    let s = Scratch::new(10.0, 20.0);
    let path = s.dir.path().join("cut.json");
    std::fs::write(&path, r#"{"member": "codex", "findings": [{"file": "a.rs", "line": 1, "severity": "P1", "claim": "x"#).unwrap();
    let arg = format!("codex={}", path.display());
    let o = s.cedr(&["council", "merge", &arg, "--quorum", "1"]);
    assert_eq!(o.status.code(), Some(2), "{}", stdout(&o));
    assert!(String::from_utf8_lossy(&o.stderr).contains("not a valid JSON report"));
}

#[test]
fn a_blocked_merge_prints_the_revision_brief_until_the_rounds_run_out() {
    let s = Scratch::new(10.0, 20.0);
    let path = s.dir.path().join("r.txt");
    std::fs::write(&path, "src/a.rs:12 - P1 - reads until EOF\n").unwrap();
    let arg = format!("codex/sol={}", path.display());
    let text = s.cedr(&["council", "merge", &arg, "--quorum", "1"]);
    assert!(stdout(&text).ends_with(
        "next: revise (round 1 of 1): give the author the revision brief (--format brief), then review again with --round 2\n"
    ), "{}", stdout(&text));
    let brief = s.cedr(&[
        "council", "merge", &arg, "--quorum", "1", "--format", "brief",
    ]);
    assert_eq!(brief.status.code(), Some(1));
    assert!(stdout(&brief).starts_with("# Council revision brief (round 1 of 1)\n"));
    assert!(
        stdout(&brief)
            .ends_with("## P1 src/a.rs:12 (codex/sol)\n- codex/sol P1 :12 reads until EOF\n")
    );
    let last = s.cedr(&["council", "merge", &arg, "--quorum", "1", "--round", "2"]);
    assert_eq!(last.status.code(), Some(1));
    assert!(stdout(&last).ends_with("next: done\n"));
}

#[test]
fn plan_shares_run_and_merge_records_counts_or_opts_out() {
    let s = Scratch::new(10.0, 20.0);
    let cfg = config().display().to_string();
    let readings = s.readings();
    let plan = s.cedr(&[
        "council",
        "plan",
        "council",
        "--config",
        &cfg,
        "--readings",
        &readings,
        "--format",
        "json",
    ]);
    let p: Value = serde_json::from_slice(&plan.stdout).unwrap();
    let run = p["run"].as_str().unwrap();
    let path = s.dir.path().join("state/cedr/route-history.jsonl");
    let records: Vec<Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|r| r["run"] == run));
    let report = s.dir.path().join("report.json");
    std::fs::write(
        &report,
        json!({"member":"codex/sol","findings":[]}).to_string(),
    )
    .unwrap();
    let report = report.display().to_string();
    let o = s.cedr(&[
        "council", "merge", &report, "--run", run, "--quorum", "1", "--format", "json",
    ]);
    assert_eq!(o.status.code(), Some(0));
    let merge: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(merge["id"].as_str().unwrap().starts_with("c-"));
    let history: Value = serde_json::from_slice(
        &s.cedr(&["route", "history", "--limit", "1", "--format", "json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(history[0]["id"], merge["id"]);
    assert_eq!(history[0]["run"], run);
    assert!(history[0]["route"].is_null());
    assert_eq!(history[0]["findings"], json!({"codex/sol":0}));
    let before = std::fs::read(&path).unwrap();
    let o = s.cedr(&[
        "council",
        "merge",
        &report,
        "--no-record",
        "--format",
        "json",
    ]);
    let merge: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(merge["id"].is_null());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        s.cedr(&["council", "merge", &report, "--run", "r-unknown"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        s.cedr(&[
            "council",
            "merge",
            &report,
            "--run",
            "r-unknown",
            "--no-record"
        ])
        .status
        .code(),
        Some(2)
    );
    let stats: Value =
        serde_json::from_slice(&s.cedr(&["council", "stats", "--format", "json"]).stdout).unwrap();
    assert_eq!(stats[0]["merges"], 1);
    assert_eq!(stats[0]["pass"], 1);
    assert_eq!(
        s.cedr(&["council", "stats", "--since", "invalid"])
            .status
            .code(),
        Some(2)
    );
    let text = stdout(&s.cedr(&[
        "council",
        "plan",
        "council",
        "--config",
        &cfg,
        "--readings",
        &readings,
    ]));
    assert!(text.contains("quorum 2: 2 eligible, run r-"));
}
