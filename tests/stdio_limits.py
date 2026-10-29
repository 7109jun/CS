"""Raw stdio tests: cancellation, concurrency limit, oversize messages, malformed input."""
import json, subprocess, os, sys, time, tempfile, shutil
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent
CLS = os.environ.get("CLS", str(ROOT / "target/debug/cls"))
d = Path(tempfile.mkdtemp())
(d / "src").mkdir()
(d / "Claudescript.toml").write_text('[project]\nname="p"\nentry="src/main.cls"\n[limits]\nmax_execution_seconds=20\nmax_concurrent_requests=2\nmax_input_bytes=2000\n')
(d / "src/main.cls").write_text('@tool("spin") function spin(): number { let n = 0; while (true) { n += 1; } }\n@tool("ping") function ping(): string { return "pong"; }\n')
fails = []
def expect(n, c, det=""):
    print(("  ok   " if c else "  FAIL ") + n + ("" if c else f" -> {det}"))
    if not c: fails.append(n)
p = subprocess.Popen([CLS, "serve", "-p", str(d)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
def send(o): p.stdin.write((o if isinstance(o, str) else json.dumps(o)) + "\n"); p.stdin.flush()
def recv(): return json.loads(p.stdout.readline())
send({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}})
r = recv(); expect("older protocol negotiated", r["result"]["protocolVersion"] == "2024-11-05", r)
send({"jsonrpc":"2.0","method":"notifications/initialized"})
send("not json"); r = recv(); expect("parse error reply", r["error"]["code"] == -32700, r)
send({"jsonrpc":"2.0","id":7,"method":"nope"}); r = recv(); expect("method not found", r["error"]["code"] == -32601 and r["id"] == 7, r)
send({"jsonrpc":"2.0","id":8,"method":"ping","params":{"pad":"x"*5000}}); r = recv(); expect("oversize message rejected", "max_input_bytes" in r["error"]["message"], r)
send({"jsonrpc":"2.0","id":9,"method":"ping"}); r = recv(); expect("server survives oversize", r["id"] == 9 and r["result"] == {}, r)
# cancellation
send({"jsonrpc":"2.0","id":20,"method":"tools/call","params":{"name":"spin","arguments":{}}})
time.sleep(0.3)
send({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":20,"reason":"test"}})
send({"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"ping","arguments":{}}})
t0 = time.time(); r = recv()
expect("cancelled request gets no response; next request served", r["id"] == 21 and r["result"]["content"][0]["text"] == "pong" and time.time() - t0 < 5, r)
# concurrency limit: two spinners fill the 2 slots, third is rejected
send({"jsonrpc":"2.0","id":30,"method":"tools/call","params":{"name":"spin","arguments":{}}})
send({"jsonrpc":"2.0","id":31,"method":"tools/call","params":{"name":"spin","arguments":{}}})
time.sleep(0.2)
send({"jsonrpc":"2.0","id":32,"method":"tools/call","params":{"name":"ping","arguments":{}}})
r = recv(); expect("concurrency limit enforced", r["id"] == 32 and r["error"]["code"] == -32000, r)
send({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":30}})
send({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":31}})
send({"jsonrpc":"2.0","id":33,"method":"ping"}); r = recv(); expect("recovers after cancel", r["id"] == 33, r)
p.stdin.close(); p.wait(timeout=5); expect("clean exit on EOF", p.returncode == 0, p.returncode)
shutil.rmtree(d)
print("\nFAILED: %s" % fails if fails else "\nall stdio limit checks passed"); sys.exit(1 if fails else 0)
