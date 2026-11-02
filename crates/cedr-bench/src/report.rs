//! Results (docs/bench.md, Result): the shape saved and printed, the counts behind it, and the text
//! report. Facts only: a difference is stated with its uncertainty and never called an improvement.

use serde::{Deserialize, Serialize};

use crate::suite::{HarnessKind, Permissions};

pub const VERSION: u32 = 1;
/// A result file larger than this is refused rather than read.
pub const MAX_RESULT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VariantName {
    Baseline,
    Candidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Passed,
    Failed,
    Timeout,
    SetupFailed,
    AgentError,
    Budget,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Timeout => "timeout",
            Status::SetupFailed => "setup_failed",
            Status::AgentError => "agent_error",
            Status::Budget => "budget",
        }
    }

    /// An unscored trial says nothing about the variant, only that the run did not finish its work.
    pub fn scored(self) -> bool {
        matches!(self, Status::Passed | Status::Failed | Status::Timeout)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    Budget,
    UnknownCost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOn {
    Never,
    Lower,
    Separated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteInfo {
    pub name: String,
    pub sha256: String,
}

/// Everything needed to reproduce a variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantInfo {
    pub harness: HarnessKind,
    pub harness_version: Option<String>,
    pub model: String,
    pub effort: Option<String>,
    pub permissions: Permissions,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub commit: String,
    pub config_sha256: String,
    pub resolved_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variants {
    pub baseline: VariantInfo,
    pub candidate: VariantInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trial {
    pub task: String,
    pub variant: VariantName,
    pub trial: u32,
    pub status: Status,
    pub test_exit: Option<i32>,
    pub duration_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub models: Vec<String>,
}

impl Trial {
    /// Whether an agent may have spent money this trial did not report. A failed setup never started one.
    pub fn spent_unknown(&self) -> bool {
        self.cost_usd.is_none() && self.status != Status::SetupFailed
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VariantSummary {
    pub scored: u32,
    pub passed: u32,
    pub pass_rate: Option<f64>,
    pub interval: Option<[f64; 2]>,
    pub unscored: u32,
    pub duration_ms_median: Option<u64>,
    pub cost_usd: Option<f64>,
    pub tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub baseline: VariantSummary,
    pub candidate: VariantSummary,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Count {
    pub scored: u32,
    pub passed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TaskRow {
    pub id: String,
    pub baseline: Count,
    pub candidate: Count,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchResult {
    pub version: u32,
    pub id: String,
    pub suite: SuiteInfo,
    pub started_at: String,
    pub finished_at: String,
    pub budget_usd: f64,
    pub spent_usd: f64,
    pub unpriced_trials: u32,
    pub complete: bool,
    pub stop: Option<Stop>,
    pub variants: Variants,
    pub trials: Vec<Trial>,
    pub summary: Summary,
    pub tasks: Vec<TaskRow>,
    pub pass_rate_delta: Option<f64>,
    pub separated: bool,
}

fn round3(n: f64) -> f64 {
    (n * 1000.0).round() / 1000.0
}

/// A 95% Wilson score interval: unlike the normal approximation, it behaves at 0 and at n passes,
/// which small trial counts reach all the time.
pub fn wilson(passed: u32, scored: u32) -> Option<[f64; 2]> {
    if scored == 0 {
        return None;
    }
    const Z: f64 = 1.959964;
    let n = f64::from(scored);
    let p = f64::from(passed) / n;
    let z2 = Z * Z;
    let denominator = 1.0 + z2 / n;
    let centre = p + z2 / (2.0 * n);
    let margin = Z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    Some([
        round3(((centre - margin) / denominator).max(0.0)),
        round3(((centre + margin) / denominator).min(1.0)),
    ])
}

fn median(mut values: Vec<u64>) -> Option<u64> {
    values.sort_unstable();
    match values.len() {
        0 => None,
        n if n % 2 == 1 => Some(values[n / 2]),
        n => Some((values[n / 2 - 1] + values[n / 2]) / 2),
    }
}

pub fn summarize(trials: &[Trial], variant: VariantName) -> VariantSummary {
    let own: Vec<&Trial> = trials.iter().filter(|t| t.variant == variant).collect();
    let scored = own.iter().filter(|t| t.status.scored()).count() as u32;
    let passed = own.iter().filter(|t| t.status == Status::Passed).count() as u32;
    let costs: Vec<f64> = own.iter().filter_map(|t| t.cost_usd).collect();
    VariantSummary {
        scored,
        passed,
        pass_rate: (scored > 0).then(|| round3(f64::from(passed) / f64::from(scored))),
        interval: wilson(passed, scored),
        unscored: own.len() as u32 - scored,
        duration_ms_median: median(own.iter().map(|t| t.duration_ms).collect()),
        cost_usd: (!costs.is_empty()).then(|| costs.iter().sum()),
        tokens: own
            .iter()
            .map(|t| {
                t.input_tokens.unwrap_or(0)
                    + t.output_tokens.unwrap_or(0)
                    + t.cached_tokens.unwrap_or(0)
            })
            .sum(),
    }
}

/// True when both intervals exist and do not overlap.
pub fn separated(baseline: &VariantSummary, candidate: &VariantSummary) -> bool {
    match (baseline.interval, candidate.interval) {
        (Some([b_lo, b_hi]), Some([c_lo, c_hi])) => b_hi < c_lo || c_hi < b_lo,
        _ => false,
    }
}

impl BenchResult {
    /// The exit code for this result (docs/bench.md, `bench run`). Incomplete wins over any
    /// comparison: a partial result never passes.
    pub fn exit_code(&self, fail_on: FailOn) -> u8 {
        if !self.complete {
            return 3;
        }
        let (b, c) = (&self.summary.baseline, &self.summary.candidate);
        // From the counts: rounded rates can hide a regression across many trials.
        let lower = b.scored > 0
            && c.scored > 0
            && u64::from(c.passed) * u64::from(b.scored)
                < u64::from(b.passed) * u64::from(c.scored);
        let held = match fail_on {
            FailOn::Never => false,
            FailOn::Lower => lower,
            FailOn::Separated => lower && separated(b, c),
        };
        u8::from(held)
    }

    pub fn unscored(&self) -> u32 {
        self.summary.baseline.unscored + self.summary.candidate.unscored
    }

    /// The text report. Result files are untrusted input, so nothing from one reaches the terminal
    /// without [`clean`].
    pub fn text(&self) -> String {
        let state = match (self.complete, self.stop) {
            (true, _) => "complete".to_string(),
            (false, Some(Stop::Budget)) => "incomplete (budget)".to_string(),
            (false, Some(Stop::UnknownCost)) => "incomplete (unknown cost)".to_string(),
            (false, None) => format!("incomplete ({} unscored)", self.unscored()),
        };
        let mut out = format!(
            "bench {}: {state}, {} of {} ({})\n",
            clean(&self.suite.name),
            money(self.spent_usd),
            money(self.budget_usd),
            clean(&self.id)
        );
        if self.unpriced_trials > 0 {
            out.push_str(&format!(
                "{} trials reported no cost, so the real spend is higher\n",
                self.unpriced_trials
            ));
        }
        out.push_str("              passed  rate   95%          median  cost\n");
        for (name, row) in [
            ("baseline", &self.summary.baseline),
            ("candidate", &self.summary.candidate),
        ] {
            let rate = row
                .pass_rate
                .map_or("-".to_string(), |r| format!("{:.0}%", r * 100.0));
            let interval = row.interval.map_or("-".to_string(), |[lo, hi]| {
                format!("{:.0}%-{:.0}%", lo * 100.0, hi * 100.0)
            });
            let median = row
                .duration_ms_median
                .map_or("-".to_string(), |ms| format!("{}s", ms / 1000));
            let cost = row.cost_usd.map_or("-".to_string(), money);
            out.push_str(&format!(
                "  {name:<11} {:<7} {rate:<6} {interval:<12} {median:<7} {cost}\n",
                format!("{}/{}", row.passed, row.scored)
            ));
        }
        for task in &self.tasks {
            out.push_str(&format!(
                "  {}  baseline {}/{}, candidate {}/{}\n",
                clean(&task.id),
                task.baseline.passed,
                task.baseline.scored,
                task.candidate.passed,
                task.candidate.scored
            ));
        }
        match self.pass_rate_delta {
            Some(delta) if self.separated => out.push_str(&format!(
                "pass rate {:+.0} points; the intervals do not overlap\n",
                delta * 100.0
            )),
            Some(delta) => out.push_str(&format!(
                "pass rate {:+.0} points; the intervals overlap, so this run does not separate them\n",
                delta * 100.0
            )),
            None => out.push_str("no pass rate to compare\n"),
        }
        out
    }
}

/// Dollars and cents, with more places below a cent so a cheap run does not read as free.
pub fn money(usd: f64) -> String {
    if usd > 0.0 && usd < 0.01 {
        format!("${usd:.4}")
    } else {
        format!("${usd:.2}")
    }
}

/// Drops control characters, so a hostile result file cannot write terminal escapes.
pub fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// Validates a result read from disk: the shape comes from serde, and the verdict fields must
/// follow from the trials, because an exit code is decided from them.
pub fn parse(bytes: &[u8]) -> Result<BenchResult, String> {
    let result: BenchResult =
        serde_json::from_slice(bytes).map_err(|e| format!("not a bench result: {e}"))?;
    if result.version != VERSION {
        return Err(format!(
            "unknown result version {}, this cedr reads {VERSION}",
            result.version
        ));
    }
    let complete = result.stop.is_none() && result.trials.iter().all(|t| t.status.scored());
    if result.complete != complete {
        return Err("complete does not match its stop and trials".into());
    }
    let baseline = summarize(&result.trials, VariantName::Baseline);
    let candidate = summarize(&result.trials, VariantName::Candidate);
    if result.summary.baseline != baseline
        || result.summary.candidate != candidate
        || result.separated != separated(&baseline, &candidate)
    {
        return Err("summary does not match its trials".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trial(variant: VariantName, status: Status, ms: u64, cost: Option<f64>) -> Trial {
        Trial {
            task: "t".into(),
            variant,
            trial: 1,
            status,
            test_exit: None,
            duration_ms: ms,
            input_tokens: Some(10),
            output_tokens: Some(5),
            cached_tokens: None,
            cost_usd: cost,
            models: vec![],
        }
    }

    #[test]
    fn wilson_matches_known_values_and_handles_the_edges() {
        assert_eq!(wilson(2, 3), Some([0.208, 0.939]));
        assert_eq!(wilson(3, 3), Some([0.439, 1.0]));
        assert_eq!(wilson(0, 3), Some([0.0, 0.561]));
        assert_eq!(wilson(0, 0), None);
    }

    #[test]
    fn unscored_trials_stay_out_of_the_rate_and_a_timeout_counts_against_it() {
        use Status::*;
        let trials = [
            trial(VariantName::Baseline, Passed, 100, Some(0.5)),
            trial(VariantName::Baseline, Timeout, 300, None),
            trial(VariantName::Baseline, AgentError, 200, Some(0.25)),
            trial(VariantName::Candidate, Passed, 1, Some(9.0)),
        ];
        let s = summarize(&trials, VariantName::Baseline);
        assert_eq!((s.passed, s.scored, s.unscored), (1, 2, 1));
        assert_eq!(s.pass_rate, Some(0.5));
        assert_eq!(s.duration_ms_median, Some(200));
        assert_eq!(s.cost_usd, Some(0.75));
        assert_eq!(s.tokens, 45);
    }

    fn result(baseline: (u32, u32), candidate: (u32, u32), complete: bool) -> BenchResult {
        let row = |(passed, scored): (u32, u32)| VariantSummary {
            scored,
            passed,
            pass_rate: (scored > 0).then(|| f64::from(passed) / f64::from(scored)),
            interval: wilson(passed, scored),
            unscored: 0,
            duration_ms_median: None,
            cost_usd: None,
            tokens: 0,
        };
        let info = VariantInfo {
            harness: HarnessKind::Claude,
            harness_version: None,
            model: "m".into(),
            effort: None,
            permissions: Permissions::Workspace,
            git_ref: "HEAD".into(),
            commit: "c".into(),
            config_sha256: "s".into(),
            resolved_models: vec![],
        };
        let (b, c) = (row(baseline), row(candidate));
        BenchResult {
            version: VERSION,
            id: "b-x".into(),
            suite: SuiteInfo {
                name: "s".into(),
                sha256: "h".into(),
            },
            started_at: String::new(),
            finished_at: String::new(),
            budget_usd: 2.0,
            spent_usd: 0.0,
            unpriced_trials: 0,
            complete,
            stop: None,
            variants: Variants {
                baseline: info.clone(),
                candidate: info,
            },
            trials: vec![],
            pass_rate_delta: None,
            separated: separated(&b, &c),
            summary: Summary {
                baseline: b,
                candidate: c,
            },
            tasks: vec![],
        }
    }

    #[test]
    fn exit_codes_follow_the_contract() {
        let lower_close = result((3, 3), (2, 3), true);
        assert_eq!(lower_close.exit_code(FailOn::Never), 0);
        assert_eq!(lower_close.exit_code(FailOn::Lower), 1);
        assert_eq!(lower_close.exit_code(FailOn::Separated), 0);

        let lower_far = result((20, 20), (0, 20), true);
        assert!(lower_far.separated);
        assert_eq!(lower_far.exit_code(FailOn::Separated), 1);

        let higher_far = result((0, 20), (20, 20), true);
        assert_eq!(higher_far.exit_code(FailOn::Separated), 0);

        let nothing_scored = result((0, 0), (0, 0), true);
        assert_eq!(nothing_scored.exit_code(FailOn::Lower), 0);

        assert_eq!(result((3, 3), (3, 3), false).exit_code(FailOn::Never), 3);
    }

    #[test]
    fn a_cheap_run_does_not_read_as_free() {
        assert_eq!(money(0.0044), "$0.0044");
        assert_eq!(money(0.0), "$0.00");
        assert_eq!(money(1.274), "$1.27");
    }

    #[test]
    fn the_report_states_uncertainty_and_never_a_verdict() {
        let mut r = result((2, 3), (3, 3), true);
        r.pass_rate_delta = Some(0.333);
        let text = r.text();
        assert!(
            text.contains("pass rate +33 points; the intervals overlap"),
            "{text}"
        );
        for word in ["better", "safe", "improved", "approved"] {
            assert!(!text.to_lowercase().contains(word), "{text}");
        }
    }

    #[test]
    fn a_hostile_result_cannot_write_terminal_escapes_or_pass_as_another_version() {
        let mut r = result((0, 0), (0, 0), true);
        r.suite.name = "evil\u{1b}]0;owned\u{7}".into();
        assert!(!r.text().contains('\u{1b}'));
        let json = serde_json::to_vec(&r).unwrap();
        assert!(parse(&json).is_ok());
        r.version = 2;
        let err = parse(&serde_json::to_vec(&r).unwrap()).unwrap_err();
        assert!(err.contains("version 2"), "{err}");
        assert!(parse(b"{}").is_err());
    }

    #[test]
    fn a_result_file_cannot_claim_a_pass_its_trials_do_not_support() {
        use Status::*;
        let bytes = |r: &BenchResult| serde_json::to_vec(r).unwrap();
        let mut r = result((0, 0), (0, 0), true);
        r.trials = vec![trial(VariantName::Baseline, AgentError, 1, None)];
        r.summary.baseline = summarize(&r.trials, VariantName::Baseline);
        assert!(parse(&bytes(&r)).unwrap_err().contains("complete"));
        r.complete = false;
        assert!(parse(&bytes(&r)).is_ok());

        let mut stopped = result((0, 0), (0, 0), true);
        stopped.stop = Some(Stop::Budget);
        assert!(parse(&bytes(&stopped)).unwrap_err().contains("complete"));

        let mut inflated = result((0, 0), (0, 0), true);
        inflated.trials = vec![trial(VariantName::Candidate, Failed, 1, None)];
        inflated.summary.candidate = VariantSummary {
            passed: 1,
            ..summarize(&inflated.trials, VariantName::Candidate)
        };
        assert!(parse(&bytes(&inflated)).unwrap_err().contains("summary"));
    }

    #[test]
    fn lower_is_decided_from_counts_rounded_rates_would_hide() {
        let mut r = result((1500, 3000), (1499, 3000), true);
        r.summary.baseline.pass_rate = Some(0.5);
        r.summary.candidate.pass_rate = Some(0.5);
        assert_eq!(r.exit_code(FailOn::Lower), 1);
    }
}
