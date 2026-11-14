//! Shared configuration isolation for command-line agents.
use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Muse,
    Grok,
    Gemini,
    Antigravity,
    Command,
}

/// Gives vendor CLIs the same filtered view of the user's tools, with private agent configuration.
/// The caller owns the temporary directory through process exit; this is not a security sandbox.
pub fn clean_home(kind: Agent, cmd: &mut Command, home: &Path) -> std::io::Result<()> {
    home_inner(kind, cmd, home, true)
}

/// A host authentication check sees only its sign-in, never the user's tools or setup.
pub fn sign_in_home(kind: Agent, cmd: &mut Command, home: &Path) -> std::io::Result<()> {
    home_inner(kind, cmd, home, false)
}
fn home_inner(kind: Agent, cmd: &mut Command, home: &Path, shadow: bool) -> std::io::Result<()> {
    if kind == Agent::Command {
        return Ok(());
    }
    let (variable, directory, credential) = match kind {
        Agent::Codex => ("CODEX_HOME", ".codex", "auth.json"),
        Agent::Claude => ("CLAUDE_CONFIG_DIR", ".claude", ".credentials.json"),
        Agent::Muse => ("MUSE_HOME", ".config/muse", "auth.json"),
        Agent::Grok => ("GROK_HOME", ".grok", "auth.json"),
        Agent::Gemini => ("GEMINI_HOME", ".gemini", "oauth_creds.json"),
        Agent::Antigravity => ("GEMINI_HOME", ".gemini", "antigravity-acp/acp_token.json"),
        Agent::Command => unreachable!(),
    };
    let user_home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let user_home = user_home.map(std::path::PathBuf::from);
    let source = std::env::var_os(variable)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| user_home.as_ref().map(|p| p.join(directory)));
    #[cfg(unix)]
    if shadow {
        let config = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty());
        let mut data = Vec::new();
        if let Some(user) = &user_home {
            for (key, default) in [
                ("XDG_DATA_HOME", ".local/share"),
                ("XDG_STATE_HOME", ".local/state"),
                ("XDG_CACHE_HOME", ".cache"),
            ] {
                let source = std::env::var_os(key)
                    .filter(|v| !v.is_empty())
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| user.join(default));
                if let Ok(relative) = source.strip_prefix(user)
                    && contains_agent_setup(user, relative)?
                {
                    data.push(relative.to_path_buf());
                    cmd.env(key, home.join(relative));
                }
            }
        }
        let config = shadow_home(
            user_home.as_deref(),
            home,
            config.as_deref().map(Path::new),
            &data,
        )?;
        cmd.env("HOME", home)
            .env("USERPROFILE", home)
            .env("XDG_CONFIG_HOME", config);
    }
    if !shadow {
        cmd.env("HOME", home)
            .env("USERPROFILE", home)
            .env("XDG_CONFIG_HOME", home.join(".config"));
    }
    let agent_home = home.join(directory);
    std::fs::create_dir_all(&agent_home)?;
    if let Some(source) = source {
        let sign_in = source.join(credential);
        match std::fs::metadata(&sign_in) {
            Ok(_) => {
                // An absolute target still works when the trial changes its working directory.
                let target = std::fs::canonicalize(sign_in)?;
                let link = agent_home.join(credential);
                if let Some(parent) = link.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                link_sign_in(&target, &link)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    // Parent SDK sessions and user overrides can inject instructions, plugins or MCP servers.
    // Keep provider credentials, including Claude's OAuth environment credential, available.
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if ((name.starts_with("CLAUDE_") || name.starts_with("CODEX_"))
            && name != "CLAUDE_CODE_OAUTH_TOKEN")
            || matches!(name.as_str(), "CLAUDECODE" | "BASH_ENV" | "ENV" | "ZDOTDIR")
        {
            cmd.env_remove(key);
        }
    }
    cmd.env(variable, agent_home);
    Ok(())
}

/// Report a vendor replacement without reading or saving its sign-in contents.
pub fn renewal_notice(home: &Path) -> std::io::Result<Option<&'static str>> {
    for relative in [".codex/auth.json", ".claude/.credentials.json"] {
        match std::fs::symlink_metadata(home.join(relative)) {
            Ok(meta) if meta.is_file() => {
                return Ok(Some(
                    "sign-in renewal was not saved; you may need to sign in to that agent again",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

/// Non-interactive checks cannot execute tools or load user agent setup.
pub fn sign_in_args(kind: Agent, model: &str) -> std::io::Result<Vec<String>> {
    let args: Vec<&str> = match kind {
        Agent::Claude => vec![
            "claude",
            "-p",
            "--safe-mode",
            "--strict-mcp-config",
            "--tools",
            "",
            "--setting-sources",
            "",
            "--no-session-persistence",
            "--model",
            model,
            "Reply OK.",
        ],
        Agent::Codex => vec!["codex", "login", "status"],
        Agent::Muse => vec!["muse", "usage", "--json"],
        Agent::Grok => vec!["grok", "models"],
        Agent::Gemini => {
            return Err(std::io::Error::other(
                "Gemini has no verified tool-free host sign-in check; use GEMINI_API_KEY or GOOGLE_API_KEY",
            ));
        }
        _ => {
            return Err(std::io::Error::other(
                "no host sign-in check for this agent",
            ));
        }
    };
    Ok(args.into_iter().map(str::to_string).collect())
}

/// Keep runtime writes outside a shadow home: ordinary entries there may link to the real home.
/// `root` must be a fresh, caller-owned directory outside that filtered view.
pub fn private_runtime(cmd: &mut Command, root: &Path) -> std::io::Result<()> {
    for (key, name) in [
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
    ] {
        let path = root.join(name);
        std::fs::create_dir_all(&path)?;
        cmd.env(key, path);
    }
    Ok(())
}

// Keep this list in step with docs/compare.md. Dotless spellings also cover XDG config children.
#[cfg(unix)]
const HIDDEN_AGENT_ROOTS: &[&str] = &[
    ".agents",
    ".codex",
    ".claude",
    ".claude.json",
    ".gemini",
    ".cursor",
    ".grok",
    ".muse",
    ".opencode",
    ".copilot",
    "Antigravity",
    "AGENTS.md",
    "AGENTS.override.md",
    "CLAUDE.md",
    "CLAUDE.local.md",
    "GEMINI.md",
];

#[cfg(unix)]
fn hidden_agent_root(name: &std::ffi::OsStr) -> bool {
    HIDDEN_AGENT_ROOTS.iter().any(|root| {
        name == std::ffi::OsStr::new(root)
            || name == std::ffi::OsStr::new(root.trim_start_matches('.'))
    })
}

/// Only descend into real directories on the filtered paths. Ordinary entries, including symlinks,
/// are linked without inspecting their targets, so building a view never walks out through a link.
#[cfg(unix)]
fn link_home_entries(
    source: &Path,
    target: &Path,
    filtered: &[std::path::PathBuf],
) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    match std::fs::symlink_metadata(source) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if hidden_agent_root(&name) {
            continue;
        }
        let branches: Vec<_> = filtered
            .iter()
            .filter_map(|p| p.strip_prefix(&name).ok().map(Path::to_path_buf))
            .collect();
        if branches.is_empty() {
            std::os::unix::fs::symlink(entry.path(), target.join(name))?;
        } else {
            link_home_entries(&entry.path(), &target.join(name), &branches)?;
        }
    }
    Ok(())
}

/// Inspect only in-home real directories when deciding whether XDG data needs a filtered view.
#[cfg(unix)]
fn contains_agent_setup(user: &Path, relative: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(user) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    }
    let mut source = user.to_path_buf();
    for part in relative.components() {
        let std::path::Component::Normal(name) = part else {
            return Ok(false);
        };
        source.push(name);
        match std::fs::symlink_metadata(&source) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        }
    }
    for entry in std::fs::read_dir(source)? {
        if hidden_agent_root(&entry?.file_name()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Config paths outside HOME (or with ambiguous components) get the filtered default instead.
/// XDG data directories need an overlay only when known agent setup is present.
#[cfg(unix)]
fn shadow_home(
    user: Option<&Path>,
    home: &Path,
    config: Option<&Path>,
    data: &[std::path::PathBuf],
) -> std::io::Result<std::path::PathBuf> {
    let config = user
        .and_then(|user| config?.strip_prefix(user).ok())
        .filter(|p| {
            !p.as_os_str().is_empty()
                && p.components().all(|c| match c {
                    std::path::Component::Normal(name) => !hidden_agent_root(name),
                    _ => false,
                })
        })
        .unwrap_or_else(|| Path::new(".config"));
    let mut filtered = vec![std::path::PathBuf::from(".config"), config.to_path_buf()];
    filtered.extend_from_slice(data);
    if let Some(user) = user {
        link_home_entries(user, home, &filtered)?;
    }
    std::fs::create_dir_all(home.join(".config"))?;
    std::fs::create_dir_all(home.join(config))?;
    Ok(home.join(config))
}

#[cfg(unix)]
fn link_sign_in(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(windows)]
fn link_sign_in(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(source, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn shadow_home_keeps_tools_and_filters_all_agent_roots() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user");
        let home = dir.path().join("shadow");
        std::fs::create_dir_all(user.join(".config")).unwrap();
        for name in HIDDEN_AGENT_ROOTS {
            for spelling in [*name, name.trim_start_matches('.')] {
                std::fs::write(user.join(spelling), "user rules").unwrap();
                std::fs::write(user.join(".config").join(spelling), "user rules").unwrap();
            }
        }
        for path in [".rustup", ".cargo", ".local", ".cache", ".config/git"] {
            std::fs::create_dir_all(user.join(path)).unwrap();
        }
        std::fs::write(user.join(".gitconfig"), "user tools").unwrap();
        shadow_home(Some(&user), &home, None, &[]).unwrap();
        for path in [
            ".rustup",
            ".cargo",
            ".local",
            ".cache",
            ".gitconfig",
            ".config/git",
        ] {
            assert_eq!(
                std::fs::read_link(home.join(path)).unwrap(),
                user.join(path)
            );
        }
        for name in HIDDEN_AGENT_ROOTS {
            for spelling in [*name, name.trim_start_matches('.')] {
                assert!(!home.join(spelling).exists(), "{spelling}");
                assert!(
                    !home.join(".config").join(spelling).exists(),
                    "config {spelling}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn xdg_data_only_needs_filtering_when_agent_setup_is_present() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user");
        let data = user.join(".local/share");
        std::fs::create_dir_all(data.join("tools")).unwrap();
        assert!(!contains_agent_setup(&user, Path::new(".local/share")).unwrap());
        std::fs::create_dir_all(data.join("muse/skills")).unwrap();
        assert!(contains_agent_setup(&user, Path::new(".local/share")).unwrap());
        let home = dir.path().join("shadow");
        shadow_home(Some(&user), &home, None, &[".local/share".into()]).unwrap();
        assert!(!home.join(".local/share/muse").exists());
        assert_eq!(
            std::fs::read_link(home.join(".local/share/tools")).unwrap(),
            data.join("tools")
        );
        std::os::unix::fs::symlink(&data, user.join("linked-data")).unwrap();
        assert!(!contains_agent_setup(&user, Path::new("linked-data")).unwrap());
        assert!(!contains_agent_setup(&user, Path::new("../outside")).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn shadow_home_filters_custom_xdg_config_without_following_links() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(user.join("nested/config")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("must-not-link"), "outside").unwrap();
        symlink(&outside, user.join(".config")).unwrap();
        symlink(&outside, user.join("ordinary-link")).unwrap();
        std::fs::write(user.join("nested/config/muse"), "user config").unwrap();
        std::fs::write(user.join("nested/config/git"), "tool config").unwrap();
        let home = dir.path().join("shadow");
        let config =
            shadow_home(Some(&user), &home, Some(&user.join("nested/config")), &[]).unwrap();
        assert_eq!(config, home.join("nested/config"));
        assert!(!config.join("muse").exists());
        assert_eq!(
            std::fs::read_link(config.join("git")).unwrap(),
            user.join("nested/config/git")
        );
        assert_eq!(std::fs::read_dir(home.join(".config")).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_link(home.join("ordinary-link")).unwrap(),
            user.join("ordinary-link")
        );
        // A linked ancestor must not be traversed either, even for an explicit XDG path.
        let home = dir.path().join("linked-xdg");
        shadow_home(
            Some(&user),
            &home,
            Some(&user.join("ordinary-link/config")),
            &[],
        )
        .unwrap();
        assert_eq!(
            std::fs::read_dir(home.join("ordinary-link/config"))
                .unwrap()
                .count(),
            0
        );
        assert!(!outside.join("config").exists());
        // An external XDG path cannot cause the view builder to inspect outside HOME.
        let home = dir.path().join("external-xdg");
        assert_eq!(
            shadow_home(Some(&user), &home, Some(&outside), &[]).unwrap(),
            home.join(".config")
        );
        assert!(!home.join(".config/must-not-link").exists());
    }
    #[test]
    fn host_checks_never_enable_tools_or_bypass_permissions() {
        assert!(sign_in_args(Agent::Gemini, "test-model").is_err());
        let claude = sign_in_args(Agent::Claude, "test-model").unwrap();
        assert!(claude.windows(2).any(|args| args == ["--tools", ""]));
        assert!(claude.contains(&"--safe-mode".into()));
        assert_eq!(
            sign_in_args(Agent::Codex, "test-model").unwrap(),
            ["codex", "login", "status"]
        );
        for kind in [Agent::Claude, Agent::Codex, Agent::Muse, Agent::Grok] {
            let args = sign_in_args(kind, "test-model").unwrap();
            assert!(
                !args
                    .iter()
                    .any(|arg| arg.contains("bypass") || arg == "--yolo")
            );
        }
    }
}

#[cfg(all(test, unix))]
mod notice_tests {
    use super::*;
    #[test]
    fn replacement_notice_does_not_change_the_real_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original");
        std::fs::write(&source, "original").unwrap();
        let home = dir.path().join("home");
        let link = home.join(".codex/auth.json");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert_eq!(renewal_notice(&home).unwrap(), None);
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, "renewed").unwrap();
        assert!(renewal_notice(&home).unwrap().is_some());
        assert_eq!(std::fs::read_to_string(source).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(link).unwrap(), "renewed");
    }
}
