//! Plan-window projection uses elapsed wall time, never a fabricated usage rate.
use crate::{Result, store::Store};
use chrono::Utc;
use serde_json::{Value, json};
pub fn project(mut v: Value, now: i64) -> Value {
    let used = v["percent"].as_f64();
    let reset = v["resetsAt"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.timestamp());
    let minutes = v["windowMinutes"].as_f64();
    let elapsed = reset
        .zip(minutes)
        .filter(|(_, m)| *m > 0.0)
        .map(|(r, m)| ((now as f64 - r as f64 + m * 60.0) / (m * 60.0)).clamp(0.0, 1.0));
    let projection = used
        .zip(elapsed)
        .filter(|(u, e)| *u > 0.0 && *e >= 0.03 && reset.is_some_and(|r| r > now))
        .map(|(u, e)| u / e);
    let outlook = match (used, elapsed, projection) {
        (Some(u), _, _) if u >= 100.0 => "at limit",
        (Some(0.0), _, _) => "idle",
        (_, _, Some(p)) if p >= 100.0 => "limit risk",
        (_, _, Some(p)) if p >= 70.0 => "tight",
        (_, _, Some(_)) => "on track",
        (Some(_), Some(e), _) if e < 0.03 => "early",
        _ => "unknown",
    };
    let runout = used
        .zip(elapsed)
        .zip(minutes)
        .zip(reset)
        .filter(|(((u, e), _), r)| *u > 0.0 && *e >= 0.03 && *r > now)
        .map(|(((u, e), m), r)| {
            crate::report::stamp((r as f64 - m * 60.0 + m * 60.0 * e * 100.0 / u) as i64)
        });
    v["elapsedShare"] = json!(elapsed);
    v["projectedPercent"] = json!(projection);
    v["runsOutAt"] = json!(runout);
    v["outlook"] = json!(outlook);
    v
}
pub fn refresh(db: &Store) -> Vec<String> {
    let mut errors = vec![];
    let path = cedr_route::readers::statusline_path("claude");
    if let Ok(b) = std::fs::read(path) {
        match serde_json::from_slice::<Value>(&b) {
            Ok(v) => {
                if db.readings(&v).is_err() {
                    errors.push("cannot save Claude readings".into());
                }
            }
            Err(_) => errors.push("invalid Claude capture".into()),
        }
    }
    for (h, command, reader) in [
        (
            "codex",
            "codex",
            cedr_route::readers::codex
                as fn(&str, std::time::Duration) -> std::result::Result<Value, String>,
        ),
        ("muse", "muse", cedr_route::readers::muse),
    ] {
        let installed = std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|p| p.join(command).is_file()));
        if !installed {
            continue;
        }
        match reader(h, std::time::Duration::from_secs(10)) {
            Ok(v) => {
                if db.readings(&v).is_err() {
                    errors.push(format!("cannot store {h} readings"));
                }
            }
            Err(_) => errors.push(format!("{h} readings unavailable")),
        }
    }
    errors
}
pub fn report(db: &Store) -> Result<Value> {
    let mut q=db.db.prepare("SELECT data FROM readings r WHERE at=(SELECT max(at) FROM readings x WHERE x.harness=r.harness AND x.window=r.window) ORDER BY harness,window")?;
    let rows = q
        .query_map([], |r| r.get::<_, String>(0))?
        .map(|r| Ok(project(serde_json::from_str(&r?)?, Utc::now().timestamp())))
        .collect::<Result<Vec<_>>>()?;
    Ok(json!(rows))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundaries() {
        let v = json!({"percent":50,"resetsAt":"1970-01-01T01:40:00Z","windowMinutes":100});
        assert_eq!(project(v.clone(), 100)["outlook"], "early");
        assert_eq!(project(v.clone(), 3000)["outlook"], "limit risk");
        assert_eq!(project(v, 4500)["outlook"], "on track");
    }
}
