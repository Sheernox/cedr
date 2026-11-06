//! Pure per-platform schedule generation, separate from installation.
use crate::Result;
#[derive(Debug)]
pub struct File {
    pub relative: String,
    pub contents: String,
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub fn files(os: &str, exe: &str, minutes: u32) -> Result<Vec<File>> {
    if minutes == 0 || minutes > 43200 {
        return Err("every must be 1-43200 minutes".into());
    }
    if exe.contains(['\n', '\r']) {
        return Err("invalid executable path".into());
    }
    match os {
        "linux" => {
            let executable = exe
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('%', "%%");
            Ok(vec![
                File {
                    relative: ".config/systemd/user/cedr-monitor.service".into(),
                    contents: format!(
                        "[Unit]\nDescription=Cedr Monitor hourly counts\n[Service]\nType=oneshot\nExecStart=\"{executable}\" monitor push --quiet\n"
                    ),
                },
                File {
                    relative: ".config/systemd/user/cedr-monitor.timer".into(),
                    contents: format!(
                        "[Unit]\nDescription=Cedr Monitor timer\n[Timer]\nOnBootSec=2min\nOnActiveSec=2min\nOnUnitActiveSec={minutes}min\nRandomizedDelaySec=10min\nPersistent=true\n[Install]\nWantedBy=timers.target\n"
                    ),
                },
            ])
        }
        "macos" => Ok(vec![File {
            relative: "Library/LaunchAgents/dev.cedr.monitor.plist".into(),
            contents: format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>Label</key><string>dev.cedr.monitor</string><key>ProgramArguments</key><array><string>{}</string><string>monitor</string><string>push</string><string>--quiet</string></array><key>RunAtLoad</key><true/><key>StartInterval</key><integer>{}</integer></dict></plist>\n",
                xml(exe),
                minutes * 60
            ),
        }]),
        "windows" => Ok(vec![File {
            relative: "cedr-monitor.xml".into(),
            contents: format!(
                "<?xml version=\"1.0\"?><Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\"><Triggers><RegistrationTrigger><Delay>PT2M</Delay></RegistrationTrigger><TimeTrigger><Repetition><Interval>PT{minutes}M</Interval></Repetition><StartBoundary>2026-01-01T00:00:00</StartBoundary><RandomDelay>PT10M</RandomDelay></TimeTrigger></Triggers><Principals><Principal id=\"Author\"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals><Actions Context=\"Author\"><Exec><Command>{}</Command><Arguments>monitor push --quiet</Arguments></Exec></Actions></Task>",
                xml(exe)
            ),
        }]),
        _ => Err("unsupported scheduler platform".into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linux_timer_starts_after_install() {
        let files = files("linux", "cedr", 60).unwrap();
        assert!(files[1].contents.contains("OnActiveSec=2min\n"));
        assert!(files[1].contents.contains("OnUnitActiveSec=60min\n"));
    }
    #[test]
    fn macos_agent_starts_at_load() {
        assert!(
            files("macos", "cedr", 60).unwrap()[0]
                .contents
                .contains("<key>RunAtLoad</key><true/>")
        );
    }
    #[test]
    fn windows_task_starts_after_registration() {
        assert!(
            files("windows", "cedr.exe", 60).unwrap()[0]
                .contents
                .contains("<RegistrationTrigger><Delay>PT2M</Delay></RegistrationTrigger>")
        );
    }
    #[test]
    fn pure_generation() {
        assert!(
            files("linux", "/a b/cedr", 60).unwrap()[0]
                .contents
                .contains("\"/a b/cedr\" monitor push")
        );
        assert!(
            files("macos", "a&b", 30).unwrap()[0]
                .contents
                .contains("a&amp;b")
        );
        assert!(
            files("windows", "cedr.exe", 60).unwrap()[0]
                .contents
                .contains("PT60M")
        );
        assert!(files("linux", "cedr", 0).is_err());
    }
}
