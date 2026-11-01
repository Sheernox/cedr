//! Read-only diagnostics. No collectors, sync jobs or package servers are started.
use cedr_route::config::{self, Sync};
use clap::Args as ClapArgs;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};
#[derive(ClapArgs)]
pub struct Args {
    /// Repository root
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Text or JSON diagnostics
    #[arg(long,value_enum,default_value_t=crate::Format::Text)]
    format: crate::Format,
}
#[derive(Serialize)]
pub struct Check {
    id: String,
    area: String,
    state: String,
    message: String,
    fix: Option<String>,
}
fn add(out: &mut Vec<Check>, id: &str, state: &str, message: impl Into<String>, fix: &str) {
    out.push(Check {
        id: id.into(),
        area: match id.split('.').next().unwrap_or(id) {
            "key" | "org" => "account",
            s => s,
        }
        .into(),
        state: state.into(),
        message: message.into(),
        fix: if state == "ok" {
            None
        } else {
            Some(fix.into())
        },
    });
}
fn installed(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p)
            .any(|d| d.join(name).is_file() || d.join(format!("{name}.exe")).is_file())
    })
}
pub fn checks(root: &Path) -> Vec<Check> {
    let mut out = vec![];
    add(
        &mut out,
        "cli.version",
        "skip",
        format!(
            "{}; release lookup unavailable offline",
            env!("CARGO_PKG_VERSION")
        ),
        "Compare cedr --version with the latest release",
    );
    let routes = config::locate(None, root).and_then(|p| config::load_local(&p).map(|c| (p, c)));
    match &routes {
        Ok((p, c)) => {
            add(&mut out, "route.config", "ok", p.display().to_string(), "");
            let (absent, _) = cedr_route::machine::absent(c);
            let stale = cedr_route::machine::stale(c, &Default::default());
            let mut missing = absent.keys().cloned().collect::<Vec<_>>();
            missing.sort();
            add(
                &mut out,
                "route.readers",
                if absent.is_empty() && stale.is_empty() {
                    "ok"
                } else {
                    "warn"
                },
                format!(
                    "absent harnesses: [{}]; stale readers: [{}]",
                    missing.join(", "),
                    stale
                        .iter()
                        .map(|s| s.harness.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "Install missing harnesses and configure their usage readers",
            );
        }
        Err(_) => {
            add(
                &mut out,
                "route.config",
                "warn",
                "routes config missing or invalid",
                "Create or repair routes.toml",
            );
            add(
                &mut out,
                "route.readers",
                "skip",
                "no routes config",
                "Configure Route first",
            );
        }
    }
    let key = cedr_route::sync::api_key().and_then(|key| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::env::var("CEDR_API_KEY")
                .ok()
                .is_none_or(|s| s.trim().is_empty())
            {
                let path = config::xdg_dir("XDG_CONFIG_HOME", ".config").join("cedr/api-key");
                if std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o777 != 0o600) {
                    return Err(cedr_route::Error::Usage(
                        "API key file requires mode 600".into(),
                    ));
                }
            }
        }
        if key.is_empty() {
            return Err(cedr_route::Error::Usage("API key is empty".into()));
        }
        Ok(key)
    });
    add(
        &mut out,
        "key.present",
        if key.is_ok() { "ok" } else { "fail" },
        if key.is_ok() {
            "API key configured"
        } else {
            "API key missing or file permissions invalid"
        },
        "Set CEDR_API_KEY or save ~/.config/cedr/api-key with mode 600",
    );
    let monitor = cedr_monitor::config::Config::load();
    let org = monitor
        .as_ref()
        .ok()
        .and_then(|c| c.org.clone())
        .or_else(|| {
            routes
                .as_ref()
                .ok()
                .and_then(|(_, c)| c.sync.as_ref().map(|s| s.org.clone()))
        });
    let settings = Sync {
        org: org.clone().unwrap_or_default(),
        routes: Default::default(),
        url: monitor
            .as_ref()
            .ok()
            .and_then(|c| c.url.clone())
            .or_else(|| {
                routes
                    .as_ref()
                    .ok()
                    .and_then(|(_, c)| c.sync.as_ref().and_then(|s| s.url.clone()))
            }),
        auto: false,
    };
    // Online diagnostics require a successful account check; offline tests omit a key or use a loopback API.
    let me = key
        .as_ref()
        .ok()
        .map(|k| cedr_route::sync::get_me(&settings, k));
    match &me {
        Some(Ok(v)) => {
            let owner = v["user"]["name"]
                .as_str()
                .or(v["name"].as_str())
                .unwrap_or("account owner");
            add(
                &mut out,
                "key.valid",
                "ok",
                format!("key belongs to {}", crate::common::clip(owner, 128)),
                "",
            );
            match &org {
                Some(id) => {
                    let member = v["organizations"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|o| o["id"].as_str() == Some(id)));
                    add(
                        &mut out,
                        "org.member",
                        if member { "ok" } else { "fail" },
                        if member {
                            "organization membership confirmed"
                        } else {
                            "configured organization is absent from account memberships"
                        },
                        "Choose an organization listed in your account",
                    );
                }
                None => add(
                    &mut out,
                    "org.member",
                    "skip",
                    "no organization configured",
                    "Set org in monitor.toml or routes.toml [sync]",
                ),
            }
        }
        Some(Err(error)) => {
            let rejected = error.to_string().contains("HTTP 4");
            add(
                &mut out,
                "key.valid",
                if rejected { "fail" } else { "skip" },
                if rejected {
                    "API key rejected"
                } else {
                    "key check unavailable offline"
                },
                "Check connectivity and the API key",
            );
            add(
                &mut out,
                "org.member",
                "skip",
                "account unavailable",
                "Verify the API key first",
            );
        }
        None => {
            add(
                &mut out,
                "key.valid",
                "skip",
                "no key",
                "Configure an API key",
            );
            add(
                &mut out,
                "org.member",
                "skip",
                "no key",
                "Configure an API key",
            );
        }
    }
    if matches!(me, Some(Ok(_)))
        && let Ok(release) = cedr_route::sync::doctor_release(&settings)
    {
        let tag = release["tag_name"].as_str().unwrap_or("");
        let parts = |s: &str| {
            s.trim_start_matches('v')
                .split('.')
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>()
                .ok()
        };
        if let (Some(latest), Some(current)) = (parts(tag), parts(env!("CARGO_PKG_VERSION"))) {
            let newer = latest > current;
            if let Some(check) = out.iter_mut().find(|c| c.id == "cli.version") {
                check.state = if newer { "warn" } else { "ok" }.into();
                check.message = format!(
                    "{}; latest {}",
                    env!("CARGO_PKG_VERSION"),
                    crate::common::clip(tag, 64)
                );
                check.fix = if newer {
                    Some("Run cedr update".into())
                } else {
                    None
                };
            }
        }
    }
    if let Some(error) = crate::update::last_error()
        && let Some(check) = out.iter_mut().find(|c| c.id == "cli.version")
    {
        check.state = "warn".into();
        check.message = format!(
            "{}; the last automatic update failed: {error}",
            check.message
        );
        check.fix = Some("Run cedr update".into());
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let statusline = std::fs::read(home.join(".claude/settings.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .is_some_and(|v| {
            v["statusLine"]["command"]
                .as_str()
                .is_some_and(|s| s.contains("cedr route statusline"))
        });
    add(
        &mut out,
        "route.statusline",
        if !installed("claude") {
            "skip"
        } else if statusline {
            "ok"
        } else {
            "warn"
        },
        if statusline {
            "status line configured"
        } else {
            "Claude status line not configured or harness absent"
        },
        "Configure Claude statusLine.command to run cedr route statusline",
    );
    let sync_error = std::fs::metadata(cedr_route::sync::log_path()).is_ok_and(|m| m.len() > 0);
    add(
        &mut out,
        "route.sync",
        if sync_error { "warn" } else { "ok" },
        if sync_error {
            "last background sync failed"
        } else {
            "no background sync error"
        },
        "Run cedr route sync",
    );
    for agent in ["claude", "codex"] {
        let (state, message) = if !installed(agent) {
            ("skip", "harness absent")
        } else {
            match Command::new(agent)
                .args(["mcp", "list"])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
            {
                Ok(o)
                    if o.status.success()
                        && String::from_utf8_lossy(&o.stdout).contains("cedr") =>
                {
                    ("ok", "cedr MCP registration found")
                }
                _ => ("warn", "cedr MCP registration missing or unreadable"),
            }
        };
        add(
            &mut out,
            &format!("mcp.{agent}"),
            state,
            message,
            &format!("{agent} mcp add cedr -- cedr mcp"),
        );
    }
    add(
        &mut out,
        "monitor.config",
        if monitor.is_ok() { "ok" } else { "fail" },
        if monitor.is_ok() {
            "monitor configuration parses"
        } else {
            "monitor configuration invalid"
        },
        "Repair ~/.config/cedr/monitor.toml",
    );
    let snapshot = cedr_monitor::store::Store::source_snapshot(&cedr_monitor::store::path()).ok();
    if let Ok(config) = &monitor {
        for harness in cedr_monitor::HARNESSES {
            let saved = snapshot
                .as_ref()
                .and_then(|a| a.iter().find(|s| s["harness"] == *harness));
            if let Some(saved) = saved {
                let state = match saved["state"].as_str() {
                    Some("ok") if saved["malformed"].as_u64().unwrap_or(0) == 0 => "ok",
                    Some("error") => "fail",
                    _ => "warn",
                };
                add(
                    &mut out,
                    "monitor.sources",
                    state,
                    format!(
                        "{harness}: {} files, {} malformed",
                        saved["files"].as_u64().unwrap_or(0),
                        saved["malformed"].as_u64().unwrap_or(0)
                    ),
                    "Run cedr monitor sources to inspect collection health",
                );
                continue;
            }
            match cedr_monitor::sources::files(harness, config) {
                Ok(files) => add(
                    &mut out,
                    "monitor.sources",
                    if files.is_empty() { "warn" } else { "ok" },
                    format!("{harness}: {} source files", files.len()),
                    "Run cedr monitor sources to inspect collection health",
                ),
                Err(_) => add(
                    &mut out,
                    "monitor.sources",
                    "fail",
                    format!("{harness}: cannot discover source files"),
                    "Repair source paths in monitor.toml",
                ),
            }
        }
    }
    let state = config::xdg_dir("XDG_STATE_HOME", ".local/state").join("cedr");
    let watermark = state.join("monitor-push.json");
    let last_push = std::fs::read(&watermark)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|v| {
            v[cedr_monitor::digest(&format!("{}:{}", settings.org, settings.url()))].as_i64()
        });
    let age = last_push.map(|at| chrono::Utc::now().timestamp().saturating_sub(at));
    let error = std::fs::metadata(state.join("monitor-push.log")).is_ok_and(|m| m.len() > 0);
    add(
        &mut out,
        "monitor.push",
        if org.is_none() {
            "skip"
        } else if error || age.is_none_or(|a| a > 86400) {
            "warn"
        } else {
            "ok"
        },
        if error {
            "last background push failed".into()
        } else if let Some(age) = age {
            format!("last successful push {} hours ago", age / 3600)
        } else {
            "no push watermark".into()
        },
        "Run cedr monitor push",
    );
    let schedule = cedr_monitor::schedule::files(std::env::consts::OS, "cedr", 60)
        .ok()
        .is_some_and(|a| a.iter().any(|f| home.join(&f.relative).is_file()));
    add(
        &mut out,
        "monitor.schedule",
        if schedule { "ok" } else { "warn" },
        if schedule {
            "push timer installed"
        } else {
            "push timer absent"
        },
        "Run cedr monitor schedule",
    );
    let repo = root.ancestors().any(|p| p.join(".git").exists()) || root.join(".cedr").exists();
    let config = cedr_sync::load_config(root);
    add(
        &mut out,
        "sync.config",
        if !repo {
            "skip"
        } else if config.is_ok() {
            "ok"
        } else {
            "warn"
        },
        if config.is_ok() {
            "Sync configuration parses"
        } else {
            "Sync configuration absent or invalid"
        },
        "Run cedr sync init or repair .cedr/sync.toml",
    );
    if config.is_ok() {
        let r = cedr_sync::check(root);
        add(
            &mut out,
            "sync.check",
            if r.is_empty() { "ok" } else { "fail" },
            format!("{} Sync findings", r.findings.len()),
            "Run cedr sync check, then cedr sync",
        );
    } else {
        add(
            &mut out,
            "sync.check",
            "skip",
            "no Sync configuration",
            "Run cedr sync init",
        );
    }
    match cedr_sync::org::load(root) {
        Ok(Some(v)) => {
            let current = v["version"].as_u64().unwrap_or(0);
            let remote = if matches!(me, Some(Ok(_))) && org.is_some() {
                key.as_ref()
                    .ok()
                    .map(|k| cedr_route::sync::get_sync_rules(&settings, k))
            } else {
                None
            };
            match remote {
                Some(Ok(cedr_route::sync::SyncRules::Rules(new))) => {
                    let newer = new["version"].as_u64().is_some_and(|n| n > current);
                    add(
                        &mut out,
                        "sync.rules",
                        if newer { "warn" } else { "ok" },
                        if newer {
                            "newer organization rules are available"
                        } else {
                            "vendored rules are current"
                        },
                        "Run cedr sync pull",
                    );
                }
                Some(Ok(cedr_route::sync::SyncRules::None)) => add(
                    &mut out,
                    "sync.rules",
                    "warn",
                    "organization no longer publishes rules",
                    "Run cedr sync pull",
                ),
                _ => add(
                    &mut out,
                    "sync.rules",
                    "skip",
                    "vendored rules hash matches; freshness unavailable offline or without a matching plan",
                    "Run cedr sync pull when connected",
                ),
            }
        }
        Ok(None) => add(
            &mut out,
            "sync.rules",
            "skip",
            "no vendored org rules",
            "Run cedr sync pull",
        ),
        Err(_) => add(
            &mut out,
            "sync.rules",
            "fail",
            "vendored org rules hash mismatch",
            "Run cedr sync pull",
        ),
    }
    if root.join(".cedr/guard.lock").is_file() {
        match cedr_core::verify::verify(root) {
            Ok(v) => add(
                &mut out,
                "guard.lock",
                if v.report.is_empty() { "ok" } else { "fail" },
                format!("{} Guard findings", v.report.findings.len()),
                "Run cedr guard verify and review changes before cedr guard lock",
            ),
            Err(_) => add(
                &mut out,
                "guard.lock",
                "fail",
                "Guard verification failed",
                "Run cedr guard verify",
            ),
        }
    } else {
        add(
            &mut out,
            "guard.lock",
            "warn",
            ".cedr/guard.lock missing",
            "Run cedr guard lock",
        );
    }
    let order = [
        "cli.version",
        "key.present",
        "key.valid",
        "org.member",
        "route.config",
        "route.readers",
        "route.statusline",
        "route.sync",
        "mcp.claude",
        "mcp.codex",
        "monitor.config",
        "monitor.sources",
        "monitor.push",
        "monitor.schedule",
        "sync.config",
        "sync.check",
        "sync.rules",
        "guard.lock",
    ];
    out.sort_by_key(|c| {
        order
            .iter()
            .position(|id| *id == c.id)
            .unwrap_or(order.len())
    });
    out
}
pub fn value(root: &Path) -> Value {
    json!({"checks":checks(root)})
}
pub fn run(args: Args) -> Result<ExitCode, String> {
    if !args.path.is_dir() {
        return Err("doctor: repository path must be a directory".into());
    }
    let checks = checks(&args.path);
    let failures = checks.iter().filter(|c| c.state == "fail").count();
    crate::telemetry::count("doctor_fail", failures);
    crate::telemetry::count(
        "doctor_warn",
        checks.iter().filter(|c| c.state == "warn").count(),
    );
    match args.format {
        crate::Format::Json => println!("{}", json!({"checks":checks})),
        crate::Format::Text => {
            for c in &checks {
                println!("{} {}: {}", c.state, c.id, c.message);
                if let Some(fix) = &c.fix {
                    println!("  {fix}");
                }
            }
            println!(
                "cedr: {} ok, {} warnings, {failures} failures",
                checks.iter().filter(|c| c.state == "ok").count(),
                checks.iter().filter(|c| c.state == "warn").count()
            );
        }
    }
    Ok(crate::exit(failures > 0))
}
