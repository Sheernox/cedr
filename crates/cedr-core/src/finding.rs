//! The finding shape shared by `cedr lint` and `cedr verify`.

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

/// Lint rules (CG) and verify checks (CV), named as in the design contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
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
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
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
fn github_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// A workflow command property value: the message escapes plus `:` and `,`.
fn github_property(s: &str) -> String {
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
