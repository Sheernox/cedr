//! Check registration without running connection-oriented client commands.
use cedr_route::machine::installed;
use serde_json::Value;
use std::{
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
    time::Duration,
};

#[derive(Clone, Copy)]
pub enum McpAgent {
    Claude,
    Codex,
    Grok,
    Gemini,
    Cursor,
    Copilot,
    Opencode,
    Muse,
    Antigravity,
}

pub const MCP_AGENTS: &[McpAgent] = &[
    McpAgent::Claude,
    McpAgent::Codex,
    McpAgent::Grok,
    McpAgent::Gemini,
    McpAgent::Cursor,
    McpAgent::Copilot,
    McpAgent::Opencode,
    McpAgent::Muse,
    McpAgent::Antigravity,
];

pub enum McpRegistration {
    Absent,
    Registered,
    Missing,
    Unreadable(String),
}

pub enum McpRepair {
    Command(&'static [&'static str]),
    Manual(String),
}

impl McpRepair {
    pub fn instructions(self) -> String {
        match self {
            Self::Command(args) => args.join(" "),
            Self::Manual(instructions) => instructions,
        }
    }
}

const STDIO: &str = r#"{"mcpServers":{"cedr":{"command":"cedr","args":["mcp"]}}}"#;
const COPILOT: &str =
    r#"{"mcpServers":{"cedr":{"type":"stdio","command":"cedr","args":["mcp"],"tools":["*"]}}}"#;

impl McpAgent {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::Gemini => "gemini",
            Self::Cursor => "cursor",
            Self::Copilot => "copilot",
            Self::Opencode => "opencode",
            Self::Muse => "muse",
            Self::Antigravity => "antigravity",
        }
    }

    /// The same repair drives Doctor's instructions and Init's plan.
    pub fn repair(self) -> McpRepair {
        match self {
            Self::Claude => {
                McpRepair::Command(&["claude", "mcp", "add", "cedr", "--", "cedr", "mcp"])
            }
            Self::Codex => {
                McpRepair::Command(&["codex", "mcp", "add", "cedr", "--", "cedr", "mcp"])
            }
            Self::Grok => McpRepair::Command(&["grok", "mcp", "add", "cedr", "--", "cedr", "mcp"]),
            Self::Gemini => McpRepair::Command(&[
                "gemini", "mcp", "add", "--scope", "user", "cedr", "cedr", "mcp",
            ]),
            Self::Opencode => opencode_repair(),
            Self::Muse => {
                McpRepair::Command(&["muse", "mcp", "add", "cedr", "--user", "--", "cedr", "mcp"])
            }
            Self::Cursor => McpRepair::Manual(format!("Merge {STDIO} into ~/.cursor/mcp.json")),
            Self::Copilot => McpRepair::Manual(format!(
                "Merge {COPILOT} into $COPILOT_HOME/mcp-config.json (default ~/.copilot/mcp-config.json)"
            )),
            Self::Antigravity => McpRepair::Manual(format!(
                "Merge {STDIO} into ~/.gemini/config/mcp_config.json"
            )),
        }
    }

    pub fn registration(self, root: &Path) -> McpRegistration {
        let home = env::home_dir();
        let home_path = |suffix: &str| home.as_ref().map(|p| p.join(suffix));
        let from_env = |name: &str| {
            env::var_os(name)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        };
        let desktop_home = match self {
            Self::Cursor => home_path(".cursor"),
            Self::Copilot => from_env("COPILOT_HOME").or_else(|| home_path(".copilot")),
            Self::Antigravity => home_path(".gemini/antigravity"),
            _ => None,
        };
        let present = match self {
            Self::Cursor => ["cursor", "cursor-agent"].iter().any(|p| installed(p)),
            Self::Copilot => ["code", "code-insiders", "copilot"]
                .iter()
                .any(|p| installed(p)),
            Self::Antigravity => ["agy", "antigravity"].iter().any(|p| installed(p)),
            _ => installed(self.name()),
        };
        let has_desktop_config = desktop_home.as_ref().is_some_and(|p| p.is_dir())
            || (matches!(self, Self::Antigravity)
                && home_path(".gemini/config/mcp_config.json").is_some_and(|p| p.is_file()));
        if !present && !has_desktop_config {
            return McpRegistration::Absent;
        }

        let mut files = Vec::new();
        let mut file = |path: Option<PathBuf>, key| {
            if let Some(path) = path {
                files.push((path, key));
            }
        };
        match self {
            Self::Claude | Self::Codex | Self::Grok | Self::Muse => return self.list(root),
            Self::Gemini => {
                file(Some(root.join(".gemini/settings.json")), "mcpServers");
                let settings = from_env("GEMINI_CLI_HOME")
                    .map(|p| p.join(".gemini/settings.json"))
                    .or_else(|| home_path(".gemini/settings.json"));
                file(settings, "mcpServers");
            }
            Self::Cursor => {
                file(Some(root.join(".cursor/mcp.json")), "mcpServers");
                file(home_path(".cursor/mcp.json"), "mcpServers");
            }
            Self::Copilot => {
                file(Some(root.join(".mcp.json")), "mcpServers");
                file(Some(root.join(".vscode/mcp.json")), "servers");
                let editor_config = match env::consts::OS {
                    "windows" => from_env("APPDATA"),
                    "macos" => home_path("Library/Application Support"),
                    _ => from_env("XDG_CONFIG_HOME").or_else(|| home_path(".config")),
                };
                if let Some(dir) = editor_config {
                    for editor in ["Code", "Code - Insiders"] {
                        file(Some(dir.join(editor).join("User/mcp.json")), "servers");
                    }
                }
                file(
                    desktop_home.map(|p| p.join("mcp-config.json")),
                    "mcpServers",
                );
            }
            Self::Opencode => {
                let config = from_env("XDG_CONFIG_HOME").or_else(|| home_path(".config"));
                for name in ["opencode.json", "opencode.jsonc"] {
                    file(Some(root.join(name)), "mcp");
                    file(Some(root.join(".opencode").join(name)), "mcp");
                    file(
                        config.as_ref().map(|p| p.join("opencode").join(name)),
                        "mcp",
                    );
                }
                file(from_env("OPENCODE_CONFIG").map(|p| root.join(p)), "mcp");
            }
            Self::Antigravity => {
                file(Some(root.join(".agents/mcp_config.json")), "mcpServers");
                file(home_path(".gemini/config/mcp_config.json"), "mcpServers");
            }
        }

        // Any unreadable layer could hide an existing entry. Init must leave it alone.
        let mut found = false;
        for (path, key) in files {
            match read_registration(&path, key) {
                Ok(registered) => found |= registered,
                Err(message) => return McpRegistration::Unreadable(message),
            }
        }
        registration(found)
    }

    fn list(self, root: &Path) -> McpRegistration {
        let args: &[&str] = if matches!(self, Self::Muse) {
            &["mcp", "list", "--json"]
        } else {
            &["mcp", "list"]
        };
        let unreadable =
            || McpRegistration::Unreadable(format!("{} {} failed", self.name(), args.join(" ")));
        let cwd = matches!(self, Self::Muse).then_some(root);
        let Ok(output) = run_mcp(self.name(), args, cwd) else {
            return unreadable();
        };
        if !output.succeeded() {
            return unreadable();
        }
        if matches!(self, Self::Muse) {
            let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) else {
                return unreadable();
            };
            let Some(servers) = value["servers"].as_array() else {
                return unreadable();
            };
            return registration(servers.iter().any(|server| server["name"] == "cedr"));
        }
        let Ok(text) = std::str::from_utf8(&output.stdout) else {
            return unreadable();
        };
        registration(text.lines().any(|line| {
            line.split_whitespace()
                .next()
                .is_some_and(|word| word.trim_end_matches(':') == "cedr")
        }))
    }
}

// OpenCode v1 only offers interactive setup. Gate the unattended command on v2.
fn opencode_repair() -> McpRepair {
    let version = run_mcp("opencode", &["--version"], None)
        .ok()
        .filter(|out| out.succeeded())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| semver::Version::parse(text.trim().trim_start_matches('v')).ok());
    let command = &[
        "opencode", "mcp", "add", "cedr", "--global", "--", "cedr", "mcp",
    ];
    let v1 =
        r#"Merge {"mcp":{"cedr":{"type":"local","command":["cedr","mcp"]}}} into opencode.json"#;
    match version {
        Some(version) if version.major >= 2 => McpRepair::Command(command),
        Some(_) => McpRepair::Manual(v1.into()),
        None => McpRepair::Manual(format!(
            "OpenCode v2: run {}. OpenCode v1: {v1}",
            command.join(" ")
        )),
    }
}

pub fn run_mcp(
    agent: &str,
    args: &[&str],
    root: Option<&Path>,
) -> io::Result<cedr_compare::exec::StepOutput> {
    let mut cmd = Command::new(agent);
    if let Some(root) = root {
        cmd.current_dir(root);
    }
    cmd.args(args)
        .env("GROK_DISABLE_AUTOUPDATER", "1")
        .env("MUSE_NO_AUTO_UPDATE", "1");
    cedr_compare::exec::run_cleanup(cmd, Duration::from_secs(10))
}

fn registration(found: bool) -> McpRegistration {
    if found {
        McpRegistration::Registered
    } else {
        McpRegistration::Missing
    }
}

fn read_registration(path: &Path, key: &str) -> Result<bool, String> {
    let unreadable = || format!("cannot read MCP registration in {path:?}");
    const MAX_BYTES: u64 = 4 * 1024 * 1024;
    // Project files can be untrusted. Refuse special files and bound memory for parsing.
    match fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() <= MAX_BYTES => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        _ => return Err(unreadable()),
    }
    let mut text = String::new();
    fs::File::open(path)
        .and_then(|f| f.take(MAX_BYTES + 1).read_to_string(&mut text))
        .map_err(|_| unreadable())?;
    if text.len() as u64 > MAX_BYTES {
        return Err(unreadable());
    }
    let value = serde_json::from_str::<Value>(&text)
        .or_else(|_| serde_json::from_str(&strip_jsonc(&text)))
        .map_err(|_| unreadable())?;
    let object = value.as_object().ok_or_else(unreadable)?;
    let Some(mut section) = object.get(key) else {
        return Ok(false);
    };
    // OpenCode v1 can itself have a server named "servers".
    if key == "mcp"
        && let Some(servers) = section.get("servers")
        && servers.get("type").and_then(Value::as_str).is_none()
    {
        section = servers;
    }
    let section = section.as_object().ok_or_else(unreadable)?;
    match section.get("cedr") {
        Some(entry) if entry.is_object() => Ok(true),
        Some(_) => Err(unreadable()),
        None => Ok(false),
    }
}

// Preserve strings (including escaped quotes) while removing JSONC comments and trailing commas.
fn strip_jsonc(text: &str) -> String {
    const STRING: &str = r#""(?:[^"\\]|\\.)*""#;
    static COMMENTS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(&format!(r"({STRING})|//[^\r\n]*|/\*(?s:.*?)\*/")).unwrap()
    });
    static COMMAS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(&format!(r"({STRING})|,([[:space:]]*[\]}}])")).unwrap());
    let without_comments = COMMENTS.replace_all(text, "${1} ");
    COMMAS
        .replace_all(&without_comments, "${1}${2}")
        .into_owned()
}
