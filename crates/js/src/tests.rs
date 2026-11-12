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
