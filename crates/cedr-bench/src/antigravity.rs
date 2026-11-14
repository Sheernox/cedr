//! Local-only Antigravity ACP adapter. Authentication files remain linked to their host location.
use crate::{
    Result,
    adapter::{self, Harness},
    container::Credential,
    report::Usage,
    rpc::Rpc,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub fn program() -> Result<PathBuf> {
    if let Some(path) = cedr_compare::exec::find_program("antigravity-acp", Path::new(".")) {
        return Ok(path);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME required")?;
    let root = home.join(".t3/tools/antigravity-acp").join(format!(
        "{}-{}",
        std::env::consts::OS,
        match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            a => a,
        }
    ));
    let v: Value = serde_json::from_slice(
        &std::fs::read(root.join("active.json"))
            .map_err(|_| "Antigravity ACP not installed; add antigravity-acp to PATH")?,
    )
    .map_err(|_| "invalid Antigravity installation manifest")?;
    let release = v["releaseId"]
        .as_str()
        .filter(|s| crate::suite::identifier(s))
        .ok_or("invalid Antigravity release id")?;
    let path = root
        .join("versions")
        .join(release)
        .join("agy_acp_server.par");
    if !path.is_file() {
        return Err("Antigravity ACP executable missing".into());
    }
    Ok(path)
}
pub fn version() -> Result<String> {
    let home = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut command = Command::new(program()?);
    command.env_clear().env("HOME", home.path());
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    let mut rpc = Rpc::start(command, Duration::from_secs(30))?;
    let info=rpc.call("initialize",json!({"protocolVersion":1,"clientInfo":{"name":"cedr-bench","version":env!("CARGO_PKG_VERSION")},"clientCapabilities":{}}))?;
    info["agentInfo"]["version"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "ACP did not report its version".into())
}
pub fn credential() -> Result<Credential> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME required")?;
    let normal = std::env::var_os("GEMINI_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".gemini"))
        .join("antigravity-acp/acp_token.json");
    if normal.is_file() {
        return Ok(Credential {
            source: normal,
            target: "/home/bench/.gemini/antigravity-acp/acp_token.json",
        });
    }
    let roots = home.join(".t3/userdata/providers/antigravity");
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(roots)
        .map_err(|_| "Antigravity sign-in missing; sign in with the vendor first")?
    {
        let p = entry
            .map_err(|e| e.to_string())?
            .path()
            .join("antigravity-acp/acp_token.json");
        if p.is_file() {
            paths.push(p);
        }
    }
    if paths.len() != 1 {
        return Err(
            "Antigravity sign-in is missing or ambiguous; set GEMINI_HOME to its root".into(),
        );
    }
    Ok(Credential {
        source: paths.remove(0),
        target: "/home/bench/.gemini/antigravity-acp/acp_token.json",
    })
}
// Prepare the filtered home and selected credential before capture and RPC.
fn trial_command(
    binary: &Path,
    home: &Path,
    workspace: &Path,
    credential: &Credential,
) -> Result<Command> {
    let mut c = Command::new(binary);
    c.env_clear().current_dir(workspace);
    for key in ["PATH", "LANG", "SYSTEMROOT", "WINDIR", "TMP", "TEMP"] {
        if let Some(value) = std::env::var_os(key) {
            c.env(key, value);
        }
    }
    cedr_agent::clean_home(cedr_agent::Agent::Antigravity, &mut c, home)
        .map_err(|e| e.to_string())?;
    cedr_agent::private_runtime(
        &mut c,
        &home
            .parent()
            .ok_or("clean home has no private parent")?
            .join("runtime"),
    )
    .map_err(|e| e.to_string())?;
    let target = home.join(".gemini/antigravity-acp/acp_token.json");
    std::fs::create_dir_all(target.parent().ok_or("missing credential parent")?)
        .map_err(|e| e.to_string())?;
    if !target.exists() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(&credential.source, &target).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&credential.source, &target)
            .map_err(|e| e.to_string())?;
    }
    Ok(c)
}

fn connection(c: Command, timeout: Duration) -> Result<Rpc> {
    let mut rpc = Rpc::start(c, timeout)?;
    rpc.call("initialize",json!({"protocolVersion":1,"clientInfo":{"name":"cedr-bench","version":env!("CARGO_PKG_VERSION")},"clientCapabilities":{}}))?;
    rpc.call("authenticate", json!({"methodId":"oauth-personal"}))?;
    Ok(rpc)
}
pub fn check(credential: &Credential) -> Result<()> {
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = scratch.path().join("workspace");
    std::fs::create_dir(&workspace).map_err(|e| e.to_string())?;
    let home = scratch.path().join("home");
    let mut command = Command::new(program()?);
    command.env_clear().current_dir(&workspace);
    for key in ["PATH", "LANG", "SYSTEMROOT", "WINDIR", "TMP", "TEMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    cedr_agent::sign_in_home(cedr_agent::Agent::Antigravity, &mut command, &home)
        .map_err(|e| e.to_string())?;
    cedr_agent::private_runtime(&mut command, &scratch.path().join("runtime"))
        .map_err(|e| e.to_string())?;
    let link = home.join(".gemini/antigravity-acp/acp_token.json");
    if !link.exists() {
        std::fs::create_dir_all(link.parent().ok_or("missing sign-in parent")?)
            .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&credential.source, &link).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&credential.source, &link).map_err(|e| e.to_string())?;
    }
    let links = cedr_agent::SignInLink::capture(&home).map_err(|e| e.to_string())?;
    let checked = (|| -> Result<()> {
        let mut rpc = Rpc::start(command, Duration::from_secs(90))?;
        rpc.call("initialize",json!({"protocolVersion":1,"clientInfo":{"name":"cedr-bench","version":env!("CARGO_PKG_VERSION")},"clientCapabilities":{}}))?;
        rpc.call("authenticate", json!({"methodId":"oauth-personal"}))?;
        Ok(())
    })();
    for link in links {
        if let Some(notice) = link.save_renewed().map_err(|e| e.to_string())? {
            eprintln!("{notice}");
        }
    }
    checked
}

pub fn invoke(
    model: &str,
    prompt: &str,
    workspace: &Path,
    home: &Path,
    credential: &Credential,
    timeout: Duration,
) -> Result<Usage> {
    // Populate the filtered home before linking/capturing the selected token.
    let command = trial_command(&program()?, home, workspace, credential)?;
    let links =
        cedr_agent::SignInLink::capture(home).map_err(|_| "cannot track sign-in renewal")?;
    let result = invoke_inner(model, prompt, workspace, home, command, timeout);
    for link in links {
        if let Some(notice) = link.save_renewed().map_err(|e| e.to_string())? {
            eprintln!("{notice}");
        }
    }
    result
}

fn invoke_inner(
    model: &str,
    prompt: &str,
    workspace: &Path,
    home: &Path,
    command: Command,
    timeout: Duration,
) -> Result<Usage> {
    let mut rpc = connection(command, timeout)?;
    let session = rpc.call("session/new", json!({"cwd":workspace,"mcpServers":[]}))?;
    let id = session["sessionId"]
        .as_str()
        .ok_or("ACP did not return a session id")?;
    let models = session["models"]["availableModels"]
        .as_array()
        .ok_or("ACP did not report available models")?;
    if !models.iter().any(|m| m["modelId"] == model) {
        return Err("Antigravity model not offered by ACP".into());
    }
    rpc.call("session/set_model", json!({"sessionId":id,"modelId":model}))?;
    let modes = session["modes"]["availableModes"]
        .as_array()
        .ok_or("ACP did not report permission modes")?;
    let mode = modes
        .iter()
        .find(|m| m["id"] == "yolo")
        .or_else(|| modes.iter().find(|m| m["id"] == "full-access"))
        .ok_or("ACP does not offer a noninteractive tool mode")?;
    rpc.call(
        "session/set_mode",
        json!({"sessionId":id,"modeId":mode["id"]}),
    )?;
    let response = rpc.call(
        "session/prompt",
        json!({"sessionId":id,"prompt":[{"type":"text","text":prompt}]}),
    )?;
    if response["stopReason"] != "end_turn" {
        return Err("Antigravity did not complete the prompt".into());
    }
    let tools = rpc.tools;
    drop(rpc);
    let usage_file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    let store = cedr_monitor::store::Store::open(usage_file.path()).map_err(|e| e.to_string())?;
    let path = home.join(".gemini/antigravity-acp/conversations");
    if path.exists() {
        for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|x| x == "db") {
                cedr_monitor::sources::protobuf::collect(
                    &path,
                    &cedr_monitor::config::Config::default(),
                    &store,
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }
    let mut stmt=store.db.prepare("SELECT model,sum(input),sum(read),sum(output),sum(reasoning),count(*) FROM responses GROUP BY model").map_err(|e|e.to_string())?;
    let rows=stmt.query_map([],|row|Ok(json!({"model":row.get::<_,String>(0)?,"input_tokens":row.get::<_,Option<i64>>(1)?,"cache_read_tokens":row.get::<_,Option<i64>>(2)?,"output_tokens":row.get::<_,Option<i64>>(3)?,"reasoning_tokens":row.get::<_,Option<i64>>(4)?,"turns":row.get::<_,i64>(5)?}))).map_err(|e|e.to_string())?.collect::<std::result::Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
    // One normalized event per resolved model, consumed by the same tested parser as fixtures.
    let text = rows
        .iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    #[cfg(test)]
    if let Ok(dir) = std::env::var("CEDR_BENCH_CAPTURE_DIR") {
        let dir = Path::new(&dir);
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("antigravity.jsonl"), &text).map_err(|e| e.to_string())?;
    }
    let mut usage = adapter::parse(Harness::Antigravity, text.as_bytes(), model);
    usage.tools = Some(tools);
    Ok(usage)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn trial_home_is_populated_before_credential_capture() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let source = dir.path().join("token.json");
        std::fs::write(&source, "{}").unwrap();
        let credential = Credential {
            source,
            target: "/home/bench/.gemini/antigravity-acp/acp_token.json",
        };
        let command =
            trial_command(Path::new("unused-acp"), &home, dir.path(), &credential).unwrap();
        assert!(home.join(".config").is_dir());
        assert_eq!(cedr_agent::SignInLink::capture(&home).unwrap().len(), 1);
        if let Some(user_home) = std::env::var_os("HOME") {
            let user_home = PathBuf::from(user_home);
            for name in [".cargo", ".local", ".gitconfig"] {
                if user_home.join(name).exists() {
                    assert!(home.join(name).exists(), "missing shadow-home entry {name}");
                }
            }
        }
        assert!(command.get_envs().any(|(key, value)| key == "XDG_DATA_HOME"
            && value.is_some_and(|p| Path::new(p).starts_with(dir.path().join("runtime")))));
    }
}
