//! Native target mappings. Unsupported source entries produce explicit notes.
use crate::{
    config::{Config, Server, selected},
    sources::{Rule, Skills},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
#[derive(Clone)]
pub struct Output {
    pub target: String,
    pub bytes: Vec<u8>,
    pub keys: Vec<String>,
}
pub type Outputs = BTreeMap<String, Output>;
fn raw(out: &mut Outputs, path: &str, target: &str, text: String) {
    out.insert(
        path.into(),
        Output {
            target: target.into(),
            bytes: text.into_bytes(),
            keys: vec![],
        },
    );
}
fn doc(out: &mut Outputs, path: &str, target: &str, text: String) {
    raw(out, path, target, crate::with_header(&text));
}
fn object(out: &mut Outputs, path: &str, target: &str, value: Value) {
    let keys = value.as_object().expect("object").keys().cloned().collect();
    out.insert(
        path.into(),
        Output {
            target: target.into(),
            bytes: format!("{}\n", serde_json::to_string_pretty(&value).expect("json"))
                .into_bytes(),
            keys,
        },
    );
}
fn note(notes: &mut Vec<String>, target: &str, what: &str) {
    notes.push(format!("{what}: not supported by {target}"));
}
fn variables(value: &str, target: &str) -> String {
    let re = regex::Regex::new(r"\$\{([A-Z_][A-Z0-9_]*)\}").expect("regex");
    match target {
        "cursor" | "copilot" => re.replace_all(value, "$${env:$1}").into_owned(),
        "opencode" => re.replace_all(value, "{env:$1}").into_owned(),
        _ => value.into(),
    }
}
fn server(s: &Server, target: &str) -> Option<Value> {
    let any_variable = s
        .env
        .values()
        .chain(s.headers.values())
        .chain(s.command.iter())
        .chain(s.url.iter())
        .chain(s.args.iter())
        .any(|v| v.contains("${"));
    if target == "antigravity" && any_variable {
        return None;
    }
    if target == "codex"
        && s.command
            .iter()
            .chain(s.url.iter())
            .chain(s.args.iter())
            .any(|v| v.contains("${"))
    {
        return None;
    }
    let mut v = json!({});
    if let Some(command) = &s.command {
        if target == "opencode" {
            v["type"] = json!("local");
            v["command"] = json!(
                std::iter::once(command.clone())
                    .chain(s.args.clone())
                    .collect::<Vec<_>>()
            );
        } else {
            v["command"] = json!(command);
            if !s.args.is_empty() {
                v["args"] = json!(s.args);
            }
        }
    } else if let Some(url) = &s.url {
        v[match target {
            "gemini" => "httpUrl",
            "antigravity" => "serverUrl",
            _ => "url",
        }] = json!(url);
        if target == "opencode" {
            v["type"] = json!("remote");
        }
    }
    if target == "codex" {
        let mut vars = vec![];
        for (k, value) in &s.env {
            if value == &format!("${{{k}}}") {
                vars.push(k);
            } else {
                return None;
            }
        }
        if !vars.is_empty() {
            v["env_vars"] = json!(vars);
        }
        for (k, value) in &s.headers {
            if k.eq_ignore_ascii_case("authorization")
                && value.starts_with("Bearer ${")
                && value.ends_with('}')
                && value.matches("${").count() == 1
            {
                v["bearer_token_env_var"] = json!(&value[9..value.len() - 1]);
            } else if value.starts_with("${")
                && value.ends_with('}')
                && value.matches("${").count() == 1
            {
                if v.get("env_http_headers").is_none() {
                    v["env_http_headers"] = json!({});
                }
                v["env_http_headers"][k] = json!(&value[2..value.len() - 1]);
            } else if value.contains("${") {
                return None;
            } else {
                if v.get("http_headers").is_none() {
                    v["http_headers"] = json!({});
                }
                v["http_headers"][k] = json!(value);
            }
        }
    } else {
        for (key, map) in [
            (
                if target == "opencode" {
                    "environment"
                } else {
                    "env"
                },
                &s.env,
            ),
            ("headers", &s.headers),
        ] {
            if !map.is_empty() {
                v[key] = json!(
                    map.iter()
                        .map(|(k, v)| (k.clone(), variables(v, target)))
                        .collect::<BTreeMap<_, _>>()
                );
            }
        }
    }
    Some(v)
}
fn permissions(c: &Config, target: &str, notes: &mut Vec<String>) -> Value {
    let mut v = json!({});
    for (decision, entries) in [
        ("allow", &c.permissions.allow),
        ("deny", &c.permissions.deny),
    ] {
        let mut list = vec![];
        for entry in entries {
            let (kind, prefix) = entry.split_once(':').expect("validated");
            let mapped = match (target, kind) {
                ("claude", "shell") if prefix == "*" => Some("Bash".into()),
                ("claude", "shell") => {
                    list.push(format!("Bash({prefix})"));
                    Some(format!("Bash({prefix} *)"))
                }
                ("claude", "read") => Some(format!("Read({prefix})")),
                ("claude", "edit") => Some(format!("Edit({prefix})")),
                ("claude", "fetch") => Some(format!("WebFetch(domain:{prefix})")),
                ("claude", "mcp") => Some(format!("mcp__{}", prefix.replace('/', "__"))),
                ("cursor", "shell") if prefix.split_whitespace().count() == 1 => {
                    Some(format!("Shell({prefix})"))
                }
                ("cursor", "read") => Some(format!("Read({prefix})")),
                ("cursor", "edit") => Some(format!("Write({prefix})")),
                ("gemini", "shell") if decision == "allow" => {
                    Some(format!("run_shell_command({prefix})"))
                }
                ("copilot", "shell") => {
                    v[prefix] = json!(decision == "allow");
                    continue;
                }
                ("opencode", "shell") => {
                    v[format!("{prefix} *")] = json!(decision);
                    continue;
                }
                ("codex", "shell") if prefix != "*" => continue,
                _ => None,
            };
            if let Some(m) = mapped {
                list.push(m);
            } else {
                note(notes, target, entry);
            }
        }
        list.sort();
        list.dedup();
        if ["claude", "cursor"].contains(&target) {
            v[decision] = json!(list);
        } else if target == "gemini" && decision == "allow" {
            v = json!(list);
        }
    }
    v
}
fn hooks(c: &Config, target: &str, notes: &mut Vec<String>) -> Value {
    let mut v = json!({});
    for hook in &c.hooks {
        if !selected(&hook.agents, target) {
            continue;
        }
        let mapping = match (target, hook.event.as_str()) {
            ("claude" | "codex", "before-shell") => Some(("PreToolUse", Some("Bash"))),
            ("claude", "after-edit") => Some(("PostToolUse", Some("Edit|Write|MultiEdit"))),
            ("codex", "after-edit") => Some(("PostToolUse", Some("apply_patch"))),
            ("gemini", "before-shell") => Some(("BeforeTool", Some("run_shell_command"))),
            ("gemini", "after-edit") => Some(("AfterTool", Some("write_file|replace"))),
            ("antigravity", "before-shell") => Some(("PreToolUse", Some("run_command"))),
            ("antigravity", "after-edit") => {
                Some(("PostToolUse", Some("write_file|replace_file_content")))
            }
            ("claude" | "codex" | "gemini" | "antigravity", "session-start") => {
                Some(("SessionStart", None))
            }
            ("claude" | "codex" | "antigravity", "stop") => Some(("Stop", None)),
            ("gemini", "stop") => Some(("AfterAgent", None)),
            ("cursor", "before-shell") => Some(("beforeShellExecution", None)),
            ("cursor", "after-edit") => Some(("afterFileEdit", None)),
            ("cursor" | "copilot", "session-start") => Some(("sessionStart", None)),
            ("cursor", "stop") => Some(("stop", None)),
            ("copilot", "stop") => Some(("sessionEnd", None)),
            _ => None,
        };
        let Some((event, matcher)) = mapping else {
            note(notes, target, &format!("hook {}", hook.event));
            continue;
        };
        let command = if target == "copilot" {
            json!({"type":"command","bash":hook.command,"powershell":hook.command,"cwd":".","timeoutSec":60})
        } else {
            json!({"type":"command","command":hook.command,"timeout":if target=="gemini" {60000}else{60},"cwd":"."})
        };
        let item = if ["cursor", "copilot"].contains(&target) {
            command
        } else {
            let mut group = json!({"hooks":[command]});
            if let Some(m) = matcher {
                group["matcher"] = json!(m);
            }
            group
        };
        if v.get(event).is_none() {
            v[event] = json!([]);
        }
        v[event].as_array_mut().expect("array").push(item);
    }
    v
}
pub fn emit(c: &Config, rules: &[Rule], skills: &Skills, org: &str) -> (Outputs, Vec<String>) {
    let mut out = Outputs::new();
    let mut notes = vec![];
    let mut shared = String::new();
    for r in rules.iter().filter(|r| r.meta.agents.is_none()) {
        if let Some(paths) = &r.meta.paths {
            shared.push_str(&format!("## When working in {}\n\n", paths.join(", ")));
        }
        shared.push_str(r.body.trim());
        shared.push_str("\n\n");
    }
    let org = if org.is_empty() {
        String::new()
    } else {
        format!("\n## Organization rules\n\n{}\n", org.trim())
    };
    shared.push_str(&org);
    doc(&mut out, "AGENTS.md", "agents", shared);
    for (name, tree) in skills {
        for (p, b) in tree {
            out.insert(
                format!(".agents/skills/{name}/{p}"),
                Output {
                    target: "agents".into(),
                    bytes: b.clone(),
                    keys: vec![],
                },
            );
        }
    }
    for target in &c.targets {
        let target = target.as_str();
        if target == "agents" {
            for entry in c.permissions.allow.iter().chain(&c.permissions.deny) {
                note(&mut notes, target, entry);
            }
            for hook in &c.hooks {
                if selected(&hook.agents, target) {
                    note(&mut notes, target, &format!("hook {}", hook.event));
                }
            }
            for (name, server) in &c.mcp {
                if selected(&server.agents, target) {
                    note(&mut notes, target, &format!("mcp.{name}"));
                }
            }
            continue;
        }
        let own: Vec<_> = rules
            .iter()
            .filter(|r| r.meta.agents.is_some() && selected(&r.meta.agents, target))
            .collect();
        let text = own
            .iter()
            .map(|r| r.body.trim())
            .collect::<Vec<_>>()
            .join("\n\n");
        let instruction = match target {
            "claude" => Some("CLAUDE.md"),
            "gemini" => Some("GEMINI.md"),
            "copilot" => Some(".github/copilot-instructions.md"),
            _ => None,
        };
        if !own.is_empty()
            && let Some(p) = instruction
        {
            doc(
                &mut out,
                p,
                target,
                format!(
                    "{}\n\n{text}{org}",
                    if target == "claude" { "@AGENTS.md" } else { "" }
                ),
            );
        }
        for r in rules {
            if !selected(&r.meta.agents, target) {
                continue;
            }
            let scoped = r.meta.paths.is_some();
            let own = r.meta.agents.is_some();
            let path = match target {
                "claude" if scoped => Some(format!(".claude/rules/{}.md", r.id)),
                "cursor" if scoped || own => Some(format!(".cursor/rules/{}.mdc", r.id)),
                "copilot" if scoped => {
                    Some(format!(".github/instructions/{}.instructions.md", r.id))
                }
                "antigravity" if scoped || own => Some(format!(".agents/rules/{}.md", r.id)),
                _ => None,
            };
            if own && path.is_none() && instruction.is_none() {
                note(
                    &mut notes,
                    target,
                    &format!("rule {} scoped to agent", r.id),
                );
            }
            if let Some(path) = path {
                let mut front = String::new();
                if let Some(d) = &r.meta.description {
                    front.push_str(&format!("description: {}\n", json!(d)));
                }
                if let Some(paths) = &r.meta.paths {
                    front.push_str(&format!(
                        "{}: {}\n",
                        match target {
                            "cursor" => "globs",
                            "copilot" => "applyTo",
                            "antigravity" => "globs",
                            _ => "paths",
                        },
                        if ["cursor", "copilot", "antigravity"].contains(&target) {
                            json!(paths.join(",")).to_string()
                        } else {
                            json!(paths).to_string()
                        }
                    ));
                }
                if target == "antigravity" {
                    front.push_str(&format!(
                        "trigger: {}\n",
                        if scoped { "glob" } else { "always_on" }
                    ));
                }
                if target == "cursor" {
                    front.push_str(&format!("alwaysApply: {}\n", !scoped));
                }
                doc(
                    &mut out,
                    &path,
                    target,
                    format!("---\n{front}---\n{}{org}", r.body.trim()),
                );
            }
        }
        if target == "claude" {
            for (name, tree) in skills {
                for (p, b) in tree {
                    out.insert(
                        format!(".claude/skills/{name}/{p}"),
                        Output {
                            target: target.into(),
                            bytes: b.clone(),
                            keys: vec![],
                        },
                    );
                }
            }
        }
        let mut servers = json!({});
        for (name, s) in &c.mcp {
            if selected(&s.agents, target) {
                if let Some(v) = server(s, target) {
                    servers[name] = v;
                } else {
                    note(&mut notes, target, &format!("mcp.{name}"));
                }
            }
        }
        let p = permissions(c, target, &mut notes);
        let h = hooks(c, target, &mut notes);
        match target {
            "claude" => {
                object(&mut out, ".mcp.json", target, json!({"mcpServers":servers}));
                object(
                    &mut out,
                    ".claude/settings.json",
                    target,
                    json!({"permissions":p,"hooks":h}),
                );
            }
            "codex" => {
                // A JSON intermediate keeps owned-key hashing identical to JSON targets.
                object(
                    &mut out,
                    ".codex/config.toml",
                    target,
                    json!({"mcp_servers":servers}),
                );
                let mut text = String::new();
                for (decision, list) in [
                    ("allow", &c.permissions.allow),
                    ("forbidden", &c.permissions.deny),
                ] {
                    for entry in list {
                        if let Some(prefix) = entry.strip_prefix("shell:")
                            && prefix != "*"
                        {
                            text.push_str(&format!(
                                "prefix_rule(pattern = {}, decision = {})\n",
                                json!(prefix.split_whitespace().collect::<Vec<_>>()),
                                json!(decision)
                            ));
                        }
                    }
                }
                raw(&mut out, ".codex/rules/cedr.rules", target, text);
                object(&mut out, ".codex/hooks.json", target, json!({"hooks":h}));
            }
            "cursor" => {
                object(
                    &mut out,
                    ".cursor/mcp.json",
                    target,
                    json!({"mcpServers":servers}),
                );
                object(
                    &mut out,
                    ".cursor/cli.json",
                    target,
                    json!({"permissions":p}),
                );
                object(
                    &mut out,
                    ".cursor/hooks.json",
                    target,
                    json!({"version":1,"hooks":h}),
                );
            }
            "gemini" => {
                object(
                    &mut out,
                    ".gemini/settings.json",
                    target,
                    json!({"context":{"fileName":if own.is_empty(){vec!["AGENTS.md"]}else{vec!["AGENTS.md","GEMINI.md"]}},"mcpServers":servers,"tools":{"allowed":p},"hooks":h}),
                );
                out.get_mut(".gemini/settings.json").expect("output").keys = vec![
                    "context.fileName".into(),
                    "mcpServers".into(),
                    "tools.allowed".into(),
                    "hooks".into(),
                ];
            }
            "copilot" => {
                object(
                    &mut out,
                    ".vscode/mcp.json",
                    target,
                    json!({"servers":servers}),
                );
                object(
                    &mut out,
                    ".vscode/settings.json",
                    target,
                    json!({"chat.tools.terminal.autoApprove":p}),
                );
                object(
                    &mut out,
                    ".github/hooks/cedr.json",
                    target,
                    json!({"version":1,"hooks":h}),
                );
            }
            "opencode" => {
                object(
                    &mut out,
                    "opencode.json",
                    target,
                    json!({"mcp":servers,"permission":{"bash":p}}),
                );
                out.get_mut("opencode.json").expect("output").keys =
                    vec!["mcp".into(), "permission.bash".into()];
            }
            "antigravity" => {
                object(
                    &mut out,
                    ".agents/mcp_config.json",
                    target,
                    json!({"mcpServers":servers}),
                );
                object(&mut out, ".agents/hooks.json", target, json!({"cedr":h}));
            }
            _ => (),
        }
    }
    notes.sort();
    notes.dedup();
    (out, notes)
}
