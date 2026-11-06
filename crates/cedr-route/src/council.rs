//! Council: plan a multi-vendor review on a council route, then merge the members' reports into one
//! gate (docs/council.md). Findings that point at the same place are grouped, never voted away.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::pick::{Context, Pick, Size};

/// Findings within this many lines of the previous one in a group join that group.
pub const GROUP_LINES: u64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    P0,
    P1,
    P2,
    P3,
}

impl Severity {
    pub fn parse(s: &str) -> Option<Severity> {
        match s {
            "P0" => Some(Severity::P0),
            "P1" => Some(Severity::P1),
            "P2" => Some(Severity::P2),
            "P3" => Some(Severity::P3),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub file: String,
    pub line: u64,
    pub severity: Severity,
    pub claim: String,
    #[serde(default)]
    pub scenario: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spare_for: Option<String>,
    /// May be absent in a JSON report when the `<member>=<path>` argument names it.
    #[serde(default)]
    pub member: String,
    #[serde(default)]
    pub pick: Option<String>,
    /// The member's own score for the change, out of 10. It is shown beside the gate and never
    /// decides it.
    #[serde(default)]
    pub score: Option<f64>,
    /// Required, so a report that lost its findings is an error rather than a clean review.
    pub findings: Vec<Finding>,
}

impl Report {
    /// A JSON report; `member` overrides the report's own when given.
    pub fn from_json(doc: Value, member: Option<&str>) -> Result<Report> {
        let mut report: Report = serde_json::from_value(doc)
            .map_err(|e| Error::Usage(format!("not a council report: {e}")))?;
        if let Some(m) = member {
            report.member = m.to_string();
        }
        if report.member.is_empty() {
            return Err(Error::Usage("a council report needs a member".into()));
        }
        if let Some(score) = report.score.filter(|s| !(0.0..=10.0).contains(s)) {
            return Err(Error::Usage(format!(
                "{} scored {score}; a score is 0 to 10",
                report.member
            )));
        }
        report.score = report.score.map(|s| if s == 0.0 { 0.0 } else { s });
        Ok(report)
    }

    /// A text report: lines `<file>:<line> - <P0..P3> - <claim>[ - <scenario>]`, tolerating the
    /// list markers, backticks and bold that review replies wrap them in, and a `Score: <n>/10`
    /// line, the last one counting. Other lines are ignored.
    pub fn from_text(text: &str, member: &str) -> Report {
        let findings = text.lines().filter_map(parse_line).collect();
        Report {
            spare_for: None,
            member: member.to_string(),
            pick: None,
            score: {
                let last = text
                    .lines()
                    .enumerate()
                    .filter(|(_, l)| !l.trim().is_empty())
                    .map(|(i, _)| i)
                    .last();
                text.lines()
                    .enumerate()
                    .filter(|(i, l)| {
                        (!l.trim().starts_with("- ") && !l.trim().starts_with("* "))
                            || Some(*i) == last
                    })
                    .filter_map(|(_, l)| score_candidate(l))
                    .last()
                    .flatten()
            },
            findings,
        }
    }
}

/// The parsed value of a single score candidate, for parser regression cases.
#[cfg(test)]
fn parse_score(line: &str) -> Option<f64> {
    score_candidate(line).flatten()
}

// Keep an invalid final score candidate so it cannot fall back to an earlier score.
fn score_candidate(line: &str) -> Option<Option<f64>> {
    let line = line
        .trim()
        .trim_start_matches(['-', '*', '`', '_', '#', '>', '|', ' ']);
    let line = if line
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("overall "))
    {
        &line[8..]
    } else {
        line
    };
    line.get(..5).filter(|p| p.eq_ignore_ascii_case("score"))?;
    let rest = &line[5..];
    if !rest.starts_with(|c: char| c == ':' || c.is_whitespace() || c == '*' || c == '|') {
        return None;
    }
    let rest = rest.trim_start_matches([':', '*', '`', '_', '|', ' ']);
    // A sentence that merely starts with the word is not a score, and must not hide one before it.
    let (score, scale) = rest.split_once('/')?;
    let scale = scale
        .trim()
        .trim_end_matches(['*', '`', '_', '.', '|', ' ']);
    let valid_scale = scale == "10"
        || scale
            .strip_prefix("10 ")
            .is_some_and(|s| s.starts_with('(') && s.ends_with(')'));
    Some(if valid_scale {
        score
            .trim()
            .trim_matches(['*', '`', '_'])
            .parse::<f64>()
            .ok()
            .filter(|s| (0.0..=10.0).contains(s))
            .map(|s| if s == 0.0 { 0.0 } else { s })
    } else {
        None
    })
}

fn parse_line(line: &str) -> Option<Finding> {
    let line = line.trim();
    let line = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .unwrap_or(line);
    let mut parts = line.splitn(4, " - ");
    let location = parts.next()?.trim().trim_matches('`');
    let severity = Severity::parse(parts.next()?.trim().trim_matches('*'))?;
    let claim = parts.next()?.trim().to_string();
    let scenario = parts
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let (file, line_no) = location.rsplit_once(':')?;
    let line_no: u64 = line_no.parse().ok()?;
    if file.is_empty() || claim.is_empty() {
        return None;
    }
    Some(Finding {
        file: file.to_string(),
        line: line_no,
        severity,
        claim,
        scenario,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Gate {
    Pass,
    Block,
    Incomplete,
}

impl Gate {
    pub fn as_str(self) -> &'static str {
        match self {
            Gate::Pass => "pass",
            Gate::Block => "block",
            Gate::Incomplete => "incomplete",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MemberFinding {
    pub member: String,
    pub severity: Severity,
    pub line: u64,
    pub claim: String,
    pub scenario: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub file: String,
    pub line: u64,
    pub severity: Severity,
    pub members: Vec<String>,
    pub findings: Vec<MemberFinding>,
}

impl Group {
    /// The claim of the group's most severe finding, first reported.
    pub fn headline(&self) -> &str {
        self.findings
            .iter()
            .min_by_key(|f| f.severity)
            .map(|f| f.claim.as_str())
            .unwrap_or("")
    }
}

/// What follows a merge: another revision round, or nothing (the gate stands).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Next {
    Revise,
    Done,
}

#[derive(Debug, Clone, Serialize)]
pub struct Merged {
    pub id: Option<String>,
    pub gate: Gate,
    pub quorum: usize,
    pub block: Severity,
    pub round: u32,
    pub rounds: Option<u32>,
    pub next: Next,
    pub stalled: bool,
    pub spares: BTreeMap<String, String>,
    /// The revision brief for the change's author; only when `next` is revise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    pub reviewers: Vec<String>,
    /// Each reviewer's score out of 10; null when it gave none.
    pub scores: BTreeMap<String, Option<f64>>,
    pub groups: Vec<Group>,
}

/// The Markdown revision brief (docs/council.md, Critique and revise): every group in merge order
/// with every finding, so dissent reaches the author.
fn brief(groups: &[Group], block: Severity, round: u32, rounds: Option<u32>) -> String {
    let rounds = rounds
        .map(|n| n.to_string())
        .unwrap_or_else(|| "unlimited".into());
    let mut out = format!(
        "# Council revision brief (revision {round} of {rounds})\n\n\
         Blocking: {block:?} and above. Fix each blocking group or rebut it with evidence; the others are advisory.\n\
         Fix only what this change needs; anything else goes on a follow-up list, not into this revision.\n\
         Reply with one line per group: `<file>:<line> fixed|rebutted - <reason>`.\n"
    );
    for g in groups {
        out.push_str(&format!(
            "\n## {:?} {}:{} ({})\n",
            g.severity,
            g.file,
            g.line,
            g.members.join(", ")
        ));
        for f in &g.findings {
            out.push_str(&format!(
                "- {} {:?} :{} {}",
                f.member, f.severity, f.line, f.claim
            ));
            if let Some(scenario) = &f.scenario {
                out.push_str(&format!(" - {scenario}"));
            }
            out.push('\n');
        }
    }
    out
}

/// Groups findings that point at the same place, then decides the gate: any finding at or above the
/// blocking severity blocks, otherwise the council passes only once the quorum has reported.
pub fn merge(reports: &[Report], settings: Settings) -> Merged {
    let Settings {
        quorum,
        block,
        rounds,
        round,
    } = settings;
    let reviewers: Vec<String> = reports
        .iter()
        .map(|r| r.member.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut scores: BTreeMap<String, Option<f64>> = BTreeMap::new();
    for r in reports {
        let score = scores.entry(r.member.clone()).or_default();
        *score = r.score.or(*score);
    }
    let mut all: Vec<(&str, &Finding)> = reports
        .iter()
        .flat_map(|r| r.findings.iter().map(move |f| (r.member.as_str(), f)))
        .collect();
    all.sort_by(|a, b| a.1.file.cmp(&b.1.file).then(a.1.line.cmp(&b.1.line)));
    let mut groups: Vec<(u64, Group)> = Vec::new(); // (last line, group)
    for (member, f) in all {
        let joins = groups
            .last()
            .is_some_and(|(last, g)| g.file == f.file && f.line - last <= GROUP_LINES);
        if !joins {
            groups.push((
                f.line,
                Group {
                    file: f.file.clone(),
                    line: f.line,
                    severity: f.severity,
                    members: Vec::new(),
                    findings: Vec::new(),
                },
            ));
        }
        let (last, g) = groups.last_mut().expect("a group exists");
        *last = f.line;
        g.severity = g.severity.min(f.severity);
        if !g.members.iter().any(|m| m == member) {
            g.members.push(member.to_string());
        }
        g.findings.push(MemberFinding {
            member: member.to_string(),
            severity: f.severity,
            line: f.line,
            claim: f.claim.clone(),
            scenario: f.scenario.clone(),
        });
    }
    let mut groups: Vec<Group> = groups.into_iter().map(|(_, g)| g).collect();
    groups.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });
    let gate = if groups.iter().any(|g| g.severity <= block) {
        Gate::Block
    } else if reviewers.len() >= quorum {
        Gate::Pass
    } else {
        Gate::Incomplete
    };
    let next = if gate == Gate::Block && rounds.is_none_or(|n| round <= n) {
        Next::Revise
    } else {
        Next::Done
    };
    Merged {
        id: None,
        gate,
        quorum,
        block,
        round,
        rounds,
        next,
        stalled: false,
        spares: reports
            .iter()
            .filter_map(|r| r.spare_for.as_ref().map(|s| (r.member.clone(), s.clone())))
            .collect(),
        brief: (next == Next::Revise).then(|| brief(&groups, block, round, rounds)),
        reviewers,
        scores,
        groups,
    }
}

/// Apply the previous merge only to blocking locations; no claims enter history.
pub fn apply_previous(merged: &mut Merged, previous: Option<Value>) -> Result<()> {
    if merged.rounds.is_none() && merged.round >= 2 && previous.is_none() {
        return Err(Error::Usage(
            "unlimited rounds at round 2 or later require --previous".into(),
        ));
    }
    if let Some(value) = previous {
        #[derive(Deserialize)]
        struct Previous {
            gate: Gate,
            block: Severity,
            groups: Vec<Location>,
        }
        #[derive(Deserialize)]
        struct Location {
            file: String,
            line: u64,
            severity: Severity,
        }
        let prev: Previous = serde_json::from_value(value)
            .map_err(|e| Error::Usage(format!("not a previous council merge: {e}")))?;
        let mut locations: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for p in prev.groups.into_iter().filter(|p| p.severity <= prev.block) {
            locations.entry(p.file).or_default().push(p.line);
        }
        for lines in locations.values_mut() {
            lines.sort_unstable();
        }
        merged.stalled = merged.gate == Gate::Block
            && prev.gate == Gate::Block
            && merged
                .groups
                .iter()
                .filter(|g| g.severity <= merged.block)
                .all(|g| {
                    locations.get(&g.file).is_some_and(|lines| {
                        let i = lines.partition_point(|line| *line < g.line.saturating_sub(10));
                        lines.get(i).is_some_and(|line| line.abs_diff(g.line) <= 10)
                    })
                });
        if merged.stalled {
            merged.next = Next::Done;
            merged.brief = None;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    #[serde(flatten)]
    pub pick: Pick,
    pub quorum: usize,
    pub quorum_reachable: bool,
    pub fixes: crate::config::Fixes,
    pub advisory: crate::config::Advisory,
    pub rounds: Option<u32>,
}

/// How a merge decides: the quorum, the least severe finding that blocks, the revision rounds
/// allowed, and which review this is (1 for the first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Settings {
    pub quorum: usize,
    pub block: Severity,
    pub rounds: Option<u32>,
    pub round: u32,
}

/// Merge settings: given values first, then the council route's, then quorum 2, P1 and 1 round.
/// `round` defaults to 1 and, with numbered rounds, is at most `rounds + 1`, the review after the
/// last revision. `config` is read only when a route is named.
pub fn settings(
    quorum: Option<usize>,
    block: Option<Severity>,
    rounds: Option<u32>,
    round: Option<u32>,
    route: Option<&str>,
    config: impl FnOnce() -> Result<crate::config::Config>,
) -> Result<Settings> {
    if quorum == Some(0) {
        return Err(Error::Usage("quorum must be at least 1".into()));
    }
    if rounds.is_some_and(|r| r > 3) {
        return Err(Error::Usage("rounds must be between 0 and 3".into()));
    }
    if round == Some(0) {
        return Err(Error::Usage("round must be at least 1".into()));
    }
    let from_route = match route {
        Some(r) => {
            let config = config()?;
            let route = config
                .routes
                .get(r)
                .filter(|x| x.council)
                .ok_or_else(|| Error::Usage(format!("{r:?} is not a council route")))?;
            Some((route.quorum(), route.block(), route.rounds()))
        }
        None => None,
    };
    let rounds = rounds
        .map(Some)
        .or(from_route.map(|r| r.2))
        .unwrap_or(Some(1));
    let round = round.unwrap_or(1);
    // The review after the last revision decides; a later round would reopen a gate that stood.
    if let Some(n) = rounds
        && round > n + 1
    {
        return Err(Error::Usage(format!(
            "round {round} is past the last review ({}) for rounds = {n}: the gate already stood",
            n + 1
        )));
    }
    Ok(Settings {
        quorum: quorum.or(from_route.map(|r| r.0)).unwrap_or(2),
        block: block.or(from_route.map(|r| r.1)).unwrap_or(Severity::P1),
        rounds,
        round,
    })
}

/// The members to start for a council route.
pub fn plan(ctx: &Context, route_id: &str, size: Size, avoid: &[String]) -> Result<Plan> {
    plan_members(ctx, route_id, size, avoid, &[])
}

/// Plan explicit members when given, keeping the configured quorum and revision settings.
pub fn plan_members(
    ctx: &Context,
    route_id: &str,
    size: Size,
    avoid: &[String],
    members: &[String],
) -> Result<Plan> {
    let route = ctx
        .config
        .routes
        .get(route_id)
        .filter(|r| r.council)
        .ok_or_else(|| Error::Usage(format!("{route_id:?} is not a council route")))?;
    let quorum = route.quorum();
    let mut pick = if members.is_empty() {
        ctx.pick(route_id, size, avoid)?
    } else {
        ctx.council_members(route_id, size, avoid, members)?
    };
    let mut vendors = BTreeMap::new();
    for d in &pick.delegates {
        *vendors
            .entry(crate::pick::vendor(&d.harness))
            .or_insert(0usize) += 1;
    }
    for (vendor, count) in vendors {
        if count >= quorum {
            pick.warnings.push(format!("quorum {quorum} can be reached by {vendor} alone; different vendors miss different things"));
        }
    }
    pick.run = Some(crate::pick::new_id().replacen("p-", "r-", 1));
    Ok(Plan {
        fixes: route.fixes.unwrap_or_default(),
        advisory: route.advisory.unwrap_or_default(),
        rounds: route.rounds(),
        quorum_reachable: pick.delegates.len() >= quorum,
        pick,
        quorum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn f(file: &str, line: u64, severity: Severity, claim: &str) -> Finding {
        Finding {
            file: file.into(),
            line,
            severity,
            claim: claim.into(),
            scenario: None,
        }
    }

    fn q(quorum: usize) -> Settings {
        Settings {
            quorum,
            block: Severity::P1,
            rounds: Some(1),
            round: 1,
        }
    }

    #[test]
    fn a_score_line_is_read_however_a_reply_decorates_it() {
        for line in [
            "Score: 8/10",
            "**Score:** 8/10",
            "- score 8 / 10.",
            "`SCORE: 8/10`",
        ] {
            assert_eq!(parse_score(line), Some(8.0), "{line}");
        }
        for line in [
            "Score: 11/10",
            "score: 8/10 once the tests pass",
            "score/a.rs:1 - P1 - it fails",
            "score.rs:12 - P1 - 3/10 of the time",
        ] {
            assert_eq!(parse_score(line), None, "{line}");
        }
    }

    fn report(member: &str, findings: Vec<Finding>) -> Report {
        Report {
            spare_for: None,
            member: member.into(),
            pick: None,
            score: None,
            findings,
        }
    }

    #[test]
    fn parses_review_lines_as_reviewers_write_them() {
        let text = "Findings:\n\
            - `crates/a.rs:264` - **P1** - redirects bypass HTTPS - an HTTPS feed redirects to HTTP\n\
            src/b.rs:9 - P3 - tie order differs\n\
            not a finding: src/c.rs:1 - P9 - nope\n";
        let r = Report::from_text(text, "codex/sol");
        assert_eq!(r.findings.len(), 2);
        let a = &r.findings[0];
        assert_eq!(
            (a.file.as_str(), a.line, a.severity),
            ("crates/a.rs", 264, Severity::P1)
        );
        assert_eq!(
            a.scenario.as_deref(),
            Some("an HTTPS feed redirects to HTTP")
        );
        assert_eq!(r.findings[1].scenario, None);
    }

    #[test]
    fn nearby_findings_group_and_keep_every_member_and_severity() {
        let m = merge(
            &[
                report(
                    "codex",
                    vec![
                        f("a.rs", 10, Severity::P2, "unbounded read"),
                        f("a.rs", 40, Severity::P3, "naming"),
                    ],
                ),
                report(
                    "claude",
                    vec![f("a.rs", 12, Severity::P1, "reads until EOF")],
                ),
            ],
            q(2),
        );
        assert_eq!(m.gate, Gate::Block);
        assert_eq!(m.groups.len(), 2);
        let g = &m.groups[0];
        assert_eq!(
            (g.line, g.severity, g.members.clone()),
            (
                10,
                Severity::P1,
                vec!["codex".to_string(), "claude".to_string()]
            )
        );
        assert_eq!(g.headline(), "reads until EOF");
        assert_eq!(g.findings.len(), 2, "dissent on severity is kept");
    }

    #[test]
    fn groups_chain_but_never_cross_files() {
        let m = merge(
            &[report(
                "a",
                vec![
                    f("x.rs", 1, Severity::P3, "1"),
                    f("x.rs", 4, Severity::P3, "2"),
                    f("x.rs", 7, Severity::P3, "3"),
                    f("y.rs", 8, Severity::P3, "4"),
                ],
            )],
            q(1),
        );
        let lines: Vec<(&str, usize)> = m
            .groups
            .iter()
            .map(|g| (g.file.as_str(), g.findings.len()))
            .collect();
        assert_eq!(lines, [("x.rs", 3), ("y.rs", 1)]);
    }

    #[test]
    fn below_quorum_is_incomplete_but_serious_findings_still_block() {
        assert_eq!(
            merge(&[report("codex", vec![])], q(2)).gate,
            Gate::Incomplete
        );
        assert_eq!(
            merge(
                &[
                    report("codex", vec![]),
                    report("claude", vec![f("a", 1, Severity::P3, "x")])
                ],
                q(2)
            )
            .gate,
            Gate::Pass
        );
        assert_eq!(
            merge(&[report("codex", vec![f("a", 1, Severity::P0, "x")])], q(2)).gate,
            Gate::Block
        );
    }

    #[test]
    fn the_blocking_severity_is_configurable() {
        let p2 = [
            report("codex", vec![f("a", 1, Severity::P2, "x")]),
            report("claude", vec![]),
        ];
        assert_eq!(merge(&p2, q(2)).gate, Gate::Pass);
        assert_eq!(
            merge(
                &p2,
                Settings {
                    block: Severity::P2,
                    ..q(2)
                }
            )
            .gate,
            Gate::Block
        );
        let p1 = [
            report("codex", vec![f("a", 1, Severity::P1, "x")]),
            report("claude", vec![]),
        ];
        assert_eq!(
            merge(
                &p1,
                Settings {
                    block: Severity::P0,
                    ..q(2)
                }
            )
            .gate,
            Gate::Pass
        );
    }

    #[test]
    fn a_blocked_gate_goes_back_for_revision_while_rounds_remain() {
        let mut a = f("src/a.rs", 12, Severity::P1, "reads until EOF");
        a.scenario = Some("a 2 GB log".into());
        let reports = [
            report("codex", vec![a]),
            report("claude", vec![f("src/a.rs", 14, Severity::P2, "no cap")]),
        ];
        let first = merge(&reports, q(2));
        assert_eq!(
            (first.next, first.round, first.rounds),
            (Next::Revise, 1, Some(1))
        );
        assert_eq!(
            first.brief.as_deref(),
            Some(
                "# Council revision brief (revision 1 of 1)\n\n\
                 Blocking: P1 and above. Fix each blocking group or rebut it with evidence; the others are advisory.\n\
                 Fix only what this change needs; anything else goes on a follow-up list, not into this revision.\n\
                 Reply with one line per group: `<file>:<line> fixed|rebutted - <reason>`.\n\n\
                 ## P1 src/a.rs:12 (codex, claude)\n\
                 - codex P1 :12 reads until EOF - a 2 GB log\n\
                 - claude P2 :14 no cap\n"
            )
        );
        let last = merge(&reports, Settings { round: 2, ..q(2) });
        assert_eq!((last.next, last.brief.is_none()), (Next::Done, true));
        let none = merge(
            &reports,
            Settings {
                rounds: Some(0),
                ..q(2)
            },
        );
        assert_eq!(none.next, Next::Done);
        let pass = merge(&[report("codex", vec![]), report("claude", vec![])], q(2));
        assert_eq!(pass.next, Next::Done);
        let v = serde_json::to_value(&pass).unwrap();
        assert!(v.get("brief").is_none() && v["next"] == "done");
    }

    #[test]
    fn rounds_come_from_flags_then_the_route_then_one() {
        let config = || {
            crate::config::Config::parse(
                "version = 1\n[harnesses.codex]\n[routes.c]\ncouncil = true\nrounds = 2\ncandidates = [{ target = \"codex/sol\" }]\n",
                std::path::Path::new("t.toml"),
            )
        };
        let s = settings(None, None, None, None, Some("c"), config).unwrap();
        assert_eq!((s.rounds, s.round), (Some(2), 1));
        assert_eq!(
            settings(None, None, Some(0), Some(1), Some("c"), config)
                .unwrap()
                .rounds,
            Some(0)
        );
        assert_eq!(
            settings(None, None, None, Some(3), Some("c"), config)
                .unwrap()
                .round,
            3
        );
        let past = settings(None, None, None, Some(4), Some("c"), config).unwrap_err();
        assert!(
            past.to_string()
                .contains("round 4 is past the last review (3) for rounds = 2")
        );
        assert!(settings(None, None, Some(0), Some(2), Some("c"), config).is_err());
        let unlimited = || {
            crate::config::Config::parse(
                "version = 1\n[harnesses.codex]\n[routes.c]\ncouncil = true\nrounds = \"unlimited\"\ncandidates = [{ target = \"codex/sol\" }]\n",
                std::path::Path::new("t.toml"),
            )
        };
        assert_eq!(
            settings(None, None, None, Some(50), Some("c"), unlimited)
                .unwrap()
                .round,
            50
        );
        assert_eq!(
            settings(None, None, None, None, None, config)
                .unwrap()
                .rounds,
            Some(1)
        );
        assert!(settings(None, None, Some(4), None, None, config).is_err());
        assert!(settings(None, None, None, Some(0), None, config).is_err());
    }

    #[test]
    fn json_reports_need_a_member_and_known_fields() {
        let ok = Report::from_json(json!({"member": "codex", "findings": [{"file": "a", "line": 1, "severity": "P2", "claim": "c"}]}), None).unwrap();
        assert_eq!(ok.findings[0].severity, Severity::P2);
        assert_eq!(
            Report::from_json(json!({"findings": []}), Some("claude"))
                .unwrap()
                .member,
            "claude"
        );
        assert!(Report::from_json(json!({"member": "", "findings": []}), None).is_err());
        assert!(
            Report::from_json(json!({"member": "codex"}), None).is_err(),
            "findings are required"
        );
        assert!(Report::from_json(json!({"member": "x", "findings": [{"file": "a", "line": 1, "severity": "P7", "claim": "c"}]}), None).is_err());
    }
    #[test]
    fn scores_use_only_the_last_candidate_and_normalize_zero() {
        for line in [
            "Score: 7/10",
            "**Score:** 7/10",
            "## Score: 7/10",
            "> Score: 7/10",
            "Score: 7/10 (solid)",
            "Score: **7**/10",
            "Overall score: 7/10",
            "| Score | 7/10 |",
            "SCORE 7 / 10.",
        ] {
            assert_eq!(Report::from_text(line, "a").score, Some(7.0), "{line}");
        }
        for line in [
            "score: 8/10 once the tests pass",
            "score/a.rs:1 - P1 - x",
            "score.rs:12 - P1 - 3/10 of the time",
            "- Score: 7/10\nexample above",
        ] {
            assert_eq!(Report::from_text(line, "a").score, None, "{line}");
        }
        assert_eq!(
            Report::from_text("Score: 8/10\nScore is subjective, though.", "a").score,
            Some(8.0)
        );
        assert_eq!(
            Report::from_text("Score: 8/10\nScore: 11/10", "a").score,
            None
        );
        assert_eq!(Report::from_text("- Score: 7/10\n\n", "a").score, Some(7.0));
        assert_eq!(
            Report::from_text("Score: -0/10", "a")
                .score
                .unwrap()
                .to_bits(),
            0f64.to_bits()
        );
        assert_eq!(
            Report::from_json(json!({"member":"a","score":-0.0,"findings":[]}), None)
                .unwrap()
                .score
                .unwrap()
                .to_bits(),
            0f64.to_bits()
        );
    }

    #[test]
    fn unlimited_requires_previous_and_stalls_only_on_returned_blocking_locations() {
        let previous = merge(
            &[report("a", vec![f("a.rs", 20, Severity::P1, "old")])],
            q(1),
        );
        let prev = serde_json::to_value(previous).unwrap();
        for (line, new_group, previous_pass, stalled) in [
            (30, false, false, true),
            (31, false, false, false),
            (20, true, false, false),
            (20, false, true, false),
        ] {
            let mut findings = vec![f("a.rs", line, Severity::P1, "changed words")];
            if new_group {
                findings.push(f("b.rs", 1, Severity::P0, "new"));
            }
            let mut m = merge(
                &[report("a", findings)],
                Settings {
                    rounds: None,
                    round: 2,
                    ..q(1)
                },
            );
            assert_eq!(m.next, Next::Revise);
            assert!(
                m.brief
                    .as_ref()
                    .unwrap()
                    .contains("(revision 2 of unlimited)")
            );
            assert!(apply_previous(&mut m, None).is_err());
            let mut p = prev.clone();
            if previous_pass {
                p["gate"] = json!("pass");
            }
            apply_previous(&mut m, Some(p)).unwrap();
            assert_eq!(m.stalled, stalled);
            assert_eq!(m.brief.is_none(), stalled);
            assert_eq!(serde_json::to_value(&m).unwrap()["rounds"], Value::Null);
        }
        let mut numbered = merge(&[], q(1));
        assert!(apply_previous(&mut numbered, None).is_ok());
    }
}
