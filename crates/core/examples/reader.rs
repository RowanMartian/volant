//! Runs Reader mode on saved pages: cargo run --release -p volant-core --example reader -- page.html URL [page2.html URL2 ...]
use std::time::Instant;
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    for pair in args.chunks(2) {
        let html = std::fs::read_to_string(&pair[0]).expect("read page");
        let t = Instant::now();
        let dom = volant_core::html::parse(&html);
        let parse = t.elapsed();
        let nodes = dom.nodes.len();
        drop(dom);
        let t = Instant::now();
        let a = volant_core::reader::extract(&html, &pair[1]);
        let total = t.elapsed();
        match a {
            Some(a) => {
                let text: String = volant_core::html::parse(&a.html).nodes.iter().filter_map(|n| match &n.data { volant_core::html::Data::Text(t) => Some(t.as_str()), _ => None }).collect::<Vec<_>>().join(" ");
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                println!("{}\n  {} KB, {} nodes: parse {:.2} ms, extract total {:.2} ms\n  title: {:?}\n  byline: {:?} site: {:?}\n  {} words ({} min), {} images\n  starts: {:.160}…\n  ends:   …{}\n",
                    pair[1], html.len() / 1024, nodes, parse.as_secs_f64() * 1e3, total.as_secs_f64() * 1e3, a.title, a.byline, a.site, a.words, a.minutes(),
                    a.html.matches("<img").count(), text, &text[text.char_indices().rev().nth(120).map(|(i, _)| i).unwrap_or(0)..]);
                if let Ok(dir) = std::env::var("OUT") { std::fs::write(format!("{dir}/{}.html", pair[0].rsplit('/').next().unwrap()), &a.html).unwrap(); }
            }
            None => println!("{}\n  {} KB, {} nodes: parse {:.2} ms — not an article ({:.2} ms)\n", pair[1], html.len() / 1024, nodes, parse.as_secs_f64() * 1e3, total.as_secs_f64() * 1e3),
        }
    }
}
