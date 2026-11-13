//! Persisted benchmark facts and consistency checks shared by show, history and MCP.
use crate::{
    Result,
    suite::{self, Kind},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tokens {
    pub input_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}
impl Tokens {
    pub fn input(&self) -> Option<u64> {
        self.input_tokens?
            .checked_add(self.cache_read_tokens.unwrap_or(0))?
            .checked_add(self.cache_write_tokens.unwrap_or(0))
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    #[serde(flatten)]
    pub tokens: Tokens,
    pub cost_usd: Option<f64>,
    pub turns: Option<u64>,
    pub tools: Option<u64>,
    pub models: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Graded,
    Timeout,
    AgentError,
    SetupFailed,
    Interrupted,
    NotReached,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub id: String,
    pub passed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gate {
    pub id: String,
    pub passed: bool,
    pub detail: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskResult {
    pub id: String,
    pub kind: Kind,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    pub score: Option<f64>,
    pub solved: bool,
    pub duration_ms: Option<u64>,
    pub turns: Option<u64>,
    #[serde(flatten)]
    pub tokens: Tokens,
    pub cost_usd: Option<f64>,
    pub patch_sha256: Option<String>,
    pub gates: Vec<Gate>,
    pub artifacts: Vec<Artifact>,
}
impl TaskResult {
    pub fn pending(id: &str, kind: Kind) -> Self {
        Self {
            id: id.into(),
            kind,
            status: Status::NotReached,
            checks: Vec::new(),
            score: if kind == Kind::Code { Some(0.0) } else { None },
            solved: false,
            duration_ms: None,
            turns: None,
            tokens: Tokens::default(),
            cost_usd: None,
            patch_sha256: None,
            gates: Vec::new(),
            artifacts: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Match,
    Mismatch,
    NoReference,
    Reference,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub input_tokens: Option<u64>,
    pub tools: Option<u64>,
    pub reference: Option<u64>,
    pub status: ProbeStatus,
    pub tools_ok: bool,
    pub cost_usd: Option<f64>,
    pub fingerprint_cost_usd: Option<f64>,
    pub tools_cost_usd: Option<f64>,
}
impl Probe {
    pub fn passed(&self) -> bool {
        self.tools_ok && matches!(self.status, ProbeStatus::Match | ProbeStatus::Reference)
    }
}
pub fn probe(usage: &Usage, reference: Option<u64>, set: bool, tools_ok: bool) -> Probe {
    let input = usage.tokens.input();
    let status = match (input, reference) {
        (Some(_), _) if set => ProbeStatus::Reference,
        (Some(a), Some(b)) if a.abs_diff(b) <= 16 => ProbeStatus::Match,
        (_, Some(_)) => ProbeStatus::Mismatch,
        _ => ProbeStatus::NoReference,
    };
    Probe {
        input_tokens: input,
        tools: usage.tools,
        reference: if set { input } else { reference },
        status,
        tools_ok,
        cost_usd: usage.cost_usd,
        fingerprint_cost_usd: usage.cost_usd,
        tools_cost_usd: Some(0.0),
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditionInfo {
    pub number: u32,
    pub suite_sha256: String,
    pub commit: Option<String>,
    pub image: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub harness: String,
    pub harness_version: String,
    pub model: String,
    pub effort: Option<String>,
    pub resolved_models: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub mode: String,
    pub clean_home: bool,
    pub network: String,
    pub pinned: bool,
    pub agent_image: Option<String>,
    pub os: String,
    pub arch: String,
    pub cli: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    UserStop,
    UnknownCost,
    Interrupted,
    ProbeFailed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchResult {
    pub version: u32,
    pub kind: String,
    pub id: String,
    pub edition: EditionInfo,
    pub config: Config,
    pub environment: Environment,
    pub probe: Probe,
    pub billing: String,
    pub started_at: String,
    pub finished_at: String,
    pub goal_usd: f64,
    pub stop_usd: Option<f64>,
    pub spent_usd: f64,
    pub over_goal: bool,
    pub unpriced_trials: usize,
    pub complete: bool,
    pub stop: Option<Stop>,
    pub score: f64,
    pub solved: usize,
    pub code_tasks: usize,
    pub eligible: bool,
    pub tasks: Vec<TaskResult>,
}
impl BenchResult {
    pub fn summarize(&mut self) {
        self.code_tasks = self.tasks.iter().filter(|t| t.kind == Kind::Code).count();
        self.score = self.tasks.iter().filter_map(|t| t.score).sum::<f64>() * 100.0
            / self.code_tasks.max(1) as f64;
        self.solved = self.tasks.iter().filter(|t| t.solved).count();
        self.spent_usd = self.probe.fingerprint_cost_usd.unwrap_or(0.0)
            + self.probe.tools_cost_usd.unwrap_or(0.0)
            + self.tasks.iter().filter_map(|t| t.cost_usd).sum::<f64>();
        self.unpriced_trials = usize::from(self.probe.fingerprint_cost_usd.is_none())
            + usize::from(self.probe.tools_cost_usd.is_none())
            + self
                .tasks
                .iter()
                .filter(|t| t.status != Status::NotReached && t.cost_usd.is_none())
                .count();
        self.over_goal = self.spent_usd > self.goal_usd;
        self.complete =
            self.stop.is_none() && self.tasks.iter().all(|t| t.status == Status::Graded);
        self.eligible = self.complete
            && self.environment.mode == "container"
            && self.environment.pinned
            && self.environment.clean_home
            && self.environment.network == "allowlist"
            && self.probe.tools_ok
            && matches!(
                self.probe.status,
                ProbeStatus::Match | ProbeStatus::Reference
            )
            && !matches!(
                self.config.harness.as_str(),
                "command" | "gemini" | "antigravity"
            );
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.kind != "bench"
            || !self.id.starts_with("b-")
            || !suite::identifier(&self.id)
            || self.tasks.is_empty()
            || !self.goal_usd.is_finite()
            || self.goal_usd <= 0.0
            || self.stop_usd.is_some_and(|v| !v.is_finite() || v <= 0.0)
        {
            return Err("invalid benchmark header".into());
        }
        if !matches!(self.environment.mode.as_str(), "container" | "local")
            || !matches!(self.billing.as_str(), "plan" | "api")
        {
            return Err("invalid environment or billing".into());
        }
        let mut ids = BTreeSet::new();
        for t in &self.tasks {
            if !suite::identifier(&t.id) || !ids.insert(&t.id) {
                return Err("invalid or duplicate task id".into());
            }
            if t.cost_usd.is_some_and(|n| !n.is_finite() || n < 0.0)
                || matches!(
                    t.status,
                    Status::NotReached | Status::Timeout | Status::Interrupted
                ) && t.cost_usd.is_some()
            {
                return Err("invalid task cost".into());
            }
            let mut checks = BTreeSet::new();
            if t.checks
                .iter()
                .any(|c| c.id.is_empty() || !checks.insert(&c.id))
            {
                return Err("duplicate check".into());
            }
            let expected = if t.kind == Kind::Code {
                if t.status == Status::Graded && t.checks.len() != 4 {
                    return Err("code grader requires four checks".into());
                }
                Some(if t.status == Status::Graded {
                    t.checks.iter().filter(|c| c.passed).count() as f64 / 4.0
                } else {
                    0.0
                })
            } else {
                if !t.checks.is_empty() {
                    return Err("visual task has checks".into());
                }
                None
            };
            if t.score != expected || t.solved != (expected == Some(1.0)) {
                return Err("inconsistent task score".into());
            }
            for a in &t.artifacts {
                if !suite::relative(&a.path)
                    || !digest(&a.sha256)
                    || !std::path::Path::new(&a.path).starts_with(&t.id)
                {
                    return Err("invalid artifact".into());
                }
            }
            if t.patch_sha256.as_ref().is_some_and(|s| !digest(s)) {
                return Err("invalid patch digest".into());
            }
        }
        for cost in [
            self.probe.cost_usd,
            self.probe.fingerprint_cost_usd,
            self.probe.tools_cost_usd,
        ]
        .into_iter()
        .flatten()
        {
            if !cost.is_finite() || cost < 0.0 {
                return Err("invalid probe cost".into());
            }
        }
        if self.probe.cost_usd
            != self
                .probe
                .fingerprint_cost_usd
                .zip(self.probe.tools_cost_usd)
                .map(|(a, b)| a + b)
        {
            return Err("inconsistent probe cost".into());
        }
        if matches!(
            self.probe.status,
            ProbeStatus::Match | ProbeStatus::Reference
        ) && self
            .probe
            .input_tokens
            .zip(self.probe.reference)
            .is_none_or(|(a, b)| a.abs_diff(b) > 16)
        {
            return Err("inconsistent fingerprint".into());
        }
        if self.stop == Some(Stop::UserStop) && self.stop_usd.is_none_or(|n| self.spent_usd < n) {
            return Err("inconsistent user stop".into());
        }
        if self.stop == Some(Stop::UnknownCost) && self.unpriced_trials == 0 {
            return Err("unknown-cost stop without unpriced usage".into());
        }
        if !digest(&self.edition.suite_sha256) || self.code_tasks == 0 {
            return Err("invalid edition digest or code task count".into());
        }
        let started = chrono::DateTime::parse_from_rfc3339(&self.started_at)
            .map_err(|_| "invalid started_at")?;
        let finished = chrono::DateTime::parse_from_rfc3339(&self.finished_at)
            .map_err(|_| "invalid finished_at")?;
        if finished < started {
            return Err("finished_at precedes started_at".into());
        }
        let mut derived = self.clone();
        derived.summarize();
        if self.score != derived.score
            || self.solved != derived.solved
            || self.code_tasks != derived.code_tasks
            || self.spent_usd != derived.spent_usd
            || self.over_goal != derived.over_goal
            || self.unpriced_trials != derived.unpriced_trials
            || self.complete != derived.complete
            || self.eligible != derived.eligible
        {
            return Err("benchmark summary disagrees with task records".into());
        }
        Ok(())
    }
}
fn digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
pub fn parse(bytes: &[u8]) -> Result<BenchResult> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("result exceeds 16 MiB".into());
    }
    let result: BenchResult = serde_json::from_slice(bytes).map_err(|e| {
        format!(
            "invalid Bench JSON at line {}, column {}",
            e.line(),
            e.column()
        )
    })?;
    result.validate()?;
    Ok(result)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grade {
    #[serde(default)]
    pub checks: Vec<Check>,
    #[serde(default)]
    pub gates: Vec<Gate>,
    #[serde(default)]
    pub artifacts: Vec<String>,
}
pub fn grade(bytes: &[u8], kind: Kind) -> Result<Grade> {
    let grade: Grade = serde_json::from_slice(bytes).map_err(|e| {
        format!(
            "malformed grader output at line {}, column {}",
            e.line(),
            e.column()
        )
    })?;
    let mut ids = BTreeSet::new();
    if kind == Kind::Code
        && (grade.checks.len() != 4 || !grade.gates.is_empty() || !grade.artifacts.is_empty())
        || kind == Kind::Visual && (!grade.checks.is_empty() || grade.gates.is_empty())
        || grade
            .checks
            .iter()
            .any(|c| !suite::identifier(&c.id) || !ids.insert(&c.id))
    {
        return Err("grader must emit four unique code checks or visual gates".into());
    }
    ids.clear();
    if grade
        .gates
        .iter()
        .any(|g| !suite::identifier(&g.id) || !ids.insert(&g.id))
        || grade.artifacts.iter().any(|p| !suite::relative(p))
    {
        return Err("invalid grader gate or artifact".into());
    }
    Ok(grade)
}
