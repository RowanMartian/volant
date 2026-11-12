//! Compiles filter lists given on the command line and reports how it went.
//! cargo run --release -p volant-core --example compile_lists -- easylist.txt easyprivacy.txt
fn main() {
    let texts: Vec<String> = std::env::args().skip(1).map(|p| std::fs::read_to_string(p).expect("read list")).collect();
    let names: Vec<String> = std::env::args().skip(1).collect();
    let lists: Vec<(&str, &str)> = names.iter().zip(&texts).map(|(n, t)| (n.as_str(), t.as_str())).collect();
    let t = std::time::Instant::now();
    let c = volant_core::ublock::compile(&lists, volant_core::ublock::Options::default());
    let kb: usize = c.network.iter().map(|n| n.len()).sum::<usize>() / 1024;
    eprintln!("{:?} in {:?}", c.stats, t.elapsed());
    eprintln!("{} network lists ({} KB), cosmetic {} KB, {} sites with scriptlets/procedural filters", c.network.len(), kb, c.cosmetic.len() / 1024, c.sites.sites());
    // LOOKUP=example.com prints what Volant would inject into a page on that host.
    if let Ok(host) = std::env::var("LOOKUP") {
        println!("{}", volant_core::ublock::bundle_json(&c.sites.lookup(&host)));
    }
    if let Ok(out) = std::env::var("OUT") {
        for (i, n) in c.network.iter().enumerate() {
            std::fs::write(format!("{out}/network-{i}.json"), n).unwrap();
        }
        std::fs::write(format!("{out}/cosmetic.json"), &c.cosmetic).unwrap();
        std::fs::write(format!("{out}/sites.txt"), c.sites.serialize()).unwrap();
    }
}
