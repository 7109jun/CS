use crate::value::*;
use crate::vm::Vm;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- helpers

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Null)
}

fn nat(name: &str, f: NativeImpl) -> Value {
    Value::Native(Rc::new(NativeFn { name: Rc::from(name), f, this: None }))
}

fn obj(items: Vec<(&str, Value)>) -> Value {
    let mut m = ObjMap::new();
    for (k, v) in items {
        m.insert(Rc::from(k), v);
    }
    Value::object(m)
}

fn s_arg(args: &[Value], i: usize, who: &str) -> Result<Rc<str>, Exc> {
    match args.get(i) {
        Some(Value::Str(s)) => Ok(s.clone()),
        Some(o) => type_err(format!("{}: argument {} must be a string, got {}", who, i, o.type_name())),
        None => type_err(format!("{}: missing string argument {}", who, i)),
    }
}

fn n_arg(args: &[Value], i: usize, who: &str) -> Result<f64, Exc> {
    match args.get(i) {
        Some(Value::Num(n)) => Ok(*n),
        Some(o) => type_err(format!("{}: argument {} must be a number, got {}", who, i, o.type_name())),
        None => type_err(format!("{}: missing number argument {}", who, i)),
    }
}

fn opt_n(args: &[Value], i: usize) -> Option<f64> {
    match args.get(i) {
        Some(Value::Num(n)) => Some(*n),
        _ => None,
    }
}

fn this_arr(args: &[Value]) -> Rc<RefCell<Vec<Value>>> {
    match args.first() {
        Some(Value::Array(a)) => a.clone(),
        _ => Rc::new(RefCell::new(vec![])),
    }
}

fn this_str(args: &[Value]) -> Rc<str> {
    match args.first() {
        Some(Value::Str(s)) => s.clone(),
        _ => Rc::from(""),
    }
}

fn clamp_rel(idx: f64, len: usize) -> usize {
    if idx.is_nan() {
        return 0;
    }
    let l = len as f64;
    let i = idx.trunc();
    let r = if i < 0.0 { (l + i).max(0.0) } else { i.min(l) };
    r as usize
}

fn pok(v: Value) -> NRes {
    Ok(Value::resolved(v))
}

fn perr(name: &str, msg: String) -> NRes {
    Ok(Value::rejected(make_error(name, &msg)))
}

fn fatal_check(vm: &mut Vm) -> Result<(), Exc> {
    vm.check_limits()
}

// ------------------------------------------------------------ string utils

pub fn str_len(s: &str) -> usize {
    if s.is_ascii() { s.len() } else { s.chars().count() }
}

pub fn str_char_at(s: &str, i: usize) -> Option<String> {
    if s.is_ascii() { s.get(i..i + 1).map(|x| x.to_string()) } else { s.chars().nth(i).map(|c| c.to_string()) }
}

fn char_slice(s: &str, a: usize, b: usize) -> String {
    if a >= b {
        return String::new();
    }
    if s.is_ascii() { s[a.min(s.len())..b.min(s.len())].to_string() } else { s.chars().skip(a).take(b - a).collect() }
}

fn byte_to_char_idx(s: &str, byte: usize) -> usize {
    if s.is_ascii() { byte } else { s[..byte].chars().count() }
}

fn char_to_byte_idx(s: &str, ci: usize) -> usize {
    if s.is_ascii() { ci.min(s.len()) } else { s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len()) }
}

// ------------------------------------------------------------- lookup table

pub fn lookup(name: &str) -> Option<Value> {
    Some(match name {
        // prelude functions
        "global.Error" => nat("Error", |_, a| Ok(make_error("Error", &to_display(&arg(&a, 0))))),
        "global.TypeError" => nat("TypeError", |_, a| Ok(make_error("TypeError", &to_display(&arg(&a, 0))))),
        "global.RangeError" => nat("RangeError", |_, a| Ok(make_error("RangeError", &to_display(&arg(&a, 0))))),
        "global.String" => nat("String", |_, a| Ok(Value::string(to_display(&arg(&a, 0))))),
        "global.Number" => nat("Number", g_number),
        "global.Boolean" => nat("Boolean", |_, a| Ok(Value::Bool(arg(&a, 0).truthy()))),
        "global.parseInt" => nat("parseInt", g_parse_int),
        "global.parseFloat" => nat("parseFloat", g_parse_float),
        "global.isNaN" => nat("isNaN", |_, a| Ok(Value::Bool(matches!(arg(&a, 0), Value::Num(n) if n.is_nan())))),
        "global.isFinite" => nat("isFinite", |_, a| Ok(Value::Bool(matches!(arg(&a, 0), Value::Num(n) if n.is_finite())))),
        "global.isInteger" => nat("isInteger", |_, a| Ok(Value::Bool(matches!(arg(&a, 0), Value::Num(n) if n.is_finite() && n.fract() == 0.0)))),
        "global.assert" => nat("assert", |_, a| {
            if arg(&a, 0).truthy() {
                Ok(Value::Null)
            } else {
                throw("AssertionError", if let Value::Str(s) = arg(&a, 1) { s.to_string() } else { "assertion failed".into() })
            }
        }),
        "global.assertEq" => nat("assertEq", |_, a| {
            if deep_equal(&arg(&a, 0), &arg(&a, 1), 0) {
                Ok(Value::Null)
            } else {
                let m = match arg(&a, 2) {
                    Value::Str(s) => format!("{}\n", s),
                    _ => String::new(),
                };
                throw("AssertionError", format!("{}expected {}\n     got {}", m, show(&arg(&a, 1)), show(&arg(&a, 0))))
            }
        }),
        "global.assertNe" => nat("assertNe", |_, a| {
            if !deep_equal(&arg(&a, 0), &arg(&a, 1), 0) {
                Ok(Value::Null)
            } else {
                throw("AssertionError", format!("expected values to differ, both are {}", show(&arg(&a, 0))))
            }
        }),
        "global.assertThrows" => nat("assertThrows", |vm, a| match vm.call_value(arg(&a, 0), vec![]) {
            Err(Exc::Throw(e)) => Ok(e),
            Err(f) => Err(f),
            Ok(_) => throw("AssertionError", if let Value::Str(s) = arg(&a, 1) { s.to_string() } else { "expected function to throw".into() }),
        }),
        "global.assertRejects" => nat("assertRejects", |vm, a| match vm.await_value(arg(&a, 0)) {
            Err(Exc::Throw(e)) => Ok(Value::resolved(e)),
            Err(f) => Err(f),
            Ok(_) => throw("AssertionError", if let Value::Str(s) = arg(&a, 1) { s.to_string() } else { "expected promise to reject".into() }),
        }),
        "global.deepEqual" => nat("deepEqual", |_, a| Ok(Value::Bool(deep_equal(&arg(&a, 0), &arg(&a, 1), 0)))),

        // prelude objects
        "global.console" => {
            let f: [(&str, NativeImpl); 5] = [("log", |vm, a| console(vm, a, false)), ("info", |vm, a| console(vm, a, false)), ("debug", |vm, a| console(vm, a, false)), ("warn", |vm, a| console(vm, a, true)), ("error", |vm, a| console(vm, a, true))];
            obj(f.iter().map(|(n, f)| (*n, nat(n, *f))).collect())
        }
        "global.Math" => math_obj(),
        "global.JSON" => obj(vec![("stringify", nat("JSON.stringify", json_stringify)), ("parse", nat("JSON.parse", json_parse))]),
        "global.Object" => obj(vec![("keys", nat("Object.keys", obj_keys)), ("values", nat("Object.values", obj_values)), ("entries", nat("Object.entries", obj_entries)), ("fromEntries", nat("Object.fromEntries", obj_from_entries))]),
        "global.Promise" => obj(vec![
            ("resolve", nat("Promise.resolve", |_, a| Ok(match arg(&a, 0) { p @ Value::Promise(_) => p, v => Value::resolved(v) }))),
            ("reject", nat("Promise.reject", |_, a| Ok(Value::rejected(arg(&a, 0))))),
            ("all", nat("Promise.all", promise_all)),
            ("race", nat("Promise.race", promise_race)),
            ("allSettled", nat("Promise.allSettled", promise_all_settled)),
        ]),

        // std/path
        "std/path.sep" => Value::str("/"),
        "std/path.join" => nat("path.join", path_join),
        "std/path.resolve" => nat("path.resolve", path_resolve),
        "std/path.normalize" => nat("path.normalize", |_, a| Ok(Value::string(normalize_path(&s_arg(&a, 0, "normalize")?)))),
        "std/path.dirname" => nat("path.dirname", |_, a| {
            let p = normalize_path(&s_arg(&a, 0, "dirname")?);
            Ok(Value::string(match p.rfind('/') {
                Some(0) => "/".into(),
                Some(i) => p[..i].to_string(),
                None => ".".into(),
            }))
        }),
        "std/path.basename" => nat("path.basename", |_, a| {
            let p = s_arg(&a, 0, "basename")?;
            let t = p.trim_end_matches('/');
            let mut b = t.rsplit('/').next().unwrap_or("").to_string();
            if let Some(Value::Str(ext)) = a.get(1) {
                if b.ends_with(&**ext) && b.len() > ext.len() {
                    b.truncate(b.len() - ext.len());
                }
            }
            Ok(Value::string(b))
        }),
        "std/path.extname" => nat("path.extname", |_, a| {
            let p = s_arg(&a, 0, "extname")?;
            let b = p.rsplit('/').next().unwrap_or("");
            Ok(Value::string(match b.rfind('.') {
                Some(i) if i > 0 => b[i..].to_string(),
                _ => String::new(),
            }))
        }),
        "std/path.isAbsolute" => nat("path.isAbsolute", |_, a| Ok(Value::Bool(s_arg(&a, 0, "isAbsolute")?.starts_with('/')))),
        "std/path.relative" => nat("path.relative", |_, a| {
            let f = normalize_path(&s_arg(&a, 0, "relative")?);
            let t = normalize_path(&s_arg(&a, 1, "relative")?);
            let fp: Vec<&str> = f.split('/').filter(|x| !x.is_empty() && *x != ".").collect();
            let tp: Vec<&str> = t.split('/').filter(|x| !x.is_empty() && *x != ".").collect();
            let mut i = 0;
            while i < fp.len() && i < tp.len() && fp[i] == tp[i] {
                i += 1;
            }
            let mut parts: Vec<String> = vec![];
            for _ in i..fp.len() {
                parts.push("..".into());
            }
            for x in &tp[i..] {
                parts.push(x.to_string());
            }
            Ok(Value::string(parts.join("/")))
        }),

        // std/fs
        "std/fs.readFile" => nat("fs.readFile", fs_read_file),
        "std/fs.readFileBase64" => nat("fs.readFileBase64", fs_read_file_b64),
        "std/fs.writeFile" => nat("fs.writeFile", |vm, a| fs_write(vm, a, false)),
        "std/fs.appendFile" => nat("fs.appendFile", |vm, a| fs_write(vm, a, true)),
        "std/fs.readDir" => nat("fs.readDir", fs_read_dir),
        "std/fs.exists" => nat("fs.exists", |vm, a| {
            let p = s_arg(&a, 0, "exists")?;
            match vm.ctx.perms.fs_path(&p, false) {
                Ok(path) => pok(Value::Bool(path.exists())),
                Err(m) => perr("PermissionError", m),
            }
        }),
        "std/fs.stat" => nat("fs.stat", fs_stat),
        "std/fs.mkdir" => nat("fs.mkdir", |vm, a| {
            let p = s_arg(&a, 0, "mkdir")?;
            let rec = arg(&a, 1).truthy();
            match vm.ctx.perms.fs_path(&p, true) {
                Err(m) => perr("PermissionError", m),
                Ok(path) => match if rec { std::fs::create_dir_all(&path) } else { std::fs::create_dir(&path) } {
                    Ok(_) => pok(Value::Null),
                    Err(e) => perr("IOError", format!("mkdir {}: {}", p, e)),
                },
            }
        }),
        "std/fs.remove" => nat("fs.remove", |vm, a| {
            let p = s_arg(&a, 0, "remove")?;
            let rec = arg(&a, 1).truthy();
            match vm.ctx.perms.fs_path(&p, true) {
                Err(m) => perr("PermissionError", m),
                Ok(path) => {
                    if path == vm.ctx.perms.root {
                        return perr("PermissionError", "refusing to remove the project root".into());
                    }
                    let r = if path.is_dir() { if rec { std::fs::remove_dir_all(&path) } else { std::fs::remove_dir(&path) } } else { std::fs::remove_file(&path) };
                    match r {
                        Ok(_) => pok(Value::Null),
                        Err(e) => perr("IOError", format!("remove {}: {}", p, e)),
                    }
                }
            }
        }),
        "std/fs.rename" => nat("fs.rename", |vm, a| fs_two(vm, a, "rename")),
        "std/fs.copyFile" => nat("fs.copyFile", |vm, a| fs_two(vm, a, "copyFile")),

        // std/http
        "std/http.fetch" => nat("http.fetch", http_fetch),
        "std/http.getText" => nat("http.getText", |vm, a| http_simple(vm, a, "GET", false)),
        "std/http.getJson" => nat("http.getJson", |vm, a| http_simple(vm, a, "GET", true)),
        "std/http.postJson" => nat("http.postJson", |vm, a| http_simple(vm, a, "POST", true)),

        // std/process
        "std/process.exec" => nat("process.exec", process_exec),

        // std/env
        "std/env.get" => nat("env.get", |vm, a| {
            let n = s_arg(&a, 0, "env.get")?;
            match vm.ctx.perms.env_var(&n) {
                Err(m) => throw("PermissionError", m),
                Ok(_) => Ok(std::env::var(&*n).map(Value::string).unwrap_or(Value::Null)),
            }
        }),
        "std/env.projectRoot" => nat("env.projectRoot", |vm, _| Ok(Value::string(vm.ctx.perms.root.to_string_lossy().to_string()))),
        "std/env.platform" => nat("env.platform", |_, _| Ok(Value::str(std::env::consts::OS))),

        // std/time
        "std/time.now" => nat("time.now", |_, _| Ok(Value::Num(now_ms()))),
        "std/time.sleep" => nat("time.sleep", |vm, a| {
            let ms = n_arg(&a, 0, "sleep")?.max(0.0);
            let end = Instant::now() + Duration::from_secs_f64(ms / 1000.0);
            loop {
                fatal_check(vm)?;
                let now = Instant::now();
                if now >= end {
                    break;
                }
                std::thread::sleep((end - now).min(Duration::from_millis(20)));
            }
            pok(Value::Null)
        }),
        "std/time.iso" => nat("time.iso", |_, a| Ok(Value::string(iso_from_ms(opt_n(&a, 0).unwrap_or_else(now_ms))))),
        "std/time.parseIso" => nat("time.parseIso", |_, a| Ok(Value::Num(parse_iso(&s_arg(&a, 0, "parseIso")?)))),

        // std/mcp
        "std/mcp.text" => nat("mcp.text", |_, a| Ok(tool_text(&s_arg(&a, 0, "text")?, false))),
        "std/mcp.errorResult" => nat("mcp.errorResult", |_, a| Ok(tool_text(&s_arg(&a, 0, "errorResult")?, true))),
        "std/mcp.image" => nat("mcp.image", |_, a| {
            let d = s_arg(&a, 0, "image")?;
            let m = s_arg(&a, 1, "image")?;
            Ok(obj(vec![("content", Value::array(vec![obj(vec![("type", Value::str("image")), ("data", Value::Str(d)), ("mimeType", Value::Str(m))])]))]))
        }),
        "std/mcp.progress" => nat("mcp.progress", |vm, a| {
            if let Some(tok) = vm.ctx.progress_token.clone() {
                let cur = n_arg(&a, 0, "progress")?;
                let mut p = json!({"progressToken": tok, "progress": cur});
                if let Some(t) = opt_n(&a, 1) {
                    p["total"] = json!(t);
                }
                if let Value::Str(m) = arg(&a, 2) {
                    p["message"] = json!(&*m);
                }
                vm.push_notification(json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": p}));
            }
            Ok(Value::Null)
        }),
        "std/mcp.log" => nat("mcp.log", |vm, a| {
            let lvl = s_arg(&a, 0, "log")?;
            let msg = s_arg(&a, 1, "log")?;
            let n = level_num(&lvl);
            if n >= vm.ctx.log_level {
                vm.push_notification(json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": &*lvl, "logger": "claudescript", "data": &*msg}}));
            }
            Ok(Value::Null)
        }),

        // std/encoding
        "std/encoding.base64Encode" => nat("base64Encode", |_, a| Ok(Value::string(b64_encode(s_arg(&a, 0, "base64Encode")?.as_bytes())))),
        "std/encoding.base64Decode" => nat("base64Decode", |_, a| {
            let b = b64_decode(&s_arg(&a, 0, "base64Decode")?).ok_or_else(|| Exc::Throw(make_error("Error", "invalid base64")))?;
            String::from_utf8(b).map(Value::string).or_else(|_| throw("Error", "decoded data is not valid UTF-8"))
        }),
        "std/encoding.hexEncode" => nat("hexEncode", |_, a| Ok(Value::string(hex(s_arg(&a, 0, "hexEncode")?.as_bytes())))),
        "std/encoding.hexDecode" => nat("hexDecode", |_, a| {
            let s = s_arg(&a, 0, "hexDecode")?;
            if s.len() % 2 != 0 || !s.is_ascii() {
                return throw("Error", "invalid hex string");
            }
            let mut out = vec![];
            for i in (0..s.len()).step_by(2) {
                out.push(u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| Exc::Throw(make_error("Error", "invalid hex string")))?);
            }
            String::from_utf8(out).map(Value::string).or_else(|_| throw("Error", "decoded data is not valid UTF-8"))
        }),
        "std/encoding.urlEncode" => nat("urlEncode", |_, a| {
            let s = s_arg(&a, 0, "urlEncode")?;
            let mut o = String::new();
            for b in s.bytes() {
                if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                    o.push(b as char);
                } else {
                    o.push_str(&format!("%{:02X}", b));
                }
            }
            Ok(Value::string(o))
        }),
        "std/encoding.urlDecode" => nat("urlDecode", |_, a| {
            let s = s_arg(&a, 0, "urlDecode")?;
            let b = s.as_bytes();
            let mut out = vec![];
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' && i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 + 0 {
                    match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                        Ok(v) => {
                            out.push(v);
                            i += 3;
                            continue;
                        }
                        Err(_) => return throw("Error", "invalid percent-encoding"),
                    }
                } else if b[i] == b'%' {
                    return throw("Error", "invalid percent-encoding");
                }
                out.push(b[i]);
                i += 1;
            }
            String::from_utf8(out).map(Value::string).or_else(|_| throw("Error", "decoded data is not valid UTF-8"))
        }),

        // std/crypto
        "std/crypto.sha256Hex" => nat("sha256Hex", |_, a| Ok(Value::string(hex(&Sha256::digest(s_arg(&a, 0, "sha256Hex")?.as_bytes()))))),
        "std/crypto.uuid" => nat("uuid", |_, _| {
            let mut b = [0u8; 16];
            fill_random(&mut b);
            b[6] = (b[6] & 0x0f) | 0x40;
            b[8] = (b[8] & 0x3f) | 0x80;
            let h = hex(&b);
            Ok(Value::string(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])))
        }),
        "std/crypto.randomHex" => nat("randomHex", |_, a| {
            let n = n_arg(&a, 0, "randomHex")?;
            if !(0.0..=1_048_576.0).contains(&n) {
                return throw("RangeError", "randomHex: byte count out of range");
            }
            let mut b = vec![0u8; n as usize];
            fill_random(&mut b);
            Ok(Value::string(hex(&b)))
        }),

        // std/regex
        "std/regex.test" => nat("regex.test", |_, a| Ok(Value::Bool(re(&a, "test")?.is_match(&s_arg(&a, 1, "test")?)))),
        "std/regex.find" => nat("regex.find", |_, a| {
            let r = re(&a, "find")?;
            let t = s_arg(&a, 1, "find")?;
            Ok(r.captures(&t).map(|c| match_value(&t, &c)).unwrap_or(Value::Null))
        }),
        "std/regex.findAll" => nat("regex.findAll", |_, a| {
            let r = re(&a, "findAll")?;
            let t = s_arg(&a, 1, "findAll")?;
            Ok(Value::array(r.captures_iter(&t).take(100_000).map(|c| match_value(&t, &c)).collect()))
        }),
        "std/regex.replaceAll" => nat("regex.replaceAll", |_, a| {
            let r = re(&a, "replaceAll")?;
            let t = s_arg(&a, 1, "replaceAll")?;
            let rep = s_arg(&a, 2, "replaceAll")?;
            Ok(Value::string(r.replace_all(&t, &*rep).to_string()))
        }),
        "std/regex.split" => nat("regex.split", |_, a| {
            let r = re(&a, "split")?;
            let t = s_arg(&a, 1, "split")?;
            Ok(Value::array(r.split(&t).map(Value::str).collect()))
        }),
        _ => return None,
    })
}

fn show(v: &Value) -> String {
    match v {
        Value::Str(s) => format!("{:?}", &**s),
        Value::Array(_) | Value::Object(_) => to_json(v).map(|j| j.to_string()).unwrap_or_else(|_| to_display(v)),
        other => to_display(other),
    }
}

fn tool_text(s: &str, is_err: bool) -> Value {
    let mut items = vec![("content", Value::array(vec![obj(vec![("type", Value::str("text")), ("text", Value::str(s))])]))];
    if is_err {
        items.push(("isError", Value::Bool(true)));
    }
    obj(items)
}

fn level_num(l: &str) -> u8 {
    ["debug", "info", "notice", "warning", "error", "critical", "alert", "emergency"].iter().position(|x| *x == l).unwrap_or(1) as u8
}

fn fill_random(b: &mut [u8]) {
    if getrandom::fill(b).is_err() {
        let mut x = now_ms() as u64 ^ 0x9E3779B97F4A7C15;
        for v in b.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *v = x as u8;
        }
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

// -------------------------------------------------------------- console

fn console(vm: &mut Vm, args: Vec<Value>, err: bool) -> NRes {
    let parts: Vec<String> = args
        .iter()
        .map(|v| match v {
            Value::Array(_) | Value::Object(_) => to_json(v).map(|j| j.to_string()).unwrap_or_else(|_| to_display(v)),
            other => to_display(other),
        })
        .collect();
    let line = parts.join(" ");
    if err || vm.ctx.console_to_stderr {
        eprintln!("{}", line);
    } else {
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{}", line);
    }
    Ok(Value::Null)
}

// ----------------------------------------------------- number conversions

fn g_number(_: &mut Vm, a: Vec<Value>) -> NRes {
    Ok(Value::Num(match arg(&a, 0) {
        Value::Num(n) => n,
        Value::Bool(b) => b as i32 as f64,
        Value::Null => 0.0,
        Value::Str(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else if let Some(h) = t.strip_prefix("0x") {
                i64::from_str_radix(h, 16).map(|v| v as f64).unwrap_or(f64::NAN)
            } else if t.chars().all(|c| c.is_ascii_digit() || "+-.eE".contains(c)) || t == "Infinity" || t == "-Infinity" {
                t.parse::<f64>().unwrap_or(f64::NAN)
            } else {
                f64::NAN
            }
        }
        _ => f64::NAN,
    }))
}

fn g_parse_int(_: &mut Vm, a: Vec<Value>) -> NRes {
    let s = s_arg(&a, 0, "parseInt")?;
    let mut radix = opt_n(&a, 1).unwrap_or(10.0) as u32;
    let mut t = s.trim();
    let neg = t.starts_with('-');
    if t.starts_with('-') || t.starts_with('+') {
        t = &t[1..];
    }
    if (radix == 16 || opt_n(&a, 1).is_none()) && (t.starts_with("0x") || t.starts_with("0X")) {
        t = &t[2..];
        radix = 16;
    }
    if !(2..=36).contains(&radix) {
        return Ok(Value::Num(f64::NAN));
    }
    let mut v: f64 = 0.0;
    let mut any = false;
    for c in t.chars() {
        match c.to_digit(radix) {
            Some(d) => {
                v = v * radix as f64 + d as f64;
                any = true;
            }
            None => break,
        }
    }
    Ok(Value::Num(if !any { f64::NAN } else if neg { -v } else { v }))
}

fn g_parse_float(_: &mut Vm, a: Vec<Value>) -> NRes {
    let s = s_arg(&a, 0, "parseFloat")?;
    let t = s.trim();
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    if t[i..].starts_with("Infinity") {
        return Ok(Value::Num(if t.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY }));
    }
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == ds {
        return Ok(Value::Num(f64::NAN));
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let es = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > es {
            i = j;
        }
    }
    Ok(Value::Num(t[..i].parse::<f64>().unwrap_or(f64::NAN)))
}

// ----------------------------------------------------------------- Math

fn math_obj() -> Value {
    fn m1(name: &'static str, f: NativeImpl) -> (&'static str, Value) {
        (name, nat(name, f))
    }
    macro_rules! un {
        ($n:expr, $f:expr) => {
            m1($n, |_, a| {
                let x = n_arg(&a, 0, $n)?;
                let f: fn(f64) -> f64 = $f;
                Ok(Value::Num(f(x)))
            })
        };
    }
    obj(vec![
        ("PI", Value::Num(std::f64::consts::PI)),
        ("E", Value::Num(std::f64::consts::E)),
        un!("floor", f64::floor),
        un!("ceil", f64::ceil),
        un!("round", |x| (x + 0.5).floor()),
        un!("trunc", f64::trunc),
        un!("abs", f64::abs),
        un!("sign", |x| if x.is_nan() { f64::NAN } else if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 }),
        un!("sqrt", f64::sqrt),
        un!("cbrt", f64::cbrt),
        un!("log", f64::ln),
        un!("log2", f64::log2),
        un!("log10", f64::log10),
        un!("exp", f64::exp),
        un!("sin", f64::sin),
        un!("cos", f64::cos),
        un!("tan", f64::tan),
        un!("atan", f64::atan),
        m1("pow", |_, a| Ok(Value::Num(n_arg(&a, 0, "pow")?.powf(n_arg(&a, 1, "pow")?)))),
        m1("atan2", |_, a| Ok(Value::Num(n_arg(&a, 0, "atan2")?.atan2(n_arg(&a, 1, "atan2")?)))),
        m1("min", |_, a| {
            let mut r = f64::INFINITY;
            for i in 0..a.len() {
                let n = n_arg(&a, i, "min")?;
                if n.is_nan() {
                    return Ok(Value::Num(f64::NAN));
                }
                r = r.min(n);
            }
            Ok(Value::Num(r))
        }),
        m1("max", |_, a| {
            let mut r = f64::NEG_INFINITY;
            for i in 0..a.len() {
                let n = n_arg(&a, i, "max")?;
                if n.is_nan() {
                    return Ok(Value::Num(f64::NAN));
                }
                r = r.max(n);
            }
            Ok(Value::Num(r))
        }),
        m1("hypot", |_, a| {
            let mut s = 0.0;
            for i in 0..a.len() {
                let n = n_arg(&a, i, "hypot")?;
                s += n * n;
            }
            Ok(Value::Num(s.sqrt()))
        }),
        m1("random", |_, _| {
            let mut b = [0u8; 8];
            fill_random(&mut b);
            Ok(Value::Num((u64::from_le_bytes(b) >> 11) as f64 / (1u64 << 53) as f64))
        }),
    ])
}

// ----------------------------------------------------------------- JSON

fn json_stringify(_: &mut Vm, a: Vec<Value>) -> NRes {
    let v = arg(&a, 0);
    let j = match to_json(&v) {
        Ok(j) => j,
        Err(e) => return throw("TypeError", e),
    };
    let indent = opt_n(&a, 1).unwrap_or(0.0).clamp(0.0, 10.0) as usize;
    if indent == 0 {
        Ok(Value::string(j.to_string()))
    } else {
        let ind = " ".repeat(indent);
        let mut buf = Vec::new();
        let fmt = serde_json::ser::PrettyFormatter::with_indent(ind.as_bytes());
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
        serde::Serialize::serialize(&j, &mut ser).map_err(|e| Exc::Throw(make_error("Error", &e.to_string())))?;
        Ok(Value::string(String::from_utf8_lossy(&buf).to_string()))
    }
}

fn json_parse(_: &mut Vm, a: Vec<Value>) -> NRes {
    let s = s_arg(&a, 0, "JSON.parse")?;
    match serde_json::from_str::<serde_json::Value>(&s) {
        Ok(j) => Ok(from_json(&j)),
        Err(e) => throw("SyntaxError", format!("invalid JSON: {}", e)),
    }
}

// --------------------------------------------------------------- Object

fn this_obj(a: &[Value], who: &str) -> Result<Rc<RefCell<ObjMap>>, Exc> {
    match a.first() {
        Some(Value::Object(o)) => Ok(o.clone()),
        Some(o) => type_err(format!("{}: expected an object, got {}", who, o.type_name())),
        None => type_err(format!("{}: missing argument", who)),
    }
}

fn obj_keys(_: &mut Vm, a: Vec<Value>) -> NRes {
    let o = this_obj(&a, "Object.keys")?;
    let r = o.borrow().keys().map(|k| Value::Str(k.clone())).collect();
    Ok(Value::array(r))
}
fn obj_values(_: &mut Vm, a: Vec<Value>) -> NRes {
    let o = this_obj(&a, "Object.values")?;
    let r = o.borrow().values().cloned().collect();
    Ok(Value::array(r))
}
fn obj_entries(_: &mut Vm, a: Vec<Value>) -> NRes {
    let o = this_obj(&a, "Object.entries")?;
    let r = o.borrow().iter().map(|(k, v)| Value::array(vec![Value::Str(k.clone()), v.clone()])).collect();
    Ok(Value::array(r))
}
fn obj_from_entries(_: &mut Vm, a: Vec<Value>) -> NRes {
    let Some(Value::Array(arr)) = a.first() else { return type_err("Object.fromEntries: expected an array") };
    let mut m = ObjMap::new();
    for e in arr.borrow().iter() {
        match e {
            Value::Array(p) if p.borrow().len() >= 2 => {
                let p = p.borrow();
                m.insert(Rc::from(to_display(&p[0])), p[1].clone());
            }
            _ => return type_err("Object.fromEntries: entries must be [key, value] pairs"),
        }
    }
    Ok(Value::object(m))
}

// -------------------------------------------------------------- Promise

fn promise_all(vm: &mut Vm, a: Vec<Value>) -> NRes {
    let Some(Value::Array(arr)) = a.first() else { return type_err("Promise.all: expected an array") };
    let items: Vec<Value> = arr.borrow().clone();
    let mut out = vec![];
    for it in items {
        match vm.await_value(it) {
            Ok(v) => out.push(v),
            Err(Exc::Throw(e)) => return Ok(Value::rejected(e)),
            Err(f) => return Err(f),
        }
    }
    Ok(Value::resolved(Value::array(out)))
}

fn promise_race(vm: &mut Vm, a: Vec<Value>) -> NRes {
    let Some(Value::Array(arr)) = a.first() else { return type_err("Promise.race: expected an array") };
    let first = arr.borrow().first().cloned();
    match first {
        None => Ok(Value::resolved(Value::Null)),
        Some(it) => match vm.await_value(it) {
            Ok(v) => Ok(Value::resolved(v)),
            Err(Exc::Throw(e)) => Ok(Value::rejected(e)),
            Err(f) => Err(f),
        },
    }
}

fn promise_all_settled(vm: &mut Vm, a: Vec<Value>) -> NRes {
    let Some(Value::Array(arr)) = a.first() else { return type_err("Promise.allSettled: expected an array") };
    let items: Vec<Value> = arr.borrow().clone();
    let mut out = vec![];
    for it in items {
        match vm.await_value(it) {
            Ok(v) => out.push(obj(vec![("status", Value::str("fulfilled")), ("value", v)])),
            Err(Exc::Throw(e)) => out.push(obj(vec![("status", Value::str("rejected")), ("reason", e)])),
            Err(f) => return Err(f),
        }
    }
    Ok(Value::resolved(Value::array(out)))
}

// ----------------------------------------------------------------- path

fn normalize_path(p: &str) -> String {
    let abs = p.starts_with('/');
    let mut out: Vec<&str> = vec![];
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.last().map_or(false, |l| *l != "..") {
                    out.pop();
                } else if !abs {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let j = out.join("/");
    if abs {
        format!("/{}", j)
    } else if j.is_empty() {
        ".".into()
    } else {
        j
    }
}

fn path_join(_: &mut Vm, a: Vec<Value>) -> NRes {
    let mut parts = vec![];
    for i in 0..a.len() {
        let s = s_arg(&a, i, "join")?;
        if !s.is_empty() {
            parts.push(s.to_string());
        }
    }
    if parts.is_empty() {
        return Ok(Value::str("."));
    }
    Ok(Value::string(normalize_path(&parts.join("/"))))
}

fn path_resolve(vm: &mut Vm, a: Vec<Value>) -> NRes {
    let mut cur = vm.ctx.perms.root.to_string_lossy().to_string();
    for i in 0..a.len() {
        let s = s_arg(&a, i, "resolve")?;
        if s.starts_with('/') {
            cur = s.to_string();
        } else if !s.is_empty() {
            cur = format!("{}/{}", cur, s);
        }
    }
    Ok(Value::string(normalize_path(&cur)))
}

// ------------------------------------------------------------------- fs

fn io_err(who: &str, p: &str, e: std::io::Error) -> NRes {
    perr("IOError", format!("{} {}: {}", who, p, e))
}

fn read_limited(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > 64 * 1024 * 1024 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "file is larger than 64 MiB"));
    }
    std::fs::read(path)
}

fn fs_read_file(vm: &mut Vm, a: Vec<Value>) -> NRes {
    let p = s_arg(&a, 0, "readFile")?;
    let path = match vm.ctx.perms.fs_path(&p, false) {
        Ok(x) => x,
        Err(m) => return perr("PermissionError", m),
    };
    match read_limited(&path) {
        Ok(b) => match String::from_utf8(b) {
            Ok(s) => pok(Value::string(s)),
            Err(_) => perr("IOError", format!("readFile {}: file is not valid UTF-8 (use readFileBase64)", p)),
        },
        Err(e) => io_err("readFile", &p, e),
    }
}

fn fs_read_file_b64(vm: &mut Vm, a: Vec<Value>) -> NRes {
    let p = s_arg(&a, 0, "readFileBase64")?;
    let path = match vm.ctx.perms.fs_path(&p, false) {
        Ok(x) => x,
        Err(m) => return perr("PermissionError", m),
    };
    match read_limited(&path) {
        Ok(b) => pok(Value::string(b64_encode(&b))),
        Err(e) => io_err("readFileBase64", &p, e),
    }
}

fn fs_write(vm: &mut Vm, a: Vec<Value>, append: bool) -> NRes {
    let p = s_arg(&a, 0, "writeFile")?;
    let c = s_arg(&a, 1, "writeFile")?;
    let path = match vm.ctx.perms.fs_path(&p, true) {
        Ok(x) => x,
        Err(m) => return perr("PermissionError", m),
    };
    let r = if append { std::fs::OpenOptions::new().create(true).append(true).open(&path).and_then(|mut f| f.write_all(c.as_bytes())) } else { std::fs::write(&path, c.as_bytes()) };
    match r {
        Ok(_) => pok(Value::Null),
        Err(e) => io_err("writeFile", &p, e),
    }
}

fn fs_two(vm: &mut Vm, a: Vec<Value>, who: &str) -> NRes {
    let f = s_arg(&a, 0, who)?;
    let t = s_arg(&a, 1, who)?;
    let (pf, pt) = match (vm.ctx.perms.fs_path(&f, true), vm.ctx.perms.fs_path(&t, true)) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(m), _) | (_, Err(m)) => return perr("PermissionError", m),
    };
    let r = if who == "rename" { std::fs::rename(&pf, &pt) } else { std::fs::copy(&pf, &pt).map(|_| ()) };
    match r {
        Ok(_) => pok(Value::Null),
        Err(e) => io_err(who, &f, e),
    }
}

fn mtime_ms(m: &std::fs::Metadata) -> f64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

fn fs_stat(vm: &mut Vm, a: Vec<Value>) -> NRes {
    let p = s_arg(&a, 0, "stat")?;
    let path = match vm.ctx.perms.fs_path(&p, false) {
        Ok(x) => x,
        Err(m) => return perr("PermissionError", m),
    };
    match std::fs::metadata(&path) {
        Ok(m) => pok(obj(vec![("isDir", Value::Bool(m.is_dir())), ("isFile", Value::Bool(m.is_file())), ("size", Value::Num(m.len() as f64)), ("modifiedMs", Value::Num(mtime_ms(&m)))])),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => pok(Value::Null),
        Err(e) => io_err("stat", &p, e),
    }
}

fn fs_read_dir(vm: &mut Vm, a: Vec<Value>) -> NRes {
    let p = s_arg(&a, 0, "readDir")?;
    let rec = arg(&a, 1).truthy();
    let path = match vm.ctx.perms.fs_path(&p, false) {
        Ok(x) => x,
        Err(m) => return perr("PermissionError", m),
    };
    let mut out = vec![];
    let mut stack = vec![path.clone()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) => return io_err("readDir", &p, e),
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let ep = e.path();
            let md = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let rel = ep.strip_prefix(&path).unwrap_or(&ep).to_string_lossy().to_string();
            out.push(obj(vec![("name", Value::string(e.file_name().to_string_lossy().to_string())), ("path", Value::string(rel)), ("isDir", Value::Bool(md.is_dir())), ("size", Value::Num(md.len() as f64))]));
            if rec && md.is_dir() && !e.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                stack.push(ep);
            }
            if out.len() > 200_000 {
                return perr("IOError", "readDir: too many entries".into());
            }
        }
        fatal_check(vm)?;
    }
    out.sort_by(|x, y| {
        let k = |v: &Value| if let Value::Object(o) = v { o.borrow().get("path").map(to_display).unwrap_or_default() } else { String::new() };
        k(x).cmp(&k(y))
    });
    pok(Value::array(out))
}

// ----------------------------------------------------------------- http

fn http_do(vm: &mut Vm, url: &str, method: &str, headers: Vec<(String, String)>, body: Option<String>, timeout: f64) -> Result<Value, (String, String)> {
    let perms = vm.ctx.perms.clone();
    perms.network_host(url).map_err(|m| ("PermissionError".to_string(), m))?;
    let mut remaining = timeout;
    if let Some(d) = vm.ctx.deadline {
        remaining = remaining.min(d.saturating_duration_since(Instant::now()).as_secs_f64().max(0.1));
    }
    let cfg = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs_f64(remaining))).http_status_as_error(false).max_redirects(0).build();
    let agent = ureq::Agent::new_with_config(cfg);
    let mut cur = url.to_string();
    let mut method = method.to_string();
    let mut body = body;
    for _hop in 0..6 {
        let mut b = ureq::http::Request::builder().method(method.as_str()).uri(cur.as_str());
        for (k, v) in &headers {
            b = b.header(k.as_str(), v.as_str());
        }
        let req = b.body(body.clone().unwrap_or_default()).map_err(|e| ("Error".to_string(), format!("invalid request: {}", e)))?;
        let mut resp = agent.run(req).map_err(|e| ("NetworkError".to_string(), format!("{}: {}", cur, e)))?;
        let status = resp.status().as_u16();
        if [301, 302, 303, 307, 308].contains(&status) {
            if let Some(loc) = resp.headers().get("location").and_then(|v| v.to_str().ok()) {
                let next = if loc.starts_with("http://") || loc.starts_with("https://") {
                    loc.to_string()
                } else if let Some(rest) = loc.strip_prefix('/') {
                    let (scheme, r) = cur.split_once("://").unwrap_or(("https", &cur));
                    let host = r.split('/').next().unwrap_or("");
                    format!("{}://{}/{}", scheme, host, rest)
                } else {
                    return Err(("NetworkError".into(), format!("unsupported redirect target '{}'", loc)));
                };
                perms.network_host(&next).map_err(|m| ("PermissionError".to_string(), format!("redirect blocked: {}", m)))?;
                if status == 303 || ((status == 301 || status == 302) && method == "POST") {
                    method = "GET".into();
                    body = None;
                }
                cur = next;
                continue;
            }
        }
        let mut hm = ObjMap::new();
        for (k, v) in resp.headers().iter() {
            let key: Rc<str> = Rc::from(k.as_str().to_lowercase());
            let val = v.to_str().unwrap_or("").to_string();
            let merged = match hm.get(&key) {
                Some(Value::Str(old)) => format!("{}, {}", old, val),
                _ => val,
            };
            hm.insert(key, Value::string(merged));
        }
        let text = resp.body_mut().with_config().limit(16 * 1024 * 1024).read_to_string().map_err(|e| ("NetworkError".to_string(), format!("reading body: {}", e)))?;
        return Ok(obj(vec![("status", Value::Num(status as f64)), ("ok", Value::Bool((200..300).contains(&status))), ("headers", Value::object(hm)), ("body", Value::string(text))]));
    }
    Err(("NetworkError".into(), "too many redirects".into()))
}

fn headers_from(v: &Value) -> Vec<(String, String)> {
    match v {
        Value::Object(o) => o.borrow().iter().map(|(k, v)| (k.to_string(), to_display(v))).collect(),
        _ => vec![],
    }
}

fn http_fetch(vm: &mut Vm, a: Vec<Value>) -> NRes {
    let url = s_arg(&a, 0, "fetch")?;
    let (mut method, mut headers, mut body, mut timeout) = ("GET".to_string(), vec![], None, 30.0);
    if let Value::Object(o) = arg(&a, 1) {
        let o = o.borrow();
        if let Some(Value::Str(m)) = o.get("method") {
            method = m.to_uppercase();
        }
        if let Some(h) = o.get("headers") {
            headers = headers_from(h);
        }
        if let Some(Value::Str(b)) = o.get("body") {
            body = Some(b.to_string());
        }
        if let Some(Value::Num(t)) = o.get("timeoutSeconds") {
            timeout = t.max(0.1);
        }
    }
    match http_do(vm, &url, &method, headers, body, timeout) {
        Ok(v) => pok(v),
        Err((n, m)) => perr(&n, m),
    }
}

fn http_simple(vm: &mut Vm, a: Vec<Value>, method: &str, json_mode: bool) -> NRes {
    let url = s_arg(&a, 0, "http")?;
    let mut headers;
    let mut body = None;
    if method == "POST" {
        let j = match to_json(&arg(&a, 1)) {
            Ok(j) => j,
            Err(e) => return perr("TypeError", e),
        };
        body = Some(j.to_string());
        headers = headers_from(&arg(&a, 2));
        headers.push(("content-type".into(), "application/json".into()));
    } else {
        headers = headers_from(&arg(&a, 1));
    }
    if json_mode {
        headers.push(("accept".into(), "application/json".into()));
    }
    match http_do(vm, &url, method, headers, body, 30.0) {
        Err((n, m)) => perr(&n, m),
        Ok(Value::Object(o)) => {
            let o = o.borrow();
            let status = if let Some(Value::Num(n)) = o.get("status") { *n } else { 0.0 };
            let text = o.get("body").map(to_display).unwrap_or_default();
            if !(200.0..300.0).contains(&status) {
                return perr("HttpError", format!("{} {} -> HTTP {}", method, url, status));
            }
            if json_mode {
                match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(j) => pok(from_json(&j)),
                    Err(e) => perr("SyntaxError", format!("response is not valid JSON: {}", e)),
                }
            } else {
                pok(Value::string(text))
            }
        }
        Ok(_) => unreachable!(),
    }
}

// -------------------------------------------------------------- process

fn process_exec(vm: &mut Vm, a: Vec<Value>) -> NRes {
    let cmd = s_arg(&a, 0, "exec")?;
    if let Err(m) = vm.ctx.perms.process_cmd(&cmd) {
        return perr("PermissionError", m);
    }
    let mut args: Vec<String> = vec![];
    if let Value::Array(x) = arg(&a, 1) {
        for v in x.borrow().iter() {
            args.push(to_display(v));
        }
    }
    let mut cwd = vm.ctx.perms.root.clone();
    let mut input: Option<String> = None;
    let mut timeout = 30.0f64;
    if let Value::Object(o) = arg(&a, 2) {
        let o = o.borrow();
        if let Some(Value::Str(c)) = o.get("cwd") {
            match vm.ctx.perms.fs_path(c, false) {
                Ok(p) => cwd = p,
                Err(m) => return perr("PermissionError", m),
            }
        }
        if let Some(Value::Str(i)) = o.get("input") {
            input = Some(i.to_string());
        }
        if let Some(Value::Num(t)) = o.get("timeoutSeconds") {
            timeout = t.max(0.05);
        }
    }
    let mut end = Instant::now() + Duration::from_secs_f64(timeout);
    if let Some(d) = vm.ctx.deadline {
        end = end.min(d);
    }
    let mut child = match std::process::Command::new(&*cmd)
        .args(&args)
        .current_dir(&cwd)
        .stdin(if input.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return perr("ProcessError", format!("cannot start '{}': {}", cmd, e)),
    };
    if let (Some(inp), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(inp.as_bytes());
        });
    }
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t1 = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = (&mut so).take(16 * 1024 * 1024).read_to_end(&mut b);
        b
    });
    let t2 = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = (&mut se).take(16 * 1024 * 1024).read_to_end(&mut b);
        b
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) => {}
            Err(e) => break Err(e.to_string()),
        }
        if Instant::now() >= end || vm.check_limits().is_err() {
            let _ = child.kill();
            let _ = child.wait();
            let fatal = vm.check_limits();
            let _ = t1.join();
            let _ = t2.join();
            fatal?;
            return perr("TimeoutError", format!("'{}' timed out after {}s", cmd, timeout));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let out = t1.join().unwrap_or_default();
    let err = t2.join().unwrap_or_default();
    match status {
        Ok(s) => pok(obj(vec![("code", Value::Num(s.code().unwrap_or(-1) as f64)), ("stdout", Value::string(String::from_utf8_lossy(&out).to_string())), ("stderr", Value::string(String::from_utf8_lossy(&err).to_string()))])),
        Err(e) => perr("ProcessError", e),
    }
}

// ----------------------------------------------------------------- time

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn iso_from_ms(ms: f64) -> String {
    let ms = ms.floor() as i64;
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, m, d, rem / 3_600_000, rem / 60_000 % 60, rem / 1000 % 60, rem % 1000)
}

fn parse_iso(s: &str) -> f64 {
    let r = (|| -> Option<f64> {
        let s = s.trim();
        let (date, rest) = match s.split_once(['T', ' ']) {
            Some((d, r)) => (d, Some(r)),
            None => (s, None),
        };
        let mut dp = date.split('-');
        let y: i64 = dp.next()?.parse().ok()?;
        let m: i64 = dp.next()?.parse().ok()?;
        let d: i64 = dp.next()?.parse().ok()?;
        if dp.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        let mut total = days_from_civil(y, m, d) as f64 * 86_400_000.0;
        if let Some(r) = rest {
            let (time, off) = if let Some(t) = r.strip_suffix('Z') {
                (t, 0.0)
            } else if let Some(i) = r.rfind(['+', '-']) {
                let (t, o) = r.split_at(i);
                let sign = if o.starts_with('-') { -1.0 } else { 1.0 };
                let o = &o[1..];
                let (hh, mm) = o.split_once(':').unwrap_or((o, "0"));
                (t, sign * (hh.parse::<f64>().ok()? * 60.0 + mm.parse::<f64>().ok()?))
            } else {
                (r, 0.0)
            };
            let mut tp = time.split(':');
            let hh: f64 = tp.next()?.parse().ok()?;
            let mm: f64 = tp.next().unwrap_or("0").parse().ok()?;
            let ss: f64 = tp.next().unwrap_or("0").parse().ok()?;
            if hh >= 24.0 || mm >= 60.0 || ss >= 61.0 {
                return None;
            }
            total += ((hh * 60.0 + mm) * 60.0 + ss) * 1000.0 - off * 60_000.0;
        }
        Some(total)
    })();
    r.unwrap_or(f64::NAN)
}

// ------------------------------------------------------------- encoding

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(data: &[u8]) -> String {
    let mut o = String::with_capacity(data.len() * 4 / 3 + 4);
    for ch in data.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        o.push(B64[(n >> 18) as usize & 63] as char);
        o.push(B64[(n >> 12) as usize & 63] as char);
        o.push(if ch.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        o.push(if ch.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    o
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let t: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let t: Vec<u8> = t.into_iter().take_while(|b| *b != b'=').collect();
    let mut out = vec![];
    let mut acc = 0u32;
    let mut bits = 0;
    for c in t {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

// ---------------------------------------------------------------- regex

fn re(a: &[Value], who: &str) -> Result<regex::Regex, Exc> {
    let p = s_arg(a, 0, who)?;
    regex::RegexBuilder::new(&p).size_limit(2 * 1024 * 1024).build().map_err(|e| Exc::Throw(make_error("SyntaxError", &format!("invalid regex: {}", e))))
}

fn match_value(text: &str, c: &regex::Captures) -> Value {
    let m = c.get(0).unwrap();
    let groups: Vec<Value> = (1..c.len()).map(|i| c.get(i).map(|g| Value::str(g.as_str())).unwrap_or(Value::Null)).collect();
    obj(vec![("text", Value::str(m.as_str())), ("index", Value::Num(byte_to_char_idx(text, m.start()) as f64)), ("groups", Value::array(groups))])
}

// =================================================================== arrays

pub fn array_method(name: &str) -> Option<NativeImpl> {
    Some(match name {
        "push" => |_, a| {
            let arr = this_arr(&a);
            let mut b = arr.borrow_mut();
            if b.len() + a.len() > crate::vm::MAX_ARRAY_LEN {
                return throw("RangeError", "array is too large");
            }
            b.extend(a.into_iter().skip(1));
            Ok(Value::Num(b.len() as f64))
        },
        "pop" => |_, a| Ok(this_arr(&a).borrow_mut().pop().unwrap_or(Value::Null)),
        "shift" => |_, a| {
            let arr = this_arr(&a);
            let mut b = arr.borrow_mut();
            Ok(if b.is_empty() { Value::Null } else { b.remove(0) })
        },
        "unshift" => |_, a| {
            let arr = this_arr(&a);
            let mut b = arr.borrow_mut();
            for (i, v) in a.into_iter().skip(1).enumerate() {
                b.insert(i, v);
            }
            Ok(Value::Num(b.len() as f64))
        },
        "slice" => |_, a| {
            let arr = this_arr(&a);
            let b = arr.borrow();
            let s = clamp_rel(opt_n(&a, 1).unwrap_or(0.0), b.len());
            let e = clamp_rel(opt_n(&a, 2).unwrap_or(b.len() as f64), b.len());
            Ok(Value::array(if s < e { b[s..e].to_vec() } else { vec![] }))
        },
        "splice" => |_, a| {
            let arr = this_arr(&a);
            let mut b = arr.borrow_mut();
            let s = clamp_rel(n_arg(&a, 1, "splice")?, b.len());
            let dc = (opt_n(&a, 2).unwrap_or((b.len() - s) as f64).max(0.0) as usize).min(b.len() - s);
            let removed: Vec<Value> = b.splice(s..s + dc, a.into_iter().skip(3)).collect();
            Ok(Value::array(removed))
        },
        "concat" => |_, a| {
            let mut out = this_arr(&a).borrow().clone();
            for o in a.iter().skip(1) {
                match o {
                    Value::Array(x) => out.extend(x.borrow().iter().cloned()),
                    other => out.push(other.clone()),
                }
            }
            Ok(Value::array(out))
        },
        "map" => |vm, a| {
            let items = this_arr(&a).borrow().clone();
            let f = arg(&a, 1);
            let mut out = Vec::with_capacity(items.len());
            for (i, x) in items.into_iter().enumerate() {
                out.push(vm.call_value(f.clone(), vec![x, Value::Num(i as f64)])?);
            }
            Ok(Value::array(out))
        },
        "flatMap" => |vm, a| {
            let items = this_arr(&a).borrow().clone();
            let f = arg(&a, 1);
            let mut out = vec![];
            for (i, x) in items.into_iter().enumerate() {
                match vm.call_value(f.clone(), vec![x, Value::Num(i as f64)])? {
                    Value::Array(r) => out.extend(r.borrow().iter().cloned()),
                    other => out.push(other),
                }
            }
            Ok(Value::array(out))
        },
        "filter" => |vm, a| {
            let items = this_arr(&a).borrow().clone();
            let f = arg(&a, 1);
            let mut out = vec![];
            for (i, x) in items.into_iter().enumerate() {
                if vm.call_value(f.clone(), vec![x.clone(), Value::Num(i as f64)])?.truthy() {
                    out.push(x);
                }
            }
            Ok(Value::array(out))
        },
        "forEach" => |vm, a| {
            let items = this_arr(&a).borrow().clone();
            let f = arg(&a, 1);
            for (i, x) in items.into_iter().enumerate() {
                vm.call_value(f.clone(), vec![x, Value::Num(i as f64)])?;
            }
            Ok(Value::Null)
        },
        "reduce" => |vm, a| {
            let items = this_arr(&a).borrow().clone();
            let f = arg(&a, 1);
            let mut acc = arg(&a, 2);
            for (i, x) in items.into_iter().enumerate() {
                acc = vm.call_value(f.clone(), vec![acc, x, Value::Num(i as f64)])?;
            }
            Ok(acc)
        },
        "find" => |vm, a| {
            let items = this_arr(&a).borrow().clone();
            let f = arg(&a, 1);
            for (i, x) in items.into_iter().enumerate() {
                if vm.call_value(f.clone(), vec![x.clone(), Value::Num(i as f64)])?.truthy() {
                    return Ok(x);
                }
            }
            Ok(Value::Null)
        },
        "findIndex" => |vm, a| {
            let items = this_arr(&a).borrow().clone();
            let f = arg(&a, 1);
            for (i, x) in items.into_iter().enumerate() {
                if vm.call_value(f.clone(), vec![x, Value::Num(i as f64)])?.truthy() {
                    return Ok(Value::Num(i as f64));
                }
            }
            Ok(Value::Num(-1.0))
        },
        "some" => |vm, a| {
            let items = this_arr(&a).borrow().clone();
            let f = arg(&a, 1);
            for (i, x) in items.into_iter().enumerate() {
                if vm.call_value(f.clone(), vec![x, Value::Num(i as f64)])?.truthy() {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        },
        "every" => |vm, a| {
            let items = this_arr(&a).borrow().clone();
            let f = arg(&a, 1);
            for (i, x) in items.into_iter().enumerate() {
                if !vm.call_value(f.clone(), vec![x, Value::Num(i as f64)])?.truthy() {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        },
        "includes" => |_, a| {
            let t = arg(&a, 1);
            Ok(Value::Bool(this_arr(&a).borrow().iter().any(|x| strict_eq(x, &t))))
        },
        "indexOf" => |_, a| {
            let t = arg(&a, 1);
            Ok(Value::Num(this_arr(&a).borrow().iter().position(|x| strict_eq(x, &t)).map(|i| i as f64).unwrap_or(-1.0)))
        },
        "lastIndexOf" => |_, a| {
            let t = arg(&a, 1);
            Ok(Value::Num(this_arr(&a).borrow().iter().rposition(|x| strict_eq(x, &t)).map(|i| i as f64).unwrap_or(-1.0)))
        },
        "join" => |_, a| {
            let sep = if let Value::Str(s) = arg(&a, 1) { s.to_string() } else { ",".into() };
            let b = this_arr(&a);
            let parts: Vec<String> = b.borrow().iter().map(|x| if matches!(x, Value::Null) { String::new() } else { to_display(x) }).collect();
            Ok(Value::string(parts.join(&sep)))
        },
        "reverse" => |_, a| {
            let arr = this_arr(&a);
            arr.borrow_mut().reverse();
            Ok(Value::Array(arr))
        },
        "sort" => |vm, a| {
            let arr = this_arr(&a);
            let items = arr.borrow().clone();
            let cmp = match arg(&a, 1) {
                Value::Null => None,
                f => Some(f),
            };
            let sorted = merge_sort(vm, items, &cmp)?;
            *arr.borrow_mut() = sorted;
            Ok(Value::Array(arr))
        },
        "fill" => |_, a| {
            let arr = this_arr(&a);
            let v = arg(&a, 1);
            for x in arr.borrow_mut().iter_mut() {
                *x = v.clone();
            }
            Ok(Value::Array(arr))
        },
        "at" => |_, a| {
            let arr = this_arr(&a);
            let b = arr.borrow();
            let i = n_arg(&a, 1, "at")?.trunc();
            let i = if i < 0.0 { b.len() as f64 + i } else { i };
            Ok(if i >= 0.0 && (i as usize) < b.len() { b[i as usize].clone() } else { Value::Null })
        },
        _ => return None,
    })
}

fn default_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
        _ => to_display(a).cmp(&to_display(b)),
    }
}

fn merge_sort(vm: &mut Vm, v: Vec<Value>, cmp: &Option<Value>) -> Result<Vec<Value>, Exc> {
    if v.len() <= 1 {
        return Ok(v);
    }
    let mid = v.len() / 2;
    let right = v[mid..].to_vec();
    let left = v[..mid].to_vec();
    let l = merge_sort(vm, left, cmp)?;
    let r = merge_sort(vm, right, cmp)?;
    let mut out = Vec::with_capacity(l.len() + r.len());
    let (mut i, mut j) = (0, 0);
    while i < l.len() && j < r.len() {
        let gt = match cmp {
            Some(f) => match vm.call_value(f.clone(), vec![l[i].clone(), r[j].clone()])? {
                Value::Num(n) => n > 0.0,
                _ => false,
            },
            None => default_cmp(&l[i], &r[j]) == std::cmp::Ordering::Greater,
        };
        if gt {
            out.push(r[j].clone());
            j += 1;
        } else {
            out.push(l[i].clone());
            i += 1;
        }
    }
    out.extend_from_slice(&l[i..]);
    out.extend_from_slice(&r[j..]);
    Ok(out)
}

// =================================================================== strings

pub fn string_method(name: &str) -> Option<NativeImpl> {
    Some(match name {
        "charAt" => |_, a| Ok(Value::string(str_char_at(&this_str(&a), n_arg(&a, 1, "charAt")?.max(0.0) as usize).unwrap_or_default())),
        "charCodeAt" => |_, a| {
            let s = this_str(&a);
            Ok(Value::Num(s.chars().nth(n_arg(&a, 1, "charCodeAt")?.max(0.0) as usize).map(|c| c as u32 as f64).unwrap_or(f64::NAN)))
        },
        "indexOf" => |_, a| {
            let s = this_str(&a);
            let t = s_arg(&a, 1, "indexOf")?;
            let from = char_to_byte_idx(&s, opt_n(&a, 2).unwrap_or(0.0).max(0.0) as usize);
            Ok(Value::Num(s[from..].find(&*t).map(|b| byte_to_char_idx(&s, from + b) as f64).unwrap_or(-1.0)))
        },
        "lastIndexOf" => |_, a| {
            let s = this_str(&a);
            let t = s_arg(&a, 1, "lastIndexOf")?;
            Ok(Value::Num(s.rfind(&*t).map(|b| byte_to_char_idx(&s, b) as f64).unwrap_or(-1.0)))
        },
        "includes" => |_, a| Ok(Value::Bool(this_str(&a).contains(&*s_arg(&a, 1, "includes")?))),
        "startsWith" => |_, a| Ok(Value::Bool(this_str(&a).starts_with(&*s_arg(&a, 1, "startsWith")?))),
        "endsWith" => |_, a| Ok(Value::Bool(this_str(&a).ends_with(&*s_arg(&a, 1, "endsWith")?))),
        "slice" => |_, a| {
            let s = this_str(&a);
            let n = str_len(&s);
            let st = clamp_rel(opt_n(&a, 1).unwrap_or(0.0), n);
            let e = clamp_rel(opt_n(&a, 2).unwrap_or(n as f64), n);
            Ok(Value::string(char_slice(&s, st, e)))
        },
        "substring" => |_, a| {
            let s = this_str(&a);
            let n = str_len(&s) as f64;
            let mut x = n_arg(&a, 1, "substring")?.max(0.0).min(n) as usize;
            let mut y = opt_n(&a, 2).unwrap_or(n).max(0.0).min(n) as usize;
            if x > y {
                std::mem::swap(&mut x, &mut y);
            }
            Ok(Value::string(char_slice(&s, x, y)))
        },
        "toUpperCase" => |_, a| Ok(Value::string(this_str(&a).to_uppercase())),
        "toLowerCase" => |_, a| Ok(Value::string(this_str(&a).to_lowercase())),
        "trim" => |_, a| Ok(Value::str(this_str(&a).trim())),
        "trimStart" => |_, a| Ok(Value::str(this_str(&a).trim_start())),
        "trimEnd" => |_, a| Ok(Value::str(this_str(&a).trim_end())),
        "split" => |_, a| {
            let s = this_str(&a);
            let sep = s_arg(&a, 1, "split")?;
            let limit = opt_n(&a, 2).map(|n| n.max(0.0) as usize).unwrap_or(usize::MAX);
            let parts: Vec<Value> = if sep.is_empty() { s.chars().map(|c| Value::string(c.to_string())).take(limit).collect() } else { s.split(&*sep).map(Value::str).take(limit).collect() };
            Ok(Value::array(parts))
        },
        "replace" => |_, a| Ok(Value::string(this_str(&a).replacen(&*s_arg(&a, 1, "replace")?, &s_arg(&a, 2, "replace")?, 1))),
        "replaceAll" => |_, a| {
            let search = s_arg(&a, 1, "replaceAll")?;
            let rep = s_arg(&a, 2, "replaceAll")?;
            let s = this_str(&a);
            if search.is_empty() {
                return Ok(Value::Str(s));
            }
            Ok(Value::string(s.replace(&*search, &rep)))
        },
        "repeat" => |_, a| {
            let s = this_str(&a);
            let n = n_arg(&a, 1, "repeat")?;
            if n < 0.0 || !n.is_finite() || (s.len() as f64) * n > crate::vm::MAX_STRING_LEN as f64 {
                return throw("RangeError", "invalid repeat count");
            }
            Ok(Value::string(s.repeat(n as usize)))
        },
        "padStart" | "padEnd" => return Some(if name == "padStart" { |_, a| pad(a, true) } else { |_, a| pad(a, false) }),
        "at" => |_, a| {
            let s = this_str(&a);
            let n = str_len(&s) as f64;
            let i = n_arg(&a, 1, "at")?.trunc();
            let i = if i < 0.0 { n + i } else { i };
            Ok(if i >= 0.0 && i < n { Value::string(str_char_at(&s, i as usize).unwrap_or_default()) } else { Value::Null })
        },
        "concat" => |_, a| {
            let mut s = this_str(&a).to_string();
            for v in a.iter().skip(1) {
                s.push_str(&to_display(v));
            }
            Ok(Value::string(s))
        },
        "localeCompare" => |_, a| {
            let (x, y) = (this_str(&a), s_arg(&a, 1, "localeCompare")?);
            Ok(Value::Num(match x.to_lowercase().cmp(&y.to_lowercase()).then(y.cmp(&x)) {
                std::cmp::Ordering::Less => -1.0,
                std::cmp::Ordering::Equal => 0.0,
                std::cmp::Ordering::Greater => 1.0,
            }))
        },
        "toString" => |_, a| Ok(Value::Str(this_str(&a))),
        _ => return None,
    })
}

fn pad(a: Vec<Value>, start: bool) -> NRes {
    let s = this_str(&a);
    let n = n_arg(&a, 1, "pad")?.max(0.0) as usize;
    let fill = if let Value::Str(f) = arg(&a, 2) { f.to_string() } else { " ".into() };
    let len = str_len(&s);
    if n <= len || fill.is_empty() {
        return Ok(Value::Str(s));
    }
    if n > crate::vm::MAX_STRING_LEN {
        return throw("RangeError", "string is too long");
    }
    let padding: String = fill.chars().cycle().take(n - len).collect();
    Ok(Value::string(if start { format!("{}{}", padding, s) } else { format!("{}{}", s, padding) }))
}

// =================================================================== numbers

fn to_fixed(n: f64, d: usize) -> String {
    if !n.is_finite() {
        return fmt_num(n);
    }
    if n.abs() >= 1e21 {
        return fmt_num(n);
    }
    let neg = n < 0.0;
    let long = format!("{:.*}", d + 30, n.abs());
    let (ip, fp) = long.split_once('.').unwrap_or((&long, ""));
    let mut digits: Vec<u8> = ip.bytes().chain(fp.bytes().take(d)).collect();
    let next = fp.as_bytes().get(d).copied().unwrap_or(b'0');
    if next >= b'5' {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let int_len = digits.len() - d;
    let mut out = String::new();
    out.push_str(std::str::from_utf8(&digits[..int_len]).unwrap());
    if d > 0 {
        out.push('.');
        out.push_str(std::str::from_utf8(&digits[int_len..]).unwrap());
    }
    if neg && out.bytes().any(|b| b != b'0' && b != b'.') {
        out.insert(0, '-');
    }
    out
}

pub fn number_method(name: &str) -> Option<NativeImpl> {
    Some(match name {
        "toFixed" => |_, a| {
            let n = n_arg(&a, 0, "toFixed")?;
            let d = opt_n(&a, 1).unwrap_or(0.0);
            if !(0.0..=100.0).contains(&d) {
                return throw("RangeError", "toFixed() digits must be between 0 and 100");
            }
            Ok(Value::string(to_fixed(n, d as usize)))
        },
        "toString" => |_, a| {
            let n = n_arg(&a, 0, "toString")?;
            let radix = opt_n(&a, 1).unwrap_or(10.0);
            if radix == 10.0 || !n.is_finite() {
                return Ok(Value::string(fmt_num(n)));
            }
            if !(2.0..=36.0).contains(&radix) || radix.fract() != 0.0 {
                return throw("RangeError", "toString() radix must be an integer between 2 and 36");
            }
            let r = radix as u64;
            let neg = n < 0.0;
            let mut v = n.abs().trunc() as u64;
            if v == 0 {
                return Ok(Value::str("0"));
            }
            let mut ds = vec![];
            while v > 0 {
                ds.push(std::char::from_digit((v % r) as u32, r as u32).unwrap());
                v /= r;
            }
            if neg {
                ds.push('-');
            }
            ds.reverse();
            Ok(Value::string(ds.into_iter().collect()))
        },
        _ => return None,
    })
}

pub fn bool_method(name: &str) -> Option<NativeImpl> {
    match name {
        "toString" => Some(|_, a| Ok(Value::string(to_display(&arg(&a, 0))))),
        _ => None,
    }
}
