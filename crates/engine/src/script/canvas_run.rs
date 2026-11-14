//! Carries out the commands a page's 2D contexts queued (see the context in
//! `prelude.js`) on the canvases in [`crate::canvas`]. The page's script
//! queues; this runs the queue when the page reads something back
//! (`getImageData`, `toDataURL`, `isPointInPath`) or a snapshot is taken.

use super::*;
use crate::canvas::path::{self, Cap, Join, Matrix, Path};
use crate::canvas::{self, Baseline, Context, Gradient, Image, Paint, TextAlign};
use std::sync::Arc;

// Opcodes (the same numbers as `prelude.js`).
const SAVE: u8 = 1;
const RESTORE: u8 = 2;
const RESET: u8 = 3;
const TRANSFORM: u8 = 10;
const SET_TRANSFORM: u8 = 11;
const SET: u8 = 20;
const DASH: u8 = 21;
const BEGIN: u8 = 30;
const MOVE: u8 = 31;
const LINE: u8 = 32;
const QUAD: u8 = 33;
const CUBIC: u8 = 34;
const ARC: u8 = 35;
const ELLIPSE: u8 = 36;
const ARC_TO: u8 = 37;
const RECT: u8 = 38;
const ROUND_RECT: u8 = 39;
const CLOSE: u8 = 40;
const SVG: u8 = 41;
const ADD_PATH: u8 = 42;
const FILL: u8 = 50;
const STROKE: u8 = 51;
const CLIP: u8 = 52;
const FILL_RECT: u8 = 53;
const STROKE_RECT: u8 = 54;
const CLEAR_RECT: u8 = 55;
const FILL_TEXT: u8 = 60;
const STROKE_TEXT: u8 = 61;
const DRAW_IMAGE: u8 = 70;
const GET_IMAGE_DATA: u8 = 80;
const PUT_IMAGE_DATA: u8 = 81;
const TO_DATA_URL: u8 = 82;
const HIT: u8 = 83;


fn num(v: &Value) -> f32 {
    match v {
        Value::Num(n) => *n as f32,
        Value::Bool(b) => *b as u8 as f32,
        _ => f32::NAN,
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        _ => String::new(),
    }
}

fn items(v: &Value) -> Vec<Value> {
    match v {
        Value::Obj(o) => match &o.borrow().kind {
            Kind::Array(a) => a.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The canvas size its attributes give (300 x 150 unless set).
fn size_of(st: &S, node: NodeId) -> (u32, u32) {
    let s = st.borrow();
    let px = |k: &str, d: u32| s.dom.attr(node, k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| *v >= 0.0 && v.is_finite()).map(|v| v as u32).unwrap_or(d);
    (px("width", 300), px("height", 150))
}

/// Takes a canvas's context out of the state (made, or remade at a new size, as needed).
fn take(st: &S, node: NodeId) -> Context {
    let (w, h) = size_of(st, node);
    let mut s = st.borrow_mut();
    match s.contexts.remove(&node) {
        Some(c) if c.width == w && c.height == h => c,
        Some(c) if c.width == 0 && c.height == 0 && w as u64 * h as u64 > 32 << 20 => c,
        _ => Context::new(w, h),
    }
}

/// Runs whatever a canvas's context has queued (so its pixels are up to date).
pub(super) fn flush(i: &mut Interp, st: &S, node: NodeId) -> Res<bool> {
    let Some(wrapper) = st.borrow().wrappers.get(&node).cloned() else { return Ok(false) };
    let ctx = i.get(&wrapper, "__ctx")?;
    if !matches!(ctx, Value::Obj(_)) {
        return Ok(false);
    }
    let q = i.get(&ctx, "_q")?;
    let list = items(&q);
    if !list.is_empty() {
        i.set(&ctx, "_q", i.array(Vec::new()))?;
    }
    run(i, st, node, &list)?;
    Ok(true)
}

/// A snapshot of a canvas's pixels, if it has a 2D context.
pub(super) fn snapshot(i: &mut Interp, st: &S, node: NodeId) -> Option<Arc<canvas::Canvas>> {
    if !flush(i, st, node).ok()? {
        return None;
    }
    let s = st.borrow();
    s.contexts.get(&node).map(|c| Arc::new(c.snapshot()))
}

fn color(v: &Value) -> Option<[f32; 4]> {
    let s = text(v);
    if s.eq_ignore_ascii_case("currentcolor") {
        return Some([0.0, 0.0, 0.0, 1.0]);
    }
    style::parse_color(&s).map(canvas::premul)
}

/// A fill or stroke style: a colour string, a gradient or a pattern.
fn paint(i: &mut Interp, st: &S, v: &Value, me: (NodeId, &Context)) -> Res<Option<Paint>> {
    match v {
        Value::Str(_) => Ok(color(v).map(Paint::Solid)),
        Value::Obj(_) => {
            let kind = text(&i.get(v, "_k")?);
            if kind == "pattern" {
                let src = i.get(v, "_i")?;
                let Some(img) = image_of(i, st, &src, me)? else { return Ok(None) };
                let rep = text(&i.get(v, "_r")?);
                let m = items(&i.get(v, "_m")?).iter().map(num).collect::<Vec<_>>();
                let m: Matrix = if m.len() == 6 && m.iter().all(|v| v.is_finite()) { [m[0], m[1], m[2], m[3], m[4], m[5]] } else { path::IDENTITY };
                let (rx, ry) = match rep.as_str() {
                    "repeat-x" => (true, false),
                    "repeat-y" => (false, true),
                    "no-repeat" => (false, false),
                    _ => (true, true),
                };
                return Ok(Some(Paint::Pattern { img, repeat_x: rx, repeat_y: ry, m }));
            }
            let a: Vec<f32> = items(&i.get(v, "_a")?).iter().map(num).collect();
            let g = match (kind.as_str(), a.len()) {
                ("linear", 4) => Gradient::Linear { p0: (a[0], a[1]), p1: (a[2], a[3]) },
                ("radial", 6) => Gradient::Radial { c0: (a[0], a[1]), r0: a[2], c1: (a[3], a[4]), r1: a[5] },
                ("conic", 3) => Gradient::Conic { c: (a[1], a[2]), angle: a[0] },
                _ => return Ok(None),
            };
            let mut stops = Vec::new();
            for s in items(&i.get(v, "_s")?) {
                let s = items(&s);
                if let (Some(o), Some(c)) = (s.first(), s.get(1).and_then(color)) {
                    stops.push((num(o).clamp(0.0, 1.0), c));
                }
            }
            // Stops in offset order; equal offsets keep the order they were added in.
            stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            Ok(Some(Paint::Gradient { g, stops: Arc::new(stops) }))
        }
        _ => Ok(None),
    }
}

/// The pixels of something `drawImage` or `createPattern` was given: an `<img>`,
/// another canvas (or this one), or an ImageBitmap / ImageData made in script.
fn image_of(i: &mut Interp, st: &S, v: &Value, me: (NodeId, &Context)) -> Res<Option<Arc<Image>>> {
    if let Some(node) = node_of(v) {
        let tag = st.borrow().dom.tag(node).map(|t| t.to_ascii_lowercase());
        return Ok(match tag.as_deref() {
            Some("canvas") if node == me.0 => Some(Arc::new(me.1.image())),
            Some("canvas") => {
                flush(i, st, node)?;
                st.borrow().contexts.get(&node).map(|c| Arc::new(c.image()))
            }
            Some("img" | "image") => {
                let src = st.borrow().dom.attr(node, "src").map(|s| s.to_string()).or_else(|| st.borrow().dom.attr(node, "href").map(|s| s.to_string()));
                match src {
                    Some(src) if !src.trim().is_empty() => picture(st, &src),
                    _ => None,
                }
            }
            _ => None,
        });
    }
    if !matches!(v, Value::Obj(_)) {
        return Ok(None);
    }
    // ImageData (or an ImageBitmap made from one): its bytes.
    let data = i.get(v, "data")?;
    if let Some(bytes) = volant_js::typed::bytes_of(&data) {
        let (w, h) = (num(&i.get(v, "width")?), num(&i.get(v, "height")?));
        if w > 0.0 && h > 0.0 && bytes.len() >= (w as usize) * (h as usize) * 4 {
            return Ok(Some(Arc::new(Image::from_straight(w as u32, h as u32, &bytes))));
        }
        return Ok(None);
    }
    // An ImageBitmap of an element.
    let inner = i.get(v, "_src")?;
    if !matches!(inner, Value::Undefined | Value::Null) {
        return image_of(i, st, &inner, me);
    }
    Ok(None)
}

/// An `<img>`'s picture, fetched and decoded once per URL.
pub(super) fn picture(st: &S, src: &str) -> Option<Arc<Image>> {
    let url = {
        let s = st.borrow();
        resolve_url(&s.url, src.trim())
    };
    if let Some(hit) = st.borrow().pictures.get(&url) {
        return hit.clone();
    }
    let (mime, bytes) = match volant_core::base64::data_url(&url) {
        Some(d) => d,
        None => {
            let (host, page) = {
                let s = st.borrow();
                (s.host.clone(), s.url.clone())
            };
            match host.get_for(&url, &page) {
                Some(r) if r.success() => (r.content_type.clone(), r.body),
                _ => {
                    st.borrow_mut().pictures.insert(url, None);
                    return None;
                }
            }
        }
    };
    let img = decode_picture(st, &bytes, &mime).map(Arc::new);
    let mut s = st.borrow_mut();
    if s.pictures.len() > 256 {
        s.pictures.clear();
    }
    s.pictures.insert(url, img.clone());
    img
}

pub(super) fn decode_picture(st: &S, bytes: &[u8], mime: &str) -> Option<Image> {
    if bytes.starts_with(b"\x89PNG") {
        if let Ok((w, h, rgba)) = volant_core::png::decode(bytes) {
            return Some(Image::from_straight(w, h, &rgba));
        }
    }
    let host = st.borrow().host.clone();
    let (w, h, rgba) = host.decode_image(bytes, mime)?;
    (rgba.len() >= w as usize * h as usize * 4).then(|| Image::from_straight(w, h, &rgba))
}

/// Path commands (the queue's, or a Path2D's) from `at`; returns where they stop.
fn path_cmd(op: u8, a: &[Value], at: usize, m: &Matrix, p: &mut Path) -> Option<usize> {
    let n = |k: usize| a.get(at + k).map(num).unwrap_or(f32::NAN);
    let finite = |k: usize| (0..k).all(|j| n(j).is_finite());
    Some(match op {
        BEGIN => {
            *p = Path::default();
            at
        }
        MOVE => {
            if finite(2) {
                p.move_to(m, n(0), n(1));
            }
            at + 2
        }
        LINE => {
            if finite(2) {
                p.line_to(m, n(0), n(1));
            }
            at + 2
        }
        QUAD => {
            if finite(4) {
                p.quad_to(m, n(0), n(1), n(2), n(3));
            }
            at + 4
        }
        CUBIC => {
            if finite(6) {
                p.cubic_to(m, n(0), n(1), n(2), n(3), n(4), n(5));
            }
            at + 6
        }
        ARC => {
            if finite(5) {
                p.ellipse(m, n(0), n(1), n(2), n(2), 0.0, n(3), n(4), n(5) != 0.0);
            }
            at + 6
        }
        ELLIPSE => {
            if finite(7) {
                p.ellipse(m, n(0), n(1), n(2), n(3), n(4), n(5), n(6), n(7) != 0.0);
            }
            at + 8
        }
        ARC_TO => {
            if finite(5) {
                p.arc_to(m, n(0), n(1), n(2), n(3), n(4));
            }
            at + 5
        }
        RECT => {
            if finite(4) {
                p.rect(m, n(0), n(1), n(2), n(3));
            }
            at + 4
        }
        ROUND_RECT => {
            let radii: Vec<(f32, f32)> = items(a.get(at + 4)?).chunks(2).map(|c| (c.first().map(num).unwrap_or(0.0), c.get(1).map(num).unwrap_or(0.0))).collect();
            if finite(4) {
                p.round_rect(m, n(0), n(1), n(2), n(3), &radii);
            }
            at + 5
        }
        CLOSE => {
            p.close();
            at
        }
        SVG => {
            p.svg(m, &text(a.get(at)?));
            at + 1
        }
        ADD_PATH => {
            let cmds = items(a.get(at)?);
            let t = [n(1), n(2), n(3), n(4), n(5), n(6)];
            let mm = if t.iter().all(|v| v.is_finite()) { path::multiply(m, &t) } else { *m };
            let mut sub = Path::default();
            build(&cmds, &mm, &mut sub);
            p.extend(&sub);
            at + 7
        }
        _ => return None,
    })
}

/// A Path2D's commands, replayed through `m`.
fn build(cmds: &[Value], m: &Matrix, p: &mut Path) {
    let mut k = 0;
    while k < cmds.len() {
        let op = num(&cmds[k]) as u8;
        match path_cmd(op, cmds, k + 1, m, p) {
            Some(next) => k = next,
            None => break,
        }
    }
}

/// The path a fill, stroke, clip or hit test uses: a Path2D's (through the current transform) or the current one.
fn path_arg(v: &Value, ctx: &Context) -> Path {
    if matches!(v, Value::Obj(_)) {
        let mut p = Path::default();
        build(&items(v), &ctx.state.m, &mut p);
        p
    } else {
        ctx.path.clone()
    }
}

/// Runs `q` on canvas `node`. Returns what the last command asked for (image data, a URL, a hit test).
pub(super) fn run(i: &mut Interp, st: &S, node: NodeId, q: &[Value]) -> Res<Value> {
    let mut ctx = take(st, node);
    let out = run_on(i, st, node, &mut ctx, q);
    st.borrow_mut().contexts.insert(node, ctx);
    out
}

fn run_on(i: &mut Interp, st: &S, node: NodeId, ctx: &mut Context, q: &[Value]) -> Res<Value> {
    let mut k = 0;
    let mut result = Value::Undefined;
    while k < q.len() {
        let op = num(&q[k]) as u8;
        k += 1;
        let n = |j: usize| q.get(k + j).map(num).unwrap_or(f32::NAN);
        let v = |j: usize| q.get(k + j).cloned().unwrap_or(Value::Undefined);
        if let Some(next) = path_cmd(op, q, k, &ctx.state.m.clone(), &mut ctx.path) {
            k = next;
            continue;
        }
        match op {
            SAVE => ctx.save(),
            RESTORE => ctx.restore(),
            RESET => {
                let (w, h) = (ctx.width, ctx.height);
                ctx.reset(w, h);
            }
            TRANSFORM | SET_TRANSFORM => {
                let m = [n(0), n(1), n(2), n(3), n(4), n(5)];
                if op == TRANSFORM {
                    ctx.transform(&m);
                } else {
                    ctx.set_transform(&m);
                }
                k += 6;
            }
            SET => {
                let (prop, val) = (n(0) as u8, v(1));
                k += 2;
                let s = &mut ctx.state;
                match prop {
                    0 | 1 => {
                        if let Some(p) = paint(i, st, &val, (node, &*ctx))? {
                            if prop == 0 {
                                ctx.state.fill = p;
                            } else {
                                ctx.state.stroke = p;
                            }
                        }
                    }
                    2 => s.line_width = num(&val),
                    3 => {
                        s.cap = match text(&val).as_str() {
                            "round" => Cap::Round,
                            "square" => Cap::Square,
                            _ => Cap::Butt,
                        }
                    }
                    4 => {
                        s.join = match text(&val).as_str() {
                            "round" => Join::Round,
                            "bevel" => Join::Bevel,
                            _ => Join::Miter,
                        }
                    }
                    5 => s.miter = num(&val),
                    6 => s.alpha = num(&val).clamp(0.0, 1.0),
                    7 => {
                        if let Some(o) = canvas::parse_op(&text(&val)) {
                            s.op = o;
                        }
                    }
                    8 => {
                        if let Some(f) = canvas::parse_font(&text(&val)) {
                            s.font = f;
                        }
                    }
                    9 => {
                        s.align = match text(&val).as_str() {
                            "end" => TextAlign::End,
                            "left" => TextAlign::Left,
                            "right" => TextAlign::Right,
                            "center" => TextAlign::Center,
                            _ => TextAlign::Start,
                        }
                    }
                    10 => {
                        s.baseline = match text(&val).as_str() {
                            "top" => Baseline::Top,
                            "hanging" => Baseline::Hanging,
                            "middle" => Baseline::Middle,
                            "ideographic" => Baseline::Ideographic,
                            "bottom" => Baseline::Bottom,
                            _ => Baseline::Alphabetic,
                        }
                    }
                    11 => s.shadow_blur = num(&val).max(0.0),
                    12 => s.shadow = color(&val).unwrap_or([0.0; 4]),
                    13 => s.shadow_offset.0 = num(&val),
                    14 => s.shadow_offset.1 = num(&val),
                    15 => s.smoothing = matches!(val, Value::Bool(true)),
                    16 => s.dash_offset = num(&val),
                    17 => s.rtl = text(&val) == "rtl",
                    _ => {}
                }
            }
            DASH => {
                ctx.state.dash = items(&v(0)).iter().map(num).collect();
                k += 1;
            }
            FILL | CLIP => {
                let (evenodd, p) = (n(0) != 0.0, path_arg(&v(1), ctx));
                k += 2;
                if op == FILL {
                    ctx.fill_path(&p, evenodd);
                } else {
                    ctx.clip_path(&p, evenodd);
                }
            }
            STROKE => {
                let p = path_arg(&v(0), ctx);
                k += 1;
                ctx.stroke_path(&p);
            }
            FILL_RECT => {
                ctx.fill_rect(n(0), n(1), n(2), n(3));
                k += 4;
            }
            STROKE_RECT => {
                ctx.stroke_rect(n(0), n(1), n(2), n(3));
                k += 4;
            }
            CLEAR_RECT => {
                ctx.clear_rect(n(0), n(1), n(2), n(3));
                k += 4;
            }
            FILL_TEXT | STROKE_TEXT => {
                // No maxWidth, or one (a bad one draws nothing).
                let max = if matches!(v(3), Value::Undefined | Value::Null) { None } else { Some(n(3)) };
                let (t, x, y) = (text(&v(0)), n(1), n(2));
                k += 4;
                ctx.text(&t, x, y, max, op == STROKE_TEXT);
            }
            DRAW_IMAGE => {
                let (src, count) = (v(0), n(1) as usize);
                let a: Vec<f32> = (0..8).map(|j| n(2 + j)).collect();
                k += 10;
                let Some(img) = image_of(i, st, &src, (node, &*ctx))? else { continue };
                let (iw, ih) = (img.width as f32, img.height as f32);
                match count {
                    2 => ctx.draw_image(&img, 0.0, 0.0, iw, ih, a[0], a[1], iw, ih),
                    4 => ctx.draw_image(&img, 0.0, 0.0, iw, ih, a[0], a[1], a[2], a[3]),
                    8 => ctx.draw_image(&img, a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]),
                    _ => {}
                }
            }
            GET_IMAGE_DATA => {
                let (x, y, w, h) = (n(0), n(1), n(2), n(3));
                k += 4;
                let bytes = ctx.get_image_data(x as i32, y as i32, w.max(0.0) as u32, h.max(0.0) as u32);
                result = volant_js::typed::new_clamped(i, bytes);
            }
            PUT_IMAGE_DATA => {
                let data = volant_js::typed::bytes_of(&v(0)).unwrap_or_default();
                let (w, h, x, y) = (n(1) as u32, n(2) as u32, n(3) as i32, n(4) as i32);
                let dirty = (n(5) as i32, n(6) as i32, n(7) as i32, n(8) as i32);
                k += 9;
                ctx.put_image_data(&data, w, h, x, y, dirty);
            }
            TO_DATA_URL => {
                k += 1;
                result = str_val(&if ctx.width == 0 || ctx.height == 0 { "data:,".to_string() } else { format!("data:image/png;base64,{}", volant_core::base64::encode(&ctx.png())) });
            }
            HIT => {
                let (stroke, x, y, evenodd) = (n(0) != 0.0, n(1), n(2), n(3) != 0.0);
                let p = path_arg(&v(4), ctx);
                k += 5;
                let hit = x.is_finite() && y.is_finite() && if stroke { path::contains(&ctx.stroke_outline(&p), (x, y), false) } else { path::contains(&p.polys(), (x, y), evenodd) };
                result = Value::Bool(hit);
            }
            _ => break,
        }
    }
    Ok(result)
}
