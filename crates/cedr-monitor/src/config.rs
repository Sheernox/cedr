//! Strict monitor configuration; no credentials are kept here.
use crate::{HARNESSES, Result};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub org: Option<String>,
    pub projects: String,
    pub exclude: Vec<String>,
    pub harnesses: Vec<String>,
    pub auto: bool,
    pub url: Option<String>,
    pub paths: BTreeMap<String, String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            org: None,
            projects: "name".into(),
            exclude: vec![],
            harnesses: vec![],
            auto: false,
            url: None,
            paths: BTreeMap::new(),
        }
    }
}
pub fn path() -> PathBuf {
    cedr_route::config::xdg_dir("XDG_CONFIG_HOME", ".config").join("cedr/monitor.toml")
}
pub fn expand(s: &str) -> PathBuf {
    if let Some(tail) = s.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(tail)
    } else {
        s.into()
    }
}
impl Config {
    pub fn load() -> Result<Self> {
        let path = path();
        match std::fs::read_to_string(&path) {
            Ok(s) => Self::parse(&s).map_err(|e| format!("{}: {e}", path.display()).into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        let c: Self = toml::from_str(s)
            .map_err(|e: toml::de::Error| format!("invalid monitor config: {}", e.message()))?;
        if c.version != 1 {
            return Err("unsupported version (expected 1)".into());
        }
        if !["name", "hash", "none"].contains(&c.projects.as_str()) {
            return Err("projects must be name, hash or none".into());
        }
        for h in c.harnesses.iter().chain(c.paths.keys()) {
            if !HARNESSES.contains(&h.as_str()) {
                return Err(format!("unknown harness {h}").into());
            }
        }
        if c.org
            .as_ref()
            .is_some_and(|org| !cedr_route::config::is_slug(org))
        {
            return Err("org must be an organization slug".into());
        }
        for (h, p) in &c.paths {
            glob::Pattern::new(p).map_err(|_| format!("paths.{h}: invalid glob"))?;
        }
        for g in &c.exclude {
            glob::Pattern::new(&expand(g).to_string_lossy())?;
        }
        if let Some(url) = &c.url {
            cedr_route::sync::validate_url(url)?;
        }
        Ok(c)
    }
    pub fn project(&self, cwd: &str) -> Option<Option<String>> {
        if self
            .exclude
            .iter()
            .any(|p| glob::Pattern::new(&expand(p).to_string_lossy()).is_ok_and(|g| g.matches(cwd)))
        {
            return None;
        }
        if self.projects == "none" || cwd.is_empty() {
            return Some(None);
        }
        let path = Path::new(cwd);
        let repo = path
            .ancestors()
            .find(|p| p.join(".git").exists())
            .unwrap_or(path);
        let value = if self.projects == "hash" {
            format!("p-{}", &crate::digest(&repo.to_string_lossy())[..12])
        } else {
            repo.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .chars()
                .filter(|c| !c.is_control())
                .take(128)
                .collect()
        };
        Some(Some(value))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_keys() {
        for s in [
            "version=2",
            "wat=true",
            "projects='raw'",
            "[paths]\nnope='x'",
        ] {
            assert!(Config::parse(s).is_err(), "{s}");
        }
        assert!(Config::parse("").is_ok());
    }
}
