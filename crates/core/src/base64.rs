//! Base64 (RFC 4648), for `data:` URLs and `toDataURL`.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decodes, ignoring whitespace; also takes the URL-safe alphabet. None if it isn't base64.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// The bytes and media type in a `data:` URL.
pub fn data_url(url: &str) -> Option<(String, Vec<u8>)> {
    let rest = url.strip_prefix("data:").or_else(|| url.get(..5).filter(|p| p.eq_ignore_ascii_case("data:")).map(|_| &url[5..]))?;
    let (meta, body) = rest.split_once(',')?;
    let b64 = meta.to_ascii_lowercase().ends_with(";base64");
    let mime = meta.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let bytes = if b64 { decode(body)? } else { crate::url::percent_decode(body.as_bytes()) };
    Some((if mime.is_empty() { "text/plain".into() } else { mime }, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            assert_eq!(decode(&encode(s.as_bytes())).unwrap(), s.as_bytes());
        }
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(data_url("data:image/png;base64,Zm8=").unwrap(), ("image/png".into(), b"fo".to_vec()));
        assert_eq!(data_url("data:,a%20b").unwrap().1, b"a b".to_vec());
    }
}
