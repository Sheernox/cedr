//! Council: plan a multi-vendor review on a council route, then merge the members' reports into one
//! gate (docs/council.md). Findings that point at the same place are grouped, never voted away.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Route;
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
    /// May be absent in a JSON report when the `<member>=<path>` argument names it.
    #[serde(default)]
    pub member: String,
    #[serde(default)]
    pub pick: Option<String>,
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
        Ok(report)
    }

    /// A text report: lines `<file>:<line> - <P0..P3> - <claim>[ - <scenario>]`, tolerating the
    /// list markers, backticks and bold that review replies wrap them in. Other lines are ignored.
    pub fn from_text(text: &str, member: &str) -> Report {
        let findings = text.lines().filter_map(parse_line).collect();
        Report {
            member: member.to_string(),
            pick: None,
            findings,
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct Merged {
    pub gate: Gate,
    pub quorum: usize,
    pub block: Severity,
    pub reviewers: Vec<String>,
    pub groups: Vec<Group>,
}

/// Groups findings that point at the same place, then decides the gate: any finding at or above the
/// blocking severity blocks, otherwise the council passes only once the quorum has reported.
pub fn merge(reports: &[Report], settings: Settings) -> Merged {
    let Settings { quorum, block } = settings;
    let reviewers: Vec<String> = reports
        .iter()
        .map(|r| r.member.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
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
    Merged {
        gate,
        quorum,
        block,
        reviewers,
        groups,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    #[serde(flatten)]
    pub pick: Pick,
    pub quorum: usize,
    pub quorum_reachable: bool,
}

/// The quorum for a merge: as given, else the council route's, else 2. `config` is read only when
/// a route is named.
/// How a merge decides: the quorum, and the least severe finding that blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Settings {
    pub quorum: usize,
    pub block: Severity,
}

/// Merge settings: given values first, then the council route's, then quorum 2 and P1. `config` is read
/// only when a route is named.
pub fn settings(
    quorum: Option<usize>,
    block: Option<Severity>,
    route: Option<&str>,
    config: impl FnOnce() -> Result<crate::config::Config>,
) -> Result<Settings> {
    if quorum == Some(0) {
        return Err(Error::Usage("quorum must be at least 1".into()));
    }
    let from_route = match route {
        Some(r) => {
            let config = config()?;
            let route = config
                .routes
                .get(r)
                .filter(|x| x.council)
                .ok_or_else(|| Error::Usage(format!("{r:?} is not a council route")))?;
            Some((route.quorum(), route.block()))
        }
        None => None,
    };
    Ok(Settings {
        quorum: quorum.or(from_route.map(|r| r.0)).unwrap_or(2),
        block: block.or(from_route.map(|r| r.1)).unwrap_or(Severity::P1),
    })
}

/// The members to start for a council route.
pub fn plan(ctx: &Context, route_id: &str, size: Size, avoid: &[String]) -> Result<Plan> {
    let route: &Route = ctx
        .config
        .routes
        .get(route_id)
        .filter(|r| r.council)
        .ok_or_else(|| Error::Usage(format!("{route_id:?} is not a council route")))?;
    let quorum = route.quorum();
    let pick = ctx.pick(route_id, size, avoid)?;
    Ok(Plan {
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
        }
    }

    fn report(member: &str, findings: Vec<Finding>) -> Report {
        Report {
            member: member.into(),
            pick: None,
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
                    quorum: 2,
                    block: Severity::P2
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
                    quorum: 2,
                    block: Severity::P0
                }
            )
            .gate,
            Gate::Pass
        );
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
}
