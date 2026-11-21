//! The finding shape shared by `cedr guard lint` and `cedr guard verify`.

use serde::{Serialize, Serializer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// Guard lint (CG), verification (CV), and Sync (CS) finding codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    Cs001,
    Cs002,
    Cs003,
    Cs004,
    Cs005,
    Cs101,
    Cs102,
    Cs103,
    Cs104,
    Cs105,

    Cg001,
    Cg002,
    Cg003,
    Cg004,
    Cg005,
    Cg006,
    Cg007,
    Cg008,
    Cg009,
    Cg010,
    Cv001,
    Cv002,
    Cv003,
    Cv004,
    Cv005,
    Cv006,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::Cs001 => "CS001",
            Rule::Cs002 => "CS002",
            Rule::Cs003 => "CS003",
            Rule::Cs004 => "CS004",
            Rule::Cs005 => "CS005",
            Rule::Cs101 => "CS101",
            Rule::Cs102 => "CS102",
            Rule::Cs103 => "CS103",
            Rule::Cs104 => "CS104",
            Rule::Cs105 => "CS105",
            Rule::Cg001 => "CG001",
            Rule::Cg002 => "CG002",
            Rule::Cg003 => "CG003",
            Rule::Cg004 => "CG004",
            Rule::Cg005 => "CG005",
            Rule::Cg006 => "CG006",
            Rule::Cg007 => "CG007",
            Rule::Cg008 => "CG008",
            Rule::Cg009 => "CG009",
            Rule::Cg010 => "CG010",
            Rule::Cv001 => "CV001",
            Rule::Cv002 => "CV002",
            Rule::Cv003 => "CV003",
            Rule::Cv004 => "CV004",
            Rule::Cv005 => "CV005",
            Rule::Cv006 => "CV006",
        }
    }

    /// Short description for SARIF rule metadata; the full definitions are in the contracts.
    pub fn description(self) -> &'static str {
        match self {
            Rule::Cs001 => "Generated file differs from what cedr sync would write",
            Rule::Cs002 => "Generated file was edited by hand",
            Rule::Cs003 => "Generated file is missing",
            Rule::Cs004 => "Invalid Sync source",
            Rule::Cs005 => "Sync could not read or compare the Guard lock or MCP config",
            Rule::Cs101 => "MCP server matches a banned server pattern",
            Rule::Cs102 => "Unrestricted shell allowed while the organization forbids it",
            Rule::Cs103 => "Unpinned MCP server while the organization requires pinned servers",
            Rule::Cs104 => "Required organization skill is missing or was changed",
            Rule::Cs105 => "Required target is not configured",
            Rule::Cg001 => "Instruction file references a missing npm script",
            Rule::Cg002 => "Relative markdown link target does not exist",
            Rule::Cg003 => "CLAUDE.md or GEMINI.md differs from AGENTS.md",
            Rule::Cg004 => "SKILL.md frontmatter is missing or invalid",
            Rule::Cg005 => "Hidden Unicode in an instruction or skill file",
            Rule::Cg006 => "Unpinned MCP server",
            Rule::Cg007 => "Broad agent permissions",
            Rule::Cg008 => "Remote MCP server over plain http",
            Rule::Cg009 => "Suspicious pattern matched in a skill",
            Rule::Cg010 => "Skill directory links outside the repo root",
            Rule::Cv001 => "Pinned content changed",
            Rule::Cv002 => "Skill or MCP server added since the pin",
            Rule::Cv003 => "Skill or MCP server removed since the pin",
            Rule::Cv004 => "MCP server source changed since the pin",
            Rule::Cv005 => "MCP server launch changed since the pin",
            Rule::Cv006 => "Legacy untyped skill pin",
        }
    }
}

/// Lint rules, in order; `.cedr/guard.toml` may name only these.
pub const LINT_RULES: [Rule; 10] = [
    Rule::Cg001,
    Rule::Cg002,
    Rule::Cg003,
    Rule::Cg004,
    Rule::Cg005,
    Rule::Cg006,
    Rule::Cg007,
    Rule::Cg008,
    Rule::Cg009,
    Rule::Cg010,
];

impl Rule {
    /// The lint rule with this id (`CG001`-`CG010`); verify checks are never named in config.
    pub fn lint(id: &str) -> Option<Rule> {
        LINT_RULES.into_iter().find(|r| r.as_str() == id)
    }
}

impl Serialize for Rule {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// One problem at a path relative to the scanned root (`/` separators) and an
/// optional 1-based line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub rule: Rule,
    pub severity: Severity,
    pub path: String,
    pub line: Option<usize>,
    pub message: String,
}

impl Finding {
    pub fn new(
        rule: Rule,
        severity: Severity,
        path: impl Into<String>,
        line: Option<usize>,
        message: impl Into<String>,
    ) -> Self {
        Finding {
            rule,
            severity,
            path: path.into(),
            line,
            message: message.into(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub version: u32,
    pub findings: Vec<Finding>,
}

impl Report {
    /// Builds a report with findings in a stable order: path, line, rule, message.
    pub fn new(mut findings: Vec<Finding>) -> Self {
        findings.sort_by(|a, b| {
            (&a.path, a.line, a.rule, &a.message).cmp(&(&b.path, b.line, b.rule, &b.message))
        });
        findings.dedup();
        Report {
            version: 1,
            findings,
        }
    }

    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("findings serialize")
    }
    /// SARIF 2.1.0 for code-scanning uploads; see [`crate::report`].
    pub fn to_sarif(&self) -> String {
        crate::report::sarif(&crate::report::from_report(self), env!("CARGO_PKG_VERSION"))
    }

    /// GitLab Code Quality JSON; see [`crate::report`].
    pub fn to_gitlab(&self) -> String {
        crate::report::gitlab(&crate::report::from_report(self))
    }

    /// One finding per line: `path:line rule severity message`.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for f in &self.findings {
            let location = match f.line {
                Some(line) => format!("{}:{line}", f.path),
                None => f.path.clone(),
            };
            out.push_str(&format!(
                "{location} {} {} {}\n",
                f.rule.as_str(),
                f.severity.as_str(),
                f.message
            ));
        }
        out
    }

    /// GitHub Actions workflow commands, one per finding, so a pull request shows them as
    /// annotations, then a `cedr: <n> errors, <m> warnings` summary line.
    pub fn to_github(&self) -> String {
        let mut out = String::new();
        for f in &self.findings {
            let mut props = format!("file={}", github_property(&f.path));
            if let Some(line) = f.line {
                props.push_str(&format!(",line={line}"));
            }
            props.push_str(&format!(",title={}", f.rule.as_str()));
            out.push_str(&format!(
                "::{} {props}::{}\n",
                f.severity.as_str(),
                github_data(&f.message)
            ));
        }
        let errors = self
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .count();
        out.push_str(&format!(
            "cedr: {errors} errors, {} warnings\n",
            self.findings.len() - errors
        ));
        out
    }
}

/// A workflow command's message, escaped as the runner expects.
pub fn github_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// A workflow command property value: the message escapes plus `:` and `,`.
pub fn github_property(s: &str) -> String {
    github_data(s).replace(':', "%3A").replace(',', "%2C")
}

/// 1-based line number of a byte offset.
pub(crate) fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}
