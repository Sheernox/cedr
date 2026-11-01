//! `.cedr/guard.toml`: extra skill roots and lint suppression (docs/design.md, Config). A missing
//! file is the empty config; anything unknown or invalid fails, so a typo never silently widens
//! or narrows what Guard checks.

use std::path::{Component, Path};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use crate::Result;
use crate::discovery::read_optional;
use crate::error::{parse_err, toml_err};
use crate::finding::{Finding, Rule};

pub const CONFIG_FILE: &str = ".cedr/guard.toml";

#[derive(Debug)]
pub struct GuardConfig {
    /// Extra skill roots relative to the repo root, normalised to `/` separators.
    pub skills: Vec<String>,
    /// Least minutes between reads of the remote servers.
    pub remote_interval: u32,
    ignore: Vec<Rule>,
    allow: Vec<(Rule, GlobSet)>,
}

const DEFAULT_REMOTE_INTERVAL: u32 = 60;
const REMOTE_INTERVALS: std::ops::RangeInclusive<u32> = 5..=10080;

impl Default for GuardConfig {
    fn default() -> Self {
        GuardConfig {
            skills: Vec::new(),
            remote_interval: DEFAULT_REMOTE_INTERVAL,
            ignore: Vec::new(),
            allow: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    lint: RawLint,
    #[serde(default)]
    remote: RawRemote,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRemote {
    interval_minutes: Option<u32>,
}

fn default_version() -> u32 {
    1
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLint {
    #[serde(default)]
    ignore: Vec<String>,
    #[serde(default)]
    allow: Vec<RawAllow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAllow {
    rule: String,
    paths: Vec<String>,
}

impl GuardConfig {
    /// Reads `root/.cedr/guard.toml`, or the empty config when there is none.
    pub fn load(root: &Path) -> Result<GuardConfig> {
        let path = root.join(CONFIG_FILE);
        let Some(text) = read_optional(&path)? else {
            return Ok(GuardConfig::default());
        };
        let raw: Raw = toml::from_str(&text).map_err(|e| toml_err(&path, &text, &e))?;
        if raw.version != 1 {
            return Err(parse_err(
                &path,
                format!("unsupported version {}", raw.version),
            ));
        }
        let remote_interval = raw
            .remote
            .interval_minutes
            .unwrap_or(DEFAULT_REMOTE_INTERVAL);
        if !REMOTE_INTERVALS.contains(&remote_interval) {
            return Err(parse_err(
                &path,
                format!("remote.interval_minutes: {remote_interval} is outside 5-10080"),
            ));
        }
        let rule = |key: &str, id: &str| {
            Rule::lint(id).ok_or_else(|| {
                parse_err(
                    &path,
                    format!("{key}: unknown rule \"{id}\" (expected CG001-CG010)"),
                )
            })
        };
        let mut skills = Vec::new();
        for dir in &raw.skills {
            let normal = normalise(dir).ok_or_else(|| {
                parse_err(
                    &path,
                    format!("skills: \"{dir}\" must be a relative path inside the repo"),
                )
            })?;
            if !root.join(&normal).is_dir() {
                return Err(parse_err(
                    &path,
                    format!("skills: \"{dir}\" is not a directory"),
                ));
            }
            skills.push(normal);
        }
        let ignore = raw
            .lint
            .ignore
            .iter()
            .map(|id| rule("lint.ignore", id))
            .collect::<Result<_>>()?;
        let mut allow = Vec::new();
        for entry in &raw.lint.allow {
            let mut set = GlobSetBuilder::new();
            for glob in &entry.paths {
                let glob = Glob::new(glob).map_err(|e| {
                    parse_err(&path, format!("lint.allow: invalid glob \"{glob}\": {e}"))
                })?;
                set.add(glob);
            }
            let set = set
                .build()
                .map_err(|e| parse_err(&path, format!("lint.allow: {e}")))?;
            allow.push((rule("lint.allow.rule", &entry.rule)?, set));
        }
        Ok(GuardConfig {
            remote_interval,
            skills,
            ignore,
            allow,
        })
    }

    /// Drops the lint findings the config ignores or allows at their path.
    pub fn suppress(&self, findings: Vec<Finding>) -> Vec<Finding> {
        findings
            .into_iter()
            .filter(|f| {
                !self.ignore.contains(&f.rule)
                    && !self
                        .allow
                        .iter()
                        .any(|(rule, paths)| *rule == f.rule && paths.is_match(&f.path))
            })
            .collect()
    }
}

/// `dir` as plain relative components joined by `/`, or `None` when it is absolute, empty or
/// steps outside with `..`.
fn normalise(dir: &str) -> Option<String> {
    let mut parts = Vec::new();
    for component in Path::new(dir).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::Severity;

    fn load(text: &str) -> std::result::Result<GuardConfig, String> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".cedr")).unwrap();
        std::fs::create_dir_all(dir.path().join("agents/skills")).unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), text).unwrap();
        GuardConfig::load(dir.path()).map_err(|e| e.to_string())
    }

    fn finding(rule: Rule, path: &str) -> Finding {
        Finding::new(rule, Severity::Warning, path, None, "m")
    }

    #[test]
    fn suppresses_ignored_rules_and_allowed_paths_only() {
        let config = load(
            "version = 1\n[lint]\nignore = [\"CG002\"]\n[[lint.allow]]\nrule = \"CG009\"\npaths = [\".agents/skills/audit/**\"]\n",
        )
        .unwrap();
        let kept = config.suppress(vec![
            finding(Rule::Cg002, "AGENTS.md"),
            finding(Rule::Cg009, ".agents/skills/audit/SKILL.md"),
            finding(Rule::Cg009, ".agents/skills/other/SKILL.md"),
            finding(Rule::Cg005, ".agents/skills/audit/SKILL.md"),
        ]);
        let kept: Vec<_> = kept.iter().map(|f| (f.rule, f.path.as_str())).collect();
        assert_eq!(
            kept,
            [
                (Rule::Cg009, ".agents/skills/other/SKILL.md"),
                (Rule::Cg005, ".agents/skills/audit/SKILL.md"),
            ]
        );
    }

    #[test]
    fn normalises_skill_roots() {
        let config = load("version = 1\nskills = [\"./agents/skills/\"]\n").unwrap();
        assert_eq!(config.skills, ["agents/skills"]);
    }

    #[test]
    fn refuses_what_would_silently_change_the_checks() {
        for (text, needle) in [
            ("version = 2\n", "unsupported version 2"),
            ("version = 1\nskill = []\n", "unknown field"),
            ("version = 1\nskills = [\"../x\"]\n", "relative path inside"),
            ("version = 1\nskills = [\"/etc\"]\n", "relative path inside"),
            ("version = 1\nskills = [\"missing\"]\n", "not a directory"),
            (
                "version = 1\n[lint]\nignore = [\"CV001\"]\n",
                "unknown rule \"CV001\"",
            ),
            (
                "version = 1\n[[lint.allow]]\nrule = \"CG002\"\npaths = [\"a[\"]\n",
                "invalid glob",
            ),
        ] {
            let err = load(text).unwrap_err();
            assert!(err.contains(needle), "{text:?}: {err}");
        }
    }
}
