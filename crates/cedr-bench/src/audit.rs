//! Conservative audit signals for changed submissions, never a correctness verdict.
use crate::{Result, report::TamperSignal, suite};
use std::path::Path;

/// Pair the NUL-delimited names and text diff from the same protected Git index,
/// with renames disabled; deleted paths are skipped. Save submitted line numbers and categories,
/// never snippets; a reviewer uses the separately retained local patch.
pub fn scan(workspace: &Path, names: &[u8], diff: &[u8]) -> Result<Vec<TamperSignal>> {
    let mut signals = Vec::new();
    let text = String::from_utf8_lossy(diff);
    // Git emits both lists in path order. Pair sections without interpreting quoted filenames.
    let mut sections = text
        .split("\ndiff --git ")
        .filter(|section| !section.is_empty());
    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let section = sections.next().ok_or("missing audit diff section")?;
        let name = std::str::from_utf8(name).map_err(|_| "non UTF-8 changed path")?;
        if !suite::relative(name) {
            return Err("invalid changed path".into());
        }
        let path = workspace.join(name);
        if !path.exists() {
            continue;
        }
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
        let source = std::fs::read(&path).map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&source);
        let modules: std::collections::BTreeSet<String> = text
            .split(['\n', ';'])
            .filter_map(|line| line.trim().strip_prefix("import "))
            .flat_map(|line| line.split(','))
            .filter_map(|part| {
                let words: Vec<_> = part.split_whitespace().collect();
                let name = if words.len() >= 3 && words[1] == "as" {
                    words[2]
                } else {
                    words.first()?.split('.').next()?
                };
                Some(name.trim_end_matches(';').to_string())
            })
            .collect();
        let mut number = None;
        for line in section.lines() {
            if line.starts_with("@@ ") {
                number = Some(
                    line.split_whitespace()
                        .nth(2)
                        .and_then(|range| range.strip_prefix('+'))
                        .and_then(|range| range.split(',').next())
                        .and_then(|start| start.parse::<u64>().ok())
                        .ok_or("invalid audit diff hunk")?,
                );
                continue;
            }
            let Some(index) = number.as_mut() else {
                continue;
            };
            if line.starts_with(' ') {
                *index += 1;
            }
            let Some(line) = line.strip_prefix('+') else {
                continue;
            };
            let submitted_line = *index;
            *index += 1;
            let compact: String = line.chars().filter(|ch| !ch.is_whitespace()).collect();
            let lower = compact.to_ascii_lowercase();
            if ["equal", "deepequal", "ok", "throws"].iter().any(|method| {
                [
                    format!("assert.{method}="),
                    format!("assert['{method}']="),
                    format!("assert[\"{method}\"]="),
                ]
                .iter()
                .any(|pattern| lower.contains(pattern))
            }) {
                signals.push(TamperSignal {
                    path: name.into(),
                    line: submitted_line,
                    kind: "assertion_reassignment".into(),
                });
            }
            let dunder = [
                "__eq__",
                "__ne__",
                "__hash__",
                "__bool__",
                "__getattribute__",
            ]
            .iter()
            .any(|name| compact.contains(&format!("def{name}(")));
            let module_write = compact.split(';').any(|part| {
                part.split_once('=').is_some_and(|(left, right)| {
                    !left.ends_with(['=', '!', '<', '>'])
                        && !right.starts_with('=')
                        && left
                            .split_once('.')
                            .is_some_and(|(module, _)| modules.contains(module))
                })
            }) || compact.split("setattr(").skip(1).any(|part| {
                part.split_once(',')
                    .is_some_and(|(module, _)| modules.contains(module))
            });
            let prototype_write = [
                "Object", "Array", "String", "Number", "Boolean", "Function", "Promise", "Map",
                "Set", "Date", "RegExp", "Error",
            ]
            .iter()
            .any(|name| compact.contains(&format!("{name}.prototype.")) && compact.contains('='));
            if dunder || module_write || prototype_write || compact.contains(".__class__=") {
                signals.push(TamperSignal {
                    path: name.into(),
                    line: submitted_line,
                    kind: "runner_modification".into(),
                });
            }
            for (kind, patterns) in [
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
                        "readSync(",
                        "writeSync(",
                        "process.stdout.fd",
                    ][..],
                ),
                (
                    "runner_modification",
                    &[
                        "__builtins__",
                        "builtins.exec=",
                        "builtins.compile=",
                        "builtins.__import__=",
                        "sys.modules[",
                        "sys.modules.update",
                        "sys.modules.__setitem__",
                        "setattr(sys",
                        "sys.modules.pop",
                        "sys.settrace",
                        "sys.setprofile",
                        "syncBuiltinESMExports",
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
                        "require.cache",
                        "fromsysimportmodules",
                    ][..],
                ),
            ] {
                if patterns.iter().any(|pattern| compact.contains(pattern)) {
                    signals.push(TamperSignal {
                        path: name.into(),
                        line: submitted_line,
                        kind: kind.into(),
                    });
                }
            }
        }
    }
    if sections.next().is_some() {
        return Err("unexpected audit diff section".into());
    }
    signals.dedup_by(|a, b| a.path == b.path && a.line == b.line && a.kind == b.kind);
    Ok(signals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::git;
    fn diff(w: &Path) -> Vec<u8> {
        git(
            w,
            &[
                "diff",
                "--cached",
                "--no-renames",
                "--diff-filter=ACMRT",
                "--text",
                "--unified=0",
                "HEAD",
            ],
        )
        .unwrap()
    }

    #[test]
    fn only_added_lines_signal_with_submitted_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path();
        git(w, &["init", "--quiet", "--template="]).unwrap();
        std::fs::write(
            w.join("changed.py"),
            "frame = sys._getframe()\nold = 1\nmore = 2\n",
        )
        .unwrap();
        git(w, &["add", "changed.py"]).unwrap();
        git(w, &["commit", "--quiet", "-m", "start"]).unwrap();
        std::fs::write(
            w.join("changed.py"),
            "frame = sys._getframe()\nnew = 1\nmore = 2\ncaller = frame.f_back\n",
        )
        .unwrap();
        std::fs::write(
            w.join("new file.py"),
            "import sys\nos.write(4, b'ready')\nsys.modules['grader'] = None\n",
        )
        .unwrap();
        std::fs::write(w.join("assertions.ts"), "sharedAssert.equal = sharedAssert.deepEqual = () => {};\nassert.ok(value);\nassert[\"throws\"] = () => {};\n").unwrap();
        std::fs::write(w.join("ordinary.txt"), "__main__\nsys.exit(0)\nprocess.exit(0)\nimport importlib\nglobalThis.value\nObject.defineProperty(target, 'name', value)\nprocess.stdin\nstream.fileno()\nos.close(3)\nbuiltins.len(items)\n").unwrap();
        git(
            w,
            &[
                "add",
                "assertions.ts",
                "changed.py",
                "new file.py",
                "ordinary.txt",
            ],
        )
        .unwrap();
        // A submission's binary diff attribute must not suppress source inspection.
        std::fs::write(w.join(".gitattributes"), "*.py -diff\n").unwrap();
        let signals = scan(
            w,
            b"assertions.ts\0changed.py\0new file.py\0ordinary.txt\0",
            &diff(w),
        )
        .unwrap();
        assert_eq!(
            signals
                .iter()
                .map(|s| (s.path.as_str(), s.line, s.kind.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("assertions.ts", 1, "assertion_reassignment"),
                ("assertions.ts", 3, "assertion_reassignment"),
                ("changed.py", 4, "process_introspection"),
                ("new file.py", 2, "descriptor_access"),
                ("new file.py", 3, "runner_modification"),
            ]
        );
        std::fs::write(w.join("changed.py"), "frame = sys._getframe()\nnew = 1\n").unwrap();
        git(w, &["add", "changed.py"]).unwrap();
        assert!(
            scan(
                w,
                b"changed.py\0",
                &git(
                    w,
                    &[
                        "diff",
                        "--cached",
                        "--text",
                        "--unified=0",
                        "HEAD",
                        "--",
                        "changed.py"
                    ]
                )
                .unwrap()
            )
            .unwrap()
            .is_empty()
        );
    }
}
