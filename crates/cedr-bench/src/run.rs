//! Ordered run loop; backends separate offline accounting tests from agent and Docker I/O.
use crate::{
    Result,
    adapter::{self, Harness},
    container::{self, Container, Credential},
    report::*,
    suite::{self, Kind, Suite, Task},
};
use cedr_compare::exec;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub const FINGERPRINT_PROMPT: &str = "Reply with the single word OK.";
pub const PROBE_PROMPT: &str = "Run exactly one shell command: printf 'cedr-bench-command\\n' > command.txt. Then use apply_patch or your native file-write tool (not the shell) to create probe.txt containing exactly cedr-bench-probe followed by a newline. Do not run any other shell commands.";

pub struct Options {
    pub harness: Harness,
    pub model: String,
    pub effort: Option<String>,
    pub version: String,
    pub local: bool,
    pub build_image: bool,
    pub set_reference: bool,
    pub stop_usd: Option<f64>,
    pub allow_unpriced: bool,
    pub tasks: Vec<String>,
    pub command: Vec<String>,
    pub billing: String,
}
pub struct Invocation {
    pub status: Status,
    pub usage: Usage,
    pub duration_ms: u64,
}
pub trait Backend {
    fn invoke(&mut self, prompt: &str, workspace: &Path, timeout: Duration) -> Result<Invocation>;
    fn grade(&mut self, suite: &Suite, task: &Task, workspace: &Path, seed: u64)
    -> Result<Vec<u8>>;
    fn environment(&self) -> Environment;
    fn image(&self) -> Option<String> {
        None
    }
}
pub struct ProcessBackend<'a> {
    options: &'a Options,
    container: Option<Container>,
    credential: Option<Credential>,
    healthy: bool,
    local_grader: Option<Container>,
}
impl<'a> ProcessBackend<'a> {
    pub fn new(suite: &Suite, options: &'a Options) -> Result<Self> {
        let credential = if options.harness == Harness::Antigravity {
            Some(crate::antigravity::credential()?)
        } else {
            container::credential(options.harness)?
        };
        let container = if options.local {
            None
        } else {
            Some(Container::prepare(
                suite,
                options.harness,
                &options.version,
                options.build_image,
            )?)
        };
        let mut backend = Self {
            options,
            container,
            credential,
            healthy: true,
            local_grader: None,
        };
        if options.local && !matches!(options.harness, Harness::Command | Harness::Antigravity) {
            let check = tempfile::tempdir().map_err(|e| e.to_string())?;
            let mut argv = vec![options.harness.name().to_string(), "--version".into()];
            backend.pin_local(&mut argv)?;
            let out = local(
                &argv,
                options.harness,
                check.path(),
                &check.path().join("home"),
                Duration::from_secs(15),
                None,
            )?;
            if !out.succeeded()
                || !container::version_matches(
                    &String::from_utf8_lossy(&out.stdout),
                    &options.version,
                )
            {
                return Err("installed agent version differs from --agent-version; install that version or omit the flag".into());
            }
        }
        if backend.credential.is_some() {
            backend.check_sign_in()?;
        }
        if let Some(c) = &backend.container {
            c.check_effort(
                options.harness,
                &options.model,
                options.effort.as_deref(),
                backend.credential.as_ref(),
            )?;
        }
        Ok(backend)
    }
    fn pin_local(&self, argv: &mut [String]) -> Result<()> {
        if self.options.harness == Harness::Muse {
            let launcher =
                exec::find_program("muse", Path::new(".")).ok_or("Muse is not installed")?;
            let versioned = launcher
                .parent()
                .ok_or("Muse installation has no parent")?
                .join(format!("muse-bin-{}", self.options.version));
            if versioned.is_file() {
                argv[0] = versioned.to_string_lossy().into_owned();
            } else {
                use std::io::Read;
                let mut prefix = [0; 2];
                std::fs::File::open(&launcher)
                    .and_then(|mut f| f.read_exact(&mut prefix))
                    .map_err(|e| e.to_string())?;
                if prefix == *b"#!" {
                    return Err("versioned Muse binary missing; install the requested version with the vendor first".into());
                }
            }
        }
        Ok(())
    }
    // Account checks cannot act on the host; a replaced credential link is preserved atomically.
    fn check_sign_in(&mut self) -> Result<()> {
        if self.options.harness == Harness::Antigravity {
            return crate::antigravity::check(
                self.credential
                    .as_ref()
                    .ok_or("Antigravity sign-in missing")?,
            );
        }
        let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = scratch.path().join("workspace");
        std::fs::create_dir(&workspace).map_err(|e| e.to_string())?;
        let home = scratch.path().join("home");
        let mut argv = cedr_agent::sign_in_args(agent(self.options.harness), &self.options.model)
            .map_err(|e| e.to_string())?;
        self.pin_local(&mut argv)?;
        let out = local(
            &argv,
            self.options.harness,
            &workspace,
            &home,
            Duration::from_secs(120),
            self.credential.as_ref(),
        )?;
        if !out.succeeded() {
            self.healthy = false;
            return Err(
                "host sign-in check failed; repair the vendor sign-in before starting another run"
                    .into(),
            );
        }
        Ok(())
    }
}
fn agent(h: Harness) -> cedr_agent::Agent {
    match h {
        Harness::Claude => cedr_agent::Agent::Claude,
        Harness::Codex => cedr_agent::Agent::Codex,
        Harness::Muse => cedr_agent::Agent::Muse,
        Harness::Grok => cedr_agent::Agent::Grok,
        Harness::Gemini => cedr_agent::Agent::Gemini,
        _ => cedr_agent::Agent::Command,
    }
}
fn local(
    argv: &[String],
    h: Harness,
    workspace: &Path,
    home: &Path,
    timeout: Duration,
    renewal: Option<&Credential>,
) -> Result<exec::StepOutput> {
    let binary = exec::find_program(&argv[0], Path::new("."))
        .ok_or_else(|| format!("{} is not installed", argv[0]))?;
    let mut cmd = Command::new(binary);
    cmd.args(&argv[1..]).current_dir(workspace);
    if h == Harness::Command {
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("CEDR_") || name.starts_with("GIT_") {
                cmd.env_remove(key);
            }
        }
        return exec::run_step(cmd, None, timeout)
            .map_err(|e| format!("cannot run command harness: {e}"));
    }
    cmd.env_clear();
    for k in ["PATH", "LANG", "SYSTEMROOT", "WINDIR", "TMP", "TEMP"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    for k in container::token_env(h) {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    if renewal.is_some() {
        cedr_agent::sign_in_home(agent(h), &mut cmd, home)
    } else {
        cedr_agent::clean_home(agent(h), &mut cmd, home)
    }
    .map_err(|e| format!("cannot prepare clean home: {e}"))?;
    let links =
        cedr_agent::SignInLink::capture(home).map_err(|_| "cannot track sign-in renewal")?;
    let runtime = home
        .parent()
        .ok_or("clean home has no private parent")?
        .join("runtime");
    cedr_agent::private_runtime(&mut cmd, &runtime).map_err(|e| e.to_string())?;
    if h == Harness::Claude {
        cmd.env("DISABLE_AUTOUPDATER", "1");
    }
    let out = exec::run_step(cmd, None, timeout).map_err(|e| format!("cannot start agent: {e}"));
    for link in links {
        if let Some(notice) = link
            .save_renewed()
            .map_err(|_| "cannot preserve renewed sign-in; original sign-in kept")?
        {
            eprintln!("cedr: {notice}");
        }
    }
    out
}
impl Backend for ProcessBackend<'_> {
    fn invoke(&mut self, prompt: &str, workspace: &Path, timeout: Duration) -> Result<Invocation> {
        if !self.healthy {
            return Err("host sign-in failed; no further invocations allowed".into());
        }
        if self.options.harness == Harness::Antigravity {
            let home = tempfile::tempdir().map_err(|e| e.to_string())?;
            let start = Instant::now();
            let usage = crate::antigravity::invoke(
                &self.options.model,
                prompt,
                workspace,
                &home.path().join("home"),
                self.credential
                    .as_ref()
                    .ok_or("Antigravity sign-in missing")?,
                timeout,
            );
            let duration_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
            if let Err(e) = self.check_sign_in() {
                self.healthy = false;
                return Err(e);
            }
            return Ok(Invocation {
                status: Status::Graded,
                usage: usage?,
                duration_ms,
            });
        }
        if self.container.is_some() && self.credential.is_some() {
            self.check_sign_in()?;
        }
        let home = tempfile::tempdir().map_err(|e| e.to_string())?;
        let mut argv = adapter::argv(
            self.options.harness,
            &self.options.model,
            self.options.effort.as_deref(),
            prompt,
            &self.options.command,
        )?;
        if self.container.is_none() {
            self.pin_local(&mut argv)?;
        }
        let prompt_file = home.path().join("prompt");
        if self.options.harness == Harness::Command {
            std::fs::write(&prompt_file, prompt).map_err(|e| e.to_string())?;
            for a in &mut argv {
                *a = a.replace("/tmp/bench-prompt", &prompt_file.to_string_lossy());
            }
        }
        for name in [
            ".codex",
            ".claude",
            ".config/muse",
            ".grok",
            ".gemini",
            ".local/share",
            ".local/state",
            ".cache",
        ] {
            std::fs::create_dir_all(home.path().join(name)).map_err(|e| e.to_string())?;
        }
        let start = Instant::now();
        let out = if let Some(c) = &self.container {
            c.run(
                &argv,
                workspace,
                home.path(),
                self.credential.as_ref(),
                self.options.harness,
                timeout,
            )
        } else {
            // The shared home builder needs an empty directory; vendor dirs above are container-only.
            let local_home = home.path().join("local");
            local(
                &argv,
                self.options.harness,
                workspace,
                &local_home,
                timeout,
                None,
            )
        }?;
        let duration_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let mut usage = adapter::parse(self.options.harness, &out.stdout, &self.options.model);
        let status = if exec::interrupted() {
            Status::Interrupted
        } else if out.status.is_none() {
            Status::Timeout
        } else if out.succeeded() {
            Status::Graded
        } else {
            Status::AgentError
        };
        if matches!(status, Status::Timeout | Status::Interrupted) {
            usage.cost_usd = None;
        }
        if status == Status::AgentError && self.container.is_some() {
            self.healthy = false;
            return Err("container agent failed, possibly because its sign-in expired; renewal is blocked; run again after checking the vendor sign-in".into());
        }
        Ok(Invocation {
            status,
            usage,
            duration_ms,
        })
    }
    fn grade(
        &mut self,
        suite: &Suite,
        task: &Task,
        workspace: &Path,
        seed: u64,
    ) -> Result<Vec<u8>> {
        let timeout = Duration::from_secs(task.timeout_seconds);
        let out = if let Some(c) = &self.container {
            c.grade(&suite.root, &task.id, workspace, timeout, seed)?
        } else if container::available() {
            if self.local_grader.is_none() {
                self.local_grader = Some(Container::grader(suite, self.options.build_image)?);
            }
            self.local_grader
                .as_ref()
                .ok_or("grader image unavailable")?
                .grade(&suite.root, &task.id, workspace, timeout, seed)?
        } else {
            if task.kind == Kind::Visual {
                return Err("visual tasks cannot be graded locally without Docker; install Docker and run again".into());
            }
            let grader = suite.root.join("graders").join(&task.id).join("grade");
            #[cfg(windows)]
            let mut command = {
                let mut command = Command::new("python3");
                command.arg(&grader);
                command
            };
            #[cfg(not(windows))]
            let mut command = Command::new(&grader);
            command
                .arg(workspace)
                .current_dir(workspace)
                .env_clear()
                .env("HOME", workspace)
                .env("BENCH_SEED", seed.to_string());
            if let Some(path) = std::env::var_os("PATH") {
                command.env("PATH", path);
            }
            exec::run_step(command, None, timeout)
                .map_err(|e| format!("cannot run host grader: {e}"))?
        };
        if !out.succeeded() {
            return Err("grader failed or timed out".into());
        }
        Ok(out.stdout)
    }
    fn image(&self) -> Option<String> {
        self.container.as_ref().map(|c| c.base.clone())
    }
    fn environment(&self) -> Environment {
        Environment {
            mode: if self.container.is_some() {
                "container"
            } else {
                "local"
            }
            .into(),
            clean_home: self.options.harness != Harness::Command && cfg!(unix),
            network: if self.container.is_some() {
                "allowlist"
            } else {
                "host"
            }
            .into(),
            pinned: self.container.as_ref().is_some_and(|c| c.pinned),
            agent_image: self.container.as_ref().map(|c| c.image.clone()),
            verified: self
                .container
                .as_ref()
                .map(|c| c.verified.clone())
                .unwrap_or_default(),
            limits: self
                .container
                .as_ref()
                .map(|_| "memory=2g,pids=256,tmp=1g,swap=0".into()),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            cli: env!("CARGO_PKG_VERSION").into(),
        }
    }
}
pub(crate) fn git(workspace: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(workspace)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=Bench",
            "-c",
            "user.email=bench@localhost",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args);
    c.env_clear();
    for key in ["PATH", "SYSTEMROOT", "WINDIR", "TMP", "TEMP"] {
        if let Some(v) = std::env::var_os(key) {
            c.env(key, v);
        }
    }
    c.env("GIT_CONFIG_NOSYSTEM", "1").env(
        "GIT_CONFIG_GLOBAL",
        if cfg!(windows) { "NUL" } else { "/dev/null" },
    );
    let o = c.output().map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err("cannot prepare benchmark git repository".into());
    }
    Ok(o.stdout)
}
fn initialize(workspace: &Path) -> Result<()> {
    git(workspace, &["init", "--quiet", "--template="])?;
    git(workspace, &["add", "--all", "--force"])?;
    git(
        workspace,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "task starting state",
        ],
    )?;
    Ok(())
}
// Use the protected starting metadata, never config or helper commands an agent left in .git.
fn patch(workspace: &Path, metadata: &Path) -> Result<(Vec<u8>, Vec<TamperSignal>)> {
    if !std::fs::symlink_metadata(workspace)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("workspace was replaced".into());
    }
    // Use the same copy policy as grading: strip every .git, retaining its ordinary files.
    // This also prevents nested repositories from becoming unaudited gitlinks.
    let snapshot = tempfile::tempdir().map_err(|e| e.to_string())?;
    let submitted = snapshot.path().join("submission");
    suite::copy_submission(workspace, &submitted)?;
    let workspace = submitted.as_path();
    let metadata = format!("--git-dir={}", metadata.display());
    let tree = format!("--work-tree={}", workspace.display());
    git(workspace, &[&metadata, &tree, "add", "--all", "--force"])?;
    let diff = git(
        workspace,
        &[&metadata, &tree, "diff", "--cached", "--binary", "HEAD"],
    )?;
    let changed = git(
        workspace,
        &[
            &metadata,
            &tree,
            "diff",
            "--cached",
            "--no-renames",
            "--name-only",
            "--diff-filter=ACMRT",
            "-z",
            "HEAD",
        ],
    )?;
    let added = git(
        workspace,
        &[
            &metadata,
            &tree,
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--diff-filter=ACMRT",
            "--text",
            "--no-color",
            "--unified=0",
            "HEAD",
        ],
    )?;
    let signals = crate::audit::scan(workspace, &changed, &added)?;
    Ok((diff, signals))
}

// Never open links or special files. Allow bounded trailing whitespace from native file writers.
fn probe_file(workspace: &Path, name: &str, contents: &[u8]) -> bool {
    let path = workspace.join(name);
    std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= 4096)
        && std::fs::read_to_string(path)
            .is_ok_and(|text| text.trim_end().as_bytes() == contents.trim_ascii_end())
}

pub fn probe<B: Backend>(suite: &Suite, options: &Options, backend: &mut B) -> Result<Probe> {
    let fingerprint = tempfile::tempdir().map_err(|e| e.to_string())?;
    let first = backend.invoke(
        FINGERPRINT_PROMPT,
        fingerprint.path(),
        Duration::from_secs(180),
    )?;
    let readiness = tempfile::tempdir().map_err(|e| e.to_string())?;
    let second = backend.invoke(PROBE_PROMPT, readiness.path(), Duration::from_secs(180))?;
    let tools_ok = first.status == Status::Graded
        && second.status == Status::Graded
        && probe_file(readiness.path(), "probe.txt", b"cedr-bench-probe\n")
        && probe_file(readiness.path(), "command.txt", b"cedr-bench-command\n");
    let reference = suite
        .edition
        .probe
        .references
        .iter()
        .find(|r| {
            r.harness == options.harness.name()
                && r.harness_version == options.version
                && r.model == options.model
                && r.effort == options.effort
        })
        .map(|r| (r.input_tokens, r.tolerance));
    let mut p = crate::report::probe_with_tolerance(
        &first.usage,
        reference,
        options.set_reference,
        tools_ok,
    );
    p.tools = second.usage.tools;
    p.fingerprint_cost_usd = first.usage.cost_usd;
    p.tools_cost_usd = second.usage.cost_usd;
    p.cost_usd = first
        .usage
        .cost_usd
        .zip(second.usage.cost_usd)
        .map(|(a, b)| a + b);
    Ok(p)
}
pub fn run<B: Backend>(
    suite: &Suite,
    options: &Options,
    backend: &mut B,
    artifacts: &Path,
    mut progress: impl FnMut(&str),
) -> Result<BenchResult> {
    if options.stop_usd.is_some_and(|n| !n.is_finite() || n <= 0.0) {
        return Err("stop USD must be finite and above zero".into());
    }
    for id in &options.tasks {
        if !suite.tasks.iter().any(|t| &t.id == id) {
            return Err(format!("unknown task {id}"));
        }
    }
    let started = chrono::Utc::now();
    let p = probe(suite, options, backend)?;
    let id = format!(
        "b-{}-{}",
        started.format("%Y%m%dT%H%M%SZ"),
        &crate::sha(format!("{started:?}{:?}", std::time::Instant::now()).as_bytes())[..6]
    );
    let mut r = BenchResult {
        version: 1,
        kind: "bench".into(),
        id,
        edition: EditionInfo {
            number: suite.edition.number,
            suite_sha256: suite.digest.clone(),
            commit: suite.commit.clone(),
            image: backend
                .image()
                .unwrap_or_else(|| suite.edition.image.clone()),
        },
        config: Config {
            harness: options.harness.name().into(),
            harness_version: options.version.clone(),
            model: options.model.clone(),
            effort: options.effort.clone(),
            resolved_models: Vec::new(),
        },
        environment: backend.environment(),
        probe: p,
        billing: options.billing.clone(),
        started_at: started.to_rfc3339(),
        finished_at: started.to_rfc3339(),
        goal_usd: suite.edition.goal_usd,
        stop_usd: options.stop_usd,
        spent_usd: 0.0,
        over_goal: false,
        unpriced_trials: 0,
        complete: false,
        stop: None,
        score: 0.0,
        solved: 0,
        code_tasks: 0,
        eligible: false,
        tamper_review: None,
        tasks: suite
            .tasks
            .iter()
            .map(|t| TaskResult::pending(&t.id, t.kind))
            .collect(),
    };
    if !r.probe.tools_ok {
        r.stop = Some(Stop::ProbeFailed);
    } else if r.probe.cost_usd.is_none() && !options.allow_unpriced {
        r.stop = Some(Stop::UnknownCost);
    }
    r.summarize();
    for (i, t) in suite.tasks.iter().enumerate() {
        if r.stop.is_some() {
            break;
        }
        if exec::interrupted() {
            r.stop = Some(Stop::Interrupted);
            break;
        }
        if !options.tasks.is_empty() && !options.tasks.contains(&t.id) {
            continue;
        }
        if options.stop_usd.is_some_and(|n| r.spent_usd >= n) {
            r.stop = Some(Stop::UserStop);
            break;
        }
        progress(&t.id);
        let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = scratch.path().join("workspace");
        suite::copy(
            &suite.root.join("tasks").join(&t.id).join("repo"),
            &workspace,
        )?;
        initialize(&workspace)?;
        let metadata = scratch.path().join("git-original");
        std::fs::rename(workspace.join(".git"), &metadata).map_err(|e| e.to_string())?;
        suite::copy(&metadata, &workspace.join(".git"))?;

        let prompt =
            std::fs::read_to_string(suite.root.join("tasks").join(&t.id).join("prompt.md"))
                .map_err(|e| e.to_string())?;
        let out = match backend.invoke(&prompt, &workspace, Duration::from_secs(t.timeout_seconds))
        {
            Ok(out) => out,
            Err(error) => {
                progress(&format!("{}: agent failed: {error}", t.id));
                r.tasks[i].error = Some(error);
                if exec::interrupted() {
                    r.tasks[i].status = Status::Interrupted;
                    r.stop = Some(Stop::Interrupted);
                } else {
                    r.tasks[i].status = Status::AgentError;
                    r.stop = Some(Stop::UnknownCost);
                }
                break;
            }
        };
        let task = &mut r.tasks[i];
        task.status = out.status;
        task.duration_ms = Some(out.duration_ms);
        task.turns = out.usage.turns;
        task.tokens = out.usage.tokens;
        task.cost_usd = out.usage.cost_usd;
        task.patch_sha256 = match patch(&workspace, &metadata) {
            Ok((diff, signals)) => {
                task.tamper_signals = signals;
                let hash = crate::sha(&diff);
                if task.kind == Kind::Code {
                    let directory = artifacts.join(&t.id);
                    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
                    // This local audit artifact is deliberately absent from the exported artifact list.
                    std::fs::write(directory.join("submission.patch"), diff)
                        .map_err(|e| e.to_string())?;
                }
                Some(hash)
            }
            Err(error) => {
                progress(&format!("{}: cannot capture patch: {error}", t.id));
                task.status = Status::SetupFailed;
                None
            }
        };
        for m in out.usage.models {
            if !r.config.resolved_models.contains(&m) {
                r.config.resolved_models.push(m);
            }
        }
        if task.status == Status::Graded {
            let copy = scratch.path().join("grade");
            let seed = u64::from_be_bytes(cedr_bench_seed()?) & ((1 << 30) - 1);
            task.seed = Some(seed);
            let graded = suite::copy_submission(&workspace, &copy)
                .and_then(|()| backend.grade(suite, t, &copy, seed))
                .and_then(|b| crate::report::grade(&b, t.kind));
            match graded {
                Ok(g) => {
                    task.checks = g.checks;
                    task.gates = g.gates;
                    if t.kind == Kind::Code {
                        task.score =
                            Some(task.checks.iter().filter(|c| c.passed).count() as f64 / 4.0);
                        task.solved = task.score == Some(1.0);
                    }
                    let emitted: BTreeSet<String> = g.artifacts.into_iter().collect();
                    let mut names: BTreeSet<String> = t.artifacts.iter().cloned().collect();
                    names.extend(emitted.iter().cloned());
                    let missing_submission = t.kind == Kind::Visual
                        && task
                            .gates
                            .iter()
                            .any(|gate| gate.id == "file" && !gate.passed);
                    let collected = (|| -> Result<Vec<Artifact>> {
                        let mut collected = Vec::new();
                        for name in names {
                            // An uncreated submission is a graded failure, not broken infrastructure.
                            // A grader's explicit artifact claims must still be fulfilled.
                            if missing_submission
                                && !emitted.contains(&name)
                                && std::fs::symlink_metadata(copy.join(&name)).is_err_and(|error| {
                                    error.kind() == std::io::ErrorKind::NotFound
                                })
                            {
                                continue;
                            }
                            let (sha256, bytes) = suite::artifact(&copy, &name)?;
                            let path = format!("{}/{name}", t.id);
                            let dest = artifacts.join(&path);
                            std::fs::create_dir_all(
                                dest.parent().ok_or("invalid artifact directory")?,
                            )
                            .map_err(|e| e.to_string())?;
                            std::fs::copy(copy.join(&name), dest).map_err(|e| e.to_string())?;
                            collected.push(Artifact {
                                path,
                                sha256,
                                bytes,
                            });
                        }
                        Ok(collected)
                    })();
                    match collected {
                        Ok(a) => task.artifacts = a,
                        Err(error) => {
                            progress(&format!("{}: artifact collection failed: {error}", t.id));
                            task.status = Status::SetupFailed;
                        }
                    }
                }
                Err(error) => {
                    progress(&format!("{}: grader failed: {error}", t.id));
                    task.status = Status::SetupFailed;
                }
            }
        }
        if task.status != Status::Graded {
            task.score = if task.kind == Kind::Code {
                Some(0.0)
            } else {
                None
            };
            task.solved = false;
        }
        if task.status == Status::Interrupted {
            r.stop = Some(Stop::Interrupted);
        } else if task.cost_usd.is_none() && !options.allow_unpriced {
            r.stop = Some(Stop::UnknownCost);
        }
        r.spent_usd += task.cost_usd.unwrap_or(0.0);
    }
    r.finished_at = chrono::Utc::now().to_rfc3339();
    r.summarize();
    r.validate()?;
    Ok(r)
}

fn cedr_bench_seed() -> Result<[u8; 8]> {
    use sha2::{Digest, Sha256};
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let bytes = Sha256::digest(temp.path().to_string_lossy().as_bytes());
    Ok(bytes[..8].try_into().expect("fixed digest length"))
}

#[cfg(test)]
mod live_tests {
    use super::*;
    #[test]
    fn ignored_helpers_and_nested_repository_contents_are_saved_hashed_and_scanned() {
        for nested in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let workspace = dir.path().join("workspace");
            std::fs::create_dir(&workspace).unwrap();
            std::fs::write(workspace.join("main.py"), "pass\n").unwrap();
            initialize(&workspace).unwrap();
            let metadata = dir.path().join("metadata");
            suite::copy(&workspace.join(".git"), &metadata).unwrap();
            std::fs::create_dir(workspace.join("lib")).unwrap();
            if nested {
                git(&workspace.join("lib"), &["init", "--quiet", "--template="]).unwrap();
            } else {
                std::fs::write(workspace.join(".gitignore"), "lib/helper.py\n").unwrap();
            }
            std::fs::write(workspace.join("main.py"), "from lib import helper\n").unwrap();
            std::fs::write(
                workspace.join("lib/helper.py"),
                "import sys\nframe = sys._getframe()\n",
            )
            .unwrap();
            let (first, signals) = patch(&workspace, &metadata).unwrap();
            assert!(String::from_utf8_lossy(&first).contains("+frame = sys._getframe()"));
            assert!(
                signals
                    .iter()
                    .any(|s| s.path == "lib/helper.py" && s.kind == "process_introspection")
            );
            std::fs::write(
                workspace.join("lib/helper.py"),
                "import sys\nframe = sys._getframe(1)\n",
            )
            .unwrap();
            let (second, _) = patch(&workspace, &metadata).unwrap();
            assert_ne!(crate::sha(&first), crate::sha(&second));
            let replay = dir.path().join("replay");
            std::fs::create_dir(&replay).unwrap();
            std::fs::write(replay.join("main.py"), "pass\n").unwrap();
            let saved = dir.path().join("submission.patch");
            std::fs::write(&saved, &second).unwrap();
            git(&replay, &["apply", saved.to_str().unwrap()]).unwrap();
            assert_eq!(
                std::fs::read(replay.join("lib/helper.py")).unwrap(),
                std::fs::read(workspace.join("lib/helper.py")).unwrap()
            );
        }
    }
    #[test]
    fn readiness_markers_trim_only_trailing_whitespace_and_remain_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("probe.txt");
        for bytes in [b"cedr-bench-probe".as_slice(), b"cedr-bench-probe\r\n \t"] {
            std::fs::write(&path, bytes).unwrap();
            assert!(probe_file(temp.path(), "probe.txt", b"cedr-bench-probe\n"));
        }
        for text in [
            " cedr-bench-probe".to_string(),
            "cedr-bench-probe-extra".to_string(),
            format!("cedr-bench-probe{}", " ".repeat(4096)),
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(!probe_file(temp.path(), "probe.txt", b"cedr-bench-probe\n"));
        }
        // The supplied printf command must produce a newline, not two literal bytes.
        assert!(PROBE_PROMPT.contains("printf 'cedr-bench-command\\n'"));
        assert!(!PROBE_PROMPT.contains("cedr-bench-command\\\\n"));
    }

    #[test]
    #[ignore = "requires Antigravity sign-in, network and paid inference"]
    fn antigravity_local_probe() {
        let suite =
            suite::load(Path::new(&std::env::var("CEDR_BENCH_LIVE_SUITE").unwrap())).unwrap();
        let options = Options {
            harness: Harness::Antigravity,
            model: "gemini-3.8-flash-high".into(),
            effort: None,
            version: "1.3.0".into(),
            local: true,
            build_image: false,
            set_reference: true,
            stop_usd: None,
            allow_unpriced: false,
            tasks: Vec::new(),
            command: Vec::new(),
            billing: "plan".into(),
        };
        let mut b = ProcessBackend::new(&suite, &options).unwrap();
        let p = probe(&suite, &options, &mut b).unwrap();
        println!("Antigravity probe: {p:?}");
        assert!(p.tools_ok);
    }
    /// Opt-in fixture capture, restricted to the four inexpensive vendor models. This test never
    /// prints agent content: only reduced usage events are written under the explicit scratch dir.
    #[test]
    #[ignore = "requires Docker, network, vendor sign-in and paid inference"]
    fn capture_vendor_probe() {
        let h: Harness = std::env::var("CEDR_BENCH_CAPTURE_HARNESS")
            .unwrap()
            .parse()
            .unwrap();
        let (model, effort) = match h {
            Harness::Claude => ("claude-haiku-5-5", None),
            Harness::Codex => ("gpt-6-luna", None),
            Harness::Muse => ("muse-spark-1.3", Some("minimal")),
            Harness::Grok => ("grok-4.7", Some("low")),
            _ => panic!("not a permitted live fixture harness"),
        };
        let suite =
            suite::load(Path::new(&std::env::var("CEDR_BENCH_LIVE_SUITE").unwrap())).unwrap();
        let version = std::env::var("CEDR_BENCH_LIVE_VERSION").unwrap();
        let options = Options {
            harness: h,
            model: model.into(),
            effort: effort.map(str::to_string),
            version,
            local: false,
            build_image: true,
            set_reference: true,
            stop_usd: None,
            allow_unpriced: false,
            tasks: Vec::new(),
            command: Vec::new(),
            billing: "plan".into(),
        };
        let mut b = ProcessBackend::new(&suite, &options).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let workspace = scratch.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        initialize(&workspace).unwrap();
        let home = scratch.path().join("home");
        for d in [".codex", ".claude", ".grok", ".config/muse"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        let argv = adapter::argv(h, model, effort, PROBE_PROMPT, &[]).unwrap();
        let output = b
            .container
            .as_ref()
            .unwrap()
            .run(
                &argv,
                &workspace,
                &home,
                b.credential.as_ref(),
                h,
                Duration::from_secs(180),
            )
            .unwrap();
        b.check_sign_in().unwrap();
        // Persist usage-shaped keys only; paths, messages, credentials and prompts cannot enter fixtures.
        let text = String::from_utf8_lossy(&output.stdout);
        let mut reduced = Vec::new();
        let events: Vec<serde_json::Value> = serde_json::from_str(&text)
            .map(|v| vec![v])
            .unwrap_or_else(|_| {
                text.lines()
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect()
            });
        for mut v in events {
            if v["payload_type"] == "run.usage" {
                let mut payload = v["payload"].clone();
                if let Some(p) = payload.as_object_mut() {
                    p.retain(|k, _| {
                        ["kind", "models", "calls", "completed_calls"].contains(&k.as_str())
                    });
                }
                reduced.push(serde_json::json!({"payload_type":"run.usage","payload":payload}));
                continue;
            }
            if let Some(o) = v.as_object_mut() {
                o.retain(|k, _| {
                    ["type", "usage", "modelUsage", "num_turns", "stats"].contains(&k.as_str())
                });
            }
            if v.get("usage").is_some() || v.get("modelUsage").is_some() || v.get("stats").is_some()
            {
                reduced.push(v);
            }
        }
        let dir = std::path::PathBuf::from(std::env::var("CEDR_BENCH_CAPTURE_DIR").unwrap());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{}.jsonl", h.name())),
            reduced
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let usage = adapter::parse(h, &output.stdout, model);
        println!(
            "{}: success={} tools_ok={} usage={usage:?}",
            h.name(),
            output.succeeded(),
            std::fs::read(workspace.join("probe.txt")).is_ok_and(|b| b == b"cedr-bench-probe\n")
        );
        assert!(output.succeeded());
        assert_eq!(
            std::fs::read(workspace.join("probe.txt")).unwrap(),
            b"cedr-bench-probe\n"
        );
        assert!(usage.tokens.input().is_some());
    }
}
