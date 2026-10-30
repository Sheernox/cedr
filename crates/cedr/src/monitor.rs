//! Monitor commands collect local counts before presenting or sending them.
use cedr_monitor::{self as monitor, Result, report::Options};
use clap::{Args, Subcommand};
use serde_json::Value;
use std::{
    path::PathBuf,
    process::{Command, ExitCode},
};
#[derive(Args)]
pub struct MonitorArgs {
    #[command(subcommand)]
    command: Option<MonitorCommand>,
    #[command(flatten)]
    report: ReportArgs,
}
#[derive(Args, Default)]
pub struct ReportArgs {
    /// Start of the report: 7d, 30d or a YYYY-MM-DD date; default 7d
    #[arg(long)]
    since: Option<String>,
    /// End of the report, exclusive: a YYYY-MM-DD date; default now
    #[arg(long)]
    until: Option<String>,
    /// One row per value of this; default model
    #[arg(long,value_parser=["harness","model","project","day","hour","session","effort"])]
    by: Option<String>,
    /// Only this agent (claude, codex, gemini, copilot, opencode, muse, antigravity)
    #[arg(long)]
    harness: Option<String>,
    /// Only this project, as the report names it
    #[arg(long)]
    project: Option<String>,
    /// Count days and hours in UTC instead of local time
    #[arg(long)]
    utc: bool,
    /// json prints the report for scripts and agents
    #[arg(long,default_value="text",value_parser=["text","json"])]
    format: String,
}
#[derive(Subcommand)]
enum MonitorCommand {
    /// Collect and report usage and API-equivalent cost (the default)
    Report(ReportArgs),
    /// The costliest sessions
    Sessions {
        /// Start: 7d, 30d or a YYYY-MM-DD date
        #[arg(long, default_value = "7d")]
        since: String,
        /// How many sessions to list
        #[arg(long, default_value_t = 15)]
        limit: usize,
        /// json prints them for scripts and agents
        #[arg(long,default_value="text",value_parser=["text","json"])]
        format: String,
    },
    /// Each plan window's use, reset, projection and outlook
    Limits {
        /// json prints them for scripts and agents
        #[arg(long,default_value="text",value_parser=["text","json"])]
        format: String,
    },
    /// What Monitor found for each agent on this machine, and why one has no data
    Sources {
        /// json prints them for scripts and agents
        #[arg(long,default_value="text",value_parser=["text","json"])]
        format: String,
    },
    /// Send hourly counts and plan readings to your cedr.dev organization
    Push {
        /// Organization ID; overrides monitor.toml and [sync] org, and reads no routes config
        #[arg(long)]
        org: Option<String>,
        /// routes.toml whose [sync] settings to use instead of the usual search
        #[arg(long)]
        config: Option<PathBuf>,
        /// Print nothing on success (for timers and background runs)
        #[arg(long)]
        quiet: bool,
        /// Send everything in the local store, not just what changed since the last push
        #[arg(long)]
        full: bool,
    },
    /// Install or remove a per-user timer that pushes on a schedule
    Schedule {
        /// Minutes between pushes
        #[arg(long, default_value_t = 60)]
        every: u32,
        /// Remove the timer instead of installing it
        #[arg(long)]
        remove: bool,
    },
    /// Delete the local usage store; logs remain available for recollection
    Reset,
}
pub fn run(a: MonitorArgs) -> std::result::Result<ExitCode, String> {
    execute(a)
        .map(|()| ExitCode::SUCCESS)
        .map_err(|e| format!("monitor: {e}"))
}
fn execute(a: MonitorArgs) -> Result<()> {
    match a.command {
        Some(MonitorCommand::Reset) => {
            let path = monitor::store::path();
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            println!("Monitor store removed");
        }
        Some(MonitorCommand::Schedule { every, remove }) => schedule(every, remove)?,
        command => {
            // A push settles its lock, organization and key before collecting, so a machine with nowhere to send
            // counts says so at once instead of after reading every log.
            let mut push_target = None;
            let mut _push_lock = None;
            if let Some(MonitorCommand::Push {
                org, config, quiet, ..
            }) = &command
            {
                let Some(lock) = cedr_route::sync::monitor_lock()? else {
                    if *quiet {
                        return Ok(());
                    }
                    return Err("another monitor push is running".into());
                };
                _push_lock = Some(lock);
                let c = monitor::config::Config::load()?;
                push_target = Some((
                    monitor::push::settings(org.as_deref(), config.as_deref(), &c)?,
                    cedr_route::sync::api_key()?,
                ));
            }
            let db = monitor::collect()?;
            match command {
                None | Some(MonitorCommand::Report(_)) => {
                    let r = match command {
                        Some(MonitorCommand::Report(r)) => r,
                        _ => a.report,
                    };
                    let v = monitor::report::report(
                        &db,
                        Options {
                            since: r.since.as_deref(),
                            until: r.until.as_deref(),
                            by: r.by.as_deref(),
                            harness: r.harness.as_deref(),
                            project: r.project.as_deref(),
                            utc: r.utc,
                        },
                    )?;
                    print_report(&v, &r.format);
                }
                Some(MonitorCommand::Sources { format }) => {
                    print_rows(&serde_json::json!(db.source_values()?), &format)
                }
                Some(MonitorCommand::Sessions {
                    since,
                    limit,
                    format,
                }) => print_rows(&monitor::report::sessions(&db, &since, limit)?, &format),
                Some(MonitorCommand::Limits { format }) => {
                    for e in monitor::limits::refresh(&db) {
                        eprintln!("cedr: {e}");
                    }
                    print_rows(&monitor::limits::report(&db)?, &format);
                }
                Some(MonitorCommand::Push {
                    org,
                    config,
                    quiet,
                    full,
                }) => {
                    let (sync, key) = push_target.take().expect("resolved before collecting");
                    let machine_config = if org.is_some() && config.is_none() {
                        None
                    } else {
                        config.clone().or_else(|| {
                            std::env::current_dir()
                                .ok()
                                .and_then(|cwd| cedr_route::config::locate(None, &cwd).ok())
                        })
                    };
                    let v = monitor::push::send(&db, &sync, &key, full, machine_config.as_deref())?;
                    if !quiet {
                        println!("accepted {} rows, skipped {}", v["accepted"], v["skipped"]);
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}
fn display(v: &Value) -> String {
    match v {
        Value::Null => "unknown".into(),
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M %:z")
                    .to_string()
            })
            .unwrap_or_else(|_| s.clone()),
        Value::Number(n) if n.as_u64().is_some() => {
            let raw = n.to_string();
            let parts = raw
                .as_bytes()
                .rchunks(3)
                .map(|b| std::str::from_utf8(b).expect("number"))
                .collect::<Vec<_>>();
            parts.into_iter().rev().collect::<Vec<_>>().join(",")
        }
        _ => v.to_string(),
    }
}
fn print_report(v: &Value, format: &str) {
    if format == "json" {
        println!("{}", serde_json::to_string_pretty(v).expect("JSON"));
        return;
    }
    println!("API-equivalent cost estimates; active time is estimated");
    println!(
        "{:<32} {:>10} {:>10} {:>12} {:>12}",
        "Group", "Responses", "Sessions", "Tokens", "Cost"
    );
    for g in std::iter::once(&v["totals"]).chain(v["groups"].as_array().into_iter().flatten()) {
        let tokens = [
            "input",
            "cacheRead",
            "cacheWrite5m",
            "cacheWrite1h",
            "output",
        ]
        .iter()
        .filter_map(|k| g[k].as_i64())
        .sum::<i64>();
        println!(
            "{:<32} {:>10} {:>10} {:>12} {:>12}",
            display(&g["key"]),
            display(&g["responses"]),
            display(&g["sessions"]),
            display(&serde_json::json!(tokens)),
            format!("${:.2}", g["costUsd"].as_f64().unwrap_or(0.0))
        );
    }
    for g in v["groups"].as_array().into_iter().flatten() {
        println!("{}:", display(&g["key"]));
        for keys in [
            [
                "input",
                "cacheWrite5m",
                "cacheWrite1h",
                "cacheRead",
                "output",
                "reasoning",
            ]
            .as_slice(),
            [
                "cacheHitRate",
                "netCacheSavingUsd",
                "toolCalls",
                "toolErrors",
                "apiErrors",
                "compactions",
                "activeMs",
                "meanWallMs",
            ]
            .as_slice(),
        ] {
            println!(
                "  {}",
                keys.iter()
                    .map(|k| format!(
                        "{k}={}",
                        if *k == "netCacheSavingUsd" {
                            g[k].as_f64()
                                .map(|n| format!("${n:.2}"))
                                .unwrap_or_else(|| "unknown".into())
                        } else {
                            display(&g[k])
                        }
                    ))
                    .collect::<Vec<_>>()
                    .join("  ")
            );
        }
    }
    let t = &v["totals"];
    if let Some(responses) = t["responses"].as_f64().filter(|n| *n > 0.0) {
        let errors = t["apiErrors"].as_f64().unwrap_or(0.0);
        println!(
            "{:<24} {:.2}% (event heuristic)",
            "Error-free share",
            (100.0 * (1.0 - errors / responses)).max(0.0)
        );
    }

    for k in [
        "input",
        "cacheWrite5m",
        "cacheWrite1h",
        "cacheRead",
        "output",
        "reasoning",
        "cacheHitRate",
        "netCacheSavingUsd",
        "toolCalls",
        "toolErrors",
        "apiErrors",
        "compactions",
        "activeMs",
        "meanWallMs",
    ] {
        let value = if k == "netCacheSavingUsd" {
            t[k].as_f64()
                .map(|n| format!("${n:.2}"))
                .unwrap_or_else(|| "unknown".into())
        } else {
            display(&t[k])
        };
        println!("{k:<24} {value}");
    }
    if v["unpriced"].as_array().is_some_and(|a| !a.is_empty()) {
        println!("Unpriced: {}", v["unpriced"]);
    }
}
fn money(v: &Value) -> String {
    v.as_f64()
        .map(|n| format!("${n:.2}"))
        .unwrap_or_else(|| "unknown".into())
}
fn print_rows(v: &Value, format: &str) {
    if format == "json" {
        println!("{}", serde_json::to_string_pretty(v).expect("JSON"));
        return;
    }
    let rows = v.as_array().map(Vec::as_slice).unwrap_or_default();
    if rows.first().is_some_and(|r| r.get("state").is_some()) {
        println!(
            "{:<14} {:<13} {:>7} {:>12} {:>10}",
            "Harness", "State", "Files", "Responses", "Malformed"
        );
        for r in rows {
            println!(
                "{:<14} {:<13} {:>7} {:>12} {:>10}",
                display(&r["harness"]),
                display(&r["state"]),
                display(&r["files"]),
                display(&r["responses"]),
                display(&r["malformed"])
            );
            if !r["first"].is_null() {
                println!("  {} to {}", display(&r["first"]), display(&r["last"]));
            }
            if !r["message"].is_null() {
                println!("  {}", display(&r["message"]));
            }
        }
    } else if rows.first().is_some_and(|r| r.get("percent").is_some()) {
        println!(
            "{:<12} {:<26} {:>8} {:>12} {:<12}",
            "Harness", "Window", "Used %", "Projected %", "Outlook"
        );
        for r in rows {
            let percent = |k: &str| {
                r[k].as_f64()
                    .map(|n| format!("{n:.2}"))
                    .unwrap_or_else(|| "unknown".into())
            };
            println!(
                "{:<12} {:<26} {:>8} {:>12} {:<12}",
                display(&r["harness"]),
                display(&r["window"]),
                percent("percent"),
                percent("projectedPercent"),
                display(&r["outlook"])
            );
            println!(
                "  reset={} elapsed={} runout={}",
                display(&r["resetsAt"]),
                display(&r["elapsedShare"]),
                display(&r["runsOutAt"])
            );
        }
    } else {
        println!(
            "{:<14} {:<24} {:>10} {:>14} {:>12}",
            "Harness", "Project", "Responses", "Tokens", "Cost"
        );
        for r in rows {
            println!(
                "{:<14} {:<24} {:>10} {:>14} {:>12}",
                display(&r["harness"]),
                display(&r["project"]),
                display(&r["responses"]),
                display(&r["tokens"]),
                money(&r["costUsd"])
            );
            println!(
                "  {} {} to {} models={}",
                display(&r["session"]),
                display(&r["first"]),
                display(&r["last"]),
                display(&r["models"])
            );
        }
    }
}
fn checked(c: &mut Command) -> Result<()> {
    let status = c.status()?;
    if !status.success() {
        return Err(format!("scheduler command failed ({status})").into());
    }
    Ok(())
}
fn schedule(every: u32, remove: bool) -> Result<()> {
    let os = std::env::consts::OS;
    let exe = std::env::current_exe()?;
    let files = monitor::schedule::files(os, &exe.to_string_lossy(), every)?;
    let home = PathBuf::from(
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .ok_or("home directory unavailable")?,
    );
    if remove {
        match os {
            "linux" => checked(Command::new("systemctl").args([
                "--user",
                "disable",
                "--now",
                "cedr-monitor.timer",
            ]))?,
            "macos" => checked(
                Command::new("launchctl")
                    .arg("unload")
                    .arg(home.join(&files[0].relative)),
            )?,
            "windows" => {
                checked(Command::new("schtasks").args(["/Delete", "/TN", "cedr monitor", "/F"]))?
            }
            _ => {}
        }
    }
    for f in &files {
        let path = home.join(&f.relative);
        if remove {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        } else {
            std::fs::create_dir_all(path.parent().ok_or("missing schedule directory")?)?;
            std::fs::write(&path, &f.contents)?;
        }
        println!(
            "{} {}",
            if remove { "removed" } else { "wrote" },
            path.display()
        );
    }
    if !remove {
        match os {
            "linux" => {
                checked(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
                checked(Command::new("systemctl").args([
                    "--user",
                    "enable",
                    "--now",
                    "cedr-monitor.timer",
                ]))?;
                println!("Check: systemctl --user status cedr-monitor.timer");
            }
            "macos" => {
                checked(
                    Command::new("launchctl")
                        .arg("load")
                        .arg(home.join(&files[0].relative)),
                )?;
                println!("Check: launchctl list dev.cedr.monitor");
            }
            "windows" => {
                let task = home.join(&files[0].relative);
                checked(
                    Command::new("schtasks")
                        .args(["/Create", "/TN", "cedr monitor", "/XML"])
                        .arg(&task)
                        .arg("/F"),
                )?;
                // Task Scheduler keeps its own copy; the XML was only the way in.
                if let Err(e) = std::fs::remove_file(&task) {
                    eprintln!("cedr: warning: could not remove {}: {e}", task.display());
                }
                println!("Check: schtasks /Query /TN \"cedr monitor\"");
            }
            _ => {}
        }
    }
    Ok(())
}
