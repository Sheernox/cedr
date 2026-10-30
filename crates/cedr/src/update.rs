//! `cedr update` and the daily background update (docs/update.md): find the release, check the
//! archive against the release's SHA256SUMS, confirm the new binary starts, then swap it in. Any
//! failure leaves the running binary as it was.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use clap::ValueEnum;
use semver::Version;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::settings::{self, Settings};

const REPO: &str = "https://github.com/Sheernox/cedr";
const API: &str = "https://api.github.com/repos/Sheernox/cedr";
const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
/// Longer than a lookup, two downloads and a test run at their timeouts.
const LOCK_STALE: Duration = Duration::from_secs(600);
const ARCHIVE_MAX: u64 = 64 * 1024 * 1024;
const RESULT: &str = "update.json";
const CARGO_FIX: &str = "cargo install --locked --git https://github.com/Sheernox/cedr cedr";

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Report the latest release without downloading it
    #[arg(long, conflicts_with_all = ["target", "auto"])]
    check: bool,
    /// Install this release (X.Y.Z), newer or older
    #[arg(long = "version", value_name = "X.Y.Z", conflicts_with = "auto")]
    target: Option<String>,
    /// Turn the daily automatic update on or off
    #[arg(long, value_enum)]
    auto: Option<Toggle>,
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
    /// Run the daily check (started in the background)
    #[arg(long, hide = true)]
    background: bool,
}

impl Args {
    pub(crate) fn background(&self) -> bool {
        self.background
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Toggle {
    On,
    Off,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Format {
    Text,
    Json,
}

/// How this binary was installed, which decides whether cedr may replace it.
#[derive(Debug, PartialEq)]
enum Install {
    Archive(PathBuf),
    Cargo,
    ReadOnly(PathBuf),
}

impl Install {
    fn name(&self) -> &'static str {
        match self {
            Self::Archive(_) => "archive",
            Self::Cargo => "cargo",
            Self::ReadOnly(_) => "read-only",
        }
    }

    fn fix(&self) -> Option<String> {
        match self {
            Self::Archive(_) => None,
            Self::Cargo => Some(CARGO_FIX.into()),
            Self::ReadOnly(dir) => Some(format!(
                "{} is not writable: rerun cedr update with permission to write there, or reinstall cedr to a directory you own",
                dir.display()
            )),
        }
    }
}

fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the package version is semver")
}

pub(crate) fn run(args: Args) -> Result<ExitCode, String> {
    if args.background {
        background();
        return Ok(ExitCode::SUCCESS);
    }
    let mut settings = Settings::load();
    if let Some(toggle) = args.auto {
        settings.set_bool("auto_update", matches!(toggle, Toggle::On));
        settings.save()?;
        println!(
            "automatic updates: {}",
            if settings.auto_update() { "on" } else { "off" }
        );
        return Ok(ExitCode::SUCCESS);
    }
    let exe = running_exe()?;
    let install = install_kind(&exe);
    let releases = Releases::new()?;
    let target = match &args.target {
        Some(v) => Some(
            Version::parse(v.trim_start_matches('v'))
                .map_err(|_| format!("update: {v:?} is not a version like 0.5.0"))?,
        ),
        None => None,
    };
    let latest = match &target {
        Some(v) => releases.tag(v)?,
        None => releases.latest()?,
    };
    let current = current();
    let wanted = match &target {
        Some(v) => *v != current,
        None => latest > current,
    };
    let mut updated = false;
    if wanted && !args.check {
        let Install::Archive(dir) = &install else {
            report(&args, &settings, &current, &latest, false, &install);
            return Ok(ExitCode::from(2));
        };
        let Ok(Some(_lock)) = cedr_route::sync::state_lock("update.lock", LOCK_STALE) else {
            return Err("update: another cedr update is running; try again in a minute".into());
        };
        releases.install(&latest, &exe, dir)?;
        updated = true;
    }
    report(&args, &settings, &current, &latest, updated, &install);
    Ok(ExitCode::SUCCESS)
}

fn report(
    args: &Args,
    settings: &Settings,
    current: &Version,
    latest: &Version,
    updated: bool,
    install: &Install,
) {
    let auto = settings.auto_update() && !settings::env_set("CEDR_NO_AUTO_UPDATE");
    let newer = latest != current && !updated;
    let fix = newer.then(|| install.fix()).flatten();
    if args.format == Format::Json {
        println!(
            "{}",
            json!({
                "current": current.to_string(),
                "latest": latest.to_string(),
                "updated": updated,
                "auto": auto,
                "install": install.name(),
                "fix": fix,
            })
        );
        return;
    }
    if updated {
        println!("cedr: updated {current} -> {latest}");
    } else if !newer {
        println!("cedr: {current} is the latest");
    } else if let Some(fix) = &fix {
        eprintln!(
            "cedr: {latest} is available ({current} installed); cedr cannot replace this install"
        );
        eprintln!("cedr: {fix}");
    } else {
        println!("cedr: {latest} is available ({current} installed); run cedr update");
    }
    if !updated {
        println!("automatic updates: {}", if auto { "on" } else { "off" });
    }
}

fn running_exe() -> Result<PathBuf, String> {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("update: cannot find the running cedr ({e})"))
}

fn install_kind(exe: &Path) -> Install {
    // Cargo's default home is under the user's profile, which Windows names USERPROFILE.
    let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let cargo_home = std::env::var_os("CARGO_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os(home).unwrap_or_default()).join(".cargo")
        });
    install_kind_in(exe, &cargo_home)
}

fn install_kind_in(exe: &Path, cargo_home: &Path) -> Install {
    let Some(dir) = exe.parent() else {
        return Install::ReadOnly(exe.to_path_buf());
    };
    if cargo_home
        .join("bin")
        .canonicalize()
        .is_ok_and(|bin| bin == dir)
    {
        return Install::Cargo;
    }
    let probe = dir.join(format!(".cedr-write-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Install::Archive(dir.to_path_buf())
        }
        Err(_) => Install::ReadOnly(dir.to_path_buf()),
    }
}

/// Where releases are looked up and downloaded; `CEDR_RELEASE_URL` replaces both for tests.
struct Releases {
    lookup: String,
    download: String,
}

impl Releases {
    fn new() -> Result<Self, String> {
        match std::env::var("CEDR_RELEASE_URL") {
            Ok(url) if !url.is_empty() => {
                let base = crate::net::checked_base(&url)
                    .map_err(|e| format!("update: CEDR_RELEASE_URL: {e}"))?;
                Ok(Self {
                    lookup: format!("{base}/releases"),
                    download: format!("{base}/releases/download"),
                })
            }
            _ => Ok(Self {
                lookup: format!("{API}/releases"),
                download: format!("{REPO}/releases/download"),
            }),
        }
    }

    fn latest(&self) -> Result<Version, String> {
        let v = self.lookup_version(&format!("{}/latest", self.lookup))?;
        if !v.pre.is_empty() {
            return Err(format!("update: the latest release {v} is a pre-release"));
        }
        Ok(v)
    }

    fn tag(&self, v: &Version) -> Result<Version, String> {
        self.lookup_version(&format!("{}/tags/v{v}", self.lookup))
    }

    fn lookup_version(&self, url: &str) -> Result<Version, String> {
        let client = crate::net::client(Duration::from_secs(15)).map_err(lookup_err)?;
        let body = crate::net::get(&client, url, 1024 * 1024).map_err(lookup_err)?;
        let release: Value =
            serde_json::from_slice(&body).map_err(|_| lookup_err("invalid JSON"))?;
        let tag = release["tag_name"].as_str().unwrap_or_default();
        Version::parse(tag.trim_start_matches('v'))
            .map_err(|_| lookup_err(format!("unexpected tag {:?}", crate::common::clip(tag, 64))))
    }

    /// Downloads, checks and swaps in `version` over `exe`, which lives in the writable `dir`.
    fn install(&self, version: &Version, exe: &Path, dir: &Path) -> Result<(), String> {
        let asset = asset()?;
        let archive_name = format!("{asset}{}", archive_ext());
        let client = crate::net::client(Duration::from_secs(120)).map_err(download_err)?;
        let base = format!("{}/v{version}", self.download);
        let sums = crate::net::get(&client, &format!("{base}/SHA256SUMS"), 64 * 1024)
            .map_err(|e| download_err(format!("SHA256SUMS: {e}")))?;
        let archive = crate::net::get(&client, &format!("{base}/{archive_name}"), ARCHIVE_MAX)
            .map_err(|e| download_err(format!("{archive_name}: {e}")))?;
        verify_sum(&sums, &archive_name, &archive)?;
        let binary = extract(&archive, asset, dir)?;
        let tmp = dir.join(format!(
            ".cedr-update-{}{}",
            std::process::id(),
            std::env::consts::EXE_SUFFIX
        ));
        let staged = stage(&tmp, &binary, version).and_then(|()| swap(&tmp, exe));
        if staged.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        staged
    }
}

fn lookup_err(e: impl std::fmt::Display) -> String {
    format!("update: release lookup failed: {e}")
}

fn download_err(e: impl std::fmt::Display) -> String {
    format!("update: download failed: {e}")
}

/// This platform's archive name without its extension, as the release workflow names it.
fn asset() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("cedr-linux-x86_64"),
        ("linux", "aarch64") => Ok("cedr-linux-arm64"),
        ("macos", "x86_64") => Ok("cedr-macos-x86_64"),
        ("macos", "aarch64") => Ok("cedr-macos-arm64"),
        ("windows", "x86_64") => Ok("cedr-windows-x86_64"),
        (os, arch) => Err(format!(
            "update: no release archive for {os} {arch}; build from source with {CARGO_FIX}"
        )),
    }
}

fn archive_ext() -> &'static str {
    if cfg!(windows) { ".zip" } else { ".tar.gz" }
}

fn verify_sum(sums: &[u8], name: &str, archive: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(sums);
    let expected = text
        .lines()
        .find_map(|l| {
            let (hash, file) = l.split_once(char::is_whitespace)?;
            (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
        })
        .ok_or_else(|| format!("update: SHA256SUMS does not list {name}"))?;
    let actual: String = Sha256::digest(archive)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected {
        return Err(format!(
            "update: {name} does not match its published SHA-256; nothing was changed"
        ));
    }
    Ok(())
}

/// The binary from the archive's `<asset>/cedr[.exe]`.
#[cfg(not(windows))]
fn extract(archive: &[u8], asset: &str, _dir: &Path) -> Result<Vec<u8>, String> {
    let want = PathBuf::from(asset).join("cedr");
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let entries = tar
        .entries()
        .map_err(|e| format!("update: reading the archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("update: reading the archive: {e}"))?;
        let is_binary = entry
            .path()
            .is_ok_and(|p| p.components().eq(want.components()));
        if is_binary && entry.header().entry_type().is_file() {
            let mut bytes = Vec::new();
            (&mut entry)
                .take(ARCHIVE_MAX)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("update: reading the archive: {e}"))?;
            return Ok(bytes);
        }
    }
    Err(format!("update: the archive has no {}", want.display()))
}

/// Windows ships bsdtar, which reads zip archives; it is run from System32 so PATH cannot replace it.
#[cfg(windows)]
fn extract(archive: &[u8], asset: &str, dir: &Path) -> Result<Vec<u8>, String> {
    let work = dir.join(format!(".cedr-unpack-{}", std::process::id()));
    let result = (|| {
        std::fs::create_dir_all(&work).map_err(|e| format!("update: {e}"))?;
        let zip = work.join("cedr.zip");
        std::fs::write(&zip, archive).map_err(|e| format!("update: {e}"))?;
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let status = std::process::Command::new(PathBuf::from(root).join("System32\\tar.exe"))
            .arg("-xf")
            .arg(&zip)
            .arg("-C")
            .arg(&work)
            .arg(format!("{asset}/cedr.exe"))
            .status()
            .map_err(|e| format!("update: running tar.exe: {e}"))?;
        if !status.success() {
            return Err("update: unpacking the archive failed".into());
        }
        std::fs::read(work.join(asset).join("cedr.exe")).map_err(|e| format!("update: {e}"))
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Writes the new binary and confirms it starts and is the version asked for.
fn stage(tmp: &Path, binary: &[u8], version: &Version) -> Result<(), String> {
    std::fs::write(tmp, binary).map_err(|e| format!("update: {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("update: {}: {e}", tmp.display()))?;
    }
    let out = std::process::Command::new(tmp)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("update: the new cedr does not start: {e}"))?;
    let reported = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || reported.trim() != format!("cedr {version}") {
        return Err(format!(
            "update: the new cedr reported {:?}, not cedr {version}; nothing was changed",
            crate::common::clip(reported.trim(), 64)
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn swap(new: &Path, exe: &Path) -> Result<(), String> {
    std::fs::rename(new, exe).map_err(|e| format!("update: replacing {}: {e}", exe.display()))
}

/// Windows lets a running program be renamed but not replaced, so it is moved aside first and
/// moved back if the new one cannot take its place.
#[cfg(windows)]
fn swap(new: &Path, exe: &Path) -> Result<(), String> {
    let dir = exe.parent().unwrap_or(Path::new("."));
    let old = dir.join(format!(".cedr-old-{}.exe", std::process::id()));
    std::fs::rename(exe, &old)
        .map_err(|e| format!("update: moving {} aside: {e}", exe.display()))?;
    if let Err(e) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(format!("update: replacing {}: {e}", exe.display()));
    }
    Ok(())
}

/// Removes binaries a Windows update moved aside, once nothing runs them.
#[cfg(windows)]
fn remove_old(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".cedr-old-")
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Starts the background update when automatic updates are on and a day has passed since the last
/// check. Costs one stat until then; never fails or delays the caller.
pub(crate) fn auto_start() {
    if settings::in_ci()
        || settings::env_set("CEDR_NO_AUTO_UPDATE")
        || !Settings::load().auto_update()
    {
        return;
    }
    if !settings::stamp_due(
        &settings::state_file("update-check"),
        SystemTime::now(),
        CHECK_EVERY,
    ) {
        return;
    }
    let Ok(exe) = running_exe() else {
        return;
    };
    #[cfg(windows)]
    if let Some(dir) = exe.parent() {
        remove_old(dir);
    }
    if matches!(install_kind(&exe), Install::Archive(_)) {
        let _ = cedr_route::sync::detached_start(
            &exe,
            &[
                std::ffi::OsStr::new("update"),
                std::ffi::OsStr::new("--background"),
            ],
        );
    }
}

/// The version the binary at `exe` reports, which may differ from this process's own.
fn on_disk_version(exe: &Path) -> Option<Version> {
    let out = std::process::Command::new(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    Version::parse(text.trim().strip_prefix("cedr ")?).ok()
}

/// The daily check: moves only forward to a newer stable release, and leaves the outcome in
/// `update.json` for the next command (an update) or `cedr doctor` (a failure).
fn background() {
    let Ok(Some(_lock)) = cedr_route::sync::state_lock("update.lock", LOCK_STALE) else {
        return;
    };
    let mut from = current();
    let result = (|| -> Result<Option<Version>, String> {
        let exe = running_exe()?;
        let Install::Archive(dir) = install_kind(&exe) else {
            return Ok(None);
        };
        // The binary on disk may be newer than this process (a manual update since it started),
        // and is never replaced by an older one.
        from = on_disk_version(&exe).unwrap_or(from.clone());
        let releases = Releases::new()?;
        let latest = releases.latest()?;
        if latest <= from {
            return Ok(None);
        }
        releases.install(&latest, &exe, &dir)?;
        Ok(Some(latest))
    })();
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let path = settings::state_file(RESULT);
    let record = match result {
        Ok(Some(to)) => json!({"from": from.to_string(), "to": to.to_string(), "at": at}),
        Ok(None) => {
            let _ = std::fs::remove_file(&path);
            return;
        }
        Err(e) => json!({"error": e, "at": at}),
    };
    let _ = std::fs::write(&path, record.to_string());
}

/// Prints the one-line note after a background update, once.
pub(crate) fn announce() {
    let path = settings::state_file(RESULT);
    let Some(record) = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return;
    };
    if let (Some(from), Some(to)) = (record["from"].as_str(), record["to"].as_str()) {
        eprintln!(
            "cedr: updated to {} (from {}); see {REPO}/releases",
            crate::common::clip(to, 32),
            crate::common::clip(from, 32)
        );
        let _ = std::fs::remove_file(&path);
    }
}

/// The last background update's failure, for `cedr doctor`.
pub(crate) fn last_error() -> Option<String> {
    let record: Value =
        serde_json::from_slice(&std::fs::read(settings::state_file(RESULT)).ok()?).ok()?;
    Some(crate::common::clip(record["error"].as_str()?, 300))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checksum_must_match_the_listed_archive() {
        let archive = b"archive bytes";
        let hash: String = Sha256::digest(archive)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let sums = format!(
            "{hash}  cedr-linux-x86_64.tar.gz\n{}  cedr-macos-arm64.tar.gz\n",
            "0".repeat(64)
        );
        assert!(verify_sum(sums.as_bytes(), "cedr-linux-x86_64.tar.gz", archive).is_ok());
        let err = verify_sum(sums.as_bytes(), "cedr-macos-arm64.tar.gz", archive).unwrap_err();
        assert!(err.contains("does not match"), "{err}");
        assert!(verify_sum(sums.as_bytes(), "cedr-windows-x86_64.zip", archive).is_err());
        let binary_mode = format!("{hash} *cedr-linux-x86_64.tar.gz\n");
        assert!(verify_sum(binary_mode.as_bytes(), "cedr-linux-x86_64.tar.gz", archive).is_ok());
    }

    #[test]
    fn cargo_installs_are_never_replaced() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.canonicalize().unwrap().join("cedr");
        assert_eq!(install_kind_in(&exe, home.path()), Install::Cargo);
        assert!(Install::Cargo.fix().unwrap().starts_with("cargo install"));
    }

    #[test]
    fn a_writable_directory_is_an_archive_install() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let elsewhere = Path::new("/nonexistent-cargo-home");
        assert_eq!(
            install_kind_in(&dir.join("cedr"), elsewhere),
            Install::Archive(dir.clone())
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "the probe is removed"
        );
    }
}
