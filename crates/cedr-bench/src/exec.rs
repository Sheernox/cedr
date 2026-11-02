//! Processes and files for trials: a step with a timeout, git, and the copy of `test_files` into a
//! clone the agent controlled.

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{Result, invalid, io_error};

/// Output past this is dropped from the front: the end holds the usage a harness reports.
pub const MAX_OUTPUT: usize = 16 * 1024 * 1024;

/// How long to wait for output after a process exits. A daemon it left behind can hold the pipe open
/// for as long as it likes, and must not hang the run.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct StepOutput {
    /// None when the step timed out and was killed.
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
}

impl StepOutput {
    pub fn succeeded(&self) -> bool {
        self.status.is_some_and(|s| s.success())
    }
}

/// Runs `cmd` to completion or until `timeout`, feeding it `stdin` and capturing the end of stdout.
/// Stderr is discarded: agents write to it freely, and nothing reads it. On a timeout, and after a
/// normal exit, the whole process group is killed, so no helper outlives its trial.
pub fn run_step(
    mut cmd: Command,
    stdin: Option<&str>,
    timeout: Duration,
) -> std::io::Result<StepOutput> {
    // Stdin is a file, not a pipe: `cedr` runs with the default SIGPIPE, so writing to a child that
    // exits without reading would kill the whole run.
    let input = match stdin {
        Some(text) => {
            let mut file = tempfile::tempfile()?;
            file.write_all(text.as_bytes())?;
            file.seek(SeekFrom::Start(0))?;
            Stdio::from(file)
        }
        None => Stdio::null(),
    };
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(input);
    isolate(&mut cmd);
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let captured = Arc::new(Mutex::new(VecDeque::new()));
    let (drained_tx, drained_rx) = mpsc::channel();
    if let Some(mut pipe) = child.stdout.take() {
        let captured = Arc::clone(&captured);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 65536];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut buf = captured.lock().unwrap_or_else(|e| e.into_inner());
                buf.extend(&chunk[..n]);
                let over = buf.len().saturating_sub(MAX_OUTPUT);
                buf.drain(..over);
            }
            let _ = drained_tx.send(());
        });
    }
    let (done_tx, done_rx) = mpsc::channel();
    // The child comes back with its status so its handle stays open through kill_tree: on Windows
    // that keeps the pid from being reused while taskkill walks the tree under it.
    std::thread::spawn(move || {
        let status = child.wait();
        let _ = done_tx.send((status, child));
    });
    let (status, held) = match done_rx.recv_timeout(timeout) {
        Ok((status, child)) => (Some(status?), Some(child)),
        Err(_) => {
            kill_tree(pid);
            (
                None,
                done_rx
                    .recv_timeout(DRAIN_GRACE)
                    .ok()
                    .map(|(_, child)| child),
            )
        }
    };
    if status.is_some() {
        kill_tree(pid);
    }
    drop(held);
    let _ = drained_rx.recv_timeout(DRAIN_GRACE);
    let stdout = captured
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .copied()
        .collect();
    Ok(StepOutput { status, stdout })
}

#[cfg(unix)]
fn isolate(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn isolate(_cmd: &mut Command) {}

#[cfg(unix)]
fn kill_tree(pid: u32) {
    // SAFETY: kill takes no pointers. The child was started as its own group leader, so -pid names
    // only that group; the group outlives a reaped leader while any member remains.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A shell command as the platform runs one.
pub fn shell(script: &str) -> Command {
    if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", script]);
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }
}

/// The program's path on `PATH`, trying Windows' executable extensions so `claude.cmd` is found.
/// Rust escapes arguments for `.cmd` and `.bat` targets, or refuses when it cannot.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    let candidates = |p: &Path| -> Vec<PathBuf> {
        if cfg!(windows) && p.extension().is_none() {
            ["exe", "cmd", "bat"]
                .iter()
                .map(|x| p.with_extension(x))
                .collect()
        } else {
            vec![p.to_path_buf()]
        }
    };
    if path.components().count() > 1 {
        // Absolute, because the step runs from inside a trial's clone.
        return candidates(path)
            .into_iter()
            .find(|p| p.is_file())
            .and_then(|p| std::fs::canonicalize(p).ok());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| candidates(&dir.join(name)))
        .find(|p| p.is_file())
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    // A hook's environment must not point these commands at another repository.
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
    ] {
        cmd.env_remove(var);
    }
    cmd.stdin(Stdio::null());
    cmd
}

fn git_stdout(dir: &Path, args: &[&str], what: &str) -> Result<String> {
    let output = git(dir)
        .args(args)
        .output()
        .map_err(|e| invalid(format!("{what}: cannot run git: {e}")))?;
    if !output.status.success() {
        return Err(invalid(format!(
            "{what}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn git_root(dir: &Path) -> Result<PathBuf> {
    let text = git_stdout(
        dir,
        &["rev-parse", "--show-toplevel"],
        &format!("{}: not in a git repo", dir.display()),
    )?;
    Ok(PathBuf::from(text.trim()))
}

pub fn resolve_ref(root: &Path, git_ref: &str) -> Result<String> {
    let spec = format!("{git_ref}^{{commit}}");
    let text = git_stdout(
        root,
        &["rev-parse", "--verify", "--quiet", &spec],
        &format!("ref \"{git_ref}\" does not resolve to a commit"),
    )?;
    Ok(text.trim().to_string())
}

/// Clones `commit` into `dest` with objects shared from `root`. The clone has no remote, so an agent
/// working in it cannot push anywhere through it.
pub fn clone_at(root: &Path, commit: &str, dest: &Path) -> std::result::Result<(), String> {
    let run = |cmd: &mut Command, what: &str| -> std::result::Result<(), String> {
        let output = cmd
            .stdout(Stdio::null())
            .output()
            .map_err(|e| format!("{what}: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{what}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    };
    run(
        git(root)
            .args(["clone", "--quiet", "--shared", "--no-checkout", "--"])
            .arg(root)
            .arg(dest),
        "git clone",
    )?;
    run(
        git(dest).args(["checkout", "--quiet", "--detach", commit]),
        "git checkout",
    )?;
    run(git(dest).args(["remote", "remove", "origin"]), "git remote")
}

/// Whether a path in a commit is agent setup: what Guard and Sync read, at the depth they read it.
fn is_agent_file(path: &str) -> bool {
    const ANY_DEPTH: [&str; 3] = ["AGENTS.md", "CLAUDE.md", "GEMINI.md"];
    const FILES: [&str; 5] = [
        ".mcp.json",
        "opencode.json",
        ".vscode/mcp.json",
        ".github/copilot-instructions.md",
        ".github/mcp.json",
    ];
    const TREES: [&str; 8] = [
        ".agents/",
        ".claude/",
        ".codex/",
        ".cursor/",
        ".gemini/",
        ".opencode/",
        ".grok/",
        ".github/skills/",
    ];
    let name = path.rsplit('/').next().unwrap_or(path);
    ANY_DEPTH.contains(&name) || FILES.contains(&path) || TREES.iter().any(|t| path.starts_with(t))
}

/// Digest of the agent files in `commit`: two equal digests measured the same setup.
pub fn config_digest(root: &Path, commit: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    let listing = git_stdout(
        root,
        &["ls-tree", "-r", commit],
        &format!("cannot list {commit}"),
    )?;
    let mut hash = Sha256::new();
    for line in listing.lines() {
        if line
            .split_once('\t')
            .is_some_and(|(_, path)| is_agent_file(path.trim_matches('"')))
        {
            hash.update(line.as_bytes());
            hash.update(b"\n");
        }
    }
    Ok(crate::suite::hex(&hash.finalize()))
}

/// Files under `dir` as (`/`-separated relative name, path), sorted so a digest does not depend on
/// the order the file system returns them. A symlink is an error: it could point outside the repo.
pub fn walk_files(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        for entry in std::fs::read_dir(dir).map_err(io_error(dir))? {
            let entry = entry.map_err(io_error(dir))?;
            let path = entry.path();
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            let kind = entry.file_type().map_err(io_error(&path))?;
            if kind.is_symlink() {
                return Err(invalid(format!(
                    "{}: test_files cannot hold symlinks",
                    path.display()
                )));
            }
            if kind.is_dir() {
                walk(&path, &format!("{name}/"), out)?;
            } else {
                out.push((name, path));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    if std::fs::metadata(dir).map_err(io_error(dir))?.is_file() {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push((name, dir.to_path_buf()));
    } else {
        walk(dir, "", &mut out)?;
    }
    out.sort();
    Ok(out)
}

/// Copies the tree at `from` onto `clone`. The agent has had the run of `clone`, and a symlink it
/// or hard link it left at any destination would send this write outside the clone, so what is
/// there is replaced rather than followed.
pub fn overlay(from: &Path, clone: &Path) -> Result<()> {
    for (name, source) in walk_files(from)? {
        let mut dest = clone.to_path_buf();
        let parts: Vec<&str> = name.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            dest.push(part);
            let last = i + 1 == parts.len();
            match std::fs::symlink_metadata(&dest) {
                Ok(meta) if meta.is_symlink() => remove_link(&dest)?,
                Ok(meta) if last && meta.is_dir() => {
                    std::fs::remove_dir_all(&dest).map_err(io_error(&dest))?;
                }
                // Removed even at the destination: writing over a hard link the agent made would
                // change the file it points at, wherever that is.
                Ok(meta) if !meta.is_dir() => {
                    std::fs::remove_file(&dest).map_err(io_error(&dest))?;
                }
                _ => {}
            }
            if !last && !dest.is_dir() {
                std::fs::create_dir(&dest).map_err(io_error(&dest))?;
            }
        }
        std::fs::copy(&source, &dest).map_err(io_error(&dest))?;
    }
    Ok(())
}

fn remove_link(path: &Path) -> Result<()> {
    std::fs::remove_file(path)
        .or_else(|_| std::fs::remove_dir(path))
        .map_err(io_error(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_files_are_recognised_at_the_depth_they_are_read() {
        for yes in [
            "AGENTS.md",
            "crates/x/AGENTS.md",
            ".claude/skills/a/SKILL.md",
            ".mcp.json",
            ".github/skills/a/SKILL.md",
        ] {
            assert!(is_agent_file(yes), "{yes}");
        }
        for no in ["src/.mcp.json", "README.md", ".github/workflows/ci.yml"] {
            assert!(!is_agent_file(no), "{no}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_timeout_kills_the_children_of_a_step_too() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("late");
        let script = format!("(sleep 1; touch {}) & wait", marker.display());
        let started = std::time::Instant::now();
        let out = run_step(shell(&script), None, Duration::from_millis(200)).unwrap();
        assert!(out.status.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(1100));
        assert!(!marker.exists(), "the backgrounded child survived");
    }

    #[cfg(unix)]
    #[test]
    fn a_daemon_holding_the_pipe_does_not_hang_a_step_that_exited() {
        // setsid escapes the process group, as a daemon does; only the grace period can end the wait.
        if find_program("setsid").is_none() {
            return;
        }
        let out = run_step(
            shell("setsid sleep 30 & echo done"),
            None,
            Duration::from_secs(20),
        )
        .unwrap();
        assert!(out.succeeded());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "done");
    }

    #[cfg(unix)]
    #[test]
    fn overlay_replaces_a_symlink_the_agent_left_instead_of_writing_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("t.txt"), "victim").unwrap();
        let clone = dir.path().join("clone");
        std::fs::create_dir(&clone).unwrap();
        // A directory link on the way, and a file link at the destination.
        std::os::unix::fs::symlink(&outside, clone.join("tests")).unwrap();
        std::os::unix::fs::symlink(outside.join("t.txt"), clone.join("top.txt")).unwrap();
        let from = dir.path().join("from");
        std::fs::create_dir_all(from.join("tests")).unwrap();
        std::fs::write(from.join("tests/t.txt"), "real").unwrap();
        std::fs::write(from.join("top.txt"), "real").unwrap();

        overlay(&from, &clone).unwrap();

        assert_eq!(
            std::fs::read_to_string(outside.join("t.txt")).unwrap(),
            "victim"
        );
        assert_eq!(
            std::fs::read_to_string(clone.join("tests/t.txt")).unwrap(),
            "real"
        );
        assert_eq!(
            std::fs::read_to_string(clone.join("top.txt")).unwrap(),
            "real"
        );
    }

    #[test]
    fn overlay_replaces_a_hard_link_instead_of_writing_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "victim").unwrap();
        let clone = dir.path().join("clone");
        std::fs::create_dir(&clone).unwrap();
        std::fs::hard_link(&outside, clone.join("check.txt")).unwrap();
        let from = dir.path().join("from");
        std::fs::create_dir(&from).unwrap();
        std::fs::write(from.join("check.txt"), "real").unwrap();

        overlay(&from, &clone).unwrap();

        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "victim");
        assert_eq!(
            std::fs::read_to_string(clone.join("check.txt")).unwrap(),
            "real"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_files_refuse_a_symlink_of_their_own() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("p")).unwrap();
        assert!(
            walk_files(dir.path())
                .unwrap_err()
                .to_string()
                .contains("symlinks")
        );
    }
}
