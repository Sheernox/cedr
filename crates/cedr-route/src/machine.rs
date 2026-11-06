//! What this machine has (docs/route.md, Machine checks): harnesses absent here, readers its file
//! is missing, and the id and name the app shows it by. Nothing here runs a program to check.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::{Config, xdg_dir};
use crate::error::{Result, io_err};
use crate::readings::Readings;

/// A plan harness whose expected built-in reader this machine's file does not configure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stale {
    pub harness: String,
    pub reader: &'static str,
}

/// Harnesses absent on this machine, with their pool summary, plus a warning per unknown skip name.
pub fn absent(config: &Config) -> (HashMap<String, String>, Vec<String>) {
    absent_with(
        config,
        &t3_providers(),
        &crate::t3::caches(),
        chrono::Utc::now(),
    )
}

fn absent_with(
    config: &Config,
    t3_providers: &Path,
    t3_caches: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> (HashMap<String, String>, Vec<String>) {
    let mut out = HashMap::new();
    let mut warnings = Vec::new();
    let skip = config
        .machine
        .as_ref()
        .map(|m| m.skip.as_slice())
        .unwrap_or_default();
    for name in skip {
        match config.harness_for(name) {
            Some(id) => {
                out.insert(id.to_string(), format!("{id} skipped on this machine"));
            }
            None => warnings.push(format!(
                "machine.skip names {name:?}, which the routes do not define"
            )),
        }
    }
    // A machine that reads T3 Code delegates through it, so T3 Code's current view of a plan
    // decides whether that plan can take work here.
    let reads_t3 = config
        .readings
        .iter()
        .any(|s| matches!(s, crate::config::ReadingsSource::T3));
    for (id, h) in &config.harnesses {
        if out.contains_key(id) {
            continue;
        }
        let instance = h.t3.as_deref().unwrap_or(id);
        let seen = (reads_t3 && h.billing == crate::config::Billing::Plan)
            .then(|| crate::t3::read(t3_caches, instance).ok().flatten())
            .flatten()
            .filter(|p| p.current(now));
        if let Some(p) = &seen {
            if !p.enabled {
                out.insert(id.clone(), format!("{id} disabled in T3 Code"));
                continue;
            }
            if p.signed_out() {
                out.insert(id.clone(), format!("{id} signed out in T3 Code"));
                continue;
            }
        }
        if let Some(command) = &h.command
            && !installed(command)
            && !t3_profile(t3_providers, instance)
            && seen.as_ref().and_then(|p| p.installed()) != Some(true)
        {
            out.insert(id.clone(), format!("{id} not installed ({command})"));
        }
    }
    (out, warnings)
}

/// Plan harnesses with a reader hint, not absent, that no local built-in reader covers and no
/// other source gave a reading for: the routes gained a plan after this machine was set up.
pub fn stale(config: &Config, readings: &Readings) -> Vec<Stale> {
    config
        .harnesses
        .iter()
        .filter(|(_, h)| h.billing == crate::config::Billing::Plan)
        .filter_map(|(id, h)| Some((id, h.reader?)))
        .filter(|(id, kind)| {
            !readings.absent.contains_key(*id)
                && !readings.entries.contains_key(*id)
                && !config.readings.iter().any(|s| {
                    s.built_in().is_some_and(|(k, harness)| {
                        k == *kind && config.harness_for(harness) == Some(id.as_str())
                    })
                })
        })
        .map(|(id, kind)| Stale {
            harness: id.clone(),
            reader: kind.as_str(),
        })
        .collect()
}

/// The warning shown for a stale harness, worded for the person who has to fix it.
pub fn stale_warning(s: &Stale) -> String {
    format!(
        "this machine has no {reader} usage reader for {harness}, which the routes expect: rerun \"Set up this machine\" in the app, or add [[readings]] kind = \"{reader}\", harness = \"{harness}\"",
        reader = s.reader,
        harness = s.harness
    )
}

/// Where T3 Code keeps the homes it gives the agents it runs itself: `$T3CODE_HOME` (default
/// `~/.t3`) `/userdata/providers/<driver>/<SHA-256 of the provider instance id>/`.
fn t3_providers() -> PathBuf {
    xdg_dir("T3CODE_HOME", ".t3")
        .join("userdata")
        .join("providers")
}

/// Whether T3 Code has a provider profile for `instance`: T3 runs that agent without a program on
/// `PATH`, so a missing `command` does not make the harness absent.
fn t3_profile(providers: &Path, instance: &str) -> bool {
    let name: String = Sha256::digest(instance.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    std::fs::read_dir(providers).is_ok_and(|drivers| {
        drivers
            .flatten()
            .any(|driver| driver.path().join(&name).is_dir())
    })
}

/// Whether `command` is an executable file: an absolute path as given, a name on `PATH`.
fn installed(command: &str) -> bool {
    let path = Path::new(command);
    if path.is_absolute() {
        return executable(path) || windows_variants(path);
    }
    let Some(dirs) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&dirs).any(|dir| {
        let candidate = dir.join(command);
        executable(&candidate) || windows_variants(&candidate)
    })
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(path: &Path) -> bool {
    path.is_file()
}

/// On Windows a program is found by its name plus one of `PATHEXT`'s extensions.
#[cfg(windows)]
fn windows_variants(candidate: &Path) -> bool {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".into());
    exts.split(';')
        .filter(|e| !e.is_empty())
        .any(|e| executable(&candidate.with_extension(e.trim_start_matches('.'))))
}

#[cfg(not(windows))]
fn windows_variants(_: &Path) -> bool {
    false
}

/// This machine's id for the app: `m-` and 12 hex characters, made once and kept in the state dir.
pub fn machine_id() -> Result<String> {
    let path = xdg_dir("XDG_STATE_HOME", ".local/state")
        .join("cedr")
        .join("machine-id");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let id = text.trim();
        if is_machine_id(id) {
            return Ok(id.to_string());
        }
    }
    let id = format!("m-{}", &crate::pick::new_id()[2..]);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    std::fs::write(&path, format!("{id}\n")).map_err(io_err(&path))?;
    Ok(id)
}

fn is_machine_id(s: &str) -> bool {
    s.len() == 14
        && s.starts_with("m-")
        && s[2..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `[machine] name`, else the host name up to its first dot, at most 64 characters.
pub fn machine_name(config: &Config) -> String {
    if let Some(name) = config.machine.as_ref().and_then(|m| m.name.as_deref()) {
        return name.trim().to_string();
    }
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| {
            std::process::Command::new("hostname")
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
        })
        .unwrap_or_default();
    let short: String = host
        .trim()
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    if short.is_empty() {
        "unnamed".to_string()
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readings::Entry;

    fn config(text: &str) -> Config {
        Config::parse(text, Path::new("t.toml")).unwrap()
    }

    const BASE: &str = r#"
version = 1
[harnesses.codex]
reader = "codex"
[harnesses.claude]
reader = "claude-statusline"
command = "/nonexistent/claude"
[harnesses.codex-api]
billing = "api"
reader = "codex"
[routes.r]
candidates = [{ target = "codex/sol" }, { target = "claude/sonnet" }]
"#;

    #[test]
    fn skipped_and_missing_programs_are_absent() {
        let c = config(&format!(
            "{BASE}\n[machine]\nskip = [\"codex\", \"gone\"]\n"
        ));
        let (absent, warnings) = absent(&c);
        assert_eq!(absent["codex"], "codex skipped on this machine");
        assert_eq!(
            absent["claude"],
            "claude not installed (/nonexistent/claude)"
        );
        assert!(!absent.contains_key("codex-api"));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("\"gone\""));
    }

    #[test]
    fn a_t3_provider_profile_counts_as_installed() {
        let text = "version = 1\n[harnesses.antigravity]\ncommand = \"/nonexistent/agy\"\n[harnesses.work]\ncommand = \"/nonexistent/agy\"\nt3 = \"agy_work\"\n[harnesses.other]\ncommand = \"/nonexistent/agy\"\n";
        let c = config(text);
        let dir = tempfile::tempdir().unwrap();
        let hex = |s: &str| -> String {
            Sha256::digest(s.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        // Profiles are named by the T3 instance id (the `t3` key, else the harness id), under any driver.
        std::fs::create_dir_all(dir.path().join("antigravity").join(hex("antigravity"))).unwrap();
        std::fs::create_dir_all(dir.path().join("antigravity").join(hex("agy_work"))).unwrap();
        // A file of that name is not a profile.
        std::fs::write(dir.path().join("antigravity").join(hex("other")), "").unwrap();
        let (absent, _) = absent_with(&c, dir.path(), dir.path(), now());
        assert!(!absent.contains_key("antigravity"));
        assert!(!absent.contains_key("work"));
        assert_eq!(absent["other"], "other not installed (/nonexistent/agy)");
        // A skip still wins over a profile.
        let c = config(&format!("{text}[machine]\nskip = [\"antigravity\"]\n"));
        let (absent, _) = absent_with(&c, dir.path(), dir.path(), now());
        assert_eq!(absent["antigravity"], "antigravity skipped on this machine");
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-10-29T02:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn t3_codes_current_view_decides_presence_on_a_machine_that_reads_it() {
        let text = "version = 1\n[harnesses.claude]\ncommand = \"/nonexistent/claude\"\nt3 = \"claudeAgent\"\n[harnesses.codex]\ncommand = \"/nonexistent/codex\"\n[harnesses.grok]\n[harnesses.muse]\ncommand = \"/nonexistent/muse\"\n[harnesses.old]\n[harnesses.future]\n[[readings]]\nkind = \"t3\"\n";
        let dir = tempfile::tempdir().unwrap();
        let cache = |name: &str, body: serde_json::Value| {
            std::fs::write(dir.path().join(format!("{name}.json")), body.to_string()).unwrap()
        };
        let at = "2026-10-29T01:58:00Z";
        cache(
            "claudeAgent",
            // A null `auth` is an unknown sign-in, not an unreadable cache: installed still counts.
            serde_json::json!({"checkedAt": at, "installed": true, "enabled": true, "auth": null}),
        );
        cache(
            "codex",
            serde_json::json!({"checkedAt": at, "installed": true, "enabled": false}),
        );
        cache(
            "grok",
            serde_json::json!({"checkedAt": at, "auth": {"status": "unauthenticated"}}),
        );
        // Muse reports no sign-in status at all: that is not signed out.
        cache(
            "muse",
            serde_json::json!({"checkedAt": at, "installed": true, "auth": {"status": "unknown"}}),
        );
        // A view from the future (a clock moved back) says nothing either.
        cache(
            "future",
            serde_json::json!({"checkedAt": "2026-10-29T05:00:00Z", "enabled": false}),
        );
        // Hours old: T3 Code is not running, so its view says nothing.
        cache(
            "old",
            serde_json::json!({"checkedAt": "2026-10-28T20:00:00Z", "enabled": false}),
        );
        let (absent, _) = absent_with(&config(text), Path::new("/nonexistent"), dir.path(), now());
        let mut found: Vec<(&str, &str)> = absent
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                ("codex", "codex disabled in T3 Code"),
                ("grok", "grok signed out in T3 Code")
            ]
        );
        // Without a t3 source, T3 Code's view is not consulted.
        let (absent, _) = absent_with(
            &config(&text.replace("[[readings]]\nkind = \"t3\"\n", "")),
            Path::new("/nonexistent"),
            dir.path(),
            now(),
        );
        assert_eq!(
            absent.len(),
            3,
            "claude, codex and muse are simply not installed: {absent:?}"
        );
    }

    #[test]
    fn a_plan_reader_hint_without_a_source_or_reading_is_stale() {
        let c = config(BASE);
        let mut r = Readings::default();
        // claude is absent here, the api harness needs no reader, codex has nothing: only codex is stale.
        r.absent.insert("claude".into(), String::new());
        assert_eq!(
            stale(&c, &r),
            vec![Stale {
                harness: "codex".into(),
                reader: "codex"
            }]
        );
        // A feed that covers the harness is enough.
        r.entries.insert(
            "codex".into(),
            Entry {
                fetched_at: None,
                error: false,
                windows: vec![],
            },
        );
        assert!(stale(&c, &r).is_empty());
        // So is the built-in reader, even before it has read anything.
        let c = config(&format!("{BASE}\n[[readings]]\nkind = \"codex\"\n"));
        assert!(
            stale(&c, &Readings::default())
                .iter()
                .all(|s| s.harness != "codex")
        );
    }

    #[test]
    fn commands_must_be_bare_names_or_absolute_paths() {
        for bad in ["bin/codex", "..\\tool", "codex --x", ""] {
            let text = format!("version = 1\n[harnesses.codex]\ncommand = {bad:?}\n");
            assert!(Config::parse(&text, Path::new("t.toml")).is_err(), "{bad}");
        }
        assert!(
            Config::parse(
                "version = 1\n[harnesses.codex]\ncommand = \"/usr/bin/codex\"\n",
                Path::new("t.toml")
            )
            .is_ok()
        );
    }

    #[test]
    fn shared_routes_name_programs_never_paths() {
        let text = "version = 1\n[harnesses.codex]\ncommand = \"/home/alice/bin/codex\"\n";
        assert!(crate::config::parse_shared(text, Path::new("org.toml")).is_err());
        let text = "version = 1\n[harnesses.codex]\ncommand = \"codex\"\n";
        assert!(crate::config::parse_shared(text, Path::new("org.toml")).is_ok());
    }
}
