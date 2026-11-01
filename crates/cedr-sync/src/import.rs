//! Imports existing native configuration without executing a server or package.
use crate::{
    Result,
    config::{self, Config, Permissions, Server},
    io,
    sources::{self, Meta},
};
use cedr_core::discovery::{self, Launch};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};
fn normalize(s: &str) -> String {
    regex::Regex::new(r"\$\{env:([A-Z_][A-Z0-9_]*)\}|\{env:([A-Z_][A-Z0-9_]*)\}")
        .expect("regex")
        .replace_all(s, |c: &regex::Captures<'_>| {
            format!("${{{}}}", c.get(1).or(c.get(2)).expect("capture").as_str())
        })
        .into_owned()
}
fn sanitize(s: &mut Server, notes: &mut Vec<String>, name: &str) {
    for (k, v) in s.env.iter_mut().chain(s.headers.iter_mut()) {
        *v = normalize(v);
        if config::secret(v) {
            let key: String = k
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_uppercase()
                    } else {
                        '_'
                    }
                })
                .collect();
            *v = format!("${{{key}}}");
            notes.push(format!("mcp.{name}.{k}: set variable {key}"));
        }
    }
}
fn permission(s: &str) -> Option<Vec<String>> {
    if s == "Bash" {
        return Some(vec!["shell:*".into()]);
    }
    for (prefix, kind) in [
        ("Bash(", "shell"),
        ("Read(", "read"),
        ("Edit(", "edit"),
        ("WebFetch(domain:", "fetch"),
    ] {
        if let Some(t) = s.strip_prefix(prefix).and_then(|s| s.strip_suffix(')')) {
            return Some(vec![format!(
                "{kind}:{}",
                if kind == "shell" {
                    t.strip_suffix(" *").unwrap_or(t)
                } else {
                    t
                }
            )]);
        }
    }
    s.strip_prefix("mcp__")
        .map(|s| vec![format!("mcp:{}", s.replace("__", "/"))])
}
/// Grok Build's `[permission]` table: compact `allow` and `deny` strings use Claude Code's spelling, and `rules` entries
/// name a tool and an optional pattern. `ask` has no place in Sync's vocabulary, so it is reported.
fn grok_permissions(v: &Value, path: &str, out: &mut Permissions, notes: &mut Vec<String>) {
    for (decision, list) in [("allow", &mut out.allow), ("deny", &mut out.deny)] {
        for p in v[decision]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            match permission(p) {
                Some(entries) => list.extend(entries),
                None => notes.push(format!("{path}: permission.{decision}: no translation")),
            }
        }
    }
    if v["ask"].as_array().is_some_and(|a| !a.is_empty()) {
        notes.push(format!("{path}: permission.ask: no translation"));
    }
    for rule in v["rules"].as_array().into_iter().flatten() {
        let pattern = rule["pattern"].as_str();
        let entry = match (rule["tool"].as_str().unwrap_or(""), pattern) {
            ("bash", None | Some("*")) => Some("shell:*".to_string()),
            ("bash", Some(p)) => Some(format!("shell:{}", p.strip_suffix(" *").unwrap_or(p))),
            ("read", Some(p)) => Some(format!("read:{p}")),
            ("edit", Some(p)) => Some(format!("edit:{p}")),
            ("webfetch", Some(p)) => p.strip_prefix("domain:").map(|d| format!("fetch:{d}")),
            ("mcp", Some(p)) => p
                .strip_suffix("__*")
                .map(|server| format!("mcp:{server}"))
                .or_else(|| p.split_once("__").map(|(s, t)| format!("mcp:{s}/{t}"))),
            _ => None,
        };
        match (rule["action"].as_str(), entry) {
            (Some("allow"), Some(e)) => out.allow.push(e),
            (Some("deny"), Some(e)) => out.deny.push(e),
            _ => notes.push(format!("{path}: permission.rules: no translation")),
        }
    }
}
pub fn init(root: &Path, targets: Option<Vec<String>>) -> Result<Vec<String>> {
    if io::read(root, ".cedr/sync.toml")?.is_some() {
        return Err(".cedr/sync.toml already exists".into());
    }
    // Reject links before discovery, whose broader Guard semantics permit internal links.
    for path in [
        ".cedr/guard.toml",
        "AGENTS.md",
        "CLAUDE.md",
        "GEMINI.md",
        ".mcp.json",
        ".cursor/mcp.json",
        ".claude/settings.json",
        ".codex/config.toml",
        ".gemini/settings.json",
        ".vscode/mcp.json",
        ".github/mcp.json",
        "opencode.json",
        ".agents/mcp_config.json",
        ".grok/config.toml",
    ] {
        io::path(root, path)?;
    }
    for rel in [
        ".agents/skills",
        ".claude/skills",
        ".cursor/skills",
        ".gemini/skills",
        ".github/skills",
        ".opencode/skills",
        ".grok/skills",
    ] {
        io::files(root, rel)?;
    }
    let workspace =
        discovery::discover(root).map_err(|_| "sync init: cannot discover agent configuration")?;
    let mut notes = vec![];
    let mut writes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut found = vec!["agents".to_string()];
    let base = io::read(root, "AGENTS.md")?.unwrap_or_default();
    for (path, target, id) in [
        ("AGENTS.md", "agents", "10-agents"),
        ("CLAUDE.md", "claude", "20-claude"),
        ("GEMINI.md", "gemini", "30-gemini"),
        (".github/copilot-instructions.md", "copilot", "40-copilot"),
    ] {
        if let Some(b) = io::read(root, path)? {
            if !found.iter().any(|s| s == target) {
                found.push(target.into());
            }
            let text =
                String::from_utf8(b.clone()).map_err(|_| format!("{path}: invalid UTF-8"))?;
            if target != "agents" && (b == base || text.trim() == "@AGENTS.md") {
                continue;
            }
            let stripped = crate::strip_header(&text);
            let text = stripped.trim();
            let text = if target == "claude" {
                text.strip_prefix("@AGENTS.md\n")
                    .unwrap_or(text)
                    .trim_start()
            } else {
                text
            };
            let front = if target == "agents" {
                String::new()
            } else {
                format!("---\nagents: [{target}]\n---\n")
            };
            writes.insert(
                format!(".cedr/rules/{id}.md"),
                format!("{front}{text}\n").into_bytes(),
            );
        }
    }
    for (rel, target, extension, key) in [
        (".cursor/rules", "cursor", ".mdc", "globs"),
        (
            ".github/instructions",
            "copilot",
            ".instructions.md",
            "applyTo",
        ),
        (".grok/rules", "grok", ".md", "paths"),
    ] {
        for path in io::files(root, rel)? {
            if !path.ends_with(extension) {
                continue;
            }
            if !found.iter().any(|s| s == target) {
                found.push(target.into());
            }
            let b = io::read(root, &path)?.unwrap_or_default();
            let text = std::str::from_utf8(&b).map_err(|_| format!("{path}: invalid UTF-8"))?;
            let normalized = text.replace("\r\n", "\n");
            let stripped = crate::strip_header(&normalized);
            let text = stripped.trim_start();
            let (meta, body) = if let Some(rest) = text.strip_prefix("---\n") {
                let (front, body) = rest
                    .split_once("\n---\n")
                    .ok_or("init: invalid rule front matter")?;
                let v: serde_yaml::Value =
                    serde_yaml::from_str(front).map_err(|_| "init: invalid rule front matter")?;
                let paths = v[key]
                    .as_str()
                    .map(|s| s.split(',').map(|s| s.trim().into()).collect())
                    .or_else(|| {
                        v[key].as_sequence().map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                    });
                (
                    Meta {
                        description: v["description"].as_str().map(String::from),
                        paths,
                        agents: Some(vec![target.into()]),
                    },
                    body.to_string(),
                )
            } else {
                (
                    Meta {
                        agents: Some(vec![target.into()]),
                        ..Default::default()
                    },
                    text.into(),
                )
            };
            let id = Path::new(&path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .trim_end_matches(extension)
                .to_string();
            writes.insert(
                format!(".cedr/rules/50-{target}-{id}.md"),
                format!(
                    "---\n{}---\n{}\n",
                    serde_yaml::to_string(&meta).map_err(|_| "init: invalid front matter")?,
                    body.trim()
                )
                .into_bytes(),
            );
        }
    }
    let mut mcp = BTreeMap::new();
    for target in config::TARGETS {
        for existing in workspace
            .servers
            .iter()
            .filter(|s| s.client.as_str() == *target)
        {
            if !found.iter().any(|s| s == target) {
                found.push((*target).into());
            }
            let mut s = match &existing.launch {
                Launch::Stdio { command, args, env } => Server {
                    command: Some(command.clone()),
                    args: args.clone(),
                    env: env.clone(),
                    ..Default::default()
                },
                Launch::Remote { url } => Server {
                    url: Some(url.clone()),
                    ..Default::default()
                },
            };
            // Recover native credential references that Guard deliberately does not retain.
            if let Some(bytes) = io::read(root, &existing.config_path)?
                && let Ok(v) = crate::merge::value(&existing.config_path, &bytes)
            {
                let key = if ["codex", "grok"].contains(target) {
                    "mcp_servers"
                } else if *target == "copilot" && existing.config_path == ".vscode/mcp.json" {
                    "servers"
                } else if *target == "opencode" {
                    "mcp"
                } else {
                    "mcpServers"
                };
                let native = &v[key][&existing.name];
                if let Some(o) = native["headers"].as_object() {
                    s.headers = o
                        .iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.into())))
                        .collect();
                }
                for var in native["env_vars"]
                    .as_array()
                    .unwrap_or(&vec![])
                    .iter()
                    .filter_map(Value::as_str)
                {
                    s.env.insert(var.into(), format!("${{{var}}}"));
                }
                for field in ["http_headers", "env_http_headers"] {
                    if let Some(o) = native[field].as_object() {
                        for (k, v) in o {
                            if let Some(v) = v.as_str() {
                                s.headers.insert(
                                    k.clone(),
                                    if field == "env_http_headers" {
                                        format!("${{{v}}}")
                                    } else {
                                        v.into()
                                    },
                                );
                            }
                        }
                    }
                }
                if let Some(var) = native["bearer_token_env_var"].as_str() {
                    s.headers
                        .insert("Authorization".into(), format!("Bearer ${{{var}}}"));
                }
            }
            sanitize(&mut s, &mut notes, &existing.name);
            if let Some(old) = mcp.get(&existing.name) {
                if serde_json::to_value(old).ok() != serde_json::to_value(&s).ok() {
                    notes.push(format!(
                        "mcp.{}: different launch; kept first in target order",
                        existing.name
                    ));
                }
            } else {
                mcp.insert(existing.name.clone(), s);
            }
        }
    }
    let mut skills = BTreeMap::new();
    for rel in [
        ".agents/skills",
        ".claude/skills",
        ".cursor/skills",
        ".gemini/skills",
        ".github/skills",
        ".opencode/skills",
        ".grok/skills",
    ] {
        let imported = sources::skills(root, rel)?;
        let target = match rel {
            ".claude/skills" => "claude",
            ".cursor/skills" => "cursor",
            ".gemini/skills" => "gemini",
            ".github/skills" => "copilot",
            ".opencode/skills" => "opencode",
            ".grok/skills" => "grok",
            _ => "agents",
        };
        if !imported.is_empty() && !found.iter().any(|s| s == target) {
            found.push(target.into());
        }
        for (name, tree) in imported {
            if let Some(old) = skills.get(&name) {
                if old != &tree {
                    notes.push(format!("skills/{name}: different copy; kept first"));
                }
            } else {
                skills.insert(name, tree);
            }
        }
    }
    for (name, tree) in skills {
        for (path, bytes) in tree {
            writes.insert(format!(".cedr/skills/{name}/{path}"), bytes);
        }
    }
    let mut permissions = Permissions::default();
    let mut hooks = vec![];
    if let Some(bytes) = io::read(root, ".claude/settings.json")? {
        let v: Value =
            serde_json::from_slice(&bytes).map_err(|_| ".claude/settings.json: invalid JSON")?;
        if !found.iter().any(|s| s == "claude") {
            found.push("claude".into());
        }
        for (key, list) in [
            ("allow", &mut permissions.allow),
            ("deny", &mut permissions.deny),
        ] {
            for p in v["permissions"][key].as_array().unwrap_or(&vec![]) {
                if let Some(p) = p.as_str() {
                    if let Some(entries) = permission(p) {
                        list.extend(entries);
                    } else {
                        notes.push(format!(
                            ".claude/settings.json: permissions.{key}: no translation"
                        ));
                    }
                }
            }
            list.sort();
            list.dedup();
        }
        if let Some(events) = v["hooks"].as_object() {
            for (event, groups) in events {
                for group in groups.as_array().unwrap_or(&vec![]) {
                    let mapped = match (event.as_str(), group["matcher"].as_str()) {
                        ("PreToolUse", Some("Bash")) => Some("before-shell"),
                        ("PostToolUse", Some("Edit|Write|MultiEdit")) => Some("after-edit"),
                        ("SessionStart", _) => Some("session-start"),
                        ("Stop", _) => Some("stop"),
                        _ => None,
                    };
                    if let Some(mapped) = mapped {
                        for command in group["hooks"].as_array().unwrap_or(&vec![]) {
                            if let Some(command) = command["command"].as_str() {
                                hooks.push(config::Hook {
                                    event: mapped.into(),
                                    command: command.into(),
                                    agents: Some(vec!["claude".into()]),
                                });
                            }
                        }
                    } else {
                        notes.push(".claude/settings.json: hooks: no translation".into());
                    }
                }
            }
        }
    }
    let grok_hooks = io::files(root, ".grok/hooks")?;
    let grok_paths = std::iter::once(".grok/config.toml")
        .chain(
            grok_hooks
                .iter()
                .map(String::as_str)
                .filter(|p| p.ends_with(".json")),
        )
        .collect();
    for (target, paths) in [
        (
            "codex",
            vec![
                ".codex/config.toml",
                ".codex/hooks.json",
                ".codex/rules/cedr.rules",
            ],
        ),
        (
            "cursor",
            vec![".cursor/mcp.json", ".cursor/cli.json", ".cursor/hooks.json"],
        ),
        ("gemini", vec![".gemini/settings.json"]),
        (
            "copilot",
            vec![
                ".vscode/mcp.json",
                ".vscode/settings.json",
                ".github/hooks/cedr.json",
            ],
        ),
        ("opencode", vec!["opencode.json"]),
        (
            "antigravity",
            vec![".agents/mcp_config.json", ".agents/hooks.json"],
        ),
        ("grok", grok_paths),
    ] {
        if paths.iter().any(|p| root.join(p).exists()) && !found.iter().any(|s| s == target) {
            found.push(target.into());
        }
        for path in paths {
            let Some(bytes) = io::read(root, path)? else {
                continue;
            };
            if path.ends_with(".rules") {
                let text = std::str::from_utf8(&bytes).map_err(|_| "Codex rules: invalid UTF-8")?;
                static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(
                    || {
                        regex::Regex::new(r#"(?s)prefix_rule\(\s*pattern\s*=\s*(\[[^\]]*\])\s*,\s*decision\s*=\s*"(allow|forbidden)"\s*,?\s*\)"#).expect("regex")
                    },
                );
                let mut translated = 0;
                for captures in PATTERN.captures_iter(text) {
                    if let Ok(words) = serde_json::from_str::<Vec<String>>(&captures[1]) {
                        let p = format!("shell:{}", words.join(" "));
                        if &captures[2] == "allow" {
                            permissions.allow.push(p);
                        } else {
                            permissions.deny.push(p);
                        }
                        translated += 1;
                    }
                }
                let remainder = PATTERN.replace_all(text, "");
                let unsupported = remainder
                    .lines()
                    .any(|line| !line.split('#').next().unwrap_or_default().trim().is_empty());
                if unsupported || (translated == 0 && !text.trim().is_empty()) {
                    notes.push(format!("{path}: execpolicy rules: no translation"));
                }
                continue;
            }
            let v = crate::merge::value(path, &bytes)?;
            if target == "cursor" {
                for (decision, list) in [
                    ("allow", &mut permissions.allow),
                    ("deny", &mut permissions.deny),
                ] {
                    for p in v["permissions"][decision]
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .filter_map(Value::as_str)
                    {
                        let translated = p
                            .strip_prefix("Shell(")
                            .and_then(|s| s.strip_suffix(')'))
                            .map(|s| format!("shell:{s}"))
                            .or_else(|| {
                                p.strip_prefix("Write(")
                                    .and_then(|s| s.strip_suffix(')'))
                                    .map(|s| format!("edit:{s}"))
                            })
                            .or_else(|| {
                                p.strip_prefix("Read(")
                                    .and_then(|s| s.strip_suffix(')'))
                                    .map(|s| format!("read:{s}"))
                            });
                        if let Some(p) = translated {
                            list.push(p);
                        } else {
                            notes.push(format!("{path}: permissions.{decision}: no translation"));
                        }
                    }
                }
            }
            if target == "gemini" {
                for p in v["tools"]["allowed"]
                    .as_array()
                    .unwrap_or(&vec![])
                    .iter()
                    .filter_map(Value::as_str)
                {
                    if let Some(p) = p
                        .strip_prefix("run_shell_command(")
                        .and_then(|s| s.strip_suffix(')'))
                    {
                        permissions.allow.push(format!("shell:{p}"));
                    } else {
                        notes.push(format!("{path}: tools.allowed: no translation"));
                    }
                }
            }
            if target == "grok" {
                grok_permissions(&v["permission"], path, &mut permissions, &mut notes);
            }
            let shell = if target == "copilot" {
                v.get("chat.tools.terminal.autoApprove")
            } else if target == "opencode" {
                v["permission"].get("bash")
            } else {
                None
            };
            if let Some(shell) = shell.and_then(Value::as_object) {
                for (prefix, decision) in shell {
                    let allow = decision == true || decision == "allow";
                    let deny = decision == false || decision == "deny";
                    if allow || deny {
                        let prefix = if target == "opencode" {
                            prefix.strip_suffix(" *").unwrap_or(prefix)
                        } else {
                            prefix
                        };
                        if allow {
                            permissions.allow.push(format!("shell:{prefix}"));
                        } else {
                            permissions.deny.push(format!("shell:{prefix}"));
                        }
                    } else {
                        notes.push(format!("{path}: permissions: no translation"));
                    }
                }
            }
            let events = if target == "antigravity" {
                v["cedr"].as_object()
            } else {
                v["hooks"].as_object()
            };
            if let Some(events) = events {
                for (event, groups) in events {
                    let mapped = match event.as_str() {
                        "beforeShellExecution" | "BeforeTool" | "PreToolUse" => {
                            Some("before-shell")
                        }
                        "afterFileEdit" | "AfterTool" | "PostToolUse" => Some("after-edit"),
                        "SessionStart" | "sessionStart" => Some("session-start"),
                        "Stop" | "stop" | "AfterAgent" | "sessionEnd" => Some("stop"),
                        _ => None,
                    };
                    let Some(event) = mapped else {
                        notes.push(format!("{path}: hooks event: no translation"));
                        continue;
                    };
                    for group in groups.as_array().unwrap_or(&vec![]) {
                        let matcher = match (target, event) {
                            ("codex", "before-shell") => Some("Bash"),
                            ("codex", "after-edit") => Some("apply_patch"),
                            ("gemini", "before-shell") => Some("run_shell_command"),
                            ("gemini", "after-edit") => Some("write_file|replace"),
                            ("grok", "before-shell") => Some("Bash"),
                            ("grok", "after-edit") => Some("Edit|Write"),
                            ("antigravity", "before-shell") => Some("run_command"),
                            ("antigravity", "after-edit") => {
                                Some("write_file|replace_file_content")
                            }
                            _ => None,
                        };
                        if let Some(expected) = matcher
                            && group["matcher"].as_str() != Some(expected)
                        {
                            notes.push(format!("{path}: hooks matcher: no translation"));
                            continue;
                        }
                        let commands = group["hooks"]
                            .as_array()
                            .cloned()
                            .unwrap_or_else(|| vec![group.clone()]);
                        for command in commands {
                            if let Some(command) =
                                command["command"].as_str().or(command["bash"].as_str())
                            {
                                hooks.push(config::Hook {
                                    event: event.into(),
                                    command: if target == "grok" {
                                        command.strip_prefix("../../").unwrap_or(command).into()
                                    } else {
                                        command.into()
                                    },
                                    agents: Some(vec![target.into()]),
                                });
                            } else {
                                notes.push(format!("{path}: hooks command: no translation"));
                            }
                        }
                    }
                }
            }
        }
    }
    permissions.allow.sort();
    permissions.allow.dedup();
    permissions.deny.sort();
    permissions.deny.dedup();
    let c = Config {
        version: 1,
        targets: targets.unwrap_or(found),
        mcp,
        permissions,
        hooks,
    };
    let text = toml::to_string_pretty(&c).map_err(|_| "init: cannot serialize sources")?;
    Config::parse(&text)?;
    writes.insert(".cedr/sync.toml".into(), text.into_bytes());
    for path in writes.keys() {
        io::path(root, path)?;
    }
    for (path, bytes) in writes {
        io::write(root, &path, &bytes)?;
    }
    notes.sort();
    Ok(notes)
}
