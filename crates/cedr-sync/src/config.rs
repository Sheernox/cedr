//! Strict source parsing. Diagnostics identify keys without echoing their values.
use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const TARGETS: &[&str] = &[
    "agents",
    "claude",
    "codex",
    "cursor",
    "gemini",
    "copilot",
    "opencode",
    "antigravity",
];
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default = "default_targets")]
    pub targets: Vec<String>,
    #[serde(default)]
    pub mcp: BTreeMap<String, Server>,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub hooks: Vec<Hook>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub agents: Option<Vec<String>>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub event: String,
    pub command: String,
    pub agents: Option<Vec<String>>,
}

pub fn secret(value: &str) -> bool {
    let variables = regex::Regex::new(r"\$\{[A-Z_][A-Z0-9_]*\}").expect("regex");
    let text = variables.replace_all(value, "");
    regex::Regex::new(r"(?i)(sk-|ghp_|cedr_|xox)|[A-Za-z0-9+/=_-]{32,}")
        .expect("regex")
        .is_match(&text)
}
pub fn refs_valid(value: &str) -> bool {
    let stripped = regex::Regex::new(r"\$\{[A-Z_][A-Z0-9_]*\}")
        .expect("regex")
        .replace_all(value, "");
    !stripped.contains("${")
}
pub fn selected(agents: &Option<Vec<String>>, target: &str) -> bool {
    agents
        .as_ref()
        .is_none_or(|a| a.iter().any(|s| s == target))
}
pub fn name(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let c: Self = toml::from_str(text).map_err(|e| {
            let key = e.message().lines().next().unwrap_or("invalid key");
            // Parser messages can contain source values. Report unknown field names only.
            let detail = if key.starts_with("unknown field `") {
                key.split('`').nth(1).unwrap_or("key")
            } else {
                "key or type"
            };
            format!(".cedr/sync.toml: invalid {detail}")
        })?;
        if c.version != 1 {
            return Err(".cedr/sync.toml: invalid version".into());
        }
        let set: BTreeSet<_> = c.targets.iter().collect();
        if c.targets.is_empty()
            || set.len() != c.targets.len()
            || c.targets.iter().any(|t| !TARGETS.contains(&t.as_str()))
        {
            return Err(".cedr/sync.toml: invalid targets".into());
        }
        let agents = |a: &Option<Vec<String>>| -> Result<()> {
            if a.as_ref()
                .is_some_and(|v| v.iter().any(|t| !set.contains(t)))
            {
                Err(".cedr/sync.toml: invalid agents".into())
            } else {
                Ok(())
            }
        };
        for (n, s) in &c.mcp {
            if !name(n) {
                return Err(".cedr/sync.toml: invalid mcp name".into());
            }
            if s.command.is_some() == s.url.is_some()
                || (s.url.is_some() && (!s.env.is_empty() || !s.args.is_empty()))
                || (s.command.is_some() && !s.headers.is_empty())
            {
                return Err(format!(".cedr/sync.toml: mcp.{n}: command/url keys"));
            }
            if s.command.as_ref().is_some_and(|v| v.trim().is_empty()) {
                return Err(format!(".cedr/sync.toml: mcp.{n}.command"));
            }
            for (k, v) in s.env.iter().chain(&s.headers) {
                if secret(v) || !refs_valid(v) {
                    return Err(format!(
                        ".cedr/sync.toml: mcp.{n}.{k}: use a variable reference"
                    ));
                }
            }
            agents(&s.agents)?;
        }
        for p in c.permissions.allow.iter().chain(&c.permissions.deny) {
            validate_permission(p)?;
        }
        for h in &c.hooks {
            if !["before-shell", "after-edit", "session-start", "stop"].contains(&h.event.as_str())
                || h.command.trim().is_empty()
            {
                return Err(".cedr/sync.toml: invalid hooks.event or hooks.command".into());
            }
            agents(&h.agents)?;
        }
        Ok(c)
    }
}
pub fn validate_permission(p: &str) -> Result<()> {
    match p.split_once(':') {
        Some(("shell" | "read" | "edit" | "fetch" | "mcp", value))
            if !value.trim().is_empty() && !value.contains(['\n', '\r']) =>
        {
            Ok(())
        }
        _ => Err(".cedr/sync.toml: invalid permissions key".into()),
    }
}

fn default_targets() -> Vec<String> {
    vec!["agents".into()]
}
