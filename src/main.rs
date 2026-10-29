#![allow(dead_code)]
mod ast;
mod bytecode;
mod check_expr;
mod check_stmt;
mod checker;
mod compiler;
mod diag;
mod lexer;
mod mcp;
mod natives;
mod parser;
mod perms;
mod project;
mod schema;
mod types;
mod value;
mod vm;

use bytecode::Program;
use project::{Config, Project};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::exit;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const BUNDLE_MAGIC: &[u8] = b"CLSB1\n";
const PACK_MAGIC: &[u8; 8] = b"CLSPACK1";

#[derive(Serialize, Deserialize)]
struct Bundle {
    config: Config,
    program: Program,
}

fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("error: {}", msg.as_ref());
    exit(1)
}

struct Opts {
    positional: Vec<String>,
    project: Option<String>,
    out: Option<String>,
    http: bool,
    stdio: bool,
    host: Option<String>,
    port: Option<u16>,
    root: Option<String>,
    json: bool,
}

fn parse_opts(args: &[String]) -> Opts {
    let mut o = Opts { positional: vec![], project: None, out: None, http: false, stdio: false, host: None, port: None, root: None, json: false };
    let mut i = 0;
    let val = |i: &mut usize| -> String {
        *i += 1;
        args.get(*i).cloned().unwrap_or_else(|| die(format!("option '{}' needs a value", args[*i - 1])))
    };
    while i < args.len() {
        match args[i].as_str() {
            "-p" | "--project" => o.project = Some(val(&mut i)),
            "-o" | "--out" => o.out = Some(val(&mut i)),
            "--http" => o.http = true,
            "--stdio" => o.stdio = true,
            "--host" => o.host = Some(val(&mut i)),
            "--port" => o.port = Some(val(&mut i).parse().unwrap_or_else(|_| die("invalid --port"))),
            "--root" => o.root = Some(val(&mut i)),
            "--json" => o.json = true,
            a if a.starts_with('-') && a.len() > 1 => die(format!("unknown option '{}'", a)),
            a => o.positional.push(a.to_string()),
        }
        i += 1;
    }
    o
}

fn load(o: &Opts) -> Project {
    let dir = PathBuf::from(o.project.clone().unwrap_or_else(|| ".".into()));
    project::load_project(&dir).unwrap_or_else(|e| die(e))
}

/// Type-check + compile. Prints diagnostics. Returns the program or exits with status 1.
fn build(project: &Project, tests: bool, quiet: bool) -> Program {
    let entry = project.root.join(&project.config.project.entry);
    if !entry.is_file() {
        die(format!("entry file '{}' does not exist", project.config.project.entry));
    }
    let mut roots = vec![entry];
    if tests {
        for f in project::discover_cls_files(&project.root) {
            if !roots.contains(&f) {
                roots.push(f);
            }
        }
    }
    let loaded = match project::load_program(project, &roots) {
        Ok(l) => l,
        Err((diags, sources)) => {
            for d in &diags {
                eprintln!("{}", diag::render(d, &sources));
            }
            eprintln!("aborting: {} error(s)", diags.len());
            exit(1)
        }
    };
    let (diags, _) = checker::check_program(&loaded, &project.config.compiler);
    let errs = diags.iter().filter(|d| d.severity == diag::Severity::Error).count();
    let warns = diags.len() - errs;
    for d in &diags {
        if !quiet || d.severity == diag::Severity::Error {
            eprintln!("{}", diag::render(d, &loaded.sources));
        }
    }
    if errs > 0 || (project.config.compiler.warnings_as_errors && warns > 0) {
        eprintln!("aborting: {} error(s), {} warning(s){}", errs, warns, if errs == 0 { " (warnings_as_errors)" } else { "" });
        exit(1);
    }
    match compiler::compile_program(&loaded, tests) {
        Ok(p) => p,
        Err(diags) => {
            for d in &diags {
                eprintln!("{}", diag::render(d, &loaded.sources));
            }
            exit(1)
        }
    }
}

fn make_vm(prog: &Program, cfg: &Config, root: &Path) -> vm::Vm {
    let mut ctx = vm::RunCtx::new(perms::Perms::new(root.to_path_buf(), cfg.permissions.clone()));
    ctx.max_call_depth = cfg.limits.max_call_depth;
    let mut v = vm::Vm::new(prog, ctx).unwrap_or_else(|e| die(e));
    v.ctx.deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs_f64(cfg.limits.max_execution_seconds));
    if let Err(e) = v.run_inits() {
        match e {
            value::Exc::Throw(e) => die(format!("module initialization failed: {}", vm::format_error(&e))),
            value::Exc::Fatal(m) => die(format!("module initialization failed: {}", m)),
        }
    }
    v.ctx.deadline = None;
    v
}

fn serve(prog: Program, cfg: Config, root: PathBuf, o: &Opts) {
    let vm = make_vm(&prog, &cfg, &root);
    let mut cfg = cfg;
    if o.http {
        cfg.mcp.transport = "http".into();
    }
    if o.stdio {
        cfg.mcp.transport = "stdio".into();
    }
    if let Some(h) = &o.host {
        cfg.mcp.host = h.clone();
    }
    if let Some(p) = o.port {
        cfg.mcp.port = p;
    }
    let transport = cfg.mcp.transport.clone();
    let (host, port) = (cfg.mcp.host.clone(), cfg.mcp.port);
    let server = mcp::Server::new(prog, cfg, vm);
    let r = if transport == "http" { mcp::serve_http(server, &host, port, None) } else { mcp::serve_stdio(server) };
    if let Err(e) = r {
        die(e);
    }
}

fn encode_bundle(prog: Program, cfg: Config) -> Vec<u8> {
    let mut out = BUNDLE_MAGIC.to_vec();
    out.extend(serde_json::to_vec(&Bundle { config: cfg, program: prog }).unwrap());
    out
}

fn decode_bundle(bytes: &[u8]) -> Result<Bundle, String> {
    let body = bytes.strip_prefix(BUNDLE_MAGIC).ok_or("not a Claudescript bundle (bad magic)")?;
    serde_json::from_slice(body).map_err(|e| format!("corrupt bundle: {}", e))
}

fn embedded_bundle() -> Option<Vec<u8>> {
    let exe = std::env::current_exe().ok()?;
    let data = std::fs::read(exe).ok()?;
    if data.len() < 16 || &data[data.len() - 8..] != PACK_MAGIC {
        return None;
    }
    let len = u64::from_le_bytes(data[data.len() - 16..data.len() - 8].try_into().ok()?) as usize;
    let start = data.len().checked_sub(16 + len)?;
    Some(data[start..start + len].to_vec())
}

const NEW_TOML: &str = r#"[project]
name = "{name}"
version = "0.1.0"
entry = "src/main.cls"

[compiler]
strict_types = true
warnings_as_errors = false

[mcp]
protocol = "2025-11-25"
transport = "stdio"

[permissions]
filesystem = "project"
network = "deny"
process = "deny"
environment = "allowlisted"

[limits]
max_execution_seconds = 30
max_concurrent_requests = 16
max_input_bytes = 1048576
max_output_bytes = 4194304
"#;

const NEW_MAIN: &str = r#"interface Greeting {
  message: string;
  length: number;
}

@tool({ description: "Greet someone by name", readOnly: true })
function greet(name: string, excited?: boolean): Greeting {
  const base = `Hello, ${name}`;
  const message = excited === true ? base + "!" : base + ".";
  return { message, length: message.length };
}

@tool({ description: "Add two numbers" })
function add(a: number, b: number): number {
  return a + b;
}

@resource("config://about", { description: "About this server", mimeType: "text/plain" })
function about(): string {
  return "Built with Claudescript";
}

@prompt({ description: "Ask for a code review", params: { code: "Code to review" } })
function review(code: string): string {
  return `Please review this code:\n\n${code}`;
}

test "greet works" {
  const g = greet("Ada", true);
  assertEq(g.message, "Hello, Ada!");
  assertEq(add(2, 3), 5);
}
"#;

fn cmd_new(o: &Opts) {
    let Some(name) = o.positional.first() else { die("usage: cls new <name>") };
    let dir = PathBuf::from(name);
    if dir.exists() {
        die(format!("'{}' already exists", name));
    }
    let proj_name = dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| name.clone());
    std::fs::create_dir_all(dir.join("src")).unwrap_or_else(|e| die(e.to_string()));
    std::fs::write(dir.join("Claudescript.toml"), NEW_TOML.replace("{name}", &proj_name)).unwrap();
    std::fs::write(dir.join("src/main.cls"), NEW_MAIN).unwrap();
    std::fs::write(dir.join(".gitignore"), "dist/\n").unwrap();
    println!("created Claudescript project '{}'\n  cd {} && cls check && cls test && cls inspect", proj_name, name);
}

fn cmd_test(o: &Opts) {
    let project = load(o);
    let prog = build(&project, true, true);
    let filter = o.positional.first().cloned();
    let (mut pass, mut fail) = (0, 0);
    let tests: Vec<_> = prog.tests.iter().filter(|t| filter.as_ref().map_or(true, |f| t.name.contains(f.as_str()) || t.module.contains(f.as_str()))).cloned().collect();
    if tests.is_empty() {
        println!("no tests found");
        return;
    }
    for t in &tests {
        let mut vm = make_vm(&prog, &project.config, &project.root);
        vm.ctx.deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs_f64(project.config.limits.max_execution_seconds));
        let start = std::time::Instant::now();
        let r = vm.call_func_index(t.func).and_then(|v| vm.await_value(v));
        let ms = start.elapsed().as_millis();
        match r {
            Ok(_) => {
                pass += 1;
                println!("  ok    {} > {} ({}ms)", t.module, t.name, ms);
            }
            Err(value::Exc::Throw(e)) => {
                fail += 1;
                println!("  FAIL  {} > {}\n        {}", t.module, t.name, vm::format_error(&e).replace('\n', "\n        "));
            }
            Err(value::Exc::Fatal(m)) => {
                fail += 1;
                println!("  FAIL  {} > {}\n        {}", t.module, t.name, m);
            }
        }
    }
    println!("\n{} passed, {} failed", pass, fail);
    if fail > 0 {
        exit(1);
    }
}

fn inspect_json(prog: &Program) -> serde_json::Value {
    serde_json::json!({
        "tools": prog.tools.iter().map(|t| serde_json::json!({"name": t.name, "description": t.description, "inputSchema": t.input_schema, "outputSchema": t.output_schema, "annotations": t.annotations})).collect::<Vec<_>>(),
        "resources": prog.resources.iter().map(|r| serde_json::json!({"uri": r.uri, "template": r.is_template, "name": r.name, "mimeType": r.mime_type})).collect::<Vec<_>>(),
        "prompts": prog.prompts.iter().map(|p| serde_json::json!({"name": p.name, "arguments": p.arguments.iter().map(|a| serde_json::json!({"name": a.name, "required": a.required})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
    })
}

fn load_any(o: &Opts, what_arg: Option<&String>) -> (Program, Config, PathBuf) {
    if let Some(p) = what_arg {
        if p.ends_with(".clsb") {
            let bytes = std::fs::read(p).unwrap_or_else(|e| die(format!("cannot read {}: {}", p, e)));
            let b = decode_bundle(&bytes).unwrap_or_else(|e| die(e));
            let root = PathBuf::from(o.root.clone().unwrap_or_else(|| ".".into())).canonicalize().unwrap_or_else(|e| die(e.to_string()));
            return (b.program, b.config, root);
        }
    }
    let project = load(o);
    let prog = build(&project, false, true);
    (prog, project.config.clone(), project.root.clone())
}

fn cmd_rpc(o: &Opts, msg: serde_json::Value) {
    let (prog, cfg, root) = load_any(o, None);
    let vm = make_vm(&prog, &cfg, &root);
    let mut server = mcp::Server::new(prog, cfg, vm);
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (notes, resp) = server.handle(&msg, flag);
    for n in notes {
        eprintln!("{}", n);
    }
    if let Some(r) = resp {
        println!("{}", serde_json::to_string_pretty(&r).unwrap());
    }
}

fn usage() {
    println!(
        "Claudescript {} — a language for building MCP servers\n\n\
USAGE: cls <command> [options]\n\n\
COMMANDS:\n  new <name>             create a new project\n  check                  type-check the project\n  build [-o file.clsb]   compile to a portable bytecode bundle\n  run | serve [target]   start the MCP server (project dir or .clsb); --stdio | --http --host H --port N\n  test [filter]          run `test \"name\" {{ }}` blocks\n  pack [-o file]         build a standalone executable (runtime + bytecode)\n  inspect [target]       list tools, resources and prompts (--json for schemas)\n  call <tool> [json]     invoke a tool locally and print the MCP result\n  rpc <json>             send one raw JSON-RPC request to the server core\n  version\n\n\
OPTIONS: -p/--project <dir>  --root <dir> (for .clsb)  --json",
        VERSION
    );
}

fn main() {
    if let Some(bytes) = embedded_bundle() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(|s| s == "--version").unwrap_or(false) {
            println!("{} (Claudescript runtime {})", std::env::args().next().unwrap_or_default(), VERSION);
            return;
        }
        let o = parse_opts(&args);
        let b = decode_bundle(&bytes).unwrap_or_else(|e| die(e));
        let root = PathBuf::from(o.root.clone().unwrap_or_else(|| ".".into())).canonicalize().unwrap_or_else(|e| die(e.to_string()));
        serve(b.program, b.config, root, &o);
        return;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        usage();
        exit(2)
    };
    let o = parse_opts(&args[1..]);
    match cmd.as_str() {
        "version" | "--version" | "-V" => println!("cls {} (protocols: {})", VERSION, project::SUPPORTED_PROTOCOLS.join(", ")),
        "help" | "--help" | "-h" => usage(),
        "new" => cmd_new(&o),
        "check" => {
            let project = load(&o);
            let entry = project.root.join(&project.config.project.entry);
            if !entry.is_file() {
                die(format!("entry file '{}' does not exist", project.config.project.entry));
            }
            let roots: Vec<PathBuf> = project::discover_cls_files(&project.root);
            let loaded = match project::load_program(&project, &roots) {
                Ok(l) => l,
                Err((d, s)) => {
                    for x in &d {
                        eprintln!("{}", diag::render(x, &s));
                    }
                    exit(1)
                }
            };
            let (diags, _) = checker::check_program(&loaded, &project.config.compiler);
            for d in &diags {
                eprintln!("{}", diag::render(d, &loaded.sources));
            }
            let errs = diags.iter().filter(|d| d.severity == diag::Severity::Error).count();
            let warns = diags.len() - errs;
            if let Err(d) = compiler::compile_program(&loaded, true) {
                for x in &d {
                    eprintln!("{}", diag::render(x, &loaded.sources));
                }
                exit(1)
            }
            println!("{} error(s), {} warning(s)", errs, warns);
            if errs > 0 || (project.config.compiler.warnings_as_errors && warns > 0) {
                exit(1);
            }
        }
        "build" => {
            let project = load(&o);
            let prog = build(&project, false, false);
            let out = PathBuf::from(o.out.clone().unwrap_or_else(|| format!("dist/{}.clsb", project.config.project.name)));
            let out = if out.is_relative() { project.root.join(out) } else { out };
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p).ok();
            }
            let bytes = encode_bundle(prog, project.config.clone());
            std::fs::write(&out, &bytes).unwrap_or_else(|e| die(format!("cannot write {}: {}", out.display(), e)));
            println!("built {} ({} bytes)", out.display(), bytes.len());
        }
        "run" | "serve" => {
            let (prog, cfg, root) = load_any(&o, o.positional.first());
            serve(prog, cfg, root, &o);
        }
        "test" => cmd_test(&o),
        "pack" => {
            let project = load(&o);
            let prog = build(&project, false, false);
            let name = &project.config.project.name;
            let out = PathBuf::from(o.out.clone().unwrap_or_else(|| format!("dist/{}{}", name, std::env::consts::EXE_SUFFIX)));
            let out = if out.is_relative() { project.root.join(out) } else { out };
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p).ok();
            }
            let exe = std::env::current_exe().unwrap_or_else(|e| die(e.to_string()));
            let mut data = std::fs::read(exe).unwrap_or_else(|e| die(e.to_string()));
            if data.len() >= 8 && &data[data.len() - 8..] == PACK_MAGIC {
                die("this executable is already a packed server");
            }
            let bundle = encode_bundle(prog, project.config.clone());
            data.extend(&bundle);
            data.extend((bundle.len() as u64).to_le_bytes());
            data.extend(PACK_MAGIC);
            std::fs::write(&out, &data).unwrap_or_else(|e| die(format!("cannot write {}: {}", out.display(), e)));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755));
            }
            println!("packed {} ({} bytes)", out.display(), data.len());
        }
        "inspect" => {
            let (prog, cfg, _) = load_any(&o, o.positional.first());
            let j = inspect_json(&prog);
            if o.json {
                println!("{}", serde_json::to_string_pretty(&j).unwrap());
            } else {
                println!("{} v{} (MCP {}, transport {})", cfg.project.name, cfg.project.version, cfg.mcp.protocol, cfg.mcp.transport);
                for t in &prog.tools {
                    println!("  tool      {}({}) — {}", t.name, t.params.join(", "), t.description);
                }
                for r in &prog.resources {
                    println!("  resource  {}{} — {}", r.uri, if r.is_template { " (template)" } else { "" }, r.name);
                }
                for p in &prog.prompts {
                    println!("  prompt    {}({})", p.name, p.arguments.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join(", "));
                }
            }
        }
        "call" => {
            let Some(tool) = o.positional.first() else { die("usage: cls call <tool> [json-args]") };
            let args: serde_json::Value = match o.positional.get(1) {
                Some(s) => serde_json::from_str(s).unwrap_or_else(|e| die(format!("invalid JSON arguments: {}", e))),
                None => serde_json::json!({}),
            };
            cmd_rpc(&o, serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}));
        }
        "rpc" => {
            let Some(s) = o.positional.first() else { die("usage: cls rpc '<json>'") };
            let msg: serde_json::Value = serde_json::from_str(s).unwrap_or_else(|e| die(format!("invalid JSON: {}", e)));
            cmd_rpc(&o, msg);
        }
        other => {
            eprintln!("unknown command '{}'\n", other);
            usage();
            exit(2)
        }
    }
}
