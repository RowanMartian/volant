//! The WHATWG Encoding Standard's labels, and decoders for UTF-8, UTF-16
//! (both orders), every single-byte encoding (windows-125x, ISO-8859-x,
//! KOI8, IBM866, the Mac ones; `latin1` and `iso-8859-1` mean windows-1252)
//! and x-user-defined.

/// The encoding a label names (ASCII whitespace trimmed, any case), by its canonical name.
pub fn for_label(label: &str) -> Option<&'static str> {
    let l = label.trim_matches(|c: char| matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' ')).to_ascii_lowercase();
    LABELS.iter().find(|(_, labels)| labels.split(' ').any(|x| x == l)).map(|(name, _)| *name)
}

#[rustfmt::skip]
const LABELS: &[(&str, &str)] = &[
    ("UTF-8", "unicode-1-1-utf-8 unicode11utf8 unicode20utf8 utf-8 utf8 x-unicode20utf8"),
    ("IBM866", "866 cp866 csibm866 ibm866"),
    ("ISO-8859-2", "csisolatin2 iso-8859-2 iso-ir-101 iso8859-2 iso88592 iso_8859-2 iso_8859-2:1987 l2 latin2"),
    ("ISO-8859-3", "csisolatin3 iso-8859-3 iso-ir-109 iso8859-3 iso88593 iso_8859-3 iso_8859-3:1988 l3 latin3"),
    ("ISO-8859-4", "csisolatin4 iso-8859-4 iso-ir-110 iso8859-4 iso88594 iso_8859-4 iso_8859-4:1988 l4 latin4"),
    ("ISO-8859-5", "csisolatincyrillic cyrillic iso-8859-5 iso-ir-144 iso8859-5 iso88595 iso_8859-5 iso_8859-5:1988"),
    ("ISO-8859-6", "arabic asmo-708 csiso88596e csiso88596i csisolatinarabic ecma-114 iso-8859-6 iso-8859-6-e iso-8859-6-i iso-ir-127 iso8859-6 iso88596 iso_8859-6 iso_8859-6:1987"),
    ("ISO-8859-7", "csisolatingreek ecma-118 elot_928 greek greek8 iso-8859-7 iso-ir-126 iso8859-7 iso88597 iso_8859-7 iso_8859-7:1987 sun_eu_greek"),
    ("ISO-8859-8", "csiso88598e csisolatinhebrew hebrew iso-8859-8 iso-8859-8-e iso-ir-138 iso8859-8 iso88598 iso_8859-8 iso_8859-8:1988 visual"),
    ("ISO-8859-8-I", "csiso88598i iso-8859-8-i logical"),
    ("ISO-8859-10", "csisolatin6 iso-8859-10 iso-ir-157 iso8859-10 iso885910 l6 latin6"),
    ("ISO-8859-13", "iso-8859-13 iso8859-13 iso885913"),
    ("ISO-8859-14", "iso-8859-14 iso8859-14 iso885914"),
    ("ISO-8859-15", "csisolatin9 iso-8859-15 iso8859-15 iso885915 iso_8859-15 l9"),
    ("ISO-8859-16", "iso-8859-16"),
    ("KOI8-R", "cskoi8r koi koi8 koi8-r koi8_r"),
    ("KOI8-U", "koi8-ru koi8-u"),
    ("macintosh", "csmacintosh mac macintosh x-mac-roman"),
    ("windows-874", "dos-874 iso-8859-11 iso8859-11 iso885911 tis-620 windows-874"),
    ("windows-1250", "cp1250 windows-1250 x-cp1250"),
    ("windows-1251", "cp1251 windows-1251 x-cp1251"),
    ("windows-1252", "ansi_x3.4-1968 ascii cp1252 cp819 csisolatin1 ibm819 iso-8859-1 iso-ir-100 iso8859-1 iso88591 iso_8859-1 iso_8859-1:1987 l1 latin1 us-ascii windows-1252 x-cp1252"),
    ("windows-1253", "cp1253 windows-1253 x-cp1253"),
    ("windows-1254", "cp1254 csisolatin5 iso-8859-9 iso-ir-148 iso8859-9 iso88599 iso_8859-9 iso_8859-9:1989 l5 latin5 windows-1254 x-cp1254"),
    ("windows-1255", "cp1255 windows-1255 x-cp1255"),
    ("windows-1256", "cp1256 windows-1256 x-cp1256"),
    ("windows-1257", "cp1257 windows-1257 x-cp1257"),
    ("windows-1258", "cp1258 windows-1258 x-cp1258"),
    ("x-mac-cyrillic", "x-mac-cyrillic x-mac-ukrainian"),
    ("GBK", "chinese csgb2312 csiso58gb231280 gb2312 gb_2312 gb_2312-80 gbk iso-ir-58 x-gbk"),
    ("gb18030", "gb18030"),
    ("Big5", "big5 big5-hkscs cn-big5 csbig5 x-x-big5"),
    ("EUC-JP", "cseucpkdfmtjapanese euc-jp x-euc-jp"),
    ("ISO-2022-JP", "csiso2022jp iso-2022-jp"),
    ("Shift_JIS", "csshiftjis ms932 ms_kanji shift-jis shift_jis sjis windows-31j x-sjis"),
    ("EUC-KR", "cseuckr csksc56011987 euc-kr iso-ir-149 korean ks_c_5601-1987 ks_c_5601-1989 ksc5601 ksc_5601 windows-949"),
    ("replacement", "csiso2022kr hz-gb-2312 iso-2022-cn iso-2022-cn-ext iso-2022-kr replacement"),
    ("UTF-16BE", "unicodefffe utf-16be"),
    ("UTF-16LE", "csunicode iso-10646-ucs-2 ucs-2 unicode unicodefeff utf-16 utf-16le"),
    ("x-user-defined", "x-user-defined"),
];

/// The encoding an HTML page is in, as HTML's "encoding sniffing" finds it:
/// a byte order mark, then the Content-Type header's charset, then a
/// `<meta charset>` or `<meta http-equiv=content-type>` in the first 1024
/// bytes, else UTF-8.
pub fn sniff_html(bytes: &[u8], content_type: &str) -> &'static str {
    if let Some(e) = bom(bytes) {
        return e.0;
    }
    let from_header = content_type
        .split(';')
        .skip(1)
        .find_map(|p| p.trim().strip_prefix("charset=").or_else(|| p.trim().strip_prefix("CHARSET=")))
        .and_then(|v| for_label(v.trim_matches(|c| c == '"' || c == '\'')));
    if let Some(e) = from_header {
        return e;
    }
    match prescan(&bytes[..bytes.len().min(1024)]) {
        // A page can't really be UTF-16 if its ASCII markup said so.
        Some("UTF-16LE" | "UTF-16BE") => "UTF-8",
        Some("x-user-defined") => "windows-1252",
        Some(e) => e,
        None => "UTF-8",
    }
}

/// A byte order mark's encoding and length.
pub fn bom(bytes: &[u8]) -> Option<(&'static str, usize)> {
    match bytes {
        [0xEF, 0xBB, 0xBF, ..] => Some(("UTF-8", 3)),
        [0xFE, 0xFF, ..] => Some(("UTF-16BE", 2)),
        [0xFF, 0xFE, ..] => Some(("UTF-16LE", 2)),
        _ => None,
    }
}

/// Decodes a page in the encoding [`sniff_html`] finds, dropping a byte order mark.
pub fn decode_html(bytes: &[u8], content_type: &str) -> (String, &'static str) {
    let enc = sniff_html(bytes, content_type);
    let skip = bom(bytes).filter(|b| b.0 == enc).map_or(0, |b| b.1);
    let text = decode(enc, &bytes[skip..], false, false).map(|t| t.0).unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned());
    (text, enc)
}

/// HTML's prescan for a `<meta>` naming the encoding.
fn prescan(b: &[u8]) -> Option<&'static str> {
    let lower = |c: u8| c.to_ascii_lowercase();
    let ws = |c: u8| matches!(c, b'\t' | b'\n' | 0x0c | b'\r' | b' ');
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"<!--") {
            i = b[i + 4..].windows(3).position(|w| w == b"-->").map_or(b.len(), |k| i + 4 + k + 3);
            continue;
        }
        if b[i] == b'<' && b.len() > i + 5 && b[i + 1..i + 5].iter().map(|&c| lower(c)).eq(*b"meta") && (ws(b[i + 5]) || b[i + 5] == b'/') {
            i += 5;
            let (mut http_equiv, mut content, mut charset) = (false, None::<String>, None::<String>);
            // Attributes, up to the tag's end.
            loop {
                while i < b.len() && (ws(b[i]) || b[i] == b'/') {
                    i += 1;
                }
                if i >= b.len() || b[i] == b'>' {
                    break;
                }
                let start = i;
                while i < b.len() && !ws(b[i]) && !matches!(b[i], b'=' | b'>' | b'/') {
                    i += 1;
                }
                let name: String = b[start..i].iter().map(|&c| lower(c) as char).collect();
                while i < b.len() && ws(b[i]) {
                    i += 1;
                }
                let mut value = String::new();
                if i < b.len() && b[i] == b'=' {
                    i += 1;
                    while i < b.len() && ws(b[i]) {
                        i += 1;
                    }
                    if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                        let q = b[i];
                        i += 1;
                        let s = i;
                        while i < b.len() && b[i] != q {
                            i += 1;
                        }
                        value = b[s..i].iter().map(|&c| lower(c) as char).collect();
                        i += 1;
                    } else {
                        let s = i;
                        while i < b.len() && !ws(b[i]) && b[i] != b'>' {
                            i += 1;
                        }
                        value = b[s..i].iter().map(|&c| lower(c) as char).collect();
                    }
                }
                match name.as_str() {
                    "http-equiv" => http_equiv |= value == "content-type",
                    "content" if content.is_none() => content = Some(value),
                    "charset" if charset.is_none() => charset = Some(value),
                    _ => {}
                }
            }
            let label = charset.or_else(|| {
                let c = content.filter(|_| http_equiv)?;
                let rest = &c[c.find("charset")? + 7..];
                let rest = rest.trim_start().strip_prefix('=')?.trim_start();
                let rest = rest.trim_start_matches(['"', '\'']);
                Some(rest.split(|ch: char| ch == ';' || ch == '"' || ch == '\'' || ch.is_ascii_whitespace()).next().unwrap_or("").to_string())
            });
            if let Some(e) = label.and_then(|l| for_label(&l)) {
                return Some(e);
            }
            continue;
        }
        i += 1;
    }
    None
}

/// A single-byte encoding's table for bytes 0x80-0xFF (ISO-8859-8-I is ISO-8859-8's).
fn single_byte(encoding: &str) -> Option<&'static [u16; 128]> {
    let name = if encoding == "ISO-8859-8-I" { "ISO-8859-8" } else { encoding };
    crate::single_byte::SINGLE_BYTE.iter().find(|(n, _)| *n == name).map(|(_, t)| t)
}

/// Decodes `bytes` as `encoding` (a canonical name). With `stream`, an
/// incomplete sequence at the end is left for the next call: returns the text
/// and how many bytes it used. `fatal` makes malformed input an error.
pub fn decode(encoding: &str, bytes: &[u8], fatal: bool, stream: bool) -> Result<(String, usize), ()> {
    match encoding {
        "UTF-8" => utf8(bytes, fatal, stream),
        "UTF-16LE" | "UTF-16BE" => utf16(bytes, encoding == "UTF-16BE", fatal, stream),
        "replacement" => {
            if bytes.is_empty() {
                Ok((String::new(), 0))
            } else if fatal {
                Err(())
            } else {
                Ok(("\u{FFFD}".into(), bytes.len()))
            }
        }
        "x-user-defined" => Ok((bytes.iter().map(|&b| if b < 0x80 { b as char } else { char::from_u32(0xF780 + b as u32 - 0x80).unwrap_or('\u{FFFD}') }).collect(), bytes.len())),
        _ => match single_byte(encoding) {
            Some(table) => {
                let mut out = String::with_capacity(bytes.len());
                for &b in bytes {
                    if b < 0x80 {
                        out.push(b as char);
                        continue;
                    }
                    match table[b as usize - 0x80] {
                        0 if fatal => return Err(()),
                        0 => out.push('\u{FFFD}'),
                        c => out.push(char::from_u32(c as u32).unwrap_or('\u{FFFD}')),
                    }
                }
                Ok((out, bytes.len()))
            }
            // The CJK encodings have no decoders yet: their ASCII comes through.
            None => Ok((bytes.iter().map(|&b| if b < 0x80 { b as char } else { '\u{FFFD}' }).collect(), bytes.len())),
        },
    }
}

fn utf8(bytes: &[u8], fatal: bool, stream: bool) -> Result<(String, usize), ()> {
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                return Ok((out, bytes.len()));
            }
            Err(e) => {
                let good = e.valid_up_to();
                // Safe: the standard library checked this prefix.
                out.push_str(std::str::from_utf8(&rest[..good]).unwrap_or(""));
                match e.error_len() {
                    // Cut off at the end: wait for more when streaming.
                    None if stream => return Ok((out, bytes.len() - (rest.len() - good))),
                    None => {
                        if fatal {
                            return Err(());
                        }
                        out.push('\u{FFFD}');
                        return Ok((out, bytes.len()));
                    }
                    // The "maximal subpart" becomes one U+FFFD, as the standard asks.
                    Some(n) => {
                        if fatal {
                            return Err(());
                        }
                        out.push('\u{FFFD}');
                        rest = &rest[good + n..];
                    }
                }
            }
        }
    }
}

fn utf16(bytes: &[u8], big: bool, fatal: bool, stream: bool) -> Result<(String, usize), ()> {
    let unit = |k: usize| if big { u16::from_be_bytes([bytes[k], bytes[k + 1]]) } else { u16::from_le_bytes([bytes[k], bytes[k + 1]]) };
    let mut out = String::with_capacity(bytes.len() / 2);
    let mut k = 0;
    while k + 1 < bytes.len() {
        let u = unit(k);
        if (0xD800..0xDC00).contains(&u) {
            if k + 3 < bytes.len() {
                let lo = unit(k + 2);
                if (0xDC00..0xE000).contains(&lo) {
                    out.push(char::from_u32(0x10000 + (((u as u32) - 0xD800) << 10) + (lo as u32 - 0xDC00)).unwrap_or('\u{FFFD}'));
                    k += 4;
                    continue;
                }
            } else if stream {
                return Ok((out, k));
            }
            if fatal {
                return Err(());
            }
            out.push('\u{FFFD}');
            k += 2;
            continue;
        }
        if (0xDC00..0xE000).contains(&u) {
            if fatal {
                return Err(());
            }
            out.push('\u{FFFD}');
        } else {
            out.push(char::from_u32(u as u32).unwrap_or('\u{FFFD}'));
        }
        k += 2;
    }
    if k < bytes.len() {
        if stream {
            return Ok((out, k));
        }
        if fatal {
            return Err(());
        }
        out.push('\u{FFFD}');
    }
    Ok((out, bytes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_decoders() {
        assert_eq!(for_label(" Latin1\n"), Some("windows-1252"));
        assert_eq!(for_label("utf8"), Some("UTF-8"));
        assert_eq!(for_label("utf-7"), None);
        assert_eq!(decode("UTF-8", b"a\xF0\x9F\x98\x80b\xC3", false, false).unwrap().0, "a😀b\u{FFFD}");
        assert_eq!(decode("UTF-8", b"a\xF0\x9F", false, true).unwrap(), ("a".to_string(), 1));
        assert!(decode("UTF-8", b"\xFF", true, false).is_err());
        assert_eq!(decode("UTF-8", b"\xF0\x80\x80", false, false).unwrap().0, "\u{FFFD}\u{FFFD}\u{FFFD}");
        assert_eq!(decode("UTF-16LE", &[0x3D, 0xD8, 0x00, 0xDE, 0x41, 0x00], false, false).unwrap().0, "😀A");
        assert_eq!(decode("UTF-16BE", &[0x00, 0x41, 0x00], false, false).unwrap().0, "A\u{FFFD}");
        assert_eq!(decode("windows-1252", &[0x80, 0xE9], false, false).unwrap().0, "€é");
        assert_eq!(decode("KOI8-U", &[0xAE, 0xC1], false, false).unwrap().0, "ўа");
        assert_eq!(decode("ISO-8859-8-I", &[0xE0], false, false).unwrap().0, "א");
        assert!(decode("windows-1253", &[0xAA], true, false).is_err());
        assert_eq!(sniff_html(b"<meta charset=koi8-r><p>", ""), "KOI8-R");
        assert_eq!(sniff_html(b"<!-- <meta charset=x --><META HTTP-EQUIV='Content-Type' CONTENT='text/html; charset=\"windows-1251\"'>", ""), "windows-1251");
        assert_eq!(sniff_html(b"<meta charset=latin1>", "text/html; charset=utf-8"), "UTF-8");
        assert_eq!(sniff_html(b"\xEF\xBB\xBF<meta charset=latin1>", ""), "UTF-8");
        assert_eq!(sniff_html(b"<meta charset=utf-16>", ""), "UTF-8");
        assert_eq!(decode_html(b"<meta charset=iso-8859-7>\xE1", "").0, "<meta charset=iso-8859-7>α");
    }
}
