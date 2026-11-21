#!/usr/bin/env python3
"""Deterministic stdio MCP server used by Worker integration tests.

It implements the protocol operations the pinned rmcp client needs:
initialize, initialized notification, ping, tools/list, tools/call,
resources/list, resources/templates/list, resources/read, prompts/list, and
prompts/get. Every side effect is an explicit file so tests can prove real
execution.
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


RESOURCE_TEXT = "alpha resource body"
HUGE_TEXT = "y" * 100000
BINARY = "AAECAw=="

RESOURCES = [
    {
        "uri": "file:///notes/alpha.txt",
        "name": "alpha",
        "description": "Small deterministic note.",
        "mimeType": "text/plain",
    },
    {
        "uri": "file:///notes/huge.txt",
        "name": "huge",
        "description": "Resource larger than the configured ceiling.",
        "mimeType": "text/plain",
    },
    {
        "uri": "file:///notes/binary.bin",
        "name": "binary",
        "description": "Binary resource that must never be returned raw.",
        "mimeType": "application/octet-stream",
    },
    {
        "uri": "file:///notes/hidden.txt",
        "name": "hidden",
        "description": "A resource the operator allowlist never exposes.",
        "mimeType": "text/plain",
    },
]

RESOURCE_TEMPLATES = [
    {
        "uriTemplate": "file:///notes/{name}.txt",
        "name": "note-template",
        "description": "Parameterised note.",
        "mimeType": "text/plain",
    }
]

PROMPTS = [
    {
        "name": "summarize",
        "description": "Summarize one topic.",
        "arguments": [
            {"name": "topic", "description": "What to summarize.", "required": True}
        ],
    },
    {
        "name": "hidden",
        "description": "A prompt the operator allowlist never exposes.",
        "arguments": [],
    },
]


def read_resource(uri):
    if uri == "file:///notes/alpha.txt":
        return [{"uri": uri, "mimeType": "text/plain", "text": RESOURCE_TEXT}]
    if uri == "file:///notes/huge.txt":
        return [{"uri": uri, "mimeType": "text/plain", "text": HUGE_TEXT}]
    if uri == "file:///notes/binary.bin":
        return [{"uri": uri, "mimeType": "application/octet-stream", "blob": BINARY}]
    return None


def get_prompt(name, arguments):
    if name == "summarize":
        topic = str((arguments or {}).get("topic", ""))
        return {
            "description": "Summarize one topic.",
            "messages": [
                {"role": "user", "content": {"type": "text", "text": "Summarize " + topic}},
                {"role": "assistant", "content": {"type": "text", "text": "ok"}},
            ],
        }
    return None


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
                        "capabilities": {
                            "tools": {},
                            "resources": {},
                            "prompts": {},
                        },
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
        elif method == "resources/list":
            respond(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {"resources": RESOURCES},
                }
            )
        elif method == "resources/templates/list":
            respond(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {"resourceTemplates": RESOURCE_TEMPLATES},
                }
            )
        elif method == "resources/read":
            uri = params.get("uri", "")
            contents = read_resource(uri)
            if contents is None:
                error(request_id, -32002, "resource not found: " + str(uri))
            else:
                respond(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": {"contents": contents},
                    }
                )
        elif method == "prompts/list":
            respond(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {"prompts": PROMPTS},
                }
            )
        elif method == "prompts/get":
            name = params.get("name", "")
            result = get_prompt(name, params.get("arguments"))
            if result is None:
                error(request_id, -32602, "prompt not found: " + str(name))
            else:
                respond(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": result,
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
