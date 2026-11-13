//! Vendor output normalization. Missing counts are never synthesized as zero.
use crate::{
    Result,
    report::{Tokens, Usage},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    Claude,
    Codex,
    Muse,
    Grok,
    Gemini,
    Antigravity,
    Command,
}
impl Harness {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Muse => "muse",
            Self::Grok => "grok",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
            Self::Command => "command",
        }
    }
}
impl std::str::FromStr for Harness {
    type Err = String;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "muse" => Ok(Self::Muse),
            "grok" => Ok(Self::Grok),
            "gemini" => Ok(Self::Gemini),
            "antigravity" => Ok(Self::Antigravity),
            "command" => Ok(Self::Command),
            _ => Err(
                "unknown harness; choose claude, codex, muse, grok, gemini, antigravity or command"
                    .into(),
            ),
        }
    }
}
fn n(v: &Value, k: &str) -> Option<u64> {
    v.get(k)?.as_u64()
}
fn add(a: &mut Option<u64>, b: Option<u64>) {
    if let Some(b) = b {
        *a = Some(a.unwrap_or(0).saturating_add(b));
    }
}
fn merge(a: &mut Tokens, b: &Tokens) {
    add(&mut a.input_tokens, b.input_tokens);
    add(&mut a.cache_read_tokens, b.cache_read_tokens);
    add(&mut a.cache_write_tokens, b.cache_write_tokens);
    add(&mut a.output_tokens, b.output_tokens);
    add(&mut a.reasoning_tokens, b.reasoning_tokens);
}
fn priced(model: &str, t: &Tokens) -> Option<f64> {
    let (p, _) = cedr_monitor::pricing::price(model)?;
    Some(
        (t.input_tokens? as f64 * p.input
            + t.cache_read_tokens.unwrap_or(0) as f64 * p.cache_read
            + t.cache_write_tokens.unwrap_or(0) as f64 * p.cache_write5m
            + t.output_tokens? as f64 * p.output)
            / 1_000_000.0,
    )
}
fn snake(v: &Value) -> Tokens {
    Tokens {
        input_tokens: n(v, "input_tokens"),
        cache_read_tokens: n(v, "cache_read_input_tokens"),
        cache_write_tokens: n(v, "cache_creation_input_tokens"),
        output_tokens: n(v, "output_tokens"),
        reasoning_tokens: n(v, "reasoning_tokens"),
    }
}
/// Cost follows the full per-model usage, never the vendor's subscription bill.
pub fn parse(h: Harness, text: &[u8], model: &str) -> Usage {
    let text = String::from_utf8_lossy(text);
    let whole: Option<Value> = serde_json::from_str(text.trim()).ok();
    let events: Vec<Value> = if let Some(Value::Array(a)) = &whole {
        a.clone()
    } else if let Some(v) = &whole {
        vec![v.clone()]
    } else {
        text.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    };
    let mut out = Usage::default();
    let mut rows = Vec::new();
    let mut reasoning_fallback = None;
    match h {
        Harness::Claude | Harness::Grok => {
            if let Some(v) = events
                .iter()
                .rev()
                .find(|v| v.get("usage").is_some() || v.get("modelUsage").is_some())
            {
                reasoning_fallback = n(&v["usage"], "reasoning_tokens");
                out.turns = n(v, "num_turns");
                out.tools = n(v, "tool_count");
                if let Some(models) = v["modelUsage"].as_object() {
                    for (m, r) in models {
                        rows.push((
                            m.clone(),
                            Tokens {
                                input_tokens: n(r, "inputTokens"),
                                cache_read_tokens: n(r, "cacheReadInputTokens"),
                                cache_write_tokens: n(r, "cacheCreationInputTokens"),
                                output_tokens: n(r, "outputTokens"),
                                reasoning_tokens: n(r, "reasoningTokens"),
                            },
                        ));
                    }
                } else {
                    rows.push((model.into(), snake(&v["usage"])));
                }
            }
        }
        Harness::Codex => {
            for e in &events {
                if e["type"] == "turn.completed" {
                    let r = &e["usage"];
                    let read = n(r, "cached_input_tokens");
                    let write = n(r, "cache_write_input_tokens");
                    rows.push((
                        model.into(),
                        Tokens {
                            input_tokens: n(r, "input_tokens")
                                .and_then(|a| a.checked_sub(read.unwrap_or(0)))
                                .and_then(|a| a.checked_sub(write.unwrap_or(0))),
                            cache_read_tokens: read,
                            cache_write_tokens: write,
                            output_tokens: n(r, "output_tokens"),
                            reasoning_tokens: n(r, "reasoning_output_tokens"),
                        },
                    ));
                    add(&mut out.turns, Some(1));
                }
                if e["type"] == "item.completed"
                    && matches!(
                        e["item"]["type"].as_str(),
                        Some("command_execution" | "file_change" | "mcp_tool_call")
                    )
                {
                    add(&mut out.tools, Some(1));
                }
            }
        }
        Harness::Muse => {
            for e in &events {
                let e = if e["payload_type"] == "run.usage" {
                    &e["payload"]
                } else {
                    e
                };
                if e["type"] == "run.usage" || e["event"] == "run.usage" || e["kind"] == "run_usage"
                {
                    if let Some(models) = e["models"].as_array() {
                        for r in models {
                            let cached = n(r, "cached_tokens");
                            rows.push((
                                r["model"].as_str().unwrap_or(model).into(),
                                Tokens {
                                    input_tokens: n(r, "input_tokens")
                                        .and_then(|v| v.checked_sub(cached.unwrap_or(0))),
                                    cache_read_tokens: cached,
                                    cache_write_tokens: None,
                                    output_tokens: n(r, "output_tokens"),
                                    reasoning_tokens: n(r, "reasoning_tokens"),
                                },
                            ));
                        }
                    }
                    out.turns = n(e, "calls");
                }
            }
        }
        Harness::Gemini => {
            if let Some(v) = events.last()
                && let Some(models) = v["stats"]["models"].as_object()
            {
                for (m, r) in models {
                    let t = &r["tokens"];
                    let cache = n(t, "cached");
                    rows.push((
                        m.clone(),
                        Tokens {
                            input_tokens: n(t, "prompt")
                                .and_then(|a| a.checked_sub(cache.unwrap_or(0))),
                            cache_read_tokens: cache,
                            cache_write_tokens: None,
                            output_tokens: n(t, "candidates")
                                .zip(n(t, "thoughts"))
                                .map(|(a, b)| a.saturating_add(b)),
                            reasoning_tokens: n(t, "thoughts"),
                        },
                    ));
                }
                out.tools = n(&v["stats"]["tools"], "totalCalls");
            }
        }
        Harness::Antigravity | Harness::Command => {
            for v in if h == Harness::Command {
                events.last().into_iter().collect::<Vec<_>>()
            } else {
                events.iter().collect()
            } {
                rows.push((
                    v["model"].as_str().unwrap_or(model).into(),
                    Tokens {
                        input_tokens: n(v, "input_tokens"),
                        cache_read_tokens: n(v, "cache_read_tokens"),
                        cache_write_tokens: n(v, "cache_write_tokens"),
                        output_tokens: n(v, "output_tokens"),
                        reasoning_tokens: n(v, "reasoning_tokens"),
                    },
                ));
                out.turns = n(v, "turns");
                out.tools = n(v, "tools");
            }
        }
    }
    let mut models = std::collections::BTreeSet::new();
    let mut cost = if rows.is_empty() { None } else { Some(0.0) };
    for (m, t) in rows {
        cost = cost.zip(priced(&m, &t)).map(|(a, b)| a + b);
        merge(&mut out.tokens, &t);
        models.insert(m);
    }
    out.models = models.into_iter().collect();
    out.tokens.reasoning_tokens = out.tokens.reasoning_tokens.or(reasoning_fallback);
    out.cost_usd = if h == Harness::Command {
        events
            .last()
            .and_then(|v| v["cost_usd"].as_f64())
            .filter(|c| c.is_finite() && (0.0..=1e9).contains(c))
            .or(cost)
    } else {
        cost
    };
    out
}
/// Build argv without a shell. Custom commands receive a prompt file rather than prompt arguments.
pub fn argv(
    h: Harness,
    model: &str,
    effort: Option<&str>,
    prompt: &str,
    command: &[String],
) -> Result<Vec<String>> {
    let strings = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut a = match h {
        Harness::Claude => strings(&[
            "claude",
            "-p",
            "--safe-mode",
            "--strict-mcp-config",
            "--output-format",
            "json",
            "--no-session-persistence",
            "--permission-mode",
            "bypassPermissions",
            "--model",
            model,
        ]),
        Harness::Codex => strings(&[
            "codex",
            "exec",
            "--json",
            "--ephemeral",
            "--skip-git-repo-check",
            "--dangerously-bypass-approvals-and-sandbox",
            "-m",
            model,
        ]),
        Harness::Muse => strings(&[
            "muse",
            "exec",
            "--json",
            "--no-foreign-personal-context",
            "--no-session-log",
            "--yolo",
            "--model",
            model,
        ]),
        Harness::Grok => strings(&[
            "grok",
            "--no-subagents",
            "--output-format",
            "json",
            "--permission-mode",
            "bypassPermissions",
            "--model",
            model,
        ]),
        Harness::Gemini => strings(&["gemini", "--output-format", "json", "--yolo", "-m", model]),
        Harness::Antigravity => return Err("Antigravity requires the ACP transport".into()),
        Harness::Command => {
            if command.is_empty() {
                return Err("command harness requires --command arguments".into());
            }
            return Ok(command
                .iter()
                .map(|s| {
                    s.replace("{model}", model)
                        .replace("{effort}", effort.unwrap_or(""))
                        .replace("{prompt_file}", "/tmp/bench-prompt")
                })
                .collect());
        }
    };
    if let Some(e) = effort {
        match h {
            Harness::Claude => a.extend(strings(&["--effort", e])),
            Harness::Codex => a.extend(["-c".into(), format!("model_reasoning_effort={e:?}")]),
            Harness::Muse | Harness::Grok => a.extend(strings(&["--reasoning-effort", e])),
            Harness::Gemini => {
                return Err("Gemini effort configuration is unverified; omit --effort".into());
            }
            _ => {}
        }
    }
    match h {
        Harness::Grok | Harness::Gemini => a.extend(strings(&["-p", prompt])),
        _ => a.push(prompt.into()),
    }
    Ok(a)
}

/// Use vendor-generated catalog metadata, not a cross-vendor effort vocabulary. A missing catalog
/// is a refusal: the user can refresh it by opening the vendor CLI and selecting the model.
pub fn check_effort(h: Harness, model: &str, effort: Option<&str>) -> Result<()> {
    let Some(effort) = effort else {
        return Ok(());
    };
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or("HOME required for model catalog")?;
    let mut levels = Vec::new();
    match h {
        Harness::Claude => levels = claude_efforts(model)?,
        Harness::Codex => {
            let path = std::env::var_os("CODEX_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"))
                .join("models_cache.json");
            let value: Value = serde_json::from_slice(
                &std::fs::read(path)
                    .map_err(|_| "Codex model catalog missing; open Codex to refresh it")?,
            )
            .map_err(|_| "invalid Codex model catalog")?;
            if let Some(rows) = value["models"].as_array() {
                for row in rows {
                    if row["slug"] == model
                        && let Some(a) = row["supported_reasoning_levels"].as_array()
                    {
                        levels.extend(
                            a.iter()
                                .filter_map(|v| v["effort"].as_str().map(str::to_string)),
                        );
                    }
                }
            }
        }
        Harness::Grok => {
            let path = std::env::var_os("GROK_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| home.join(".grok"))
                .join("models_cache.json");
            let value: Value = serde_json::from_slice(
                &std::fs::read(path).map_err(|_| "Grok model catalog missing; run grok models")?,
            )
            .map_err(|_| "invalid Grok model catalog")?;
            if let Some(rows) = value["models"].as_object() {
                for row in rows.values() {
                    let r = &row["info"];
                    if r["id"] == model
                        && let Some(a) = r["reasoning_efforts"].as_array()
                    {
                        levels.extend(a.iter().filter_map(|v| {
                            v.as_str()
                                .or_else(|| v["value"].as_str())
                                .map(str::to_string)
                        }));
                    }
                }
            }
        }
        Harness::Muse => {
            let path = home.join(".local/share/muse/model-catalog");
            for file in std::fs::read_dir(path)
                .map_err(|_| "Muse model catalog missing; open Muse to refresh it")?
            {
                let p = file.map_err(|e| e.to_string())?.path();
                if p.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let value: Value =
                    serde_json::from_slice(&std::fs::read(p).map_err(|e| e.to_string())?)
                        .map_err(|_| "invalid Muse model catalog")?;
                if let Some(rows) = value["rows"].as_array() {
                    for r in rows {
                        if r["model_id"] == model
                            && let Some(a) = r["reasoning_effort_variants"].as_array()
                        {
                            levels.extend(
                                a.iter()
                                    .filter_map(|v| v["tier"].as_str().map(str::to_string)),
                            );
                        }
                    }
                }
            }
        }
        Harness::Command => return Ok(()),
        _ => {
            return Err(
                "this CLI does not expose verifiable model effort metadata; omit --effort".into(),
            );
        }
    }
    if levels.iter().any(|e| e == effort) {
        Ok(())
    } else {
        Err(format!(
            "{} model {model} does not offer effort {effort}; offered: {}",
            h.name(),
            levels.join(", ")
        ))
    }
}

fn claude_levels(response: &Value, model: &str) -> Vec<String> {
    response["response"]["response"]["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["value"] == model || r["resolvedModel"] == model)
        .flat_map(|r| r["supportedEffortLevels"].as_array().into_iter().flatten())
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}
// The SDK initialize control request lists model capabilities without submitting a model turn.
fn claude_efforts(model: &str) -> Result<Vec<String>> {
    use std::process::Command;
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut command = Command::new(
        cedr_compare::exec::find_program("claude", std::path::Path::new("."))
            .ok_or("Claude is not installed")?,
    );
    command
        .args([
            "-p",
            "--safe-mode",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
        ])
        .current_dir(scratch.path())
        .env_clear();
    for key in [
        "PATH",
        "LANG",
        "SYSTEMROOT",
        "WINDIR",
        "TMP",
        "TEMP",
        "ANTHROPIC_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    cedr_agent::clean_home(
        cedr_agent::Agent::Claude,
        &mut command,
        &scratch.path().join("home"),
    )
    .map_err(|e| e.to_string())?;
    cedr_agent::private_runtime(&mut command, &scratch.path().join("runtime"))
        .map_err(|e| e.to_string())?;
    if let Some(config) = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".claude")))
    {
        command.env("CLAUDE_CONFIG_DIR", config);
    }
    command.env("DISABLE_AUTOUPDATER", "1");
    let request = "{\"type\":\"control_request\",\"request_id\":\"bench-models\",\"request\":{\"subtype\":\"initialize\"}}\n";
    let output =
        cedr_compare::exec::run_step(command, Some(request), std::time::Duration::from_secs(30))
            .map_err(|e| e.to_string())?;
    if !output.succeeded() {
        return Err(
            "Claude model capability discovery failed; check its sign-in and installed version"
                .into(),
        );
    }
    let mut levels = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Ok(response) = serde_json::from_str::<Value>(line) {
            levels.extend(claude_levels(&response, model));
        }
    }
    Ok(levels)
}
#[cfg(test)]
mod tests {
    #[test]
    fn claude_discovery_uses_resolved_model_and_actual_cli_levels() {
        let value = serde_json::from_str(include_str!(
            "../../../fixtures/bench/usage/claude-models.json"
        ))
        .unwrap();
        let levels = super::claude_levels(&value, "claude-haiku-5-5");
        assert!(levels.iter().any(|s| s == "high"));
        assert!(!levels.iter().any(|s| s == "ultrathink"));
        assert!(super::claude_levels(&value, "unknown").is_empty());
    }
}
