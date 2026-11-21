//! Daily status from local Monitor, Route, Sync and Guard data.
use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use cedr_monitor::{self as monitor, limits, report, store::Store};
use cedr_route::history::{self, PendingPick};
use chrono::{DateTime, Local, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::Format;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    limits: Option<Vec<Value>>,
    spend: Option<Spend>,
    pending_picks: Option<Vec<PendingPick>>,
    sync: Option<SyncState>,
    guard: Option<GuardState>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Spend {
    today_usd: f64,
    last7_days_usd: f64,
    unpriced_today: Value,
    unpriced_last7_days: Value,
}

#[derive(Serialize)]
struct SyncState {
    state: &'static str,
    findings: usize,
}

#[derive(Serialize)]
struct GuardState {
    state: &'static str,
    findings: Option<usize>,
    error: Option<String>,
}

fn closest_limits(db: &Store) -> monitor::Result<Option<Vec<Value>>> {
    let now = Utc::now();
    let rows = limits::report(db)?;
    let mut by_harness = BTreeMap::new();
    for row in rows.as_array().into_iter().flatten() {
        let (Some(harness), Some(percent)) = (row["harness"].as_str(), row["percent"].as_f64())
        else {
            continue;
        };
        let reset = row["resetsAt"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
        if reset.is_some_and(|at| at <= now) {
            continue;
        }
        let entry = by_harness.entry(harness).or_insert((percent, row));
        if percent > entry.0 {
            *entry = (percent, row);
        }
    }
    Ok((!by_harness.is_empty()).then(|| {
        by_harness
            .into_values()
            .map(|(_, row)| row.clone())
            .collect()
    }))
}

fn spend(db: &Store) -> monitor::Result<Option<Spend>> {
    let has_responses: bool =
        db.db
            .query_row("SELECT EXISTS(SELECT 1 FROM responses)", [], |r| r.get(0))?;
    if !has_responses && !db.source_values()?.iter().any(|v| v["state"] == "ok") {
        return Ok(None);
    }
    let date = Local::now().date_naive().to_string();
    let today = report::report(
        db,
        report::Options {
            since: Some(&date),
            ..Default::default()
        },
    )?;
    let week = report::report(db, report::Options::default())?;
    Ok(Some(Spend {
        today_usd: today["totals"]["costUsd"].as_f64().unwrap_or(0.0),
        last7_days_usd: week["totals"]["costUsd"].as_f64().unwrap_or(0.0),
        unpriced_today: today["unpriced"].clone(),
        unpriced_last7_days: week["unpriced"].clone(),
    }))
}

fn repo(root: &Path) -> &Path {
    root.ancestors()
        .find(|p| p.join(".git").exists())
        .unwrap_or(root)
}

fn check_state(count: usize) -> &'static str {
    if count == 0 { "clean" } else { "drift" }
}

pub fn snapshot(path: &Path) -> Result<Snapshot, String> {
    let path = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !path.is_dir() {
        return Err("repository path must be a directory".into());
    }
    let root = repo(&path);
    let db = monitor::collect().map_err(|e| format!("Monitor collection: {e}"))?;
    for warning in limits::refresh_cached(&db).1 {
        eprintln!("cedr: {warning}");
    }
    let limits = closest_limits(&db).map_err(|e| format!("Monitor limits: {e}"))?;
    let spend = spend(&db).map_err(|e| format!("Monitor spend: {e}"))?;
    let history = history::default_path();
    let pending_picks = if exists(&history)? {
        Some(history::pending_picks(&history, 5).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let sync = if exists(&root.join(".cedr/sync.toml"))? {
        let findings = cedr_sync::check(root).findings.len();
        Some(SyncState {
            state: check_state(findings),
            findings,
        })
    } else {
        None
    };
    let guard = if exists(&root.join(cedr_core::lock::LOCK_FILE))?
        || exists(&root.join(cedr_core::lock::LEGACY_LOCK_FILE))?
    {
        Some(match cedr_core::verify::verify(root) {
            Ok(v) => {
                let findings = v.report.findings.len();
                GuardState {
                    state: if v.report.has_errors() {
                        "drift"
                    } else {
                        "clean"
                    },
                    findings: Some(findings),
                    error: None,
                }
            }
            Err(e) => GuardState {
                state: "error",
                findings: None,
                error: Some(e.to_string()),
            },
        })
    } else {
        None
    };
    Ok(Snapshot {
        limits,
        spend,
        pending_picks,
        sync,
        guard,
    })
}

fn exists(path: &Path) -> Result<bool, String> {
    path.try_exists()
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn print(snapshot: Snapshot) {
    if let Some(limits) = snapshot.limits {
        println!("Plan limits (last recorded)");
        for row in limits {
            let harness = row["harness"].as_str().unwrap_or("unknown");
            let window = row["window"].as_str().unwrap_or("unknown");
            let percent = row["percent"].as_f64().unwrap_or_default();
            println!("  {harness} {window}: {percent:.1}% used");
        }
    }
    if let Some(spend) = snapshot.spend {
        println!(
            "API-equivalent spend: ${:.2} today, ${:.2} over 7 days",
            spend.today_usd, spend.last7_days_usd
        );
        if let Some(models) = spend
            .unpriced_last7_days
            .as_array()
            .filter(|models| !models.is_empty())
        {
            let ids: Vec<_> = models.iter().filter_map(Value::as_str).collect();
            println!("  Unpriced models: {}", ids.join(", "));
        }
    }
    if let Some(picks) = snapshot.pending_picks {
        if picks.is_empty() {
            println!("Route: no picks waiting for a verdict");
        } else {
            println!("Route picks waiting for a verdict");
            for pick in picks {
                println!("  {} {} {}", pick.id, pick.route, pick.target);
            }
            println!("  Record one with cedr route verdict <id> <verdict>");
        }
    }
    if let Some(sync) = snapshot.sync {
        println!("Sync: {} ({} findings)", sync.state, sync.findings);
    }
    if let Some(guard) = snapshot.guard {
        if let Some(error) = guard.error {
            println!("Guard: {error}");
        } else if let Some(findings) = guard.findings {
            println!("Guard: {} ({findings} findings)", guard.state);
        }
    }
}

pub fn run(args: Args) -> Result<ExitCode, String> {
    let snapshot = snapshot(Path::new(".")).map_err(|e| format!("status: {e}"))?;
    match args.format {
        Format::Text => print(snapshot),
        Format::Json => println!("{}", serde_json::json!(snapshot)),
    }
    Ok(ExitCode::SUCCESS)
}
