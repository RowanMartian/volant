//! The standard library: Object, Function, Array, String, Number, Boolean,
//! Math, JSON, Error types, Map, Set, Date, console and the global functions.

use crate::interp::{num_to_string, str_to_number, to_i32, to_u32, Callable, Interp, Kind, Obj, Res, Slot, Value};
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
            Some(Slot::Accessor(a, b)) => (a.map(Value::Obj), b.map(Value::Obj)),
            _ => (None, None),
        };
        if has(i, "get")? {
            og = Interp::is_callable(&g).then_some(g);
        }
        if has(i, "set")? {
            os = Interp::is_callable(&s).then_some(s);
        }
        o.borrow_mut().set_slot(k, Slot::accessor(og, os));
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
                Kind::Typed(t) if !t.data_view => return Ok(t.value_list()),
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
            if let Kind::RegExp(_, sf) = &o.borrow().kind {
                return format!("/{}/{}", sf.0, sf.1);
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

/// The structured clone algorithm (`structuredClone`, `postMessage`):
/// a deep copy keeping shared and cyclic references, Maps, Sets, Dates,
/// RegExps, buffers and errors; functions and symbols cannot be cloned.
pub fn structured_clone(i: &mut Interp, v: &Value, memo: &mut Vec<(Obj, Value)>) -> Res<Value> {
    let o = match v {
        Value::Obj(o) => o.clone(),
        Value::Str(s) if s.starts_with("\0@") => return Err(i.error("DataCloneError", "Symbol() could not be cloned.")),
        _ => return Ok(v.clone()),
    };
    if let Some((_, c)) = memo.iter().find(|(k, _)| Rc::ptr_eq(k, &o)) {
        return Ok(c.clone());
    }
    enum Shape {
        Array(usize),
        Plain,
        Error,
        Map(Vec<(Value, Value)>),
        Set(Vec<Value>),
        Done(Value),
        Typed(crate::typed::View),
    }
    // Platform objects the prelude makes serializable carry a hidden cloner.
    if matches!(o.borrow().kind, Kind::Plain) {
        let cloner = i.get(v, "\0clone")?;
        if Interp::is_callable(&cloner) {
            let c = i.call(&cloner, v.clone(), vec![])?;
            memo.push((o, c.clone()));
            return Ok(c);
        }
    }
    let shape = match &o.borrow().kind {
        Kind::Array(a) => Shape::Array(a.len()),
        Kind::Plain => Shape::Plain,
        Kind::Error => Shape::Error,
        Kind::Map(t) => Shape::Map(t.iter().cloned().collect()),
        Kind::Set(t) => Shape::Set(t.keys().cloned().collect()),
        Kind::Date(t) => Shape::Done(i.obj(Kind::Date(*t))),
        Kind::Boxed(p) => Shape::Done(i.obj(Kind::Boxed(p.clone()))),
        Kind::RegExp(re, sf) => Shape::Done(i.obj(Kind::RegExp(re.clone(), sf.clone()))),
        Kind::Buffer(b) => Shape::Done(crate::typed::new_buffer(i, b.clone())),
        Kind::Typed(view) => Shape::Typed(view.clone()),
        Kind::Function(_) => return Err(i.error("DataCloneError", "function could not be cloned.")),
        _ => return Err(i.error("DataCloneError", "The object could not be cloned.")),
    };
    let out = match shape {
        Shape::Done(c) => {
            memo.push((o, c.clone()));
            return Ok(c);
        }
        Shape::Typed(view) => {
            let buffer = structured_clone(i, &Value::Obj(view.buffer.clone()), memo)?;
            let Value::Obj(buffer) = buffer else { return Ok(Value::Undefined) };
            let proto = o.borrow().proto.clone();
            let c = i.obj(Kind::Typed(crate::typed::View { buffer, ..view }));
            if let Value::Obj(co) = &c {
                co.borrow_mut().proto = proto;
            }
            memo.push((o, c.clone()));
            return Ok(c);
        }
        Shape::Array(n) => {
            let c = i.array(Vec::with_capacity(n));
            memo.push((o.clone(), c.clone()));
            let items = items(i, v)?;
            let mut out = Vec::with_capacity(items.len());
            for x in &items {
                out.push(structured_clone(i, x, memo)?);
            }
            if let Value::Obj(co) = &c {
                co.borrow_mut().kind = Kind::Array(out);
            }
            c
        }
        Shape::Map(entries) => {
            let c = i.obj(Kind::Map(Default::default()));
            memo.push((o.clone(), c.clone()));
            let mut cloned = Vec::with_capacity(entries.len());
            for (k, x) in &entries {
                cloned.push((structured_clone(i, k, memo)?, structured_clone(i, x, memo)?));
            }
            if let Value::Obj(co) = &c {
                if let Kind::Map(t) = &mut co.borrow_mut().kind {
                    for (k, x) in cloned {
                        t.set(k, x);
                    }
                }
            }
            return Ok(c);
        }
        Shape::Set(keys) => {
            let c = i.obj(Kind::Set(Default::default()));
            memo.push((o.clone(), c.clone()));
            let mut cloned = Vec::with_capacity(keys.len());
            for k in &keys {
                cloned.push(structured_clone(i, k, memo)?);
            }
            if let Value::Obj(co) = &c {
                if let Kind::Set(t) = &mut co.borrow_mut().kind {
                    for k in cloned {
                        t.set(k.clone(), k);
                    }
                }
            }
            return Ok(c);
        }
        Shape::Error => {
            let c = i.obj(Kind::Error);
            memo.push((o.clone(), c.clone()));
            let name = i.get(v, "name")?;
            let name = i.to_str(&name)?;
            // The clone keeps the standard error type.
            let proto = ["EvalError", "RangeError", "ReferenceError", "SyntaxError", "TypeError", "URIError"]
                .contains(&&*name)
                .then(|| i.global_obj.borrow().get_own(&name).cloned())
                .flatten();
            if let (Value::Obj(co), Some(Slot::Data(ctor))) = (&c, proto) {
                let p = i.get(&ctor, "prototype")?;
                if let Value::Obj(p) = p {
                    co.borrow_mut().proto = Some(p);
                }
            }
            for k in ["message", "stack", "cause"] {
                if o.borrow().get_own(k).is_some() {
                    let x = i.get(v, k)?;
                    let x = structured_clone(i, &x, memo)?;
                    i.set(&c, k, x)?;
                }
            }
            return Ok(c);
        }
        Shape::Plain => {
            let c = i.obj(Kind::Plain);
            memo.push((o.clone(), c.clone()));
            c
        }
    };
    // Own enumerable string-keyed properties (arrays' too, past their items).
    let is_array = matches!(o.borrow().kind, Kind::Array(_));
    for k in i.keys_of(v)? {
        if is_array && k.parse::<usize>().is_ok() {
            continue;
        }
        let x = i.get(v, &k)?;
        if Interp::is_callable(&x) && !is_array {
            return Err(i.error("DataCloneError", &format!("{k} could not be cloned.")));
        }
        let x = structured_clone(i, &x, memo)?;
        i.set(&out, &k, x)?;
    }
    Ok(out)
}

/// Browser-style text, in local time: "Fri Nov 13 2026 10:00:00 GMT-0500
/// (Eastern Standard Time)"; part 1 the date, 2 the time, 3 the UTC form.
fn human(ms: f64, part: u8) -> String {
    if ms.is_nan() {
        return "Invalid Date".into();
    }
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let off = if part == 3 { 0.0 } else { crate::tz::local_offset(ms) };
    let f = fields(ms + off);
    let y = f[0] as i64;
    let year = if y < 0 { format!("-{:06}", -y) } else { format!("{y:04}") };
    let (day, month, date) = (DAYS[f[7] as usize], MONTHS[f[1] as usize], f[2] as i64);
    let time = format!("{:02}:{:02}:{:02}", f[3] as i64, f[4] as i64, f[5] as i64);
    let mins = (off / 60_000.0) as i64;
    let zone = || {
        let abbr = crate::tz::local_abbr(ms);
        let name = match abbr.as_str() {
            "UTC" | "GMT" if mins == 0 => "Coordinated Universal Time".to_string(),
            "PST" => "Pacific Standard Time".into(),
            "PDT" => "Pacific Daylight Time".into(),
            "MST" => "Mountain Standard Time".into(),
            "MDT" => "Mountain Daylight Time".into(),
            "CST" => "Central Standard Time".into(),
            "CDT" => "Central Daylight Time".into(),
            "EST" => "Eastern Standard Time".into(),
            "EDT" => "Eastern Daylight Time".into(),
            "AKST" => "Alaska Standard Time".into(),
            "AKDT" => "Alaska Daylight Time".into(),
            "HST" => "Hawaii-Aleutian Standard Time".into(),
            "AST" => "Atlantic Standard Time".into(),
            "ADT" => "Atlantic Daylight Time".into(),
            "NST" => "Newfoundland Standard Time".into(),
            "NDT" => "Newfoundland Daylight Time".into(),
            "GMT" => "Greenwich Mean Time".into(),
            "BST" => "British Summer Time".into(),
            "IST" if mins == 60 => "Irish Standard Time".into(),
            "IST" => "India Standard Time".into(),
            "WET" => "Western European Standard Time".into(),
            "WEST" => "Western European Summer Time".into(),
            "CET" => "Central European Standard Time".into(),
            "CEST" => "Central European Summer Time".into(),
            "EET" => "Eastern European Standard Time".into(),
            "EEST" => "Eastern European Summer Time".into(),
            "MSK" => "Moscow Standard Time".into(),
            "JST" => "Japan Standard Time".into(),
            "KST" => "Korean Standard Time".into(),
            "AEST" => "Australian Eastern Standard Time".into(),
            "AEDT" => "Australian Eastern Daylight Time".into(),
            "ACST" => "Australian Central Standard Time".into(),
            "AWST" => "Australian Western Standard Time".into(),
            "NZST" => "New Zealand Standard Time".into(),
            "NZDT" => "New Zealand Daylight Time".into(),
            _ => format!("GMT{}{:02}:{:02}", if mins < 0 { '-' } else { '+' }, mins.abs() / 60, mins.abs() % 60),
        };
        format!("GMT{}{:02}{:02} ({name})", if mins < 0 { '-' } else { '+' }, mins.abs() / 60, mins.abs() % 60)
    };
    match part {
        0 => format!("{day} {month} {date:02} {year} {time} {}", zone()),
        1 => format!("{day} {month} {date:02} {year}"),
        2 => format!("{time} {}", zone()),
        _ => format!("{day}, {date:02} {month} {year} {time} GMT"),
    }
}

/// The ISO 8601 form of a time in ms since the epoch (UTC), as `toISOString` gives.
pub fn date_string(t: f64) -> String {
    if !t.is_finite() {
        return "Invalid Date".into();
    }
    let f = fields(t);
    let y = f[0] as i64;
    let year = if (0..=9999).contains(&y) { format!("{y:04}") } else if y < 0 { format!("-{:06}", -y) } else { format!("+{y:06}") };
    format!("{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", f[1] as i64 + 1, f[2] as i64, f[3] as i64, f[4] as i64, f[5] as i64, f[6] as i64)
}

/// (year, month 0–11, day, hours, minutes, seconds, ms, weekday) of a time, in UTC.
pub fn fields(t: f64) -> [f64; 8] {
    if !t.is_finite() {
        return [f64::NAN; 8];
    }
    let ms_total = t.floor() as i64;
    let days = ms_total.div_euclid(86_400_000);
    let rem = ms_total.rem_euclid(86_400_000);
    let (y, m, d) = civil(days);
    let secs = rem / 1000;
    [y as f64, (m - 1) as f64, d as f64, (secs / 3600) as f64, (secs % 3600 / 60) as f64, (secs % 60) as f64, (rem % 1000) as f64, (days + 4).rem_euclid(7) as f64]
}

/// The civil date (year, month 1–12, day) of a day count from 1970-01-01.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

/// The year a time (seconds since 1970, UTC) falls in.
pub fn year_of_secs(t: i64) -> i64 {
    civil(t.div_euclid(86_400)).0
}

/// [`days_from_civil`] for the time-zone rules.
pub fn days_from_civil_pub(y: i64, m: i64, d: i64) -> i64 {
    days_from_civil(y, m, d)
}

/// Times past ±100,000,000 days are invalid (TimeClip).
fn clip(t: f64) -> f64 {
    if t.is_finite() && t.abs() <= 8.64e15 {
        t.trunc() + 0.0
    } else {
        f64::NAN
    }
}

fn json_stringify(i: &mut Interp, v: &Value, indent: &str, cur: &str, seen: &mut Vec<*const RefCell<crate::interp::Object>>) -> Res<Option<String>> {
    Ok(match v {
        Value::Undefined => None,
        Value::Null => Some("null".into()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Num(n) => Some(if n.is_finite() { num_to_string(*n) } else { "null".into() }),
        Value::Str(s) => Some(volant_core::json::string(s).replace("\\u003c", "<")),
        Value::BigInt(_) => {
            let to_json = i.get(v, "toJSON")?;
            if !Interp::is_callable(&to_json) {
                return i.throw("TypeError", "BigInt value can't be serialized in JSON");
            }
            let r = i.call(&to_json, v.clone(), vec![])?;
            return json_stringify(i, &r, indent, cur, seen);
        }
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
                Value::BigInt(_) => i.bigint_proto.clone(),
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
            Value::BigInt(_) => "BigInt".into(),
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
                        Kind::Proxy(t, _) if matches!(&**t, Value::Obj(x) if matches!(x.borrow().kind, Kind::Array(_))) => "Array".into(),
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
    def(i, &object, "is", |_i, _t, a| {
        // SameValue: NaN is NaN, and 0 isn't -0.
        Ok(Value::Bool(match (arg(&a, 0), arg(&a, 1)) {
            (Value::Num(x), Value::Num(y)) => (x.is_nan() && y.is_nan()) || (x == y && x.is_sign_negative() == y.is_sign_negative()),
            (x, y) => Interp::strict_eq(&x, &y),
        }))
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
                i.set(&d, "get", g.map_or(Value::Undefined, Value::Obj))?;
                i.set(&d, "set", s.map_or(Value::Undefined, Value::Obj))?;
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
        let (src, class_ctor) = match &t {
            Value::Obj(o) => match &o.borrow().kind {
                Kind::Function(Callable::Closure(c)) => (c.code.src.clone(), c.code.class_ctor),
                Kind::Function(_) => (None, false),
                _ => return i.throw("TypeError", "Function.prototype.toString requires that 'this' be a Function"),
            },
            _ => return i.throw("TypeError", "Function.prototype.toString requires that 'this' be a Function"),
        };
        if let Some(src) = src {
            return Ok(Value::Str(src.text().into()));
        }
        let n = i.get(&t, "name")?;
        let n = i.to_str(&n)?;
        Ok(Value::Str(if class_ctor { format!("class {n} {{ }}") } else { format!("function {n}() {{ [native code] }}") }.into()))
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
    /// An array-like's `length` (for the generic push and pop).
    fn like_len(i: &mut Interp, t: &Value) -> Res<usize> {
        let len = i.get(t, "length")?;
        let n = i.to_number(&len)?;
        Ok(if n.is_finite() && n > 0.0 { (n as usize).min(1 << 53) } else { 0 })
    }
    fn is_array(t: &Value) -> bool {
        matches!(t, Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_)))
    }
    // On array-likes these touch only the end (copying the whole object each
    // time made building one with push.apply quadratic).
    def(i, &ap, "push", |i, t, a| {
        if is_array(&t) {
            return with_array(i, &t, |arr| {
                arr.extend(a);
                Value::Num(arr.len() as f64)
            });
        }
        let n = like_len(i, &t)?;
        let total = n + a.len();
        for (k, x) in a.into_iter().enumerate() {
            i.set(&t, &(n + k).to_string(), x)?;
        }
        i.set(&t, "length", Value::Num(total as f64))?;
        Ok(Value::Num(total as f64))
    });
    def(i, &ap, "pop", |i, t, _a| {
        if is_array(&t) {
            return with_array(i, &t, |arr| arr.pop().unwrap_or(Value::Undefined));
        }
        let n = like_len(i, &t)?;
        if n == 0 {
            i.set(&t, "length", Value::Num(0.0))?;
            return Ok(Value::Undefined);
        }
        let k = (n - 1).to_string();
        let v = i.get(&t, &k)?;
        i.delete_property(&t, &k)?;
        i.set(&t, "length", Value::Num((n - 1) as f64))?;
        Ok(v)
    });
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
        let s = rel(int_arg(i, &a, 1, 0.0)?, v.len());
        Ok(Value::Num(v[s..].iter().position(|y| Interp::strict_eq(&x, y)).map(|p| (p + s) as f64).unwrap_or(-1.0)))
    });
    def(i, &ap, "lastIndexOf", |i, t, a| {
        let v = items(i, &t)?;
        let x = arg(&a, 0);
        let n = if a.len() > 1 { int_arg(i, &a, 1, 0.0)? } else { v.len() as f64 - 1.0 };
        let k = if n < 0.0 { v.len() as f64 + n } else { n.min(v.len() as f64 - 1.0) };
        if k < 0.0 {
            return Ok(Value::Num(-1.0));
        }
        Ok(Value::Num(v[..=k as usize].iter().rposition(|y| Interp::strict_eq(&x, y)).map(|p| p as f64).unwrap_or(-1.0)))
    });
    def(i, &ap, "includes", |i, t, a| {
        let v = items(i, &t)?;
        let x = arg(&a, 0);
        let s = rel(int_arg(i, &a, 1, 0.0)?, v.len());
        Ok(Value::Bool(v[s..].iter().any(|y| Interp::same_value_zero(&x, y))))
    });
    def(i, &ap, "copyWithin", |i, t, a| {
        let len = items(i, &t)?.len();
        let to = rel(int_arg(i, &a, 0, 0.0)?, len);
        let from = rel(int_arg(i, &a, 1, 0.0)?, len);
        let end = rel(int_arg(i, &a, 2, len as f64)?, len);
        let count = end.saturating_sub(from).min(len - to);
        if count > 0 {
            with_array(i, &t, |arr| {
                let part: Vec<Value> = arr[from..from + count].to_vec();
                arr[to..to + count].clone_from_slice(&part);
            })?;
        }
        Ok(t)
    });
    def(i, &ap, "toReversed", |i, t, _a| {
        let mut v = items(i, &t)?;
        v.reverse();
        Ok(i.array(v))
    });
    def(i, &ap, "toSorted", |i, t, a| {
        let cmp = arg(&a, 0);
        if !matches!(cmp, Value::Undefined) && !Interp::is_callable(&cmp) {
            return i.throw("TypeError", "The comparison function must be either a function or undefined");
        }
        let mut v = items(i, &t)?;
        sort_values(i, &mut v, &cmp)?;
        Ok(i.array(v))
    });
    def(i, &ap, "toSpliced", |i, t, a| {
        let mut v = items(i, &t)?;
        let len = v.len();
        let s = rel(int_arg(i, &a, 0, 0.0)?, len);
        let count = match a.len() {
            0 => 0,
            1 => len - s,
            _ => (int_arg(i, &a, 1, 0.0)?.max(0.0) as usize).min(len - s),
        };
        v.splice(s..s + count, a.into_iter().skip(2));
        Ok(i.array(v))
    });
    def(i, &ap, "with", |i, t, a| {
        let mut v = items(i, &t)?;
        let n = int_arg(i, &a, 0, 0.0)?;
        let k = if n < 0.0 { v.len() as f64 + n } else { n };
        if k < 0.0 || k >= v.len() as f64 {
            return i.throw("RangeError", "Invalid index");
        }
        v[k as usize] = arg(&a, 1);
        Ok(i.array(v))
    });
    def(i, &ap, "toLocaleString", |i, t, _a| {
        let v = items(i, &t)?;
        let mut parts = Vec::with_capacity(v.len());
        for x in v {
            if matches!(x, Value::Undefined | Value::Null) {
                parts.push(String::new());
                continue;
            }
            let f = i.get(&x, "toLocaleString")?;
            let r = if Interp::is_callable(&f) { i.call(&f, x, vec![])? } else { x };
            parts.push(i.to_str(&r)?.to_string());
        }
        Ok(Value::Str(parts.join(",").into()))
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
    for (name, want_index) in [("findLast", false), ("findLastIndex", true)] {
        def(i, &ap, name, move |i, t, a| {
            let v = items(i, &t)?;
            let cb = arg(&a, 0);
            if !Interp::is_callable(&cb) {
                return i.throw("TypeError", "callback is not a function");
            }
            let this = arg(&a, 1);
            for (k, x) in v.iter().enumerate().rev() {
                let r = i.call(&cb, this.clone(), vec![x.clone(), Value::Num(k as f64), t.clone()])?;
                if Interp::truthy(&r) {
                    return Ok(if want_index { Value::Num(k as f64) } else { x.clone() });
                }
            }
            Ok(if want_index { Value::Num(-1.0) } else { Value::Undefined })
        });
    }
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
    def(i, &string, "fromCodePoint", |i, _t, a| {
        let mut out = String::new();
        for v in &a {
            let n = i.to_number(v)?;
            match (n.fract() == 0.0 && (0.0..=1_114_111.0).contains(&n)).then(|| char::from_u32(n as u32)) {
                Some(Some(c)) => out.push(c),
                Some(None) => out.push('\u{FFFD}'),
                None => {
                    let shown = i.to_str(v)?;
                    return i.throw("RangeError", &format!("Invalid code point {shown}"));
                }
            }
        }
        Ok(Value::Str(out.into()))
    });
    def(i, &string, "raw", |i, _t, a| {
        let strs = arg(&a, 0);
        let raw = i.get(&strs, "raw")?;
        let pieces = items(i, &raw)?;
        let mut out = String::new();
        for (k, p) in pieces.iter().enumerate() {
            out.push_str(&i.to_str(p)?);
            if k + 1 < pieces.len() {
                if let Some(v) = a.get(k + 1) {
                    out.push_str(&i.to_str(v)?);
                }
            }
        }
        Ok(Value::Str(out.into()))
    });
    def(i, &sp, "toString", |i, t, _a| Ok(Value::Str(this_str(i, &t)?)));
    def(i, &sp, "valueOf", |i, t, _a| Ok(Value::Str(this_str(i, &t)?)));
    def(i, &sp, "charAt", |i, t, a| {
        let s = this_str(i, &t)?;
        let k = int_arg(i, &a, 0, 0.0)?;
        Ok(if k < 0.0 {
            Value::Str("".into())
        } else {
            match crate::interp::string_at(&s, k as usize) {
                Value::Undefined => Value::Str("".into()),
                v => v,
            }
        })
    });
    def(i, &sp, "charCodeAt", |i, t, a| {
        let s = this_str(i, &t)?;
        let k = int_arg(i, &a, 0, 0.0)?;
        Ok(Value::Num(if k < 0.0 { f64::NAN } else { crate::interp::utf16_unit(&s, k as usize).map(|u| u as f64).unwrap_or(f64::NAN) }))
    });
    def(i, &sp, "codePointAt", |i, t, a| {
        let s = this_str(i, &t)?;
        let k = int_arg(i, &a, 0, 0.0)?;
        // The code point starting at UTF-16 unit k (a lone low half gives itself).
        let k = k.max(0.0) as usize;
        Ok(match crate::interp::utf16_unit(&s, k) {
            Some(hi @ 0xD800..=0xDBFF) => match crate::interp::utf16_unit(&s, k + 1) {
                Some(lo @ 0xDC00..=0xDFFF) => Value::Num((0x10000 + ((hi as u32 - 0xD800) << 10) + (lo as u32 - 0xDC00)) as f64),
                _ => Value::Num(hi as f64),
            },
            Some(u) => Value::Num(u as f64),
            None => Value::Undefined,
        })
    });
    // Positions are UTF-16 indexes, as JavaScript counts them.
    use crate::interp::{byte_at16, index16, slice16, utf16_len};
    let pos_arg = |i: &mut Interp, a: &[Value], k: usize, dflt: f64, len: usize| -> Res<usize> { Ok(int_arg(i, a, k, dflt)?.clamp(0.0, len as f64) as usize) };
    def(i, &sp, "indexOf", move |i, t, a| {
        let s = this_str(i, &t)?;
        let n = i.to_str(&arg(&a, 0))?;
        let from = pos_arg(i, &a, 1, 0.0, utf16_len(&s))?;
        let start = byte_at16(&s, from);
        Ok(Value::Num(s[start..].find(&*n).map(|p| index16(&s, start + p) as f64).unwrap_or(-1.0)))
    });
    def(i, &sp, "lastIndexOf", move |i, t, a| {
        let s = this_str(i, &t)?;
        let n = i.to_str(&arg(&a, 0))?;
        let len = utf16_len(&s);
        let from = match arg(&a, 1) {
            Value::Undefined => len,
            v => {
                let f = i.to_number(&v)?;
                if f.is_nan() { len } else { f.clamp(0.0, len as f64) as usize }
            }
        };
        Ok(Value::Num(s.rmatch_indices(&*n).map(|(p, _)| index16(&s, p)).find(|&p| p <= from).map(|p| p as f64).unwrap_or(-1.0)))
    });
    def(i, &sp, "includes", move |i, t, a| {
        let s = this_str(i, &t)?;
        let nv = arg(&a, 0);
        if crate::regexp::regex_of(&nv).is_some() {
            return i.throw("TypeError", "the first argument to includes can't be a regular expression");
        }
        let n = i.to_str(&nv)?;
        let from = pos_arg(i, &a, 1, 0.0, utf16_len(&s))?;
        Ok(Value::Bool(s[byte_at16(&s, from)..].contains(&*n)))
    });
    def(i, &sp, "startsWith", move |i, t, a| {
        let s = this_str(i, &t)?;
        let n = i.to_str(&arg(&a, 0))?;
        let from = pos_arg(i, &a, 1, 0.0, utf16_len(&s))?;
        Ok(Value::Bool(s[byte_at16(&s, from)..].starts_with(&*n)))
    });
    def(i, &sp, "endsWith", move |i, t, a| {
        let s = this_str(i, &t)?;
        let n = i.to_str(&arg(&a, 0))?;
        let len = utf16_len(&s);
        let end = pos_arg(i, &a, 1, len as f64, len)?;
        Ok(Value::Bool(s[..byte_at16(&s, end)].ends_with(&*n)))
    });
    for name in ["slice", "substring", "substr"] {
        def(i, &sp, name, move |i, t, a| {
            let s = this_str(i, &t)?;
            let len = utf16_len(&s);
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
            Ok(Value::Str(slice16(&s, start, end)))
        });
    }
    def(i, &sp, "toUpperCase", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.to_uppercase().into())));
    def(i, &sp, "toLowerCase", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.to_lowercase().into())));
    def(i, &sp, "toLocaleUpperCase", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.to_uppercase().into())));
    def(i, &sp, "toLocaleLowerCase", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.to_lowercase().into())));
    def(i, &sp, "trim", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.trim().into())));
    def(i, &sp, "trimStart", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.trim_start().into())));
    def(i, &sp, "normalize", |i, t, a| {
        let s = this_str(i, &t)?;
        let form = if matches!(arg(&a, 0), Value::Undefined) { "NFC".into() } else { i.to_str(&arg(&a, 0))? };
        let compose = match &*form {
            "NFC" | "NFKC" => true,
            "NFD" | "NFKD" => false,
            _ => return i.throw("RangeError", "The normalization form should be one of NFC, NFD, NFKC, NFKD."),
        };
        if s.is_ascii() {
            return Ok(Value::Str(s));
        }
        Ok(Value::Str(crate::normalize::normalize(&s, compose).into()))
    });
    def(i, &sp, "isWellFormed", |i, t, _a| {
        this_str(i, &t)?;
        Ok(Value::Bool(true))
    });
    def(i, &sp, "toWellFormed", |i, t, _a| Ok(Value::Str(this_str(i, &t)?)));
    def(i, &sp, "trimEnd", |i, t, _a| Ok(Value::Str(this_str(i, &t)?.trim_end().into())));
    for (name, start) in [("padStart", true), ("padEnd", false)] {
        def(i, &sp, name, move |i, t, a| {
            let s = this_str(i, &t)?;
            let target = int_arg(i, &a, 0, 0.0)?.max(0.0) as usize;
            let fill = match arg(&a, 1) {
                Value::Undefined => " ".into(),
                f => i.to_str(&f)?,
            };
            let len = utf16_len(&s);
            if target <= len || fill.is_empty() {
                return Ok(Value::Str(s));
            }
            // The filler repeated to exactly the missing number of UTF-16 units.
            let mut pad = String::new();
            let mut have = 0;
            for c in fill.chars().cycle() {
                let w = c.len_utf16();
                if have + w > target - len {
                    if have < target - len {
                        pad.push('\u{FFFD}');
                    }
                    break;
                }
                pad.push(c);
                have += w;
            }
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
        let n = int_arg(i, &a, 0, 0.0)?;
        let k = if n < 0.0 { utf16_len(&s) as f64 + n } else { n };
        Ok(if k >= 0.0 { crate::interp::string_at(&s, k as usize) } else { Value::Undefined })
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
                    let r = i.call(&with, Value::Undefined, vec![Value::Str(pat.clone()), Value::Num(index16(&s, offset + p) as f64), Value::Str(s.clone())])?;
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
    let number = ctor(i, "Number", &np, |i, _t, a| {
        Ok(Value::Num(match a.first() {
            None => 0.0,
            Some(v) => match i.to_numeric(v)? {
                Value::BigInt(b) => b.to_f64(),
                v => i.to_number(&v)?,
            },
        }))
    });
    number.borrow_mut().set_own("MAX_SAFE_INTEGER", Value::Num(9007199254740991.0));
    number.borrow_mut().set_own("MIN_SAFE_INTEGER", Value::Num(-9007199254740991.0));
    number.borrow_mut().set_own("EPSILON", Value::Num(f64::EPSILON));
    number.borrow_mut().set_own("MAX_VALUE", Value::Num(f64::MAX));
    number.borrow_mut().set_own("MIN_VALUE", Value::Num(5e-324));
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
        Ok(Value::Str(if n.abs() >= 1e21 || !n.is_finite() { num_to_string(n) } else { to_fixed(n, d) }.into()))
    });
    def(i, &np, "toString", move |i, t, a| {
        let n = this_num(i, &t)?;
        let radix = int_arg(i, &a, 0, 10.0)? as u32;
        if !(2..=36).contains(&radix) {
            return i.throw("RangeError", "toString() radix must be between 2 and 36");
        }
        if radix == 10 || !n.is_finite() {
            return Ok(Value::Str(num_to_string(n).into()));
        }
        if n.fract() != 0.0 || n.abs() >= 1e30 {
            return Ok(Value::Str(radix_string(n, radix).into()));
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
    // ---------------------------------------------------------- BigInt
    let gp = i.bigint_proto.clone();
    let bigint = ctor(i, "BigInt", &gp, |i, _t, a| {
        let v = i.to_primitive(&arg(&a, 0), true)?;
        match v {
            Value::Num(n) => match crate::bigint::BigInt::from_f64(n) {
                Some(b) => Ok(Value::BigInt(Rc::new(b))),
                None => i.throw("RangeError", &format!("{} can't be converted to BigInt because it isn't an integer", num_to_string(n))),
            },
            v => Ok(Value::BigInt(i.to_bigint(&v)?)),
        }
    });
    fn this_bigint(i: &mut Interp, t: &Value) -> Res<Rc<crate::bigint::BigInt>> {
        match t {
            Value::BigInt(b) => Ok(b.clone()),
            Value::Obj(o) => match &o.borrow().kind {
                Kind::Boxed(Value::BigInt(b)) => Ok(b.clone()),
                _ => i.throw("TypeError", "BigInt.prototype method called on a non-BigInt"),
            },
            _ => i.throw("TypeError", "BigInt.prototype method called on a non-BigInt"),
        }
    }
    def(i, &gp, "toString", |i, t, a| {
        let b = this_bigint(i, &t)?;
        let radix = if matches!(arg(&a, 0), Value::Undefined) { 10.0 } else { i.to_number(&arg(&a, 0))?.trunc() };
        if !(2.0..=36.0).contains(&radix) {
            return i.throw("RangeError", "toString() radix must be between 2 and 36");
        }
        Ok(Value::Str(b.to_string_radix(radix as u32).into()))
    });
    def(i, &gp, "toLocaleString", |i, t, _a| {
        let b = this_bigint(i, &t)?;
        let s = b.to_string_radix(10);
        let (sign, digits) = s.split_at(if b.is_negative() { 1 } else { 0 });
        let mut out = String::from(sign);
        for (k, c) in digits.chars().enumerate() {
            if k > 0 && (digits.len() - k) % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        Ok(Value::Str(out.into()))
    });
    def(i, &gp, "valueOf", |i, t, _a| Ok(Value::BigInt(this_bigint(i, &t)?)));
    gp.borrow_mut().set_own("\0@toStringTag", Value::Str("BigInt".into()));
    for (name, signed) in [("asIntN", true), ("asUintN", false)] {
        let f = i.native(move |i, _t, a| {
            let bits = i.to_number(&arg(&a, 0))?;
            let bits = if bits.is_nan() { 0.0 } else { bits.trunc() };
            if !(0.0..=9007199254740991.0).contains(&bits) {
                return i.throw("RangeError", "invalid bit count");
            }
            let b = i.to_bigint(&arg(&a, 1))?;
            let r = if signed { b.as_int_n(bits as u64) } else { b.as_uint_n(bits as u64) };
            match r {
                Ok(v) => Ok(Value::BigInt(Rc::new(v))),
                Err(e) => i.throw("RangeError", e),
            }
        });
        bigint.borrow_mut().set_own(name, f);
    }

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
        ("sinh", f64::sinh),
        ("cosh", f64::cosh),
        ("tanh", f64::tanh),
        ("asinh", f64::asinh),
        ("acosh", f64::acosh),
        ("atanh", f64::atanh),
        ("expm1", f64::exp_m1),
        ("log1p", f64::ln_1p),
        ("clz32", |x: f64| to_u32(x).leading_zeros() as f64),
    ];
    for &(name, f) in unary {
        def(i, mo, name, move |i, _t, a| Ok(Value::Num(f(i.to_number(&arg(&a, 0))?))));
    }
    def(i, mo, "pow", |i, _t, a| Ok(Value::Num(i.to_number(&arg(&a, 0))?.powf(i.to_number(&arg(&a, 1))?))));
    def(i, mo, "imul", |i, _t, a| {
        let x = to_u32(i.to_number(&arg(&a, 0))?);
        let y = to_u32(i.to_number(&arg(&a, 1))?);
        Ok(Value::Num(x.wrapping_mul(y) as i32 as f64))
    });
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
        let Some(v) = volant_core::json::parse(&s) else { return i.throw("SyntaxError", "JSON.parse: not valid JSON") };
        let v = from_json(i, &v);
        let reviver = arg(&a, 1);
        if !Interp::is_callable(&reviver) {
            return Ok(v);
        }
        /// The reviver walk: children first, then the holder's own key.
        fn walk(i: &mut Interp, reviver: &Value, holder: &Value, key: Rc<str>) -> Res<Value> {
            let v = i.get(holder, &key)?;
            if let Value::Obj(o) = &v {
                let len = match &o.borrow().kind {
                    Kind::Array(a) => Some(a.len()),
                    _ => None,
                };
                let keys: Vec<Rc<str>> = match len {
                    Some(n) => (0..n).map(|k| k.to_string().into()).collect(),
                    None => i.keys_of(&v)?,
                };
                for k in keys {
                    let nv = walk(i, reviver, &v, k.clone())?;
                    if matches!(nv, Value::Undefined) {
                        i.delete_property(&v, &k)?;
                    } else {
                        i.set(&v, &k, nv)?;
                    }
                }
            }
            i.call(reviver, holder.clone(), vec![Value::Str(key), v])
        }
        let root = i.obj(Kind::Plain);
        i.set(&root, "", v)?;
        walk(i, &reviver, &root, "".into())
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
    let mut error_ctor: Option<Obj> = None;
    for name in ["Error", "TypeError", "RangeError", "SyntaxError", "ReferenceError", "EvalError", "URIError"] {
        let proto = if name == "Error" {
            ep.clone()
        } else {
            let p = Rc::new(RefCell::new(crate::interp::Object::new(Some(ep.clone()), Kind::Plain)));
            p.borrow_mut().set_own("name", Value::Str(name.into()));
            p
        };
        let proto2 = proto.clone();
        let made = ctor(i, name, &proto, move |i, _t, a| {
            let e = i.obj(Kind::Error);
            if let Value::Obj(o) = &e {
                o.borrow_mut().proto = Some(proto2.clone());
                if !matches!(arg(&a, 0), Value::Undefined) {
                    let m = i.to_str(&arg(&a, 0))?;
                    o.borrow_mut().set_own("message", Value::Str(m));
                }
                if let Value::Obj(opts) = arg(&a, 1) {
                    if opts.borrow().get_own("cause").is_some() {
                        let c = i.get(&Value::Obj(opts.clone()), "cause")?;
                        o.borrow_mut().set_own("cause", c);
                    }
                }
                o.borrow_mut().set_own("stack", Value::Str("".into()));
            }
            Ok(e)
        });
        // TypeError and the rest inherit from Error, as constructors too.
        match &error_ctor {
            None => error_ctor = Some(made),
            Some(base) => made.borrow_mut().proto = Some(base.clone()),
        }
    }

    // ---------------------------------------------------------- Map, Set
    let mp = i.map_proto.clone();
    ctor(i, "Map", &mp, |i, _t, a| {
        let m = i.obj(Kind::Map(Default::default()));
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
    fn map_op<T>(i: &Interp, t: &Value, f: impl FnOnce(&mut crate::table::Table) -> T) -> Res<T> {
        if let Value::Obj(o) = t {
            if let Kind::Map(m) = &mut o.borrow_mut().kind {
                return Ok(f(m));
            }
        }
        i.throw("TypeError", "not a Map")
    }
    def(i, &mp, "get", |i, t, a| {
        let k = arg(&a, 0);
        map_op(i, &t, |m| m.get(&k).cloned().unwrap_or(Value::Undefined))
    });
    def(i, &mp, "set", |i, t, a| {
        let (k, v) = (arg(&a, 0), arg(&a, 1));
        map_op(i, &t, |m| m.set(k, v))?;
        Ok(t)
    });
    def(i, &mp, "has", |i, t, a| {
        let k = arg(&a, 0);
        map_op(i, &t, |m| Value::Bool(m.has(&k)))
    });
    def(i, &mp, "delete", |i, t, a| {
        let k = arg(&a, 0);
        map_op(i, &t, |m| Value::Bool(m.delete(&k)))
    });
    def(i, &mp, "clear", |i, t, _a| map_op(i, &t, |m| {
        m.clear();
        Value::Undefined
    }));
    def(i, &mp, "forEach", |i, t, a| {
        let entries: Vec<(Value, Value)> = map_op(i, &t, |m| m.iter().cloned().collect())?;
        let cb = arg(&a, 0);
        for (k, v) in entries {
            i.call(&cb, Value::Undefined, vec![v, k, t.clone()])?;
        }
        Ok(Value::Undefined)
    });
    for (name, which) in [("keys", 0), ("values", 1), ("entries", 2)] {
        def(i, &mp, name, move |i, t, _a| {
            let entries: Vec<(Value, Value)> = map_op(i, &t, |m| m.iter().cloned().collect())?;
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
        let mut items = crate::table::Table::default();
        if !matches!(arg(&a, 0), Value::Undefined | Value::Null) {
            for v in i.iterate(&arg(&a, 0))? {
                items.set(v, Value::Undefined);
            }
        }
        Ok(i.obj(Kind::Set(Box::new(items))))
    });
    fn set_op<T>(i: &Interp, t: &Value, f: impl FnOnce(&mut crate::table::Table) -> T) -> Res<T> {
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
            if !s.has(&v) {
                s.set(v, Value::Undefined)
            }
        })?;
        Ok(t)
    });
    def(i, &setp, "has", |i, t, a| {
        let v = arg(&a, 0);
        set_op(i, &t, |s| Value::Bool(s.has(&v)))
    });
    def(i, &setp, "delete", |i, t, a| {
        let v = arg(&a, 0);
        set_op(i, &t, |s| Value::Bool(s.delete(&v)))
    });
    def(i, &setp, "clear", |i, t, _a| set_op(i, &t, |s| {
        s.clear();
        Value::Undefined
    }));
    def(i, &setp, "forEach", |i, t, a| {
        let items: Vec<Value> = set_op(i, &t, |s| s.keys().cloned().collect())?;
        let cb = arg(&a, 0);
        for v in items {
            i.call(&cb, Value::Undefined, vec![v.clone(), v, t.clone()])?;
        }
        Ok(Value::Undefined)
    });
    for name in ["values", "keys"] {
        def(i, &setp, name, |i, t, _a| {
            let items: Vec<Value> = set_op(i, &t, |s| s.keys().cloned().collect())?;
            Ok(i.array(items))
        });
    }
    def(i, &setp, "entries", |i, t, _a| {
        let items: Vec<Value> = set_op(i, &t, |s| s.keys().cloned().collect())?;
        let pairs = items.into_iter().map(|v| i.array(vec![v.clone(), v])).collect();
        Ok(i.array(pairs))
    });

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
                if (0.0..100.0).contains(&p[0].trunc()) {
                    p[0] = p[0].trunc() + 1900.0;
                }
                clip(crate::tz::local_to_utc(make_time(p)))
            }
            Some(Value::Num(n)) => clip(*n),
            Some(Value::Str(s)) => parse_date(s),
            Some(Value::Obj(o)) if matches!(o.borrow().kind, Kind::Date(_)) => match o.borrow().kind {
                Kind::Date(t) => t,
                _ => f64::NAN,
            },
            Some(v) => i.to_number(v)?,
        };
        if !is_new {
            return Ok(Value::Str(human(now(), 0).into()));
        }
        Ok(i.obj(Kind::Date(ms)))
    });
    def(i, &date, "now", move |_i, _t, _a| Ok(Value::Num(now())));
    def(i, &date, "UTC", |i, _t, a| {
        let mut p = [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        for (k, v) in a.iter().take(7).enumerate() {
            p[k] = i.to_number(v)?;
        }
        Ok(Value::Num(clip(make_time(p))))
    });
    def(i, &date, "parse", |i, _t, a| {
        let s = i.to_str(&arg(&a, 0))?;
        Ok(Value::Num(parse_date(&s)))
    });
    def(i, &dp, "setTime", |i, t, a| {
        let ms = clip(i.to_number(&arg(&a, 0))?);
        if let Value::Obj(o) = &t {
            if let Kind::Date(d) = &mut o.borrow_mut().kind {
                *d = ms;
                return Ok(Value::Num(ms));
            }
        }
        i.throw("TypeError", "not a Date")
    });
    // setFullYear … setMilliseconds, in local time, and their UTC twins.
    for (name, at) in [("FullYear", 0usize), ("Month", 1), ("Date", 2), ("Hours", 3), ("Minutes", 4), ("Seconds", 5), ("Milliseconds", 6)] {
        for (n, utc) in [(format!("set{name}"), false), (format!("setUTC{name}"), true)] {
            let most = match at {
                0 => 3,
                1 => 2,
                2 => 1,
                3 => 4,
                4 => 3,
                5 => 2,
                _ => 1,
            };
            def(i, &dp, &n, move |i, t, a| {
                let Value::Obj(o) = &t else { return i.throw("TypeError", "not a Date") };
                let Kind::Date(ms) = o.borrow().kind else { return i.throw("TypeError", "not a Date") };
                // setFullYear on an invalid date starts from +0.
                let ms = if ms.is_nan() && at == 0 { 0.0 } else { ms };
                let off = if utc { 0.0 } else { crate::tz::local_offset(ms) };
                let mut p = time_parts(ms + off);
                for (k, v) in a.iter().take(most).enumerate() {
                    p[at + k] = i.to_number(v)?;
                }
                if a.is_empty() {
                    p[at] = f64::NAN;
                }
                let new = clip(if utc { make_time(p) } else { crate::tz::local_to_utc(make_time(p)) });
                if let Kind::Date(d) = &mut o.borrow_mut().kind {
                    *d = new;
                }
                Ok(Value::Num(new))
            });
        }
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
    def(i, &dp, "getTimezoneOffset", move |i, t, _a| {
        let ms = this_time(i, &t)?;
        Ok(Value::Num(if ms.is_nan() { f64::NAN } else { -crate::tz::local_offset(ms) / 60_000.0 + 0.0 }))
    });
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
    for (name, field) in [("FullYear", 0usize), ("Month", 1), ("Date", 2), ("Hours", 3), ("Minutes", 4), ("Seconds", 5), ("Milliseconds", 6), ("Day", 7)] {
        for (n, utc) in [(format!("getUTC{name}"), true), (format!("get{name}"), false)] {
            def(i, &dp, &n, move |i, t, _a| {
                let ms = this_time(i, &t)?;
                let ms = if utc { ms } else { ms + crate::tz::local_offset(ms) };
                Ok(Value::Num(fields(ms)[field]))
            });
        }
    }

    // For Intl.DateTimeFormat: the local zone's name and other zones' offsets.
    let local_zone = i.native(|_i, _t, _a| Ok(Value::Str(crate::tz::local_name().into())));
    i.global_obj.borrow_mut().set_hidden("__localZone", local_zone);
    let zone_offset = i.native(|i, _t, a| {
        let name = i.to_str(&arg(&a, 0))?;
        let t = i.to_number(&arg(&a, 1))?;
        Ok(crate::tz::zone_offset_minutes(&name, t).map(Value::Num).unwrap_or(Value::Null))
    });
    i.global_obj.borrow_mut().set_hidden("__zoneOffset", zone_offset);

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
        let mut memo = Vec::new();
        structured_clone(i, &arg(&a, 0), &mut memo)
    });
    global(i, "structuredClone", structured);
    for (name, base) in [("WeakMap", "Map"), ("WeakSet", "Set"), ("WeakRef", "Object")] {
        let v = i.global_obj.borrow().get_own(base).and_then(|s| if let Slot::Data(v) = s { Some(v.clone()) } else { None });
        if let Some(v) = v {
            global(i, name, v);
        }
    }

    // Object.groupBy and Map.groupBy (ES2024).
    fn group(i: &mut Interp, a: &[Value]) -> Res<Vec<(Value, Vec<Value>)>> {
        let cb = arg(a, 1);
        if !Interp::is_callable(&cb) {
            return i.throw("TypeError", "callback is not a function");
        }
        let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
        for (k, x) in i.iterate(&arg(a, 0))?.into_iter().enumerate() {
            let key = i.call(&cb, Value::Undefined, vec![x.clone(), Value::Num(k as f64)])?;
            match groups.iter_mut().find(|(g, _)| Interp::same_value_zero(g, &key)) {
                Some((_, list)) => list.push(x),
                None => groups.push((key, vec![x])),
            }
        }
        Ok(groups)
    }
    def(i, &object, "groupBy", |i, _t, a| {
        let groups = group(i, &a)?;
        let o = i.obj(Kind::Plain);
        if let Value::Obj(ob) = &o {
            ob.borrow_mut().proto = None;
        }
        for (k, list) in groups {
            let k = i.key(&k)?;
            let arr = i.array(list);
            i.set(&o, &k, arr)?;
        }
        Ok(o)
    });
    let map_ctor = i.global_obj.borrow().get_own("Map").and_then(|s| if let Slot::Data(v) = s { Some(v.clone()) } else { None });
    if let Some(Value::Obj(mc)) = map_ctor {
        def(i, &mc, "groupBy", |i, _t, a| {
            let groups = group(i, &a)?;
            let m = i.obj(Kind::Map(Default::default()));
            let set = i.get(&m, "set")?;
            for (k, list) in groups {
                let arr = i.array(list);
                i.call(&set, m.clone(), vec![k, arr])?;
            }
            Ok(m)
        });
    }

    // Silence the unused-callable lint for Callable (used by interp's matches).
    let _ = |c: &Callable| matches!(c, Callable::Native(_));
    let _ = to_i32;
    let _ = str_to_number;
}

/// `toFixed`: rounded half away from zero on the exact value, as the spec
/// asks (Rust's formatting rounds ties to even).
fn to_fixed(n: f64, d: usize) -> String {
    let exact = format!("{:.*}", d + 30, n.abs());
    let (int, frac) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut digits: Vec<u8> = int.bytes().chain(frac.bytes().take(d)).map(|b| b - b'0').collect();
    let up = frac.as_bytes().get(d).is_some_and(|&b| b >= b'5');
    if up {
        let mut k = digits.len();
        loop {
            if k == 0 {
                digits.insert(0, 1);
                break;
            }
            k -= 1;
            if digits[k] == 9 {
                digits[k] = 0;
            } else {
                digits[k] += 1;
                break;
            }
        }
    }
    let int_len = digits.len() - d;
    let mut out = String::with_capacity(digits.len() + 2);
    if n < 0.0 {
        out.push('-');
    }
    for (k, x) in digits.iter().enumerate() {
        if k == int_len {
            out.push('.');
        }
        out.push((b'0' + x) as char);
    }
    out
}

/// A number in another base, fraction included (as `Math.random().toString(36)` uses).
fn radix_string(n: f64, radix: u32) -> String {
    let neg = n < 0.0;
    let a = n.abs();
    let mut int = a.trunc();
    let mut frac = a - int;
    let mut head = Vec::new();
    loop {
        let digit = (int % radix as f64) as u32;
        head.push(std::char::from_digit(digit, radix).unwrap_or('0'));
        int = (int / radix as f64).trunc();
        if int < 1.0 {
            break;
        }
    }
    let mut out: String = if neg { "-".into() } else { String::new() };
    out.extend(head.iter().rev());
    if frac > 0.0 {
        out.push('.');
        // Enough digits to tell the double apart, as engines print.
        let mut delta = 0.5 * (f64::EPSILON * a.max(f64::MIN_POSITIVE)).max(f64::from_bits(1));
        let mut k = 0;
        while frac >= delta && k < 1100 {
            frac *= radix as f64;
            delta *= radix as f64;
            let digit = frac.trunc() as u32;
            frac -= digit as f64;
            out.push(std::char::from_digit(digit.min(radix - 1), radix).unwrap_or('0'));
            k += 1;
        }
    }
    out
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
    let f = fields(ms);
    [f[0], f[1], f[2], f[3], f[4], f[5], f[6]]
}

/// `Date.parse` for the ISO forms (`2026-11-12`, `2026-11-12T06:47:13Z`).
fn parse_date(s: &str) -> f64 {
    let t = s.trim();
    iso_date(t).unwrap_or_else(|| loose_date(t))
}

/// The ECMAScript date-time string format: `[+-YY]YYYY[-MM[-DD]][THH:mm[:ss[.sss]]][Z|+-HH:mm]`.
/// (A date alone is UTC; a date and time without a zone, local time.)
fn iso_date(t: &str) -> Option<f64> {
    let b = t.as_bytes();
    let mut k = 0;
    let digits = |k: &mut usize, n: usize| -> Option<i64> {
        let part = t.get(*k..*k + n)?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        *k += n;
        part.parse().ok()
    };
    let year = match b.first()? {
        b'+' | b'-' => {
            let neg = b[0] == b'-';
            k = 1;
            let y = digits(&mut k, 6)?;
            if neg { -y } else { y }
        }
        _ => digits(&mut k, 4)?,
    };
    let (mut month, mut day) = (1, 1);
    if b.get(k) == Some(&b'-') {
        k += 1;
        month = digits(&mut k, 2)?;
        if b.get(k) == Some(&b'-') {
            k += 1;
            day = digits(&mut k, 2)?;
        }
    }
    let (mut h, mut mi, mut sec, mut ms) = (0, 0, 0, 0.0);
    let mut offset = 0;
    let (mut timed, mut zoned) = (false, false);
    if matches!(b.get(k), Some(b'T' | b't' | b' ')) {
        timed = true;
        k += 1;
        h = digits(&mut k, 2)?;
        if b.get(k) != Some(&b':') {
            return None;
        }
        k += 1;
        mi = digits(&mut k, 2)?;
        if b.get(k) == Some(&b':') {
            k += 1;
            sec = digits(&mut k, 2)?;
            if matches!(b.get(k), Some(b'.' | b',')) {
                k += 1;
                let from = k;
                while b.get(k).is_some_and(u8::is_ascii_digit) {
                    k += 1;
                }
                let frac = t.get(from..k)?;
                if frac.is_empty() {
                    return None;
                }
                ms = (format!("0.{frac}").parse::<f64>().ok()? * 1000.0).trunc();
            }
        }
        match b.get(k) {
            Some(b'Z' | b'z') => {
                k += 1;
                zoned = true;
            }
            Some(&c @ (b'+' | b'-')) => {
                k += 1;
                zoned = true;
                let oh = digits(&mut k, 2)?;
                if b.get(k) == Some(&b':') {
                    k += 1;
                }
                let om = digits(&mut k, 2).unwrap_or(0);
                offset = (oh * 60 + om) * if c == b'+' { 1 } else { -1 };
            }
            _ => {}
        }
    }
    if k != b.len() || !(1..=12).contains(&month) || !(1..=31).contains(&day) || h > 24 || mi > 59 || sec > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let t = ((days * 86_400 + h * 3600 + mi * 60 + sec - offset * 60) * 1000) as f64 + ms;
    Some(clip(if timed && !zoned { crate::tz::local_to_utc(t) } else { t }))
}

/// The forms browsers accept beyond ISO: `Thu, 01 Jan 1970 00:00:00 GMT`,
/// `Jan 2, 2020 10:00 PM`, `2020/01/02`, `1/2/2020`, `Tue Mar 05 2024 10:00:00 GMT+0100 (CET)`.
fn loose_date(t: &str) -> f64 {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    // Drop a parenthesised zone name.
    let t = match t.find('(') {
        Some(p) => &t[..p],
        None => t,
    };
    let (mut year, mut month, mut day): (Option<i64>, Option<i64>, Option<i64>) = (None, None, None);
    let (mut h, mut mi, mut sec, mut ms) = (0i64, 0i64, 0i64, 0f64);
    let mut offset = 0i64;
    let mut zoned = false;
    let mut pm: Option<bool> = None;
    let mut numbers: Vec<i64> = Vec::new();
    let lower = t.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while !rest.is_empty() {
        let c = rest.as_bytes()[0];
        if c.is_ascii_whitespace() || c == b',' {
            rest = &rest[1..];
            continue;
        }
        if c.is_ascii_alphabetic() {
            let end = rest.find(|ch: char| !ch.is_ascii_alphabetic()).unwrap_or(rest.len());
            let word = &rest[..end];
            rest = &rest[end..];
            if let Some(m) = MONTHS.iter().position(|m| word.starts_with(m)) {
                month = Some(m as i64 + 1);
            } else if word == "am" || word == "pm" {
                pm = Some(word == "pm");
            } else if matches!(word, "gmt" | "utc" | "ut" | "z") {
                // A following +hhmm is read as the offset below.
                zoned = true;
            } else if let Some(z) = match word { "est" => Some(-5), "edt" => Some(-4), "cst" => Some(-6), "cdt" => Some(-5), "mst" => Some(-7), "mdt" => Some(-6), "pst" => Some(-8), "pdt" => Some(-7), _ => None } {
                offset = z * 60;
                zoned = true;
            } else if !matches!(word, "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun" | "t")
                && !["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"].contains(&word)
            {
                return f64::NAN;
            }
            continue;
        }
        if (c == b'+' || c == b'-') && rest[1..].starts_with(|ch: char| ch.is_ascii_digit()) && (year.is_some() || numbers.len() >= 2 || h > 0 || mi > 0) {
            // A zone offset: +hhmm or +hh:mm.
            let body = &rest[1..];
            let end = body.find(|ch: char| !ch.is_ascii_digit() && ch != ':').unwrap_or(body.len());
            let d: String = body[..end].chars().filter(|ch| *ch != ':').collect();
            let v: i64 = d.parse().unwrap_or(0);
            let (oh, om) = if d.len() <= 2 { (v, 0) } else { (v / 100, v % 100) };
            offset = (oh * 60 + om) * if c == b'+' { 1 } else { -1 };
            zoned = true;
            rest = &body[end..];
            continue;
        }
        if c.is_ascii_digit() {
            let end = rest.find(|ch: char| !ch.is_ascii_digit()).unwrap_or(rest.len());
            let n: i64 = rest[..end].parse().unwrap_or(0);
            let after = &rest[end..];
            if after.starts_with(':') {
                // A time: hh:mm[:ss[.sss]].
                let mut parts = vec![n];
                let mut r = after;
                while let Some(x) = r.strip_prefix(':') {
                    let e = x.find(|ch: char| !ch.is_ascii_digit()).unwrap_or(x.len());
                    parts.push(x[..e].parse().unwrap_or(0));
                    r = &x[e..];
                }
                if let Some(x) = r.strip_prefix('.') {
                    let e = x.find(|ch: char| !ch.is_ascii_digit()).unwrap_or(x.len());
                    ms = format!("0.{}", &x[..e]).parse::<f64>().unwrap_or(0.0) * 1000.0;
                    r = &x[e..];
                }
                h = parts[0];
                mi = parts.get(1).copied().unwrap_or(0);
                sec = parts.get(2).copied().unwrap_or(0);
                rest = r;
                continue;
            }
            if after.starts_with('/') || (after.starts_with('-') && after[1..].starts_with(|ch: char| ch.is_ascii_digit())) {
                // A numeric date: y/m/d or m/d/y.
                let sep = after.as_bytes()[0] as char;
                let mut parts = vec![(n, end)];
                let mut r = after;
                while let Some(x) = r.strip_prefix(sep) {
                    let e = x.find(|ch: char| !ch.is_ascii_digit()).unwrap_or(x.len());
                    if e == 0 {
                        break;
                    }
                    parts.push((x[..e].parse().unwrap_or(0), e));
                    r = &x[e..];
                }
                if parts.len() == 3 {
                    if parts[0].1 >= 3 {
                        year = Some(parts[0].0);
                        month = Some(parts[1].0);
                        day = Some(parts[2].0);
                    } else {
                        month = Some(parts[0].0);
                        day = Some(parts[1].0);
                        year = Some(parts[2].0);
                    }
                } else if parts.len() == 2 {
                    month = Some(parts[0].0);
                    day = Some(parts[1].0);
                }
                rest = r;
                continue;
            }
            numbers.push(n);
            rest = after;
            continue;
        }
        if c == b'.' || c == b'/' || c == b'-' {
            rest = &rest[1..];
            continue;
        }
        return f64::NAN;
    }
    for n in numbers {
        if day.is_none() && (1..=31).contains(&n) && (month.is_some() || year.is_none()) && !(year.is_none() && n > 31) {
            day = Some(n);
        } else if year.is_none() {
            year = Some(if n < 50 { 2000 + n } else if n < 100 { 1900 + n } else { n });
        } else {
            return f64::NAN;
        }
    }
    let (Some(year), Some(month)) = (year, month) else { return f64::NAN };
    let day = day.unwrap_or(1);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || h > 24 || mi > 59 || sec > 59 {
        return f64::NAN;
    }
    match pm {
        Some(true) if h < 12 => h += 12,
        Some(false) if h == 12 => h = 0,
        _ => {}
    }
    let days = days_from_civil(year, month, day);
    let t = ((days * 86_400 + h * 3600 + mi * 60 + sec - offset * 60) * 1000) as f64 + ms.trunc();
    clip(if zoned { t } else { crate::tz::local_to_utc(t) })
}
