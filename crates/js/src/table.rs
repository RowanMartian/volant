//! The ordered hash table behind `Map` and `Set`: entries in insertion order,
//! found by SameValueZero in constant time (a deleted entry leaves a gap until
//! there are enough gaps to be worth closing up).

use crate::interp::Value;
use std::collections::HashMap;
use std::rc::Rc;

/// A value as SameValueZero sees it, so it can be hashed.
#[derive(Hash, PartialEq, Eq, Clone)]
enum Key {
    Undefined,
    Null,
    Bool(bool),
    Num(u64),
    Str(Rc<str>),
    Obj(usize),
}

fn key(v: &Value) -> Key {
    match v {
        Value::Undefined => Key::Undefined,
        Value::Null => Key::Null,
        Value::Bool(b) => Key::Bool(*b),
        // -0 is 0, and every NaN is the same NaN.
        Value::Num(n) if *n == 0.0 => Key::Num(0),
        Value::Num(n) if n.is_nan() => Key::Num(f64::NAN.to_bits()),
        Value::Num(n) => Key::Num(n.to_bits()),
        Value::Str(s) => Key::Str(s.clone()),
        Value::Obj(o) => Key::Obj(Rc::as_ptr(o) as *const u8 as usize),
    }
}

#[derive(Default, Clone)]
pub struct Table {
    slots: Vec<Option<(Value, Value)>>,
    index: HashMap<Key, usize>,
}

impl Table {
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn get(&self, k: &Value) -> Option<&Value> {
        self.index.get(&key(k)).and_then(|&i| self.slots[i].as_ref()).map(|e| &e.1)
    }

    pub fn has(&self, k: &Value) -> bool {
        self.index.contains_key(&key(k))
    }

    /// Sets a key's value (a new key goes at the end; -0 is stored as 0).
    pub fn set(&mut self, k: Value, v: Value) {
        let k = match k {
            Value::Num(n) if n == 0.0 => Value::Num(0.0),
            k => k,
        };
        match self.index.get(&key(&k)) {
            Some(&i) => {
                if let Some(e) = &mut self.slots[i] {
                    e.1 = v;
                }
            }
            None => {
                self.index.insert(key(&k), self.slots.len());
                self.slots.push(Some((k, v)));
            }
        }
    }

    pub fn delete(&mut self, k: &Value) -> bool {
        let Some(i) = self.index.remove(&key(k)) else { return false };
        self.slots[i] = None;
        // Close up the gaps once they're most of the table.
        if self.slots.len() > 32 && self.index.len() * 2 < self.slots.len() {
            self.slots.retain(Option::is_some);
            for (n, e) in self.slots.iter().enumerate() {
                if let Some((k, _)) = e {
                    self.index.insert(key(k), n);
                }
            }
        }
        true
    }

    pub fn clear(&mut self) {
        self.slots.clear();
        self.index.clear();
    }

    /// The entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &(Value, Value)> {
        self.slots.iter().flatten()
    }

    pub fn keys(&self) -> impl Iterator<Item = &Value> {
        self.iter().map(|e| &e.0)
    }
}
