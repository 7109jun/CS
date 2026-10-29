import json, subprocess, sys, os, tempfile, shutil
from pathlib import Path
CLS = os.environ.get("CLS", str(Path(__file__).resolve().parents[3] / "target/debug/cls"))
SRC = Path(__file__).parent / "src"
fails = []
def project(perms="", limits=""):
    d = Path(tempfile.mkdtemp())
    shutil.copytree(SRC, d / "src")
    (d / "data.txt").write_text("secret")
    (d / "Claudescript.toml").write_text(f'[project]\nname="p"\nentry="src/main.cls"\n[permissions]\n{perms}\n[limits]\n{limits}\n')
    return d
def call(d, tool, args):
    r = subprocess.run([CLS, "call", "-p", str(d), tool, json.dumps(args)], capture_output=True, text=True)
    try: return json.loads(r.stdout)["result"]
    except Exception: return {"isError": True, "content": [{"text": r.stdout + r.stderr}]}
def err(r): return bool(r.get("isError"))
def txt(r): return r["content"][0]["text"]
def expect(name, cond, detail=""):
    print(("  ok   " if cond else "  FAIL ") + name + ("" if cond else f" -> {detail}"))
    if not cond: fails.append(name)

d = project('filesystem="deny"');            r = call(d, "rd", {"path": "data.txt"}); expect("fs deny", err(r) and "denied" in txt(r), r)
d = project('filesystem="project"');         r = call(d, "rd", {"path": "data.txt"}); expect("fs project read", not err(r) and txt(r) == "secret", r)
r = call(d, "rd", {"path": "../x"});         expect("fs project blocks ..", err(r) and "outside" in txt(r), r)
os.symlink("/etc", d / "link"); r = call(d, "rd", {"path": "link/hostname"}); expect("fs project blocks symlink escape", err(r) and "outside" in txt(r), r)
r = call(d, "wr", {"path": "o.txt"});        expect("fs project write", not err(r) and (d / "o.txt").read_text() == "x", r)
d = project('filesystem="project-readonly"'); r = call(d, "wr", {"path": "o.txt"}); expect("fs readonly blocks write", err(r) and "read-only" in txt(r), r)
r = call(d, "rd", {"path": "data.txt"});     expect("fs readonly allows read", not err(r), r)
d = project('filesystem="allow"');           r = call(d, "rd", {"path": "/etc/hostname"}); expect("fs allow absolute", not err(r), r)
d = project('network="deny"');               r = call(d, "get", {"url": "http://127.0.0.1:1/"}); expect("net deny", err(r) and "denied" in txt(r), r)
d = project('network="allowlisted"\nnetwork_allow=["example.com"]'); r = call(d, "get", {"url": "http://127.0.0.1:1/"}); expect("net allowlist blocks other host", err(r) and "network_allow" in txt(r), r)
d = project('network="allow"');              r = call(d, "get", {"url": "http://127.0.0.1:1/"}); expect("net allow attempts connection", err(r) and "denied" not in txt(r), r)
d = project('process="deny"');               r = call(d, "run", {"cmd": "echo"}); expect("process deny", err(r) and "denied" in txt(r), r)
d = project('process="allowlisted"\nprocess_allow=["echo"]'); r = call(d, "run", {"cmd": "echo"}); expect("process allowlisted ok", not err(r) and txt(r) == "hi", r)
r = call(d, "run", {"cmd": "ls"});          expect("process allowlisted blocks", err(r) and "process_allow" in txt(r), r)
d = project('environment="deny"');           r = call(d, "getenv", {"name": "HOME"}); expect("env deny", err(r), r)
d = project('environment="allowlisted"\nenv_allow=["HOME"]'); r = call(d, "getenv", {"name": "HOME"}); expect("env allowlisted ok", not err(r), r)
r = call(d, "getenv", {"name": "PATH"});    expect("env allowlisted blocks", err(r), r)
d = project('', 'max_output_bytes=1000');    r = call(d, "big", {"n": 5000}); expect("output limit", err(r) and "max_output_bytes" in txt(r), r)
d = project('', 'max_call_depth=50');        r = call(d, "deep", {"n": 1000}); expect("call depth limit", err(r) and "depth" in txt(r), r)
r = call(d, "deep", {"n": 10});             expect("shallow recursion ok", not err(r) and txt(r) == "10", r)
print("\nFAILED: %s" % fails if fails else "\nall permission/limit checks passed"); sys.exit(1 if fails else 0)
