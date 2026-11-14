use super::*;
use crate::{ApproxFonts, NoImages};

struct FakeHost;

impl Host for FakeHost {
    fn request(&self, req: &Request) -> Option<Reply> {
        let body = req.body.map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
        match req.url {
            "https://example.com/data.json" => Some(Reply::ok(200, "application/json", br#"{"items":["alpha","beta"]}"#.to_vec())),
            "https://example.com/dir/app.js" => Some(Reply::ok(200, "text/javascript", b"document.getElementById('ext').textContent = 'external ran'".to_vec())),
            "https://example.com/dir/mod/util.js" => Some(Reply::ok(200, "text/javascript", b"export const twice = n => n * 2; export default 'util';".to_vec())),
            "https://cdn.example.com/lib/fmt.js" => Some(Reply::ok(200, "text/javascript", b"import u from 'https://example.com/dir/mod/util.js'; export const fmt = s => `[${s}|${u}]`;".to_vec())),
            "https://example.com/echo" => Some(Reply::ok(200, "text/plain", format!("{} {body}", req.method).into_bytes())),
            "https://example.com/headers" => {
                let h: Vec<String> = req.headers.iter().filter(|(k, _)| k != "Referer" && k != "Origin").map(|(k, v)| format!("{}={v}", k.to_ascii_lowercase())).collect();
                let mut r = Reply::ok(200, "text/plain", h.join("&").into_bytes());
                r.headers = vec![("x-answer".into(), "42".into()), ("set-cookie".into(), "hidden=1".into())];
                Some(r)
            }
            // Another origin: open to all, closed, open with credentials, and one that needs a preflight.
            u if u.starts_with("https://api.other.com/") => {
                let path = &u["https://api.other.com".len()..];
                let origin = req.headers.iter().find(|(k, _)| k == "Origin").map(|(_, v)| v.clone()).unwrap_or_default();
                let token = req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("x-token"));
                let mut r = Reply::ok(200, "text/plain", format!("{} {path} creds={} origin={origin} token={token}", req.method, req.credentials).into_bytes());
                let mut h = vec![("x-secret".to_string(), "s".to_string()), ("x-shown".to_string(), "v".to_string())];
                match path {
                    "/open" => h.extend([("access-control-allow-origin".into(), "*".into()), ("access-control-expose-headers".into(), "x-shown".into())]),
                    "/creds" => h.extend([("access-control-allow-origin".into(), "https://example.com".into()), ("access-control-allow-credentials".into(), "true".into())]),
                    "/put" if req.method == "OPTIONS" => {
                        h.extend([("access-control-allow-origin".into(), "https://example.com".into()), ("access-control-allow-methods".into(), "PUT".into()), ("access-control-allow-headers".into(), "x-token".into())]);
                        r.body.clear();
                    }
                    "/put" => h.push(("access-control-allow-origin".into(), "https://example.com".into())),
                    _ => {}
                }
                r.headers = h;
                Some(r)
            }
            _ => None,
        }
    }
}

fn page(html: &str) -> Runtime {
    let mut rt = Runtime::new(html, "https://example.com/dir/page.html?q=1", Env { width: 800.0, height: 600.0, dark: false }, Rc::new(FakeHost));
    rt.run_scripts();
    rt
}

fn text_of(rt: &Runtime, id: &str) -> String {
    let s = rt.state.borrow();
    let n = (0..s.dom.nodes.len() as NodeId).find(|&n| s.dom.attr(n, "id") == Some(id) && connected(&s.dom, n)).expect(id);
    s.dom.text(n)
}

fn console(rt: &mut Runtime) -> String {
    rt.take_console().join("\n")
}

#[test]
fn urls_resolve() {
    let b = "https://a.com/x/y/z.html?q=1#f";
    assert_eq!(resolve_url(b, "w.html"), "https://a.com/x/y/w.html");
    assert_eq!(resolve_url(b, "../w"), "https://a.com/x/w");
    assert_eq!(resolve_url(b, "/root?a"), "https://a.com/root?a");
    assert_eq!(resolve_url(b, "//cdn.com/s.js"), "https://cdn.com/s.js");
    assert_eq!(resolve_url(b, "?p=2"), "https://a.com/x/y/z.html?p=2");
    assert_eq!(resolve_url(b, "#top"), "https://a.com/x/y/z.html?q=1#top");
    assert_eq!(resolve_url(b, "mailto:me@x.com"), "mailto:me@x.com");
    assert_eq!(resolve_url("https://a.com", "b/./c/../d"), "https://a.com/b/d");
}

#[test]
fn scripts_build_and_change_the_dom() {
    let mut rt = page(
        r#"<html><head><title>Old</title></head><body>
        <ul id="list"></ul><p id="greet">hi</p><div id="ext"></div><p id="count"></p>
        <script>
          const list = document.getElementById('list')
          for (const name of ['one', 'two', 'three']) {
            const li = document.createElement('li')
            li.textContent = name
            li.className = 'item'
            list.appendChild(li)
          }
          list.children[1].remove()
          document.querySelector('#greet').innerHTML = '<b>Hello</b>, <i>world</i>'
          document.title = 'New ' + document.querySelectorAll('li.item').length
          document.getElementById('count').textContent = [...document.querySelectorAll('li')].map(l => l.textContent).join('+')
          const p = document.createElement('p'); p.id = 'made'; p.dataset.kind = 'note'; p.style.color = 'red'
          p.append('made ', document.createElement('span'))
          document.body.insertBefore(p, document.body.firstElementChild)
          console.log(document.body.firstElementChild === p, p.parentNode === document.body, p.closest('body').tagName, document.documentElement.nodeName)
          console.log(location.hostname, location.pathname, new URL('../x?y=2', location.href).href, document.querySelector('ul').matches('#list'))
        </script>
        <script src="app.js"></script>
        </body></html>"#,
    );
    assert_eq!(console(&mut rt), "true true BODY HTML\nexample.com /dir/page.html https://example.com/x?y=2 true");
    assert_eq!(text_of(&rt, "list"), "onethree");
    assert_eq!(text_of(&rt, "greet"), "Hello, world");
    assert_eq!(text_of(&rt, "count"), "one+three");
    assert_eq!(text_of(&rt, "ext"), "external ran");
    let snap = rt.snapshot();
    assert_eq!(snap.title, "New 2");
    let made = (0..snap.dom.nodes.len() as NodeId).find(|&n| snap.dom.attr(n, "id") == Some("made")).unwrap();
    assert_eq!(snap.dom.attr(made, "data-kind"), Some("note"));
    assert_eq!(snap.dom.attr(made, "style"), Some("color: red"));
    // The snapshot is in document order and lays out.
    let page = render_snapshot(&snap, &[], Env { width: 800.0, height: 600.0, dark: false }, &ApproxFonts, &NoImages);
    let svg = crate::svg::to_svg(&page);
    assert!(svg.contains(">Hello</text>") && svg.contains(">one</text>") && !svg.contains(">two</text>"), "{svg}");
}

#[test]
fn events_bubble_and_cancel() {
    let mut rt = page(
        r#"<body><div id="outer"><button id="btn">Press</button></div><p id="out"></p><a id="link" href="/next">go</a><a id="stay" href="/stay">no</a>
        <script>
          const log = []
          const out = document.getElementById('out')
          document.getElementById('outer').addEventListener('click', e => log.push('outer:' + e.target.id))
          document.addEventListener('click', () => log.push('document'), { capture: true })
          window.addEventListener('click', () => log.push('window'))
          const btn = document.getElementById('btn')
          btn.addEventListener('click', e => { log.push('button'); out.textContent = 'clicked ' + (++btn.dataset.n || 1) }, { once: false })
          document.getElementById('stay').onclick = e => { e.preventDefault(); log.push('stayed') }
          document.addEventListener('DOMContentLoaded', () => log.push('ready'))
          window.onload = () => log.push('loaded')
          setTimeout(() => log.push('timer'), 100)
          btn.addEventListener('custom', e => log.push('custom ' + e.detail))
          btn.dispatchEvent(new CustomEvent('custom', { detail: 42 }))
          window.report = () => console.log(log.join(', '))
        </script></body>"#,
    );
    let btn = {
        let s = rt.state.borrow();
        (0..s.dom.nodes.len() as NodeId).find(|&n| s.dom.attr(n, "id") == Some("btn")).unwrap()
    };
    rt.click(btn);
    assert_eq!(text_of(&rt, "out"), "clicked 1");
    rt.interp.run("report()").unwrap();
    assert_eq!(console(&mut rt), "custom 42, ready, loaded, timer, document, button, outer:btn, window");
    let link = |rt: &Runtime, id: &str| {
        let s = rt.state.borrow();
        (0..s.dom.nodes.len() as NodeId).find(|&n| s.dom.attr(n, "id") == Some(id)).unwrap()
    };
    let stay = link(&rt, "stay");
    rt.click(stay);
    assert_eq!(rt.take_navigation(), None, "cancelled by preventDefault");
    let go = link(&rt, "link");
    rt.click(go);
    assert_eq!(rt.take_navigation().map(|n| n.url).as_deref(), Some("https://example.com/next"));
}

#[test]
fn forms_typing_and_submit() {
    let mut rt = page(
        r#"<body><form id="f" action="/search"><input id="q" name="q" placeholder="Search"><input type="checkbox" name="safe" id="safe">
        <select name="lang" id="lang"><option value="en">English</option><option value="fr">French</option></select>
        <input type="submit" value="Go"></form><p id="live"></p>
        <script>
          const q = document.getElementById('q')
          q.addEventListener('input', () => { document.getElementById('live').textContent = 'typing ' + q.value })
        </script></body>"#,
    );
    let id = |rt: &Runtime, id: &str| {
        let s = rt.state.borrow();
        (0..s.dom.nodes.len() as NodeId).find(|&n| s.dom.attr(n, "id") == Some(id)).unwrap()
    };
    let q = id(&rt, "q");
    rt.click(q);
    assert_eq!(rt.focused(), Some(q));
    rt.type_text("volant rs");
    rt.key("Backspace");
    assert_eq!(text_of(&rt, "live"), "typing volant r");
    // The caret moves, and typing goes where it is.
    rt.key("Home");
    rt.type_text(">");
    rt.key("End");
    rt.key("ArrowLeft");
    rt.key("Delete");
    rt.key("ArrowLeft");
    rt.key("Backspace");
    assert_eq!(text_of(&rt, "live"), "typing >volan ");
    rt.key("Home");
    rt.key("Delete");
    rt.key("End");
    rt.key("Backspace");
    rt.type_text("t r");
    assert_eq!(text_of(&rt, "live"), "typing volant r");
    let safe = id(&rt, "safe");
    rt.click(safe);
    let lang = id(&rt, "lang");
    rt.click(lang);
    let (menu_for, items) = rt.take_menu().expect("a select opens its menu");
    assert_eq!(menu_for, lang);
    assert_eq!(items.iter().map(|m| (m.label.as_str(), m.selected)).collect::<Vec<_>>(), [("English", true), ("French", false)]);
    rt.choose(lang, 1);
    rt.click(q);
    rt.key("Enter");
    assert_eq!(rt.take_navigation().map(|n| n.url).as_deref(), Some("https://example.com/search?q=volant+r&safe=on&lang=fr"));
    // The snapshot shows the typed value inside the field.
    let snap = rt.snapshot();
    let page = render_snapshot(&snap, &[], Env { width: 800.0, height: 600.0, dark: false }, &ApproxFonts, &NoImages);
    let svg = crate::svg::to_svg(&page);
    assert!(svg.contains(">volant r</text>") && svg.contains("French") && svg.contains(">✓</text>"), "{svg}");
    // Hit testing finds the field, mapped back to the runtime's node.
    let hit = page.items.iter().find_map(|it| match it {
        crate::Item::Text { x, y, text, .. } if text == "volant r" => Some((*x + 2.0, *y - 4.0)),
        _ => None,
    });
    let (hx, hy) = hit.unwrap();
    let node = page.hit(hx, hy).unwrap();
    let runtime_node = snap.map[node as usize];
    let s = rt.state.borrow();
    let el = if matches!(s.dom.nodes[runtime_node as usize].data, Data::Text(_)) { s.dom.parent(runtime_node) } else { runtime_node };
    assert_eq!(el, q);
}

#[test]
fn platform_apis() {
    let mut rt = page(
        r#"<body><div id="box" class="a b"></div><script>
          const out = []
          fetch('/data.json').then(r => r.json()).then(d => out.push('fetch ' + d.items.join('/')))
          const x = new XMLHttpRequest(); x.open('POST', '/echo'); x.onload = () => out.push('xhr ' + x.responseText); x.send('body')
          const u = new URL('https://user@volant.app:8443/p/a?x=1&y=two#h')
          u.searchParams.append('z', 'a b')
          out.push(u.host + ' ' + u.pathname + ' ' + u.search + ' ' + u.hash)
          localStorage.setItem('k', 1); out.push('storage ' + localStorage.getItem('k') + typeof localStorage.getItem('k'))
          const box = document.getElementById('box')
          box.classList.add('c'); box.classList.remove('a'); box.classList.toggle('b')
          out.push('class ' + box.className + ' ' + box.classList.contains('c'))
          out.push('b64 ' + btoa('Volant') + ' ' + atob(btoa('Volant')))
          out.push('media ' + matchMedia('(min-width: 600px)').matches + ' ' + matchMedia('(prefers-color-scheme: dark)').matches)
          out.push('computed ' + getComputedStyle(box).display + ' ' + innerWidth)
          new IntersectionObserver(entries => out.push('seen ' + entries[0].isIntersecting)).observe(box)
          const fd = new FormData(); fd.append('a', 1); out.push('form ' + [...fd.keys()].join())
          async function go() { const r = await fetch('/echo', { method: 'PUT', body: JSON.stringify({ n: 1 }) }); out.push('async ' + await r.text()) }
          go()
          setTimeout(() => console.log(out.join(' | ')), 10)
        </script></body>"#,
    );
    assert_eq!(
        console(&mut rt),
        "volant.app:8443 /p/a ?x=1&y=two&z=a+b #h | storage 1string | class c true | b64 Vm9sYW50 Volant | media true false | computed block 800 | form a | async PUT {\"n\":1} | fetch alpha/beta | xhr POST body | seen true"
    );
}

#[test]
fn broken_scripts_dont_stop_the_page() {
    let mut rt = page(
        r#"<body><p id="a"></p><p id="b"></p>
        <script>document.getElementById('a').textContent = 'first'; undefinedFunction()</script>
        <script>this is not javascript</script>
        <script>document.getElementById('b').textContent = 'third'</script>
        <script src="missing.js"></script>
        <script>while (true) {}</script>
        </body>"#,
    );
    assert_eq!(text_of(&rt, "a"), "first");
    assert_eq!(text_of(&rt, "b"), "third");
    let c = console(&mut rt);
    assert!(c.contains("ReferenceError") && c.contains("SyntaxError") && c.contains("couldn't load https://example.com/dir/missing.js") && c.contains("too long"), "{c}");
}

#[test]
fn inserted_scripts_run_and_fire_load() {
    let mut rt = page(
        r#"<body><p id="ext"></p><p id="note"></p><script>
          const s = document.createElement('script')
          s.src = 'app.js'
          s.onload = () => { document.getElementById('note').textContent = 'loaded ' + document.getElementById('ext').textContent }
          document.head.appendChild(s)
          const inline = document.createElement('script')
          inline.textContent = 'window.fromInline = 7'
          document.body.append(inline)
          setTimeout(() => console.log('inline', window.fromInline), 5)
        </script></body>"#,
    );
    assert_eq!(text_of(&rt, "note"), "loaded external ran");
    assert_eq!(console(&mut rt), "inline 7");
}

#[test]
fn module_scripts_and_import_maps() {
    let mut rt = page(
        r#"<body><p id="out"></p>
        <script type="importmap">{"imports": {"fmt": "https://cdn.example.com/lib/fmt.js", "m/": "./mod/"}}</script>
        <script type="module">
          import { twice } from 'm/util.js'
          import { fmt } from 'fmt'
          document.getElementById('out').textContent = fmt(twice(21))
          const lazy = await import('./mod/util.js')
          console.log('lazy', lazy.default, import.meta.url.startsWith('https://example.com/dir/page.html'))
        </script>
        <script>console.log('classic first')</script>
        <script nomodule>console.log('never')</script></body>"#,
    );
    assert_eq!(text_of(&rt, "out"), "[42|util]");
    assert_eq!(console(&mut rt), "classic first\nlazy util true");
}

#[test]
fn post_forms_cookies_and_request_headers() {
    let mut rt = page(
        "<form id=f method=post action=/login><input name=user value='amy lee'><input name=pass value='a&b'>\
         <button id=go name=via value=button>Sign in</button></form>\
         <form id=m method=post enctype=multipart/form-data action=/up><input name=note value=hi><button id=up>Up</button></form>\
         <script>
           document.cookie = 'theme=dark; path=/';
           document.cookie = 'gone=1';
           document.cookie = 'gone=; max-age=0';
           console.log(document.cookie);
           fetch('/headers', { method: 'POST', headers: { 'X-Token': 't1' }, body: new URLSearchParams({ a: '1' }) })
             .then(r => r.text().then(t => console.log(r.status, r.headers.get('x-answer'), r.headers.get('set-cookie'), t)));
           const x = new XMLHttpRequest(); x.open('GET', '/headers'); x.setRequestHeader('Accept', 'application/json');
           x.onload = () => console.log('xhr', x.responseText, x.getResponseHeader('X-Answer')); x.send();
         </script>",
    );
    rt.tick();
    let out = console(&mut rt);
    assert_eq!(out, "theme=dark\n200 42 null x-token=t1&content-type=application/x-www-form-urlencoded;charset=UTF-8\nxhr accept=application/json 42");
    let id = |rt: &Runtime, id: &str| {
        let s = rt.state.borrow();
        (0..s.dom.nodes.len() as NodeId).find(|&n| s.dom.attr(n, "id") == Some(id)).unwrap()
    };
    let go = id(&rt, "go");
    rt.click(go);
    let nav = rt.take_navigation().expect("the form submits");
    assert_eq!((nav.url.as_str(), nav.method.as_str()), ("https://example.com/login", "POST"));
    assert_eq!(nav.body.as_deref(), Some(&b"user=amy+lee&pass=a%26b&via=button"[..]));
    assert_eq!(nav.content_type.as_deref(), Some("application/x-www-form-urlencoded"));
    let up = id(&rt, "up");
    rt.click(up);
    let nav = rt.take_navigation().expect("the multipart form submits");
    let ty = nav.content_type.unwrap();
    let boundary = ty.strip_prefix("multipart/form-data; boundary=").unwrap();
    let body = String::from_utf8(nav.body.unwrap()).unwrap();
    assert_eq!(body, format!("--{boundary}\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nhi\r\n--{boundary}--\r\n"));
}

#[test]
fn hover_focus_target_and_caret_in_snapshots() {
    let mut rt = page(
        "<style>a:hover { color: red } .card:hover .hint { display: block } .hint { display: none } :target { outline: 0 }</style>\
         <p id=plain>plain <b id=bold>text</b></p><a id=link href='#sec'>go</a><div class=card id=card><span class=hint id=hint>hi</span></div>\
         <input id=f value=abc><h2 id=sec>Section</h2>\
         <script>let log = []; const c = document.getElementById('card');\
           c.addEventListener('mouseenter', () => log.push('enter')); c.addEventListener('mouseleave', () => log.push('leave'));\
           c.addEventListener('mouseover', e => log.push('over:' + e.target.id));</script>",
    );
    let id = |rt: &Runtime, id: &str| {
        let s = rt.state.borrow();
        (0..s.dom.nodes.len() as NodeId).find(|&n| s.dom.attr(n, "id") == Some(id)).unwrap()
    };
    let (plain, bold, link, card, hint) = (id(&rt, "plain"), id(&rt, "bold"), id(&rt, "link"), id(&rt, "card"), id(&rt, "hint"));
    // No :hover rule covers plain text: no new layout.
    assert!(!rt.hover(Some(plain)));
    assert!(!rt.hover(Some(bold)));
    assert!(rt.hover(Some(link)), "a:hover changes the link");
    assert!(rt.hover(Some(hint)), ".card:hover .hint changes");
    assert!(!rt.hover(Some(card)), "still inside the card");
    assert!(rt.hover(None));
    rt.interp.run("console.log(log.join())").unwrap();
    assert_eq!(console(&mut rt), "over:hint,enter,over:card,leave");
    rt.hover(Some(hint));
    let snap = rt.snapshot();
    let marked = |snap: &Snapshot, id: &str, mark: &str| (0..snap.dom.nodes.len() as NodeId).any(|n| snap.dom.attr(n, "id") == Some(id) && snap.dom.attr(n, mark).is_some());
    assert!(marked(&snap, "card", css::STATE_HOVER) && marked(&snap, "hint", css::STATE_HOVER) && !marked(&snap, "link", css::STATE_HOVER));
    // Focus puts the caret at the end of the field; arrows move it.
    let f = id(&rt, "f");
    rt.click(f);
    rt.key("ArrowLeft");
    let snap = rt.snapshot();
    let page = render_snapshot(&snap, &[], Env { width: 800.0, height: 600.0, dark: false }, &ApproxFonts, &NoImages);
    let svg = crate::svg::to_svg(&page);
    assert!(svg.contains(">ab</text>") && svg.contains(">c</text>"), "the caret splits the value: {svg}");
    // A same-page link scrolls to its target, which is :target.
    rt.click(link);
    assert!(rt.take_navigation().is_none(), "#links stay on the page");
    let snap = rt.snapshot();
    assert!(snap.url.ends_with("#sec"));
    let page = render_snapshot(&snap, &[], Env { width: 800.0, height: 600.0, dark: false }, &ApproxFonts, &NoImages);
    let y = anchor_y(&snap, &page, "sec").expect("the section is on the page");
    assert!(y > 40.0, "{y}");
}

#[test]
fn canvas_draws_pixels() {
    let mut rt = page(
        "<canvas id=c width=200 height=100></canvas><canvas id=empty></canvas>\
         <script>const c = document.getElementById('c'), g = c.getContext('2d');\
           g.fillStyle = 'red'; g.fillRect(0, 0, 10, 10);\
           g.clearRect(0, 0, 200, 100);\
           g.fillStyle = '#00f'; g.translate(50, 20); g.fillRect(0, 0, 10, 5);\
           g.beginPath(); g.arc(0, 0, 10, 0, Math.PI * 2); g.strokeStyle = 'rgba(0,0,0,0.5)'; g.lineWidth = 2; g.stroke();\
           g.setTransform(1, 0, 0, 1, 0, 0); g.font = 'bold 16px serif'; g.textAlign = 'center'; g.fillText('Hi', 100, 90);\
           console.log(g.measureText('Hi').width > 10, c.width, document.getElementById('empty').width, c.getContext('webgl'));</script>",
    );
    assert_eq!(console(&mut rt), "true 200 300 null");
    let snap = rt.snapshot();
    assert_eq!(snap.canvases.len(), 1, "only the canvas with a context");
    let canvas = snap.canvases.values().next().unwrap();
    assert_eq!((canvas.width, canvas.height), (200, 100));
    let px = |x: usize, y: usize| canvas.pixels[(y * 200 + x) * 4..(y * 200 + x) * 4 + 4].to_vec();
    // The red square was cleared; the blue one is where the transform put it.
    assert_eq!(px(5, 5), vec![0, 0, 0, 0]);
    assert_eq!(px(55, 22), vec![0, 0, 255, 255]);
    // The circle's stroke, half transparent black.
    assert!((px(60, 20)[3] as i32 - 128).abs() < 70, "{:?}", px(60, 20));
    // Laid out like a picture of that size.
    let page = render_snapshot(&snap, &[], Env { width: 800.0, height: 600.0, dark: false }, &ApproxFonts, &NoImages);
    let boxes: Vec<(f32, f32, String)> = page.items.iter().filter_map(|i| match i {
        crate::Item::Image { w, h, src, .. } => Some((*w, *h, src.clone())),
        _ => None,
    }).collect();
    assert_eq!(boxes.len(), 2);
    assert_eq!((boxes[0].0, boxes[0].1), (200.0, 100.0));
    assert!(snap.canvases.contains_key(&boxes[0].2));
    assert_eq!((boxes[1].0, boxes[1].1), (300.0, 150.0));
}

#[test]
fn assess_finds_pages_canard_cannot_show() {
    let check = |html: &str, errors: usize| {
        let mut rt = Runtime::new(html, "https://example.com/", Env { width: 800.0, height: 600.0, dark: false }, Rc::new(FakeHost));
        assess(&rt.snapshot(), errors)
    };
    let article = format!("<p>{}</p>", "Jet engines push air backwards. ".repeat(30));
    assert_eq!(check(&article, 0), None);
    assert!(check("<div id=root></div><script src=app.js></script>", 0).is_some());
    assert!(check("<frameset><frame src=a.html></frameset>", 0).is_some());
    assert!(check("<video src=a.mp4></video>", 0).is_some());
    assert!(check("<p>Loading</p><script></script>", 4).is_some());
    assert_eq!(check(&article, 4), None);
}

#[test]
fn same_origin_policy_and_cors() {
    let mut rt = page(
        "<script>
           const log = (k, p) => p.then(r => r.text().then(t => console.log(k, r.status, r.type, t, r.headers.get('x-secret'), r.headers.get('x-shown'))), e => console.log(k, 'refused'));
           log('open', fetch('https://api.other.com/open'));
           log('closed', fetch('https://api.other.com/closed'));
           log('creds', fetch('https://api.other.com/creds', { credentials: 'include' }));
           log('nocreds', fetch('https://api.other.com/creds'));
           log('put', fetch('https://api.other.com/put', { method: 'PUT', headers: { 'X-Token': '1' } }));
           log('del', fetch('https://api.other.com/open', { method: 'DELETE' }));
           log('opaque', fetch('https://api.other.com/closed', { mode: 'no-cors' }));
           log('same', fetch('https://api.other.com/open', { mode: 'same-origin' }));
           const x = new XMLHttpRequest(); x.open('GET', 'https://api.other.com/closed');
           x.onerror = () => console.log('xhr refused'); x.onload = () => console.log('xhr read', x.responseText); x.send();
         </script>",
    );
    rt.tick();
    let out = console(&mut rt);
    let line = |k: &str| out.lines().find(|l| l.starts_with(&format!("{k} "))).unwrap_or("").to_string();
    // Readable when allowed, with only the headers the server exposes; no cookies unless asked.
    assert_eq!(line("open"), "open 200 cors GET /open creds=false origin=https://example.com token=false null v");
    assert_eq!(line("closed"), "closed refused");
    assert_eq!(line("creds"), "creds 200 cors GET /creds creds=true origin=https://example.com token=false null null");
    assert_eq!(line("nocreds"), "nocreds 200 cors GET /creds creds=false origin=https://example.com token=false null null");
    // A PUT with a custom header asks first, and goes when the preflight allows it.
    assert_eq!(line("put"), "put 200 cors PUT /put creds=false origin=https://example.com token=true null null");
    assert_eq!(line("del"), "del refused");
    // no-cors: sent, but the page learns nothing.
    assert_eq!(line("opaque"), "opaque 0 opaque  null null");
    assert_eq!(line("same"), "same refused");
    assert_eq!(line("xhr"), "xhr refused");
    assert!(out.contains("Blocked by CORS"), "{out}");
}

#[test]
fn node_types_documents_and_dom_exceptions() {
    let mut rt = page(
        r#"<!DOCTYPE html><html><body><!--marker--><p id="p">a<!-- x -->b</p><script>
          const out = []
          const p = document.getElementById('p')
          out.push(document.doctype.name + ' ' + document.doctype.nodeType + ' ' + document.body.firstChild.nodeType + ' ' + document.body.firstChild.data)
          out.push(p.childNodes.length + ' ' + p.textContent + ' ' + p.innerHTML)
          const t = new Text('hello'); out.push(t.splitText(2).data + ' ' + t.data + ' ' + (t instanceof CharacterData))
          try { t.appendChild(document.createElement('b')) } catch (e) { out.push(e.name + ' ' + e.code + ' ' + (e instanceof DOMException)) }
          try { document.appendChild(document.createElement('b')) } catch (e) { out.push(e.name) }
          try { p.classList.add('') } catch (e) { out.push(e.name + ' ' + e.code) }
          const d = document.implementation.createHTMLDocument('T')
          const q = d.createElement('i'); d.body.appendChild(q)
          out.push(d.title + ' ' + (q.ownerDocument === d) + ' ' + (d.body !== document.body) + ' ' + d.documentElement.nodeName)
          document.body.appendChild(q); out.push(String(q.ownerDocument === document))
          const x = new DOMParser().parseFromString('<p class=z>hi</p>', 'text/html')
          out.push(x.querySelector('.z').textContent + ' ' + (x.body.parentNode === x.documentElement))
          out.push(String(Object.getPrototypeOf(TypeError) === Error))
          console.log(out.join(' | '))
        </script></body></html>"#,
    );
    assert_eq!(
        console(&mut rt),
        "html 10 8 marker | 3 ab a<!-- x -->b | llo he true | HierarchyRequestError 3 true | HierarchyRequestError | SyntaxError 12 | T true true HTML | true | hi true | true"
    );
}
