//! Strict conversion of legacy routing files; unsupported fields must never disappear silently.
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, Result};
use toml::{Table, Value};

fn table(value: &Value) -> Result<&Table> {
    value
        .as_table()
        .ok_or_else(|| Error::Usage("import: expected a table".into()))
}

fn keys(value: &Value, allowed: &[&str], context: &str) -> Result<()> {
    for key in table(value)?.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(Error::Usage(format!("import: unknown key {context}.{key}")));
        }
    }
    Ok(())
}

fn harness(id: &str) -> &str {
    if id == "claudeAgent" { "claude" } else { id }
}

/// Converts and validates before returning any output. The source path labels parse errors only.
pub fn convert(text: &str, path: &Path) -> Result<String> {
    let source: Value = toml::from_str(text).map_err(|e| crate::error::toml_err(path, text, &e))?;
    keys(
        &source,
        &[
            "providers",
            "families",
            "thresholds",
            "evaluation",
            "routes",
        ],
        "source",
    )?;
    let mut out = Table::new();
    out.insert("version".into(), Value::Integer(1));
    for key in ["evaluation", "thresholds"] {
        if let Some(v) = source.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    let mut harnesses = Table::new();
    if let Some(providers) = source.get("providers") {
        for (id, v) in table(providers)? {
            keys(v, &["expect"], &format!("providers.{id}"))?;
            let mut h = table(v)?.clone();
            match id.as_str() {
                "claudeAgent" => {
                    h.insert("t3".into(), "claudeAgent".into());
                    h.insert("aliases".into(), Value::Array(vec!["claudeAgent".into()]));
                    h.insert("effort_option".into(), "effort".into());
                }
                "codex" | "muse" => {
                    h.insert("effort_option".into(), "reasoningEffort".into());
                }
                "antigravity" => {
                    h.insert(
                        "brief_prefix".into(),
                        "Load the workstation-rules skill first".into(),
                    );
                }
                _ => {}
            }
            harnesses.insert(harness(id).into(), Value::Table(h));
        }
    }
    out.insert("harnesses".into(), Value::Table(harnesses));
    let mut windows = Table::new();
    if let Some(families) = source.get("families") {
        for (id, v) in table(families)? {
            keys(v, &["provider", "model"], &format!("families.{id}"))?;
            let mut w = table(v)?.clone();
            let provider = w
                .remove("provider")
                .ok_or_else(|| Error::Usage(format!("families.{id}: missing provider")))?;
            let provider = provider
                .as_str()
                .ok_or_else(|| Error::Usage(format!("families.{id}.provider: expected string")))?;
            w.insert("harness".into(), harness(provider).into());
            windows.insert(id.clone(), Value::Table(w));
        }
    }
    out.insert("model_windows".into(), Value::Table(windows));
    out.insert(
        "orchestrator".into(),
        Value::Table(Table::from_iter([
            ("harness".into(), "claude".into()),
            ("model".into(), "opus".into()),
        ])),
    );
    let mut routes = Table::new();
    if let Some(source_routes) = source.get("routes") {
        for (id, v) in table(source_routes)? {
            keys(
                v,
                &[
                    "work",
                    "enabled",
                    "role",
                    "council",
                    "quorum",
                    "block",
                    "rounds",
                    "judge",
                    "escalate",
                    "candidates",
                ],
                &format!("routes.{id}"),
            )?;
            let mut route = table(v)?.clone();
            if let Some(candidates) = route.get_mut("candidates") {
                let candidates = candidates.as_array_mut().ok_or_else(|| {
                    Error::Usage(format!("routes.{id}.candidates: expected array"))
                })?;
                for c in candidates {
                    keys(
                        c,
                        &["target", "effort", "tier"],
                        &format!("routes.{id}.candidates"),
                    )?;
                    if let Some(target) = c
                        .get("target")
                        .and_then(Value::as_str)
                        .and_then(|t| t.strip_prefix("claudeAgent/"))
                    {
                        let target = format!("claude/{target}");
                        c.as_table_mut()
                            .expect("checked table")
                            .insert("target".into(), target.into());
                    }
                }
            }
            routes.insert(id.clone(), Value::Table(route));
        }
    }
    out.insert("routes".into(), Value::Table(routes));
    let output = toml::to_string_pretty(&out).map_err(|e| Error::Usage(format!("import: {e}")))?;
    Config::parse(&output, path)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    const SOURCE: &str = include_str!("../../../fixtures/route/import.toml");

    #[test]
    fn mapping_keeps_tiers_aliases_and_policy() {
        let text = convert(SOURCE, Path::new("fixture")).unwrap();
        let c = Config::parse(&text, Path::new("output")).unwrap();
        assert_eq!(c.harnesses["claude"].aliases, ["claudeAgent"]);
        assert_eq!(
            c.harnesses["codex"].effort_option.as_deref(),
            Some("reasoningEffort")
        );
        assert_eq!(
            c.harnesses["muse"].effort_option.as_deref(),
            Some("reasoningEffort")
        );
        assert_eq!(
            c.harnesses["antigravity"].brief_prefix,
            "Load the workstation-rules skill first"
        );
        assert_eq!(c.model_windows["sonnet"].harness, "claude");
        let r = &c.routes["review"];
        assert_eq!(r.candidates[0].target, "claude/sonnet");
        assert_eq!(r.candidates[0].tier, Some(2));
        assert!(!r.enabled);
        assert_eq!(c.orchestrator.unwrap().model, "opus");
    }

    #[test]
    fn unknown_fields_are_errors_at_every_source_level() {
        for source in [
            format!("unknown = true\n{SOURCE}"),
            SOURCE.replace("expect =", "unknown ="),
            SOURCE.replace("provider =", "unknown ="),
            SOURCE.replace("work =", "unknown ="),
            SOURCE.replace("target =", "unknown ="),
            SOURCE.replace("preview =", "unknown ="),
            SOURCE.replace("out =", "unknown ="),
        ] {
            assert!(
                convert(&source, Path::new("fixture"))
                    .unwrap_err()
                    .to_string()
                    .contains("unknown")
            );
        }
    }
}
