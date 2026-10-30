//! Shared push metadata and credential configuration.
use cedr_route::config::{self, Sync};
use std::{
    fs,
    path::{Path, PathBuf},
};
/// Where to push. `--org` without `--config` goes to the default API URL and reads no config at
/// all: in CI the checkout is untrusted, and a `.cedr/routes.toml` in it must never redirect the
/// API key. Otherwise the config found as Route finds it supplies `[sync]`, with `--org` applied.
pub(crate) fn settings(org: Option<&str>, config: Option<&Path>) -> Result<Sync, String> {
    let mut sync = match (org, config) {
        (Some(org), None) => Sync {
            org: org.to_string(),
            routes: Default::default(),
            url: None,
            auto: false,
        },
        _ => {
            let cwd =
                std::env::current_dir().map_err(|e| format!("push: current directory: {e}"))?;
            let path = config::locate(config, &cwd).map_err(|e| e.to_string())?;
            config::load_local(&path)
                .map_err(|e| e.to_string())?
                .sync
                .ok_or("push: no organization: pass --org or add [sync] org to routes.toml")?
        }
    };
    if let Some(org) = org {
        sync.org = org.to_string();
    }
    if !config::is_slug(&sync.org) {
        return Err(format!(
            "push: \"{}\" is not an organization slug (lowercase letters, digits and -)",
            sync.org
        ));
    }
    Ok(sync)
}

pub(crate) fn repo_name(flag: Option<&str>, root: &Path) -> Result<String, String> {
    let name = match flag {
        Some(name) => name.to_string(),
        None => root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let valid = (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'));
    if valid {
        Ok(name)
    } else {
        Err(format!(
            "push: repo name \"{name}\" must be 1-128 of A-Z a-z 0-9 . _ / -; pass --repo"
        ))
    }
}

pub(crate) fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// The checkout's `HEAD` commit read from `.git` (a directory, or a worktree's `gitdir:` file)
/// without running git; `None` when it is not a checkout or the ref cannot be resolved. Every
/// read is bounded and a ref must be a plain `refs/...` name, since the checkout is untrusted.
pub(crate) fn head_commit(root: &Path) -> Option<String> {
    let dot_git = root.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let text = read_small(&dot_git, SMALL)?;
        let dir = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
        if dir.is_absolute() {
            dir
        } else {
            root.join(dir)
        }
    };
    // A linked worktree keeps its own HEAD but shares refs with the main repository.
    let common = read_small(&git_dir.join("commondir"), SMALL)
        .map(|c| git_dir.join(c.trim()))
        .unwrap_or_else(|| git_dir.clone());
    let head = read_small(&git_dir.join("HEAD"), SMALL)?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref:").map(str::trim) else {
        return is_sha(head).then(|| head.to_string());
    };
    let plain = reference.starts_with("refs/")
        && !reference.contains('\\')
        && reference
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if !plain {
        return None;
    }
    for dir in [&git_dir, &common] {
        if let Some(sha) = read_small(&dir.join(reference), SMALL) {
            let sha = sha.trim();
            if is_sha(sha) {
                return Some(sha.to_string());
            }
        }
    }
    let packed = read_small(&common.join("packed-refs"), PACKED_REFS)?;
    packed.lines().find_map(|line| {
        let (sha, name) = line.split_once(' ')?;
        (name == reference && is_sha(sha)).then(|| sha.to_string())
    })
}

/// Git metadata files are tiny; packed refs can be large but are still bounded.
const SMALL: u64 = 4096;
const PACKED_REFS: u64 = 16 * 1024 * 1024;

/// A regular file's text, or `None` when it is not a regular file, unreadable or over `max`.
fn read_small(path: &Path, max: u64) -> Option<String> {
    use std::io::Read;
    if !fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    fs::File::open(path)
        .ok()?
        .take(max + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= max).then_some(text)
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
