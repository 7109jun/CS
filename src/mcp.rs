//! MCP server core (JSON-RPC 2.0) plus stdio and Streamable HTTP transports.
use crate::bytecode::*;
use crate::project::{Config, SUPPORTED_PROTOCOLS};
use crate::schema;
use crate::value::*;
use crate::vm::{error_message, Vm};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const E_PARSE: i64 = -32700;
pub const E_INVALID_REQUEST: i64 = -32600;
pub const E_METHOD: i64 = -32601;
pub const E_PARAMS: i64 = -32602;
pub const E_INTERNAL: i64 = -32603;
pub const E_BUSY: i64 = -32000;
pub const E_NOT_FOUND: i64 = -32002;

fn err_resp(id: Json, code: i64, msg: impl Into<String>) -> Json {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg.into()}})
}

pub struct Server {
    pub prog: Program,
    pub cfg: Config,
    pub vm: Vm,
}

pub type Reply = (Vec<Json>, Option<Json>);

impl Server {
    pub fn new(prog: Program, cfg: Config, vm: Vm) -> Server {
        Server { prog, cfg, vm }
    }

    /// Process one JSON-RPC message. Returns (notifications to emit first, response if it was a request).
    pub fn handle(&mut self, msg: &Json, cancel: Arc<AtomicBool>) -> Reply {
        let Some(obj) = msg.as_object() else {
            return (vec![], Some(err_resp(Json::Null, E_INVALID_REQUEST, "request must be a JSON object")));
        };
        let id = obj.get("id").cloned();
        let method = obj.get("method").and_then(|m| m.as_str());
        let Some(method) = method else {
            // a response from the client (we never send requests) or garbage
            return if id.is_some() && (obj.contains_key("result") || obj.contains_key("error")) { (vec![], None) } else { (vec![], Some(err_resp(id.unwrap_or(Json::Null), E_INVALID_REQUEST, "missing 'method'"))) };
        };
        if obj.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
            return (vec![], id.map(|i| err_resp(i, E_INVALID_REQUEST, "jsonrpc must be \"2.0\"")));
        }
        let params = obj.get("params").cloned().unwrap_or(json!({}));
        let Some(id) = id else {
            // notification: nothing to answer
            return (vec![], None);
        };
        if !(id.is_string() || id.is_number()) {
            return (vec![], Some(err_resp(Json::Null, E_INVALID_REQUEST, "id must be a string or number")));
        }
        let mut notes = vec![];
        let result = self.dispatch(method, &params, cancel, &mut notes);
        match result {
            Ok(Some(r)) => (notes, Some(json!({"jsonrpc": "2.0", "id": id, "result": r}))),
            Ok(None) => (notes, None), // cancelled: no response
            Err((code, m)) => (notes, Some(err_resp(id, code, m))),
        }
    }

    fn dispatch(&mut self, method: &str, p: &Json, cancel: Arc<AtomicBool>, notes: &mut Vec<Json>) -> Result<Option<Json>, (i64, String)> {
        match method {
            "initialize" => Ok(Some(self.initialize(p))),
            "ping" => Ok(Some(json!({}))),
            "tools/list" => Ok(Some(json!({"tools": self.prog.tools.iter().map(tool_json).collect::<Vec<_>>()}))),
            "tools/call" => self.tools_call(p, cancel, notes),
            "resources/list" => Ok(Some(json!({"resources": self.prog.resources.iter().filter(|r| !r.is_template).map(resource_json).collect::<Vec<_>>()}))),
            "resources/templates/list" => Ok(Some(json!({"resourceTemplates": self.prog.resources.iter().filter(|r| r.is_template).map(template_json).collect::<Vec<_>>()}))),
            "resources/read" => self.resources_read(p, cancel, notes),
            "prompts/list" => Ok(Some(json!({"prompts": self.prog.prompts.iter().map(prompt_json).collect::<Vec<_>>()}))),
            "prompts/get" => self.prompts_get(p, cancel, notes),
            "logging/setLevel" => {
                let l = p.get("level").and_then(|l| l.as_str()).ok_or((E_PARAMS, "missing 'level'".to_string()))?;
                let n = ["debug", "info", "notice", "warning", "error", "critical", "alert", "emergency"].iter().position(|x| *x == l).ok_or((E_PARAMS, format!("invalid log level '{}'", l)))?;
                self.vm.ctx.log_level = n as u8;
                Ok(Some(json!({})))
            }
            _ => Err((E_METHOD, format!("method not found: {}", method))),
        }
    }

    fn initialize(&self, p: &Json) -> Json {
        let requested = p.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
        let version = if SUPPORTED_PROTOCOLS.contains(&requested) { requested.to_string() } else { self.cfg.mcp.protocol.clone() };
        let mut caps = json!({"tools": {"listChanged": false}, "logging": {}});
        if !self.prog.resources.is_empty() {
            caps["resources"] = json!({"listChanged": false, "subscribe": false});
        }
        if !self.prog.prompts.is_empty() {
            caps["prompts"] = json!({"listChanged": false});
        }
        let mut r = json!({"protocolVersion": version, "capabilities": caps, "serverInfo": {"name": self.cfg.project.name, "version": self.cfg.project.version}});
        if let Some(d) = &self.cfg.project.description {
            r["serverInfo"]["title"] = json!(d);
        }
        if let Some(i) = &self.cfg.project.instructions {
            r["instructions"] = json!(i);
        }
        r
    }

    /// Run a Claudescript function with limits applied. Ok(None) = cancelled.
    fn run(&mut self, global: u32, args: Vec<Value>, cancel: Arc<AtomicBool>, p: &Json, notes: &mut Vec<Json>) -> Result<Option<Result<Value, String>>, (i64, String)> {
        let lim = &self.cfg.limits;
        self.vm.ctx.deadline = Some(Instant::now() + Duration::from_secs_f64(lim.max_execution_seconds));
        self.vm.ctx.cancel = Some(cancel.clone());
        self.vm.ctx.progress_token = p.get("_meta").and_then(|m| m.get("progressToken")).cloned();
        self.vm.ctx.notifications.clear();
        let r = self.vm.call_global(global, args).and_then(|v| self.vm.await_value(v));
        notes.append(&mut self.vm.ctx.notifications);
        self.vm.ctx.deadline = None;
        self.vm.ctx.cancel = None;
        match r {
            Ok(v) => Ok(Some(Ok(v))),
            Err(Exc::Throw(e)) => Ok(Some(Err(error_message(&e)))),
            Err(Exc::Fatal(m)) => {
                if cancel.load(Ordering::Relaxed) {
                    Ok(None)
                } else {
                    Ok(Some(Err(m)))
                }
            }
        }
    }

    fn tools_call(&mut self, p: &Json, cancel: Arc<AtomicBool>, notes: &mut Vec<Json>) -> Result<Option<Json>, (i64, String)> {
        let name = p.get("name").and_then(|n| n.as_str()).ok_or((E_PARAMS, "missing tool 'name'".to_string()))?;
        let tool = self.prog.tools.iter().find(|t| t.name == name).cloned().ok_or((E_PARAMS, format!("unknown tool: {}", name)))?;
        let args_json = match p.get("arguments") {
            None | Some(Json::Null) => json!({}),
            Some(a) => a.clone(),
        };
        if !args_json.is_object() {
            return Err((E_PARAMS, "'arguments' must be an object".into()));
        }
        if let Err(e) = schema::validate(&tool.input_schema, &args_json) {
            return Ok(Some(tool_error(&format!("Invalid arguments for tool '{}': {}", name, e))));
        }
        let args: Vec<Value> = tool.params.iter().map(|n| args_json.get(n).map(from_json).unwrap_or(Value::Null)).collect();
        let outcome = self.run(tool.global, args, cancel, p, notes)?;
        let Some(outcome) = outcome else { return Ok(None) };
        let v = match outcome {
            Ok(v) => v,
            Err(m) => return Ok(Some(tool_error(&m))),
        };
        let result = match tool_result(&tool, &v) {
            Ok(r) => r,
            Err(m) => tool_error(&m),
        };
        let size = result.to_string().len();
        if size > self.cfg.limits.max_output_bytes {
            return Ok(Some(tool_error(&format!("tool output is {} bytes, exceeding limits.max_output_bytes ({})", size, self.cfg.limits.max_output_bytes))));
        }
        Ok(Some(result))
    }

    fn resources_read(&mut self, p: &Json, cancel: Arc<AtomicBool>, notes: &mut Vec<Json>) -> Result<Option<Json>, (i64, String)> {
        let uri = p.get("uri").and_then(|n| n.as_str()).ok_or((E_PARAMS, "missing 'uri'".to_string()))?.to_string();
        let mut found: Option<(ResourceDef, Vec<Value>)> = None;
        for r in &self.prog.resources {
            if !r.is_template {
                if r.uri == uri {
                    found = Some((r.clone(), vec![]));
                    break;
                }
            } else if let Some(vars) = match_template(&r.uri, &uri) {
                let args = r.params.iter().map(|n| vars.get(n).map(|s| Value::str(s)).unwrap_or(Value::Null)).collect();
                found = Some((r.clone(), args));
                break;
            }
        }
        let (def, args) = found.ok_or((E_NOT_FOUND, format!("resource not found: {}", uri)))?;
        let outcome = self.run(def.global, args, cancel, p, notes)?;
        let Some(outcome) = outcome else { return Ok(None) };
        let v = outcome.map_err(|m| (E_INTERNAL, m))?;
        let mime = def.mime_type.clone();
        let mk = |text: Option<String>, blob: Option<String>, m: Option<String>| {
            let mut o = json!({"uri": uri});
            if let Some(m) = m.or_else(|| mime.clone()) {
                o["mimeType"] = json!(m);
            }
            if let Some(t) = text {
                o["text"] = json!(t);
            }
            if let Some(b) = blob {
                o["blob"] = json!(b);
            }
            o
        };
        let to_content = |v: &Value| -> Result<Json, (i64, String)> {
            match v {
                Value::Str(s) => Ok(mk(Some(s.to_string()), None, None)),
                Value::Object(o) => {
                    let o = o.borrow();
                    let get = |k: &str| o.get(k).and_then(|x| if let Value::Str(s) = x { Some(s.to_string()) } else { None });
                    Ok(mk(get("text"), get("blob"), get("mimeType")))
                }
                _ => Err((E_INTERNAL, "resource handler returned an invalid value".into())),
            }
        };
        let contents = match &v {
            Value::Array(a) => a.borrow().iter().map(&to_content).collect::<Result<Vec<_>, _>>()?,
            other => vec![to_content(other)?],
        };
        let r = json!({"contents": contents});
        if r.to_string().len() > self.cfg.limits.max_output_bytes {
            return Err((E_INTERNAL, "resource exceeds limits.max_output_bytes".into()));
        }
        Ok(Some(r))
    }

    fn prompts_get(&mut self, p: &Json, cancel: Arc<AtomicBool>, notes: &mut Vec<Json>) -> Result<Option<Json>, (i64, String)> {
        let name = p.get("name").and_then(|n| n.as_str()).ok_or((E_PARAMS, "missing prompt 'name'".to_string()))?;
        let def = self.prog.prompts.iter().find(|x| x.name == name).cloned().ok_or((E_PARAMS, format!("unknown prompt: {}", name)))?;
        let given = p.get("arguments").and_then(|a| a.as_object()).cloned().unwrap_or_default();
        let mut args = vec![];
        for a in &def.arguments {
            match given.get(&a.name) {
                Some(Json::String(s)) => args.push(Value::str(s)),
                Some(Json::Null) | None => {
                    if a.required {
                        return Err((E_PARAMS, format!("missing required argument '{}'", a.name)));
                    }
                    args.push(Value::Null)
                }
                Some(other) => args.push(Value::string(other.to_string())),
            }
        }
        let outcome = self.run(def.global, args, cancel, p, notes)?;
        let Some(outcome) = outcome else { return Ok(None) };
        let v = outcome.map_err(|m| (E_INTERNAL, m))?;
        let msg = |role: &str, text: &str| json!({"role": role, "content": {"type": "text", "text": text}});
        let messages: Vec<Json> = match &v {
            Value::Str(s) => vec![msg("user", s)],
            Value::Array(a) => {
                let mut out = vec![];
                for m in a.borrow().iter() {
                    let Value::Object(o) = m else { return Err((E_INTERNAL, "prompt returned an invalid message".into())) };
                    let o = o.borrow();
                    let role = o.get("role").map(to_display).unwrap_or_else(|| "user".into());
                    out.push(msg(&role, &o.get("text").map(to_display).unwrap_or_default()));
                }
                out
            }
            _ => return Err((E_INTERNAL, "prompt handler returned an invalid value".into())),
        };
        let mut r = json!({"messages": messages});
        if let Some(d) = &def.description {
            r["description"] = json!(d);
        }
        Ok(Some(r))
    }
}

fn tool_json(t: &ToolDef) -> Json {
    let mut o = json!({"name": t.name, "description": t.description, "inputSchema": t.input_schema});
    if let Some(title) = &t.title {
        o["title"] = json!(title);
    }
    if let Some(s) = &t.output_schema {
        o["outputSchema"] = s.clone();
    }
    if t.annotations.as_object().map_or(false, |m| !m.is_empty()) {
        o["annotations"] = t.annotations.clone();
    }
    o
}

fn resource_json(r: &ResourceDef) -> Json {
    let mut o = json!({"uri": r.uri, "name": r.name});
    add_opt(&mut o, r);
    o
}

fn template_json(r: &ResourceDef) -> Json {
    let mut o = json!({"uriTemplate": r.uri, "name": r.name});
    add_opt(&mut o, r);
    o
}

fn add_opt(o: &mut Json, r: &ResourceDef) {
    if let Some(t) = &r.title {
        o["title"] = json!(t);
    }
    if let Some(d) = &r.description {
        o["description"] = json!(d);
    }
    if let Some(m) = &r.mime_type {
        o["mimeType"] = json!(m);
    }
}

fn prompt_json(p: &PromptDef) -> Json {
    let mut o = json!({"name": p.name, "arguments": p.arguments.iter().map(|a| {
        let mut x = json!({"name": a.name, "required": a.required});
        if let Some(d) = &a.description { x["description"] = json!(d); }
        x
    }).collect::<Vec<_>>()});
    if let Some(t) = &p.title {
        o["title"] = json!(t);
    }
    if let Some(d) = &p.description {
        o["description"] = json!(d);
    }
    o
}

fn tool_error(msg: &str) -> Json {
    json!({"content": [{"type": "text", "text": msg}], "isError": true})
}

fn tool_result(tool: &ToolDef, v: &Value) -> Result<Json, String> {
    let text = |s: String| json!({"content": [{"type": "text", "text": s}]});
    match tool.result {
        ResultKind::Void => Ok(json!({"content": []})),
        ResultKind::Text => Ok(text(to_display(v))),
        ResultKind::Passthrough => {
            let j = to_json(v)?;
            if !j.get("content").map_or(false, |c| c.is_array()) {
                return Err("tool returned an invalid ToolResult (missing 'content' array)".into());
            }
            if let (Some(sc), Some(os)) = (j.get("structuredContent"), &tool.output_schema) {
                schema::validate(os, sc).map_err(|e| format!("structured output failed validation: {}", e))?;
            }
            Ok(j)
        }
        ResultKind::Structured => {
            let j = to_json(v)?;
            if let Some(os) = &tool.output_schema {
                schema::validate(os, &j).map_err(|e| format!("tool output failed validation against its declared type: {}", e))?;
            }
            let mut r = text(serde_json::to_string(&j).unwrap_or_default());
            r["structuredContent"] = j;
            Ok(r)
        }
        ResultKind::Json => {
            let j = to_json(v)?;
            Ok(text(serde_json::to_string(&j).unwrap_or_default()))
        }
    }
}

fn match_template(tpl: &str, uri: &str) -> Option<HashMap<String, String>> {
    let mut pat = String::from("^");
    let mut names = vec![];
    let mut rest = tpl;
    while let Some(i) = rest.find('{') {
        pat.push_str(&regex::escape(&rest[..i]));
        let j = rest[i..].find('}')? + i;
        let name = &rest[i + 1..j];
        names.push(name.to_string());
        pat.push_str("([^/]+)");
        rest = &rest[j + 1..];
    }
    pat.push_str(&regex::escape(rest));
    pat.push('$');
    let re = regex::Regex::new(&pat).ok()?;
    let c = re.captures(uri)?;
    let mut m = HashMap::new();
    for (i, n) in names.iter().enumerate() {
        m.insert(n.clone(), c.get(i + 1)?.as_str().to_string());
    }
    Some(m)
}

// =============================================================== job queue

pub struct Job {
    pub msg: Json,
    pub cancel: Arc<AtomicBool>,
    pub reply: Box<dyn FnOnce(Reply) + Send>,
    pub pending: Arc<AtomicUsize>,
}

type InFlight = Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>;

fn id_key(session: &str, id: &Json) -> String {
    format!("{}|{}", session, id)
}

fn worker_loop(server: &mut Server, rx: std::sync::mpsc::Receiver<Option<Job>>) {
    while let Ok(Some(job)) = rx.recv() {
        let reply = server.handle(&job.msg, job.cancel.clone());
        job.pending.fetch_sub(1, Ordering::SeqCst);
        (job.reply)(reply);
    }
}

fn has_id(m: &Json) -> bool {
    m.get("id").is_some() && m.get("method").is_some()
}

// ================================================================== stdio

pub fn serve_stdio(mut server: Server) -> Result<(), String> {
    server.vm.ctx.console_to_stderr = true;
    let max_in = server.cfg.limits.max_input_bytes;
    let max_conc = server.cfg.limits.max_concurrent_requests;
    let out: Arc<Mutex<std::io::Stdout>> = Arc::new(Mutex::new(std::io::stdout()));
    let (tx, rx) = channel::<Option<Job>>();
    let inflight: InFlight = Arc::new(Mutex::new(HashMap::new()));
    let pending = Arc::new(AtomicUsize::new(0));

    let out2 = out.clone();
    let inflight2 = inflight.clone();
    let pending2 = pending.clone();
    let tx2 = tx.clone();
    let reader = std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut rd = BufReader::new(stdin.lock());
        loop {
            let mut line = Vec::new();
            // bounded line read
            match read_line_limited(&mut rd, &mut line, max_in) {
                Ok(LineRead::Eof) => break,
                Ok(LineRead::TooLong) => {
                    write_msg(&out2, &err_resp(Json::Null, E_INVALID_REQUEST, format!("message exceeds limits.max_input_bytes ({})", max_in)));
                    continue;
                }
                Ok(LineRead::Line) => {}
                Err(_) => break,
            }
            let text = String::from_utf8_lossy(&line);
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let parsed: Json = match serde_json::from_str(text) {
                Ok(j) => j,
                Err(e) => {
                    write_msg(&out2, &err_resp(Json::Null, E_PARSE, format!("parse error: {}", e)));
                    continue;
                }
            };
            let msgs = if let Json::Array(a) = parsed { a } else { vec![parsed] };
            for m in msgs {
                if m.get("method").and_then(|x| x.as_str()) == Some("notifications/cancelled") {
                    if let Some(rid) = m.get("params").and_then(|p| p.get("requestId")) {
                        if let Some(f) = inflight2.lock().unwrap().get(&id_key("", rid)) {
                            f.store(true, Ordering::SeqCst);
                        }
                    }
                    continue;
                }
                if !has_id(&m) {
                    continue;
                }
                let id = m["id"].clone();
                if pending2.load(Ordering::SeqCst) >= max_conc {
                    write_msg(&out2, &err_resp(id, E_BUSY, format!("too many concurrent requests (limit {})", max_conc)));
                    continue;
                }
                pending2.fetch_add(1, Ordering::SeqCst);
                let flag = Arc::new(AtomicBool::new(false));
                let key = id_key("", &id);
                inflight2.lock().unwrap().insert(key.clone(), flag.clone());
                let (o, inf) = (out2.clone(), inflight2.clone());
                let job = Job {
                    msg: m,
                    cancel: flag,
                    pending: pending2.clone(),
                    reply: Box::new(move |(notes, resp)| {
                        inf.lock().unwrap().remove(&key);
                        for n in notes {
                            write_msg(&o, &n);
                        }
                        if let Some(r) = resp {
                            write_msg(&o, &r);
                        }
                    }),
                };
                if tx2.send(Some(job)).is_err() {
                    return;
                }
            }
        }
        let _ = tx2.send(None);
    });
    drop(tx);
    worker_loop(&mut server, rx);
    let _ = reader.join();
    Ok(())
}

enum LineRead {
    Eof,
    TooLong,
    Line,
}

fn read_line_limited<R: BufRead>(rd: &mut R, buf: &mut Vec<u8>, max: usize) -> std::io::Result<LineRead> {
    let mut too_long = false;
    let mut any = false;
    loop {
        let (consumed, done) = {
            let chunk = rd.fill_buf()?;
            if chunk.is_empty() {
                return Ok(if any { if too_long { LineRead::TooLong } else { LineRead::Line } } else { LineRead::Eof });
            }
            any = true;
            match chunk.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    if !too_long {
                        buf.extend_from_slice(&chunk[..i]);
                    }
                    (i + 1, true)
                }
                None => {
                    if !too_long {
                        buf.extend_from_slice(chunk);
                    }
                    (chunk.len(), false)
                }
            }
        };
        rd.consume(consumed);
        if buf.len() > max {
            too_long = true;
            buf.clear();
        }
        if done {
            return Ok(if too_long { LineRead::TooLong } else { LineRead::Line });
        }
    }
}

fn write_msg(out: &Arc<Mutex<std::io::Stdout>>, m: &Json) {
    let mut o = out.lock().unwrap();
    let _ = writeln!(o, "{}", m);
    let _ = o.flush();
}

// ================================================================== HTTP

struct HttpReq {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

enum ReadErr {
    Closed,
    Bad(u16, &'static str),
}

fn read_request(rd: &mut BufReader<TcpStream>, max_body: usize) -> Result<HttpReq, ReadErr> {
    let mut line = String::new();
    match rd.by_ref().take(16 * 1024).read_line(&mut line) {
        Ok(0) | Err(_) => return Err(ReadErr::Closed),
        Ok(_) => {}
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path), Some(_ver)) = (parts.next(), parts.next(), parts.next()) else { return Err(ReadErr::Bad(400, "malformed request line")) };
    let (method, path) = (method.to_string(), path.split('?').next().unwrap_or("").to_string());
    let mut headers = HashMap::new();
    let mut total = 0;
    loop {
        let mut h = String::new();
        match rd.by_ref().take(16 * 1024).read_line(&mut h) {
            Ok(0) | Err(_) => return Err(ReadErr::Closed),
            Ok(n) => total += n,
        }
        if total > 64 * 1024 {
            return Err(ReadErr::Bad(431, "headers too large"));
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    if headers.get("transfer-encoding").map_or(false, |v| v.to_lowercase().contains("chunked")) {
        return Err(ReadErr::Bad(411, "chunked request bodies are not supported; send Content-Length"));
    }
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > max_body {
        if len <= 64 * 1024 * 1024 {
            let _ = std::io::copy(&mut rd.by_ref().take(len as u64), &mut std::io::sink());
        }
        return Err(ReadErr::Bad(413, "request body exceeds limits.max_input_bytes"));
    }
    let mut body = vec![0u8; len];
    rd.read_exact(&mut body).map_err(|_| ReadErr::Closed)?;
    Ok(HttpReq { method, path, headers, body })
}

fn status_text(c: u16) -> &'static str {
    match c {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

fn respond(s: &mut TcpStream, code: u16, headers: &[(&str, String)], body: &[u8]) -> std::io::Result<()> {
    let mut h = format!("HTTP/1.1 {} {}\r\nContent-Length: {}\r\n", code, status_text(code), body.len());
    for (k, v) in headers {
        h.push_str(&format!("{}: {}\r\n", k, v));
    }
    h.push_str("\r\n");
    s.write_all(h.as_bytes())?;
    s.write_all(body)?;
    s.flush()
}

fn origin_allowed(origin: &str, allowed: &[String]) -> bool {
    if allowed.iter().any(|a| a == "*" || a == origin) {
        return true;
    }
    let host = origin.split_once("://").map(|x| x.1).unwrap_or(origin);
    let host = host.split('/').next().unwrap_or("");
    let h = if host.starts_with('[') { host.split(']').next().unwrap_or("").trim_start_matches('[') } else { host.split(':').next().unwrap_or("") };
    matches!(h, "localhost" | "127.0.0.1" | "::1")
}

struct HttpShared {
    tx: Mutex<Sender<Option<Job>>>,
    sessions: Mutex<std::collections::HashSet<String>>,
    inflight: InFlight,
    pending: Arc<AtomicUsize>,
    cfg: Config,
    shutdown: AtomicBool,
}

pub fn serve_http(mut server: Server, host: &str, port: u16, ready: Option<Sender<u16>>) -> Result<(), String> {
    let listener = TcpListener::bind((host, port)).map_err(|e| format!("cannot bind {}:{}: {}", host, port, e))?;
    let actual = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    eprintln!("[claudescript] MCP server '{}' listening on http://{}:{}{}", server.cfg.project.name, host, actual, server.cfg.mcp.path);
    if let Some(r) = ready {
        let _ = r.send(actual);
    }
    server.vm.ctx.console_to_stderr = true;
    let (tx, rx) = channel::<Option<Job>>();
    let shared = Arc::new(HttpShared {
        tx: Mutex::new(tx),
        sessions: Mutex::new(Default::default()),
        inflight: Arc::new(Mutex::new(HashMap::new())),
        pending: Arc::new(AtomicUsize::new(0)),
        cfg: server.cfg.clone(),
        shutdown: AtomicBool::new(false),
    });
    let conns = Arc::new(AtomicUsize::new(0));
    let sh = shared.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if conns.load(Ordering::SeqCst) >= 256 {
                let mut s = stream;
                let _ = respond(&mut s, 503, &[("Connection", "close".into())], b"too many connections");
                continue;
            }
            conns.fetch_add(1, Ordering::SeqCst);
            let (sh, conns) = (sh.clone(), conns.clone());
            std::thread::spawn(move || {
                handle_conn(stream, &sh);
                conns.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    worker_loop(&mut server, rx);
    Ok(())
}

fn handle_conn(stream: TcpStream, sh: &Arc<HttpShared>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_nodelay(true);
    let Ok(mut w) = stream.try_clone() else { return };
    let mut rd = BufReader::new(stream);
    loop {
        if sh.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let req = match read_request(&mut rd, sh.cfg.limits.max_input_bytes) {
            Ok(r) => r,
            Err(ReadErr::Closed) => return,
            Err(ReadErr::Bad(c, m)) => {
                let _ = respond(&mut w, c, &[("Connection", "close".into()), ("Content-Type", "text/plain".into())], m.as_bytes());
                return;
            }
        };
        let close = req.headers.get("connection").map_or(false, |v| v.eq_ignore_ascii_case("close"));
        if route(&mut w, &req, sh).is_err() || close {
            return;
        }
    }
}

fn json_headers(extra: &[(&str, String)]) -> Vec<(&'static str, String)> {
    let mut h: Vec<(&'static str, String)> = vec![("Content-Type", "application/json".into())];
    for (k, v) in extra {
        h.push((Box::leak(k.to_string().into_boxed_str()), v.clone()));
    }
    h
}

fn http_json_err(w: &mut TcpStream, code: u16, rpc_code: i64, msg: &str) -> std::io::Result<()> {
    respond(w, code, &json_headers(&[]), err_resp(Json::Null, rpc_code, msg).to_string().as_bytes())
}

fn route(w: &mut TcpStream, req: &HttpReq, sh: &Arc<HttpShared>) -> std::io::Result<()> {
    let origin = req.headers.get("origin").cloned();
    if let Some(o) = &origin {
        if !origin_allowed(o, &sh.cfg.mcp.allowed_origins) {
            return respond(w, 403, &[("Content-Type", "text/plain".into())], b"origin not allowed");
        }
    }
    let mut cors: Vec<(&str, String)> = vec![];
    if let Some(o) = &origin {
        cors.push(("Access-Control-Allow-Origin", o.clone()));
        cors.push(("Vary", "Origin".into()));
        cors.push(("Access-Control-Expose-Headers", "Mcp-Session-Id".into()));
    }
    if req.path != sh.cfg.mcp.path {
        return respond(w, 404, &[("Content-Type", "text/plain".into())], b"not found");
    }
    match req.method.as_str() {
        "OPTIONS" => {
            let mut h = cors.clone();
            h.push(("Access-Control-Allow-Methods", "POST, DELETE, OPTIONS".into()));
            h.push(("Access-Control-Allow-Headers", "Content-Type, Accept, Mcp-Session-Id, MCP-Protocol-Version, Last-Event-ID".into()));
            respond(w, 204, &h, b"")
        }
        "GET" => {
            let mut h = cors.clone();
            h.push(("Allow", "POST, DELETE, OPTIONS".into()));
            respond(w, 405, &h, b"")
        }
        "DELETE" => {
            let Some(sid) = req.headers.get("mcp-session-id") else { return respond(w, 400, &cors, b"") };
            if sh.sessions.lock().unwrap().remove(sid) { respond(w, 204, &cors, b"") } else { respond(w, 404, &cors, b"") }
        }
        "POST" => post(w, req, sh, &cors),
        _ => respond(w, 405, &[("Allow", "POST, DELETE, OPTIONS".into())], b""),
    }
}

fn post(w: &mut TcpStream, req: &HttpReq, sh: &Arc<HttpShared>, cors: &[(&str, String)]) -> std::io::Result<()> {
    if let Some(v) = req.headers.get("mcp-protocol-version") {
        if !SUPPORTED_PROTOCOLS.contains(&v.as_str()) {
            return http_json_err(w, 400, E_INVALID_REQUEST, &format!("unsupported MCP-Protocol-Version '{}'", v));
        }
    }
    let parsed: Json = match serde_json::from_slice(&req.body) {
        Ok(j) => j,
        Err(e) => return http_json_err(w, 400, E_PARSE, &format!("parse error: {}", e)),
    };
    let is_batch = parsed.is_array();
    let msgs: Vec<Json> = if let Json::Array(a) = parsed { a } else { vec![parsed] };
    if msgs.is_empty() {
        return http_json_err(w, 400, E_INVALID_REQUEST, "empty batch");
    }
    let has_init = msgs.iter().any(|m| m.get("method").and_then(|x| x.as_str()) == Some("initialize"));
    let mut session = req.headers.get("mcp-session-id").cloned();
    let mut new_session = None;
    if has_init {
        let mut b = [0u8; 16];
        let _ = getrandom::fill(&mut b);
        let sid: String = b.iter().map(|x| format!("{:02x}", x)).collect();
        sh.sessions.lock().unwrap().insert(sid.clone());
        new_session = Some(sid.clone());
        session = Some(sid);
    } else {
        match &session {
            None => return http_json_err(w, 400, E_INVALID_REQUEST, "missing Mcp-Session-Id header (send initialize first)"),
            Some(s) => {
                if !sh.sessions.lock().unwrap().contains(s) {
                    return respond(w, 404, &json_headers(cors), err_resp(Json::Null, E_INVALID_REQUEST, "unknown or expired session").to_string().as_bytes());
                }
            }
        }
    }
    let sid = session.unwrap_or_default();
    let mut notes_all: Vec<Json> = vec![];
    let mut responses: Vec<Json> = vec![];
    for m in msgs {
        if m.get("method").and_then(|x| x.as_str()) == Some("notifications/cancelled") {
            if let Some(rid) = m.get("params").and_then(|p| p.get("requestId")) {
                if let Some(f) = sh.inflight.lock().unwrap().get(&id_key(&sid, rid)) {
                    f.store(true, Ordering::SeqCst);
                }
            }
            continue;
        }
        if !has_id(&m) {
            continue;
        }
        let id = m["id"].clone();
        if sh.pending.load(Ordering::SeqCst) >= sh.cfg.limits.max_concurrent_requests {
            responses.push(err_resp(id, E_BUSY, format!("too many concurrent requests (limit {})", sh.cfg.limits.max_concurrent_requests)));
            continue;
        }
        sh.pending.fetch_add(1, Ordering::SeqCst);
        let flag = Arc::new(AtomicBool::new(false));
        let key = id_key(&sid, &id);
        sh.inflight.lock().unwrap().insert(key.clone(), flag.clone());
        let (rtx, rrx) = channel::<Reply>();
        let job = Job { msg: m, cancel: flag, pending: sh.pending.clone(), reply: Box::new(move |r| {
            let _ = rtx.send(r);
        }) };
        let sent = sh.tx.lock().unwrap().send(Some(job)).is_ok();
        if !sent {
            return http_json_err(w, 503, E_INTERNAL, "server is shutting down");
        }
        let r = rrx.recv();
        sh.inflight.lock().unwrap().remove(&key);
        match r {
            Ok((notes, resp)) => {
                notes_all.extend(notes);
                if let Some(x) = resp {
                    responses.push(x);
                }
            }
            Err(_) => responses.push(err_resp(id, E_INTERNAL, "worker failed")),
        }
    }
    let mut h: Vec<(&str, String)> = cors.to_vec();
    if let Some(s) = &new_session {
        h.push(("Mcp-Session-Id", s.clone()));
    }
    if responses.is_empty() {
        return respond(w, 202, &h, b"");
    }
    let body_json = if is_batch { Json::Array(responses.clone()) } else { responses[0].clone() };
    if notes_all.is_empty() {
        h.push(("Content-Type", "application/json".into()));
        respond(w, 200, &h, body_json.to_string().as_bytes())
    } else {
        let mut sse = String::new();
        for n in &notes_all {
            sse.push_str(&format!("event: message\ndata: {}\n\n", n));
        }
        for r in &responses {
            sse.push_str(&format!("event: message\ndata: {}\n\n", r));
        }
        h.push(("Content-Type", "text/event-stream".into()));
        h.push(("Cache-Control", "no-cache".into()));
        respond(w, 200, &h, sse.as_bytes())
    }
}
