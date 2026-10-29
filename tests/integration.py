"""Compatibility tests: drive a Claudescript server with the official MCP Python SDK."""
import asyncio, re, json, os, subprocess, sys, time, socket
from pathlib import Path
from mcp import ClientSession, StdioServerParameters, types
from mcp.client.stdio import stdio_client
try:
    from mcp.client.streamable_http import streamable_http_client
except ImportError:
    from mcp.client.streamable_http import streamablehttp_client as _old
    import contextlib
    @contextlib.asynccontextmanager
    async def streamable_http_client(url):
        async with _old(url) as (r, w, _g):
            yield r, w

ROOT = Path(__file__).resolve().parent.parent
CLS = os.environ.get("CLS", str(ROOT / "target/debug/cls"))
PROJ = ROOT / "examples/projecttools"
failures = []

def snake(n): return re.sub(r"([A-Z])", lambda m: "_" + m.group(1).lower(), n)
class V:
    """Attribute access that works with both camelCase (mcp 1.x) and snake_case (mcp 2.x) models."""
    def __init__(self, o): self._o = o
    def __getattr__(self, n):
        o = object.__getattribute__(self, "_o")
        for k in (n, snake(n)):
            if hasattr(o, k): return wrap(getattr(o, k))
        raise AttributeError(n)
    def __getitem__(self, k): return wrap(self._o[k])
    def __repr__(self): return repr(self._o)
def wrap(x):
    if isinstance(x, (list, tuple)): return [wrap(i) for i in x]
    if hasattr(x, 'model_fields'): return V(x)
    return x


def check(name, cond, detail=""):
    print(("  ok   " if cond else "  FAIL ") + name + ("" if cond else f"  -> {detail}"))
    if not cond: failures.append(name)

async def exercise(s: ClientSession, label):
    print(f"[{label}]")
    init = wrap(await s.initialize())
    check("protocol negotiated", init.protocolVersion in ("2025-11-25", "2025-06-18"), init.protocolVersion)
    check("server name", init.serverInfo.name == "ProjectTools")
    tools = {t.name: t for t in wrap((await s.list_tools()).tools)}
    check("tools listed", {"listFiles","readText","writeText","stats","slow","boom","spin","mode"} <= set(tools), list(tools))
    check("input schema", tools["stats"].inputSchema["properties"]["values"]["items"]["type"] == "number")
    check("output schema", tools["stats"].outputSchema is not None)
    check("annotations", tools["readText"].annotations and tools["readText"].annotations.readOnlyHint is True)

    r = wrap(await s.call_tool("stats", {"values": [1, 2, 3, 4]}))
    check("stats structured", not r.isError and r.structuredContent["mean"] == 2.5 and r.structuredContent["max"] == 4, r)
    r = wrap(await s.call_tool("readText", {"path": "data/notes.txt"}))
    check("readText", r.content[0].text.strip() == "hello from notes", r)
    r = wrap(await s.call_tool("readText", {"path": "../../etc/passwd"}))
    check("fs sandbox blocks escape", r.isError and "outside the project" in r.content[0].text, r)
    r = wrap(await s.call_tool("readText", {"path": "/etc/passwd"}))
    check("fs sandbox blocks absolute", r.isError, r)
    r = wrap(await s.call_tool("stats", {"values": "nope"}))
    check("input validation (type)", r.isError and "Invalid arguments" in r.content[0].text, r)
    r = wrap(await s.call_tool("stats", {}))
    check("input validation (required)", r.isError, r)
    r = wrap(await s.call_tool("mode", {"kind": "medium"}))
    check("enum validation", r.isError, r)
    r = wrap(await s.call_tool("mode", {"kind": "fast", "tags": ["a", "b"]}))
    check("enum + optional ok", not r.isError and r.content[0].text == "fast:a+b", r)
    r = wrap(await s.call_tool("boom", {"reason": "x"}))
    check("thrown error -> isError", r.isError and "failed: x" in r.content[0].text, r)
    r = wrap(await s.call_tool("stats", {"values": []}))
    check("handler error surfaced", r.isError and "must not be empty" in r.content[0].text, r)
    t0 = time.time()
    r = wrap(await s.call_tool("spin", {}))
    check("execution time limit", r.isError and "time limit" in r.content[0].text and time.time() - t0 < 10, r)
    try:
        await s.call_tool("nope", {})
        check("unknown tool -> protocol error", False)
    except Exception as e:
        check("unknown tool -> protocol error", "unknown tool" in str(e), e)
    r = wrap(await s.call_tool("writeText", {"path": "data/out.txt", "content": "abc"}))
    check("writeText", not r.isError and (PROJ / "data/out.txt").read_text() == "abc", r)
    (PROJ / "data/out.txt").unlink(missing_ok=True)
    r = wrap(await s.call_tool("listFiles", {"dir": "data"}))
    check("listFiles", not r.isError and any(e["path"] == "notes.txt" for e in r.structuredContent["result"]) if r.structuredContent and "result" in r.structuredContent else not r.isError, r)

    # progress + logging
    progress, logs = [], []
    async def on_progress(p, total, msg): progress.append((p, total, msg))
    r = wrap(await s.call_tool("slow", {"steps": 3}, progress_callback=on_progress))
    check("slow result", not r.isError and r.content[0].text == "done", r)
    check("progress notifications", len(progress) == 3 and progress[-1][0] == 3, progress)

    # resources
    res = wrap((await s.list_resources()).resources)
    check("resources listed", [str(r.uri).rstrip("/") for r in res] == ["project://readme"], res)
    tpl = wrap(await s.list_resource_templates()).resourceTemplates
    check("templates listed", [t.uriTemplate for t in tpl] == ["project://file/{name}"], tpl)
    rd = wrap(await s.read_resource("project://readme"))
    check("read resource", rd.contents[0].text.strip() == "hello from notes", rd)
    rd = wrap(await s.read_resource("project://file/notes.txt"))
    check("read template resource", rd.contents[0].text.strip() == "hello from notes", rd)
    try:
        await s.read_resource("project://missing")
        check("missing resource errors", False)
    except Exception as e:
        check("missing resource errors", "not found" in str(e), e)

    # prompts
    ps = wrap((await s.list_prompts()).prompts)
    check("prompts listed", ps and ps[0].name == "summarizePrompt" and [a.name for a in ps[0].arguments] == ["path", "style"] and ps[0].arguments[0].required and not ps[0].arguments[1].required, ps)
    g = wrap(await s.get_prompt("summarizePrompt", {"path": "a.txt"}))
    check("get prompt", "Summarize a.txt in a neutral style." in g.messages[0].content.text, g)
    try:
        await s.get_prompt("summarizePrompt", {})
        check("missing prompt arg errors", False)
    except Exception as e:
        check("missing prompt arg errors", "required" in str(e), e)
    await s.send_ping()
    check("ping", True)
    await s.set_logging_level("debug")
    check("setLevel", True)

async def run_stdio():
    params = StdioServerParameters(command=CLS, args=["serve", "-p", str(PROJ)])
    async with stdio_client(params) as (r, w):
        async with ClientSession(r, w) as s:
            await exercise(s, "stdio")

def free_port():
    with socket.socket() as so:
        so.bind(("127.0.0.1", 0)); return so.getsockname()[1]

async def run_http():
    port = free_port()
    p = subprocess.Popen([CLS, "serve", "-p", str(PROJ), "--http", "--port", str(port)], stderr=subprocess.PIPE)
    try:
        for _ in range(50):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.2).close(); break
            except OSError:
                time.sleep(0.1)
        async with streamable_http_client(f"http://127.0.0.1:{port}/mcp") as streams:
            r, w = streams[0], streams[1]
            async with ClientSession(r, w) as s:
                await exercise(s, "streamable-http")
        # raw HTTP checks
        import http.client
        c = http.client.HTTPConnection("127.0.0.1", port)
        c.request("POST", "/mcp", body=json.dumps({"jsonrpc":"2.0","id":1,"method":"ping"}), headers={"Content-Type":"application/json"})
        resp = c.getresponse(); resp.read()
        check("http: session required", resp.status == 400, resp.status)
        c.request("POST", "/mcp", body="{}", headers={"Content-Type":"application/json","Origin":"http://evil.example"})
        resp = c.getresponse(); resp.read()
        check("http: bad Origin rejected", resp.status == 403, resp.status)
        c.request("GET", "/mcp")
        resp = c.getresponse(); resp.read()
        check("http: GET -> 405", resp.status == 405, resp.status)
        c.request("POST", "/mcp", body="{bad", headers={"Content-Type":"application/json"})
        resp = c.getresponse(); body = resp.read()
        check("http: parse error", resp.status == 400 and json.loads(body)["error"]["code"] == -32700, body)
        c.request("POST", "/mcp", body=json.dumps({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}), headers={"Content-Type":"application/json","Accept":"application/json, text/event-stream"})
        resp = c.getresponse(); body = json.loads(resp.read()); sid = resp.getheader("Mcp-Session-Id")
        check("http: initialize + session id", resp.status == 200 and sid and body["result"]["protocolVersion"] == "2025-11-25", body)
        c.request("POST", "/mcp", body=json.dumps({"jsonrpc":"2.0","method":"notifications/initialized"}), headers={"Content-Type":"application/json","Mcp-Session-Id":sid})
        resp = c.getresponse(); resp.read()
        check("http: notification -> 202", resp.status == 202, resp.status)
        c.request("POST", "/mcp", body=json.dumps({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"slow","arguments":{"steps":2},"_meta":{"progressToken":"p1"}}}), headers={"Content-Type":"application/json","Mcp-Session-Id":sid})
        resp = c.getresponse(); body = resp.read().decode()
        check("http: SSE when notifications emitted", resp.getheader("Content-Type") == "text/event-stream" and body.count("notifications/progress") == 2 and '"id":2' in body.replace(" ", ""), body)
        c.request("DELETE", "/mcp", headers={"Mcp-Session-Id": sid})
        resp = c.getresponse(); resp.read()
        check("http: DELETE session", resp.status == 204, resp.status)
        c.request("POST", "/mcp", body=json.dumps({"jsonrpc":"2.0","id":3,"method":"ping"}), headers={"Content-Type":"application/json","Mcp-Session-Id":sid})
        resp = c.getresponse(); resp.read()
        check("http: deleted session -> 404", resp.status == 404, resp.status)
        big = json.dumps({"jsonrpc":"2.0","id":9,"method":"ping","params":{"pad":"x"*(1024*1024+10)}})
        c2 = http.client.HTTPConnection("127.0.0.1", port)
        c2.request("POST", "/mcp", body=big, headers={"Content-Type":"application/json"})
        resp = c2.getresponse(); resp.read()
        check("http: oversize body -> 413", resp.status == 413, resp.status)
    finally:
        p.terminate(); p.wait()

async def main():
    await run_stdio()
    await run_http()
    print("\nFAILED: %s" % failures if failures else "\nall integration checks passed")
    sys.exit(1 if failures else 0)

asyncio.run(main())
