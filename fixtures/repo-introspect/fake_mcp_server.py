"""Minimal MCP stdio server for `cedr lock --introspect` tests.

Answers initialize and tools/list, ignores notifications, and exits when
stdin closes.
"""

import json
import sys

TOOLS = [
    {
        "name": "lookup",
        "description": "Look up a term",
        "inputSchema": {
            "type": "object",
            "properties": {"term": {"type": "string"}},
            "required": ["term"],
        },
    },
    {
        "name": "echo",
        "description": "Echo text back",
        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
    },
]


def reply(message_id, result=None, error=None):
    body = {"jsonrpc": "2.0", "id": message_id}
    if error is None:
        body["result"] = result
    else:
        body["error"] = error
    sys.stdout.write(json.dumps(body) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message.get("method")
    if method == "initialize":
        reply(
            message["id"],
            {
                "protocolVersion": message["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake", "version": "1.0.0"},
            },
        )
    elif method == "tools/list":
        reply(message["id"], {"tools": TOOLS})
    else:
        reply(message["id"], error={"code": -32601, "message": f"method not found: {method}"})
