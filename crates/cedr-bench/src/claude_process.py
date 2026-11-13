"""Bound Claude container cleanup by its terminal protocol result.

A background shell tool can keep the CLI alive after it has emitted its final
usage and answer. Once that result arrives, stop helpers before grading. The
container exits with this launcher, including helpers that escaped its group.
"""
from __future__ import annotations

import json
import os
import signal
import subprocess
import sys


def main() -> int:
    process = subprocess.Popen(sys.argv[1:], stdout=subprocess.PIPE, start_new_session=True)
    try:
        assert process.stdout is not None
        for line in process.stdout:
            sys.stdout.buffer.write(line)
            sys.stdout.buffer.flush()
            try:
                event = json.loads(line)
            except (ValueError, UnicodeDecodeError):
                continue
            if (isinstance(event, dict) and event.get("type") == "result"
                    and isinstance(event.get("subtype"), str) and isinstance(event.get("is_error"), bool)):
                return 1 if event["is_error"] else 0
        return process.wait()
    finally:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()


if __name__ == "__main__":
    sys.exit(main())
