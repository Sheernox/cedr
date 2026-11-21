//! Build an offline starter file and publish it without replacing an existing entry.
use std::fmt::Write as _;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::{CONFIG_VERSION, Config, ReaderKind};
use crate::error::{Error, Result, io_err};
use crate::machine;

struct Agent {
    id: &'static str,
    reader: Option<ReaderKind>,
    strong: &'static str,
    cheap: &'static str,
    models: [&'static str; 2],
}

// Targets name families. Exact ids are fallbacks for readers that have no model catalogue.
const AGENTS: &[Agent] = &[
    Agent {
        id: "claude",
        reader: Some(ReaderKind::ClaudeStatusline),
        strong: "opus",
        cheap: "sonnet",
        models: ["claude-opus-5-5", "claude-sonnet-5-5"],
    },
    Agent {
        id: "codex",
        reader: Some(ReaderKind::Codex),
        strong: "astra",
        cheap: "sol",
        models: ["gpt-6.2-astra", "gpt-6.2-sol"],
    },
    Agent {
        id: "antigravity",
        reader: None,
        strong: "neon",
        cheap: "flash",
        models: ["gemini-4-neon", "gemini-3.8-flash"],
    },
    Agent {
        id: "grok",
        reader: Some(ReaderKind::Grok),
        strong: "grok",
        cheap: "build-fast",
        models: ["grok-4.7", "grok-4.7-build-fast"],
    },
    Agent {
        id: "muse",
        reader: Some(ReaderKind::Muse),
        strong: "wonder",
        cheap: "ember",
        models: ["muse-wonder-1.0", "muse-ember-1.0"],
    },
];

#[derive(Serialize)]
pub struct Orchestrator {
    pub harness: &'static str,
    pub model: String,
}

pub struct Starter {
    pub path: PathBuf,
    pub harnesses: Vec<&'static str>,
    pub orchestrator: Orchestrator,
    pub routes: Vec<&'static str>,
    pub text: String,
}

fn machine_tables(agents: &[&Agent]) -> String {
    let mut text = format!(
        "# Starter routes. Edit these routes or add candidates with cedr route add.\n\
         # Organizations can share routes from the app at cedr.dev.\n\n\
         version = {CONFIG_VERSION}\n\n[[readings]]\nkind = \"t3\"\n"
    );
    for agent in agents {
        let id = agent.id;
        let [strong, cheap] = agent.models;
        writeln!(
            text,
            "\n[harnesses.{id}]\ncommand = \"{id}\"\nmodels = [\"{strong}\", \"{cheap}\"]"
        )
        .unwrap();
        if id == "claude" {
            text.push_str("t3 = \"claudeAgent\"\n");
        }
        if let Some(reader) = agent.reader {
            let reader = reader.as_str();
            writeln!(
                text,
                "reader = \"{reader}\"\n\n[[readings]]\nkind = \"{reader}\"\nharness = \"{id}\""
            )
            .unwrap();
        }
    }
    text
}

/// Uses Route's normal machine check, including managed T3 Code agents.
pub fn detect() -> Result<Vec<String>> {
    let config = Config::parse(
        &machine_tables(&AGENTS.iter().collect::<Vec<_>>()),
        Path::new("routes.toml"),
    )?;
    let (absent, _) = machine::absent(&config);
    Ok(AGENTS
        .iter()
        .filter(|agent| !absent.contains_key(agent.id))
        .map(|agent| agent.id.to_string())
        .collect())
}

pub fn prepare(path: PathBuf, harnesses: &[String], orchestrator: Option<&str>) -> Result<Starter> {
    for id in harnesses {
        if !AGENTS.iter().any(|a| a.id == id) {
            return Err(Error::Usage(format!(
                "unknown harness {id:?}; choose claude, codex, antigravity, grok or muse"
            )));
        }
    }
    let agents: Vec<&Agent> = AGENTS
        .iter()
        .filter(|a| harnesses.iter().any(|h| h == a.id))
        .collect();
    let first = agents.first().ok_or_else(|| {
        Error::Usage("no supported agents detected; install an agent or pass --harness H".into())
    })?;
    let (harness, model) = match orchestrator {
        Some(value) => value
            .split_once('/')
            .filter(|(_, model)| {
                !model.is_empty() && !model.contains('/') && !model.chars().any(char::is_whitespace)
            })
            .ok_or_else(|| Error::Usage("--orchestrator must be H/MODEL".into()))?,
        None => (first.id, first.strong),
    };
    let host = agents.iter().find(|a| a.id == harness).ok_or_else(|| {
        Error::Usage(format!(
            "orchestrator harness {harness:?} is not selected; pass --harness {harness}"
        ))
    })?;
    let mut text = machine_tables(&agents);
    let quoted = toml_edit::Value::from(model);
    writeln!(
        text,
        "\n[orchestrator]\nharness = \"{harness}\"\nmodel = {quoted}"
    )
    .unwrap();

    let mut routes = vec!["review.code", "code.impl", "research.gather"];
    if agents.len() >= 2 {
        routes.push("review.council");
    }
    for (route, work, role) in [
        ("review.code", "Independent code review", "review"),
        ("code.impl", "Implement a code change", "implementation"),
        ("research.gather", "Gather information", "research"),
        ("review.council", "Multi-vendor code review", "review"),
    ] {
        if !routes.contains(&route) {
            continue;
        }
        writeln!(
            text,
            "\n[routes.\"{route}\"]\nwork = \"{work}\"\nrole = \"{role}\""
        )
        .unwrap();
        let council = route == "review.council";
        if council {
            text.push_str("council = true\n");
        }
        text.push_str("candidates = [\n");
        if route == "code.impl" {
            for agent in &agents {
                writeln!(
                    text,
                    "  {{ target = \"{}/{}\", tier = 1, sizes = [\"small\", \"medium\"] }},",
                    agent.id, agent.cheap
                )
                .unwrap();
            }
        }
        for agent in &agents {
            let id = agent.id;
            if route == "review.code" && id == harness {
                continue;
            }
            let family = if route == "research.gather" {
                agent.cheap
            } else {
                agent.strong
            };
            let tier = if route == "code.impl" { 2 } else { 1 };
            writeln!(text, "  {{ target = \"{id}/{family}\", tier = {tier} }},").unwrap();
        }
        if !council {
            text.push_str("  { target = \"self\" },\n");
        }
        text.push_str("]\n");
    }

    Config::parse(&text, &path)?;
    Ok(Starter {
        path,
        harnesses: agents.iter().map(|a| a.id).collect(),
        orchestrator: Orchestrator {
            harness: host.id,
            model: model.to_string(),
        },
        routes,
        text,
    })
}

fn already(path: &Path) -> Error {
    Error::Usage(format!(
        "{} already exists; use cedr route add or edit the file",
        path.display()
    ))
}

/// Refuse directories and symlinks too, including dangling links.
pub fn ensure_new(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Err(already(path)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path)(e)),
    }
}

impl Starter {
    /// Publish only a complete file and let the filesystem arbitrate concurrent writers.
    pub fn write(&self) -> Result<()> {
        let path = &self.path;
        ensure_new(path)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(io_err(path))?;
        tmp.write_all(self.text.as_bytes()).map_err(io_err(path))?;
        tmp.as_file().sync_all().map_err(io_err(path))?;
        tmp.persist_noclobber(path).map_err(|e| {
            if e.error.kind() == ErrorKind::AlreadyExists {
                already(path)
            } else {
                io_err(path)(e.error)
            }
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::AGENTS;
    use crate::pool::resolve;
    use crate::readings::Readings;

    #[test]
    fn starter_families_resolve_to_their_fallback_models() {
        let mut readings = Readings::default();
        for agent in AGENTS {
            readings.models.insert(
                agent.id.to_string(),
                agent.models.iter().map(|m| m.to_string()).collect(),
            );
        }
        for agent in AGENTS {
            for (family, model) in [
                (agent.strong, agent.models[0]),
                (agent.cheap, agent.models[1]),
            ] {
                assert_eq!(
                    resolve(agent.id, family, &readings).as_deref(),
                    Some(model),
                    "{}/{family}",
                    agent.id
                );
            }
        }
    }
}
