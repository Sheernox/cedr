//! What this machine has (docs/route.md, Machine checks): harnesses absent here, readers its file
//! is missing, and the id and name the app shows it by. Nothing here runs a program to check.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

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
    for (id, h) in &config.harnesses {
        if out.contains_key(id) {
            continue;
        }
        if let Some(command) = &h.command
            && !installed(command)
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
