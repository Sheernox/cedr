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

/// Remember the original target before the vendor can replace its sign-in link.
pub struct SignInLink {
    link: std::path::PathBuf,
    source: std::path::PathBuf,
    hash: Vec<u8>,
    refresh_required: bool,
}

fn content_hash(bytes: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).to_vec()
}

// A captured refresh-token format must not degrade into a signed-out object.
fn refresh_state(value: &serde_json::Value) -> (bool, bool) {
    match value {
        serde_json::Value::Object(fields) => {
            fields.iter().fold((false, false), |state, (key, v)| {
                let nested = refresh_state(v);
                let refresh = key.to_ascii_lowercase().replace(['_', '-'], "") == "refreshtoken";
                (
                    state.0 || refresh || nested.0,
                    state.1 || (refresh && v.as_str().is_some_and(|s| !s.is_empty())) || nested.1,
                )
            })
        }
        serde_json::Value::Array(items) => items.iter().fold((false, false), |state, v| {
            let nested = refresh_state(v);
            (state.0 || nested.0, state.1 || nested.1)
        }),
        _ => (false, false),
    }
}
fn usable(bytes: &[u8], refresh_required: bool) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|value| {
        value.as_object().is_some_and(|o| !o.is_empty())
            && (!refresh_required || refresh_state(&value).1)
    })
}

// Exchange retains the displaced candidate so it can be ranked before cleanup.
#[cfg(target_os = "linux")]
fn exchange(left: &Path, right: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let left = std::ffi::CString::new(left.as_os_str().as_bytes())?;
    let right = std::ffi::CString::new(right.as_os_str().as_bytes())?;
    // Both paths are live, NUL-terminated names, and no pointers escape the syscall.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
// Preserve each candidate's vendor write time when staging it; a fresh temporary
// inode's timestamp must never make an older refresh token win.
struct Candidate {
    bytes: Vec<u8>,
    written: std::time::SystemTime,
    permissions: std::fs::Permissions,
}
impl Candidate {
    fn read(path: &Path) -> std::io::Result<Self> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(Self {
            bytes,
            written: metadata.modified()?,
            permissions: metadata.permissions(),
        })
    }
    fn write(&self, path: &Path, permissions: std::fs::Permissions) -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::options()
            .write(true)
            .truncate(true)
            .open(path)?;
        file.write_all(&self.bytes)?;
        file.set_permissions(permissions)?;
        file.set_times(std::fs::FileTimes::new().set_modified(self.written))?;
        file.sync_all()
    }
}
fn exchange_or_fallback(left: &Path, right: &Path) -> std::io::Result<bool> {
    #[cfg(target_os = "linux")]
    return exchange_result(exchange(left, right));
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (left, right);
        Ok(false)
    }
}
#[cfg(target_os = "linux")]
fn exchange_result(result: std::io::Result<()>) -> std::io::Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(e) if exchange_unsupported(&e) => Ok(false),
        Err(e) => Err(e),
    }
}
#[cfg(target_os = "linux")]
fn exchange_unsupported(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP | libc::EXDEV)
    )
}

impl SignInLink {
    pub fn capture(home: &Path) -> std::io::Result<Vec<Self>> {
        let mut links = Vec::new();
        for name in [
            ".claude/.credentials.json",
            ".codex/auth.json",
            ".config/muse/auth.json",
            ".grok/auth.json",
            ".gemini/oauth_creds.json",
            ".gemini/antigravity-acp/acp_token.json",
        ] {
            let link = home.join(name);
            match std::fs::read_link(&link) {
                Ok(source) => {
                    let real = std::fs::File::open(&source)?;
                    use std::io::Read;
                    let mut bytes = Vec::new();
                    (&real).read_to_end(&mut bytes)?;
                    links.push(Self {
                        link,
                        source,
                        hash: content_hash(&bytes),
                        refresh_required: serde_json::from_slice(&bytes)
                            .is_ok_and(|v| refresh_state(&v).0),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(links)
    }
    /// Resolve by usable content and write order, retaining the real path's mode.
    /// Exchange ranks every displaced candidate. Unsupported platforms/filesystems
    /// retain a final check-to-rename window without a vendor-shared lock.
    pub fn save_renewed(&self) -> std::io::Result<Option<&'static str>> {
        self.save_with_hooks(|| {}, || {}, exchange_or_fallback)
    }
    fn save_with_hooks(
        &self,
        mut before_replace: impl FnMut(),
        mut after_replace: impl FnMut(),
        mut swap: impl FnMut(&Path, &Path) -> std::io::Result<bool>,
    ) -> std::io::Result<Option<&'static str>> {
        let replacement = std::fs::symlink_metadata(&self.link)?;
        if replacement.file_type().is_symlink() {
            return Ok(None);
        }
        if !replacement.is_file() {
            return Ok(Some(
                "trial sign-in is not a regular file; real sign-in kept",
            ));
        }
        let mut best = Candidate::read(&self.link)?;
        if content_hash(&best.bytes) == self.hash {
            return Ok(None);
        }
        if !usable(&best.bytes, self.refresh_required) {
            return Ok(Some("trial sign-in is unusable; real sign-in kept"));
        }
        let parent = self
            .source
            .parent()
            .ok_or_else(|| std::io::Error::other("sign-in has no parent"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?.into_temp_path();
        // An I/O error must retain a displaced credential that could not be ranked.
        temporary.disable_cleanup(true);
        let outcome = (|| {
            let mut changed = false;
            let mut permissions = Candidate::read(&self.source)?.permissions;
            let mut installed_hash = None;
            for _ in 0..4 {
                if installed_hash.is_some() {
                    let staged = Candidate::read(&temporary)?;
                    if self.rank(&staged) > self.rank(&best) {
                        permissions = staged.permissions.clone();
                        best = staged;
                    }
                }
                let real = Candidate::read(&self.source)?;
                if installed_hash.as_ref() != Some(&content_hash(&real.bytes)) {
                    changed |= content_hash(&real.bytes) != self.hash;
                    permissions = real.permissions.clone();
                }
                let real_is_installed = installed_hash.as_ref() == Some(&content_hash(&real.bytes));
                if self.rank(&real) > self.rank(&best)
                    || (self.rank(&real) == self.rank(&best)
                        && (!real_is_installed || real.bytes == best.bytes))
                {
                    return Ok(if real.bytes == best.bytes {
                        changed.then_some("trial renewal saved after real sign-in changed")
                    } else {
                        Some("newer sign-in kept; trial renewal was not saved")
                    });
                }
                best.write(&temporary, permissions.clone())?;
                let prior_installed = installed_hash.replace(content_hash(&best.bytes));
                before_replace();
                if swap(&temporary, &self.source)? {
                    after_replace();
                    // Do not roll back blindly: that could displace an even newer renewal.
                    // Keep the best snapshot while the next pass also ranks the real path.
                    let displaced = Candidate::read(&temporary)?;
                    permissions = displaced.permissions.clone();
                    if self.rank(&displaced) > self.rank(&best)
                        || (self.rank(&displaced) == self.rank(&best)
                            && prior_installed.as_ref() != Some(&content_hash(&displaced.bytes)))
                    {
                        best = displaced;
                    }
                } else {
                    // A filesystem without exchange cannot close this last rename window.
                    let latest = Candidate::read(&self.source)?;
                    if self.rank(&latest) >= self.rank(&best) {
                        return Ok(Some("newer sign-in kept; trial renewal was not saved"));
                    }
                    if latest.bytes != real.bytes || latest.written != real.written {
                        continue;
                    }
                    std::fs::rename(&temporary, &self.source)?;
                    return Ok(changed.then_some("trial renewal saved after real sign-in changed"));
                }
            }
            // The bounded exit must install the best observed candidate, not just keep
            // whichever inode the last exchange happened to leave at the real path.
            let staged = Candidate::read(&temporary)?;
            if self.rank(&staged) > self.rank(&best) {
                permissions = staged.permissions.clone();
                best = staged;
            }
            let real = Candidate::read(&self.source)?;
            if self.rank(&best) > self.rank(&real)
                || (self.rank(&best) == self.rank(&real)
                    && installed_hash.as_ref() == Some(&content_hash(&real.bytes))
                    && best.bytes != real.bytes)
            {
                best.write(&temporary, permissions)?;
                std::fs::rename(&temporary, &self.source)?;
            }
            Ok(Some(
                "sign-in resolved after concurrent writes by usable content and write order",
            ))
        })();
        if outcome.is_ok() {
            temporary.disable_cleanup(false);
        }
        outcome.map_err(|error: std::io::Error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "cannot resolve sign-in; staged candidate retained beside real sign-in: {error}"
                ),
            )
        })
    }
    fn rank(&self, candidate: &Candidate) -> (bool, bool, std::time::SystemTime) {
        (
            usable(&candidate.bytes, self.refresh_required),
            content_hash(&candidate.bytes) != self.hash,
            candidate.written,
        )
    }
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
}

#[cfg(all(test, unix))]
mod renewal_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn replaced_links_are_saved_atomically_and_unchanged_links_are_left_alone() {
        for relative in [".codex/auth.json", ".claude/.credentials.json"] {
            let dir = tempfile::tempdir().unwrap();
            let original = dir.path().join("original");
            std::fs::write(&original, r#"{"token":"old"}"#).unwrap();
            std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o600)).unwrap();
            let home = dir.path().join("home");
            let link = home.join(relative);
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(&original, &link).unwrap();
            let links = SignInLink::capture(&home).unwrap();
            links[0].save_renewed().unwrap();
            assert_eq!(std::fs::read(&original).unwrap(), br#"{"token":"old"}"#);
            std::fs::remove_file(&link).unwrap();
            std::fs::write(&link, r#"{"token":"renewed"}"#).unwrap();
            let mut prior = std::fs::File::open(&original).unwrap();
            links[0].save_renewed().unwrap();
            assert_eq!(std::fs::read(&original).unwrap(), br#"{"token":"renewed"}"#);
            use std::io::Read;
            let mut retained = String::new();
            prior.read_to_string(&mut retained).unwrap();
            assert_eq!(retained, r#"{"token":"old"}"#);
            assert_eq!(
                std::fs::metadata(original).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn concurrent_sign_in_and_invalid_renewals_preserve_the_original() {
        for replacement in ["", "invalid", "{}", r#"{"token":"stale"}"#] {
            let dir = tempfile::tempdir().unwrap();
            let original = dir.path().join("original");
            let home = dir.path().join("home");
            let link = home.join(".codex/auth.json");
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::fs::write(&original, r#"{"token":"old"}"#).unwrap();
            symlink(&original, &link).unwrap();
            let links = SignInLink::capture(&home).unwrap();
            std::fs::remove_file(&link).unwrap();
            std::fs::write(&link, replacement).unwrap();
            if replacement.contains("stale") {
                let newer = dir.path().join("newer");
                std::fs::write(&newer, r#"{"token":"new"}"#).unwrap();
                std::fs::rename(newer, &original).unwrap();
                assert!(
                    links[0]
                        .save_renewed()
                        .unwrap()
                        .unwrap()
                        .contains("newer sign-in kept")
                );
                assert_eq!(
                    std::fs::read_to_string(original).unwrap(),
                    r#"{"token":"new"}"#
                );
            } else {
                assert!(
                    links[0]
                        .save_renewed()
                        .unwrap()
                        .unwrap()
                        .contains("unusable")
                );
                assert_eq!(
                    std::fs::read_to_string(original).unwrap(),
                    r#"{"token":"old"}"#
                );
            }
        }
    }
    // Explicit mtimes make order tests deterministic without waiting on filesystem clocks.
    fn stamp(path: &Path, seconds: u64) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds)),
            )
            .unwrap();
    }
    #[test]
    fn valid_content_and_write_order_decide_every_conflict() {
        for (real, trial, time, expected) in [
            (
                r#"{"nested":{"RefreshToken":"R0"}}"#,
                r#"{"refresh_token":"R2"}"#,
                300,
                "R2",
            ),
            (
                r#"{"refresh_token":""}"#,
                r#"{"refresh_token":"R2"}"#,
                300,
                "R2",
            ),
            (
                r#"{"refresh_token":"R1"}"#,
                r#"{"refresh_token":"R2"}"#,
                300,
                "R1",
            ),
            (
                r#"{"refresh_token":"R1"}"#,
                r#"{"refresh_token":"R2"}"#,
                100,
                "R2",
            ),
            (
                r#"{"refresh_token":"R1"}"#,
                r#"{"refresh_token":"R2"}"#,
                200,
                "R1",
            ),
            (
                r#"{"refresh_token":"R1"}"#,
                r#"{"refresh_token":""}"#,
                100,
                "R1",
            ),
            (
                r#"{"refresh_token":"R1"}"#,
                r#"{"access":"only"}"#,
                100,
                "R1",
            ),
            (r#"{"refresh_token":"R1"}"#, "null", 100, "R1"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("original");
            std::fs::write(&source, r#"{"nested":{"RefreshToken":"R0"}}"#).unwrap();
            let link = dir.path().join("home/.codex/auth.json");
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(&source, &link).unwrap();
            let captured = SignInLink::capture(&dir.path().join("home")).unwrap();
            std::fs::remove_file(&link).unwrap();
            std::fs::write(&link, trial).unwrap();
            stamp(&link, 200);
            std::fs::write(&source, real).unwrap();
            stamp(&source, time);
            std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o640)).unwrap();
            captured[0].save_renewed().unwrap();
            assert!(std::fs::read_to_string(&source).unwrap().contains(expected));
            assert_eq!(
                std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
                0o640
            );
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn exchange_ranks_both_race_windows_and_keeps_best_at_the_bound() {
        for (repeat, after, written) in
            [(false, true, 300), (true, false, 300), (false, false, 200)]
        {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("original");
            std::fs::write(&source, r#"{"refresh_token":"A"}"#).unwrap();
            let link = dir.path().join("home/.codex/auth.json");
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(&source, &link).unwrap();
            let captured = SignInLink::capture(&dir.path().join("home")).unwrap();
            std::fs::remove_file(&link).unwrap();
            std::fs::write(&link, r#"{"refresh_token":"B"}"#).unwrap();
            stamp(&link, 200);
            let calls = std::cell::Cell::new(0);
            let install = |token: &str, time| {
                let next = dir.path().join("vendor");
                std::fs::write(&next, format!(r#"{{"refresh_token":"{token}"}}"#)).unwrap();
                stamp(&next, time);
                std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o600)).unwrap();
                std::fs::rename(next, &source).unwrap();
            };
            captured[0]
                .save_with_hooks(
                    || {
                        calls.set(calls.get() + 1);
                        let token = if repeat {
                            format!("C-{}", calls.get())
                        } else {
                            "C".into()
                        };
                        install(
                            &token,
                            if repeat {
                                written + calls.get() * 2
                            } else {
                                written
                            },
                        );
                    },
                    || {
                        // A second vendor rename used to land in the rollback's temp
                        // path and be deleted, invalidating the C restored at source.
                        if after {
                            install("D", 400);
                        }
                    },
                    exchange_or_fallback,
                )
                .unwrap();
            assert_eq!(
                calls.get(),
                if repeat {
                    4
                } else if after {
                    1
                } else {
                    2
                }
            );
            assert!(
                std::fs::read_to_string(&source)
                    .unwrap()
                    .contains(if repeat {
                        "C-4"
                    } else if after {
                        "D"
                    } else {
                        "C"
                    })
            );
            assert_eq!(
                std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
                0o600
            );
            if repeat {
                assert_eq!(
                    std::fs::metadata(&source).unwrap().modified().unwrap(),
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(308)
                );
            }
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        }
    }
    #[test]
    fn signed_out_shapes_and_identical_trials_never_replace_sign_in() {
        let original = r#"{"tokens":{"refresh_token":"A"},"last_refresh":"old"}"#;
        for trial in [
            r#"{"tokens":{"refresh_token":""},"last_refresh":"timestamp"}"#,
            r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","refreshTokenExpiresAt":123},"loginEnded":"timestamp"}"#,
            original,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("original");
            std::fs::write(&source, original).unwrap();
            let link = dir.path().join("home/.codex/auth.json");
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(&source, &link).unwrap();
            let captured = SignInLink::capture(&dir.path().join("home")).unwrap();
            std::fs::remove_file(&link).unwrap();
            std::fs::write(&link, trial).unwrap();
            stamp(&link, 300);
            let expected = if trial == original {
                r#"{"refresh_token":"B"}"#
            } else {
                original
            };
            std::fs::write(&source, expected).unwrap();
            stamp(&source, 200);
            captured[0].save_renewed().unwrap();
            assert_eq!(std::fs::read_to_string(&source).unwrap(), expected);
        }
        for key in ["refresh_at", "last_refresh", "refreshTokenExpiresAt"] {
            assert_eq!(
                refresh_state(&serde_json::json!({key: "timestamp"})),
                (false, false)
            );
        }
        assert_eq!(
            refresh_state(&serde_json::json!({"Refresh-Token": "A"})),
            (true, true)
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn unsupported_exchange_uses_rechecked_rename() {
        for code in [libc::EINVAL, libc::ENOSYS, libc::ENOTSUP, libc::EXDEV] {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("original");
            std::fs::write(&source, r#"{"refresh_token":"A"}"#).unwrap();
            let link = dir.path().join("home/.codex/auth.json");
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(&source, &link).unwrap();
            let captured = SignInLink::capture(&dir.path().join("home")).unwrap();
            std::fs::remove_file(&link).unwrap();
            std::fs::write(&link, r#"{"refresh_token":"B"}"#).unwrap();
            captured[0]
                .save_with_hooks(
                    || {},
                    || {},
                    |_, _| exchange_result(Err(std::io::Error::from_raw_os_error(code))),
                )
                .unwrap();
            assert!(std::fs::read_to_string(source).unwrap().contains('B'));
        }
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
