//! The harnesses a variant can run (docs/bench.md, Harnesses): the command line for each, and the
//! outcome read back from its output. The parsers read output the agent's run produced, so anything
//! unexpected reads as an unknown cost or an error, never a panic.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::exec;
use crate::suite::{HarnessKind, Permissions, Variant};

/// What a harness reported about a finished run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentEnd {
    Finished,
    /// The harness reports a failed run.
    Error,
    /// The harness stopped at the budget limit it was given.
    BudgetLimit,
}

/// The argv's program, for presence checks and version probes.
pub fn program(variant: &Variant) -> &str {
    match variant.harness {
        HarnessKind::Claude => "claude",
        HarnessKind::Codex => "codex",
        HarnessKind::Command => variant
            .command
            .as_ref()
            .and_then(|argv| argv.first())
            .map_or("", String::as_str),
    }
}

/// Whether a run's cost can be known: the budget cannot see an unpriced variant.
pub fn priced(variant: &Variant) -> bool {
    match variant.harness {
        HarnessKind::Claude => true,
        HarnessKind::Codex => cedr_monitor::pricing::price(&variant.model).is_some(),
        HarnessKind::Command => false,
    }
}

/// The agent's command, with the prompt to send on stdin when the harness reads it from there. The
/// vendor CLIs read stdin so that a prompt can never be taken for a flag or hit an argv limit.
pub fn command(
    variant: &Variant,
    prompt: &str,
    prompt_file: &Path,
    budget_left: f64,
) -> Option<(Command, Option<String>)> {
    match variant.harness {
        HarnessKind::Claude => {
            let mut cmd = Command::new(exec::find_program("claude")?);
            cmd.args([
                "-p",
                "--output-format",
                "json",
                "--no-session-persistence",
                "--setting-sources",
                "project,local",
                "--model",
                &variant.model,
                "--permission-mode",
                match variant.permissions {
                    Permissions::Workspace => "acceptEdits",
                    Permissions::Full => "bypassPermissions",
                },
                "--max-budget-usd",
                &format!("{budget_left:.4}"),
            ]);
            if let Some(effort) = &variant.effort {
                cmd.args(["--effort", effort]);
            }
            Some((cmd, Some(prompt.to_string())))
        }
        HarnessKind::Codex => {
            let mut cmd = Command::new(exec::find_program("codex")?);
            cmd.args([
                "exec",
                "--json",
                "--ephemeral",
                "--ignore-user-config",
                "--skip-git-repo-check",
                "-m",
                &variant.model,
            ]);
            match variant.permissions {
                Permissions::Workspace => cmd.args(["--sandbox", "workspace-write"]),
                Permissions::Full => cmd.arg("--dangerously-bypass-approvals-and-sandbox"),
            };
            if let Some(effort) = &variant.effort {
                cmd.args(["-c", &format!("model_reasoning_effort=\"{effort}\"")]);
            }
            cmd.arg("-");
            Some((cmd, Some(prompt.to_string())))
        }
        HarnessKind::Command => {
            let argv = variant.command.as_ref()?;
            let fill = |arg: &String| {
                arg.replace("{prompt_file}", &prompt_file.to_string_lossy())
                    .replace("{model}", &variant.model)
                    .replace("{effort}", variant.effort.as_deref().unwrap_or(""))
            };
            let mut cmd = Command::new(exec::find_program(&argv[0])?);
            cmd.args(argv[1..].iter().map(fill));
            Some((cmd, None))
        }
    }
}

/// Reads a finished run from its stdout and whether the process exited cleanly.
pub fn parse(variant: &Variant, stdout: &[u8], exited_ok: bool) -> (AgentEnd, Usage) {
    let text = String::from_utf8_lossy(stdout);
    let (end, usage) = match variant.harness {
        HarnessKind::Claude => parse_claude(&text),
        HarnessKind::Codex => parse_codex(&text, &variant.model),
        HarnessKind::Command => (AgentEnd::Finished, parse_command(&text)),
    };
    let end = match end {
        AgentEnd::Finished if !exited_ok => AgentEnd::Error,
        other => other,
    };
    (end, usage)
}

fn token(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn add(total: &mut Option<u64>, n: Option<u64>) {
    if let Some(n) = n {
        *total = Some(total.unwrap_or(0).saturating_add(n));
    }
}

/// A cost a harness reports: a finite, non-negative number, since this total decides when a run stops.
fn cost(value: Option<f64>) -> Option<f64> {
    value.filter(|c| c.is_finite() && *c >= 0.0)
}

fn parse_claude(text: &str) -> (AgentEnd, Usage) {
    let parsed: Option<Value> = serde_json::from_str(text.trim()).ok();
    let result = match &parsed {
        Some(Value::Array(events)) => events.iter().rev().find(|e| e["type"] == "result"),
        Some(value) => Some(value),
        None => None,
    };
    let Some(result) = result else {
        return (AgentEnd::Error, Usage::default());
    };
    let mut usage = Usage {
        cost_usd: cost(result.get("total_cost_usd").and_then(Value::as_f64)),
        ..Usage::default()
    };
    // modelUsage covers subagents as well; top-level usage is only the last request's context.
    if let Some(models) = result.get("modelUsage").and_then(Value::as_object) {
        for (model, row) in models {
            usage.models.push(model.clone());
            add(&mut usage.input_tokens, token(row, "inputTokens"));
            add(&mut usage.output_tokens, token(row, "outputTokens"));
            add(&mut usage.cached_tokens, token(row, "cacheReadInputTokens"));
            add(
                &mut usage.cached_tokens,
                token(row, "cacheCreationInputTokens"),
            );
        }
    } else if let Some(row) = result.get("usage") {
        usage.input_tokens = token(row, "input_tokens");
        usage.output_tokens = token(row, "output_tokens");
        add(
            &mut usage.cached_tokens,
            token(row, "cache_read_input_tokens"),
        );
        add(
            &mut usage.cached_tokens,
            token(row, "cache_creation_input_tokens"),
        );
    }
    let subtype = result.get("subtype").and_then(Value::as_str).unwrap_or("");
    let end = if subtype == "error_max_budget_usd" {
        AgentEnd::BudgetLimit
    } else if result["is_error"] == true || subtype != "success" {
        AgentEnd::Error
    } else {
        AgentEnd::Finished
    };
    (end, usage)
}

fn parse_codex(text: &str, model: &str) -> (AgentEnd, Usage) {
    let mut usage = Usage {
        models: vec![model.to_string()],
        ..Usage::default()
    };
    let (mut input, mut cached, mut write, mut output) = (None, None, None, None);
    let mut completed = false;
    let mut failed = false;
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match event["type"].as_str() {
            Some("turn.completed") => {
                completed = true;
                let row = &event["usage"];
                add(&mut input, token(row, "input_tokens"));
                add(&mut cached, token(row, "cached_input_tokens"));
                add(&mut write, token(row, "cache_write_input_tokens"));
                add(&mut output, token(row, "output_tokens"));
            }
            Some("turn.failed" | "error") => failed = true,
            _ => {}
        }
    }
    // input_tokens includes the cached share, as Monitor's Codex source reads it.
    let (read, written) = (cached.unwrap_or(0), write.unwrap_or(0));
    let fresh = input.map(|n: u64| n.saturating_sub(read).saturating_sub(written));
    usage.input_tokens = fresh;
    usage.output_tokens = output;
    if cached.is_some() || write.is_some() {
        usage.cached_tokens = Some(read.saturating_add(written));
    }
    if let (Some(fresh), Some(output), Some(price)) =
        (fresh, output, cedr_monitor::pricing::price(model))
    {
        usage.cost_usd = Some(
            (fresh as f64 * price.input
                + read as f64 * price.cache_read
                + written as f64 * price.cache_write5m
                + output as f64 * price.output)
                / 1_000_000.0,
        );
    }
    let end = if failed || !completed {
        AgentEnd::Error
    } else {
        AgentEnd::Finished
    };
    (end, usage)
}

/// A `command` may end its stdout with `{"cost_usd": n, "input_tokens": n, "output_tokens": n,
/// "cached_tokens": n, "models": [..]}`; anything else leaves the trial unpriced.
fn parse_command(text: &str) -> Usage {
    let last = text.lines().rev().find(|l| !l.trim().is_empty());
    let Some(Value::Object(row)) = last.and_then(|l| serde_json::from_str(l).ok()) else {
        return Usage::default();
    };
    let row = Value::Object(row);
    Usage {
        input_tokens: token(&row, "input_tokens"),
        output_tokens: token(&row, "output_tokens"),
        cached_tokens: token(&row, "cached_tokens"),
        cost_usd: cost(row.get("cost_usd").and_then(Value::as_f64)),
        models: row
            .get("models")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|m| m.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shapes captured from the real CLIs, reduced to the fields read here.
    const CLAUDE_OK: &str = r#"{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.00558663,
      "usage":{"input_tokens":4,"cache_creation_input_tokens":24634,"cache_read_input_tokens":47223,"output_tokens":120},
      "modelUsage":{"claude-haiku-5-5":{"inputTokens":1191,"outputTokens":137,"cacheReadInputTokens":47223,"cacheCreationInputTokens":24634,"costUSD":0.00558663}}}"#;
    const CODEX_OK: &str = concat!(
        r#"{"type":"thread.started","thread_id":"x"}"#,
        "\n",
        r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"{\"type\":\"turn.failed\"}"}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":60646,"cached_input_tokens":51456,"cache_write_input_tokens":0,"output_tokens":89,"reasoning_output_tokens":0}}"#,
        "\n"
    );

    fn variant(harness: HarnessKind, model: &str) -> Variant {
        Variant {
            harness,
            model: model.into(),
            effort: None,
            git_ref: None,
            permissions: Permissions::Workspace,
            command: None,
        }
    }

    #[test]
    fn claude_reads_the_reported_cost_and_tokens_across_models() {
        let (end, usage) = parse(
            &variant(HarnessKind::Claude, "x"),
            CLAUDE_OK.as_bytes(),
            true,
        );
        assert_eq!(end, AgentEnd::Finished);
        assert_eq!(usage.cost_usd, Some(0.00558663));
        assert_eq!(usage.input_tokens, Some(1191));
        assert_eq!(usage.cached_tokens, Some(47223 + 24634));
        assert_eq!(usage.models, ["claude-haiku-5-5"]);
    }

    #[test]
    fn claude_tells_its_budget_limit_from_a_failure() {
        let limit = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"total_cost_usd":0.5}"#;
        let v = variant(HarnessKind::Claude, "x");
        let (end, usage) = parse(&v, limit.as_bytes(), false);
        assert_eq!(end, AgentEnd::BudgetLimit);
        assert_eq!(usage.cost_usd, Some(0.5));
        let failed = r#"{"type":"result","subtype":"error_during_execution","is_error":true}"#;
        assert_eq!(parse(&v, failed.as_bytes(), false).0, AgentEnd::Error);
        assert_eq!(parse(&v, b"not json", true).0, AgentEnd::Error);
        assert_eq!(parse(&v, CLAUDE_OK.as_bytes(), false).0, AgentEnd::Error);
    }

    #[test]
    fn codex_prices_fresh_cached_and_output_tokens_apart() {
        let v = variant(HarnessKind::Codex, "gpt-6.1-sol");
        let (end, usage) = parse(&v, CODEX_OK.as_bytes(), true);
        assert_eq!(end, AgentEnd::Finished);
        assert_eq!(usage.input_tokens, Some(60646 - 51456));
        assert_eq!(usage.cached_tokens, Some(51456));
        let p = cedr_monitor::pricing::price("gpt-6.1-sol").unwrap();
        let expected = (9190.0 * p.input + 51456.0 * p.cache_read + 89.0 * p.output) / 1_000_000.0;
        assert!((usage.cost_usd.unwrap() - expected).abs() < 1e-9);
    }

    #[test]
    fn codex_without_a_completed_turn_is_an_error_and_an_unknown_model_is_unpriced() {
        let v = variant(HarnessKind::Codex, "gpt-6.1-sol");
        assert_eq!(parse(&v, b"", true).0, AgentEnd::Error);
        let failed = r#"{"type":"turn.failed","error":{"message":"m"}}"#;
        assert_eq!(parse(&v, failed.as_bytes(), true).0, AgentEnd::Error);
        let unknown = variant(HarnessKind::Codex, "no-such-model");
        assert!(!priced(&unknown));
        assert_eq!(parse(&unknown, CODEX_OK.as_bytes(), true).1.cost_usd, None);
    }

    #[test]
    fn a_reported_cost_that_could_hide_spend_is_treated_as_unknown() {
        let v = variant(HarnessKind::Command, "m");
        let usage = |text: &str| parse(&v, text.as_bytes(), true).1;
        assert_eq!(usage("working\n{\"cost_usd\": 0.5}\n").cost_usd, Some(0.5));
        assert_eq!(usage("{\"cost_usd\": -5}").cost_usd, None);
        assert_eq!(usage("no usage here").cost_usd, None);
        let c = variant(HarnessKind::Claude, "m");
        let negative =
            r#"{"type":"result","subtype":"success","is_error":false,"total_cost_usd":-1}"#;
        assert_eq!(parse(&c, negative.as_bytes(), true).1.cost_usd, None);
    }
}
