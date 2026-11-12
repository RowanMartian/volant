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
//! volant-tools render IN.html OUT.svg [--width N] [--reader URL] [--dark] [--sans]
//!                                              draw a page with Volant's own engine
//!                                              (--reader: as Reader view of URL)
//! ```

mod site;

use std::path::{Path, PathBuf};
use volant_core::{hash, macho, zip};

fn read(p: &str) -> Result<Vec<u8>, String> {
    std::fs::read(p).map_err(|e| format!("{p}: {e}"))
}

fn write(p: &str, data: &[u8]) -> Result<(), String> {
    std::fs::write(p, data).map_err(|e| format!("{p}: {e}"))
}

fn run(args: &[String]) -> Result<(), String> {
    let arg = |i: usize| args.get(i).map(String::as_str).ok_or_else(|| format!("missing argument {i}; see the top of crates/tools/src/main.rs"));
    match arg(0)? {
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
            let started = std::time::Instant::now();
            let page = volant_engine::render(&doc, &css, width, &volant_engine::ApproxFonts);
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
    if let Err(e) = run(&args) {
        eprintln!("volant-tools: {e}");
        std::process::exit(1);
    }
}
