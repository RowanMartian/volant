// Volant's web platform, written in JavaScript on top of the engine's native
// hooks (__fetch, __media, __resolve, __navigate, __setURL, __url, __width,
// __dark, __computed, __makeEvent). Runs once in every page before its scripts.
(function (g) {
  'use strict'
  const noop = () => {}
  const define = (o, k, get, set) => Object.defineProperty(o, k, { get, set: set || noop })

  // ---------------------------------------------------------------- events
  const EventProto = Object.getPrototypeOf(__makeEvent('x'))
  const eventClass = (name, defaults) => {
    function E(type, init) {
      const e = __makeEvent(String(type), init)
      if (defaults) for (const k in defaults) if (!(init && k in init)) e[k] = defaults[k]
      return e
    }
    E.prototype = EventProto
    Object.defineProperty(E, 'name', { value: name })
    g[name] = E
  }
  eventClass('Event')
  eventClass('CustomEvent', { detail: null })
  for (const n of ['UIEvent', 'FocusEvent', 'InputEvent', 'SubmitEvent', 'ErrorEvent', 'ProgressEvent', 'MessageEvent', 'PopStateEvent',
    'HashChangeEvent', 'AnimationEvent', 'TransitionEvent', 'StorageEvent', 'PageTransitionEvent', 'WheelEvent', 'TouchEvent', 'DragEvent', 'ClipboardEvent']) eventClass(n)
  eventClass('MouseEvent', { button: 0, buttons: 0, clientX: 0, clientY: 0, pageX: 0, pageY: 0, screenX: 0, screenY: 0, altKey: false, ctrlKey: false, metaKey: false, shiftKey: false })
  eventClass('PointerEvent', { pointerId: 1, pointerType: 'mouse', button: 0, clientX: 0, clientY: 0 })
  eventClass('KeyboardEvent', { key: '', code: '', keyCode: 0, altKey: false, ctrlKey: false, metaKey: false, shiftKey: false, repeat: false })
  EventProto.initEvent = function (type, bubbles, cancelable) { this.type = type; this.bubbles = !!bubbles; this.cancelable = !!cancelable }
  EventProto.initCustomEvent = function (type, bubbles, cancelable, detail) { this.initEvent(type, bubbles, cancelable); this.detail = detail }

  class EventTarget {
    addEventListener(type, f, opts) {
      if (!f) return
      const l = this.__listeners || (this.__listeners = {})
      ;(l[type] || (l[type] = [])).push({ f, once: !!(opts && opts.once) })
    }
    removeEventListener(type, f) {
      const l = this.__listeners && this.__listeners[type]
      if (l) this.__listeners[type] = l.filter(x => x.f !== f)
    }
    dispatchEvent(e) {
      try { e.target = this; e.currentTarget = this } catch (_) {}
      const h = this['on' + e.type]
      if (typeof h === 'function') h.call(this, e)
      const l = (this.__listeners && this.__listeners[e.type]) || []
      for (const x of l.slice()) {
        if (x.once) this.removeEventListener(e.type, x.f)
        if (typeof x.f === 'function') x.f.call(this, e)
        else if (x.f && x.f.handleEvent) x.f.handleEvent(e)
      }
      return !e.defaultPrevented
    }
  }
  g.EventTarget = EventTarget

  class AbortSignal extends EventTarget {
    constructor() { super(); this.aborted = false; this.reason = undefined; this.onabort = null }
    throwIfAborted() { if (this.aborted) throw this.reason }
    static abort(reason) { const c = new AbortController(); c.abort(reason); return c.signal }
    static timeout(ms) { const c = new AbortController(); setTimeout(() => c.abort(new Error('TimeoutError')), ms); return c.signal }
    static any(list) { const c = new AbortController(); for (const s of list) s.addEventListener('abort', () => c.abort(s.reason)); return c.signal }
  }
  class AbortController {
    constructor() { this.signal = new AbortSignal() }
    abort(reason) {
      if (this.signal.aborted) return
      this.signal.aborted = true
      this.signal.reason = reason === undefined ? new Error('AbortError') : reason
      this.signal.dispatchEvent(new Event('abort'))
    }
  }
  g.AbortSignal = AbortSignal
  g.AbortController = AbortController

  // ---------------------------------------------------------------- URLs
  const enc = s => encodeURIComponent(s).replace(/%20/g, '+').replace(/[!'()~]/g, c => '%' + c.charCodeAt(0).toString(16).toUpperCase())
  const dec = s => { try { return decodeURIComponent(s.replace(/\+/g, ' ')) } catch (_) { return s } }
  class URLSearchParams {
    constructor(init) {
      this._p = []
      this._owner = null
      if (init == null) return
      if (typeof init === 'string') {
        for (const part of init.replace(/^\?/, '').split('&')) {
          if (!part) continue
          const i = part.indexOf('=')
          this._p.push(i < 0 ? [dec(part), ''] : [dec(part.slice(0, i)), dec(part.slice(i + 1))])
        }
      } else if (init instanceof URLSearchParams) {
        this._p = init._p.map(x => x.slice())
      } else if (Array.isArray(init) || typeof init[Symbol.iterator] === 'function') {
        for (const [k, v] of init) this._p.push([String(k), String(v)])
      } else {
        for (const k of Object.keys(init)) this._p.push([k, String(init[k])])
      }
    }
    _changed() { if (this._owner) this._owner._setSearch(this.toString()) }
    get size() { return this._p.length }
    append(k, v) { this._p.push([String(k), String(v)]); this._changed() }
    delete(k) { this._p = this._p.filter(x => x[0] !== k); this._changed() }
    get(k) { const f = this._p.find(x => x[0] === k); return f ? f[1] : null }
    getAll(k) { return this._p.filter(x => x[0] === k).map(x => x[1]) }
    has(k) { return this._p.some(x => x[0] === k) }
    set(k, v) {
      const i = this._p.findIndex(x => x[0] === k)
      if (i < 0) this._p.push([String(k), String(v)])
      else { this._p[i][1] = String(v); this._p = this._p.filter((x, j) => j <= i || x[0] !== k) }
      this._changed()
    }
    sort() { this._p.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0)); this._changed() }
    forEach(f, t) { for (const [k, v] of this._p) f.call(t, v, k, this) }
    keys() { return this._p.map(x => x[0]).values() }
    values() { return this._p.map(x => x[1]).values() }
    entries() { return this._p.map(x => [x[0], x[1]]).values() }
    [Symbol.iterator]() { return this.entries() }
    toString() { return this._p.map(([k, v]) => enc(k) + '=' + enc(v)).join('&') }
  }
  g.URLSearchParams = URLSearchParams

  const URL_RE = /^([a-zA-Z][a-zA-Z0-9+.-]*:)(?:\/\/(?:([^@/?#]*)@)?([^/?#:]*|\[[^\]]*\])(?::(\d*))?)?([^?#]*)(\?[^#]*)?(#.*)?$/
  class URL {
    constructor(url, base) {
      url = String(url)
      if (base === undefined && !/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(url)) throw new TypeError('Invalid URL: ' + url)
      this._set(__resolve(url, base === undefined ? undefined : String(base)))
    }
    _set(href) {
      const m = URL_RE.exec(href)
      if (!m) throw new TypeError('Invalid URL: ' + href)
      this.protocol = m[1].toLowerCase()
      const auth = m[2] || ''
      this.username = auth.split(':')[0] || ''
      this.password = auth.split(':')[1] || ''
      this.hostname = (m[3] || '').toLowerCase()
      const defaultPort = { 'http:': '80', 'https:': '443' }[this.protocol]
      this.port = m[4] && m[4] !== defaultPort ? m[4] : ''
      this.pathname = m[5] || (m[3] !== undefined ? '/' : '')
      this._search = m[6] && m[6] !== '?' ? m[6] : ''
      this.hash = m[7] && m[7] !== '#' ? m[7] : ''
      this._params = new URLSearchParams(this._search)
      this._params._owner = this
    }
    _setSearch(s) { this._search = s ? '?' + s : '' }
    get search() { return this._search }
    set search(s) { s = String(s); this._search = s && s !== '?' ? (s[0] === '?' ? s : '?' + s) : ''; this._params = new URLSearchParams(this._search); this._params._owner = this }
    get searchParams() { return this._params }
    get host() { return this.hostname + (this.port ? ':' + this.port : '') }
    set host(h) { const [n, p] = String(h).split(':'); this.hostname = n; this.port = p || '' }
    get origin() { return this.hostname ? this.protocol + '//' + this.host : 'null' }
    get href() {
      const auth = this.username ? this.username + (this.password ? ':' + this.password : '') + '@' : ''
      const slashes = this.hostname || this.protocol === 'file:' ? '//' : ''
      return this.protocol + slashes + auth + this.host + this.pathname + this._search + this.hash
    }
    set href(h) { this._set(__resolve(String(h))) }
    toString() { return this.href }
    toJSON() { return this.href }
    static canParse(u, b) { try { new URL(u, b); return true } catch (_) { return false } }
    static parse(u, b) { try { return new URL(u, b) } catch (_) { return null } }
    static createObjectURL() { return 'blob:' + __url() + '#' + Math.random().toString(36).slice(2) }
    static revokeObjectURL() {}
  }
  g.URL = URL
  g.webkitURL = URL

  // ---------------------------------------------------------------- location, history
  const location = {}
  for (const k of ['protocol', 'host', 'hostname', 'port', 'pathname', 'search', 'hash', 'origin']) {
    define(location, k, () => new URL(__url())[k], v => { const u = new URL(__url()); u[k] = v; if (k === 'hash') __setURL(u.href); else __navigate(u.href) })
  }
  define(location, 'href', () => __url(), v => __navigate(String(v)))
  location.assign = u => __navigate(String(u))
  location.replace = u => __navigate(String(u))
  location.reload = () => __navigate(__url())
  location.toString = () => __url()
  location.ancestorOrigins = []
  g.location = location
  define(document, 'location', () => location, v => __navigate(String(v)))
  const history = {
    length: 1, state: null, scrollRestoration: 'auto',
    pushState(state, _t, url) { this.state = state; if (url != null) __setURL(String(url)); this.length++ },
    replaceState(state, _t, url) { this.state = state; if (url != null) __setURL(String(url)) },
    back: noop, forward: noop, go: noop,
  }
  g.history = history

  // ---------------------------------------------------------------- window
  g.navigator = {
    userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15 Volant/1.0',
    appName: 'Netscape', appVersion: '5.0 (Macintosh)', product: 'Gecko', vendor: 'Apple Computer, Inc.', platform: 'MacIntel',
    language: 'en-US', languages: ['en-US', 'en'], onLine: true, cookieEnabled: true, hardwareConcurrency: 8, maxTouchPoints: 0,
    deviceMemory: 8, webdriver: false, doNotTrack: '1', pdfViewerEnabled: false,
    clipboard: { writeText: () => Promise.resolve(), readText: () => Promise.resolve('') },
    permissions: { query: () => Promise.resolve({ state: 'prompt', onchange: null }) },
    sendBeacon: () => true, vibrate: () => false, share: () => Promise.reject(new Error('NotAllowedError')),
    javaEnabled: () => false,
  }
  g.screen = { width: 1512, height: 982, availWidth: 1512, availHeight: 950, colorDepth: 24, pixelDepth: 24, orientation: { type: 'landscape-primary', angle: 0, addEventListener: noop } }
  for (const k of ['innerWidth', 'outerWidth']) define(g, k, () => __width())
  define(g, 'innerHeight', () => 900)
  define(g, 'outerHeight', () => 1000)
  g.devicePixelRatio = 2
  for (const k of ['scrollX', 'scrollY', 'pageXOffset', 'pageYOffset', 'screenX', 'screenY', 'screenLeft', 'screenTop']) g[k] = 0
  g.scrollTo = g.scrollBy = g.scroll = noop
  g.alert = noop
  g.print = noop
  g.confirm = () => false
  g.prompt = () => null
  g.open = () => null
  g.focus = g.blur = g.close = g.stop = noop
  g.top = g.parent = g.frames = g
  g.opener = null
  g.frameElement = null
  g.name = ''
  g.length = 0
  g.closed = false
  g.isSecureContext = true
  g.crossOriginIsolated = false
  define(g, 'origin', () => location.origin)
  g.postMessage = (data, origin) => setTimeout(() => {
    const e = new Event('message'); e.data = data; e.origin = location.origin; e.source = g
    g.dispatchEvent(e)
  }, 0)
  g.matchMedia = q => ({
    media: String(q), matches: __media(String(q)), onchange: null,
    addListener: noop, removeListener: noop, addEventListener: noop, removeEventListener: noop, dispatchEvent: () => true,
  })
  const computedProps = ['display', 'visibility', 'opacity', 'color', 'backgroundColor', 'fontSize', 'fontWeight', 'lineHeight', 'position', 'width', 'height', 'overflow']
  g.getComputedStyle = el => {
    const s = { getPropertyValue: p => __computed(el, p) || (el && el.style ? el.style.getPropertyValue(p) : '') }
    for (const k of computedProps) define(s, k, () => __computed(el, k))
    for (const k of ['transform', 'zIndex', 'marginTop', 'marginBottom', 'paddingTop', 'paddingBottom', 'borderTopWidth', 'transitionDuration', 'animationName', 'pointerEvents', 'cursor', 'direction', 'boxSizing', 'content'])
      define(s, k, () => (el && el.style && el.style[k]) || (k === 'transform' || k === 'animationName' ? 'none' : k === 'transitionDuration' ? '0s' : k === 'direction' ? 'ltr' : ''))
    return s
  }

  class Storage {
    constructor() { Object.defineProperty(this, '_d', { value: new Map() }) }
    get length() { return this._d.size }
    key(i) { return [...this._d.keys()][i] ?? null }
    getItem(k) { return this._d.has(String(k)) ? this._d.get(String(k)) : null }
    setItem(k, v) { this._d.set(String(k), String(v)) }
    removeItem(k) { this._d.delete(String(k)) }
    clear() { this._d.clear() }
  }
  g.Storage = Storage
  g.localStorage = new Storage()
  g.sessionStorage = new Storage()

  const t0 = Date.now()
  g.performance = {
    timeOrigin: t0, now: () => Date.now() - t0,
    mark: noop, measure: noop, clearMarks: noop, clearMeasures: noop,
    getEntries: () => [], getEntriesByType: () => [], getEntriesByName: () => [],
    timing: { navigationStart: t0 }, navigation: { type: 0 }, memory: undefined,
  }

  // ---------------------------------------------------------------- network
  class Headers {
    constructor(init) {
      this._h = new Map()
      if (init) {
        if (init instanceof Headers) init.forEach((v, k) => this.set(k, v))
        else if (Array.isArray(init)) for (const [k, v] of init) this.append(k, v)
        else for (const k of Object.keys(init)) this.set(k, init[k])
      }
    }
    get(k) { return this._h.has(String(k).toLowerCase()) ? this._h.get(String(k).toLowerCase()) : null }
    set(k, v) { this._h.set(String(k).toLowerCase(), String(v)) }
    append(k, v) { const o = this.get(k); this.set(k, o == null ? v : o + ', ' + v) }
    has(k) { return this._h.has(String(k).toLowerCase()) }
    delete(k) { this._h.delete(String(k).toLowerCase()) }
    forEach(f, t) { for (const [k, v] of this._h) f.call(t, v, k, this) }
    entries() { return this._h.entries() }
    keys() { return this._h.keys() }
    values() { return this._h.values() }
    [Symbol.iterator]() { return this._h.entries() }
  }
  g.Headers = Headers
  class Response {
    constructor(body, init) {
      init = init || {}
      this._body = body == null ? '' : String(body)
      this.status = init.status === undefined ? 200 : init.status
      this.statusText = init.statusText || ''
      this.ok = this.status >= 200 && this.status < 300
      this.headers = new Headers(init.headers)
      this.url = init.url || ''
      this.redirected = false
      this.type = 'basic'
      this.bodyUsed = false
    }
    text() { this.bodyUsed = true; return Promise.resolve(this._body) }
    json() { this.bodyUsed = true; try { return Promise.resolve(JSON.parse(this._body)) } catch (e) { return Promise.reject(e) } }
    blob() { return Promise.resolve(new Blob([this._body], { type: this.headers.get('content-type') || '' })) }
    arrayBuffer() { return Promise.resolve(new TextEncoder().encode(this._body)) }
    formData() { return Promise.resolve(new FormData()) }
    clone() { return new Response(this._body, { status: this.status, headers: this.headers, url: this.url }) }
    static json(data, init) { return new Response(JSON.stringify(data), Object.assign({ headers: { 'content-type': 'application/json' } }, init)) }
    static error() { return new Response('', { status: 0 }) }
    static redirect(url, status) { return new Response('', { status: status || 302, headers: { location: url } }) }
  }
  g.Response = Response
  class Request {
    constructor(input, init) {
      init = init || {}
      this.url = __resolve(typeof input === 'string' ? input : input.url)
      this.method = (init.method || (input && input.method) || 'GET').toUpperCase()
      this.headers = new Headers(init.headers || (input && input.headers))
      this.body = init.body == null ? null : init.body
      this.signal = init.signal || null
      this.credentials = init.credentials || 'same-origin'
      this.mode = init.mode || 'cors'
    }
    clone() { return new Request(this) }
  }
  g.Request = Request
  const bodyText = b => (b == null ? undefined : typeof b === 'string' ? b : b instanceof URLSearchParams ? b.toString() : b instanceof FormData ? new URLSearchParams(b._e).toString() : JSON.stringify(b))
  g.fetch = (input, init) => {
    init = init || {}
    const url = typeof input === 'string' || input instanceof URL ? String(input) : input.url
    const method = (init.method || (input && input.method) || 'GET').toUpperCase()
    if (init.signal && init.signal.aborted) return Promise.reject(init.signal.reason)
    const r = __fetch(method, url, bodyText(init.body !== undefined ? init.body : input && input.body))
    if (!r) return Promise.reject(new TypeError('Failed to fetch'))
    return Promise.resolve(new Response(r[2], { status: r[0], headers: { 'content-type': r[1] }, url: r[3] }))
  }

  class XMLHttpRequest extends EventTarget {
    constructor() {
      super()
      this.readyState = 0; this.status = 0; this.statusText = ''; this.responseText = ''; this.response = ''
      this.responseType = ''; this.responseURL = ''; this.timeout = 0; this.withCredentials = false
      this.upload = new EventTarget()
      this._type = ''
    }
    open(method, url) { this._method = String(method).toUpperCase(); this._url = String(url); this.readyState = 1 }
    setRequestHeader() {}
    overrideMimeType() {}
    getResponseHeader(k) { return String(k).toLowerCase() === 'content-type' ? this._type : null }
    getAllResponseHeaders() { return this._type ? 'content-type: ' + this._type + '\r\n' : '' }
    abort() { this._aborted = true }
    send(body) {
      const r = __fetch(this._method || 'GET', this._url, bodyText(body))
      setTimeout(() => {
        if (this._aborted) return
        if (!r) {
          this.readyState = 4
          this.dispatchEvent(new Event('readystatechange'))
          this.dispatchEvent(new Event('error'))
          this.dispatchEvent(new Event('loadend'))
          return
        }
        this.status = r[0]; this.statusText = r[0] === 200 ? 'OK' : ''; this._type = r[1]; this.responseURL = r[3]
        this.responseText = r[2]
        if (this.responseType === 'json') { try { this.response = JSON.parse(r[2]) } catch (_) { this.response = null } }
        else if (this.responseType === 'document') { this.response = new DOMParser().parseFromString(r[2], 'text/html') }
        else this.response = r[2]
        this.readyState = 4
        this.dispatchEvent(new Event('readystatechange'))
        this.dispatchEvent(new Event('load'))
        this.dispatchEvent(new Event('loadend'))
      }, 0)
    }
  }
  Object.assign(XMLHttpRequest, { UNSENT: 0, OPENED: 1, HEADERS_RECEIVED: 2, LOADING: 3, DONE: 4 })
  g.XMLHttpRequest = XMLHttpRequest
  g.WebSocket = class WebSocket extends EventTarget {
    constructor(url) { super(); this.url = url; this.readyState = 3; setTimeout(() => { this.dispatchEvent(new Event('error')); this.dispatchEvent(new Event('close')) }, 0) }
    send() {} close() {}
  }
  g.EventSource = class EventSource extends EventTarget { constructor(url) { super(); this.url = url; this.readyState = 2 } close() {} }

  // ---------------------------------------------------------------- observers
  g.MutationObserver = class MutationObserver { constructor(cb) { this.cb = cb } observe() {} disconnect() {} takeRecords() { return [] } }
  g.ResizeObserver = class ResizeObserver { constructor(cb) { this.cb = cb } observe() {} unobserve() {} disconnect() {} }
  g.PerformanceObserver = class PerformanceObserver { constructor() {} observe() {} disconnect() {} takeRecords() { return [] } }
  g.PerformanceObserver.supportedEntryTypes = []
  g.ReportingObserver = class ReportingObserver { observe() {} disconnect() {} }
  // Everything counts as on screen: lazy loaders load at once.
  g.IntersectionObserver = class IntersectionObserver {
    constructor(cb, opts) { this.cb = cb; this.root = null; this.rootMargin = (opts && opts.rootMargin) || '0px'; this.thresholds = [0]; this._on = true }
    observe(el) {
      setTimeout(() => {
        if (!this._on) return
        const r = el.getBoundingClientRect()
        this.cb([{ target: el, isIntersecting: true, intersectionRatio: 1, boundingClientRect: r, intersectionRect: r, rootBounds: null, time: performance.now() }], this)
      }, 0)
    }
    unobserve() {}
    disconnect() { this._on = false }
    takeRecords() { return [] }
  }

  // ---------------------------------------------------------------- data
  const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
  g.btoa = s => {
    s = String(s)
    let out = ''
    for (let i = 0; i < s.length; i += 3) {
      const a = s.charCodeAt(i), b = s.charCodeAt(i + 1), c = s.charCodeAt(i + 2)
      if (a > 255 || b > 255 || c > 255) throw new Error('InvalidCharacterError')
      const n = (a << 16) | ((b || 0) << 8) | (c || 0)
      out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63] + (i + 1 < s.length ? B64[(n >> 6) & 63] : '=') + (i + 2 < s.length ? B64[n & 63] : '=')
    }
    return out
  }
  g.atob = s => {
    s = String(s).replace(/[\s=]/g, '')
    let out = '', bits = 0, n = 0
    for (const ch of s) {
      const v = B64.indexOf(ch)
      if (v < 0) throw new Error('InvalidCharacterError')
      n = (n << 6) | v; bits += 6
      if (bits >= 8) { bits -= 8; out += String.fromCharCode((n >> bits) & 255) }
    }
    return out
  }
  // Typed arrays are plain arrays here (no binary data yet).
  for (const name of ['Uint8Array', 'Int8Array', 'Uint8ClampedArray', 'Uint16Array', 'Int16Array', 'Uint32Array', 'Int32Array', 'Float32Array', 'Float64Array', 'BigInt64Array', 'BigUint64Array']) {
    const T = function (x) {
      if (typeof x === 'number') return new Array(x).fill(0)
      if (x && typeof x.byteLength === 'number' && !Array.isArray(x)) return new Array(x.byteLength).fill(0)
      return Array.from(x || [])
    }
    T.from = (a, f) => Array.from(a, f)
    T.of = (...a) => a
    T.BYTES_PER_ELEMENT = 1
    g[name] = T
  }
  g.ArrayBuffer = function (n) { this.byteLength = n || 0 }
  g.ArrayBuffer.isView = () => false
  g.DataView = function (b) { this.buffer = b; this.byteLength = b ? b.byteLength : 0 }
  g.TextEncoder = class TextEncoder {
    get encoding() { return 'utf-8' }
    encode(s) {
      const out = []
      for (const ch of unescape(encodeURIComponent(String(s === undefined ? '' : s)))) out.push(ch.charCodeAt(0))
      return out
    }
  }
  g.TextDecoder = class TextDecoder {
    constructor(label) { this.encoding = label || 'utf-8' }
    decode(bytes) {
      if (!bytes) return ''
      let s = ''
      for (const b of Array.from(bytes)) s += '%' + (b < 16 ? '0' : '') + b.toString(16)
      try { return decodeURIComponent(s) } catch (_) { return Array.from(bytes).map(b => String.fromCharCode(b)).join('') }
    }
  }
  g.unescape = g.unescape || (s => String(s).replace(/%([0-9a-fA-F]{2})/g, (_, h) => String.fromCharCode(parseInt(h, 16))))
  g.escape = g.escape || (s => String(s).replace(/[^A-Za-z0-9@*_+\-./]/g, c => { const n = c.charCodeAt(0); return n < 256 ? '%' + (n < 16 ? '0' : '') + n.toString(16).toUpperCase() : '%u' + n.toString(16).toUpperCase().padStart(4, '0') }))
  g.Blob = class Blob {
    constructor(parts, opts) { this._t = (parts || []).map(String).join(''); this.type = (opts && opts.type) || ''; this.size = this._t.length }
    text() { return Promise.resolve(this._t) }
    arrayBuffer() { return Promise.resolve(new TextEncoder().encode(this._t)) }
    slice(a, b) { return new Blob([this._t.slice(a, b)], { type: this.type }) }
  }
  g.File = class File extends g.Blob { constructor(parts, name, opts) { super(parts, opts); this.name = name; this.lastModified = Date.now() } }
  g.FileReader = class FileReader extends EventTarget {
    readAsText(b) { b.text().then(t => { this.result = t; this.readyState = 2; this.dispatchEvent(new Event('load')); this.dispatchEvent(new Event('loadend')) }) }
    readAsDataURL(b) { b.text().then(t => { this.result = 'data:' + (b.type || 'text/plain') + ';base64,' + btoa(unescape(encodeURIComponent(t))); this.dispatchEvent(new Event('load')); this.dispatchEvent(new Event('loadend')) }) }
    abort() {}
  }
  class FormData {
    constructor(form) {
      this._e = []
      if (form && form.elements) {
        for (const el of form.elements) {
          if (!el.name || el.disabled) continue
          const t = (el.type || '').toLowerCase()
          if ((t === 'checkbox' || t === 'radio') && !el.checked) continue
          if (t === 'submit' || t === 'button' || t === 'reset' || t === 'file') continue
          this._e.push([el.name, el.value])
        }
      }
    }
    append(k, v) { this._e.push([String(k), v instanceof g.Blob ? v : String(v)]) }
    set(k, v) { this.delete(k); this.append(k, v) }
    get(k) { const f = this._e.find(x => x[0] === k); return f ? f[1] : null }
    getAll(k) { return this._e.filter(x => x[0] === k).map(x => x[1]) }
    has(k) { return this._e.some(x => x[0] === k) }
    delete(k) { this._e = this._e.filter(x => x[0] !== k) }
    forEach(f, t) { for (const [k, v] of this._e) f.call(t, v, k, this) }
    entries() { return this._e.map(x => x.slice()).values() }
    keys() { return this._e.map(x => x[0]).values() }
    values() { return this._e.map(x => x[1]).values() }
    [Symbol.iterator]() { return this.entries() }
  }
  g.FormData = FormData
  g.crypto = {
    getRandomValues(a) { for (let i = 0; i < a.length; i++) a[i] = Math.floor(Math.random() * 256); return a },
    randomUUID() {
      const h = '0123456789abcdef'
      let s = ''
      for (let i = 0; i < 36; i++) s += i === 8 || i === 13 || i === 18 || i === 23 ? '-' : i === 14 ? '4' : i === 19 ? h[8 + Math.floor(Math.random() * 4)] : h[Math.floor(Math.random() * 16)]
      return s
    },
    subtle: undefined,
  }
  const registry = new Map()
  g.customElements = {
    define(name, cls) { registry.set(name, cls) },
    get(name) { return registry.get(name) },
    whenDefined(name) { return Promise.resolve(registry.get(name)) },
    upgrade: noop,
  }
  g.getSelection = () => ({ rangeCount: 0, isCollapsed: true, type: 'None', removeAllRanges: noop, addRange: noop, collapse: noop, getRangeAt: () => document.createRange(), toString: () => '' })
  document.createRange = () => ({
    setStart: noop, setEnd: noop, setStartBefore: noop, setEndAfter: noop, selectNode: noop, selectNodeContents: noop, collapse: noop, detach: noop,
    getBoundingClientRect: () => ({ x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0 }),
    getClientRects: () => [],
    createContextualFragment(html) {
      const t = document.createElement('template'); t.innerHTML = html
      const f = document.createDocumentFragment()
      while (t.firstChild) f.appendChild(t.firstChild)
      return f
    },
  })
  document.createTreeWalker = (root) => {
    const nodes = []
    const walk = n => { for (const c of n.childNodes) { nodes.push(c); walk(c) } }
    walk(root)
    let i = -1
    return { currentNode: root, nextNode() { i++; this.currentNode = nodes[i] || null; return this.currentNode } }
  }
  document.implementation = { hasFeature: () => true, createHTMLDocument: () => new DOMParser().parseFromString('', 'text/html') }
  g.Image = function (w, h) { const i = document.createElement('img'); if (w) i.width = w; if (h) i.height = h; return i }
  g.Audio = function (src) { const a = document.createElement('audio'); if (src) a.src = src; return a }
  g.Option = function (text, value, def, selected) {
    const o = document.createElement('option')
    if (text !== undefined) o.textContent = text
    if (value !== undefined) o.value = value
    if (selected) o.selected = true
    return o
  }
  g.DOMParser = class DOMParser {
    parseFromString(html) {
      const root = document.createElement('html')
      root.innerHTML = String(html)
      const body = root.querySelector('body') || root
      const head = root.querySelector('head') || root
      const t = root.querySelector('title')
      return {
        documentElement: root, body, head, title: t ? t.textContent : '',
        querySelector: s => root.querySelector(s), querySelectorAll: s => root.querySelectorAll(s),
        getElementById: id => root.querySelector('#' + id), getElementsByTagName: n => root.getElementsByTagName(n),
        getElementsByClassName: n => root.getElementsByClassName(n), createElement: n => document.createElement(n),
      }
    }
  }
  g.XMLSerializer = class XMLSerializer { serializeToString(n) { return n.outerHTML || n.textContent || '' } }
  g.CSS = { supports: () => true, escape: s => String(s).replace(/([^\w-])/g, '\\$1') }
  g.trustedTypes = undefined
  g.Intl = g.Intl || {
    NumberFormat: function (loc, o) { return { format: n => (o && o.maximumFractionDigits === 0 ? Math.round(n) : n).toLocaleString(), formatToParts: n => [{ type: 'integer', value: String(n) }], resolvedOptions: () => ({ locale: 'en-US' }) } },
    DateTimeFormat: function () { return { format: d => new Date(d === undefined ? Date.now() : d).toLocaleDateString(), formatToParts: () => [], resolvedOptions: () => ({ locale: 'en-US', timeZone: 'UTC' }) } },
    Collator: function () { return { compare: (a, b) => String(a).localeCompare(String(b)) } },
    PluralRules: function () { return { select: n => (n === 1 ? 'one' : 'other') } },
    RelativeTimeFormat: function () { return { format: (v, u) => (v < 0 ? -v + ' ' + u + 's ago' : 'in ' + v + ' ' + u + 's') } },
    ListFormat: function () { return { format: l => Array.from(l).join(', ') } },
    Segmenter: function () { return { segment: s => Array.from(String(s)).map((c, i) => ({ segment: c, index: i })) } },
    getCanonicalLocales: l => [].concat(l || []),
  }

  // ---------------------------------------------------------------- the language, filled in
  const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']
  const D = Date.prototype
  D.getTimezoneOffset = function () { return 0 }
  D.toLocaleDateString = function () { return (this.getMonth() + 1) + '/' + this.getDate() + '/' + this.getFullYear() }
  D.toLocaleTimeString = function () { const h = this.getHours(); return ((h % 12) || 12) + ':' + String(this.getMinutes()).padStart(2, '0') + ':' + String(this.getSeconds()).padStart(2, '0') + (h < 12 ? ' AM' : ' PM') }
  D.toLocaleString = function () { return this.toLocaleDateString() + ', ' + this.toLocaleTimeString() }
  D.toDateString = function () { return ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'][this.getDay()] + ' ' + months[this.getMonth()] + ' ' + String(this.getDate()).padStart(2, '0') + ' ' + this.getFullYear() }
  D.toUTCString = D.toGMTString = function () { return this.toDateString() + ' ' + this.toISOString().slice(11, 19) + ' GMT' }
  const A = Array.prototype
  A.findLast = A.findLast || function (f, t) { for (let i = this.length - 1; i >= 0; i--) if (f.call(t, this[i], i, this)) return this[i] }
  A.findLastIndex = A.findLastIndex || function (f, t) { for (let i = this.length - 1; i >= 0; i--) if (f.call(t, this[i], i, this)) return i; return -1 }
  A.toSorted = A.toSorted || function (f) { return this.slice().sort(f) }
  A.toReversed = A.toReversed || function () { return this.slice().reverse() }
  A.toSpliced = A.toSpliced || function (...a) { const c = this.slice(); c.splice(...a); return c }
  A.with = A.with || function (i, v) { const c = this.slice(); c[i < 0 ? c.length + i : i] = v; return c }
  A.item = A.item || function (i) { return this.at(i) }
  A.copyWithin = A.copyWithin || function (t, s, e) { const part = this.slice(s, e); for (let i = 0; i < part.length && t + i < this.length; i++) this[t + i] = part[i]; return this }
  const N = Number.prototype
  N.toPrecision = N.toPrecision || function (p) { if (p === undefined) return String(this); const n = Number(this); if (n === 0) return (0).toFixed(p - 1); const e = Math.floor(Math.log10(Math.abs(n))); return e < -6 || e >= p ? n.toExponential ? n.toExponential(p - 1) : String(n) : n.toFixed(Math.max(0, p - 1 - e)) }
  N.toExponential = N.toExponential || function (d) { const n = Number(this); if (n === 0) return '0e+0'; const e = Math.floor(Math.log10(Math.abs(n))); const m = n / Math.pow(10, e); return (d === undefined ? String(m) : m.toFixed(d)) + 'e' + (e < 0 ? '-' : '+') + Math.abs(e) }
  const S = String.prototype
  S.normalize = S.normalize || function () { return String(this) }
  S.anchor = S.anchor || function (n) { return '<a name="' + n + '">' + this + '</a>' }
  String.raw = String.raw || function (strs, ...subs) { let out = ''; const raw = strs.raw || strs; for (let i = 0; i < raw.length; i++) { out += raw[i]; if (i < subs.length) out += subs[i] } return out }
  String.fromCodePoint = String.fromCodePoint || function (...cps) { return cps.map(c => c > 0xffff ? String.fromCharCode(0xd800 + ((c - 0x10000) >> 10), 0xdc00 + ((c - 0x10000) & 0x3ff)) : String.fromCharCode(c)).join('') }
  Object.is = Object.is || ((a, b) => (a === b ? a !== 0 || 1 / a === 1 / b : a !== a && b !== b))
  Object.getOwnPropertyDescriptor = Object.getOwnPropertyDescriptor || ((o, k) => (o != null && Object.prototype.hasOwnProperty.call(o, k) ? { value: o[k], writable: true, enumerable: true, configurable: true } : undefined))
  Object.getOwnPropertyDescriptors = Object.getOwnPropertyDescriptors || (o => { const r = {}; for (const k of Object.getOwnPropertyNames(o)) r[k] = Object.getOwnPropertyDescriptor(o, k); return r })
  Object.defineProperties = Object.defineProperties || ((o, ps) => { for (const k of Object.keys(ps)) Object.defineProperty(o, k, ps[k]); return o })
  Object.getOwnPropertySymbols = Object.getOwnPropertySymbols || (() => [])
  Object.isExtensible = Object.isExtensible || (() => true)
  Object.isSealed = Object.isSealed || (() => false)
  Object.groupBy = Object.groupBy || ((items, f) => { const r = {}; let i = 0; for (const x of items) { const k = f(x, i++); (r[k] || (r[k] = [])).push(x) } return r })
  Map.groupBy = Map.groupBy || ((items, f) => { const r = new Map(); let i = 0; for (const x of items) { const k = f(x, i++); if (!r.has(k)) r.set(k, []); r.get(k).push(x) } return r })
  g.Reflect = g.Reflect || {
    ownKeys: o => Object.getOwnPropertyNames(o), has: (o, k) => k in o, get: (o, k) => o[k], set: (o, k, v) => { o[k] = v; return true },
    apply: (f, t, a) => f.apply(t, a), construct: (F, a) => new F(...a), defineProperty: (o, k, d) => { Object.defineProperty(o, k, d); return true },
    getPrototypeOf: o => Object.getPrototypeOf(o), setPrototypeOf: (o, p) => { Object.setPrototypeOf(o, p); return true },
    deleteProperty: (o, k) => delete o[k], getOwnPropertyDescriptor: (o, k) => Object.getOwnPropertyDescriptor(o, k), isExtensible: () => true,
  }
  Math.imul = Math.imul || ((a, b) => ((a | 0) * (b | 0)) | 0)
  Math.clz32 = Math.clz32 || (x => { x = x >>> 0; if (x === 0) return 32; let n = 0; while (!(x & 0x80000000)) { x <<= 1; n++ } return n })
  Math.log1p = Math.log1p || (x => Math.log(1 + x))
  Math.expm1 = Math.expm1 || (x => Math.exp(x) - 1)
  Math.sinh = Math.sinh || (x => (Math.exp(x) - Math.exp(-x)) / 2)
  Math.cosh = Math.cosh || (x => (Math.exp(x) + Math.exp(-x)) / 2)
  Math.tanh = Math.tanh || (x => { const a = Math.exp(x), b = Math.exp(-x); return (a - b) / (a + b) })
  g.BigInt = g.BigInt || (n => Math.trunc(Number(n)))
  g.Proxy = g.Proxy || function (target) { return target }
  g.SharedArrayBuffer = undefined
})(window)
