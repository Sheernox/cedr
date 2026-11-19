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
    let sum =
        |left: Option<u64>, right: Option<u64>| left.zip(right).and_then(|(a, b)| a.checked_add(b));
    a.input_tokens = sum(a.input_tokens, b.input_tokens);
    a.cache_read_tokens = sum(a.cache_read_tokens, b.cache_read_tokens);
    a.cache_write_tokens = sum(a.cache_write_tokens, b.cache_write_tokens);
    a.output_tokens = sum(a.output_tokens, b.output_tokens);
    a.reasoning_tokens = sum(a.reasoning_tokens, b.reasoning_tokens);
}
fn priced(model: &str, t: &Tokens) -> Option<f64> {
    let (p, _) = cedr_monitor::pricing::price(model)?;
    Some(
        (t.input_tokens? as f64 * p.input
            + t.cache_read_tokens? as f64 * p.cache_read
            + t.cache_write_tokens? as f64 * p.cache_write5m
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

fn generic_tokens(v: &Value) -> Tokens {
    Tokens {
        input_tokens: n(v, "input_tokens"),
        cache_read_tokens: n(v, "cache_read_tokens"),
        cache_write_tokens: n(v, "cache_write_tokens"),
        output_tokens: n(v, "output_tokens"),
        reasoning_tokens: n(v, "reasoning_tokens"),
    }
}
// Claude's result totals include side queries, but only request usage can select a
// long-prompt tier. Deduplicate content blocks by message id before applying it.
fn claude_cost(model: &str, row: &Value, events: &[Value]) -> Option<f64> {
    let tokens = Tokens {
        input_tokens: n(row, "inputTokens"),
        cache_read_tokens: n(row, "cacheReadInputTokens"),
        cache_write_tokens: n(row, "cacheCreationInputTokens"),
        output_tokens: n(row, "outputTokens"),
        reasoning_tokens: None,
    };
    let (price, _) = cedr_monitor::pricing::price(model)?;
    let mut requests = std::collections::BTreeMap::new();
    for event in events {
        let message = &event["message"];
        if event["type"] == "assistant" && message["model"] == model {
            requests.insert(message["id"].as_str()?, &message["usage"]);
        }
    }
    let reported_hours = requests.values().try_fold(0_u64, |total, usage| {
        total.checked_add(n(&usage["cache_creation"], "ephemeral_1h_input_tokens").unwrap_or(0))
    })?;
    let hour = n(row, "cacheCreationEphemeral1hInputTokens").unwrap_or(reported_hours);
    if hour < reported_hours {
        return None;
    }
    tokens.cache_write_tokens.unwrap_or(0).checked_sub(hour)?;
    let mut cost = priced(model, &tokens)?
        + hour as f64 * (price.cache_write1h - price.cache_write5m) / 1_000_000.0;
    if let Some(tier) = &price.long_prompt {
        let mut seen = 0_u64;
        for usage in requests.values() {
            let request = snake(usage);
            let input = request.input()?;
            seen = seen.checked_add(input)?;
            if input > tier.above_input_tokens {
                let hour = n(&usage["cache_creation"], "ephemeral_1h_input_tokens").unwrap_or(0);
                request.cache_write_tokens.unwrap_or(0).checked_sub(hour)?;
                let base = priced(model, &request)?
                    + hour as f64 * (price.cache_write1h - price.cache_write5m) / 1_000_000.0;
                cost += base * (tier.multiplier - 1.0);
            }
        }
        // Unobserved side queries are provably below the threshold only when their
        // combined input is. Otherwise refuse pricing rather than guessing a tier.
        if tokens.input()?.checked_sub(seen)? > tier.above_input_tokens {
            return None;
        }
    }
    Some(cost)
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
    let mut missing_requests = false;
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
                                reasoning_tokens: n(r, "reasoningTokens")
                                    .or_else(|| n(r, "thinkingTokens")),
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
                    // OpenAI bills cache writes separately; older CLI streams cannot price them.
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
                                    cache_write_tokens: Some(0),
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
                            cache_write_tokens: Some(0),
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
        Harness::Antigravity => {
            for v in &events {
                let tokens = generic_tokens(v);
                if v.get("model").is_some() || tokens != Tokens::default() {
                    rows.push((v["model"].as_str().unwrap_or(model).into(), tokens));
                }
            }
            if let Some(totals) = events.last() {
                out.turns = n(totals, "turns");
                out.tools = n(totals, "tools");
                // The Docker stdout capture can drop old lines. A launcher summary
                // lets us detect that a request is absent instead of pricing a subset.
                missing_requests =
                    totals.get("model").is_none() && out.turns != Some(rows.len() as u64);
            }
        }
        Harness::Command => {
            if let Some(v) = events.last() {
                rows.push((
                    v["model"].as_str().unwrap_or(model).into(),
                    generic_tokens(v),
                ));
                out.turns = n(v, "turns");
                out.tools = n(v, "tools");
            }
        }
    }
    let mut models = std::collections::BTreeSet::new();
    let mut cost = if rows.is_empty() { None } else { Some(0.0) };
    for (index, (m, t)) in rows.into_iter().enumerate() {
        let row_cost = if h == Harness::Claude {
            events
                .iter()
                .rev()
                .find_map(|v| v["modelUsage"].get(&m))
                .and_then(|row| claude_cost(&m, row, &events))
                .or_else(|| {
                    (events.iter().all(|v| v.get("modelUsage").is_none()))
                        .then(|| priced(&m, &t))
                        .flatten()
                })
        } else {
            priced(&m, &t)
        };
        cost = cost.zip(row_cost).map(|(a, b)| a + b);
        if index == 0 {
            out.tokens = t;
        } else {
            merge(&mut out.tokens, &t);
        }
        models.insert(m);
    }
    out.models = models.into_iter().collect();
    out.tokens.reasoning_tokens = out.tokens.reasoning_tokens.or(reasoning_fallback);
    out.cost_usd = if h == Harness::Command
        && out.tokens.input_tokens.is_some()
        && out.tokens.cache_read_tokens.is_some()
        && out.tokens.cache_write_tokens.is_some()
        && out.tokens.output_tokens.is_some()
    {
        events
            .last()
            .and_then(|v| v["cost_usd"].as_f64())
            .filter(|c| c.is_finite() && (0.0..=1e9).contains(c))
            .or(cost)
    } else {
        cost
    };
    if missing_requests {
        out.tokens = Tokens::default();
        out.cost_usd = None;
    }
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
            "stream-json",
            "--verbose",
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
        Harness::Antigravity => {
            let effort = effort
                .ok_or("Antigravity requires --effort; choose a level offered by the model")?;
            return Ok(vec![
                format!("{model}-{effort}"),
                "/home/bench/.bench-prompt".into(),
            ]);
        }
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
pub(crate) fn claude_levels(response: &Value, model: &str) -> Vec<String> {
    response["response"]["response"]["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["value"] == model || r["resolvedModel"] == model)
        .flat_map(|r| r["supportedEffortLevels"].as_array().into_iter().flatten())
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
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
