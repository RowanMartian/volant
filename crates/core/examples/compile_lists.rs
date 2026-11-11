//! Compiles filter lists given on the command line and reports how it went.
//! cargo run --release -p volant-core --example compile_lists -- easylist.txt easyprivacy.txt
fn main() {
    let texts: Vec<String> = std::env::args().skip(1).map(|p| std::fs::read_to_string(p).expect("read list")).collect();
    let mut all: Vec<&str> = vec![volant_core::blocker::BUILTIN];
    all.extend(texts.iter().map(String::as_str));
    let t = std::time::Instant::now();
    let c = volant_core::blocker::compile(&all);
    eprintln!("{} rules, {} skipped, {} KB JSON, {:?}", c.rules, c.skipped, c.json.len() / 1024, t.elapsed());
    if let Ok(out) = std::env::var("OUT") {
        std::fs::write(out, &c.json).unwrap();
    }
}
