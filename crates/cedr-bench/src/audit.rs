//! Conservative audit signals for changed submissions, never a correctness verdict.
use crate::{Result, report::TamperSignal, suite};
use std::path::Path;

/// Scan changed/added regular files only. Relative paths and signal names are saved,
/// never source snippets. A reviewer uses the separately retained local patch.
pub fn scan(workspace: &Path, names: &[u8]) -> Result<Vec<TamperSignal>> {
    let mut signals = Vec::new();
    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::str::from_utf8(name).map_err(|_| "non UTF-8 changed path")?;
        if !suite::relative(name) {
            return Err("invalid changed path".into());
        }
        let path = workspace.join(name);
        // Check every ancestor so an agent cannot redirect the scan outside its workspace.
        let mut current = workspace.to_path_buf();
        let mut linked = false;
        for component in Path::new(name).components() {
            current.push(component);
            if std::fs::symlink_metadata(&current)
                .map_err(|e| e.to_string())?
                .is_symlink()
            {
                linked = true;
                break;
            }
        }
        if linked {
            continue;
        }
        if !std::fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .is_file()
        {
            continue;
        }
        if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
            signals.push(TamperSignal {
                path: name.into(),
                line: 1,
                kind: "scan_limit".into(),
            });
            continue;
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            let compact: String = line.chars().filter(|ch| !ch.is_whitespace()).collect();
            for (kind, patterns) in [
                (
                    "process_control",
                    &[
                        "os._exit(",
                        "sys.exit(",
                        "process.exit(",
                        "process.abort(",
                        "os.kill(",
                        "os.killpg(",
                        "signal.signal(",
                        "signal.raise_signal(",
                        "signal.pthread_kill(",
                        "process.kill(",
                        "pthread_kill(",
                        "killpg(",
                        "raise_signal(",
                        "importsignal",
                        "fromsignalimport",
                        "fromosimport_exit",
                        "fromsysimportexit",
                    ][..],
                ),
                (
                    "process_introspection",
                    &[
                        "sys._getframe",
                        "inspect.stack",
                        "inspect.currentframe",
                        "gc.get_objects",
                        "gc.get_referrers",
                        ".f_back",
                        ".f_globals",
                        ".f_locals",
                        "__subclasses__",
                        "__globals__",
                        "_getframe",
                        "currentframe(",
                        "get_referrers(",
                        "get_objects(",
                        "importinspect",
                        "frominspectimport",
                        "importgc",
                        "fromgcimport",
                    ][..],
                ),
                (
                    "descriptor_access",
                    &[
                        "/proc/self",
                        "/dev/fd",
                        "os.read(",
                        "os.write(",
                        "os.fdopen(",
                        "os.dup",
                        "os.closerange(",
                        "os.close(",
                        "readSync(",
                        "writeSync(",
                        "process.stdin",
                        "process.stdout.fd",
                        "fileno(",
                    ][..],
                ),
                (
                    "runner_modification",
                    &[
                        "__builtins__",
                        "builtins.",
                        "sys.modules[",
                        "sys.modules.update",
                        "sys.modules.__setitem__",
                        "setattr(sys",
                        "sys.modules.pop",
                        "sys.settrace",
                        "sys.setprofile",
                        "importlib",
                        "__main__",
                        "syncBuiltinESMExports",
                        "Object.defineProperty",
                        "process.removeAllListeners",
                        "process.emit=",
                        "process.exit=",
                        "process.abort=",
                        "os.read=",
                        "os.write=",
                        "os.close=",
                        "sys.exit=",
                        "fs.readSync=",
                        "fs.writeSync=",
                        "globalThis.",
                        "require.cache",
                        "frombuiltinsimport",
                        "__import__(",
                        "fromsysimportmodules",
                    ][..],
                ),
            ] {
                if patterns.iter().any(|pattern| compact.contains(pattern)) {
                    signals.push(TamperSignal {
                        path: name.into(),
                        line: (index + 1) as u64,
                        kind: kind.into(),
                    });
                }
            }
        }
    }
    Ok(signals)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signals_changed_introspection_without_scanning_untouched_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("attack.py"), "import sys\nframe = sys._getframe()\nsys.modules['__main__'] = frame\nos.write(4, b'ready')\nos._exit(0)\n").unwrap();
        std::fs::write(dir.path().join("unchanged.py"), "sys.exit(0)").unwrap();
        let signals = scan(dir.path(), b"attack.py\0").unwrap();
        assert_eq!(signals.len(), 4);
        assert!(
            signals
                .iter()
                .any(|signal| signal.kind == "process_introspection" && signal.line == 2)
        );
        assert!(signals.iter().all(|signal| signal.path == "attack.py"));
        std::fs::write(dir.path().join("clean.py"), "def add(a,b): return a+b").unwrap();
        assert!(scan(dir.path(), b"clean.py\0").unwrap().is_empty());
    }
}
