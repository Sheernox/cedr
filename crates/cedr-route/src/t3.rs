//! T3 Code's own view of the providers it runs, from the cache files it keeps for its UI
//! (docs/route.md: Built-in readers, Machine checks, Model options). Reading one runs nothing and
//! reads no credentials: only the fields below are kept, the rest of the file (sign-in email
//! included) is skipped while parsing. The format is T3 Code's own and undocumented, so every
//! field is optional and a value of another shape reads as missing.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::config::xdg_dir;

/// Where T3 Code keeps what it last read of each provider instance it runs: `$T3CODE_HOME/caches`
/// (default `~/.t3/caches`), one `<instance id>.json` each.
pub fn caches() -> PathBuf {
    xdg_dir("T3CODE_HOME", ".t3").join("caches")
}

/// A provider cache holds a provider's models, skills and commands; one past this is not one.
pub const MAX_BYTES: usize = 4 << 20;

/// T3 Code's machine view (sign-in, enabled, installed) older than this says nothing about now:
/// T3 Code refreshes it every few minutes while it runs.
pub const HEALTH_SECONDS: i64 = 3600;

/// Effort option ids T3 Code's providers use, for a harness whose routes name none.
const EFFORT_OPTIONS: [&str; 2] = ["effort", "reasoningEffort"];

/// One provider instance as T3 Code last saw it.
#[derive(Debug, Default, serde::Deserialize)]
pub struct Provider {
    #[serde(default)]
    pub driver: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    installed: Value,
    #[serde(rename = "checkedAt", default)]
    checked_at: Value,
    #[serde(default)]
    auth: Option<AuthField>,
    #[serde(default)]
    models: Value,
    #[serde(rename = "usageLimits", default)]
    pub usage: Value,
    #[serde(default)]
    skills: Value,
}

/// `auth` in any shape: an object keeps only `status` and `label` (never `email`), and anything
/// else (null, another type) leaves the sign-in unknown instead of discarding the cache.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum AuthField {
    Fields {
        #[serde(default)]
        status: Value,
        #[serde(default)]
        label: Value,
    },
    Other(serde::de::IgnoredAny),
}

impl Provider {
    fn auth(&self, field: &str) -> Option<&str> {
        match &self.auth {
            Some(AuthField::Fields { status, label }) => match field {
                "status" => status.as_str(),
                _ => label.as_str(),
            },
            _ => None,
        }
    }
}

fn yes() -> bool {
    true
}

/// An effort option a model offers: its option id and its values, in T3 Code's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effort {
    pub option: String,
    pub values: Vec<String>,
}

/// A skill T3 Code reports the provider loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub scope: String,
}

/// Whether `instance` can name a cache file: an instance id may come from shared routes, so it
/// names a file in the caches directory, never a path.
pub fn safe_instance(instance: &str) -> bool {
    !instance.is_empty()
        && !instance.starts_with('.')
        && instance
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// The cache for `instance` in `dir`. Ok(None) for an unsafe id or no cache; an error names the
/// file when it cannot be read or is not a provider cache.
pub fn read(dir: &Path, instance: &str) -> Result<Option<Provider>, String> {
    if !safe_instance(instance) {
        return Ok(None);
    }
    let path = dir.join(format!("{instance}.json"));
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let bytes = crate::readings::read_limited(file, MAX_BYTES)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() > MAX_BYTES {
        return Err(format!("{instance} cache exceeds {MAX_BYTES} bytes"));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|_| {
        format!("{instance} is not a T3 Code provider cache, or T3 Code changed its format")
    })
}

impl Provider {
    /// When T3 Code last checked the provider.
    pub fn checked_at(&self) -> Option<DateTime<Utc>> {
        crate::readings::parse_ts(&self.checked_at)
    }

    /// Whether this view is recent enough to say anything about the machine now. A time more than
    /// a few minutes ahead (a clock moved back, a corrupt cache) is not current either.
    pub fn current(&self, now: DateTime<Utc>) -> bool {
        self.checked_at().is_some_and(|at| {
            let age = (now - at).num_seconds();
            (-300..=HEALTH_SECONDS).contains(&age)
        })
    }

    /// T3 Code found the provider's program, or runs it itself.
    pub fn installed(&self) -> Option<bool> {
        self.installed.as_bool()
    }

    /// T3 Code says the provider is signed out. An unknown status (some providers report none)
    /// is not signed out.
    pub fn signed_out(&self) -> bool {
        self.auth("status") == Some("unauthenticated")
    }

    /// The subscription T3 Code names for the sign-in (`Claude Max`), from a label of the form
    /// `<plan> Subscription`; other labels describe the account, not a plan, and are skipped.
    pub fn plan(&self) -> Option<String> {
        let plan = self.auth("label")?.strip_suffix(" Subscription")?.trim();
        (!plan.is_empty() && plan.chars().count() <= 64 && !plan.contains('@'))
            .then(|| plan.to_string())
    }

    /// The model ids the provider offers.
    pub fn offered(&self) -> Vec<String> {
        self.models
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["slug"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Each model's effort option: the select option named `option`, or with none named, one of
    /// the ids T3 Code's providers use for effort. Models without one (effort in the model id, as
    /// Antigravity's) are left out.
    pub fn efforts(&self, option: Option<&str>) -> Vec<(String, Effort)> {
        let mut out = Vec::new();
        for m in self.models.as_array().into_iter().flatten() {
            let Some(slug) = m["slug"].as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            let descriptors = m["capabilities"]["optionDescriptors"].as_array();
            let found = descriptors.into_iter().flatten().find(|d| {
                d["id"].as_str().is_some_and(|id| match option {
                    Some(o) => id == o,
                    None => EFFORT_OPTIONS.contains(&id),
                })
            });
            let Some(d) = found else {
                continue;
            };
            let values: Vec<String> = d["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| o["id"].as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect();
            if values.is_empty() {
                continue;
            }
            out.push((
                slug.to_string(),
                Effort {
                    option: d["id"].as_str().unwrap_or_default().to_string(),
                    values,
                },
            ));
        }
        out
    }

    /// The enabled skills T3 Code reports the provider loads, with their scope (`user`, `system`).
    pub fn skills(&self) -> Vec<Skill> {
        self.skills
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["enabled"].as_bool() != Some(false))
            .filter_map(|s| {
                Some(Skill {
                    name: s["name"].as_str().filter(|n| !n.is_empty())?.to_string(),
                    scope: s["scope"].as_str().unwrap_or("").to_string(),
                })
            })
            .collect()
    }
}

/// The efforts Route orders, lowest first (docs/route.md, Model options).
pub const EFFORT_LADDER: [&str; 8] = [
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
    "ultra",
    "ultrathink",
];

/// The effort a model takes for `wanted`: `wanted` when offered, else the highest offered effort
/// below it, else the lowest offered. Efforts outside the known ladder match only exactly.
pub fn clamp_effort<'a>(wanted: &str, offered: &'a [String]) -> Option<&'a str> {
    if let Some(v) = offered.iter().find(|v| *v == wanted) {
        return Some(v);
    }
    let rank = |e: &str| EFFORT_LADDER.iter().position(|l| *l == e);
    let want = rank(wanted)?;
    let ranked = || offered.iter().filter_map(|v| Some((rank(v)?, v.as_str())));
    ranked()
        .filter(|(r, _)| *r < want)
        .max_by_key(|(r, _)| *r)
        .or_else(|| ranked().min_by_key(|(r, _)| *r))
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_effort_a_model_lacks_falls_to_the_nearest_it_offers() {
        let grok = v(&["low", "medium", "high", "xhigh"]);
        assert_eq!(clamp_effort("max", &grok), Some("xhigh"));
        assert_eq!(clamp_effort("high", &grok), Some("high"));
        assert_eq!(clamp_effort("minimal", &grok), Some("low"));
        let claude = v(&["low", "medium", "high", "xhigh", "max", "ultrathink"]);
        assert_eq!(clamp_effort("ultra", &claude), Some("max"));
        assert_eq!(clamp_effort("turbo", &claude), None);
    }

    #[test]
    fn a_cache_gives_efforts_plan_and_skills_but_never_the_email() {
        let p: Provider = serde_json::from_value(serde_json::json!({
            "driver": "codex", "installed": true, "checkedAt": "2026-10-29T01:59:00Z",
            "auth": {"status": "authenticated", "label": "ChatGPT Pro Subscription", "email": "a@example.invalid"},
            "models": [
                {"slug": "gpt-6.1-sol", "capabilities": {"optionDescriptors": [
                    {"id": "reasoningEffort", "options": [{"id": "low"}, {"id": "ultra"}]},
                    {"id": "serviceTier", "options": [{"id": "default"}]}]}},
                {"slug": "gemini-4-argon-high"}],
            "skills": [{"name": "council", "scope": "user"}, {"name": "off", "scope": "user", "enabled": false}]}))
        .unwrap();
        assert_eq!(
            p.efforts(None),
            [(
                "gpt-6.1-sol".to_string(),
                Effort {
                    option: "reasoningEffort".into(),
                    values: v(&["low", "ultra"])
                }
            )]
        );
        assert!(p.efforts(Some("effort")).is_empty());
        assert_eq!(p.plan().as_deref(), Some("ChatGPT Pro"));
        assert_eq!(
            p.skills(),
            [Skill {
                name: "council".into(),
                scope: "user".into()
            }]
        );
        assert!(!format!("{p:?}").contains("example.invalid"));
        let account: Provider =
            serde_json::from_value(serde_json::json!({"auth": {"label": "Google account"}}))
                .unwrap();
        assert_eq!(account.plan(), None);
        for auth in [serde_json::json!(null), serde_json::json!("x")] {
            let p: Provider = serde_json::from_value(serde_json::json!({"auth": auth})).unwrap();
            assert!(!p.signed_out() && p.plan().is_none());
        }
    }
}
