//! `cedr mcp`: Route as MCP tools on stdio (docs/route.md, MCP tools), so any orchestrating agent
//! can pick a delegate and record verdicts without shelling out. Each call reads the config afresh.

use std::path::PathBuf;

use cedr_route::council::{self, Report};
use cedr_route::history::{self, Verdict};
use cedr_route::pick::{self, Size};
use cedr_route::session::{Overrides, Session};
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::schemars::JsonSchema;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::Value;

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

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct MergeArgs {
    /// One report object per member: {"member", "pick"?, "findings": [{"file", "line", "severity", "claim", "scenario"?}]}
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
    /// The review being merged: 1 (default) for the first, 2 after one revision
    round: Option<u32>,
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

fn reply(result: cedr_route::Result<Value>) -> CallToolResult {
    match result {
        Ok(v) => CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&v).expect("value serializes"),
        )]),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
    }
}

/// Runs blocking work (config reads, HTTPS readings, history writes) off the async runtime.
async fn blocking(
    f: impl FnOnce() -> cedr_route::Result<Value> + Send + 'static,
) -> CallToolResult {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => reply(r),
        Err(e) => reply(Err(cedr_route::Error::Usage(format!("tool failed: {e}")))),
    }
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

#[tool_router]
impl RouteServer {
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
            let v = Verdict::parse(&a.verdict)?;
            history::record_verdict(
                &history::default_path(),
                &a.id,
                v,
                a.task.as_deref(),
                a.note.as_deref(),
                Utc::now(),
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

    #[tool(description = "Name the council members to start for a council route, with its quorum.")]
    async fn council_plan(&self, Parameters(a): Parameters<PlanArgs>) -> CallToolResult {
        let me = self.clone();
        blocking(move || {
            let size = Size::parse(a.size.as_deref().unwrap_or("medium"))?;
            let session = me.session(a.models)?;
            let plan = council::plan(
                &session.context(),
                &a.route,
                size,
                &a.avoid.unwrap_or_default(),
            )?;
            history::record_pick(&history::default_path(), &plan.pick, size, Utc::now())?;
            crate::route::auto_sync(me.config.as_deref());
            Ok(json(&plan))
        })
        .await
    }

    #[tool(
        description = "Merge council members' reports into one gate: pass, block (at or above the blocking severity) or incomplete (below quorum); a blocked gate with a round left returns next=revise and the author's revision brief."
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

    #[tool(description = "Picks, verdicts, latency and cascade outcomes per route and model.")]
    async fn route_stats(&self, Parameters(a): Parameters<StatsArgs>) -> CallToolResult {
        let me = self.clone();
        blocking(move || {
            let cwd = std::env::current_dir().map_err(|source| cedr_route::Error::Io {
                path: PathBuf::from("."),
                source,
            })?;
            let loaded = Session::config_only(me.config.as_deref(), &cwd)?;
            Ok(json(history::stats(
                &history::default_path(),
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
