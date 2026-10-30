//! Sync CLI, online pull and privacy-bounded inventory push.
use crate::{ReportFormat, common};
use clap::{Args as ClapArgs, Subcommand};
use serde_json::{Value, json};
use std::{path::PathBuf, process::ExitCode};
#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Repository root
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Print unified diffs without writing
    #[arg(long)]
    dry_run: bool,
    /// Replace unmanaged or manually edited generated files
    #[arg(long)]
    force: bool,
}
#[derive(Subcommand)]
enum Command {
    /// Import existing native files into .cedr sources
    Init {
        /// Repository root
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Comma-separated target names
        #[arg(long, value_delimiter = ',')]
        targets: Option<Vec<String>>,
    },
    /// Check drift and organization rules offline
    Check {
        /// Repository root
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Output text, JSON or GitHub annotations
        #[arg(long,value_enum,default_value_t=ReportFormat::Text)]
        format: ReportFormat,
    },
    /// Vendor organization rules from the app
    Pull(Online),
    /// Report configuration state to the app
    Push(Online),
}
#[derive(ClapArgs)]
struct Online {
    /// Repository root
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Organization public id
    #[arg(long)]
    org: Option<String>,
    /// Explicit routes configuration for API settings
    #[arg(long)]
    config: Option<PathBuf>,
    /// Repository name displayed in the app (push only)
    #[arg(long)]
    repo: Option<String>,
}
pub fn run(args: Args) -> Result<ExitCode, String> {
    match args.command {
        Some(Command::Init { path, targets }) => {
            for n in cedr_sync::import::init(&path, targets)? {
                println!("{n}");
            }
            println!("wrote .cedr/sync.toml; run cedr sync");
        }
        Some(Command::Check { path, format }) => {
            let r = cedr_sync::check(&path);
            crate::print_report(&r, format);
            return Ok(crate::exit(!r.is_empty()));
        }
        Some(Command::Pull(a)) => pull(a)?,
        Some(Command::Push(a)) => push(a)?,
        None => {
            let p = cedr_sync::plan(&args.path)?;
            crate::telemetry::count("sync_agents", p.config.targets.len());
            if args.dry_run {
                for f in &p.files {
                    print!("{}", f.diff);
                }
            } else if !cedr_sync::apply(&args.path, &p, args.force)? {
                for f in &p.files {
                    if f.state != "ok" {
                        eprintln!("refused {}: import first or use --force", f.path);
                    }
                }
                return Ok(ExitCode::from(1));
            } else {
                for f in &p.files {
                    println!(
                        "{} {}",
                        if f.bytes.is_none() {
                            "removed"
                        } else if f.state == "ok" {
                            "unchanged"
                        } else {
                            "wrote"
                        },
                        f.path
                    );
                }
            }
            for n in p.notes {
                println!("{n}");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}
fn pull(a: Online) -> Result<(), String> {
    let s = common::settings(a.org.as_deref(), a.config.as_deref())?;
    let key = cedr_route::sync::api_key().map_err(|e| e.to_string())?;
    let old = cedr_sync::io::read(&a.path, ".cedr/org-rules.json")?
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["version"].as_u64())
        .unwrap_or(0);
    match cedr_route::sync::get_sync_rules(&s, &key).map_err(|e| e.to_string())? {
        cedr_route::sync::SyncRules::Rules(v) => {
            cedr_sync::org::validate(&v)?;
            let new = v["version"].as_u64().ok_or("org rules: missing version")?;
            if v["org"] != s.org {
                return Err("org rules: organization mismatch".into());
            }
            cedr_sync::io::write(
                &a.path,
                ".cedr/org-rules.json",
                format!("{}\n", serde_json::to_string_pretty(&v).expect("json")).as_bytes(),
            )?;
            if old == new {
                println!("org rules v{new} unchanged");
            } else {
                println!("org rules v{old} -> v{new}");
            }
        }
        cedr_route::sync::SyncRules::None => {
            if cedr_sync::io::read(&a.path, ".cedr/org-rules.json")?.is_some() {
                std::fs::remove_file(cedr_sync::io::path(&a.path, ".cedr/org-rules.json")?)
                    .map_err(|_| "cannot remove .cedr/org-rules.json")?;
            }
            println!("organization has no rules");
        }
        cedr_route::sync::SyncRules::PlanRequired => {
            println!("organization plan lacks org rules; kept vendored file")
        }
    }
    Ok(())
}
pub fn check_value(path: &std::path::Path) -> Result<Value, String> {
    let p = match cedr_sync::plan(path) {
        Ok(p) => p,
        Err(_) => return Ok(json!({"findings":cedr_sync::check(path).findings,"files":[]})),
    };
    Ok(
        json!({"findings":p.report.findings,"files":p.files.iter().map(|f|json!({"path":f.path,"target":f.target,"state":f.state})).collect::<Vec<_>>()}),
    )
}
fn push(a: Online) -> Result<(), String> {
    let s = common::settings(a.org.as_deref(), a.config.as_deref())?;
    let key = cedr_route::sync::api_key().map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(&a.path).map_err(|_| "sync push: cannot resolve root")?;
    let repo = common::repo_name(a.repo.as_deref(), &root)?;
    let p = cedr_sync::plan(&root)?;
    let version = p
        .policy
        .as_ref()
        .and_then(|v| v["version"].as_u64())
        .unwrap_or(0);
    let rules = p
        .policy
        .as_ref()
        .map(|v| json!({"version":v["version"],"hash":v["hash"]}));
    let findings:Vec<_>=p.report.findings.iter().take(1000).map(|f|json!({"rule":f.rule,"severity":f.severity,"path":common::clip(&f.path,256),"line":f.line,"message":common::clip(&f.message,1024)})).collect();
    let files: Vec<_> = p
        .files
        .iter()
        .take(512)
        .map(|f| json!({"path":common::clip(&f.path,256),"target":f.target,"state":f.state}))
        .collect();
    let body = json!({"org":s.org,"repo":repo,"at":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs,true),"cli":env!("CARGO_PKG_VERSION"),"commit":common::head_commit(&root),"targets":p.config.targets,"rules":rules,"counts":p.counts,"files":files,"findings":findings});
    if body.to_string().len() > 512 * 1024 {
        return Err("sync push: body exceeds 512 KiB".into());
    }
    let response = cedr_route::sync::send_sync_repo(&s, &key, &body).map_err(|e| e.to_string())?;
    println!("pushed {repo} to {}", s.org);
    if let Some(new) = response["rules"]["version"].as_u64()
        && new > version
    {
        println!("org rules v{new} are available: run cedr sync pull");
    }
    Ok(())
}
