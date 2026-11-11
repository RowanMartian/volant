//! Ranking suggestions as you type. Every word you type has to appear in the
//! page's address or title; matches at the start of the address or at the start
//! of a word count for more.

/// Lowercases ASCII only. Cheap, allocation-light, and good enough for matching.
pub fn fold(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Scores how well `query` (already folded and split into words) matches a page.
/// `addr` is the display form of the URL (`example.com/path`), folded.
/// Returns `None` when some word doesn't match at all.
pub fn score(words: &[&str], addr: &str, title: &str) -> Option<u32> {
    if words.is_empty() {
        return Some(0);
    }
    let mut total = 0u32;
    for (i, w) in words.iter().enumerate() {
        let mut best = 0u32;
        if let Some(p) = addr.find(w) {
            best = if p == 0 { if i == 0 { 120 } else { 60 } } else if is_boundary(addr, p) { 45 } else { 15 };
        }
        if let Some(p) = title.find(w) {
            let t = if p == 0 { 50 } else if is_boundary(title, p) { 40 } else { 10 };
            best = best.max(t);
        }
        if best == 0 {
            return None;
        }
        total += best + w.len() as u32;
    }
    Some(total)
}

fn is_boundary(s: &str, p: usize) -> bool {
    s.as_bytes().get(p.wrapping_sub(1)).map(|&b| !b.is_ascii_alphanumeric()).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranks() {
        let w = ["git"];
        let a = score(&w, "github.com", "GitHub").unwrap();
        let b = score(&w, "example.com/legit", "Example").unwrap();
        assert!(a > b);
        assert!(score(&["rust", "book"], "doc.rust-lang.org/book", "the rust programming language").is_some());
        assert!(score(&["zzz"], "example.com", "example").is_none());
    }
}
