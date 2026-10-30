//! Vendored organization policy verification and offline checks.
use crate::{
    Result,
    config::{Config, Server},
    hash, io,
    sources::Skills,
};
use cedr_core::{
    canonical,
    finding::{Finding, Rule, Severity},
    source::Source,
};
use serde_json::{Value, json};
use std::path::Path;
pub fn policy_hash(value: &Value) -> String {
    let mut value = value.clone();
    if let Some(o) = value.as_object_mut() {
        for k in ["org", "version", "hash", "updatedAt"] {
            o.remove(k);
        }
    }
    hash(canonical::to_string(&value).as_bytes())
}
pub fn validate(v: &Value) -> Result<()> {
    if !v.is_object() || v["hash"].as_str() != Some(policy_hash(v).as_str()) {
        return Err(".cedr/org-rules.json: hash mismatch".into());
    }
    if v["instructions"].as_str().unwrap_or("").len() > 32768 {
        return Err(".cedr/org-rules.json: instructions exceeds 32 KiB".into());
    }
    Ok(())
}
pub fn load(root: &Path) -> Result<Option<Value>> {
    io::read(root, ".cedr/org-rules.json")?
        .map(|b| {
            let v = serde_json::from_slice(&b).map_err(|_| ".cedr/org-rules.json: invalid JSON")?;
            validate(&v)?;
            Ok(v)
        })
        .transpose()
}
fn source(s: &Server) -> Source {
    s.command
        .as_ref()
        .map(|c| Source::from_command(c, &s.args))
        .unwrap_or_else(|| Source::Url(s.url.clone().unwrap_or_default()))
}
fn wildcard(pattern: &str, text: &str) -> bool {
    let re = format!(
        "(?i)^{}$",
        pattern
            .split('*')
            .map(regex::escape)
            .collect::<Vec<_>>()
            .join(".*")
    );
    regex::Regex::new(&re).is_ok_and(|r| r.is_match(text))
}
pub fn apply(c: &mut Config, skills: &mut Skills, v: &Value) -> Result<Vec<Finding>> {
    let mut findings = vec![];
    let mut add = |rule, severity, message: String| {
        findings.push(Finding::new(
            rule,
            severity,
            ".cedr/sync.toml",
            None,
            message,
        ))
    };
    for t in v["targets"].as_array().unwrap_or(&vec![]) {
        if let Some(t) = t.as_str()
            && !c.targets.iter().any(|s| s == t)
        {
            add(
                Rule::Cs105,
                Severity::Warning,
                format!("required target {t} is missing"),
            );
        }
    }
    if v["checks"]["unrestrictedShell"] == json!(false)
        && c.permissions.allow.iter().any(|s| s == "shell:*")
    {
        add(
            Rule::Cs102,
            Severity::Error,
            "organization forbids unrestricted shell".into(),
        );
    }
    c.mcp.retain(|name, s| {
        let source = source(s);
        let banned = v["banned"].as_array().is_some_and(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .any(|p| wildcard(p, name) || wildcard(p, &source.to_string()))
        });
        if banned {
            add(
                Rule::Cs101,
                Severity::Error,
                format!("mcp.{name}: organization bans this server"),
            );
        }
        if v["checks"]["pinnedServers"] == json!(true) && !source.pinned() {
            add(
                Rule::Cs103,
                Severity::Error,
                format!("mcp.{name}: server is unpinned"),
            );
        }
        !banned
    });
    for p in v["deny"].as_array().unwrap_or(&vec![]) {
        let p = p.as_str().ok_or(".cedr/org-rules.json: invalid deny")?;
        crate::config::validate_permission(p)?;
        if !c.permissions.deny.iter().any(|s| s == p) {
            c.permissions.deny.push(p.into());
        }
    }
    c.permissions
        .allow
        .retain(|p| !c.permissions.deny.contains(p));
    for s in v["skills"].as_array().unwrap_or(&vec![]) {
        let name = s["name"]
            .as_str()
            .filter(|s| crate::config::name(s))
            .ok_or(".cedr/org-rules.json: invalid skills.name")?;
        let mut tree = std::collections::BTreeMap::new();
        let mut size = 0;
        for f in s["files"]
            .as_array()
            .ok_or(".cedr/org-rules.json: invalid skills.files")?
        {
            let p = f["path"]
                .as_str()
                .ok_or(".cedr/org-rules.json: invalid skills.path")?;
            io::path(Path::new("."), p)?;
            let b = f["content"]
                .as_str()
                .ok_or(".cedr/org-rules.json: invalid skills.content")?
                .as_bytes()
                .to_vec();
            size += b.len();
            tree.insert(p.into(), b);
        }
        if size > 1024 * 1024 {
            return Err(".cedr/org-rules.json: skill exceeds 1 MiB".into());
        }
        skills.insert(name.into(), tree);
    }
    Ok(findings)
}
