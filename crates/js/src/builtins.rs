//! The standard library: Object, Function, Array, String, Number, Boolean,
//! Math, JSON, Error types, Map, Set, Date, console and the global functions.

use crate::interp::{num_to_string, str_to_number, to_i32, Callable, Interp, Kind, Obj, Res, Slot, Value};
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).cloned().unwrap_or(Value::Undefined)
}

pub(crate) fn def(i: &Interp, o: &Obj, name: &str, f: impl Fn(&mut Interp, Value, Vec<Value>) -> Res<Value> + 'static) {
    let fv = i.native(f);
    if let Value::Obj(fo) = &fv {
        fo.borrow_mut().set_own("name", Value::Str(name.into()));
    }
    o.borrow_mut().set_own(name, fv);
}

fn global(i: &mut Interp, name: &str, v: Value) {
    i.global_obj.borrow_mut().set_own(name, v);
}

/// `Object.defineProperty`: an own data property or accessor, set directly
/// (an inherited setter isn't run), enumerable only if the descriptor says so.
fn define_property(i: &mut Interp, o: &Obj, k: &str, desc: &Value) -> Res<()> {
    let ov = Value::Obj(o.clone());
    let has = |i: &mut Interp, name: &str| -> Res<bool> { i.has(desc, name) };
    let existed = i.has_own(&ov, k)?;
    let (g, s) = (i.get(desc, "get")?, i.get(desc, "set")?);
    if Interp::is_callable(&g) || Interp::is_callable(&s) {
        let old = o.borrow().get_own(k).cloned();
        let (mut og, mut os) = match old {
            Some(Slot::Accessor(a, b)) => (a, b),
            _ => (None, None),
        };
        if has(i, "get")? {
            og = Interp::is_callable(&g).then_some(g);
        }
        if has(i, "set")? {
            os = Interp::is_callable(&s).then_some(s);
        }
        o.borrow_mut().set_slot(k, Slot::Accessor(og, os));
    } else if has(i, "value")? || !existed {
        let v = i.get(desc, "value")?;
        let is_array_slot = matches!(o.borrow().kind, Kind::Array(_)) && (k == "length" || k.parse::<usize>().is_ok());
        if is_array_slot {
            i.set(&ov, k, v)?;
        } else {
            o.borrow_mut().set_slot(k, Slot::Data(v));
        }
    }
    if has(i, "enumerable")? {
        let e = i.get(desc, "enumerable")?;
        o.borrow_mut().set_enumerable(k, Interp::truthy(&e));
    } else if !existed {
        o.borrow_mut().set_enumerable(k, false);
    }
    Ok(())
}

/// A constructor function with a prototype object.
fn ctor(i: &mut Interp, name: &str, proto: &Obj, f: impl Fn(&mut Interp, Value, Vec<Value>) -> Res<Value> + 'static) -> Obj {
    let fv = i.native(f);
    let Value::Obj(fo) = fv.clone() else { unreachable!() };
    fo.borrow_mut().set_own("prototype", Value::Obj(proto.clone()));
    fo.borrow_mut().set_own("name", Value::Str(name.into()));
    proto.borrow_mut().set_own("constructor", fv.clone());
    global(i, name, fv);
    fo
}

/// Runs `f` on the array behind `this`; array-likes (`length` and indices,
/// as jQuery objects are) are read into a vector and written back.
fn with_array<T>(i: &mut Interp, this: &Value, f: impl FnOnce(&mut Vec<Value>) -> T) -> Res<T> {
    let Value::Obj(o) = this else { return i.throw("TypeError", "not an array") };
    if let Kind::Array(a) = &mut o.borrow_mut().kind {
        return Ok(f(a));
    }
    let len = i.get(this, "length")?;
    let n = i.to_number(&len)?;
    let n = if n.is_finite() && n > 0.0 { (n as usize).min(10_000_000) } else { 0 };
    let mut v = Vec::with_capacity(n);
    for k in 0..n {
        v.push(i.get(this, &k.to_string())?);
    }
    let r = f(&mut v);
    for (k, x) in v.iter().enumerate() {
        i.set(this, &k.to_string(), x.clone())?;
    }
    for k in v.len()..n {
        o.borrow_mut().delete(&k.to_string());
    }
    i.set(this, "length", Value::Num(v.len() as f64))?;
    Ok(r)
}

/// The elements of an array or array-like (`length` and indices), as Array methods see them.
pub fn items(i: &mut Interp, this: &Value) -> Res<Vec<Value>> {
    match this {
        Value::Obj(o) => {
            match &o.borrow().kind {
                Kind::Array(a) => return Ok(a.clone()),
                Kind::Typed(t) if !t.data_view => return Ok(t.values().into_iter().map(Value::Num).collect()),
                _ => {}
            }
            let len = i.get(this, "length")?;
            let n = i.to_number(&len)?;
            let n = if n.is_finite() && n > 0.0 { (n as usize).min(10_000_000) } else { 0 };
            (0..n).map(|k| i.get(this, &k.to_string())).collect()
        }
        Value::Str(s) => Ok(s.chars().map(|c| Value::Str(c.to_string().into())).collect()),
        Value::Undefined | Value::Null => i.throw("TypeError", "Array method called on null or undefined"),
        _ => Ok(Vec::new()),
    }
}

fn int_arg(i: &mut Interp, a: &[Value], k: usize, default: f64) -> Res<f64> {
    match a.get(k) {
        None | Some(Value::Undefined) => Ok(default),
        Some(v) => {
            let n = i.to_number(v)?;
            Ok(if n.is_nan() { 0.0 } else { n.trunc() })
        }
    }
}

/// A relative index (negative counts from the end), clamped to 0..=len.
fn rel(n: f64, len: usize) -> usize {
    if n < 0.0 {
        (len as f64 + n).max(0.0) as usize
    } else {
        (n as usize).min(len)
    }
}

fn this_str(i: &mut Interp, this: &Value) -> Res<Rc<str>> {
    match this {
        Value::Undefined | Value::Null => i.throw("TypeError", "String method called on null or undefined"),
        v => i.to_str(v),
    }
}

/// How console.log shows a value.
pub fn inspect(i: &mut Interp, v: &Value, top: bool, depth: u32) -> String {
    match v {
        Value::Str(s) if top => s.to_string(),
        Value::Str(s) => format!("'{s}'"),
        Value::Obj(o) => {
            if depth > 3 {
                return "[...]".into();
            }
            let kind = match &o.borrow().kind {
                Kind::Array(a) => Some(a.clone()),
                _ => None,
            };
            if let Some(a) = kind {
                let parts: Vec<String> = a.iter().map(|x| inspect(i, x, false, depth + 1)).collect();
                return format!("[ {} ]", parts.join(", ")).replace("[  ]", "[]");
            }
            if Interp::is_callable(v) {
                let n = i.get(v, "name").ok().and_then(|n| i.to_str(&n).ok()).unwrap_or_default();
                return format!("[Function: {}]", if n.is_empty() { "(anonymous)" } else { &n });
            }
            if matches!(o.borrow().kind, Kind::Error) {
                return i.describe(v);
            }
            if let Kind::RegExp(_, src, flags) = &o.borrow().kind {
                return format!("/{src}/{flags}");
            }
            if let Kind::Promise(p) = &o.borrow().kind {
                return match &*p.borrow() {
                    crate::promise::PState::Pending(_) => "Promise { <pending> }".into(),
                    crate::promise::PState::Fulfilled(v) => {
                        let v = v.clone();
                        format!("Promise {{ {} }}", inspect(i, &v, false, depth + 1))
                    }
                    crate::promise::PState::Rejected(v) => {
                        let v = v.clone();
                        format!("Promise {{ <rejected> {} }}", inspect(i, &v, false, depth + 1))
                    }
                };
            }
            if matches!(o.borrow().kind, Kind::Date(_) | Kind::Boxed(_)) {
                return i.to_str(v).map(|s| s.to_string()).unwrap_or_default();
            }
            let keys = o.borrow().own_keys();
            let mut parts = Vec::new();
            for k in keys {
                let val = i.get(v, &k).unwrap_or(Value::Undefined);
                parts.push(format!("{k}: {}", inspect(i, &val, false, depth + 1)));
            }
            if parts.is_empty() {
                "{}".into()
            } else {
                format!("{{ {} }}", parts.join(", "))
            }
        }
        other => i.to_str(other).map(|s| s.to_string()).unwrap_or_default(),
    }
}

/// An ISO-ish date for a time in ms since the epoch (UTC).
pub fn date_string(t: f64) -> String {
    if !t.is_finite() {
        return "Invalid Date".into();
    }
    let secs = (t / 1000.0).floor() as i64;
    let ms = (t - secs as f64 * 1000.0) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn json_stringify(i: &mut Interp, v: &Value, indent: &str, cur: &str, seen: &mut Vec<*const RefCell<crate::interp::Object>>) -> Res<Option<String>> {
    Ok(match v {
        Value::Undefined => None,
        Value::Null => Some("null".into()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Num(n) => Some(if n.is_finite() { num_to_string(*n) } else { "null".into() }),
        Value::Str(s) => Some(volant_core::json::string(s).replace("\\u003c", "<")),
        Value::Obj(o) => {
            if Interp::is_callable(v) {
                return Ok(None);
            }
            let ptr = Rc::as_ptr(o);
            if seen.contains(&ptr) {
                return i.throw("TypeError", "converting a circular structure to JSON");
            }
            let to_json = i.get(v, "toJSON")?;
            if Interp::is_callable(&to_json) {
                let r = i.call(&to_json, v.clone(), vec![])?;
                return json_stringify(i, &r, indent, cur, seen);
            }
            if let Kind::Boxed(p) = &o.borrow().kind {
                let p = p.clone();
                return json_stringify(i, &p, indent, cur, seen);
            }
            seen.push(ptr);
            let inner = format!("{cur}{indent}");
            let (open, close, sep) = if indent.is_empty() { ("", "", ",") } else { ("\n", "\n", ",\n") };
            let array = match &o.borrow().kind {
                Kind::Array(a) => Some(a.clone()),
                _ => None,
            };
            let out = if let Some(a) = array {
                let mut parts = Vec::new();
                for x in &a {
                    parts.push(format!("{inner}{}", json_stringify(i, x, indent, &inner, seen)?.unwrap_or_else(|| "null".into())));
                }
                if parts.is_empty() {
                    "[]".into()
                } else {
                    format!("[{open}{}{close}{cur}]", parts.join(sep))
                }
            } else {
                let keys = o.borrow().own_keys();
                let mut parts = Vec::new();
                for k in keys {
                    let val = i.get(v, &k)?;
                    if let Some(s) = json_stringify(i, &val, indent, &inner, seen)? {
                        let colon = if indent.is_empty() { ":" } else { ": " };
                        parts.push(format!("{inner}{}{colon}{s}", volant_core::json::string(&k).replace("\\u003c", "<")));
                    }
                }
                if parts.is_empty() {
                    "{}".into()
                } else {
                    format!("{{{open}{}{close}{cur}}}", parts.join(sep))
                }
            };
            seen.pop();
            Some(out)
        }
    })
}

fn from_json(i: &Interp, v: &volant_core::json::Value) -> Value {
    use volant_core::json::Value as J;
    match v {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Num(n) => Value::Num(*n),
        J::Str(s) => Value::Str(s.as_str().into()),
        J::Arr(a) => i.array(a.iter().map(|x| from_json(i, x)).collect()),
        J::Obj(kv) => {
            let o = i.obj(Kind::Plain);
            if let Value::Obj(oo) = &o {
                for (k, x) in kv {
                    oo.borrow_mut().set_own(k, from_json(i, x));
                }
            }
            o
        }
    }
}

fn sort_values(i: &mut Interp, v: &mut [Value], cmp: &Value) -> Res<()> {
    // Insertion sort into a merge sort: stable, and the comparator may throw.
    let mut err = None;
    let mut work = v.to_vec();
    let mut buf = work.clone();
    let n = work.len();
    let mut width = 1;
    let mut compare = |i: &mut Interp, a: &Value, b: &Value| -> std::cmp::Ordering {
        if err.is_some() {
            return std::cmp::Ordering::Equal;
        }
        // undefined sorts last.
        match (a, b) {
            (Value::Undefined, Value::Undefined) => return std::cmp::Ordering::Equal,
            (Value::Undefined, _) => return std::cmp::Ordering::Greater,
            (_, Value::Undefined) => return std::cmp::Ordering::Less,
            _ => {}
        }
        if Interp::is_callable(cmp) {
            match i.call(cmp, Value::Undefined, vec![a.clone(), b.clone()]).and_then(|r| i.to_number(&r)) {
                Ok(n) if n < 0.0 => std::cmp::Ordering::Less,
                Ok(n) if n > 0.0 => std::cmp::Ordering::Greater,
                Ok(_) => std::cmp::Ordering::Equal,
                Err(e) => {
                    err = Some(e);
                    std::cmp::Ordering::Equal
                }
            }
        } else {
            let (x, y) = (i.to_str(a).unwrap_or_default(), i.to_str(b).unwrap_or_default());
            x.encode_utf16().cmp(y.encode_utf16())
        }
    };
    while width < n {
        let mut lo = 0;
        while lo < n {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            let (mut a, mut b, mut k) = (lo, mid, lo);
            while a < mid && b < hi {
                if compare(i, &work[b], &work[a]).is_lt() {
                    buf[k] = work[b].clone();
                    b += 1;
                } else {
                    buf[k] = work[a].clone();
                    a += 1;
                }
                k += 1;
            }
            while a < mid {
                buf[k] = work[a].clone();
                a += 1;
                k += 1;
            }
            while b < hi {
                buf[k] = work[b].clone();
                b += 1;
                k += 1;
            }
            lo = hi;
        }
        std::mem::swap(&mut work, &mut buf);
        width *= 2;
    }
    if let Some(e) = err {
        return Err(e);
    }
    v.clone_from_slice(&work);
    Ok(())
}

pub fn install(i: &mut Interp) {
    let g = Value::Obj(i.global_obj.clone());
    for n in ["globalThis", "window", "self"] {
        global(i, n, g.clone());
    }
    global(i, "NaN", Value::Num(f64::NAN));
    global(i, "Infinity", Value::Num(f64::INFINITY));

    // ---------------------------------------------------------- console
    let console = i.obj(Kind::Plain);
    let Value::Obj(co) = &console else { unreachable!() };
    for name in ["log", "info", "warn", "error", "debug"] {
        def(i, co, name, |i, _t, a| {
            let parts: Vec<String> = a.iter().map(|v| inspect(i, v, true, 0)).collect();
            i.output.push(parts.join(" "));
            Ok(Value::Undefined)
        });
    }
    global(i, "console", console.clone());

    // ---------------------------------------------------------- Object
    let op = i.object_proto.clone();
    let object = ctor(i, "Object", &op, |i, _t, a| Ok(match arg(&a, 0) {
        v @ Value::Obj(_) => v,
        Value::Undefined | Value::Null => i.obj(Kind::Plain),
        p => {
            let proto = match &p {
                Value::Str(s) if s.starts_with("\0@") => i.symbol_proto.clone(),
                Value::Str(_) => i.string_proto.clone(),
                Value::Num(_) => i.number_proto.clone(),
                _ => i.boolean_proto.clone(),
            };
            let o = i.obj(Kind::Boxed(p));
            if let Value::Obj(ob) = &o {
                ob.borrow_mut().proto = Some(proto);
            }
            o
        }
    }));
    def(i, &op, "hasOwnProperty", |i, t, a| {
        let k = i.key(&arg(&a, 0))?;
        Ok(Value::Bool(i.has_own(&t, &k)?))
    });
    def(i, &op, "toString", |i, t, _a| {
        let tag: String = match &t {
            Value::Undefined => "Undefined".into(),
            Value::Null => "Null".into(),
            Value::Bool(_) => "Boolean".into(),
            Value::Num(_) => "Number".into(),
            Value::Str(_) => "String".into(),
            Value::Obj(o) => {
                // An explicit [Symbol.toStringTag] wins (DOM objects, window, Math, JSON…).
                if let Ok(Value::Str(tag)) = i.get(&t, "\0@toStringTag") {
                    tag.to_string()
                } else {
                    match &o.borrow().kind {
                        Kind::Array(_) => "Array".into(),
                        Kind::Function(_) => "Function".into(),
                        Kind::Error => "Error".into(),
                        Kind::Date(_) => "Date".into(),
                        Kind::RegExp(..) => "RegExp".into(),
                        Kind::Map(_) => "Map".into(),
                        Kind::Set(_) => "Set".into(),
                        Kind::Promise(_) => "Promise".into(),
                        Kind::Proxy(t, _) if matches!(t, Value::Obj(x) if matches!(x.borrow().kind, Kind::Array(_))) => "Array".into(),
                        Kind::Boxed(Value::Num(_)) => "Number".into(),
                        Kind::Boxed(Value::Str(s)) if s.starts_with("\0@") => "Symbol".into(),
                        Kind::Boxed(Value::Str(_)) => "String".into(),
                        Kind::Boxed(Value::Bool(_)) => "Boolean".into(),
                        _ => "Object".into(),
                    }
                }
            }
        };
        Ok(Value::Str(format!("[object {tag}]").into()))
    });
    def(i, &op, "valueOf", |_i, t, _a| Ok(t));
    def(i, &op, "isPrototypeOf", |_i, t, a| {
        let (Value::Obj(p), Value::Obj(o)) = (&t, &arg(&a, 0)) else { return Ok(Value::Bool(false)) };
        let mut cur = o.borrow().proto.clone();
        while let Some(c) = cur {
            if Rc::ptr_eq(&c, p) {
                return Ok(Value::Bool(true));
            }
            cur = c.borrow().proto.clone();
        }
        Ok(Value::Bool(false))
    });
    def(i, &object, "keys", |i, _t, a| {
        let v = arg(&a, 0);
        let keys = i.keys_of(&v)?;
        Ok(i.array(keys.into_iter().map(Value::Str).collect()))
    });
    def(i, &object, "values", |i, _t, a| {
        let v = arg(&a, 0);
        let keys = i.keys_of(&v)?;
        let vals = keys.iter().map(|k| i.get(&v, k)).collect::<Res<Vec<_>>>()?;
        Ok(i.array(vals))
    });
    def(i, &object, "entries", |i, _t, a| {
        let v = arg(&a, 0);
        let keys = i.keys_of(&v)?;
        let mut out = Vec::new();
        for k in keys {
            let val = i.get(&v, &k)?;
            out.push(i.array(vec![Value::Str(k), val]));
        }
        Ok(i.array(out))
    });
    def(i, &object, "assign", |i, _t, a| {
        let target = arg(&a, 0);
        for src in a.iter().skip(1) {
            if let Value::Obj(_) = src {
                let keys = i.keys_of(src)?;
                for k in keys {
                    let v = i.get(src, &k)?;
                    i.set(&target, &k, v)?;
                }
            }
        }
        Ok(target)
    });
    def(i, &object, "fromEntries", |i, _t, a| {
        let o = i.obj(Kind::Plain);
        for pair in i.iterate(&arg(&a, 0))? {
            let k = i.get(&pair, "0")?;
            let k = i.key(&k)?;
            let v = i.get(&pair, "1")?;
            i.set(&o, &k, v)?;
        }
        Ok(o)
    });
    /// `Object.defineProperties(o, props)`: each own enumerable key of `props` is a descriptor.
    fn define_all(i: &mut Interp, o: &Obj, props: &Value) -> Res<()> {
        if matches!(props, Value::Undefined) {
            return Ok(());
        }
        let Value::Obj(po) = props else { return i.throw("TypeError", "Property description must be an object") };
        let mut keys = i.keys_of(props)?;
        keys.extend(po.borrow().entries().filter(|(k, _, shown)| *shown && k.starts_with("\0@")).map(|(k, _, _)| k.clone()));
        for k in keys {
            let d = i.get(props, &k)?;
            if !matches!(d, Value::Obj(_)) {
                return i.throw("TypeError", "Property description must be an object");
            }
            define_property(i, o, &k, &d)?;
        }
        Ok(())
    }
    def(i, &object, "create", |i, _t, a| {
        let proto = match arg(&a, 0) {
            Value::Obj(p) => Some(p),
            Value::Null => None,
            _ => return i.throw("TypeError", "Object prototype may only be an Object or null"),
        };
        let o = i.obj(Kind::Plain);
        if let Value::Obj(oo) = &o {
            oo.borrow_mut().proto = proto;
            define_all(i, oo, &arg(&a, 1))?;
        }
        Ok(o)
    });
    def(i, &object, "defineProperties", |i, _t, a| {
        let o = arg(&a, 0);
        let Value::Obj(oo) = &o else { return i.throw("TypeError", "Object.defineProperties called on non-object") };
        define_all(i, oo, &arg(&a, 1))?;
        Ok(o)
    });
    def(i, &object, "hasOwn", |i, _t, a| {
        let k = i.key(&arg(&a, 1))?;
        Ok(Value::Bool(i.has_own(&arg(&a, 0), &k)?))
    });
    def(i, &object, "getPrototypeOf", |i, _t, a| i.get(&arg(&a, 0), "__proto__"));
    def(i, &object, "setPrototypeOf", |i, _t, a| {
        let o = arg(&a, 0);
        i.set(&o, "__proto__", arg(&a, 1))?;
        Ok(o)
    });
    for name in ["freeze", "seal", "preventExtensions"] {
        def(i, &object, name, |_i, _t, a| Ok(arg(&a, 0)));
    }
    def(i, &object, "isFrozen", |_i, _t, _a| Ok(Value::Bool(false)));
    def(i, &object, "defineProperty", |i, _t, a| {
        let (o, desc) = (arg(&a, 0), arg(&a, 2));
        let k = i.key(&arg(&a, 1))?;
        let Value::Obj(oo) = &o else { return i.throw("TypeError", "defineProperty on a non-object") };
        define_property(i, oo, &k, &desc)?;
        Ok(o)
    });
    def(i, &object, "getOwnPropertyNames", |i, _t, a| {
        let o = arg(&a, 0);
        let keys = match &o {
            Value::Obj(oo) if Interp::proxy_parts(&o).is_none() => oo.borrow().all_keys(),
            _ => i.keys_of(&o)?,
        };
        Ok(i.array(keys.into_iter().map(Value::Str).collect()))
    });
    def(i, &object, "getOwnPropertySymbols", |i, _t, a| {
        let keys: Vec<Value> = match &arg(&a, 0) {
            Value::Obj(o) => o.borrow().entries().filter(|(k, _, _)| k.starts_with("\0@")).map(|(k, _, _)| Value::Str(k.clone())).collect(),
            _ => Vec::new(),
        };
        Ok(i.array(keys))
    });
    def(i, &object, "getOwnPropertyDescriptor", |i, _t, a| {
        let o = arg(&a, 0);
        let k = i.key(&arg(&a, 1))?;
        if !i.has_own(&o, &k)? {
            return Ok(Value::Undefined);
        }
        let d = i.obj(Kind::Plain);
        let (slot, enumerable) = match &o {
            Value::Obj(oo) => {
                let b = oo.borrow();
                (b.get_own(&k).cloned(), b.is_enumerable(&k))
            }
            _ => (None, true),
        };
        match slot {
            Some(Slot::Accessor(g, s)) => {
                i.set(&d, "get", g.unwrap_or(Value::Undefined))?;
                i.set(&d, "set", s.unwrap_or(Value::Undefined))?;
            }
            _ => {
                let v = i.get(&o, &k)?;
                i.set(&d, "value", v)?;
                i.set(&d, "writable", Value::Bool(true))?;
            }
        }
        i.set(&d, "enumerable", Value::Bool(enumerable))?;
        i.set(&d, "configurable", Value::Bool(true))?;
        Ok(d)
    });
    def(i, &op, "propertyIsEnumerable", |i, t, a| {
        let k = i.key(&arg(&a, 0))?;
        let own = i.has_own(&t, &k)?;
        Ok(Value::Bool(own && matches!(&t, Value::Obj(o) if o.borrow().is_enumerable(&k))))
    });

    // ---------------------------------------------------------- Function
    let fp = i.function_proto.clone();
    def(i, &fp, "call", |i, t, a| i.call(&t, arg(&a, 0), a.into_iter().skip(1).collect()));
    def(i, &fp, "apply", |i, t, a| {
        let args = match arg(&a, 1) {
            Value::Undefined | Value::Null => Vec::new(),
            v => i.iterate(&v)?,
        };
        i.call(&t, arg(&a, 0), args)
    });
    def(i, &fp, "bind", |i, t, a| {
        let (f, this) = (t, arg(&a, 0));
        let bound: Vec<Value> = a.into_iter().skip(1).collect();
        let name = match i.get(&f, "name")? {
            Value::Str(s) => format!("bound {s}"),
            _ => "bound ".into(),
        };
        let length = match i.get(&f, "length")? {
            Value::Num(n) => (n - bound.len() as f64).max(0.0),
            _ => 0.0,
        };
        let g = i.native(move |i, _t, more| {
            let mut all = bound.clone();
            all.extend(more);
            i.call(&f, this.clone(), all)
        });
        if let Value::Obj(o) = &g {
            let mut b = o.borrow_mut();
            b.set_hidden("name", Value::Str(name.into()));
            b.set_hidden("length", Value::Num(length));
        }
        Ok(g)
    });
    def(i, &fp, "toString", |i, t, _a| {
        let n = i.get(&t, "name")?;
        let n = i.to_str(&n)?;
        Ok(Value::Str(format!("function {n}() {{ [code] }}").into()))
    });
    // new Function(a, b, body): compiled from source, in the global scope.
    let fv = i.native(|i, _t, a| {
        let mut parts = Vec::new();
        for v in &a {
            parts.push(i.to_str(v)?.to_string());
        }
        let body = parts.pop().unwrap_or_default();
        let src = format!("(function anonymous({}\n) {{\n{body}\n}})", parts.join(","));
        match i.run(&src) {
            Ok(f) => Ok(f),
            Err(e) => i.throw("SyntaxError", &e),
        }
    });
    if let Value::Obj(fo) = &fv {
        fo.borrow_mut().set_own("prototype", Value::Obj(fp.clone()));
    }
    global(i, "Function", fv);
    // eval: indirect (global) eval, which is what most code needs.
    let eval = i.native(|i, _t, a| match arg(&a, 0) {
        Value::Str(code) => match i.run(&code) {
            Ok(v) => Ok(v),
            Err(e) => {
                let (name, msg) = e.split_once(": ").unwrap_or(("Error", &e));
                let name = if ["SyntaxError", "TypeError", "ReferenceError", "RangeError"].contains(&name) { name.to_string() } else { "Error".to_string() };
                i.throw(&name, msg)
            }
        },
        other => Ok(other),
    });
    global(i, "eval", eval);

    // ---------------------------------------------------------- Array
    let ap = i.array_proto.clone();
    let array = ctor(i, "Array", &ap, |i, _t, a| {
        Ok(match a.as_slice() {
            [Value::Num(n)] => i.array(vec![Value::Undefined; (*n as usize).min(10_000_000)]),
            _ => i.array(a),
        })
    });
    def(i, &array, "isArray", |_i, _t, a| {
        let mut v = arg(&a, 0);
        while let Some((t, _)) = Interp::proxy_parts(&v) {
            v = t;
        }
        Ok(Value::Bool(matches!(v, Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_)))))
    });
    def(i, &array, "from", |i, _t, a| {
        let mut v = match arg(&a, 0) {
            Value::Undefined | Value::Null => Vec::new(),
            src => i.iterate(&src)?,
        };
        let f = arg(&a, 1);
        if Interp::is_callable(&f) {
            for (k, x) in v.iter_mut().enumerate() {
                *x = i.call(&f, Value::Undefined, vec![x.clone(), Value::Num(k as f64)])?;
            }
        }
        Ok(i.array(v))
    });
    def(i, &array, "of", |i, _t, a| Ok(i.array(a)));
    def(i, &ap, "push", |i, t, a| with_array(i, &t, |arr| {
        arr.extend(a);
        Value::Num(arr.len() as f64)
    }));
    def(i, &ap, "pop", |i, t, _a| with_array(i, &t, |arr| arr.pop().unwrap_or(Value::Undefined)));
    def(i, &ap, "shift", |i, t, _a| with_array(i, &t, |arr| if arr.is_empty() { Value::Undefined } else { arr.remove(0) }));
    def(i, &ap, "unshift", |i, t, a| with_array(i, &t, |arr| {
        arr.splice(0..0, a);
        Value::Num(arr.len() as f64)
    }));
    def(i, &ap, "slice", |i, t, a| {
        let v = items(i, &t)?;
        let s = rel(int_arg(i, &a, 0, 0.0)?, v.len());
        let e = rel(int_arg(i, &a, 1, v.len() as f64)?, v.len());
        Ok(i.array(if s < e { v[s..e].to_vec() } else { Vec::new() }))
    });
    def(i, &ap, "splice", |i, t, a| {
        let len = items(i, &t)?.len();
        let s = rel(int_arg(i, &a, 0, 0.0)?, len);
        let count = if a.len() < 2 { len - s } else { (int_arg(i, &a, 1, 0.0)?.max(0.0) as usize).min(len - s) };
        let insert: Vec<Value> = a.into_iter().skip(2).collect();
        let removed = with_array(i, &t, |arr| arr.splice(s..s + count, insert).collect::<Vec<_>>())?;
        Ok(i.array(removed))
    });
    def(i, &ap, "concat", |i, t, a| {
        let mut v = items(i, &t)?;
        for x in a {
            match &x {
                Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_)) => v.extend(items(i, &x)?),
                _ => v.push(x),
            }
        }
        Ok(i.array(v))
    });
    def(i, &ap, "join", |i, t, a| {
        let sep = match arg(&a, 0) {
            Value::Undefined => ",".into(),
            s => i.to_str(&s)?,
        };
        let v = items(i, &t)?;
        let parts = v.iter().map(|x| if matches!(x, Value::Undefined | Value::Null) { Ok("".into()) } else { i.to_str(x) }).collect::<Res<Vec<Rc<str>>>>()?;
        Ok(Value::Str(parts.join(&sep).into()))
    });
    def(i, &ap, "toString", |i, t, _a| {
        let join = i.get(&t, "join")?;
        i.call(&join, t, vec![])
    });
    def(i, &ap, "reverse", |i, t, _a| {
        with_array(i, &t, |arr| arr.reverse())?;
        Ok(t)
    });
    def(i, &ap, "indexOf", |i, t, a| {
        let v = items(i, &t)?;
        let x = arg(&a, 0);
        Ok(Value::Num(v.iter().position(|y| Interp::strict_eq(&x, y)).map(|p| p as f64).unwrap_or(-1.0)))
    });
    def(i, &ap, "lastIndexOf", |i, t, a| {
        let v = items(i, &t)?;
        let x = arg(&a, 0);
        Ok(Value::Num(v.iter().rposition(|y| Interp::strict_eq(&x, y)).map(|p| p as f64).unwrap_or(-1.0)))
    });
    def(i, &ap, "includes", |i, t, a| {
        let v = items(i, &t)?;
        let x = arg(&a, 0);
        Ok(Value::Bool(v.iter().any(|y| Interp::same_value_zero(&x, y))))
    });
    def(i, &ap, "at", |i, t, a| {
        let v = items(i, &t)?;
        let n = int_arg(i, &a, 0, 0.0)?;
        let k = if n < 0.0 { v.len() as f64 + n } else { n };
        Ok(if k >= 0.0 { v.get(k as usize).cloned().unwrap_or(Value::Undefined) } else { Value::Undefined })
    });
    // The callback methods.
    fn each(i: &mut Interp, t: &Value, a: &[Value], mut f: impl FnMut(&mut Interp, usize, &Value, Value) -> Res<bool>) -> Res<()> {
        let v = items(i, t)?;
        let cb = arg(a, 0);
        if !Interp::is_callable(&cb) {
            return i.throw("TypeError", "callback is not a function");
        }
        let this = arg(a, 1);
        for (k, x) in v.iter().enumerate() {
            let r = i.call(&cb, this.clone(), vec![x.clone(), Value::Num(k as f64), t.clone()])?;
            if !f(i, k, x, r)? {
                break;
            }
        }
        Ok(())
    }
    def(i, &ap, "forEach", |i, t, a| {
        each(i, &t, &a, |_, _, _, _| Ok(true))?;
        Ok(Value::Undefined)
    });
    def(i, &ap, "map", |i, t, a| {
        let mut out = Vec::new();
        each(i, &t, &a, |_, _, _, r| {
            out.push(r);
            Ok(true)
        })?;
        Ok(i.array(out))
    });
    def(i, &ap, "filter", |i, t, a| {
        let mut out = Vec::new();
        each(i, &t, &a, |_, _, x, r| {
            if Interp::truthy(&r) {
                out.push(x.clone());
            }
            Ok(true)
        })?;
        Ok(i.array(out))
    });
    def(i, &ap, "find", |i, t, a| {
        let mut found = Value::Undefined;
        each(i, &t, &a, |_, _, x, r| {
            if Interp::truthy(&r) {
                found = x.clone();
                return Ok(false);
            }
            Ok(true)
        })?;
        Ok(found)
    });
    def(i, &ap, "findIndex", |i, t, a| {
        let mut found = -1.0;
        each(i, &t, &a, |_, k, _, r| {
            if Interp::truthy(&r) {
                found = k as f64;
                return Ok(false);
            }
            Ok(true)
        })?;
        Ok(Value::Num(found))
    });
    def(i, &ap, "some", |i, t, a| {
        let mut any = false;
        each(i, &t, &a, |_, _, _, r| {
            any = Interp::truthy(&r);
            Ok(!any)
        })?;
        Ok(Value::Bool(any))
    });
    def(i, &ap, "every", |i, t, a| {
        let mut all = true;
        each(i, &t, &a, |_, _, _, r| {
            all = Interp::truthy(&r);
            Ok(all)
        })?;
        Ok(Value::Bool(all))
    });
    for (name, right) in [("reduce", false), ("reduceRight", true)] {
        def(i, &ap, name, move |i, t, a| {
            let mut v = items(i, &t)?;
            if right {
                v.reverse();
            }
            let cb = arg(&a, 0);
            let mut it = v.into_iter().enumerate();
            let mut acc = if a.len() > 1 {
                arg(&a, 1)
            } else {
                match it.next() {
                    Some((_, x)) => x,
                    None => return i.throw("TypeError", "reduce of an empty array with no initial value"),
                }
            };
            for (k, x) in it {
                acc = i.call(&cb, Value::Undefined, vec![acc, x, Value::Num(k as f64), t.clone()])?;
            }
            Ok(acc)
        });
    }
    def(i, &ap, "sort", |i, t, a| {
        let mut v = items(i, &t)?;
        sort_values(i, &mut v, &arg(&a, 0))?;
        with_array(i, &t, |arr| *arr = v)?;
        Ok(t)
    });
    def(i, &ap, "fill", |i, t, a| {
        let x = arg(&a, 0);
        let len = items(i, &t)?.len();
        let s = rel(int_arg(i, &a, 1, 0.0)?, len);
        let e = rel(int_arg(i, &a, 2, len as f64)?, len);
        with_array(i, &t, |arr| {
            for k in s..e.max(s) {
                arr[k] = x.clone();
            }
        })?;
        Ok(t)
    });
    def(i, &ap, "flat", |i, t, a| {
        fn flat(i: &mut Interp, v: Vec<Value>, depth: f64, out: &mut Vec<Value>) -> Res<()> {
            for x in v {
                let inner = match &x {
                    Value::Obj(o) if depth >= 1.0 => match &o.borrow().kind {
                        Kind::Array(a) => Some(a.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                match inner {
                    Some(a) => flat(i, a, depth - 1.0, out)?,
                    None => out.push(x),
                }
            }
            Ok(())
        }
        let v = items(i, &t)?;
        let d = int_arg(i, &a, 0, 1.0)?;
        let mut out = Vec::new();
        flat(i, v, d, &mut out)?;
        Ok(i.array(out))
    });
    def(i, &ap, "flatMap", |i, t, a| {
        let mut out = Vec::new();
        each(i, &t, &a, |i, _, _, r| {
            match &r {
                Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_)) => out.extend(items(i, &r)?),
                _ => out.push(r),
            }
            Ok(true)
        })?;
        Ok(i.array(out))
    });
    def(i, &ap, "keys", |i, t, _a| {
        let n = items(i, &t)?.len();
        Ok(i.array((0..n).map(|k| Value::Num(k as f64)).collect()))
    });
    def(i, &ap, "values", |i, t, _a| {
        let v = items(i, &t)?;
        Ok(i.array(v))
    });
    def(i, &ap, "entries", |i, t, _a| {
        let v = items(i, &t)?;
        let out = v.into_iter().enumerate().map(|(k, x)| i.array(vec![Value::Num(k as f64), x])).collect();
        Ok(i.array(out))
    });

    // ---------------------------------------------------------- String
    let sp = i.string_proto.clone();
    let string = ctor(i, "String", &sp, |i, _t, a| Ok(Value::Str(if a.is_empty() { "".into() } else { i.to_str(&a[0])? })));
    def(i, &string, "fromCharCode", |i, _t, a| {
        let units: Vec<u16> = a.iter().map(|v| i.to_number(v).map(|n| n as u32 as u16)).collect::<Res<_>>()?;
        Ok(Value::Str(String::from_utf16_lossy(&units).into()))
    });
    def(i, &sp, "toString", |i, t, _a| Ok(Value::Str(this_str(i, &t)?)));
    def(i, &sp, "valueOf", |i, t, _a| Ok(Value::Str(this_str(i, &t)?)));
    def(i, &sp, "charAt", |i, t, a| {
        let s = this_str(i, &t)?;
        let k = int_arg(i, &a, 0, 0.0)?;
        Ok(Value::Str(if k < 0.0 { "".into() } else { s.chars().nth(k as usize).map(|c| c.to_string()).unwrap_or_default().into() }))
    });
    def(i, &sp, "charCodeAt", |i, t, a| {
        let s = this_str(i, &t)?;
        let k = int_arg(i, &a, 0, 0.0)?;
        Ok(Value::Num(if k < 0.0 { f64::NAN } else { s.encode_utf16().nth(k as usize).map(|u| u as f64).unwrap_or(f64::NAN) }))
    });
    def(i, &sp, "codePointAt", |i, t, a| {
        let s = this_str(i, &t)?;
        let k = int_arg(i, &a, 0, 0.0)?;
        Ok(s.chars().nth(k.max(0.0) as usize).map(|c| Value::Num(c as u32 as f64)).unwrap_or(Value::Undefined))
    });
    fn char_index(s: &str, byte: usize) -> f64 {
        s[..byte].chars().count() as f64
    }
    def(i, &sp, "indexOf", |i, t, a| {
        let s = this_str(i, &t)?;
        let n = i.to_str(&arg(&a, 0))?;
        let from = int_arg(i, &a, 1, 0.0)?.max(0.0) as usize;
        let start = s.char_indices().nth(from).map(|x| x.0).unwrap_or(s.len());
        Ok(Value::Num(s[start..].find(&*n).map(|p| char_index(&s, start + p)).unwrap_or(-1.0)))
    });
    def(i, &sp, "lastIndexOf", |i, t, a| {
        let s = this_str(i, &t)?;
        let n = i.to_str(&arg(&a, 0))?;
        Ok(Value::Num(s.rfind(&*n).map(|p| char_index(&s, p)).unwrap_or(-1.0)))
    });
    def(i, &sp, "includes", |i, t, a| {
        let s = this_str(i, &t)?;
        let n = i.to_str(&arg(&a, 0))?;
        Ok(Value::Bool(s.contains(&*n)))
    });
    def(i, &sp, "startsWith", |i, t, a| {
        let s = this_str(i, &t)?;
        let n = i.to_str(&arg(&a, 0))?;
        let from = int_arg(i, &a, 1, 0.0)?.max(0.0) as usize;
        let rest: String = s.chars().skip(from).collect();
        Ok(Value::Bool(rest.starts_with(&*n)))
    });
    def(i, &sp, "endsWith", |i, t, a| {
        let s = this_str(i, &t)?;
        let n = i.to_str(&arg(&a, 0))?;
        Ok(Value::Bool(s.ends_with(&*n)))
    });
    for name in ["slice", "substring", "substr"] {
        def(i, &sp, name, move |i, t, a| {
            let s = this_str(i, &t)?;
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len();
            let (start, end) = match name {
                "slice" => (rel(int_arg(i, &a, 0, 0.0)?, len), rel(int_arg(i, &a, 1, len as f64)?, len)),
                "substring" => {
                    let x = int_arg(i, &a, 0, 0.0)?.clamp(0.0, len as f64) as usize;
                    let y = int_arg(i, &a, 1, len as f64)?.clamp(0.0, len as f64) as usize;
                    (x.min(y), x.max(y))
                }
                _ => {
                    let x = rel(int_arg(i, &a, 0, 0.0)?, len);
                    let n = int_arg(i, &a, 1, (len - x) as f64)?.max(0.0) as usize;
                    (x, (x + n).min(len))
                }
            };
            Ok(Value::Str(if start < end { chars[start..end].iter().collect::<String>().into() } else { "".into() }))
        });
    }
    def(i, &sp, "toUpperCase", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.to_uppercase().into())));
    def(i, &sp, "toLowerCase", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.to_lowercase().into())));
    def(i, &sp, "toLocaleUpperCase", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.to_uppercase().into())));
    def(i, &sp, "toLocaleLowerCase", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.to_lowercase().into())));
    def(i, &sp, "trim", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.trim().into())));
    def(i, &sp, "trimStart", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.trim_start().into())));
    def(i, &sp, "trimEnd", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.trim_end().into())));
    for (name, start) in [("padStart", true), ("padEnd", false)] {
        def(i, &sp, name, move |i, t, a| {
            let s = this_str(i, &t)?;
            let target = int_arg(i, &a, 0, 0.0)?.max(0.0) as usize;
            let fill = match arg(&a, 1) {
                Value::Undefined => " ".into(),
                f => i.to_str(&f)?,
            };
            let len = s.chars().count();
            if target <= len || fill.is_empty() {
                return Ok(Value::Str(s));
            }
            let pad: String = fill.chars().cycle().take(target - len).collect();
            Ok(Value::Str(if start { format!("{pad}{s}") } else { format!("{s}{pad}") }.into()))
        });
    }
    def(i, &sp, "repeat", |i, t, a| {
        let s = this_str(i, &t)?;
        let n = int_arg(i, &a, 0, 0.0)?;
        if n < 0.0 || n * s.len() as f64 > 1e8 {
            return i.throw("RangeError", "invalid repeat count");
        }
        Ok(Value::Str(s.repeat(n as usize).into()))
    });
    def(i, &sp, "concat", |i, t, a| {
        let mut s = this_str(i, &t)?.to_string();
        for x in a {
            s.push_str(&i.to_str(&x)?);
        }
        Ok(Value::Str(s.into()))
    });
    def(i, &sp, "at", |i, t, a| {
        let s = this_str(i, &t)?;
        let chars: Vec<char> = s.chars().collect();
        let n = int_arg(i, &a, 0, 0.0)?;
        let k = if n < 0.0 { chars.len() as f64 + n } else { n };
        Ok(if k >= 0.0 { chars.get(k as usize).map(|c| Value::Str(c.to_string().into())).unwrap_or(Value::Undefined) } else { Value::Undefined })
    });
    def(i, &sp, "split", |i, t, a| {
        let s = this_str(i, &t)?;
        let limit = match arg(&a, 1) {
            Value::Undefined => usize::MAX,
            l => i.to_number(&l)? as usize,
        };
        let parts: Vec<Value> = match arg(&a, 0) {
            Value::Undefined => vec![Value::Str(s.clone())],
            sep => {
                let sep = i.to_str(&sep)?;
                if sep.is_empty() {
                    s.chars().map(|c| Value::Str(c.to_string().into())).collect()
                } else {
                    s.split(&*sep).map(|p| Value::Str(p.into())).collect()
                }
            }
        };
        Ok(i.array(parts.into_iter().take(limit).collect()))
    });
    for (name, all) in [("replace", false), ("replaceAll", true)] {
        def(i, &sp, name, move |i, t, a| {
            let s = this_str(i, &t)?;
            let pat = i.to_str(&arg(&a, 0))?;
            let with = arg(&a, 1);
            let mut out = String::new();
            let mut rest: &str = &s;
            let mut offset = 0;
            loop {
                let Some(p) = rest.find(&*pat) else { break };
                out.push_str(&rest[..p]);
                let rep = if Interp::is_callable(&with) {
                    let r = i.call(&with, Value::Undefined, vec![Value::Str(pat.clone()), Value::Num((offset + p) as f64), Value::Str(s.clone())])?;
                    i.to_str(&r)?
                } else {
                    i.to_str(&with)?.replace("$&", &pat).into()
                };
                out.push_str(&rep);
                let adv = p + pat.len().max(1);
                if pat.is_empty() {
                    out.push_str(&rest[p..adv.min(rest.len())]);
                }
                offset += adv;
                rest = &rest[adv.min(rest.len())..];
                if !all || rest.is_empty() {
                    break;
                }
            }
            out.push_str(rest);
            Ok(Value::Str(out.into()))
        });
    }
    def(i, &sp, "localeCompare", |i, t, a| {
        let s = this_str(i, &t)?;
        let o = i.to_str(&arg(&a, 0))?;
        Ok(Value::Num(match s.cmp(&o) {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Equal => 0.0,
            std::cmp::Ordering::Greater => 1.0,
        }))
    });

    // ---------------------------------------------------------- Number, Boolean
    let np = i.number_proto.clone();
    let number = ctor(i, "Number", &np, |i, _t, a| Ok(Value::Num(if a.is_empty() { 0.0 } else { i.to_number(&a[0])? })));
    number.borrow_mut().set_own("MAX_SAFE_INTEGER", Value::Num(9007199254740991.0));
    number.borrow_mut().set_own("MIN_SAFE_INTEGER", Value::Num(-9007199254740991.0));
    number.borrow_mut().set_own("EPSILON", Value::Num(f64::EPSILON));
    number.borrow_mut().set_own("MAX_VALUE", Value::Num(f64::MAX));
    number.borrow_mut().set_own("POSITIVE_INFINITY", Value::Num(f64::INFINITY));
    number.borrow_mut().set_own("NEGATIVE_INFINITY", Value::Num(f64::NEG_INFINITY));
    number.borrow_mut().set_own("NaN", Value::Num(f64::NAN));
    def(i, &number, "isInteger", |_i, _t, a| Ok(Value::Bool(matches!(arg(&a, 0), Value::Num(n) if n.is_finite() && n.fract() == 0.0))));
    def(i, &number, "isSafeInteger", |_i, _t, a| Ok(Value::Bool(matches!(arg(&a, 0), Value::Num(n) if n.fract() == 0.0 && n.abs() <= 9007199254740991.0))));
    def(i, &number, "isFinite", |_i, _t, a| Ok(Value::Bool(matches!(arg(&a, 0), Value::Num(n) if n.is_finite()))));
    def(i, &number, "isNaN", |_i, _t, a| Ok(Value::Bool(matches!(arg(&a, 0), Value::Num(n) if n.is_nan()))));
    let this_num = |i: &mut Interp, t: &Value| -> Res<f64> {
        match t {
            Value::Num(n) => Ok(*n),
            Value::Obj(o) => match &o.borrow().kind {
                Kind::Boxed(Value::Num(n)) => Ok(*n),
                _ => Err(i.error("TypeError", "not a number")),
            },
            _ => Err(i.error("TypeError", "not a number")),
        }
    };
    def(i, &np, "toFixed", move |i, t, a| {
        let n = this_num(i, &t)?;
        let d = int_arg(i, &a, 0, 0.0)?.clamp(0.0, 100.0) as usize;
        Ok(Value::Str(if n.abs() >= 1e21 { num_to_string(n) } else { format!("{n:.d$}") }.into()))
    });
    def(i, &np, "toString", move |i, t, a| {
        let n = this_num(i, &t)?;
        let radix = int_arg(i, &a, 0, 10.0)? as u32;
        if radix == 10 || !(2..=36).contains(&radix) || n.fract() != 0.0 || !n.is_finite() {
            return Ok(Value::Str(num_to_string(n).into()));
        }
        let mut x = n.abs() as u128;
        let mut digits = Vec::new();
        loop {
            digits.push(std::char::from_digit((x % radix as u128) as u32, radix).unwrap_or('0'));
            x /= radix as u128;
            if x == 0 {
                break;
            }
        }
        if n < 0.0 {
            digits.push('-');
        }
        Ok(Value::Str(digits.into_iter().rev().collect::<String>().into()))
    });
    def(i, &np, "valueOf", move |i, t, _a| Ok(Value::Num(this_num(i, &t)?)));
    def(i, &np, "toLocaleString", move |i, t, _a| {
        let n = this_num(i, &t)?;
        if n.fract() != 0.0 || !n.is_finite() {
            return Ok(Value::Str(num_to_string(n).into()));
        }
        // Thousands separators.
        let s = format!("{}", n.abs() as u128);
        let mut out = String::new();
        for (k, c) in s.chars().enumerate() {
            if k > 0 && (s.len() - k) % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        Ok(Value::Str(if n < 0.0 { format!("-{out}") } else { out }.into()))
    });
    let bp = i.boolean_proto.clone();
    ctor(i, "Boolean", &bp, |_i, _t, a| Ok(Value::Bool(Interp::truthy(&arg(&a, 0)))));
    def(i, &bp, "toString", |i, t, _a| Ok(Value::Str(i.to_str(&t)?)));
    def(i, &bp, "valueOf", |_i, t, _a| Ok(t));

    let parse_int = i.native(|i, _t, a| {
        let s = i.to_str(&arg(&a, 0))?;
        let mut t = s.trim();
        let neg = t.starts_with('-');
        t = t.trim_start_matches(['-', '+']);
        let mut radix = int_arg(i, &a, 1, 0.0)? as u32;
        if (radix == 0 || radix == 16) && (t.starts_with("0x") || t.starts_with("0X")) {
            t = &t[2..];
            radix = 16;
        }
        if radix == 0 {
            radix = 10;
        }
        if !(2..=36).contains(&radix) {
            return Ok(Value::Num(f64::NAN));
        }
        let digits: String = t.chars().take_while(|c| c.is_digit(radix)).collect();
        if digits.is_empty() {
            return Ok(Value::Num(f64::NAN));
        }
        let mut n = 0f64;
        for c in digits.chars() {
            n = n * radix as f64 + c.to_digit(radix).unwrap_or(0) as f64;
        }
        Ok(Value::Num(if neg { -n } else { n }))
    });
    let parse_float = i.native(|i, _t, a| {
        let s = i.to_str(&arg(&a, 0))?;
        let t = s.trim_start();
        if t.starts_with("Infinity") || t.starts_with("+Infinity") {
            return Ok(Value::Num(f64::INFINITY));
        }
        if t.starts_with("-Infinity") {
            return Ok(Value::Num(f64::NEG_INFINITY));
        }
        // The longest prefix that parses.
        let mut best = f64::NAN;
        let mut end = 0;
        for (k, c) in t.char_indices() {
            if !(c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) {
                break;
            }
            end = k + c.len_utf8();
            if let Ok(v) = t[..end].parse::<f64>() {
                best = v;
            }
        }
        let _ = end;
        Ok(Value::Num(best))
    });
    global(i, "parseInt", parse_int.clone());
    global(i, "parseFloat", parse_float.clone());
    number.borrow_mut().set_own("parseInt", parse_int);
    number.borrow_mut().set_own("parseFloat", parse_float);
    let is_nan = i.native(|i, _t, a| Ok(Value::Bool(i.to_number(&arg(&a, 0))?.is_nan())));
    global(i, "isNaN", is_nan);
    let is_finite = i.native(|i, _t, a| Ok(Value::Bool(i.to_number(&arg(&a, 0))?.is_finite())));
    global(i, "isFinite", is_finite);

    // ---------------------------------------------------------- Math
    let math = i.obj(Kind::Plain);
    let Value::Obj(mo) = &math else { unreachable!() };
    mo.borrow_mut().set_own("PI", Value::Num(std::f64::consts::PI));
    mo.borrow_mut().set_own("E", Value::Num(std::f64::consts::E));
    mo.borrow_mut().set_own("LN2", Value::Num(std::f64::consts::LN_2));
    mo.borrow_mut().set_own("LN10", Value::Num(std::f64::consts::LN_10));
    mo.borrow_mut().set_own("SQRT2", Value::Num(std::f64::consts::SQRT_2));
    let unary: &[(&str, fn(f64) -> f64)] = &[
        ("abs", f64::abs),
        ("floor", f64::floor),
        ("ceil", f64::ceil),
        ("trunc", f64::trunc),
        ("sqrt", f64::sqrt),
        ("cbrt", f64::cbrt),
        ("sin", f64::sin),
        ("cos", f64::cos),
        ("tan", f64::tan),
        ("asin", f64::asin),
        ("acos", f64::acos),
        ("atan", f64::atan),
        ("log", f64::ln),
        ("log2", f64::log2),
        ("log10", f64::log10),
        ("exp", f64::exp),
        ("sign", |x: f64| if x.is_nan() || x == 0.0 { x } else { x.signum() }),
        ("round", |x: f64| (x + 0.5).floor()),
        ("fround", |x: f64| x as f32 as f64),
    ];
    for &(name, f) in unary {
        def(i, mo, name, move |i, _t, a| Ok(Value::Num(f(i.to_number(&arg(&a, 0))?))));
    }
    def(i, mo, "pow", |i, _t, a| Ok(Value::Num(i.to_number(&arg(&a, 0))?.powf(i.to_number(&arg(&a, 1))?))));
    def(i, mo, "atan2", |i, _t, a| Ok(Value::Num(i.to_number(&arg(&a, 0))?.atan2(i.to_number(&arg(&a, 1))?))));
    def(i, mo, "max", |i, _t, a| {
        let mut m = f64::NEG_INFINITY;
        for v in &a {
            let n = i.to_number(v)?;
            if n.is_nan() {
                return Ok(Value::Num(f64::NAN));
            }
            m = m.max(n);
        }
        Ok(Value::Num(m))
    });
    def(i, mo, "min", |i, _t, a| {
        let mut m = f64::INFINITY;
        for v in &a {
            let n = i.to_number(v)?;
            if n.is_nan() {
                return Ok(Value::Num(f64::NAN));
            }
            m = m.min(n);
        }
        Ok(Value::Num(m))
    });
    def(i, mo, "hypot", |i, _t, a| {
        let mut s = 0.0;
        for v in &a {
            let n = i.to_number(v)?;
            s += n * n;
        }
        Ok(Value::Num(s.sqrt()))
    });
    let seed = Rc::new(std::cell::Cell::new(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(88172645463325252) | 1,
    ));
    def(i, mo, "random", move |_i, _t, _a| {
        let mut x = seed.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        seed.set(x);
        Ok(Value::Num((x >> 11) as f64 / (1u64 << 53) as f64))
    });
    mo.borrow_mut().set_own("\0@toStringTag", Value::Str("Math".into()));
    global(i, "Math", math);

    // ---------------------------------------------------------- JSON
    let json = i.obj(Kind::Plain);
    let Value::Obj(jo) = &json else { unreachable!() };
    def(i, jo, "stringify", |i, _t, a| {
        let indent = match arg(&a, 2) {
            Value::Num(n) => " ".repeat(n.clamp(0.0, 10.0) as usize),
            Value::Str(s) => s.chars().take(10).collect(),
            _ => String::new(),
        };
        Ok(match json_stringify(i, &arg(&a, 0), &indent, "", &mut Vec::new())? {
            Some(s) => Value::Str(s.into()),
            None => Value::Undefined,
        })
    });
    def(i, jo, "parse", |i, _t, a| {
        let s = i.to_str(&arg(&a, 0))?;
        match volant_core::json::parse(&s) {
            Some(v) => Ok(from_json(i, &v)),
            None => i.throw("SyntaxError", "JSON.parse: not valid JSON"),
        }
    });
    jo.borrow_mut().set_own("\0@toStringTag", Value::Str("JSON".into()));
    global(i, "JSON", json);

    // ---------------------------------------------------------- errors
    let ep = i.error_proto.clone();
    ep.borrow_mut().set_own("name", Value::Str("Error".into()));
    ep.borrow_mut().set_own("message", Value::Str("".into()));
    def(i, &ep, "toString", |i, t, _a| {
        let n = i.get(&t, "name")?;
        let n = i.to_str(&n)?;
        let m = i.get(&t, "message")?;
        let m = i.to_str(&m)?;
        Ok(Value::Str(if m.is_empty() { n.to_string() } else { format!("{n}: {m}") }.into()))
    });
    for name in ["Error", "TypeError", "RangeError", "SyntaxError", "ReferenceError", "EvalError", "URIError"] {
        let proto = if name == "Error" {
            ep.clone()
        } else {
            let p = Rc::new(RefCell::new(crate::interp::Object::new(Some(ep.clone()), Kind::Plain)));
            p.borrow_mut().set_own("name", Value::Str(name.into()));
            p
        };
        let proto2 = proto.clone();
        ctor(i, name, &proto, move |i, _t, a| {
            let e = i.obj(Kind::Error);
            if let Value::Obj(o) = &e {
                o.borrow_mut().proto = Some(proto2.clone());
                if !matches!(arg(&a, 0), Value::Undefined) {
                    let m = i.to_str(&arg(&a, 0))?;
                    o.borrow_mut().set_own("message", Value::Str(m));
                }
                o.borrow_mut().set_own("stack", Value::Str("".into()));
            }
            Ok(e)
        });
    }

    // ---------------------------------------------------------- Map, Set
    let mp = i.map_proto.clone();
    ctor(i, "Map", &mp, |i, _t, a| {
        let m = i.obj(Kind::Map(Vec::new()));
        if !matches!(arg(&a, 0), Value::Undefined | Value::Null) {
            let set = i.get(&m, "set")?;
            for pair in i.iterate(&arg(&a, 0))? {
                let k = i.get(&pair, "0")?;
                let v = i.get(&pair, "1")?;
                i.call(&set, m.clone(), vec![k, v])?;
            }
        }
        Ok(m)
    });
    fn map_op<T>(i: &Interp, t: &Value, f: impl FnOnce(&mut Vec<(Value, Value)>) -> T) -> Res<T> {
        if let Value::Obj(o) = t {
            if let Kind::Map(m) = &mut o.borrow_mut().kind {
                return Ok(f(m));
            }
        }
        i.throw("TypeError", "not a Map")
    }
    def(i, &mp, "get", |i, t, a| {
        let k = arg(&a, 0);
        map_op(i, &t, |m| m.iter().find(|(x, _)| Interp::same_value_zero(x, &k)).map(|p| p.1.clone()).unwrap_or(Value::Undefined))
    });
    def(i, &mp, "set", |i, t, a| {
        let (k, v) = (arg(&a, 0), arg(&a, 1));
        map_op(i, &t, |m| match m.iter_mut().find(|(x, _)| Interp::same_value_zero(x, &k)) {
            Some(p) => p.1 = v,
            None => m.push((k, v)),
        })?;
        Ok(t)
    });
    def(i, &mp, "has", |i, t, a| {
        let k = arg(&a, 0);
        map_op(i, &t, |m| Value::Bool(m.iter().any(|(x, _)| Interp::same_value_zero(x, &k))))
    });
    def(i, &mp, "delete", |i, t, a| {
        let k = arg(&a, 0);
        map_op(i, &t, |m| {
            let before = m.len();
            m.retain(|(x, _)| !Interp::same_value_zero(x, &k));
            Value::Bool(m.len() != before)
        })
    });
    def(i, &mp, "clear", |i, t, _a| map_op(i, &t, |m| {
        m.clear();
        Value::Undefined
    }));
    def(i, &mp, "forEach", |i, t, a| {
        let entries = map_op(i, &t, |m| m.clone())?;
        let cb = arg(&a, 0);
        for (k, v) in entries {
            i.call(&cb, Value::Undefined, vec![v, k, t.clone()])?;
        }
        Ok(Value::Undefined)
    });
    for (name, which) in [("keys", 0), ("values", 1), ("entries", 2)] {
        def(i, &mp, name, move |i, t, _a| {
            let entries = map_op(i, &t, |m| m.clone())?;
            let out = entries.into_iter().map(|(k, v)| match which {
                0 => k,
                1 => v,
                _ => i.array(vec![k, v]),
            });
            let out: Vec<Value> = out.collect();
            Ok(i.array(out))
        });
    }
    let setp = i.set_proto.clone();
    ctor(i, "Set", &setp, |i, _t, a| {
        let mut items: Vec<Value> = Vec::new();
        if !matches!(arg(&a, 0), Value::Undefined | Value::Null) {
            for v in i.iterate(&arg(&a, 0))? {
                if !items.iter().any(|x| Interp::same_value_zero(x, &v)) {
                    items.push(v);
                }
            }
        }
        Ok(i.obj(Kind::Set(items)))
    });
    fn set_op<T>(i: &Interp, t: &Value, f: impl FnOnce(&mut Vec<Value>) -> T) -> Res<T> {
        if let Value::Obj(o) = t {
            if let Kind::Set(s) = &mut o.borrow_mut().kind {
                return Ok(f(s));
            }
        }
        i.throw("TypeError", "not a Set")
    }
    def(i, &setp, "add", |i, t, a| {
        let v = arg(&a, 0);
        set_op(i, &t, |s| {
            if !s.iter().any(|x| Interp::same_value_zero(x, &v)) {
                s.push(v)
            }
        })?;
        Ok(t)
    });
    def(i, &setp, "has", |i, t, a| {
        let v = arg(&a, 0);
        set_op(i, &t, |s| Value::Bool(s.iter().any(|x| Interp::same_value_zero(x, &v))))
    });
    def(i, &setp, "delete", |i, t, a| {
        let v = arg(&a, 0);
        set_op(i, &t, |s| {
            let before = s.len();
            s.retain(|x| !Interp::same_value_zero(x, &v));
            Value::Bool(s.len() != before)
        })
    });
    def(i, &setp, "clear", |i, t, _a| set_op(i, &t, |s| {
        s.clear();
        Value::Undefined
    }));
    def(i, &setp, "forEach", |i, t, a| {
        let items = set_op(i, &t, |s| s.clone())?;
        let cb = arg(&a, 0);
        for v in items {
            i.call(&cb, Value::Undefined, vec![v.clone(), v, t.clone()])?;
        }
        Ok(Value::Undefined)
    });
    for name in ["values", "keys"] {
        def(i, &setp, name, |i, t, _a| {
            let items = set_op(i, &t, |s| s.clone())?;
            Ok(i.array(items))
        });
    }

    // ---------------------------------------------------------- Date
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0);
    let dp = i.date_proto.clone();
    let date = ctor(i, "Date", &dp, move |i, t, a| {
        let is_new = matches!(&t, Value::Obj(o) if o.borrow().get_own("__new__").is_some());
        let ms = match a.first() {
            None => now(),
            // new Date(year, month, day, hours, minutes, seconds, ms)
            Some(_) if a.len() >= 2 => {
                let mut p = [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
                for (k, v) in a.iter().take(7).enumerate() {
                    p[k] = i.to_number(v)?;
                }
                if (0.0..100.0).contains(&p[0]) {
                    p[0] += 1900.0;
                }
                make_time(p)
            }
            Some(Value::Num(n)) => *n,
            Some(Value::Str(s)) => parse_date(s),
            Some(Value::Obj(o)) if matches!(o.borrow().kind, Kind::Date(_)) => match o.borrow().kind {
                Kind::Date(t) => t,
                _ => f64::NAN,
            },
            Some(v) => i.to_number(v)?,
        };
        if !is_new {
            return Ok(Value::Str(date_string(now()).into()));
        }
        Ok(i.obj(Kind::Date(ms)))
    });
    def(i, &date, "now", move |_i, _t, _a| Ok(Value::Num(now())));
    def(i, &date, "UTC", |i, _t, a| {
        let mut p = [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        for (k, v) in a.iter().take(7).enumerate() {
            p[k] = i.to_number(v)?;
        }
        Ok(Value::Num(make_time(p)))
    });
    def(i, &date, "parse", |i, _t, a| {
        let s = i.to_str(&arg(&a, 0))?;
        Ok(Value::Num(parse_date(&s)))
    });
    def(i, &dp, "setTime", |i, t, a| {
        let ms = i.to_number(&arg(&a, 0))?;
        if let Value::Obj(o) = &t {
            if let Kind::Date(d) = &mut o.borrow_mut().kind {
                *d = ms;
                return Ok(Value::Num(ms));
            }
        }
        i.throw("TypeError", "not a Date")
    });
    // setFullYear … setMilliseconds (and their UTC twins; Volant's local time is UTC).
    for (name, at) in [("FullYear", 0usize), ("Month", 1), ("Date", 2), ("Hours", 3), ("Minutes", 4), ("Seconds", 5), ("Milliseconds", 6)] {
        for n in [format!("set{name}"), format!("setUTC{name}")] {
            def(i, &dp, &n, move |i, t, a| {
                let Value::Obj(o) = &t else { return i.throw("TypeError", "not a Date") };
                let Kind::Date(ms) = o.borrow().kind else { return i.throw("TypeError", "not a Date") };
                let mut p = time_parts(ms);
                for (k, v) in a.iter().enumerate() {
                    if at + k < 7 {
                        p[at + k] = i.to_number(v)?;
                    }
                }
                let new = make_time(p);
                if let Kind::Date(d) = &mut o.borrow_mut().kind {
                    *d = new;
                }
                Ok(Value::Num(new))
            });
        }
    }
    for n in ["getMilliseconds", "getUTCMilliseconds"] {
        def(i, &dp, n, |i, t, _a| match &t {
            Value::Obj(o) => match o.borrow().kind {
                Kind::Date(ms) => Ok(Value::Num(ms.rem_euclid(1000.0))),
                _ => i.throw("TypeError", "not a Date"),
            },
            _ => i.throw("TypeError", "not a Date"),
        });
    }
    let this_time = |i: &Interp, t: &Value| -> Res<f64> {
        match t {
            Value::Obj(o) => match &o.borrow().kind {
                Kind::Date(ms) => Ok(*ms),
                _ => i.throw("TypeError", "not a Date"),
            },
            _ => i.throw("TypeError", "not a Date"),
        }
    };
    def(i, &dp, "getTime", move |i, t, _a| Ok(Value::Num(this_time(i, &t)?)));
    def(i, &dp, "valueOf", move |i, t, _a| Ok(Value::Num(this_time(i, &t)?)));
    def(i, &dp, "toISOString", move |i, t, _a| Ok(Value::Str(date_string(this_time(i, &t)?).into())));
    // Browser-style text: "Fri Nov 13 2026 10:00:00 GMT+0000 (Coordinated Universal Time)".
    fn human(ms: f64, part: u8) -> String {
        if ms.is_nan() {
            return "Invalid Date".into();
        }
        let s = date_string(ms);
        const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let day = DAYS[((ms / 86_400_000.0).floor() + 4.0).rem_euclid(7.0) as usize];
        let month = MONTHS[s.get(5..7).and_then(|m| m.parse::<usize>().ok()).unwrap_or(1).clamp(1, 12) - 1];
        let (year, date, time) = (s.get(0..4).unwrap_or(""), s.get(8..10).unwrap_or(""), s.get(11..19).unwrap_or(""));
        match part {
            0 => format!("{day} {month} {date} {year} {time} GMT+0000 (Coordinated Universal Time)"),
            1 => format!("{day} {month} {date} {year}"),
            2 => format!("{time} GMT+0000 (Coordinated Universal Time)"),
            _ => format!("{day}, {date} {month} {year} {time} GMT"),
        }
    }
    for (name, part) in [("toString", 0u8), ("toDateString", 1), ("toTimeString", 2), ("toUTCString", 3), ("toGMTString", 3)] {
        def(i, &dp, name, move |i, t, _a| Ok(Value::Str(human(this_time(i, &t)?, part).into())));
    }
    def(i, &dp, "getYear", move |i, t, _a| {
        let f = i.get(&t, "getFullYear")?;
        let y = i.call(&f, t, vec![])?;
        Ok(Value::Num(i.to_number(&y)? - 1900.0))
    });
    def(i, &dp, "setYear", move |i, t, a| {
        let y = i.to_number(&arg(&a, 0))?;
        let y = if (0.0..=99.0).contains(&y.trunc()) { y.trunc() + 1900.0 } else { y };
        let f = i.get(&t, "setFullYear")?;
        i.call(&f, t, vec![Value::Num(y)])
    });
    def(i, &dp, "toJSON", move |i, t, _a| Ok(Value::Str(date_string(this_time(i, &t)?).into())));
    for (name, field) in [("getUTCFullYear", 0), ("getUTCMonth", 1), ("getUTCDate", 2), ("getUTCHours", 3), ("getUTCMinutes", 4), ("getUTCSeconds", 5), ("getUTCDay", 6)] {
        for n in [name.to_string(), name.replace("UTC", "")] {
            def(i, &dp, &n, move |i, t, _a| {
                let ms = this_time(i, &t)?;
                let s = date_string(ms);
                let f = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<f64>().ok()).unwrap_or(f64::NAN);
                Ok(Value::Num(match field {
                    0 => f(0, 4),
                    1 => f(5, 7) - 1.0,
                    2 => f(8, 10),
                    3 => f(11, 13),
                    4 => f(14, 16),
                    5 => f(17, 19),
                    _ => ((ms / 86_400_000.0).floor() + 4.0).rem_euclid(7.0),
                }))
            });
        }
    }

    // ---------------------------------------------------------- URIs
    for (name, keep) in [("encodeURIComponent", "-_.!~*'()"), ("encodeURI", "-_.!~*'();/?:@&=+$,#")] {
        let f = i.native(move |i, _t, a| {
            let s = i.to_str(&arg(&a, 0))?;
            let mut out = String::new();
            for b in s.bytes() {
                if b.is_ascii_alphanumeric() || keep.as_bytes().contains(&b) {
                    out.push(b as char);
                } else {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            Ok(Value::Str(out.into()))
        });
        global(i, name, f);
    }
    for name in ["decodeURIComponent", "decodeURI"] {
        let f = i.native(|i, _t, a| {
            let s = i.to_str(&arg(&a, 0))?;
            let b = s.as_bytes();
            let mut out = Vec::new();
            let mut k = 0;
            while k < b.len() {
                if b[k] == b'%' {
                    let hex = s.get(k + 1..k + 3).and_then(|h| u8::from_str_radix(h, 16).ok());
                    let Some(v) = hex else { return i.throw("URIError", "malformed URI sequence") };
                    out.push(v);
                    k += 3;
                } else {
                    out.push(b[k]);
                    k += 1;
                }
            }
            match String::from_utf8(out) {
                Ok(t) => Ok(Value::Str(t.into())),
                Err(_) => i.throw("URIError", "malformed URI sequence"),
            }
        });
        global(i, name, f);
    }

    // Promises, timers, iterators, Symbols, regular expressions.
    crate::promise::install(i);
    crate::proxy::install(i);
    crate::regexp::install(i);
    crate::typed::install(i);

    // ---------------------------------------------------------- misc
    let structured = i.native(|i, _t, a| {
        // A deep copy through JSON (enough for plain data).
        let mut seen = Vec::new();
        match json_stringify(i, &arg(&a, 0), "", "", &mut seen)? {
            Some(s) => Ok(volant_core::json::parse(&s).map(|v| from_json(i, &v)).unwrap_or(Value::Undefined)),
            None => Ok(Value::Undefined),
        }
    });
    global(i, "structuredClone", structured);
    for (name, base) in [("WeakMap", "Map"), ("WeakSet", "Set"), ("WeakRef", "Object")] {
        let v = i.global_obj.borrow().get_own(base).and_then(|s| if let Slot::Data(v) = s { Some(v.clone()) } else { None });
        if let Some(v) = v {
            global(i, name, v);
        }
    }

    // Silence the unused-callable lint for Callable (used by interp's matches).
    let _ = |c: &Callable| matches!(c, Callable::Native(_));
    let _ = to_i32;
    let _ = str_to_number;
}

/// Days from 1970-01-01 to a civil date (month 1–12).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// ms since the epoch from (year, month 0–11, day, h, m, s, ms), any of them overflowing.
fn make_time(p: [f64; 7]) -> f64 {
    if p.iter().any(|x| !x.is_finite()) {
        return f64::NAN;
    }
    let y = p[0].trunc() as i64 + (p[1].trunc() as i64).div_euclid(12);
    let m = (p[1].trunc() as i64).rem_euclid(12) + 1;
    let days = days_from_civil(y, m, 1) as f64 + p[2].trunc() - 1.0;
    days * 86_400_000.0 + p[3].trunc() * 3_600_000.0 + p[4].trunc() * 60_000.0 + p[5].trunc() * 1000.0 + p[6].trunc()
}

/// (year, month 0–11, day, h, m, s, ms) of a time.
fn time_parts(ms: f64) -> [f64; 7] {
    let s = date_string(ms);
    let f = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<f64>().ok()).unwrap_or(f64::NAN);
    [f(0, 4), f(5, 7) - 1.0, f(8, 10), f(11, 13), f(14, 16), f(17, 19), ms.rem_euclid(1000.0)]
}

/// `Date.parse` for the ISO forms (`2026-11-12`, `2026-11-12T06:47:13Z`).
fn parse_date(s: &str) -> f64 {
    let t = s.trim();
    let num = |a: usize, b: usize| t.get(a..b).and_then(|x| x.parse::<i64>().ok());
    let (Some(y), Some(m), Some(d)) = (num(0, 4), num(5, 7), num(8, 10)) else { return f64::NAN };
    let (hh, mm, ss) = (num(11, 13).unwrap_or(0), num(14, 16).unwrap_or(0), num(17, 19).unwrap_or(0));
    // Days from the civil date (inverse of date_string's algorithm).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    ((days * 86_400 + hh * 3600 + mm * 60 + ss) * 1000) as f64
}
