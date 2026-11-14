use crate::run;

fn out(src: &str) -> String {
    match run(src) {
        (lines, Ok(())) => lines.join("\n"),
        (_, Err(e)) => format!("ERROR {e}"),
    }
}

#[test]
fn basics() {
    assert_eq!(out("console.log(1 + 2 * 3, 'a' + 1, 7 / 2, 2 ** 10, 7 % 3, -7 % 3)"), "7 a1 3.5 1024 1 -1");
    assert_eq!(out("console.log(0.1 + 0.2, 1e21, 1 / 0, -0 === 0, NaN === NaN, typeof null, typeof undefined)"), "0.30000000000000004 1e+21 Infinity true false object undefined");
    assert_eq!(out("console.log('5' * '2', '5' + 2, '5' - 2, [] + [], 1 == '1', 1 === '1', null == undefined, null === undefined)"), "10 52 3 \"\" true false true false".replace("\"\"", ""));
    assert_eq!(out("console.log(5 & 3, 5 | 3, 5 ^ 3, ~5, 1 << 4, -16 >> 2, -16 >>> 28)"), "1 7 6 -6 16 -4 15");
    assert_eq!(out("let x; x ??= 4; let y = 0; y ||= 9; console.log(x, y, null?.a, ({a:{b:3}}).a?.b)"), "4 9 undefined 3");
    assert_eq!(out("console.log(`a${1 + 1}b${'c'}`, 0x1f, 0b101, 1_000)"), "a2bc 31 5 1000");
}

#[test]
fn asi_and_control_flow() {
    let src = "
        let total = 0
        for (let i = 0; i < 10; i++) {
            if (i % 2) continue
            if (i > 6) break
            total += i
        }
        let j = 0
        while (true) { if (++j >= 3) break }
        do { j++ } while (j < 5)
        outer: for (const a of [1, 2, 3]) {
            for (const b of [1, 2, 3]) {
                if (b === 2) continue outer
                if (a === 3) break outer
                total += 100
            }
        }
        switch (j) {
            case 4: total = -1; break
            case 5: total += 1000
            case 6: total += 1; break
            default: total = -2
        }
        console.log(total, j)
        function f() {
            return
            42
        }
        console.log(f())
    ";
    assert_eq!(out(src), "1213 5\nundefined");
}

#[test]
fn closures_and_hoisting() {
    let src = "
        console.log(hoisted(), typeof later)
        function hoisted() { return 'up' }
        var later = 1
        function counter() { let n = 0; return () => ++n }
        const c = counter(); c(); c()
        console.log(c())
        const fns = []
        for (let i = 0; i < 3; i++) fns.push(() => i)
        console.log(fns.map(f => f()).join())
        const fact = function me(n) { return n <= 1 ? 1 : n * me(n - 1) }
        console.log(fact(10))
        function args() { return arguments.length }
        console.log(args(1, 2, 3))
    ";
    assert_eq!(out(src), "up undefined\n3\n0,1,2\n3628800\n3");
}

#[test]
fn objects_classes_and_this() {
    let src = "
        const o = { a: 1, get double() { return this.a * 2 }, set double(v) { this.a = v / 2 }, ['k' + 1]: 'x' }
        o.double = 10
        console.log(o.a, o.double, o.k1, Object.keys(o).join())
        class Animal {
            static count = 0
            #secret = 1
            constructor(name) { this.name = name; Animal.count++ }
            speak() { return `${this.name} makes a sound` }
            get label() { return '<' + this.name + '>' }
        }
        class Dog extends Animal {
            constructor(name) { super(name); this.kind = 'dog' }
            speak() { return super.speak() + ' (woof)' }
        }
        const d = new Dog('Rex')
        console.log(d.speak(), d.label, d instanceof Animal, d instanceof Dog, Animal.count)
        function P(x) { this.x = x }
        P.prototype.get = function () { return this.x }
        console.log(new P(5).get(), 'x' in new P(1), delete o.a, o.a)
        const bound = o.speak
        const m = { v: 7, f() { return this.v } }
        const g = m.f.bind({ v: 8 })
        console.log(m.f(), g(), m.f.call({ v: 9 }), m.f.apply({ v: 10 }, []))
    ";
    assert_eq!(out(src), "5 10 x a,double,k1\nRex makes a sound (woof) <Rex> true true 1\n5 true true undefined\n7 8 9 10");
}

#[test]
fn destructuring_spread_rest() {
    let src = "
        const [a, , b = 5, ...rest] = [1, 2, undefined, 4, 5]
        const { x, y: { z }, w = 'dflt', ...others } = { x: 1, y: { z: 2 }, p: 3, q: 4 }
        console.log(a, b, rest.join(), x, z, w, Object.keys(others).join())
        function f(first, { k } = { k: 'kk' }, ...more) { return [first, k, more.length] }
        console.log(f(1).join(), f(1, { k: 2 }, 3, 4).join())
        const merged = { ...{ a: 1, b: 2 }, b: 3 }
        console.log(JSON.stringify(merged), Math.max(...[3, 9, 2]), [...'abc'].join('-'))
        let p = 1, q = 2;
        [p, q] = [q, p]
        console.log(p, q)
        for (const [k, v] of Object.entries({ one: 1, two: 2 })) console.log(k, v)
    ";
    assert_eq!(out(src), "1 5 4,5 1 2 dflt p,q\n1,kk,0 1,2,2\n{\"a\":1,\"b\":3} 9 a-b-c\n2 1\none 1\ntwo 2");
}

#[test]
fn arrays_and_strings() {
    let src = "
        const a = [5, 1, 10, 2]
        console.log(a.slice().sort().join(), a.slice().sort((x, y) => x - y).join(), a.indexOf(10), a.includes(3))
        console.log(a.map(x => x * 2).filter(x => x > 4).reduce((s, x) => s + x, 0))
        console.log([1, [2, [3, [4]]]].flat(Infinity).join(), [1, 2, 3].at(-1), Array.from({ length: 3 }, (_, i) => i * i).join())
        const s = '  Hello, World  '
        console.log(s.trim().toUpperCase(), s.trim().split(', ').length, 'abc'.padStart(5, '*'), 'x'.repeat(3))
        console.log('a-b-c'.replaceAll('-', '+'), 'hello'.slice(-3), 'hello'.substring(3, 1), 'hello'.charCodeAt(1), 'héllo'.length)
        console.log([3, 1, 2].reverse(), [1, 2, 3].splice(1, 1), 'Volant'.startsWith('Vol'))
    ";
    assert_eq!(out(src), "1,10,2,5 1,2,5,10 2 false\n30\n1,2,3,4 3 0,1,4\nHELLO, WORLD 2 **abc xxx\na+b+c llo el 101 5\n[ 2, 1, 3 ] [ 2 ] true");
}

#[test]
fn json_map_set_math() {
    let src = r#"
        const v = JSON.parse('{"a":[1,2,{"b":null}],"c":"dé","e":true}')
        console.log(v.a[2].b, v.c, v.e, JSON.stringify(v))
        console.log(JSON.stringify({ a: [1, { b: 2 }] }, null, 2))
        const m = new Map([['x', 1]])
        m.set('y', 2).set('x', 3)
        console.log(m.get('x'), m.size, [...m.keys()].join(), m.has('z'))
        const st = new Set([1, 2, 2, 3, NaN, NaN])
        console.log(st.size, st.has(NaN), [...st].length)
        console.log(Math.floor(3.7), Math.round(2.5), Math.round(-2.5), Math.abs(-4), Math.min(), (255).toString(16), (3.14159).toFixed(2))
        console.log(parseInt('42px'), parseInt('ff', 16), parseFloat('3.5e2abc'), Number(''), Number('x'), isNaN('x'))
        console.log(new Date(0).toISOString(), new Date('2026-11-12T06:30:00Z').getTime())
    "#;
    assert_eq!(
        out(src),
        "null dé true {\"a\":[1,2,{\"b\":null}],\"c\":\"dé\",\"e\":true}\n{\n  \"a\": [\n    1,\n    {\n      \"b\": 2\n    }\n  ]\n}\n3 2 x,y false\n4 true 4\n3 3 -2 4 Infinity ff 3.14\n42 255 350 0 NaN true\n1970-01-01T00:00:00.000Z 1794465000000"
    );
}

#[test]
fn exceptions() {
    let src = "
        function risky(n) { if (n > 2) throw new RangeError('too big: ' + n); return n }
        try { risky(1); risky(5) } catch (e) { console.log(e.name, e.message, e instanceof RangeError, e instanceof Error, String(e)) }
        finally { console.log('finally') }
        try { undefinedThing.x } catch ({ name }) { console.log(name) }
        try { null.x } catch (e) { console.log(e.name) }
        try { (void 0)() } catch (e) { console.log(e.name) }
        class MyError extends Error { constructor(m) { super(m); this.name = 'MyError' } }
        try { throw new MyError('custom') } catch (e) { console.log(e.name, e.message, e instanceof MyError) }
        function f() { try { return 'try' } finally { console.log('cleanup') } }
        console.log(f())
        try { throw 42 } catch (e) { console.log(e) }
    ";
    assert_eq!(out(src), "RangeError too big: 5 true true RangeError: too big: 5\nfinally\nReferenceError\nTypeError\nTypeError\nMyError custom true\ncleanup\ntry\n42");
    assert!(out("throw new TypeError('nope')").contains("TypeError: nope"));
    assert!(out("let = ;").starts_with("ERROR"));
}

#[test]
fn timers_and_limits() {
    assert_eq!(out("setTimeout(() => console.log('later'), 10); console.log('now')"), "now\nlater");
    let e = out("while (true) {}");
    assert!(e.starts_with("ERROR") && e.contains("too long"), "{e}");
    let e = out("function r() { return r() } r()");
    assert!(e.starts_with("ERROR") && e.contains("RangeError"), "{e}");
}

#[test]
fn a_real_program() {
    // A little of everything, as page scripts tend to do.
    let src = "
        const words = 'the quick brown fox jumps over the lazy dog the end'.split(' ')
        const freq = words.reduce((m, w) => (m[w] = (m[w] || 0) + 1, m), {})
        const top = Object.entries(freq).sort(([, a], [, b]) => b - a).slice(0, 2)
        console.log(top.map(([w, n]) => `${w}:${n}`).join(' '))
        const fib = n => n < 2 ? n : fib(n - 1) + fib(n - 2)
        console.log(fib(20))
        const queue = [{ id: 1, deps: [] }, { id: 2, deps: [1] }, { id: 3, deps: [1, 2] }]
        const done = new Set()
        while (done.size < queue.length) for (const t of queue) if (!done.has(t.id) && t.deps.every(d => done.has(d))) done.add(t.id)
        console.log([...done].join('>'))
    ";
    assert_eq!(out(src), "the:3 quick:1\n6765\n1>2>3");
}

#[test]
fn uri_and_partial_output() {
    assert_eq!(out("console.log(encodeURIComponent('a b&c/é'), decodeURIComponent('a%20b%26c%2F%C3%A9'), encodeURI('/a b?x=1'))"), "a%20b%26c%2F%C3%A9 a b&c/é /a%20b?x=1");
    let (printed, r) = run("console.log('before'); missing()");
    assert_eq!(printed, vec!["before".to_string()]);
    assert!(r.unwrap_err().contains("ReferenceError"));
}

#[test]
fn regular_expressions() {
    let src = r#"
        const re = /(\d{4})-(\d{2})-(\d{2})/
        const m = '2026-11-12'.match(re)
        console.log(m[0], m[1], m.index, re.test('no'), re.source, /a/gi.flags)
        console.log('a1b22c333'.match(/\d+/g).join('|'), 'x'.match(/y/), 'aaa'.search(/a$/))
        console.log('John Smith'.replace(/(\w+)\s(\w+)/, '$2, $1'), 'a.b.c'.replace(/\./g, '-'), 'abc'.replace(/b/, m => m.toUpperCase()))
        console.log('one, two;three'.split(/[,;]\s*/).join('|'), 'a1b2'.split(/(\d)/).join('|'), 'abc'.split(/(?:)/).length)
        const { groups: { year } } = /(?<year>\d{4})/.exec('in 1939 the')
        console.log(year, [...'a1b2c3'.matchAll(/[a-z](\d)/g)].map(x => x[1]).join(''))
        const g = /o/g; let n = 0; while (g.exec('foo boo')) n++
        console.log(n, g.lastIndex, new RegExp('h(i)', 'i').exec('HI')[1], String(/x\/y/))
        console.log('camelCaseString'.replace(/([A-Z])/g, ' $1').toLowerCase(), /^[\w.+-]+@\w+\.\w+$/.test('me@volant.app'))
        console.log('  trim  me '.replace(/^\s+|\s+$/g, ''), 'x'.replace('x', '$&$&'), 10 / 2 / 5)
    "#;
    assert_eq!(
        out(src),
        "2026-11-12 2026 0 false (\\d{4})-(\\d{2})-(\\d{2}) gi\n1|22|333 null 2\nSmith, John a-b-c aBc\none|two|three a|1|b|2| 3\n1939 123\n4 0 I /x\\/y/\ncamel case string true\ntrim  me xx 1"
    );
}

#[test]
fn promises_and_async() {
    let src = r#"
        const log = []
        Promise.resolve(1).then(v => log.push('then ' + v))
        new Promise((res, rej) => rej(new Error('bad'))).catch(e => log.push('caught ' + e.message)).finally(() => log.push('finally'))
        queueMicrotask(() => log.push('micro'))
        log.push('sync')
        setTimeout(() => { console.log(log.join(', ')) }, 0)
        const sleep = ms => new Promise(r => setTimeout(r, ms))
        async function work(x) { await sleep(10); return x * 2 }
        async function main() {
            const a = await work(2)
            const [b, c] = await Promise.all([work(3), work(4)])
            let err
            try { await Promise.reject(new TypeError('nope')) } catch (e) { err = e.name }
            const settled = await Promise.allSettled([work(1), Promise.reject(1)])
            const first = await Promise.race([sleep(50).then(() => 'slow'), sleep(5).then(() => 'fast')])
            console.log(a, b, c, err, settled.map(s => s.status).join('/'), first)
        }
        // `await` runs the event loop in place (see promise.rs), so main() finishes here.
        const arrow = async () => 7
        arrow().then(v => console.log('arrow', v))
        main().then(() => console.log('main done'))
        const order = []
        setTimeout(() => order.push(30), 30); setTimeout(() => order.push(10), 10); setTimeout(() => console.log(order.join()), 40)
        let ticks = 0; const iv = setInterval(() => { if (++ticks === 3) clearInterval(iv) }, 5)
        setTimeout(() => console.log('ticks', ticks), 100)
    "#;
    assert_eq!(out(src), "arrow 7\nsync, then 1, caught bad, micro, finally\n4 6 8 TypeError fulfilled/rejected fast\nmain done\n10,30\nticks 3");
}

#[test]
fn generators_iterators_symbols() {
    let src = r#"
        function* count(n) { for (let i = 0; i < n; i++) yield i; return 'end' }
        const g = count(3)
        console.log(g.next().value, g.next().value, g.next().value, JSON.stringify(g.next()), [...count(4)].join())
        function* naturals() { let i = 0; while (true) yield i++ }
        const it = naturals(); it.next(); console.log(it.next().value)
        function* inner() { yield 'a'; yield 'b' }
        function* outer() { yield* inner(); yield* [1, 2] }
        console.log([...outer()].join())
        class Range { constructor(a, b) { this.a = a; this.b = b } *[Symbol.iterator]() { for (let i = this.a; i <= this.b; i++) yield i } }
        console.log([...new Range(1, 5)].join(), typeof Symbol.iterator, typeof Symbol('x'))
        const custom = { [Symbol.iterator]() { let n = 0; return { next: () => ({ value: n, done: n++ >= 3 }) } } }
        for (const v of custom) console.log('v', v)
        const m = new Map([['a', 1], ['b', 2]])
        console.log(m.keys().next().value, [...m.values()].join(), Array.from(m.entries()).length, [1, 2].entries().next().value.join(':'))
        const s = Symbol('id'); const o = { [s]: 1, x: 2 }
        console.log(Object.keys(o).join(), o[s])
    "#;
    assert_eq!(out(src), "0 1 2 {\"value\":\"end\",\"done\":true} 0,1,2,3\n1\na,b,1,2\n1,2,3,4,5 symbol symbol\nv 0\nv 1\nv 2\na 1,2 2 0:1\nx 1");
}

#[test]
fn for_with_expression_init() {
    assert_eq!(out("let i, n; for (i = 5 > 3 ? (n = 1, 0) : 9; i < 3; i++) n += i; console.log(i, n)"), "3 4");
    assert_eq!(out("let s = 'a.b', i, x; for (i = s.indexOf('.'), i === -1 ? (x = 0) : (x = i); x < 3;) x++; console.log(x)"), "3");
}

#[test]
fn dates_and_static_blocks() {
    let src = "
        const d = new Date(2026, 10, 12, 9, 30)
        console.log(d.toISOString(), d.getDate(), d.getMonth(), d.getDay())
        d.setDate(d.getDate() + 30); console.log(d.toISOString())
        d.setMonth(13); d.setHours(25, 61); console.log(d.toISOString(), Date.UTC(2000, 0, 1))
        class C { static n = 1; static { C.n += 41 } }
        console.log(C.n)
    ";
    assert_eq!(out(src), "2026-11-12T09:30:00.000Z 12 10 4\n2026-12-12T09:30:00.000Z\n2027-02-13T02:01:00.000Z 946684800000\n42");
}

#[test]
fn nested_templates() {
    let src = r#"
        const a = 1, css = (s, ...v) => s.raw.join('|') + v.join(',')
        console.log(`x${ `in${a}ner` + '}' + "{" }y`, `${ { b: 2 }.b }`, css`p { color: ${'red'} } @media (x) { q {} }`)
    "#;
    assert_eq!(out(src), "xin1ner}{y 2 p { color: | } @media (x) { q {} }red");
}

/// Runs `files[entry]` as a module, with the other files importable by path.
fn modules(files: &[(&str, &str)], entry: &str) -> String {
    let files: Vec<(String, String)> = files.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let entry = entry.to_string();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let mut i = crate::Interp::new();
            let map: std::collections::HashMap<String, String> = files.into_iter().collect();
            let src = map[&entry].clone();
            i.loader = Some(std::rc::Rc::new(crate::module::MapLoader(map)));
            let r = i.run_module(&src, &entry);
            i.run_jobs(f64::INFINITY, 1000);
            let mut out = i.output.join("\n");
            if let Err(e) = r {
                out.push_str(&format!("\nERROR {e}"));
            }
            for e in &i.errors {
                out.push_str(&format!("\nERROR {e}"));
            }
            out
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn es_modules() {
    let files = [
        ("/app/main.js", "import def, { a, inc, count as c } from './lib.js';\nimport * as all from './lib.js';\nimport './side.js';\nexport { a };\nconsole.log(def(), a, c);\ninc(); inc();\nconsole.log(c, all.count, Object.keys(all).sort().join());\nconst m = await import('../app/lazy.js');\nconsole.log(m.lazy, import.meta.url, typeof all.default);\nimport('./missing.js').catch(e => console.log('caught', e.message.includes('missing')));"),
        ("/app/lib.js", "export const a = 'A';\nexport let count = 0;\nexport function inc() { count++ }\nexport default function () { return 'def' }\nconsole.log('lib runs once');"),
        ("/app/side.js", "import { a } from './lib.js';\nconsole.log('side sees', a);"),
        ("/app/lazy.js", "export * from './lib.js';\nexport { inc as plus } from './lib.js';\nexport const lazy = 'L';"),
    ];
    assert_eq!(
        modules(&files, "/app/main.js"),
        "lib runs once\nside sees A\ndef A 0\n2 2 a,count,default,inc\nL /app/main.js function\ncaught true"
    );
    // A cycle: each side can call the other's hoisted functions.
    let cycle = [
        ("/a.js", "import { b } from './b.js';\nexport function a() { return 'a' }\nconsole.log(b());"),
        ("/b.js", "import { a } from './a.js';\nexport function b() { return 'b' + a() }"),
    ];
    assert_eq!(modules(&cycle, "/a.js"), "ba");
    // Errors: a syntax error names the module; imports aren't allowed in scripts.
    assert!(modules(&[("/x.js", "import { q } from './y.js'")], "/x.js").contains("couldn't load module /y.js"));
    assert!(out("import x from 'y'").contains("only appear at the top of a module"));
    assert_eq!(out("const o = { import: 1, export: 2 }; console.log(o.import + o.export)"), "3");
}

#[test]
fn templates_hold_any_expression() {
    // A quote inside a regex inside ${} (Vue's decodeEntities).
    assert_eq!(out(r#"const e = 'a"b'; console.log(`<div foo="${e.replace(/"/g, "&quot;")}">`)"#), r#"<div foo="a&quot;b">"#);
    assert_eq!(out("const o = {a: {b: 2}}; console.log(`${ {x: 1}.x }-${`${o.a.b}`}-${'}'}`)"), "1-2-}");
}

#[test]
fn with_statements() {
    // How Vue's runtime-compiled templates read their component's state.
    let src = "const ctx = {msg: 'hi', n: 1}; let outer = 'o';
        const render = new Function('_ctx', 'with (_ctx) { n = n + 1; return msg + n + outer }');
        globalThis.outer = 'g';
        console.log(render(ctx), ctx.n)";
    assert_eq!(out(src), "hi2o 2"); // a global `let` shadows the global object
}

#[test]
fn proxies_and_reflect() {
    let src = "const log = [];
        const target = {a: 1, list: [1, 2]};
        const p = new Proxy(target, {
            get(t, k, r) { if (typeof k === 'string') log.push('get ' + k); return Reflect.get(t, k, r) },
            set(t, k, v) { log.push('set ' + k); t[k] = v * 10; return true },
            has(t, k) { return k !== 'hidden' },
            deleteProperty(t, k) { log.push('del ' + k); return delete t[k] },
            ownKeys(t) { return ['a', 'b'] },
        });
        p.b = 2;
        console.log(p.a, p.b, 'zz' in p, 'hidden' in p, Object.keys(p).join());
        delete p.a;
        console.log(target.a, log.join('|'));
        const arr = new Proxy([1, 2, 3], {});
        arr.push(4);
        console.log(Array.isArray(arr), arr.length, Object.prototype.toString.call(arr), typeof new Proxy(function () {}, {}));
        const f = new Proxy(function (x) { return x + 1 }, { apply(t, self, args) { return t(...args) * 2 } });
        const C = new Proxy(class { constructor(v) { this.v = v } }, { construct(t, args) { return new t(args[0] + '!') } });
        console.log(f(1), new C('hi').v, Reflect.has({q: 1}, 'q'), Reflect.ownKeys({x: 1, y: 2}).join());";
    assert_eq!(out(src), "1 20 true false a,b\nundefined set b|get a|get b|del a\ntrue 4 [object Array] function\n4 hi! true x,y");
}

#[test]
fn evaluation_order() {
    // The target object is evaluated before the value (Vue: `(p = {...}).ctx = {_: p}`).
    assert_eq!(out("let p; (p = {n: 1}).ctx = {_: p}; console.log(p.ctx._ === p)"), "true");
    assert_eq!(out("const a = [0, 0, 0]; let i = 0; a[i++]++; a[i++] += 5; console.log(a.join(), i)"), "1,5,0 2");
    assert_eq!(out("const o = {}; let n = 0; o[n++] ||= 'x'; o.y ??= n; console.log(o[0], o.y, n)"), "x 1 1");
}

#[test]
fn define_property() {
    // jQuery's addProp: a setter on the prototype that defines an own property (no recursion).
    let src = "function E() {} 
        Object.defineProperty(E.prototype, 'which', { enumerable: true, configurable: true,
            get() { return 'proto' },
            set(v) { Object.defineProperty(this, 'which', { enumerable: true, configurable: true, writable: true, value: v }) } });
        const e = new E(); e.which = 5;
        const o = { a: 1 }; Object.defineProperty(o, 'hidden', { value: 2 }); Object.defineProperty(o, 'a', { enumerable: false });
        class K { m() {} get g() { return 1 } }
        const keys = []; for (const k in new K()) keys.push(k);
        console.log(e.which, new E().which, Object.keys(o).length, o.a, o.hidden, Object.getOwnPropertyNames(o).join(), keys.length,
            Object.getOwnPropertyDescriptor(o, 'hidden').enumerable, o.propertyIsEnumerable('a'))";
    assert_eq!(out(src), "5 proto 0 1 2 a,hidden 0 false false");
}

#[test]
fn bytecode_vm_semantics() {
    // Generators receive values through next(v), and return/throw resume them.
    let src = "function* g() { const a = yield 1; try { yield a * 2 } finally { console.log('fin') } }
        const it = g(); const r = [it.next().value, it.next(21).value, it.return(9).value, it.next().done];
        console.log(r.join());";
    assert_eq!(out(src), "fin\n1,42,9,true");
    // Closures in loops capture a fresh binding per iteration.
    assert_eq!(out("const fs = []; for (let i = 0; i < 3; i++) fs.push(() => i); console.log(fs.map(f => f()).join())"), "0,1,2");
    // Deep (non-native) recursion doesn't overflow the Rust stack.
    assert_eq!(out("function d(n) { return n ? 1 + d(n - 1) : 0 } console.log(d(5000))"), "5000");
    // Runaway recursion is a catchable RangeError.
    assert_eq!(out("function f() { return f() } try { f() } catch (e) { console.log(e instanceof RangeError) }"), "true");
    // Sloppy-mode `this` is the global object; strict mode keeps undefined.
    assert_eq!(out("function s() { return this === globalThis } function t() { 'use strict'; return this } console.log(s(), t())"), "true undefined");
    // A method's name isn't bound inside it (only a named function expression's is).
    assert_eq!(out("const send = () => 'outer'; class X { send() { return send() } } console.log(new X().send(), ({ send() { return send() } }).send(), ({ m() {} }).m.name, ({ k: function () {} }).k.name)"), "outer outer m k");
    // typeof on an undeclared global doesn't throw.
    assert_eq!(out("console.log(typeof nope, typeof nope === 'undefined')"), "undefined true");
    // Lazy function properties behave like eager ones.
    assert_eq!(out("function foo(a, b) {} foo.prototype.x = 1; console.log(foo.name, foo.length, new foo().x, Object.keys(foo).length)"), "foo 2 1 0");
}

#[test]
fn async_ordering() {
    let src = "const log = [];
        async function a() { log.push('a1'); await null; log.push('a2'); await null; log.push('a3') }
        Promise.resolve().then(() => log.push('p1')).then(() => log.push('p2'));
        a(); log.push('sync');
        setTimeout(() => console.log(log.join()), 0);";
    assert_eq!(out(src), "a1,sync,p1,a2,p2,a3");
}

/// Scripts found on real sites by the compatibility survey (scripts/compat).
#[test]
fn syntax_from_real_sites() {
    // A dot after a number's own dot is a property access.
    assert_eq!(out("console.log(1..toString(), 1.5.toFixed(2), 0..constructor === Number, .5 + 1.e2)"), "1 1.50 true 100.5");
    // Names with combining marks and other letters.
    assert_eq!(out("var \u{2118} = 2, nukta\u{93c} = 3, br\u{115}ve = 4; console.log(\u{2118} + nukta\u{93c} + br\u{115}ve)"), "9");
    // Old-style HTML comments around a script.
    assert_eq!(out("<!-- hide\nconsole.log('in')\n--> shown"), "in");
    // A regular expression after `for (…)`, `if (…)` and a function's `}`; division elsewhere.
    let src = "
        var n = 0, a = [' -: ', 'x'];
        for (var i = 0; i < a.length; i++) /^ *-+: *$/.test(a[i]) ? n++ : n;
        function yc(e) { return 1 }/[1-9]/.test('5') && n++;
        if (n) /x/.test('x') && n++;
        var o = { a: 4 }, h = 2;
        console.log(n, o.a / h / 1, (8) / 2, [6][0] / 3, {}.x);
    ";
    assert_eq!(out(src), "3 2 4 2 undefined");
    // `for (yield …;` in a generator, and assigning through super.
    assert_eq!(out("function* g() { for (yield {s: 1}; false;) {} } console.log(g().next().value.s)"), "1");
    assert_eq!(out("class A {} class B extends A { set(v) { super.speed = v; return this.speed } } console.log(new B().set(5))"), "5");
    // A repeated lookahead.
    assert_eq!(out("console.log(/((?=lg)?[vl]k-?\\d{3}) bui/i.exec('LK-123 build')[1])"), "LK-123");
}

#[test]
fn typed_arrays() {
    // Views over one buffer share its bytes, little-endian.
    let src = "
        var b = new ArrayBuffer(8), u8 = new Uint8Array(b), u32 = new Uint32Array(b);
        u32[0] = 0x01020304;
        console.log(u8[0], u8[3], u8.length, u32.length, b.byteLength, u8.buffer === b);
        var s = u8.subarray(2, 6); s[0] = 255;
        console.log(s.length, s.byteOffset, u8[2], Array.from(s).join(','));
        var c = new Uint8ClampedArray([300, -5, 1.5, 2.5]), i8 = new Int8Array([200, -129]);
        console.log(c.join(' '), i8.join(' '), new Float32Array([0.5])[0]);
        var dv = new DataView(b); dv.setUint16(0, 0x1234); dv.setUint16(2, 0x1234, true);
        console.log(u8[0].toString(16), u8[2].toString(16), dv.getUint16(0), dv.getInt8(1));
        var m = new Uint8Array([3, 1, 2]).map(x => x * 2);
        console.log(m instanceof Uint8Array, m.join(), new Int16Array([10, -2, 1]).sort().join(), [...new Uint8Array([7, 8])].join());
        console.log(Object.prototype.toString.call(u8), ArrayBuffer.isView(u8), ArrayBuffer.isView([]), Array.isArray(u8), u8[99], Uint8Array.BYTES_PER_ELEMENT);
        var t = new Uint8Array(4); t.set([9, 9], 1); console.log(t.join(), t.slice(1, 3).join(), t.indexOf(9), t.includes(0), Object.keys(t).length);
    ";
    assert_eq!(
        out(src),
        "4 1 8 2 8 true\n4 2 255 255,1,0,0\n255 0 2 2 -56 127 0.5\n12 34 4660 52\ntrue 6,2,4 -2,1,10 7,8\n[object Uint8Array] true false false undefined 1\n0,9,9,0 9,9 1 true 4"
    );
}

/// What polyfills (core-js and others) test before deciding to replace built-ins.
#[test]
fn feature_detection() {
    let src = "
        var P = Object.create({}, { next: { value: function () { return 1 }, writable: true, configurable: true } });
        var o = Object.defineProperties({}, { a: { value: 1, enumerable: true }, b: { get: function () { return 2 } } });
        console.log(P.next(), Object.getOwnPropertyNames(P).join(), o.a, o.b, Object.keys(o).join());
        var s = Symbol('x'), q = {}; q[s] = 1;
        console.log(String(s), s.description, Object(s) instanceof Symbol, Object.getOwnPropertySymbols(q).length, Symbol.keyFor(Symbol.for('k')));
        var ai = [][Symbol.iterator](), IP = Object.getPrototypeOf(Object.getPrototypeOf(ai));
        console.log(IP !== Object.prototype, typeof IP[Symbol.iterator], Object.getPrototypeOf(new Set()[Symbol.iterator]()).hasOwnProperty('next'));
    ";
    assert_eq!(out(src), "1 next 1 2 a\nSymbol(x) x true 1 k\ntrue function true");
}

#[test]
fn spec_details_from_web_platform_tests() {
    // new.target, Error subclasses inheriting from Error as constructors, and lone-surrogate escapes in classes.
    assert_eq!(out("function F() { return new.target === F } function G() { this.t = new.target === G } console.log(F(), new G().t)"), "false true");
    assert_eq!(out("console.log(Object.getPrototypeOf(TypeError) === Error, new Error('m', { cause: 7 }).cause)"), "true 7");
    assert_eq!(out(r"console.log(/[\udc00-\udfff]/.test('u'), 'function'.replace(/(^|[^\ud800-\udbff])([\udc00-\udfff]+)/g, 'X'))"), "false function");
}

#[test]
fn reflect_construct_new_target() {
    let src = "function Base() { this.x = this.init ? this.init() : 'none'; this.t = new.target === Sub }
        function Sub() {}
        Sub.prototype = Object.create(Base.prototype, { constructor: { value: Sub } })
        Sub.prototype.init = function () { return 'sub' }
        const o = Reflect.construct(Base, [], Sub)
        class A { constructor() { this.n = new.target.name } } class B {}
        class C extends A {}
        console.log(o.x, o.t, Object.getPrototypeOf(o) === Sub.prototype, Reflect.construct(A, [], B).n, new C().n, Reflect.construct(Error, ['m'], B) instanceof B)";
    assert_eq!(out(src), "sub true true B C true");
}

#[test]
fn for_await() {
    let src = r#"
        const o = { [Symbol.asyncIterator]() { let i = 0; return { next() { return Promise.resolve({ value: i, done: i++ > 1 }) } } } };
        async function* g() { yield 1; yield 2; yield 3 }
        (async () => {
            const seen = [];
            for await (const v of o) seen.push('a' + v);
            for await (const v of [1, Promise.resolve(2)]) seen.push('s' + v);
            for await (const v of g()) { if (v == 2) break; seen.push('g' + v) }
            let x; for await (x of g()) {}
            seen.push('x' + x);
            console.log(seen.join(' '));
        })()
    "#;
    assert_eq!(out(src), "a0 a1 s1 s2 g1 x3");
}

#[test]
fn array_search_from_index() {
    assert_eq!(out("var a=[0,10,2,10,4,10]; console.log(a.indexOf(10,2), a.indexOf(10,-1), a.indexOf(10,9), a.lastIndexOf(10,4), a.lastIndexOf(10,-2), a.includes(0,1), a.includes(10,-1))"), "3 5 -1 3 3 false true");
    assert_eq!(out("var d=new Uint8Array([0,10,2,10]); console.log(d.indexOf(10,2), d.lastIndexOf(10,2), d.includes(0,1))"), "3 1 false");
    assert_eq!(out("var a=[1,2,3,4,5]; console.log(a.findLast(x=>x<3), a.findLastIndex(x=>x>9), a.toSpliced(1,2,'x').join(), a.with(-1,9).join(), [3,1,2].toSorted().join(), a.toReversed().join(), [1,2,3,4,5].copyWithin(0,3).join(), a.join())"), "2 -1 1,x,4,5 1,2,3,4,9 1,2,3 5,4,3,2,1 4,5,3,4,5 1,2,3,4,5");
    assert_eq!(out("console.log(JSON.stringify(Object.groupBy([1,2,3],x=>x%2?'o':'e')), Map.groupBy([1,2,3],x=>x>1).get(true).join())"), "{\"o\":[1,3],\"e\":[2]} 2,3");
}

#[test]
fn private_names_are_hidden() {
    let src = r#"
        class A { #x = 1; static #n = 0; #m() { return this.#x } constructor() { A.#n++ } has(o) { return #x in o } get() { return this.#m() + A.#n } }
        const a = new A();
        console.log(a.get(), a.has(a), a.has({}), JSON.stringify(a), Object.keys(a).length, Reflect.ownKeys(a).length);
        const o = { '#x': 1 }; console.log(Object.keys(o)[0], o['#x']);
    "#;
    assert_eq!(out(src), "2 true false {} 0 0\n#x 1");
}

#[test]
fn function_source_text() {
    let src = r#"
        function f(a, b) { return a + b }
        const g = async (x) => x * 2;
        const o = { m(z) { return z }, get p() { return 1 } };
        class C extends Object { m() {} }
        console.log(f.toString(), '|', g.toString(), '|', o.m.toString(), '|', String(Object.getOwnPropertyDescriptor(o, 'p').get), '|', C.toString(), '|', Math.max.toString());
    "#;
    assert_eq!(out(src), "function f(a, b) { return a + b } | async (x) => x * 2 | m(z) { return z } | get p() { return 1 } | class C extends Object { m() {} } | function max() { [native code] }");
}

#[test]
fn json_reviver_and_numbers() {
    assert_eq!(out(r#"console.log(JSON.stringify(JSON.parse('{"a":[1,{"b":2}],"c":3}', (k, v) => typeof v == 'number' ? v * 2 : v)))"#), r#"{"a":[2,{"b":4}],"c":6}"#);
    assert_eq!(out("console.log((2.5).toFixed(0), (1.005).toFixed(2), (-2.5).toFixed(0), (1234.5).toFixed(0), (0.5).toString(2), (255.5).toString(16), Math.imul(0xffffffff, 5), Math.clz32(1), Math.atanh(0))"), "3 1.00 -3 1235 0.1 ff.8 -5 31 0");
    assert_eq!(out("var o={[Symbol.toPrimitive](h){return h=='number'?42:'s'}}; console.log(+o, `${o}`, String.raw`a\\n${1}`, String.fromCodePoint(0x1F600).length)"), "42 s a\\n1 2");
}

#[test]
fn date_parsing_and_text() {
    let src = r#"
        console.log(Date.parse('Thu, 01 Jan 1970 00:00:00 GMT'), Date.parse('2020-01-02T03:04:05.123Z'), Date.parse('2020-01-02T03:04:05+05:30'), Date.parse('Sat, 13 Mar 2010 11:29:05 -0800'), Date.parse('garbage'));
        console.log(new Date(Date.UTC(2020, 5, 15, 10, 20, 30)).toUTCString(), new Date(-1e14).toISOString(), new Date(8.64e15 + 1).getTime());
    "#;
    assert_eq!(out(src), "0 1577934245123 1577914445000 1268508545000 NaN\nMon, 15 Jun 2020 10:20:30 GMT -001199-02-15T14:13:20.000Z NaN");
}

#[test]
fn structured_clone_keeps_shapes() {
    let src = r#"
        const a = { d: new Date(5), m: new Map([[1, { y: 2 }]]), s: new Set([1]), t: new Uint8Array([1, 2]), e: new RangeError('bad') };
        a.self = a;
        const c = structuredClone(a);
        console.log(c.self === c, c.d.getTime(), c.m.get(1).y, c.m.get(1) !== a.m.get(1), c.s.has(1), c.t.join(), c.e instanceof RangeError, c.e.message);
        try { structuredClone({ f() {} }) } catch (e) { console.log(e.name) }
    "#;
    assert_eq!(out(src), "true 5 2 true true 1,2 true bad\nDataCloneError");
}

#[test]
fn regex_indices_and_normalize() {
    assert_eq!(out("const m = /(?<n>b+)/d.exec('abbc'); console.log(m.indices[0].join(), m.indices.groups.n.join(), /x/v.unicodeSets)"), "1,3 1,3 true");
    assert_eq!(out("console.log('Crème'.normalize('NFD').length, 'Cre\\u0300me'.normalize() === 'Crème', 'Crème brûlée'.normalize('NFD').replace(/[\\u0300-\\u036f]/g, ''))"), "6 true Creme brulee");
}

#[test]
fn super_property_receivers() {
    let src = r#"
        class A { constructor() { this._s = 1 } get speed() { return this._s * 10 } set speed(v) { this._s = v } m() { return 'A' + this._s } static sm() { return 'As' } }
        class B extends A { get speed() { return super.speed + 1 } set speed(v) { super.speed = v * 2 } m() { return 'B' + super.m() } static sm() { return 'B' + super.sm() } }
        const b = new B(); console.log(b.speed); b.speed = 3; console.log(b.speed, b._s, b.m(), B.sm());
        const o = { __proto__: { hi() { return 'p' + this.x } }, x: 1, hi() { return 'o' + super.hi() } }; console.log(o.hi());
        class C extends A { f() { super.plain = 5; return [this.plain, Object.hasOwn(this, 'plain')].join() } } console.log(new C().f());
    "#;
    assert_eq!(out(src), "11\n61 6 BA6 BAs\nop1\n5,true");
}

#[test]
fn bigint_values() {
    let src = r#"
        console.log(typeof 1n, 2n ** 64n, -7n / 2n, -7n % 2n, 0x1fn, -5n & 3n, ~5n, -5n >> 1n, 1n << 70n);
        console.log(1n == 1, 1n === 1, 10n > '9', [3n, 1n, 2n].sort().join(), new Map([[1n, 'a']]).get(1n), Object.is(0n, -0n));
        let x = 5n; x++; x += 10n; let s = 0n; for (let i = 0n; i < 4n; i++) s += i;
        console.log(x, s, BigInt('0x10'), BigInt.asIntN(8, 255n), BigInt.asUintN(64, -1n), Number(2n ** 53n + 1n), 255n.toString(16));
        const a = new BigInt64Array([1n, -2n]); console.log(a[1], typeof a[0], new DataView(new ArrayBuffer(8)).getBigUint64(0));
        for (const f of [() => 1n + 1, () => 1n >>> 1n, () => BigInt(1.5), () => 1n / 0n, () => JSON.stringify(1n)]) try { f() } catch (e) { console.log(e.name) }
        class K { constructor() {} } console.log(K.name, new K().constructor.name);
    "#;
    assert_eq!(
        out(src),
        "bigint 18446744073709551616 -3 -1 31 3 -6 -3 1180591620717411303424\n\
         true false true 1,2,3 a true\n\
         16 6 16 -1 18446744073709551615 9007199254740992 ff\n\
         -2 bigint 0\n\
         TypeError\nTypeError\nRangeError\nRangeError\nTypeError\n\
         K K"
    );
}
