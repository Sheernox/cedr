//! Docker subprocess lifecycle. Only the proxy has a route outside the internal task network.
use crate::{Result, adapter::Harness, suite::Suite};
use cedr_compare::exec;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn hosts(h: Harness) -> &'static [&'static str] {
    match h {
        Harness::Claude => &[
            "api.anthropic.com",
            "claude.ai",
            "platform.claude.com",
            "console.anthropic.com",
        ],
        Harness::Codex => &["chatgpt.com", "api.openai.com", "auth.openai.com"],
        Harness::Muse => &["api.meta.ai", "auth.meta.com"],
        Harness::Grok => &[
            "api.x.ai",
            "cli-chat-proxy.grok.com",
            "grok.com",
            "accounts.x.ai",
        ],
        Harness::Gemini => &[
            "generativelanguage.googleapis.com",
            "cloudcode-pa.googleapis.com",
            "oauth2.googleapis.com",
        ],
        _ => &[],
    }
}
pub fn allowed(h: Harness, authority: &str) -> bool {
    authority
        .rsplit_once(':')
        .is_some_and(|(host, port)| port == "443" && hosts(h).contains(&host))
}
/// An isolated bridge omits the host gateway, which an ordinary internal bridge can expose.
pub fn network_args(name: &str) -> Vec<String> {
    [
        "network",
        "create",
        "--internal",
        "--opt",
        "com.docker.network.bridge.gateway_mode_ipv4=isolated",
        name,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}
pub fn version_matches(reported: &str, version: &str) -> bool {
    reported
        .split_whitespace()
        .any(|s| s.trim_matches(['(', ')']).trim_start_matches('v') == version)
}
fn docker(args: &[&str]) -> Result<String> {
    let mut c = Command::new("docker");
    c.args(args);
    let o = exec::run_step(c, None, Duration::from_secs(1800))
        .map_err(|e| format!("cannot start Docker: {e}"))?;
    if !o.succeeded() {
        return Err(format!(
            "Docker {} failed; check the daemon, image and local permissions",
            args.first().unwrap_or(&"operation")
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().into())
}
#[derive(Clone)]
pub struct Credential {
    pub source: PathBuf,
    pub target: &'static str,
}
pub fn token_env(h: Harness) -> &'static [&'static str] {
    match h {
        Harness::Claude => &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
        Harness::Codex => &["OPENAI_API_KEY"],
        Harness::Muse => &["MUSE_API_KEY", "META_API_KEY"],
        Harness::Grok => &["XAI_API_KEY"],
        Harness::Gemini => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        _ => &[],
    }
}
pub fn credential(h: Harness) -> Result<Option<Credential>> {
    if token_env(h)
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|v| !v.is_empty()))
    {
        return Ok(None);
    }
    if h == Harness::Command {
        return Ok(None);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or("HOME required for sign-in")?;
    let (source, target) = match h {
        Harness::Claude => (
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude"))
                .join(".credentials.json"),
            "/home/bench/.claude/.credentials.json",
        ),
        Harness::Codex => (
            std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"))
                .join("auth.json"),
            "/home/bench/.codex/auth.json",
        ),
        Harness::Muse => (
            home.join(".config/muse/auth.json"),
            "/home/bench/.config/muse/auth.json",
        ),
        Harness::Grok => (home.join(".grok/auth.json"), "/home/bench/.grok/auth.json"),
        Harness::Gemini => (
            home.join(".gemini/oauth_creds.json"),
            "/home/bench/.gemini/oauth_creds.json",
        ),
        _ => return Ok(None),
    };
    if !source.is_file() {
        return Err(format!(
            "{}: no sign-in file; sign in or set a supported provider token",
            h.name()
        ));
    }
    Ok(Some(Credential {
        source: std::fs::canonicalize(source).map_err(|e| e.to_string())?,
        target,
    }))
}
/// Container argv is public for offline verification of the isolation boundary.
pub fn task_args(
    network: &str,
    image: &str,
    workspace: &Path,
    home: &Path,
    name: &str,
    credential: Option<&Credential>,
    h: Harness,
) -> Vec<String> {
    let mut args: Vec<String> = [
        "run",
        "--rm",
        "--name",
        name,
        "--network",
        network,
        "--user",
        "1000:1000",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--read-only",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,mode=1777",
        "--workdir",
        "/workspace",
        "--env",
        "HOME=/home/bench",
        "--env",
        "CODEX_HOME=/home/bench/.codex",
        "--env",
        "CLAUDE_CONFIG_DIR=/home/bench/.claude",
        "--env",
        "XDG_CONFIG_HOME=/home/bench/.config",
        "--env",
        "XDG_DATA_HOME=/home/bench/.local/share",
        "--env",
        "XDG_STATE_HOME=/home/bench/.local/state",
        "--env",
        "XDG_CACHE_HOME=/home/bench/.cache",
        "--env",
        "LANG=C.UTF-8",
        "--env",
        "DISABLE_AUTOUPDATER=1",
        "--env",
        "HTTP_PROXY=http://bench-proxy:3128",
        "--env",
        "HTTPS_PROXY=http://bench-proxy:3128",
        "--env",
        "http_proxy=http://bench-proxy:3128",
        "--env",
        "https_proxy=http://bench-proxy:3128",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend([
        "--mount".into(),
        format!("type=bind,src={},dst=/workspace", workspace.display()),
        "--mount".into(),
        format!("type=bind,src={},dst=/home/bench", home.display()),
    ]);
    if let Some(c) = credential {
        args.extend([
            "--mount".into(),
            format!(
                "type=bind,src={},dst={},readonly",
                c.source.display(),
                c.target
            ),
        ]);
    }
    for key in token_env(h) {
        if std::env::var_os(key).is_some() {
            args.extend(["--env".into(), key.to_string()]);
        }
    }
    args.push(image.into());
    args
}
/// Dockerfile COPY sources are rooted at the suite, independent of the caller's directory.
pub fn build_args(root: &Path, tag: &str) -> Result<Vec<String>> {
    Ok(vec![
        "build".into(),
        "--file".into(),
        root.join("image/Dockerfile")
            .to_str()
            .ok_or("non UTF-8 image path")?
            .into(),
        "--provenance=false".into(),
        "--quiet".into(),
        "--tag".into(),
        tag.into(),
        root.to_str().ok_or("non UTF-8 suite path")?.into(),
    ])
}

/// Base images may set HOME for their non-root user; install the vendor binary as root.
pub fn claude_install(version: &str) -> String {
    format!(
        "RUN curl -fsSL https://claude.ai/install.sh -o /tmp/install.sh && HOME=/root bash /tmp/install.sh {version} && cp -L /root/.local/bin/claude /usr/local/bin/claude\n"
    )
}

pub struct Container {
    pub base: String,
    pub image: String,
    pub pinned: bool,
    network: String,
    proxy: String,
    task: String,
    _scratch: tempfile::TempDir,
}
impl Container {
    fn set_base(&mut self, suite: &Suite, build: bool) -> Result<()> {
        let base = &suite.edition.image;
        let pinned = base
            .rsplit_once("@sha256:")
            .is_some_and(|(_, d)| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()));
        if build {
            if !suite.root.join("image/Dockerfile").is_file() {
                return Err("--build-image needs suite image/Dockerfile".into());
            }
            let tag = format!("cedr-bench-base:{}", &suite.digest[..16]);
            let args = build_args(&suite.root, &tag)?;
            docker(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
            self.base = docker(&["image", "inspect", "--format", "{{.Id}}", &tag])?;
        } else {
            if !pinned {
                return Err(
                    "edition.toml: image must be pinned by digest (or use --build-image)".into(),
                );
            }
            docker(&["pull", "--quiet", base])?;
            self.base = docker(&["image", "inspect", "--format", "{{.Id}}", base])?;
        }
        self.pinned = pinned
            && docker(&["image", "inspect", "--format", "{{.Id}}", base])
                .is_ok_and(|id| id == self.base);
        Ok(())
    }
    pub fn grader(suite: &Suite, build: bool) -> Result<Self> {
        let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
        let task = format!(
            "cedr-grade-{}",
            scratch
                .path()
                .file_name()
                .ok_or("missing scratch name")?
                .to_string_lossy()
        );
        let mut c = Self {
            base: String::new(),
            image: String::new(),
            pinned: false,
            network: String::new(),
            proxy: String::new(),
            task,
            _scratch: scratch,
        };
        c.set_base(suite, build)?;
        Ok(c)
    }
    pub fn prepare(suite: &Suite, h: Harness, version: &str, build: bool) -> Result<Self> {
        if matches!(h, Harness::Antigravity | Harness::Command) {
            return Err("this harness supports --local only".into());
        }
        let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
        let suffix = scratch
            .path()
            .file_name()
            .ok_or("missing temporary name")?
            .to_string_lossy()
            .replace('.', "");
        let mut c = Self {
            base: String::new(),
            image: String::new(),
            pinned: false,
            network: format!("cedr-bench-{suffix}"),
            proxy: format!("cedr-proxy-{suffix}"),
            task: format!("cedr-task-{suffix}"),
            _scratch: scratch,
        };
        c.set_base(suite, build)?;
        if version.is_empty()
            || !version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        {
            return Err("agent version must be an exact version".into());
        }
        let base_tag = format!("cedr-bench-base:{}", c.base.trim_start_matches("sha256:"));
        docker(&["tag", &c.base, &base_tag])?;
        let tag = format!(
            "cedr-bench-agent:{}-{}-{}",
            &c.base.trim_start_matches("sha256:")[..16],
            h.name(),
            version
        );
        if docker(&["image", "inspect", "--format", "{{.Id}}", &tag]).is_err() {
            let context = c._scratch.path().join("layer");
            std::fs::create_dir(&context).map_err(|e| e.to_string())?;
            let install = match h {
                Harness::Claude => claude_install(version),
                Harness::Codex => format!("RUN npm install -g @openai/codex@{version}\n"),
                Harness::Gemini => format!("RUN npm install -g @google/gemini-cli@{version}\n"),
                Harness::Muse | Harness::Grok => {
                    let mut binary = exec::find_program(h.name(), Path::new("."))
                        .ok_or("static agent binary missing")?;
                    let prefix = magic(&binary)?;
                    if !prefix.starts_with(b"\x7fELF") && h == Harness::Muse {
                        binary = binary
                            .parent()
                            .ok_or("Muse executable has no parent")?
                            .join(format!("muse-bin-{version}"));
                    }
                    if !magic(&binary)?.starts_with(b"\x7fELF") {
                        return Err("a launcher script is not a static agent binary".into());
                    }
                    let out = Command::new("ldd")
                        .arg(&binary)
                        .output()
                        .map_err(|e| e.to_string())?;
                    if !String::from_utf8_lossy(&out.stdout).contains("statically linked")
                        && !String::from_utf8_lossy(&out.stderr)
                            .contains("not a dynamic executable")
                    {
                        return Err("agent is not a self-contained static binary; a vendor installer is required".into());
                    }
                    std::fs::copy(binary, context.join("agent")).map_err(|e| e.to_string())?;
                    format!("COPY --chmod=755 agent /usr/local/bin/{}\n", h.name())
                }
                _ => unreachable!(),
            };
            let dockerfile = format!(
                "FROM {}\nUSER root\n{}RUN mkdir -p /home/bench && chown 1000:1000 /home/bench\nUSER 1000:1000\n",
                base_tag, install
            );
            std::fs::write(context.join("Dockerfile"), dockerfile).map_err(|e| e.to_string())?;
            docker(&[
                "build",
                "--provenance=false",
                "--quiet",
                "--tag",
                &tag,
                context.to_str().ok_or("invalid layer path")?,
            ])?;
        }
        c.image = docker(&["image", "inspect", "--format", "{{.Id}}", &tag])?;
        let reported = docker(&[
            "run",
            "--rm",
            "--network=none",
            &c.image,
            h.name(),
            "--version",
        ])?;
        if !version_matches(&reported, version) {
            return Err("installed agent version differs from the requested version".into());
        }

        let network = network_args(&c.network);
        docker(&network.iter().map(String::as_str).collect::<Vec<_>>())
            .map_err(|_|"cannot establish isolated Docker egress; use a Docker engine supporting isolated bridge gateway mode".to_string())?;
        let script = c._scratch.path().join("proxy.py");
        std::fs::write(&script, include_str!("proxy.py")).map_err(|e| e.to_string())?;
        docker(&[
            "run",
            "-d",
            "--name",
            &c.proxy,
            "--network",
            "bridge",
            "--user",
            "1000:1000",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--read-only",
            "--env",
            &format!("BENCH_HOSTS={}", hosts(h).join(",")),
            "--mount",
            &format!("type=bind,src={},dst=/proxy.py,readonly", script.display()),
            &c.base,
            "python3",
            "/proxy.py",
        ])?;
        docker(&[
            "network",
            "connect",
            "--alias",
            "bench-proxy",
            &c.network,
            &c.proxy,
        ])?;
        // Check readiness and the actual proxy policy before spending any tokens.
        let check = "import socket; s=socket.create_connection(('bench-proxy',3128),10); s.sendall(b'CONNECT example.com:443 HTTP/1.1\\r\\n\\r\\n'); assert b'403' in s.recv(1024)";
        docker(&[
            "run",
            "--rm",
            "--network",
            &c.network,
            &c.base,
            "python3",
            "-c",
            check,
        ])?;
        Ok(c)
    }
    pub fn run(
        &self,
        argv: &[String],
        workspace: &Path,
        home: &Path,
        credential: Option<&Credential>,
        h: Harness,
        timeout: Duration,
    ) -> Result<exec::StepOutput> {
        let mut cmd = Command::new("docker");
        let mut args = task_args(
            &self.network,
            &self.image,
            workspace,
            home,
            &self.task,
            credential,
            h,
        );
        if h == Harness::Claude {
            let script = self._scratch.path().join("claude_process.py");
            std::fs::write(&script, include_str!("claude_process.py"))
                .map_err(|e| e.to_string())?;
            args.pop(); // Insert the read-only launcher mount before the image argument.
            args.extend([
                "--mount".into(),
                format!(
                    "type=bind,src={},dst=/bench-agent.py,readonly",
                    script.display()
                ),
                self.image.clone(),
                "python3".into(),
                "/bench-agent.py".into(),
            ]);
        }
        cmd.args(args).args(argv);
        let out = exec::run_step(cmd, None, timeout).map_err(|e| e.to_string());
        let _ = docker(&["rm", "--force", &self.task]);
        out
    }
    pub fn grade(
        &self,
        suite: &Path,
        id: &str,
        workspace: &Path,
        timeout: Duration,
    ) -> Result<exec::StepOutput> {
        let mut cmd = Command::new("docker");
        cmd.args([
            "run",
            "--rm",
            "--name",
            &self.task,
            "--network=none",
            "--user",
            "1000:1000",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--read-only",
            "--tmpfs",
            "/tmp:rw,nosuid,nodev,mode=1777",
            "--workdir",
            "/workspace",
            "--env",
            "HOME=/tmp",
            "--mount",
        ])
        .arg(format!(
            "type=bind,src={},dst=/workspace",
            workspace.display()
        ))
        .arg("--mount")
        .arg(format!(
            "type=bind,src={},dst=/graders,readonly",
            suite.join("graders").display()
        ))
        .arg(&self.base)
        .arg(format!("/graders/{id}/grade"))
        .arg("/workspace");
        let out = exec::run_step(cmd, None, timeout).map_err(|e| e.to_string());
        let _ = docker(&["rm", "--force", &self.task]);
        out
    }
}
impl Drop for Container {
    fn drop(&mut self) {
        for args in [
            ["rm", "--force", self.task.as_str()],
            ["rm", "--force", self.proxy.as_str()],
            ["network", "rm", self.network.as_str()],
        ] {
            if args[2].is_empty() {
                continue;
            }
            let _ = Command::new("docker")
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn magic(path: &Path) -> Result<[u8; 4]> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|_| "static vendor binary missing")?;
    let mut bytes = [0; 4];
    file.read_exact(&mut bytes)
        .map_err(|_| "not an executable binary")?;
    Ok(bytes)
}
