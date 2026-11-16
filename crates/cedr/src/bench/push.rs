//! Explicit measurement uploads. Construct an allowlist rather than serializing local evidence.
use super::{BenchResult, read_recorded_result, results};
use crate::{Format, common, settings};
use cedr_route::{config, machine};
use clap::Args;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::LazyLock;

const MAX_BODY: usize = 256 * 1024;
// Invisible formatting must not hide local path syntax in the outgoing measurements.
static MODEL_FORMAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\p{Cf}\p{Default_Ignorable_Code_Point}]")
        .expect("constant Unicode model-path pattern")
});

#[derive(Args)]
pub struct PushArgs {
    /// Saved IDs; without IDs, send every unsent Bench result on this machine
    ids: Vec<String>,
    /// Organization public ID, overriding [sync] org
    #[arg(long)]
    org: Option<String>,
    /// Local routes.toml holding API settings
    #[arg(long)]
    config: Option<PathBuf>,
    /// Print request bodies without sending or marking them; needs no API key
    #[arg(long)]
    dry_run: bool,
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
}

/// Match the server's UTF-16 string limits without splitting a Unicode scalar.
fn clip(value: &str, max: usize) -> String {
    let mut units = 0;
    value
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= max
        })
        .collect()
}

fn text(value: &str, max: usize) -> String {
    clip(&cedr_compare::report::clean(value), max)
}

fn model(value: &str) -> String {
    let cleaned = cedr_compare::report::clean(value);
    let visible = MODEL_FORMAT.replace_all(&cleaned, "");
    let candidate = visible.trim();
    let file_uri = candidate
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("file:"));
    let path = if file_uri { &candidate[5..] } else { candidate };
    // Bare tags such as a:mini are model names; drive paths carry a separator.
    let drive = path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && path.as_bytes().get(1) == Some(&b':')
        && path.contains('/');
    if file_uri
        || path.contains('\\')
        || path.starts_with('/')
        || path.starts_with('~')
        || path.starts_with("./")
        || path.starts_with("../")
        || drive
    {
        "[local model]".into()
    } else {
        clip(&cleaned, 256)
    }
}

/// Evidence, free-form grader text and local review notes never enter an outgoing value.
fn measurement(r: &BenchResult) -> Result<Value, String> {
    let mut tasks = Vec::new();
    if r.tasks.len() > 256 {
        return Err("a Bench push holds at most 256 tasks".into());
    }
    if r.config.resolved_models.len() > 128 {
        return Err("a Bench push holds at most 128 resolved models".into());
    }
    for t in &r.tasks {
        if t.gates.len() > 128 || t.tamper_signals.len() > 128 {
            return Err("a pushed task holds at most 128 gates and 128 signals".into());
        }
        if t.checks
            .iter()
            .any(|c| !cedr_bench::suite::identifier(&c.id))
            || t.gates
                .iter()
                .any(|g| !cedr_bench::suite::identifier(&g.id))
        {
            return Err("push requires check and gate IDs to be plain suite identifiers".into());
        }
        let signals: Vec<_> = t
            .tamper_signals
            .iter()
            .map(|s| {
                let kind = match s.kind.as_str() {
                    "process_introspection"
                    | "descriptor_access"
                    | "runner_modification"
                    | "assertion_reassignment"
                    | "evidence_mismatch"
                    | "scan_limit" => s.kind.as_str(),
                    _ => "other",
                };
                json!({"kind": kind})
            })
            .collect();
        tasks.push(json!({
            "id": t.id, "kind": t.kind, "status": t.status,
            "checks": t.checks, "score": t.score, "solved": t.solved,
            "duration_ms": t.duration_ms, "turns": t.turns,
            "input_tokens": t.tokens.input_tokens, "cache_read_tokens": t.tokens.cache_read_tokens,
            "cache_write_tokens": t.tokens.cache_write_tokens, "output_tokens": t.tokens.output_tokens,
            "reasoning_tokens": t.tokens.reasoning_tokens, "cost_usd": t.cost_usd,
            "gates": t.gates.iter().map(|g| json!({"id":g.id,"passed":g.passed})).collect::<Vec<_>>(),
            "tamper_signals": signals,
        }));
    }
    Ok(json!({
        "version": r.version, "kind": r.kind, "id": r.id, "edition": r.edition,
        "config": {"harness":r.config.harness,"harness_version":text(&r.config.harness_version,512),
            "model":model(&r.config.model),"effort":r.config.effort.as_ref().map(|s|text(s,128)),
            "resolved_models":r.config.resolved_models.iter().map(|s|model(s)).collect::<Vec<_>>()},
        "environment": {"mode":r.environment.mode,"clean_home":r.environment.clean_home,
            "network":r.environment.network,"pinned":r.environment.pinned,
            "os":text(&r.environment.os,64),"arch":text(&r.environment.arch,64),"cli":text(&r.environment.cli,64)},
        "probe": r.probe, "billing":r.billing,"started_at":r.started_at,"finished_at":r.finished_at,
        "goal_usd":r.goal_usd,"stop_usd":r.stop_usd,"spent_usd":r.spent_usd,"over_goal":r.over_goal,
        "unpriced_trials":r.unpriced_trials,"complete":r.complete,"stop":r.stop,
        "score":r.score,"solved":r.solved,"code_tasks":r.code_tasks,"tasks":tasks,
        "tamper_review":r.tamper_review.as_ref().map(|v|json!({"verdict":v.verdict,"time":v.time})),
    }))
}

fn marker(org: &str) -> PathBuf {
    settings::state_file(&format!(
        "bench-pushed-{}.txt",
        cedr_bench::sha(org.as_bytes())
    ))
}

enum Receipt {
    Pushed,
    Duplicate,
    NotRetained,
}

/// The server is idempotent, so a lost local marker may cause a retry but cannot duplicate history.
fn send(sync: &config::Sync, key: &str, id: &str, body: &Value) -> Result<Receipt, String> {
    let reply = cedr_route::sync::send_bench(sync, key, body).map_err(|e| e.to_string())?;
    let duplicate = reply["duplicate"]
        .as_bool()
        .ok_or("bench reply: missing duplicate")?;
    let retained = reply["retained"]
        .as_bool()
        .ok_or("bench reply: missing retained")?;
    if reply["ok"] != true || reply["id"].as_str() != Some(id) {
        return Err("bench reply: expected ok and matching result id".into());
    }
    let path = marker(&sync.org);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(file, "{id}").map_err(|e| e.to_string())?;
    Ok(if !retained {
        Receipt::NotRetained
    } else if duplicate {
        Receipt::Duplicate
    } else {
        Receipt::Pushed
    })
}

pub fn run(args: PushArgs) -> Result<ExitCode, String> {
    let sync = common::settings(args.org.as_deref(), args.config.as_deref())?;
    let config = if args.org.is_some() && args.config.is_none() {
        config::Config::parse("version = 1", Path::new("routes.toml"))
    } else {
        config::machine_path(args.config.as_deref()).and_then(|p| config::load_local(&p))
    }
    .map_err(|e| e.to_string())?;
    let machine = json!({"id":machine::machine_id().map_err(|e|e.to_string())?,
        "name":text(&machine::machine_name(&config),128)});
    let ids = if args.ids.is_empty() {
        let sent: HashSet<String> = match std::fs::read_to_string(marker(&sync.org)) {
            Ok(s) => s.lines().map(str::to_owned).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(e) => return Err(e.to_string()),
        };
        let entries = match std::fs::read_dir(results()) {
            Ok(entries) => Some(entries),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        let mut ids = Vec::new();
        for entry in entries.into_iter().flatten() {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|s| s == "json")
                && let Some(id) = path.file_stem().and_then(|s| s.to_str())
                && !sent.contains(id)
            {
                ids.push(id.to_string());
            }
        }
        ids.sort();
        ids
    } else {
        args.ids
    };
    let key = if args.dry_run || ids.is_empty() {
        String::new()
    } else {
        cedr_route::sync::api_key().map_err(|e| e.to_string())?
    };
    let (mut pushed, mut duplicates, mut failed, mut bodies) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut not_retained = Vec::new();
    for id in ids {
        let outcome = (|| {
            let result = read_recorded_result(&id)?;
            if result.id != id {
                return Err("push requires a saved Bench result ID, not a file path".into());
            }
            let body = json!({"org":sync.org,"cli":env!("CARGO_PKG_VERSION"),"machine":machine,"result":measurement(&result)?});
            if serde_json::to_vec(&body).map_err(|e| e.to_string())?.len() > MAX_BODY {
                return Err("Bench result exceeds the 256 KiB push limit".into());
            }
            if args.dry_run {
                bodies.push(body);
            } else {
                match send(&sync, &key, &id, &body)? {
                    Receipt::Pushed => pushed.push(id.clone()),
                    Receipt::Duplicate => duplicates.push(id.clone()),
                    Receipt::NotRetained => not_retained.push(id.clone()),
                }
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = outcome {
            failed.push(json!({"id":id,"error":cedr_compare::report::clean(&error)}));
        }
    }
    let output = if args.dry_run {
        json!(bodies)
    } else {
        json!({"pushed":pushed,"duplicates":duplicates,"not_retained":not_retained,"failed":failed})
    };
    if args.dry_run || matches!(args.format, Format::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "Bench: {} pushed, {} already received, {} not retained, {} failed",
            pushed.len(),
            duplicates.len(),
            not_retained.len(),
            failed.len()
        );
    }
    for id in &not_retained {
        eprintln!(
            "cedr: bench push: {id}: not retained by the organization's history allowance; the local result remains"
        );
    }
    for failure in &failed {
        eprintln!(
            "cedr: bench push: {}: {}",
            cedr_compare::report::clean(failure["id"].as_str().unwrap_or("result")),
            failure["error"].as_str().unwrap_or("push failed")
        );
    }
    Ok(ExitCode::from(if failed.is_empty() { 0 } else { 2 }))
}
