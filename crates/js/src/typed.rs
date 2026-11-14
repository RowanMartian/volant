//! Binary data: ArrayBuffer, the eleven typed arrays and DataView.
//!
//! An ArrayBuffer owns its bytes (`Kind::Buffer`); a typed array or DataView
//! is a window onto one (`Kind::Typed`), so views made over the same buffer
//! see each other's writes. Elements are read and written straight from the
//! bytes, little-endian like the Macs Volant runs on; DataView picks the byte
//! order per call and defaults to big-endian, as the standard says.

use crate::builtins::{arg, def, items};
use crate::interp::{Interp, Kind, Obj, Res, Slot, Value};
use std::rc::Rc;

/// What a typed array holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Elem {
    I8,
    U8,
    U8Clamped,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
    I64,
    U64,
}

/// Every typed array constructor, by name.
pub const ELEMS: [(&str, Elem); 11] = [
    ("Int8Array", Elem::I8),
    ("Uint8Array", Elem::U8),
    ("Uint8ClampedArray", Elem::U8Clamped),
    ("Int16Array", Elem::I16),
    ("Uint16Array", Elem::U16),
    ("Int32Array", Elem::I32),
    ("Uint32Array", Elem::U32),
    ("Float32Array", Elem::F32),
    ("Float64Array", Elem::F64),
    ("BigInt64Array", Elem::I64),
    ("BigUint64Array", Elem::U64),
];

/// The biggest buffer a page may make (1 GiB).
const MAX_BYTES: usize = 1 << 30;

/// `x` reduced modulo 2^bits, as JS converts numbers to fixed-width integers.
fn wrap(x: f64, bits: u32) -> f64 {
    if !x.is_finite() {
        return 0.0;
    }
    x.trunc().rem_euclid(2f64.powi(bits as i32))
}

impl Elem {
    pub fn size(self) -> usize {
        match self {
            Elem::I8 | Elem::U8 | Elem::U8Clamped => 1,
            Elem::I16 | Elem::U16 => 2,
            Elem::I32 | Elem::U32 | Elem::F32 => 4,
            Elem::F64 | Elem::I64 | Elem::U64 => 8,
        }
    }

    pub fn name(self) -> &'static str {
        ELEMS.iter().find(|(_, e)| *e == self).map(|(n, _)| *n).unwrap_or("Uint8Array")
    }

    fn index(self) -> usize {
        ELEMS.iter().position(|(_, e)| *e == self).unwrap_or(1)
    }

    /// BigInt64 and BigUint64 hold BigInts rather than Numbers.
    pub fn is_big(self) -> bool {
        matches!(self, Elem::I64 | Elem::U64)
    }

    /// The element at byte `at` as a JS value (a BigInt for the 64-bit kinds).
    pub fn read_value(self, b: &[u8], at: usize, little: bool) -> Value {
        if !self.is_big() {
            return Value::Num(self.read(b, at, little));
        }
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&b[at..at + 8]);
        let bits = if little { u64::from_le_bytes(raw) } else { u64::from_be_bytes(raw) };
        let big = if self == Elem::I64 { crate::bigint::BigInt::from_i64(bits as i64) } else { crate::bigint::BigInt::from_u64(bits) };
        Value::BigInt(Rc::new(big))
    }

    /// Stores a value converted as JS does: ToBigInt for the 64-bit kinds, else ToNumber.
    pub fn write_value(self, i: &mut Interp, b: &mut [u8], at: usize, v: &Value, little: bool) -> Res<()> {
        if self.is_big() {
            let bits = i.to_bigint(v)?.to_u64_wrapping();
            let raw = if little { bits.to_le_bytes() } else { bits.to_be_bytes() };
            b[at..at + 8].copy_from_slice(&raw);
        } else {
            let n = match v {
                Value::Num(n) => *n,
                v => i.to_number(v)?,
            };
            self.write(b, at, n, little);
        }
        Ok(())
    }

    /// The element at byte `at` (which must be in bounds).
    pub fn read(self, b: &[u8], at: usize, little: bool) -> f64 {
        let mut raw = [0u8; 8];
        let n = self.size();
        raw[..n].copy_from_slice(&b[at..at + n]);
        if !little {
            raw[..n].reverse();
        }
        match self {
            Elem::I8 => raw[0] as i8 as f64,
            Elem::U8 | Elem::U8Clamped => raw[0] as f64,
            Elem::I16 => i16::from_le_bytes([raw[0], raw[1]]) as f64,
            Elem::U16 => u16::from_le_bytes([raw[0], raw[1]]) as f64,
            Elem::I32 => i32::from_le_bytes(raw[..4].try_into().unwrap()) as f64,
            Elem::U32 => u32::from_le_bytes(raw[..4].try_into().unwrap()) as f64,
            Elem::F32 => f32::from_le_bytes(raw[..4].try_into().unwrap()) as f64,
            Elem::F64 => f64::from_le_bytes(raw),
            Elem::I64 => i64::from_le_bytes(raw) as f64,
            Elem::U64 => u64::from_le_bytes(raw) as f64,
        }
    }

    /// Stores `v` at byte `at` (which must be in bounds), converted as JS does.
    pub fn write(self, b: &mut [u8], at: usize, v: f64, little: bool) {
        let mut raw: [u8; 8] = match self {
            Elem::I8 | Elem::U8 => [wrap(v, 8) as u8, 0, 0, 0, 0, 0, 0, 0],
            Elem::U8Clamped => {
                let c = if v.is_nan() { 0.0 } else { v.clamp(0.0, 255.0).round_ties_even() };
                [c as u8, 0, 0, 0, 0, 0, 0, 0]
            }
            Elem::I16 | Elem::U16 => pad(&(wrap(v, 16) as u16).to_le_bytes()),
            Elem::I32 | Elem::U32 => pad(&(wrap(v, 32) as u32).to_le_bytes()),
            Elem::F32 => pad(&(v as f32).to_le_bytes()),
            Elem::F64 => v.to_le_bytes(),
            Elem::I64 | Elem::U64 => (if v.is_finite() { v.trunc() as i64 } else { 0 }).to_le_bytes(),
        };
        let n = self.size();
        if !little {
            raw[..n].reverse();
        }
        b[at..at + n].copy_from_slice(&raw[..n]);
    }
}

fn pad(b: &[u8]) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..b.len()].copy_from_slice(b);
    out
}

/// A typed array or DataView: which buffer, what's in it, and which part.
#[derive(Clone)]
pub struct View {
    /// The ArrayBuffer object (`Kind::Buffer`).
    pub buffer: Obj,
    pub elem: Elem,
    /// Where it starts in the buffer, in bytes.
    pub offset: usize,
    /// How many elements (a DataView's are bytes).
    pub len: usize,
    /// A DataView rather than a typed array.
    pub data_view: bool,
}

impl View {
    pub fn byte_len(&self) -> usize {
        self.len * self.elem.size()
    }

    /// Element `i`, or None past the end.
    pub fn get(&self, i: usize) -> Option<f64> {
        if i >= self.len {
            return None;
        }
        let b = self.buffer.borrow();
        let Kind::Buffer(bytes) = &b.kind else { return None };
        let at = self.offset + i * self.elem.size();
        (at + self.elem.size() <= bytes.len()).then(|| self.elem.read(bytes, at, true))
    }

    /// Element `i` as a JS value, or None past the end.
    pub fn get_value(&self, i: usize) -> Option<Value> {
        if !self.elem.is_big() {
            return self.get(i).map(Value::Num);
        }
        if i >= self.len {
            return None;
        }
        let b = self.buffer.borrow();
        let Kind::Buffer(bytes) = &b.kind else { return None };
        let at = self.offset + i * 8;
        (at + 8 <= bytes.len()).then(|| self.elem.read_value(bytes, at, true))
    }

    /// Stores a JS value at element `i`, converted as JS does (ignored past the end).
    pub fn put_value(&self, interp: &mut Interp, i: usize, v: &Value) -> Res<()> {
        if !self.elem.is_big() {
            let n = match v {
                Value::Num(n) => *n,
                v => interp.to_number(v)?,
            };
            self.put(i, n);
            return Ok(());
        }
        let bits = interp.to_bigint(v)?.to_u64_wrapping();
        if i < self.len {
            let mut b = self.buffer.borrow_mut();
            if let Kind::Buffer(bytes) = &mut b.kind {
                let at = self.offset + i * 8;
                if at + 8 <= bytes.len() {
                    bytes[at..at + 8].copy_from_slice(&bits.to_le_bytes());
                }
            }
        }
        Ok(())
    }

    /// All the elements, as JS values.
    pub fn value_list(&self) -> Vec<Value> {
        if !self.elem.is_big() {
            return self.values().into_iter().map(Value::Num).collect();
        }
        (0..self.len).map_while(|k| self.get_value(k)).collect()
    }

    /// Stores element `i` (ignored past the end, as in browsers).
    pub fn put(&self, i: usize, v: f64) {
        if i >= self.len {
            return;
        }
        let mut b = self.buffer.borrow_mut();
        let Kind::Buffer(bytes) = &mut b.kind else { return };
        let at = self.offset + i * self.elem.size();
        if at + self.elem.size() <= bytes.len() {
            self.elem.write(bytes, at, v, true);
        }
    }

    /// All the elements, as numbers.
    pub fn values(&self) -> Vec<f64> {
        let b = self.buffer.borrow();
        let Kind::Buffer(bytes) = &b.kind else { return Vec::new() };
        let size = self.elem.size();
        (0..self.len).map(|i| self.offset + i * size).take_while(|at| at + size <= bytes.len()).map(|at| self.elem.read(bytes, at, true)).collect()
    }
}

/// The typed array or DataView behind a value.
pub fn view_of(v: &Value) -> Option<View> {
    match v {
        Value::Obj(o) => match &o.borrow().kind {
            Kind::Typed(t) => Some(t.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The bytes a value holds: an ArrayBuffer's, or a view's part of its buffer.
pub fn bytes_of(v: &Value) -> Option<Vec<u8>> {
    let Value::Obj(o) = v else { return None };
    let b = o.borrow();
    match &b.kind {
        Kind::Buffer(bytes) => Some(bytes.clone()),
        Kind::Typed(t) => {
            let buf = t.buffer.borrow();
            let Kind::Buffer(bytes) = &buf.kind else { return None };
            bytes.get(t.offset..(t.offset + t.byte_len()).min(bytes.len())).map(|s| s.to_vec())
        }
        _ => None,
    }
}

/// A new ArrayBuffer holding `bytes`.
pub fn new_buffer(i: &Interp, bytes: Vec<u8>) -> Value {
    let o = i.obj(Kind::Buffer(bytes));
    if let Value::Obj(ob) = &o {
        ob.borrow_mut().proto = Some(i.buffer_proto.clone());
    }
    o
}

/// A new typed array over `buffer`.
fn new_view(i: &Interp, view: View, proto: Option<Obj>) -> Value {
    let proto = proto.unwrap_or_else(|| if view.data_view { i.data_view_proto.clone() } else { i.typed_protos[view.elem.index()].clone() });
    let o = i.obj(Kind::Typed(view));
    if let Value::Obj(ob) = &o {
        ob.borrow_mut().proto = Some(proto);
    }
    o
}

/// A new typed array of `elem` holding `values`.
pub fn new_typed(i: &Interp, elem: Elem, values: &[f64]) -> Value {
    let mut bytes = vec![0u8; values.len() * elem.size()];
    for (k, v) in values.iter().enumerate() {
        elem.write(&mut bytes, k * elem.size(), *v, true);
    }
    let Value::Obj(buffer) = new_buffer(i, bytes) else { unreachable!() };
    new_view(i, View { buffer, elem, offset: 0, len: values.len(), data_view: false }, None)
}

/// A new Uint8ClampedArray holding `bytes` (canvas image data).
pub fn new_clamped(i: &Interp, bytes: Vec<u8>) -> Value {
    let len = bytes.len();
    let Value::Obj(buffer) = new_buffer(i, bytes) else { unreachable!() };
    new_view(i, View { buffer, elem: Elem::U8Clamped, offset: 0, len, data_view: false }, None)
}

/// A new Uint8Array holding `bytes` (for the web engine: TextEncoder, fetch, crypto).
pub fn new_bytes(i: &Interp, bytes: Vec<u8>) -> Value {
    let len = bytes.len();
    let Value::Obj(buffer) = new_buffer(i, bytes) else { unreachable!() };
    new_view(i, View { buffer, elem: Elem::U8, offset: 0, len, data_view: false }, None)
}

fn num(i: &mut Interp, v: &Value) -> Res<f64> {
    match v {
        Value::Num(n) => Ok(*n),
        v => i.to_number(v),
    }
}

/// A whole-number argument (`undefined` gives `default`).
fn int(i: &mut Interp, a: &[Value], k: usize, default: f64) -> Res<f64> {
    match a.get(k) {
        None | Some(Value::Undefined) => Ok(default),
        Some(v) => {
            let n = num(i, v)?;
            Ok(if n.is_nan() { 0.0 } else { n.trunc() })
        }
    }
}

/// `start`/`end` arguments clamped to `0..=len` (negatives count from the end).
fn span(i: &mut Interp, a: &[Value], at: usize, len: usize) -> Res<(usize, usize)> {
    let fix = |n: f64| -> usize {
        let n = if n < 0.0 { (len as f64 + n).max(0.0) } else { n.min(len as f64) };
        n as usize
    };
    let s = fix(int(i, a, at, 0.0)?);
    let e = fix(int(i, a, at + 1, len as f64)?);
    Ok((s, e.max(s)))
}

fn this_view(i: &Interp, t: &Value) -> Res<View> {
    match view_of(t) {
        Some(v) if !v.data_view => Ok(v),
        _ => i.throw("TypeError", "not a typed array"),
    }
}

fn proto_of(t: &Value) -> Option<Obj> {
    match t {
        Value::Obj(o) => o.borrow().proto.clone(),
        _ => None,
    }
}

/// Builds a typed array of `elem` the way its constructor does:
/// `new T(length)`, `new T(typedArray | arrayLike | iterable)`, `new T(buffer, offset, length)`.
fn construct(i: &mut Interp, elem: Elem, a: &[Value]) -> Res<Value> {
    let first = arg(a, 0);
    let size = elem.size();
    if let Value::Obj(o) = &first {
        let is_buffer = matches!(o.borrow().kind, Kind::Buffer(_));
        if is_buffer {
            let total = match &o.borrow().kind {
                Kind::Buffer(b) => b.len(),
                _ => 0,
            };
            let offset = int(i, a, 1, 0.0)?;
            if offset < 0.0 || offset as usize > total || offset as usize % size != 0 {
                return i.throw("RangeError", &format!("start offset of {} should be a multiple of {size}", elem.name()));
            }
            let offset = offset as usize;
            let len = match a.get(2) {
                None | Some(Value::Undefined) => {
                    if (total - offset) % size != 0 {
                        return i.throw("RangeError", &format!("byte length of {} should be a multiple of {size}", elem.name()));
                    }
                    (total - offset) / size
                }
                Some(_) => {
                    let n = int(i, a, 2, 0.0)?;
                    if n < 0.0 || offset + n as usize * size > total {
                        return i.throw("RangeError", "invalid typed array length");
                    }
                    n as usize
                }
            };
            return Ok(new_view(i, View { buffer: o.clone(), elem, offset, len, data_view: false }, None));
        }
        if elem.is_big() {
            // BigInts are stored one by one, each through ToBigInt.
            let list = match view_of(&first) {
                Some(v) => v.value_list(),
                None => i.iterate(&first)?,
            };
            let made = new_typed(i, elem, &vec![0.0; list.len()]);
            if let Some(v) = view_of(&made) {
                for (k, x) in list.iter().enumerate() {
                    v.put_value(i, k, x)?;
                }
            }
            return Ok(made);
        }
        let values: Vec<f64> = match view_of(&first) {
            Some(v) => v.values(),
            None => {
                let list = i.iterate(&first)?;
                let mut out = Vec::with_capacity(list.len());
                for x in &list {
                    out.push(num(i, x)?);
                }
                out
            }
        };
        return Ok(new_typed(i, elem, &values));
    }
    let n = match &first {
        Value::Undefined | Value::Null => 0.0,
        v => num(i, v)?,
    };
    if !(0.0..=(MAX_BYTES / size) as f64).contains(&n) || n.is_nan() {
        return i.throw("RangeError", "invalid typed array length");
    }
    let len = n as usize;
    let Value::Obj(buffer) = new_buffer(i, vec![0u8; len * size]) else { unreachable!() };
    Ok(new_view(i, View { buffer, elem, offset: 0, len, data_view: false }, None))
}

fn call_method(i: &mut Interp, t: &Value, name: &str, a: Vec<Value>) -> Res<Value> {
    let f = i.get(t, name)?;
    i.call(&f, t.clone(), a)
}

fn getter(i: &Interp, proto: &Obj, name: &str, f: impl Fn(&mut Interp, Value) -> Res<Value> + 'static) {
    let g = i.native(move |i, t, _a| f(i, t));
    if let Value::Obj(go) = &g {
        go.borrow_mut().set_hidden("name", Value::Str(format!("get {name}").into()));
    }
    proto.borrow_mut().set_slot(name, Slot::Accessor(Some(g), None));
    proto.borrow_mut().set_enumerable(name, false);
}

fn hide_all(o: &Obj) {
    let keys: Vec<Rc<str>> = o.borrow().entries().map(|(k, _, _)| k.clone()).collect();
    for k in keys {
        o.borrow_mut().set_enumerable(&k, false);
    }
}

pub fn install(i: &mut Interp) {
    // ---------------------------------------------------------- ArrayBuffer
    let bp = i.buffer_proto.clone();
    let buffer_ctor = i.native(|i, _t, a| {
        let n = int(i, &a, 0, 0.0)?;
        if n < 0.0 || n as usize > MAX_BYTES {
            return i.throw("RangeError", "Array buffer allocation failed");
        }
        Ok(new_buffer(i, vec![0u8; n as usize]))
    });
    let Value::Obj(bc) = buffer_ctor.clone() else { unreachable!() };
    bc.borrow_mut().set_hidden("prototype", Value::Obj(bp.clone()));
    bc.borrow_mut().set_hidden("name", Value::Str("ArrayBuffer".into()));
    bp.borrow_mut().set_hidden("constructor", buffer_ctor.clone());
    bp.borrow_mut().set_hidden("\0@toStringTag", Value::Str("ArrayBuffer".into()));
    def(i, &bc, "isView", |_i, _t, a| Ok(Value::Bool(view_of(&arg(&a, 0)).is_some())));
    getter(i, &bp, "byteLength", |i, t| match &t {
        Value::Obj(o) => match &o.borrow().kind {
            Kind::Buffer(b) => Ok(Value::Num(b.len() as f64)),
            _ => i.throw("TypeError", "not an ArrayBuffer"),
        },
        _ => i.throw("TypeError", "not an ArrayBuffer"),
    });
    getter(i, &bp, "maxByteLength", |i, t| i.get(&t, "byteLength"));
    getter(i, &bp, "resizable", |_i, _t| Ok(Value::Bool(false)));
    getter(i, &bp, "detached", |_i, _t| Ok(Value::Bool(false)));
    def(i, &bp, "slice", |i, t, a| {
        let Some(bytes) = (match &t {
            Value::Obj(o) => match &o.borrow().kind {
                Kind::Buffer(b) => Some(b.clone()),
                _ => None,
            },
            _ => None,
        }) else {
            return i.throw("TypeError", "not an ArrayBuffer");
        };
        let (s, e) = span(i, &a, 0, bytes.len())?;
        Ok(new_buffer(i, bytes[s..e].to_vec()))
    });
    hide_all(&bp);
    i.global_obj.borrow_mut().set_hidden("ArrayBuffer", buffer_ctor.clone());
    // SharedArrayBuffer needs cross-origin isolation, which Canard doesn't offer.

    // ---------------------------------------------------------- %TypedArray%
    let tp = i.typed_proto.clone();
    let typed_ctor = i.native(|i, _t, _a| i.throw("TypeError", "Abstract class TypedArray not directly constructable"));
    let Value::Obj(tc) = typed_ctor.clone() else { unreachable!() };
    tc.borrow_mut().set_hidden("prototype", Value::Obj(tp.clone()));
    tc.borrow_mut().set_hidden("name", Value::Str("TypedArray".into()));
    tp.borrow_mut().set_hidden("constructor", typed_ctor.clone());
    getter(i, &tp, "length", |i, t| Ok(Value::Num(this_view(i, &t)?.len as f64)));
    getter(i, &tp, "byteLength", |i, t| Ok(Value::Num(this_view(i, &t)?.byte_len() as f64)));
    getter(i, &tp, "byteOffset", |i, t| Ok(Value::Num(this_view(i, &t)?.offset as f64)));
    getter(i, &tp, "buffer", |i, t| Ok(Value::Obj(this_view(i, &t)?.buffer)));
    getter(i, &tp, "\0@toStringTag", |_i, t| Ok(view_of(&t).filter(|v| !v.data_view).map(|v| Value::Str(v.elem.name().into())).unwrap_or(Value::Undefined)));

    def(i, &tp, "subarray", |i, t, a| {
        let v = this_view(i, &t)?;
        let (s, e) = span(i, &a, 0, v.len)?;
        let view = View { offset: v.offset + s * v.elem.size(), len: e - s, ..v };
        Ok(new_view(i, view, proto_of(&t)))
    });
    def(i, &tp, "slice", |i, t, a| {
        let v = this_view(i, &t)?;
        let (s, e) = span(i, &a, 0, v.len)?;
        let values = v.values();
        let out = new_typed(i, v.elem, &values[s.min(values.len())..e.min(values.len())]);
        if let (Value::Obj(o), Some(p)) = (&out, proto_of(&t)) {
            o.borrow_mut().proto = Some(p);
        }
        Ok(out)
    });
    def(i, &tp, "set", |i, t, a| {
        let v = this_view(i, &t)?;
        let at = int(i, &a, 1, 0.0)?;
        let src = arg(&a, 0);
        let values: Vec<f64> = match view_of(&src) {
            Some(s) => s.values(),
            None => {
                let list = items(i, &src)?;
                let mut out = Vec::with_capacity(list.len());
                for x in &list {
                    out.push(num(i, x)?);
                }
                out
            }
        };
        if at < 0.0 || at as usize + values.len() > v.len {
            return i.throw("RangeError", "offset is out of bounds");
        }
        let mut b = v.buffer.borrow_mut();
        if let Kind::Buffer(bytes) = &mut b.kind {
            let size = v.elem.size();
            for (k, x) in values.iter().enumerate() {
                let pos = v.offset + (at as usize + k) * size;
                if pos + size <= bytes.len() {
                    v.elem.write(bytes, pos, *x, true);
                }
            }
        }
        Ok(Value::Undefined)
    });
    def(i, &tp, "fill", |i, t, a| {
        let v = this_view(i, &t)?;
        let x = num(i, &arg(&a, 0))?;
        let (s, e) = span(i, &a, 1, v.len)?;
        for k in s..e {
            v.put(k, x);
        }
        Ok(t)
    });
    def(i, &tp, "copyWithin", |i, t, a| {
        let v = this_view(i, &t)?;
        let values = v.values();
        let (to, _) = span(i, &a, 0, v.len)?;
        let (s, e) = span(i, &a, 1, v.len)?;
        for (k, x) in values[s..e].iter().enumerate() {
            v.put(to + k, *x);
        }
        Ok(t)
    });
    def(i, &tp, "reverse", |i, t, _a| {
        let v = this_view(i, &t)?;
        for (k, x) in v.values().into_iter().rev().enumerate() {
            v.put(k, x);
        }
        Ok(t)
    });
    // Sorted as numbers unless a comparison is given.
    def(i, &tp, "sort", |i, t, a| {
        let v = this_view(i, &t)?;
        let cmp = arg(&a, 0);
        let mut values = v.values();
        if Interp::is_callable(&cmp) {
            let mut err = None;
            values.sort_by(|x, y| {
                if err.is_some() {
                    return std::cmp::Ordering::Equal;
                }
                match i.call(&cmp, Value::Undefined, vec![Value::Num(*x), Value::Num(*y)]).and_then(|r| i.to_number(&r)) {
                    Ok(n) if n < 0.0 => std::cmp::Ordering::Less,
                    Ok(n) if n > 0.0 => std::cmp::Ordering::Greater,
                    Ok(_) => std::cmp::Ordering::Equal,
                    Err(e) => {
                        err = Some(e);
                        std::cmp::Ordering::Equal
                    }
                }
            });
            if let Some(e) = err {
                return Err(e);
            }
        } else {
            values.sort_by(|x, y| x.total_cmp(y));
        }
        for (k, x) in values.into_iter().enumerate() {
            v.put(k, x);
        }
        Ok(t)
    });
    def(i, &tp, "toSorted", |i, t, a| {
        let copy = call_method(i, &t, "slice", vec![])?;
        call_method(i, &copy, "sort", a)
    });
    def(i, &tp, "toReversed", |i, t, _a| {
        let copy = call_method(i, &t, "slice", vec![])?;
        call_method(i, &copy, "reverse", vec![])
    });
    def(i, &tp, "with", |i, t, a| {
        let v = this_view(i, &t)?;
        let n = int(i, &a, 0, 0.0)?;
        let k = if n < 0.0 { v.len as f64 + n } else { n };
        if k < 0.0 || k >= v.len as f64 {
            return i.throw("RangeError", "invalid index");
        }
        let x = num(i, &arg(&a, 1))?;
        let copy = call_method(i, &t, "slice", vec![])?;
        if let Some(c) = view_of(&copy) {
            c.put(k as usize, x);
        }
        Ok(copy)
    });
    // The rest work as on arrays; those that make arrays make typed arrays here.
    let ap = i.array_proto.clone();
    for name in ["forEach", "join", "indexOf", "lastIndexOf", "includes", "find", "findIndex", "findLast", "findLastIndex", "every", "some", "reduce", "reduceRight", "at", "keys", "values", "entries", "toLocaleString"] {
        if let Some(Slot::Data(f)) = ap.borrow().get_own(name).cloned() {
            tp.borrow_mut().set_hidden(name, f);
        }
    }
    let values_fn = ap.borrow().get_own(crate::promise::SYM_ITERATOR).cloned();
    if let Some(Slot::Data(f)) = values_fn {
        tp.borrow_mut().set_hidden(crate::promise::SYM_ITERATOR, f.clone());
        tp.borrow_mut().set_hidden("values", f);
    }
    for name in ["map", "filter"] {
        let Some(Slot::Data(f)) = ap.borrow().get_own(name).cloned() else { continue };
        def(i, &tp, name, move |i, t, a| {
            let v = this_view(i, &t)?;
            let arr = i.call(&f, t.clone(), a)?;
            let list = items(i, &arr)?;
            let mut values = Vec::with_capacity(list.len());
            for x in &list {
                values.push(num(i, x)?);
            }
            let out = new_typed(i, v.elem, &values);
            if let (Value::Obj(o), Some(p)) = (&out, proto_of(&t)) {
                o.borrow_mut().proto = Some(p);
            }
            Ok(out)
        });
    }
    def(i, &tp, "toString", |i, t, _a| call_method(i, &t, "join", vec![]));
    hide_all(&tp);
    i.global_obj.borrow_mut().set_hidden("\0TypedArray", typed_ctor.clone());

    // ---------------------------------------------------------- Int8Array … BigUint64Array
    for (name, elem) in ELEMS {
        let proto = i.typed_protos[elem.index()].clone();
        proto.borrow_mut().proto = Some(tp.clone());
        let f = i.native(move |i, _t, a| construct(i, elem, &a));
        let Value::Obj(fo) = f.clone() else { unreachable!() };
        {
            let mut b = fo.borrow_mut();
            b.proto = Some(tc.clone());
            b.set_hidden("prototype", Value::Obj(proto.clone()));
            b.set_hidden("name", Value::Str(name.into()));
            b.set_hidden("length", Value::Num(3.0));
            b.set_hidden("BYTES_PER_ELEMENT", Value::Num(elem.size() as f64));
        }
        proto.borrow_mut().set_hidden("constructor", f.clone());
        proto.borrow_mut().set_hidden("BYTES_PER_ELEMENT", Value::Num(elem.size() as f64));
        def(i, &fo, "from", move |i, _t, a| {
            let list = i.iterate(&arg(&a, 0))?;
            let map = arg(&a, 1);
            let mut values = Vec::with_capacity(list.len());
            for (k, x) in list.into_iter().enumerate() {
                let x = if Interp::is_callable(&map) { i.call(&map, arg(&a, 2), vec![x, Value::Num(k as f64)])? } else { x };
                values.push(num(i, &x)?);
            }
            Ok(new_typed(i, elem, &values))
        });
        def(i, &fo, "of", move |i, _t, a| {
            let mut values = Vec::with_capacity(a.len());
            for x in &a {
                values.push(num(i, x)?);
            }
            Ok(new_typed(i, elem, &values))
        });
        hide_all(&fo);
        i.global_obj.borrow_mut().set_hidden(name, f);
    }

    // ---------------------------------------------------------- DataView
    let dp = i.data_view_proto.clone();
    let dv = i.native(|i, _t, a| {
        let Value::Obj(buffer) = arg(&a, 0) else { return i.throw("TypeError", "First argument to DataView constructor must be an ArrayBuffer") };
        let total = match &buffer.borrow().kind {
            Kind::Buffer(b) => b.len(),
            _ => return i.throw("TypeError", "First argument to DataView constructor must be an ArrayBuffer"),
        };
        let offset = int(i, &a, 1, 0.0)?;
        if offset < 0.0 || offset as usize > total {
            return i.throw("RangeError", "Start offset is outside the bounds of the buffer");
        }
        let offset = offset as usize;
        let len = int(i, &a, 2, (total - offset) as f64)?;
        if len < 0.0 || offset + len as usize > total {
            return i.throw("RangeError", "Invalid DataView length");
        }
        Ok(new_view(i, View { buffer, elem: Elem::U8, offset, len: len as usize, data_view: true }, None))
    });
    let Value::Obj(dvo) = dv.clone() else { unreachable!() };
    dvo.borrow_mut().set_hidden("prototype", Value::Obj(dp.clone()));
    dvo.borrow_mut().set_hidden("name", Value::Str("DataView".into()));
    dp.borrow_mut().set_hidden("constructor", dv.clone());
    dp.borrow_mut().set_hidden("\0@toStringTag", Value::Str("DataView".into()));
    fn this_dv(i: &Interp, t: &Value) -> Res<View> {
        match view_of(t) {
            Some(v) if v.data_view => Ok(v),
            _ => i.throw("TypeError", "not a DataView"),
        }
    }
    getter(i, &dp, "buffer", |i, t| Ok(Value::Obj(this_dv(i, &t)?.buffer)));
    getter(i, &dp, "byteLength", |i, t| Ok(Value::Num(this_dv(i, &t)?.len as f64)));
    getter(i, &dp, "byteOffset", |i, t| Ok(Value::Num(this_dv(i, &t)?.offset as f64)));
    for (name, elem) in [
        ("Int8", Elem::I8),
        ("Uint8", Elem::U8),
        ("Int16", Elem::I16),
        ("Uint16", Elem::U16),
        ("Int32", Elem::I32),
        ("Uint32", Elem::U32),
        ("Float32", Elem::F32),
        ("Float64", Elem::F64),
        ("BigInt64", Elem::I64),
        ("BigUint64", Elem::U64),
    ] {
        def(i, &dp, &format!("get{name}"), move |i, t, a| {
            let v = this_dv(i, &t)?;
            let at = int(i, &a, 0, 0.0)?;
            let little = Interp::truthy(&arg(&a, 1));
            if at < 0.0 || at as usize + elem.size() > v.len {
                return i.throw("RangeError", "Offset is outside the bounds of the DataView");
            }
            let b = v.buffer.borrow();
            let Kind::Buffer(bytes) = &b.kind else { return Ok(Value::Undefined) };
            Ok(elem.read_value(bytes, v.offset + at as usize, little))
        });
        def(i, &dp, &format!("set{name}"), move |i, t, a| {
            let v = this_dv(i, &t)?;
            let at = int(i, &a, 0, 0.0)?;
            let x = arg(&a, 1);
            // The value is converted before the bounds check, as in browsers.
            let x = if elem.is_big() { Value::BigInt(i.to_bigint(&x)?) } else { Value::Num(num(i, &x)?) };
            let little = Interp::truthy(&arg(&a, 2));
            if at < 0.0 || at as usize + elem.size() > v.len {
                return i.throw("RangeError", "Offset is outside the bounds of the DataView");
            }
            let buf = v.buffer.clone();
            let mut b = buf.borrow_mut();
            if let Kind::Buffer(bytes) = &mut b.kind {
                elem.write_value(i, bytes, v.offset + at as usize, &x, little)?;
            }
            Ok(Value::Undefined)
        });
    }
    hide_all(&dp);
    i.global_obj.borrow_mut().set_hidden("DataView", dv);
}
