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

use crate::config::{Config, xdg_dir};
use crate::council::{Gate, Merged, Next, Severity};
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
        /// Written only when the target was a preview version.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        preview: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        judges: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        escalates: Option<String>,
    },
    Council {
        id: String,
        at: String,
        route: Option<String>,
        run: Option<String>,
        round: u32,
        rounds: u32,
        gate: Gate,
        next: Next,
        quorum: usize,
        block: Severity,
        reviewers: Vec<String>,
        groups: BTreeMap<Severity, usize>,
        findings: BTreeMap<String, usize>,
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
            preview: d.preview,
            run: pick.run.clone(),
            judges: pick.judges.clone(),
            escalates: pick.escalates.clone(),
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

/// Validates a cascade link against the current config and inherits the draft's run.
pub fn link_pick(
    path: &Path,
    config: &Config,
    pick: &mut Pick,
    judges: Option<&str>,
    escalates: Option<&str>,
) -> Result<()> {
    if judges.is_some() && escalates.is_some() {
        return Err(Error::Usage(
            "judges and escalates are mutually exclusive".into(),
        ));
    }
    let Some(id) = judges.or(escalates) else {
        return Ok(());
    };
    let records = read(path)?;
    let draft = records
        .iter()
        .find_map(|r| match r {
            Record::Pick {
                id: p, route, run, ..
            } if p == id => Some((route, run)),
            _ => None,
        })
        .ok_or_else(|| Error::Usage(format!("no recorded pick {id:?}")))?;
    let route = config.routes.get(draft.0);
    let expected = route.and_then(|r| {
        if judges.is_some() {
            r.judge.as_ref()
        } else {
            r.escalate.as_ref()
        }
    });
    if expected != Some(&pick.route) {
        return Err(Error::Usage(format!(
            "{} is not the draft route's {}",
            pick.route,
            if judges.is_some() {
                "judge"
            } else {
                "escalate"
            }
        )));
    }
    pick.run = draft.1.clone();
    pick.judges = judges.map(str::to_string);
    pick.escalates = escalates.map(str::to_string);
    Ok(())
}

/// Refuses a named run absent from this machine, including merges with recording disabled.
pub fn validate_run(path: &Path, run: Option<&str>) -> Result<()> {
    if let Some(run) = run {
        let known = read(path)?.iter().any(|r| matches!(r, Record::Pick {run: Some(r), ..} | Record::Council {run: Some(r), ..} if r == run));
        if !known {
            return Err(Error::Usage(format!("no recorded run {run:?}")));
        }
    }
    Ok(())
}

/// Records counts only; reports and claims never enter the history file.
pub fn record_council(
    path: &Path,
    merged: &mut Merged,
    route: Option<&str>,
    run: Option<&str>,
    now: DateTime<Utc>,
) -> Result<()> {
    validate_run(path, run)?;
    let id = crate::pick::new_id().replacen("p-", "c-", 1);
    let mut groups = BTreeMap::from([
        (Severity::P0, 0),
        (Severity::P1, 0),
        (Severity::P2, 0),
        (Severity::P3, 0),
    ]);
    let mut findings: BTreeMap<String, usize> =
        merged.reviewers.iter().map(|r| (r.clone(), 0)).collect();
    for g in &merged.groups {
        *groups.get_mut(&g.severity).expect("all severities") += 1;
        for f in &g.findings {
            *findings.entry(f.member.clone()).or_default() += 1;
        }
    }
    append(
        path,
        &[Record::Council {
            id: id.clone(),
            at: stamp(now),
            route: route.map(str::to_string),
            run: run.map(str::to_string),
            round: merged.round,
            rounds: merged.rounds,
            gate: merged.gate,
            next: merged.next,
            quorum: merged.quorum,
            block: merged.block,
            reviewers: merged.reviewers.clone(),
            groups,
            findings,
        }],
    )?;
    merged.id = Some(id);
    Ok(())
}

/// Parses the inclusive UTC date boundary used by both stats commands.
pub fn since_date(since: Option<&str>) -> Result<Option<String>> {
    since
        .map(|s| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .filter(|d| d.format("%Y-%m-%d").to_string() == s)
                .map(|_| format!("{s}T00:00:00Z"))
                .ok_or_else(|| Error::Usage("since must be YYYY-MM-DD".into()))
        })
        .transpose()
}

fn included(at: &str, since: Option<&str>) -> bool {
    since.is_none_or(|s| at >= s)
}

fn latest_verdicts(records: &[Record]) -> HashMap<&str, &Record> {
    let mut latest: HashMap<&str, &Record> = HashMap::new();
    for r in records {
        if let Record::Verdict { id, at, .. } = r {
            let replace = match latest.get(id.as_str()) {
                Some(Record::Verdict { at: old, .. }) => at >= old,
                _ => true,
            };
            if replace {
                latest.insert(id.as_str(), r);
            }
        }
    }
    latest
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StatRow {
    pub route: String,
    pub target: String,
    pub preview: bool,
    pub picks: usize,
    pub accepted: usize,
    pub reworked: usize,
    pub redone: usize,
    pub rejected: usize,
    pub verdicts: usize,
    pub accepted_pct: Option<f64>,
    pub median_minutes: Option<f64>,
    pub provisional: bool,
    pub judged: Option<usize>,
    pub escalated: Option<usize>,
}

/// Picks grouped by route, target and preview status, with latest verdict latency.
pub fn stats(
    path: &Path,
    route: Option<&str>,
    since: Option<&str>,
    config: &Config,
) -> Result<Vec<StatRow>> {
    let since = since_date(since)?;
    let records = read(path)?;
    let latest = latest_verdicts(&records);
    let judged: std::collections::HashSet<&str> = records
        .iter()
        .filter_map(|r| match r {
            Record::Pick {
                judges: Some(id), ..
            } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let escalated: std::collections::HashSet<&str> = records
        .iter()
        .filter_map(|r| match r {
            Record::Pick {
                escalates: Some(id),
                ..
            } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let mut rows: BTreeMap<_, (StatRow, Vec<f64>)> = BTreeMap::new();
    for r in &records {
        let Record::Pick {
            id,
            at,
            route: rid,
            target,
            preview,
            ..
        } = r
        else {
            continue;
        };
        if route.is_some_and(|r| r != rid) || !included(at, since.as_deref()) {
            continue;
        }
        let cascade = config.routes.get(rid).is_some_and(|r| r.judge.is_some());
        let (row, minutes) = rows
            .entry((rid.clone(), target.clone(), *preview))
            .or_insert_with(|| {
                (
                    StatRow {
                        route: rid.clone(),
                        target: target.clone(),
                        preview: *preview,
                        judged: cascade.then_some(0),
                        escalated: cascade.then_some(0),
                        ..Default::default()
                    },
                    Vec::new(),
                )
            });
        row.picks += 1;
        if let Some(n) = &mut row.judged {
            *n += usize::from(judged.contains(id.as_str()));
        }
        if let Some(n) = &mut row.escalated {
            *n += usize::from(escalated.contains(id.as_str()));
        }
        if let Some(Record::Verdict {
            verdict, at: end, ..
        }) = latest.get(id.as_str())
        {
            match verdict {
                Verdict::Accepted => row.accepted += 1,
                Verdict::Reworked => row.reworked += 1,
                Verdict::Redone => row.redone += 1,
                Verdict::Rejected => row.rejected += 1,
            }
            row.verdicts += 1;
            if let (Ok(start), Ok(end)) = (
                DateTime::parse_from_rfc3339(at),
                DateTime::parse_from_rfc3339(end),
            ) {
                minutes.push((end - start).num_milliseconds() as f64 / 60000.0);
            }
        }
    }
    Ok(rows
        .into_values()
        .map(|(mut r, mut minutes)| {
            r.accepted_pct = (r.verdicts > 0)
                .then(|| (r.accepted as f64 * 1000.0 / r.verdicts as f64).round() / 10.0);
            minutes.sort_by(f64::total_cmp);
            let n = minutes.len();
            r.median_minutes = (n > 0)
                .then(|| ((minutes[(n - 1) / 2] + minutes[n / 2]) / 2.0 * 10.0).round() / 10.0);
            r.provisional = r.verdicts < PROVISIONAL_BELOW;
            r
        })
        .collect())
}

/// Latest pick and council records, with verdicts folded into their picks.
pub fn history(path: &Path, route: Option<&str>, limit: usize) -> Result<Vec<serde_json::Value>> {
    if limit > 500 {
        return Err(Error::Usage("history limit must be at most 500".into()));
    }
    let records = read(path)?;
    let latest = latest_verdicts(&records);
    let mut entries = Vec::new();
    for r in &records {
        let (at, rid) = match r {
            Record::Pick { at, route, .. } => (at, Some(route.as_str())),
            Record::Council { at, route, .. } => (at, route.as_deref()),
            _ => continue,
        };
        if route.is_some() && route != rid {
            continue;
        }
        let mut v = serde_json::to_value(r).expect("record serializes");
        if let Record::Pick { id, .. } = r {
            let verdict = latest
                .get(id.as_str())
                .map(|r| {
                    let mut v = serde_json::to_value(r).expect("record serializes");
                    v.as_object_mut().expect("object").remove("type");
                    v.as_object_mut().expect("object").remove("id");
                    v
                })
                .unwrap_or(serde_json::Value::Null);
            v["verdict"] = verdict;
        }
        entries.push((at, v));
    }
    entries.reverse();
    entries.sort_by(|a, b| b.0.cmp(a.0));
    Ok(entries.into_iter().take(limit).map(|(_, v)| v).collect())
}

#[derive(Debug, Default, Serialize)]
pub struct CouncilStatRow {
    pub route: Option<String>,
    pub merges: usize,
    pub pass: usize,
    pub block: usize,
    pub incomplete: usize,
    pub revised: usize,
    pub passed_after_revision: usize,
    pub blocking_findings_per_merge: f64,
}

/// Merge totals and revision outcomes, grouped by route.
pub fn council_stats(
    path: &Path,
    route: Option<&str>,
    since: Option<&str>,
) -> Result<Vec<CouncilStatRow>> {
    let since = since_date(since)?;
    let records = read(path)?;
    type RunOutcomes<'a> = HashMap<&'a str, (bool, &'a str, Gate)>;
    let mut rows: BTreeMap<_, (CouncilStatRow, usize, RunOutcomes<'_>)> = BTreeMap::new();
    for r in &records {
        let Record::Council {
            at,
            route: rid,
            run,
            round,
            gate,
            groups,
            block,
            ..
        } = r
        else {
            continue;
        };
        if route.is_some() && route != rid.as_deref() || !included(at, since.as_deref()) {
            continue;
        }
        let (row, blocking, runs) = rows.entry(rid.clone()).or_insert_with(|| {
            (
                CouncilStatRow {
                    route: rid.clone(),
                    ..Default::default()
                },
                0,
                HashMap::new(),
            )
        });
        row.merges += 1;
        match gate {
            Gate::Pass => row.pass += 1,
            Gate::Block => row.block += 1,
            Gate::Incomplete => row.incomplete += 1,
        }
        *blocking += groups
            .iter()
            .filter(|(s, _)| *s <= block)
            .map(|(_, n)| n)
            .sum::<usize>();
        if let Some(run) = run {
            let entry = runs.entry(run.as_str()).or_insert((false, at, *gate));
            entry.0 |= *round >= 2;
            if at.as_str() >= entry.1 {
                entry.1 = at;
                entry.2 = *gate;
            }
        }
    }
    Ok(rows
        .into_values()
        .map(|(mut row, blocking, runs)| {
            row.revised = runs.values().filter(|r| r.0).count();
            row.passed_after_revision = runs.values().filter(|r| r.0 && r.2 == Gate::Pass).count();
            row.blocking_findings_per_merge =
                (blocking as f64 / row.merges as f64 * 10.0).round() / 10.0;
            row
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pick::{Delegate, T3Target};
    use crate::pool::State;

    fn test_config() -> Config {
        Config::parse("version = 1", Path::new("test")).unwrap()
    }

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
            run: None,
            judges: None,
            escalates: None,
            cascade: None,
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
                    preview: target.ends_with("gpt-6.2-sol"),
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
    fn stats_split_preview_measure_median_and_count_distinct_cascade_links() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        let config = Config::parse(
            include_str!("../../../fixtures/route/routes.toml"),
            Path::new("fixture"),
        )
        .unwrap();
        let mut p = pick(&["a", "b", "c"], "codex/sol");
        p.route = "impl".into();
        p.run = Some("r-draft".into());
        record_pick(&path, &p, Size::Small, now()).unwrap();
        for (id, minutes) in [("a", 2), ("b", 8), ("c", 5)] {
            record_verdict(
                &path,
                id,
                Verdict::Accepted,
                None,
                None,
                now() + chrono::Duration::minutes(minutes),
            )
            .unwrap();
        }
        // The latest verdict is chosen by time, even if an older synced line follows it.
        record_verdict(
            &path,
            "a",
            Verdict::Rejected,
            None,
            None,
            now() + chrono::Duration::minutes(1),
        )
        .unwrap();
        let mut preview = p.clone();
        preview.delegates.truncate(1);
        preview.delegates[0].id = "preview".into();
        preview.delegates[0].preview = true;
        record_pick(
            &path,
            &preview,
            Size::Small,
            now() + chrono::Duration::days(1),
        )
        .unwrap();
        let mut judge = pick(&["judge1", "judge2"], "codex/sol");
        judge.judges = Some("a".into());
        judge.escalates = Some("b".into());
        record_pick(&path, &judge, Size::Small, now()).unwrap();
        let rows = stats(&path, Some("impl"), None, &config).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (
                rows[0].picks,
                rows[0].accepted,
                rows[0].median_minutes,
                rows[0].judged,
                rows[0].escalated
            ),
            (3, 3, Some(5.0), Some(1), Some(1))
        );
        assert!(rows[1].preview);
        assert_eq!(rows[1].median_minutes, None);
        let rows = stats(&path, Some("impl"), Some("2026-10-30"), &config).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].preview);
        assert!(stats(&path, None, Some("2026-02-30"), &config).is_err());
    }

    #[test]
    fn council_counts_revisions_latest_gates_and_blocking_groups() {
        use crate::council::{self, Report, Settings};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        let mut p = pick(&["draft"], "codex/sol");
        p.run = Some("r-plan".into());
        record_pick(&path, &p, Size::Small, now()).unwrap();
        let reports = vec![Report::from_json(serde_json::json!({"member":"a","findings":[{"file":"private.rs","line":1,"severity":"P1","claim":"private claim"},{"file":"other.rs","line":10,"severity":"P3","claim":"advisory"}]}),None).unwrap(),Report {member:"b".into(),pick:None,findings:vec![]}];
        let settings = Settings {
            quorum: 2,
            block: Severity::P1,
            rounds: 1,
            round: 1,
        };
        let mut first = council::merge(&reports, settings);
        assert!(record_council(&path, &mut first, None, Some("unknown"), now()).is_err());
        record_council(&path, &mut first, Some("review"), Some("r-plan"), now()).unwrap();
        let clean = vec![
            Report {
                member: "a".into(),
                pick: None,
                findings: vec![],
            },
            Report {
                member: "b".into(),
                pick: None,
                findings: vec![],
            },
        ];
        let mut second = council::merge(
            &clean,
            Settings {
                round: 2,
                ..settings
            },
        );
        record_council(
            &path,
            &mut second,
            Some("review"),
            Some("r-plan"),
            now() + chrono::Duration::minutes(2),
        )
        .unwrap();
        let rows = council_stats(&path, None, None).unwrap();
        let r = &rows[0];
        assert_eq!(
            (
                r.merges,
                r.pass,
                r.block,
                r.incomplete,
                r.revised,
                r.passed_after_revision
            ),
            (2, 1, 1, 0, 1, 1)
        );
        assert_eq!(r.blocking_findings_per_merge, 0.5);
        assert!(
            council_stats(&path, None, Some("2026-10-30"))
                .unwrap()
                .is_empty()
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("private"));
        let record: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
        assert_eq!(record["findings"], serde_json::json!({"a":2,"b":0}));
        assert_eq!(
            record["groups"],
            serde_json::json!({"P0":0,"P1":1,"P2":0,"P3":1})
        );
        let h = history(&path, None, 2).unwrap();
        assert_eq!(h[0]["id"], second.id.unwrap());
        assert_eq!(h[1]["id"], first.id.unwrap());
        assert_eq!(history(&path, Some("review.code"), 20).unwrap().len(), 1);
        assert!(history(&path, None, 501).is_err());
        assert!(history(&path, None, 0).unwrap().is_empty());
    }

    #[test]
    fn history_joins_latest_verdict_and_orders_by_record_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        record_pick(
            &path,
            &pick(&["new"], "codex/sol"),
            Size::Small,
            now() + chrono::Duration::days(1),
        )
        .unwrap();
        record_pick(&path, &pick(&["old"], "codex/sol"), Size::Small, now()).unwrap();
        record_verdict(
            &path,
            "old",
            Verdict::Accepted,
            Some("task"),
            None,
            now() + chrono::Duration::days(2),
        )
        .unwrap();
        let h = history(&path, None, 20).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0]["id"], "new");
        assert!(h[0]["verdict"].is_null());
        assert_eq!(h[1]["verdict"]["verdict"], "accepted");
        assert_eq!(h[1]["verdict"]["task"], "task");
        assert!(h[1]["verdict"].get("id").is_none());
        assert!(h[1]["verdict"].get("type").is_none());
        assert_eq!(history(&path, None, 1).unwrap().len(), 1);
    }

    #[test]
    fn preview_is_written_only_when_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        record_pick(
            &path,
            &pick(&["p-1"], "codex/gpt-6.1-sol"),
            Size::Small,
            now(),
        )
        .unwrap();
        record_pick(
            &path,
            &pick(&["p-2"], "codex/gpt-6.2-sol"),
            Size::Small,
            now(),
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(!lines[0].contains("preview"), "{}", lines[0]);
        assert!(
            lines[1].ends_with(r#""size":"small","preview":true}"#),
            "{}",
            lines[1]
        );
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
        let rows = stats(&path, None, None, &test_config()).unwrap();
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
        assert_eq!(
            stats(&path, Some("review.code"), None, &test_config())
                .unwrap()
                .len(),
            1
        );
        assert!(
            stats(&path, Some("other"), None, &test_config())
                .unwrap()
                .is_empty()
        );
    }
}
