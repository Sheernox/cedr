//! Local pick history, verdicts and stats (docs/route.md, History and verdicts).
//!
//! One JSON object per line, appended with a single write so concurrent pickers never interleave
//! records. History holds routing metadata only: no briefs, prompts, code or credentials.

use std::collections::{BTreeMap, HashMap};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::config::xdg_dir;
use crate::error::{Error, Result, io_err};
use crate::pick::{Pick, Size};

pub const MAX_NOTE: usize = 120;
/// Fewer verdicts than this for a route and model is too few to move a route.
pub const PROVISIONAL_BELOW: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Accepted,
    Reworked,
    Redone,
    Rejected,
}

impl Verdict {
    pub fn parse(s: &str) -> Result<Verdict> {
        match s {
            "accepted" => Ok(Verdict::Accepted),
            "reworked" => Ok(Verdict::Reworked),
            "redone" => Ok(Verdict::Redone),
            "rejected" => Ok(Verdict::Rejected),
            _ => Err(Error::Usage(format!(
                "verdict {s:?} is not accepted, reworked, redone or rejected"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Record {
    Pick {
        id: String,
        at: String,
        route: String,
        rules: String,
        target: String,
        effort: String,
        reason: String,
        state: String,
        size: String,
    },
    Verdict {
        id: String,
        at: String,
        verdict: Verdict,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
}

/// The history file under `$XDG_STATE_HOME/cedr/`.
pub fn default_path() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state")
        .join("cedr")
        .join("route-history.jsonl")
}

fn stamp(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn append(path: &Path, records: &[Record]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    let mut body = String::new();
    for r in records {
        body.push_str(&serde_json::to_string(r).expect("records serialize"));
        body.push('\n');
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io_err(path))?;
    file.write_all(body.as_bytes()).map_err(io_err(path))
}

fn read(path: &Path) -> Result<Vec<Record>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(path)(e)),
    };
    // A torn or foreign line is skipped rather than failing every later stats call.
    Ok(text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

/// Records every delegate of a pick.
pub fn record_pick(path: &Path, pick: &Pick, size: Size, now: DateTime<Utc>) -> Result<()> {
    let records: Vec<Record> = pick
        .delegates
        .iter()
        .map(|d| Record::Pick {
            id: d.id.clone(),
            at: stamp(now),
            route: pick.route.clone(),
            rules: pick.rules.clone(),
            target: d.target.clone(),
            effort: d.effort.clone(),
            reason: d.reason.clone(),
            state: d.state.as_str().to_string(),
            size: size.as_str().to_string(),
        })
        .collect();
    if records.is_empty() {
        return Ok(());
    }
    append(path, &records)
}

/// Records a verdict on a recorded pick; a later verdict for the same pick replaces the earlier one.
pub fn record_verdict(
    path: &Path,
    id: &str,
    verdict: Verdict,
    task: Option<&str>,
    note: Option<&str>,
    now: DateTime<Utc>,
) -> Result<()> {
    if let Some(n) = note
        && (n.chars().count() > MAX_NOTE || n.contains(['\n', '\r']))
    {
        return Err(Error::Usage(format!(
            "a note is one line of at most {MAX_NOTE} characters"
        )));
    }
    let known = read(path)?
        .iter()
        .any(|r| matches!(r, Record::Pick { id: p, .. } if p == id));
    if !known {
        return Err(Error::Usage(format!(
            "no recorded pick {id:?} in {}",
            path.display()
        )));
    }
    append(
        path,
        &[Record::Verdict {
            id: id.to_string(),
            at: stamp(now),
            verdict,
            task: task.map(str::to_string),
            note: note.map(str::to_string),
        }],
    )
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StatRow {
    pub route: String,
    pub target: String,
    pub picks: usize,
    pub accepted: usize,
    pub reworked: usize,
    pub redone: usize,
    pub rejected: usize,
    pub verdicts: usize,
    /// Accepted over verdicts recorded, in percent; none without verdicts.
    pub accepted_pct: Option<f64>,
    pub provisional: bool,
}

/// Picks and latest verdicts grouped by route and target, optionally for one route.
pub fn stats(path: &Path, route: Option<&str>) -> Result<Vec<StatRow>> {
    let records = read(path)?;
    let mut latest: HashMap<&str, Verdict> = HashMap::new();
    for r in &records {
        if let Record::Verdict { id, verdict, .. } = r {
            latest.insert(id.as_str(), *verdict);
        }
    }
    let mut rows: BTreeMap<(String, String), StatRow> = BTreeMap::new();
    for r in &records {
        let Record::Pick {
            id,
            route: rid,
            target,
            ..
        } = r
        else {
            continue;
        };
        if route.is_some_and(|want| want != rid) {
            continue;
        }
        let row = rows
            .entry((rid.clone(), target.clone()))
            .or_insert_with(|| StatRow {
                route: rid.clone(),
                target: target.clone(),
                ..StatRow::default()
            });
        row.picks += 1;
        match latest.get(id.as_str()) {
            Some(Verdict::Accepted) => row.accepted += 1,
            Some(Verdict::Reworked) => row.reworked += 1,
            Some(Verdict::Redone) => row.redone += 1,
            Some(Verdict::Rejected) => row.rejected += 1,
            None => continue,
        }
        row.verdicts += 1;
    }
    Ok(rows
        .into_values()
        .map(|mut r| {
            r.accepted_pct = (r.verdicts > 0)
                .then(|| (r.accepted as f64 * 1000.0 / r.verdicts as f64).round() / 10.0);
            r.provisional = r.verdicts < PROVISIONAL_BELOW;
            r
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pick::{Delegate, T3Target};
    use crate::pool::State;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-29T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn pick(ids: &[&str], target: &str) -> Pick {
        Pick {
            route: "review.code".into(),
            work: String::new(),
            role: "review".into(),
            rules: "abc1234".into(),
            orchestrator: None,
            skipped: Vec::new(),
            reason: None,
            warnings: Vec::new(),
            council: false,
            delegates: ids
                .iter()
                .map(|id| Delegate {
                    id: id.to_string(),
                    target: target.into(),
                    harness: "codex".into(),
                    model: "gpt-6.1-sol".into(),
                    effort: "high".into(),
                    state: State::Ready,
                    reason: "primary".into(),
                    summary: String::new(),
                    tag: String::new(),
                    prefix: String::new(),
                    t3: T3Target {
                        provider_instance_id: "codex".into(),
                        model: "gpt-6.1-sol".into(),
                        options: Default::default(),
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn the_latest_verdict_counts_and_unjudged_picks_stay_out_of_the_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        record_pick(
            &path,
            &pick(&["p-1", "p-2", "p-3"], "codex/gpt-6.1-sol"),
            Size::Medium,
            now(),
        )
        .unwrap();
        record_verdict(&path, "p-1", Verdict::Rejected, None, None, now()).unwrap();
        record_verdict(
            &path,
            "p-1",
            Verdict::Accepted,
            Some("task-1"),
            Some("fine"),
            now(),
        )
        .unwrap();
        record_verdict(&path, "p-2", Verdict::Reworked, None, None, now()).unwrap();
        let rows = stats(&path, None).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(
            (r.picks, r.accepted, r.reworked, r.rejected, r.verdicts),
            (3, 1, 1, 0, 2)
        );
        assert_eq!(r.accepted_pct, Some(50.0));
        assert!(r.provisional);
    }

    #[test]
    fn verdicts_need_a_recorded_pick_and_a_one_line_note() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        record_pick(&path, &pick(&["p-1"], "codex/x"), Size::Small, now()).unwrap();
        assert!(record_verdict(&path, "p-9", Verdict::Accepted, None, None, now()).is_err());
        assert!(
            record_verdict(&path, "p-1", Verdict::Accepted, None, Some("a\nb"), now()).is_err()
        );
        let long = "x".repeat(MAX_NOTE + 1);
        assert!(record_verdict(&path, "p-1", Verdict::Accepted, None, Some(&long), now()).is_err());
    }

    #[test]
    fn stats_skip_torn_lines_and_filter_by_route() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        record_pick(&path, &pick(&["p-1"], "codex/x"), Size::Small, now()).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"pi")
            .unwrap();
        assert_eq!(stats(&path, Some("review.code")).unwrap().len(), 1);
        assert!(stats(&path, Some("other")).unwrap().is_empty());
    }
}
