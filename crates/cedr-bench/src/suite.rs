//! Edition validation and canonical content digests, independent of network or Docker.
use crate::{Result, sha};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Edition {
    pub number: u32,
    pub goal_usd: f64,
    pub image: String,
    pub tasks: Vec<String>,
    #[serde(default)]
    pub probe: References,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct References {
    #[serde(default)]
    pub references: Vec<Reference>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub harness: String,
    pub harness_version: String,
    pub model: String,
    pub effort: Option<String>,
    pub input_tokens: u64,
    pub tools: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Code,
    Visual,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub kind: Kind,
    pub language: String,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub artifacts: Vec<String>,
    pub difficulty: Option<String>,
    pub summary: Option<String>,
}
#[derive(Debug)]
pub struct Suite {
    pub root: PathBuf,
    pub edition: Edition,
    pub tasks: Vec<Task>,
    pub digest: String,
    pub commit: Option<String>,
    _snapshot: tempfile::TempDir,
}
pub fn identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !matches!(id, "." | "..")
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
pub fn relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.contains(':')
        && Path::new(path)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
}
/// Never follows links, including a caller-supplied root. Special files are refused too.
pub fn files(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    fn walk(root: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        if !std::fs::symlink_metadata(root)
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            return Err("suite directory must be a real directory".into());
        }
        for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "non UTF-8 suite path")?;
            if name == ".git" {
                continue;
            }
            let name = format!("{prefix}{name}");
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                walk(&entry.path(), &format!("{name}/"), out)?;
            } else if kind.is_file() {
                out.push((name, entry.path()));
            } else {
                return Err(format!("{name}: symlinks and special files are refused"));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, "", &mut out)?;
    out.sort();
    Ok(out)
}
pub fn digest(root: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for (name, path) in files(root)? {
        if name.starts_with("probes/") {
            continue;
        }
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            metadata.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = name.ends_with("/grade");
        hash.update([u8::from(executable)]);
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
/// Parse separately published references using the same identity rules as a local suite.
pub fn parse_references(text: &str) -> Result<References> {
    if text.len() > 1024 * 1024 {
        return Err("probe reference file exceeds 1 MiB".into());
    }
    let references: References =
        toml::from_str(text).map_err(|_| "invalid probe reference file")?;
    let mut keys = BTreeSet::new();
    for r in &references.references {
        if r.harness.is_empty()
            || r.harness_version.is_empty()
            || r.model.is_empty()
            || !keys.insert((&r.harness, &r.harness_version, &r.model, &r.effort))
        {
            return Err("invalid or duplicate probe reference identity".into());
        }
    }
    Ok(references)
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    toml::from_str(&std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))
}
pub fn load(root: &Path) -> Result<Suite> {
    let digest = digest(root)?;
    let mut edition: Edition = read(&root.join("edition.toml"))?;
    let references = root.join(format!("probes/edition-{}.toml", edition.number));
    edition.probe = if references.exists() {
        parse_references(&std::fs::read_to_string(&references).map_err(|e| e.to_string())?)?
    } else {
        References::default()
    };
    if edition.number == 0
        || !edition.goal_usd.is_finite()
        || edition.goal_usd <= 0.0
        || edition.tasks.is_empty()
    {
        return Err("edition.toml: number, goal_usd and tasks must be positive/nonempty".into());
    }
    let mut seen = BTreeSet::new();
    let mut tasks = Vec::new();
    for id in &edition.tasks {
        if !identifier(id) || !seen.insert(id) {
            return Err("edition.toml: invalid or duplicate task id".into());
        }
        let dir = root.join("tasks").join(id);
        let task: Task = read(&dir.join("task.toml"))?;
        if task.id != *id
            || task.timeout_seconds == 0
            || task.timeout_seconds > 86400
            || task.artifacts.iter().any(|a| !relative(a))
        {
            return Err(format!(
                "tasks/{id}/task.toml: invalid id, timeout_seconds or artifacts"
            ));
        }
        if !dir.join("prompt.md").is_file()
            || !dir.join("repo").is_dir()
            || !root.join("graders").join(id).join("grade").is_file()
        {
            return Err(format!("task {id}: missing prompt, repository or grader"));
        }
        tasks.push(task);
    }
    if !tasks.iter().any(|t| t.kind == Kind::Code) {
        return Err("edition.toml: at least one code task required".into());
    }
    let mut keys = BTreeSet::new();
    for r in &edition.probe.references {
        if !keys.insert((&r.harness, &r.harness_version, &r.model, &r.effort)) {
            return Err("edition.toml: duplicate probe reference".into());
        }
    }
    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    // Freeze local suites too: a source edit while an agent is running must not change its grader.
    let snapshot = tempfile::tempdir().map_err(|e| e.to_string())?;
    copy(root, snapshot.path())?;
    if crate::suite::digest(snapshot.path())? != digest {
        return Err("suite changed while taking its snapshot; retry".into());
    }
    Ok(Suite {
        root: snapshot.path().to_path_buf(),
        edition,
        tasks,
        digest,
        commit,
        _snapshot: snapshot,
    })
}
pub fn copy(from: &Path, to: &Path) -> Result<()> {
    copy_inner(from, to, false)
}
/// Grading ignores submission symlinks, without inspecting their targets.
pub fn copy_submission(from: &Path, to: &Path) -> Result<()> {
    copy_inner(from, to, true)
}
fn copy_inner(from: &Path, to: &Path, skip_links: bool) -> Result<()> {
    if !std::fs::symlink_metadata(from)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("suite directory must be a real directory".into());
    }
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            copy_inner(&entry.path(), &to.join(name), skip_links)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), to.join(name)).map_err(|e| e.to_string())?;
        } else if !(skip_links && kind.is_symlink()) {
            return Err("symlinks and special files are refused".into());
        }
    }
    Ok(())
}
/// A cached edition is immutable after its first successful fetch.
pub fn verify(root: &Path, expected: &str) -> Result<()> {
    if digest(root)? != expected {
        return Err("cached suite digest changed; remove the cache and fetch again".into());
    }
    Ok(())
}
pub fn artifact(root: &Path, name: &str) -> Result<(String, u64)> {
    if !relative(name) {
        return Err("invalid artifact path".into());
    }
    let mut path = root.to_path_buf();
    for part in Path::new(name).components() {
        path.push(part);
        if std::fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .is_symlink()
        {
            return Err("artifact symlink refused".into());
        }
    }
    if !path.is_file() {
        return Err("artifact is not a regular file".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok((sha(&bytes), bytes.len() as u64))
}
