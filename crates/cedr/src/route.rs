//! `cedr route ...`: pick, tag, list, verdicts and stats over `cedr-route` (docs/route.md, Commands).
//! Exit codes: 0 delegate to the printed target, 1 keep the work, 2 usage, config or I/O failure.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cedr_route::history::{self, Verdict};
use cedr_route::pick::{self, Pick, Size};
use cedr_route::readers;
use cedr_route::readings::{MAX_BYTES, parse_doc, read_limited};
use cedr_route::session::{Overrides, Session};
use cedr_route::sync::Pulled;
use cedr_route::{Error, Result};
use chrono::Utc;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::Value;

#[derive(Clone, Copy, ValueEnum)]
pub enum PickFormat {
    Text,
    Json,
    T3,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ListFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum SizeArg {
    Small,
    Medium,
    Large,
}

impl From<SizeArg> for Size {
    fn from(s: SizeArg) -> Size {
        match s {
            SizeArg::Small => Size::Small,
            SizeArg::Medium => Size::Medium,
            SizeArg::Large => Size::Large,
        }
    }
}

#[derive(Args)]
pub struct ConfigArg {
    /// routes.toml to use instead of the usual search
    #[arg(long, global = true)]
    config: Option<PathBuf>,
}

impl ConfigArg {
    pub fn path(&self) -> Option<&Path> {
        self.config.as_deref()
    }
}

#[derive(Args)]
pub struct SourceArgs {
    /// Readings JSON replacing the configured sources (`-` for stdin)
    #[arg(long)]
    pub readings: Option<PathBuf>,
    /// {"<harness>": [model ids]} replacing the model lists (`-` for stdin)
    #[arg(long)]
    pub models: Option<PathBuf>,
    /// Do not record the pick in the local history
    #[arg(long)]
    pub no_record: bool,
}

#[derive(Subcommand)]
pub enum RouteCommand {
    /// Choose the harness and model for a route
    Pick {
        route: String,
        #[arg(long, value_enum, default_value_t = SizeArg::Medium)]
        size: SizeArg,
        /// Harness to skip (it just failed on a limit or capacity); repeatable
        #[arg(long)]
        avoid: Vec<String>,
        #[arg(long, value_enum, default_value_t = PickFormat::Text)]
        format: PickFormat,
        #[command(flatten)]
        sources: SourceArgs,
    },
    /// Tag a model the user named, without routing
    Manual {
        /// <harness>/<model>, a family name or an exact id
        target: String,
        #[arg(long, default_value = "")]
        effort: String,
        #[arg(long, value_enum, default_value_t = PickFormat::Text)]
        format: PickFormat,
        #[command(flatten)]
        sources: SourceArgs,
    },
    /// State of every harness's plan
    Pools {
        #[arg(long, value_enum, default_value_t = ListFormat::Text)]
        format: ListFormat,
        /// Readings JSON replacing the configured sources (`-` for stdin)
        #[arg(long)]
        readings: Option<PathBuf>,
    },
    /// Routes and their tier chains
    List,
    /// Record a verdict on a pick
    Verdict {
        id: String,
        #[arg(value_parser = ["accepted", "reworked", "redone", "rejected"])]
        verdict: String,
        /// The orchestrator's task id for the delegated work
        #[arg(long)]
        task: Option<String>,
        /// One line, at most 120 characters
        #[arg(long)]
        note: Option<String>,
    },
    /// Picks and verdicts per route and model
    Stats {
        #[arg(long)]
        route: Option<String>,
        #[arg(long, value_enum, default_value_t = ListFormat::Text)]
        format: ListFormat,
    },
    /// Claude Code status line: capture plan usage for the claude-statusline reader
    Statusline {
        /// The harness the capture is for
        #[arg(long, default_value = "claude")]
        harness: String,
        /// Your own status line command, run with the same input; its output is printed instead
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Push history, pull the org's shared routes and send this machine's snapshot to the cedr.dev app
    Sync {
        #[arg(long, conflicts_with = "pull_only")]
        push_only: bool,
        #[arg(long)]
        pull_only: bool,
        /// Print only errors (background syncs)
        #[arg(long)]
        quiet: bool,
    },
}

/// A JSON object from a file or stdin (`-`), refused past the readings size cap.
pub(crate) fn read_json(path: &Path) -> Result<Value> {
    let io = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    let bytes = if path == Path::new("-") {
        read_limited(std::io::stdin().lock(), MAX_BYTES).map_err(io)?
    } else {
        read_limited(std::fs::File::open(path).map_err(io)?, MAX_BYTES).map_err(io)?
    };
    parse_doc(&bytes).map_err(|message| Error::Parse {
        path: path.to_path_buf(),
        message,
    })
}

pub(crate) fn cwd() -> Result<PathBuf> {
    std::env::current_dir().map_err(|source| Error::Io {
        path: PathBuf::from("."),
        source,
    })
}

pub(crate) fn open(
    config: Option<&Path>,
    readings: Option<&Path>,
    models: Option<&Path>,
) -> Result<Session> {
    let overrides = Overrides {
        readings: readings.map(read_json).transpose()?,
        models: models.map(read_json).transpose()?,
    };
    let session = Session::open(config, &cwd()?, overrides)?;
    for w in &session.warnings {
        eprintln!("cedr: {w}");
    }
    Ok(session)
}

/// The T3 target object, spaced the way Python's `json.dumps` prints it so Python tooling sees identical text.
fn t3_json(d: &pick::Delegate) -> String {
    let q = |s: &str| serde_json::to_string(s).expect("string serializes");
    let options: Vec<String> =
        d.t3.options
            .iter()
            .map(|(k, v)| format!("{}: {}", q(k), v))
            .collect();
    format!(
        "{{\"providerInstanceId\": {}, \"model\": {}, \"options\": {{{}}}}}",
        q(&d.t3.provider_instance_id),
        q(&d.t3.model),
        options.join(", ")
    )
}

pub(crate) fn print_pick(p: &Pick, format: PickFormat) -> ExitCode {
    if let PickFormat::Json = format {
        println!(
            "{}",
            serde_json::to_string_pretty(p).expect("pick serializes")
        );
    } else if p.delegates.is_empty() {
        let orch = p
            .orchestrator
            .as_ref()
            .map(|o| format!("; {o}"))
            .unwrap_or_default();
        println!(
            "keep: {} ({}){orch}",
            p.route,
            p.reason.as_deref().unwrap_or("")
        );
    } else {
        for d in &p.delegates {
            println!("{}", d.tag);
            match format {
                PickFormat::T3 => println!("  delegate_task role={} target={}", p.role, t3_json(d)),
                _ => println!("  {} role={}", d.target, p.role),
            }
            if !d.prefix.is_empty() {
                println!("  brief starts: {}", d.prefix);
            }
        }
        if p.council && !p.skipped.is_empty() {
            println!(
                "absent (report, never substitute): {}",
                p.skipped.join(", ")
            );
        }
    }
    if p.delegates.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub(crate) fn record(
    p: &Pick,
    size: Size,
    sources: &SourceArgs,
    config: Option<&Path>,
) -> Result<()> {
    if sources.no_record {
        return Ok(());
    }
    history::record_pick(&history::default_path(), p, size, Utc::now())?;
    auto_sync(config);
    Ok(())
}

/// Starts a background sync when one is due and the config in use has `[sync]` with `auto` on
/// (docs/route.md, Sync). The throttle comes first, so a status line render costs one stat until
/// a sync is due. It never fails the caller: a problem starting it goes to the sync log, which the
/// next manual `cedr route sync` shows.
pub(crate) fn auto_sync(config: Option<&Path>) {
    let start = || -> Result<()> {
        if !cedr_route::sync::auto_now()? {
            return Ok(());
        }
        // No routes.toml (a status line outside any setup) means nothing to sync, and a broken one
        // is reported by the command that reads it, not once per status line render here.
        let Ok(path) = cedr_route::config::locate(config, &cwd()?) else {
            return Ok(());
        };
        let Ok(local) = cedr_route::config::load_local(&path) else {
            return Ok(());
        };
        if !local.sync.is_some_and(|s| s.auto) {
            return Ok(());
        }
        let exe = std::env::current_exe().map_err(|e| {
            Error::Config(format!(
                "sync background: cannot find the cedr program ({e})"
            ))
        })?;
        cedr_route::sync::auto_start(&exe, &path)
    };
    if let Err(e) = start() {
        write_sync_log(&e.to_string());
    }
}

/// Leaves a background sync's error for the next manual run; failing that, stderr is all there is.
fn write_sync_log(message: &str) {
    let path = cedr_route::sync::log_path();
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, format!("{message}\n")));
    if let Err(e) = written {
        eprintln!("cedr: {message} (and cannot write {}: {e})", path.display());
    }
}

pub fn run(command: RouteCommand, config: &ConfigArg) -> Result<ExitCode> {
    let config = config.config.as_deref();
    match command {
        RouteCommand::Pick {
            route,
            size,
            avoid,
            format,
            sources,
        } => {
            let session = open(
                config,
                sources.readings.as_deref(),
                sources.models.as_deref(),
            )?;
            let p = session.context().pick(&route, size.into(), &avoid)?;
            record(&p, size.into(), &sources, config)?;
            Ok(print_pick(&p, format))
        }
        RouteCommand::Manual {
            target,
            effort,
            format,
            sources,
        } => {
            let session = open(
                config,
                sources.readings.as_deref(),
                sources.models.as_deref(),
            )?;
            let p = session.context().manual(&target, &effort)?;
            record(&p, Size::Medium, &sources, config)?;
            Ok(print_pick(&p, format))
        }
        RouteCommand::Pools { format, readings } => {
            let session = open(config, readings.as_deref(), None)?;
            let rows = session.context().pools();
            match format {
                ListFormat::Json => println!(
                    "{}",
                    serde_json::to_string_pretty(&rows).expect("rows serialize")
                ),
                ListFormat::Text => {
                    for r in rows {
                        let family = if r.family.is_empty() {
                            String::new()
                        } else {
                            format!(" ({} models)", r.family)
                        };
                        println!("{:8} {}{family}", r.state.as_str(), r.summary);
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        RouteCommand::List => {
            let loaded = Session::config_only(config, &cwd()?)?;
            for r in pick::list(&loaded.config) {
                let chain: Vec<String> = r.tiers.iter().map(|t| t.join(" = ")).collect();
                println!(
                    "{:16} {} {}",
                    r.id,
                    if r.enabled { "on " } else { "off" },
                    chain.join(" > ")
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        RouteCommand::Verdict {
            id,
            verdict,
            task,
            note,
        } => {
            let v = Verdict::parse(&verdict)?;
            history::record_verdict(
                &history::default_path(),
                &id,
                v,
                task.as_deref(),
                note.as_deref(),
                Utc::now(),
            )?;
            println!("recorded {verdict} for {id}");
            auto_sync(config);
            Ok(ExitCode::SUCCESS)
        }
        RouteCommand::Stats { route, format } => {
            let rows = history::stats(&history::default_path(), route.as_deref())?;
            match format {
                ListFormat::Json => println!(
                    "{}",
                    serde_json::to_string_pretty(&rows).expect("rows serialize")
                ),
                ListFormat::Text => {
                    println!(
                        "{:16} {:32} {:>5} {:>8} {:>8} {:>6} {:>8} {:>9}",
                        "route",
                        "target",
                        "picks",
                        "accepted",
                        "reworked",
                        "redone",
                        "rejected",
                        "accepted%"
                    );
                    for r in rows {
                        let pct = r
                            .accepted_pct
                            .map(|p| format!("{p:.1}"))
                            .unwrap_or_else(|| "-".into());
                        let mark = if r.provisional { " (provisional)" } else { "" };
                        println!(
                            "{:16} {:32} {:>5} {:>8} {:>8} {:>6} {:>8} {:>9}{mark}",
                            r.route,
                            r.target,
                            r.picks,
                            r.accepted,
                            r.reworked,
                            r.redone,
                            r.rejected,
                            pct
                        );
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        RouteCommand::Statusline { harness, command } => {
            let code = statusline(&harness, &command);
            auto_sync(config);
            Ok(code)
        }
        RouteCommand::Sync {
            push_only,
            pull_only,
            quiet,
        } => {
            if !quiet {
                // A background run's failure surfaces here, once.
                let log = cedr_route::sync::log_path();
                if let Ok(text) = std::fs::read_to_string(&log) {
                    eprintln!("cedr: the last background sync failed: {}", text.trim());
                    let _ = std::fs::remove_file(&log);
                }
            }
            let Some(_lock) = cedr_route::sync::lock()? else {
                return if quiet {
                    Ok(ExitCode::SUCCESS)
                } else {
                    Err(Error::Config("another sync is running".into()))
                };
            };
            let result = sync(config, !pull_only, !push_only, quiet);
            match (&result, quiet) {
                (Err(e), true) => write_sync_log(&e.to_string()),
                (Ok(Some(warning)), true) => write_sync_log(warning),
                (Ok(None), true) => {
                    let _ = std::fs::remove_file(cedr_route::sync::log_path());
                }
                (Ok(Some(warning)), false) => eprintln!("cedr: {warning}"),
                _ => {}
            }
            result.map(|_| ExitCode::SUCCESS)
        }
    }
}

/// Captures Claude Code's status line input for the `claude-statusline` reader. A status line must
/// never fail or go blank, so problems are reported on stderr and the line is still printed.
fn statusline(harness: &str, command: &[String]) -> ExitCode {
    let input = read_limited(std::io::stdin().lock(), MAX_BYTES).unwrap_or_default();
    let doc = serde_json::from_slice::<Value>(&input)
        .ok()
        .and_then(|v| readers::from_statusline(&v, harness, Utc::now()));
    if let Some(d) = &doc
        && let Err(e) = readers::save_capture(&readers::statusline_path(harness), d)
    {
        eprintln!("cedr: cannot save the {harness} reading ({e})");
    }
    let Some((program, args)) = command.split_first() else {
        println!("{}", readers::statusline_text(harness, doc.as_ref()));
        return ExitCode::SUCCESS;
    };
    let run = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            if let Some(mut stdin) = child.stdin.take() {
                // A command that ignores its input closes the pipe early; that is not an error.
                let _ = stdin.write_all(&input);
            }
            child.wait_with_output()
        });
    match run {
        Ok(out) if out.status.success() && !out.stdout.is_empty() => {
            let _ = std::io::stdout().write_all(&out.stdout);
        }
        Ok(out) => {
            eprintln!(
                "cedr: status line command {program} exited with {} and printed {} bytes; showing Cedr's line",
                out.status,
                out.stdout.len()
            );
            println!("{}", readers::statusline_text(harness, doc.as_ref()));
        }
        Err(e) => {
            eprintln!("cedr: status line command {program} failed ({e})");
            println!("{}", readers::statusline_text(harness, doc.as_ref()));
        }
    }
    ExitCode::SUCCESS
}

/// Push, pull, then the machine snapshot. The snapshot is the app's view of this machine, not the
/// sync itself: its failure comes back as a warning and never fails the sync.
fn sync(config: Option<&Path>, push: bool, pull: bool, quiet: bool) -> Result<Option<String>> {
    let local = cedr_route::config::load_local(&cedr_route::config::locate(config, &cwd()?)?)?;
    let settings = local.sync.ok_or_else(|| {
        Error::Config("routes.toml has no [sync] section naming the organization".into())
    })?;
    let key = cedr_route::sync::api_key()?;
    let say = |line: String| {
        if !quiet {
            println!("{line}");
        }
    };
    if push {
        let pushed = cedr_route::sync::push(&settings, &key, &history::default_path())?;
        say(format!(
            "pushed {} records to {} ({} skipped)",
            pushed.accepted, settings.org, pushed.skipped
        ));
    }
    if pull {
        match cedr_route::sync::pull(&settings, &key)? {
            Pulled::Routes {
                path,
                rules,
                updated_at,
                updated_by,
            } => say(format!(
                "pulled the shared routes of {} to {} (rules {rules}, updated {updated_at} by {updated_by})",
                settings.org,
                path.display()
            )),
            Pulled::None => say(format!("{} has no shared routes", settings.org)),
            Pulled::NotOnPlan => say(format!(
                "{}'s plan does not include saved routes (Pro and up)",
                settings.org
            )),
        }
    }
    if !push {
        return Ok(None);
    }
    // The snapshot follows the pull, so it reports the routes this machine now uses.
    let org_routes = cedr_route::config::org_routes_path(&settings.org);
    if settings.routes == cedr_route::config::RoutesFrom::Org && !org_routes.exists() {
        say(format!(
            "no plan state sent: {} shares no routes for this machine to report on",
            settings.org
        ));
        return Ok(None);
    }
    let sent = open(config, None, None)
        .and_then(|session| cedr_route::sync::snapshot(&session, &settings))
        .and_then(|snapshot| {
            cedr_route::sync::send_machine(&settings, &key, &snapshot)?;
            Ok(snapshot)
        });
    match sent {
        Ok(snapshot) => {
            say(format!(
                "sent this machine's plan state to {} as {}",
                settings.org,
                snapshot["name"].as_str().unwrap_or_default()
            ));
            Ok(None)
        }
        Err(e) => Ok(Some(format!(
            "history synced, but the plan state was not sent: {e}"
        ))),
    }
}
