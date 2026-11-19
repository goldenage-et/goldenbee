#!/usr/bin/env python3
"""Deterministic stdio MCP server used by Worker integration tests.

It implements only the protocol operations the pinned rmcp client needs:
initialize, initialized notification, ping, tools/list, and tools/call.
Every side effect is an explicit file so tests can prove real execution.
"""

import json
import os
import sys
import time

PID_FILE = os.environ.get("MCP_PID_FILE")
ENV_FILE = os.environ.get("MCP_ENV_FILE")
EFFECT_FILE = os.environ.get("MCP_EFFECT_FILE")
MODE = os.environ.get("MCP_MODE", "deterministic")


def write_file(path, content, mode="w"):
    if path:
        with open(path, mode, encoding="utf-8") as handle:
            handle.write(content)


def respond(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def error(request_id, code, message):
    respond(
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": code, "message": message},
        }
    )


TOOLS = [
    {
        "name": "record_effect",
        "description": "Append one deterministic value to the effect file.",
        "inputSchema": {
            "type": "object",
            "properties": {"value": {"type": "string"}},
            "required": ["value"],
        },
    },
    {
        "name": "guarded_effect",
        "description": "Append one value that requires a per-tool grant.",
        "inputSchema": {
            "type": "object",
            "properties": {"value": {"type": "string"}},
            "required": ["value"],
        },
    },
    {
        "name": "hidden_effect",
        "description": "A tool the operator allowlist never exposes.",
        "inputSchema": {
            "type": "object",
            "properties": {"value": {"type": "string"}},
            "required": ["value"],
        },
    },
    {
        "name": "crash_server",
        "description": "Exit the server process while handling the call.",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "slow_call",
        "description": "Sleep beyond a configured call deadline.",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "huge_result",
        "description": "Return more text than the configured output cap.",
        "inputSchema": {"type": "object", "properties": {}},
    },
]


def main():
    write_file(PID_FILE, str(os.getpid()))
    if ENV_FILE:
        write_file(ENV_FILE, json.dumps(dict(os.environ), sort_keys=True))

    if MODE == "exit_on_start":
        sys.exit(2)
    if MODE == "malformed":
        sys.stdout.write("this is not a JSON-RPC response\n")
        sys.stdout.flush()
        time.sleep(0.2)
        sys.exit(3)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError:
            error(None, -32700, "parse error")
            continue
        method = request.get("method")
        request_id = request.get("id")
        params = request.get("params") or {}
        if method == "initialize":
            respond(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {"tools": {}},
                        "serverInfo": {
                            "name": "goldenbee-mcp-fixture",
                            "version": "1.0.0",
                        },
                    },
                }
            )
        elif method == "notifications/initialized":
            continue
        elif method == "ping":
            respond({"jsonrpc": "2.0", "id": request_id, "result": {}})
        elif method == "tools/list":
            respond(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {"tools": TOOLS},
                }
            )
        elif method == "tools/call":
            name = params.get("name")
            arguments = params.get("arguments") or {}
            if name == "record_effect":
                value = str(arguments.get("value", ""))
                with open(EFFECT_FILE, "a", encoding="utf-8") as handle:
                    handle.write(value + "\n")
                respond(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": {
                            "content": [
                                {"type": "text", "text": "recorded:" + value}
                            ]
                        },
                    }
                )
            elif name == "guarded_effect":
                value = str(arguments.get("value", ""))
                with open(EFFECT_FILE, "a", encoding="utf-8") as handle:
                    handle.write("guarded:" + value + "\n")
                respond(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": {
                            "content": [
                                {"type": "text", "text": "guarded:" + value}
                            ]
                        },
                    }
                )
            elif name == "hidden_effect":
                with open(EFFECT_FILE, "a", encoding="utf-8") as handle:
                    handle.write("hidden\n")
                respond(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": {
                            "content": [{"type": "text", "text": "hidden"}]
                        },
                    }
                )
            elif name == "crash_server":
                sys.exit(7)
            elif name == "slow_call":
                time.sleep(5)
                respond(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": {
                            "content": [{"type": "text", "text": "slow"}]
                        },
                    }
                )
            elif name == "huge_result":
                respond(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": {
                            "content": [{"type": "text", "text": "x" * 100000}]
                        },
                    }
                )
            else:
                error(request_id, -32601, "unknown tool")
        else:
            error(request_id, -32601, "method not found")


if __name__ == "__main__":
    main()
