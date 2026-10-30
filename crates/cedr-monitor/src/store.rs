//! Transactional counts-only store. Response identities survive file moves and replay.
use crate::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub fn path() -> PathBuf {
    cedr_route::config::xdg_dir("XDG_STATE_HOME", ".local/state").join("cedr/monitor.sqlite")
}
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub id: String,
    pub harness: String,
    pub at: i64,
    pub session: String,
    pub project: Option<String>,
    pub model: String,
    pub effort: Option<String>,
    pub sidechain: bool,
    pub input: Option<i64>,
    pub cache_write5m: Option<i64>,
    pub cache_write1h: Option<i64>,
    pub cache_read: Option<i64>,
    pub output: Option<i64>,
    pub reasoning: Option<i64>,
    pub web_searches: Option<i64>,
    pub web_fetches: Option<i64>,
    pub wall_ms: Option<i64>,
    pub context: Option<i64>,
    pub reported_cost_usd: Option<f64>,
}
pub struct Store {
    pub db: Connection,
}
impl Store {
    /// Existing source health for read-only diagnostics, without migrations or file creation.
    pub fn source_snapshot(path: &Path) -> Result<Vec<serde_json::Value>> {
        let store = Self {
            db: Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?,
        };
        store.source_values()
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .mode(0o600)
                .open(path)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(30))?;
        db.execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE IF NOT EXISTS responses(harness TEXT,id TEXT,at INTEGER,session TEXT,project TEXT,model TEXT,effort TEXT,sidechain INTEGER,input INTEGER,write5 INTEGER,write1 INTEGER,read INTEGER,output INTEGER,reasoning INTEGER,searches INTEGER,fetches INTEGER,wall INTEGER,context INTEGER,cost REAL,PRIMARY KEY(harness,id)); CREATE INDEX IF NOT EXISTS response_time ON responses(at); CREATE INDEX IF NOT EXISTS response_session ON responses(harness,session,at); CREATE TABLE IF NOT EXISTS events(harness TEXT,id TEXT,at INTEGER,session TEXT,project TEXT,model TEXT,effort TEXT,sidechain INTEGER,kind TEXT,PRIMARY KEY(harness,id,kind)); CREATE INDEX IF NOT EXISTS event_time ON events(at); CREATE TABLE IF NOT EXISTS cursors(path TEXT PRIMARY KEY,data TEXT); CREATE TABLE IF NOT EXISTS readings(harness TEXT,window TEXT,at TEXT,data TEXT,PRIMARY KEY(harness,window,at)); CREATE TABLE IF NOT EXISTS sources(harness TEXT PRIMARY KEY,data TEXT);")?;
        Ok(Self { db })
    }
    /// Stores a response; one with no known time (still 0) cannot be placed in an hour and is left out.
    pub fn upsert(&self, r: &Row) -> Result<()> {
        if r.at <= 0 {
            return Ok(());
        }
        self.db.prepare_cached("INSERT INTO responses VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(harness,id) DO UPDATE SET at=excluded.at,session=excluded.session,project=excluded.project,model=excluded.model,effort=coalesce(excluded.effort,responses.effort),sidechain=excluded.sidechain,input=excluded.input,write5=excluded.write5,write1=excluded.write1,read=excluded.read,output=excluded.output,reasoning=excluded.reasoning,searches=excluded.searches,fetches=excluded.fetches,wall=coalesce(excluded.wall,responses.wall),context=excluded.context,cost=excluded.cost")?.execute(params![r.harness,r.id,r.at,r.session,r.project,r.model,r.effort,r.sidechain,r.input,r.cache_write5m,r.cache_write1h,r.cache_read,r.output,r.reasoning,r.web_searches,r.web_fetches,r.wall_ms,r.context,r.reported_cost_usd])?;
        Ok(())
    }
    /// Stores a counted event; like a response, one with no known time is left out.
    pub fn event(&self, r: &Row, id: &str, kind: &str) -> Result<()> {
        if r.at <= 0 {
            return Ok(());
        }
        self.db
            .prepare_cached("INSERT OR REPLACE INTO events VALUES(?,?,?,?,?,?,?,?,?)")?
            .execute(params![
                r.harness,
                id,
                r.at,
                r.session,
                r.project,
                r.model,
                r.effort,
                r.sidechain,
                kind
            ])?;
        Ok(())
    }
    pub fn readings(&self, doc: &serde_json::Value) -> Result<()> {
        let transaction = if self.db.is_autocommit() {
            Some(self.db.unchecked_transaction()?)
        } else {
            None
        };
        for h in doc["harnesses"].as_array().into_iter().flatten() {
            for w in h["windows"].as_array().into_iter().flatten() {
                let minutes = w["windowMinutes"]
                    .as_i64()
                    .or_else(|| match w["kind"].as_str() {
                        Some("session") => Some(300),
                        Some("weekly") => Some(10080),
                        _ => None,
                    });
                let window = w["id"]
                    .as_str()
                    .unwrap_or("unknown")
                    .chars()
                    .take(64)
                    .collect::<String>();
                let v = serde_json::json!({"harness":h["harness"],"window":window,"kind":w["kind"],"percent":w["percent"],"resetsAt":w["resetsAt"],"windowMinutes":minutes,"at":h["fetchedAt"]});
                self.db.execute(
                    "INSERT OR REPLACE INTO readings VALUES(?,?,?,?)",
                    params![
                        h["harness"].as_str(),
                        Some(window.as_str()),
                        h["fetchedAt"].as_str(),
                        v.to_string()
                    ],
                )?;
            }
        }
        if let Some(t) = transaction {
            t.commit()?;
        }
        Ok(())
    }
    pub fn source_values(&self) -> Result<Vec<serde_json::Value>> {
        let mut q = self
            .db
            .prepare("SELECT data FROM sources ORDER BY harness")?;
        q.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<Vec<_>>>()
    }
}
