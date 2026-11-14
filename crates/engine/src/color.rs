//! CSS Color 4 and 5 functions: `rgb()`, `hsl()`, `hwb()`, `lab()`, `lch()`,
//! `oklab()`, `oklch()`, `color()` and `color-mix()`, parsed to the grammar
//! (legacy comma syntax, modern space syntax, `none`) and converted to sRGB.
//! Colours outside sRGB are clipped to it.

use crate::style::Color;

/// A colour space a CSS colour can be written in (or mixed in).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Space {
    Srgb,
    SrgbLinear,
    DisplayP3,
    A98,
    ProPhoto,
    Rec2020,
    XyzD50,
    XyzD65,
    Lab,
    Lch,
    Oklab,
    Oklch,
    Hsl,
    Hwb,
}

impl Space {
    fn named(s: &str) -> Option<Space> {
        Some(match s {
            "srgb" => Space::Srgb,
            "srgb-linear" => Space::SrgbLinear,
            "display-p3" => Space::DisplayP3,
            "a98-rgb" => Space::A98,
            "prophoto-rgb" => Space::ProPhoto,
            "rec2020" => Space::Rec2020,
            "xyz-d50" => Space::XyzD50,
            "xyz" | "xyz-d65" => Space::XyzD65,
            "lab" => Space::Lab,
            "lch" => Space::Lch,
            "oklab" => Space::Oklab,
            "oklch" => Space::Oklch,
            "hsl" => Space::Hsl,
            "hwb" => Space::Hwb,
            _ => return None,
        })
    }

    /// Which component is a hue, for polar spaces.
    fn hue(self) -> Option<usize> {
        match self {
            Space::Lch | Space::Oklch => Some(2),
            Space::Hsl | Space::Hwb => Some(0),
            _ => None,
        }
    }
}

type M3 = [[f64; 3]; 3];

fn mul(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

fn inv(m: &M3) -> M3 {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let k = 1.0 / det;
    [
        [(e * i - f * h) * k, (c * h - b * i) * k, (b * f - c * e) * k],
        [(f * g - d * i) * k, (a * i - c * g) * k, (c * d - a * f) * k],
        [(d * h - e * g) * k, (b * g - a * h) * k, (a * e - b * d) * k],
    ]
}

const SRGB: M3 = [
    [0.41239079926595934, 0.357584339383878, 0.1804807884018343],
    [0.21263900587151027, 0.715168678767756, 0.07219231536073371],
    [0.01933081871559182, 0.11919477979462598, 0.9505321522496607],
];
const P3: M3 = [
    [0.4865709486482162, 0.26566769316909306, 0.1982172852343625],
    [0.2289745640697488, 0.6917385218365064, 0.079286914093745],
    [0.0, 0.04511338185890264, 1.043944368900976],
];
const A98: M3 = [
    [0.5766690429101305, 0.1855582379065463, 0.1882286462349947],
    [0.29734497525053605, 0.6273635662554661, 0.07529145849399788],
    [0.02703136138641234, 0.07068885253582723, 0.9913375368376388],
];
const REC2020: M3 = [
    [0.6369580483012914, 0.14461690358620832, 0.1688809751641721],
    [0.2627002120112671, 0.6779980715188708, 0.05930171646986196],
    [0.0, 0.028072693049087428, 1.060985057710791],
];
/// ProPhoto is relative to D50.
const PROPHOTO: M3 = [
    [0.7977666449006423, 0.13518129740053308, 0.0313477341283922],
    [0.2880748288194013, 0.711835234241873, 0.00008993693872564],
    [0.0, 0.0, 0.8251046025104602],
];
/// Bradford adaptation from D50 to D65.
const D50_D65: M3 = [
    [0.955473421488075, -0.02309845494876471, 0.06325924320057072],
    [-0.0283697093338637, 1.0099953980813041, 0.021041441191917323],
    [0.012314014864481998, -0.020507649298898964, 1.330365926242124],
];
const XYZ_LMS: M3 = [
    [0.8190224379967030, 0.3619062600528904, -0.1288737815209879],
    [0.0329836539323885, 0.9292868615863434, 0.0361446663506424],
    [0.0481771893596242, 0.2642395317527308, 0.6335478284694309],
];
const LMS_OKLAB: M3 = [
    [0.2104542683093140, 0.7936177747023054, -0.0040720430116193],
    [1.9779985324311684, -2.4285922420485799, 0.4505937096174110],
    [0.0259040424655478, 0.7827717124575296, -0.8086757549230774],
];
const D50: [f64; 3] = [0.3457 / 0.3585, 1.0, (1.0 - 0.3457 - 0.3585) / 0.3585];

fn signed(c: f64, f: impl Fn(f64) -> f64) -> f64 {
    c.signum() * f(c.abs())
}

fn srgb_lin(c: f64) -> f64 {
    signed(c, |c| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) })
}

fn srgb_gam(c: f64) -> f64 {
    signed(c, |c| if c > 0.0031308 { 1.055 * c.powf(1.0 / 2.4) - 0.055 } else { 12.92 * c })
}

const R2020_A: f64 = 1.09929682680944;
const R2020_B: f64 = 0.018053968510807;

fn lin(space: Space, c: f64) -> f64 {
    match space {
        Space::A98 => signed(c, |c| c.powf(563.0 / 256.0)),
        Space::ProPhoto => signed(c, |c| if c <= 16.0 / 512.0 { c / 16.0 } else { c.powf(1.8) }),
        Space::Rec2020 => signed(c, |c| if c < R2020_B * 4.5 { c / 4.5 } else { ((c + R2020_A - 1.0) / R2020_A).powf(1.0 / 0.45) }),
        Space::SrgbLinear => c,
        _ => srgb_lin(c),
    }
}

fn gam(space: Space, c: f64) -> f64 {
    match space {
        Space::A98 => signed(c, |c| c.powf(256.0 / 563.0)),
        Space::ProPhoto => signed(c, |c| if c >= 1.0 / 512.0 { c.powf(1.0 / 1.8) } else { 16.0 * c }),
        Space::Rec2020 => signed(c, |c| if c > R2020_B { R2020_A * c.powf(0.45) - (R2020_A - 1.0) } else { 4.5 * c }),
        Space::SrgbLinear => c,
        _ => srgb_gam(c),
    }
}

const KAPPA: f64 = 24389.0 / 27.0;
const EPS: f64 = 216.0 / 24389.0;

fn lab_xyz50([l, a, b]: [f64; 3]) -> [f64; 3] {
    let f1 = (l + 16.0) / 116.0;
    let (f0, f2) = (a / 500.0 + f1, f1 - b / 200.0);
    let x = if f0.powi(3) > EPS { f0.powi(3) } else { (116.0 * f0 - 16.0) / KAPPA };
    let y = if l > KAPPA * EPS { f1.powi(3) } else { l / KAPPA };
    let z = if f2.powi(3) > EPS { f2.powi(3) } else { (116.0 * f2 - 16.0) / KAPPA };
    [x * D50[0], y * D50[1], z * D50[2]]
}

fn xyz50_lab(xyz: [f64; 3]) -> [f64; 3] {
    let f = [0, 1, 2].map(|i| {
        let v = xyz[i] / D50[i];
        if v > EPS {
            v.cbrt()
        } else {
            (KAPPA * v + 16.0) / 116.0
        }
    });
    [116.0 * f[1] - 16.0, 500.0 * (f[0] - f[1]), 200.0 * (f[1] - f[2])]
}

fn polar([l, a, b]: [f64; 3]) -> [f64; 3] {
    let c = (a * a + b * b).sqrt();
    // An achromatic colour's hue is missing (NaN), as the spec says.
    let h = if c < 1e-9 { f64::NAN } else { b.atan2(a).to_degrees().rem_euclid(360.0) };
    [l, c, h]
}

fn rect([l, c, h]: [f64; 3]) -> [f64; 3] {
    let h = if h.is_nan() { 0.0 } else { h.to_radians() };
    [l, c * h.cos(), c * h.sin()]
}

fn hsl_rgb([h, s, l]: [f64; 3]) -> [f64; 3] {
    let h = if h.is_nan() { 0.0 } else { h.rem_euclid(360.0) };
    let (s, l) = (s / 100.0, l / 100.0);
    let f = |n: f64| {
        let k = (n + h / 30.0) % 12.0;
        l - s * l.min(1.0 - l) * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    };
    [f(0.0), f(8.0), f(4.0)]
}

fn rgb_hsl([r, g, b]: [f64; 3]) -> [f64; 3] {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    let mut h = f64::NAN;
    let mut s = 0.0;
    if d > 1e-9 {
        s = if l == 0.0 || l == 1.0 { 0.0 } else { (max - l) / l.min(1.0 - l) };
        h = 60.0
            * if max == r {
                (g - b) / d + if g < b { 6.0 } else { 0.0 }
            } else if max == g {
                (b - r) / d + 2.0
            } else {
                (r - g) / d + 4.0
            };
    }
    [h, s * 100.0, l * 100.0]
}

fn hwb_rgb([h, w, b]: [f64; 3]) -> [f64; 3] {
    let (w, b) = (w / 100.0, b / 100.0);
    if w + b >= 1.0 {
        let g = w / (w + b);
        return [g, g, g];
    }
    hsl_rgb([h, 100.0, 50.0]).map(|c| c * (1.0 - w - b) + w)
}

fn rgb_hwb(rgb: [f64; 3]) -> [f64; 3] {
    let h = rgb_hsl(rgb)[0];
    let w = rgb[0].min(rgb[1]).min(rgb[2]);
    let b = 1.0 - rgb[0].max(rgb[1]).max(rgb[2]);
    [h, w * 100.0, b * 100.0]
}

/// Components in `space` to CIE XYZ (D65).
fn to_xyz(space: Space, c: [f64; 3]) -> [f64; 3] {
    let rgb = |m: &M3| mul(m, c.map(|v| lin(space, v)));
    match space {
        Space::Srgb | Space::SrgbLinear => rgb(&SRGB),
        Space::DisplayP3 => rgb(&P3),
        Space::A98 => rgb(&A98),
        Space::Rec2020 => rgb(&REC2020),
        Space::ProPhoto => mul(&D50_D65, rgb(&PROPHOTO)),
        Space::XyzD65 => c,
        Space::XyzD50 => mul(&D50_D65, c),
        Space::Lab => mul(&D50_D65, lab_xyz50(c)),
        Space::Lch => mul(&D50_D65, lab_xyz50(rect(c))),
        Space::Oklab | Space::Oklch => {
            let lab = if space == Space::Oklch { rect(c) } else { c };
            mul(&inv(&XYZ_LMS), mul(&inv(&LMS_OKLAB), lab).map(|v| v * v * v))
        }
        Space::Hsl => to_xyz(Space::Srgb, hsl_rgb(c)),
        Space::Hwb => to_xyz(Space::Srgb, hwb_rgb(c)),
    }
}

/// CIE XYZ (D65) to components in `space`.
fn from_xyz(space: Space, xyz: [f64; 3]) -> [f64; 3] {
    let rgb = |m: &M3, xyz: [f64; 3]| mul(&inv(m), xyz).map(|v| gam(space, v));
    match space {
        Space::Srgb | Space::SrgbLinear => rgb(&SRGB, xyz),
        Space::DisplayP3 => rgb(&P3, xyz),
        Space::A98 => rgb(&A98, xyz),
        Space::Rec2020 => rgb(&REC2020, xyz),
        Space::ProPhoto => rgb(&PROPHOTO, mul(&inv(&D50_D65), xyz)),
        Space::XyzD65 => xyz,
        Space::XyzD50 => mul(&inv(&D50_D65), xyz),
        Space::Lab => xyz50_lab(mul(&inv(&D50_D65), xyz)),
        Space::Lch => polar(xyz50_lab(mul(&inv(&D50_D65), xyz))),
        Space::Oklab | Space::Oklch => {
            let lab = mul(&LMS_OKLAB, mul(&XYZ_LMS, xyz).map(f64::cbrt));
            if space == Space::Oklch {
                polar(lab)
            } else {
                lab
            }
        }
        Space::Hsl => rgb_hsl(from_xyz(Space::Srgb, xyz)),
        Space::Hwb => rgb_hwb(from_xyz(Space::Srgb, xyz)),
    }
}

/// A colour in some space, with alpha; NaN is a missing (`none`) component.
#[derive(Debug, Clone, Copy)]
struct Value {
    space: Space,
    c: [f64; 3],
    alpha: f64,
}

impl Value {
    fn to(self, space: Space) -> Value {
        if space == self.space {
            return self;
        }
        let c = self.c.map(|v| if v.is_nan() { 0.0 } else { v });
        let c = if let Some(h) = self.space.hue() {
            let mut c = c;
            c[h] = self.c[h];
            c
        } else {
            c
        };
        Value { space, c: from_xyz(space, to_xyz(self.space, c)), alpha: self.alpha }
    }

    fn srgb(self) -> Color {
        let c = if self.space == Space::Srgb { self.c.map(|v| if v.is_nan() { 0.0 } else { v }) } else { self.to(Space::Srgb).c };
        let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let a = if self.alpha.is_nan() { 0.0 } else { self.alpha };
        Color { r: byte(c[0]), g: byte(c[1]), b: byte(c[2]), a: byte(a) }
    }
}

impl From<Color> for Value {
    fn from(c: Color) -> Value {
        Value { space: Space::Srgb, c: [c.r, c.g, c.b].map(|v| v as f64 / 255.0), alpha: c.a as f64 / 255.0 }
    }
}

/// A CSS `<number>`: digits with an optional fraction and exponent (no `inf`, `1.`, or hex).
fn number(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let int = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    i += int;
    let mut frac = 0;
    if b.get(i) == Some(&b'.') {
        frac = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if frac == 0 {
            return None;
        }
        i += 1 + frac;
    }
    if int + frac == 0 {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let exp = b[j..].iter().take_while(|c| c.is_ascii_digit()).count();
        if exp > 0 {
            i = j + exp;
        }
    }
    if i != b.len() {
        return None;
    }
    s.parse().ok().filter(|v: &f64| v.is_finite())
}

/// A component: a number, a percentage scaled so 100% is `pct`, or `none` (NaN).
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Num,
    Pct,
    None,
}

fn component(s: &str, pct: f64) -> Option<(f64, Kind)> {
    if s == "none" {
        return Some((f64::NAN, Kind::None));
    }
    if let Some(n) = s.strip_suffix('%') {
        return Some((number(n)? / 100.0 * pct, Kind::Pct));
    }
    Some((number(s)?, Kind::Num))
}

/// A hue: a number (degrees) or an angle.
fn hue(s: &str) -> Option<f64> {
    if s == "none" {
        return Some(f64::NAN);
    }
    for (unit, per) in [("deg", 1.0), ("grad", 0.9), ("rad", 180.0 / std::f64::consts::PI), ("turn", 360.0)] {
        if let Some(n) = s.strip_suffix(unit) {
            return Some(number(n)? * per);
        }
    }
    number(s)
}

fn alpha(s: Option<&str>) -> Option<f64> {
    match s {
        None => Some(1.0),
        Some(s) => Some(component(s, 1.0)?.0.clamp(0.0, 1.0)).map(|a| if s == "none" { f64::NAN } else { a }),
    }
}

/// The top-level pieces of a function's arguments: values, commas and slashes.
#[derive(Debug, PartialEq)]
enum Tok<'a> {
    Val(&'a str),
    Comma,
    Slash,
}

fn tokens(s: &str) -> Option<Vec<Tok<'_>>> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0usize, None::<usize>);
    for (i, ch) in s.char_indices() {
        match ch {
            '(' => {
                depth += 1;
                start.get_or_insert(i);
            }
            ')' => {
                depth = depth.checked_sub(1)?;
            }
            _ if depth > 0 => {}
            ',' | '/' | ' ' | '\t' | '\n' | '\r' | '\u{c}' => {
                if let Some(st) = start.take() {
                    out.push(Tok::Val(&s[st..i]));
                }
                match ch {
                    ',' => out.push(Tok::Comma),
                    '/' => out.push(Tok::Slash),
                    _ => {}
                }
            }
            _ => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(st) = start {
        // An unclosed nested function closes at the end, as CSS does.
        out.push(Tok::Val(&s[st..]));
    }
    Some(out)
}

/// Three components and an optional alpha, and whether they were written the legacy (comma) way.
fn args<'a>(t: &[Tok<'a>]) -> Option<([&'a str; 3], Option<&'a str>, bool)> {
    let val = |k: usize| match t.get(k) {
        Some(Tok::Val(v)) => Some(*v),
        _ => None,
    };
    if t.contains(&Tok::Comma) {
        // a, b, c[, d]
        if !(t.len() == 5 || t.len() == 7) || (1..t.len()).step_by(2).any(|k| t[k] != Tok::Comma) {
            return None;
        }
        let v = [val(0)?, val(2)?, val(4)?];
        return Some((v, if t.len() == 7 { Some(val(6)?) } else { None }, true));
    }
    match t.len() {
        3 => Some(([val(0)?, val(1)?, val(2)?], None, false)),
        5 if t[3] == Tok::Slash => Some(([val(0)?, val(1)?, val(2)?], Some(val(4)?), false)),
        _ => None,
    }
}

thread_local! {
    static CURRENT: std::cell::Cell<Color> = const { std::cell::Cell::new(Color::BLACK) };
}

/// What `currentColor` is inside colour functions from now on (on this thread).
pub fn set_current(c: Color) {
    CURRENT.with(|k| k.set(c));
}

/// A colour function, already lowercased (`rgb(…)`, `oklch(…)`, `color-mix(…)`, …).
pub fn function(v: &str) -> Option<Color> {
    value(v).map(Value::srgb)
}

fn value(v: &str) -> Option<Value> {
    let open = v.find('(')?;
    let name = &v[..open];
    let body = &v[open + 1..];
    // A missing `)` at the very end is allowed (the end of the input closes it).
    let body = body.strip_suffix(')').unwrap_or(body);
    let t = tokens(body)?;
    if name == "color-mix" {
        return mix(&t);
    }
    if name == "color" {
        return color_fn(&t);
    }
    let (c, a, legacy) = args(&t)?;
    let a_val = alpha(a)?;
    if legacy && a.is_some_and(|a| a == "none") {
        return None;
    }
    let fixed = |space: Space, c: [f64; 3]| Some(Value { space, c, alpha: a_val });
    match name {
        "rgb" | "rgba" => {
            let p = c.map(|s| component(s, 255.0));
            let [Some(r), Some(g), Some(b)] = p else { return None };
            if legacy && (r.1 == Kind::None || g.1 == Kind::None || b.1 == Kind::None || r.1 != g.1 || g.1 != b.1) {
                return None;
            }
            fixed(Space::Srgb, [r.0, g.0, b.0].map(|v| if v.is_nan() { v } else { v.clamp(0.0, 255.0) / 255.0 }))
        }
        "hsl" | "hsla" => {
            let h = hue(c[0])?;
            let (s, l) = (component(c[1], 100.0)?, component(c[2], 100.0)?);
            if legacy && (h.is_nan() || s.1 != Kind::Pct || l.1 != Kind::Pct) {
                return None;
            }
            // (Not clamped: out of range values make colours outside sRGB, which are then clipped.)
            fixed(Space::Hsl, [h, s.0.max(0.0), l.0])
        }
        "hwb" if !legacy => {
            let h = hue(c[0])?;
            let (w, b) = (component(c[1], 100.0)?.0, component(c[2], 100.0)?.0);
            fixed(Space::Hwb, [h, w, b])
        }
        "lab" if !legacy => {
            let l = component(c[0], 100.0)?.0;
            fixed(Space::Lab, [l.clamp(0.0, 100.0), component(c[1], 125.0)?.0, component(c[2], 125.0)?.0])
        }
        "lch" if !legacy => {
            let l = component(c[0], 100.0)?.0;
            fixed(Space::Lch, [l.clamp(0.0, 100.0), component(c[1], 150.0)?.0.max(0.0), hue(c[2])?])
        }
        "oklab" if !legacy => {
            let l = component(c[0], 1.0)?.0;
            fixed(Space::Oklab, [l.clamp(0.0, 1.0), component(c[1], 0.4)?.0, component(c[2], 0.4)?.0])
        }
        "oklch" if !legacy => {
            let l = component(c[0], 1.0)?.0;
            fixed(Space::Oklch, [l.clamp(0.0, 1.0), component(c[1], 0.4)?.0.max(0.0), hue(c[2])?])
        }
        _ => None,
    }
}

/// `color(space c1 c2 c3 [/ alpha])`.
fn color_fn(t: &[Tok]) -> Option<Value> {
    let Some(Tok::Val(name)) = t.first() else { return None };
    let space = Space::named(name).filter(|s| !matches!(s, Space::Lab | Space::Lch | Space::Oklab | Space::Oklch | Space::Hsl | Space::Hwb))?;
    let (c, a, legacy) = args(&t[1..])?;
    if legacy {
        return None;
    }
    let mut v = [0.0; 3];
    for k in 0..3 {
        v[k] = component(c[k], 1.0)?.0;
    }
    Some(Value { space, c: v, alpha: alpha(a)? })
}

/// `color-mix(in space [hue-method hue], color [p%], color [p%])`.
fn mix(t: &[Tok]) -> Option<Value> {
    let parts: Vec<&[Tok]> = t.split(|x| *x == Tok::Comma).collect();
    let [method, a, b] = parts[..] else { return None };
    let words: Vec<&str> = method.iter().map(|x| if let Tok::Val(v) = x { Some(*v) } else { None }).collect::<Option<_>>()?;
    let (space, hue_method) = match words[..] {
        ["in", s] => (Space::named(s)?, "shorter"),
        ["in", s, m, "hue"] if Space::named(s)?.hue().is_some() && matches!(m, "shorter" | "longer" | "increasing" | "decreasing") => (Space::named(s)?, m),
        _ => return None,
    };
    let stop = |p: &[Tok]| -> Option<(Value, Option<f64>)> {
        let vals: Vec<&str> = p.iter().map(|x| if let Tok::Val(v) = x { Some(*v) } else { None }).collect::<Option<_>>()?;
        let pct = |s: &str| s.strip_suffix('%').and_then(number).filter(|v| (0.0..=100.0).contains(v)).map(|v| v / 100.0);
        let (c, p) = match vals[..] {
            [c] => (c, None),
            [c, p] if pct(p).is_some() => (c, pct(p)),
            [p, c] if pct(p).is_some() => (c, pct(p)),
            _ => return None,
        };
        let color = if c == "currentcolor" { Some(CURRENT.with(|k| k.get())) } else { crate::style::parse_color(c) };
        Some((color.map(Value::from).or_else(|| value(c))?, p))
    };
    let ((ca, pa), (cb, pb)) = (stop(a)?, stop(b)?);
    let (mut pa, mut pb) = match (pa, pb) {
        (None, None) => (0.5, 0.5),
        (Some(a), None) => (a, 1.0 - a),
        (None, Some(b)) => (1.0 - b, b),
        (Some(a), Some(b)) => (a, b),
    };
    let sum = pa + pb;
    if sum == 0.0 {
        return None;
    }
    let mult = if sum < 1.0 { sum } else { 1.0 };
    pa /= sum;
    pb /= sum;
    let (x, y) = (ca.to(space), cb.to(space));
    let alpha = |v: f64| if v.is_nan() { None } else { Some(v) };
    let a_out = match (alpha(x.alpha), alpha(y.alpha)) {
        (Some(p), Some(q)) => p * pa + q * pb,
        (Some(p), None) | (None, Some(p)) => p,
        (None, None) => 0.0,
    };
    let (xa, ya) = (alpha(x.alpha).unwrap_or(a_out), alpha(y.alpha).unwrap_or(a_out));
    let h = space.hue();
    let mut out = [0.0; 3];
    for k in 0..3 {
        let (mut p, mut q) = (x.c[k], y.c[k]);
        if p.is_nan() && q.is_nan() {
            out[k] = if Some(k) == h { f64::NAN } else { 0.0 };
            continue;
        }
        if p.is_nan() {
            p = q;
        }
        if q.is_nan() {
            q = p;
        }
        if Some(k) == h {
            let d = q - p;
            match hue_method {
                "shorter" if d > 180.0 => p += 360.0,
                "shorter" if d < -180.0 => q += 360.0,
                "longer" if (0.0..180.0).contains(&d) => p += 360.0,
                "longer" if (-180.0..=0.0).contains(&d) && d != 0.0 => q += 360.0,
                "increasing" if d < 0.0 => q += 360.0,
                "decreasing" if d > 0.0 => p += 360.0,
                _ => {}
            }
            out[k] = (p * pa + q * pb).rem_euclid(360.0);
        } else {
            // Premultiplied by alpha, so a transparent colour doesn't tint.
            out[k] = if a_out == 0.0 { p * pa + q * pb } else { (p * xa * pa + q * ya * pb) / a_out };
        }
    }
    Some(Value { space, c: out, alpha: a_out * mult })
}

#[cfg(test)]
mod tests {
    use crate::style::parse_color;

    fn rgba(s: &str) -> Option<(u8, u8, u8, u8)> {
        parse_color(s).map(|c| (c.r, c.g, c.b, c.a))
    }

    #[test]
    fn legacy_and_modern_syntax() {
        assert_eq!(rgba("rgb(0, 255, 0)"), Some((0, 255, 0, 255)));
        assert_eq!(rgba("rgb(0 255 0 / 50%)"), Some((0, 255, 0, 128)));
        assert_eq!(rgba("rgb(0% 100% 0%)"), Some((0, 255, 0, 255)));
        assert_eq!(rgba("rgb(0, 255, 0"), Some((0, 255, 0, 255)));
        assert_eq!(rgba("rgb(none 255 0)"), Some((0, 255, 0, 255)));
        assert_eq!(rgba("rgba(0, 255, 0, 0.5)"), Some((0, 255, 0, 128)));
        // Mixing commas and spaces, numbers and percentages (legacy), trailing commas: invalid.
        for bad in ["rgb(255.0, 0, 0,)", "rgb(255, 0, 0 / 1)", "rgb(100%, 0, 0)", "rgba(255, 0, 0, 1,)", "rgb(255 0 0,)", "rgb(none, 0, 0)", "rgb(1., 0, 0)", "hsl(0, 0, 50%)", "hsl(0, 100%, 50%,)", "hwb(0, 0%, 0%)"] {
            assert_eq!(rgba(bad), None, "{bad}");
        }
        assert_eq!(rgba("hsl(120deg 100% 25%)"), Some((0, 128, 0, 255)));
        assert_eq!(rgba("hsl(120 100 25)"), Some((0, 128, 0, 255)));
        assert_eq!(rgba("hwb(120 0% 50%)"), Some((0, 128, 0, 255)));
    }

    #[test]
    fn lab_oklch_and_color() {
        assert_eq!(rgba("lab(100 0 0)"), Some((255, 255, 255, 255)));
        assert_eq!(rgba("oklch(0% 0 0)"), Some((0, 0, 0, 255)));
        // Tailwind's red-500.
        let (r, g, b, _) = rgba("oklch(63.7% 0.237 25.331)").unwrap();
        assert!(r > 240 && (40..=60).contains(&g) && (40..=70).contains(&b), "{r} {g} {b}");
        let (r, g, b, _) = rgba("lch(54.29% 106.84 40.85)").unwrap();
        assert!(r >= 250 && g < 10 && b < 10, "{r} {g} {b}");
        assert_eq!(rgba("color(srgb 0.5 0 0.5)"), Some((128, 0, 128, 255)));
        assert_eq!(rgba("color(display-p3 1 1 1)"), Some((255, 255, 255, 255)));
        let (r, g, b, _) = rgba("oklab(0.6279 0.2249 0.1258)").unwrap();
        assert!(r >= 250 && g < 10 && b < 10, "{r} {g} {b}");
    }

    #[test]
    fn color_mix() {
        assert_eq!(rgba("color-mix(in srgb, red, blue)"), Some((128, 0, 128, 255)));
        assert_eq!(rgba("color-mix(in srgb, red 25%, blue)"), Some((64, 0, 191, 255)));
        // How Tailwind writes `bg-red-500/50`.
        let (r, _, _, a) = rgba("color-mix(in oklab, rgb(255 0 0) 50%, transparent)").unwrap();
        assert_eq!((r, a), (255, 128));
        assert_eq!(rgba("color-mix(in srgb, red 30%, blue 30%)").map(|c| c.3), Some(153));
        assert_eq!(rgba("color-mix(in hsl, red, blue)"), Some((255, 0, 255, 255)));
    }
}
