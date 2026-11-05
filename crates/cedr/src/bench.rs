//! `cedr bench ...`: run a suite against a baseline and a candidate, and read the results
//! (docs/bench.md). Exit codes: run and show 0 complete, 1 the `--fail-on` condition held, 3
//! incomplete; 2 usage, suite or I/O failure.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cedr_bench::report::{self, BenchResult, Ci, CiProvider, FailOn, Source};
use cedr_bench::run::{self, RunOptions, Selection};
use cedr_bench::suite;
use cedr_route::{config, machine};
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
        /// Do not save or send this run
        #[arg(long)]
        no_record: bool,
        /// Skip automatic sending for this run; a later bench push can still send it
        #[arg(long)]
        no_push: bool,
        /// Repository name recorded with the run
        #[arg(long, value_name = "NAME")]
        repo: Option<String>,
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
    /// Send saved runs to an organization (unsent runs from the current repo by default)
    Push {
        /// Saved run ids; explicit ids are sent even when already sent
        #[arg(value_name = "ID")]
        ids: Vec<String>,
        /// Send every unsent result on this machine, including other repos
        #[arg(long, conflicts_with = "repo")]
        all: bool,
        /// Organization public id
        #[arg(long)]
        org: Option<String>,
        /// routes.toml holding API settings
        #[arg(long)]
        config: Option<PathBuf>,
        /// Repo to select; with ids, override or supply their saved repo name
        #[arg(long, value_name = "NAME")]
        repo: Option<String>,
        /// Print the bodies without sending or marking them sent
        #[arg(long)]
        dry_run: bool,
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
            no_push,
            repo,
            format,
        } => {
            if let Some(name) = repo.as_deref() {
                common::repo_name(Some(name), Path::new(""))?;
            }
            let loaded = load(suite.as_deref())?;
            let source = source(repo.as_deref(), Some(&loaded.root));
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
            // Progress must never end a run that has spent money, so its failures are ignored.
            let mut result = run::run(&loaded, &options, |line| {
                let _ = writeln!(std::io::stderr(), "cedr: bench: {line}");
            })
            .map_err(|e| e.to_string())?;
            result.source = Some(source);
            let saved = if no_record { Ok(()) } else { save(&result) };
            print_result(&result, format);
            // The measurement is on stdout by now, and it cost money: report the failure, keep the output.
            saved?;
            if !no_record && !no_push {
                auto_push(&result);
            }
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
        BenchCommand::Push {
            ids,
            all,
            org,
            config,
            repo,
            dry_run,
            format,
        } => push(
            ids,
            all,
            org.as_deref(),
            config.as_deref(),
            repo.as_deref(),
            dry_run,
            format,
        ),
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

/// Captures repository identity and GitHub run context, never a remote URL or checkout path.
fn source(flag: Option<&str>, root: Option<&Path>) -> Source {
    let valid = |name: String| {
        (!name.split('/').any(|part| matches!(part, "." | "..")))
            .then(|| common::repo_name(Some(&name), Path::new("")).ok())
            .flatten()
    };
    let github = std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true");
    let repo = flag
        .map(str::to_owned)
        .or_else(|| {
            github
                .then(|| std::env::var("GITHUB_REPOSITORY").ok())
                .flatten()
                .and_then(valid)
        })
        .or_else(|| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(root?)
                .args(["remote", "get-url", "origin"])
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            let remote = String::from_utf8(output.stdout).ok()?;
            remote_repo(remote.trim()).and_then(valid)
        })
        .or_else(|| common::repo_name(None, root?).ok().and_then(valid));
    let ci = github.then(github_ci).flatten();
    Source {
        repo,
        root_commit: root.and_then(root_commit),
        ci,
    }
}

/// Shared history identifies a repo across clones and linked worktrees without keeping a path.
fn root_commit(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-list", "--max-parents=0", "HEAD", "--"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .min()
        .map(str::to_owned)
}

/// Local remotes may contain account names. Only network remotes can supply owner/name.
fn remote_repo(remote: &str) -> Option<String> {
    let (raw_path, path) = if let Some((_, rest)) = remote.split_once("://") {
        let url = reqwest::Url::parse(remote).ok()?;
        if url.scheme() == "file" || url.host_str().is_none() {
            return None;
        }
        let raw = rest.find(['/', '\\']).map_or("", |i| &rest[i..]);
        let raw = raw.split(['?', '#']).next()?;
        (raw, url.path().to_owned())
    } else {
        let (authority, path) = remote.split_once(':')?;
        let host = authority.rsplit('@').next()?;
        if host.is_empty()
            || authority.contains(['/', '\\'])
            || (authority.len() == 1 && authority.as_bytes()[0].is_ascii_alphabetic())
        {
            return None;
        }
        (path, path.to_owned())
    };
    // URL parsing normalizes dot segments, so reject them before normalization as well.
    if raw_path.split(['/', '\\']).any(|part| {
        let dots = part.to_ascii_lowercase().replace("%2e", ".");
        matches!(dots.as_str(), "." | "..")
    }) {
        return None;
    }
    let parts: Vec<_> = path.trim_end_matches('/').rsplit('/').take(2).collect();
    if parts.len() != 2
        || parts
            .iter()
            .any(|p| p.is_empty() || matches!(*p, "." | ".."))
    {
        return None;
    }
    let name = parts[0].strip_suffix(".git").unwrap_or(parts[0]);
    if name.is_empty() || matches!(name, "." | "..") {
        return None;
    }
    Some(format!("{}/{name}", parts[1]))
}

/// A bare push must identify a repo without treating an arbitrary working directory as one.
fn current_repo(flag: Option<&str>) -> Result<Source, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = cedr_bench::exec::git_root(&cwd).ok();
    let source = source(flag, root.as_deref());
    // A name alone can collide across organizations, so a bare push needs the repo's history too.
    if source.repo.is_none() || source.root_commit.is_none() {
        return Err(
            "bench push: cannot determine repo; run it inside the repo, name ids or pass --all"
                .into(),
        );
    }
    Ok(source)
}

fn github_ci() -> Option<Ci> {
    let server = std::env::var("GITHUB_SERVER_URL").ok()?;
    let repo = std::env::var("GITHUB_REPOSITORY").ok()?;
    let run = std::env::var("GITHUB_RUN_ID").ok()?;
    if server.is_empty() || repo.is_empty() || run.is_empty() {
        return None;
    }
    let run_url = format!("{}/{repo}/actions/runs/{run}", server.trim_end_matches('/'));
    let url = reqwest::Url::parse(&run_url).ok()?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let pr = std::env::var("GITHUB_REF").ok().and_then(|r| {
        let number = r.strip_prefix("refs/pull/")?.strip_suffix("/merge")?;
        (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
            .then(|| number.parse().ok())
            .flatten()
    });
    Some(Ci {
        provider: CiProvider::Github,
        pr,
        run_url,
    })
}

/// Missing configuration is the sole silent lookup failure; a selected but broken file warns.
fn local_config() -> Result<Option<config::Config>, String> {
    let path = match config::machine_path(None) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    config::load_local(&path)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn push_machine(config: Option<&config::Config>) -> Result<Value, String> {
    let fallback = config::Config::parse("version = 1", Path::new("routes.toml"))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "id": machine::machine_id().map_err(|e| e.to_string())?,
        "name": push_machine_name(config.unwrap_or(&fallback)),
    }))
}

fn push_machine_name(config: &config::Config) -> String {
    common::clip(&report::clean(&machine::machine_name(config)), 64)
}

const MAX_PUSH_BYTES: usize = 256 * 1024;

fn push_body(
    result: &BenchResult,
    sync: &config::Sync,
    machine: &Value,
    repo: Option<&str>,
) -> Result<Value, String> {
    if !run::is_result_id(&result.id) {
        return Err("invalid saved result id".into());
    }
    let repo = repo
        .or_else(|| result.source.as_ref().and_then(|s| s.repo.as_deref()))
        .ok_or("no repo name saved; pass --repo")?;
    let repo = common::repo_name(Some(repo), Path::new(""))?;
    let mut outgoing = result.clone();
    clean_outgoing(&mut outgoing);
    let mut saved = serde_json::to_value(outgoing).map_err(|e| e.to_string())?;
    saved
        .as_object_mut()
        .expect("result object")
        .remove("source");
    let body = json!({
        "org": sync.org, "cli": env!("CARGO_PKG_VERSION"), "repo": repo,
        "machine": machine, "ci": result.source.as_ref().and_then(|s| s.ci.as_ref()),
        "result": saved,
    });
    if body.to_string().len() > MAX_PUSH_BYTES {
        return Err(format!(
            "result {}: body over 256 KiB ({MAX_PUSH_BYTES} bytes)",
            result.id
        ));
    }
    Ok(body)
}

/// Clean only the outgoing copy: local reports retain exactly what the harness measured.
fn clean_outgoing(result: &mut BenchResult) {
    fn text(value: &str, max: usize) -> String {
        common::clip(&report::clean(value), max)
    }
    fn model(value: &str) -> String {
        let value = report::clean(value);
        let file = value.strip_prefix("file:");
        let path = file.unwrap_or(&value);
        // A separator is required: `a:mini` is a model name, while `C:\\` and `C:/x` are paths.
        let drive = path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && path.as_bytes().get(1) == Some(&b':')
            && matches!(path.as_bytes().get(2), Some(b'/' | b'\\'));
        let is_path = file.is_some()
            || path.starts_with('/')
            || path.starts_with("\\\\")
            || ["~/", "./", "../", ".\\", "..\\"]
                .iter()
                .any(|prefix| path.starts_with(prefix))
            || drive;
        let name = if is_path {
            let path = if drive { &path[2..] } else { path };
            path.trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("")
        } else {
            &value
        };
        common::clip(name, 256)
    }
    fn required(value: String) -> String {
        if value.is_empty() {
            "unknown".into()
        } else {
            value
        }
    }
    fn optional(value: &mut Option<String>, max: usize) {
        *value = value
            .as_deref()
            .map(|s| text(s, max))
            .filter(|s| !s.is_empty());
    }
    fn models(values: &mut Vec<String>) {
        *values = values
            .iter()
            .map(|s| model(s))
            .filter(|s| !s.is_empty())
            .collect();
    }
    result.suite.name = required(text(&result.suite.name, 128));
    for variant in [
        &mut result.variants.baseline,
        &mut result.variants.candidate,
    ] {
        variant.model = required(model(&variant.model));
        optional(&mut variant.effort, 128);
        variant.git_ref = required(text(&variant.git_ref, 256));
        optional(&mut variant.harness_version, 512);
        models(&mut variant.resolved_models);
    }
    for trial in &mut result.trials {
        models(&mut trial.models);
    }
}

fn pushed_path(org: &str) -> PathBuf {
    settings::state_file(&format!("bench-pushed-{org}.txt"))
}

/// Mark only accepted results. A lost marker is harmless because the server deduplicates ids.
fn send_result(sync: &config::Sync, key: &str, id: &str, body: &Value) -> Result<bool, String> {
    let reply = cedr_route::sync::send_bench(sync, key, body).map_err(|e| e.to_string())?;
    let duplicate = reply["duplicate"]
        .as_bool()
        .ok_or("bench reply: missing duplicate")?;
    if reply["ok"] != true || reply["id"].as_str() != Some(id) {
        return Err("bench reply: expected ok and matching result id".into());
    }
    let path = pushed_path(&sync.org);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(file, "{id}").map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(duplicate)
}

fn push(
    ids: Vec<String>,
    all: bool,
    org: Option<&str>,
    config: Option<&Path>,
    repo: Option<&str>,
    dry_run: bool,
    format: Format,
) -> Result<ExitCode, String> {
    if let Some(repo) = repo {
        common::repo_name(Some(repo), Path::new(""))?;
    }
    let selected_repo = if ids.is_empty() && !all {
        Some(current_repo(repo)?)
    } else {
        None
    };
    let sync = common::settings(org, config)?;
    let key = if dry_run {
        String::new()
    } else {
        cedr_route::sync::api_key().map_err(|e| e.to_string())?
    };
    // Match common::settings: --org alone never reads the checkout's config in CI.
    let config = if org.is_some() && config.is_none() {
        None
    } else {
        let path = config::machine_path(config).map_err(|e| e.to_string())?;
        Some(config::load_local(&path).map_err(|e| e.to_string())?)
    };
    let machine = push_machine(config.as_ref())?;
    let ids = if ids.is_empty() {
        let path = pushed_path(&sync.org);
        let sent = match std::fs::read_to_string(&path) {
            Ok(text) => text.lines().map(str::to_owned).collect::<HashSet<_>>(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut ids = Vec::new();
        match std::fs::read_dir(results_dir()) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry.map_err(|e| e.to_string())?.path();
                    if path.extension().is_some_and(|e| e == "json")
                        && let Some(id) = path.file_stem().and_then(|s| s.to_str())
                        && !sent.contains(id)
                    {
                        ids.push(id.to_string());
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", results_dir().display())),
        }
        ids.sort();
        ids
    } else {
        ids
    };
    let mut pushed = Vec::new();
    let mut duplicates = Vec::new();
    let mut failed = Vec::new();
    let mut bodies = Vec::new();
    let mut skipped = 0;
    for id in ids {
        let outcome: Result<(), String> = (|| {
            if !run::is_result_id(&id) {
                return Err("expected a saved run id".into());
            }
            let result = match read_result(&id) {
                Ok(result) => result,
                Err(_) if selected_repo.is_some() => {
                    skipped += 1;
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            if result.id != id {
                return Err("saved result id does not match its file name".into());
            }
            if let Some(selected) = &selected_repo
                && !result.source.as_ref().is_some_and(|saved| {
                    saved
                        .repo
                        .as_deref()
                        .zip(selected.repo.as_deref())
                        .is_some_and(|(saved, current)| saved.eq_ignore_ascii_case(current))
                        && saved.root_commit.is_some()
                        && saved.root_commit == selected.root_commit
                })
            {
                skipped += 1;
                return Ok(());
            }
            let body = push_body(&result, &sync, &machine, repo)?;
            if dry_run {
                bodies.push(body);
                return Ok(());
            }
            let duplicate = send_result(&sync, &key, &id, &body)?;
            if duplicate {
                duplicates.push(id.clone());
            }
            pushed.push(id.clone());
            Ok(())
        })();
        if let Err(error) = outcome {
            failed.push(json!({"id": id, "error": error}));
        }
    }
    if dry_run {
        match format {
            Format::Json => print_json(&json!(bodies)),
            Format::Text => {
                for body in bodies {
                    print_json(&body);
                }
            }
        }
        for failure in &failed {
            eprintln!(
                "cedr: bench: {}: {}",
                report::clean(failure["id"].as_str().unwrap_or("")),
                report::clean(failure["error"].as_str().unwrap_or(""))
            );
        }
    } else {
        match format {
            Format::Json => print_json(
                &json!({"org": sync.org, "pushed": pushed, "duplicates": duplicates, "failed": failed, "skipped": skipped}),
            ),
            Format::Text => {
                for failure in &failed {
                    println!(
                        "{}: {}",
                        report::clean(failure["id"].as_str().unwrap_or("")),
                        report::clean(failure["error"].as_str().unwrap_or(""))
                    );
                }
                let suffix = if duplicates.is_empty() {
                    String::new()
                } else {
                    format!(" ({} duplicates)", duplicates.len())
                };
                println!("pushed {} to {}{suffix}", pushed.len(), sync.org);
                if skipped > 0 {
                    println!(
                        "{skipped} saved results from other repos left alone; name them or pass --all"
                    );
                }
            }
        }
    }
    Ok(ExitCode::from(if failed.is_empty() { 0 } else { 2 }))
}

/// Upload failures cannot replace an expensive measurement's report or gate.
fn auto_push(result: &BenchResult) {
    let outcome: Result<Option<String>, String> = (|| {
        let Some(config) = local_config()? else {
            return Ok(None);
        };
        let Some(sync) = config.sync.as_ref().filter(|s| s.auto) else {
            return Ok(None);
        };
        let key = cedr_route::sync::api_key().map_err(|e| e.to_string())?;
        let machine = push_machine(Some(&config))?;
        let body = push_body(result, sync, &machine, None)?;
        send_result(sync, &key, &result.id, &body)?;
        Ok(Some(sync.org.clone()))
    })();
    match outcome {
        Ok(Some(org)) => {
            let _ = writeln!(
                std::io::stderr(),
                "cedr: bench: pushed {} to {org}",
                result.id
            );
        }
        Ok(None) => {}
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "cedr: bench: {} was not sent: {}; cedr bench push can send it later",
                result.id,
                report::clean(&error)
            );
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
/// repo would plan, plus the automatic upload destination. Starts no agent.
pub(crate) fn doctor_checks(root: &Path, mut add: impl FnMut(&str, &str, String, &str)) {
    match resolve_budget() {
        Ok((cap, source)) => add(
            "bench.budget",
            "ok",
            format!("{} ({source})", report::money(cap)),
            "",
        ),
        Err(e) => add(
            "bench.budget",
            "fail",
            e,
            "Fix or remove the settings file it names; cedr bench budget --unset clears a broken value",
        ),
    }
    match local_config() {
        Ok(Some(config)) => match config.sync {
            Some(sync) if sync.auto => add(
                "bench.push",
                "ok",
                format!("results sent automatically to {}", sync.org),
                "",
            ),
            Some(sync) => add(
                "bench.push",
                "skip",
                format!(
                    "automatic sending off (auto = false); organization {}",
                    sync.org
                ),
                "Run cedr bench push to send saved results, or set [sync] auto = true",
            ),
            None => add(
                "bench.push",
                "skip",
                "no [sync]; results are not sent automatically".into(),
                "Add [sync] to routes.toml to send results automatically",
            ),
        },
        Ok(None) => add(
            "bench.push",
            "skip",
            "no config; results are not sent automatically".into(),
            "Add [sync] to routes.toml to send results automatically",
        ),
        Err(error) => add(
            "bench.push",
            "warn",
            format!("automatic sending unavailable: {error}"),
            "Repair routes.toml; cedr bench push can send saved results later",
        ),
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

/// Prints the report. A reader that has gone away must not replace the run's exit code with a panic.
fn print_result(result: &BenchResult, format: Format) {
    let text = match format {
        Format::Text => result.text(),
        Format::Json => {
            let mut json = serde_json::to_string_pretty(result).expect("result serializes");
            json.push('\n');
            json
        }
    };
    let _ = std::io::stdout().write_all(text.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outgoing_machine_name_removes_controls_before_clipping() {
        let mut config = config::Config::parse("version=1", Path::new("routes.toml")).unwrap();
        // Exercise the boundary independently of the config parser's one-line restriction.
        config.machine = Some(config::Machine {
            name: Some(format!("host\n\t{}", "é".repeat(70))),
            skip: Vec::new(),
        });
        assert_eq!(
            push_machine_name(&config),
            format!("host{}", "é".repeat(60))
        );
    }
}
