//! The pick, manual tags, pool rows and route listings (docs/route.md, Pick and Route tag).

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::config::{Candidate, Config, Preview, Route};
use crate::error::{Error, Result};
use crate::pool::{Pool, State, pool, resolve, round_pct, versions};
use crate::readings::Readings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Small,
    Medium,
    Large,
}

impl Size {
    pub fn as_str(self) -> &'static str {
        match self {
            Size::Small => "small",
            Size::Medium => "medium",
            Size::Large => "large",
        }
    }

    pub fn parse(s: &str) -> Result<Size> {
        match s {
            "small" => Ok(Size::Small),
            "medium" => Ok(Size::Medium),
            "large" => Ok(Size::Large),
            _ => Err(Error::Usage(format!(
                "size {s:?} is not small, medium or large"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct T3Target {
    #[serde(rename = "providerInstanceId")]
    pub provider_instance_id: String,
    pub model: String,
    pub options: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Delegate {
    pub id: String,
    pub target: String,
    pub harness: String,
    pub model: String,
    pub effort: String,
    pub state: State,
    pub reason: String,
    /// The model is a version the last evaluation did not list (docs/route.md, Preview versions).
    pub preview: bool,
    pub summary: String,
    pub tag: String,
    pub prefix: String,
    pub t3: T3Target,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pick {
    pub run: Option<String>,
    #[serde(skip)]
    pub judges: Option<String>,
    #[serde(skip)]
    pub escalates: Option<String>,
    pub route: String,
    pub work: String,
    pub role: String,
    pub rules: String,
    pub orchestrator: Option<String>,
    pub delegates: Vec<Delegate>,
    pub skipped: Vec<String>,
    pub reason: Option<String>,
    /// A cascade route's judge and escalation; none when the work is kept.
    pub cascade: Option<Cascade>,
    /// Stale evaluation and failed readings sources, so an orchestrator sees them too.
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub council: bool,
}

/// Where a cascade's draft goes to be judged, and where work the judge escalates goes.
#[derive(Debug, Clone, Serialize)]
pub struct Cascade {
    pub judge: String,
    /// None: the orchestrator takes escalated work.
    pub escalate: Option<String>,
}

/// Everything a pick reads, so the CLI and the MCP server share one call.
pub struct Context<'a> {
    pub config: &'a Config,
    pub rules: &'a str,
    pub readings: &'a Readings,
    pub now: DateTime<Utc>,
    /// Copied into every pick so the caller cannot drop them.
    pub warnings: &'a [String],
}

/// A fresh pick id: `p-` and 12 lowercase hex characters.
pub fn new_id() -> String {
    let mut bytes = [0u8; 6];
    // The OS random source only fails on broken systems; a clock-derived id is still unique enough
    // to link a verdict to its pick.
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = Utc::now().timestamp_nanos_opt().unwrap_or_default();
        bytes.copy_from_slice(&nanos.to_le_bytes()[..6]);
    }
    format!(
        "p-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

/// An eligible candidate in a tier, with its resolved model and pool.
struct Choice<'a> {
    harness: &'a str,
    model: String,
    /// `model` is a version the last evaluation did not list.
    preview: bool,
    effort: &'a str,
    pool: Pool,
    /// An API-key harness: ready, but after the tier's ready plans, so plan headroom is used first.
    api: bool,
}

impl Choice<'_> {
    /// Sort key for the least used plan; an unknown use sorts last and is shown as `?`.
    fn usage_key(&self) -> f64 {
        self.pool.usage.unwrap_or(1000.0)
    }

    /// Order within a tier: ready plans, then API keys, then plans by worsening state; then least used.
    fn rank(&self) -> (u8, f64) {
        let group = match (self.pool.state, self.api) {
            (State::Ready, false) => 0,
            (State::Ready, true) => 1,
            (State::Unknown, _) => 2,
            (State::Conserve, _) => 3,
            (State::Out, _) => 4,
            (State::Absent, _) => 5,
        };
        (group, if self.api { 0.0 } else { self.usage_key() })
    }

    /// The use shown in a balanced reason: `key` for an API key, `?` when unknown.
    fn shown(&self) -> String {
        match (self.api, self.pool.usage) {
            (true, _) => "key".to_string(),
            (false, Some(u)) => format!("{}%", round_pct(u)),
            (false, None) => "?".to_string(),
        }
    }
}

impl Context<'_> {
    fn is_api(&self, harness: &str) -> bool {
        self.config
            .harnesses
            .get(harness)
            .is_some_and(|h| h.billing == crate::config::Billing::Api)
    }

    fn orchestrator(&self) -> Option<String> {
        let o = self.config.orchestrator.as_ref()?;
        let model = resolve(&o.harness, &o.model, self.readings).unwrap_or_else(|| o.model.clone());
        Some(pool(&o.harness, &model, self.readings, self.config, self.now).summary)
    }

    fn delegate(&self, route: &str, c: &Choice, reason: &str, orch: Option<&str>) -> Delegate {
        let (harness, model, effort, p) = (c.harness, c.model.as_str(), c.effort, &c.pool);
        let h = &self.config.harnesses[harness];
        let id = new_id();
        let target = format!("{harness}/{model}");
        let effort_part = if effort.is_empty() {
            String::new()
        } else {
            format!(" {effort}")
        };
        let orch_part = orch.map(|o| format!("; {o}")).unwrap_or_default();
        let reason = if c.preview {
            format!("{reason} (preview)")
        } else {
            reason.to_string()
        };
        let mut options = Map::new();
        if let Some(opt) = h.effort_option.as_ref().filter(|_| !effort.is_empty()) {
            options.insert(opt.clone(), Value::String(effort.to_string()));
        }
        Delegate {
            tag: format!(
                "[route {route} -> {target}{effort_part} | {reason} | {}{orch_part} | rules {} | pick {id}]",
                p.summary, self.rules
            ),
            id,
            target,
            harness: harness.to_string(),
            model: model.to_string(),
            effort: effort.to_string(),
            state: p.state,
            reason,
            preview: c.preview,
            summary: p.summary.clone(),
            prefix: h.brief_prefix.clone(),
            t3: T3Target {
                provider_instance_id: h.t3.clone().unwrap_or_else(|| harness.to_string()),
                model: model.to_string(),
                options,
            },
        }
    }

    /// The model a route target resolves to for a pick of `size`, and whether it is a preview
    /// version: one `[evaluation] models` does not list, held back to the newest listed version
    /// except for small trial picks or when the harness offers no listed version.
    fn resolve_for(&self, harness: &str, name: &str, size: Size) -> Option<(String, bool)> {
        let all = versions(harness, name, self.readings)?;
        let newest = all.first()?.clone();
        let mode = match &self.config.evaluation {
            Some(ev) if !ev.models.is_empty() => ev.preview,
            _ => Preview::Use,
        };
        if mode == Preview::Use || newest == name || self.config.evaluated(harness, &newest) {
            return Some((newest, false));
        }
        if mode == Preview::Trial && size == Size::Small {
            return Some((newest, true));
        }
        Some(
            match all.into_iter().find(|m| self.config.evaluated(harness, m)) {
                Some(held) => (held, false),
                None => (newest, true),
            },
        )
    }

    /// Walks the route's tiers in order. In a tier, eligible candidates compete: the best state
    /// wins, then the least used plan right now, so equally suited models spread work across every
    /// plan. `self` ends the walk with the orchestrator keeping the work. A council takes every
    /// eligible member.
    pub fn pick(&self, route_id: &str, size: Size, avoid: &[String]) -> Result<Pick> {
        let route = self.config.routes.get(route_id).ok_or_else(|| {
            let known: Vec<&str> = self.config.routes.keys().map(String::as_str).collect();
            Error::Usage(format!(
                "unknown route {route_id:?}; routes: {}",
                known.join(", ")
            ))
        })?;
        let orch = self.orchestrator();
        let mut result = Pick {
            route: route_id.to_string(),
            work: route.work.clone(),
            role: route.role.clone(),
            rules: self.rules.to_string(),
            orchestrator: orch.clone(),
            delegates: Vec::new(),
            skipped: Vec::new(),
            reason: None,
            run: route
                .judge
                .as_ref()
                .map(|_| new_id().replacen("p-", "r-", 1)),
            judges: None,
            escalates: None,
            cascade: None,
            warnings: self.warnings.to_vec(),
            council: route.council,
        };
        if !route.enabled {
            result.reason = Some("disabled".into());
            return Ok(result);
        }
        for group in tiers(&route.candidates) {
            if group[0].target == "self" {
                break;
            }
            let mut options: Vec<Choice> = Vec::new();
            for c in group {
                let (harness, name) = c.target.split_once('/').expect("validated target");
                if avoid.iter().any(|a| a == harness) {
                    result.skipped.push(format!("{harness}=avoided"));
                    continue;
                }
                let Some((model, preview)) = self.resolve_for(harness, name, size) else {
                    result.skipped.push(format!("{harness}=unresolved"));
                    continue;
                };
                let p = pool(harness, &model, self.readings, self.config, self.now);
                let eligible = match p.state {
                    State::Ready => true,
                    State::Conserve => size == Size::Small,
                    State::Unknown => size != Size::Large,
                    State::Out | State::Absent => false,
                };
                if eligible {
                    options.push(Choice {
                        harness,
                        model,
                        preview,
                        effort: &c.effort,
                        pool: p,
                        api: self.is_api(harness),
                    });
                } else {
                    result
                        .skipped
                        .push(format!("{harness}={}", p.state.as_str()));
                }
            }
            if route.council {
                for c in &options {
                    result
                        .delegates
                        .push(self.delegate(route_id, c, "council", orch.as_deref()));
                }
                continue;
            }
            if options.is_empty() {
                continue;
            }
            options.sort_by(|a, b| {
                let (ga, ua) = a.rank();
                let (gb, ub) = b.rank();
                ga.cmp(&gb).then(ua.total_cmp(&ub))
            });
            let reason = if !result.skipped.is_empty() {
                format!("fallback:{}", result.skipped.join(","))
            } else if group.len() > 1 {
                let others: Vec<String> = options[1..]
                    .iter()
                    .map(|o| format!("{}={}", o.harness, o.shown()))
                    .collect();
                let tail = if others.is_empty() {
                    String::new()
                } else {
                    format!("<{}", others.join(","))
                };
                format!("balanced:{}{tail}", options[0].shown())
            } else {
                "primary".to_string()
            };
            result
                .delegates
                .push(self.delegate(route_id, &options[0], &reason, orch.as_deref()));
            break;
        }
        if result.delegates.is_empty() {
            // Kept work records no pick, so a run would name nothing.
            result.run = None;
            result.reason = Some(if result.skipped.is_empty() {
                "orchestrator-only route".to_string()
            } else {
                format!("no eligible delegate: {}", result.skipped.join(", "))
            });
        } else if let Some(judge) = &route.judge {
            result.cascade = Some(Cascade {
                judge: judge.clone(),
                escalate: route.escalate.clone(),
            });
        }
        Ok(result)
    }

    /// A tag for a model the user named: no routing decision, but the same record of pool state.
    /// A named version is kept; a family name gets its newest version.
    pub fn manual(&self, target: &str, effort: &str) -> Result<Pick> {
        let (harness, name) = target
            .split_once('/')
            .filter(|(h, n)| !n.is_empty() && self.config.harnesses.contains_key(*h))
            .ok_or_else(|| {
                Error::Usage(format!(
                    "{target:?} is not <harness>/<model> with a defined harness"
                ))
            })?;
        let offered = self
            .readings
            .models
            .get(harness)
            .is_some_and(|m| !m.is_empty());
        let model = match resolve(harness, name, self.readings) {
            Some(m) => m,
            None if offered => {
                return Err(Error::Usage(format!(
                    "{harness} offers no model matching {name:?}; check the harness's model list"
                )));
            }
            None => name.to_string(),
        };
        let orch = self.orchestrator();
        let choice = Choice {
            harness,
            pool: pool(harness, &model, self.readings, self.config, self.now),
            model,
            preview: false,
            effort,
            api: self.is_api(harness),
        };
        let delegate = self.delegate("manual", &choice, "user-named", orch.as_deref());
        Ok(Pick {
            route: "manual".into(),
            work: String::new(),
            role: "general".into(),
            rules: self.rules.to_string(),
            orchestrator: orch,
            delegates: vec![delegate],
            skipped: Vec::new(),
            reason: None,
            run: None,
            judges: None,
            escalates: None,
            cascade: None,
            warnings: self.warnings.to_vec(),
            council: false,
        })
    }

    /// Each harness's shared windows, then one row per model window a route uses, whose state
    /// includes that window as a pick for its models would.
    pub fn pools(&self) -> Vec<PoolRow> {
        let routed: Vec<&str> = self
            .config
            .routes
            .values()
            .flat_map(|r| r.candidates.iter().map(|c| c.target.as_str()))
            .collect();
        let mut rows = Vec::new();
        for harness in self.config.harnesses.keys() {
            let mut families: Vec<&str> = self
                .config
                .model_windows
                .values()
                .filter(|w| &w.harness == harness)
                .map(|w| w.model.as_str())
                .filter(|f| {
                    routed.iter().any(|t| {
                        t.split_once('/')
                            .is_some_and(|(h, m)| h == harness && m.contains(f))
                    })
                })
                .collect();
            families.sort_unstable();
            families.dedup();
            for family in std::iter::once("").chain(families) {
                let p = pool(harness, family, self.readings, self.config, self.now);
                rows.push(PoolRow {
                    harness: harness.clone(),
                    family: family.to_string(),
                    state: p.state,
                    summary: p.summary,
                    usage: p.usage.map(|u| (u * 10.0).round() / 10.0),
                });
            }
        }
        rows
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PoolRow {
    pub harness: String,
    pub family: String,
    pub state: State,
    pub summary: String,
    #[serde(rename = "use")]
    pub usage: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteRow {
    pub id: String,
    pub work: String,
    pub enabled: bool,
    pub council: bool,
    pub role: String,
    pub tiers: Vec<Vec<String>>,
    pub judge: Option<String>,
    pub escalate: Option<String>,
}

/// Consecutive candidates sharing a `tier` value form one group; a candidate without one stands alone.
pub fn tiers(candidates: &[Candidate]) -> Vec<&[Candidate]> {
    let mut groups: Vec<&[Candidate]> = Vec::new();
    let mut start = 0;
    for i in 1..=candidates.len() {
        let joins = i < candidates.len()
            && candidates[i].tier.is_some()
            && candidates[i].tier == candidates[i - 1].tier;
        if !joins {
            groups.push(&candidates[start..i]);
            start = i;
        }
    }
    groups
}

/// Routes with their tier chains, sorted by id.
pub fn list(config: &Config) -> Vec<RouteRow> {
    config
        .routes
        .iter()
        .map(|(id, r): (&String, &Route)| RouteRow {
            id: id.clone(),
            work: r.work.clone(),
            enabled: r.enabled,
            council: r.council,
            role: r.role.clone(),
            tiers: tiers(&r.candidates)
                .into_iter()
                .map(|g| g.iter().map(|c| c.target.clone()).collect())
                .collect(),
            judge: r.judge.clone(),
            escalate: r.escalate.clone(),
        })
        .collect()
}

/// A warning when the last evaluation is older than `stale_days` or not a date. Without `last` there
/// is nothing to age: a file can carry only the evaluated models for the preview gate.
pub fn stale_evaluation(config: &Config, now: DateTime<Utc>) -> Option<String> {
    let ev = config.evaluation.as_ref()?;
    let last = chrono::NaiveDate::parse_from_str(ev.last.as_deref()?, "%Y-%m-%d").ok();
    let Some(last) = last else {
        return Some(
            "routes.toml has no valid [evaluation] last date; re-run the route evaluation".into(),
        );
    };
    let age = (now.date_naive() - last).num_days();
    (age > ev.stale_days).then(|| format!("the route evaluation is {age} days old; re-run it"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    const CONFIG: &str = r#"version = 1
[orchestrator]
harness = "claude"
model = "opus"
[harnesses.claude]
expect = ["five_hour", "seven_day"]
effort_option = "effort"
t3 = "claudeAgent"
[harnesses.codex]
expect = ["primary"]
effort_option = "reasoningEffort"
[harnesses.antigravity]
brief_prefix = "Load the workstation-rules skill first"

[routes."review.code"]
role = "review"
candidates = [
  { target = "codex/sol", effort = "high", tier = 1 },
  { target = "claude/sonnet", effort = "high", tier = 1 },
  { target = "antigravity/argon-high", tier = 2 },
  { target = "self" },
]
[routes.council]
council = true
candidates = [
  { target = "codex/sol", effort = "high" },
  { target = "claude/opus", effort = "high" },
  { target = "antigravity/argon-high" },
]
[routes.design]
candidates = [{ target = "self" }]
[routes.off]
enabled = false
candidates = [{ target = "codex/sol" }]
"#;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-29T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn setup(codex: f64, claude_week: f64, argon_fresh: bool) -> (Config, Readings) {
        let config = Config::parse(CONFIG, Path::new("t.toml")).unwrap();
        let mut r = Readings::default();
        let argon_at = if argon_fresh {
            "2026-10-29T01:59:00Z"
        } else {
            "2026-10-29T01:00:00Z"
        };
        r.merge(&config, &json!({
            "harnesses": [
                {"harness": "codex", "fetchedAt": "2026-10-29T01:59:00Z", "windows": [
                    {"id": "primary", "kind": "weekly", "percent": codex, "resetsAt": "2026-11-04T00:00:00Z"}]},
                {"harness": "claude", "fetchedAt": "2026-10-29T01:59:00Z", "windows": [
                    {"id": "five_hour", "kind": "session", "percent": 5, "resetsAt": "2026-10-29T05:00:00Z"},
                    {"id": "seven_day", "kind": "weekly", "percent": claude_week, "resetsAt": "2026-11-02T00:00:00Z"}]},
                {"harness": "antigravity", "fetchedAt": argon_at, "windows": [
                    {"id": "weekly", "kind": "weekly", "percent": 3, "resetsAt": "2026-11-02T00:00:00Z"}]}
            ],
            "models": {
                "codex": ["gpt-6.1-sol", "gpt-6-sol"],
                "claude": ["claude-opus-5-5", "claude-sonnet-5-5"],
                "antigravity": ["gemini-4-argon-high"]
            }
        }));
        (config, r)
    }

    fn pick(config: &Config, r: &Readings, route: &str, size: Size, avoid: &[&str]) -> Pick {
        let ctx = Context {
            config,
            rules: "abc1234",
            readings: r,
            now: now(),
            warnings: &[],
        };
        let avoid: Vec<String> = avoid.iter().map(|s| s.to_string()).collect();
        ctx.pick(route, size, &avoid).unwrap()
    }

    #[test]
    fn balances_a_tier_toward_the_least_used_plan() {
        let (c, r) = setup(40.0, 20.0, true);
        let p = pick(&c, &r, "review.code", Size::Medium, &[]);
        let d = &p.delegates[0];
        assert_eq!(
            (d.target.as_str(), d.reason.as_str()),
            ("claude/claude-sonnet-5-5", "balanced:20%<codex=40%")
        );
        assert_eq!(d.t3.provider_instance_id, "claudeAgent");
        assert_eq!(
            d.t3.options,
            json!({"effort": "high"}).as_object().unwrap().clone()
        );
        assert!(d.tag.starts_with(
            "[route review.code -> claude/claude-sonnet-5-5 high | balanced:20%<codex=40% | claude s5% w20%; claude s5% w20% | rules abc1234 | pick p-"
        ), "{}", d.tag);
    }

    #[test]
    fn falls_back_to_the_next_tier_and_names_what_was_skipped() {
        let (c, r) = setup(95.0, 91.0, true);
        let p = pick(&c, &r, "review.code", Size::Medium, &[]);
        let d = &p.delegates[0];
        assert_eq!(
            (d.target.as_str(), d.reason.as_str()),
            (
                "antigravity/gemini-4-argon-high",
                "fallback:codex=out,claude=out"
            )
        );
        assert_eq!(d.prefix, "Load the workstation-rules skill first");
        assert!(d.t3.options.is_empty());
    }

    #[test]
    fn unknown_pools_take_medium_work_but_not_large() {
        let (c, r) = setup(95.0, 91.0, false);
        assert_eq!(
            pick(&c, &r, "review.code", Size::Medium, &[]).delegates[0].harness,
            "antigravity"
        );
        let large = pick(&c, &r, "review.code", Size::Large, &[]);
        assert!(large.delegates.is_empty());
        assert_eq!(
            large.reason.as_deref(),
            Some("no eligible delegate: codex=out, claude=out, antigravity=unknown")
        );
    }

    #[test]
    fn conserve_pools_take_only_small_work() {
        let (c, r) = setup(87.0, 95.0, true);
        assert_eq!(
            pick(&c, &r, "review.code", Size::Small, &[]).delegates[0].harness,
            "codex"
        );
        assert_eq!(
            pick(&c, &r, "review.code", Size::Medium, &[]).delegates[0].harness,
            "antigravity"
        );
    }

    #[test]
    fn avoided_harnesses_are_skipped() {
        let (c, r) = setup(10.0, 20.0, true);
        let d = &pick(&c, &r, "review.code", Size::Medium, &["claude"]).delegates[0];
        assert_eq!(
            (d.harness.as_str(), d.reason.as_str()),
            ("codex", "fallback:claude=avoided")
        );
    }

    #[test]
    fn councils_take_every_eligible_member_and_report_the_rest() {
        let (c, r) = setup(10.0, 95.0, true);
        let p = pick(&c, &r, "council", Size::Medium, &[]);
        let got: Vec<&str> = p.delegates.iter().map(|d| d.harness.as_str()).collect();
        assert_eq!(got, ["codex", "antigravity"]);
        assert_eq!(p.skipped, ["claude=out"]);
        assert!(p.delegates.iter().all(|d| d.reason == "council"));
        assert_ne!(p.delegates[0].id, p.delegates[1].id);
    }

    #[test]
    fn disabled_and_orchestrator_only_routes_keep_the_work() {
        let (c, r) = setup(10.0, 20.0, true);
        assert_eq!(
            pick(&c, &r, "off", Size::Medium, &[]).reason.as_deref(),
            Some("disabled")
        );
        assert_eq!(
            pick(&c, &r, "design", Size::Medium, &[]).reason.as_deref(),
            Some("orchestrator-only route")
        );
    }

    #[test]
    fn manual_keeps_named_versions_and_rejects_unknown_models() {
        let (c, r) = setup(10.0, 20.0, true);
        let ctx = Context {
            config: &c,
            rules: "abc1234",
            readings: &r,
            now: now(),
            warnings: &[],
        };
        assert_eq!(
            ctx.manual("codex/gpt-6-sol", "").unwrap().delegates[0].model,
            "gpt-6-sol"
        );
        assert_eq!(
            ctx.manual("codex/sol", "low").unwrap().delegates[0].model,
            "gpt-6.1-sol"
        );
        assert!(ctx.manual("codex/astra", "").is_err());
        assert!(ctx.manual("grok/x", "").is_err());
    }

    #[test]
    fn api_keys_come_after_ready_plans_but_before_unknown_ones() {
        let text = CONFIG.replace(
            "[harnesses.antigravity]",
            "[harnesses.codex-api]\nbilling = \"api\"\neffort_option = \"reasoningEffort\"\nmodels = [\"gpt-6.1-sol\"]\n[harnesses.antigravity]",
        ).replace(
            "  { target = \"claude/sonnet\", effort = \"high\", tier = 1 },\n  { target = \"antigravity",
            "  { target = \"claude/sonnet\", effort = \"high\", tier = 1 },\n  { target = \"codex-api/sol\", effort = \"high\", tier = 1 },\n  { target = \"antigravity",
        );
        let pick_with = |codex: f64, claude: f64, fresh: bool| {
            let (_, base) = setup(codex, claude, fresh);
            let config = Config::parse(&text, Path::new("t.toml")).unwrap();
            let mut r = base.clone();
            r.fill_models(&config);
            if !fresh {
                r.entries.get_mut("claude").unwrap().fetched_at = None;
            }
            pick(&config, &r, "review.code", Size::Medium, &[]).delegates[0].clone()
        };
        let d = pick_with(40.0, 20.0, true);
        assert_eq!(
            (d.harness.as_str(), d.reason.as_str()),
            ("claude", "balanced:20%<codex=40%,codex-api=key")
        );
        let d = pick_with(95.0, 95.0, true);
        assert_eq!(
            (d.harness.as_str(), d.reason.as_str(), d.summary.as_str()),
            (
                "codex-api",
                "fallback:codex=out,claude=out",
                "codex-api api key"
            )
        );
        let d = pick_with(95.0, 20.0, false);
        assert_eq!(
            d.harness, "codex-api",
            "a key beats a plan whose state is unknown"
        );
    }

    #[test]
    fn an_evaluation_without_a_last_date_is_never_stale() {
        let now = Utc::now();
        let with = |ev: &str| {
            Config::parse(
                &format!("version = 1\n[evaluation]\n{ev}"),
                Path::new("t.toml"),
            )
            .unwrap()
        };
        assert_eq!(
            stale_evaluation(&with("models = [\"codex/gpt-6.1-sol\"]\n"), now),
            None
        );
        assert!(stale_evaluation(&with("last = \"soon\"\n"), now).is_some());
    }

    /// Codex offers gpt-6.2-sol, gpt-6.1-sol and gpt-6-sol; the evaluation lists only gpt-6.1-sol.
    fn preview_setup(mode: &str, listed: &str) -> (Config, Readings) {
        let text = CONFIG.replace(
            "[orchestrator]",
            &format!("[evaluation]\nmodels = [{listed}]\npreview = \"{mode}\"\n[orchestrator]"),
        );
        let config = Config::parse(&text, Path::new("t.toml")).unwrap();
        let (_, mut r) = setup(10.0, 95.0, true);
        r.models.insert(
            "codex".into(),
            vec![
                "gpt-6.2-sol".into(),
                "gpt-6.1-sol".into(),
                "gpt-6-sol".into(),
            ],
        );
        (config, r)
    }

    fn picked(c: &Config, r: &Readings, size: Size) -> (String, bool, String) {
        let d = pick(c, r, "review.code", size, &[]).delegates[0].clone();
        (d.model, d.preview, d.reason)
    }

    #[test]
    fn trial_holds_preview_versions_back_except_for_small_picks() {
        let (c, r) = preview_setup("trial", "\"codex/gpt-6.1-sol\"");
        assert_eq!(
            picked(&c, &r, Size::Medium),
            ("gpt-6.1-sol".into(), false, "fallback:claude=out".into())
        );
        assert_eq!(
            picked(&c, &r, Size::Small),
            (
                "gpt-6.2-sol".into(),
                true,
                "fallback:claude=out (preview)".into()
            )
        );
    }

    #[test]
    fn hold_and_use_never_and_always_take_the_newest() {
        let (c, r) = preview_setup("hold", "\"codex/gpt-6.1-sol\"");
        assert_eq!(picked(&c, &r, Size::Small).0, "gpt-6.1-sol");
        let (c, r) = preview_setup("use", "\"codex/gpt-6.1-sol\"");
        assert_eq!(
            picked(&c, &r, Size::Medium),
            ("gpt-6.2-sol".into(), false, "fallback:claude=out".into())
        );
    }

    #[test]
    fn a_family_with_no_listed_version_uses_its_preview_for_every_size() {
        let (c, r) = preview_setup("hold", "\"claude/claude-opus-5-5\"");
        assert_eq!(
            picked(&c, &r, Size::Large),
            (
                "gpt-6.2-sol".into(),
                true,
                "fallback:claude=out (preview)".into()
            )
        );
    }

    #[test]
    fn evaluated_entries_match_harness_aliases() {
        let text = CONFIG.replace(
            "t3 = \"claudeAgent\"",
            "t3 = \"claudeAgent\"\naliases = [\"claudeAgent\"]",
        );
        let c = Config::parse(
            &text.replace(
                "[orchestrator]",
                "[evaluation]\nmodels = [\"claudeAgent/claude-sonnet-5-5\"]\n[orchestrator]",
            ),
            Path::new("t.toml"),
        )
        .unwrap();
        let (_, mut r) = setup(95.0, 10.0, true);
        r.models.insert(
            "claude".into(),
            vec!["claude-sonnet-5-6".into(), "claude-sonnet-5-5".into()],
        );
        let d = &pick(&c, &r, "review.code", Size::Medium, &[]).delegates[0];
        assert_eq!((d.model.as_str(), d.preview), ("claude-sonnet-5-5", false));
    }

    #[test]
    fn an_exact_id_in_a_route_is_never_in_preview() {
        let (_, r) = preview_setup("hold", "\"codex/gpt-6.1-sol\"");
        let text = CONFIG
            .replace("codex/sol\", effort = \"high\", tier = 1", "codex/gpt-6.2-sol\", effort = \"high\", tier = 1")
            .replace("[orchestrator]", "[evaluation]\nmodels = [\"codex/gpt-6.1-sol\"]\npreview = \"hold\"\n[orchestrator]");
        let exact = Config::parse(&text, Path::new("t.toml")).unwrap();
        assert_eq!(
            picked(&exact, &r, Size::Large),
            ("gpt-6.2-sol".into(), false, "fallback:claude=out".into())
        );
    }

    #[test]
    fn cascade_routes_name_their_judge_and_escalation() {
        let text = format!(
            "{CONFIG}[routes.impl]\njudge = \"review.code\"\ncandidates = [{{ target = \"codex/sol\" }}, {{ target = \"self\" }}]\n"
        );
        let c = Config::parse(&text, Path::new("t.toml")).unwrap();
        let (_, r) = setup(10.0, 20.0, true);
        let p = pick(&c, &r, "impl", Size::Medium, &[]);
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(
            v["cascade"],
            json!({"judge": "review.code", "escalate": null})
        );
        assert_eq!(v["delegates"][0]["preview"], json!(false));
        let kept = pick(&c, &r, "impl", Size::Medium, &["codex"]);
        assert!(kept.delegates.is_empty() && kept.cascade.is_none() && kept.run.is_none());
        assert!(
            pick(&c, &r, "review.code", Size::Medium, &[])
                .cascade
                .is_none()
        );
    }

    #[test]
    fn untiered_candidates_stand_alone() {
        let (c, _) = setup(0.0, 0.0, true);
        let groups: Vec<usize> = tiers(&c.routes["review.code"].candidates)
            .iter()
            .map(|g| g.len())
            .collect();
        assert_eq!(groups, [2, 1, 1]);
        assert_eq!(tiers(&c.routes["council"].candidates).len(), 3);
    }
}
