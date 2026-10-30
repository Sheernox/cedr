//! SQL hourly aggregation and report calculations over nullable counters.
use crate::{Result, pricing, store::Store};
use chrono::{DateTime, Local, TimeZone, Utc};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
pub fn stamp(t: i64) -> String {
    DateTime::from_timestamp(t, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
pub fn since(s: &str, utc: bool) -> Result<i64> {
    if let Some(days) = s.strip_suffix('d') {
        let d: i64 = days.parse()?;
        if !(0..=36500).contains(&d) {
            return Err("since must be a nonnegative number of days".into());
        }
        return Ok((Utc::now() - chrono::Duration::days(d)).timestamp());
    }
    let d = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")?
        .and_hms_opt(0, 0, 0)
        .ok_or("invalid date")?;
    if utc {
        Ok(Utc.from_utc_datetime(&d).timestamp())
    } else {
        Local
            .from_local_datetime(&d)
            .earliest()
            .map(|t| t.timestamp())
            .ok_or_else(|| "date has no local midnight".into())
    }
}
/// Each row is a complete UTC hour, with gaps calculated within session and hour.
pub fn hourly(db: &Store, start: i64) -> Result<Vec<Value>> {
    hourly_range(db, start, i64::MAX)
}
fn hourly_range(db: &Store, start: i64, end: i64) -> Result<Vec<Value>> {
    let sql = "WITH timed AS (SELECT *,min(300000,max(0,(at-lag(at) OVER(PARTITION BY harness,session,at/3600 ORDER BY at))*1000)) active FROM responses), combined AS (SELECT at,harness,model,effort,project,session,sidechain,1 responses,input,write5,write1,read,output,reasoning,searches,fetches,wall,CASE WHEN wall IS NULL THEN 0 ELSE 1 END samples,active,context,cost,0 tools,0 toolerrors,0 apierrors,0 compactions FROM timed UNION ALL SELECT at,harness,model,effort,project,session,sidechain,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,NULL,NULL,kind='toolCalls',kind='toolErrors',kind='apiErrors',kind='compactions' FROM events) SELECT at/3600*3600 hour,harness,model,effort,project,session,sidechain,sum(responses),sum(input),sum(write5),sum(write1),sum(read),sum(output),sum(reasoning),sum(searches),sum(fetches),sum(wall),sum(samples),sum(active),max(context),sum(cost),sum(tools),sum(toolerrors),sum(apierrors),sum(compactions) FROM combined WHERE at>=? AND at<? GROUP BY hour,harness,model,effort,project,session,sidechain ORDER BY hour,harness,model,session";
    let mut q = db.db.prepare(sql)?;
    let mut out = vec![];
    let mut rows = q.query([start, end])?;
    while let Some(r) = rows.next()? {
        let mut v = json!({"hour":stamp(r.get(0)?),"harness":r.get::<_,String>(1)?,"model":r.get::<_,String>(2)?,"effort":r.get::<_,Option<String>>(3)?,"project":r.get::<_,Option<String>>(4)?,"session":r.get::<_,String>(5)?,"sidechain":r.get::<_,bool>(6)?,"responses":r.get::<_,i64>(7)?});
        for (i, key) in [
            "input",
            "cacheWrite5m",
            "cacheWrite1h",
            "cacheRead",
            "output",
            "reasoning",
            "webSearches",
            "webFetches",
            "wallMs",
            "wallSamples",
            "activeMs",
            "contextPeak",
        ]
        .iter()
        .enumerate()
        {
            v[key] = json!(r.get::<_, Option<i64>>(8 + i)?);
        }
        v["activeMs"] = json!(v["activeMs"].as_i64().unwrap_or(0));
        v["reportedCostUsd"] = json!(r.get::<_, Option<f64>>(20)?);
        for (i, k) in ["toolCalls", "toolErrors", "apiErrors", "compactions"]
            .iter()
            .enumerate()
        {
            v[k] = json!(r.get::<_, i64>(21 + i)?);
        }
        out.push(v);
    }
    Ok(out)
}
#[derive(Default)]
struct Group {
    values: BTreeMap<String, i64>,
    known: BTreeSet<String>,
    sessions: BTreeSet<String>,
    cost: f64,
    saving: f64,
    wall: i64,
    samples: i64,
}
impl Group {
    fn add(&mut self, v: &Value) {
        for k in [
            "responses",
            "input",
            "cacheWrite5m",
            "cacheWrite1h",
            "cacheRead",
            "output",
            "reasoning",
            "toolCalls",
            "toolErrors",
            "apiErrors",
            "compactions",
            "activeMs",
        ] {
            if let Some(n) = v[k].as_i64() {
                *self.values.entry(k.into()).or_default() += n;
                self.known.insert(k.into());
            }
        }
        self.sessions
            .insert(v["session"].as_str().unwrap_or("").into());
        self.wall += v["wallMs"].as_i64().unwrap_or(0);
        self.samples += v["wallSamples"].as_i64().unwrap_or(0);
        if let Some(p) = pricing::price(v["model"].as_str().unwrap_or("")) {
            let n = |k: &str| v[k].as_i64().unwrap_or(0) as f64 / 1e6;
            self.cost += n("input") * p.input
                + n("cacheWrite5m") * p.cache_write5m
                + n("cacheWrite1h") * p.cache_write1h
                + n("cacheRead") * p.cache_read
                + n("output") * p.output;
            self.saving += n("cacheRead") * (p.input - p.cache_read)
                - n("cacheWrite5m") * (p.cache_write5m - p.input)
                - n("cacheWrite1h") * (p.cache_write1h - p.input);
        }
    }
    fn json(self, key: &str) -> Value {
        let denom = ["input", "cacheRead", "cacheWrite5m", "cacheWrite1h"]
            .iter()
            .map(|k| self.values.get(*k).copied().unwrap_or(0))
            .sum::<i64>();
        let mut v = json!({"key":key,"sessions":self.sessions.len(),"costUsd":self.cost,"netCacheSavingUsd":self.saving,"cacheHitRate":if denom>0{self.values.get("cacheRead").map(|n|*n as f64/denom as f64)}else{None},"meanWallMs":if self.samples>0{Some(self.wall as f64/self.samples as f64)}else{None}});
        for k in [
            "responses",
            "input",
            "cacheWrite5m",
            "cacheWrite1h",
            "cacheRead",
            "output",
            "reasoning",
            "toolCalls",
            "toolErrors",
            "apiErrors",
            "compactions",
            "activeMs",
        ] {
            v[k] = if self.known.contains(k) {
                json!(self.values[k])
            } else {
                Value::Null
            };
        }
        v
    }
}
#[derive(Default)]
pub struct Options<'a> {
    pub since: Option<&'a str>,
    pub until: Option<&'a str>,
    pub by: Option<&'a str>,
    pub harness: Option<&'a str>,
    pub project: Option<&'a str>,
    pub utc: bool,
}
pub fn report(db: &Store, o: Options<'_>) -> Result<Value> {
    let start = since(o.since.unwrap_or("7d"), o.utc)?;
    let end = o
        .until
        .map(|s| since(s, o.utc))
        .transpose()?
        .unwrap_or_else(|| Utc::now().timestamp() + 1);
    let by = o.by.unwrap_or("model");
    if ![
        "harness", "model", "project", "day", "hour", "session", "effort",
    ]
    .contains(&by)
    {
        return Err("invalid --by grouping".into());
    }
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut totals = Group::default();
    let mut unpriced = BTreeSet::new();
    for v in hourly_range(db, start, end)? {
        let at = DateTime::parse_from_rfc3339(v["hour"].as_str().unwrap_or(""))
            .map(|t| t.timestamp())
            .unwrap_or(0);
        if at >= end
            || o.harness.is_some_and(|h| v["harness"] != h)
            || o.project.is_some_and(|p| v["project"] != p)
        {
            continue;
        }
        let key = match by {
            "day" => {
                let t = DateTime::<Utc>::from_timestamp(at, 0).unwrap_or_default();
                if o.utc {
                    t.format("%Y-%m-%d").to_string()
                } else {
                    t.with_timezone(&Local).format("%Y-%m-%d").to_string()
                }
            }
            "hour" => v["hour"].as_str().unwrap_or("").into(),
            _ => v[by].as_str().unwrap_or("unknown").into(),
        };
        if v["responses"].as_i64().unwrap_or(0) > 0
            && pricing::price(v["model"].as_str().unwrap_or("")).is_none()
        {
            unpriced.insert(v["model"].clone().as_str().unwrap_or("unknown").to_string());
        }
        totals.add(&v);
        groups.entry(key).or_default().add(&v);
    }
    Ok(
        json!({"since":stamp(start),"until":stamp(end),"totals":totals.json("total"),"groups":groups.into_iter().map(|(k,g)|g.json(&k)).collect::<Vec<_>>(),"unpriced":unpriced}),
    )
}
pub fn sessions(db: &Store, start: &str, limit: usize) -> Result<Value> {
    let mut q=db.db.prepare("SELECT session,harness,project,group_concat(DISTINCT model),min(at),max(at),count(*),sum(input+coalesce(read,0)+coalesce(write5,0)+coalesce(write1,0)+coalesce(output,0)) FROM responses WHERE at>=? GROUP BY harness,session")?;
    let mut out = vec![];
    let mut costs: BTreeMap<String, Group> = BTreeMap::new();
    for row in hourly(db, since(start, false)?)? {
        costs
            .entry(row["session"].as_str().unwrap_or("").to_string())
            .or_default()
            .add(&row);
    }
    for row in q.query_map([since(start,false)?],|r|Ok(json!({"session":r.get::<_,String>(0)?,"harness":r.get::<_,String>(1)?,"project":r.get::<_,Option<String>>(2)?,"models":r.get::<_,String>(3)?.split(',').collect::<Vec<_>>(),"first":stamp(r.get(4)?),"last":stamp(r.get(5)?),"responses":r.get::<_,i64>(6)?,"tokens":r.get::<_,Option<i64>>(7)?})))?{let mut v=row?;let cost=costs.get(v["session"].as_str().unwrap_or("")).map(|g|g.cost).unwrap_or(0.0);v["costUsd"]=json!(cost);out.push(v);}
    out.sort_by(|a, b| {
        b["costUsd"]
            .as_f64()
            .partial_cmp(&a["costUsd"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit);
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Row;
    #[test]
    fn hourly_keys_active_and_formulas() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        for (i, at) in [1793354400, 1793354460, 1793355060].into_iter().enumerate() {
            db.upsert(&Row {
                id: i.to_string(),
                at,
                harness: "claude".into(),
                session: "s-fixture".into(),
                model: "claude-opus-5-5".into(),
                input: Some(100),
                cache_read: Some(900),
                cache_write5m: Some(20),
                cache_write1h: Some(10),
                output: Some(20),
                ..Default::default()
            })
            .unwrap();
        }
        let rows = hourly(&db, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["activeMs"], 360000);
        let v = report(
            &db,
            Options {
                since: Some("2026-10-30"),
                until: Some("2026-10-31"),
                utc: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v["totals"]["responses"], 3);
        assert!((v["totals"]["cacheHitRate"].as_f64().unwrap() - 900.0 / 1030.0).abs() < 1e-10);
        assert!(
            (v["totals"]["netCacheSavingUsd"].as_f64().unwrap()
                - 3.0 * (900.0 * 3.8 - 20.0 - 40.0) / 1e6)
                .abs()
                < 1e-10
        );
    }
}
