//! `cedr update` (docs/update.md) against a loopback release server: a copy of cedr in a temp
//! directory updates itself to a fake release whose binary is a script, and refuses archives that
//! do not match SHA256SUMS or binaries that report the wrong version.
#![cfg(all(unix, any(target_arch = "x86_64", target_arch = "aarch64")))]

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn asset() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "cedr-linux-x86_64",
        ("linux", "aarch64") => "cedr-linux-arm64",
        ("macos", "x86_64") => "cedr-macos-x86_64",
        ("macos", "aarch64") => "cedr-macos-arm64",
        other => panic!("no asset for {other:?}"),
    }
}

/// A tar.gz holding `<asset>/cedr`, a script that reports `reports` for --version.
fn archive(reports: &str) -> Vec<u8> {
    let script = format!("#!/bin/sh\necho '{reports}'\n");
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    let mut header = tar::Header::new_gnu();
    header.set_size(script.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, format!("{}/cedr", asset()), script.as_bytes())
        .unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Serves `routes` (path to body) until the test ends; anything else is a 404.
fn serve(routes: HashMap<String, Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let routes = Arc::new(routes);
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(socket) = socket else { continue };
            let routes = Arc::clone(&routes);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                        break;
                    }
                }
                let path = line.split(' ').nth(1).unwrap_or_default();
                let mut socket = socket;
                match routes.get(path) {
                    Some(body) => {
                        let _ = write!(
                            socket,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(body);
                    }
                    None => {
                        let _ = write!(
                            socket,
                            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                    }
                }
            });
        }
    });
    url
}

fn release(version: &str, archive: Vec<u8>, sum: Option<String>) -> HashMap<String, Vec<u8>> {
    let name = format!("{}.tar.gz", asset());
    let sum = sum.unwrap_or_else(|| sha256(&archive));
    let tag = format!(r#"{{"tag_name":"v{version}"}}"#).into_bytes();
    HashMap::from([
        ("/releases/latest".to_string(), tag.clone()),
        (format!("/releases/tags/v{version}"), tag),
        (
            format!("/releases/download/v{version}/SHA256SUMS"),
            format!("{sum}  {name}\n").into_bytes(),
        ),
        (format!("/releases/download/v{version}/{name}"), archive),
    ])
}

/// A copy of cedr in its own writable directory, as an archive install is.
fn installed() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("cedr");
    std::fs::copy(env!("CARGO_BIN_EXE_cedr"), &exe).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, exe)
}

fn run(exe: &Path, home: &Path, url: &str, args: &[&str], auto: bool) -> Output {
    let mut c = Command::new(exe);
    c.args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "")
        .env("CEDR_TELEMETRY", "0")
        .env("CEDR_RELEASE_URL", url);
    if !auto {
        c.env("CEDR_NO_AUTO_UPDATE", "1");
    }
    output(&mut c)
}

/// Runs a command, retrying while its binary is "text file busy": another test thread forking
/// while this one's copy of cedr was still open for writing leaves a descriptor to it in that child.
fn output(c: &mut Command) -> Output {
    for _ in 0..100 {
        match c.output() {
            Err(e) if e.raw_os_error() == Some(26) => std::thread::sleep(Duration::from_millis(20)),
            result => return result.unwrap(),
        }
    }
    panic!("the copied cedr stayed busy");
}

#[test]
fn update_installs_the_checked_release_in_place() {
    let url = serve(release("99.0.0", archive("cedr 99.0.0"), None));
    let (dir, exe) = installed();
    let home = tempfile::tempdir().unwrap();

    let check = run(
        &exe,
        home.path(),
        &url,
        &["update", "--check", "--format", "json"],
        false,
    );
    let v: Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(v["latest"], "99.0.0");
    assert_eq!(v["updated"], false);
    assert_eq!(v["install"], "archive");

    let out = run(&exe, home.path(), &url, &["update"], false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("-> 99.0.0"));
    let replaced = output(Command::new(&exe).arg("--version"));
    assert_eq!(
        String::from_utf8_lossy(&replaced.stdout).trim(),
        "cedr 99.0.0"
    );
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["cedr"], "no staging files left behind");
}

#[test]
fn a_mismatched_checksum_or_wrong_binary_changes_nothing() {
    for (routes, expect) in [
        (
            release("99.0.0", archive("cedr 99.0.0"), Some("0".repeat(64))),
            "does not match",
        ),
        (
            release("99.0.0", archive("cedr 1.0.0"), None),
            "not cedr 99.0.0",
        ),
    ] {
        let url = serve(routes);
        let (dir, exe) = installed();
        let before = std::fs::read(&exe).unwrap();
        let home = tempfile::tempdir().unwrap();
        let out = run(&exe, home.path(), &url, &["update"], false);
        assert_eq!(out.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(expect),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(std::fs::read(&exe).unwrap(), before);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[test]
fn the_daily_background_update_records_what_it_did() {
    let url = serve(release("99.0.0", archive("cedr 99.0.0"), None));
    let (_dir, exe) = installed();
    let home = tempfile::tempdir().unwrap();
    run(&exe, home.path(), &url, &["lint", "."], true);
    let result = home.path().join(".local/state/cedr/update.json");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !result.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let v: Value = serde_json::from_slice(&std::fs::read(&result).unwrap()).unwrap();
    assert_eq!(v["to"], "99.0.0");
    assert_eq!(v["from"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn no_background_update_when_turned_off() {
    let url = serve(release("99.0.0", archive("cedr 99.0.0"), None));
    let (_dir, exe) = installed();
    let home = tempfile::tempdir().unwrap();
    let off = run(&exe, home.path(), &url, &["update", "--auto", "off"], false);
    assert!(String::from_utf8_lossy(&off.stdout).contains("automatic updates: off"));
    run(&exe, home.path(), &url, &["lint", "."], true);
    assert!(!home.path().join(".local/state/cedr/update-check").exists());
}

#[test]
fn a_second_updater_waits_for_the_first() {
    let url = serve(release("99.0.0", archive("cedr 99.0.0"), None));
    let (_dir, exe) = installed();
    let home = tempfile::tempdir().unwrap();
    let state = home.path().join(".local/state/cedr");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("update.lock"), "held").unwrap();
    let before = std::fs::read(&exe).unwrap();
    let out = run(&exe, home.path(), &url, &["update"], false);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("another cedr update is running"));
    assert_eq!(std::fs::read(&exe).unwrap(), before);
}
