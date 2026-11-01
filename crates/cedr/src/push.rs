//! `cedr guard push`: send a repo's pinned inventory and current findings to the cedr.dev app
//! (docs/design.md, Push). It reports and never gates: exit 0 when sent, 2 on any failure.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::common::{clip, head_commit, repo_name, settings};
use cedr_core::finding::Finding;
use cedr_core::lock;
use cedr_core::source::public_source;
use cedr_core::{lint, verify};
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
    /// Include available npm updates and their surface-change counts
    #[arg(long)]
    updates: bool,
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
    let settings = settings(args.org.as_deref(), args.config.as_deref())?;
    let root =
        fs::canonicalize(&args.path).map_err(|e| format!("push: {}: {e}", args.path.display()))?;
    let repo = repo_name(args.repo.as_deref(), &root)?;
    let (lock, _) = lock::read_lock(&root).map_err(|e| e.to_string())?;
    let lint = lint::lint(&root).map_err(|e| e.to_string())?;
    let verification = verify::verify(&root).map_err(|e| e.to_string())?;
    let mut body = json!({
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
    if args.updates {
        let report = cedr_core::outdated::outdated(&root).map_err(|e| e.to_string())?;
        for warning in &report.warnings {
            eprintln!("cedr: warning: {}", warning.escape_debug());
        }
        if report.updates.len() > MAX_ENTRIES {
            eprintln!(
                "cedr: warning: sent {MAX_ENTRIES} of {} updates",
                report.updates.len()
            );
        }
        body["updates"] = updates_body(&report.updates);
    }
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

/// The app stores change counts, keeping package text and descriptions out of update records.
fn updates_body(updates: &[cedr_core::outdated::Update]) -> Value {
    json!(updates.iter().take(MAX_ENTRIES).map(|u| json!({
        "client": u.client.as_str(), "name": clip(&u.name, MAX_NAME),
        "source": clip(&public_source(&u.source), MAX_NAME), "latest": clip(&u.latest, MAX_NAME),
        "severity": u.severity, "changes": u.changes.len(),
    })).collect::<Vec<_>>())
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

fn clip_all(list: &[String], entries: usize, chars: usize) -> Vec<String> {
    list.iter().take(entries).map(|s| clip(s, chars)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cedr_route::config;
    use std::path::Path;

    const SHA: &str = "3f2269d0c1aa3f2269d0c1aa3f2269d0c1aa3f22";

    #[test]
    fn update_payload_uses_counts_and_app_limits() {
        use cedr_core::{
            diff::{Change, ChangeKind, ChangeSeverity},
            discovery::Client,
            outdated::{Update, UpdateSeverity},
        };
        let updates: Vec<_> = (0..513)
            .map(|_| Update {
                client: Client::Antigravity,
                name: "n".repeat(257),
                source: "npm:example@1.0.0".into(),
                latest: "npm:example@1.1.0".into(),
                severity: UpdateSeverity::High,
                changes: vec![Change {
                    kind: ChangeKind::ToolAdded,
                    tool: Some("tool".into()),
                    detail: "description".into(),
                    before: None,
                    severity: ChangeSeverity::High,
                }],
            })
            .collect();
        let body = updates_body(&updates);
        assert_eq!(body.as_array().unwrap().len(), 512);
        assert_eq!(body[0].as_object().unwrap().len(), 6);
        assert_eq!(body[0]["client"], "antigravity");
        assert_eq!(body[0]["name"].as_str().unwrap().len(), 256);
        assert_eq!(body[0]["severity"], "high");
        assert_eq!(body[0]["changes"], 1);
    }

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
            updates: false,
        };
        let sync = settings(args.org.as_deref(), args.config.as_deref()).unwrap();
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
