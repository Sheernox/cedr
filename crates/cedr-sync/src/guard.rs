//! Advisory Guard verification for native MCP settings managed or checked by Sync.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use cedr_core::{
    discovery::{Client, MCP_CONFIGS, discover_servers},
    finding::{Finding, Rule, Severity},
    lock::{LEGACY_LOCK_FILE, LOCK_FILE, Lock},
    verify::mcp_findings,
};
use serde_json::Value;

use crate::{Result, io, merge};

pub(crate) fn lock_file(root: &Path) -> Option<&'static str> {
    [LOCK_FILE, LEGACY_LOCK_FILE]
        .into_iter()
        .find(|path| root.join(path).exists())
}

/// Compare entry values so formatting or another section of a shared file does not trigger a warning.
pub(crate) fn written_servers(
    path: &str,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
    changed: &mut BTreeMap<Client, BTreeSet<String>>,
) -> Result<()> {
    let Some((client, _, key)) = MCP_CONFIGS.into_iter().find(|(_, file, _)| *file == path) else {
        return Ok(());
    };
    let read = |bytes: Option<&[u8]>| {
        bytes
            .map(|bytes| merge::value(path, bytes))
            .transpose()
            .map(|v| v.and_then(|mut v| v.get_mut(key).map(Value::take)))
    };
    let old = read(before)?;
    let new = read(after)?;
    let old = old.as_ref().and_then(Value::as_object);
    let new = new.as_ref().and_then(Value::as_object);
    for name in old.into_iter().chain(new).flat_map(|v| v.keys()) {
        if old.and_then(|v| v.get(name)) != new.and_then(|v| v.get(name)) {
            changed.entry(client).or_default().insert(name.clone());
        }
    }
    Ok(())
}

/// An unreadable pin or config is advisory because compilation and its gate are independent.
pub(crate) fn skipped(path: &str, reason: &str) -> Finding {
    Finding::new(
        Rule::Cs005,
        Severity::Warning,
        path,
        None,
        format!("could not check Guard MCP pins: {}", reason.escape_debug()),
    )
}

pub(crate) fn warnings(
    root: &Path,
    lock_file: &str,
    selected: impl Fn(Client, &str) -> bool,
) -> Vec<Finding> {
    let load = || -> Result<_> {
        let lock = Lock::read(&io::path(root, lock_file)?).map_err(|e| e.to_string())?;
        for (_, path, _) in MCP_CONFIGS {
            io::path(root, path)?;
        }
        let servers = discover_servers(root).map_err(|e| e.to_string())?;
        Ok(mcp_findings(&lock, lock_file, &servers, selected))
    };
    match load() {
        Ok(mut findings) => {
            for finding in &mut findings {
                finding.severity = Severity::Warning;
                finding.message = format!(
                    "{}. Run cedr guard lock for this repo",
                    finding.message.replace(|c: char| c.is_control(), " "),
                );
            }
            findings
        }
        Err(error) => vec![skipped(lock_file, &error)],
    }
}
