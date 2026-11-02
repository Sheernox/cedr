//! Suite files (docs/bench.md, Suite): parsing, validation and the digest that identifies a suite.
//! A suite is code the user chose to run, but it still cannot set its own budget, and every value
//! that reaches a command line is checked so a typo cannot turn into a flag.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Result, exec, invalid, io_error};

pub const DEFAULT_TRIALS: u32 = 3;
pub const MAX_TRIALS: u32 = 100;
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HarnessKind {
    Claude,
    Codex,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permissions {
    #[default]
    Workspace,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub harness: HarnessKind,
    pub model: String,
    pub effort: Option<String>,
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub permissions: Permissions,
    pub command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub prompt: Option<String>,
    pub prompt_file: Option<String>,
    pub setup: Option<String>,
    pub test: String,
    pub test_files: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub version: u32,
    #[serde(default = "default_trials")]
    pub trials: u32,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    pub baseline: Variant,
    pub candidate: Variant,
    pub tasks: Vec<Task>,
}

fn default_trials() -> u32 {
    DEFAULT_TRIALS
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

/// A validated suite with where it came from.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub suite: Suite,
    /// The file stem, shown in reports and recorded in results.
    pub name: String,
    pub path: PathBuf,
    /// The git top level of the suite file; `ref`s and relative paths resolve against it.
    pub root: PathBuf,
    /// Digest of the suite file and the prompt and test files it names.
    pub sha256: String,
}

/// The suite named by `arg`, or the only `*.toml` in `.cedr/bench/` of the repo at `cwd`.
pub fn locate(arg: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(path) = arg {
        return Ok(path.to_path_buf());
    }
    let root = exec::git_root(cwd)?;
    let dir = root.join(".cedr").join("bench");
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(invalid(format!(
                "no suite: pass a path or add one to {}",
                dir.display()
            )));
        }
        Err(e) => return Err(io_error(&dir)(e)),
    };
    for entry in entries {
        let path = entry.map_err(io_error(&dir))?.path();
        if path.extension().is_some_and(|x| x == "toml") {
            found.push(path);
        }
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(invalid(format!(
            "no suite: pass a path or add one to {}",
            dir.display()
        ))),
        n => Err(invalid(format!(
            "{n} suites in {}: pass the one to use",
            dir.display()
        ))),
    }
}

pub fn load(path: &Path) -> Result<Loaded> {
    let text = std::fs::read_to_string(path).map_err(io_error(path))?;
    let suite: Suite = toml::from_str(&text)
        .map_err(|e| invalid(format!("{}: {}", path.display(), e.message())))?;
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let root = exec::git_root(parent)?;
    validate(&suite, path)?;
    for task in &suite.tasks {
        for (key, rel) in [
            ("prompt_file", &task.prompt_file),
            ("test_files", &task.test_files),
        ] {
            if let Some(rel) = rel {
                let full = root.join(rel);
                if !full.exists() {
                    return Err(invalid(format!(
                        "{}: tasks.{}.{key}: {rel} does not exist",
                        path.display(),
                        task.id
                    )));
                }
            }
        }
    }
    let sha256 = digest(&text, &suite, &root)?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "suite".into());
    Ok(Loaded {
        suite,
        name,
        path: path.to_path_buf(),
        root,
        sha256,
    })
}

fn validate(suite: &Suite, path: &Path) -> Result<()> {
    let file = path.display();
    if suite.version != 1 {
        return Err(invalid(format!(
            "{file}: version: unknown version {}, this cedr reads 1",
            suite.version
        )));
    }
    if !(1..=MAX_TRIALS).contains(&suite.trials) {
        return Err(invalid(format!(
            "{file}: trials: must be 1 to {MAX_TRIALS}"
        )));
    }
    if suite.timeout_seconds == 0 {
        return Err(invalid(format!("{file}: timeout_seconds: must be above 0")));
    }
    validate_variant(&suite.baseline, "baseline", path)?;
    validate_variant(&suite.candidate, "candidate", path)?;
    if suite.tasks.is_empty() {
        return Err(invalid(format!(
            "{file}: tasks: a suite needs at least one"
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    for task in &suite.tasks {
        if !is_task_id(&task.id) {
            return Err(invalid(format!(
                "{file}: tasks.id: \"{}\" must be letters, digits, ., _ and -",
                task.id
            )));
        }
        if !seen.insert(task.id.as_str()) {
            return Err(invalid(format!(
                "{file}: tasks.id: \"{}\" appears twice",
                task.id
            )));
        }
        if task.prompt.is_some() == task.prompt_file.is_some() {
            return Err(invalid(format!(
                "{file}: tasks.{}: set one of prompt and prompt_file",
                task.id
            )));
        }
        if task.test.trim().is_empty() {
            return Err(invalid(format!(
                "{file}: tasks.{}.test: must not be empty",
                task.id
            )));
        }
        for (key, rel) in [
            ("prompt_file", &task.prompt_file),
            ("test_files", &task.test_files),
        ] {
            if rel.as_deref().is_some_and(|r| !is_contained(r)) {
                return Err(invalid(format!(
                    "{file}: tasks.{}.{key}: must be relative to the repo root, without ..",
                    task.id
                )));
            }
        }
    }
    Ok(())
}

fn validate_variant(variant: &Variant, name: &str, path: &Path) -> Result<()> {
    let file = path.display();
    if !is_flag_safe(&variant.model) {
        return Err(invalid(format!(
            "{file}: {name}.model: \"{}\" must be letters, digits, ., _, :, /, [, ] and -, and cannot start with -",
            variant.model
        )));
    }
    if let Some(effort) = &variant.effort
        && !is_flag_safe(effort)
    {
        return Err(invalid(format!(
            "{file}: {name}.effort: \"{effort}\" must be letters, digits, ., _ and -, and cannot start with -"
        )));
    }
    if let Some(git_ref) = &variant.git_ref
        && (git_ref.is_empty() || git_ref.starts_with('-'))
    {
        return Err(invalid(format!(
            "{file}: {name}.ref: \"{git_ref}\" is not a git ref"
        )));
    }
    match (variant.harness, &variant.command) {
        (HarnessKind::Command, None) => Err(invalid(format!(
            "{file}: {name}.command: the command harness needs a command"
        ))),
        (HarnessKind::Command, Some(argv)) if argv.is_empty() || argv[0].is_empty() => Err(
            invalid(format!("{file}: {name}.command: must name a program")),
        ),
        (HarnessKind::Claude | HarnessKind::Codex, Some(_)) => Err(invalid(format!(
            "{file}: {name}.command: only the command harness takes a command"
        ))),
        _ => Ok(()),
    }
}

fn is_task_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn is_flag_safe(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/' | b'[' | b']')
        })
}

/// Relative with no `..`, so a suite names only files inside its own repo.
pub(crate) fn is_contained(rel: &str) -> bool {
    let path = Path::new(rel);
    !rel.is_empty()
        && !rel.starts_with(['/', '\\'])
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

fn digest(text: &str, suite: &Suite, root: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(text.as_bytes());
    for task in &suite.tasks {
        if let Some(rel) = &task.prompt_file {
            let path = root.join(rel);
            hash.update(std::fs::read(&path).map_err(io_error(&path))?);
        }
        if let Some(rel) = &task.test_files {
            for (name, path) in exec::walk_files(&root.join(rel))? {
                hash.update(name.as_bytes());
                hash.update(std::fs::read(&path).map_err(io_error(&path))?);
            }
        }
    }
    Ok(hex(&hash.finalize()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Task {
    pub fn prompt_text(&self, root: &Path) -> Result<String> {
        match (&self.prompt, &self.prompt_file) {
            (Some(prompt), _) => Ok(prompt.clone()),
            (None, Some(rel)) => {
                let path = root.join(rel);
                std::fs::read_to_string(&path).map_err(io_error(&path))
            }
            (None, None) => Err(invalid(format!("tasks.{}: no prompt", self.id))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
version = 1
[baseline]
harness = "claude"
model = "claude-sonnet-5-5"
[candidate]
harness = "codex"
model = "gpt-6.1-sol"
[[tasks]]
id = "a"
prompt = "do it"
test = "true"
"#;

    fn parse(text: &str) -> Result<()> {
        let suite: Suite = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
        validate(&suite, Path::new("s.toml"))
    }

    fn error(text: &str) -> String {
        parse(text).unwrap_err().to_string()
    }

    #[test]
    fn a_minimal_suite_takes_the_defaults() {
        let suite: Suite = toml::from_str(VALID).unwrap();
        validate(&suite, Path::new("s.toml")).unwrap();
        assert_eq!(suite.trials, DEFAULT_TRIALS);
        assert_eq!(suite.timeout_seconds, DEFAULT_TIMEOUT_SECONDS);
        assert_eq!(suite.baseline.permissions, Permissions::Workspace);
    }

    #[test]
    fn a_suite_cannot_set_its_own_budget() {
        let text = VALID.replace("version = 1", "version = 1\nbudget_usd = 500.0");
        assert!(error(&text).contains("budget_usd"));
    }

    #[test]
    fn values_that_reach_a_command_line_cannot_become_flags() {
        let bad_model = VALID.replace("gpt-6.1-sol", "--dangerously-bypass");
        assert!(error(&bad_model).contains("candidate.model"));
        let bad_ref = VALID.replace(
            "model = \"gpt-6.1-sol\"",
            "model = \"m\"\nref = \"--upload-pack=x\"",
        );
        assert!(error(&bad_ref).contains("candidate.ref"));
        let bad_effort = VALID.replace(
            "model = \"gpt-6.1-sol\"",
            "model = \"m\"\neffort = \"low\\\"; x\"",
        );
        assert!(error(&bad_effort).contains("candidate.effort"));
    }

    #[test]
    fn a_task_names_exactly_one_prompt_and_stays_inside_the_repo() {
        let both = VALID.replace("prompt = \"do it\"", "prompt = \"a\"\nprompt_file = \"b\"");
        assert!(error(&both).contains("one of prompt"));
        let neither = VALID.replace("prompt = \"do it\"", "");
        assert!(error(&neither).contains("one of prompt"));
        for bad in ["../../etc/passwd", "/etc/passwd", "a/../../b"] {
            let text = VALID.replace(
                "test = \"true\"",
                &format!("test = \"true\"\ntest_files = \"{bad}\""),
            );
            assert!(error(&text).contains("test_files"), "{bad}");
        }
    }

    #[test]
    fn duplicate_tasks_and_a_misplaced_command_are_refused() {
        let twice = format!("{VALID}\n[[tasks]]\nid = \"a\"\nprompt = \"p\"\ntest = \"t\"\n");
        assert!(error(&twice).contains("appears twice"));
        let misplaced = VALID.replace(
            "harness = \"codex\"",
            "harness = \"codex\"\ncommand = [\"x\"]",
        );
        assert!(error(&misplaced).contains("candidate.command"));
        let missing = VALID.replace("harness = \"codex\"", "harness = \"command\"");
        assert!(error(&missing).contains("candidate.command"));
    }
}
