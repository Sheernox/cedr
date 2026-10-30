//! Pool state per harness and model, and family-name resolution (docs/route.md, Pool state and
//! Model resolution).

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::config::Config;
use crate::readings::{Readings, WindowKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Ready,
    Unknown,
    Conserve,
    Out,
    /// Not on this machine (docs/route.md, Machine checks); overrides every reading.
    Absent,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Ready => "ready",
            State::Unknown => "unknown",
            State::Conserve => "conserve",
            State::Out => "out",
            State::Absent => "absent",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Pool {
    pub state: State,
    pub summary: String,
    /// The highest percent used across usable windows; none when the state is unknown.
    pub usage: Option<f64>,
}

/// Rounds like Python's `round()` (ties to even), so tags match pickers written in Python exactly.
pub fn round_pct(value: f64) -> i64 {
    value.round_ties_even() as i64
}

/// A model id split into family words and version numbers: "gpt-6.1-sol" -> (["gpt", "sol"], [6, 1]).
fn parts(id: &str) -> (Vec<&str>, Vec<u64>) {
    let mut words = Vec::new();
    let mut version = Vec::new();
    for token in id.split('-') {
        let numeric = !token.is_empty()
            && token
                .split('.')
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if numeric {
            version.extend(token.split('.').map(|n| n.parse::<u64>().unwrap_or(0)));
        } else {
            words.push(token);
        }
    }
    (words, version)
}

/// The model id for a target's model part: an offered id is kept as named; otherwise `name` is a
/// family whose words end the id's words, and the newest version wins. None without a model list,
/// with no match, or when the name matches more than one family.
pub fn resolve(harness: &str, name: &str, readings: &Readings) -> Option<String> {
    versions(harness, name, readings)?.into_iter().next()
}

/// Every offered version `name` stands for, newest first: just the id when it is offered as named,
/// else its family's versions. None as for `resolve`.
pub fn versions(harness: &str, name: &str, readings: &Readings) -> Option<Vec<String>> {
    let offered = readings.models.get(harness)?;
    if offered.iter().any(|m| m == name) {
        return Some(vec![name.to_string()]);
    }
    let want: Vec<&str> = name.split('-').collect();
    let mut matches: Vec<(Vec<&str>, Vec<u64>, &String)> = offered
        .iter()
        .filter_map(|m| {
            let (words, version) = parts(m);
            (words.len() >= want.len() && words[words.len() - want.len()..] == want[..])
                .then_some((words, version, m))
        })
        .collect();
    let families: BTreeSet<&Vec<&str>> = matches.iter().map(|(w, _, _)| w).collect();
    if families.len() != 1 {
        return None;
    }
    // A stable sort keeps the first of equal versions ahead ("gpt-6.1-sol" and "gpt-6-1-sol"
    // parse alike).
    matches.sort_by(|a, b| b.1.cmp(&a.1));
    Some(matches.into_iter().map(|(_, _, m)| m.clone()).collect())
}

/// The state of every window that applies to `model`, a short summary for the route tag, and the
/// pool's use. Plans are there to be used, so the pick follows current use, not pace.
pub fn pool(
    harness: &str,
    model: &str,
    readings: &Readings,
    config: &Config,
    now: DateTime<Utc>,
) -> Pool {
    if let Some(summary) = readings.absent.get(harness) {
        return Pool {
            state: State::Absent,
            summary: summary.clone(),
            usage: None,
        };
    }
    // A key billed per token has no limit windows: always ready, with no use to balance on.
    if config
        .harnesses
        .get(harness)
        .is_some_and(|h| h.billing == crate::config::Billing::Api)
    {
        return Pool {
            state: State::Ready,
            summary: format!("{harness} api key"),
            usage: None,
        };
    }
    let t = &config.thresholds;
    let unknown = |summary: String| Pool {
        state: State::Unknown,
        summary,
        usage: None,
    };
    let families: Vec<(&str, &str)> = config
        .model_windows
        .iter()
        .filter(|(_, w)| w.harness == harness)
        .map(|(id, w)| (id.as_str(), w.model.as_str()))
        .collect();
    let Some(entry) = readings.entries.get(harness) else {
        return unknown(format!("{harness} no reading"));
    };
    if entry.error {
        return unknown(format!("{harness} probe error"));
    }
    match entry.fetched_at {
        Some(at) if (now - at).num_milliseconds() as f64 / 1000.0 <= t.fresh_seconds as f64 => {}
        _ => return unknown(format!("{harness} stale")),
    }
    let mut expect: BTreeSet<&str> = config
        .harnesses
        .get(harness)
        .map(|h| h.expect.iter().map(String::as_str).collect())
        .unwrap_or_default();
    expect.extend(
        families
            .iter()
            .filter(|(_, f)| model.contains(f))
            .map(|(w, _)| *w),
    );
    let mut state = State::Ready;
    let mut shown = Vec::new();
    let mut seen = BTreeSet::new();
    let mut usage: f64 = 0.0;
    for w in &entry.windows {
        let family = families.iter().find(|(id, _)| *id == w.id).map(|(_, f)| *f);
        if let Some(f) = family
            && !model.contains(f)
        {
            continue;
        }
        seen.insert(w.id.as_str());
        let session = w.kind == WindowKind::Session;
        let here = match (w.percent, w.resets_at) {
            (None, _) => State::Unknown,
            (Some(_), Some(reset)) if reset <= now => State::Unknown,
            (Some(p), _) if p >= t.out => State::Out,
            (Some(p), _)
                if p >= if session {
                    t.session_conserve
                } else {
                    t.weekly_conserve
                } =>
            {
                State::Conserve
            }
            _ => State::Ready,
        };
        state = state.max(here);
        // A window past its reset still shows the previous cycle's use, so it counts as unknown.
        let pct = match (here, w.percent) {
            (State::Unknown, _) | (_, None) => "?".to_string(),
            (_, Some(p)) => {
                usage = usage.max(p);
                format!("{}", round_pct(p))
            }
        };
        let label = family.unwrap_or(if session { "s" } else { "w" });
        shown.push(format!("{label}{pct}%"));
    }
    let missing: Vec<&str> = expect.difference(&seen).copied().collect();
    if !missing.is_empty() {
        shown.push(format!("missing {}", missing.join(",")));
        state = state.max(State::Unknown);
    }
    let body = if shown.is_empty() {
        "no windows".to_string()
    } else {
        shown.join(" ")
    };
    Pool {
        state,
        summary: format!("{harness} {body}"),
        usage: (state != State::Unknown).then_some(usage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readings::Readings;
    use serde_json::json;
    use std::path::Path;

    const NOW: &str = "2026-10-29T02:00:00Z";

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(NOW)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn config() -> Config {
        Config::parse(
            r#"version = 1
[harnesses.claude]
expect = ["five_hour", "seven_day"]
[harnesses.codex]
expect = ["primary"]
[model_windows.seven_day_fable]
harness = "claude"
model = "fable"
"#,
            Path::new("t.toml"),
        )
        .unwrap()
    }

    fn readings(doc: serde_json::Value) -> Readings {
        let mut r = Readings::default();
        r.merge(&config(), &doc);
        r
    }

    fn claude(five: f64, week: f64, fable: f64) -> Readings {
        readings(
            json!({"harnesses": [{"harness": "claude", "fetchedAt": "2026-10-29T01:59:00Z", "windows": [
                {"id": "five_hour", "kind": "session", "percent": five, "resetsAt": "2026-10-29T05:00:00Z"},
                {"id": "seven_day", "kind": "weekly", "percent": week, "resetsAt": "2026-11-02T00:00:00Z"},
                {"id": "seven_day_fable", "kind": "weekly", "percent": fable, "resetsAt": "2026-11-02T00:00:00Z"}
            ]}]}),
        )
    }

    #[test]
    fn model_windows_apply_only_to_their_models() {
        let r = claude(10.0, 20.0, 95.0);
        let opus = pool("claude", "claude-opus-5-5", &r, &config(), now());
        assert_eq!(
            (opus.state, opus.summary.as_str(), opus.usage),
            (State::Ready, "claude s10% w20%", Some(20.0))
        );
        let fable = pool("claude", "claude-fable-5-1", &r, &config(), now());
        assert_eq!(
            (fable.state, fable.summary.as_str()),
            (State::Out, "claude s10% w20% fable95%")
        );
    }

    #[test]
    fn session_and_weekly_thresholds_set_conserve_and_out() {
        assert_eq!(
            pool("claude", "opus", &claude(86.0, 1.0, 0.0), &config(), now()).state,
            State::Conserve
        );
        assert_eq!(
            pool("claude", "opus", &claude(1.0, 90.0, 0.0), &config(), now()).state,
            State::Out
        );
    }

    #[test]
    fn missing_stale_errored_and_reset_windows_are_unknown_never_unused() {
        let missing = readings(
            json!({"harnesses": [{"harness": "codex", "fetchedAt": "2026-10-29T01:59:00Z", "windows": []}]}),
        );
        let p = pool("codex", "gpt-6.1-sol", &missing, &config(), now());
        assert_eq!(
            (p.state, p.summary.as_str(), p.usage),
            (State::Unknown, "codex missing primary", None)
        );

        let stale = readings(
            json!({"harnesses": [{"harness": "codex", "fetchedAt": "2026-10-29T01:40:00Z", "windows": []}]}),
        );
        assert_eq!(
            pool("codex", "x", &stale, &config(), now()).summary,
            "codex stale"
        );

        let errored = readings(
            json!({"harnesses": [{"harness": "codex", "fetchedAt": "2026-10-29T01:59:00Z", "error": "401"}]}),
        );
        assert_eq!(
            pool("codex", "x", &errored, &config(), now()).summary,
            "codex probe error"
        );

        let reset = readings(
            json!({"harnesses": [{"harness": "codex", "fetchedAt": "2026-10-29T01:59:00Z", "windows": [
            {"id": "primary", "kind": "weekly", "percent": 99, "resetsAt": "2026-10-29T01:00:00Z"}]}]}),
        );
        let p = pool("codex", "x", &reset, &config(), now());
        assert_eq!(
            (p.state, p.summary.as_str(), p.usage),
            (State::Unknown, "codex w?%", None)
        );

        assert_eq!(
            pool("codex", "x", &Readings::default(), &config(), now()).summary,
            "codex no reading"
        );
    }

    #[test]
    fn percentages_round_ties_to_even_like_python() {
        assert_eq!(
            (round_pct(4.5), round_pct(5.5), round_pct(57.85)),
            (4, 6, 58)
        );
    }

    #[test]
    fn resolves_families_to_the_newest_version() {
        let r = readings(
            json!({"models": {"codex": ["gpt-6.1-sol", "gpt-6-sol", "gpt-5.6-sol", "gpt-6-luna"],
                                            "claude": ["claude-sonnet-5-5", "claude-sonnet-5", "claude-opus-5-5"]}}),
        );
        assert_eq!(resolve("codex", "sol", &r).as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(
            resolve("codex", "gpt-6-sol", &r).as_deref(),
            Some("gpt-6-sol")
        );
        assert_eq!(
            resolve("claude", "sonnet", &r).as_deref(),
            Some("claude-sonnet-5-5")
        );
        assert_eq!(resolve("codex", "astra", &r), None);
        let tie = readings(json!({"models": {"codex": ["gpt-6.1-sol", "gpt-6-1-sol"]}}));
        assert_eq!(
            resolve("codex", "sol", &tie).as_deref(),
            Some("gpt-6.1-sol")
        );
        let two = readings(json!({"models": {"codex": ["gpt-6-sol", "gpt-6-mini-sol"]}}));
        assert_eq!(
            resolve("codex", "sol", &two),
            None,
            "a name matching two families is ambiguous"
        );
        assert_eq!(resolve("muse", "wonder", &r), None, "no model list");
    }
}
