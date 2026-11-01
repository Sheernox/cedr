//! `cedr guard remote`: reads the tools a remote MCP server lists right now and compares them with
//! the locked surface (docs/design.md, Remote servers). A repo's config picks the URLs, so it is
//! hostile input: no credentials are ever sent, and only public HTTPS addresses are contacted.

use std::io::{BufRead, BufReader, Read};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::diff::{self, Change};
use crate::discovery::{Client as AgentClient, Transport};
use crate::lock;
use crate::source::redact_url;
use crate::surface::{Capability, Method, Surface, Tool};
use crate::{Result, canonical};

/// Test-only escape hatch, like `CEDR_TELEMETRY_URL`: allows plain HTTP to loopback addresses.
const ALLOW_LOOPBACK: &str = "CEDR_GUARD_REMOTE_ALLOW_LOOPBACK";
const PROTOCOL: &str = "2025-06-18";
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_BODY: u64 = 4 * 1024 * 1024;
const MAX_PAGES: usize = 20;
const MAX_TOOLS: usize = 2000;
pub(crate) const MAX_SERVERS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Unchanged,
    Changed,
    Failed,
    AuthRequired,
    Unsupported,
    NotPinned,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Unchanged => "unchanged",
            Status::Changed => "changed",
            Status::Failed => "failed",
            Status::AuthRequired => "auth_required",
            Status::Unsupported => "unsupported",
            Status::NotPinned => "not_pinned",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerResult {
    pub client: AgentClient,
    pub name: String,
    pub url: String,
    pub status: Status,
    pub changes: Vec<Change>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    pub at: String,
    /// What these results were compared with; see [`baseline`].
    pub baseline: String,
    pub servers: Vec<ServerResult>,
}

impl Report {
    /// 1 when a surface changed, else 2 when a server could not be read, so an outage is never
    /// mistaken for an unchanged surface; otherwise 0.
    pub fn exit_code(&self) -> u8 {
        let any = |s: &[Status]| self.servers.iter().any(|r| s.contains(&r.status));
        if any(&[Status::Changed]) {
            1
        } else if any(&[Status::Failed, Status::AuthRequired]) {
            2
        } else {
            0
        }
    }

    pub fn to_text(&self) -> String {
        if self.servers.is_empty() {
            return "no remote MCP servers in the lock\n".to_string();
        }
        let mut out = String::new();
        for r in &self.servers {
            let label = format!("{}.{}", r.client.as_str(), r.name);
            out.push_str(&format!("{label} {} {}", r.url, r.status.as_str()));
            if let Some(error) = &r.error {
                out.push_str(&format!(": {error}"));
            }
            out.push('\n');
            for c in &r.changes {
                let tool = c.tool.as_deref().unwrap_or("-");
                // Text from a server is untrusted: it must not write to the terminal as it likes.
                out.push_str(&format!(
                    "  {} {} {} {}\n",
                    c.severity.as_str(),
                    c.kind.as_str(),
                    tool.escape_debug(),
                    c.detail.escape_debug(),
                ));
            }
        }
        out
    }
}

/// A failed read with the status it reports. Messages name a host at most, never a path or query.
#[derive(Debug)]
pub(crate) struct Failure(Status, String);

fn failed(message: impl Into<String>) -> Failure {
    Failure(Status::Failed, message.into())
}

/// Names a checkout's saved results: per checkout, on this machine only.
pub fn state_key(root: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let real = std::fs::canonicalize(root).map_err(crate::error::io_err(root))?;
    let digest = crate::skill::hex(&Sha256::digest(real.to_string_lossy().as_bytes()));
    Ok(digest[..12].to_string())
}

/// A digest of the lock and its remote surfaces. Saved results stand only while it matches, so
/// switching branches or relocking inside the interval can't repeat an old `unchanged`.
pub fn baseline(root: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let (found, _) = lock::read_lock(root)?;
    let mut hash = Sha256::new();
    hash.update(found.to_toml());
    for entry in found.mcp.iter().filter(|e| e.transport == Transport::Http) {
        let file = root
            .join(lock::SURFACE_DIR)
            .join(lock::surface_file_name(entry.client, &entry.name));
        // A missing surface file hashes as empty and is reported as `failed` by the check.
        hash.update(std::fs::read(file).unwrap_or_default());
        hash.update([0]);
    }
    Ok(crate::skill::hex(&hash.finalize()))
}

/// Checks every remote server in the repo's lock, one at a time.
pub fn check(root: &Path, at: String) -> Result<Report> {
    let (found, _) = lock::read_lock(root)?;
    let baseline = baseline(root)?;
    let mut servers = Vec::new();
    for entry in found.mcp.iter().filter(|e| e.transport == Transport::Http) {
        let raw = entry.source.strip_prefix("url:").unwrap_or(&entry.source);
        let mut result = ServerResult {
            client: entry.client,
            name: entry.name.clone(),
            url: redact_url(raw),
            status: Status::NotPinned,
            changes: Vec::new(),
            error: None,
        };
        if servers.len() >= MAX_SERVERS {
            result.status = Status::Failed;
            result.error = Some(format!("not read: more than {MAX_SERVERS} remote servers"));
        } else if entry.method != Method::None {
            let file = root
                .join(lock::SURFACE_DIR)
                .join(lock::surface_file_name(entry.client, &entry.name));
            match compare(raw, &entry.source, &file) {
                Ok(changes) if changes.is_empty() => result.status = Status::Unchanged,
                Ok(changes) => {
                    result.status = Status::Changed;
                    result.changes = changes;
                }
                Err(Failure(status, message)) => {
                    result.status = status;
                    result.error = Some(message);
                }
            }
        }
        servers.push(result);
    }
    Ok(Report {
        version: 1,
        at,
        baseline,
        servers,
    })
}

fn compare(raw: &str, source: &str, locked: &Path) -> std::result::Result<Vec<Change>, Failure> {
    let old = Surface::read(locked)
        .map_err(|_| failed("the locked surface cannot be read; run cedr guard lock --remote"))?;
    let new = surface(raw, source)?;
    Ok(diff::diff(&old, &new).changes)
}

/// The surface a remote server shows now, as `cedr guard lock --remote` records it.
pub(crate) fn surface(raw: &str, source: &str) -> std::result::Result<Surface, Failure> {
    let url = target(raw, std::env::var_os(ALLOW_LOOPBACK).is_some())?;
    let host = url.host_str().unwrap_or_default().to_string();
    let (tools, capabilities) = list(&url, &host)?;
    Ok(Surface::new(
        source.to_string(),
        Method::Introspect,
        None,
        tools,
        vec![host],
        capabilities,
    ))
}

pub(crate) fn failure_text(failure: &Failure) -> &str {
    &failure.1
}

/// Refuses a URL that could carry a credential or leave public HTTPS.
fn target(raw: &str, allow_loopback: bool) -> std::result::Result<Url, Failure> {
    let unsupported = |m: &str| Failure(Status::Unsupported, m.to_string());
    let url = Url::parse(raw).map_err(|_| unsupported("the URL does not parse"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(unsupported(
            "the URL holds user info, which may be a credential",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(unsupported(
            "the URL has a query, which may hold a credential",
        ));
    }
    let plain_allowed = allow_loopback && url.scheme() == "http";
    if url.scheme() != "https" && !plain_allowed {
        return Err(unsupported("only https URLs are read"));
    }
    if url.host_str().is_none() {
        return Err(unsupported("the URL has no host"));
    }
    Ok(url)
}

/// Whether an address may be contacted: public unicast only, so a config cannot reach the machine,
/// its network or a cloud metadata service.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (64..128).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (b == 18 || b == 19)))
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(mapped));
            }
            // Global unicast only, so a range missing from a blocklist can't reach a LAN. Tunnels
            // inside it (6to4, Teredo) can wrap a private IPv4 address.
            let s = v6.segments();
            (s[0] & 0xe000) == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] == 0 || s[1] == 0x0db8))
        }
    }
}

/// Resolves once and checks every address; the client then connects only to these, so a second,
/// different DNS answer cannot move the request to a private address.
fn addresses(url: &Url, host: &str) -> std::result::Result<Vec<SocketAddr>, Failure> {
    let port = url.port_or_known_default().unwrap_or(443);
    let bare = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(
            (bare.as_str(), port)
                .to_socket_addrs()
                .map(Iterator::collect),
        );
    });
    let found: Vec<SocketAddr> = rx
        .recv_timeout(RESOLVE_TIMEOUT)
        .map_err(|_| failed(format!("{host} did not resolve in time")))?
        .map_err(|_| failed(format!("{host} did not resolve")))?;
    if found.is_empty() {
        return Err(failed(format!("{host} did not resolve")));
    }
    // Only `target`'s test switch lets a plain http URL get this far.
    let loopback_ok = url.scheme() == "http";
    for addr in &found {
        if !is_public(addr.ip()) && !(loopback_ok && addr.ip().is_loopback()) {
            return Err(failed(format!(
                "{host} is not a public address; not contacted"
            )));
        }
    }
    Ok(found)
}

fn list(url: &Url, host: &str) -> std::result::Result<(Vec<Tool>, Vec<Capability>), Failure> {
    let addrs = addresses(url, host)?;
    let client = Client::builder()
        .https_only(url.scheme() == "https")
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("cedr/", env!("CARGO_PKG_VERSION")))
        .resolve_to_addrs(host, &addrs)
        .build()
        .map_err(|_| failed("cannot build the HTTP client"))?;
    let mut session = Session {
        client: &client,
        url,
        host,
        id: None,
    };
    let init = session.call(
        1,
        "initialize",
        json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "cedr", "version": env!("CARGO_PKG_VERSION")},
        }),
    )?;
    session.notify_initialized();
    let capabilities = capabilities(&init["capabilities"]);
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..MAX_PAGES {
        let params = cursor.as_ref().map_or(json!({}), |c| json!({"cursor": c}));
        let reply = session.call(2 + page as i64, "tools/list", params)?;
        let listed = reply["tools"]
            .as_array()
            .ok_or_else(|| failed(format!("{host} sent an invalid tool list")))?;
        for tool in listed {
            tools.push(
                parse_tool(tool).ok_or_else(|| failed(format!("{host} sent an invalid tool")))?,
            );
        }
        if tools.len() > MAX_TOOLS {
            return Err(failed(format!("{host} lists more than {MAX_TOOLS} tools")));
        }
        cursor = match reply.get("nextCursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(c)) if c.is_empty() => None,
            Some(Value::String(c)) => Some(c.clone()),
            // Treating this as the last page could hide tools and read as unchanged.
            Some(_) => return Err(failed(format!("{host} sent an invalid page cursor"))),
        };
        if cursor.is_none() {
            return Ok((tools, capabilities));
        }
    }
    Err(failed(format!(
        "{host} lists more than {MAX_PAGES} pages of tools"
    )))
}

fn parse_tool(tool: &Value) -> Option<Tool> {
    Some(Tool {
        name: tool["name"].as_str()?.to_string(),
        description: tool["description"].as_str().map(String::from),
        schema: tool.get("inputSchema").map(canonical::to_string),
    })
}

fn capabilities(declared: &Value) -> Vec<Capability> {
    [
        ("prompts", Capability::McpPrompts),
        ("resources", Capability::McpResources),
        ("tools", Capability::McpTools),
    ]
    .into_iter()
    .filter(|(key, _)| declared.get(key).is_some())
    .map(|(_, capability)| capability)
    .collect()
}

struct Session<'a> {
    client: &'a Client,
    url: &'a Url,
    host: &'a str,
    id: Option<String>,
}

impl Session<'_> {
    fn post(&self, body: &Value) -> reqwest::Result<Response> {
        let mut request = self
            .client
            .post(self.url.clone())
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL);
        if let Some(id) = &self.id {
            request = request.header("mcp-session-id", id);
        }
        request.body(body.to_string()).send()
    }

    /// One JSON-RPC request; its `result`, or the failure to report.
    fn call(
        &mut self,
        id: i64,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, Failure> {
        let host = self.host;
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let response = self.post(&body).map_err(|e| {
            failed(if e.is_timeout() {
                format!("{host} did not answer in time")
            } else {
                format!("{host} could not be reached")
            })
        })?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(Failure(
                Status::AuthRequired,
                format!("{host} needs credentials ({status}); cedr sends none"),
            ));
        }
        if status.is_redirection() {
            return Err(failed(format!(
                "{host} redirected; redirects are not followed"
            )));
        }
        if !status.is_success() {
            return Err(failed(format!("{host} answered {status}")));
        }
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.id = Some(session.to_string());
        }
        let message = read_reply(response, id)
            .ok_or_else(|| failed(format!("{host} sent no answer to {method}")))?;
        if message.get("error").is_some() {
            return Err(failed(format!("{host} refused {method}")));
        }
        message
            .get("result")
            .filter(|r| r.is_object())
            .cloned()
            .ok_or_else(|| failed(format!("{host} sent no result for {method}")))
    }

    /// Servers answer 202 or nothing useful; a real problem surfaces at `tools/list`.
    fn notify_initialized(&self) {
        let _ = self.post(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }
}

/// The reply with this id, from a JSON body or an event stream, read up to the body cap. A stream
/// may stay open after the answer, so reading stops as soon as it arrives.
fn read_reply(response: Response, id: i64) -> Option<Value> {
    let stream = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("text/event-stream"));
    let limited = response.take(MAX_BODY);
    if stream {
        return find_in_stream(BufReader::new(limited), id);
    }
    serde_json::from_reader::<_, Value>(limited)
        .ok()
        .filter(|m| m["id"].as_i64() == Some(id))
}

/// An event's `data:` lines join with newlines and end at a blank line, so a message may span lines.
fn find_in_stream(reader: impl BufRead, id: i64) -> Option<Value> {
    let mut data = String::new();
    for line in reader.lines().map_while(std::result::Result::ok) {
        if line.is_empty() {
            if let Ok(message) = serde_json::from_str::<Value>(&data)
                && message["id"].as_i64() == Some(id)
            {
                return Some(message);
            }
            data.clear();
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    serde_json::from_str::<Value>(&data)
        .ok()
        .filter(|m| m["id"].as_i64() == Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_message_may_span_data_lines() {
        let body = "event: ping\ndata: {\"id\":1}\n\ndata: {\"id\":2,\ndata: \"result\":{}}\n\n";
        assert_eq!(
            find_in_stream(body.as_bytes(), 2),
            Some(json!({"id": 2, "result": {}}))
        );
        assert_eq!(find_in_stream(body.as_bytes(), 3), None);
    }

    #[test]
    fn only_public_addresses_may_be_contacted() {
        for private in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "2001::1",
            "::ffff:10.0.0.1",
            "2001:db8::1",
            "64:ff9b::a00:1",
            "2002:a00:1::",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in [
            "1.1.1.1",
            "8.8.8.8",
            "2606:4700:4700::1111",
            "::ffff:1.1.1.1",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn a_url_that_could_carry_a_credential_or_leave_https_is_refused() {
        for raw in [
            "http://mcp.example.net/",
            "https://someone@localhost/",
            "https://mcp.example.net/?key=abc",
            "https://mcp.example.net/#t",
            "not a url",
        ] {
            let err = target(raw, false).unwrap_err();
            assert_eq!(err.0, Status::Unsupported, "{raw}");
            assert!(!err.1.contains("abc") && !err.1.contains("someone"));
        }
        assert!(target("https://mcp.example.net/mcp", false).is_ok());
        assert!(target("http://127.0.0.1:1/mcp", true).is_ok());
    }

    #[test]
    fn plain_http_with_the_test_switch_still_reaches_only_loopback() {
        let url = target("http://10.0.0.1:1/mcp", true).unwrap();
        assert!(addresses(&url, "10.0.0.1").is_err());
    }

    #[test]
    fn a_failed_read_exits_two_and_a_change_wins_over_it() {
        let result = |status| ServerResult {
            client: AgentClient::Claude,
            name: "s".into(),
            url: "u".into(),
            status,
            changes: vec![],
            error: None,
        };
        let report = |s: Vec<Status>| Report {
            version: 1,
            at: String::new(),
            baseline: String::new(),
            servers: s.into_iter().map(result).collect(),
        };
        assert_eq!(
            report(vec![Status::Unchanged, Status::NotPinned]).exit_code(),
            0
        );
        assert_eq!(
            report(vec![Status::Unchanged, Status::Failed]).exit_code(),
            2
        );
        assert_eq!(report(vec![Status::AuthRequired]).exit_code(), 2);
        assert_eq!(report(vec![Status::Failed, Status::Changed]).exit_code(), 1);
    }
}
