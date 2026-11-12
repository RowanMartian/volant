//! RegExp objects and the string methods that take them: `exec`, `test`,
//! `match`, `matchAll`, `search`, `replace`, `replaceAll` and `split`.

use crate::interp::{Interp, Kind, Obj, Res, Slot, Value};
use crate::regex::{Captures, Regex};
use std::rc::Rc;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).cloned().unwrap_or(Value::Undefined)
}

/// The compiled pattern behind a RegExp value.
pub fn regex_of(v: &Value) -> Option<(Rc<Regex>, Obj)> {
    match v {
        Value::Obj(o) => match &o.borrow().kind {
            Kind::RegExp(r, _, _) => Some((r.clone(), o.clone())),
            _ => None,
        },
        _ => None,
    }
}

fn def(i: &Interp, o: &Obj, name: &str, f: impl Fn(&mut Interp, Value, Vec<Value>) -> Res<Value> + 'static) {
    let fv = i.native(f);
    if let Value::Obj(fo) = &fv {
        fo.borrow_mut().set_own("name", Value::Str(name.into()));
    }
    o.borrow_mut().set_own(name, fv);
}

fn getter(i: &Interp, o: &Obj, name: &str, f: impl Fn(&mut Interp, Value, Vec<Value>) -> Res<Value> + 'static) {
    let fv = i.native(f);
    o.borrow_mut().set_slot(name, Slot::Accessor(Some(fv), None));
}

fn substr(chars: &[char], a: usize, b: usize) -> Value {
    Value::Str(chars[a.min(chars.len())..b.min(chars.len())].iter().collect::<String>().into())
}

/// The array `exec` returns: the match, its groups, `index`, `input` and `groups`.
fn match_array(i: &mut Interp, re: &Regex, caps: &Captures, chars: &[char], input: &Rc<str>) -> Res<Value> {
    let items: Vec<Value> = caps.iter().map(|c| c.map(|(a, b)| substr(chars, a, b)).unwrap_or(Value::Undefined)).collect();
    let arr = i.array(items);
    let start = caps[0].map(|c| c.0).unwrap_or(0);
    i.set(&arr, "index", Value::Num(start as f64))?;
    i.set(&arr, "input", Value::Str(input.clone()))?;
    let groups = if re.names.is_empty() {
        Value::Undefined
    } else {
        let g = i.obj(Kind::Plain);
        for (name, k) in &re.names {
            let v = caps.get(*k).copied().flatten().map(|(a, b)| substr(chars, a, b)).unwrap_or(Value::Undefined);
            i.set(&g, name, v)?;
        }
        g
    };
    i.set(&arr, "groups", groups)?;
    Ok(arr)
}

/// RegExp.prototype.exec, honouring `lastIndex` for global and sticky patterns.
fn exec(i: &mut Interp, re: &Regex, o: &Obj, input: &Rc<str>) -> Res<Option<(Captures, Vec<char>)>> {
    let chars: Vec<char> = input.chars().collect();
    let uses_last = re.global || re.sticky;
    let ov = Value::Obj(o.clone());
    let start = if uses_last {
        let l = i.get(&ov, "lastIndex")?;
        let n = i.to_number(&l)?;
        if n.is_nan() || n < 0.0 { 0 } else { n as usize }
    } else {
        0
    };
    if start > chars.len() {
        i.set(&ov, "lastIndex", Value::Num(0.0))?;
        return Ok(None);
    }
    match re.exec_at(&chars, start) {
        None => {
            if uses_last {
                i.set(&ov, "lastIndex", Value::Num(0.0))?;
            }
            Ok(None)
        }
        Some(caps) => {
            if uses_last {
                let end = caps[0].map(|c| c.1).unwrap_or(0);
                i.set(&ov, "lastIndex", Value::Num(end as f64))?;
            }
            Ok(Some((caps, chars)))
        }
    }
}

/// Every match from the start (for global replace, match and matchAll).
fn all_matches(re: &Regex, chars: &[char]) -> Vec<Captures> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos <= chars.len() {
        let Some(caps) = re.exec_at(chars, pos) else { break };
        let (a, b) = caps[0].unwrap_or((pos, pos));
        pos = if b == a { b + 1 } else { b };
        out.push(caps);
        if !re.global || out.len() > 1_000_000 {
            break;
        }
    }
    out
}

/// `$&`, `$1`, `$<name>`, `` $` ``, `$'` and `$$` in a replacement string.
fn expand(template: &str, re: &Regex, caps: &Captures, chars: &[char]) -> String {
    let t: Vec<char> = template.chars().collect();
    let (a, b) = caps[0].unwrap_or((0, 0));
    let mut out = String::new();
    let mut k = 0;
    while k < t.len() {
        if t[k] != '$' || k + 1 >= t.len() {
            out.push(t[k]);
            k += 1;
            continue;
        }
        let n = t[k + 1];
        match n {
            '$' => {
                out.push('$');
                k += 2;
            }
            '&' => {
                out.extend(&chars[a..b]);
                k += 2;
            }
            '`' => {
                out.extend(&chars[..a]);
                k += 2;
            }
            '\'' => {
                out.extend(&chars[b.min(chars.len())..]);
                k += 2;
            }
            '0'..='9' => {
                let one = n.to_digit(10).unwrap_or(0) as usize;
                let two = t.get(k + 2).and_then(|d| d.to_digit(10)).map(|d| one * 10 + d as usize);
                let (g, used) = match two {
                    Some(g) if g >= 1 && g <= re.groups => (g, 3),
                    _ => (one, 2),
                };
                if g >= 1 && g <= re.groups {
                    if let Some(Some((x, y))) = caps.get(g) {
                        out.extend(&chars[*x..*y]);
                    }
                    k += used;
                } else {
                    out.push('$');
                    k += 1;
                }
            }
            '<' if !re.names.is_empty() => {
                let close = t[k + 2..].iter().position(|&c| c == '>');
                match close {
                    Some(len) => {
                        let name: String = t[k + 2..k + 2 + len].iter().collect();
                        if let Some((_, g)) = re.names.iter().find(|(x, _)| *x == name) {
                            if let Some(Some((x, y))) = caps.get(*g) {
                                out.extend(&chars[*x..*y]);
                            }
                        }
                        k += len + 3;
                    }
                    None => {
                        out.push('$');
                        k += 1;
                    }
                }
            }
            _ => {
                out.push('$');
                k += 1;
            }
        }
    }
    out
}

fn replace_regex(i: &mut Interp, s: &Rc<str>, re: &Regex, o: &Obj, with: &Value) -> Res<Value> {
    let chars: Vec<char> = s.chars().collect();
    if re.global {
        i.set(&Value::Obj(o.clone()), "lastIndex", Value::Num(0.0))?;
    }
    let matches = all_matches(re, &chars);
    let mut out = String::new();
    let mut last = 0;
    for caps in &matches {
        let (a, b) = caps[0].unwrap_or((0, 0));
        out.extend(&chars[last.min(a)..a]);
        let rep = if Interp::is_callable(with) {
            let mut args: Vec<Value> = caps.iter().map(|c| c.map(|(x, y)| substr(&chars, x, y)).unwrap_or(Value::Undefined)).collect();
            args.push(Value::Num(a as f64));
            args.push(Value::Str(s.clone()));
            if !re.names.is_empty() {
                let g = i.obj(Kind::Plain);
                for (name, k) in &re.names {
                    let v = caps.get(*k).copied().flatten().map(|(x, y)| substr(&chars, x, y)).unwrap_or(Value::Undefined);
                    i.set(&g, name, v)?;
                }
                args.push(g);
            }
            let r = i.call(with, Value::Undefined, args)?;
            i.to_str(&r)?.to_string()
        } else {
            let t = i.to_str(with)?;
            expand(&t, re, caps, &chars)
        };
        out.push_str(&rep);
        last = b;
    }
    out.extend(&chars[last.min(chars.len())..]);
    Ok(Value::Str(out.into()))
}

fn split_regex(i: &mut Interp, s: &Rc<str>, re: &Regex, limit: usize) -> Value {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<Value> = Vec::new();
    if chars.is_empty() {
        return i.array(if re.exec_at(&chars, 0).is_some() { vec![] } else { vec![Value::Str(s.clone())] });
    }
    let (mut p, mut q) = (0usize, 0usize);
    while q < chars.len() && out.len() < limit {
        let Some(caps) = re.exec_at(&chars, q) else { break };
        let (a, b) = caps[0].unwrap_or((q, q));
        if a >= chars.len() {
            break;
        }
        if a == b && a == p {
            q = a + 1;
            continue;
        }
        out.push(substr(&chars, p, a));
        for c in caps.iter().skip(1) {
            out.push(c.map(|(x, y)| substr(&chars, x, y)).unwrap_or(Value::Undefined));
        }
        p = b;
        q = if a == b { b + 1 } else { b };
    }
    out.push(substr(&chars, p, chars.len()));
    out.truncate(limit);
    i.array(out)
}

fn to_regex(i: &mut Interp, v: &Value, flags: &str) -> Res<(Rc<Regex>, Obj)> {
    if let Some(r) = regex_of(v) {
        return Ok(r);
    }
    let src: Rc<str> = match v {
        Value::Undefined => "(?:)".into(),
        v => i.to_str(v)?,
    };
    let rv = i.regexp(&src, &flags.into())?;
    Ok(regex_of(&rv).expect("a regexp"))
}

pub fn install(i: &mut Interp) {
    let rp = i.regexp_proto.clone();
    let ctor = i.native(|i, _t, a| {
        let (src, flags): (Rc<str>, Rc<str>) = match (&arg(&a, 0), arg(&a, 1)) {
            (Value::Obj(o), f) if matches!(o.borrow().kind, Kind::RegExp(..)) => {
                let (s, fl) = match &o.borrow().kind {
                    Kind::RegExp(_, s, fl) => (s.clone(), fl.clone()),
                    _ => unreachable!(),
                };
                (s, if matches!(f, Value::Undefined) { fl } else { i.to_str(&f)? })
            }
            (p, f) => (
                if matches!(p, Value::Undefined) { "(?:)".into() } else { i.to_str(p)? },
                if matches!(f, Value::Undefined) { "".into() } else { i.to_str(&f)? },
            ),
        };
        i.regexp(&src, &flags)
    });
    let Value::Obj(co) = ctor.clone() else { unreachable!() };
    co.borrow_mut().set_own("prototype", Value::Obj(rp.clone()));
    co.borrow_mut().set_own("name", Value::Str("RegExp".into()));
    rp.borrow_mut().set_own("constructor", ctor.clone());
    def(i, &co, "escape", |i, _t, a| {
        let s = i.to_str(&arg(&a, 0))?;
        let mut out = String::new();
        for c in s.chars() {
            if "\\^$.*+?()[]{}|/-".contains(c) {
                out.push('\\');
            }
            out.push(c);
        }
        Ok(Value::Str(out.into()))
    });
    i.global_obj.borrow_mut().set_own("RegExp", ctor);

    def(i, &rp, "exec", |i, t, a| {
        let Some((re, o)) = regex_of(&t) else { return i.throw("TypeError", "exec called on a non-RegExp") };
        let input = i.to_str(&arg(&a, 0))?;
        match exec(i, &re, &o, &input)? {
            Some((caps, chars)) => match_array(i, &re, &caps, &chars, &input),
            None => Ok(Value::Null),
        }
    });
    def(i, &rp, "test", |i, t, a| {
        let Some((re, o)) = regex_of(&t) else { return i.throw("TypeError", "test called on a non-RegExp") };
        let input = i.to_str(&arg(&a, 0))?;
        Ok(Value::Bool(exec(i, &re, &o, &input)?.is_some()))
    });
    def(i, &rp, "toString", |_i, t, _a| {
        Ok(Value::Str(match &t {
            Value::Obj(o) => match &o.borrow().kind {
                Kind::RegExp(_, s, f) => format!("/{s}/{f}").into(),
                _ => "/(?:)/".into(),
            },
            _ => "/(?:)/".into(),
        }))
    });
    getter(i, &rp, "source", |_i, t, _a| {
        Ok(match &t {
            Value::Obj(o) => match &o.borrow().kind {
                Kind::RegExp(_, s, _) => Value::Str(s.clone()),
                _ => Value::Undefined,
            },
            _ => Value::Undefined,
        })
    });
    getter(i, &rp, "flags", |_i, t, _a| {
        Ok(match &t {
            Value::Obj(o) => match &o.borrow().kind {
                Kind::RegExp(_, _, f) => Value::Str(f.clone()),
                _ => Value::Undefined,
            },
            _ => Value::Undefined,
        })
    });
    for (name, flag) in [("global", 'g'), ("ignoreCase", 'i'), ("multiline", 'm'), ("dotAll", 's'), ("unicode", 'u'), ("sticky", 'y'), ("hasIndices", 'd')] {
        getter(i, &rp, name, move |_i, t, _a| {
            Ok(match &t {
                Value::Obj(o) => match &o.borrow().kind {
                    Kind::RegExp(_, _, f) => Value::Bool(f.contains(flag)),
                    _ => Value::Undefined,
                },
                _ => Value::Undefined,
            })
        });
    }

    // String methods that take patterns: regexes go here, strings keep the old paths.
    let sp = i.string_proto.clone();
    let old = |name: &str| match sp.borrow().get_own(name) {
        Some(Slot::Data(v)) => v.clone(),
        _ => Value::Undefined,
    };
    let this_str = |i: &mut Interp, t: &Value| -> Res<Rc<str>> {
        match t {
            Value::Undefined | Value::Null => i.throw("TypeError", "String method called on null or undefined"),
            v => i.to_str(v),
        }
    };
    def(i, &sp, "match", move |i, t, a| {
        let s = this_str(i, &t)?;
        let (re, o) = to_regex(i, &arg(&a, 0), "")?;
        if !re.global {
            return match exec(i, &re, &o, &s)? {
                Some((caps, chars)) => match_array(i, &re, &caps, &chars, &s),
                None => Ok(Value::Null),
            };
        }
        let chars: Vec<char> = s.chars().collect();
        let found: Vec<Value> = all_matches(&re, &chars).iter().map(|c| c[0].map(|(x, y)| substr(&chars, x, y)).unwrap_or(Value::Undefined)).collect();
        i.set(&Value::Obj(o), "lastIndex", Value::Num(0.0))?;
        Ok(if found.is_empty() { Value::Null } else { i.array(found) })
    });
    def(i, &sp, "matchAll", move |i, t, a| {
        let s = this_str(i, &t)?;
        let (re, _) = to_regex(i, &arg(&a, 0), "g")?;
        if !re.global {
            return i.throw("TypeError", "matchAll needs a global (g) regular expression");
        }
        let chars: Vec<char> = s.chars().collect();
        let mut out = Vec::new();
        for caps in all_matches(&re, &chars) {
            out.push(match_array(i, &re, &caps, &chars, &s)?);
        }
        Ok(crate::promise::make_iter(i, out))
    });
    def(i, &sp, "search", move |i, t, a| {
        let s = this_str(i, &t)?;
        let (re, _) = to_regex(i, &arg(&a, 0), "")?;
        let chars: Vec<char> = s.chars().collect();
        Ok(Value::Num(re.exec_at(&chars, 0).and_then(|c| c[0]).map(|c| c.0 as f64).unwrap_or(-1.0)))
    });
    for name in ["replace", "replaceAll"] {
        let prev = old(name);
        def(i, &sp, name, move |i, t, a| {
            let pat = arg(&a, 0);
            if let Some((re, o)) = regex_of(&pat) {
                if name == "replaceAll" && !re.global {
                    return i.throw("TypeError", "replaceAll needs a global (g) regular expression");
                }
                let s = this_str(i, &t)?;
                return replace_regex(i, &s, &re, &o, &arg(&a, 1));
            }
            // A string pattern: `$` patterns still apply to string replacements.
            let with = arg(&a, 1);
            if !Interp::is_callable(&with) {
                let w = i.to_str(&with)?;
                if w.contains('$') {
                    let s = this_str(i, &t)?;
                    let p = i.to_str(&pat)?;
                    let escaped: String = p.chars().flat_map(|c| if "\\^$.*+?()[]{}|/".contains(c) { vec!['\\', c] } else { vec![c] }).collect();
                    let flags = if name == "replaceAll" { "g" } else { "" };
                    let rv = i.regexp(&escaped.into(), &flags.into())?;
                    let (re, o) = regex_of(&rv).expect("a regexp");
                    return replace_regex(i, &s, &re, &o, &with);
                }
            }
            i.call(&prev, t, a)
        });
    }
    let prev_split = old("split");
    def(i, &sp, "split", move |i, t, a| {
        if let Some((re, _)) = regex_of(&arg(&a, 0)) {
            let s = this_str(i, &t)?;
            let limit = match arg(&a, 1) {
                Value::Undefined => usize::MAX,
                l => i.to_number(&l)? as u32 as usize,
            };
            return Ok(split_regex(i, &s, &re, limit));
        }
        i.call(&prev_split, t, a)
    });
}
