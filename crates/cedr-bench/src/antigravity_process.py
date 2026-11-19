"""Drive Antigravity over ACP and emit only Bench usage, never the conversation."""

from __future__ import annotations

from contextlib import closing
import json
import os
from pathlib import Path
import queue
import re
import signal
import sqlite3
import subprocess
import sys
import threading
import time
from typing import BinaryIO, Callable, Iterator

INSTALL = Path("/opt/antigravity")
GEMINI_HOME = Path("/home/bench/.gemini")
WORKSPACE = "/workspace"


FRAME_LIMIT = 16 * 1024 * 1024
# Antigravity records a failed model request as an error step but leaves session/prompt unanswered.
ERROR_STEP_TYPE = 17
ERROR_QUIET_SECONDS = 60
POLL_SECONDS = 5


class AcpError(Exception):
    """A launcher-authored error safe to expose outside the container."""


class ProviderError(AcpError):
    """The turn ended on a model provider error; usage up to that point is still reported."""

    def __init__(self, failure: tuple[int | None, str]) -> None:
        super().__init__(failure[1])
        self.status = failure[0]


class Acp:
    """Keep tool permissions and notifications flowing while one request is outstanding."""

    def __init__(self, stdin: BinaryIO, stdout: BinaryIO) -> None:
        self.stdin = stdin
        self.request_id = 0
        self.tools = 0
        self.lines: queue.Queue[bytes | None] = queue.Queue(maxsize=64)
        threading.Thread(target=self.read, args=(stdout,), daemon=True).start()

    def read(self, stdout: BinaryIO) -> None:
        """Read frames on a thread so a waiting request can still notice a stalled turn."""
        while True:
            line = stdout.readline(FRAME_LIMIT + 1)
            if not line or not line.endswith(b"\n"):
                self.lines.put(None)
                return
            self.lines.put(line)

    def send(self, message: dict) -> None:
        self.stdin.write(json.dumps({"jsonrpc": "2.0", **message}).encode() + b"\n")
        self.stdin.flush()

    def call(
        self, method: str, params: dict, stalled: Callable[[float], tuple[int | None, str] | None] | None = None
    ) -> dict:
        """`stalled` gets the seconds since the agent last spoke and returns an error to stop waiting."""
        self.request_id += 1
        self.send({"id": self.request_id, "method": method, "params": params})
        quiet_since = time.monotonic()
        while True:
            try:
                line = self.lines.get(timeout=POLL_SECONDS)
            except queue.Empty:
                failure = stalled(time.monotonic() - quiet_since) if stalled else None
                if failure is not None:
                    raise ProviderError(failure) from None
                continue
            quiet_since = time.monotonic()
            if line is None:
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
        elif method == "fs/read_text_file" and "id" in event:
            try:
                self.send({"id": event["id"], "result": {"content": read_text_file(event["params"])}})
            except ReadRefused as refused:
                self.send({"id": event["id"], "error": {"code": refused.code, "message": str(refused)}})
        elif method is not None and "id" in event:
            self.send({
                "id": event["id"],
                "error": {"code": -32601, "message": "client method unsupported"},
            })


class ReadRefused(Exception):
    """A client file read the launcher declines, reported to the agent as an ACP error."""

    def __init__(self, code: int, message: str) -> None:
        super().__init__(message)
        self.code = code


CLIENT_FILE_MAX_BYTES = 8 * 1024 * 1024


def read_text_file(params: dict) -> str:
    """Serve Antigravity's view_file the way T3 Code does: workspace text files only, optionally sliced by line."""
    requested = params.get("path")
    if not isinstance(requested, str) or not requested:
        raise ReadRefused(-32602, "File read needs a path.")
    real = Path(os.path.realpath(Path(WORKSPACE, requested)))
    root = Path(os.path.realpath(WORKSPACE))
    if real != root and root not in real.parents:
        raise ReadRefused(-32602, f"Path '{requested}' is outside the session workspace.")
    try:
        info = real.stat()
    except OSError:
        raise ReadRefused(-32002, f"File '{requested}' not found.") from None
    if not real.is_file() or info.st_size > CLIENT_FILE_MAX_BYTES:
        raise ReadRefused(-32602, f"File '{requested}' is not a readable text file under {CLIENT_FILE_MAX_BYTES} bytes.")
    try:
        text = real.read_bytes().decode("utf-8")
    except OSError:
        raise ReadRefused(-32603, f"Could not read '{requested}'.") from None
    except UnicodeDecodeError:
        raise ReadRefused(-32602, f"File '{requested}' is not a UTF-8 text file.") from None
    if "\0" in text:
        raise ReadRefused(-32602, f"File '{requested}' contains binary data.")
    line, limit = params.get("line"), params.get("limit")
    if line is None and limit is None:
        return text
    lines = text.split("\n")
    start = max(0, (line if isinstance(line, int) else 1) - 1)
    end = len(lines) if not isinstance(limit, int) else min(len(lines), start + limit)
    return "\n".join(lines[start:end])


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


def conversation(session_id: str) -> Path:
    return GEMINI_HOME / "antigravity-acp" / "conversations" / f"{session_id}.db"


def provider_error(payload: bytes | None) -> tuple[int | None, str]:
    """Name the provider failure by code and status only; the payload can quote the conversation."""
    text = (payload or b"").decode("utf-8", "replace")
    found = re.search(r"Error (\d{3}), Message: .*?, Status: (\d{3} )?([A-Za-z][A-Za-z _]{0,40}?)(?:,|$)", text, re.S)
    if found is None:
        return None, "Antigravity stopped the turn on a model provider error"
    return int(found.group(1)), f"Antigravity stopped the turn on a model provider error ({found.group(1)} {found.group(3).strip()})"


def stalled_on_error(session_id: str) -> Callable[[float], tuple[int | None, str] | None]:
    """Report a turn whose newest step is an error once the agent has been quiet long enough."""

    def check(quiet: float) -> tuple[int | None, str] | None:
        db = conversation(session_id)
        if quiet < ERROR_QUIET_SECONDS or not db.is_file():
            return None
        try:
            with closing(sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True, timeout=1)) as connection:
                row = connection.execute(
                    "SELECT step_type, substr(step_payload, 1, 65536) FROM steps ORDER BY idx DESC LIMIT 1"
                ).fetchone()
        except sqlite3.Error:
            return None
        if row is None or row[0] != ERROR_STEP_TYPE:
            return None
        return provider_error(row[1] if isinstance(row[1], bytes) else None)

    return check


def emit_usage(session_id: str, model: str, tools: int, provider_status: int | None = None) -> None:
    """Read committed WAL rows while the ACP server still owns the session."""
    db = conversation(session_id)
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
    summary = {"turns": turns, "tools": tools}
    if provider_status is not None:
        summary["provider_status"] = provider_status
    print(json.dumps(summary))


def run(acp: Acp, args: list[str], prompt: str | None) -> None:
    # T3 Code serves Antigravity's file reads; without that, view_file calls stall until the agent gives up.
    initialized = acp.call("initialize", {
        "protocolVersion": 1,
        "clientCapabilities": {"fs": {"readTextFile": True, "writeTextFile": False}, "terminal": False},
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
    try:
        result = acp.call(
            "session/prompt",
            {"sessionId": session_id, "prompt": [{"type": "text", "text": prompt}]},
            stalled=stalled_on_error(session_id),
        )
    except ProviderError as failure:
        emit_usage(session_id, model_id.rsplit("-", 1)[0], acp.tools, failure.status)
        raise
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
