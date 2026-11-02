//! The run loop (docs/bench.md, Trials and Budget): plan what a suite would do, then for each task
//! and trial number run the baseline and the candidate, each in its own fresh clone.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::harness::{self, AgentEnd, Usage};
use crate::report::{
    BenchResult, Count, Status, Stop, SuiteInfo, Summary, TaskRow, Trial, VERSION, VariantInfo,
    VariantName, Variants, separated, summarize,
};
use crate::suite::{HarnessKind, Loaded, Task, Variant};
use crate::{Result, exec, invalid};

/// What a run would do, resolved but with nothing started.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub suite: String,
    pub sha256: String,
    pub trials_each: u32,
    pub total_trials: u64,
    pub budget_usd: f64,
    pub budget_source: String,
    pub variants: Variants,
    pub tasks: Vec<PlannedTask>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedTask {
    pub id: String,
    pub setup: Option<String>,
    pub test: String,
    pub test_files: Option<String>,
}

/// Which tasks and how many trials a command asked for.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub tasks: Vec<String>,
    pub trials: Option<u32>,
}

impl Selection {
    fn tasks<'a>(&self, loaded: &'a Loaded) -> Result<Vec<&'a Task>> {
        for id in &self.tasks {
            if !loaded.suite.tasks.iter().any(|t| &t.id == id) {
                return Err(invalid(format!(
                    "{}: no task \"{id}\"",
                    loaded.path.display()
                )));
            }
        }
        Ok(loaded
            .suite
            .tasks
            .iter()
            .filter(|t| self.tasks.is_empty() || self.tasks.contains(&t.id))
            .collect())
    }

    fn trials(&self, loaded: &Loaded) -> Result<u32> {
        let trials = self.trials.unwrap_or(loaded.suite.trials);
        if (1..=crate::suite::MAX_TRIALS).contains(&trials) {
            Ok(trials)
        } else {
            Err(invalid(format!(
                "--trials: must be 1 to {}",
                crate::suite::MAX_TRIALS
            )))
        }
    }
}

fn variant_info(root: &Path, variant: &Variant) -> Result<VariantInfo> {
    let git_ref = variant.git_ref.clone().unwrap_or_else(|| "HEAD".into());
    let commit = exec::resolve_ref(root, &git_ref)?;
    Ok(VariantInfo {
        harness: variant.harness,
        harness_version: harness_version(variant),
        model: variant.model.clone(),
        effort: variant.effort.clone(),
        permissions: variant.permissions,
        config_sha256: exec::config_digest(root, &commit)?,
        git_ref,
        commit,
        resolved_models: Vec::new(),
    })
}

fn harness_version(variant: &Variant) -> Option<String> {
    if variant.harness == HarnessKind::Command {
        return None;
    }
    let mut cmd = Command::new(exec::find_program(harness::program(variant))?);
    cmd.arg("--version");
    let out = exec::run_step(cmd, None, Duration::from_secs(10)).ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    out.succeeded()
        .then(|| text.lines().next().unwrap_or("").trim().to_string())
        .filter(|line| !line.is_empty())
}

fn differ(a: &Variant, b: &Variant, a_commit: &str, b_commit: &str) -> bool {
    a.harness != b.harness
        || a.model != b.model
        || a.effort != b.effort
        || a.permissions != b.permissions
        || a.command != b.command
        || a_commit != b_commit
}

/// Resolves the suite against the repo and lists what a run would do. Starts no agent.
pub fn plan(
    loaded: &Loaded,
    selection: &Selection,
    budget_usd: f64,
    budget_source: &str,
) -> Result<Plan> {
    let suite = &loaded.suite;
    let tasks = selection.tasks(loaded)?;
    let trials_each = selection.trials(loaded)?;
    let baseline = variant_info(&loaded.root, &suite.baseline)?;
    let candidate = variant_info(&loaded.root, &suite.candidate)?;
    if !differ(
        &suite.baseline,
        &suite.candidate,
        &baseline.commit,
        &candidate.commit,
    ) {
        return Err(invalid(format!(
            "{}: baseline and candidate are the same: same fields and the same commit",
            loaded.path.display()
        )));
    }
    let mut warnings = Vec::new();
    for (name, variant) in [
        ("baseline", &suite.baseline),
        ("candidate", &suite.candidate),
    ] {
        let program = harness::program(variant);
        if exec::find_program(program).is_none() {
            warnings.push(format!("{name}: {program} is not installed"));
        }
        if variant.permissions == crate::suite::Permissions::Full {
            warnings.push(format!(
                "{name}: full permissions: the agent runs without its vendor's sandbox"
            ));
        }
        if !harness::priced(variant) {
            warnings.push(format!(
                "{name}: unpriced: the budget cannot see what it spends; a run needs --allow-unpriced"
            ));
        }
    }
    Ok(Plan {
        suite: loaded.name.clone(),
        sha256: loaded.sha256.clone(),
        trials_each,
        total_trials: tasks.len() as u64 * u64::from(trials_each) * 2,
        budget_usd,
        budget_source: budget_source.to_string(),
        variants: Variants {
            baseline,
            candidate,
        },
        tasks: tasks
            .iter()
            .map(|t| PlannedTask {
                id: t.id.clone(),
                setup: t.setup.clone(),
                test: t.test.clone(),
                test_files: t.test_files.clone(),
            })
            .collect(),
        warnings,
    })
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub selection: Selection,
    pub budget_usd: f64,
    pub allow_unpriced: bool,
}

/// Runs the suite, reporting each trial to `progress`. Only a failure that stops the whole run is an
/// error; a trial's own failure is in its status.
pub fn run(
    loaded: &Loaded,
    options: &RunOptions,
    mut progress: impl FnMut(&str),
) -> Result<BenchResult> {
    let suite = &loaded.suite;
    let plan = plan(loaded, &options.selection, options.budget_usd, "")?;
    for (name, variant) in [
        ("baseline", &suite.baseline),
        ("candidate", &suite.candidate),
    ] {
        if exec::find_program(harness::program(variant)).is_none() {
            return Err(invalid(format!(
                "{name}: {} is not installed",
                harness::program(variant)
            )));
        }
        if !options.allow_unpriced && !harness::priced(variant) {
            return Err(invalid(format!(
                "{name}: the budget cannot see what {} spends; pass --allow-unpriced to run it anyway",
                variant.model
            )));
        }
    }
    let tasks = options.selection.tasks(loaded)?;
    let timeout = Duration::from_secs(suite.timeout_seconds);
    let started_at = now();
    let mut trials: Vec<Trial> = Vec::new();
    let mut spent = 0.0_f64;
    let mut dearest = 0.0_f64;
    let mut stop = None;
    let variants = [
        (
            VariantName::Baseline,
            &suite.baseline,
            &plan.variants.baseline,
        ),
        (
            VariantName::Candidate,
            &suite.candidate,
            &plan.variants.candidate,
        ),
    ];
    'run: for task in &tasks {
        let prompt = task.prompt_text(&loaded.root)?;
        for number in 1..=plan.trials_each {
            for (name, variant, info) in variants {
                if over_budget(spent, dearest, options.budget_usd) {
                    stop = Some(Stop::Budget);
                    break 'run;
                }
                let label = format!(
                    "{} {} {number}/{}",
                    task.id,
                    variant_label(name),
                    plan.trials_each
                );
                progress(&format!("{label}: running"));
                let budget_left = options.budget_usd - spent;
                let trial = run_trial(TrialInput {
                    root: &loaded.root,
                    task,
                    prompt: &prompt,
                    variant,
                    commit: &info.commit,
                    name,
                    number,
                    timeout,
                    budget_left,
                });
                if let Some(cost) = trial.cost_usd {
                    spent += cost;
                    dearest = dearest.max(cost);
                }
                progress(&format!("{label}: {}", trial.status.name()));
                trials.push(trial);
            }
        }
    }
    Ok(assemble(
        loaded,
        plan,
        trials,
        spent,
        options.budget_usd,
        stop,
        started_at,
    ))
}

/// Whether the next trial would risk passing the cap: what is spent plus the dearest trial so far.
/// The first trial always runs when the cap is above 0, since nothing is known about its cost.
pub fn over_budget(spent: f64, dearest: f64, budget: f64) -> bool {
    spent >= budget || spent + dearest > budget
}

fn variant_label(name: VariantName) -> &'static str {
    match name {
        VariantName::Baseline => "baseline",
        VariantName::Candidate => "candidate",
    }
}

struct TrialInput<'a> {
    root: &'a Path,
    task: &'a Task,
    prompt: &'a str,
    variant: &'a Variant,
    commit: &'a str,
    name: VariantName,
    number: u32,
    timeout: Duration,
    budget_left: f64,
}

/// Removes what a trial's environment must not carry (docs/bench.md, Trust).
fn scrub(cmd: &mut Command, secrets_too: bool) {
    const SECRET_WORDS: [&str; 6] = [
        "API_KEY",
        "ACCESS_KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "CREDENTIAL",
    ];
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("CEDR_")
            || name.starts_with("GIT_")
            || (secrets_too && SECRET_WORDS.iter().any(|w| name.contains(w)))
        {
            cmd.env_remove(&key);
        }
    }
}

/// Runs one trial. Anything that goes wrong is the trial's status, never an error: a run that has
/// already spent money keeps what it measured.
fn run_trial(input: TrialInput) -> Trial {
    let started = Instant::now();
    let finish = |status, test_exit, usage: Usage| Trial {
        task: input.task.id.clone(),
        variant: input.name,
        trial: input.number,
        status,
        test_exit,
        duration_ms: started.elapsed().as_millis() as u64,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_tokens: usage.cached_tokens,
        cost_usd: usage.cost_usd.map(|c| (c * 1e6).round() / 1e6),
        models: usage.models,
    };
    let unscored = |status| finish(status, None, Usage::default());
    let Ok(dir) = tempfile::Builder::new().prefix("cedr-bench-").tempdir() else {
        return unscored(Status::SetupFailed);
    };
    let clone = dir.path().join("repo");
    let prompt_file = dir.path().join("prompt.md");
    if exec::clone_at(input.root, input.commit, &clone).is_err()
        || std::fs::write(&prompt_file, input.prompt).is_err()
    {
        return unscored(Status::SetupFailed);
    }

    let step = |script: &str| {
        let mut cmd = exec::shell(script);
        cmd.current_dir(&clone);
        scrub(&mut cmd, true);
        exec::run_step(cmd, None, input.timeout)
    };
    if let Some(setup) = &input.task.setup
        && !step(setup).is_ok_and(|out| out.succeeded())
    {
        return unscored(Status::SetupFailed);
    }

    let Some((mut agent, stdin)) =
        harness::command(input.variant, input.prompt, &prompt_file, input.budget_left)
    else {
        return unscored(Status::AgentError);
    };
    agent.current_dir(&clone);
    scrub(&mut agent, false);
    let output = match exec::run_step(agent, stdin.as_deref(), input.timeout) {
        Ok(output) => output,
        Err(_) => return unscored(Status::AgentError),
    };
    let timed_out = output.status.is_none();
    let (end, usage) = harness::parse(input.variant, &output.stdout, output.succeeded());
    if timed_out {
        return finish(Status::Timeout, None, usage);
    }
    match end {
        AgentEnd::Finished => {}
        AgentEnd::Error => return finish(Status::AgentError, None, usage),
        AgentEnd::BudgetLimit => return finish(Status::Budget, None, usage),
    }

    // The agent can leave the clone in a state the copy cannot land on; its cost still counts.
    if let Some(rel) = &input.task.test_files
        && exec::overlay(&input.root.join(rel), &clone).is_err()
    {
        return finish(Status::AgentError, None, usage);
    }
    let Ok(tested) = step(&input.task.test) else {
        return finish(Status::SetupFailed, None, usage);
    };
    let status = match tested.status {
        None => Status::Timeout,
        Some(s) if s.success() => Status::Passed,
        Some(_) => Status::Failed,
    };
    finish(status, tested.status.and_then(|s| s.code()), usage)
}

fn assemble(
    loaded: &Loaded,
    plan: Plan,
    trials: Vec<Trial>,
    spent: f64,
    budget_usd: f64,
    stop: Option<Stop>,
    started_at: String,
) -> BenchResult {
    let baseline = summarize(&trials, VariantName::Baseline);
    let candidate = summarize(&trials, VariantName::Candidate);
    let count = |task: &str, variant: VariantName| -> Count {
        let own = trials
            .iter()
            .filter(|t| t.task == task && t.variant == variant && t.status.scored());
        Count {
            scored: own.clone().count() as u32,
            passed: own.filter(|t| t.status == Status::Passed).count() as u32,
        }
    };
    let tasks = plan
        .tasks
        .iter()
        .map(|t| TaskRow {
            id: t.id.clone(),
            baseline: count(&t.id, VariantName::Baseline),
            candidate: count(&t.id, VariantName::Candidate),
        })
        .collect();
    let resolved = |variant: VariantName| {
        let mut models: Vec<String> = trials
            .iter()
            .filter(|t| t.variant == variant)
            .flat_map(|t| t.models.iter().cloned())
            .collect();
        models.sort();
        models.dedup();
        models
    };
    let mut variants = plan.variants;
    variants.baseline.resolved_models = resolved(VariantName::Baseline);
    variants.candidate.resolved_models = resolved(VariantName::Candidate);
    let complete = stop.is_none() && trials.iter().all(|t| t.status.scored());
    let pass_rate_delta = match (baseline.pass_rate, candidate.pass_rate) {
        (Some(b), Some(c)) => Some(((c - b) * 1000.0).round() / 1000.0),
        _ => None,
    };
    let finished_at = now();
    BenchResult {
        version: VERSION,
        id: new_id(&started_at),
        suite: SuiteInfo {
            name: loaded.name.clone(),
            sha256: loaded.sha256.clone(),
        },
        started_at,
        finished_at,
        budget_usd,
        spent_usd: (spent * 10000.0).round() / 10000.0,
        unpriced_trials: trials.iter().filter(|t| t.cost_usd.is_none()).count() as u32,
        complete,
        stop,
        variants,
        separated: separated(&baseline, &candidate),
        summary: Summary {
            baseline,
            candidate,
        },
        tasks,
        trials,
        pass_rate_delta,
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `b-<UTC time>-<6 hex>`: sorts by time, and is safe in a file name.
fn new_id(started_at: &str) -> String {
    let compact: String = started_at
        .chars()
        .filter(|c| !matches!(c, '-' | ':'))
        .collect();
    let mut hash = Sha256::new();
    hash.update(std::process::id().to_le_bytes());
    hash.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
            .to_le_bytes(),
    );
    format!("b-{compact}-{}", &crate::suite::hex(&hash.finalize())[..6])
}

/// Whether `id` has the shape [`new_id`] gives, so it can name a file under the results directory.
pub fn is_result_id(id: &str) -> bool {
    id.starts_with("b-") && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_stops_before_the_next_trial_could_pass_the_cap() {
        assert!(!over_budget(0.0, 0.0, 2.0), "the first trial always runs");
        assert!(!over_budget(1.0, 0.9, 2.0));
        assert!(
            over_budget(1.2, 0.9, 2.0),
            "the dearest trial would pass it"
        );
        assert!(over_budget(2.0, 0.0, 2.0));
    }

    #[test]
    fn result_ids_are_safe_file_names() {
        let id = new_id("2026-11-01T19:30:00Z");
        assert!(id.starts_with("b-20261101T193000Z-"), "{id}");
        assert!(is_result_id(&id));
        for bad in ["../../etc/passwd", "b-../x", "x", "b-a/b"] {
            assert!(!is_result_id(bad), "{bad}");
        }
    }
}
