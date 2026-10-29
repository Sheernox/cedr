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
