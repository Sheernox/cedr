//! `cedr council ...`: plan a multi-vendor review and merge the members' reports into one gate
//! (docs/council.md). Exit codes: plan 0 quorum reachable, 3 below it, 1 no member; merge 0 pass,
//! 1 block, 3 incomplete; 2 usage, config or I/O failure.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cedr_route::council::{self, Gate, Merged, Next, Report};
use cedr_route::readings::{MAX_BYTES, read_limited};
use cedr_route::session::Session;
use cedr_route::{Error, Result};
use clap::Subcommand;
use serde_json::Value;

use crate::route::{self, PickFormat, SizeArg, SourceArgs, cwd, open, print_pick, record};

#[derive(Subcommand)]
pub enum CouncilCommand {
    /// Name the members to start for a council route
    Plan {
        route: String,
        #[arg(long, value_enum, default_value_t = SizeArg::Medium)]
        size: SizeArg,
        /// Harness to skip; repeatable
        #[arg(long)]
        avoid: Vec<String>,
        #[arg(long, value_enum, default_value_t = PickFormat::Text)]
        format: PickFormat,
        #[command(flatten)]
        sources: SourceArgs,
    },
    /// Recorded council outcomes per route
    Stats {
        #[arg(long)]
        route: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum, default_value_t = route::ListFormat::Text)]
        format: route::ListFormat,
    },
    /// Merge the members' reports and decide the gate
    Merge {
        /// <path> (JSON) or <member>=<path> (text or JSON); `-` reads stdin
        #[arg(required = true)]
        reports: Vec<String>,
        /// Council route whose quorum and blocking severity apply
        #[arg(long)]
        route: Option<String>,
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        no_record: bool,
        /// Quorum to use instead of the route's (default 2)
        #[arg(long)]
        quorum: Option<usize>,
        /// Least severe finding that blocks, instead of the route's (default P1)
        #[arg(long, value_parser = ["P0", "P1", "P2", "P3"])]
        block: Option<String>,
        /// Revision rounds instead of the route's (default 1)
        #[arg(long)]
        rounds: Option<u32>,
        /// The review being merged: 1 for the first, 2 after one revision
        #[arg(long)]
        round: Option<u32>,
        #[arg(long, value_enum, default_value_t = MergeFormat::Text)]
        format: MergeFormat,
    },
}

/// Merge output: the gate summary, its JSON, or only the author's revision brief.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum MergeFormat {
    Text,
    Json,
    Brief,
}

/// One report argument: a JSON report, or text whose member the argument names. An argument that is
/// an existing file is a path even if it contains `=`.
fn read_report(arg: &str) -> Result<Report> {
    let (member, path) = if arg == "-" || Path::new(arg).exists() {
        (None, PathBuf::from(arg))
    } else {
        match arg.split_once('=') {
            Some((m, p)) if !m.is_empty() => (Some(m), PathBuf::from(p)),
            _ => (None, PathBuf::from(arg)),
        }
    };
    let io = |source| Error::Io {
        path: path.clone(),
        source,
    };
    let bytes = if path == Path::new("-") {
        read_limited(std::io::stdin().lock(), MAX_BYTES).map_err(io)?
    } else {
        read_limited(std::fs::File::open(&path).map_err(io)?, MAX_BYTES).map_err(io)?
    };
    if bytes.len() > MAX_BYTES {
        return Err(Error::Usage(format!(
            "{}: report exceeds {MAX_BYTES} bytes",
            path.display()
        )));
    }
    // Anything shaped like JSON must parse as a report: a truncated JSON review read as text would
    // lose its findings and could pass the gate.
    let looks_json = bytes
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|b| matches!(b, b'{' | b'['));
    if looks_json {
        let doc: Value = serde_json::from_slice(&bytes).map_err(|e| Error::Parse {
            path: path.clone(),
            message: format!("not a valid JSON report ({e})"),
        })?;
        return Report::from_json(doc, member);
    }
    let member = member.ok_or_else(|| {
        Error::Usage(format!(
            "{}: a text report needs <member>=<path>",
            path.display()
        ))
    })?;
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::Usage(format!("{}: report is not UTF-8 text", path.display())))?;
    Ok(Report::from_text(&text, member))
}

pub fn print_merged(m: &Merged, format: MergeFormat) {
    match format {
        MergeFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(m).expect("merge serializes")
            );
            return;
        }
        MergeFormat::Brief => {
            match &m.brief {
                Some(b) => print!("{b}"),
                None => eprintln!("cedr: no revision brief: the council's next step is done"),
            }
            return;
        }
        MergeFormat::Text => {}
    }
    println!(
        "council: {} ({} of quorum {} reviewed, {:?} and above block: {})",
        m.gate.as_str(),
        m.reviewers.len(),
        m.quorum,
        m.block,
        m.reviewers.join(", ")
    );
    for g in &m.groups {
        println!(
            "{:?} {}:{} [{}] {}",
            g.severity,
            g.file,
            g.line,
            g.members.join(", "),
            g.headline()
        );
        if g.findings.len() > 1 {
            for f in &g.findings {
                println!("  {} {:?} :{} {}", f.member, f.severity, f.line, f.claim);
            }
        }
    }
    match m.next {
        Next::Revise => println!(
            "next: revise (round {} of {}): give the author the revision brief (--format brief), then review again with --round {}",
            m.round,
            m.rounds,
            m.round + 1
        ),
        Next::Done => println!("next: done"),
    }
}

pub fn gate_code(gate: Gate) -> ExitCode {
    match gate {
        Gate::Pass => ExitCode::SUCCESS,
        Gate::Block => ExitCode::from(1),
        Gate::Incomplete => ExitCode::from(3),
    }
}

pub fn run(command: CouncilCommand, config: &route::ConfigArg) -> Result<ExitCode> {
    let config = config.path();
    match command {
        CouncilCommand::Stats {
            route,
            since,
            format,
        } => {
            let rows = cedr_route::history::council_stats(
                &cedr_route::history::default_path(),
                route.as_deref(),
                since.as_deref(),
            )?;
            match format {
                route::ListFormat::Json => println!(
                    "{}",
                    serde_json::to_string_pretty(&rows).expect("rows serialize")
                ),
                route::ListFormat::Text => {
                    for r in rows {
                        println!(
                            "{} merges {} pass {} block {} incomplete {} revised {} passed_after_revision {} blocking_findings_per_merge {:.1}",
                            r.route.as_deref().unwrap_or("-"),
                            r.merges,
                            r.pass,
                            r.block,
                            r.incomplete,
                            r.revised,
                            r.passed_after_revision,
                            r.blocking_findings_per_merge
                        );
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }

        CouncilCommand::Plan {
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
            let plan = council::plan(&session.context(), &route, size.into(), &avoid)?;
            record(&plan.pick, size.into(), &sources, config)?;
            if let PickFormat::Json = format {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&plan).expect("plan serializes")
                );
            } else {
                print_pick(&plan.pick, format);
                println!(
                    "quorum {}: {} eligible, run {}",
                    plan.quorum,
                    plan.pick.delegates.len(),
                    plan.pick.run.as_deref().expect("plan run")
                );
            }
            Ok(match (plan.pick.delegates.len(), plan.quorum_reachable) {
                (0, _) => ExitCode::from(1),
                (_, true) => ExitCode::SUCCESS,
                (_, false) => ExitCode::from(3),
            })
        }
        CouncilCommand::Merge {
            reports,
            route,
            run,
            no_record,
            quorum,
            block,
            rounds,
            round,
            format,
        } => {
            let block = block.as_deref().and_then(council::Severity::parse);
            let settings =
                council::settings(quorum, block, rounds, round, route.as_deref(), || {
                    Ok(Session::config_only(config, &cwd()?)?.config)
                })?;
            let reports = reports
                .iter()
                .map(|a| read_report(a))
                .collect::<Result<Vec<_>>>()?;
            let mut merged = council::merge(&reports, settings);
            cedr_route::history::validate_run(
                &cedr_route::history::default_path(),
                run.as_deref(),
            )?;
            if !no_record {
                cedr_route::history::record_council(
                    &cedr_route::history::default_path(),
                    &mut merged,
                    route.as_deref(),
                    run.as_deref(),
                    chrono::Utc::now(),
                )?;
                route::auto_sync(config);
            }
            print_merged(&merged, format);
            Ok(gate_code(merged.gate))
        }
    }
}
