//! `cedr route sync` against a stand-in for the app API on localhost: resumable pushes, shared
//! routes pulled and used, refused shared files, and copies removed when the org stops sharing.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use common::{Scratch, config, stdout};
use serde_json::{Value, json};

/// One recorded request: method, path with query, authorization header, body.
type Seen = Arc<Mutex<Vec<(String, String, String, String)>>>;

/// Answers each request with the next (status, body) from `replies`, recording what it saw.
/// Machine snapshots are answered `{"ok": true}` without using a reply and are left out of `seen`.
fn serve(replies: Vec<(u16, String)>) -> (String, Seen) {
    let (url, seen, _) = serve_all(replies, 200);
    (url, seen)
}

/// `serve`, plus the machine snapshots it received, each answered with `machine_status`.
fn serve_all(replies: Vec<(u16, String)>, machine_status: u16) -> (String, Seen, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://localhost:{}/api/v1",
        listener.local_addr().unwrap().port()
    );
    let seen: Seen = Arc::default();
    let machines: Seen = Arc::default();
    let log = seen.clone();
    let machine_log = machines.clone();
    std::thread::spawn(move || {
        let mut replies = replies.into_iter();
        loop {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let (mut auth, mut length) = (String::new(), 0usize);
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                match k.to_ascii_lowercase().as_str() {
                    "authorization" => auth = v.trim().to_string(),
                    "content-length" => length = v.trim().parse().unwrap(),
                    _ => {}
                }
            }
            let mut buf = vec![0; length];
            reader.read_exact(&mut buf).unwrap();
            let mut parts = request.split_whitespace();
            let (method, path) = (parts.next().unwrap(), parts.next().unwrap());
            let record = (
                method.into(),
                path.into(),
                auth,
                String::from_utf8(buf).unwrap(),
            );
            let (status, body) = if path.ends_with("/route/machine") {
                machine_log.lock().unwrap().push(record);
                (
                    machine_status,
                    json!({"ok": machine_status == 200}).to_string(),
                )
            } else {
                log.lock().unwrap().push(record);
                replies
                    .next()
                    .unwrap_or((500, json!({"error": "no reply left"}).to_string()))
            };
            // A redirect points at plain HTTP on another host, which sync must refuse to follow.
            let location = if status == 307 {
                "location: http://example.invalid/steal\r\n"
            } else {
                ""
            };
            write!(stream, "HTTP/1.1 {status} X\r\n{location}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (url, seen, machines)
}

/// A local routes.toml that syncs with `url` for org `acme`, routing with `routes`.
fn local_config(s: &Scratch, url: &str, routes: &str) -> String {
    let path = s.dir.path().join("routes.toml");
    let text = std::fs::read_to_string(config()).unwrap();
    std::fs::write(
        &path,
        format!(
            "{text}\n[sync]\norg = \"acme\"\nroutes = \"{routes}\"\nurl = \"{url}\"\nauto = false\n"
        ),
    )
    .unwrap();
    path.display().to_string()
}

#[test]
fn pushes_resume_after_the_last_accepted_record() {
    let s = Scratch::new(10.0, 20.0);
    let ok = json!({"accepted": 1, "skipped": 0}).to_string();
    let (url, seen) = serve(vec![(200, ok.clone()), (200, ok)]);
    let cfg = local_config(&s, &url, "local");
    let readings = s.readings();
    assert_eq!(
        s.cedr(&[
            "route",
            "pick",
            "review.code",
            "--config",
            &cfg,
            "--readings",
            &readings
        ])
        .status
        .code(),
        Some(0)
    );
    let push = |s: &Scratch| {
        s.command(&["route", "sync", "--push-only", "--config", &cfg])
            .env("CEDR_API_KEY", "cedr_test")
            .output()
            .unwrap()
    };
    let first = push(&s);
    assert_eq!(
        first.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(stdout(&first).starts_with("pushed 1 records to acme (0 skipped)\n"));
    let again = push(&s);
    assert!(
        stdout(&again).starts_with("pushed 0 records to acme (0 skipped)\n"),
        "nothing new: no request"
    );
    assert_eq!(
        s.cedr(&[
            "route",
            "pick",
            "review.code",
            "--config",
            &cfg,
            "--readings",
            &readings
        ])
        .status
        .code(),
        Some(0)
    );
    push(&s);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let (method, path, auth, body) = &seen[0];
    assert_eq!(
        (method.as_str(), path.as_str(), auth.as_str()),
        ("POST", "/api/v1/route/history", "Bearer cedr_test")
    );
    let body: Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        (body["org"].as_str(), body["records"][0]["type"].as_str()),
        (Some("acme"), Some("pick"))
    );
    let second: Value = serde_json::from_str(&seen[1].3).unwrap();
    assert_eq!(
        second["records"].as_array().unwrap().len(),
        1,
        "only the new pick"
    );
}

#[test]
fn a_failed_push_names_the_api_error_and_keeps_the_records() {
    let s = Scratch::new(10.0, 20.0);
    let (url, seen) = serve(vec![
        (403, json!({"error": "not a member of acme"}).to_string()),
        (200, json!({"accepted": 1, "skipped": 0}).to_string()),
    ]);
    let cfg = local_config(&s, &url, "local");
    let readings = s.readings();
    s.cedr(&[
        "route",
        "pick",
        "review.code",
        "--config",
        &cfg,
        "--readings",
        &readings,
    ]);
    let push = || {
        s.command(&["route", "sync", "--push-only", "--config", &cfg])
            .env("CEDR_API_KEY", "cedr_test")
            .output()
            .unwrap()
    };
    let failed = push();
    assert_eq!(failed.status.code(), Some(2));
    let err = String::from_utf8_lossy(&failed.stderr);
    assert!(
        err.contains("sync push: HTTP 403") && err.contains("not a member of acme"),
        "{err}"
    );
    assert!(!err.contains("cedr_test"));
    assert!(stdout(&push()).starts_with("pushed 1 records to acme (0 skipped)\n"));
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[test]
fn pulled_shared_routes_drive_picks_until_the_org_stops_sharing() {
    let s = Scratch::new(40.0, 20.0);
    let shared = std::fs::read_to_string(config()).unwrap().replace(
        "{ target = \"claude/sonnet\", effort = \"high\", tier = 1 },\n",
        "",
    );
    let (url, seen) = serve(vec![
        (200, json!({"org": "acme", "routes": shared, "rules": "x", "updatedAt": "2026-10-29T07:00:00Z", "updatedBy": "Sam"}).to_string()),
        (404, json!({"error": "no_routes"}).to_string()),
    ]);
    let cfg = local_config(&s, &url, "org");
    let readings = s.readings();
    let before = s.cedr(&[
        "route",
        "pick",
        "review.code",
        "--config",
        &cfg,
        "--readings",
        &readings,
    ]);
    assert_eq!(before.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&before.stderr).contains("run `cedr route sync`"));
    let pull = || {
        s.command(&["route", "sync", "--pull-only", "--config", &cfg])
            .env("CEDR_API_KEY", "cedr_test")
            .output()
            .unwrap()
    };
    let pulled = pull();
    assert!(
        stdout(&pulled).contains("by Sam"),
        "{}{}",
        stdout(&pulled),
        String::from_utf8_lossy(&pulled.stderr)
    );
    assert_eq!(seen.lock().unwrap()[0].1, "/api/v1/route/routes?org=acme");
    let shared_pick = stdout(&s.cedr(&[
        "route",
        "pick",
        "review.code",
        "--config",
        &cfg,
        "--readings",
        &readings,
    ]));
    assert!(
        shared_pick.contains("-> codex/gpt-6.1-sol high | primary |"),
        "the shared routes dropped claude: {shared_pick}"
    );
    assert_eq!(stdout(&pull()), "acme has no shared routes\n");
    assert!(
        !s.dir
            .path()
            .join("config/cedr/org-routes/acme.toml")
            .exists()
    );
}

#[test]
fn shared_routes_that_would_run_commands_are_refused() {
    let s = Scratch::new(10.0, 20.0);
    let shared = format!(
        "{}\n[[readings]]\nkind = \"command\"\ncommand = [\"sh\", \"-c\", \"curl evil\"]\n",
        std::fs::read_to_string(config()).unwrap()
    );
    let (url, _) = serve(vec![(
        200,
        json!({"org": "acme", "routes": shared}).to_string(),
    )]);
    let cfg = local_config(&s, &url, "org");
    let o = s
        .command(&["route", "sync", "--pull-only", "--config", &cfg])
        .env("CEDR_API_KEY", "cedr_test")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("cannot contain [[readings]]"));
    assert!(
        !s.dir
            .path()
            .join("config/cedr/org-routes/acme.toml")
            .exists()
    );
}

#[test]
fn a_key_file_others_can_read_is_refused() {
    let s = Scratch::new(10.0, 20.0);
    let cfg = local_config(&s, "https://api.cedr.dev/v1", "local");
    let key = s.dir.path().join("config/cedr/api-key");
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    std::fs::write(&key, "cedr_secret\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let o = s.cedr(&["route", "sync", "--config", &cfg]);
    assert_eq!(o.status.code(), Some(2));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("chmod 600") && !err.contains("cedr_secret"),
        "{err}"
    );
}

#[test]
fn a_local_file_may_read_for_harnesses_only_the_shared_routes_define() {
    let s = Scratch::new(10.0, 20.0);
    let shared = std::fs::read_to_string(config()).unwrap();
    let (url, _) = serve(vec![(
        200,
        json!({"org": "acme", "routes": shared}).to_string(),
    )]);
    let local = s.dir.path().join("routes.toml");
    std::fs::write(&local, format!("version = 1\n[[readings]]\nkind = \"claude-statusline\"\n[sync]\norg = \"acme\"\nroutes = \"org\"\nurl = \"{url}\"\nauto = false\n")).unwrap();
    let cfg = local.display().to_string();
    let pulled = s
        .command(&["route", "sync", "--pull-only", "--config", &cfg])
        .env("CEDR_API_KEY", "cedr_test")
        .output()
        .unwrap();
    assert_eq!(
        pulled.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&pulled.stderr)
    );
    let readings = s.readings();
    let pick = s.cedr(&[
        "route",
        "pick",
        "review.code",
        "--config",
        &cfg,
        "--readings",
        &readings,
    ]);
    assert_eq!(
        pick.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&pick.stderr)
    );
}

#[test]
fn incomplete_acknowledgements_keep_the_records_for_next_time() {
    let s = Scratch::new(10.0, 20.0);
    let (url, seen) = serve(vec![
        (200, "{}".to_string()),
        (200, json!({"accepted": 0, "skipped": 0}).to_string()),
        (200, json!({"accepted": 1, "skipped": 0}).to_string()),
    ]);
    let cfg = local_config(&s, &url, "local");
    let readings = s.readings();
    s.cedr(&[
        "route",
        "pick",
        "review.code",
        "--config",
        &cfg,
        "--readings",
        &readings,
    ]);
    let push = || {
        s.command(&["route", "sync", "--push-only", "--config", &cfg])
            .env("CEDR_API_KEY", "cedr_test")
            .output()
            .unwrap()
    };
    assert!(String::from_utf8_lossy(&push().stderr).contains("no accepted and skipped counts"));
    assert!(String::from_utf8_lossy(&push().stderr).contains("accounted for 0 of 1 records"));
    assert!(stdout(&push()).starts_with("pushed 1 records to acme (0 skipped)\n"));
    let seen = seen.lock().unwrap();
    assert!(seen.iter().all(|r| {
        serde_json::from_str::<Value>(&r.3).unwrap()["records"]
            .as_array()
            .unwrap()
            .len()
            == 1
    }));
}

#[test]
fn redirects_off_https_are_refused_and_echoed_keys_are_redacted() {
    let s = Scratch::new(10.0, 20.0);
    let (url, _) = serve(vec![
        (307, String::new()),
        (
            401,
            json!({"error": "invalid_api_key", "message": "bad key cedr_test"}).to_string(),
        ),
    ]);
    let cfg = local_config(&s, &url, "local");
    let readings = s.readings();
    s.cedr(&[
        "route",
        "pick",
        "review.code",
        "--config",
        &cfg,
        "--readings",
        &readings,
    ]);
    let push = || {
        s.command(&["route", "sync", "--push-only", "--config", &cfg])
            .env("CEDR_API_KEY", "cedr_test")
            .output()
            .unwrap()
    };
    let redirected = String::from_utf8_lossy(&push().stderr).into_owned();
    assert!(redirected.contains("not https://"), "{redirected}");
    let echoed = String::from_utf8_lossy(&push().stderr).into_owned();
    assert!(
        echoed.contains("bad key [redacted]") && !echoed.contains("cedr_test"),
        "{echoed}"
    );
}

#[test]
fn only_plan_required_removes_the_copy_and_other_refusals_fail() {
    let s = Scratch::new(10.0, 20.0);
    let shared = std::fs::read_to_string(config()).unwrap();
    let (url, _) = serve(vec![
        (200, json!({"org": "acme", "routes": shared}).to_string()),
        (403, json!({"error": "not_a_member", "message": "You are not a member of an organization named acme."}).to_string()),
        (403, json!({"error": "plan_required", "message": "no"}).to_string()),
    ]);
    let cfg = local_config(&s, &url, "org");
    let pull = || {
        s.command(&["route", "sync", "--pull-only", "--config", &cfg])
            .env("CEDR_API_KEY", "cedr_test")
            .output()
            .unwrap()
    };
    let copy = s.dir.path().join("config/cedr/org-routes/acme.toml");
    assert_eq!(pull().status.code(), Some(0));
    let refused = pull();
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("not_a_member"));
    assert!(
        copy.exists(),
        "a refusal that is not about the plan keeps the copy"
    );
    assert_eq!(
        stdout(&pull()),
        "acme's plan does not include saved routes (Pro and up)\n"
    );
    assert!(!copy.exists());
}

#[test]
fn a_pick_starts_a_background_sync_that_sends_the_machine_snapshot() {
    let s = Scratch::new(10.0, 20.0);
    let ok = json!({"accepted": 1, "skipped": 0}).to_string();
    let (url, seen, machines) = serve_all(
        vec![(200, ok), (404, json!({"error": "no_routes"}).to_string())],
        200,
    );
    let cfg = s.dir.path().join("routes.toml");
    let text = std::fs::read_to_string(config()).unwrap();
    std::fs::write(
        &cfg,
        format!("{text}\n[[readings]]\nkind = \"file\"\npath = \"readings.json\"\n\n[machine]\nname = \"laptop\"\nskip = [\"codex\"]\n\n[sync]\norg = \"acme\"\nurl = \"{url}\"\n"),
    )
    .unwrap();
    let cfg = cfg.display().to_string();
    let pick = s
        .command(&["route", "pick", "review.code", "--config", &cfg])
        .env("CEDR_API_KEY", "cedr_test")
        .output()
        .unwrap();
    assert_eq!(
        pick.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&pick.stderr)
    );
    assert!(
        stdout(&pick).contains("fallback:codex=absent"),
        "{}",
        stdout(&pick)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while machines.lock().unwrap().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "no background sync arrived: {:?}",
            std::fs::read_to_string(s.dir.path().join("state/cedr/route-sync.log"))
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "the pick was pushed and the routes pulled first"
    );
    let (method, _, auth, body) = machines.lock().unwrap()[0].clone();
    assert_eq!(
        (method.as_str(), auth.as_str()),
        ("PUT", "Bearer cedr_test")
    );
    let snap: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (
            snap["org"].as_str(),
            snap["name"].as_str(),
            snap["routes"].as_str()
        ),
        (Some("acme"), Some("laptop"), Some("local"))
    );
    let id = snap["machine"].as_str().unwrap();
    assert!(id.len() == 14 && id.starts_with("m-"), "{id}");
    let codex = snap["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["harness"] == "codex")
        .unwrap();
    // The app validates these exact keys (cedr.dev app/src/lib/routing/machines.ts).
    let keys: Vec<&str> = codex
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["family", "harness", "state", "summary", "use"]);
    assert_eq!(
        (codex["state"].as_str(), codex["summary"].as_str()),
        (Some("absent"), Some("codex skipped on this machine"))
    );
    let review = snap["next"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["route"] == "review.code")
        .unwrap();
    assert_eq!(review["targets"], json!(["claude/claude-sonnet-5-5"]));
    let design = snap["next"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["route"] == "design")
        .unwrap();
    assert_eq!(
        (
            design["targets"].as_array().unwrap().len(),
            design["reason"].as_str()
        ),
        (0, Some("orchestrator-only route"))
    );
}

#[test]
fn the_snapshot_never_fails_a_sync_that_pushed_and_pulled() {
    let s = Scratch::new(10.0, 20.0);
    let (url, _, machines) = serve_all(
        vec![
            (404, json!({"error": "no_routes"}).to_string()),
            (404, json!({"error": "no_routes"}).to_string()),
        ],
        400,
    );
    let sync = |routes: &str| {
        let cfg = local_config(&s, &url, routes);
        s.command(&["route", "sync", "--config", &cfg])
            .env("CEDR_API_KEY", "cedr_test")
            .output()
            .unwrap()
    };
    let refused = sync("local");
    assert_eq!(refused.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("plan state was not sent: sync snapshot: HTTP 400"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    // An org that shares no routes leaves a machine routing with them nothing to report.
    let unshared = sync("org");
    assert_eq!(
        unshared.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&unshared.stderr)
    );
    assert!(stdout(&unshared).contains("no plan state sent: acme shares no routes"));
    assert_eq!(
        machines.lock().unwrap().len(),
        1,
        "only the local machine tried"
    );
}
