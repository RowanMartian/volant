//! Micro-benchmarks for the hot paths: what runs on every keystroke in the
//! command bar, on every new tab, and when filter lists are compiled.
//! cargo run --release -p volant-core --example bench [easylist.txt ...]
use std::time::Instant;
use volant_core::history::History;

fn time<R>(label: &str, runs: u32, mut f: impl FnMut() -> R) {
    let _ = f();
    let t = Instant::now();
    for _ in 0..runs {
        std::hint::black_box(f());
    }
    let per = t.elapsed() / runs;
    println!("{label:<44} {:>9.3} ms", per.as_secs_f64() * 1000.0);
}

fn main() {
    let now = 1_750_000_000u64;
    let words = ["rust", "news", "docs", "github", "apple", "search", "video", "mail", "maps", "blog", "shop", "wiki"];
    let mut h = History::ephemeral();
    let mut seed = 42u64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let t = Instant::now();
    for i in 0..100_000u64 {
        let w = words[(rnd() % 12) as usize];
        let host = format!("{}{}.{}", w, rnd() % 3000, ["com", "org", "io"][(rnd() % 3) as usize]);
        let url = format!("https://www.{host}/{}/page-{i}?ref={}", words[(rnd() % 12) as usize], rnd() % 100);
        let title = format!("{} {} — article {i} about {}", w.to_uppercase(), words[(rnd() % 12) as usize], words[(rnd() % 12) as usize]);
        h.visit(&url, &title, now - rnd() % (86_400 * 365));
    }
    println!("history: 100,000 pages built in {:?}", t.elapsed());
    // Typing in the command bar: the index is warmed when the bar opens, then
    // every keystroke runs a query plus an inline completion.
    for phrase in ["github docs", "news", "zzzz"] {
        let mut worst = std::time::Duration::ZERO;
        let mut total = std::time::Duration::ZERO;
        let runs = 20;
        let mut warm_t = std::time::Duration::ZERO;
        for _ in 0..runs {
            h.cool();
            let t = Instant::now();
            h.warm(now);
            warm_t += t.elapsed();
            for n in 1..=phrase.len() {
                let typed = &phrase[..n];
                let t = Instant::now();
                std::hint::black_box(h.query(typed, 7, now));
                std::hint::black_box(h.complete(typed, now));
                let e = t.elapsed();
                worst = worst.max(e);
                total += e;
            }
        }
        println!(
            "typing {phrase:<14} per key avg {:>7.3} ms, worst {:>7.3} ms (index build {:.1} ms)",
            total.as_secs_f64() * 1000.0 / (runs * phrase.len()) as f64,
            worst.as_secs_f64() * 1000.0,
            warm_t.as_secs_f64() * 1000.0 / runs as f64
        );
    }
    if std::env::var("PERKEY").is_ok() {
        h.cool();
        h.warm(now);
        let phrase = "github docs";
        for n in 1..=phrase.len() {
            let t = Instant::now();
            std::hint::black_box(h.query(&phrase[..n], 7, now));
            let q = t.elapsed();
            let t = Instant::now();
            std::hint::black_box(h.complete(&phrase[..n], now));
            println!("  {:<12} query {:>6.3} ms  complete {:>6.3} ms", &phrase[..n], q.as_secs_f64() * 1e3, t.elapsed().as_secs_f64() * 1e3);
        }
    }
    time("top sites, cached (new tab page)", 50, || h.top_sites(8, now).len());
    time("top sites, recomputed", 10, || {
        h.remove("https://nothing.example/");
        h.top_sites(8, now).len()
    });

    let lists: Vec<String> = std::env::args().skip(1).map(|p| std::fs::read_to_string(p).unwrap()).collect();
    if !lists.is_empty() {
        let all: Vec<(&str, &str)> = lists.iter().map(|t| ("list", t.as_str())).collect();
        time("compile filter lists", 3, || volant_core::ublock::compile(&all, volant_core::ublock::Options::default()).stats.network_rules);
    }
}
