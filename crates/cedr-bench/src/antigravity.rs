//! T3's managed Antigravity release and the tool-free host renewal check.
use crate::{Result, rpc::Rpc};
use serde::Deserialize;
use serde_json::json;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const SERVER: &str = "agy_acp_server.par";
const HARNESS: &str = "localharness_external";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Active {
    release_id: String,
}

#[derive(Deserialize)]
struct File {
    name: String,
    bytes: u64,
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    executable: File,
    harness: File,
}

pub(crate) struct Install {
    pub version: String,
    dir: PathBuf,
}

impl Install {
    pub fn locate(requested: Option<&str>) -> Result<Self> {
        if let Some(dir) = std::env::var_os("CEDR_ANTIGRAVITY_DIR") {
            return Self::read(Path::new(&dir), requested);
        }
        let home = std::env::var_os("HOME").ok_or("HOME required for Antigravity install")?;
        let root = PathBuf::from(home).join(".t3/tools/antigravity-acp/linux-x64");
        Self::active(&root, requested)
    }

    fn active(root: &Path, requested: Option<&str>) -> Result<Self> {
        let active: Active = serde_json::from_slice(
            &std::fs::read(root.join("active.json")).map_err(|_| {
                "Antigravity install missing; install it in T3 Code or set CEDR_ANTIGRAVITY_DIR"
            })?,
        )
        .map_err(|_| "invalid Antigravity active.json")?;
        let release = Path::new(&active.release_id);
        let mut parts = release.components();
        if !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some() {
            return Err("invalid Antigravity active release ID".into());
        }
        Self::read(&root.join("versions").join(release), requested)
    }

    fn read(dir: &Path, requested: Option<&str>) -> Result<Self> {
        let dir =
            std::fs::canonicalize(dir).map_err(|_| "cannot find Antigravity release directory")?;
        let manifest: Manifest = serde_json::from_slice(
            &std::fs::read(dir.join(".install-complete.json"))
                .map_err(|_| "Antigravity release is missing .install-complete.json")?,
        )
        .map_err(|_| "invalid Antigravity .install-complete.json")?;
        if requested.is_some_and(|version| version != manifest.version) {
            return Err(format!(
                "installed Antigravity version {} differs from --agent-version",
                manifest.version
            ));
        }
        for (name, file) in [(SERVER, manifest.executable), (HARNESS, manifest.harness)] {
            let metadata = std::fs::metadata(dir.join(name))
                .map_err(|_| format!("Antigravity release is missing {name}"))?;
            if file.name != name || !metadata.is_file() || metadata.len() != file.bytes {
                return Err(format!(
                    "Antigravity {name} does not match .install-complete.json"
                ));
            }
        }
        Ok(Self {
            version: manifest.version,
            dir,
        })
    }

    pub fn layer(&self, context: &Path) -> Result<String> {
        for name in [SERVER, HARNESS] {
            std::fs::copy(self.dir.join(name), context.join(name))
                .map_err(|e| format!("cannot stage Antigravity {name} for Docker: {e}"))?;
        }
        Ok(format!(
            "COPY --chmod=755 {SERVER} {HARNESS} /opt/antigravity/\n"
        ))
    }

    // Preserve the person's credential location and let only the vendor renew it.
    pub fn check_sign_in(&self, workspace: &Path) -> Result<()> {
        let mut cmd = Command::new(self.dir.join(SERVER));
        cmd.arg("--uid=").current_dir(workspace).env_clear();
        for key in [
            "PATH",
            "HOME",
            "GEMINI_HOME",
            "LANG",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "http_proxy",
            "https_proxy",
            "NO_PROXY",
            "no_proxy",
        ] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        cmd.env("AGY_ACP_FORCE_FILE_STORAGE", "1")
            .env("PYTHONUNBUFFERED", "1")
            .env("TMPDIR", workspace)
            .env("ANTIGRAVITY_HARNESS_PATH", self.dir.join(HARNESS));
        let mut rpc = Rpc::start(cmd, Duration::from_secs(120))?;
        rpc.call(
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}}),
        )
        .map_err(|_| "cannot initialize Antigravity ACP on the host")?;
        let session = rpc
            .call("session/new", json!({"cwd": workspace,"mcpServers": []}))
            .map_err(|_| "cannot open an Antigravity ACP session on the host")?;
        if session["sessionId"].as_str().is_none_or(str::is_empty) {
            return Err("Antigravity did not open a host sign-in session".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn sqlite_usage_rows_preserve_cache_hits_and_missing_token_fields() {
        let launcher = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/antigravity_process.py");
        let script = r#"
import runpy
import sys

decode = runpy.run_path(sys.argv[1])["decode_usage"]
columns = ("input_tokens", "cache_read_tokens", "cache_write_tokens", "output_tokens", "reasoning_tokens")
for raw, expected in [
    ("0a0d220b108f5718a60248d6015050", (11151, 0, 0, 294, 214)),
    ("0a10220e10ed1318a00228864748d8015048", (2541, 9094, 0, 288, 216)),
    ("0a0e220c108d13184b28f24a48495002", (2445, 9586, 0, 75, 73)),
    ("0a0b2209184b28f24a48495002", (None, 9586, 0, 75, 73)),
    ("0a0c220a108d1328f24a48495002", (2445, 9586, 0, None, 73)),
    ("0a0e220c108d13184b28f24a48495003", (None, None, 0, None, None)),
    ("0a0e220c108d13184b28f24a484950", (None, None, 0, None, None)),
]:
    usage = decode(bytes.fromhex(raw))
    assert tuple(usage[column] for column in columns) == expected, raw
"#;
        let status = Command::new("python3")
            .args(["-B", "-c", script])
            .arg(launcher)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn client_reads_stay_in_the_workspace_and_slice_by_line() {
        let launcher = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/antigravity_process.py");
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("a.py"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(workspace.join("bin"), b"a\0b").unwrap();
        std::fs::write(root.path().join("secret"), "outside").unwrap();
        std::os::unix::fs::symlink(root.path().join("secret"), workspace.join("link")).unwrap();
        let script = r#"
import runpy
import sys

namespace = runpy.run_path(sys.argv[1])
read, refused = namespace["read_text_file"], namespace["ReadRefused"]
read.__globals__["WORKSPACE"] = sys.argv[2]
assert read({"path": "a.py"}) == "one\ntwo\nthree\n"
assert read({"path": sys.argv[2] + "/a.py", "line": 2, "limit": 1}) == "two"
for params, code in [({"path": "../secret"}, -32602), ({"path": "link"}, -32602), ({"path": "bin"}, -32602), ({"path": "missing"}, -32002)]:
    try:
        read(params)
    except refused as error:
        assert error.code == code, params
    else:
        raise AssertionError(params)
"#;
        let status = Command::new("python3")
            .args(["-B", "-c", script])
            .arg(launcher)
            .arg(&workspace)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn active_release_requires_the_requested_version_and_both_file_sizes() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("versions/release");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            root.path().join("active.json"),
            r#"{"releaseId":"release"}"#,
        )
        .unwrap();
        let manifest = json!({
            "version": "1.3.0",
            "executable": {"name": SERVER, "bytes": 3},
            "harness": {"name": HARNESS, "bytes": 4},
        });
        std::fs::write(dir.join(".install-complete.json"), manifest.to_string()).unwrap();
        std::fs::write(dir.join(SERVER), "par").unwrap();
        std::fs::write(dir.join(HARNESS), "tool").unwrap();
        let install = Install::active(root.path(), None).unwrap();
        assert_eq!(install.version, "1.3.0");
        assert_eq!(install.dir, dir.canonicalize().unwrap());
        assert!(
            Install::read(&dir, Some("1.4.0"))
                .err()
                .unwrap()
                .contains("differs from --agent-version")
        );
        std::fs::write(dir.join(HARNESS), "shortened binary").unwrap();
        assert!(
            Install::read(&dir, Some("1.3.0"))
                .err()
                .unwrap()
                .contains("localharness_external does not match")
        );
    }
}
