//! Throws random and truncated HTML at the parser and Reader; any panic fails.
//! cargo run --release -p volant-core --example fuzz [pages...]
fn main() {
    let mut seed = 0x9e3779b97f4a7c15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    const BITS: &[&str] = &[
        "<", ">", "</", "/>", "<p>", "</p>", "<div class=\"content\">", "</div>", "<a href=\"", "\">", "<img src=x", "&amp;", "&", "&#", "&#x", ";", "<!--",
        "-->", "<script>", "</script>", "<title>", "é", "日本", "😀", " ", "text, more text. ", "=", "'", "\"", "<li>", "<table><tr><td>", "<!doctype html>",
        "<h1>", "</h1>", "<article>", "<pre>", "<br>", "\n", "<style>", "x{}", "<svg>", "</svg>", "<noscript>", "&nbsp;", "<b><i></b></i>",
    ];
    let mut n = 0;
    for _ in 0..30_000 {
        let len = (rnd() % 120) as usize;
        let s: String = (0..len).map(|_| BITS[(rnd() % BITS.len() as u64) as usize]).collect();
        let _ = volant_core::html::parse(&s);
        let _ = volant_core::reader::extract(&s, "https://x.example/a/b?c#d");
        n += 1;
    }
    for path in std::env::args().skip(1) {
        let page = std::fs::read_to_string(&path).unwrap();
        for _ in 0..200 {
            let mut cut = (rnd() as usize) % (page.len() + 1);
            while !page.is_char_boundary(cut) {
                cut -= 1;
            }
            let start = (rnd() as usize) % (cut + 1);
            let mut start = start;
            while !page.is_char_boundary(start) {
                start -= 1;
            }
            let _ = volant_core::reader::extract(&page[start..cut], "https://x.example/");
            n += 1;
        }
    }
    const FILTER: &[&str] = &["||", "^", "$", "*", "|", "@@", "##", "#@#", "#?#", ".ad", "domain=", "~", "third-party", ",", "script", "/", "a.com", "é", "0.0.0.0 ", "!", "[", "=", ":has-text(", "image", "~image", "|", "\\", "?"];
    for _ in 0..30_000 {
        let lines: Vec<String> = (0..(rnd() % 6)).map(|_| (0..(rnd() % 10)).map(|_| FILTER[(rnd() % FILTER.len() as u64) as usize]).collect()).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let c = volant_core::ublock::compile(&[("fuzz", &refs.join("\n"))], volant_core::ublock::Options::default());
        assert!(c.network.iter().all(|n| n.starts_with('[') && n.ends_with(']')));
        let _ = c.sites.lookup("www.example.com");
        n += 1;
    }
    const TYPED: &[&str] = &["a", ".", "com", "/", ":", "8080", "http", "://", " ", "?", "#", "@", "[", "]", "é", "localhost", "1", "-", "www."];
    for _ in 0..50_000 {
        let s: String = (0..(rnd() % 8)).map(|_| TYPED[(rnd() % TYPED.len() as u64) as usize]).collect();
        let u = volant_core::omnibox::resolve(&s, "duckduckgo");
        let _ = (volant_core::omnibox::host(&u), volant_core::omnibox::display(&u), volant_core::search::query_of(&u));
        n += 1;
    }
    println!("{n} inputs, no panics");
}
