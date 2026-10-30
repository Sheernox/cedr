//! The CLI's own settings in `$XDG_CONFIG_HOME/cedr/cli.toml` (docs/telemetry.md, Settings):
//! telemetry, the install ID, whether the notice was shown, and automatic updates. Rewrites keep
//! keys this version does not know, so an older cedr never drops a newer one's settings.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use toml_edit::{DocumentMut, value};

pub(crate) struct Settings {
    doc: DocumentMut,
    /// False when the file exists but does not parse: then nothing is sent or updated, and the
    /// file is never overwritten.
    pub(crate) valid: bool,
}

pub(crate) fn path() -> PathBuf {
    cedr_route::config::xdg_dir("XDG_CONFIG_HOME", ".config")
        .join("cedr")
        .join("cli.toml")
}

/// Path of a file under `$XDG_STATE_HOME/cedr`.
pub(crate) fn state_file(name: &str) -> PathBuf {
    cedr_route::config::xdg_dir("XDG_STATE_HOME", ".local/state")
        .join("cedr")
        .join(name)
}

impl Settings {
    pub(crate) fn load() -> Self {
        match std::fs::read_to_string(path()) {
            Ok(text) => match text.parse::<DocumentMut>() {
                Ok(doc) => Self { doc, valid: true },
                Err(_) => Self {
                    doc: DocumentMut::new(),
                    valid: false,
                },
            },
            Err(_) => Self {
                doc: DocumentMut::new(),
                valid: true,
            },
        }
    }

    fn flag(&self, key: &str) -> bool {
        self.valid && self.doc.get(key).and_then(|v| v.as_bool()).unwrap_or(true)
    }

    pub(crate) fn telemetry(&self) -> bool {
        self.flag("telemetry")
    }

    pub(crate) fn auto_update(&self) -> bool {
        self.flag("auto_update")
    }

    pub(crate) fn notice_shown(&self) -> bool {
        self.doc.get("notice").and_then(|v| v.as_integer()) == Some(1)
    }

    pub(crate) fn install_id(&self) -> Option<&str> {
        self.doc
            .get("install_id")
            .and_then(|v| v.as_str())
            .filter(|s| is_uuid(s))
    }

    pub(crate) fn set_bool(&mut self, key: &str, on: bool) {
        self.doc[key] = value(on);
    }

    pub(crate) fn set_notice_shown(&mut self) {
        self.doc["notice"] = value(1);
    }

    pub(crate) fn set_install_id(&mut self, id: Option<&str>) {
        match id {
            Some(id) => self.doc["install_id"] = value(id),
            None => {
                self.doc.remove("install_id");
            }
        }
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        if !self.valid {
            return Err(format!(
                "{} does not parse; fix or remove it",
                path().display()
            ));
        }
        write_atomic(&path(), self.doc.to_string().as_bytes())
    }
}

/// Replaces `path` through a temporary file and a rename, so a reader or an interrupted write never
/// sees a half-written file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(format!(
        ".{}.{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })
}

/// How far a stamp may sit in the future and still count as recent: file times come from a coarser
/// clock than `SystemTime::now`, so a stamp written just now can read slightly ahead of it.
const STAMP_SKEW: Duration = Duration::from_secs(5);

/// Whether the work a stamp throttles is due (its last start is `every` or more ago); marks it
/// started when due. A stamp far in the future (the clock moved back) counts as due.
pub(crate) fn stamp_due(stamp: &Path, now: SystemTime, every: Duration) -> bool {
    let recent = std::fs::metadata(stamp)
        .and_then(|m| m.modified())
        .is_ok_and(|t| match now.duration_since(t) {
            Ok(age) => age < every,
            Err(ahead) => ahead.duration() < STAMP_SKEW,
        });
    if recent {
        return false;
    }
    if let Some(dir) = stamp.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(stamp, b"").is_ok()
}

/// A random UUID v4 from the OS random source; never derived from the machine or the person.
pub(crate) fn new_uuid() -> Option<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).ok()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex = b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
            }
        })
}

/// Whether an opt-out variable is set: anything but empty, `0`, `false` or `off`, so
/// `DO_NOT_TRACK=false` reads as tracking allowed, as other tools read it.
pub(crate) fn env_set(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| {
        let v = v.trim().to_ascii_lowercase();
        !matches!(v.as_str(), "" | "0" | "false" | "off")
    })
}

/// CI as docs/telemetry.md detects it; CI runs never print the notice or update themselves.
pub(crate) fn in_ci() -> bool {
    env_set("CI")
        || [
            "GITHUB_ACTIONS",
            "GITLAB_CI",
            "BUILDKITE",
            "CIRCLECI",
            "TF_BUILD",
            "JENKINS_URL",
        ]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_version_4_uuids() {
        let id = new_uuid().unwrap();
        assert!(is_uuid(&id), "{id}");
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
        assert_ne!(id, new_uuid().unwrap());
    }

    #[test]
    fn rewrites_keep_unknown_keys_and_unset_flags_default_on() {
        let mut s = Settings {
            doc: "future = \"kept\"\nauto_update = false\n".parse().unwrap(),
            valid: true,
        };
        assert!(s.telemetry());
        assert!(!s.auto_update());
        s.set_bool("telemetry", false);
        s.set_install_id(None);
        let text = s.doc.to_string();
        assert!(text.contains("future = \"kept\""), "{text}");
        assert!(!s.telemetry());
    }

    #[test]
    fn a_settings_file_that_does_not_parse_turns_everything_off() {
        let s = Settings {
            doc: DocumentMut::new(),
            valid: false,
        };
        assert!(!s.telemetry() && !s.auto_update());
        assert!(s.save().is_err());
    }

    #[test]
    fn stamps_throttle_and_tolerate_a_stamp_just_ahead_of_the_clock() {
        let dir = tempfile::tempdir().unwrap();
        let stamp = dir.path().join("state/stamp");
        let every = Duration::from_secs(3600);
        let now = SystemTime::now();
        assert!(stamp_due(&stamp, now, every));
        // The stamp was written after `now`, so it reads slightly ahead of it.
        assert!(!stamp_due(&stamp, now, every));
        assert!(stamp_due(
            &stamp,
            now + every + Duration::from_secs(10),
            every
        ));
        assert!(
            stamp_due(&stamp, now - Duration::from_secs(60), every),
            "clock moved back"
        );
    }

    #[test]
    fn a_tampered_install_id_is_ignored() {
        let s = Settings {
            doc: "install_id = \"/home/someone\"\n".parse().unwrap(),
            valid: true,
        };
        assert_eq!(s.install_id(), None);
    }
}
