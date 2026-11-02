//! `cedr bench ...`: run a suite against a baseline and a candidate, and read the results
//! (docs/bench.md). Exit codes: run and show 0 complete, 1 the `--fail-on` condition held, 3
//! incomplete; 2 usage, suite or I/O failure.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cedr_bench::report::{self, BenchResult, FailOn};
use cedr_bench::run::{self, RunOptions, Selection};
use cedr_bench::suite;
use clap::{Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::Format;
use crate::{common, settings};

const DEFAULT_BUDGET_USD: f64 = 2.0;

#[derive(Clone, Copy, ValueEnum)]
pub enum FailOnArg {
    Never,
    Lower,
    Separated,
}

impl From<FailOnArg> for FailOn {
    fn from(arg: FailOnArg) -> FailOn {
        match arg {
            FailOnArg::Never => FailOn::Never,
            FailOnArg::Lower => FailOn::Lower,
            FailOnArg::Separated => FailOn::Separated,
        }
    }
}

#[derive(Subcommand)]
pub enum BenchCommand {
    /// Write a starter suite to .cedr/bench/
    Init {
        /// File name, without .toml
        #[arg(long, default_value = "suite")]
        name: String,
    },
    /// Validate a suite and show what a run would do, starting nothing
    Plan {
        /// Suite file; default the only *.toml in .cedr/bench/
        suite: Option<PathBuf>,
        /// Only this task; repeatable
        #[arg(long = "task", value_name = "ID")]
        tasks: Vec<String>,
        /// Trials per task and variant, instead of the suite's
        #[arg(long)]
        trials: Option<u32>,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Run a suite's agents and tests, within a budget (a suite is code: run ones you trust)
    Run {
        /// Suite file; default the only *.toml in .cedr/bench/
        suite: Option<PathBuf>,
        /// Cap in USD at API prices, instead of the one in your settings
        #[arg(long, value_name = "USD")]
        budget_usd: Option<f64>,
        /// Only this task; repeatable
        #[arg(long = "task", value_name = "ID")]
        tasks: Vec<String>,
        /// Trials per task and variant, instead of the suite's
        #[arg(long)]
        trials: Option<u32>,
        /// When a complete run exits 1: never, the candidate's pass rate is lower, or it is lower
        /// and the intervals do not overlap
        #[arg(long, value_enum, default_value_t = FailOnArg::Never)]
        fail_on: FailOnArg,
        /// Run variants whose spend the budget cannot see
        #[arg(long)]
        allow_unpriced: bool,
        /// Do not save the result
        #[arg(long)]
        no_record: bool,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Saved runs, newest first
    History {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Print a saved run, with the same report and exit code as when it ran
    Show {
        /// A run id from `cedr bench history`, or a result file
        id: String,
        #[arg(long, value_enum, default_value_t = FailOnArg::Never)]
        fail_on: FailOnArg,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Show or change the cap a run spends up to
    Budget {
        /// New cap in USD at API prices
        #[arg(value_name = "USD", conflicts_with_all = ["unset", "pull"])]
        usd: Option<f64>,
        /// Go back to the default
        #[arg(long, conflicts_with = "pull")]
        unset: bool,
        /// Store the cap set in the cedr.dev app (needs CEDR_API_KEY)
        #[arg(long)]
        pull: bool,
        /// Organization public id, with --pull
        #[arg(long, requires = "pull")]
        org: Option<String>,
        /// routes.toml holding API settings, with --pull
        #[arg(long, requires = "pull")]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
}

pub fn run(command: BenchCommand) -> Result<ExitCode, String> {
    match command {
        BenchCommand::Init { name } => init(&name),
        BenchCommand::Plan {
            suite,
            tasks,
            trials,
            format,
        } => {
            let plan = plan_value(suite.as_deref(), tasks, trials)?;
            match format {
                Format::Json => print_json(&plan),
                Format::Text => print!("{}", plan_text(&plan)),
            }
            Ok(ExitCode::SUCCESS)
        }
        BenchCommand::Run {
            suite,
            budget_usd,
            tasks,
            trials,
            fail_on,
            allow_unpriced,
            no_record,
            format,
        } => {
            let loaded = load(suite.as_deref())?;
            if let Some(cap) = budget_usd {
                check_cap(cap, "--budget-usd")?;
            }
            let budget_usd = match budget_usd {
                Some(cap) => cap,
                None => resolve_budget()?.0,
            };
            let options = RunOptions {
                selection: Selection { tasks, trials },
                budget_usd,
                allow_unpriced,
            };
            let result = run::run(&loaded, &options, |line| {
                eprintln!("cedr: bench: {line}");
            })
            .map_err(|e| e.to_string())?;
            let saved = if no_record { Ok(()) } else { save(&result) };
            print_result(&result, format);
            // The measurement is on stdout by now, and it cost money: report the failure, keep the output.
            saved?;
            Ok(ExitCode::from(result.exit_code(fail_on.into())))
        }
        BenchCommand::History { limit, format } => {
            let rows = history_rows(limit)?;
            match format {
                Format::Json => print_json(&Value::Array(rows)),
                Format::Text => print!("{}", history_text(&rows)),
            }
            Ok(ExitCode::SUCCESS)
        }
        BenchCommand::Show {
            id,
            fail_on,
            format,
        } => {
            let result = read_result(&id)?;
            print_result(&result, format);
            Ok(ExitCode::from(result.exit_code(fail_on.into())))
        }
        BenchCommand::Budget {
            usd,
            unset,
            pull,
            org,
            config,
            format,
        } => {
            let mut settings = settings::Settings::load();
            if let Some(cap) = usd {
                check_cap(cap, "USD")?;
                settings.set_bench_budget_usd(Some(cap));
                settings.save()?;
            } else if unset {
                settings.set_bench_budget_usd(None);
                settings.save()?;
            } else if pull {
                let sync = common::settings(org.as_deref(), config.as_deref())?;
                let key = cedr_route::sync::api_key().map_err(|e| e.to_string())?;
                let cap =
                    cedr_route::sync::get_bench_budget(&sync, &key).map_err(|e| e.to_string())?;
                settings.set_bench_budget_usd(cap);
                settings.save()?;
            }
            let (cap, source) = resolve_budget()?;
            match format {
                Format::Json => print_json(&json!({"budget_usd": cap, "source": source})),
                Format::Text => println!("bench budget {} ({source})", report::money(cap)),
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

const STARTER: &str = r#"# A Cedr Bench suite: https://cedr.dev/docs/bench
# A suite is code: setup and test are shell commands that run with your permissions.
# Check it with `cedr bench plan`; run it with `cedr bench run --budget-usd 5`.
version = 1
trials = 3
timeout_seconds = 600

# Compare a change to your agent setup: main against the commit you are on.
# To compare models instead, drop the refs and change a model.
[baseline]
harness = "claude"          # claude, codex, or command for any other agent
model = "claude-sonnet-5-5"
ref = "main"

[candidate]
harness = "claude"
model = "claude-sonnet-5-5"
ref = "HEAD"

[[tasks]]
id = "first-task"
prompt = "Describe a real task here, one the tests below can judge."
# setup = "npm ci"          # runs before the agent, in a fresh clone
test = "npm test"           # exit 0 is a pass
# test_files = ".cedr/bench/first-task"   # copied over the clone after the agent stops
"#;

fn init(name: &str) -> Result<ExitCode, String> {
    let valid = !name.is_empty()
        && !name.starts_with(['.', '-'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !valid {
        return Err(format!(
            "--name: \"{name}\" must be letters, digits, ., _ and -"
        ));
    }
    let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let root = cedr_bench::exec::git_root(&cwd).map_err(|e| e.to_string())?;
    let path = root
        .join(".cedr")
        .join("bench")
        .join(format!("{name}.toml"));
    if path.exists() {
        eprintln!("cedr: {} already exists", path.display());
        return Ok(ExitCode::from(1));
    }
    settings::write_atomic(&path, STARTER.as_bytes())?;
    println!("{}", path.display());
    Ok(ExitCode::SUCCESS)
}

fn check_cap(cap: f64, key: &str) -> Result<(), String> {
    if cap.is_finite() && cap > 0.0 {
        Ok(())
    } else {
        Err(format!("{key}: must be a number above 0"))
    }
}

/// The cap and where it came from, without a `--budget-usd` flag (docs/bench.md, Budget).
fn resolve_budget() -> Result<(f64, &'static str), String> {
    Ok(match settings::Settings::load().bench_budget_usd()? {
        Some(cap) => (cap, "settings"),
        None => (DEFAULT_BUDGET_USD, "default"),
    })
}

fn load(arg: Option<&Path>) -> Result<suite::Loaded, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let path = suite::locate(arg, &cwd).map_err(|e| e.to_string())?;
    suite::load(&path).map_err(|e| e.to_string())
}

/// The plan as JSON, shared by the command and its MCP tool.
pub(crate) fn plan_value(
    suite: Option<&Path>,
    tasks: Vec<String>,
    trials: Option<u32>,
) -> Result<Value, String> {
    let loaded = load(suite)?;
    let (cap, source) = resolve_budget()?;
    let plan =
        run::plan(&loaded, &Selection { tasks, trials }, cap, source).map_err(|e| e.to_string())?;
    serde_json::to_value(plan).map_err(|e| e.to_string())
}

fn plan_text(plan: &Value) -> String {
    let budget = report::money(plan["budget_usd"].as_f64().unwrap_or(0.0));
    let mut out = format!(
        "bench {}: {} trials ({} per task and variant), budget {budget} ({})\n",
        report::clean(plan["suite"].as_str().unwrap_or("")),
        plan["total_trials"],
        plan["trials_each"],
        plan["budget_source"].as_str().unwrap_or(""),
    );
    for name in ["baseline", "candidate"] {
        let v = &plan["variants"][name];
        let commit = v["commit"].as_str().unwrap_or("");
        let text = |key: &str| report::clean(v[key].as_str().unwrap_or("-"));
        out.push_str(&format!(
            "  {name:<10} {} {} effort {} {}, {} at {}\n",
            text("harness"),
            text("model"),
            text("effort"),
            text("permissions"),
            text("ref"),
            &commit[..commit.len().min(12)],
        ));
    }
    for task in plan["tasks"].as_array().into_iter().flatten() {
        let text = |key: &str| report::clean(task[key].as_str().unwrap_or(""));
        out.push_str(&format!("  task {}\n", text("id")));
        if task["setup"].is_string() {
            out.push_str(&format!("    setup: {}\n", text("setup")));
        }
        out.push_str(&format!("    test:  {}\n", text("test")));
        if task["test_files"].is_string() {
            out.push_str(&format!("    test files: {}\n", text("test_files")));
        }
    }
    for warning in plan["warnings"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "warning: {}\n",
            report::clean(warning.as_str().unwrap_or(""))
        ));
    }
    out
}

/// `cedr doctor`'s Bench checks (docs/doctor.md): the cap in force, and whether each suite in the
/// repo would plan. Starts no agent.
pub(crate) fn doctor_checks(root: &Path, mut add: impl FnMut(&str, &str, String, &str)) {
    match resolve_budget() {
        Ok((cap, source)) => add(
            "bench.budget",
            "ok",
            format!("{} ({source})", report::money(cap)),
            "",
        ),
        Err(e) => add("bench.budget", "fail", e, "Run cedr bench budget --unset"),
    }
    let dir = root.join(".cedr").join("bench");
    let mut suites: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    suites.sort();
    if suites.is_empty() {
        add(
            "bench.suite",
            "skip",
            "no suite in .cedr/bench/".into(),
            "Run cedr bench init to start one",
        );
    }
    for path in suites {
        let planned = suite::load(&path)
            .and_then(|loaded| run::plan(&loaded, &Selection::default(), 0.0, ""))
            .map_err(|e| e.to_string());
        let name = path.display();
        match planned {
            Ok(plan) if plan.warnings.is_empty() => {
                add("bench.suite", "ok", format!("{name} plans"), "")
            }
            Ok(plan) => add(
                "bench.suite",
                "warn",
                format!("{name}: {}", plan.warnings.join("; ")),
                "Run cedr bench plan",
            ),
            Err(e) => add("bench.suite", "fail", e, "Run cedr bench plan"),
        }
    }
}

fn results_dir() -> PathBuf {
    settings::state_file("bench")
}

fn save(result: &BenchResult) -> Result<(), String> {
    let path = results_dir().join(format!("{}.json", result.id));
    let text = serde_json::to_vec_pretty(result).map_err(|e| e.to_string())?;
    settings::write_atomic(&path, &text)
}

/// A result by run id or by file path. Files are capped before reading and validated after.
pub(crate) fn read_result(id: &str) -> Result<BenchResult, String> {
    let path = if run::is_result_id(id) {
        results_dir().join(format!("{id}.json"))
    } else {
        PathBuf::from(id)
    };
    read_result_file(&path)
}

fn read_result_file(path: &Path) -> Result<BenchResult, String> {
    let name = path.display();
    let size = std::fs::metadata(path)
        .map_err(|e| format!("{name}: {e}"))?
        .len();
    if size > report::MAX_RESULT_BYTES {
        return Err(format!("{name}: over {} bytes", report::MAX_RESULT_BYTES));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{name}: {e}"))?;
    report::parse(&bytes).map_err(|e| format!("{name}: {e}"))
}

/// History rows, newest first (ids sort by start time). A file that does not parse is named on
/// stderr and skipped: one bad file must not hide the others.
pub(crate) fn history_rows(limit: usize) -> Result<Vec<Value>, String> {
    let dir = results_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    let mut rows = Vec::new();
    for path in paths.iter().rev() {
        if rows.len() == limit {
            break;
        }
        match read_result_file(path) {
            Ok(r) => rows.push(json!({
                "id": r.id,
                "suite": r.suite.name,
                "started_at": r.started_at,
                "complete": r.complete,
                "stop": r.stop,
                "spent_usd": r.spent_usd,
                "baseline": {"passed": r.summary.baseline.passed, "scored": r.summary.baseline.scored},
                "candidate": {"passed": r.summary.candidate.passed, "scored": r.summary.candidate.scored},
            })),
            Err(e) => eprintln!("cedr: bench: skipped {e}"),
        }
    }
    Ok(rows)
}

fn history_text(rows: &[Value]) -> String {
    if rows.is_empty() {
        return "no saved bench runs\n".into();
    }
    let mut out = String::new();
    for row in rows {
        let text = |key: &str| report::clean(row[key].as_str().unwrap_or(""));
        out.push_str(&format!(
            "{}  {}  baseline {}/{}  candidate {}/{}  {}{}\n",
            text("id"),
            text("suite"),
            row["baseline"]["passed"],
            row["baseline"]["scored"],
            row["candidate"]["passed"],
            row["candidate"]["scored"],
            report::money(row["spent_usd"].as_f64().unwrap_or(0.0)),
            if row["complete"] == true {
                ""
            } else {
                "  incomplete"
            },
        ));
    }
    out
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("value serializes")
    );
}

fn print_result(result: &BenchResult, format: Format) {
    match format {
        Format::Text => print!("{}", result.text()),
        Format::Json => println!(
            "{}",
            serde_json::to_string_pretty(result).expect("result serializes")
        ),
    }
}
