//! Bench CLI and read-only MCP access. No automatic result upload.
use crate::{Format, settings};
use cedr_bench::{
    adapter::Harness,
    report::{self, BenchResult},
    run::{self, Options, ProcessBackend},
    suite,
};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

#[derive(Args)]
pub struct Configuration {
    /// claude, codex, muse, grok, gemini, antigravity (local), or command (local)
    #[arg(long)]
    harness: Harness,
    #[arg(long)]
    model: String,
    #[arg(long)]
    effort: Option<String>,
    /// Local suite directory; otherwise fetch the edition from the public suite repository
    #[arg(long, conflicts_with = "edition")]
    suite: Option<PathBuf>,
    #[arg(long)]
    edition: Option<u32>,
    /// Run the agent on this host with a clean home
    #[arg(long)]
    local: bool,
    /// Build the edition's image/Dockerfile instead of pulling its pinned image
    #[arg(long)]
    build_image: bool,
    /// Exact vendor version; defaults to the installed agent version
    #[arg(long)]
    agent_version: Option<String>,
    /// Print the measured probe reference for probes/edition-<n>.toml
    #[arg(long)]
    set_reference: bool,
    /// Custom argv, one argument per flag; placeholders: {prompt_file}, {model}, {effort}
    #[arg(long = "command", allow_hyphen_values = true)]
    command: Vec<String>,
    #[arg(long,value_enum,default_value_t=Format::Text)]
    format: Format,
}
#[derive(Subcommand)]
pub enum BenchCommand {
    /// Run every edition task once, recording a score, list cost and visual artifacts
    Run {
        #[command(flatten)]
        config: Configuration,
        /// Optional spending stop checked before each task; there is no stop by default
        #[arg(long)]
        stop_at_usd: Option<f64>,
        #[arg(long)]
        allow_unpriced: bool,
        /// Diagnostic subset; omitted edition tasks still count as zero
        #[arg(long = "task")]
        tasks: Vec<String>,
        #[arg(long,default_value="plan",value_parser=["plan","api"])]
        billing: String,
    },
    /// Verify shell and file tools and compare context tokens with an edition reference
    Probe {
        #[command(flatten)]
        config: Configuration,
    },
    /// Saved runs, newest first
    History {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long,value_enum,default_value_t=Format::Text)]
        format: Format,
    },
    /// Validate and print one saved result
    Show {
        id: String,
        #[arg(long,value_enum,default_value_t=Format::Text)]
        format: Format,
    },
    /// Record a person's review of the saved patches for a flagged run
    Review {
        id: String,
        #[arg(
            long,
            required_unless_present = "tampered",
            conflicts_with = "tampered"
        )]
        clean: bool,
        #[arg(long)]
        tampered: bool,
        #[arg(long)]
        note: String,
    },
    /// Export unchanged results and screenshot artifacts for publishing
    Export {
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(long)]
        out: PathBuf,
    },
}
fn cache() -> PathBuf {
    cedr_route::config::xdg_dir("XDG_CACHE_HOME", ".cache").join("cedr/bench")
}
fn results() -> PathBuf {
    settings::state_file("bench")
}
fn load(c: &Configuration) -> Result<suite::Suite, String> {
    if let Some(path) = &c.suite {
        return suite::load(path);
    }
    let number = c.edition.unwrap_or(1);
    if number == 0 {
        return Err("edition must be positive".into());
    }
    let base = cache();
    let dest = base.join(format!("edition-{number}"));
    let receipt = base.join(format!("edition-{number}.json"));
    if !dest.exists() {
        std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
        // A failed fetch never leaves a half-populated edition at its canonical cache path.
        let temporary = base.join(format!("fetch-{}", std::process::id()));
        if temporary.exists() {
            return Err(
                "unfinished edition fetch; remove the fetch directory in the Bench cache".into(),
            );
        }
        let mut command = Command::new("git");
        command
            .args([
                "clone",
                "--quiet",
                "--depth",
                "1",
                "--branch",
                &format!("edition-{number}"),
                "https://github.com/Sheernox/cedr-bench.git",
            ])
            .arg(&temporary);
        let fetched = cedr_compare::exec::run_step(command, None, Duration::from_secs(120))
            .map_err(|e| e.to_string())?;
        if !fetched.succeeded() {
            let _ = std::fs::remove_dir_all(&temporary);
            return Err(
                "cannot fetch suite from github.com; check that the edition is published".into(),
            );
        }
        let suite = suite::load(&temporary)?;
        let tagged = Command::new("git")
            .arg("-C")
            .arg(&temporary)
            .args([
                "rev-parse",
                "--verify",
                &format!("refs/tags/edition-{number}^{{commit}}"),
            ])
            .output()
            .map_err(|e| e.to_string())?;
        if !tagged.status.success()
            || suite.commit.as_deref() != Some(String::from_utf8_lossy(&tagged.stdout).trim())
        {
            return Err("fetched edition is not the requested edition tag".into());
        }

        if suite.edition.number != number {
            return Err("fetched edition number does not match tag".into());
        }
        settings::write_atomic(
            &receipt,
            &serde_json::to_vec(&json!({"sha256":suite.digest,"commit":suite.commit}))
                .map_err(|e| e.to_string())?,
        )?;
        std::fs::rename(temporary, &dest).map_err(|e| e.to_string())?;
    }
    let expected: Value =
        serde_json::from_slice(&std::fs::read(receipt).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    suite::verify(
        &dest,
        expected["sha256"]
            .as_str()
            .ok_or("invalid cached edition digest")?,
    )?;
    let mut suite = suite::load(&dest)?;
    let reference_path = base.join(format!("probes/edition-{number}.toml"));
    // HEAD resolves the repository's default branch, independently of its frozen tag.
    let url = format!(
        "https://raw.githubusercontent.com/Sheernox/cedr-bench/HEAD/probes/edition-{number}.toml"
    );
    let fresh = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .ok()
        .and_then(|client| client.get(url).send().ok())
        .filter(|response| response.status().is_success())
        .and_then(|response| {
            use std::io::Read;
            let mut text = String::new();
            response
                .take(1024 * 1024 + 1)
                .read_to_string(&mut text)
                .ok()?;
            Some(text)
        });
    suite.edition.probe = cached_references(&reference_path, fresh)?;
    if json!(suite.commit) != expected["commit"] || suite.edition.number != number {
        return Err("cached edition commit changed".into());
    }
    Ok(suite)
}
fn cached_references(path: &Path, fresh: Option<String>) -> Result<suite::References, String> {
    if let Some(text) = fresh {
        let references = suite::parse_references(&text)?;
        settings::write_atomic(path, text.as_bytes())?;
        return Ok(references);
    }
    match std::fs::read_to_string(path) {
        Ok(text) => suite::parse_references(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(suite::References::default())
        }
        Err(error) => Err(format!("cannot read cached probe references: {error}")),
    }
}
fn options(c: &Configuration) -> Result<Options, String> {
    if c.harness != Harness::Command && !c.command.is_empty() {
        return Err("--command requires --harness command".into());
    }
    let detected_anti = if c.harness == Harness::Antigravity {
        Some(cedr_bench::antigravity::version()?)
    } else {
        None
    };
    if c.agent_version
        .as_ref()
        .zip(detected_anti.as_ref())
        .is_some_and(|(a, b)| a != b)
    {
        return Err("requested Antigravity version differs from installed ACP version".into());
    }
    let version = if let Some(v) = &c.agent_version {
        v.clone()
    } else if let Some(v) = detected_anti {
        v
    } else if c.harness == Harness::Muse {
        let launcher = cedr_compare::exec::find_program("muse", Path::new("."))
            .ok_or("Muse is not installed")?;
        std::fs::read_to_string(
            launcher
                .parent()
                .ok_or("Muse installation has no parent")?
                .join(".muse-version"),
        )
        .map_err(|_| "Muse version marker missing; use --agent-version")?
        .trim()
        .to_string()
    } else if c.harness == Harness::Command {
        "custom".into()
    } else {
        let mut cmd = Command::new(c.harness.name());
        cmd.arg("--version");
        let out = cedr_compare::exec::run_step(cmd, None, Duration::from_secs(10))
            .map_err(|e| e.to_string())?;
        if !out.succeeded() {
            return Err("cannot determine agent version; use --agent-version".into());
        }
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .find(|s| s.starts_with(|c: char| c.is_ascii_digit()))
            .ok_or("unrecognized vendor version")?
            .to_string()
    };
    if c.local {
        cedr_bench::adapter::check_effort(c.harness, &c.model, c.effort.as_deref())?;
    }
    Ok(Options {
        harness: c.harness,
        model: c.model.clone(),
        effort: c.effort.clone(),
        version,
        local: c.local,
        build_image: c.build_image,
        set_reference: c.set_reference,
        stop_usd: None,
        allow_unpriced: false,
        tasks: Vec::new(),
        command: c.command.clone(),
        billing: "plan".into(),
    })
}
fn print<T: serde::Serialize>(value: &T) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())?
    );
    Ok(())
}
fn summary(r: &BenchResult) {
    println!(
        "bench edition {}: {}, score {:.2}, solved {}/{}, {}, over_goal={} ({})",
        r.edition.number,
        if r.complete { "complete" } else { "incomplete" },
        r.score,
        r.solved,
        r.code_tasks,
        r.cost_text(),
        if r.unpriced_trials > 0 {
            "unknown (cost incomplete)".to_string()
        } else {
            r.over_goal.to_string()
        },
        r.id
    );
    println!(
        "  fingerprint {:?}: {:?} input tokens; tolerance={}; tools_ok={}",
        r.probe.status, r.probe.input_tokens, r.probe.tolerance, r.probe.tools_ok
    );
    if let Some(stop) = r.stop {
        println!("  stop: {stop:?}");
    }
    for task in &r.tasks {
        let score = task
            .score
            .map(|n| format!("{:.0}%", n * 100.0))
            .unwrap_or_else(|| "visual".into());
        let cost = task
            .cost_usd
            .map(|n| format!("${n:.4}"))
            .unwrap_or_else(|| "unpriced".into());
        println!("  {} {:?}: {score}, {cost}", task.id, task.status);
        for signal in &task.tamper_signals {
            println!(
                "    signal {}:{}: {}",
                signal.path, signal.line, signal.kind
            );
        }
        if task.patch_sha256.is_some() && task.kind == suite::Kind::Code {
            println!(
                "    saved patch: {}",
                results()
                    .join(&r.id)
                    .join("artifacts")
                    .join(&task.id)
                    .join("submission.patch")
                    .display()
            );
        }
        for artifact in &task.artifacts {
            println!("    {} ({} bytes)", artifact.path, artifact.bytes);
        }
    }
    if let Some(review) = &r.tamper_review {
        println!(
            "  tamper review {:?} at {}: {}",
            review.verdict, review.time, review.note
        );
    } else if r.tasks.iter().any(|task| !task.tamper_signals.is_empty()) {
        println!(
            "  read the saved patches, then: cedr bench review {} --clean --note <text> (or --tampered)",
            r.id
        );
    }
}
fn reference(c: &Configuration, o: &Options, p: &report::Probe) -> Result<(), String> {
    if c.set_reference && p.status == report::ProbeStatus::Reference {
        let input = p.input_tokens.ok_or("probe did not report input tokens")?;
        eprintln!(
            "\n[[references]]\nharness = {:?}\nharness_version = {:?}\nmodel = {:?}",
            o.harness.name(),
            o.version,
            o.model
        );
        if let Some(e) = &o.effort {
            eprintln!("effort = {e:?}");
        }
        eprintln!("input_tokens = {input}");
        if let Some(t) = p.tools {
            eprintln!("tools = {t}");
        }
    }
    Ok(())
}
pub fn run(command: BenchCommand) -> Result<ExitCode, String> {
    match command {
        BenchCommand::Run {
            config,
            stop_at_usd,
            allow_unpriced,
            tasks,
            billing,
        } => {
            let suite = load(&config)?;
            let mut options = options(&config)?;
            options.stop_usd = match stop_at_usd {
                Some(v) => Some(v),
                None => settings::Settings::load().bench_stop_usd()?,
            };
            if options.stop_usd.is_some_and(|n| !n.is_finite() || n <= 0.0) {
                return Err("--stop-at-usd must be finite and above zero".into());
            }
            options.allow_unpriced = allow_unpriced;
            options.tasks = tasks;
            options.billing = billing;
            for id in &options.tasks {
                if !suite.tasks.iter().any(|t| &t.id == id) {
                    return Err(format!("unknown task {id}"));
                }
            }
            if options.stop_usd.is_none() {
                eprintln!(
                    "cedr: bench: no spending stop is set; use --stop-at-usd USD to stop before another task"
                );
            }
            cedr_compare::exec::catch_interrupts();
            let temporary = results().join(format!("pending-{}", std::process::id()));
            std::fs::create_dir_all(&temporary).map_err(|e| e.to_string())?;
            let mut backend = ProcessBackend::new(&suite, &options)?;
            let r = run::run(&suite, &options, &mut backend, &temporary, |s| {
                eprintln!("cedr: bench: {s}")
            })?;
            let artifacts = results().join(&r.id).join("artifacts");
            std::fs::create_dir_all(artifacts.parent().ok_or("missing result parent")?)
                .map_err(|e| e.to_string())?;
            std::fs::rename(temporary, artifacts).map_err(|e| e.to_string())?;
            settings::write_atomic(
                &results().join(format!("{}.json", r.id)),
                &serde_json::to_vec_pretty(&r).map_err(|e| e.to_string())?,
            )?;
            reference(&config, &options, &r.probe)?;
            match config.format {
                Format::Json => print(&r)?,
                Format::Text => summary(&r),
            };
            Ok(ExitCode::from(if r.complete { 0 } else { 3 }))
        }
        BenchCommand::Probe { config } => {
            let suite = load(&config)?;
            let options = options(&config)?;
            let mut backend = ProcessBackend::new(&suite, &options)?;
            let p = run::probe(&suite, &options, &mut backend)?;
            reference(&config, &options, &p)?;
            match config.format {
                Format::Json => print(&p)?,
                Format::Text => println!(
                    "probe {:?}: input={:?}, tools={:?}, reference={:?}, tolerance={}, tools_ok={}",
                    p.status, p.input_tokens, p.tools, p.reference, p.tolerance, p.tools_ok
                ),
            };
            Ok(ExitCode::from(if p.passed() { 0 } else { 3 }))
        }
        BenchCommand::Show { id, format } => {
            let r = read_result(&id)?;
            match format {
                Format::Json => print(&r)?,
                Format::Text => summary(&r),
            };
            Ok(ExitCode::from(if r.complete { 0 } else { 3 }))
        }
        BenchCommand::History { limit, format } => {
            let rows = history_rows(limit)?;
            match format {
                Format::Json => print(&rows)?,
                Format::Text => {
                    if rows.is_empty() {
                        println!("no saved bench runs");
                    }
                    for r in rows {
                        println!(
                            "{} score={} spent={} complete={}",
                            r["id"].as_str().unwrap_or("unknown"),
                            r["score"],
                            r["cost"].as_str().unwrap_or("unknown"),
                            r["complete"]
                        );
                    }
                }
            };
            Ok(ExitCode::SUCCESS)
        }
        BenchCommand::Review {
            id,
            clean,
            tampered: _,
            note,
        } => {
            let mut r = read_result(&id)?;
            if r.tasks.iter().all(|task| task.tamper_signals.is_empty()) {
                return Err("run has no tamper signals to review".into());
            }
            if clean {
                verify_review_patches(&r)?;
            }
            let review = report::TamperReview {
                verdict: if clean {
                    report::TamperVerdict::Clean
                } else {
                    report::TamperVerdict::Tampered
                },
                note,
                time: chrono::Utc::now().to_rfc3339(),
            };
            review.validate()?;
            r.tamper_review = Some(review);
            r.summarize();
            r.validate()?;
            settings::write_atomic(
                &result_path(&id),
                &serde_json::to_vec_pretty(&r).map_err(|e| e.to_string())?,
            )?;
            summary(&r);
            Ok(ExitCode::SUCCESS)
        }
        BenchCommand::Export { ids, out } => {
            export(&ids, &out)?;
            println!("exported {} results", ids.len());
            Ok(ExitCode::SUCCESS)
        }
    }
}
// A clean review requires the exact retained evidence, not only its recorded hash.
fn verify_review_patches(r: &BenchResult) -> Result<(), String> {
    for task in r.tasks.iter().filter(|t| !t.tamper_signals.is_empty()) {
        if task
            .tamper_signals
            .iter()
            .any(|s| s.kind == "evidence_mismatch")
        {
            return Err("evidence_mismatch cannot receive a clean review; rerun the task".into());
        }
        let path = results()
            .join(&r.id)
            .join("artifacts")
            .join(&task.id)
            .join("submission.patch");
        let expected = task
            .patch_sha256
            .as_deref()
            .ok_or_else(|| format!("missing patch digest; expected patch at {}", path.display()))?;
        let (hash, _) = suite::artifact(
            &results().join(&r.id).join("artifacts"),
            &format!("{}/submission.patch", task.id),
        )
        .map_err(|_| format!("cannot read saved patch; expected {}", path.display()))?;
        if hash != expected {
            return Err(format!("saved patch digest mismatch: {}", path.display()));
        }
    }
    Ok(())
}
fn result_path(id: &str) -> PathBuf {
    if id.starts_with("b-") && suite::identifier(id) {
        results().join(format!("{id}.json"))
    } else {
        PathBuf::from(id)
    }
}
pub(crate) fn read_result(id: &str) -> Result<BenchResult, String> {
    let path = result_path(id);
    if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
        return Err("result exceeds 16 MiB".into());
    }
    let r = report::parse(&std::fs::read(&path).map_err(|e| e.to_string())?)?;
    if path.file_stem().and_then(|s| s.to_str()) != Some(&r.id) {
        return Err("result id does not match file name".into());
    }
    Ok(r)
}
pub(crate) fn history_rows(limit: usize) -> Result<Vec<Value>, String> {
    let entries = match std::fs::read_dir(results()) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut paths = entries
        .map(|e| e.map(|e| e.path()).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let mut rows = Vec::new();
    for p in paths
        .iter()
        .rev()
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
    {
        if rows.len() == limit {
            break;
        }
        let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
        if serde_json::from_slice::<Value>(&bytes)
            .ok()
            .is_none_or(|value| value["kind"] != "bench")
        {
            continue;
        }
        match read_result(p.to_str().ok_or("non UTF-8 result path")?){Ok(r)=>rows.push(json!({"id":r.id,"edition":r.edition.number,"started_at":r.started_at,"complete":r.complete,"stop":r.stop,"score":r.score,"solved":r.solved,"spent_usd":r.spent_usd,"unpriced_trials":r.unpriced_trials,"over_goal":r.over_goal,"cost":r.cost_text(),"eligible":r.eligible})),Err(e)=>eprintln!("cedr: bench: skipped {e}")}
    }
    Ok(rows)
}
fn export(ids: &[String], out: &Path) -> Result<(), String> {
    if out.exists() {
        return Err("export destination already exists".into());
    }
    let records = ids
        .iter()
        .map(|id| read_result(id))
        .collect::<Result<Vec<_>, _>>()?;
    let originals = ids
        .iter()
        .map(|id| {
            let path = if id.starts_with("b-") && suite::identifier(id) {
                results().join(format!("{id}.json"))
            } else {
                PathBuf::from(id)
            };
            serde_json::from_slice::<Value>(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, String>>()?;
    // Validate every source before creating the destination.
    for r in &records {
        for t in &r.tasks {
            for a in &t.artifacts {
                if a.path.ends_with(".png") {
                    let (hash, bytes) =
                        suite::artifact(&results().join(&r.id).join("artifacts"), &a.path)?;
                    if hash != a.sha256 || bytes != a.bytes {
                        return Err("artifact digest mismatch".into());
                    }
                }
            }
        }
    }
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    for r in &records {
        for t in &r.tasks {
            for a in &t.artifacts {
                if a.path.ends_with(".png") {
                    let relative = Path::new(&a.path)
                        .strip_prefix(&t.id)
                        .map_err(|_| "artifact task directory mismatch")?;
                    let dest = out.join("shots").join(&r.id).join(&t.id).join(relative);
                    std::fs::create_dir_all(dest.parent().ok_or("missing artifact parent")?)
                        .map_err(|e| e.to_string())?;
                    std::fs::copy(results().join(&r.id).join("artifacts").join(&a.path), dest)
                        .map_err(|e| e.to_string())?;
                }
            }
        }
    }
    settings::write_atomic(
        &out.join("results.json"),
        &serde_json::to_vec_pretty(&originals).map_err(|e| e.to_string())?,
    )
}

#[cfg(test)]
mod reference_tests {
    use super::*;
    #[test]
    fn remote_references_use_fresh_data_then_cache_then_no_reference_offline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probes/edition-1.toml");
        assert!(
            cached_references(&path, None)
                .unwrap()
                .references
                .is_empty()
        );
        let published = "[[references]]\nharness='codex'\nharness_version='test'\nmodel='test-model'\ninput_tokens=10\n";
        assert_eq!(
            cached_references(&path, Some(published.into()))
                .unwrap()
                .references[0]
                .input_tokens,
            10
        );
        assert_eq!(
            cached_references(&path, None).unwrap().references[0].input_tokens,
            10
        );
        assert!(cached_references(&path, Some(format!("{published}{published}"))).is_err());
        assert_eq!(cached_references(&path, None).unwrap().references.len(), 1);
    }
}
