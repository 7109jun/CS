use crate::bytecode::FuncProto;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

pub type Str = Rc<str>;
pub type ObjMap = IndexMap<Str, Value>;
pub type Cell = Rc<RefCell<Value>>;

#[derive(Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(Str),
    Array(Rc<RefCell<Vec<Value>>>),
    Object(Rc<RefCell<ObjMap>>),
    Closure(Rc<Closure>),
    Native(Rc<NativeFn>),
    Promise(Rc<RefCell<PromiseState>>),
}

pub struct RtFunc {
    pub proto: FuncProto,
    pub consts: Vec<Value>,
}

pub struct Closure {
    pub func: Rc<RtFunc>,
    pub cells: Vec<Cell>,
}

pub type NativeImpl = fn(&mut crate::vm::Vm, Vec<Value>) -> NRes;

pub struct NativeFn {
    pub name: Str,
    pub f: NativeImpl,
    /// Bound receiver for builtin methods (`arr.map`).
    pub this: Option<Value>,
}

pub enum PromiseState {
    Resolved(Value),
    Rejected(Value),
}

/// Non-local exit from running code.
pub enum Exc {
    /// A catchable thrown value.
    Throw(Value),
    /// Uncatchable abort: time limit, cancellation, output limit...
    Fatal(String),
}

pub type NRes = Result<Value, Exc>;

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(Rc::from(s))
    }
    pub fn string(s: String) -> Value {
        Value::Str(Rc::from(s))
    }
    pub fn array(v: Vec<Value>) -> Value {
        Value::Array(Rc::new(RefCell::new(v)))
    }
    pub fn object(m: ObjMap) -> Value {
        Value::Object(Rc::new(RefCell::new(m)))
    }
    pub fn new_object() -> Value {
        Value::object(ObjMap::new())
    }
    pub fn resolved(v: Value) -> Value {
        Value::Promise(Rc::new(RefCell::new(PromiseState::Resolved(v))))
    }
    pub fn rejected(v: Value) -> Value {
        Value::Promise(Rc::new(RefCell::new(PromiseState::Rejected(v))))
    }
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
            Value::Closure(_) | Value::Native(_) => "function",
            Value::Promise(_) => "promise",
        }
    }
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            _ => true,
        }
    }
}

pub fn make_error(name: &str, msg: &str) -> Value {
    let mut m = ObjMap::new();
    m.insert(Rc::from("name"), Value::str(name));
    m.insert(Rc::from("message"), Value::str(msg));
    Value::object(m)
}

pub fn throw<T>(name: &str, msg: impl AsRef<str>) -> Result<T, Exc> {
    Err(Exc::Throw(make_error(name, msg.as_ref())))
}

pub fn type_err<T>(msg: impl AsRef<str>) -> Result<T, Exc> {
    throw("TypeError", msg)
}

/// JavaScript-compatible number formatting.
pub fn fmt_num(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if n == 0.0 {
        return "0".into();
    }
    let neg = n < 0.0;
    let a = n.abs();
    let s = format!("{:e}", a);
    let (mant, exp) = s.split_once('e').unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let e: i32 = exp.parse().unwrap();
    let k = digits.len() as i32;
    let nn = e + 1;
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if k <= nn && nn <= 21 {
        out.push_str(&digits);
        for _ in 0..(nn - k) {
            out.push('0');
        }
    } else if 0 < nn && nn <= 21 {
        out.push_str(&digits[..nn as usize]);
        out.push('.');
        out.push_str(&digits[nn as usize..]);
    } else if -6 < nn && nn <= 0 {
        out.push_str("0.");
        for _ in 0..(-nn) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let ex = nn - 1;
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if ex < 0 { '-' } else { '+' });
        out.push_str(&ex.abs().to_string());
    }
    out
}

/// String conversion used by templates and `+` with strings.
pub fn to_display(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => fmt_num(*n),
        Value::Str(s) => s.to_string(),
        Value::Array(a) => a.borrow().iter().map(|x| if matches!(x, Value::Null) { String::new() } else { to_display(x) }).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
        Value::Closure(c) => format!("[function {}]", c.func.proto.name),
        Value::Native(n) => format!("[native {}]", n.name),
        Value::Promise(_) => "[object Promise]".into(),
    }
}

pub fn strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => Rc::ptr_eq(x, y),
        (Value::Object(x), Value::Object(y)) => Rc::ptr_eq(x, y),
        (Value::Closure(x), Value::Closure(y)) => Rc::ptr_eq(x, y),
        (Value::Native(x), Value::Native(y)) => Rc::ptr_eq(x, y),
        (Value::Promise(x), Value::Promise(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

pub fn deep_equal(a: &Value, b: &Value, depth: u32) -> bool {
    if depth > 200 {
        return false;
    }
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| deep_equal(p, q, depth + 1))
        }
        (Value::Object(x), Value::Object(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).map_or(false, |w| deep_equal(v, w, depth + 1)))
        }
        _ => strict_eq(a, b),
    }
}

// ---------- JSON ----------

pub fn to_json(v: &Value) -> Result<serde_json::Value, String> {
    to_json_d(v, 0)
}

fn to_json_d(v: &Value, depth: u32) -> Result<serde_json::Value, String> {
    use serde_json::Value as J;
    if depth > 128 {
        return Err("value is too deeply nested or cyclic".into());
    }
    Ok(match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Num(n) => {
            if !n.is_finite() {
                J::Null
            } else if n.fract() == 0.0 && n.abs() < 9.007199254740992e15 {
                J::Number(serde_json::Number::from(*n as i64))
            } else {
                J::Number(serde_json::Number::from_f64(*n).ok_or("invalid number")?)
            }
        }
        Value::Str(s) => J::String(s.to_string()),
        Value::Array(a) => {
            let mut out = Vec::new();
            for x in a.borrow().iter() {
                out.push(match x {
                    Value::Closure(_) | Value::Native(_) => J::Null,
                    other => to_json_d(other, depth + 1)?,
                });
            }
            J::Array(out)
        }
        Value::Object(o) => {
            let mut m = serde_json::Map::new();
            for (k, x) in o.borrow().iter() {
                if matches!(x, Value::Closure(_) | Value::Native(_)) {
                    continue;
                }
                m.insert(k.to_string(), to_json_d(x, depth + 1)?);
            }
            J::Object(m)
        }
        Value::Closure(_) | Value::Native(_) => return Err("functions cannot be converted to JSON".into()),
        Value::Promise(_) => return Err("promises cannot be converted to JSON (missing await?)".into()),
    })
}

pub fn from_json(j: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    match j {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => Value::Num(n.as_f64().unwrap_or(f64::NAN)),
        J::String(s) => Value::str(s),
        J::Array(a) => Value::array(a.iter().map(from_json).collect()),
        J::Object(o) => {
            let mut m = ObjMap::new();
            for (k, v) in o {
                m.insert(Rc::from(k.as_str()), from_json(v));
            }
            Value::object(m)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn number_formatting_matches_js() {
        assert_eq!(fmt_num(1.0), "1");
        assert_eq!(fmt_num(-0.5), "-0.5");
        assert_eq!(fmt_num(1e21), "1e+21");
        assert_eq!(fmt_num(1.5e-7), "1.5e-7");
        assert_eq!(fmt_num(123456789012.0), "123456789012");
        assert_eq!(fmt_num(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(fmt_num(f64::NAN), "NaN");
    }
    #[test]
    fn deep_equality() {
        let a = from_json(&serde_json::json!({"a": [1, 2, {"b": null}]}));
        let b = from_json(&serde_json::json!({"a": [1, 2, {"b": null}]}));
        assert!(deep_equal(&a, &b, 0));
        assert!(!strict_eq(&a, &b));
    }
}
