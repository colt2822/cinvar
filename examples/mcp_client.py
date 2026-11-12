#!/usr/bin/env python3
"""Demo 4 - drive CINVAR from an external MCP client.

Speaks JSON-RPC 2.0 over the cinvar MCP server's stdio transport, the same way
Claude Code or any other MCP host does. No third-party packages required.

    python3 examples/mcp_client.py /path/to/project
"""
import json
import os
import subprocess
import sys


def send(proc, payload):
    proc.stdin.write(json.dumps(payload) + "\n")
    proc.stdin.flush()
    reply = proc.stdout.readline()
    if not reply:
        raise RuntimeError("server closed the connection")
    return json.loads(reply)


def main():
    project = sys.argv[1] if len(sys.argv) > 1 else os.getcwd()
    binary = os.environ.get("CINVAR_BIN", "target/debug/cinvar")
    # Agents see only the project by default. The human launching the server chooses extra folders:
    footage = os.path.abspath(os.path.join(project, "..", "footage"))
    env = dict(os.environ, CINVAR_MEDIA_ROOTS=footage)
    proc = subprocess.Popen(
        [binary, "mcp", "serve", "--project", project],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        env=env,
    )
    try:
        init = send(proc, {
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "cinvar-demo", "version": "0.1"}},
        })
        print("initialize ->", json.dumps(init["result"]["serverInfo"]))
        send(proc, {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})

        tools = send(proc, {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
        names = [t["name"] for t in tools["result"]["tools"]]
        print("tools/list ->", len(names), "tools")

        status = send(proc, {"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                             "params": {"name": "cinvar.project.status", "arguments": {}}})
        print("project.status ->", status["result"]["content"][0]["text"][:400])

        calls = [
            ("cinvar.media.import_folder", {"path": footage}),
            ("cinvar.timeline.create", {"name": "MCP cut", "width": 1920, "height": 1080, "fps": "30"}),
        ]
        for name, args in calls:
            res = send(proc, {"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                              "params": {"name": name, "arguments": args}})
            result = res["result"]
            print(name, "->", "ERROR" if result.get("isError") else "ok",
                  result["content"][0]["text"][:200])
    finally:
        proc.stdin.close()
        proc.wait(timeout=10)
        print("server exit code:", proc.returncode)


if __name__ == "__main__":
    main()
