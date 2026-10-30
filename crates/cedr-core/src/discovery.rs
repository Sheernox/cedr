//! Finds the instruction files, skills and MCP servers in a repo, per the
//! design's Discovery table. Skills and client configs live at fixed paths
//! under the root and are read directly, because repos often git-ignore their
//! agent folders; instruction files are found at any depth with the
//! gitignore-aware walker.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::config::GuardConfig;
use crate::error::{io_err, parse_err};
use crate::source::Source;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Client {
    Antigravity,
    Claude,
    Codex,
    Copilot,
    Cursor,
    Gemini,
    Opencode,
}

impl Client {
    pub fn as_str(self) -> &'static str {
        match self {
            Client::Claude => "claude",
            Client::Codex => "codex",
            Client::Cursor => "cursor",
            Client::Gemini => "gemini",
            Client::Copilot => "copilot",
            Client::Opencode => "opencode",
            Client::Antigravity => "antigravity",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Stdio,
    Http,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::Stdio => "stdio",
            Transport::Http => "http",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Remote {
        url: String,
    },
}

/// One MCP server entry in one client's config.
#[derive(Clone, Debug)]
pub struct Server {
    pub client: Client,
    pub name: String,
    /// Config file relative to the root.
    pub config_path: String,
    pub line: Option<usize>,
    pub launch: Launch,
}

impl Server {
    /// Fingerprints launch inputs without retaining environment values.
    pub fn launch_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let value = match &self.launch {
            Launch::Stdio { command, args, env } => serde_json::json!({
                "command": command, "args": args, "env": env.keys().collect::<Vec<_>>(), "url": null
            }),
            Launch::Remote { url } => serde_json::json!({
                "command": null, "args": [], "env": [], "url": url
            }),
        };
        format!(
            "sha256-{}",
            crate::skill::hex(&Sha256::digest(crate::canonical::to_string(&value)))
        )
    }

    pub fn transport(&self) -> Transport {
        match self.launch {
            Launch::Stdio { .. } => Transport::Stdio,
            Launch::Remote { .. } => Transport::Http,
        }
    }

    pub fn source(&self) -> Source {
        match &self.launch {
            Launch::Stdio { command, args, .. } => Source::from_command(command, args),
            Launch::Remote { url } => Source::Url(url.clone()),
        }
    }
}

/// A skill directory holding a `SKILL.md`.
#[derive(Clone, Debug)]
pub struct SkillDir {
    /// Directory name, which CG004 requires the frontmatter `name` to match.
    pub dir_name: String,
    /// Directory relative to the root, e.g. `.agents/skills/deploy`.
    pub path: String,
    /// Absolute directory with symlinks resolved; the content hash walks this.
    pub real_path: PathBuf,
}

impl SkillDir {
    pub fn skill_md(&self) -> String {
        format!("{}/SKILL.md", self.path)
    }
}

#[derive(Debug)]
pub struct Workspace {
    pub root: PathBuf,
    /// Standard instruction files, including Copilot instructions, relative to the root.
    pub instructions: Vec<String>,
    pub skills: Vec<SkillDir>,
    pub servers: Vec<Server>,
    /// Skill directories (or whole skill roots) skipped because they link outside the root,
    /// relative to the root; lint reports them as CG010.
    pub skipped: Vec<String>,
    pub config: GuardConfig,
}

pub const INSTRUCTION_FILES: [&str; 3] = ["AGENTS.md", "CLAUDE.md", "GEMINI.md"];
const SKILL_ROOTS: [&str; 6] = [
    ".agents/skills",
    ".claude/skills",
    ".cursor/skills",
    ".gemini/skills",
    ".github/skills",
    ".opencode/skills",
];

pub fn discover(root: &Path) -> Result<Workspace> {
    let meta = fs::metadata(root).map_err(io_err(root))?;
    if !meta.is_dir() {
        return Err(parse_err(root, "not a directory"));
    }
    let config = GuardConfig::load(root)?;
    let (skills, skipped) = skills(root, &config.skills)?;
    Ok(Workspace {
        root: root.to_path_buf(),
        instructions: instruction_files(root)?,
        skills,
        servers: servers(root)?,
        skipped,
        config,
    })
}

/// Path of `path` relative to `root` with `/` separators.
pub fn relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn instruction_files(root: &Path) -> Result<Vec<String>> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| {
            let name = entry.file_name();
            name != ".git" && name != "node_modules"
        })
        .build();
    let canonical_root = fs::canonicalize(root).map_err(io_err(root))?;
    let mut found = Vec::new();
    for entry in walker {
        let entry = entry.map_err(|e| parse_err(root, e))?;
        let is_file = entry
            .file_type()
            .is_some_and(|t| t.is_file() || t.is_symlink());
        let name = entry.file_name().to_string_lossy();
        if is_file && INSTRUCTION_FILES.contains(&name.as_ref()) {
            ensure_inside(&canonical_root, entry.path())?;
            found.push(relative(root, entry.path()));
        }
    }
    let copilot = root.join(".github/copilot-instructions.md");
    if copilot.exists() {
        ensure_inside(&canonical_root, &copilot)?;
        found.push(".github/copilot-instructions.md".into());
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// Reject directory and instruction links that escape the scan boundary.
pub(crate) fn ensure_inside(root: &Path, path: &Path) -> Result<PathBuf> {
    inside(root, path)?.ok_or_else(|| parse_err(path, "canonical path escapes the scanned root"))
}

/// The canonical path of `path`, or `None` when it escapes `root`.
fn inside(root: &Path, path: &Path) -> Result<Option<PathBuf>> {
    let real = fs::canonicalize(path).map_err(io_err(path))?;
    Ok(real.starts_with(root).then_some(real))
}

/// Skill directories under the standard roots and the config's extra ones, and the paths
/// skipped because they link outside the root.
fn skills(root: &Path, extra: &[String]) -> Result<(Vec<SkillDir>, Vec<String>)> {
    let canonical_root = fs::canonicalize(root).map_err(io_err(root))?;
    let mut found = Vec::new();
    let mut skipped = Vec::new();
    // `.claude/skills` entries are often symlinks into `.agents/skills`; a skill
    // reached twice is one skill.
    let mut seen = HashSet::new();
    for base in SKILL_ROOTS
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
    {
        let dir = root.join(base);
        if dir.exists() && inside(&canonical_root, &dir)?.is_none() {
            skipped.push(base.to_string());
            continue;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io_err(&dir)(e)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(io_err(&dir))?;
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        for name in names {
            let path = dir.join(&name);
            if !path.join("SKILL.md").is_file() {
                continue;
            }
            let Some(real_path) = inside(&canonical_root, &path)? else {
                skipped.push(format!("{base}/{name}"));
                continue;
            };
            if !seen.insert(real_path.clone()) {
                continue;
            }
            found.push(SkillDir {
                dir_name: name.clone(),
                path: format!("{base}/{name}"),
                real_path,
            });
        }
    }
    Ok((found, skipped))
}

fn servers(root: &Path) -> Result<Vec<Server>> {
    let canonical_root = fs::canonicalize(root).map_err(io_err(root))?;
    let mut found = Vec::new();
    for (client, rel) in [
        (Client::Claude, ".mcp.json"),
        (Client::Cursor, ".cursor/mcp.json"),
        (Client::Gemini, ".gemini/settings.json"),
        (Client::Copilot, ".vscode/mcp.json"),
        (Client::Copilot, ".github/mcp.json"),
        (Client::Opencode, "opencode.json"),
        (Client::Antigravity, ".agents/mcp_config.json"),
    ] {
        if root.join(rel).exists() {
            ensure_inside(&canonical_root, &root.join(rel))?;
        }
        if let Some(text) = read_optional(&root.join(rel))? {
            found.extend(json_servers(client, rel, &text, &root.join(rel))?);
        }
    }
    let rel = ".codex/config.toml";
    if root.join(rel).exists() {
        ensure_inside(&canonical_root, &root.join(rel))?;
    }
    if let Some(text) = read_optional(&root.join(rel))? {
        found.extend(codex_servers(rel, &text, &root.join(rel))?);
    }
    found.sort_by(|a, b| (a.client, &a.name).cmp(&(b.client, &b.name)));
    for pair in found.windows(2) {
        if pair[0].client == pair[1].client && pair[0].name == pair[1].name {
            return Err(parse_err(
                root,
                format!(
                    "duplicate MCP server {}.{} in {} and {}",
                    pair[0].client.as_str(),
                    pair[0].name,
                    pair[0].config_path,
                    pair[1].config_path
                ),
            ));
        }
    }
    Ok(found)
}

pub(crate) fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path)(e)),
    }
}

#[derive(Deserialize)]
struct RawServer {
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(alias = "httpUrl", alias = "serverUrl")]
    url: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    transport: Option<String>,
}

impl RawServer {
    fn launch(self, name: &str, path: &Path) -> Result<Launch> {
        let remote_kind = [self.kind.as_deref(), self.transport.as_deref()]
            .into_iter()
            .flatten()
            .any(|k| matches!(k, "http" | "sse" | "streamable-http" | "streamableHttp"));
        if let Some(url) = self.url {
            return Ok(Launch::Remote { url });
        }
        if remote_kind {
            return Err(parse_err(
                path,
                format!("MCP server {name:?} is remote but has no url"),
            ));
        }
        match self.command {
            Some(command) => Ok(Launch::Stdio {
                command,
                args: self.args,
                env: self.env,
            }),
            None => Err(parse_err(
                path,
                format!("MCP server {name:?} has neither command nor url"),
            )),
        }
    }
}

fn json_servers(client: Client, rel: &str, text: &str, path: &Path) -> Result<Vec<Server>> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| parse_err(path, e))?;
    let section_name = match rel {
        ".vscode/mcp.json" => "servers",
        "opencode.json" => "mcp",
        _ => "mcpServers",
    };
    let Some(map) = value.get(section_name) else {
        return Ok(Vec::new());
    };
    let mut map = map.clone();
    if client == Client::Opencode {
        let entries = map
            .as_object_mut()
            .ok_or_else(|| parse_err(path, "mcp must be an object"))?;
        for (name, entry) in entries {
            let entry = entry
                .as_object_mut()
                .ok_or_else(|| parse_err(path, "MCP entry must be an object"))?;
            match entry.get("type").and_then(serde_json::Value::as_str) {
                Some("local") => {
                    let command = entry
                        .get("command")
                        .and_then(serde_json::Value::as_array)
                        .filter(|a| !a.is_empty() && a.iter().all(serde_json::Value::is_string))
                        .ok_or_else(|| {
                            parse_err(
                                path,
                                format!("MCP server {name:?} needs a nonempty command array"),
                            )
                        })?
                        .clone();
                    entry.insert("command".into(), command[0].clone());
                    entry.insert(
                        "args".into(),
                        serde_json::Value::Array(command[1..].to_vec()),
                    );
                    if let Some(env) = entry.remove("environment") {
                        entry.insert("env".into(), env);
                    }
                }
                Some("remote")
                    if entry
                        .get("url")
                        .and_then(serde_json::Value::as_str)
                        .is_some() => {}
                _ => {
                    return Err(parse_err(
                        path,
                        format!("MCP server {name:?} needs type local or remote with url"),
                    ));
                }
            }
        }
    }
    let map: BTreeMap<String, RawServer> =
        serde_json::from_value(map).map_err(|e| parse_err(path, e))?;
    let section = text.find(&format!("\"{section_name}\"")).unwrap_or(0);
    map.into_iter()
        .map(|(name, raw)| {
            // The entry's key: the quoted name followed by a colon.
            let key = format!(
                r"{}\s*:",
                regex::escape(&serde_json::to_string(&name).expect("string serializes"))
            );
            let line = regex::Regex::new(&key)
                .expect("escaped key")
                .find(&text[section..])
                .map(|m| crate::finding::line_of(text, section + m.start()));
            Ok(Server {
                client,
                line,
                config_path: rel.to_string(),
                launch: raw.launch(&name, path)?,
                name,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct CodexConfig {
    #[serde(default)]
    mcp_servers: BTreeMap<String, RawServer>,
}

fn codex_servers(rel: &str, text: &str, path: &Path) -> Result<Vec<Server>> {
    let config: CodexConfig =
        toml::from_str(text).map_err(|e| crate::error::toml_err(path, text, &e))?;
    config
        .mcp_servers
        .into_iter()
        .map(|(name, raw)| {
            let headers = [
                format!("[mcp_servers.{name}]"),
                format!("[mcp_servers.\"{name}\"]"),
            ];
            let line = text
                .lines()
                .position(|l| headers.iter().any(|h| l.trim() == h))
                .map(|i| i + 1);
            Ok(Server {
                client: Client::Codex,
                line,
                config_path: rel.to_string(),
                launch: raw.launch(&name, path)?,
                name,
            })
        })
        .collect()
}

/// Reads a file that discovery already found, as text.
pub(crate) fn read_text(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(io_err(path))
}
