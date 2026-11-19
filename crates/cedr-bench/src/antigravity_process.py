"""Drive Antigravity over ACP and emit only Bench usage, never the conversation."""

from __future__ import annotations

from contextlib import closing
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import subprocess
import sys
from typing import BinaryIO, Iterator

INSTALL = Path("/opt/antigravity")
GEMINI_HOME = Path("/home/bench/.gemini")
WORKSPACE = "/workspace"


class AcpError(Exception):
    """A launcher-authored error safe to expose outside the container."""


class Acp:
    """Keep tool permissions and notifications flowing while one request is outstanding."""

    def __init__(self, stdin: BinaryIO, stdout: BinaryIO) -> None:
        self.stdin = stdin
        self.stdout = stdout
        self.request_id = 0
        self.tools = 0

    def send(self, message: dict) -> None:
        self.stdin.write(json.dumps({"jsonrpc": "2.0", **message}).encode() + b"\n")
        self.stdin.flush()

    def call(self, method: str, params: dict) -> dict:
        self.request_id += 1
        self.send({"id": self.request_id, "method": method, "params": params})
        while True:
            line = self.stdout.readline(16 * 1024 * 1024 + 1)
            if not line or not line.endswith(b"\n"):
                raise AcpError(f"Antigravity ACP closed stdout or sent an oversized frame during {method}")
            try:
                event = json.loads(line)
            except (ValueError, UnicodeDecodeError):
                continue
            if not isinstance(event, dict):
                continue
            if "method" not in event and event.get("id") == self.request_id:
                if not isinstance(event.get("result"), dict) or "error" in event:
                    raise AcpError(f"Antigravity ACP refused {method}")
                return event["result"]
            self.handle(event)

    def handle(self, event: dict) -> None:
        method = event.get("method")
        if method == "session/update":
            if event["params"]["update"].get("sessionUpdate") == "tool_call":
                self.tools += 1
        elif method == "session/request_permission":
            option = next(
                (item for item in event["params"]["options"] if item["kind"] == "allow_once"),
                None,
            )
            if option is None:
                raise AcpError("Antigravity ACP has no single-use permission option")
            self.send({
                "id": event["id"],
                "result": {"outcome": {"outcome": "selected", "optionId": option["optionId"]}},
            })
        elif method is not None and "id" in event:
            self.send({
                "id": event["id"],
                "error": {"code": -32601, "message": "client method unsupported"},
            })


def varint(data: bytes, start: int) -> tuple[int, int]:
    """Limit unsigned protobuf integers to the wire format's 64 bits."""
    value = 0
    for shift in range(0, 64, 7):
        if start >= len(data):
            break
        byte = data[start]
        start += 1
        if shift == 63 and byte > 1:
            break
        value |= (byte & 127) << shift
        if byte < 128:
            return value, start
    raise ValueError("invalid protobuf varint")


def fields(data: bytes) -> Iterator[tuple[int, int | bytes | None]]:
    """Skip unneeded wire types without treating their payloads as token counts."""
    pos = 0
    while pos < len(data):
        key, pos = varint(data, pos)
        number, wire = key >> 3, key & 7
        if not 0 < number < 2**29:
            raise ValueError("invalid protobuf field number")
        value: int | bytes | None = None
        if wire == 0:
            value, end = varint(data, pos)
        elif wire == 2:
            length, pos = varint(data, pos)
            end = pos + length
            value = data[pos:end]
        elif wire in (1, 5):
            end = pos + (8 if wire == 1 else 4)
        else:
            raise ValueError("unsupported protobuf wire type")
        if end > len(data):
            raise ValueError("truncated protobuf field")
        yield number, value
        pos = end


def selected_fields(data: bytes, numbers: set[int]) -> dict[int, int | bytes | None]:
    """Reject duplicate known fields in the vendor's undocumented usage layout."""
    result = {}
    for number, value in fields(data):
        if number in numbers:
            if number in result:
                raise ValueError("ambiguous protobuf field")
            result[number] = value
    return result


def decode_usage(data: bytes) -> dict[str, int | None]:
    """Read one gen_metadata row. Partial or changed schemas cannot yield a price."""
    usage: dict[str, int | None] = {
        "input_tokens": None,
        "cache_read_tokens": None,
        "cache_write_tokens": 0,
        "output_tokens": None,
        "reasoning_tokens": None,
    }
    if not isinstance(data, bytes):
        return usage
    try:
        wrapper = selected_fields(data, {1}).get(1)
        if not isinstance(wrapper, bytes):
            return usage
        counts = selected_fields(wrapper, {4}).get(4)
        if not isinstance(counts, bytes):
            return usage
        tokens: dict[int, int] = {}
        for number, count in selected_fields(counts, {2, 3, 5, 9, 10}).items():
            if not isinstance(count, int):
                return usage
            tokens[number] = count
        fresh = tokens.get(2)
        cached = tokens.get(5, 0)
        output, thinking, visible = (tokens.get(field) for field in (3, 9, 10))
        # Thinking is already part of output. An inconsistent split signals a schema change.
        if output is not None and thinking is not None and visible is not None:
            if output != thinking + visible:
                return usage
    except ValueError:
        return usage
    return {
        **usage,
        "input_tokens": fresh,
        "cache_read_tokens": cached,
        "output_tokens": output,
        "reasoning_tokens": thinking,
    }


def emit_usage(session_id: str, model: str, tools: int) -> None:
    """Read committed WAL rows while the ACP server still owns the session."""
    db = GEMINI_HOME / "antigravity-acp" / "conversations" / f"{session_id}.db"
    turns = None
    if db.is_file():
        with closing(sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True)) as connection:
            if connection.execute(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='gen_metadata'"
            ).fetchone():
                turns = 0
                for (data,) in connection.execute("SELECT data FROM gen_metadata ORDER BY idx"):
                    print(json.dumps({"model": model, **decode_usage(data)}))
                    turns += 1
    print(json.dumps({"turns": turns, "tools": tools}))


def run(acp: Acp, args: list[str], prompt: str | None) -> None:
    initialized = acp.call("initialize", {
        "protocolVersion": 1,
        "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False}, "terminal": False},
    })
    if args == ["--initialize"]:
        version = initialized.get("agentInfo", {}).get("version")
        if not isinstance(version, str):
            raise AcpError("Antigravity ACP did not report its version")
        print(json.dumps(version))
        return

    session = acp.call("session/new", {"cwd": WORKSPACE, "mcpServers": []})
    session_id = session.get("sessionId")
    if not isinstance(session_id, str) or not re.fullmatch(r"[a-zA-Z0-9_-]+", session_id):
        raise AcpError("Antigravity ACP did not return a usable session ID")
    models = [
        option["value"]
        for config in session.get("configOptions", [])
        if config.get("id") == "model"
        for option in config.get("options", [])
        if isinstance(option.get("value"), str)
    ]
    if args == ["--discover"]:
        print(json.dumps(models))
        return

    model_id = args[0]
    if model_id not in models:
        raise AcpError("Antigravity ACP does not offer the requested model and effort")
    acp.call("session/set_model", {"sessionId": session_id, "modelId": model_id})
    acp.tools = 0
    result = acp.call("session/prompt", {
        "sessionId": session_id,
        "prompt": [{"type": "text", "text": prompt}],
    })
    if result.get("stopReason") != "end_turn":
        raise AcpError("Antigravity ACP did not complete the prompt")
    emit_usage(session_id, model_id.rsplit("-", 1)[0], acp.tools)


def main(args: list[str]) -> int:
    """Run with the task environment and let Docker bound the invocation deadline."""
    prompt = None
    if args not in (["--initialize"], ["--discover"]):
        if len(args) != 2:
            print("Antigravity launcher requires a model ID and prompt file", file=sys.stderr)
            return 2
        try:
            path = Path(args[1])
            prompt = path.read_text(encoding="utf-8")
            path.unlink()
        except (OSError, UnicodeError):
            print("cannot read or remove the Antigravity prompt file", file=sys.stderr)
            return 1
    env = {
        key: os.environ[key]
        for key in (
            "PATH", "HOME", "LANG", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME",
            "XDG_CACHE_HOME", "BENCH_SEED", "HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy",
        )
        if key in os.environ
    }
    env.update(
        GEMINI_HOME=str(GEMINI_HOME),
        AGY_ACP_FORCE_FILE_STORAGE="1",
        PYTHONUNBUFFERED="1",
        TMPDIR="/tmp",
        ANTIGRAVITY_HARNESS_PATH=str(INSTALL / "localharness_external"),
    )
    try:
        process = subprocess.Popen(
            [INSTALL / "agy_acp_server.par", "--uid="],
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
    except OSError:
        print("cannot start the Antigravity ACP server", file=sys.stderr)
        return 1
    try:
        assert process.stdin is not None and process.stdout is not None
        run(Acp(process.stdin, process.stdout), args, prompt)
    except AcpError as error:
        print(str(error), file=sys.stderr)
        return 1
    except (OSError, sqlite3.Error, ValueError, KeyError, TypeError, AttributeError, RecursionError):
        print("cannot read Antigravity ACP or its session usage", file=sys.stderr)
        return 1
    finally:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
