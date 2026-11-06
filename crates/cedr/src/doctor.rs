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
    let routes = cedr_route::session::Session::config_only(None, root);
    let machine = config::machine_path(None).and_then(|p| config::load_local(&p));
    match &routes {
        Ok(loaded) => {
            let c = &loaded.config;
            add(
                &mut out,
                "route.config",
                if loaded.warnings.is_empty() {
                    "ok"
                } else {
                    "warn"
                },
                std::iter::once(loaded.path.display().to_string())
                    .chain(loaded.warnings.iter().cloned())
                    .collect::<Vec<_>>()
                    .join("; "),
                "Move repository machine settings to the user routes.toml or explicitly select a trusted file with --config; point [orchestrator] at a harness the routes define",
            );
            // The readings and machine checks a pick builds, so doctor and picks agree on which
            // harnesses are stale: absent ones and ones a feed already covers are not. The account
            // probes are left out: a harness with its own built-in reader is never stale, and
            // doctor starts no agent programs.
            let mut feeds = c.clone();
            feeds.readings.retain(|s| {
                !matches!(
                    s,
                    config::ReadingsSource::Codex { .. }
                        | config::ReadingsSource::Muse { .. }
                        | config::ReadingsSource::Grok { .. }
                )
            });
            let (mut readings, _) =
                cedr_route::readings::collect(&feeds, &loaded.readings_dir, chrono::Utc::now());
            let (absent, _) = cedr_route::machine::absent(c);
            readings.absent = absent.clone();
            let stale = cedr_route::machine::stale(c, &readings);
            let mut missing = absent.keys().cloned().collect::<Vec<_>>();
            missing.sort();
            // T3 Code keeps current readings for every plan it runs; a machine running it reads
            // those first (docs/route.md, Built-in readers).
            let no_t3 = cedr_route::t3::caches().is_dir()
                && !c
                    .readings
                    .iter()
                    .any(|s| matches!(s, config::ReadingsSource::T3));
            let mut message = format!(
                "absent harnesses: [{}]; stale readers: [{}]",
                missing.join(", "),
                stale
                    .iter()
                    .map(|s| s.harness.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if no_t3 {
                message.push_str("; T3 Code runs on this machine but no readings source reads it");
            }
            add(
                &mut out,
                "route.readers",
                if absent.is_empty() && stale.is_empty() && !no_t3 {
                    "ok"
                } else {
                    "warn"
                },
                message,
                "Install missing harnesses and configure their usage readers; with T3 Code, add [[readings]] kind = \"t3\" as the first source in the user routes.toml",
            );
            add(
                &mut out,
                "route.orchestrator",
                if c.orchestrator.is_some() {
                    "ok"
                } else {
                    "warn"
                },
                match &c.orchestrator {
                    Some(o) => format!("{}/{}", o.harness, o.model),
                    None => "no orchestrator set: route tags leave out the delegating plan's use"
                        .to_string(),
                },
                "Add [orchestrator] with the harness and model that delegate to this machine's routes.toml",
            );
            // Route targets that the model lists (T3 Code's, a feed's or the config's own) do not
            // offer, and present harnesses that list no models at all, whose targets then resolve to
            // nothing (docs/route.md, Model resolution): a pick of them is a routing error, not a
            // fallback.
            readings.fill_models(c);
            let targets: Vec<(&str, &str)> = c
                .routes
                .values()
                .filter(|r| r.enabled)
                .flat_map(|r| r.candidates.iter())
                .filter_map(|cand| cand.target.split_once('/'))
                .filter(|(h, _)| !readings.absent.contains_key(*h))
                .collect();
            let listed = |h: &str| readings.models.get(h).is_some_and(|m| !m.is_empty());
            let mut unlisted: Vec<&str> = targets
                .iter()
                .map(|(h, _)| *h)
                .filter(|h| !listed(h))
                .collect();
            unlisted.sort();
            unlisted.dedup();
            let mut missing_targets: Vec<String> = targets
                .iter()
                .filter(|(h, m)| listed(h) && cedr_route::pool::resolve(h, m, &readings).is_none())
                .map(|(h, m)| format!("{h}/{m}"))
                .collect();
            missing_targets.sort();
            missing_targets.dedup();
            // An effort off the ladder is passed on as written (docs/route.md, Model options), so
            // it is a typo unless the model's readings offer it.
            let mut efforts: Vec<String> = c
                .routes
                .iter()
                .filter(|(_, r)| r.enabled)
                .flat_map(|(id, r)| r.candidates.iter().map(move |cand| (id, cand)))
                .filter(|(_, cand)| {
                    !cand.effort.is_empty()
                        && !cedr_route::t3::EFFORT_LADDER.contains(&cand.effort.as_str())
                })
                .filter(|(_, cand)| {
                    let offered = cand.target.split_once('/').and_then(|(h, m)| {
                        let id = cedr_route::pool::resolve(h, m, &readings)?;
                        readings.efforts.get(h)?.get(&id).cloned()
                    });
                    !offered.is_some_and(|e| e.values.contains(&cand.effort))
                })
                .map(|(id, cand)| format!("{id} {}={}", cand.target, cand.effort))
                .collect();
            efforts.sort();
            efforts.dedup();
            let mut problems = vec![];
            if !unlisted.is_empty() {
                problems.push(format!(
                    "harnesses with no model list: [{}]",
                    unlisted.join(", ")
                ));
            }
            if !missing_targets.is_empty() {
                problems.push(format!(
                    "targets no offered model matches: [{}]",
                    missing_targets.join(", ")
                ));
            }
            if !efforts.is_empty() {
                problems.push(format!(
                    "efforts off the ladder that no model reading offers: [{}]",
                    efforts.join(", ")
                ));
            }
            add(
                &mut out,
                "route.targets",
                if problems.is_empty() { "ok" } else { "warn" },
                if problems.is_empty() {
                    "every route target matches an offered model".to_string()
                } else {
                    problems.join("; ")
                },
                "Add models = [\"<model id>\", ...] to each listed [harnesses.<id>] in routes.toml, or configure a readings source that lists models (kind = \"t3\" under T3 Code); point the targets at models the harness offers (cedr route models lists them); use an effort from minimal, low, medium, high, xhigh, max, ultra, ultrathink unless the model offers your own",
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
            add(
                &mut out,
                "route.targets",
                "skip",
                "no routes config",
                "Configure Route first",
            );
            add(
                &mut out,
                "route.orchestrator",
                "skip",
                "no routes config",
                "Configure Route first",
            );
        }
    }
    let monitor = cedr_monitor::config::Config::load();
    let org = monitor
        .as_ref()
        .ok()
        .and_then(|c| c.org.clone())
        .or_else(|| {
            machine
                .as_ref()
                .ok()
                .and_then(|c| c.sync.as_ref().map(|s| s.org.clone()))
        });
    let key_path = config::xdg_dir("XDG_CONFIG_HOME", ".config").join("cedr/api-key");
    let key = cedr_route::sync::api_key().and_then(|key| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::env::var("CEDR_API_KEY")
                .ok()
                .is_none_or(|s| s.trim().is_empty())
            {
                if std::fs::metadata(&key_path)
                    .is_ok_and(|m| m.permissions().mode() & 0o777 != 0o600)
                {
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
    // No key at all is a failure only when something here needs the hosted app (an org for Monitor
    // pushes or Route sync); a key that exists but is broken always is.
    let no_key = std::env::var("CEDR_API_KEY")
        .ok()
        .is_none_or(|s| s.trim().is_empty())
        && std::fs::symlink_metadata(&key_path)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    let (state, message) = match &key {
        Ok(_) => ("ok", "API key configured".to_string()),
        Err(_) if no_key && org.is_none() => (
            "skip",
            "no API key; none needed without an org in monitor.toml or routes.toml [sync]"
                .to_string(),
        ),
        Err(_) if no_key => (
            "fail",
            "no API key, but an organization is configured for the hosted app".to_string(),
        ),
        Err(e) => ("fail", format!("API key unusable: {e}")),
    };
    add(
        &mut out,
        "key.present",
        state,
        message,
        "Set CEDR_API_KEY or save ~/.config/cedr/api-key with mode 600",
    );
    let settings = Sync {
        org: org.clone().unwrap_or_default(),
        routes: Default::default(),
        url: monitor
            .as_ref()
            .ok()
            .and_then(|c| c.url.clone())
            .or_else(|| {
                machine
                    .as_ref()
                    .ok()
                    .and_then(|c| c.sync.as_ref().and_then(|s| s.url.clone()))
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
    let home = PathBuf::from(
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .unwrap_or_default(),
    );
    // Claude Code reads its settings from CLAUDE_CONFIG_DIR when set.
    let claude_settings = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("settings.json"),
        None => home.join(".claude/settings.json"),
    };
    let statusline = std::fs::read(claude_settings)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .is_some_and(|v| {
            v["statusLine"]["command"]
                .as_str()
                .is_some_and(|s| s.contains("cedr route statusline"))
        });
    // Claude Code run through its SDK (T3 Code and other orchestrators) renders no status line,
    // so its own usage cache is the reading there.
    // T3 Code runs Claude Code from a provider profile with no program on PATH; Route's machine
    // check counts that as present, so this check does too.
    let installed_claude = installed("claude")
        || routes.as_ref().is_ok_and(|l| {
            l.config.harnesses.contains_key("claude")
                && !cedr_route::machine::absent(&l.config)
                    .0
                    .contains_key("claude")
        });
    // Route reads the cache only for a single claude-statusline source (docs/route.md).
    let sources = routes.as_ref().map_or(0, |l| {
        l.config
            .readings
            .iter()
            .filter(|s| matches!(s, config::ReadingsSource::ClaudeStatusline { .. }))
            .count()
    });
    // T3 Code's own reading of Claude comes first when the routes read it.
    let from_t3 = installed_claude
        && routes.as_ref().is_ok_and(|l| {
            let c = &l.config;
            let instance = c
                .harnesses
                .get("claude")
                .and_then(|h| h.t3.as_deref())
                .unwrap_or("claude");
            c.readings
                .iter()
                .any(|s| matches!(s, config::ReadingsSource::T3))
                && cedr_route::readers::t3(
                    &cedr_route::t3::caches(),
                    &[cedr_route::readers::T3Harness {
                        harness: "claude",
                        instance,
                        effort_option: None,
                    }],
                    chrono::Utc::now(),
                    |h| c.limits(Some(h), None).fresh_seconds,
                )
                .0["harnesses"]
                    .as_array()
                    .is_some_and(|e| !e.is_empty())
        });
    let cache = if installed_claude && !statusline && !from_t3 {
        cedr_route::readers::claude_usage("claude")
    } else {
        Ok(None)
    };
    let (state, message) = match (installed_claude, statusline, &cache) {
        (false, _, _) => ("skip", "harness absent".to_string()),
        _ if from_t3 => ("ok", "readings come from T3 Code".to_string()),
        (true, true, _) => ("ok", "status line configured".to_string()),
        (true, false, Err(e)) => (
            "warn",
            format!("Claude Code's usage cache cannot be read ({e})"),
        ),
        (true, false, Ok(Some(_))) if sources != 1 => (
            "warn",
            format!(
                "no status line, and Claude Code's usage cache is read only with one claude-statusline readings source (the routes have {sources})"
            ),
        ),
        (true, false, Ok(Some(_))) => (
            "ok",
            "no status line; readings come from Claude Code's usage cache".to_string(),
        ),
        (true, false, Ok(None)) => (
            "warn",
            "Claude has no plan reading: no status line runs cedr route statusline (hosts that run Claude Code through its SDK show none), and Claude Code has not cached its plan usage yet".to_string(),
        ),
    };
    add(
        &mut out,
        "route.statusline",
        state,
        message,
        "Configure Claude statusLine.command to run cedr route statusline, or run Claude Code signed in to a Pro or Max plan; without Claude on this machine, add claude to [machine] skip",
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
    for agent in ["claude", "codex", "grok"] {
        let (state, message) = if !installed(agent) {
            ("skip", "harness absent")
        } else {
            match Command::new(agent)
                .args(["mcp", "list"])
                // Never let a check replace the installed Grok Build.
                .env("GROK_DISABLE_AUTOUPDATER", "1")
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
        let edited = r
            .findings
            .iter()
            .filter(|f| f.rule == cedr_core::finding::Rule::Cs002)
            .count();
        add(
            &mut out,
            "sync.check",
            if r.is_empty() { "ok" } else { "fail" },
            if edited == 0 {
                format!("{} Sync findings", r.findings.len())
            } else {
                format!(
                    "{} Sync findings, {edited} of them generated files edited by hand (CS002)",
                    r.findings.len()
                )
            },
            if edited == 0 {
                "Run cedr sync check, then cedr sync"
            } else {
                "Run cedr sync check to see which files; copy their edits into the .cedr/ sources it names (rules in .cedr/rules/, AGENTS.md from .cedr/rules/10-agents.md) and run cedr sync, or run cedr sync --force to discard the edits"
            },
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
    add_shadowed(&mut out, root, config.is_ok());
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
    crate::bench::doctor_checks(root, |id, state, message, fix| {
        add(&mut out, id, state, message, fix)
    });
    let order = [
        "cli.version",
        "key.present",
        "key.valid",
        "org.member",
        "route.config",
        "route.readers",
        "route.orchestrator",
        "route.targets",
        "route.statusline",
        "route.sync",
        "mcp.claude",
        "mcp.codex",
        "mcp.grok",
        "monitor.config",
        "monitor.sources",
        "monitor.push",
        "monitor.schedule",
        "sync.config",
        "sync.check",
        "sync.shadowed",
        "sync.rules",
        "guard.lock",
        "bench.budget",
        "bench.push",
        "bench.suite",
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

/// The agent each T3 Code driver is, under Sync's target names, and whether it reads the shared
/// `.agents/skills/` folder (docs/sync.md, the target table), which Sync writes whatever the targets.
fn sync_target(driver: &str) -> Option<(&'static str, bool)> {
    Some(match driver {
        "claudeAgent" => ("claude", false),
        "codex" => ("codex", true),
        "grok" => ("grok", false),
        "antigravity" => ("antigravity", true),
        "cursor" => ("cursor", true),
        "opencode" => ("opencode", true),
        _ => return None,
    })
}

/// `sync.shadowed`: skills Sync writes into this repo that an agent T3 Code runs also loads from
/// elsewhere under the same name (its user folder, a bundle), so which copy it uses is unclear.
/// T3 Code lists only what agents load outside a repo, so this is the overlap it can show.
fn add_shadowed(out: &mut Vec<Check>, root: &Path, configured: bool) {
    const FIX: &str =
        "Rename the repo skill in .cedr/skills or remove the other copy, then run cedr sync";
    let dir = cedr_route::t3::caches();
    let targets = cedr_sync::load_config(root)
        .map(|c| c.targets)
        .unwrap_or_default();
    let repo: Vec<String> = std::fs::read_dir(root.join(".cedr/skills"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("SKILL.md").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    if !configured || repo.is_empty() || !dir.is_dir() {
        add(
            out,
            "sync.shadowed",
            "skip",
            "no Sync skills, or no T3 Code on this machine",
            FIX,
        );
        return;
    }
    let mut shadowed = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".json").map(str::to_string))
        .collect();
    files.sort();
    for instance in files.iter().take(32) {
        let Ok(Some(p)) = cedr_route::t3::read(&dir, instance) else {
            continue;
        };
        let Some((target, shared)) = sync_target(&p.driver) else {
            continue;
        };
        let written = shared || targets.iter().any(|t| t == target);
        if !p.enabled || !written {
            continue;
        }
        let mut names: Vec<String> = p
            .skills()
            .into_iter()
            .filter(|s| repo.contains(&s.name))
            .map(|s| format!("{} ({})", s.name, s.scope))
            .collect();
        names.sort();
        names.dedup();
        if !names.is_empty() {
            shadowed.push(format!("{target}: {}", names.join(", ")));
        }
    }
    add(
        out,
        "sync.shadowed",
        if shadowed.is_empty() { "ok" } else { "warn" },
        if shadowed.is_empty() {
            "no agent loads another skill under a Sync skill's name".to_string()
        } else {
            format!(
                "agents also load these Sync skill names from elsewhere: {}",
                shadowed.join("; ")
            )
        },
        FIX,
    );
}
