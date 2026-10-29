//! `cedr`: route work to AI coding agents (`cedr route`, `cedr mcp`), and lint, lock and
//! verify the skills and MCP servers they load. Exit codes: 0 clean (or delegate), 1 findings,
//! changes (or keep the work), 2 usage or I/O failure.

mod council;
mod mcp;
mod route;

use std::path::PathBuf;
use std::process::ExitCode;

use cedr_core::finding::Report;
use cedr_core::lock::{self, LockOptions};
use cedr_core::source::Source;
use cedr_core::surface::{self, Surface};
use cedr_core::{diff, lint, verify};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "cedr",
    version,
    about = "Route work to AI coding agents, and pin the skills and MCP servers they load"
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

#[derive(Subcommand)]
enum Command {
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
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
        /// Fail on warnings too
        #[arg(long)]
        strict: bool,
    },
    /// Pin every skill and MCP server in cedr.lock and snapshot server surfaces
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
    /// Fail when the setup no longer matches cedr.lock (never uses the network)
    Verify {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
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
        Command::Route { .. } | Command::Council { .. } | Command::Mcp { .. } => {
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
            println!(
                "wrote {}: {} skills, {} MCP servers",
                path.join(lock::LOCK_FILE).display(),
                outcome.lock.skill.len(),
                outcome.lock.mcp.len()
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Verify { path, format } => {
            let report = verify::verify(&path)?;
            print_report(&report, format);
            Ok(exit(!report.is_empty()))
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

fn print_report(report: &Report, format: Format) {
    match format {
        Format::Json => println!("{}", report.to_json()),
        Format::Text => print!("{}", report.to_text()),
    }
}

fn exit(failed: bool) -> ExitCode {
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
