//! `cedr push`: send a repo's pinned inventory and current findings to the cedr.dev app
//! (docs/design.md, Push). It reports and never gates: exit 0 when sent, 2 on any failure.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cedr_core::finding::Finding;
use cedr_core::lock;
use cedr_core::source::public_source;
use cedr_core::{lint, verify};
use cedr_route::config::{self, Sync};
use clap::Args;
use serde_json::{Value, json};

#[derive(Args)]
pub struct PushArgs {
    /// Repo root
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Name the app shows for this repo; default the root directory's name
    #[arg(long)]
    repo: Option<String>,
    /// Organization slug; overrides [sync] org, so CI needs only CEDR_API_KEY and this
    #[arg(long)]
    org: Option<String>,
    /// routes.toml whose [sync] settings to use instead of the usual search
    #[arg(long)]
    config: Option<PathBuf>,
}

/// The most entries of each list, and the longest strings, a push carries; the app refuses more.
const MAX_ENTRIES: usize = 512;
const MAX_SERVER_LIST: usize = 256;
const MAX_FINDINGS: usize = 1000;
const MAX_NAME: usize = 256;
const MAX_TEXT: usize = 1024;
/// The app refuses larger bodies with 413.
const MAX_BODY: usize = 2 * 1024 * 1024;

pub fn run(args: &PushArgs) -> Result<ExitCode, String> {
    let settings = settings(args)?;
    let root =
        fs::canonicalize(&args.path).map_err(|e| format!("push: {}: {e}", args.path.display()))?;
    let repo = repo_name(args.repo.as_deref(), &root)?;
    let (lock, _) = lock::read_lock(&root).map_err(|e| e.to_string())?;
    let lint = lint::lint(&root).map_err(|e| e.to_string())?;
    let verification = verify::verify(&root).map_err(|e| e.to_string())?;
    let body = json!({
        "org": settings.org,
        "repo": repo,
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "cli": env!("CARGO_PKG_VERSION"),
        "commit": head_commit(&root),
        "skills": lock.skill.iter().take(MAX_ENTRIES).map(|s| json!({
            "name": clip(&s.name, MAX_NAME),
            "path": clip(&s.path, MAX_NAME),
            "content": clip(&s.content, MAX_TEXT),
            "allowed_tools": clip_all(&s.allowed_tools, MAX_ENTRIES, MAX_TEXT),
        })).collect::<Vec<_>>(),
        "mcp": lock.mcp.iter().take(MAX_ENTRIES).map(|m| json!({
            "client": m.client.as_str(),
            "name": clip(&m.name, MAX_NAME),
            "transport": m.transport.as_str(),
            "source": clip(&public_source(&m.source), MAX_NAME),
            "pinned": m.pinned,
            "method": m.method.as_str(),
            "surface": m.surface.as_deref().map(|h| clip(h, MAX_TEXT)),
            "tools": clip_all(&m.tools, MAX_SERVER_LIST, MAX_NAME),
            "egress": clip_all(&m.egress, MAX_SERVER_LIST, MAX_NAME),
        })).collect::<Vec<_>>(),
        "lint": findings(&lint.findings),
        "verify": findings(&verification.report.findings),
    });
    let size = body.to_string().len();
    if size > MAX_BODY {
        return Err(format!(
            "push: the inventory is {size} bytes, over the app's {MAX_BODY}-byte limit"
        ));
    }
    for warning in truncated(
        &lock,
        lint.findings.len(),
        verification.report.findings.len(),
    ) {
        eprintln!("cedr: warning: {warning}");
    }
    let key = cedr_route::sync::api_key().map_err(|e| e.to_string())?;
    cedr_route::sync::send_guard(&settings, &key, &body).map_err(|e| e.to_string())?;
    println!(
        "pushed {repo} to {}: {} skills, {} MCP servers, {} lint and {} verify findings",
        settings.org,
        lock.skill.len(),
        lock.mcp.len(),
        lint.findings.len(),
        verification.report.findings.len()
    );
    Ok(ExitCode::SUCCESS)
}

/// Where to push. `--org` without `--config` goes to the default API URL and reads no config at
/// all: in CI the checkout is untrusted, and a `.cedr/routes.toml` in it must never redirect the
/// API key. Otherwise the config found as Route finds it supplies `[sync]`, with `--org` applied.
fn settings(args: &PushArgs) -> Result<Sync, String> {
    let mut sync = match (&args.org, &args.config) {
        (Some(org), None) => Sync {
            org: org.clone(),
            routes: Default::default(),
            url: None,
            auto: false,
        },
        _ => {
            let cwd =
                std::env::current_dir().map_err(|e| format!("push: current directory: {e}"))?;
            let path = config::locate(args.config.as_deref(), &cwd).map_err(|e| e.to_string())?;
            config::load_local(&path)
                .map_err(|e| e.to_string())?
                .sync
                .ok_or("push: no organization: pass --org or add [sync] org to routes.toml")?
        }
    };
    if let Some(org) = &args.org {
        sync.org = org.clone();
    }
    if !config::is_slug(&sync.org) {
        return Err(format!(
            "push: \"{}\" is not an organization slug (lowercase letters, digits and -)",
            sync.org
        ));
    }
    Ok(sync)
}

fn repo_name(flag: Option<&str>, root: &Path) -> Result<String, String> {
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

fn findings(list: &[Finding]) -> Vec<Value> {
    list.iter()
        .take(MAX_FINDINGS)
        .map(|f| {
            json!({
                "rule": f.rule,
                "severity": f.severity,
                "path": clip(&f.path, MAX_NAME),
                "line": f.line,
                "message": clip(&f.message, MAX_TEXT),
            })
        })
        .collect()
}

/// What a push leaves out at the app's list limits, so a large repo never looks fully reported.
fn truncated(lock: &lock::Lock, lint: usize, verify: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut over = |what: &str, total: usize, max: usize| {
        if total > max {
            out.push(format!(
                "sent {max} of {total} {what}; the app keeps at most {max}"
            ));
        }
    };
    over("skills", lock.skill.len(), MAX_ENTRIES);
    over("MCP servers", lock.mcp.len(), MAX_ENTRIES);
    over("lint findings", lint, MAX_FINDINGS);
    over("verify findings", verify, MAX_FINDINGS);
    for m in lock.mcp.iter().take(MAX_ENTRIES) {
        over(
            &format!("tools of {}", m.name),
            m.tools.len(),
            MAX_SERVER_LIST,
        );
        over(
            &format!("egress hosts of {}", m.name),
            m.egress.len(),
            MAX_SERVER_LIST,
        );
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn clip_all(list: &[String], entries: usize, chars: usize) -> Vec<String> {
    list.iter().take(entries).map(|s| clip(s, chars)).collect()
}

/// The checkout's `HEAD` commit read from `.git` (a directory, or a worktree's `gitdir:` file)
/// without running git; `None` when it is not a checkout or the ref cannot be resolved. Every
/// read is bounded and a ref must be a plain `refs/...` name, since the checkout is untrusted.
fn head_commit(root: &Path) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "3f2269d0c1aa3f2269d0c1aa3f2269d0c1aa3f22";

    #[test]
    fn reads_head_through_loose_and_packed_refs() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        fs::create_dir_all(git.join("refs/heads")).unwrap();
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(
            git.join("packed-refs"),
            format!("# pack-refs\n{SHA} refs/heads/main\n"),
        )
        .unwrap();
        assert_eq!(head_commit(dir.path()).as_deref(), Some(SHA));

        let loose = SHA.replace('3', "4");
        fs::write(git.join("refs/heads/main"), format!("{loose}\n")).unwrap();
        assert_eq!(head_commit(dir.path()), Some(loose));
    }

    #[test]
    fn reads_a_worktree_head_with_shared_refs() {
        let dir = tempfile::tempdir().unwrap();
        let common = dir.path().join("main/.git");
        let wt = common.join("worktrees/feature");
        fs::create_dir_all(common.join("refs/heads")).unwrap();
        fs::create_dir_all(&wt).unwrap();
        fs::write(common.join("refs/heads/feature"), SHA).unwrap();
        fs::write(wt.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
        fs::write(wt.join("commondir"), "../..\n").unwrap();
        let checkout = dir.path().join("feature");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join(".git"), format!("gitdir: {}\n", wt.display())).unwrap();
        assert_eq!(head_commit(&checkout).as_deref(), Some(SHA));
        assert_eq!(head_commit(dir.path()), None);
    }

    #[test]
    fn refuses_refs_that_leave_the_git_dir_or_are_not_files() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        fs::create_dir_all(git.join("refs/heads")).unwrap();
        fs::write(dir.path().join("outside"), SHA).unwrap();
        for head in [
            "ref: ../outside",
            "ref: refs/../../outside",
            "ref: /dev/zero",
            "ref: refs/heads",
        ] {
            fs::write(git.join("HEAD"), head).unwrap();
            assert_eq!(head_commit(dir.path()), None, "{head}");
        }
    }

    #[test]
    fn org_without_config_never_reads_the_checkout() {
        let args = PushArgs {
            path: ".".into(),
            repo: None,
            org: Some("acme".into()),
            config: None,
        };
        let sync = settings(&args).unwrap();
        assert_eq!(
            (sync.org.as_str(), sync.url()),
            ("acme", config::DEFAULT_API_URL)
        );
    }

    #[test]
    fn warns_about_each_list_cut_to_the_limits() {
        let lock = lock::Lock {
            version: 1,
            skill: Vec::new(),
            mcp: Vec::new(),
        };
        assert!(truncated(&lock, MAX_FINDINGS, 0).is_empty());
        assert_eq!(
            truncated(&lock, MAX_FINDINGS + 5, 0),
            ["sent 1000 of 1005 lint findings; the app keeps at most 1000"]
        );
    }

    #[test]
    fn repo_names_follow_the_contract() {
        let root = Path::new("/tmp/my repo");
        assert!(repo_name(None, root).is_err());
        assert_eq!(repo_name(Some("acme/web"), root).unwrap(), "acme/web");
        assert!(repo_name(Some(&"a".repeat(129)), root).is_err());
        assert!(repo_name(Some(""), root).is_err());
    }
}
