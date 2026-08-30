#!/usr/bin/env python3
"""Minimal newline-delimited stdio MCP server for the live A2A capture."""

import json
import pathlib
import sys


log_path = pathlib.Path(sys.argv[1])
log_path.parent.mkdir(parents=True, exist_ok=True)

tools = [
    {
        "name": "agents",
        "description": "List the amux fleet.",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "send",
        "description": "Send a message to another amux agent.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "to": {"type": "string"},
                "text": {"type": "string"},
                "context": {"type": "string"},
            },
            "required": ["to", "text"],
        },
    },
    {
        "name": "spawn",
        "description": "Create an amux child agent.",
        "inputSchema": {"type": "object", "properties": {"kind": {"type": "string"}, "prompt": {"type": "string"}}},
    },
    {
        "name": "stop",
        "description": "Stop an amux child agent.",
        "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
    },
    {
        "name": "status",
        "description": "Set the current amux work status.",
        "inputSchema": {"type": "object", "properties": {"working_on": {"type": ["string", "null"]}}},
    },
]


def reply(request_id, result):
    if request_id is not None:
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    with log_path.open("a", encoding="utf-8") as log:
        log.write(json.dumps(request) + "\n")
    method = request.get("method")
    request_id = request.get("id")
    if method == "initialize":
        reply(request_id, {
            "protocolVersion": request.get("params", {}).get("protocolVersion", "2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "amux-capture", "version": "1.0"},
        })
    elif method == "tools/list":
        reply(request_id, {"tools": tools})
    elif method == "tools/call":
        reply(request_id, {"content": [{"type": "text", "text": "{\"id\":\"a2a-mcp-capture\"}"}]})
    elif method == "ping":
        reply(request_id, {})
