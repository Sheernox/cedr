//! Bounded JSON-line RPC for vendor discovery and Antigravity ACP. No conversation data is saved.
use crate::Result;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub struct Rpc {
    child: Child,
    input: ChildStdin,
    lines: Receiver<Value>,
    next: u64,
    deadline: Instant,
    pub tools: u64,
}
impl Rpc {
    pub fn start(mut command: Command, timeout: Duration) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start agent RPC: {e}"))?;
        let input = child.stdin.take().ok_or("RPC stdin missing")?;
        let output = child.stdout.take().ok_or("RPC stdout missing")?;
        let (tx, rx) = mpsc::sync_channel(64);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut bytes = Vec::new();
                match reader
                    .by_ref()
                    .take(16 * 1024 * 1024 + 1)
                    .read_until(b'\n', &mut bytes)
                {
                    Ok(0) | Err(_) => break,
                    Ok(n) if n > 16 * 1024 * 1024 => break,
                    Ok(_) => {
                        if let Ok(v) = serde_json::from_slice(&bytes)
                            && tx.send(v).is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Self {
            child,
            input,
            lines: rx,
            next: 0,
            deadline: Instant::now() + timeout,
            tools: 0,
        })
    }
    pub fn send(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.input, value).map_err(|_| "cannot write agent RPC")?;
        self.input
            .write_all(b"\n")
            .and_then(|()| self.input.flush())
            .map_err(|_| "agent RPC closed stdin".into())
    }
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        loop {
            if cedr_compare::exec::interrupted() {
                return Err("agent RPC interrupted".into());
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("agent RPC timed out".into());
            }
            let value = match self
                .lines
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(v) => v,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Err("agent RPC closed stdout".into()),
            };
            if value["id"] == id && value.get("method").is_none() {
                if value.get("error").is_some() {
                    return Err(format!(
                        "agent RPC refused {method} (code {})",
                        value["error"]["code"]
                    ));
                }
                return Ok(value["result"].clone());
            }
            if value["params"]["update"]["sessionUpdate"] == "tool_call" {
                self.tools += 1;
            }
            if value["method"] == "session/request_permission" {
                let options = value["params"]["options"]
                    .as_array()
                    .ok_or("missing ACP permission choices")?;
                let option = options
                    .iter()
                    .find(|o| o["kind"] == "allow_once")
                    .ok_or("ACP has no single-use permission option")?;
                self.send(&json!({"jsonrpc":"2.0","id":value["id"],"result":{"outcome":{"outcome":"selected","optionId":option["optionId"]}}}))?;
            } else if value.get("method").is_some() && value.get("id").is_some() {
                self.send(&json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32601,"message":"client method unsupported"}}))?;
            }
        }
    }
}
impl Drop for Rpc {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
        }
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/T", "/F", "/PID", &self.child.id().to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
