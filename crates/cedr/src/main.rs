//! `cedr`: sync agent configuration, diagnose setup, monitor local usage, route work to AI coding agents (`cedr route`, `cedr mcp`), and lint, lock and
//! verify the skills and MCP servers they load. Exit codes: 0 clean (or delegate), 1 findings,
//! changes (or keep the work), 2 usage or I/O failure.

mod bench;
mod common;
mod council;
mod doctor;
mod mcp;
mod monitor;
mod net;
mod push;
mod route;
mod settings;
mod sync;
mod telemetry;
mod update;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use cedr_core::finding::Report;
use cedr_core::lock::{self, LockOptions};
use cedr_core::source::Source;
use cedr_core::surface::{self, Surface};
use cedr_core::{diff, lint, verify};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};

const EXAMPLES: &str = "\
Examples:
  cedr sync init && cedr sync          import and compile agent setup
  cedr doctor --format json            diagnose this machine and repo
  cedr monitor --since 7d              report local API-equivalent usage
  cedr route pick review.code            pick the agent for a kind of work
  cedr route verdict <pick id> accepted  record how it went
  cedr council plan review.council       name the reviewers, one per vendor
  cedr council merge a=a.md b=b.md --route review.council
  cedr guard lint && cedr guard lock    pin the skills and MCP servers a repo loads
  cedr guard verify                     fail when the setup no longer matches the lock
  cedr guard outdated --fail-on high    inspect newer npm server surfaces
  cedr guard push --updates             report pins, findings and available updates
  cedr bench run --budget-usd 5          measure a change with a task suite
  claude mcp add cedr -- cedr mcp        give an agent cedr's tools
  cedr update                           install the latest release
  cedr telemetry off                    stop sending anonymous usage data

Start with a routes.toml: https://cedr.dev/docs/route (the app can generate one).";

#[derive(Parser)]
#[command(
    name = "cedr",
    version,
    about = "Route, review, pin, sync, monitor and bench the work of AI coding agents",
    after_help = EXAMPLES
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
}

/// Formats for lint and verify findings; `github` prints workflow annotations.
#[derive(Clone, Copy, ValueEnum)]
enum ReportFormat {
    Text,
    Json,
    Github,
}

#[derive(Clone, Copy, ValueEnum)]
enum FailOn {
    High,
    Medium,
    Low,
    Never,
}

impl FailOn {
    fn threshold(self) -> Option<cedr_core::diff::ChangeSeverity> {
        use cedr_core::diff::ChangeSeverity;
        match self {
            Self::High => Some(ChangeSeverity::High),
            Self::Medium => Some(ChangeSeverity::Medium),
            Self::Low => Some(ChangeSeverity::Low),
            Self::Never => None,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Compile and check shared agent configuration
    Sync(sync::Args),
    /// Diagnose the local machine and repository without changing them
    Doctor(doctor::Args),
    /// Collect local coding-agent usage and report API-equivalent costs
    Monitor(monitor::MonitorArgs),
    /// Pick the harness and model for each kind of work, and record verdicts
    Route {
        #[command(flatten)]
        config: route::ConfigArg,
        #[command(subcommand)]
        command: route::RouteCommand,
    },
    /// Plan a multi-vendor review and merge its reports into one gate
    Council {
        #[command(flatten)]
        config: route::ConfigArg,
        #[command(subcommand)]
        command: council::CouncilCommand,
    },
    /// Lint, pin and verify the skills and MCP servers your agents load
    Guard {
        #[command(subcommand)]
        command: GuardCommand,
    },
    /// Measure whether an agent configuration, skill or model change helps
    Bench {
        #[command(subcommand)]
        command: bench::BenchCommand,
    },
    /// Serve the route, council, sync, monitor, bench and doctor tools over MCP on stdio
    Mcp {
        /// routes.toml to use instead of the usual search
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Update cedr to the latest release, or turn the daily automatic update on or off
    Update(update::Args),
    /// Show or change whether cedr sends anonymous usage data
    Telemetry {
        #[command(subcommand)]
        command: telemetry::TelemetryCommand,
    },
    /// Print a shell completion script, e.g. `cedr completions zsh > ~/.zfunc/_cedr`
    Completions { shell: clap_complete::Shell },
    /// Guard's commands under their old names, hidden by `cli_command()`.
    #[command(flatten)]
    LegacyGuard(GuardCommand),
}

/// Guard's commands, under `cedr guard` like every other product's.
#[derive(Subcommand)]
enum GuardCommand {
    /// Find problems in a repo's agent setup
    Lint {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// github prints GitHub Actions annotations
        #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
        format: ReportFormat,
        /// Fail on warnings too
        #[arg(long)]
        strict: bool,
    },
    /// Pin every skill and MCP server in .cedr/guard.lock and snapshot server surfaces
    Lock {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Skip the network; servers are recorded with method "none"
        #[arg(long)]
        offline: bool,
        /// Run each stdio server and read its tools over MCP (executes them)
        #[arg(long)]
        introspect: bool,
        /// Read each remote server's tools over MCP without credentials (contacts its URL)
        #[arg(long, conflicts_with = "offline")]
        remote: bool,
    },
    /// Fail when the setup no longer matches the lock (never uses the network)
    Verify {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// github prints GitHub Actions annotations
        #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
        format: ReportFormat,
    },
    /// Inspect newer npm server versions without installing or running packages
    Outdated {
        /// Repo root containing the Guard lock
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Output format; github prints workflow annotations
        #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
        format: ReportFormat,
        /// Fail when an update reaches this surface-change severity
        #[arg(long, value_enum, default_value_t = FailOn::Never)]
        fail_on: FailOn,
    },
    /// Compare what locked remote MCP servers list now with the lock
    Remote {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Read now even when the last read is within remote.interval_minutes
        #[arg(long, conflicts_with = "last")]
        force: bool,
        /// Print the last results without contacting anything
        #[arg(long)]
        last: bool,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Send this repo's pinned inventory and findings to the cedr.dev app
    Push(push::PushArgs),
    /// Print the surface of npm:<pkg>@<ver> or file:<path-to.tgz> as JSON
    Surface { source: String },
    /// Print the registry's latest version of an npm source as JSON
    Latest { source: String },
    /// Compare two surface documents
    Diff {
        old: PathBuf,
        new: PathBuf,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
}

/// Guard's commands sat at the top level until 0.6.0. They still run, so existing CI and scripts keep
/// working and the GitHub Action can use older pinned releases, but help and completions list only `cedr guard`.
const LEGACY_GUARD: [&str; 8] = [
    "lint", "lock", "verify", "outdated", "push", "surface", "latest", "diff",
];

fn cli_command() -> clap::Command {
    LEGACY_GUARD.into_iter().fold(Cli::command(), |cmd, name| {
        cmd.mut_subcommand(name, |sub| sub.hide(true))
    })
}

fn main() -> ExitCode {
    // Like other Unix tools, end quietly when the reader goes away (`cedr route history | head`)
    // instead of panicking on a closed stdout.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let started = Instant::now();
    let matches = cli_command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let path = command_path(&matches);
    let background = match &cli.command {
        Command::Update(args) => args.background(),
        Command::Telemetry { command } => {
            matches!(command, telemetry::TelemetryCommand::Flush)
        }
        _ => false,
    };
    let quiet = telemetry::silent(&path, background);
    if !quiet {
        if !settings::Settings::load().valid {
            eprintln!(
                "cedr: warning: {} cannot be read or does not parse, so telemetry and automatic updates are off until it is fixed",
                settings::path().display()
            );
        }
        update::announce();
        if !matches!(cli.command, Command::Update(_)) {
            update::auto_start();
        }
    }
    let code = dispatch(cli.command);
    if !quiet {
        telemetry::record(&path, exit_number(&code), started.elapsed());
    }
    code
}

/// The subcommand path, at most two words (`route pick`), for usage events.
fn command_path(matches: &clap::ArgMatches) -> String {
    let mut parts = Vec::new();
    let mut current = matches;
    while let Some((name, sub)) = current.subcommand() {
        parts.push(name);
        if parts.len() == 2 {
            break;
        }
        current = sub;
    }
    parts.join(" ")
}

fn exit_number(code: &ExitCode) -> u8 {
    (0..=u8::MAX)
        .find(|n| ExitCode::from(*n) == *code)
        .unwrap_or(2)
}

fn dispatch(command: Command) -> ExitCode {
    let outcome = match command {
        Command::Route { config, command } => {
            route::run(command, &config).map_err(|e| e.to_string())
        }
        Command::Council { config, command } => {
            council::run(command, &config).map_err(|e| e.to_string())
        }
        Command::Bench { command } => bench::run(command),
        Command::Mcp { config } => mcp::serve(config)
            .map(|()| ExitCode::SUCCESS)
            .map_err(|e| format!("mcp: {e}")),
        Command::Sync(args) => sync::run(args),
        Command::Doctor(args) => doctor::run(args),
        Command::Monitor(args) => monitor::run(args),
        Command::Update(args) => update::run(args),
        Command::Telemetry { command } => telemetry::run(command),
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut cli_command(), "cedr", &mut std::io::stdout());
            Ok(ExitCode::SUCCESS)
        }
        Command::Guard { command } | Command::LegacyGuard(command) => match command {
            GuardCommand::Push(args) => push::run(&args),
            command => run(command).map_err(|e| e.to_string()),
        },
    };
    match outcome {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cedr: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(command: GuardCommand) -> cedr_core::Result<ExitCode> {
    match command {
        GuardCommand::Push(_) => unreachable!("handled in dispatch"),
        GuardCommand::Lint {
            path,
            format,
            strict,
        } => {
            let report = lint::lint(&path)?;
            print_report(&report, format);
            let failed = report.has_errors() || (strict && !report.is_empty());
            Ok(exit(failed))
        }
        GuardCommand::Lock {
            path,
            offline,
            introspect,
            remote,
        } => {
            if remote {
                eprintln!(
                    "cedr: warning: --remote contacts every remote MCP server in the config, without credentials"
                );
            }
            if introspect {
                eprintln!(
                    "cedr: warning: --introspect runs every configured stdio MCP server with its command and env"
                );
            }
            let outcome = lock::lock(
                &path,
                LockOptions {
                    offline,
                    introspect,
                    remote,
                },
            )?;
            for warning in &outcome.warnings {
                eprintln!("cedr: warning: {warning}");
            }
            print_skipped(&outcome.skipped);
            if outcome.moved {
                println!("moved {} to {}", lock::LEGACY_LOCK_FILE, lock::LOCK_FILE);
            }
            telemetry::count("guard_skills", outcome.lock.skill.len());
            telemetry::count("guard_servers", outcome.lock.mcp.len());
            println!(
                "wrote {}: {} skills, {} MCP servers",
                path.join(lock::LOCK_FILE).display(),
                outcome.lock.skill.len(),
                outcome.lock.mcp.len()
            );
            Ok(ExitCode::SUCCESS)
        }
        GuardCommand::Verify { path, format } => {
            let verification = verify::verify(&path)?;
            print_skipped(&verification.skipped);
            print_report(&verification.report, format);
            Ok(exit(!verification.report.is_empty()))
        }
        GuardCommand::Outdated {
            path,
            format,
            fail_on,
        } => {
            let report = cedr_core::outdated::outdated(&path)?;
            Ok(print_updates(&report, format, fail_on))
        }
        GuardCommand::Remote {
            path,
            force,
            last,
            format,
        } => guard_remote(&path, force, last, format),
        GuardCommand::Surface { source } => {
            let parsed: Source = source.parse()?;
            println!("{}", surface::from_source(&parsed)?.to_json());
            Ok(ExitCode::SUCCESS)
        }
        GuardCommand::Latest { source } => {
            let parsed: Source = source.parse()?;
            let latest = surface::latest(&parsed)?;
            let quote = |s: &str| serde_json::to_string(s).expect("string serializes");
            println!(
                "{{\"source\":{},\"latest\":{}}}",
                quote(&source),
                quote(&latest.to_string())
            );
            Ok(ExitCode::SUCCESS)
        }
        GuardCommand::Diff { old, new, format } => {
            let result = diff::diff(&Surface::read(&old)?, &Surface::read(&new)?);
            match format {
                Format::Json => println!("{}", result.to_json()),
                Format::Text => print!("{}", result.to_text()),
            }
            Ok(exit(!result.hashes_match))
        }
    }
}

/// Runs the check, or repeats the last results while they are inside the configured interval, so
/// however often a scheduler calls this, servers are read at a bounded rate.
fn guard_remote(
    path: &Path,
    force: bool,
    last: bool,
    format: Format,
) -> cedr_core::Result<ExitCode> {
    use cedr_core::remote::Report;
    let state = settings::state_file(&format!(
        "guard-remote/{}.json",
        cedr_core::remote::state_key(path)?
    ));
    let saved: Option<Report> = std::fs::read(&state)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let age = |r: &Report| {
        chrono::DateTime::parse_from_rfc3339(&r.at)
            .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_minutes())
            .ok()
    };
    let interval = i64::from(cedr_core::config::GuardConfig::load(path)?.remote_interval);
    let baseline = if last {
        String::new()
    } else {
        cedr_core::remote::baseline(path)?
    };
    let report = match saved {
        Some(r) if last => r,
        None if last => {
            return Err(cedr_core::Error::Source(
                "no remote results yet: run cedr guard remote".into(),
            ));
        }
        Some(r)
            if !force
                && r.baseline == baseline
                && age(&r).is_some_and(|a| (0..interval).contains(&a)) =>
        {
            eprintln!(
                "cedr: last read {}, within remote.interval_minutes ({interval}); --force reads now",
                r.at
            );
            r
        }
        saved => {
            let claim = RemoteClaim::take(&state).map_err(|e| {
                cedr_core::Error::Source(format!(
                    "cannot record the read in {} ({e})",
                    state.display()
                ))
            })?;
            let Some(_claim) = claim else {
                return match saved {
                    Some(r) => {
                        eprintln!(
                            "cedr: another read is running; showing the one from {}",
                            r.at
                        );
                        print_remote(&r, format);
                        Ok(ExitCode::from(r.exit_code()))
                    }
                    None => Err(cedr_core::Error::Source(
                        "another read is running, with no earlier results yet".into(),
                    )),
                };
            };
            let report = cedr_core::remote::check(
                path,
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            )?;
            telemetry::count("guard_remote_servers", report.servers.len());
            let write = state
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| {
                    std::fs::write(
                        &state,
                        serde_json::to_vec(&report).expect("report serializes"),
                    )
                });
            if let Err(e) = write {
                eprintln!("cedr: warning: cannot save remote results ({e})");
            }
            report
        }
    };
    print_remote(&report, format);
    Ok(ExitCode::from(report.exit_code()))
}

fn print_remote(report: &cedr_core::remote::Report, format: Format) {
    match format {
        Format::Json => println!(
            "{}",
            serde_json::to_string(report).expect("report serializes")
        ),
        Format::Text => print!("{}", report.to_text()),
    }
}

/// One read per checkout at a time, so overlapping scheduled runs can't multiply requests. The file
/// goes when the run ends; a crashed run's is taken over once older than the longest run can be
/// (64 servers, 22 requests each, 20 seconds a request).
struct RemoteClaim(PathBuf);

impl RemoteClaim {
    const STALE: Duration = Duration::from_secs(8 * 60 * 60);

    /// `None` when another run holds the claim; a failure to make it is an error, never a false
    /// "another read is running" that repeats old results.
    fn take(state: &Path) -> std::io::Result<Option<RemoteClaim>> {
        let path = state.with_extension("lock");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let stale = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Self::STALE);
        if stale {
            std::fs::remove_file(&path)?;
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => Ok(Some(RemoteClaim(path))),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl Drop for RemoteClaim {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Warnings remain advisory; only observed surface changes can trigger the chosen gate.
fn print_updates(
    report: &cedr_core::outdated::Report,
    format: ReportFormat,
    fail_on: FailOn,
) -> ExitCode {
    for warning in &report.warnings {
        eprintln!("cedr: warning: {}", warning.escape_debug());
    }
    match format {
        ReportFormat::Text => print!("{}", report.to_text()),
        ReportFormat::Json => println!("{}", report.to_json()),
        ReportFormat::Github => print!("{}", report.to_github()),
    }
    exit(
        report
            .updates
            .iter()
            .any(|u| u.severity.reaches(fail_on.threshold())),
    )
}

fn print_report(report: &Report, format: ReportFormat) {
    match format {
        ReportFormat::Json => println!("{}", report.to_json()),
        ReportFormat::Text => print!("{}", report.to_text()),
        ReportFormat::Github => print!("{}", report.to_github()),
    }
}

/// Skipped paths come from the repo, so control characters are escaped: a crafted name must
/// never start a line a CI runner reads as a workflow command.
fn print_skipped(skipped: &[String]) {
    for path in skipped {
        eprintln!(
            "cedr: skipped {}: links outside the repo",
            path.escape_debug()
        );
    }
}

fn exit(failed: bool) -> ExitCode {
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;
    use cedr_core::{
        discovery::Client,
        outdated::{Report, Update, UpdateSeverity},
    };

    #[test]
    fn outdated_flags_gate_updates_but_never_registry_warnings() {
        for (severity, expected) in [
            (UpdateSeverity::High, [1, 1, 1, 0]),
            (UpdateSeverity::Medium, [0, 1, 1, 0]),
            (UpdateSeverity::Low, [0, 0, 1, 0]),
            (UpdateSeverity::None, [0, 0, 0, 0]),
        ] {
            let report = Report {
                version: 1,
                updates: vec![Update {
                    client: Client::Copilot,
                    name: "example".into(),
                    source: "npm:example@1.0.0".into(),
                    latest: "npm:example@1.1.0".into(),
                    severity,
                    changes: vec![],
                }],
                warnings: vec!["copilot.broken: registry unavailable".into()],
                not_checked: vec![],
            };
            for (flag, expected) in ["high", "medium", "low", "never"].into_iter().zip(expected) {
                let cli = Cli::try_parse_from([
                    "cedr",
                    "guard",
                    "outdated",
                    "--fail-on",
                    flag,
                    "--format",
                    "json",
                ])
                .unwrap();
                let Command::Guard {
                    command:
                        GuardCommand::Outdated {
                            format, fail_on, ..
                        },
                } = cli.command
                else {
                    panic!("expected outdated")
                };
                assert_eq!(
                    print_updates(&report, format, fail_on),
                    ExitCode::from(expected)
                );
            }
        }
        let report = Report {
            version: 1,
            updates: vec![],
            warnings: vec!["copilot.broken: registry unavailable".into()],
            not_checked: vec![],
        };
        assert_eq!(
            print_updates(&report, ReportFormat::Github, FailOn::Low),
            ExitCode::SUCCESS
        );
    }
}
