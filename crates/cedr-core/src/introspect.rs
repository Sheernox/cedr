//! `cedr lock --introspect`: runs a configured stdio server and asks it for
//! its tools over MCP. This executes the server; the CLI warns before calling it.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::transport::TokioChildProcess;

use crate::surface::Tool;
use crate::{Error, Result, canonical};

const TIMEOUT: Duration = Duration::from_secs(20);

/// Calls `initialize` and `tools/list` with a 20-second limit for both, then
/// shuts the server down. Only PATH, HOME and configured env reach the child;
/// it runs in `cwd` and its stderr is discarded.
pub(crate) fn tools(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Vec<Tool>> {
    let fail = |e: &dyn std::fmt::Display| Error::Introspect(format!("{command}: {e}"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| fail(&e))?;
    runtime.block_on(async {
        let cmd = child_command(command, args, env, cwd);
        let (transport, _) = TokioChildProcess::builder(cmd)
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| fail(&e))?;
        let session = async {
            let client = ().serve(transport).await.map_err(|e| fail(&e))?;
            let listed = client.list_all_tools().await.map_err(|e| fail(&e));
            // The child is killed when the transport drops even if cancel fails.
            let _ = client.cancel().await;
            listed
        };
        let listed = tokio::time::timeout(TIMEOUT, session)
            .await
            .map_err(|_| fail(&"no tools/list answer within 20 seconds"))??;
        Ok(listed
            .into_iter()
            .map(|tool| Tool {
                name: tool.name.into_owned(),
                description: tool.description.map(|d| d.into_owned()),
                schema: Some(canonical::to_string(&serde_json::Value::Object(
                    (*tool.input_schema).clone(),
                ))),
            })
            .collect())
    })
}

/// Builds the restricted launch environment before spawning a server.
fn child_command(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(command);
    cmd.env_clear();
    for key in ["PATH", "HOME"] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd.args(args).envs(env).current_dir(cwd);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn child_environment_contains_only_allowlisted_and_configured_keys() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let output = runtime.block_on(async {
            child_command(
                "/usr/bin/env",
                &[],
                &BTreeMap::from([
                    ("CEDR_CONFIGURED".into(), "yes".into()),
                    ("HOME".into(), "/configured".into()),
                ]),
                Path::new("."),
            )
            .output()
            .await
            .unwrap()
        });
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let vars: BTreeMap<_, _> = text.lines().map(|l| l.split_once('=').unwrap()).collect();
        assert_eq!(vars["CEDR_CONFIGURED"], "yes");
        assert_eq!(vars["HOME"], "/configured");
        assert!(
            vars.keys()
                .all(|k| ["PATH", "HOME", "CEDR_CONFIGURED"].contains(k))
        );
    }
}
