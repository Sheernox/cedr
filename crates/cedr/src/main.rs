//! `cedr`: sync agent configuration, diagnose setup, monitor local usage, route work to AI coding agents (`cedr route`, `cedr mcp`), and lint, lock and
//! verify the skills and MCP servers they load. Exit codes: 0 clean (or delegate), 1 findings,
//! changes (or keep the work), 2 usage or I/O failure.

mod common;
mod council;
mod doctor;
mod mcp;
mod monitor;
mod push;
mod route;
mod sync;

use std::path::PathBuf;
use std::process::ExitCode;

use cedr_core::finding::Report;
use cedr_core::lock::{self, LockOptions};
use cedr_core::source::Source;
use cedr_core::surface::{self, Surface};
use cedr_core::{diff, lint, verify};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};

const EXAMPLES: &str = "\
Examples:
  cedr sync init && cedr sync          import and compile agent setup
  cedr doctor --format json            diagnose this machine and repo
  cedr monitor --since 7d              report local API-equivalent usage
  cedr route pick review.code            pick the agent for a kind of work
  cedr route verdict <pick id> accepted  record how it went
  cedr council plan review.council       name the reviewers, one per vendor
  cedr council merge a=a.md b=b.md --route review.council
  cedr lint && cedr lock && cedr verify  pin the skills and MCP servers a repo loads
  cedr outdated --fail-on high          inspect newer npm server surfaces
  cedr push --updates                   report pins, findings and available updates
  claude mcp add cedr -- cedr mcp        give an agent the route and council tools

Start with a routes.toml: https://cedr.dev/docs/route (the app can generate one).";

#[derive(Parser)]
#[command(
    name = "cedr",
    version,
    about = "Route work to AI coding agents, and pin the skills and MCP servers they load",
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
    /// Serve the route and council tools over MCP on stdio
    Mcp {
        /// routes.toml to use instead of the usual search
        #[arg(long)]
        config: Option<PathBuf>,
    },
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
    /// Send this repo's pinned inventory and findings to the cedr.dev app
    Push(push::PushArgs),
    /// Print a shell completion script, e.g. `cedr completions zsh > ~/.zfunc/_cedr`
    Completions { shell: clap_complete::Shell },
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

fn main() -> ExitCode {
    // Like other Unix tools, end quietly when the reader goes away (`cedr route history | head`)
    // instead of panicking on a closed stdout.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let outcome = match Cli::parse().command {
        Command::Route { config, command } => {
            route::run(command, &config).map_err(|e| e.to_string())
        }
        Command::Council { config, command } => {
            council::run(command, &config).map_err(|e| e.to_string())
        }
        Command::Mcp { config } => mcp::serve(config)
            .map(|()| ExitCode::SUCCESS)
            .map_err(|e| format!("mcp: {e}")),
        Command::Sync(args) => sync::run(args),
        Command::Doctor(args) => doctor::run(args),
        Command::Push(args) => push::run(&args),
        Command::Monitor(args) => monitor::run(args),
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "cedr", &mut std::io::stdout());
            Ok(ExitCode::SUCCESS)
        }
        command => run(command).map_err(|e| e.to_string()),
    };
    match outcome {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cedr: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(command: Command) -> cedr_core::Result<ExitCode> {
    match command {
        Command::Sync(_)
        | Command::Doctor(_)
        | Command::Route { .. }
        | Command::Council { .. }
        | Command::Mcp { .. }
        | Command::Monitor(_)
        | Command::Push(_)
        | Command::Completions { .. } => {
            unreachable!("handled in main")
        }
        Command::Lint {
            path,
            format,
            strict,
        } => {
            let report = lint::lint(&path)?;
            print_report(&report, format);
            let failed = report.has_errors() || (strict && !report.is_empty());
            Ok(exit(failed))
        }
        Command::Lock {
            path,
            offline,
            introspect,
        } => {
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
                },
            )?;
            for warning in &outcome.warnings {
                eprintln!("cedr: warning: {warning}");
            }
            print_skipped(&outcome.skipped);
            if outcome.moved {
                println!("moved {} to {}", lock::LEGACY_LOCK_FILE, lock::LOCK_FILE);
            }
            println!(
                "wrote {}: {} skills, {} MCP servers",
                path.join(lock::LOCK_FILE).display(),
                outcome.lock.skill.len(),
                outcome.lock.mcp.len()
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Verify { path, format } => {
            let verification = verify::verify(&path)?;
            print_skipped(&verification.skipped);
            print_report(&verification.report, format);
            Ok(exit(!verification.report.is_empty()))
        }
        Command::Outdated {
            path,
            format,
            fail_on,
        } => {
            let report = cedr_core::outdated::outdated(&path)?;
            Ok(print_updates(&report, format, fail_on))
        }
        Command::Surface { source } => {
            let parsed: Source = source.parse()?;
            println!("{}", surface::from_source(&parsed)?.to_json());
            Ok(ExitCode::SUCCESS)
        }
        Command::Latest { source } => {
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
        Command::Diff { old, new, format } => {
            let result = diff::diff(&Surface::read(&old)?, &Surface::read(&new)?);
            match format {
                Format::Json => println!("{}", result.to_json()),
                Format::Text => print!("{}", result.to_text()),
            }
            Ok(exit(!result.hashes_match))
        }
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
                    "outdated",
                    "--fail-on",
                    flag,
                    "--format",
                    "json",
                ])
                .unwrap();
                let Command::Outdated {
                    format, fail_on, ..
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
