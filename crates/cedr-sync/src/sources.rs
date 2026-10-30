//! Rules and bounded skill trees used by both compilation and import.
use crate::{Result, config, io};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub description: Option<String>,
    pub paths: Option<Vec<String>>,
    pub agents: Option<Vec<String>>,
}
pub struct Rule {
    pub id: String,
    pub meta: Meta,
    pub body: String,
}
pub fn split(text: &str) -> Result<(Meta, String)> {
    let text = text.replace("\r\n", "\n");
    if let Some(rest) = text.strip_prefix("---\n") {
        let (front, body) = rest
            .split_once("\n---\n")
            .ok_or("rules: invalid front matter")?;
        let meta =
            serde_yaml::from_str(front).map_err(|_| "rules: invalid front matter key or type")?;
        Ok((meta, body.to_string()))
    } else {
        Ok((Meta::default(), text))
    }
}
pub fn rules(root: &Path, targets: &[String]) -> Result<Vec<Rule>> {
    let mut out = vec![];
    for path in io::files(root, ".cedr/rules")? {
        if !path.ends_with(".md") {
            continue;
        }
        let text = String::from_utf8(io::read(root, &path)?.unwrap_or_default())
            .map_err(|_| format!("{path}: invalid UTF-8"))?;
        let (meta, body) = split(&text).map_err(|_| format!("{path}: invalid front matter key"))?;
        if meta
            .agents
            .as_ref()
            .is_some_and(|a| a.iter().any(|t| !targets.contains(t)))
        {
            return Err(format!("{path}: invalid agents"));
        }
        let id = Path::new(&path)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        out.push(Rule { id, meta, body });
    }
    Ok(out)
}
pub type Skills = BTreeMap<String, BTreeMap<String, Vec<u8>>>;
pub fn skills(root: &Path, rel: &str) -> Result<Skills> {
    let mut out: Skills = BTreeMap::new();
    let mut sizes: BTreeMap<String, usize> = BTreeMap::new();
    let dir = io::path(root, rel)?;
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir).map_err(|_| format!("{rel}: cannot list skills"))? {
            let entry = entry.map_err(|_| format!("{rel}: cannot list skills"))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "skills: non UTF-8 name")?;
            let path = io::path(root, &format!("{rel}/{name}"))?;
            if !path.is_dir()
                || !config::name(&name)
                || io::read(root, &format!("{rel}/{name}/SKILL.md"))?.is_none()
            {
                return Err(format!(
                    "{rel}/{name}: missing SKILL.md or invalid directory"
                ));
            }
        }
    }

    for path in io::files(root, rel)? {
        let tail = path
            .strip_prefix(&format!("{rel}/"))
            .ok_or("skills: invalid path")?;
        let (name, file) = tail.split_once('/').ok_or("skills: expected a directory")?;
        if !config::name(name) {
            return Err("skills: invalid name".into());
        }
        if std::fs::metadata(io::path(root, &path)?)
            .map_err(|_| "skills: cannot inspect file")?
            .len()
            > 1024 * 1024
        {
            return Err(format!("skills/{name}: exceeds 1 MiB"));
        }
        let bytes = io::read(root, &path)?.unwrap_or_default();
        let tree = out.entry(name.to_string()).or_default();
        let size = sizes.entry(name.into()).or_default();
        *size += bytes.len();
        if *size > 1024 * 1024 {
            return Err(format!("skills/{name}: exceeds 1 MiB"));
        }
        tree.insert(file.to_string(), bytes);
    }
    for (name, tree) in &out {
        let skill = tree
            .get("SKILL.md")
            .ok_or_else(|| format!("skills/{name}: missing SKILL.md"))?;
        let text =
            std::str::from_utf8(skill).map_err(|_| format!("skills/{name}: invalid UTF-8"))?;
        let front = text
            .strip_prefix("---\n")
            .and_then(|s| s.split_once("\n---"))
            .map(|p| p.0)
            .ok_or("skills: missing front matter")?;
        let value: serde_yaml::Value =
            serde_yaml::from_str(front).map_err(|_| "skills: invalid front matter")?;
        if value["name"].as_str() != Some(name)
            || value["description"].as_str().is_none_or(str::is_empty)
        {
            return Err(format!("skills/{name}: invalid name or description"));
        }
    }
    Ok(out)
}
