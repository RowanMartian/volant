//! Proxy and Reflect. A proxy's traps (get, set, has, deleteProperty,
//! ownKeys, apply, construct) are consulted by the interpreter's own property
//! operations, so code like Vue's reactivity sees every read and write.

use crate::builtins::{arg, def};
use crate::interp::{Interp, Kind, Obj, Res, Value};
use std::rc::Rc;

impl Interp {
    /// The (target, handler) of a proxy.
    pub fn proxy_parts(v: &Value) -> Option<(Value, Value)> {
        match v {
            Value::Obj(o) => match &o.borrow().kind {
                Kind::Proxy(t, h) => Some((t.clone(), h.clone())),
                _ => None,
            },
            _ => None,
        }
    }

    /// The handler's trap `name`, if it has one.
    fn trap(&mut self, handler: &Value, name: &str) -> Res<Option<Value>> {
        if matches!(handler, Value::Null | Value::Undefined) {
            return self.throw("TypeError", "the proxy was revoked");
        }
        let t = self.get(handler, name)?;
        Ok(Self::is_callable(&t).then_some(t))
    }

    pub(crate) fn proxy_get(&mut self, target: Value, handler: Value, k: &str, receiver: &Value) -> Res<Value> {
        match self.trap(&handler, "get")? {
            Some(t) => self.call(&t, handler, vec![target, key_value(k), receiver.clone()]),
            None => self.get(&target, k),
        }
    }

    pub(crate) fn proxy_set(&mut self, target: Value, handler: Value, k: &str, v: Value, receiver: &Value) -> Res<()> {
        match self.trap(&handler, "set")? {
            Some(t) => {
                self.call(&t, handler, vec![target, key_value(k), v, receiver.clone()])?;
                Ok(())
            }
            None => self.set(&target, k, v),
        }
    }

    /// The `in` operator (and `with`): a proxy's `has` trap decides.
    pub fn has(&mut self, v: &Value, k: &str) -> Res<bool> {
        let Value::Obj(o) = v else { return Ok(false) };
        if let Some((target, handler)) = Self::proxy_parts(v) {
            return match self.trap(&handler, "has")? {
                Some(t) => {
                    let r = self.call(&t, handler, vec![target, key_value(k)])?;
                    Ok(Self::truthy(&r))
                }
                None => self.has(&target, k),
            };
        }
        Ok(self.has_property(o, k))
    }

    /// Whether `v` has `k` as an own property (`hasOwnProperty`, `Object.hasOwn`).
    pub fn has_own(&mut self, v: &Value, k: &str) -> Res<bool> {
        if let Some((target, handler)) = Self::proxy_parts(v) {
            return match self.trap(&handler, "getOwnPropertyDescriptor")? {
                Some(t) => {
                    let r = self.call(&t, handler, vec![target, key_value(k)])?;
                    Ok(!matches!(r, Value::Undefined))
                }
                None => self.has_own(&target, k),
            };
        }
        Ok(match v {
            Value::Obj(o) => {
                // A function's name, length and prototype are made when first asked about.
                if o.borrow().get_own(k).is_none() && crate::vm::lazy_function_prop(self, o, k).is_some() {
                    return Ok(true);
                }
                let b = o.borrow();
                b.get_own(k).is_some()
                    || matches!(&b.kind, Kind::Array(arr) if k.parse::<usize>().map(|x| x < arr.len()).unwrap_or(false) || k == "length")
                    || matches!(&b.kind, Kind::Typed(t) if !t.data_view && k.parse::<usize>().map(|x| x < t.len).unwrap_or(false))
            }
            Value::Str(s) => k.parse::<usize>().map(|x| x < s.chars().count()).unwrap_or(false) || k == "length",
            _ => false,
        })
    }

    /// `delete v[k]`.
    pub fn delete_property(&mut self, v: &Value, k: &str) -> Res<bool> {
        let Value::Obj(o) = v else { return Ok(true) };
        if let Some((target, handler)) = Self::proxy_parts(v) {
            return match self.trap(&handler, "deleteProperty")? {
                Some(t) => {
                    let r = self.call(&t, handler, vec![target, key_value(k)])?;
                    Ok(Self::truthy(&r))
                }
                None => self.delete_property(&target, k),
            };
        }
        let mut b = o.borrow_mut();
        if let Kind::Array(a) = &mut b.kind {
            if let Ok(i) = k.parse::<usize>() {
                if i < a.len() {
                    a[i] = Value::Undefined;
                }
                return Ok(true);
            }
        }
        b.delete(k);
        Ok(true)
    }

    /// An object's own enumerable string keys (a proxy's `ownKeys` trap decides).
    pub fn keys_of(&mut self, v: &Value) -> Res<Vec<Rc<str>>> {
        match v {
            Value::Obj(o) => {
                if let Some((target, handler)) = Self::proxy_parts(v) {
                    return match self.trap(&handler, "ownKeys")? {
                        Some(t) => {
                            let r = self.call(&t, handler, vec![target])?;
                            let list = self.iterate(&r)?;
                            let mut out = Vec::new();
                            for k in list {
                                if let Value::Str(s) = k {
                                    if !s.starts_with('\0') {
                                        out.push(s);
                                    }
                                }
                            }
                            Ok(out)
                        }
                        None => self.keys_of(&target),
                    };
                }
                Ok(o.borrow().own_keys())
            }
            Value::Str(s) => Ok((0..s.chars().count()).map(|k| k.to_string().into()).collect()),
            _ => Ok(Vec::new()),
        }
    }
}

/// A property key as a value for a trap (symbols stay as their hidden strings).
fn key_value(k: &str) -> Value {
    Value::Str(k.into())
}

pub fn install(i: &mut Interp) {
    let proxy = i.native(|i, _this, a| {
        let (target, handler) = (arg(&a, 0), arg(&a, 1));
        let (Value::Obj(t), Value::Obj(_)) = (&target, &handler) else {
            return i.throw("TypeError", "Proxy needs a target and a handler object");
        };
        let proto = t.borrow().proto.clone();
        let p = i.obj(Kind::Proxy(target.clone(), handler));
        if let Value::Obj(po) = &p {
            po.borrow_mut().proto = proto;
        }
        Ok(p)
    });
    let revocable = i.native(|i, _this, a| {
        let ctor = i.get(&Value::Obj(i.global_obj.clone()), "Proxy")?;
        let p = i.construct(&ctor, a)?;
        let pc = p.clone();
        let revoke = i.native(move |_i, _t, _a| {
            if let Value::Obj(o) = &pc {
                if let Kind::Proxy(_, h) = &mut o.borrow_mut().kind {
                    *h = Value::Null;
                }
            }
            Ok(Value::Undefined)
        });
        let r = i.obj(Kind::Plain);
        i.set(&r, "proxy", p)?;
        i.set(&r, "revoke", revoke)?;
        Ok(r)
    });
    i.set(&proxy, "revocable", revocable).ok();
    i.global_obj.borrow_mut().set_own("Proxy", proxy);

    let reflect: Obj = match i.obj(Kind::Plain) {
        Value::Obj(o) => o,
        _ => unreachable!(),
    };
    def(i, &reflect, "get", |i, _t, a| {
        let k = i.key(&arg(&a, 1))?;
        i.get(&arg(&a, 0), &k)
    });
    def(i, &reflect, "set", |i, _t, a| {
        let k = i.key(&arg(&a, 1))?;
        i.set(&arg(&a, 0), &k, arg(&a, 2))?;
        Ok(Value::Bool(true))
    });
    def(i, &reflect, "has", |i, _t, a| {
        let k = i.key(&arg(&a, 1))?;
        Ok(Value::Bool(i.has(&arg(&a, 0), &k)?))
    });
    def(i, &reflect, "deleteProperty", |i, _t, a| {
        let k = i.key(&arg(&a, 1))?;
        Ok(Value::Bool(i.delete_property(&arg(&a, 0), &k)?))
    });
    def(i, &reflect, "ownKeys", |i, _t, a| {
        let mut keys: Vec<Value> = i.keys_of(&arg(&a, 0))?.into_iter().map(Value::Str).collect();
        // Non-enumerable own keys too (hidden internals stay hidden).
        if let Value::Obj(o) = &arg(&a, 0) {
            for k in o.borrow().own_keys() {
                if !k.starts_with('\0') && !keys.iter().any(|x| matches!(x, Value::Str(s) if *s == k)) {
                    keys.push(Value::Str(k));
                }
            }
        }
        Ok(i.array(keys))
    });
    def(i, &reflect, "apply", |i, _t, a| {
        let args = i.iterate(&arg(&a, 2))?;
        i.call(&arg(&a, 0), arg(&a, 1), args)
    });
    def(i, &reflect, "construct", |i, _t, a| {
        let args = i.iterate(&arg(&a, 1))?;
        i.construct(&arg(&a, 0), args)
    });
    def(i, &reflect, "getPrototypeOf", |_i, _t, a| {
        Ok(match &arg(&a, 0) {
            Value::Obj(o) => o.borrow().proto.clone().map(Value::Obj).unwrap_or(Value::Null),
            _ => Value::Null,
        })
    });
    def(i, &reflect, "setPrototypeOf", |i, _t, a| {
        i.set(&arg(&a, 0), "__proto__", arg(&a, 1))?;
        Ok(Value::Bool(true))
    });
    for (name, src) in [
        ("defineProperty", "(o, k, d) => { try { Object.defineProperty(o, k, d); return true } catch (e) { return false } }"),
        ("getOwnPropertyDescriptor", "(o, k) => Object.getOwnPropertyDescriptor(o, k)"),
        ("isExtensible", "o => Object.isExtensible(o)"),
        ("preventExtensions", "o => { Object.preventExtensions(o); return true }"),
    ] {
        if let Ok(f) = i.run(&format!("({src})")) {
            reflect.borrow_mut().set_own(name, f);
        }
    }
    reflect.borrow_mut().set_own("\0@toStringTag", Value::Str("Reflect".into()));
    i.global_obj.borrow_mut().set_own("Reflect", Value::Obj(reflect));
}
