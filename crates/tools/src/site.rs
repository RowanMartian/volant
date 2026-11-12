//! Builds Volant's website: wraps each page in `site/` with the shared head,
//! navigation and footer, and writes it to `docs/` (what GitHub Pages serves).
//!
//! A page starts with a comment of `key: value` lines (title, description,
//! nav), then its body. `{{icon:name}}` inserts one of the icons below,
//! `{{dmg}}` the download link and `{{repo}}` the repository. Anything
//! after `<!-- scripts -->` goes after the footer.

use std::path::Path;

const SITE: &str = "https://rowanmartian.github.io/volant/";
pub const DMG: &str = "https://github.com/RowanMartian/volant/releases/download/latest/Volant.dmg";
pub const REPO: &str = "https://github.com/RowanMartian/volant";

const NAV: &[(&str, &str, &str)] = &[
    ("features", "features.html", "Features"),
    ("blocking", "blocking.html", "Blocking"),
    ("performance", "performance.html", "Performance"),
    ("help", "help.html", "Help"),
    ("download", "download.html", "Download"),
];

/// Stroked 24-point line icons, filled ones, and a few drawn on their own grid.
enum Icon {
    Stroke(&'static str),
    Fill(&'static str),
    Raw(&'static str),
}

const ICONS: &[(&str, Icon)] = &[
    ("download", Icon::Raw(r##"<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M10 3v10M5.5 8.5 10 13l4.5-4.5M4 17h12"/></svg>"##)),
    ("shield", Icon::Stroke(r##"<path d="M12 3 4 6v6c0 5 3.5 8 8 9 4.5-1 8-4 8-9V6z"/>"##)),
    ("shield-fill", Icon::Fill(r##"<path d="M12 2.5 3.5 5.7v6.1c0 5.3 3.7 8.6 8.5 9.7 4.8-1.1 8.5-4.4 8.5-9.7V5.7z"/>"##)),
    ("reader", Icon::Stroke(r##"<path d="M5 5h14M5 10h14M5 15h9M5 20h6"/>"##)),
    ("moon", Icon::Stroke(r##"<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z"/>"##)),
    ("search", Icon::Stroke(r##"<circle cx="11" cy="11" r="6.5"/><path d="m16 16 4.5 4.5"/>"##)),
    ("update", Icon::Stroke(r##"<path d="M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5"/>"##)),
    ("lock", Icon::Stroke(r##"<rect x="5" y="10" width="14" height="10" rx="2.5"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/>"##)),
    ("sidebar", Icon::Stroke(r##"<rect x="3.5" y="4.5" width="17" height="15" rx="3"/><path d="M9.5 4.5v15"/>"##)),
    ("star", Icon::Stroke(r##"<path d="m12 3.5 2.6 5.3 5.9.9-4.3 4.1 1 5.8-5.2-2.8-5.2 2.8 1-5.8-4.3-4.1 5.9-.9z"/>"##)),
    ("sun", Icon::Stroke(r##"<circle cx="12" cy="12" r="4"/><path d="M12 2.5v2M12 19.5v2M4.6 4.6 6 6M18 18l1.4 1.4M2.5 12h2M19.5 12h2M4.6 19.4 6 18M18 6l1.4-1.4"/>"##)),
    ("find", Icon::Stroke(r##"<path d="M4 6h10M4 12h6M4 18h6"/><circle cx="16" cy="15" r="3.5"/><path d="m18.6 17.6 2.4 2.4"/>"##)),
    ("pin", Icon::Stroke(r##"<path d="M9 4h6l-1 5 3 3v2H7v-2l3-3zM12 14v6"/>"##)),
    ("box", Icon::Stroke(r##"<path d="M12 4v10M7.5 9.5 12 14l4.5-4.5M5 19h14"/>"##)),
    ("bolt", Icon::Stroke(r##"<path d="M13 3 5 13.5h6L10 21l8-10.5h-6z"/>"##)),
    ("picker", Icon::Stroke(r##"<path d="M4 4l6.5 16 2.3-6.7L19.5 11z"/><path d="m13 13 6 6"/>"##)),
    ("list", Icon::Stroke(r##"<path d="M9 6h11M9 12h11M9 18h11"/><circle cx="4.5" cy="6" r="1"/><circle cx="4.5" cy="12" r="1"/><circle cx="4.5" cy="18" r="1"/>"##)),
    ("code", Icon::Stroke(r##"<path d="m8 8-4 4 4 4M16 8l4 4-4 4M13.5 5l-3 14"/>"##)),
    ("check", Icon::Stroke(r##"<path d="m5 12.5 4.5 4.5L19 7.5"/>"##)),
    ("eye", Icon::Stroke(r##"<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/>"##)),
    ("keyboard", Icon::Stroke(r##"<rect x="2.5" y="6" width="19" height="12" rx="3"/><path d="M6.5 10h1M10.5 10h1M14.5 10h1M8 14h8"/>"##)),
    ("github", Icon::Raw(r##"<svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0 0 16 8c0-4.42-3.58-8-8-8z"/></svg>"##)),
];

/// `{title}`, `{description}`, `{site}` and `{canonical}` are filled in per page.
const HEAD: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<meta name="description" content="{description}">
<meta name="theme-color" content="#0d0c11">
<meta property="og:type" content="website">
<meta property="og:site_name" content="Volant">
<meta property="og:title" content="{title}">
<meta property="og:description" content="{description}">
<meta property="og:image" content="{site}og.png">
<meta property="og:url" content="{site}{canonical}">
<link rel="canonical" href="{site}{canonical}">
<link rel="icon" href="logo.svg" type="image/svg+xml">
<link rel="stylesheet" href="site.css">
<link rel="stylesheet" href="motion.css">
<script src="brand.js" defer></script>
<script src="motion.js" defer></script>
</head>
<body>
"##;

const FOOTER: &str = r##"<footer><div class="wrap">
  <div class="cols">
    <div>
      <div class="brand"><img src="logo.svg" alt="" width="26" height="26">Volant</div>
      <div>The featherweight browser for your Mac.<br>Free and open source, MIT licensed.</div>
    </div>
    <div><h4>Product</h4><ul><li><a href="features.html">Features</a></li><li><a href="blocking.html">Blocking</a></li><li><a href="performance.html">Performance</a></li><li><a href="download.html">Download</a></li></ul></div>
    <div><h4>Help</h4><ul><li><a href="help.html">Shortcuts and FAQ</a></li><li><a href="download.html#install">Installing</a></li><li><a href="privacy.html">Privacy</a></li></ul></div>
    <div><h4>Project</h4><ul><li><a href="https://github.com/RowanMartian/volant">Source on GitHub</a></li><li><a href="https://github.com/RowanMartian/volant/releases/tag/latest">Release notes</a></li><li><a href="https://github.com/RowanMartian/volant/issues">Report a problem</a></li></ul></div>
  </div>
  <p class="legal">Blocking is powered by uBlock: Volant uses uBlock Origin's filter lists and filter syntax. uBlock Origin is by Raymond Hill and contributors; Volant is an independent project, not made or endorsed by them. Mac, macOS, Safari and WebKit are trademarks of Apple Inc.</p>
</div></footer>
"##;

fn icon(name: &str) -> Option<String> {
    let (_, i) = ICONS.iter().find(|(n, _)| *n == name)?;
    Some(match i {
        Icon::Stroke(d) => format!(r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{d}</svg>"#),
        Icon::Fill(d) => format!(r#"<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">{d}</svg>"#),
        Icon::Raw(s) => s.to_string(),
    })
}

fn nav(current: &str) -> String {
    let links: String = NAV
        .iter()
        .map(|(key, href, label)| format!(r#"<a href="{href}"{}>{label}</a>"#, if *key == current { r#" aria-current="page""# } else { "" }))
        .collect();
    format!(
        "<nav><div class=\"wrap\">\n  <a class=\"home\" href=\"./\"><img src=\"logo.svg\" alt=\"\" width=\"30\" height=\"30\">Volant</a>\n  <div class=\"links\">{links}<a href=\"{REPO}\">GitHub</a></div>\n  <a class=\"btn primary\" href=\"{DMG}\">Download</a>\n</div></nav>\n"
    )
}

/// Replaces every `{{icon:name}}` (names are `a-z` and `-`).
fn icons(body: &str) -> Result<String, String> {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(i) = rest.find("{{icon:") {
        let after = &rest[i + 7..];
        let name_len = after.bytes().take_while(|c| c.is_ascii_lowercase() || *c == b'-').count();
        if name_len == 0 || !after[name_len..].starts_with("}}") {
            out.push_str(&rest[..i + 7]);
            rest = after;
            continue;
        }
        let name = &after[..name_len];
        out.push_str(&rest[..i]);
        out.push_str(&icon(name).ok_or_else(|| format!("no icon called {name:?}"))?);
        rest = &after[name_len + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// One page of `site/` as the finished HTML.
pub fn page(name: &str, text: &str) -> Result<String, String> {
    let meta_text = text
        .strip_prefix("<!--")
        .and_then(|t| t.find("-->\n").map(|end| &t[..end]))
        .ok_or_else(|| format!("{name}: no front-matter comment"))?;
    let body = &text[4 + meta_text.len() + 4..];
    let mut title = None;
    let mut description = None;
    let mut current = "";
    for line in meta_text.trim().lines() {
        let (k, v) = line.split_once(':').ok_or_else(|| format!("{name}: bad front matter line {line:?}"))?;
        match k.trim() {
            "title" => title = Some(v.trim()),
            "description" => description = Some(v.trim()),
            "nav" => current = v.trim(),
            _ => {}
        }
    }
    let title = title.ok_or_else(|| format!("{name}: no title"))?;
    let description = description.ok_or_else(|| format!("{name}: no description"))?;
    let body = icons(body).map_err(|e| format!("{name}: {e}"))?.replace("{{dmg}}", DMG).replace("{{repo}}", REPO);
    let canonical = if name == "index.html" { "" } else { name };
    let head = HEAD.replace("{title}", title).replace("{description}", description).replace("{site}", SITE).replace("{canonical}", canonical);
    let (body, scripts) = match body.split_once("<!-- scripts -->") {
        Some((b, s)) => (b.to_string(), s.trim().to_string()),
        None => (body, String::new()),
    };
    let mut page = head;
    page.push_str(&nav(current));
    page.push_str(body.trim_end());
    page.push_str("\n\n");
    page.push_str(FOOTER);
    page.push_str(&scripts);
    if !scripts.is_empty() {
        page.push('\n');
    }
    page.push_str("</body>\n</html>\n");
    Ok(page)
}

/// Builds every page in `root/site` into `root/docs`.
pub fn build(root: &Path) -> Result<Vec<String>, String> {
    let src = root.join("site");
    let out = root.join("docs");
    let mut names: Vec<String> = std::fs::read_dir(&src)
        .map_err(|e| format!("{}: {e}", src.display()))?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".html"))
        .collect();
    names.sort();
    for name in &names {
        let text = std::fs::read_to_string(src.join(name)).map_err(|e| format!("{name}: {e}"))?;
        std::fs::write(out.join(name), page(name, &text)?).map_err(|e| format!("{name}: {e}"))?;
    }
    std::fs::write(out.join(".nojekyll"), "").map_err(|e| e.to_string())?;
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_page() {
        let p = page("x.html", "<!--\ntitle: T — V\ndescription: D\nnav: help\n-->\n<p>{{icon:check}} <a href=\"{{dmg}}\">get</a> {{icon:nope</p>\n<!-- scripts -->\n<script>1</script>\n").unwrap();
        assert!(p.contains("<title>T — V</title>") && p.contains(r#"<link rel="canonical" href="https://rowanmartian.github.io/volant/x.html">"#));
        assert!(p.contains(r#"<a href="help.html" aria-current="page">Help</a>"#));
        assert!(p.contains(r#"<path d="m5 12.5 4.5 4.5L19 7.5"/></svg> <a href="https://github.com/RowanMartian/volant/releases/download/latest/Volant.dmg">get</a> {{icon:nope</p>"#));
        assert!(p.ends_with("</footer>\n<script>1</script>\n</body>\n</html>\n"));
        assert!(page("y.html", "<!--\ntitle: a\ndescription: b\n-->\n{{icon:missing}}").is_err());
        assert!(page("z.html", "no front matter").is_err());
    }
}
