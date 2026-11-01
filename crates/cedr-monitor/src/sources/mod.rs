//! Isolated local collectors. Only usage metadata crosses the parser/store boundary.
mod protobuf;
use crate::{
    Result,
    config::Config,
    cursors::{self, Cursor},
    store::{Row, Store},
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
fn n(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(Value::as_i64)
}
fn time(v: &Value) -> Option<i64> {
    v.as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.timestamp())
        .or_else(|| {
            v.as_i64().map(|t| {
                if t > 100_000_000_000 {
                    t / 1_000_000
                } else {
                    t
                }
            })
        })
}
fn add(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => a.checked_add(b),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        _ => None,
    }
}
fn sub(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    a.and_then(|a| a.checked_sub(b.unwrap_or(0)))
}
fn bounded(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(128).collect()
}
fn metadata(h: &str, v: &Value, state: &Value, c: &Config) -> Option<Row> {
    let cwd = if !s(v, "cwd").is_empty() {
        s(v, "cwd")
    } else {
        s(state, "cwd")
    };
    let native = if !s(v, "sessionId").is_empty() {
        s(v, "sessionId")
    } else {
        s(state, "session")
    };
    // A record without its own time (Claude Code writes some tool results and progress records that way) belongs to the
    // last time seen earlier in the same file. With none, 0 stays until a harness reads a nested time; the store
    // refuses anything still at 0 rather than count it at the epoch.
    let at = time(&v["timestamp"])
        .or_else(|| time(&v["recorded_at"]))
        .or_else(|| state["lastAt"].as_i64())
        .unwrap_or(0);
    Some(Row {
        harness: h.into(),
        at,
        session: format!("s-{}", &crate::digest(&format!("{h}:{native}"))[..12]),
        project: c.project(cwd)?,
        model: if s(state, "model").is_empty() {
            "unknown".into()
        } else {
            bounded(s(state, "model"))
        },
        effort: state["effort"].as_str().map(bounded),
        sidechain: state["sidechain"].as_bool().unwrap_or(false),
        ..Row::default()
    })
}
fn valid(r: &Row) -> bool {
    [
        r.input,
        r.cache_read,
        r.cache_write5m,
        r.cache_write1h,
        r.output,
        r.reasoning,
        r.web_searches,
        r.web_fetches,
        r.wall_ms,
        r.context,
    ]
    .into_iter()
    .flatten()
    .all(|x| x >= 0)
        && !matches!((r.reasoning,r.output),(Some(a),Some(b)) if a>b)
        && r.at > 0
        && !r.id.is_empty()
}
fn counts(r: &mut Row, u: &Value, keys: [&str; 6], inclusive: bool, thoughts_extra: bool) {
    if keys.iter().any(|key| {
        u.get(key)
            .is_some_and(|v| !v.is_null() && !v.as_i64().is_some_and(|n| n >= 0))
    }) {
        r.input = Some(-1);
        return;
    }
    r.input = n(u, keys[0]);
    r.cache_read = n(u, keys[1]);
    r.cache_write5m = n(u, keys[2]);
    r.cache_write1h = Some(0);
    r.output = n(u, keys[3]);
    r.reasoning = n(u, keys[4]);
    r.context = r.input;
    if inclusive {
        r.input = sub(sub(r.input, r.cache_read), r.cache_write5m);
    } else {
        r.context = add(add(r.input, r.cache_read), r.cache_write5m);
    }
    if thoughts_extra {
        r.output = add(r.output, r.reasoning);
    }
}
/// Parse one update while retaining only bounded, non-content session metadata.
fn parse(
    h: &str,
    v: &Value,
    offset: u64,
    state: &mut Value,
    c: &Config,
    db: &Store,
) -> Result<bool> {
    if !v.is_object() {
        return Ok(false);
    }
    if !state.is_object() {
        *state = json!({});
    }
    if h == "gemini" {
        if let Some(set) = v.get("$set") {
            for (key, target) in [("sessionId", "session"), ("cwd", "cwd")] {
                if !set[key].is_null() {
                    state[target] = set[key].clone();
                }
            }
            if let Some(cwd) = set["directories"][0].as_str() {
                state["cwd"] = json!(cwd);
            }
            for (i, m) in set["messages"].as_array().into_iter().flatten().enumerate() {
                parse(h, m, i as u64, state, c, db)?;
            }
            return Ok(true);
        }
        if let Some(patch) = v.get("$patch") {
            for id in patch["removeIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if let Some(r) = metadata(h, v, state, c) {
                    db.db.execute(
                        "DELETE FROM responses WHERE harness='gemini' AND id=?",
                        [format!("{}:{id}", r.session)],
                    )?;
                }
            }
            return Ok(true);
        }
        if let Some(cwd) = v["directories"][0].as_str() {
            state["cwd"] = json!(cwd);
        }
    }
    if h == "opencode" && v["sessionID"].is_string() {
        state["session"] = v["sessionID"].clone();
    }
    let p = &v["payload"];
    let typ = s(v, "type");
    if h == "codex" {
        if typ == "session_meta" {
            state["session"] = p["id"].clone();
            state["cwd"] = p["cwd"].clone();
            state["sidechain"] = json!(p["source"].get("subagent").is_some());
        }
        if typ == "turn_context" {
            for k in ["model", "effort", "cwd"] {
                if !p[k].is_null() {
                    state[k] = p[k].clone();
                }
            }
        }
    }
    if h == "gemini" {
        for k in ["sessionId", "cwd"] {
            if !v[k].is_null() {
                state[if k == "sessionId" { "session" } else { k }] = v[k].clone();
            }
        }
    }
    if h == "copilot" && typ == "session.start" {
        state["session"] = v["data"]["sessionId"].clone();
        state["cwd"] = v["data"]["context"]["cwd"].clone();
    }
    if h == "grok" && typ == "turn_started" && s(state, "model").is_empty() {
        state["model"] = v["model_id"].clone();
    }
    if h == "muse" {
        let record = &p["record"];
        let kind = s(v, "payload_type");
        if kind == "runtime.session.metadata" {
            state["cwd"] = record["workspace_root"].clone();
            state["model"] = record["model_id"].clone();
        }
        if kind == "runtime.session.route_facts" {
            state["cwd"] = record["cwd"].clone();
        }
        if kind == "run.model.configured" {
            state["model"] = record["model_id"].clone();
        }
    }
    if h == "muse" {
        let e = &p["event"];
        if s(&e["details"], "phase") == "stream_succeeded" {
            for f in e["details"]["facets"].as_array().into_iter().flatten() {
                if s(&f["detail"], "kind") == "provider" && f["detail"]["model"].is_string() {
                    state["model"] = json!(bounded(s(&f["detail"], "model")));
                }
            }
        }
        if s(e, "kind") == "model_completed"
            && let (Some(id), Some(duration)) = (state["pending_id"].as_str(), n(e, "duration_ms"))
            && state["pending_input"] == e["usage"]["input_tokens"]
            && state["pending_output"] == e["usage"]["output_tokens"]
            && duration >= 0
        {
            db.db.execute(
                "UPDATE responses SET wall=? WHERE harness='muse' AND id=?",
                params![duration, id],
            )?;
        }
    }
    if h == "claude"
        && typ == "api-request"
        && let Some(at) = time(&v["timestamp"])
    {
        if !state["requests"].is_object() {
            state["requests"] = json!({});
        }
        let requests = state["requests"].as_object_mut().expect("object");
        requests.insert(s(v, "id").chars().take(128).collect(), json!(at));
        if requests.len() > 128
            && let Some(key) = requests.keys().next().cloned()
        {
            requests.remove(&key);
        }
    }
    if h == "claude"
        && v["message"]["model"]
            .as_str()
            .is_some_and(|m| !m.is_empty())
    {
        state["model"] = v["message"]["model"].clone();
    }
    if let Some(at) = time(&v["timestamp"]).or_else(|| time(&v["recorded_at"])) {
        state["lastAt"] = json!(at);
    }
    let Some(mut r) = metadata(h, v, state, c) else {
        return Ok(true);
    };
    let native = if !s(v, "uuid").is_empty() {
        s(v, "uuid").to_string()
    } else if !s(v, "id").is_empty() {
        s(v, "id").to_string()
    } else {
        format!("{}:{offset}", s(state, "file"))
    };
    let event = |kind: &str, r: &Row| db.event(r, &native, kind);
    match h {
        "claude" => {
            let m = &v["message"];
            if !s(m, "model").is_empty() {
                r.model = bounded(s(m, "model"));
            }
            r.sidechain = v["isSidechain"].as_bool().unwrap_or(false);
            r.effort = v["perTurnEffort"]
                .as_str()
                .or_else(|| v["effort"].as_str())
                .map(bounded);
            if v["isApiErrorMessage"] == true {
                event("apiErrors", &r)?;
            }
            if typ == "system" && s(v, "subtype") == "compact_boundary" {
                event("compactions", &r)?;
            }
            for (i, b) in m["content"].as_array().into_iter().flatten().enumerate() {
                let kind = match s(b, "type") {
                    "tool_use" => Some("toolCalls"),
                    "tool_result" if b["is_error"] == true => Some("toolErrors"),
                    _ => None,
                };
                if let Some(k) = kind {
                    let id = if s(b, "id").is_empty() {
                        format!("{native}:{i}")
                    } else {
                        s(b, "id").into()
                    };
                    db.event(&r, &id, k)?;
                }
            }
            let u = &m["usage"];
            if !u.is_object() {
                return Ok(true);
            }
            r.id = s(m, "id").into();
            r.wall_ms = state["requests"]
                .get(s(v, "requestRef"))
                .and_then(Value::as_i64)
                .filter(|start| *start <= r.at)
                .map(|start| (r.at - start) * 1000);

            counts(
                &mut r,
                u,
                [
                    "input_tokens",
                    "cache_read_input_tokens",
                    "cache_creation_input_tokens",
                    "output_tokens",
                    "unused",
                    "unused",
                ],
                false,
                false,
            );
            r.cache_write1h = n(&u["cache_creation"], "ephemeral_1h_input_tokens");
            r.cache_write5m = sub(r.cache_write5m, r.cache_write1h);
            r.reasoning = n(&u["output_tokens_details"], "thinking_tokens");
            r.web_searches = n(&u["server_tool_use"], "web_search_requests");
            r.web_fetches = n(&u["server_tool_use"], "web_fetch_requests");
        }
        "codex" => {
            if typ == "response_item"
                && ["function_call", "custom_tool_call"].contains(&s(p, "type"))
            {
                db.event(
                    &r,
                    if s(p, "call_id").is_empty() {
                        &native
                    } else {
                        s(p, "call_id")
                    },
                    "toolCalls",
                )?;
            }
            if typ == "compacted" {
                event("compactions", &r)?;
            }
            if typ == "event_msg" && ["error", "stream_error"].contains(&s(p, "type")) {
                event("apiErrors", &r)?;
            }
            if typ == "event_msg"
                && s(p, "type") == "task_complete"
                && let Some(id) = state["last_response"].as_str()
            {
                db.db.execute(
                    "UPDATE responses SET wall=? WHERE harness='codex' AND id=?",
                    params![n(p, "duration_ms"), id],
                )?;
            }
            if typ == "event_msg" && s(p, "type") == "token_count" {
                for key in ["primary", "secondary"] {
                    let w = &p["rate_limits"][key];
                    if w.is_object() {
                        let minutes = n(w, "window_minutes");
                        db.readings(&json!({"harnesses":[{"harness":"codex","fetchedAt":v["timestamp"],"windows":[{"id":if ["","codex"].contains(&s(&p["rate_limits"],"limit_id")){key.to_string()}else{format!("{}:{key}",s(&p["rate_limits"],"limit_id"))},"kind":if minutes.unwrap_or(0)<=300{"session"}else{"weekly"},"percent":w["used_percent"],"resetsAt":n(w,"resets_at").and_then(|t|chrono::DateTime::from_timestamp(t,0)).map(|t|t.to_rfc3339()),"windowMinutes":minutes}]}]}))?;
                    }
                }
                let actual:bool=db.db.query_row("SELECT EXISTS(SELECT 1 FROM responses WHERE harness='codex' AND session=? AND id NOT LIKE 'legacy:%')",[&r.session],|row|row.get(0))?;
                if state["has_records"] == true || actual {
                    return Ok(true);
                }
                let total = &p["info"]["total_token_usage"];
                let total = n(total, "total_tokens")
                    .or_else(|| add(n(total, "input_tokens"), n(total, "output_tokens")))
                    .unwrap_or(0);
                if total <= state["total"].as_i64().unwrap_or(0) {
                    return Ok(true);
                }
                state["total"] = json!(total);
                r.id = format!("legacy:{}:{total}", r.session);
                counts(
                    &mut r,
                    &p["info"]["last_token_usage"],
                    [
                        "input_tokens",
                        "cached_input_tokens",
                        "cache_write_input_tokens",
                        "output_tokens",
                        "reasoning_output_tokens",
                        "unused",
                    ],
                    true,
                    false,
                );
            } else if typ == "token_usage_record" {
                state["has_records"] = json!(true);
                db.db.execute("DELETE FROM responses WHERE harness='codex' AND session=? AND id LIKE 'legacy:%'",[&r.session])?;
                // A rollout can carry another thread's responses (a subagent's, a forked thread's); each belongs to
                // the thread that made it, not to the rollout's own session.
                let thread = s(p, "thread_id");
                if !thread.is_empty() {
                    r.session = format!("s-{}", &crate::digest(&format!("codex:{thread}"))[..12]);
                }
                r.id = s(p, "response_id").into();
                counts(
                    &mut r,
                    &p["usage"],
                    [
                        "input_tokens",
                        "cached_input_tokens",
                        "cache_write_input_tokens",
                        "output_tokens",
                        "reasoning_output_tokens",
                        "unused",
                    ],
                    true,
                    false,
                );
                state["last_response"] = json!(r.id);
            } else {
                return Ok(true);
            }
        }
        "gemini" => {
            let m = v.get("message").unwrap_or(v);
            let u = &m["tokens"];
            if !u.is_object() {
                return Ok(true);
            }
            r.id = format!("{}:{}", r.session, s(m, "id"));
            if !s(m, "model").is_empty() {
                r.model = bounded(s(m, "model"));
            }
            r.at = time(&m["timestamp"]).unwrap_or(r.at);
            counts(
                &mut r,
                u,
                ["input", "cached", "unused", "output", "thoughts", "unused"],
                true,
                true,
            );
            let prefix = format!("{}:", r.id);
            db.db.execute(
                "DELETE FROM events WHERE harness='gemini' AND substr(id,1,length(?1))=?1",
                [prefix],
            )?;
            for (i, t) in m["toolCalls"].as_array().into_iter().flatten().enumerate() {
                let id = format!("{}:{}:{i}", r.id, s(t, "id"));
                db.event(&r, &id, "toolCalls")?;
                if s(t, "status") == "error" {
                    db.event(&r, &id, "toolErrors")?;
                }
            }
        }
        "copilot" => {
            let d = &v["data"];
            if typ == "tool.execution_complete" {
                db.event(&r, s(d, "toolCallId"), "toolCalls")?;
                if d["success"] == false {
                    db.event(&r, s(d, "toolCallId"), "toolErrors")?;
                }
            }
            if typ != "session.shutdown" {
                return Ok(true);
            }
            if let Some(models) = d["modelMetrics"].as_object() {
                for (model, m) in models {
                    let mut r = r.clone();
                    r.id = format!("{}:{model}", r.session);
                    r.model = bounded(model);
                    counts(
                        &mut r,
                        &m["usage"],
                        [
                            "inputTokens",
                            "cacheReadTokens",
                            "cacheWriteTokens",
                            "outputTokens",
                            "reasoningTokens",
                            "unused",
                        ],
                        true,
                        false,
                    );
                    r.wall_ms = n(m, "totalApiDurationMs");
                    if valid(&r) {
                        db.upsert(&r)?;
                    } else {
                        return Ok(false);
                    }
                }
            }
            return Ok(true);
        }
        "opencode" => {
            if s(v, "type") == "tool" {
                r.at = v["state"]["time"]["end"]
                    .as_i64()
                    .or_else(|| v["state"]["time"]["start"].as_i64())
                    .map(|t| t / 1000)
                    .unwrap_or(r.at);
                db.event(&r, s(v, "id"), "toolCalls")?;
                if s(&v["state"], "status") == "error" {
                    db.event(&r, s(v, "id"), "toolErrors")?;
                }
                return Ok(true);
            }
            if !["assistant"].contains(&s(v, "role")) && s(v, "type") != "assistant" {
                return Ok(true);
            }
            r.id = s(v, "id").into();
            r.model = bounded(v["modelID"].as_str().unwrap_or(s(v, "model")));
            r.at = v["time"]["created"]
                .as_i64()
                .map(|x| x / 1000)
                .unwrap_or(r.at);
            let u = &v["tokens"];
            counts(
                &mut r,
                u,
                ["input", "unused", "unused", "output", "reasoning", "unused"],
                false,
                true,
            );
            r.cache_read = n(&u["cache"], "read");
            r.cache_write5m = n(&u["cache"], "write");
            r.reported_cost_usd = v["cost"].as_f64();
            if v.get("error").is_some_and(|x| !x.is_null()) {
                event("apiErrors", &r)?;
            }
        }
        "grok" => return grok(v, &r, &native, db),
        "muse" => {
            let e = &p["event"];
            if s(v, "payload_type") != "runtime.session" || s(e, "kind") != "goal_usage_attribution"
            {
                return Ok(true);
            }
            let a = &e["record"];
            let q = &a["quantity"];
            if s(a, "usage_family") != "provider" || q["reported"] != true {
                return Ok(true);
            }
            r.id = s(a, "usage_id").into();
            state["pending_id"] = json!(r.id);
            state["pending_input"] = q["input_tokens"].clone();
            state["pending_output"] = q["output_tokens"].clone();

            r.sidechain = s(&a["owner"], "owner_type") == "reminder_child";
            counts(
                &mut r,
                q,
                [
                    "input_tokens",
                    "cached_tokens",
                    "unused",
                    "output_tokens",
                    "reasoning_tokens",
                    "unused",
                ],
                true,
                false,
            );
        }
        _ => return Ok(true),
    }
    // Counter-bearing objects contain only numbers, so reject wrong JSON types before storage.
    let numeric = match h {
        "claude" => &v["message"]["usage"],
        "codex" if typ == "token_usage_record" => &p["usage"],
        "codex" => &p["info"]["last_token_usage"],
        "gemini" => v
            .get("message")
            .unwrap_or(v)
            .get("tokens")
            .unwrap_or(&Value::Null),
        "opencode" => &v["tokens"],
        "muse" => &p["event"]["record"]["quantity"],
        _ => &Value::Null,
    };
    for key in [
        "input_tokens",
        "output_tokens",
        "cached_input_tokens",
        "cache_write_input_tokens",
        "reasoning_output_tokens",
        "cached_tokens",
        "reasoning_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
        "input",
        "cached",
        "output",
        "thoughts",
        "reasoning",
    ] {
        if numeric
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.as_i64().is_some_and(|n| n >= 0))
        {
            return Ok(false);
        }
    }
    // A present counter of the wrong type is malformed, including negative values.
    if !valid(&r) {
        return Ok(false);
    }
    db.upsert(&r)?;
    Ok(true)
}
/// One Grok record: a session's whole `usage.json`, or one line of its `events.jsonl`. Neither holds prompts or code.
fn grok(v: &Value, base: &Row, native: &str, db: &Store) -> Result<bool> {
    let Some(turns) = v["turns"].as_array() else {
        let mut r = base.clone();
        r.at = time(&v["ts"]).unwrap_or(r.at);
        match s(v, "type") {
            "tool_started" => db.event(&r, native, "toolCalls")?,
            "tool_completed" if s(v, "outcome") != "success" => {
                db.event(&r, native, "toolErrors")?
            }
            _ => (),
        }
        return Ok(true);
    };
    let mut ok = true;
    for t in turns {
        let Some(models) = t["modelUsage"].as_object() else {
            ok = false;
            continue;
        };
        for (model, m) in models {
            let mut r = base.clone();
            // A fork copies its parent's turns, with the same end times, into its own file. Keying on the end time
            // rather than the session or turn number is what makes a copied turn the same response.
            r.id = format!("{}:{model}", s(t, "endedAt"));
            r.model = bounded(model);
            r.at = time(&t["endedAt"]).unwrap_or(0);
            counts(
                &mut r,
                m,
                [
                    "inputTokens",
                    "cachedReadTokens",
                    "cacheCreationTokens",
                    "outputTokens",
                    "reasoningTokens",
                    "unused",
                ],
                true,
                false,
            );
            // These are sums over a turn's model calls, so they say nothing about the size of the context.
            r.context = None;
            r.reported_cost_usd = n(m, "costUsdTicks")
                .filter(|ticks| *ticks >= 0)
                .map(|ticks| ticks as f64 / GROK_TICKS_PER_USD);
            if !valid(&r) {
                ok = false;
                continue;
            }
            // The first session to report a turn keeps it, so a fork never moves its parent's usage.
            let owner: Option<String> = db
                .db
                .query_row(
                    "SELECT session FROM responses WHERE harness='grok' AND id=?",
                    [&r.id],
                    |row| row.get(0),
                )
                .ok();
            if owner.is_none_or(|o| o == r.session) {
                db.upsert(&r)?;
            }
        }
    }
    Ok(ok)
}
const GROK_TICKS_PER_USD: f64 = 1e10;
/// Grok URL-encodes the working directory to name a session's group directory.
fn url_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| text.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        if let Some(byte) = hex {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
/// What a Grok session's location says about it, from names and `usage.json` only: session files holding conversation
/// text stay closed. A group name too long to encode is a slug, with the real path in a `.cwd` file beside the sessions.
fn grok_state(state: &mut Value, path: &Path, children: &HashSet<String>) {
    let dir = path.parent().unwrap_or(Path::new(""));
    let session = dir.file_name().unwrap_or_default().to_string_lossy();
    state["session"] = json!(session);
    state["sidechain"] = json!(children.contains(session.as_ref()));
    if let Some(group) = dir.parent() {
        let cwd = std::fs::read_to_string(group.join(".cwd"))
            .map(|c| c.trim().to_string())
            .unwrap_or_else(|_| {
                url_decoded(&group.file_name().unwrap_or_default().to_string_lossy())
            });
        state["cwd"] = json!(cwd);
    }
    // Tool events need the model the usage rows name (`grok-4.7-build`, where `turn_started` says `grok-4.7`), or
    // reports split a session's tool calls from its tokens.
    let usage = dir.join("usage.json");
    let primary = std::fs::metadata(&usage)
        .ok()
        .filter(|m| m.len() <= cursors::MAX_LINE as u64)
        .and_then(|_| std::fs::File::open(&usage).ok())
        .and_then(|f| serde_json::from_reader::<_, Value>(f).ok())
        .and_then(|u| u["session"]["primaryModelId"].as_str().map(String::from));
    if let Some(model) = primary {
        state["model"] = json!(model);
    }
}
/// Sessions that ran as a subagent, named by the `subagents/<session id>/` directories of their parents. A parent's
/// usage already includes its subagents' model calls, so a child's own `usage.json` would count them twice.
fn grok_children(paths: &[PathBuf]) -> HashSet<String> {
    paths
        .iter()
        .filter_map(|p| std::fs::read_dir(p.parent()?.join("subagents")).ok())
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}
// Cursors retain bounded primitive metadata, never arbitrary log objects or content.
fn sanitize_state(state: &mut Value) {
    if let Some(o) = state.as_object_mut() {
        o.retain(|key, value| match key.as_str() {
            "session" | "cwd" | "model" | "effort" | "file" | "last_response" | "pending_id" => {
                if let Some(text) = value.as_str() {
                    *value = json!(
                        text.chars()
                            .take(if key == "cwd" { 4096 } else { 128 })
                            .collect::<String>()
                    );
                    true
                } else {
                    false
                }
            }
            "sidechain" | "has_records" => value.is_boolean(),
            "total" | "pending_input" | "pending_output" => value.as_i64().is_some(),
            "requests" => value.as_object().is_some_and(|m| {
                m.len() <= 128
                    && m.keys().all(|k| k.len() <= 512)
                    && m.values().all(|v| v.as_i64().is_some())
            }),
            _ => false,
        });
    }
}
fn patterns(h: &str, c: &Config, home: &str) -> Vec<String> {
    if let Some(p) = c.paths.get(h) {
        let p = crate::config::expand(p).to_string_lossy().into_owned();
        if p.contains('*') || Path::new(&p).is_file() {
            return vec![p];
        }
        return vec![format!("{p}/**/*")];
    }
    let data = cedr_route::config::xdg_dir("XDG_DATA_HOME", ".local/share");
    match h {
        "claude" => vec![format!(
            "{}/projects/**/*.jsonl",
            std::env::var("CLAUDE_CONFIG_DIR").unwrap_or(format!("{home}/.claude"))
        )],
        "codex" => {
            let p = std::env::var("CODEX_HOME").unwrap_or(format!("{home}/.codex"));
            vec![
                format!("{p}/sessions/**/rollout-*.jsonl"),
                format!("{p}/archived_sessions/**/rollout-*.jsonl"),
            ]
        }
        "gemini" => vec![format!("{home}/.gemini/tmp/*/chats/session-*.json*")],
        "copilot" => vec![format!(
            "{}/session-state/*/events.jsonl",
            std::env::var("COPILOT_HOME").unwrap_or(format!("{home}/.copilot"))
        )],
        "muse" => vec![format!("{}/muse/sessions/**/session.jsonl", data.display())],
        "grok" => {
            let p = std::env::var("GROK_HOME").unwrap_or(format!("{home}/.grok"));
            vec![
                format!("{p}/sessions/*/*/usage.json"),
                format!("{p}/sessions/*/*/events.jsonl"),
            ]
        }
        "opencode" => vec![
            std::env::var("OPENCODE_DB")
                .unwrap_or(format!("{}/opencode/opencode.db", data.display())),
            format!("{}/opencode/storage/message/**/*.json", data.display()),
            format!("{}/opencode/storage/part/**/*.json", data.display()),
        ],
        "antigravity" => vec![
            format!("{home}/.gemini/antigravity*/**/conversations/*.db"),
            // T3 Code runs Antigravity with a home of its own per provider instance.
            format!("{home}/.t3/userdata/providers/antigravity/*/**/conversations/*.db"),
        ],
        _ => vec![],
    }
}
/// Read-only source discovery for diagnostics; does not collect or write usage.
pub fn files(h: &str, c: &Config) -> Result<Vec<PathBuf>> {
    files_in(h, c, &std::env::var("HOME").unwrap_or_default())
}
fn files_in(h: &str, c: &Config, home: &str) -> Result<Vec<PathBuf>> {
    let mut out = vec![];
    for p in patterns(h, c, home) {
        for path in glob::glob(&p)? {
            let path = path?;
            if path.is_file()
                && matches!(
                    path.extension().and_then(|x| x.to_str()),
                    Some("jsonl" | "json" | "db")
                )
            {
                if h == "muse" && path.components().any(|x| x.as_os_str() == "subagent") {
                    continue;
                }
                // Also under a `[paths]` directory: the session files beside these hold the conversation.
                if h == "grok"
                    && !matches!(
                        path.file_name().and_then(|x| x.to_str()),
                        Some("usage.json" | "events.jsonl")
                    )
                {
                    continue;
                }
                out.push(path);
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}
/// Readers whose attribution changed, with the revision a store must have read their files at. A store behind it
/// rereads every file of that harness once from the start, so stored rows move to the corrected identity (rows are
/// keyed by response, so nothing is counted twice). Codex 2: responses take their own `thread_id` as session.
const REREADS: &[(&str, i64)] = &[("codex", 2)];

pub fn collect(c: &Config, db: &mut Store) -> Result<()> {
    for &h in crate::HARNESSES {
        if !c.harnesses.is_empty() && !c.harnesses.iter().any(|x| x == h) {
            continue;
        }
        let revision = REREADS.iter().find(|(x, _)| *x == h).map(|(_, r)| *r);
        let read_at: i64 = db
            .db
            .query_row(
                "SELECT value FROM meta WHERE key=?",
                [format!("reader:{h}")],
                |r| r.get(0),
            )
            .unwrap_or(1);
        let reread = revision.is_some_and(|r| read_at < r);
        let mut count = 0;
        let mut malformed = 0u64;
        let mut error = None;
        match files(h, c) {
            Ok(paths) => {
                count = paths.len();
                let children = if h == "grok" {
                    grok_children(&paths)
                } else {
                    HashSet::new()
                };
                for path in paths {
                    if h == "grok"
                        && path.file_name().is_some_and(|x| x == "usage.json")
                        && path
                            .parent()
                            .and_then(Path::file_name)
                            .is_some_and(|x| children.contains(x.to_string_lossy().as_ref()))
                    {
                        continue;
                    }
                    let result = (|| -> Result<u64> {
                        let key = path.to_string_lossy();
                        let old = db
                            .db
                            .query_row(
                                "SELECT data FROM cursors WHERE path=?",
                                [key.as_ref()],
                                |r| r.get::<_, String>(0),
                            )
                            .ok()
                            .filter(|_| !reread);
                        let mut cursor = old
                            .and_then(|x| serde_json::from_str::<Cursor>(&x).ok())
                            .unwrap_or_default();
                        let stat = std::fs::metadata(&path)?;
                        let mtime = stat
                            .modified()?
                            .duration_since(std::time::UNIX_EPOCH)?
                            .as_nanos();
                        #[cfg(unix)]
                        let (device, inode) = {
                            use std::os::unix::fs::MetadataExt;
                            (stat.dev(), stat.ino())
                        };
                        #[cfg(not(unix))]
                        let (device, inode) = (0, 0);
                        let document = path.extension().is_some_and(|x| x == "json" || x == "db");
                        if document
                            && cursor.mtime == mtime
                            && cursor.size == stat.len()
                            && cursor.device == device
                            && cursor.inode == inode
                        {
                            return Ok(cursor.malformed);
                        }
                        if document {
                            cursor.malformed = 0;
                        }
                        db.db.execute_batch("BEGIN IMMEDIATE")?;
                        if path.extension().is_some_and(|x| x == "db") {
                            if h == "antigravity" {
                                cursor.malformed = protobuf::collect(&path, c, db)?;
                            } else {
                                opencode_sqlite(&path, c, db)?;
                            }
                        } else if path.extension().is_some_and(|x| x == "json") {
                            if std::fs::metadata(&path)?.len() > cursors::MAX_LINE as u64 {
                                return Err("JSON document exceeds 8 MiB".into());
                            }
                            let v: Value = serde_json::from_reader(std::fs::File::open(&path)?)?;
                            let mut state = json!({"session":path.file_stem().unwrap_or_default().to_string_lossy(),"cwd":v["directories"][0]});
                            if h == "grok" {
                                grok_state(&mut state, &path, &children);
                            }
                            if let Some(messages) = v["messages"].as_array() {
                                state["session"] = v["sessionId"].clone();
                                for (i, m) in messages.iter().enumerate() {
                                    if !parse(h, m, i as u64, &mut state, c, db)? {
                                        cursor.malformed += 1;
                                    }
                                }
                            } else if !parse(h, &v, 0, &mut state, c, db)? {
                                cursor.malformed += 1;
                            }
                        } else {
                            if cursor.state.is_null() {
                                cursor.state = json!({"session":path.parent().and_then(Path::file_name).unwrap_or_default().to_string_lossy(),"file":crate::digest(&key)});
                            }
                            // Only a log that grew can need a newer model name; collection runs often.
                            if h == "grok" && stat.len() != cursor.size {
                                grok_state(&mut cursor.state, &path, &children);
                            }
                            cursor = cursors::read(&path, cursor, |v, offset, state| {
                                if !state.is_object() || state.get("file").is_none() {
                                    *state = json!({"session":path.parent().and_then(Path::file_name).unwrap_or_default().to_string_lossy(),"file":crate::digest(&key)});
                                }
                                // A cursor that starts over (a new or replaced file) has just dropped its state.
                                if h == "grok" && state["cwd"].is_null() {
                                    grok_state(state, &path, &children);
                                }
                                let parsed = parse(h, &v, offset, state, c, db)?;
                                sanitize_state(state);
                                Ok(parsed)
                            })?;
                        }
                        if document {
                            cursor.mtime = mtime;
                            cursor.size = stat.len();
                            cursor.device = device;
                            cursor.inode = inode;
                        }
                        db.db.execute(
                            "INSERT OR REPLACE INTO cursors VALUES(?,?)",
                            params![key.as_ref(), serde_json::to_string(&cursor)?],
                        )?;
                        db.db.execute_batch("COMMIT")?;
                        Ok(cursor.malformed)
                    })();
                    match result {
                        Ok(m) => malformed += m,
                        Err(e) => {
                            let _ = db.db.execute_batch("ROLLBACK");
                            error = Some(if e.to_string().contains("mid-write") {
                                "conversation mid-write; retry next collection"
                            } else {
                                "cannot read usage metadata; check local source format and permissions"
                            });
                        }
                    }
                }
            }
            Err(_) => error = Some("cannot enumerate source files; check permissions"),
        }
        let (responses, first, last): (i64, Option<i64>, Option<i64>) = db.db.query_row(
            "SELECT count(*),min(at),max(at) FROM responses WHERE harness=?",
            [h],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let stamp = |t: Option<i64>| {
            t.and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        };
        let reason = error.or(if h == "cursor" {
            Some("no documented local token log")
        } else if count > 0 && responses == 0 {
            Some("usage unknown; no completed usage records found")
        } else {
            None
        });
        let v = json!({"harness":h,"state":if h=="cursor"{"unsupported"}else if error.is_some(){"error"}else if count==0{"absent"}else{"ok"},"files":count,"responses":responses,"first":stamp(first),"last":stamp(last),"malformed":malformed,"message":reason});
        db.db.execute(
            "INSERT OR REPLACE INTO sources VALUES(?,?)",
            params![h, v.to_string()],
        )?;
        // A response that moves session records its old key in `repairs` (the store's `response_moved` trigger, in
        // the same transaction as the move), so an interrupted reread loses nothing and simply runs again.
        if let Some(r) = revision.filter(|_| reread && error.is_none()) {
            db.db.execute(
                "INSERT OR REPLACE INTO meta VALUES(?,?)",
                params![format!("reader:{h}"), r],
            )?;
        }
    }
    Ok(())
}
/// Compatibility adapter for the documented legacy SQLite message layout.
fn opencode_sqlite(path: &Path, c: &Config, db: &Store) -> Result<()> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut q = conn.prepare("SELECT id,session_id,data FROM message")?;
    for item in q.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })? {
        let (id, session, data) = item?;
        let mut v: Value = serde_json::from_str(&data)?;
        v["id"] = json!(id);
        let mut state = json!({"session":session});
        parse("opencode", &v, 0, &mut state, c, db)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(h: &str, name: &str) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Store::open(&dir.path().join("monitor.sqlite")).unwrap();
        let mut c = Config {
            harnesses: vec![h.into()],
            ..Config::default()
        };
        c.paths.insert(
            h.into(),
            format!(
                "{}/../../fixtures/monitor/{name}",
                env!("CARGO_MANIFEST_DIR")
            ),
        );
        collect(&c, &mut db).unwrap();
        collect(&c, &mut db).unwrap();
        (dir, db)
    }
    fn usage(db: &Store) -> (i64, i64, i64) {
        db.db
            .query_row(
                "SELECT count(*),sum(input),sum(output) FROM responses",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }
    #[test]
    fn claude_last_block_and_split() {
        let (_d, db) = fixture("claude", "claude.jsonl");
        assert_eq!(usage(&db), (1, 100, 20));
        let split: (i64, i64) = db
            .db
            .query_row("SELECT write5,write1 FROM responses", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(split, (20, 10));
        let n: i64 = db
            .db
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
    #[test]
    fn codex_records_ignore_cumulative_and_archive() {
        let (_d, db) = fixture("codex", "codex.jsonl");
        assert_eq!(usage(&db), (1, 60, 20));
        let mut state = json!({});
        let c = Config::default();
        for (i, l) in include_str!("../../../../fixtures/monitor/codex.jsonl")
            .lines()
            .enumerate()
        {
            parse(
                "codex",
                &serde_json::from_str(l).unwrap(),
                i as u64,
                &mut state,
                &c,
                &db,
            )
            .unwrap();
        }
        assert_eq!(usage(&db), (1, 60, 20));
    }
    #[test]
    fn codex_responses_take_their_own_thread_as_session() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        let mut state = json!({});
        let lines = [
            json!({"timestamp":"2026-10-30T10:00:00Z","type":"session_meta","payload":{"id":"parent-session","cwd":"/example"}}),
            json!({"timestamp":"2026-10-30T10:00:01Z","type":"turn_context","payload":{"model":"gpt-6.1-sol","cwd":"/example"}}),
            json!({"timestamp":"2026-10-30T10:00:02Z","type":"token_usage_record","payload":{"response_id":"response-parent","thread_id":"parent-session","usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":10,"reasoning_output_tokens":2}}}),
            json!({"timestamp":"2026-10-30T10:00:03Z","type":"token_usage_record","payload":{"response_id":"response-child","thread_id":"child-session","usage":{"input_tokens":200,"cached_input_tokens":100,"output_tokens":20,"reasoning_output_tokens":4}}}),
            json!({"timestamp":"2026-10-30T10:00:04Z","type":"token_usage_record","payload":{"response_id":"response-bare","usage":{"input_tokens":10,"output_tokens":1}}}),
        ];
        // Replaying the rollout (a reread after the tail changed) must not add or move anything.
        for _ in 0..2 {
            for (i, v) in lines.iter().enumerate() {
                parse("codex", v, i as u64, &mut state, &Config::default(), &db).unwrap();
            }
        }
        let session = |id: &str| -> String {
            db.db
                .query_row("SELECT session FROM responses WHERE id=?", [id], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        let digest =
            |native: &str| format!("s-{}", &crate::digest(&format!("codex:{native}"))[..12]);
        assert_eq!(session("response-parent"), digest("parent-session"));
        assert_eq!(session("response-child"), digest("child-session"));
        assert_eq!(session("response-bare"), digest("parent-session"));
        assert_eq!(usage(&db), (3, 50 + 100 + 10, 31));
    }
    #[test]
    fn a_store_read_before_thread_attribution_rereads_codex_once() {
        let (dir, mut db) = fixture("codex", "codex.jsonl");
        db.db
            .execute_batch(
                "UPDATE responses SET session='s-stale'; DELETE FROM meta; DELETE FROM repairs",
            )
            .unwrap();
        let mut c = Config {
            harnesses: vec!["codex".into()],
            ..Config::default()
        };
        c.paths.insert(
            "codex".into(),
            format!(
                "{}/../../fixtures/monitor/codex.jsonl",
                env!("CARGO_MANIFEST_DIR")
            ),
        );
        collect(&c, &mut db).unwrap();
        let stale: i64 = db
            .db
            .query_row(
                "SELECT count(*) FROM responses WHERE session='s-stale'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!((stale, usage(&db)), (0, (1, 60, 20)));
        let repairs: Vec<String> = db
            .db
            .prepare("SELECT session FROM repairs")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            repairs,
            ["s-stale"],
            "the old key is kept for the app's copy"
        );
        db.db
            .execute_batch("UPDATE responses SET session='s-stale'")
            .unwrap();
        collect(&c, &mut db).unwrap();
        let kept: i64 = db
            .db
            .query_row(
                "SELECT count(*) FROM responses WHERE session='s-stale'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            kept, 1,
            "a store already at the revision resumes from its cursors"
        );
        drop(dir);
    }
    #[test]
    fn antigravity_conversations_run_by_t3_are_found_and_counted_once() {
        let home = tempfile::tempdir().unwrap();
        let roots = [
            home.path().join(".gemini/antigravity/conversations"),
            home.path()
                .join(".t3/userdata/providers/antigravity/ab12/antigravity-acp/conversations"),
        ];
        for root in &roots {
            std::fs::create_dir_all(root).unwrap();
        }
        // The same conversation copied into both roots, and one only T3 has.
        for (root, name) in [
            (&roots[0], "shared"),
            (&roots[1], "shared"),
            (&roots[1], "t3-only"),
        ] {
            protobuf::tests::conversation(&root.join(format!("{name}.db")));
        }
        let found = files_in(
            "antigravity",
            &Config::default(),
            &home.path().to_string_lossy(),
        )
        .unwrap();
        assert_eq!(found.len(), 3);
        let db = Store::open(&home.path().join("monitor.sqlite")).unwrap();
        for _ in 0..2 {
            for path in &found {
                protobuf::collect(path, &Config::default(), &db).unwrap();
            }
        }
        assert_eq!(usage(&db), (2, 200, 50));
    }
    #[test]
    fn codex_legacy_growing_only() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        let mut state = json!({"session":"s","model":"gpt-6.1-sol"});
        for total in [100, 100, 150] {
            let v = json!({"timestamp":"2026-10-30T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":total},"last_token_usage":{"input_tokens":40,"output_tokens":10}}}});
            parse("codex", &v, 0, &mut state, &Config::default(), &db).unwrap();
        }
        assert_eq!(usage(&db), (2, 80, 20));
    }
    #[test]
    fn quota_snapshots_and_live_readings_share_window_identity() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        let mut state = json!({"has_records":true});
        let v = json!({"type":"event_msg","timestamp":"2026-10-30T10:00:00Z","payload":{"type":"token_count","rate_limits":{"limit_id":"codex","primary":{"used_percent":19,"window_minutes":10080,"resets_at":1793354400}}}});
        parse("codex", &v, 0, &mut state, &Config::default(), &db).unwrap();
        db.readings(&json!({"harnesses":[{"harness":"codex","fetchedAt":"2026-10-30T10:01:00Z","windows":[{"id":"primary","kind":"weekly","percent":20}]}]})).unwrap();
        let report = crate::limits::report(&db).unwrap();
        assert_eq!(report.as_array().unwrap().len(), 1);
        assert_eq!(report[0]["percent"], 20);
    }
    #[test]
    fn gemini_updates() {
        let (_d, db) = fixture("gemini", "gemini.jsonl");
        assert_eq!(usage(&db), (1, 80, 25));
    }
    #[test]
    fn copilot_shutdown_last_wins() {
        let (_d, db) = fixture("copilot", "copilot.jsonl");
        assert_eq!(usage(&db), (1, 80, 30));
    }
    #[test]
    fn opencode_disjoint_counts() {
        let (_d, db) = fixture("opencode", "opencode.json");
        assert_eq!(usage(&db), (1, 100, 25));
    }
    fn response(db: &Store, id: &str) -> (i64, i64, i64, i64, Option<f64>, Option<i64>, bool) {
        db.db
            .query_row(
                "SELECT input,read,output,reasoning,cost,context,sidechain FROM responses WHERE id=?",
                [id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap()
    }
    #[test]
    fn grok_counts_uncached_input_and_takes_cost_only_when_reported() {
        let (_d, db) = fixture("grok", "grok");
        let first = response(&db, "2026-10-30T10:00:05.100000000+00:00:grok-4.7-build");
        assert_eq!(first, (900, 100, 50, 20, Some(0.025), None, false));
        let no_cost = response(&db, "2026-10-30T10:05:05.100000000+00:00:grok-4.7-build");
        assert_eq!(no_cost.4, None);
    }
    #[test]
    fn a_grok_fork_does_not_count_or_move_the_turns_it_copied() {
        let (_d, db) = fixture("grok", "grok");
        // parent: 2 turns, fork: those 2 copied plus 1 of its own. The subagent's 7000 is inside the parent's turn 2.
        assert_eq!(usage(&db), (3, 900 + 9000 + 300, 50 + 70 + 30));
        let sessions: i64 = db
            .db
            .query_row("SELECT count(DISTINCT session) FROM responses", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(sessions, 2);
        let parent = format!("s-{}", &crate::digest("grok:01aaaaaa-1parent")[..12]);
        let kept: i64 = db
            .db
            .query_row(
                "SELECT count(*) FROM responses WHERE session=?",
                [parent],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 2, "copied turns stay with the session that ran them");
    }
    #[test]
    fn grok_tools_count_from_events_with_the_billed_model_and_subagents_marked() {
        let (_d, db) = fixture("grok", "grok");
        let events = |kind: &str| -> i64 {
            db.db
                .query_row("SELECT count(*) FROM events WHERE kind=?", [kind], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        // Parent 2 started, one of them failed; subagent 1 started, which a parent-only count would miss.
        assert_eq!((events("toolCalls"), events("toolErrors")), (3, 1));
        let side: i64 = db
            .db
            .query_row("SELECT count(*) FROM events WHERE sidechain", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(side, 1);
        let models: i64 = db
            .db
            .query_row(
                "SELECT count(*) FROM events WHERE model<>'grok-4.7-build'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(models, 0);
    }
    #[test]
    fn grok_never_opens_a_session_file_holding_conversation_text() {
        let dir = format!("{}/../../fixtures/monitor/grok", env!("CARGO_MANIFEST_DIR"));
        let mut c = Config::default();
        c.paths.insert("grok".into(), dir);
        let names: HashSet<String> = files("grok", &c)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            HashSet::from(["usage.json".to_string(), "events.jsonl".to_string()])
        );
    }
    #[test]
    fn grok_project_comes_from_the_group_directory_or_its_cwd_file() {
        let (_d, db) = fixture("grok", "grok");
        let projects: Vec<String> = db
            .db
            .prepare("SELECT DISTINCT project FROM responses ORDER BY project")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(projects, ["grok demo"]);
        assert_eq!(url_decoded("%2Fx%2Fa%20b%zz%"), "/x/a b%zz%");
        let root = tempfile::tempdir().unwrap();
        let session = root.path().join("slug-abc123/01bbbbbb");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(root.path().join("slug-abc123/.cwd"), "/long/path/repo\n").unwrap();
        let mut state = json!({});
        grok_state(&mut state, &session.join("usage.json"), &HashSet::new());
        assert_eq!(state["cwd"], "/long/path/repo");
    }
    #[test]
    fn muse_root_usage() {
        let (_d, db) = fixture("muse", "muse.jsonl");
        assert_eq!(usage(&db), (1, 80, 30));
        let side: bool = db
            .db
            .query_row("SELECT sidechain FROM responses", [], |r| r.get(0))
            .unwrap();
        assert!(side);
    }
    #[test]
    fn reject_inconsistent_and_negative() {
        let d = tempfile::tempdir().unwrap();
        let db = Store::open(&d.path().join("db")).unwrap();
        let mut state = json!({"session":"s","model":"gpt-6.1-sol"});
        for (input, cached, output, reasoning) in [(1, 2, 3, 1), (1, 0, 3, 4), (-1, 0, 3, 1)] {
            let v = json!({"timestamp":"2026-10-30T10:00:00Z","type":"token_usage_record","payload":{"response_id":"r","usage":{"input_tokens":input,"cached_input_tokens":cached,"output_tokens":output,"reasoning_output_tokens":reasoning}}});
            assert!(!parse("codex", &v, 0, &mut state, &Config::default(), &db).unwrap());
        }
    }

    #[test]
    fn records_without_a_time_take_the_last_time_in_the_file_never_the_epoch() {
        let c = Config::default();
        let mut state = json!({});
        let timed = json!({"timestamp": "2026-10-30T10:00:00Z", "cwd": "/tmp/p", "sessionId": "s"});
        let untimed = json!({"cwd": "/tmp/p", "sessionId": "s"});
        assert_eq!(metadata("claude", &untimed, &state, &c).unwrap().at, 0);
        state["lastAt"] = json!(time(&timed["timestamp"]).unwrap());
        assert_eq!(
            metadata("claude", &untimed, &state, &c).unwrap().at,
            time(&timed["timestamp"]).unwrap()
        );
    }
}
