//! `cedr mcp`: Route as MCP tools on stdio (docs/route.md, MCP tools), so any orchestrating agent
//! can pick a delegate and record verdicts without shelling out. Each call reads the config afresh.

use std::path::PathBuf;

use cedr_core::{discovery, lint, outdated, verify};
use cedr_route::council::{self, Report};
use cedr_route::history;
use cedr_route::pick::{self, Size};
use cedr_route::session::{Overrides, Session};
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::schemars::JsonSchema;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::Value;

use crate::FailOn;

#[derive(Clone)]
pub struct RouteServer {
    config: Option<PathBuf>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct PickArgs {
    /// Route id from routes.toml, e.g. "review.code"
    route: String,
    judges: Option<String>,
    escalates: Option<String>,
    /// small, medium (default) or large
    size: Option<String>,
    /// Harnesses to skip because they just failed on a limit or capacity
    avoid: Option<Vec<String>>,
    /// {"<harness>": [model ids]} the orchestrator offers, replacing the configured lists
    models: Option<Value>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct PlanArgs {
    /// Route id from routes.toml, e.g. "review.code"
    route: String,
    /// small, medium (default) or large
    size: Option<String>,
    /// Harnesses to skip because they just failed on a limit or capacity
    avoid: Option<Vec<String>>,
    /// Exact member targets; disables spare replacements
    members: Option<Vec<String>>,
    /// {"<harness>": [model ids]} the orchestrator offers, replacing the configured lists
    models: Option<Value>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ManualArgs {
    /// <harness>/<model>: a family name or an exact id
    target: String,
    effort: Option<String>,
}

/// The published shape of a council report (docs/council.md, Reports). The handler still parses each
/// report with `Report::from_json`, which rejects anything else, so this only tells clients to send
/// report objects rather than JSON text.
#[derive(JsonSchema)]
#[schemars(crate = "rmcp::schemars", deny_unknown_fields)]
#[allow(dead_code)]
struct ReportSchema {
    /// <harness>/<model>
    member: String,
    /// Pick id of this member's review
    pick: Option<String>,
    /// The member's own score out of 10; shown, never decides the gate
    score: Option<f64>,
    /// The member this spare stood in for
    spare_for: Option<String>,
    /// Every finding; empty when the member reviewed and found nothing
    findings: Vec<FindingSchema>,
}

#[derive(JsonSchema)]
#[schemars(crate = "rmcp::schemars", deny_unknown_fields)]
#[allow(dead_code)]
struct FindingSchema {
    file: String,
    line: u64,
    severity: SeveritySchema,
    claim: String,
    scenario: Option<String>,
}

#[derive(JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[allow(dead_code)]
enum SeveritySchema {
    P0,
    P1,
    P2,
    P3,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct MergeArgs {
    /// One report object per member
    #[schemars(with = "Vec<ReportSchema>")]
    reports: Vec<Value>,
    /// Council route whose quorum and blocking severity apply
    route: Option<String>,
    run: Option<String>,
    /// Quorum to use instead of the route's (default 2)
    quorum: Option<usize>,
    /// Least severe finding that blocks: P0, P1 (default), P2 or P3
    block: Option<String>,
    /// Revision rounds instead of the route's (default 1, at most 3)
    rounds: Option<u32>,
    /// The review being merged: 1 (default) for the first, 2 after one revision, at most rounds + 1
    round: Option<u32>,
    /// Previous round merge JSON, required from round 2 with unlimited rounds
    previous: Option<Value>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct VerdictArgs {
    /// Pick id from the route tag, e.g. "p-1a2b3c4d5e6f"
    id: String,
    /// accepted, reworked, redone or rejected
    verdict: String,
    /// The orchestrator's task id for the delegated work
    task: Option<String>,
    /// One line, at most 120 characters
    note: Option<String>,
    /// Monitor session id (s- plus 12 hex digits) or the harness's native id
    session: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ShowArgs {
    route: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct StatsArgs {
    route: Option<String>,
    since: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct HistoryArgs {
    route: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct MonitorReportArgs {
    since: Option<String>,
    by: Option<String>,
    harness: Option<String>,
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct MonitorPushPlanArgs {
    /// Organization ID, instead of the configured one
    org: Option<String>,
    /// Where the organization's usage would start: all, now, YYYY-MM-DD or an RFC 3339 time
    start: Option<String>,
    /// Everything from the start, not just what changed since the last push
    full: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct RepoArgs {
    /// Repository root, default current directory
    path: Option<PathBuf>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
struct GuardLintArgs {
    /// Repository root, default current directory
    path: Option<PathBuf>,
    /// Treat lint warnings as findings that fail the gate
    #[serde(default)]
    strict: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
struct GuardVerifyArgs {
    /// Repository root, default current directory
    path: Option<PathBuf>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
struct GuardOutdatedArgs {
    /// Repository root containing the Guard lock, default current directory
    path: Option<PathBuf>,
    /// Fail the gate at this surface-change severity, default never
    fail_on: Option<FailOn>,
}

fn reply(result: cedr_route::Result<Value>) -> CallToolResult {
    match result {
        Ok(v) => {
            let mut content = vec![ContentBlock::text(
                serde_json::to_string_pretty(&v).expect("value serializes"),
            )];
            if v["kind"] == "bench" && v["unpriced_trials"].as_u64().is_some_and(|n| n > 0) {
                content.push(ContentBlock::text(format!("Cost incomplete: at least ${:.4}, {} trials unpriced; cost comparisons and goal status are unknown.", v["spent_usd"].as_f64().unwrap_or(0.0), v["unpriced_trials"])));
            }
            CallToolResult::success(content)
        }
        Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
    }
}

/// Runs blocking work (config reads, HTTPS readings, history writes) off the async runtime.
async fn blocking(
    f: impl FnOnce() -> cedr_route::Result<Value> + Send + 'static,
) -> CallToolResult {
    blocking_tool(move || reply(f())).await
}

async fn blocking_tool(f: impl FnOnce() -> CallToolResult + Send + 'static) -> CallToolResult {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r,
        Err(e) => reply(Err(cedr_route::Error::Usage(format!("tool failed: {e}")))),
    }
}

/// Keep CLI exit-1 reports readable as JSON and reserve plain text for a failed check.
async fn guard_check(
    f: impl FnOnce() -> cedr_core::Result<(Value, bool)> + Send + 'static,
) -> CallToolResult {
    blocking_tool(move || match f() {
        Ok((value, failed)) => {
            let mut result = reply(Ok(value));
            result.is_error = Some(failed);
            result
        }
        Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
    })
    .await
}

impl RouteServer {
    fn session(&self, models: Option<Value>) -> cedr_route::Result<Session> {
        let cwd = std::env::current_dir().map_err(|source| cedr_route::Error::Io {
            path: PathBuf::from("."),
            source,
        })?;
        let session = Session::open(
            self.config.as_deref(),
            &cwd,
            Overrides {
                readings: None,
                models,
            },
        )?;
        // Picks carry these too; stderr is the server's log for the other tools.
        for w in &session.warnings {
            eprintln!("cedr: {w}");
        }
        Ok(session)
    }
}

fn json(v: impl serde::Serialize) -> Value {
    serde_json::to_value(v).expect("value serializes")
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ComparePlanArgs {
    /// Suite file; default the only *.toml in .cedr/compare/
    suite: Option<PathBuf>,
    /// Only these task ids
    task: Option<Vec<String>>,
    /// Trials per task and variant, instead of the suite's
    trials: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct BenchmarkHistoryArgs {
    /// How many runs, newest first; default 20
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct BenchmarkShowArgs {
    /// A saved run id from history, or a result file
    id: String,
}

#[tool_router]
impl RouteServer {
    #[tool(
        description = "Saved standard Bench runs with scores and known spend subtotals, newest first. Cost is incomplete when unpriced_trials > 0; over_goal is unknown-safe, not a cost verdict."
    )]
    async fn bench_history(
        &self,
        Parameters(a): Parameters<BenchmarkHistoryArgs>,
    ) -> CallToolResult {
        blocking(move || {
            crate::bench::history_rows(a.limit.unwrap_or(20))
                .map(Value::Array)
                .map_err(cedr_route::Error::Usage)
        })
        .await
    }
    #[tool(
        description = "Validate and read one standard Bench result, including tasks and artifacts. spent_usd is the known subtotal; unpriced_trials > 0 means incomplete cost and over_goal cannot claim under goal."
    )]
    async fn bench_show(&self, Parameters(a): Parameters<BenchmarkShowArgs>) -> CallToolResult {
        blocking(move || {
            crate::bench::read_result(&a.id)
                .map(json)
                .map_err(cedr_route::Error::Usage)
        })
        .await
    }
    #[tool(
        description = "Validate a Compare suite and show what a run would do, starting nothing."
    )]
    async fn compare_plan(&self, Parameters(a): Parameters<ComparePlanArgs>) -> CallToolResult {
        blocking(move || {
            crate::compare::plan_value(a.suite.as_deref(), a.task.unwrap_or_default(), a.trials)
                .map_err(cedr_route::Error::Usage)
        })
        .await
    }
    #[tool(description = "Saved Compare runs with their pass counts and spend, newest first.")]
    async fn compare_history(
        &self,
        Parameters(a): Parameters<BenchmarkHistoryArgs>,
    ) -> CallToolResult {
        blocking(move || {
            crate::compare::history_rows(a.limit.unwrap_or(20))
                .map(Value::Array)
                .map_err(cedr_route::Error::Usage)
        })
        .await
    }
    #[tool(description = "One saved Compare run: its variants, trials, counts and spend.")]
    async fn compare_show(&self, Parameters(a): Parameters<BenchmarkShowArgs>) -> CallToolResult {
        blocking(move || {
            crate::compare::read_result(&a.id)
                .map(json)
                .map_err(cedr_route::Error::Usage)
        })
        .await
    }
    #[tool(description = "Lint repo instructions, skills and MCP configuration offline")]
    async fn guard_lint(&self, Parameters(a): Parameters<GuardLintArgs>) -> CallToolResult {
        guard_check(move || {
            let ws = discovery::discover(&a.path.unwrap_or_else(|| PathBuf::from(".")))?;
            let report = lint::lint_workspace(&ws)?;
            let failed = crate::lint_failed(&report, a.strict);
            Ok((json(report), failed))
        })
        .await
    }

    #[tool(description = "Compare repo skills and MCP settings with the Guard lock offline")]
    async fn guard_verify(&self, Parameters(a): Parameters<GuardVerifyArgs>) -> CallToolResult {
        guard_check(move || {
            let verification = verify::verify(&a.path.unwrap_or_else(|| PathBuf::from(".")))?;
            crate::print_skipped(&verification.skipped);
            let failed = !verification.report.is_empty();
            Ok((json(verification.report), failed))
        })
        .await
    }

    #[tool(
        description = "Compare locked npm tool surfaces with newer versions fetched from the npm registry, without running packages"
    )]
    async fn guard_outdated(&self, Parameters(a): Parameters<GuardOutdatedArgs>) -> CallToolResult {
        guard_check(move || {
            let report = outdated::outdated(&a.path.unwrap_or_else(|| PathBuf::from(".")))?;
            crate::print_outdated_warnings(&report.warnings);
            let failed = a.fail_on.unwrap_or_default().reached(&report);
            Ok((json(report), failed))
        })
        .await
    }

    #[tool(description = "Check generated agent files and organization rules offline")]
    async fn sync_check(&self, Parameters(a): Parameters<RepoArgs>) -> CallToolResult {
        blocking(move || {
            crate::sync::check_value(&a.path.unwrap_or_else(|| PathBuf::from(".")))
                .map_err(cedr_route::Error::Usage)
        })
        .await
    }
    #[tool(description = "Plan agent configuration changes as unified diffs without writing")]
    async fn sync_plan(&self, Parameters(a): Parameters<RepoArgs>) -> CallToolResult {
        blocking(move || {
            cedr_sync::plan(&a.path.unwrap_or_else(|| PathBuf::from(".")))
                .map(|p| json(p.files))
                .map_err(cedr_route::Error::Usage)
        })
        .await
    }
    #[tool(description = "Diagnose this machine and repository without changing them")]
    async fn doctor(&self, Parameters(a): Parameters<RepoArgs>) -> CallToolResult {
        blocking(move || {
            Ok(crate::doctor::value(
                &a.path.unwrap_or_else(|| PathBuf::from(".")),
            ))
        })
        .await
    }
    #[tool(
        description = "Summarize local plan limits, spend, pending verdicts and repository checks"
    )]
    async fn status(&self, Parameters(a): Parameters<RepoArgs>) -> CallToolResult {
        blocking(move || {
            crate::status::snapshot(&a.path.unwrap_or_else(|| PathBuf::from(".")))
                .map(json)
                .map_err(cedr_route::Error::Usage)
        })
        .await
    }
    #[tool(description = "Report local coding-agent token usage and API-equivalent cost estimates")]
    async fn monitor_report(&self, Parameters(a): Parameters<MonitorReportArgs>) -> CallToolResult {
        blocking(move || {
            let db =
                cedr_monitor::collect().map_err(|e| cedr_route::Error::Usage(e.to_string()))?;
            cedr_monitor::report::report(
                &db,
                cedr_monitor::report::Options {
                    since: a.since.as_deref(),
                    by: a.by.as_deref(),
                    harness: a.harness.as_deref(),
                    project: a.project.as_deref(),
                    ..Default::default()
                },
            )
            .map_err(|e| cedr_route::Error::Usage(e.to_string()))
        })
        .await
    }
    #[tool(description = "Show latest plan windows and usage projections")]
    async fn monitor_limits(&self) -> CallToolResult {
        blocking(move || {
            let db =
                cedr_monitor::collect().map_err(|e| cedr_route::Error::Usage(e.to_string()))?;
            for warning in cedr_monitor::limits::refresh(&db) {
                eprintln!("cedr: {warning}");
            }
            cedr_monitor::limits::report(&db).map_err(|e| cedr_route::Error::Usage(e.to_string()))
        })
        .await
    }
    #[tool(
        description = "Show what cedr monitor push would send to the organization (destination, hours, rows by harness, project labels, readings, start) without sending or saving anything. Run before a first upload; pushing itself stays a CLI step the person approves."
    )]
    async fn monitor_push_plan(
        &self,
        Parameters(a): Parameters<MonitorPushPlanArgs>,
    ) -> CallToolResult {
        blocking(move || {
            let usage = |e: Box<dyn std::error::Error + Send + Sync>| {
                cedr_route::Error::Usage(e.to_string())
            };
            let start = a
                .start
                .as_deref()
                .map(cedr_monitor::push::parse_start)
                .transpose()
                .map_err(usage)?;
            let sync = crate::monitor::dry_run_target(a.org.as_deref(), None).map_err(usage)?;
            let db = cedr_monitor::collect().map_err(usage)?;
            crate::monitor::dry_run_value(&db, &sync, a.full.unwrap_or(false), start).map_err(usage)
        })
        .await
    }
    #[tool(description = "Show local usage source availability and collection health")]
    async fn monitor_sources(&self) -> CallToolResult {
        blocking(move || {
            let db =
                cedr_monitor::collect().map_err(|e| cedr_route::Error::Usage(e.to_string()))?;
            Ok(json(
                db.source_values()
                    .map_err(|e| cedr_route::Error::Usage(e.to_string()))?,
            ))
        })
        .await
    }

    #[tool(
        description = "Pick the harness, model and effort for a route. The brief must start with the returned tag."
    )]
    async fn route_pick(&self, Parameters(a): Parameters<PickArgs>) -> CallToolResult {
        let me = self.clone();
        blocking(move || {
            let size = Size::parse(a.size.as_deref().unwrap_or("medium"))?;
            let session = me.session(a.models)?;
            let mut p = session
                .context()
                .pick(&a.route, size, &a.avoid.unwrap_or_default())?;
            history::link_pick(
                &history::default_path(),
                &session.loaded.config,
                &mut p,
                a.judges.as_deref(),
                a.escalates.as_deref(),
            )?;
            history::record_pick(&history::default_path(), &p, size, Utc::now())?;
            crate::route::auto_sync(me.config.as_deref());
            Ok(json(&p))
        })
        .await
    }

    #[tool(description = "Tag a model the user named, without routing.")]
    async fn route_manual(&self, Parameters(a): Parameters<ManualArgs>) -> CallToolResult {
        let me = self.clone();
        blocking(move || {
            let session = me.session(None)?;
            let p = session
                .context()
                .manual(&a.target, a.effort.as_deref().unwrap_or(""))?;
            history::record_pick(&history::default_path(), &p, Size::Medium, Utc::now())?;
            crate::route::auto_sync(me.config.as_deref());
            Ok(json(&p))
        })
        .await
    }

    #[tool(description = "Record a verdict on a delegated pick after checking its work.")]
    async fn route_verdict(&self, Parameters(a): Parameters<VerdictArgs>) -> CallToolResult {
        let me = self.clone();
        blocking(move || {
            crate::route::record_verdict(
                &a.id,
                &a.verdict,
                a.task.as_deref(),
                a.note.as_deref(),
                a.session.as_deref(),
            )?;
            crate::route::auto_sync(me.config.as_deref());
            Ok(serde_json::json!({"recorded": true}))
        })
        .await
    }

    #[tool(description = "State of every harness's plan: ready, conserve, unknown or out.")]
    async fn route_pools(&self) -> CallToolResult {
        let me = self.clone();
        blocking(move || Ok(json(me.session(None)?.context().pools()))).await
    }

    #[tool(
        description = "Name the council members to start for a council route, with its quorum, fixes, advisory, rounds and spare assignments. Named members disable spares. Each member's brief must start with its tag. In T3 Code, start each member with delegate_task, passing its t3 object as the target."
    )]
    async fn council_plan(&self, Parameters(a): Parameters<PlanArgs>) -> CallToolResult {
        let me = self.clone();
        blocking(move || {
            let size = Size::parse(a.size.as_deref().unwrap_or("medium"))?;
            let session = me.session(a.models)?;
            let plan = council::plan_members(
                &session.context(),
                &a.route,
                size,
                &a.avoid.unwrap_or_default(),
                &a.members.unwrap_or_default(),
            )?;
            history::record_pick(&history::default_path(), &plan.pick, size, Utc::now())?;
            crate::route::auto_sync(me.config.as_deref());
            Ok(json(&plan))
        })
        .await
    }

    #[tool(
        description = "Merge council members' reports into one gate: pass, block (at or above the blocking severity) or incomplete (below quorum); a blocked gate with a round left returns next=revise and the author's revision brief. Unlimited rounds require previous merge JSON from round 2; repeated blocking locations stop as stalled. Reports may name spare_for. A report may carry the member's score out of 10, which never decides the gate."
    )]
    async fn council_merge(&self, Parameters(a): Parameters<MergeArgs>) -> CallToolResult {
        let me = self.clone();
        blocking(move || {
            let block = match a.block.as_deref() {
                None => None,
                Some(b) => Some(council::Severity::parse(b).ok_or_else(|| {
                    cedr_route::Error::Usage(format!("block {b:?} is not P0, P1, P2 or P3"))
                })?),
            };
            let settings = council::settings(
                a.quorum,
                block,
                a.rounds,
                a.round,
                // Reports here are JSON objects, which cannot be cut off, so there is no strict flag.
                false,
                a.route.as_deref(),
                || {
                    let cwd = std::env::current_dir().map_err(|source| cedr_route::Error::Io {
                        path: PathBuf::from("."),
                        source,
                    })?;
                    Ok(Session::config_only(me.config.as_deref(), &cwd)?.config)
                },
            )?;
            let reports = a
                .reports
                .into_iter()
                .map(|r| Report::from_json(r, None))
                .collect::<cedr_route::Result<Vec<_>>>()?;
            let mut merged = council::merge(&reports, settings);
            council::apply_previous(&mut merged, a.previous)?;
            history::record_council(
                &history::default_path(),
                &mut merged,
                a.route.as_deref(),
                a.run.as_deref(),
                Utc::now(),
            )?;
            crate::route::auto_sync(me.config.as_deref());
            Ok(json(merged))
        })
        .await
    }

    #[tool(
        description = "Picks, verdicts, latency, cascade outcomes and matched Monitor usage per route and model."
    )]
    async fn route_stats(&self, Parameters(a): Parameters<StatsArgs>) -> CallToolResult {
        let me = self.clone();
        blocking(move || {
            let cwd = std::env::current_dir().map_err(|source| cedr_route::Error::Io {
                path: PathBuf::from("."),
                source,
            })?;
            let loaded = Session::config_only(me.config.as_deref(), &cwd)?;
            Ok(json(crate::route::stats(
                a.route.as_deref(),
                a.since.as_deref(),
                &loaded.config,
            )?))
        })
        .await
    }
    #[tool(description = "Latest picks with verdicts and council results, newest first.")]
    async fn route_history(&self, Parameters(a): Parameters<HistoryArgs>) -> CallToolResult {
        blocking(move || {
            Ok(json(history::history(
                &history::default_path(),
                a.route.as_deref(),
                a.limit.unwrap_or(20),
            )?))
        })
        .await
    }
    #[tool(description = "Council merge totals and revision outcomes per route.")]
    async fn council_stats(&self, Parameters(a): Parameters<StatsArgs>) -> CallToolResult {
        blocking(move || {
            Ok(json(history::council_stats(
                &history::default_path(),
                a.route.as_deref(),
                a.since.as_deref(),
            )?))
        })
        .await
    }

    #[tool(
        description = "What a route would pick at each size now, and each candidate's model, state and thresholds. Records nothing."
    )]
    async fn route_show(&self, Parameters(a): Parameters<ShowArgs>) -> CallToolResult {
        let me = self.clone();
        blocking(move || Ok(json(me.session(None)?.context().show(&a.route)?))).await
    }

    #[tool(description = "The model ids each harness offers, for use in targets.")]
    async fn route_models(&self) -> CallToolResult {
        let me = self.clone();
        blocking(move || Ok(json(me.session(None)?.context().models()))).await
    }

    #[tool(
        description = "Routes with their tier chains, and each cascade's judge and escalate routes."
    )]
    async fn route_list(&self) -> CallToolResult {
        let me = self.clone();
        blocking(move || {
            let cwd = std::env::current_dir().map_err(|source| cedr_route::Error::Io {
                path: PathBuf::from("."),
                source,
            })?;
            let loaded = Session::config_only(me.config.as_deref(), &cwd)?;
            Ok(json(pick::list(&loaded.config)))
        })
        .await
    }
}

#[tool_handler(
    name = "cedr",
    instructions = "Cedr Route: call route_pick before delegating work, start the brief with the returned tag, and record route_verdict after checking the delegate's work."
)]
impl ServerHandler for RouteServer {}

/// Serves the tools on stdio until the client disconnects.
pub fn serve(config: Option<PathBuf>) -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let service = RouteServer { config }
            .serve(rmcp::transport::stdio())
            .await
            .map_err(std::io::Error::other)?;
        service.waiting().await.map_err(std::io::Error::other)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The JSON Schema keywords schemars emits for these arguments, checked the way a client would.
    fn valid(schema: &Value, root: &Value, v: &Value) -> bool {
        if let Some(r) = schema["$ref"].as_str() {
            let name = r.trim_start_matches("#/$defs/");
            return valid(&root["$defs"][name], root, v);
        }
        let kind = match v {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(n) if n.is_u64() || n.is_i64() => "integer",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        let types: Vec<&str> = match &schema["type"] {
            Value::String(t) => vec![t],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        if !types.is_empty()
            && !types
                .iter()
                .any(|t| *t == kind || (*t == "number" && kind == "integer"))
        {
            return false;
        }
        if let Some(options) = schema["enum"].as_array()
            && !options.contains(v)
        {
            return false;
        }
        if let (Some(items), Value::Array(a)) = (schema.get("items"), v) {
            return a.iter().all(|i| valid(items, root, i));
        }
        if let Value::Object(o) = v {
            let props = &schema["properties"];
            let required = schema["required"].as_array().into_iter().flatten();
            return required
                .filter_map(Value::as_str)
                .all(|k| o.contains_key(k))
                && o.iter().all(|(k, x)| match props.get(k) {
                    Some(p) => valid(p, root, x),
                    None => schema["additionalProperties"] != false,
                });
        }
        true
    }

    #[test]
    fn compare_tools_publish_the_arguments_clients_use() {
        let tools = RouteServer::tool_router().list_all();
        for (name, arguments) in [
            (
                "compare_plan",
                json!({"suite": ".cedr/compare/tasks.toml", "task": ["parser"], "trials": 2}),
            ),
            ("compare_history", json!({"limit": 5})),
            ("bench_history", json!({"limit":5})),
            ("bench_show", json!({"id":"b-20261113T000000Z-abcdef"})),
            ("compare_show", json!({"id": "c-20261101T193000Z-abcdef"})),
        ] {
            let tool = tools.iter().find(|t| t.name == name).unwrap();
            let schema = Value::Object((*tool.input_schema).clone());
            assert!(valid(&schema, &schema, &arguments), "{name}");
        }
    }

    #[test]
    fn council_merge_publishes_the_report_objects_its_handler_reads() {
        let tool = RouteServer::tool_router()
            .list_all()
            .into_iter()
            .find(|t| t.name == "council_merge")
            .unwrap();
        let schema = Value::Object((*tool.input_schema).clone());
        let report = json!({"member": "codex/sol", "pick": "p-1", "score": 7.5, "spare_for": "claude/opus",
            "findings": [{"file": "src/a.rs", "line": 12, "severity": "P1", "claim": "c", "scenario": null}]});
        assert!(Report::from_json(report.clone(), None).is_ok());
        assert!(valid(
            &schema,
            &schema,
            &json!({"reports": [report.clone()]})
        ));
        assert!(valid(
            &schema,
            &schema,
            &json!({"reports": [{"member": "a", "findings": []}]})
        ));
        // What a client that read the old `items: true` might send, and fields the handler refuses.
        assert!(!valid(
            &schema,
            &schema,
            &json!({"reports": [report.to_string()]})
        ));
        let mut extra = report.clone();
        extra["verdict"] = json!("ok");
        assert!(Report::from_json(extra.clone(), None).is_err());
        assert!(!valid(&schema, &schema, &json!({"reports": [extra]})));
        let mut p9 = report;
        p9["findings"][0]["severity"] = json!("P9");
        assert!(!valid(&schema, &schema, &json!({"reports": [p9]})));
    }
}
