//! The WHATWG Encoding Standard's labels, and decoders for the encodings the
//! web uses most: UTF-8, UTF-16 (both orders), windows-1252 (which `latin1`
//! and `iso-8859-1` also mean) and x-user-defined.

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

/// windows-1252's 0x80–0x9F (the rest is Latin-1).
const W1252: [u16; 32] = [
    0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C,
    0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
];

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
        // windows-1252, and (until they have tables of their own) the other single-byte encodings.
        _ => Ok((bytes.iter().map(|&b| if (0x80..0xA0).contains(&b) { char::from_u32(W1252[b as usize - 0x80] as u32).unwrap_or('\u{FFFD}') } else { b as char }).collect(), bytes.len())),
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
    }
}
