//! `.cedr/guard.lock`: every skill and MCP server with its pin, plus the surface
//! snapshots in `.cedr/surfaces/` next to it. A lock at the legacy root path
//! `cedr.lock` is still read, and moved by the next `cedr lock`.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::discovery::{self, Client, Launch, Server, Transport};
use crate::error::{io_err, parse_err};
use crate::npm::Registry;
use crate::skill::{self, ParsedFrontmatter};
use crate::source::{Source, is_dist_tag};
use crate::surface::{self, Method, Surface};
use crate::{Result, introspect};

pub const LOCK_FILE: &str = ".cedr/guard.lock";
/// Where 0.2.0 and earlier wrote the lock.
pub const LEGACY_LOCK_FILE: &str = "cedr.lock";
pub const SURFACE_DIR: &str = ".cedr/surfaces";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Lock {
    pub version: u32,
    #[serde(default)]
    pub skill: Vec<SkillEntry>,
    #[serde(default)]
    pub mcp: Vec<McpEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct SkillEntry {
    pub name: String,
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct McpEntry {
    pub client: Client,
    pub name: String,
    pub transport: Transport,
    pub source: String,
    pub pinned: bool,
    pub launch: String,
    pub integrity: Option<String>,
    pub surface: Option<String>,
    pub method: Method,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub egress: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LockOptions {
    pub offline: bool,
    pub introspect: bool,
}

#[derive(Debug)]
pub struct LockOutcome {
    pub lock: Lock,
    /// Servers recorded without a surface, and why.
    pub warnings: Vec<String>,
    /// Skill directories skipped because they link outside the repo.
    pub skipped: Vec<String>,
    /// A legacy `cedr.lock` was removed in favour of `.cedr/guard.lock`.
    pub moved: bool,
}

impl Lock {
    pub fn read(path: &Path) -> Result<Lock> {
        let text = fs::read_to_string(path).map_err(io_err(path))?;
        let lock: Lock =
            toml::from_str(&text).map_err(|e| crate::error::toml_err(path, &text, &e))?;
        if lock.version != 1 {
            return Err(parse_err(
                path,
                format!("unsupported lock version {}", lock.version),
            ));
        }
        Ok(lock)
    }

    /// Deterministic TOML: skills by name then path, servers by client then
    /// name, fixed key order, trailing newline.
    pub fn to_toml(&self) -> String {
        let mut skills = self.skill.clone();
        skills.sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
        let mut servers = self.mcp.clone();
        servers.sort_by(|a, b| (a.client, &a.name).cmp(&(b.client, &b.name)));

        let mut out = format!("version = {}\n", self.version);
        for s in &skills {
            out.push_str("\n[[skill]]\n");
            kv(&mut out, "name", &quote(&s.name));
            kv(&mut out, "path", &quote(&s.path));
            kv(&mut out, "content", &quote(&s.content));
            kv(&mut out, "allowed_tools", &array(&s.allowed_tools));
        }
        for m in &servers {
            out.push_str("\n[[mcp]]\n");
            kv(&mut out, "client", &quote(m.client.as_str()));
            kv(&mut out, "name", &quote(&m.name));
            kv(&mut out, "transport", &quote(m.transport.as_str()));
            kv(&mut out, "source", &quote(&m.source));
            kv(&mut out, "pinned", if m.pinned { "true" } else { "false" });
            kv(&mut out, "launch", &quote(&m.launch));
            if let Some(integrity) = &m.integrity {
                kv(&mut out, "integrity", &quote(integrity));
            }
            if let Some(surface) = &m.surface {
                kv(&mut out, "surface", &quote(surface));
            }
            kv(&mut out, "method", &quote(m.method.as_str()));
            kv(&mut out, "tools", &array(&m.tools));
            kv(&mut out, "egress", &array(&m.egress));
        }
        out
    }
}

fn kv(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(value);
    out.push('\n');
}

/// A TOML basic string.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn array(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|s| quote(s)).collect();
    format!("[{}]", quoted.join(", "))
}

/// `.cedr/surfaces/<client>.<name>.json`, with characters outside
/// `[A-Za-z0-9._-]` in the server name percent-encoded so a name cannot leave the directory.
pub fn surface_file_name(client: Client, name: &str) -> String {
    let safe: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("{}.{safe}.json", client.as_str())
}

/// Builds the lock for `root`, writes `.cedr/guard.lock` and the surface files,
/// removes surface files no entry refers to any more, and removes a legacy `cedr.lock`.
pub fn lock(root: &Path, options: LockOptions) -> Result<LockOutcome> {
    let ws = discovery::discover(root)?;
    let mut skills = Vec::new();
    for dir in &ws.skills {
        let text = String::from_utf8_lossy(&skill::file_bytes(&dir.real_path.join("SKILL.md"))?)
            .into_owned();
        let allowed_tools = match skill::frontmatter(&text) {
            ParsedFrontmatter::Valid(fm) => fm.allowed_tools,
            ParsedFrontmatter::Absent | ParsedFrontmatter::Invalid(_) => Vec::new(),
        };
        skills.push(SkillEntry {
            name: dir.dir_name.clone(),
            path: dir.path.clone(),
            content: skill::content_hash(&dir.real_path)?,
            allowed_tools,
        });
    }

    let registry = if options.offline {
        None
    } else {
        Some(Registry::new()?)
    };
    let mut warnings = Vec::new();
    let mut servers = Vec::new();
    let mut filenames = std::collections::HashSet::new();
    for server in &ws.servers {
        let name = surface_file_name(server.client, &server.name);
        if !filenames.insert(name.clone()) {
            return Err(parse_err(
                root,
                format!("surface filename collision: {name}"),
            ));
        }
    }
    let mut surfaces = Vec::new();
    for server in &ws.servers {
        let (entry, surface) =
            server_entry(root, server, registry.as_ref(), options, &mut warnings);
        if let Some(surface) = surface {
            surfaces.push((surface_file_name(server.client, &server.name), surface));
        }
        servers.push(entry);
    }

    let lock = Lock {
        version: 1,
        skill: skills,
        mcp: servers,
    };
    write_surfaces(root, &surfaces)?;
    let path = root.join(LOCK_FILE);
    guard_dir(root, false)?;
    write_atomic(&path, lock.to_toml().as_bytes())?;
    let legacy = root.join(LEGACY_LOCK_FILE);
    let moved = legacy.is_file();
    if moved {
        fs::remove_file(&legacy).map_err(io_err(&legacy))?;
    }
    Ok(LockOutcome {
        lock,
        warnings,
        skipped: ws.skipped,
        moved,
    })
}

fn server_entry(
    root: &Path,
    server: &Server,
    registry: Option<&Registry>,
    options: LockOptions,
    warnings: &mut Vec<String>,
) -> (McpEntry, Option<Surface>) {
    let label = format!("{}.{}", server.client.as_str(), server.name);
    let configured = server.source();
    let mut entry = McpEntry {
        client: server.client,
        name: server.name.clone(),
        transport: server.transport(),
        source: configured.to_string(),
        pinned: configured.pinned(),
        launch: server.launch_hash(),
        integrity: None,
        surface: None,
        method: Method::None,
        tools: Vec::new(),
        egress: Vec::new(),
    };

    let mut static_surface = None;
    if let (Some(registry), Source::Npm { name, version }) = (registry, &configured) {
        match npm_surface(registry, name, version.as_deref(), configured.pinned()) {
            Ok(surface) => {
                entry.source = surface.source.clone();
                entry.integrity = surface.integrity.clone();
                static_surface = Some(surface);
            }
            Err(e) => warnings.push(format!("{label}: {e}; recorded without a surface")),
        }
    }

    let mut surface = None;
    if options.introspect
        && let Launch::Stdio { command, args, env } = &server.launch
    {
        match introspect::tools(command, args, env, root) {
            // Introspection sees the tools; egress and capabilities still come
            // from the package source when it was fetched.
            Ok(tools) => {
                let (egress, capabilities) = static_surface
                    .as_ref()
                    .map(|s| (s.egress.clone(), s.capabilities.clone()))
                    .unwrap_or_default();
                surface = Some(Surface::new(
                    entry.source.clone(),
                    Method::Introspect,
                    entry.integrity.clone(),
                    tools,
                    egress,
                    capabilities,
                ));
            }
            Err(e) => warnings.push(format!("{label}: introspection failed: {e}")),
        }
    }
    let surface = surface.or(static_surface);
    if let Some(s) = &surface {
        entry.method = s.method;
        entry.surface = Some(s.hash.clone());
        entry.tools = s.tools.iter().map(|t| t.name.clone()).collect();
        entry.egress = s.egress.clone();
    }
    (entry, surface)
}

/// Unpinned sources preserve the configured dist-tag; ranges retain the
/// default `latest` resolution.
fn npm_surface(
    registry: &Registry,
    name: &str,
    version: Option<&str>,
    pinned: bool,
) -> Result<Surface> {
    let wanted = npm_selector(version, pinned);
    let doc = registry.version_doc(name, wanted)?;
    let (bytes, integrity) = registry.tarball(&doc)?;
    surface::extract_tarball(format!("npm:{name}@{}", doc.version), &bytes, integrity)
}

fn npm_selector(version: Option<&str>, pinned: bool) -> &str {
    version
        .filter(|v| pinned || is_dist_tag(v))
        .unwrap_or("latest")
}

fn write_surfaces(root: &Path, surfaces: &[(String, Surface)]) -> Result<()> {
    let mut names = std::collections::HashSet::new();
    for (file, _) in surfaces {
        if !names.insert(file) {
            return Err(parse_err(
                root,
                format!("surface filename collision: {file}"),
            ));
        }
    }
    let dir = root.join(SURFACE_DIR);
    if surfaces.is_empty() && !dir.exists() {
        return Ok(());
    }
    guard_dir(root, true)?;
    for (file, surface) in surfaces {
        write_atomic(&dir.join(file), surface.to_json_pretty().as_bytes())?;
    }
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err(&dir)(e)),
    };
    for entry in entries {
        let entry = entry.map_err(io_err(&dir))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let current = surfaces.iter().any(|(file, _)| *file == name);
        if name.ends_with(".json") && !current {
            let path = entry.path();
            fs::remove_file(&path).map_err(io_err(&path))?;
        }
    }
    Ok(())
}

/// Creates `.cedr/` (and `.cedr/surfaces/` with `surfaces`) under `root` and checks each stays
/// inside the root, so a linked `.cedr` never sends a write or a removal elsewhere.
fn guard_dir(root: &Path, surfaces: bool) -> Result<()> {
    let canonical_root = fs::canonicalize(root).map_err(io_err(root))?;
    let dirs: &[&str] = if surfaces {
        &[".cedr", SURFACE_DIR]
    } else {
        &[".cedr"]
    };
    for rel in dirs {
        let dir = root.join(rel);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_err(&dir)(e)),
        }
        discovery::ensure_inside(&canonical_root, &dir)?;
    }
    Ok(())
}

/// Writes `path` through a temporary file in the same directory and a rename: a failed write
/// never leaves a partial file, and a symlink at `path` is replaced, never written through.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    let _ = fs::remove_file(&tmp);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| {
            std::io::Write::write_all(&mut f, bytes)?;
            f.sync_all()
        })
        .and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(path)(e));
    }
    Ok(())
}

/// Reads the lock under `root` (`.cedr/guard.lock`, else the legacy `cedr.lock`) with its
/// path relative to the root, explaining how to create one when neither exists.
pub fn read_lock(root: &Path) -> Result<(Lock, &'static str)> {
    for rel in [LOCK_FILE, LEGACY_LOCK_FILE] {
        let path = root.join(rel);
        if path.exists() {
            return Ok((Lock::read(&path)?, rel));
        }
    }
    Err(parse_err(
        &root.join(LOCK_FILE),
        "no lock: run cedr lock first",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_resolution_preserves_dist_tags() {
        for tag in ["next", "beta", "canary-1", "latest"] {
            assert_eq!(npm_selector(Some(tag), false), tag);
        }
        assert_eq!(npm_selector(Some("2.0.0"), true), "2.0.0");
        for version in [None, Some("^1.0.0"), Some("1.x"), Some("v1")] {
            assert_eq!(npm_selector(version, false), "latest");
        }
    }

    #[test]
    fn toml_is_sorted_escaped_and_round_trips() {
        let entry = |client, name: &str| McpEntry {
            client,
            name: name.into(),
            transport: Transport::Stdio,
            source: "npm:x@1.0.0".into(),
            pinned: true,
            launch: "sha256-test".into(),
            integrity: None,
            surface: None,
            method: Method::None,
            tools: vec![],
            egress: vec![],
        };
        let lock = Lock {
            version: 1,
            skill: vec![SkillEntry {
                name: "q\"uote".into(),
                path: ".agents/skills/q".into(),
                content: "sha256-0".into(),
                allowed_tools: vec!["Bash(npm run *)".into()],
            }],
            mcp: vec![
                entry(Client::Gemini, "b"),
                entry(Client::Claude, "z"),
                entry(Client::Claude, "a"),
            ],
        };
        let text = lock.to_toml();
        assert!(text.ends_with("egress = []\n"));
        assert!(text.contains("name = \"q\\\"uote\""));
        let order: Vec<_> = text.lines().filter(|l| l.starts_with("name = ")).collect();
        assert_eq!(
            order,
            [
                "name = \"q\\\"uote\"",
                "name = \"a\"",
                "name = \"z\"",
                "name = \"b\""
            ]
        );
        let parsed: Lock = toml::from_str(&text).unwrap();
        assert_eq!(parsed.to_toml(), text);
    }

    #[test]
    fn surface_names_encode_utf8_and_do_not_collide() {
        let names = ["a/b", "a_b", "a%b", "a%2Fb", "a:b", "é"];
        let files: std::collections::HashSet<_> = names
            .iter()
            .map(|n| surface_file_name(Client::Claude, n))
            .collect();
        assert_eq!(files.len(), names.len());
        assert!(files.contains("claude.%C3%A9.json"));
        assert!(files.contains("claude.a%2Fb.json"));
        let root = tempfile::tempdir().unwrap();
        let surface = Surface::new(
            "command:x".into(),
            Method::Static,
            None,
            vec![],
            vec![],
            vec![],
        );
        assert!(
            write_surfaces(
                root.path(),
                &[
                    ("same.json".into(), surface.clone()),
                    ("same.json".into(), surface)
                ]
            )
            .is_err()
        );
    }

    #[test]
    fn surface_file_names_stay_in_the_directory() {
        assert_eq!(
            surface_file_name(Client::Claude, "github"),
            "claude.github.json"
        );
        assert_eq!(
            surface_file_name(Client::Codex, "../x/y"),
            "codex...%2Fx%2Fy.json"
        );
    }
}
