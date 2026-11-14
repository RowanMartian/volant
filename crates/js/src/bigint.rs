//! Arbitrary-precision integers for JavaScript's BigInt: a sign and a
//! magnitude in base 2^32 (least significant limb first, no zero limbs on
//! top; zero is the empty magnitude and never negative).

use std::cmp::Ordering;

#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct BigInt {
    neg: bool,
    mag: Vec<u32>,
}

/// The largest BigInt Canard makes, in limbs (2^30 bits, as other engines allow).
const MAX_LIMBS: usize = 1 << 25;
pub const TOO_BIG: &str = "Maximum BigInt size exceeded";

fn trim(mut v: Vec<u32>) -> Vec<u32> {
    while v.last() == Some(&0) {
        v.pop();
    }
    v
}

fn mag_cmp(a: &[u32], b: &[u32]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

fn mag_add(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(a.len() + 1);
    let mut carry = 0u64;
    for i in 0..a.len() {
        let s = a[i] as u64 + *b.get(i).unwrap_or(&0) as u64 + carry;
        out.push(s as u32);
        carry = s >> 32;
    }
    if carry != 0 {
        out.push(carry as u32);
    }
    out
}

/// a - b, for a >= b.
fn mag_sub(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0i64;
    for i in 0..a.len() {
        let mut d = a[i] as i64 - *b.get(i).unwrap_or(&0) as i64 - borrow;
        borrow = 0;
        if d < 0 {
            d += 1 << 32;
            borrow = 1;
        }
        out.push(d as u32);
    }
    trim(out)
}

fn mag_mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate() {
        let mut carry = 0u64;
        for (j, &y) in b.iter().enumerate() {
            let t = x as u64 * y as u64 + out[i + j] as u64 + carry;
            out[i + j] = t as u32;
            carry = t >> 32;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let t = out[k] as u64 + carry;
            out[k] = t as u32;
            carry = t >> 32;
            k += 1;
        }
    }
    trim(out)
}

/// (a / d, a % d) for a one-limb divisor.
fn mag_divrem_small(a: &[u32], d: u32) -> (Vec<u32>, u32) {
    let mut q = vec![0u32; a.len()];
    let mut r = 0u64;
    for i in (0..a.len()).rev() {
        let cur = (r << 32) | a[i] as u64;
        q[i] = (cur / d as u64) as u32;
        r = cur % d as u64;
    }
    (trim(q), r as u32)
}

fn mag_shl(a: &[u32], bits: usize) -> Vec<u32> {
    if a.is_empty() {
        return Vec::new();
    }
    let (limbs, s) = (bits / 32, bits % 32);
    let mut out = vec![0u32; limbs];
    if s == 0 {
        out.extend_from_slice(a);
    } else {
        let mut carry = 0u32;
        for &x in a {
            out.push((x << s) | carry);
            carry = x >> (32 - s);
        }
        if carry != 0 {
            out.push(carry);
        }
    }
    out
}

fn mag_shr(a: &[u32], bits: usize) -> Vec<u32> {
    let (limbs, s) = (bits / 32, bits % 32);
    if limbs >= a.len() {
        return Vec::new();
    }
    let a = &a[limbs..];
    let mut out = Vec::with_capacity(a.len());
    if s == 0 {
        out.extend_from_slice(a);
    } else {
        for i in 0..a.len() {
            let hi = if i + 1 < a.len() { a[i + 1] << (32 - s) } else { 0 };
            out.push((a[i] >> s) | hi);
        }
    }
    trim(out)
}

/// Knuth's algorithm D: (u / v, u % v), v not zero.
fn mag_divrem(u: &[u32], v: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if mag_cmp(u, v) == Ordering::Less {
        return (Vec::new(), u.to_vec());
    }
    if v.len() == 1 {
        let (q, r) = mag_divrem_small(u, v[0]);
        return (q, if r == 0 { Vec::new() } else { vec![r] });
    }
    const B: u64 = 1 << 32;
    let s = v[v.len() - 1].leading_zeros() as usize;
    let v = mag_shl(v, s);
    let mut un = mag_shl(u, s);
    un.resize(u.len() + 1, 0);
    let n = v.len();
    let m = un.len() - n - 1;
    let mut q = vec![0u32; m + 1];
    for j in (0..=m).rev() {
        let num = ((un[j + n] as u64) << 32) | un[j + n - 1] as u64;
        let mut qhat = num / v[n - 1] as u64;
        let mut rhat = num % v[n - 1] as u64;
        while qhat >= B || qhat * v[n - 2] as u64 > ((rhat << 32) | un[j + n - 2] as u64) {
            qhat -= 1;
            rhat += v[n - 1] as u64;
            if rhat >= B {
                break;
            }
        }
        let mut borrow = 0i64;
        let mut carry = 0u64;
        for i in 0..n {
            let p = qhat * v[i] as u64 + carry;
            carry = p >> 32;
            let t = un[i + j] as i64 - borrow - (p & 0xffff_ffff) as i64;
            un[i + j] = t as u32;
            borrow = if t < 0 { 1 } else { 0 };
        }
        let t = un[j + n] as i64 - borrow - carry as i64;
        un[j + n] = t as u32;
        if t < 0 {
            qhat -= 1;
            let mut c = 0u64;
            for i in 0..n {
                let s = un[i + j] as u64 + v[i] as u64 + c;
                un[i + j] = s as u32;
                c = s >> 32;
            }
            un[j + n] = un[j + n].wrapping_add(c as u32);
        }
        q[j] = qhat as u32;
    }
    un.truncate(n);
    (trim(q), mag_shr(&trim(un), s))
}

/// The magnitude's low `bits` bits.
fn mag_low_bits(a: &[u32], bits: u64) -> Vec<u32> {
    let limbs = bits.div_ceil(32) as usize;
    if limbs >= a.len() {
        return a.to_vec();
    }
    let mut out = a[..limbs].to_vec();
    let s = (bits % 32) as u32;
    if s != 0 {
        *out.last_mut().unwrap() &= (1u32 << s) - 1;
    }
    trim(out)
}

impl BigInt {
    fn make(neg: bool, mag: Vec<u32>) -> BigInt {
        let mag = trim(mag);
        BigInt { neg: neg && !mag.is_empty(), mag }
    }
    fn checked(neg: bool, mag: Vec<u32>) -> Result<BigInt, &'static str> {
        if mag.len() > MAX_LIMBS {
            return Err(TOO_BIG);
        }
        Ok(BigInt::make(neg, mag))
    }

    pub fn zero() -> BigInt {
        BigInt::default()
    }
    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }
    pub fn is_negative(&self) -> bool {
        self.neg
    }

    pub fn from_u64(n: u64) -> BigInt {
        BigInt::make(false, vec![n as u32, (n >> 32) as u32])
    }
    pub fn from_i64(n: i64) -> BigInt {
        let mut b = BigInt::from_u64(n.unsigned_abs());
        b.neg = n < 0;
        b
    }

    /// The integer a Number holds exactly, or None when it isn't one.
    pub fn from_f64(f: f64) -> Option<BigInt> {
        if !f.is_finite() || f.fract() != 0.0 {
            return None;
        }
        let bits = f.abs().to_bits();
        let exp = ((bits >> 52) & 0x7ff) as i64;
        if exp == 0 {
            return Some(BigInt::zero());
        }
        let mant = (bits & ((1 << 52) - 1)) | (1 << 52);
        let shift = exp - 1075;
        let m = BigInt::from_u64(mant);
        let mag = if shift >= 0 { mag_shl(&m.mag, shift as usize) } else { mag_shr(&m.mag, (-shift) as usize) };
        Some(BigInt::make(f < 0.0, mag))
    }

    /// The nearest Number (ties to even), as Number(bigint) gives.
    pub fn to_f64(&self) -> f64 {
        let bits = self.bit_length();
        let v = if bits <= 64 {
            let lo = *self.mag.first().unwrap_or(&0) as u64 | (*self.mag.get(1).unwrap_or(&0) as u64) << 32;
            lo as f64
        } else if bits > 1100 {
            f64::INFINITY
        } else {
            // The top 64 bits, with a sticky bit for any lower ones, round once.
            let shift = bits - 64;
            let top = mag_shr(&self.mag, shift as usize);
            let mut t = *top.first().unwrap_or(&0) as u64 | (*top.get(1).unwrap_or(&0) as u64) << 32;
            if mag_low_bits(&self.mag, shift) != Vec::<u32>::new() {
                t |= 1;
            }
            (t as f64) * 2f64.powi(shift as i32)
        };
        if self.neg {
            -v
        } else {
            v
        }
    }

    pub fn bit_length(&self) -> u64 {
        match self.mag.last() {
            None => 0,
            Some(&top) => (self.mag.len() as u64 - 1) * 32 + (32 - top.leading_zeros() as u64),
        }
    }

    /// The low 64 bits, two's complement (for BigInt64Array and friends).
    pub fn to_u64_wrapping(&self) -> u64 {
        let lo = *self.mag.first().unwrap_or(&0) as u64 | (*self.mag.get(1).unwrap_or(&0) as u64) << 32;
        if self.neg {
            lo.wrapping_neg()
        } else {
            lo
        }
    }

    /// StringToBigInt: decimal with an optional sign, or 0x / 0o / 0b; blank is 0.
    pub fn parse(s: &str) -> Option<BigInt> {
        let s = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        if s.is_empty() {
            return Some(BigInt::zero());
        }
        let lower = s.get(..2).map(|p| p.to_ascii_lowercase());
        let radix = match lower.as_deref() {
            Some("0x") => 16,
            Some("0o") => 8,
            Some("0b") => 2,
            _ => 10,
        };
        if radix != 10 {
            return BigInt::parse_digits(&s[2..], radix);
        }
        let (neg, digits) = match s.as_bytes()[0] {
            b'-' => (true, &s[1..]),
            b'+' => (false, &s[1..]),
            _ => (false, s),
        };
        let mut b = BigInt::parse_digits(digits, 10)?;
        b.neg = neg && !b.is_zero();
        Some(b)
    }

    /// Digits in `radix` (no sign, no prefix; `_` separators are the caller's business).
    pub fn parse_digits(digits: &str, radix: u32) -> Option<BigInt> {
        if digits.is_empty() {
            return None;
        }
        let mut mag: Vec<u32> = Vec::new();
        // Several digits at a time: radix^k fits a limb.
        let mut k = 1u32;
        let mut chunk = radix;
        while (chunk as u64) * (radix as u64) <= u32::MAX as u64 {
            chunk *= radix;
            k += 1;
        }
        let bytes: Vec<char> = digits.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            let n = (bytes.len() - i).min(k as usize);
            let mut val = 0u32;
            for &c in &bytes[i..i + n] {
                val = val * radix + c.to_digit(radix)?;
            }
            let mul = if n as u32 == k { chunk } else { radix.pow(n as u32) };
            let mut carry = val as u64;
            for limb in mag.iter_mut() {
                let t = *limb as u64 * mul as u64 + carry;
                *limb = t as u32;
                carry = t >> 32;
            }
            if carry != 0 {
                mag.push(carry as u32);
            }
            i += n;
        }
        Some(BigInt::make(false, mag))
    }

    pub fn to_string_radix(&self, radix: u32) -> String {
        if self.is_zero() {
            return "0".into();
        }
        let mut k = 1u32;
        let mut chunk = radix;
        while (chunk as u64) * (radix as u64) <= u32::MAX as u64 {
            chunk *= radix;
            k += 1;
        }
        let mut parts: Vec<u32> = Vec::new();
        let mut cur = self.mag.clone();
        while !cur.is_empty() {
            let (q, r) = mag_divrem_small(&cur, chunk);
            parts.push(r);
            cur = q;
        }
        let mut out = String::new();
        if self.neg {
            out.push('-');
        }
        let digit = |mut v: u32, width: Option<u32>| {
            let mut ds = Vec::new();
            while v > 0 {
                ds.push(std::char::from_digit(v % radix, radix).unwrap());
                v /= radix;
            }
            if let Some(w) = width {
                while ds.len() < w as usize {
                    ds.push('0');
                }
            }
            ds.into_iter().rev().collect::<String>()
        };
        out.push_str(&digit(*parts.last().unwrap(), None));
        for &p in parts.iter().rev().skip(1) {
            out.push_str(&digit(p, Some(k)));
        }
        out
    }

    pub fn neg(&self) -> BigInt {
        BigInt::make(!self.neg, self.mag.clone())
    }

    pub fn add(&self, o: &BigInt) -> Result<BigInt, &'static str> {
        if self.neg == o.neg {
            return BigInt::checked(self.neg, mag_add(&self.mag, &o.mag));
        }
        match mag_cmp(&self.mag, &o.mag) {
            Ordering::Equal => Ok(BigInt::zero()),
            Ordering::Greater => Ok(BigInt::make(self.neg, mag_sub(&self.mag, &o.mag))),
            Ordering::Less => Ok(BigInt::make(o.neg, mag_sub(&o.mag, &self.mag))),
        }
    }
    pub fn sub(&self, o: &BigInt) -> Result<BigInt, &'static str> {
        self.add(&o.neg())
    }
    pub fn mul(&self, o: &BigInt) -> Result<BigInt, &'static str> {
        if self.mag.len() + o.mag.len() > MAX_LIMBS + 1 {
            return Err(TOO_BIG);
        }
        BigInt::checked(self.neg != o.neg, mag_mul(&self.mag, &o.mag))
    }
    /// Truncating division and its remainder (which takes the dividend's sign); None for ÷ 0.
    pub fn divrem(&self, o: &BigInt) -> Option<(BigInt, BigInt)> {
        if o.is_zero() {
            return None;
        }
        let (q, r) = mag_divrem(&self.mag, &o.mag);
        Some((BigInt::make(self.neg != o.neg, q), BigInt::make(self.neg, r)))
    }
    /// self ** e, e not negative.
    pub fn pow(&self, e: &BigInt) -> Result<BigInt, &'static str> {
        if e.is_zero() {
            return Ok(BigInt::from_u64(1));
        }
        if self.is_zero() || self.mag == [1] {
            let odd = e.mag[0] & 1 == 1;
            return Ok(BigInt::make(self.neg && odd, self.mag.clone()));
        }
        if e.mag.len() > 1 || (e.mag[0] as u64) * self.bit_length() > (MAX_LIMBS as u64) * 32 {
            return Err(TOO_BIG);
        }
        let mut n = e.mag[0];
        let mut base = self.clone();
        let mut acc = BigInt::from_u64(1);
        while n > 0 {
            if n & 1 == 1 {
                acc = acc.mul(&base)?;
            }
            n >>= 1;
            if n > 0 {
                base = base.mul(&base)?;
            }
        }
        Ok(acc)
    }

    /// self << n (n may be negative: a right shift, rounding down).
    pub fn shl(&self, n: &BigInt) -> Result<BigInt, &'static str> {
        if n.neg {
            return self.shr(&n.neg());
        }
        if self.is_zero() {
            return Ok(BigInt::zero());
        }
        if n.mag.len() > 1 || n.mag.first().copied().unwrap_or(0) as usize > MAX_LIMBS * 32 {
            return Err(TOO_BIG);
        }
        BigInt::checked(self.neg, mag_shl(&self.mag, n.mag.first().copied().unwrap_or(0) as usize))
    }
    /// self >> n, rounding toward -∞ (n may be negative: a left shift).
    pub fn shr(&self, n: &BigInt) -> Result<BigInt, &'static str> {
        if n.neg {
            return self.shl(&n.neg());
        }
        Ok(self.shr_by(n.bit_length(), n))
    }
    fn shr_by(&self, len: u64, n: &BigInt) -> BigInt {
        // A shift past every bit leaves 0 (or -1 for a negative number).
        let past = len > 40 || n.mag.first().copied().unwrap_or(0) as u64 >= self.bit_length();
        if !self.neg {
            if past {
                return BigInt::zero();
            }
            return BigInt::make(false, mag_shr(&self.mag, n.mag.first().copied().unwrap_or(0) as usize));
        }
        if past {
            return BigInt::from_i64(-1);
        }
        // floor(-m / 2^k) = -(((m - 1) >> k) + 1)
        let m1 = mag_sub(&self.mag, &[1]);
        let s = mag_shr(&m1, n.mag.first().copied().unwrap_or(0) as usize);
        BigInt::make(true, mag_add(&s, &[1]))
    }

    /// Two's complement limbs, `len` long.
    fn twos(&self, len: usize) -> Vec<u32> {
        let mut v = self.mag.clone();
        v.resize(len, 0);
        if self.neg {
            for x in v.iter_mut() {
                *x = !*x;
            }
            for x in v.iter_mut() {
                let (s, c) = x.overflowing_add(1);
                *x = s;
                if !c {
                    break;
                }
            }
        }
        v
    }
    fn from_twos(mut v: Vec<u32>) -> BigInt {
        let neg = v.last().is_some_and(|&t| t & 0x8000_0000 != 0);
        if neg {
            for x in v.iter_mut() {
                *x = !*x;
            }
            for x in v.iter_mut() {
                let (s, c) = x.overflowing_add(1);
                *x = s;
                if !c {
                    break;
                }
            }
        }
        BigInt::make(neg, v)
    }
    fn bitwise(&self, o: &BigInt, f: impl Fn(u32, u32) -> u32) -> BigInt {
        let len = self.mag.len().max(o.mag.len()) + 1;
        let (a, b) = (self.twos(len), o.twos(len));
        BigInt::from_twos(a.iter().zip(&b).map(|(&x, &y)| f(x, y)).collect())
    }
    pub fn and(&self, o: &BigInt) -> BigInt {
        self.bitwise(o, |x, y| x & y)
    }
    pub fn or(&self, o: &BigInt) -> BigInt {
        self.bitwise(o, |x, y| x | y)
    }
    pub fn xor(&self, o: &BigInt) -> BigInt {
        self.bitwise(o, |x, y| x ^ y)
    }
    /// ~x = -x - 1
    pub fn not(&self) -> BigInt {
        self.neg().sub(&BigInt::from_u64(1)).unwrap_or_default()
    }

    /// x mod 2^bits, as BigInt.asUintN gives.
    pub fn as_uint_n(&self, bits: u64) -> Result<BigInt, &'static str> {
        if bits == 0 {
            return Ok(BigInt::zero());
        }
        let low = mag_low_bits(&self.mag, bits);
        if !self.neg || low.is_empty() {
            return Ok(BigInt::make(false, low));
        }
        if bits > MAX_LIMBS as u64 * 32 {
            return Err(TOO_BIG);
        }
        let full = mag_shl(&[1], bits as usize);
        Ok(BigInt::make(false, mag_sub(&full, &low)))
    }
    /// x wrapped into a signed `bits`-bit integer, as BigInt.asIntN gives.
    pub fn as_int_n(&self, bits: u64) -> Result<BigInt, &'static str> {
        if bits == 0 {
            return Ok(BigInt::zero());
        }
        let u = self.as_uint_n(bits)?;
        if u.bit_length() == bits {
            let full = BigInt::make(false, mag_shl(&[1], bits as usize));
            return u.sub(&full);
        }
        Ok(u)
    }

    pub fn cmp_big(&self, o: &BigInt) -> Ordering {
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => mag_cmp(&self.mag, &o.mag),
            (true, true) => mag_cmp(&o.mag, &self.mag),
        }
    }
    /// How this compares with a Number (None for NaN).
    pub fn cmp_f64(&self, f: f64) -> Option<Ordering> {
        if f.is_nan() {
            return None;
        }
        if f == f64::INFINITY {
            return Some(Ordering::Less);
        }
        if f == f64::NEG_INFINITY {
            return Some(Ordering::Greater);
        }
        let fl = f.floor();
        let b = BigInt::from_f64(fl).unwrap_or_default();
        match self.cmp_big(&b) {
            Ordering::Equal if fl != f => Some(Ordering::Less),
            o => Some(o),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> BigInt {
        BigInt::parse(s).unwrap()
    }

    #[test]
    fn arithmetic() {
        let a = p("123456789012345678901234567890");
        let b = p("-987654321098765432109876543210");
        assert_eq!(a.add(&b).unwrap().to_string_radix(10), "-864197532086419753208641975320");
        assert_eq!(a.mul(&b).unwrap().to_string_radix(10), "-121932631137021795226185032733622923332237463801111263526900");
        let (q, r) = b.divrem(&a).unwrap();
        assert_eq!((q.to_string_radix(10), r.to_string_radix(10)), ("-8".into(), "-9000000000900000000090".into()));
        let big = p("340282366920938463463374607431768211457");
        let (q, r) = big.divrem(&p("18446744073709551629")).unwrap();
        assert_eq!(q.mul(&p("18446744073709551629")).unwrap().add(&r).unwrap(), big);
        assert_eq!(p("2").pow(&p("100")).unwrap().to_string_radix(16), "10000000000000000000000000");
        assert_eq!(p("-255").to_string_radix(16), "-ff");
        assert_eq!(p("0x1F").to_string_radix(10), "31");
        assert!(BigInt::parse("1.5").is_none() && BigInt::parse("-0x1").is_none());
    }

    #[test]
    fn bits_and_shifts() {
        assert_eq!(p("-5").and(&p("3")).to_string_radix(10), "3");
        assert_eq!(p("-5").or(&p("3")).to_string_radix(10), "-5");
        assert_eq!(p("-5").xor(&p("3")).to_string_radix(10), "-8");
        assert_eq!(p("5").not().to_string_radix(10), "-6");
        assert_eq!(p("-5").shr(&p("1")).unwrap().to_string_radix(10), "-3");
        assert_eq!(p("-1").shr(&p("100")).unwrap().to_string_radix(10), "-1");
        assert_eq!(p("1").shl(&p("64")).unwrap().to_string_radix(10), "18446744073709551616");
        assert_eq!(p("255").as_int_n(8).unwrap().to_string_radix(10), "-1");
        assert_eq!(p("-1").as_uint_n(64).unwrap().to_string_radix(10), "18446744073709551615");
        assert_eq!(p("-1").to_u64_wrapping(), u64::MAX);
    }

    #[test]
    fn numbers() {
        assert_eq!(BigInt::from_f64(1e21).unwrap().to_string_radix(10), "1000000000000000000000");
        assert_eq!(p("9007199254740993").to_f64(), 9007199254740992.0);
        assert_eq!(p("123456789012345678901234567890").to_f64(), 1.2345678901234568e29);
        assert_eq!(p("10").cmp_f64(10.5), Some(Ordering::Less));
        assert_eq!(p("-10").cmp_f64(-10.5), Some(Ordering::Greater));
        assert!(BigInt::from_f64(0.5).is_none());
    }
}
