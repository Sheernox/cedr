//! Pool state per harness and model, and family-name resolution (docs/route.md, Pool state and
//! Model resolution).

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::config::{Config, Limits};
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

/// The family of a window the config does not name: the one word of its id that some, but not
/// all, of the harness's models carry (`seven_day_fable` with `claude-opus-5-5` and
/// `claude-fable-5-1` is `fable`). Feeds name model-only windows this way, and without it such a
/// window would count against every model and read as a second weekly window.
fn inferred_family<'a>(id: &'a str, offered: &[String]) -> Option<&'a str> {
    let words: Vec<Vec<&str>> = offered.iter().map(|m| parts(m).0).collect();
    let mut found = id.split(['_', '-']).filter(|word| {
        let carry = words.iter().filter(|w| w.contains(word)).count();
        carry > 0 && carry < words.len()
    });
    let family = found.next()?;
    found.all(|other| other == family).then_some(family)
}

/// Whether `model` belongs to `family`: the id contains it, and a family ending in a version
/// ("opus-4") is not just the start of a longer one ("claude-opus-4-5"). A date suffix of eight
/// digits or more ("claude-opus-4-5-20250929") is the same version, not a longer one.
pub fn in_family(model: &str, family: &str) -> bool {
    if !family.ends_with(|c: char| c.is_ascii_digit()) {
        return model.contains(family);
    }
    model.match_indices(family).any(|(at, _)| {
        let rest = &model[at + family.len()..];
        if rest.starts_with(|c: char| c.is_ascii_digit()) {
            return false;
        }
        let Some(next) = rest.strip_prefix(['-', '.']) else {
            return true;
        };
        let digits = next.chars().take_while(char::is_ascii_digit).count();
        digits == 0 || digits >= 8
    })
}

/// Whether a window limited to `family` applies to a pick of `model`: `model` is in the family, or
/// `model` is a bare family name that no model list resolved ("opus") and `family` one of its
/// versions ("opus-4-5"), since the pick may run any of them.
fn applies(model: &str, family: &str) -> bool {
    in_family(model, family)
        || (!model.is_empty()
            && !model.ends_with(|c: char| c.is_ascii_digit())
            && family
                .strip_prefix(model)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit())))
}

/// A harness's model windows as `(window id, family)`: those `[model_windows]` names, then those
/// its reading limits to a `model` itself, then those `inferred_family` places.
pub fn model_windows<'a>(
    harness: &str,
    readings: &'a Readings,
    config: &'a Config,
) -> Vec<(&'a str, &'a str)> {
    let mut families: Vec<(&str, &str)> = config
        .model_windows
        .iter()
        .filter(|(_, w)| w.harness == harness)
        .map(|(id, w)| (id.as_str(), w.model.as_str()))
        .collect();
    let Some(entry) = readings.entries.get(harness) else {
        return families;
    };
    let expect = config
        .harnesses
        .get(harness)
        .map(|h| h.expect.as_slice())
        .unwrap_or_default();
    let offered = readings
        .models
        .get(harness)
        .map(Vec::as_slice)
        .unwrap_or_default();
    // A source that scopes any window with `model` has said which are scoped; the rest are shared.
    let scoped_by_source = entry.windows.iter().any(|w| w.model.is_some());
    for w in &entry.windows {
        if expect.contains(&w.id) || families.iter().any(|(id, _)| *id == w.id) {
            continue;
        }
        let inferred = || {
            (!scoped_by_source)
                .then(|| inferred_family(&w.id, offered))
                .flatten()
        };
        if let Some(f) = w.model.as_deref().or_else(inferred) {
            families.push((w.id.as_str(), f));
        }
    }
    families
}

/// A plan's pool under its own thresholds, for answers about the plan rather than one route.
pub fn pool(
    harness: &str,
    model: &str,
    readings: &Readings,
    config: &Config,
    now: DateTime<Utc>,
) -> Pool {
    let limits = config.limits(Some(harness), None);
    pool_with(harness, model, readings, config, now, &limits)
}

/// The state of every window that applies to `model` under `t`, a short summary for the route tag,
/// and the pool's use. Plans are there to be used, so the pick follows current use, not pace.
pub fn pool_with(
    harness: &str,
    model: &str,
    readings: &Readings,
    config: &Config,
    now: DateTime<Utc>,
    t: &Limits,
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
    let unknown = |summary: String| Pool {
        state: State::Unknown,
        summary,
        usage: None,
    };
    let Some(entry) = readings.entries.get(harness) else {
        return unknown(format!("{harness} no reading"));
    };
    let mut expect: BTreeSet<&str> = config
        .harnesses
        .get(harness)
        .map(|h| h.expect.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let families = model_windows(harness, readings, config);
    if entry.error {
        return unknown(format!("{harness} probe error"));
    }
    match entry.fetched_at {
        Some(at) if (now - at).num_milliseconds() as f64 / 1000.0 <= t.fresh_seconds as f64 => {}
        _ => return unknown(format!("{harness} stale")),
    }
    expect.extend(
        families
            .iter()
            .filter(|(_, f)| applies(model, f))
            .map(|(w, _)| *w),
    );
    let mut state = State::Ready;
    let mut shown = Vec::new();
    let mut seen = BTreeSet::new();
    let mut usage: f64 = 0.0;
    for w in &entry.windows {
        let family = families.iter().find(|(id, _)| *id == w.id).map(|(_, f)| *f);
        if let Some(f) = family
            && !applies(model, f)
        {
            continue;
        }
        seen.insert(w.id.as_str());
        let session = w.kind == WindowKind::Session;
        // A window read earlier than its entry (carried over from an older reading) ages on its own.
        let stale = w.fetched_at.is_some_and(|at| {
            (now - at).num_milliseconds() as f64 / 1000.0 > t.fresh_seconds as f64
        });
        let here = match (w.percent, w.resets_at) {
            _ if stale => State::Unknown,
            (None, _) => State::Unknown,
            (Some(_), Some(reset)) if reset <= now => State::Unknown,
            (Some(p), _) if p >= if session { t.session_out } else { t.weekly_out } => State::Out,
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
        // A family ending in a digit ("opus-4-5") would run into its percent.
        let sep = if label.ends_with(|c: char| c.is_ascii_digit()) {
            ":"
        } else {
            ""
        };
        shown.push(format!("{label}{sep}{pct}%"));
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
    fn a_model_window_the_config_does_not_name_is_labelled_and_applied_by_its_family() {
        let config = Config::parse(
            "version = 1\n[harnesses.claude]\nexpect = [\"five_hour\", \"seven_day\"]\n",
            Path::new("t.toml"),
        )
        .unwrap();
        let mut r = Readings::default();
        r.merge(
            &config,
            &json!({"harnesses": [{"harness": "claude", "fetchedAt": "2026-10-29T01:59:00Z", "windows": [
                {"id": "five_hour", "kind": "session", "percent": 39, "resetsAt": "2026-10-29T05:00:00Z"},
                {"id": "seven_day", "kind": "weekly", "percent": 45, "resetsAt": "2026-11-02T00:00:00Z"},
                {"id": "seven_day_fable", "kind": "weekly", "percent": 95, "resetsAt": "2026-11-02T00:00:00Z"}
            ]}], "models": {"claude": ["claude-opus-5-5", "claude-fable-5-1"]}}),
        );
        let opus = pool("claude", "claude-opus-5-5", &r, &config, now());
        assert_eq!(
            (opus.state, opus.summary.as_str()),
            (State::Ready, "claude s39% w45%")
        );
        let fable = pool("claude", "claude-fable-5-1", &r, &config, now());
        assert_eq!(
            (fable.state, fable.summary.as_str()),
            (State::Out, "claude s39% w45% fable95%")
        );
    }

    #[test]
    fn a_window_its_source_limits_to_a_model_applies_only_to_that_family_without_a_model_list() {
        let config = Config::parse(
            "version = 1\n[harnesses.claude]\nexpect = [\"five_hour\", \"seven_day\"]\n",
            Path::new("t.toml"),
        )
        .unwrap();
        let mut r = Readings::default();
        r.merge(
            &config,
            &json!({"harnesses": [{"harness": "claude", "fetchedAt": "2026-10-29T01:59:00Z", "windows": [
                {"id": "five_hour", "kind": "session", "percent": 39, "resetsAt": "2026-10-29T05:00:00Z"},
                {"id": "seven_day", "kind": "weekly", "percent": 45, "resetsAt": "2026-11-02T00:00:00Z"},
                {"id": "seven_day_fable", "kind": "weekly", "percent": 95, "resetsAt": "2026-11-02T00:00:00Z", "model": "fable"}
            ]}]}),
        );
        let opus = pool("claude", "claude-opus-5-5", &r, &config, now());
        assert_eq!(
            (opus.state, opus.summary.as_str()),
            (State::Ready, "claude s39% w45%")
        );
        assert_eq!(
            pool("claude", "claude-fable-5-1", &r, &config, now()).summary,
            "claude s39% w45% fable95%"
        );
    }

    #[test]
    fn a_family_name_without_a_model_list_takes_the_limits_of_its_versions() {
        let config = Config::parse(
            "version = 1\n[harnesses.claude]\nexpect = [\"five_hour\", \"seven_day\"]\n",
            Path::new("t.toml"),
        )
        .unwrap();
        let mut r = Readings::default();
        r.merge(
            &config,
            &json!({"harnesses": [{"harness": "claude", "fetchedAt": "2026-10-29T01:59:00Z", "windows": [
                {"id": "five_hour", "kind": "session", "percent": 39, "resetsAt": "2026-10-29T05:00:00Z"},
                {"id": "seven_day", "kind": "weekly", "percent": 45, "resetsAt": "2026-11-02T00:00:00Z"},
                {"id": "seven_day_opus_4_5", "kind": "weekly", "percent": 100, "resetsAt": "2026-11-02T00:00:00Z", "model": "opus-4-5"}
            ]}]}),
        );
        let opus = pool("claude", "opus", &r, &config, now());
        assert_eq!(
            (opus.state, opus.summary.as_str()),
            (State::Out, "claude s39% w45% opus-4-5:100%")
        );
        // Another family, and another version of this one, keep only the shared windows.
        for model in ["sonnet", "opus-4", "opus-4-6"] {
            assert_eq!(
                pool("claude", model, &r, &config, now()).summary,
                "claude s39% w45%",
                "{model}"
            );
        }
    }

    #[test]
    fn a_window_read_earlier_than_its_entry_ages_under_the_picks_own_threshold() {
        let r = readings(
            json!({"harnesses": [{"harness": "claude", "fetchedAt": "2026-10-29T01:59:00Z", "windows": [
                {"id": "five_hour", "kind": "session", "percent": 10, "resetsAt": "2026-10-29T05:00:00Z"},
                {"id": "seven_day", "kind": "weekly", "percent": 20, "resetsAt": "2026-11-02T00:00:00Z"},
                {"id": "seven_day_fable", "kind": "weekly", "percent": 30, "resetsAt": "2026-11-02T00:00:00Z",
                 "fetchedAt": "2026-10-29T01:55:00Z"}
            ]}]}),
        );
        let c = config();
        let mut t = c.limits(Some("claude"), None);
        let fable = |t: &Limits| pool_with("claude", "claude-fable-5-1", &r, &c, now(), t).summary;
        assert_eq!(fable(&t), "claude s10% w20% fable30%");
        t.fresh_seconds = 60;
        assert_eq!(fable(&t), "claude s10% w20% fable?%");
    }

    #[test]
    fn a_source_that_scopes_one_window_leaves_the_others_shared() {
        let config = Config::parse(
            "version = 1\n[harnesses.antigravity]\nexpect = []\n",
            Path::new("t.toml"),
        )
        .unwrap();
        let mut r = Readings::default();
        r.merge(
            &config,
            &json!({"harnesses": [{"harness": "antigravity", "fetchedAt": "2026-10-29T01:59:00Z", "windows": [
                {"id": "gemini-5h@acct", "kind": "session", "percent": 0, "resetsAt": "2026-10-29T05:00:00Z"},
                {"id": "gemini-weekly@acct", "kind": "weekly", "percent": 17, "resetsAt": "2026-11-02T00:00:00Z"},
                {"id": "gemini-argon-wtu-weekly@acct", "kind": "weekly", "percent": 93,
                 "resetsAt": "2026-11-02T00:00:00Z", "model": "argon"}
            ]}], "models": {"antigravity": [
                "gemini-4-argon-high", "gemini-4-argon-medium", "gemini-4-argon-low",
                "gemini-3.8-flash-high", "gemini-3.8-flash-medium", "gemini-3.8-flash-low",
                "claude-opus-5-5-low", "claude-opus-5-5-medium"]}}),
        );
        let p = |m: &str| {
            let p = pool("antigravity", m, &r, &config, now());
            (p.state, p.summary)
        };
        let shared = (State::Ready, "antigravity s0% w17%".to_string());
        assert_eq!(p("gemini-3.8-flash-high"), shared);
        assert_eq!(p("claude-opus-5-5-low"), shared);
        assert_eq!(
            p("gemini-4-argon-high"),
            (State::Out, "antigravity s0% w17% argon93%".to_string())
        );
    }

    #[test]
    fn a_versioned_family_does_not_match_a_longer_version() {
        assert!(in_family("claude-opus-4", "opus-4"));
        assert!(in_family("claude-opus-4-5", "opus-4-5"));
        assert!(!in_family("claude-opus-4-5", "opus-4"));
        assert!(!in_family("claude-opus-45", "opus-4"));
        assert!(in_family("claude-fable-5-1", "fable"));
        assert!(in_family("claude-opus-4-5-20250929", "opus-4-5"));
        assert!(!in_family("claude-opus-4-55", "opus-4-5"));
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
