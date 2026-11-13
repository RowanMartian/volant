use super::*;
use crate::{ApproxFonts, NoImages};

struct FakeHost;

impl Host for FakeHost {
    fn fetch(&self, method: &str, url: &str, body: Option<&str>) -> Option<(u16, String, Vec<u8>)> {
        match url {
            "https://example.com/data.json" => Some((200, "application/json".into(), br#"{"items":["alpha","beta"]}"#.to_vec())),
            "https://example.com/dir/app.js" => Some((200, "text/javascript".into(), b"document.getElementById('ext').textContent = 'external ran'".to_vec())),
            "https://example.com/dir/mod/util.js" => Some((200, "text/javascript".into(), b"export const twice = n => n * 2; export default 'util';".to_vec())),
            "https://cdn.example.com/lib/fmt.js" => Some((200, "text/javascript".into(), b"import u from 'https://example.com/dir/mod/util.js'; export const fmt = s => `[${s}|${u}]`;".to_vec())),
            "https://example.com/echo" => Some((200, "text/plain".into(), format!("{method} {}", body.unwrap_or("")).into_bytes())),
            _ => None,
        }
    }
}

fn page(html: &str) -> Runtime {
    let mut rt = Runtime::new(html, "https://example.com/dir/page.html?q=1", Env { width: 800.0, dark: false }, Rc::new(FakeHost));
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
    let page = render_snapshot(&snap, &[], Env { width: 800.0, dark: false }, &ApproxFonts, &NoImages);
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
    assert_eq!(rt.take_navigation().as_deref(), Some("https://example.com/next"));
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
    let safe = id(&rt, "safe");
    rt.click(safe);
    let lang = id(&rt, "lang");
    rt.click(lang);
    rt.click(q);
    rt.key("Enter");
    assert_eq!(rt.take_navigation().as_deref(), Some("https://example.com/search?q=volant+r&safe=on&lang=fr"));
    // The snapshot shows the typed value inside the field.
    let snap = rt.snapshot();
    let page = render_snapshot(&snap, &[], Env { width: 800.0, dark: false }, &ApproxFonts, &NoImages);
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
