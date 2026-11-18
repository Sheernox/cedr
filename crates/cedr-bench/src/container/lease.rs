//! A locked file proves a Bench owner is alive even after PID reuse. Scope the
//! registry to this user's state directory and this Docker daemon.
use super::{Result, docker, docker_cleanup};
use std::{
    collections::BTreeSet,
    fs, io,
    io::Write,
    path::{Path, PathBuf},
};

pub(super) const LABEL: &str = "dev.cedr.bench.run";

pub(super) struct Lease {
    path: PathBuf,
    _lock: fs::File,
    pub(super) run: String,
}

impl Lease {
    pub(super) fn start(state: &Path) -> Result<Self> {
        let daemon = docker(&["info", "--format", "{{.ID}}"])?;
        if daemon.is_empty() {
            return Err("Docker did not report its daemon ID".into());
        }
        let dir = state.join("leases").join(crate::sha(daemon.as_bytes()));
        fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create Bench lease directory: {e}"))?;
        let scope =
            scope_id(&dir).map_err(|e| format!("cannot load Bench ownership scope: {e}"))?;
        let prefix = format!("cedr-bench-{scope}-");
        if let Err(e) = sweep(&dir, &prefix) {
            eprintln!("cedr: bench: could not reclaim abandoned Docker resources: {e}");
        }
        Self::register(&dir, &prefix).map_err(|e| format!("cannot register Bench owner: {e}"))
    }

    fn register(dir: &Path, prefix: &str) -> io::Result<Self> {
        let temp = tempfile::Builder::new()
            .prefix(prefix)
            .rand_bytes(24)
            .tempfile_in(dir)?;
        // Publish the lease only after locking it, before creating any Docker resources.
        temp.as_file().lock()?;
        let run = temp
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let path = dir.join(format!("{run}.lease"));
        let lock = temp.persist_noclobber(&path)?;
        Ok(Self {
            run,
            path,
            _lock: lock,
        })
    }
}

/// A persistent random scope lets later runs recognize resources that an
/// orphaned Docker client creates after its lease has already been reclaimed.
fn scope_id(dir: &Path) -> io::Result<String> {
    let path = dir.join("scope");
    if !path.try_exists()? {
        let mut temp = tempfile::Builder::new()
            .prefix("")
            .rand_bytes(24)
            .tempfile_in(dir)?;
        let id = temp
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        temp.write_all(id.as_bytes())?;
        match temp.persist_noclobber(&path) {
            Ok(_) => (),
            Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.error),
        }
    }
    let id = fs::read_to_string(path)?;
    if valid_id(&id) {
        Ok(id)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Bench scope",
        ))
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 24 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn sweep(dir: &Path, prefix: &str) -> Result<()> {
    let mut runs = BTreeSet::new();
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let name = entry.map_err(|e| e.to_string())?.file_name();
        if let Some(run) = name.to_str().and_then(|s| s.strip_suffix(".lease")) {
            runs.insert(run.to_string());
        }
    }
    let format = format!("{{{{.Label \"{LABEL}\"}}}}");
    for list in [&["ps", "-a"][..], &["network", "ls"]] {
        let labels = docker_cleanup(
            &[
                list,
                &["--filter", &format!("label={LABEL}"), "--format", &format],
            ]
            .concat(),
        )?;
        runs.extend(labels.lines().map(str::to_owned));
    }
    for run in runs {
        if run.strip_prefix(prefix).is_some_and(valid_id) {
            let path = dir.join(format!("{run}.lease"));
            if let Err(e) = recover(&path, &run, cleanup) {
                eprintln!("cedr: bench: could not reclaim abandoned Docker resources: {e}");
            }
        }
    }
    Ok(())
}

fn recover(path: &Path, run: &str, clean: impl FnOnce(&str) -> Result<()>) -> io::Result<()> {
    let file = match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return clean(run).map_err(io::Error::other);
        }
        Err(e) => return Err(e),
    };
    match file.try_lock() {
        Ok(()) => (),
        Err(fs::TryLockError::WouldBlock) => return Ok(()),
        Err(fs::TryLockError::Error(e)) => return Err(e),
    }
    clean(run).map_err(io::Error::other)?;
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Remove by immutable Docker ID, containers first, keeping the lease for retry on failure.
fn cleanup(run: &str) -> Result<()> {
    let filter = format!("label={LABEL}={run}");
    for (list, remove) in [
        (&["ps", "-aq"][..], ["rm", "--force"]),
        (&["network", "ls", "-q"][..], ["network", "rm"]),
    ] {
        let args = [list, &["--no-trunc", "--filter", &filter]].concat();
        let ids = docker_cleanup(&args)?;
        let mut failed = false;
        for id in ids.lines() {
            if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("Docker returned an invalid resource ID".into());
            }
            failed |= docker_cleanup(&[remove[0], remove[1], id]).is_err();
        }
        // Docker may have already auto-removed an object from the listing.
        if failed && !docker_cleanup(&args)?.is_empty() {
            return Err(format!(
                "Docker could not remove all Bench resources listed by {}",
                list.join(" ")
            ));
        }
    }
    Ok(())
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Err(error) =
            cleanup(&self.run).and_then(|()| fs::remove_file(&self.path).map_err(|e| e.to_string()))
        {
            eprintln!("cedr: bench: Docker cleanup incomplete; the next run will retry: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_skips_a_live_owner_and_retries_failures_and_late_resources() {
        let dir = tempfile::tempdir().unwrap();
        let scope = scope_id(dir.path()).unwrap();
        assert_eq!(scope_id(dir.path()).unwrap(), scope);
        let run = format!("cedr-bench-{scope}-{}", "x".repeat(24));
        let path = dir.path().join(format!("{run}.lease"));
        let owner = fs::File::create(&path).unwrap();
        owner.lock().unwrap();
        recover(&path, &run, |_| panic!("live owner was reaped")).unwrap();
        owner.unlock().unwrap();
        drop(owner);
        assert!(recover(&path, &run, |_| Err("daemon unavailable".into())).is_err());
        assert!(path.exists());
        recover(&path, &run, |_| Ok(())).unwrap();
        assert!(!path.exists());
        let mut late = true;
        recover(&path, &run, |_| {
            late = false;
            Ok(())
        })
        .unwrap();
        assert!(!late);
    }
}
