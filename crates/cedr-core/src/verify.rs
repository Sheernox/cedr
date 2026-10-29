//! `cedr verify`: compares the repo with its `cedr.lock` without touching the network.

use std::collections::BTreeMap;
use std::path::Path;

use crate::discovery::{self, Client};
use crate::finding::{Finding, Report, Rule, Severity};
use crate::lock::{self, LOCK_FILE};
use crate::source::Source;
use crate::{Result, skill};

pub fn verify(root: &Path) -> Result<Report> {
    let lock = lock::read_lock(root)?;
    let ws = discovery::discover(root)?;
    let mut findings = Vec::new();
    let error = |rule, path: &str, line, message: String| {
        Finding::new(rule, Severity::Error, path, line, message)
    };

    let current_skills: BTreeMap<&str, &discovery::SkillDir> =
        ws.skills.iter().map(|s| (s.path.as_str(), s)).collect();
    for entry in &lock.skill {
        match current_skills.get(entry.path.as_str()) {
            None => findings.push(error(
                Rule::Cv003,
                &entry.path,
                None,
                format!("skill \"{}\" was removed since it was pinned", entry.name),
            )),
            Some(dir) => {
                if skill::content_hash(&dir.real_path)? != entry.content {
                    findings.push(error(
                        Rule::Cv001,
                        &entry.path,
                        None,
                        format!(
                            "skill \"{}\" content changed since it was pinned",
                            entry.name
                        ),
                    ));
                }
            }
        }
    }
    for dir in &ws.skills {
        if !lock.skill.iter().any(|e| e.path == dir.path) {
            findings.push(error(
                Rule::Cv002,
                &dir.path,
                None,
                format!(
                    "skill \"{}\" was added and is not in {LOCK_FILE}",
                    dir.dir_name
                ),
            ));
        }
    }

    let current_servers: BTreeMap<(Client, &str), &discovery::Server> = ws
        .servers
        .iter()
        .map(|s| ((s.client, s.name.as_str()), s))
        .collect();
    for entry in &lock.mcp {
        let label = format!("MCP server \"{}\" ({})", entry.name, entry.client.as_str());
        let Some(server) = current_servers.get(&(entry.client, entry.name.as_str())) else {
            findings.push(error(
                Rule::Cv003,
                LOCK_FILE,
                None,
                format!("{label} was removed since it was pinned"),
            ));
            continue;
        };
        if entry.launch != server.launch_hash() {
            findings.push(error(
                Rule::Cv005,
                &server.config_path,
                server.line,
                format!("{label} launch changed since it was pinned"),
            ));
        }
        let current = server.source();
        // An unpinned entry is recorded at the version it resolved to, while
        // the config still names none; it matches while both stay unpinned
        // and name the same package.
        let unchanged = match entry.source.parse::<Source>() {
            Ok(pinned) if !entry.pinned && !current.pinned() => {
                pinned.identity() == current.identity()
            }
            _ => entry.source == current.to_string(),
        };
        if !unchanged {
            findings.push(error(
                Rule::Cv004,
                &server.config_path,
                server.line,
                format!("{label} source changed: {} -> {current}", entry.source),
            ));
        }
    }
    for server in &ws.servers {
        let known = lock
            .mcp
            .iter()
            .any(|e| e.client == server.client && e.name == server.name);
        if !known {
            findings.push(error(
                Rule::Cv002,
                &server.config_path,
                server.line,
                format!(
                    "MCP server \"{}\" ({}) was added and is not in {LOCK_FILE}",
                    server.name,
                    server.client.as_str()
                ),
            ));
        }
    }
    Ok(Report::new(findings))
}
