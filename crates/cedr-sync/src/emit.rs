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
/// Whether an owned value says nothing: an empty list or map, or a map of only such values (`{"allow": [], "deny": []}`).
fn is_empty(v: &Value) -> bool {
    match v {
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.values().all(is_empty),
        _ => false,
    }
}
/// A merged settings file of the keys Sync owns. Keys with nothing in them are left out, and a file left with only its
/// format `version` is not written at all, so a repo without permissions or hooks gets no empty settings files.
fn object(out: &mut Outputs, path: &str, target: &str, mut value: Value) {
    let map = value.as_object_mut().expect("object");
    map.retain(|k, v| k == "version" || !is_empty(v));
    if map.keys().all(|k| k == "version") {
        return;
    }
    let keys = map.keys().cloned().collect();
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
/// Grok Build skips the tools of a server whose name it cannot namespace as `<server>__<tool>`.
fn grok_server_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && !name.ends_with('_')
        && !name.contains("__")
}
/// Grok Build resolves a command that is only a relative path against the hook file's folder, and a hook that cannot
/// start is skipped without a word, so point it back at the repo root. A command with arguments runs in a shell there.
fn grok_hook_command(command: &str) -> String {
    let relative = command.contains('/')
        && !command.starts_with(['/', '~', '$', '"', '\''])
        && !command.contains(char::is_whitespace);
    if relative {
        format!("../../{}", command.strip_prefix("./").unwrap_or(command))
    } else {
        command.to_string()
    }
}
/// `[permission] rules` entries. A rule with no `pattern` covers the whole tool.
fn grok_rules(c: &Config) -> Value {
    let mut rules = vec![];
    for (action, entries) in [
        ("allow", &c.permissions.allow),
        ("deny", &c.permissions.deny),
    ] {
        for entry in entries {
            let (kind, value) = entry.split_once(':').expect("validated");
            let mut rule = |tool: &str, pattern: Option<String>| {
                let mut r = json!({"action": action, "tool": tool});
                if let Some(p) = pattern {
                    r["pattern"] = json!(p);
                }
                rules.push(r);
            };
            match kind {
                "shell" if value == "*" => rule("bash", None),
                "shell" => {
                    rule("bash", Some(value.to_string()));
                    rule("bash", Some(format!("{value} *")));
                }
                "read" | "edit" => rule(kind, Some(value.to_string())),
                "fetch" => rule("webfetch", Some(format!("domain:{value}"))),
                _ => rule(
                    "mcp",
                    Some(match value.split_once('/') {
                        Some((server, tool)) => format!("{server}__{tool}"),
                        None => format!("{value}__*"),
                    }),
                ),
            }
        }
    }
    json!(rules)
}
fn permissions(c: &Config, target: &str, notes: &mut Vec<String>) -> Value {
    if target == "grok" {
        return grok_rules(c);
    }
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
            ("grok", "before-shell") => Some(("PreToolUse", Some("Bash"))),
            ("grok", "after-edit") => Some(("PostToolUse", Some("Edit|Write"))),
            ("claude" | "codex" | "gemini" | "antigravity" | "grok", "session-start") => {
                Some(("SessionStart", None))
            }
            ("claude" | "codex" | "antigravity" | "grok", "stop") => Some(("Stop", None)),
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
        } else if target == "grok" {
            // Grok Build has no `cwd` field: its hooks always run in the workspace root.
            json!({"type":"command","command":grok_hook_command(&hook.command),"timeout":60})
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
const GROK_TRUST: &str = "grok: Grok Build holds back this repo's instructions, skills, MCP servers and hooks until the folder is trusted: run grok --trust or /hooks-trust once";
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
    let both_claude = ["claude", "grok"]
        .iter()
        .all(|t| c.targets.iter().any(|x| x == t));
    if both_claude {
        for r in rules.iter().filter(|r| selected(&r.meta.agents, "claude")) {
            if r.meta.paths.is_some() {
                notes.push(format!("rule {0}: Grok Build also loads .claude/rules/{0}.md, for every file, unless [compat.claude] rules = false in ~/.grok/config.toml", r.id));
            }
            if r.meta.agents.is_some() && !selected(&r.meta.agents, "grok") {
                notes.push(format!(
                    "rule {}: Grok Build also reads CLAUDE.md, so this rule reaches grok",
                    r.id
                ));
            }
        }
    }
    for target in &c.targets {
        let target = target.as_str();
        if target == "agents" {
            // `agents` is only AGENTS.md and shared skills. Say so once, and only when no other target would carry
            // the servers, permissions and hooks, since then they reach no agent at all.
            let unused = !c.mcp.is_empty()
                || !c.permissions.allow.is_empty()
                || !c.permissions.deny.is_empty()
                || !c.hooks.is_empty();
            if unused && c.targets.iter().all(|t| t == "agents") {
                notes.push("MCP servers, permissions and hooks reach no agent: add a target such as claude or codex".into());
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
                // Grok Build always reads `CLAUDE.md`, which holds this rule too, so a second copy would only repeat it.
                "grok" if own && both_claude && selected(&r.meta.agents, "claude") => continue,
                "grok" if own => {
                    // Grok Build loads every file in `.grok/rules/` as it is, with no front matter or path scoping.
                    let heading = r.meta.paths.as_ref().map_or(String::new(), |paths| {
                        format!("## When working in {}\n\n", paths.join(", "))
                    });
                    doc(
                        &mut out,
                        &format!(".grok/rules/{}.md", r.id),
                        target,
                        format!("{heading}{}{org}", r.body.trim()),
                    );
                    continue;
                }
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
        if let Some(root) = match target {
            "claude" => Some(".claude"),
            "grok" => Some(".grok"),
            _ => None,
        } {
            for (name, tree) in skills {
                for (p, b) in tree {
                    out.insert(
                        format!("{root}/skills/{name}/{p}"),
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
                if target == "grok" && !grok_server_name(name) {
                    note(&mut notes, target, &format!("mcp.{name}"));
                } else if let Some(v) = server(s, target) {
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
                if !text.is_empty() {
                    raw(&mut out, ".codex/rules/cedr.rules", target, text);
                }
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
                // Gemini's settings always carry context.fileName; the other owned keys only when they say something.
                let gemini = out.get_mut(".gemini/settings.json").expect("output");
                let present: Value = serde_json::from_slice(&gemini.bytes).expect("json");
                gemini.keys = ["context.fileName", "mcpServers", "tools.allowed", "hooks"]
                    .into_iter()
                    .filter(|k| {
                        k.split('.')
                            .try_fold(&present, |v, part| v.get(part))
                            .is_some()
                    })
                    .map(String::from)
                    .collect();
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
            "grok" => {
                object(
                    &mut out,
                    ".grok/config.toml",
                    target,
                    json!({"mcp_servers":servers,"permission":{"rules":p}}),
                );
                object(
                    &mut out,
                    ".grok/hooks/cedr.json",
                    target,
                    json!({"hooks":h}),
                );
                notes.push(GROK_TRUST.into());
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
