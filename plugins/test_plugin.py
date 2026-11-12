#!/usr/bin/env python3
"""End-to-end check for a plugin directory, through the real MCP server and sandbox.

    python3 plugins/test_plugin.py plugins/project-stats [path/to/cinvar]

It builds a throwaway config dir + project, installs the plugin, enables it, calls every tool
and prints PASS/FAIL. Exit code 0 only if the plugin ran sandboxed. Needs bubblewrap on Linux.
"""
import json, os, shutil, subprocess, sys, tempfile

plugin = os.path.abspath(sys.argv[1])
binary = os.path.abspath(sys.argv[2] if len(sys.argv) > 2 else os.environ.get("CINVAR_BIN", "target/debug/cinvar"))
manifest = json.load(open(os.path.join(plugin, "plugin.json")))
tmp = tempfile.mkdtemp(prefix="cinvar-plugin-test-")
cfg = os.path.join(tmp, "config")
os.makedirs(os.path.join(cfg, "plugins"))
shutil.copytree(plugin, os.path.join(cfg, "plugins", manifest["name"]))
open(os.path.join(cfg, "config.toml"), "w").write("[security]\nallow_plugin_subprocess = true\n")
env = dict(os.environ, CINVAR_CONFIG_DIR=cfg, CINVAR_MCP_PERMISSIONS="full")
subprocess.run([binary, "project", "create", "--name", "PluginTest", "--dir", tmp], env=env, check=True, capture_output=True)
project = os.path.join(tmp, "plugintest")
proc = subprocess.Popen([binary, "mcp", "serve", "--project", project], env=env, text=True, bufsize=1,
                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
_id = 0
def rpc(method, params=None):
    global _id; _id += 1
    proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": _id, "method": method, "params": params or {}}) + "\n")
    proc.stdin.flush()
    return json.loads(proc.stdout.readline())
def call(name, args=None):
    r = rpc("tools/call", {"name": name, "arguments": args or {}})["result"]
    return r.get("isError", False), json.loads(r["content"][0]["text"]) if r["content"][0]["text"].startswith(("{", "[")) else r["content"][0]["text"]
ok = True
def check(label, cond, detail=""):
    global ok; ok &= bool(cond); print(("PASS " if cond else "FAIL ") + label + (f"  [{detail}]" if not cond else ""))
try:
    rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "plugin-test", "version": "1"}})
    err, res = call("cinvar.plugins.set_enabled", {"id": manifest.get("id") or manifest["name"], "enabled": True})
    check("plugin enabled", not err, res)
    proc.stdin.close(); proc.wait(timeout=10)
    # enabling is persisted; a fresh server session picks the plugin up
    proc = subprocess.Popen([binary, "mcp", "serve", "--project", project], env=env, text=True, bufsize=1,
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "plugin-test", "version": "1"}})
    names = [t["name"] for t in rpc("tools/list")["result"]["tools"]]
    for tool in manifest["tools"]:
        full = f"plugin.{manifest['name']}.{tool['name']}"
        check(f"tool listed: {full}", full in names or full.replace(".", "_") in names)
        err, res = call(full, {})
        check(f"tool runs: {full}", not err, res)
        if not err:
            check(f"ran inside the sandbox: {full}", isinstance(res, dict) and res.get("sandboxed") is True, res)
            print("     output:", json.dumps(res)[:300])
finally:
    try: proc.kill()
    except Exception: pass
    shutil.rmtree(tmp, ignore_errors=True)
print("RESULT", "PASS" if ok else "FAIL"); sys.exit(0 if ok else 1)
