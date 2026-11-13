//! Volant's build tools, so packaging needs nothing but Rust and Apple's
//! signing and disk-image tools:
//!
//! ```text
//! volant-tools site [ROOT]                     site/*.html → docs/ (the website)
//! volant-tools lipo -output OUT THIN THIN...   one universal binary from thin ones
//! volant-tools lipo -info FILE                 the architectures in a binary
//! volant-tools zip SRC_DIR OUT.zip             zip a folder, keeping it as the top level
//! volant-tools unzip IN.zip DIR                unpack (as Volant's updater does)
//! volant-tools sha256 FILE...                  SHA-256 of each file
//! volant-tools render IN.html OUT.svg [--width N] [--reader URL] [--css FILE]... [--dark] [--sans]
//!                                              draw a page with Volant's own engine
//!                                              (--reader: as Reader view of URL; images
//!                                              without a known size are drawn 3:2)
//!     [--scripts URL] [--eval JS] [--type TEXT] [--then JS]
//!                                              …after running the page's JavaScript as at URL
//!                                              (then JS, typing into the focused field, more JS)
//!                                              (other files fetched with curl; console printed)
//! volant-tools js FILE.js                      run a script with Volant's own JavaScript engine
//! ```

mod site;

use std::path::{Path, PathBuf};
use volant_core::{hash, macho, zip};

/// The network for page scripts in previews: curl (where it's installed).
struct CurlHost;

impl volant_engine::script::Host for CurlHost {
    fn fetch(&self, method: &str, url: &str, body: Option<&str>) -> Option<(u16, String, Vec<u8>)> {
        if let Some(path) = url.strip_prefix("file://") {
            // Local files, for testing pages offline.
            let path = path.split(['?', '#']).next().unwrap_or(path);
            return std::fs::read(path).ok().map(|b| (200, String::new(), b));
        }
        if !url.starts_with("http") {
            return None;
        }
        // VOLANT_CACHE=dir replays GETs from (and records them into) dir, so a
        // page's scripts can be timed without the network.
        let cache = std::env::var("VOLANT_CACHE").ok().filter(|_| method == "GET").map(|dir| {
            let digest = hash::sha256(url.as_bytes());
            let name: String = digest.iter().take(10).map(|b| format!("{b:02x}")).collect();
            format!("{dir}/{name}")
        });
        if let Some(c) = &cache {
            if let Ok(b) = std::fs::read(c) {
                let nl = b.iter().position(|&x| x == b'\n')?;
                let head = String::from_utf8_lossy(&b[..nl]).to_string();
                let (code, ty) = head.split_once(' ').unwrap_or((&head, ""));
                return Some((code.parse().ok()?, ty.to_string(), b[nl + 1..].to_vec()));
            }
        }
        let mut cmd = std::process::Command::new("curl");
        cmd.args(["-sL", "--max-time", "20", "-X", method, "-w", "\n%{http_code} %{content_type}", url]);
        if let Some(b) = body {
            cmd.args(["--data-binary", b]);
        }
        let out = cmd.output().ok()?;
        let mut body = out.stdout;
        let nl = body.iter().rposition(|&b| b == b'\n')?;
        let tail = String::from_utf8_lossy(&body[nl + 1..]).to_string();
        body.truncate(nl);
        let (code, ty) = tail.split_once(' ').unwrap_or((&tail, ""));
        // VOLANT_DUMP=dir keeps a copy of everything fetched (for debugging pages).
        if let Ok(dir) = std::env::var("VOLANT_DUMP") {
            let n = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
            let _ = std::fs::write(format!("{dir}/{n:03}.txt"), &body);
            eprintln!("fetched {n:03}: {url}");
        }
        if let Some(c) = &cache {
            let mut rec = format!("{code} {ty}\n").into_bytes();
            rec.extend_from_slice(&body);
            let _ = std::fs::write(c, rec);
        }
        Some((code.parse().ok()?, ty.to_string(), body))
    }
}

/// For previews: every image is a 1200 x 800 photo (nothing is downloaded).
struct PreviewImages;

impl volant_engine::Images for PreviewImages {
    fn size(&self, src: &str) -> Option<(f32, f32)> {
        (!src.is_empty()).then_some((1200.0, 800.0))
    }
}

fn read(p: &str) -> Result<Vec<u8>, String> {
    std::fs::read(p).map_err(|e| format!("{p}: {e}"))
}

fn write(p: &str, data: &[u8]) -> Result<(), String> {
    std::fs::write(p, data).map_err(|e| format!("{p}: {e}"))
}

fn run(args: &[String]) -> Result<(), String> {
    let arg = |i: usize| args.get(i).map(String::as_str).ok_or_else(|| format!("missing argument {i}; see the top of crates/tools/src/main.rs"));
    match arg(0)? {
        "js" => {
            let src = String::from_utf8(read(arg(1)?)?).map_err(|_| "the script isn't UTF-8".to_string())?;
            let t = std::time::Instant::now();
            let (printed, result) = volant_js::run(&src);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            for l in printed {
                println!("{l}");
            }
            result.map_err(|e| format!("uncaught {e}"))?;
            eprintln!("(ran in {ms:.1} ms)");
        }
        "site" => {
            let root = args.get(1).map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
            let built = site::build(&root)?;
            println!("built {}", built.join(", "));
        }
        "lipo" if arg(1)? == "-info" => {
            let data = read(arg(2)?)?;
            let names: Vec<&str> = match macho::slices(&data) {
                Ok(s) => s.iter().map(|x| macho::arch_name(x.0)).collect(),
                Err(_) => vec![macho::arch_name(macho::cpu(&data)?.0)],
            };
            println!("{}: {}", arg(2)?, names.join(" "));
        }
        "lipo" if arg(1)? == "-output" => {
            let out = arg(2)?;
            let thins: Vec<Vec<u8>> = args[3..].iter().map(|p| read(p)).collect::<Result<_, _>>()?;
            let refs: Vec<&[u8]> = thins.iter().map(Vec::as_slice).collect();
            write(out, &macho::fat(&refs)?)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
            }
        }
        "zip" => {
            let data = zip::pack_dir(Path::new(arg(1)?))?;
            write(arg(2)?, &data)?;
            println!("{}: {} bytes", arg(2)?, data.len());
        }
        "unzip" => {
            let data = read(arg(1)?)?;
            let files = zip::extract_to(&data, Path::new(arg(2)?), 1 << 30)?;
            println!("{} files", files.len());
        }
        "sha256" => {
            for p in &args[1..] {
                println!("{}  {p}", hash::hex(&hash::sha256(&read(p)?)));
            }
        }
        "render" => {
            let html = String::from_utf8_lossy(&read(arg(1)?)?).into_owned();
            let out = arg(2)?;
            let flag = |name: &str| args.iter().position(|a| a == name);
            let value = |name: &str| flag(name).and_then(|i| args.get(i + 1)).cloned();
            let width: f32 = value("--width").map(|w| w.parse().map_err(|_| "bad --width")).transpose()?.unwrap_or(900.0);
            let (doc, css) = match value("--reader") {
                Some(url) => {
                    let a = volant_core::reader::extract(&html, &url).ok_or("no article found on that page")?;
                    let host = volant_core::omnibox::host(&url).to_string();
                    let o = volant_engine::reader::Options { dark: flag("--dark").is_some(), size: 20.0, serif: flag("--sans").is_none() };
                    volant_engine::reader::document(&a, &host, &o)
                }
                None => (html, String::new()),
            };
            let mut sheets = vec![css];
            for (i, a) in args.iter().enumerate() {
                if a == "--css" {
                    let f = args.get(i + 1).ok_or("--css needs a file")?;
                    sheets.push(String::from_utf8_lossy(&read(f)?).into_owned());
                }
            }
            let env = volant_engine::Env { width, dark: flag("--dark").is_some() };
            let started = std::time::Instant::now();
            let page = match value("--scripts") {
                Some(url) => {
                    use volant_engine::script;
                    let host: std::rc::Rc<dyn script::Host> = std::rc::Rc::new(CurlHost);
                    let mut rt = script::Runtime::new(&doc, &url, env, host.clone());
                    let setup = started.elapsed();
                    rt.run_scripts();
                    let ran = started.elapsed();
                    if let Some(code) = value("--eval") {
                        match rt.interp.run(&code) {
                            Ok(v) => {
                                let shown = rt.interp.to_str(&v).map(|s| s.to_string()).unwrap_or_default();
                                println!("eval: {shown}");
                            }
                            Err(e) => println!("eval error: {e}"),
                        }
                    }
                    if let Some(text) = value("--type") {
                        rt.type_text(&text);
                        rt.tick();
                    }
                    if let Some(code) = value("--then") {
                        match rt.interp.run(&code) {
                            Ok(v) => println!("then: {}", rt.interp.to_str(&v).map(|s| s.to_string()).unwrap_or_default()),
                            Err(e) => println!("then error: {e}"),
                        }
                    }
                    for line in rt.take_console() {
                        let n = line.chars().count();
                        println!("console: {}", if n > 400 { format!("{}…{}", line.chars().take(120).collect::<String>(), line.chars().skip(n - 260).collect::<String>()) } else { line });
                    }
                    let snap = rt.snapshot();
                    // Linked style sheets, fetched too (unless given with --css).
                    if !args.iter().any(|a| a == "--css") {
                        for (href, media) in script::stylesheet_links(&snap.dom, &snap.url) {
                            if let Some((200..=299, _, b)) = host.fetch("GET", &href, None) {
                                let text = String::from_utf8_lossy(&b).into_owned();
                                sheets.push(if media.is_empty() || media == "all" { text } else { format!("@media {media} {{\n{text}\n}}") });
                            }
                        }
                    }
                    println!("scripts: set up in {:.0} ms, ran in {:.0} ms", setup.as_secs_f64() * 1000.0, (ran - setup).as_secs_f64() * 1000.0);
                    script::render_snapshot(&snap, &sheets, env, &volant_engine::ApproxFonts, &PreviewImages)
                }
                None => volant_engine::render_in(&doc, &sheets, env, &volant_engine::ApproxFonts, &PreviewImages),
            };
            let took = started.elapsed();
            write(out, volant_engine::svg::to_svg(&page).as_bytes())?;
            println!("{out}: {} x {} px, {} items, {} links, laid out in {:.1} ms", page.width, page.height.ceil(), page.items.len(), page.links.len(), took.as_secs_f64() * 1000.0);
        }
        other => return Err(format!("unknown command {other:?}; see the top of crates/tools/src/main.rs")),
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Page scripts nest deeply: give them the stack a browser's content thread has.
    let worker = std::thread::Builder::new().stack_size(volant_engine::script::STACK).spawn(move || run(&args));
    let result = match worker {
        Ok(h) => h.join().unwrap_or_else(|_| Err("crashed".into())),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = result {
        eprintln!("volant-tools: {e}");
        std::process::exit(1);
    }
}
