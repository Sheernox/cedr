//! Shared SARIF 2.1.0 and GitLab Code Quality rendering for findings.
//! Guard, Sync and Council findings are converted to [`OutputFinding`] first, so every command
//! emits the same shapes. Exit codes are decided by the caller and never change with the format.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::finding::{Report, Severity};

/// SARIF `level` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputLevel {
    Error,
    Warning,
    Note,
}

impl OutputLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
        }
    }
    /// GitHub workflow command for this level.
    pub fn github_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "notice",
        }
    }
}

/// GitLab Code Quality `severity` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitlabSeverity {
    Info,
    Minor,
    Major,
    Critical,
    Blocker,
}

impl GitlabSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Minor => "minor",
            Self::Major => "major",
            Self::Critical => "critical",
            Self::Blocker => "blocker",
        }
    }
}

/// One finding in the shape SARIF and GitLab both need. `path` is relative to the repo root with
/// `/` separators and may be empty when no file is known; `line` is 1-based when known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputFinding {
    pub rule_id: String,
    pub rule_description: String,
    pub level: OutputLevel,
    pub gitlab_severity: GitlabSeverity,
    pub message: String,
    pub path: String,
    pub line: Option<u64>,
}

impl From<&crate::finding::Finding> for OutputFinding {
    fn from(f: &crate::finding::Finding) -> Self {
        let (level, gitlab_severity) = match f.severity {
            Severity::Error => (OutputLevel::Error, GitlabSeverity::Major),
            Severity::Warning => (OutputLevel::Warning, GitlabSeverity::Minor),
        };
        Self {
            rule_id: f.rule.as_str().to_string(),
            rule_description: f.rule.description().to_string(),
            level,
            gitlab_severity,
            message: f.message.clone(),
            path: f.path.clone(),
            line: f.line.map(|n| n as u64),
        }
    }
}

/// Findings from a Guard or Sync [`Report`], in report order.
pub fn from_report(report: &Report) -> Vec<OutputFinding> {
    report.findings.iter().map(OutputFinding::from).collect()
}

/// SHA-256 hex of rule, path, line and message, joined by NUL so no field can bleed into the next.
pub fn fingerprint(f: &OutputFinding) -> String {
    let line = f.line.map(|n| n.to_string()).unwrap_or_default();
    let digest =
        Sha256::digest(format!("{}\0{}\0{}\0{}", f.rule_id, f.path, line, f.message).as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// SARIF 2.1.0 with one run, one rule per finding kind and one result per finding.
pub fn sarif(findings: &[OutputFinding], tool_version: &str) -> String {
    serde_json::to_string_pretty(&sarif_value(findings, tool_version)).expect("sarif serializes")
}

pub fn sarif_value(findings: &[OutputFinding], tool_version: &str) -> Value {
    // Rules in first-appearance order, so ruleIndex is stable for a given finding list.
    let mut rules: Vec<&OutputFinding> = Vec::new();
    for f in findings {
        if !rules.iter().any(|r| r.rule_id == f.rule_id) {
            rules.push(f);
        }
    }
    let rule_index = |id: &str| rules.iter().position(|r| r.rule_id == id).unwrap_or(0);
    let results: Vec<Value> = findings
        .iter()
        .map(|f| {
            let mut result = json!({
                "ruleId": f.rule_id,
                "ruleIndex": rule_index(&f.rule_id),
                "level": f.level.as_str(),
                "message": {"text": f.message},
            });
            if !f.path.is_empty() {
                let mut physical = json!({"artifactLocation": {"uri": f.path}});
                if let Some(line) = f.line {
                    physical["region"] = json!({"startLine": line});
                }
                result["locations"] = json!([{"physicalLocation": physical}]);
            }
            result
        })
        .collect();
    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {"driver": {
                "name": "cedr",
                "version": tool_version,
                "informationUri": "https://cedr.dev",
                "rules": rules.iter().map(|r| json!({
                    "id": r.rule_id,
                    "name": r.rule_id,
                    "shortDescription": {"text": r.rule_description},
                    "defaultConfiguration": {"level": r.level.as_str()},
                })).collect::<Vec<_>>(),
            }},
            "results": results,
        }],
    })
}

/// GitLab Code Quality report: a JSON array, one entry per finding.
pub fn gitlab(findings: &[OutputFinding]) -> String {
    serde_json::to_string_pretty(&gitlab_value(findings)).expect("gitlab report serializes")
}

pub fn gitlab_value(findings: &[OutputFinding]) -> Value {
    Value::Array(
        findings
            .iter()
            .map(|f| {
                json!({
                    "description": f.message,
                    "check_name": f.rule_id,
                    "fingerprint": fingerprint(f),
                    "severity": f.gitlab_severity.as_str(),
                    "location": {
                        "path": f.path,
                        "lines": {"begin": f.line.unwrap_or(1)},
                    },
                })
            })
            .collect(),
    )
}

/// GitHub workflow annotations, one per finding (no summary line; callers add their own).
pub fn github(findings: &[OutputFinding]) -> String {
    let mut out = String::new();
    for f in findings {
        let mut props = format!("file={}", github_property(&f.path));
        if let Some(line) = f.line {
            props.push_str(&format!(",line={line}"));
        }
        props.push_str(&format!(",title={}", github_property(&f.rule_id)));
        out.push_str(&format!(
            "::{} {props}::{}\n",
            f.level.github_str(),
            github_data(&f.message)
        ));
    }
    out
}

fn github_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn github_property(s: &str) -> String {
    github_data(s).replace(':', "%3A").replace(',', "%2C")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{Finding, Rule};

    fn sample() -> Vec<OutputFinding> {
        vec![
            OutputFinding::from(&Finding::new(
                Rule::Cg001,
                Severity::Error,
                "AGENTS.md",
                Some(12),
                "npm script \"deploy\" not found in package.json",
            )),
            OutputFinding::from(&Finding::new(
                Rule::Cg002,
                Severity::Warning,
                "docs/guide.md",
                None,
                "link target missing",
            )),
        ]
    }

    #[test]
    fn sarif_matches_expected_document() {
        let got: Value = serde_json::from_str(&sarif(&sample(), "0.0.0-test")).unwrap();
        let expected = json!({
            "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
            "version": "2.1.0",
            "runs": [{
                "tool": {"driver": {
                    "name": "cedr",
                    "version": "0.0.0-test",
                    "informationUri": "https://cedr.dev",
                    "rules": [
                        {"id": "CG001", "name": "CG001",
                         "shortDescription": {"text": Rule::Cg001.description()},
                         "defaultConfiguration": {"level": "error"}},
                        {"id": "CG002", "name": "CG002",
                         "shortDescription": {"text": Rule::Cg002.description()},
                         "defaultConfiguration": {"level": "warning"}},
                    ],
                }},
                "results": [
                    {"ruleId": "CG001", "ruleIndex": 0, "level": "error",
                     "message": {"text": "npm script \"deploy\" not found in package.json"},
                     "locations": [{"physicalLocation": {
                         "artifactLocation": {"uri": "AGENTS.md"},
                         "region": {"startLine": 12}}}]},
                    {"ruleId": "CG002", "ruleIndex": 1, "level": "warning",
                     "message": {"text": "link target missing"},
                     "locations": [{"physicalLocation": {
                         "artifactLocation": {"uri": "docs/guide.md"}}}]},
                ],
            }],
        });
        assert_eq!(got, expected);
    }

    #[test]
    fn sarif_empty_findings_still_one_run() {
        let got: Value = serde_json::from_str(&sarif(&[], "1.2.3")).unwrap();
        assert_eq!(got["runs"][0]["results"], json!([]));
        assert_eq!(got["runs"][0]["tool"]["driver"]["rules"], json!([]));
        assert_eq!(got["runs"][0]["tool"]["driver"]["name"], "cedr");
    }

    #[test]
    fn gitlab_shape_and_fingerprint() {
        let findings = sample();
        let got: Value = serde_json::from_str(&gitlab(&findings)).unwrap();
        let arr = got.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["check_name"], "CG001");
        assert_eq!(arr[0]["severity"], "major");
        assert_eq!(arr[0]["location"]["path"], "AGENTS.md");
        assert_eq!(arr[0]["location"]["lines"]["begin"], 12);
        assert_eq!(arr[1]["severity"], "minor");
        // No line known: GitLab requires a begin line, so it defaults to 1.
        assert_eq!(arr[1]["location"]["lines"]["begin"], 1);
        for (entry, f) in arr.iter().zip(&findings) {
            assert_eq!(entry["fingerprint"].as_str().unwrap(), fingerprint(f));
            assert_eq!(entry["fingerprint"].as_str().unwrap().len(), 64);
        }
        // Same input, same fingerprint; a different message differs.
        let mut other = findings[0].clone();
        other.message.push('x');
        assert_ne!(fingerprint(&findings[0]), fingerprint(&other));
    }

    #[test]
    fn note_level_maps_to_sarif_note_and_github_notice() {
        let f = OutputFinding {
            rule_id: "P3".into(),
            rule_description: "Council review finding (P3)".into(),
            level: OutputLevel::Note,
            gitlab_severity: GitlabSeverity::Minor,
            message: "nit".into(),
            path: "src/lib.rs".into(),
            line: Some(3),
        };
        let doc: Value = serde_json::from_str(&sarif(std::slice::from_ref(&f), "1")).unwrap();
        assert_eq!(doc["runs"][0]["results"][0]["level"], "note");
        assert!(github(&[f]).starts_with("::notice "));
    }
}
