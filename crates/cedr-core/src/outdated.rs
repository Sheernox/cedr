//! Read-only update checks over locked npm sources, reusing registry resolution,
//! static surface extraction and the same change classification as `cedr diff`.
use std::path::Path;

use serde::Serialize;

use crate::diff::{self, Change, ChangeSeverity};
use crate::discovery::Client;
use crate::lock::{self, Lock, McpEntry};
use crate::npm::Registry;
use crate::source::{Source, public_source};
use crate::surface::{self, Method, Surface};
use crate::{Error, Result};

#[derive(Debug, Serialize)]
pub struct Update {
    pub client: Client,
    pub name: String,
    pub source: String,
    pub latest: String,
    pub severity: UpdateSeverity,
    pub changes: Vec<Change>,
}

/// `None` means that no surface change was observed, including version-only checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateSeverity {
    High,
    Medium,
    Low,
    None,
}

impl UpdateSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::None => "none",
        }
    }

    /// A missing threshold is the default `never` policy.
    pub fn reaches(self, threshold: Option<ChangeSeverity>) -> bool {
        let severity = match self {
            Self::High => Some(ChangeSeverity::High),
            Self::Medium => Some(ChangeSeverity::Medium),
            Self::Low => Some(ChangeSeverity::Low),
            Self::None => None,
        };
        matches!((severity, threshold), (Some(s), Some(t)) if s <= t)
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub version: u32,
    pub updates: Vec<Update>,
    #[serde(skip)]
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub not_checked: Vec<String>,
}

impl Report {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("updates serialize")
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for u in &self.updates {
            out.push_str(&format!(
                "{}.{}: {} -> {} ({}; {} changes)\n",
                u.client.as_str(),
                u.name.escape_debug(),
                u.source.escape_debug(),
                u.latest.escape_debug(),
                u.severity.as_str(),
                u.changes.len()
            ));
            for c in &u.changes {
                out.push_str(&format!(
                    "  {} {}{}: {}\n",
                    c.severity.as_str(),
                    c.kind.as_str(),
                    c.tool
                        .as_ref()
                        .map(|t| format!(" {}", t.escape_debug()))
                        .unwrap_or_default(),
                    c.detail.escape_debug()
                ));
            }
        }
        for label in &self.not_checked {
            out.push_str(&format!("{}: not checked\n", label.escape_debug()));
        }
        if out.is_empty() {
            out.push_str("no newer npm versions\n");
        }
        out
    }

    pub fn to_github(&self) -> String {
        let mut out = String::new();
        for u in &self.updates {
            let level = if u.severity == UpdateSeverity::High {
                "error"
            } else {
                "warning"
            };
            let message = format!(
                "{}.{}: {} -> {} ({}; {} changes)",
                u.client.as_str(),
                u.name,
                u.source,
                u.latest,
                u.severity.as_str(),
                u.changes.len()
            );
            let escaped = message
                .replace('%', "%25")
                .replace('\r', "%0D")
                .replace('\n', "%0A");
            // Pointed at the lock, where the pin lives, so the annotation lands on a file instead of the workflow.
            out.push_str(&format!(
                "::{level} file={},title=cedr outdated::{escaped}\n",
                lock::LOCK_FILE
            ));
        }
        out
    }
}

/// Checks each locked npm entry; remote failures are reported per server.
pub fn outdated(root: &Path) -> Result<Report> {
    let (lock, _) = lock::read_lock(root)?;
    let registry = Registry::new();
    check(root, &lock, registry.as_ref().map_err(|e| e.to_string()))
}

fn check(
    root: &Path,
    lock: &Lock,
    registry: std::result::Result<&Registry, String>,
) -> Result<Report> {
    let mut report = Report {
        version: 1,
        updates: Vec::new(),
        warnings: Vec::new(),
        not_checked: Vec::new(),
    };
    for entry in &lock.mcp {
        let label = format!("{}.{}", entry.client.as_str(), entry.name);
        if !entry.source.starts_with("npm:") {
            report.not_checked.push(label);
            continue;
        }
        // A missing or unreadable baseline is a local I/O failure, not a registry warning.
        let old = if entry.method != Method::None {
            let path = root
                .join(lock::SURFACE_DIR)
                .join(lock::surface_file_name(entry.client, &entry.name));
            let canonical = std::fs::canonicalize(root).map_err(crate::error::io_err(root))?;
            crate::discovery::ensure_inside(&canonical, &path)?;
            Some(Surface::read(&path)?)
        } else {
            None
        };
        let result = registry
            .as_ref()
            .map_err(|e| Error::Registry(e.clone()))
            .and_then(|r| update(entry, old.as_ref(), r));
        match result {
            Ok(Some(u)) => report.updates.push(u),
            Ok(None) => {}
            Err(e) => report.warnings.push(format!("{label}: {e}")),
        }
    }
    Ok(report)
}

fn update(entry: &McpEntry, old: Option<&Surface>, registry: &Registry) -> Result<Option<Update>> {
    let source: Source = entry.source.parse()?;
    let latest = surface::latest_with(&source, registry)?;
    let (name, version) = source.npm_exact()?;
    let (_, newer) = latest.npm_exact()?;
    let version = semver::Version::parse(version).map_err(|e| Error::Source(e.to_string()))?;
    let newer = semver::Version::parse(newer).map_err(|e| Error::Source(e.to_string()))?;
    if newer.cmp_precedence(&version) != std::cmp::Ordering::Greater {
        return Ok(None);
    }
    let changes = if let Some(old) = old {
        let doc = registry.version_doc(name, &newer.to_string())?;
        let (bytes, integrity) = registry.tarball(&doc)?;
        diff::diff(
            old,
            &surface::extract_tarball(latest.to_string(), &bytes, integrity)?,
        )
        .changes
    } else {
        Vec::new()
    };
    let severity = match changes.iter().map(|c| c.severity).min() {
        Some(ChangeSeverity::High) => UpdateSeverity::High,
        Some(ChangeSeverity::Medium) => UpdateSeverity::Medium,
        Some(ChangeSeverity::Low) => UpdateSeverity::Low,
        None => UpdateSeverity::None,
    };
    Ok(Some(Update {
        client: entry.client,
        name: entry.name.clone(),
        source: public_source(&entry.source),
        latest: latest.to_string(),
        severity,
        changes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::Transport;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread;
    use std::time::{Duration, Instant};

    struct Mock {
        base: String,
        stop: Arc<AtomicBool>,
        join: Option<thread::JoinHandle<()>>,
    }

    impl Mock {
        fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let url = base.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let done = stop.clone();
            let bytes = tarball();
            let join = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(15);
                while !done.load(Ordering::Relaxed) && Instant::now() < deadline {
                    let (mut stream, _) = match listener.accept() {
                        Ok(s) => s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::park_timeout(Duration::from_millis(2));
                            continue;
                        }
                        Err(e) => panic!("mock accept: {e}"),
                    };
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut request = [0; 4096];
                    let n = stream.read(&mut request).unwrap();
                    let request = String::from_utf8_lossy(&request[..n]);
                    let (status, body) = if request.contains("/broken/") {
                        ("503 Unavailable", b"unavailable".to_vec())
                    } else if request.starts_with("GET /package.tgz ") {
                        ("200 OK", bytes.clone())
                    } else {
                        ("200 OK", serde_json::json!({"version":"1.1.0", "dist":{"tarball":format!("{url}/package.tgz"),"integrity":crate::npm::sri_sha512(&bytes)}}).to_string().into_bytes())
                    };
                    write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    stream.write_all(&body).unwrap();
                }
            });
            Self {
                base,
                stop,
                join: Some(join),
            }
        }
    }
    impl Drop for Mock {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.join.take().unwrap().join().unwrap();
        }
    }

    fn tarball() -> Vec<u8> {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/npm/fake-weather/1.1.0");
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);
        tar.append_dir_all("package", fixture).unwrap();
        tar.into_inner().unwrap().finish().unwrap()
    }

    fn entry(name: &str, method: Method) -> McpEntry {
        McpEntry {
            client: Client::Copilot,
            name: name.into(),
            transport: Transport::Stdio,
            source: format!("npm:{name}@1.0.0"),
            pinned: false,
            launch: String::new(),
            integrity: None,
            surface: None,
            method,
            tools: vec![],
            egress: vec![],
        }
    }

    #[test]
    fn mock_registry_reports_changes_matching_surfaces_and_partial_failure() {
        let mock = Mock::new();
        let registry = Registry::mock(mock.base.clone());
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(lock::SURFACE_DIR)).unwrap();
        let bytes = tarball();
        let same = surface::extract_tarball(
            "npm:same@1.0.0".into(),
            &bytes,
            crate::npm::sri_sha512(&bytes),
        )
        .unwrap();
        assert!(!same.tools.is_empty());
        let empty = Surface::new(
            "npm:changed@1.0.0".into(),
            Method::Static,
            None,
            vec![],
            vec![],
            vec![],
        );
        for (name, s) in [("same", &same), ("changed", &empty)] {
            fs::write(
                root.path()
                    .join(lock::SURFACE_DIR)
                    .join(lock::surface_file_name(Client::Copilot, name)),
                s.to_json(),
            )
            .unwrap();
        }
        let mut entries = vec![
            entry("changed", Method::Static),
            entry("same", Method::Static),
            entry("version-only", Method::None),
            entry("broken", Method::None),
        ];
        let mut current = entry("current", Method::None);
        current.source = "npm:current@1.1.0".into();
        entries.push(current);
        let mut local = entry("local", Method::None);
        local.source = "command:example-mcp".into();
        entries.push(local);
        let lock = Lock {
            version: 1,
            skill: vec![],
            mcp: entries,
        };
        let report = check(root.path(), &lock, Ok(&registry)).unwrap();
        assert_eq!(report.updates.len(), 3);
        assert!(!report.updates[0].changes.is_empty());
        assert_eq!(report.updates[1].severity, UpdateSeverity::None);
        assert_eq!(report.updates[2].severity, UpdateSeverity::None);
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("copilot.broken"));
        assert!(report.warnings[0].contains("503"));
        assert!(report.to_text().contains("copilot.local: not checked"));
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 2);
        assert_eq!(json["version"], 1);
        let u = &json["updates"][0];
        assert_eq!(u.as_object().unwrap().len(), 6);
        assert_eq!(u["source"], "npm:changed@1.0.0");
        assert_eq!(u["latest"], "npm:changed@1.1.0");
        assert!(u["changes"].is_array());
    }

    #[test]
    fn thresholds_and_annotations_escape_untrusted_names() {
        for (severity, expected) in [
            (UpdateSeverity::High, [true, true, true, false]),
            (UpdateSeverity::Medium, [false, true, true, false]),
            (UpdateSeverity::Low, [false, false, true, false]),
            (UpdateSeverity::None, [false, false, false, false]),
        ] {
            for (threshold, expected) in [
                Some(ChangeSeverity::High),
                Some(ChangeSeverity::Medium),
                Some(ChangeSeverity::Low),
                None,
            ]
            .into_iter()
            .zip(expected)
            {
                assert_eq!(severity.reaches(threshold), expected);
            }
        }
        let mut report = Report {
            version: 1,
            updates: vec![Update {
                client: Client::Copilot,
                name: "x%\n::error\r".into(),
                source: "npm:x@1.0.0".into(),
                latest: "npm:x@1.1.0".into(),
                severity: UpdateSeverity::High,
                changes: vec![],
            }],
            warnings: vec![],
            not_checked: vec![],
        };
        let github = report.to_github();
        assert_eq!(github.lines().count(), 1);
        assert!(github.starts_with("::error file=.cedr/guard.lock,title=cedr outdated::"));
        assert!(github.contains("x%25%0A::error%0D"));
        report.updates[0].severity = UpdateSeverity::None;
        assert!(report.to_github().starts_with("::warning "));
    }
}
