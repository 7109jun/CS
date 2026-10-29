use crate::bytecode::*;
use crate::natives;
use crate::perms::Perms;
use crate::value::*;
use indexmap::IndexMap;
use serde_json::Value as Json;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub const MAX_STRING_LEN: usize = 64 * 1024 * 1024;
pub const MAX_ARRAY_LEN: usize = 20_000_000;
const MAX_STACK: usize = 4_000_000;

pub struct RunCtx {
    pub perms: Perms,
    pub deadline: Option<Instant>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub max_call_depth: usize,
    /// When true, console output goes to stderr (stdout is reserved for the MCP protocol).
    pub console_to_stderr: bool,
    pub notifications: Vec<Json>,
    pub progress_token: Option<Json>,
    pub log_level: u8,
}

impl RunCtx {
    pub fn new(perms: Perms) -> RunCtx {
        RunCtx { perms, deadline: None, cancel: None, max_call_depth: 400, console_to_stderr: false, notifications: vec![], progress_token: None, log_level: 1 }
    }
}

struct Frame {
    closure: Rc<Closure>,
    ip: usize,
    base: usize,
    cells: Vec<Cell>,
}

struct Handler {
    catch_ip: usize,
    stack_len: usize,
    frame: usize,
}

pub struct Vm {
    funcs: Vec<Rc<RtFunc>>,
    pub globals: Vec<Value>,
    stack: Vec<Value>,
    frames: Vec<Frame>,
    handlers: Vec<Handler>,
    pub ctx: RunCtx,
    ticks: u32,
    inits: Vec<u32>,
}

enum CallOutcome {
    Pushed,
    Done,
}

enum Unwind {
    Resume,
    Done(NRes),
}

fn to_i32(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    let m = n.trunc() % 4294967296.0;
    (m as i64) as i32 as i64 as i32 // wraps through i64 then truncates to 32 bits
}

fn to_i32_wrap(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    let t = n.trunc();
    let m = t.rem_euclid(4294967296.0);
    (m as u64 as u32) as i32
}

impl Vm {
    pub fn new(program: &Program, ctx: RunCtx) -> Result<Vm, String> {
        let funcs: Vec<Rc<RtFunc>> = program
            .funcs
            .iter()
            .map(|p| {
                let consts = p
                    .consts
                    .iter()
                    .map(|c| match c {
                        Const::Num(n) => Value::Num(*n),
                        Const::Str(s) => Value::str(s),
                    })
                    .collect();
                Rc::new(RtFunc { proto: p.clone(), consts })
            })
            .collect();
        let mut globals = Vec::with_capacity(program.globals.len());
        for g in &program.globals {
            match &g.native {
                Some(n) => match natives::lookup(n) {
                    Some(v) => globals.push(v),
                    None => return Err(format!("native '{}' is declared but not implemented", n)),
                },
                None => globals.push(Value::Null),
            }
        }
        let _ = to_i32;
        Ok(Vm { funcs, globals, stack: Vec::with_capacity(1024), frames: Vec::new(), handlers: Vec::new(), ctx, ticks: 0, inits: program.inits.clone() })
    }

    pub fn run_inits(&mut self) -> Result<(), Exc> {
        for i in self.inits.clone() {
            let f = self.funcs[i as usize].clone();
            let c = Value::Closure(Rc::new(Closure { func: f, cells: vec![] }));
            self.call_value(c, vec![])?;
        }
        Ok(())
    }

    // ---------------- errors ----------------
    pub fn current_line(&self) -> Option<(String, u32, String)> {
        let fr = self.frames.last()?;
        let p = &fr.closure.func.proto;
        let ip = fr.ip.min(p.lines.len().saturating_sub(1));
        Some((p.module.clone(), *p.lines.get(ip).unwrap_or(&0), p.name.clone()))
    }

    pub fn trace(&self) -> String {
        let mut out = String::new();
        for fr in self.frames.iter().rev().take(8) {
            let p = &fr.closure.func.proto;
            let ip = fr.ip.min(p.lines.len().saturating_sub(1));
            out.push_str(&format!("    at {} ({}:{})\n", p.name, p.module, p.lines.get(ip).unwrap_or(&0)));
        }
        out
    }

    pub fn err(&self, name: &str, msg: impl AsRef<str>) -> Exc {
        let e = make_error(name, msg.as_ref());
        if let Value::Object(o) = &e {
            o.borrow_mut().insert(Rc::from("stack"), Value::string(format!("{}: {}\n{}", name, msg.as_ref(), self.trace())));
        }
        Exc::Throw(e)
    }

    pub fn type_error<T>(&self, msg: impl AsRef<str>) -> Result<T, Exc> {
        Err(self.err("TypeError", msg))
    }

    pub fn range_error<T>(&self, msg: impl AsRef<str>) -> Result<T, Exc> {
        Err(self.err("RangeError", msg))
    }

    pub fn check_limits(&mut self) -> Result<(), Exc> {
        if let Some(c) = &self.ctx.cancel {
            if c.load(Ordering::Relaxed) {
                return Err(Exc::Fatal("request cancelled".into()));
            }
        }
        if let Some(d) = self.ctx.deadline {
            if Instant::now() >= d {
                return Err(Exc::Fatal("execution time limit exceeded".into()));
            }
        }
        Ok(())
    }

    pub fn truncate_to(&mut self, frames: usize, stack: usize) {
        self.frames.truncate(frames);
        self.stack.truncate(stack);
        while self.handlers.last().map_or(false, |h| h.frame >= frames) {
            self.handlers.pop();
        }
    }

    // ---------------- public calling API ----------------
    pub fn call_value(&mut self, f: Value, args: Vec<Value>) -> NRes {
        let stop = self.frames.len();
        let stack_len = self.stack.len();
        self.stack.push(f);
        let argc = args.len();
        self.stack.extend(args);
        let r = match self.do_call(argc) {
            Ok(CallOutcome::Pushed) => self.execute(stop),
            Ok(CallOutcome::Done) => Ok(self.stack.pop().unwrap_or(Value::Null)),
            Err(e) => Err(e),
        };
        if let Err(Exc::Fatal(_)) = &r {
            self.truncate_to(stop, stack_len);
        }
        r
    }

    pub fn call_func_index(&mut self, idx: u32) -> NRes {
        let f = self.funcs[idx as usize].clone();
        self.call_value(Value::Closure(Rc::new(Closure { func: f, cells: vec![] })), vec![])
    }

    pub fn call_global(&mut self, slot: u32, args: Vec<Value>) -> NRes {
        let f = self.globals[slot as usize].clone();
        self.call_value(f, args)
    }

    /// Resolve a promise value to its result (await semantics).
    pub fn await_value(&self, v: Value) -> NRes {
        match v {
            Value::Promise(p) => match &*p.borrow() {
                PromiseState::Resolved(v) => Ok(v.clone()),
                PromiseState::Rejected(e) => Err(Exc::Throw(e.clone())),
            },
            other => Ok(other),
        }
    }

    // ---------------- call machinery ----------------
    fn do_call(&mut self, argc: usize) -> Result<CallOutcome, Exc> {
        let fi = self.stack.len() - argc - 1;
        let callee = self.stack[fi].clone();
        match callee {
            Value::Closure(c) => {
                if self.frames.len() >= self.ctx.max_call_depth {
                    self.stack.truncate(fi);
                    return self.range_error("maximum call depth exceeded");
                }
                if self.stack.len() > MAX_STACK {
                    self.stack.truncate(fi);
                    return self.range_error("stack overflow");
                }
                let p = &c.func.proto;
                let np = p.n_params as usize;
                let base = fi + 1;
                if p.has_rest {
                    let fixed = np - 1;
                    let rest: Vec<Value> = if argc > fixed { self.stack.split_off(base + fixed) } else { vec![] };
                    while self.stack.len() < base + fixed {
                        self.stack.push(Value::Null);
                    }
                    self.stack.push(Value::array(rest));
                } else {
                    if argc > np {
                        self.stack.truncate(base + np);
                    }
                    while self.stack.len() < base + np {
                        self.stack.push(Value::Null);
                    }
                }
                let slots = p.n_slots as usize;
                while self.stack.len() < base + slots {
                    self.stack.push(Value::Null);
                }
                let cells: Vec<Cell> = (0..p.n_cells).map(|_| Rc::new(RefCell::new(Value::Null))).collect();
                if let Some(top) = self.frames.last_mut() {
                    let _ = top;
                }
                self.frames.push(Frame { closure: c, ip: 0, base, cells });
                Ok(CallOutcome::Pushed)
            }
            Value::Native(n) => {
                let mut args = self.stack.split_off(fi + 1);
                self.stack.pop();
                if let Some(this) = &n.this {
                    args.insert(0, this.clone());
                }
                let r = (n.f)(self, args)?;
                self.stack.push(r);
                Ok(CallOutcome::Done)
            }
            other => {
                self.stack.truncate(fi);
                self.type_error(format!("{} is not a function", describe_for_error(&other)))
            }
        }
    }

    fn unwind(&mut self, exc: Value, stop: usize) -> Unwind {
        let mut exc = exc;
        loop {
            let fi = self.frames.len() - 1;
            if let Some(h) = self.handlers.last() {
                if h.frame == fi {
                    let h = self.handlers.pop().unwrap();
                    self.stack.truncate(h.stack_len);
                    self.stack.push(exc);
                    self.frames[fi].ip = h.catch_ip;
                    return Unwind::Resume;
                }
            }
            let fr = self.frames.pop().unwrap();
            self.stack.truncate(fr.base.saturating_sub(1));
            if fr.closure.func.proto.is_async {
                let p = Value::rejected(exc);
                if self.frames.len() == stop {
                    return Unwind::Done(Ok(p));
                }
                self.stack.push(p);
                return Unwind::Resume;
            }
            if self.frames.len() <= stop {
                return Unwind::Done(Err(Exc::Throw(exc)));
            }
            exc = exc;
        }
    }

    // ---------------- the interpreter loop ----------------
    fn execute(&mut self, stop: usize) -> NRes {
        'outer: loop {
            let fi = self.frames.len() - 1;
            let func = self.frames[fi].closure.func.clone();
            let mut ip = self.frames[fi].ip;
            let base = self.frames[fi].base;
            let code = &func.proto.code;

            macro_rules! tri {
                ($e:expr) => {
                    match $e {
                        Ok(v) => v,
                        Err(Exc::Throw(v)) => {
                            self.frames[fi].ip = ip;
                            match self.unwind(v, stop) {
                                Unwind::Resume => continue 'outer,
                                Unwind::Done(r) => return r,
                            }
                        }
                        Err(Exc::Fatal(m)) => return Err(Exc::Fatal(m)),
                    }
                };
            }
            macro_rules! pop {
                () => {
                    self.stack.pop().unwrap_or(Value::Null)
                };
            }
            macro_rules! throw_err {
                ($name:expr, $msg:expr) => {{
                    self.frames[fi].ip = ip;
                    let e = self.err($name, $msg);
                    if let Exc::Throw(v) = e {
                        match self.unwind(v, stop) {
                            Unwind::Resume => continue 'outer,
                            Unwind::Done(r) => return r,
                        }
                    }
                    unreachable!()
                }};
            }

            loop {
                let op = code[ip];
                ip += 1;
                self.ticks = self.ticks.wrapping_add(1);
                if self.ticks & 0x3ff == 0 {
                    self.frames[fi].ip = ip;
                    tri!(self.check_limits());
                }
                match op {
                    Op::Nop => {}
                    Op::Const(i) => self.stack.push(func.consts[i as usize].clone()),
                    Op::Null => self.stack.push(Value::Null),
                    Op::True => self.stack.push(Value::Bool(true)),
                    Op::False => self.stack.push(Value::Bool(false)),
                    Op::Pop => {
                        self.stack.pop();
                    }
                    Op::Dup => {
                        let v = self.stack.last().cloned().unwrap_or(Value::Null);
                        self.stack.push(v);
                    }
                    Op::Dup2 => {
                        let n = self.stack.len();
                        let a = self.stack[n - 2].clone();
                        let b = self.stack[n - 1].clone();
                        self.stack.push(a);
                        self.stack.push(b);
                    }
                    Op::Swap => {
                        let n = self.stack.len();
                        self.stack.swap(n - 1, n - 2);
                    }
                    Op::Rot3 => {
                        let c = pop!();
                        let n = self.stack.len();
                        self.stack.insert(n - 2, c);
                    }
                    Op::GetLocal(s) => {
                        let v = self.stack[base + s as usize].clone();
                        self.stack.push(v);
                    }
                    Op::SetLocal(s) => {
                        let v = pop!();
                        self.stack[base + s as usize] = v;
                    }
                    Op::NewCell(c) => {
                        let v = pop!();
                        self.frames[fi].cells[c as usize] = Rc::new(RefCell::new(v));
                    }
                    Op::GetCell(c) => {
                        let v = self.frames[fi].cells[c as usize].borrow().clone();
                        self.stack.push(v);
                    }
                    Op::SetCell(c) => {
                        let v = pop!();
                        *self.frames[fi].cells[c as usize].borrow_mut() = v;
                    }
                    Op::RefreshCell(c) => {
                        let v = self.frames[fi].cells[c as usize].borrow().clone();
                        self.frames[fi].cells[c as usize] = Rc::new(RefCell::new(v));
                    }
                    Op::GetUpval(u) => {
                        let v = self.frames[fi].closure.cells[u as usize].borrow().clone();
                        self.stack.push(v);
                    }
                    Op::SetUpval(u) => {
                        let v = pop!();
                        *self.frames[fi].closure.cells[u as usize].borrow_mut() = v;
                    }
                    Op::GetGlobal(g) => {
                        let v = self.globals[g as usize].clone();
                        self.stack.push(v);
                    }
                    Op::SetGlobal(g) => {
                        let v = pop!();
                        self.globals[g as usize] = v;
                    }
                    Op::Add => {
                        let b = pop!();
                        let a = pop!();
                        match (&a, &b) {
                            (Value::Num(x), Value::Num(y)) => self.stack.push(Value::Num(x + y)),
                            (Value::Str(_), _) | (_, Value::Str(_)) => {
                                let (x, y) = (to_display(&a), to_display(&b));
                                if x.len() + y.len() > MAX_STRING_LEN {
                                    throw_err!("RangeError", "string is too long");
                                }
                                let mut s = x;
                                s.push_str(&y);
                                self.stack.push(Value::string(s));
                            }
                            _ => throw_err!("TypeError", format!("cannot add {} and {}", a.type_name(), b.type_name())),
                        }
                    }
                    Op::Sub | Op::Mul | Op::Div | Op::Mod | Op::Pow | Op::BitAnd | Op::BitOr | Op::BitXor | Op::Shl | Op::Shr => {
                        let b = pop!();
                        let a = pop!();
                        let (Value::Num(x), Value::Num(y)) = (&a, &b) else {
                            throw_err!("TypeError", format!("arithmetic requires numbers, got {} and {}", a.type_name(), b.type_name()));
                        };
                        let (x, y) = (*x, *y);
                        let r = match op {
                            Op::Sub => x - y,
                            Op::Mul => x * y,
                            Op::Div => x / y,
                            Op::Mod => x % y,
                            Op::Pow => x.powf(y),
                            Op::BitAnd => (to_i32_wrap(x) & to_i32_wrap(y)) as f64,
                            Op::BitOr => (to_i32_wrap(x) | to_i32_wrap(y)) as f64,
                            Op::BitXor => (to_i32_wrap(x) ^ to_i32_wrap(y)) as f64,
                            Op::Shl => to_i32_wrap(x).wrapping_shl((to_i32_wrap(y) & 31) as u32) as f64,
                            _ => (to_i32_wrap(x) >> (to_i32_wrap(y) & 31)) as f64,
                        };
                        self.stack.push(Value::Num(r));
                    }
                    Op::Neg => {
                        let a = pop!();
                        match a {
                            Value::Num(n) => self.stack.push(Value::Num(-n)),
                            other => throw_err!("TypeError", format!("cannot negate {}", other.type_name())),
                        }
                    }
                    Op::Plus => {
                        let a = pop!();
                        match a {
                            Value::Num(_) => self.stack.push(a),
                            other => throw_err!("TypeError", format!("expected a number, got {}", other.type_name())),
                        }
                    }
                    Op::Not => {
                        let a = pop!();
                        self.stack.push(Value::Bool(!a.truthy()));
                    }
                    Op::BitNot => {
                        let a = pop!();
                        match a {
                            Value::Num(n) => self.stack.push(Value::Num(!to_i32_wrap(n) as f64)),
                            other => throw_err!("TypeError", format!("cannot apply '~' to {}", other.type_name())),
                        }
                    }
                    Op::Eq => {
                        let b = pop!();
                        let a = pop!();
                        self.stack.push(Value::Bool(strict_eq(&a, &b)));
                    }
                    Op::Ne => {
                        let b = pop!();
                        let a = pop!();
                        self.stack.push(Value::Bool(!strict_eq(&a, &b)));
                    }
                    Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                        let b = pop!();
                        let a = pop!();
                        let r = match (&a, &b) {
                            (Value::Num(x), Value::Num(y)) => match op {
                                Op::Lt => x < y,
                                Op::Le => x <= y,
                                Op::Gt => x > y,
                                _ => x >= y,
                            },
                            (Value::Str(x), Value::Str(y)) => match op {
                                Op::Lt => x < y,
                                Op::Le => x <= y,
                                Op::Gt => x > y,
                                _ => x >= y,
                            },
                            _ => throw_err!("TypeError", format!("cannot compare {} and {}", a.type_name(), b.type_name())),
                        };
                        self.stack.push(Value::Bool(r));
                    }
                    Op::TypeOf => {
                        let a = pop!();
                        let s = match &a {
                            Value::Promise(_) => "object",
                            other => other.type_name(),
                        };
                        self.stack.push(Value::str(s));
                    }
                    Op::Jump(t) => ip = t as usize,
                    Op::JumpIfFalse(t) => {
                        let v = pop!();
                        if !v.truthy() {
                            ip = t as usize;
                        }
                    }
                    Op::JumpIfTrue(t) => {
                        let v = pop!();
                        if v.truthy() {
                            ip = t as usize;
                        }
                    }
                    Op::AndJump(t) => {
                        if !self.stack.last().map_or(false, |v| v.truthy()) {
                            ip = t as usize;
                        } else {
                            self.stack.pop();
                        }
                    }
                    Op::OrJump(t) => {
                        if self.stack.last().map_or(false, |v| v.truthy()) {
                            ip = t as usize;
                        } else {
                            self.stack.pop();
                        }
                    }
                    Op::NullishJump(t) => {
                        if !matches!(self.stack.last(), Some(Value::Null) | None) {
                            ip = t as usize;
                        } else {
                            self.stack.pop();
                        }
                    }
                    Op::JumpIfNullKeep(t) => {
                        if matches!(self.stack.last(), Some(Value::Null) | None) {
                            ip = t as usize;
                        }
                    }
                    Op::Call(argc) => {
                        self.frames[fi].ip = ip;
                        match tri!(self.do_call(argc as usize)) {
                            CallOutcome::Pushed => continue 'outer,
                            CallOutcome::Done => {}
                        }
                    }
                    Op::CallSpread => {
                        self.frames[fi].ip = ip;
                        let args = pop!();
                        let Value::Array(a) = args else { unreachable!() };
                        let items: Vec<Value> = a.borrow().clone();
                        let argc = items.len();
                        self.stack.extend(items);
                        match tri!(self.do_call(argc)) {
                            CallOutcome::Pushed => continue 'outer,
                            CallOutcome::Done => {}
                        }
                    }
                    Op::CallMethod(k, argc) => {
                        self.frames[fi].ip = ip;
                        let name = match &func.consts[k as usize] {
                            Value::Str(s) => s.clone(),
                            _ => unreachable!(),
                        };
                        match tri!(self.call_method(&name, argc as usize)) {
                            CallOutcome::Pushed => continue 'outer,
                            CallOutcome::Done => {}
                        }
                    }
                    Op::CallMethodSpread(k) => {
                        self.frames[fi].ip = ip;
                        let name = match &func.consts[k as usize] {
                            Value::Str(s) => s.clone(),
                            _ => unreachable!(),
                        };
                        let args = pop!();
                        let Value::Array(a) = args else { unreachable!() };
                        let items: Vec<Value> = a.borrow().clone();
                        let argc = items.len();
                        self.stack.extend(items);
                        match tri!(self.call_method(&name, argc)) {
                            CallOutcome::Pushed => continue 'outer,
                            CallOutcome::Done => {}
                        }
                    }
                    Op::Return => {
                        let ret = pop!();
                        let fr = self.frames.pop().unwrap();
                        self.stack.truncate(fr.base.saturating_sub(1));
                        while self.handlers.last().map_or(false, |h| h.frame >= self.frames.len()) {
                            self.handlers.pop();
                        }
                        let ret = if fr.closure.func.proto.is_async {
                            match ret {
                                Value::Promise(_) => ret,
                                other => Value::resolved(other),
                            }
                        } else {
                            ret
                        };
                        if self.frames.len() <= stop {
                            return Ok(ret);
                        }
                        self.stack.push(ret);
                        continue 'outer;
                    }
                    Op::Closure(idx) => {
                        let f = self.funcs[idx as usize].clone();
                        let cells: Vec<Cell> = f
                            .proto
                            .upvals
                            .iter()
                            .map(|u| if u.from_cell { self.frames[fi].cells[u.index as usize].clone() } else { self.frames[fi].closure.cells[u.index as usize].clone() })
                            .collect();
                        self.stack.push(Value::Closure(Rc::new(Closure { func: f, cells })));
                    }
                    Op::NewArray(n) => {
                        let at = self.stack.len() - n as usize;
                        let items = self.stack.split_off(at);
                        self.stack.push(Value::array(items));
                    }
                    Op::ArrayPush => {
                        let v = pop!();
                        if let Some(Value::Array(a)) = self.stack.last() {
                            if a.borrow().len() >= MAX_ARRAY_LEN {
                                throw_err!("RangeError", "array is too large");
                            }
                            a.borrow_mut().push(v);
                        }
                    }
                    Op::ArraySpread => {
                        let v = pop!();
                        let items: Vec<Value> = match &v {
                            Value::Array(a) => a.borrow().clone(),
                            Value::Str(s) => s.chars().map(|c| Value::string(c.to_string())).collect(),
                            Value::Null => vec![],
                            other => throw_err!("TypeError", format!("{} is not iterable", other.type_name())),
                        };
                        if let Some(Value::Array(a)) = self.stack.last() {
                            if a.borrow().len() + items.len() > MAX_ARRAY_LEN {
                                throw_err!("RangeError", "array is too large");
                            }
                            a.borrow_mut().extend(items);
                        }
                    }
                    Op::NewObject => self.stack.push(Value::new_object()),
                    Op::InitProp(k) => {
                        let v = pop!();
                        if let (Some(Value::Object(o)), Value::Str(key)) = (self.stack.last(), &func.consts[k as usize]) {
                            o.borrow_mut().insert(key.clone(), v);
                        }
                    }
                    Op::ObjSpread => {
                        let v = pop!();
                        match &v {
                            Value::Object(src) => {
                                let entries: Vec<(Str, Value)> = src.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                                if let Some(Value::Object(o)) = self.stack.last() {
                                    let mut m = o.borrow_mut();
                                    for (k, v) in entries {
                                        m.insert(k, v);
                                    }
                                }
                            }
                            Value::Null => {}
                            other => throw_err!("TypeError", format!("cannot spread {} into an object", other.type_name())),
                        }
                    }
                    Op::GetProp(k) => {
                        let obj = pop!();
                        let name = match &func.consts[k as usize] {
                            Value::Str(s) => s.clone(),
                            _ => unreachable!(),
                        };
                        self.frames[fi].ip = ip;
                        let v = tri!(self.get_prop(&obj, &name));
                        self.stack.push(v);
                    }
                    Op::SetProp(k) => {
                        let v = pop!();
                        let obj = pop!();
                        let name = match &func.consts[k as usize] {
                            Value::Str(s) => s.clone(),
                            _ => unreachable!(),
                        };
                        match &obj {
                            Value::Object(o) => {
                                o.borrow_mut().insert(name, v.clone());
                            }
                            other => {
                                self.frames[fi].ip = ip;
                                let m = format!("cannot set property '{}' of {}", name, other.type_name());
                                throw_err!("TypeError", m);
                            }
                        }
                        self.stack.push(v);
                    }
                    Op::GetIndex => {
                        let idx = pop!();
                        let obj = pop!();
                        self.frames[fi].ip = ip;
                        let v = tri!(self.get_index(&obj, &idx));
                        self.stack.push(v);
                    }
                    Op::SetIndex => {
                        let v = pop!();
                        let idx = pop!();
                        let obj = pop!();
                        self.frames[fi].ip = ip;
                        tri!(self.set_index(&obj, &idx, v.clone()));
                        self.stack.push(v);
                    }
                    Op::Len => {
                        let v = pop!();
                        let n = match &v {
                            Value::Array(a) => a.borrow().len(),
                            Value::Str(s) => natives::str_len(s),
                            _ => 0,
                        };
                        self.stack.push(Value::Num(n as f64));
                    }
                    Op::ToIter => {
                        let v = pop!();
                        match &v {
                            Value::Array(_) => self.stack.push(v),
                            Value::Str(s) => {
                                let items: Vec<Value> = s.chars().map(|c| Value::string(c.to_string())).collect();
                                self.stack.push(Value::array(items));
                            }
                            other => throw_err!("TypeError", format!("{} is not iterable", other.type_name())),
                        }
                    }
                    Op::Throw => {
                        let v = pop!();
                        self.frames[fi].ip = ip;
                        // attach a stack trace to plain Error objects that lack one
                        if let Value::Object(o) = &v {
                            let missing = !o.borrow().contains_key("stack");
                            if missing {
                                let name = o.borrow().get("name").map(to_display).unwrap_or_else(|| "Error".into());
                                let msg = o.borrow().get("message").map(to_display).unwrap_or_default();
                                o.borrow_mut().insert(Rc::from("stack"), Value::string(format!("{}: {}\n{}", name, msg, self.trace())));
                            }
                        }
                        match self.unwind(v, stop) {
                            Unwind::Resume => continue 'outer,
                            Unwind::Done(r) => return r,
                        }
                    }
                    Op::TryBegin(t) => {
                        self.handlers.push(Handler { catch_ip: t as usize, stack_len: self.stack.len(), frame: fi });
                    }
                    Op::TryEnd => {
                        self.handlers.pop();
                    }
                    Op::Await => {
                        let v = pop!();
                        self.frames[fi].ip = ip;
                        let r = tri!(self.await_value(v));
                        self.stack.push(r);
                    }
                    Op::Concat(n) => {
                        let at = self.stack.len() - n as usize;
                        let parts = self.stack.split_off(at);
                        let mut s = String::new();
                        for p in &parts {
                            s.push_str(&to_display(p));
                            if s.len() > MAX_STRING_LEN {
                                throw_err!("RangeError", "string is too long");
                            }
                        }
                        self.stack.push(Value::string(s));
                    }
                    Op::NonNull => {
                        if matches!(self.stack.last(), Some(Value::Null)) {
                            throw_err!("TypeError", "non-null assertion failed: value is null");
                        }
                    }
                    Op::Fail(k) => {
                        let msg = to_display(&func.consts[k as usize]);
                        throw_err!("TypeError", msg);
                    }
                    Op::ObjRest(k) => {
                        let v = pop!();
                        let keys = to_display(&func.consts[k as usize]);
                        let excl: Vec<&str> = if keys.is_empty() { vec![] } else { keys.split('\u{0}').collect() };
                        let mut m = ObjMap::new();
                        if let Value::Object(o) = &v {
                            for (key, val) in o.borrow().iter() {
                                if !excl.contains(&&**key) {
                                    m.insert(key.clone(), val.clone());
                                }
                            }
                        }
                        self.stack.push(Value::object(m));
                    }
                    Op::ArrRest(n) => {
                        let v = pop!();
                        let items: Vec<Value> = match &v {
                            Value::Array(a) => a.borrow().iter().skip(n as usize).cloned().collect(),
                            _ => vec![],
                        };
                        self.stack.push(Value::array(items));
                    }
                }
            }
        }
    }

    // ---------------- property access ----------------
    pub fn get_prop(&mut self, obj: &Value, name: &str) -> NRes {
        match obj {
            Value::Object(o) => Ok(o.borrow().get(name).cloned().unwrap_or(Value::Null)),
            Value::Array(a) => {
                if name == "length" {
                    return Ok(Value::Num(a.borrow().len() as f64));
                }
                match natives::array_method(name) {
                    Some(f) => Ok(Value::Native(Rc::new(NativeFn { name: Rc::from(format!("Array.{}", name)), f, this: Some(obj.clone()) }))),
                    None => Ok(Value::Null),
                }
            }
            Value::Str(s) => {
                if name == "length" {
                    return Ok(Value::Num(natives::str_len(s) as f64));
                }
                match natives::string_method(name) {
                    Some(f) => Ok(Value::Native(Rc::new(NativeFn { name: Rc::from(format!("String.{}", name)), f, this: Some(obj.clone()) }))),
                    None => Ok(Value::Null),
                }
            }
            Value::Num(_) => match natives::number_method(name) {
                Some(f) => Ok(Value::Native(Rc::new(NativeFn { name: Rc::from(format!("Number.{}", name)), f, this: Some(obj.clone()) }))),
                None => Ok(Value::Null),
            },
            Value::Bool(_) => match natives::bool_method(name) {
                Some(f) => Ok(Value::Native(Rc::new(NativeFn { name: Rc::from(format!("Boolean.{}", name)), f, this: Some(obj.clone()) }))),
                None => Ok(Value::Null),
            },
            Value::Null => self.type_error(format!("cannot read property '{}' of null", name)),
            other => Ok(if name == "name" { Value::string(describe_for_error(other)) } else { Value::Null }),
        }
    }

    fn call_method(&mut self, name: &str, argc: usize) -> Result<CallOutcome, Exc> {
        let ri = self.stack.len() - argc - 1;
        let recv = self.stack[ri].clone();
        let native: Option<NativeImpl> = match &recv {
            Value::Array(_) => natives::array_method(name),
            Value::Str(_) => natives::string_method(name),
            Value::Num(_) => natives::number_method(name),
            Value::Bool(_) => natives::bool_method(name),
            Value::Object(o) => {
                let callee = o.borrow().get(name).cloned().unwrap_or(Value::Null);
                match callee {
                    Value::Closure(_) | Value::Native(_) => {
                        self.stack[ri] = callee;
                        return self.do_call(argc);
                    }
                    _ => {
                        self.stack.truncate(ri);
                        return self.type_error(format!("'{}' is not a function", name));
                    }
                }
            }
            Value::Null => {
                self.stack.truncate(ri);
                return self.type_error(format!("cannot call method '{}' of null", name));
            }
            _ => None,
        };
        match native {
            Some(f) => {
                let args = self.stack.split_off(ri);
                let r = f(self, args)?;
                self.stack.push(r);
                Ok(CallOutcome::Done)
            }
            None => {
                self.stack.truncate(ri);
                self.type_error(format!("'{}' is not a function on {}", name, recv.type_name()))
            }
        }
    }

    fn get_index(&mut self, obj: &Value, idx: &Value) -> NRes {
        match (obj, idx) {
            (Value::Array(a), Value::Num(n)) => {
                let a = a.borrow();
                if *n >= 0.0 && n.fract() == 0.0 && (*n as usize) < a.len() {
                    Ok(a[*n as usize].clone())
                } else {
                    Ok(Value::Null)
                }
            }
            (Value::Str(s), Value::Num(n)) => {
                if *n >= 0.0 && n.fract() == 0.0 {
                    Ok(natives::str_char_at(s, *n as usize).map(Value::string).unwrap_or(Value::Null))
                } else {
                    Ok(Value::Null)
                }
            }
            (Value::Object(o), Value::Str(k)) => Ok(o.borrow().get(&**k).cloned().unwrap_or(Value::Null)),
            (Value::Object(o), Value::Num(n)) => Ok(o.borrow().get(fmt_num(*n).as_str()).cloned().unwrap_or(Value::Null)),
            (Value::Null, _) => self.type_error("cannot index into null"),
            (_, Value::Str(k)) => self.get_prop(obj, k),
            _ => self.type_error(format!("cannot index {} with {}", obj.type_name(), idx.type_name())),
        }
    }

    fn set_index(&mut self, obj: &Value, idx: &Value, v: Value) -> Result<(), Exc> {
        match (obj, idx) {
            (Value::Array(a), Value::Num(n)) => {
                let mut a = a.borrow_mut();
                if *n < 0.0 || n.fract() != 0.0 {
                    return self.range_error(format!("invalid array index {}", fmt_num(*n)));
                }
                let i = *n as usize;
                if i < a.len() {
                    a[i] = v;
                } else if i == a.len() {
                    if a.len() >= MAX_ARRAY_LEN {
                        return self.range_error("array is too large");
                    }
                    a.push(v);
                } else {
                    return self.range_error(format!("array index {} is out of bounds (length {})", i, a.len()));
                }
                Ok(())
            }
            (Value::Object(o), Value::Str(k)) => {
                o.borrow_mut().insert(k.clone(), v);
                Ok(())
            }
            (Value::Object(o), Value::Num(n)) => {
                o.borrow_mut().insert(Rc::from(fmt_num(*n)), v);
                Ok(())
            }
            _ => self.type_error(format!("cannot assign to index of {}", obj.type_name())),
        }
    }

    // ---------------- misc services for natives ----------------
    pub fn push_notification(&mut self, n: Json) {
        if self.ctx.notifications.len() < 1000 {
            self.ctx.notifications.push(n);
        }
    }
}

pub fn describe_for_error(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Closure(c) => format!("function '{}'", c.func.proto.name),
        Value::Native(n) => format!("native '{}'", n.name),
        other => other.type_name().to_string(),
    }
}

pub fn format_error(v: &Value) -> String {
    match v {
        Value::Object(o) => {
            let o = o.borrow();
            if let Some(Value::Str(s)) = o.get("stack") {
                return s.trim_end().to_string();
            }
            let name = o.get("name").map(to_display).unwrap_or_else(|| "Error".into());
            let msg = o.get("message").map(to_display).unwrap_or_default();
            format!("{}: {}", name, msg)
        }
        other => to_display(other),
    }
}

pub fn error_message(v: &Value) -> String {
    match v {
        Value::Object(o) => {
            let o = o.borrow();
            let name = o.get("name").map(to_display).unwrap_or_else(|| "Error".into());
            let msg = o.get("message").map(to_display).unwrap_or_default();
            if name == "Error" {
                msg
            } else {
                format!("{}: {}", name, msg)
            }
        }
        other => to_display(other),
    }
}

#[allow(dead_code)]
pub fn _unused(_: IndexMap<String, String>, _: AtomicBool) {}
