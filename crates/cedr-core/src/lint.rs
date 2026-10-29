//! `cedr lint`: rules CG001-CG009 from the design contract.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::discovery::{self, Launch, Workspace, read_optional, relative};
use crate::error::{io_err, parse_err};
use crate::finding::{Finding, Report, Rule, Severity, line_of};
use crate::skill::{self, Field, ParsedFrontmatter};
use crate::{Result, hidden, risk};

/// Skill files larger than this are not scanned for text patterns.
const MAX_SCAN_BYTES: u64 = 1024 * 1024;

pub fn lint(root: &Path) -> Result<Report> {
    let ws = discovery::discover(root)?;
    let mut findings = Vec::new();
    let mut packages = PackageCache::default();
    for rel in &ws.instructions {
        let path = root.join(rel);
        let text = discovery::read_text(&path)?;
        findings.extend(dead_commands(root, rel, &text, &mut packages)?);
        findings.extend(broken_links(root, rel, &text));
        findings.extend(hidden_unicode(rel, &text));
    }
    findings.extend(conflicting_instructions(&ws)?);
    for skill in &ws.skills {
        findings.extend(skill_findings(root, skill)?);
    }
    findings.extend(server_findings(&ws));
    findings.extend(broad_permissions(root)?);
    Ok(Report::new(findings))
}

static NPM_RUN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|[^\w./-])npm\s+(?:--prefix[=\s]+(?P<dir>[^\s`'\x22]+)\s+)?run(?:-script)?\s+(?P<script>[A-Za-z0-9_][\w:.-]*)",
    )
    .expect("regex")
});
static PNPM_YARN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|[^\w./-])(?P<tool>pnpm|yarn)\s+(?:(?P<run>run)\s+)?(?P<script>[A-Za-z_][\w:.-]*)",
    )
    .expect("regex")
});

// Subcommands the package managers run themselves rather than as scripts.
const PNPM_BUILTINS: &[&str] = &[
    "add",
    "approve-builds",
    "audit",
    "bin",
    "cat-file",
    "cat-index",
    "config",
    "create",
    "dedupe",
    "deploy",
    "dlx",
    "doctor",
    "env",
    "exec",
    "fetch",
    "find-hash",
    "help",
    "i",
    "ignored-builds",
    "import",
    "init",
    "install",
    "install-test",
    "it",
    "licenses",
    "link",
    "list",
    "ln",
    "ls",
    "outdated",
    "pack",
    "patch",
    "patch-commit",
    "patch-remove",
    "prune",
    "publish",
    "rb",
    "rebuild",
    "remove",
    "rm",
    "root",
    "run",
    "self-update",
    "server",
    "setup",
    "store",
    "un",
    "uninstall",
    "unlink",
    "up",
    "update",
    "upgrade",
    "why",
    "x",
];
const YARN_BUILTINS: &[&str] = &[
    "add",
    "audit",
    "autoclean",
    "bin",
    "cache",
    "config",
    "constraints",
    "create",
    "dedupe",
    "dlx",
    "exec",
    "explain",
    "generate-lock-entry",
    "global",
    "help",
    "import",
    "info",
    "init",
    "install",
    "licenses",
    "link",
    "list",
    "login",
    "logout",
    "node",
    "npm",
    "outdated",
    "owner",
    "pack",
    "patch",
    "patch-commit",
    "plugin",
    "policies",
    "publish",
    "rebuild",
    "remove",
    "run",
    "search",
    "set",
    "stage",
    "tag",
    "team",
    "unlink",
    "unplug",
    "up",
    "upgrade",
    "upgrade-interactive",
    "version",
    "versions",
    "why",
    "workspace",
    "workspaces",
];

/// Scripts of each `package.json` read so far; `None` when the file is absent.
#[derive(Default)]
struct PackageCache(BTreeMap<PathBuf, Option<Vec<String>>>);

impl PackageCache {
    fn scripts(&mut self, path: &Path) -> Result<Option<&Vec<String>>> {
        if !self.0.contains_key(path) {
            let scripts = match read_optional(path)? {
                None => None,
                Some(text) => {
                    let value: serde_json::Value =
                        serde_json::from_str(&text).map_err(|e| parse_err(path, e))?;
                    Some(
                        value
                            .get("scripts")
                            .and_then(|s| s.as_object())
                            .map(|s| s.keys().cloned().collect())
                            .unwrap_or_default(),
                    )
                }
            };
            self.0.insert(path.to_path_buf(), scripts);
        }
        Ok(self.0[path].as_ref())
    }
}

/// CG001: a script command whose script is not in the nearest `package.json`
/// (searched upward from the instruction file), or in `D/package.json` for
/// `npm --prefix D`, with `D` relative to the instruction file.
fn dead_commands(
    root: &Path,
    rel: &str,
    text: &str,
    packages: &mut PackageCache,
) -> Result<Vec<Finding>> {
    let dir = root
        .join(rel)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut commands = Vec::new();
    for c in NPM_RUN.captures_iter(text) {
        let m = c.name("script").expect("group");
        commands.push((
            "npm",
            c.name("dir").map(|d| d.as_str().to_string()),
            m.as_str(),
            m.start(),
        ));
    }
    for c in PNPM_YARN.captures_iter(text) {
        let tool = c.name("tool").expect("group").as_str();
        let m = c.name("script").expect("group");
        let builtins = if tool == "pnpm" {
            PNPM_BUILTINS
        } else {
            YARN_BUILTINS
        };
        if c.name("run").is_none() && builtins.contains(&m.as_str()) {
            continue;
        }
        commands.push((tool, None, m.as_str(), m.start()));
    }
    let mut findings = Vec::new();
    for (tool, prefix, script, offset) in commands {
        let script = script.trim_end_matches(['.', ':', ',']);
        let package = match &prefix {
            Some(d) => Some(dir.join(d).join("package.json")),
            None => nearest_package(root, &dir),
        };
        let line = Some(line_of(text, offset));
        let missing = match &package {
            Some(p) => match packages.scripts(p)? {
                Some(scripts) => {
                    if scripts.iter().any(|s| s == script) {
                        continue;
                    }
                    format!("not found in {}", relative(root, p))
                }
                None => format!("not found: {} does not exist", relative(root, p)),
            },
            None => "not found: no package.json".to_string(),
        };
        findings.push(Finding::new(
            Rule::Cg001,
            Severity::Error,
            rel,
            line,
            format!("{tool} script \"{script}\" {missing}"),
        ));
    }
    Ok(findings)
}

fn nearest_package(root: &Path, start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let candidate = dir.join("package.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        if dir == root {
            return None;
        }
        dir = dir.parent()?;
    }
}

static INLINE_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\]\(\s*(?:<(?P<angle>[^>\n]+)>|(?P<bare>[^\s)<>]+))").expect("regex")
});
static REFERENCE_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^ {0,3}\[[^\]\n]+\]:\s*(?:<(?P<angle>[^>\n]+)>|(?P<bare>\S+))").expect("regex")
});
static URL_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:").expect("regex"));
static LINE_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":\d+(?:-\d+)?$").expect("regex"));
static INLINE_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`+[^`]*`+").expect("regex"));

/// CG002: relative markdown links outside code whose target is missing.
/// A trailing `:LINE` or `:START-END` is a line reference, not part of the path.
fn broken_links(root: &Path, rel: &str, text: &str) -> Vec<Finding> {
    let dir = root
        .join(rel)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut findings = Vec::new();
    let mut fence: Option<String> = None;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(marker) = &fence {
            if trimmed.starts_with(marker.as_str()) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(trimmed[..3].to_string());
            continue;
        }
        let code_free = INLINE_CODE.replace_all(line, |c: &regex::Captures| " ".repeat(c[0].len()));
        let targets = INLINE_LINK
            .captures_iter(&code_free)
            .chain(REFERENCE_LINK.captures_iter(&code_free))
            .filter_map(|c| {
                c.name("angle")
                    .or(c.name("bare"))
                    .map(|m| m.as_str().to_string())
            })
            .collect::<Vec<_>>();
        for target in targets {
            let Some(path) = link_path(&target) else {
                continue;
            };
            if !dir.join(&path).exists() {
                findings.push(Finding::new(
                    Rule::Cg002,
                    Severity::Warning,
                    rel,
                    Some(index + 1),
                    format!("link target \"{target}\" does not exist"),
                ));
            }
        }
    }
    findings
}

fn link_path(target: &str) -> Option<String> {
    if target.starts_with('#') || target.starts_with('/') || URL_SCHEME.is_match(target) {
        return None;
    }
    let path = target.split(['#', '?']).next().unwrap_or_default();
    let path = LINE_SUFFIX.replace(path, "");
    let path = path.replace("%20", " ");
    (!path.is_empty()).then_some(path)
}

/// CG005 over one file.
fn hidden_unicode(rel: &str, text: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut last_line = 0;
    for (offset, c) in hidden::find_hidden(text) {
        let line = line_of(text, offset);
        if line == last_line {
            continue;
        }
        last_line = line;
        findings.push(Finding::new(
            Rule::Cg005,
            Severity::Error,
            rel,
            Some(line),
            format!(
                "hidden Unicode U+{:04X} ({})",
                c as u32,
                hidden::describe(c)
            ),
        ));
    }
    findings
}

/// CG003: `CLAUDE.md` or `GEMINI.md` beside `AGENTS.md` that says something else.
fn conflicting_instructions(ws: &Workspace) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    for agents in ws.instructions.iter().filter(|r| r.ends_with("AGENTS.md")) {
        let agents_path = ws.root.join(agents);
        let dir = agents_path.parent().unwrap_or(&ws.root);
        let agents_text = discovery::read_text(&agents_path)?;
        let agents_real = fs::canonicalize(&agents_path).map_err(io_err(&agents_path))?;
        for sibling in ["CLAUDE.md", "GEMINI.md"] {
            let path = dir.join(sibling);
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink()
                && fs::canonicalize(&path).is_ok_and(|real| real == agents_real)
            {
                continue;
            }
            discovery::ensure_inside(
                &fs::canonicalize(&ws.root).map_err(io_err(&ws.root))?,
                &path,
            )?;
            let Some(text) = read_optional(&path)? else {
                continue;
            };
            let import = matches!(text.trim(), "@AGENTS.md" | "@./AGENTS.md");
            if import || text == agents_text {
                continue;
            }
            findings.push(Finding::new(
                Rule::Cg003,
                Severity::Warning,
                relative(&ws.root, &path),
                None,
                format!(
                    "{sibling} differs from AGENTS.md; make it a symlink to AGENTS.md or a lone @AGENTS.md import"
                ),
            ));
        }
    }
    Ok(findings)
}

/// CG002, CG004, CG005 and CG009 for one skill directory.
fn skill_findings(root: &Path, skill: &discovery::SkillDir) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    let skill_md = skill.skill_md();
    let text = String::from_utf8_lossy(&skill::file_bytes(&skill.real_path.join("SKILL.md"))?)
        .into_owned();
    findings.extend(frontmatter_findings(skill, &skill_md, &text));
    findings.extend(broken_links(root, &skill_md, &text));

    let mut files = Vec::new();
    skill::collect_files(&skill.real_path, "", &mut files)?;
    files.sort();
    for file in files {
        let path = skill.real_path.join(&file);
        let meta = fs::symlink_metadata(&path).map_err(io_err(&path))?;
        if !(meta.is_file() || meta.file_type().is_symlink()) || meta.len() > MAX_SCAN_BYTES {
            continue;
        }
        let bytes = skill::file_bytes(&path)?;
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let rel = format!("{}/{file}", skill.path);
        findings.extend(hidden_unicode(&rel, &text));
        let mut reported = Vec::new();
        for m in risk::scan(&text, &rel) {
            let line = line_of(&text, m.offset);
            if reported.contains(&(line, m.rule)) {
                continue;
            }
            reported.push((line, m.rule));
            let severity = if m.high {
                Severity::Error
            } else {
                Severity::Warning
            };
            findings.push(Finding::new(
                Rule::Cg009,
                severity,
                &rel,
                Some(line),
                format!("SkillSpector {}", m.summary),
            ));
        }
    }
    Ok(findings)
}

/// CG004: SKILL.md frontmatter must carry a `name` equal to its directory
/// (at most 64 characters) and a `description` of at most 1024 characters.
fn frontmatter_findings(skill: &discovery::SkillDir, rel: &str, text: &str) -> Vec<Finding> {
    let error = |line: Option<usize>, message: String| {
        Finding::new(Rule::Cg004, Severity::Error, rel, line, message)
    };
    let fm = match skill::frontmatter(text) {
        ParsedFrontmatter::Absent => {
            return vec![error(
                Some(1),
                "SKILL.md has no frontmatter with name and description".into(),
            )];
        }
        ParsedFrontmatter::Invalid(message) => return vec![error(Some(1), message)],
        ParsedFrontmatter::Valid(fm) => fm,
    };
    let mut findings = Vec::new();
    match &fm.name {
        Field::Missing => findings.push(error(Some(1), "frontmatter has no name".into())),
        Field::NotText => findings.push(error(fm.name_line, "frontmatter name is not text".into())),
        Field::Text(name) => {
            if name != &skill.dir_name {
                findings.push(error(
                    fm.name_line,
                    format!(
                        "name \"{name}\" does not match its directory \"{}\"",
                        skill.dir_name
                    ),
                ));
            }
            let len = name.chars().count();
            if len > 64 {
                findings.push(error(
                    fm.name_line,
                    format!("name is {len} characters; the limit is 64"),
                ));
            }
        }
    }
    match &fm.description {
        Field::Missing => findings.push(error(Some(1), "frontmatter has no description".into())),
        Field::NotText => findings.push(error(
            fm.description_line,
            "frontmatter description is not text".into(),
        )),
        Field::Text(description) => {
            let len = description.chars().count();
            if len > 1024 {
                findings.push(error(
                    fm.description_line,
                    format!("description is {len} characters; the limit is 1024"),
                ));
            }
        }
    }
    findings
}

/// CG006 and CG008.
fn server_findings(ws: &Workspace) -> Vec<Finding> {
    let mut findings = Vec::new();
    for server in &ws.servers {
        let source = server.source();
        if !source.pinned() {
            findings.push(Finding::new(
                Rule::Cg006,
                Severity::Warning,
                &server.config_path,
                server.line,
                format!(
                    "MCP server \"{}\" ({}) is not pinned: {source}",
                    server.name,
                    server.client.as_str()
                ),
            ));
        }
        if let Launch::Remote { url } = &server.launch
            && is_plain_http_remote(url)
        {
            findings.push(Finding::new(
                Rule::Cg008,
                Severity::Warning,
                &server.config_path,
                server.line,
                format!(
                    "MCP server \"{}\" ({}) uses plain http: {url}",
                    server.name,
                    server.client.as_str()
                ),
            ));
        }
    }
    findings
}

fn is_plain_http_remote(url: &str) -> bool {
    let Some(rest) = url
        .get(..7)
        .filter(|s| s.eq_ignore_ascii_case("http://"))
        .map(|_| &url[7..])
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    let host = host.to_ascii_lowercase();
    let local = host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host.starts_with("127.");
    !local
}

/// CG007: Claude `Bash`/`Bash(*)` allows, Codex full access or no approvals.
fn broad_permissions(root: &Path) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    let rel = ".claude/settings.json";
    let path = root.join(rel);
    if let Some(text) = read_optional(&path)? {
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| parse_err(&path, e))?;
        let allows = value
            .pointer("/permissions/allow")
            .and_then(|a| a.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str());
        for allow in allows {
            if allow == "Bash" || allow == "Bash(*)" {
                let needle = serde_json::to_string(allow).expect("string serializes");
                let line = text.find(&needle).map(|i| line_of(&text, i));
                findings.push(Finding::new(
                    Rule::Cg007,
                    Severity::Warning,
                    rel,
                    line,
                    format!("permissions.allow {needle} lets the agent run any shell command"),
                ));
            }
        }
    }
    let rel = ".codex/config.toml";
    let path = root.join(rel);
    if let Some(text) = read_optional(&path)? {
        let table: toml::Table = toml::from_str(&text).map_err(|e| parse_err(&path, e))?;
        for (key, broad, why) in [
            (
                "sandbox_mode",
                "danger-full-access",
                "turns the sandbox off",
            ),
            (
                "approval_policy",
                "never",
                "never asks before running commands",
            ),
        ] {
            if table.get(key).and_then(|v| v.as_str()) == Some(broad) {
                let line = text
                    .lines()
                    .position(|l| l.trim_start().starts_with(key))
                    .map(|i| i + 1);
                findings.push(Finding::new(
                    Rule::Cg007,
                    Severity::Warning,
                    rel,
                    line,
                    format!("{key} = \"{broad}\" {why}"),
                ));
            }
        }
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_paths_skip_urls_anchors_and_line_suffixes() {
        assert_eq!(link_path("https://x.dev"), None);
        assert_eq!(link_path("mailto:a@b"), None);
        assert_eq!(link_path("#top"), None);
        assert_eq!(link_path("docs/a.md#part").as_deref(), Some("docs/a.md"));
        assert_eq!(link_path("src/a.ts:10").as_deref(), Some("src/a.ts"));
        assert_eq!(link_path("src/a.ts:10-20").as_deref(), Some("src/a.ts"));
    }

    #[test]
    fn plain_http_excludes_loopback() {
        assert!(is_plain_http_remote("http://mcp.example.net/sse"));
        assert!(is_plain_http_remote("HTTP://10.0.0.5:8080"));
        assert!(!is_plain_http_remote("http://localhost:3000/mcp"));
        assert!(!is_plain_http_remote("http://127.0.0.1:3000"));
        assert!(!is_plain_http_remote("http://[::1]:3000"));
        assert!(!is_plain_http_remote("https://mcp.example.net"));
    }
}
