//! A server's tool surface: the tools a model sees, the hosts its code calls
//! and what it can do on the machine, as one hashed JSON document.

mod extract;

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::io_err;
use crate::npm::{self, Registry};
use crate::skill::hex;
use crate::source::Source;
use crate::{Error, Result, canonical};

pub(crate) use extract::Extraction;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Static,
    Introspect,
    None,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Static => "static",
            Method::Introspect => "introspect",
            Method::None => "none",
        }
    }
}

/// Declared in the alphabetical order of their names so sorting the enum
/// sorts the JSON array.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    ChildProcess,
    Env,
    FsWrite,
    Network,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::ChildProcess => "child_process",
            Capability::Env => "env",
            Capability::FsWrite => "fs_write",
            Capability::Network => "network",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: Option<String>,
    pub schema: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Surface {
    pub version: u32,
    pub source: String,
    pub method: Method,
    pub integrity: Option<String>,
    pub tools: Vec<Tool>,
    pub egress: Vec<String>,
    pub capabilities: Vec<Capability>,
    pub hash: String,
}

impl Surface {
    /// Builds a surface with sorted, de-duplicated lists and its hash.
    pub fn new(
        source: String,
        method: Method,
        integrity: Option<String>,
        mut tools: Vec<Tool>,
        mut egress: Vec<String>,
        mut capabilities: Vec<Capability>,
    ) -> Surface {
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools.dedup_by(|a, b| a.name == b.name);
        egress.sort();
        egress.dedup();
        capabilities.sort();
        capabilities.dedup();
        let hash = surface_hash(&tools, &egress, &capabilities);
        Surface {
            version: 1,
            source,
            method,
            integrity,
            tools,
            egress,
            capabilities,
            hash,
        }
    }

    /// Re-sorts and re-hashes a surface read from disk, so a hand-edited or
    /// stale `hash` field is never trusted.
    pub fn normalized(self) -> Surface {
        Surface::new(
            self.source,
            self.method,
            self.integrity,
            self.tools,
            self.egress,
            self.capabilities,
        )
    }

    pub(crate) fn from_extraction(
        source: String,
        integrity: Option<String>,
        extraction: Extraction,
    ) -> Surface {
        Surface::new(
            source,
            Method::Static,
            integrity,
            extraction.tools,
            extraction.egress.into_iter().collect(),
            extraction.capabilities.into_iter().collect(),
        )
    }

    pub fn read(path: &Path) -> Result<Surface> {
        let text = fs::read_to_string(path).map_err(io_err(path))?;
        let surface: Surface = serde_json::from_str(&text)
            .map_err(|e| crate::error::parse_err(path, format!("not a surface document: {e}")))?;
        if surface.version != 1 {
            return Err(crate::error::parse_err(
                path,
                format!("unsupported surface version {}", surface.version),
            ));
        }
        Ok(surface.normalized())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("surface serializes")
    }

    pub fn to_json_pretty(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("surface serializes");
        text.push('\n');
        text
    }
}

/// `sha256-` plus the hex digest of the canonical JSON of `{tools, egress, capabilities}`.
pub fn surface_hash(tools: &[Tool], egress: &[String], capabilities: &[Capability]) -> String {
    let value = json!({
        "tools": tools,
        "egress": egress,
        "capabilities": capabilities,
    });
    let digest = Sha256::digest(canonical::to_string(&value).as_bytes());
    format!("sha256-{}", hex(&digest))
}

/// Statically extracts the surface of `npm:<pkg>@<exact version>` (fetched
/// from the registry) or `file:<path-to.tgz>`.
pub fn from_source(source: &Source) -> Result<Surface> {
    match source {
        Source::File(path) => {
            let bytes = fs::read(path).map_err(io_err(Path::new(path)))?;
            let integrity = npm::sri_sha512(&bytes);
            extract_tarball(source.to_string(), &bytes, integrity)
        }
        Source::Npm { .. } => {
            let (name, version) = source.npm_exact()?;
            let registry = Registry::new()?;
            let doc = registry.version_doc(name, version)?;
            let (bytes, integrity) = registry.tarball(&doc)?;
            extract_tarball(source.to_string(), &bytes, integrity)
        }
        other => Err(Error::Source(format!(
            "surfaces are extracted from npm: and file: sources only, not {other}"
        ))),
    }
}

/// The registry's `latest` version of an npm source, as `npm:<pkg>@<version>`.
pub fn latest(source: &Source) -> Result<Source> {
    let Source::Npm { name, .. } = source else {
        return Err(Error::Source(format!(
            "{source} is not an npm source; use npm:<pkg>"
        )));
    };
    let doc = Registry::new()?.version_doc(name, "latest")?;
    Ok(Source::Npm {
        name: name.clone(),
        version: Some(doc.version),
    })
}

pub(crate) fn extract_tarball(source: String, tgz: &[u8], integrity: String) -> Result<Surface> {
    let files = npm::source_files(tgz, extract::is_scanned)?;
    Ok(Surface::from_extraction(
        source,
        Some(integrity),
        extract::extract(&files),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_over_sorted_canonical_fields() {
        let tool = |n: &str| Tool {
            name: n.into(),
            description: None,
            schema: None,
        };
        let a = Surface::new(
            "x".into(),
            Method::Static,
            None,
            vec![tool("b"), tool("a")],
            vec!["z.dev".into(), "a.dev".into()],
            vec![Capability::Network, Capability::ChildProcess],
        );
        let b = Surface::new(
            "y".into(),
            Method::Introspect,
            Some("sha512-x".into()),
            vec![tool("a"), tool("b")],
            vec!["a.dev".into(), "z.dev".into()],
            vec![Capability::ChildProcess, Capability::Network],
        );
        assert_eq!(a.hash, b.hash);
        let expected = r#"{"capabilities":["child_process","network"],"egress":["a.dev","z.dev"],"tools":[{"description":null,"name":"a","schema":null},{"description":null,"name":"b","schema":null}]}"#;
        assert_eq!(
            a.hash,
            format!("sha256-{}", hex(&Sha256::digest(expected.as_bytes())))
        );
    }
}
