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
    pub build_image: bool,
    pub set_reference: bool,
    pub stop_usd: Option<f64>,
    pub allow_unpriced: bool,
    pub tasks: Vec<String>,
    pub billing: String,
}
pub struct Invocation {
    pub status: Status,
    pub usage: Usage,
    pub duration_ms: u64,
    pub credentials: Vec<String>,
    /// HTTP status of a model provider failure the agent reported, when that ended the task.
    pub provider_status: Option<u16>,
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
    container: Container,
    credential: Option<Credential>,
    healthy: bool,
}
impl<'a> ProcessBackend<'a> {
    pub fn new(suite: &Suite, options: &'a Options, state: &Path) -> Result<Self> {
        let credential = container::credential(options.harness)?;
        let container = Container::prepare(
            suite,
            options.harness,
            &options.version,
            options.build_image,
            state,
        )?;
        let mut backend = Self {
            options,
            container,
            credential,
            healthy: true,
        };
        if backend.credential.is_some() {
            backend.check_sign_in()?;
        }
        backend.container.check_effort(
            options.harness,
            &options.model,
            options.effort.as_deref(),
            backend.credential.as_ref(),
        )?;
        Ok(backend)
    }
    fn pin_host_check(&self, argv: &mut [String]) -> Result<()> {
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
    // Proven tool-free container checks let only vendor code coordinate host renewals.
    fn check_sign_in(&mut self) -> Result<()> {
        if exec::interrupted() {
            return Err("bench interrupted".into());
        }
        let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
        let check = if self.options.harness == Harness::Antigravity {
            self.container.check_antigravity_sign_in(scratch.path())
        } else {
            let mut argv =
                cedr_agent::sign_in_args(agent(self.options.harness), &self.options.model)
                    .map_err(|e| e.to_string())?;
            self.pin_host_check(&mut argv)?;
            let out = host_sign_in(&argv, self.options.harness, scratch.path())?;
            if out.succeeded() {
                Ok(())
            } else {
                Err("vendor refused the account check".into())
            }
        };
        if let Err(error) = check {
            self.healthy = false;
            return Err(format!(
                "host sign-in check failed ({error}); repair the vendor sign-in before starting another run"
            ));
        }
        self.credential = container::credential(self.options.harness)?;
        Ok(())
    }
}
// These exact commands have no user tools or setup execution path. Do not use
// this home for inference: the vendor alone owns credential writes and locking.
fn host_sign_in(argv: &[String], h: Harness, workspace: &Path) -> Result<exec::StepOutput> {
    let binary = exec::find_program(&argv[0], Path::new("."))
        .ok_or_else(|| format!("{} is not installed", argv[0]))?;
    let cmd = host_sign_in_command(&binary, &argv[1..], h, workspace)?;
    exec::run_step(cmd, None, Duration::from_secs(120))
        .map_err(|_| "cannot start host sign-in check".into())
}

fn host_sign_in_command(
    binary: &Path,
    args: &[String],
    h: Harness,
    workspace: &Path,
) -> Result<Command> {
    cedr_agent::host_sign_in_command(binary, args, agent(h), workspace).map_err(|e| e.to_string())
}

pub(crate) fn agent(h: Harness) -> cedr_agent::Agent {
    match h {
        Harness::Claude => cedr_agent::Agent::Claude,
        Harness::Codex => cedr_agent::Agent::Codex,
        Harness::Muse => cedr_agent::Agent::Muse,
        Harness::Grok => cedr_agent::Agent::Grok,
        Harness::Gemini => cedr_agent::Agent::Gemini,
        Harness::Antigravity => cedr_agent::Agent::Antigravity,
        Harness::Command => cedr_agent::Agent::Command,
    }
}
impl Backend for ProcessBackend<'_> {
    fn invoke(&mut self, prompt: &str, workspace: &Path, timeout: Duration) -> Result<Invocation> {
        if !self.healthy {
            return Err("host sign-in failed; no further invocations allowed".into());
        }
        if self.credential.is_some() {
            self.check_sign_in()?;
        }
        let home = tempfile::tempdir().map_err(|e| e.to_string())?;
        let argv = adapter::argv(
            self.options.harness,
            &self.options.model,
            self.options.effort.as_deref(),
            prompt,
            &[],
        )?;
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
        if self.options.harness == Harness::Antigravity {
            container::antigravity_settings(home.path())?;
            std::fs::write(home.path().join(".bench-prompt"), prompt)
                .map_err(|e| format!("cannot stage the Antigravity prompt: {e}"))?;
        }
        self.credential = container::credential(self.options.harness)?;
        let credentials =
            crate::evidence::credential_values(self.credential.as_ref(), self.options.harness)?;
        let start = Instant::now();
        let out = self.container.run(
            &argv,
            workspace,
            home.path(),
            self.credential.as_ref(),
            self.options.harness,
            timeout,
        )?;
        let duration_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let mut usage = adapter::parse(self.options.harness, &out.stdout, &self.options.model);
        let provider_status = adapter::provider_status(self.options.harness, &out.stdout);
        let status = if exec::interrupted() {
            Status::Interrupted
        } else if out.status.is_none() {
            Status::Timeout
        } else if out.succeeded() {
            Status::Graded
        } else {
            Status::AgentError
        };
        if matches!(
            status,
            Status::Timeout | Status::Interrupted | Status::AgentError
        ) {
            usage.cost_usd = None;
        }
        if status == Status::AgentError {
            self.healthy = false;
        }
        Ok(Invocation {
            status,
            usage,
            duration_ms,
            credentials,
            provider_status: provider_status.filter(|_| status == Status::AgentError),
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
        let out = self
            .container
            .grade(&suite.root, &task.id, workspace, timeout, seed)?;
        if !out.succeeded() {
            return Err("grader failed or timed out".into());
        }
        Ok(out.stdout)
    }
    fn image(&self) -> Option<String> {
        Some(self.container.base.clone())
    }
    fn environment(&self) -> Environment {
        Environment {
            mode: "container".into(),
            clean_home: cfg!(unix),
            network: "allowlist".into(),
            pinned: self.container.pinned,
            agent_image: Some(self.container.image.clone()),
            verified: self.container.verified.clone(),
            limits: Some("memory=2g,pids=256,tmp=1g,swap=0".into()),
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
// info/attributes outranks submitted attributes, including nested files and macros.
const RAW_ATTRIBUTES: &str =
    "* -ident -filter -text !eol !working-tree-encoding -binary -export-subst diff\n";
fn initialize(workspace: &Path) -> Result<()> {
    git(workspace, &["init", "--quiet", "--template="])?;
    std::fs::create_dir_all(workspace.join(".git/info")).map_err(|e| e.to_string())?;
    std::fs::write(workspace.join(".git/info/attributes"), RAW_ATTRIBUTES)
        .map_err(|e| e.to_string())?;
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
fn patch(
    workspace: &Path,
    metadata: &Path,
    masker: &crate::evidence::Masker,
) -> Result<(Vec<u8>, Vec<TamperSignal>)> {
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
    std::fs::write(metadata.join("info/attributes"), RAW_ATTRIBUTES).map_err(|e| e.to_string())?;
    let metadata = format!("--git-dir={}", metadata.display());
    let tree = format!("--work-tree={}", workspace.display());
    git(workspace, &[&metadata, &tree, "add", "--all", "--force"])?;
    let diff = git(
        workspace,
        &[
            &metadata,
            &tree,
            "diff",
            "--cached",
            "--binary",
            "--text",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--unified=3",
            "HEAD",
        ],
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
            "--diff-filter=ACMRTD",
            "-z",
            "HEAD",
        ],
    )?;
    if masker.exposed(&[&changed, &diff]) {
        return Err("credential_exposed".into());
    }
    let signals = crate::audit::scan(workspace, &changed, &diff)?;
    Ok((diff, signals))
}

// Replaying saved evidence must reconstruct exactly the files handed to the grader.
// Comparing raw bytes fails closed even for future Git transformations we did not predict.
fn verify_evidence(start: &Path, graded: &Path, saved: &Path) -> Result<bool> {
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let replay = scratch.path().join("replay");
    suite::copy_submission(start, &replay)?;
    let bytes = std::fs::read(saved).map_err(|e| e.to_string())?;
    if !bytes.is_empty()
        && git(
            &replay,
            &[
                "apply",
                "--binary",
                "--whitespace=nowarn",
                saved.to_str().ok_or("invalid patch path")?,
            ],
        )
        .is_err()
    {
        return Ok(false);
    }
    fn files(
        root: &Path,
        directory: &Path,
        out: &mut std::collections::BTreeSet<std::path::PathBuf>,
    ) -> Result<()> {
        for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if entry.file_name() == ".git" || kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                files(root, &entry.path(), out)?;
            } else if kind.is_file() {
                out.insert(
                    entry
                        .path()
                        .strip_prefix(root)
                        .map_err(|e| e.to_string())?
                        .to_path_buf(),
                );
            }
        }
        Ok(())
    }
    let mut expected = std::collections::BTreeSet::new();
    let mut actual = std::collections::BTreeSet::new();
    files(&replay, &replay, &mut expected)?;
    files(graded, graded, &mut actual)?;
    if expected != actual {
        return Ok(false);
    }
    for name in expected {
        use std::io::Read;
        let mut left = std::fs::File::open(replay.join(&name)).map_err(|e| e.to_string())?;
        let mut right = std::fs::File::open(graded.join(name)).map_err(|e| e.to_string())?;
        let mut remaining = left.metadata().map_err(|e| e.to_string())?.len();
        if remaining != right.metadata().map_err(|e| e.to_string())?.len() {
            return Ok(false);
        }
        let mut a = [0; 65536];
        let mut b = [0; 65536];
        while remaining > 0 {
            let count = remaining.min(a.len() as u64) as usize;
            left.read_exact(&mut a[..count])
                .map_err(|e| e.to_string())?;
            right
                .read_exact(&mut b[..count])
                .map_err(|e| e.to_string())?;
            if a[..count] != b[..count] {
                return Ok(false);
            }
            remaining -= count as u64;
        }
    }
    Ok(true)
}

// Never open links or special files. Allow bounded trailing whitespace from native file writers.
fn probe_file(workspace: &Path, name: &str, contents: &[u8]) -> bool {
    let path = workspace.join(name);
    std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= 4096)
        && std::fs::read_to_string(path)
            .is_ok_and(|text| text.trim_end().as_bytes() == contents.trim_ascii_end())
}

pub fn probe<B: Backend>(suite: &Suite, options: &Options, backend: &mut B) -> Result<Probe> {
    if exec::interrupted() {
        return Err("bench probe interrupted".into());
    }
    let fingerprint = tempfile::tempdir().map_err(|e| e.to_string())?;
    let first = backend.invoke(
        FINGERPRINT_PROMPT,
        fingerprint.path(),
        Duration::from_secs(180),
    )?;
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
    let mut p =
        crate::report::probe_with_tolerance(&first.usage, reference, options.set_reference, false);
    if exec::interrupted() || first.status == Status::Interrupted {
        // The first request may have known spend even though readiness never ran.
        p.tools = None;
        return Ok(p);
    }
    let readiness = tempfile::tempdir().map_err(|e| e.to_string())?;
    let second = backend.invoke(PROBE_PROMPT, readiness.path(), Duration::from_secs(180))?;
    p.tools_ok = !exec::interrupted()
        && first.status == Status::Graded
        && second.status == Status::Graded
        && probe_file(readiness.path(), "probe.txt", b"cedr-bench-probe\n")
        && probe_file(readiness.path(), "command.txt", b"cedr-bench-command\n");
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
    if exec::interrupted() {
        r.stop = Some(Stop::Interrupted);
    } else if !r.probe.tools_ok {
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
            Err(_) => {
                progress(&format!(
                    "{}: agent failed; check the vendor sign-in and runtime",
                    t.id
                ));
                r.tasks[i].error = Some("agent invocation failed".into());
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
        let provider_status = out.provider_status;
        // No task evidence leaves private staging until all task-controlled output is checked.
        let capture = tempfile::Builder::new()
            .prefix(".capture-")
            .tempdir_in(artifacts)
            .map_err(|_| "cannot stage task evidence")?;
        let staged = capture.path().join("evidence");
        std::fs::create_dir(&staged).map_err(|_| "cannot stage task evidence")?;
        let masker = crate::evidence::Masker::new(out.credentials);
        let mut exposed = out
            .usage
            .models
            .iter()
            .any(|m| masker.exposed(&[m.as_bytes()]));
        task.patch_sha256 = if exposed {
            None
        } else {
            match patch(&workspace, &metadata, &masker) {
                Ok((diff, signals)) => {
                    task.tamper_signals = signals;
                    crate::evidence::write_private(&staged.join("submission.patch"), &diff)?;
                    Some(crate::sha(&diff))
                }
                Err(error) => {
                    exposed = error == "credential_exposed" || masker.exposed(&[error.as_bytes()]);
                    progress(&format!("{}: cannot capture patch", t.id));
                    task.status = Status::SetupFailed;
                    None
                }
            }
        };
        if exposed {
            task.status = Status::SetupFailed;
        }
        let copy = scratch.path().join("grade");
        if exposed {
            // The task is withheld as a whole, including grading and artifact metadata.
        } else if suite::copy_submission(&workspace, &copy).is_err() {
            progress(&format!("{}: cannot copy submission for grading", t.id));
            task.status = Status::SetupFailed;
            task.error = Some("cannot copy submission for grading".into());
        } else if task.patch_sha256.is_some() {
            let verified = verify_evidence(
                &suite.root.join("tasks").join(&t.id).join("repo"),
                &copy,
                &staged.join("submission.patch"),
            );
            match verified {
                Ok(true) => progress(&format!("{}: evidence verified", t.id)),
                _ => {
                    progress(&format!("{}: evidence_mismatch", t.id));
                    task.tamper_signals.push(TamperSignal {
                        path: "submission.patch".into(),
                        line: 1,
                        kind: "evidence_mismatch".into(),
                    });
                }
            }
        }
        if task.status == Status::Graded {
            let seed = u64::from_be_bytes(cedr_bench_seed()?) & ((1 << 30) - 1);
            task.seed = Some(seed);
            let graded = backend.grade(suite, t, &copy, seed).and_then(|b| {
                exposed |= masker.exposed(&[&b]);
                if exposed {
                    Err("credential_exposed".into())
                } else {
                    let grade = crate::report::grade(&b, t.kind)?;
                    exposed |= grade
                        .checks
                        .iter()
                        .map(|c| &c.id)
                        .chain(
                            grade
                                .gates
                                .iter()
                                .flat_map(|g| std::iter::once(&g.id).chain(g.detail.iter())),
                        )
                        .chain(grade.artifacts.iter())
                        .any(|s| masker.exposed(&[s.as_bytes()]));
                    if exposed {
                        Err("credential_exposed".into())
                    } else {
                        Ok(grade)
                    }
                }
            });
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
                            let bytes_to_save = std::fs::read(copy.join(&name))
                                .map_err(|_| "cannot read artifact")?;
                            exposed |= masker.exposed(&[name.as_bytes(), &bytes_to_save]);
                            if exposed {
                                return Err("credential_exposed".into());
                            }
                            let path = format!("{}/{name}", t.id);
                            let dest = staged.join(&name);
                            std::fs::create_dir_all(
                                dest.parent().ok_or("invalid artifact directory")?,
                            )
                            .map_err(|e| e.to_string())?;
                            crate::evidence::write_private(&dest, &bytes_to_save)?;
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
                            exposed |= masker.exposed(&[error.as_bytes()]);
                            progress(&format!("{}: artifact collection failed", t.id));
                            task.status = Status::SetupFailed;
                        }
                    }
                }
                Err(error) => {
                    exposed |= masker.exposed(&[error.as_bytes()]);
                    progress(&format!("{}: grader failed", t.id));
                    task.status = Status::SetupFailed;
                }
            }
        }
        // Inspect actual strings before serialization so escaping cannot hide a
        // registered form in grader metadata. This does not decode nested JSON.
        exposed |= task
            .error
            .iter()
            .chain(task.checks.iter().map(|c| &c.id))
            .chain(
                task.gates
                    .iter()
                    .flat_map(|g| std::iter::once(&g.id).chain(g.detail.iter())),
            )
            .chain(task.artifacts.iter().map(|a| &a.path))
            .chain(task.tamper_signals.iter().flat_map(|s| [&s.path, &s.kind]))
            .any(|s| masker.exposed(&[s.as_bytes()]));
        if exposed {
            // Keep only trusted identifiers and fixed-vocabulary/numeric facts after a match.
            task.status = Status::SetupFailed;
            task.error = None;
            task.patch_sha256 = None;
            task.checks.clear();
            task.gates.clear();
            task.artifacts.clear();
            task.tamper_signals = vec![TamperSignal {
                path: "submission.patch".into(),
                line: 1,
                kind: "descriptor_access".into(),
            }];
        } else {
            for model in out.usage.models {
                if !r.config.resolved_models.contains(&model) {
                    r.config.resolved_models.push(model);
                }
            }
            std::fs::rename(&staged, artifacts.join(&t.id))
                .map_err(|_| "cannot retain task evidence")?;
        }
        progress(&format!(
            "{}: credential guard {} us",
            t.id,
            masker.elapsed().as_micros()
        ));
        if task.status != Status::Graded {
            task.score = if task.kind == Kind::Code {
                Some(0.0)
            } else {
                None
            };
            task.solved = false;
        }
        if task.status == Status::AgentError {
            task.error = Some(match provider_status {
                Some(code) => format!("the agent's model provider failed (HTTP {code}); try the run again later"),
                None => "container agent failed, possibly because its sign-in expired; renewal is blocked; check the vendor sign-in before running again".into(),
            });
            r.stop = Some(Stop::UnknownCost);
        } else if task.status == Status::Interrupted {
            r.stop = Some(Stop::Interrupted);
        } else if task.cost_usd.is_none() && !options.allow_unpriced {
            r.stop = Some(Stop::UnknownCost);
        }
        r.spent_usd += task.cost_usd.unwrap_or(0.0);
    }
    r.finished_at = chrono::Utc::now().to_rfc3339();
    r.summarize();
    if r.eligible {
        crate::evidence::verify_tasks(artifacts, &r.tasks)?;
    }
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
    #[cfg(unix)]
    #[test]
    fn real_home_check_leaves_renewal_writes_to_vendor() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&config).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        let source = config.join("auth.json");
        let vault = dir.path().join("vault.json");
        std::fs::write(
            &vault,
            r#"{"refresh_token":"old-credential-for-guard-regression"}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(&vault, &source).unwrap();
        let stale = Credential::resolve(&source, "/test").unwrap();
        let vendor = dir.path().join("vendor");
        std::fs::write(
            &vendor,
            concat!(
                "#!/bin/sh\n",
                "test \"$1\" = login && test \"$2\" = status || exit 1\n",
                "test -f \"$CODEX_HOME/auth.json\" || exit 2\n",
                "printf '%s' '{\"refresh_token\":\"renewed\"}' > \"$CODEX_HOME/renewed\"\n",
                "mv \"$CODEX_HOME/renewed\" \"$CODEX_HOME/auth.json\"\n",
            ),
        )
        .unwrap();
        std::fs::set_permissions(&vendor, std::fs::Permissions::from_mode(0o700)).unwrap();
        let argv = [
            vendor.to_string_lossy().into_owned(),
            "login".into(),
            "status".into(),
        ];
        let mut cmd =
            host_sign_in_command(&vendor, &argv[1..], Harness::Codex, &workspace).unwrap();
        cmd.env_clear()
            .env("CODEX_HOME", &config)
            .env("PATH", "/usr/bin:/bin");
        assert!(
            exec::run_step(cmd, None, Duration::from_secs(10))
                .unwrap()
                .succeeded()
        );
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            r#"{"refresh_token":"renewed"}"#
        );
        let current = Credential::resolve(&source, "/test").unwrap();
        assert_ne!(stale.source, current.source);
        assert_eq!(current.source, source);
        let values = crate::evidence::credential_values(Some(&current), Harness::Command).unwrap();
        assert!(
            !values
                .iter()
                .any(|value| value == "old-credential-for-guard-regression")
        );
        assert_eq!(
            std::fs::read_to_string(vault).unwrap(),
            r#"{"refresh_token":"old-credential-for-guard-regression"}"#
        );
        assert_eq!(std::fs::read_dir(workspace).unwrap().count(), 0);
    }
    #[cfg(unix)]
    #[test]
    fn real_home_checks_preserve_configured_locations_and_disable_updates() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let config = home.join(".codex");
        std::fs::create_dir_all(&config).unwrap();
        let vault = dir.path().join("vault.json");
        std::fs::write(&vault, "{}").unwrap();
        symlink(&vault, config.join("auth.json")).unwrap();
        for h in [
            Harness::Claude,
            Harness::Codex,
            Harness::Muse,
            Harness::Grok,
        ] {
            let cmd = host_sign_in_command(Path::new("vendor"), &[], h, dir.path()).unwrap();
            let overrides: std::collections::BTreeMap<_, _> = cmd.get_envs().collect();
            for key in [
                "HOME",
                "USERPROFILE",
                "CODEX_HOME",
                "CLAUDE_CONFIG_DIR",
                "MUSE_HOME",
                "MUSE_AUTH_PATH",
                "GROK_HOME",
                "XDG_CONFIG_HOME",
            ] {
                assert!(!overrides.contains_key(std::ffi::OsStr::new(key)));
            }
        }
        let mut cmd = host_sign_in_command(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "test -L \"$CODEX_HOME/auth.json\" && test -f \"$CODEX_HOME/auth.json\"".into(),
            ],
            Harness::Codex,
            dir.path(),
        )
        .unwrap();
        cmd.env_clear()
            .env("HOME", &home)
            .env("CODEX_HOME", &config);
        assert!(
            exec::run_step(cmd, None, Duration::from_secs(10))
                .unwrap()
                .succeeded()
        );
        for (h, key) in [
            (Harness::Claude, "DISABLE_AUTOUPDATER"),
            (Harness::Muse, "MUSE_NO_AUTO_UPDATE"),
            (Harness::Grok, "GROK_DISABLE_AUTOUPDATER"),
        ] {
            let cmd = host_sign_in_command(Path::new("vendor"), &[], h, dir.path()).unwrap();
            assert!(
                cmd.get_envs()
                    .any(|(k, v)| k == key && v == Some(std::ffi::OsStr::new("1")))
            );
        }
    }
    #[test]
    fn ignored_helpers_and_nested_repository_contents_are_saved_hashed_and_scanned() {
        for nested in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let workspace = dir.path().join("workspace");
            std::fs::create_dir(&workspace).unwrap();
            std::fs::write(workspace.join("main.py"), "pass\n").unwrap();
            let start = dir.path().join("start");
            suite::copy_submission(&workspace, &start).unwrap();
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
            let (first, signals) = patch(
                &workspace,
                &metadata,
                &crate::evidence::Masker::new(Vec::new()),
            )
            .unwrap();
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
            let (second, _) = patch(
                &workspace,
                &metadata,
                &crate::evidence::Masker::new(Vec::new()),
            )
            .unwrap();
            assert_ne!(crate::sha(&first), crate::sha(&second));
            let replay = dir.path().join("replay");
            std::fs::create_dir(&replay).unwrap();
            std::fs::write(replay.join("main.py"), "pass\n").unwrap();
            let saved = dir.path().join("submission.patch");
            std::fs::write(&saved, &second).unwrap();
            assert!(verify_evidence(&start, &workspace, &saved).unwrap());
            git(&replay, &["apply", saved.to_str().unwrap()]).unwrap();
            assert_eq!(
                std::fs::read(replay.join("lib/helper.py")).unwrap(),
                std::fs::read(workspace.join("lib/helper.py")).unwrap()
            );
        }
    }
    #[test]
    fn attributes_cannot_hide_graded_bytes_and_replay_fails_closed() {
        for attributes in [
            "ident",
            "-diff",
            "binary",
            "filter=hide",
            "text eol=lf",
            "working-tree-encoding=UTF-16",
            "export-subst",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let start = dir.path().join("start");
            std::fs::create_dir(&start).unwrap();
            std::fs::write(start.join("main.py"), "pass\n").unwrap();
            let workspace = dir.path().join("ws");
            suite::copy_submission(&start, &workspace).unwrap();
            initialize(&workspace).unwrap();
            let metadata = dir.path().join("metadata");
            suite::copy(&workspace.join(".git"), &metadata).unwrap();
            let git_dir = format!("--git-dir={}", metadata.display());
            git(
                &workspace,
                &[
                    &git_dir,
                    "config",
                    "filter.hide.clean",
                    "git hash-object --stdin",
                ],
            )
            .unwrap();
            std::fs::write(
                workspace.join(".gitattributes"),
                format!("helper.py {attributes}\n"),
            )
            .unwrap();
            let code = b"s = \"$Id: \"; import sys; sys._getframe(); s = \" $\"\r\n";
            std::fs::write(workspace.join("helper.py"), code).unwrap();
            let (diff, signals) = patch(
                &workspace,
                &metadata,
                &crate::evidence::Masker::new(Vec::new()),
            )
            .unwrap();
            assert!(
                diff.windows(code.len() - 2)
                    .any(|part| part == &code[..code.len() - 2])
            );
            assert!(signals.iter().any(|s| s.kind == "process_introspection"));
            let saved = dir.path().join("saved.patch");
            std::fs::write(&saved, diff).unwrap();
            let graded = dir.path().join("graded");
            suite::copy_submission(&workspace, &graded).unwrap();
            assert!(
                verify_evidence(&start, &graded, &saved).unwrap(),
                "{attributes}"
            );
            std::fs::write(graded.join("helper.py"), "hidden bytes").unwrap();
            assert!(!verify_evidence(&start, &graded, &saved).unwrap());
        }
    }
    #[test]
    fn ordinary_submissions_verify_including_deletions_and_binary_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let start = dir.path().join("start");
        std::fs::create_dir(&start).unwrap();
        std::fs::write(start.join("removed"), "old").unwrap();
        let ws = dir.path().join("ws");
        suite::copy_submission(&start, &ws).unwrap();
        initialize(&ws).unwrap();
        let metadata = dir.path().join("metadata");
        suite::copy(&ws.join(".git"), &metadata).unwrap();
        std::fs::remove_file(ws.join("removed")).unwrap();
        std::fs::write(ws.join("binary"), b"a\0b\xff").unwrap();
        let (diff, signals) =
            patch(&ws, &metadata, &crate::evidence::Masker::new(Vec::new())).unwrap();
        assert!(signals.is_empty());
        let saved = dir.path().join("saved.patch");
        std::fs::write(&saved, diff).unwrap();
        assert!(verify_evidence(&start, &ws, &saved).unwrap());
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
            build_image: true,
            set_reference: true,
            stop_usd: None,
            allow_unpriced: false,
            tasks: Vec::new(),
            billing: "plan".into(),
        };
        let scratch = tempfile::tempdir().unwrap();
        let mut b = ProcessBackend::new(&suite, &options, scratch.path()).unwrap();
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
