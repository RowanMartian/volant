// Volant's web platform, written in JavaScript on top of the engine's native
// hooks (__fetch, __media, __resolve, __navigate, __setURL, __url, __width,
// __dark, __computed, __makeEvent). Runs once in every page before its scripts.
(function (g) {
  'use strict'
  const noop = () => {}
  const define = (o, k, get, set) => Object.defineProperty(o, k, { get, set: set || noop })

  // ---------------------------------------------------------------- DOMException
  const DOM_CODES = { IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4, InvalidCharacterError: 5, NoModificationAllowedError: 7,
    NotFoundError: 8, NotSupportedError: 9, InUseAttributeError: 10, InvalidStateError: 11, SyntaxError: 12, InvalidModificationError: 13,
    NamespaceError: 14, InvalidAccessError: 15, TypeMismatchError: 17, SecurityError: 18, NetworkError: 19, AbortError: 20, URLMismatchError: 21,
    QuotaExceededError: 22, TimeoutError: 23, InvalidNodeTypeError: 24, DataCloneError: 25 }
  const DOM_CONSTANTS = { INDEX_SIZE_ERR: 1, DOMSTRING_SIZE_ERR: 2, HIERARCHY_REQUEST_ERR: 3, WRONG_DOCUMENT_ERR: 4, INVALID_CHARACTER_ERR: 5,
    NO_DATA_ALLOWED_ERR: 6, NO_MODIFICATION_ALLOWED_ERR: 7, NOT_FOUND_ERR: 8, NOT_SUPPORTED_ERR: 9, INUSE_ATTRIBUTE_ERR: 10, INVALID_STATE_ERR: 11,
    SYNTAX_ERR: 12, INVALID_MODIFICATION_ERR: 13, NAMESPACE_ERR: 14, INVALID_ACCESS_ERR: 15, VALIDATION_ERR: 16, TYPE_MISMATCH_ERR: 17,
    SECURITY_ERR: 18, NETWORK_ERR: 19, ABORT_ERR: 20, URL_MISMATCH_ERR: 21, QUOTA_EXCEEDED_ERR: 22, TIMEOUT_ERR: 23, INVALID_NODE_TYPE_ERR: 24, DATA_CLONE_ERR: 25 }
  function DOMException(message, name) {
    if (!new.target) throw new TypeError("Constructor DOMException requires 'new'")
    const e = Object.create(DOMException.prototype)
    Object.defineProperty(e, 'message', { value: message === undefined ? '' : String(message), writable: true, configurable: true })
    Object.defineProperty(e, 'name', { value: name === undefined ? 'Error' : String(name), writable: true, configurable: true })
    Object.defineProperty(e, 'code', { value: DOM_CODES[e.name] || 0, writable: true, configurable: true })
    Object.defineProperty(e, 'stack', { value: '', writable: true, configurable: true })
    return e
  }
  DOMException.prototype = Object.create(Error.prototype)
  Object.defineProperty(DOMException.prototype, 'constructor', { value: DOMException, writable: true, configurable: true })
  Object.defineProperty(DOMException.prototype, 'toString', { value: function () { return this.name + (this.message ? ': ' + this.message : '') }, writable: true, configurable: true })
  for (const k in DOM_CONSTANTS) {
    Object.defineProperty(DOMException, k, { value: DOM_CONSTANTS[k], enumerable: true })
    Object.defineProperty(DOMException.prototype, k, { value: DOM_CONSTANTS[k], enumerable: true })
  }
  g.DOMException = DOMException

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
  // USVString conversion. Strings here are UTF-8 underneath, so lone
  // surrogates are already U+FFFD and this is just ToString.
  const usv = String
  const needs = (n, k, what) => { if (n < k) throw new TypeError(what + ': ' + k + ' argument' + (k > 1 ? 's' : '') + ' required, but only ' + n + ' present.') }
  class URLSearchParams {
    constructor(init) {
      this._p = []
      this._owner = null
      if (init === undefined) return
      if (init !== null && (typeof init === 'object' || typeof init === 'function')) {
        const it = init[Symbol.iterator]
        if (it !== undefined) {
          if (typeof it !== 'function') throw new TypeError("URLSearchParams: the iterator isn't callable")
          for (const pair of init) {
            if (pair === null || typeof pair !== 'object' && typeof pair !== 'function' || typeof pair[Symbol.iterator] !== 'function') throw new TypeError('URLSearchParams: each pair must be a sequence')
            const kv = Array.from(pair)
            if (kv.length !== 2) throw new TypeError('URLSearchParams: each pair must hold a name and a value')
            this._p.push([usv(kv[0]), usv(kv[1])])
          }
        } else {
          for (const k of Reflect.ownKeys(init)) {
            if (typeof k !== 'string') continue
            const d = Object.getOwnPropertyDescriptor(init, k)
            if (d && d.enumerable) this._p.push([usv(k), usv(init[k])])
          }
        }
        return
      }
      let str = usv(init)
      if (str[0] === '?') str = str.slice(1)
      this._p = __formParse(str)
    }
    _changed() { const o = this._owner; if (o) { const s = UrlState.get(o); s.p = __urlSet(s.p[0], 'query', this.toString()) } }
    get size() { return this._p.length }
    append(k, v) { needs(arguments.length, 2, "URLSearchParams.append"); this._p.push([usv(k), usv(v)]); this._changed() }
    delete(k, v) {
      needs(arguments.length, 1, 'URLSearchParams.delete'); k = usv(k)
      if (v === undefined) this._p = this._p.filter(x => x[0] !== k)
      else { v = usv(v); this._p = this._p.filter(x => x[0] !== k || x[1] !== v) }
      this._changed()
    }
    get(k) { needs(arguments.length, 1, 'URLSearchParams.get'); k = usv(k); const f = this._p.find(x => x[0] === k); return f ? f[1] : null }
    getAll(k) { needs(arguments.length, 1, 'URLSearchParams.getAll'); k = usv(k); return this._p.filter(x => x[0] === k).map(x => x[1]) }
    has(k, v) {
      needs(arguments.length, 1, 'URLSearchParams.has'); k = usv(k)
      if (v === undefined) return this._p.some(x => x[0] === k)
      v = usv(v); return this._p.some(x => x[0] === k && x[1] === v)
    }
    set(k, v) {
      needs(arguments.length, 2, 'URLSearchParams.set'); k = usv(k); v = usv(v)
      const i = this._p.findIndex(x => x[0] === k)
      if (i < 0) this._p.push([k, v])
      else { this._p[i][1] = v; this._p = this._p.filter((x, j) => j <= i || x[0] !== k) }
      this._changed()
    }
    sort() {
      // Stable, by UTF-16 code units of the names.
      this._p = this._p.map((x, i) => [x, i]).sort((a, b) => (a[0][0] < b[0][0] ? -1 : a[0][0] > b[0][0] ? 1 : a[1] - b[1])).map(x => x[0])
      this._changed()
    }
    forEach(f, t) {
      if (typeof f !== 'function') throw new TypeError('URLSearchParams.forEach: the callback must be a function')
      for (let i = 0; i < this._p.length; i++) { const [k, v] = this._p[i]; f.call(t, v, k, this) }
    }
    *_iter(kind) { for (let i = 0; i < this._p.length; i++) { const [k, v] = this._p[i]; yield kind === 0 ? k : kind === 1 ? v : [k, v] } }
    keys() { return this._iter(0) }
    values() { return this._iter(1) }
    entries() { return this._iter(2) }
    [Symbol.iterator]() { return this._iter(2) }
    toString() { return this._p.map(([k, v]) => __formEncode(k) + '=' + __formEncode(v)).join('&') }
    get [Symbol.toStringTag]() { return 'URLSearchParams' }
  }
  Object.defineProperty(URLSearchParams.prototype, '_iter', { enumerable: false })
  Object.defineProperty(URLSearchParams.prototype, '_changed', { enumerable: false })
  g.URLSearchParams = URLSearchParams

  // A URL's state: the native parser's parts (href, origin, protocol,
  // username, password, host, hostname, port, pathname, search, hash,
  // query) and its URLSearchParams.
  const UrlState = new WeakMap()
  const urlState = u => { const s = UrlState.get(u); if (!s) throw new TypeError('Illegal invocation'); return s }
  const syncParams = s => { s.params._p = __formParse(s.p[11]) }
  class URL {
    constructor(url, base) {
      needs(arguments.length, 1, 'URL constructor')
      const p = __urlParse(usv(url), base === undefined ? undefined : usv(base))
      if (!p) throw new TypeError("URL constructor: " + String(url) + ' is not a valid URL.')
      const params = new URLSearchParams()
      params._owner = this
      const s = { p, params }
      UrlState.set(this, s)
      syncParams(s)
    }
    get href() { return urlState(this).p[0] }
    set href(v) {
      const s = urlState(this)
      const p = __urlParse(usv(v))
      if (!p) throw new TypeError("URL.href: " + String(v) + ' is not a valid URL.')
      s.p = p
      syncParams(s)
    }
    get origin() { return urlState(this).p[1] }
    get searchParams() { return urlState(this).params }
    toString() { return urlState(this).p[0] }
    toJSON() { return urlState(this).p[0] }
    static canParse(u, b) { needs(arguments.length, 1, 'URL.canParse'); return __urlParse(usv(u), b === undefined ? undefined : usv(b)) !== null }
    static parse(u, b) { needs(arguments.length, 1, 'URL.parse'); try { return new URL(u, b) } catch (_) { return null } }
    static createObjectURL() { return 'blob:' + new URL(__url()).origin + '/' + Math.random().toString(36).slice(2) }
    static revokeObjectURL() {}
  }
  ;['protocol', 'username', 'password', 'host', 'hostname', 'port', 'pathname', 'search', 'hash'].forEach((k, n) => {
    const idx = n + 2
    Object.defineProperty(URL.prototype, k, {
      get() { return urlState(this).p[idx] },
      set(v) {
        const s = urlState(this)
        s.p = __urlSet(s.p[0], k, usv(v))
        if (k === 'search') syncParams(s)
      },
      enumerable: true, configurable: true,
    })
  })
  Object.defineProperty(URL.prototype, Symbol.toStringTag, { value: 'URL', configurable: true })
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
  // Documents without a browsing context have no location.
  Object.defineProperty(Document.prototype, 'location', { get() { return null }, set(_) {}, configurable: true, enumerable: true })
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
    sendBeacon(url) { if (!__urlParse(String(url), document.baseURI)) throw new TypeError('sendBeacon: ' + url + ' is not a valid URL.'); return true }, vibrate: () => false, share: () => Promise.reject(new Error('NotAllowedError')),
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
  g.open = (url = '') => { url = String(url); if (url !== '' && !__urlParse(url, document.baseURI)) throw new DOMException(url + ' is not a valid URL.', 'SyntaxError'); return null }
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
    blob() { this.bodyUsed = true; return Promise.resolve(new Blob([this._bytes || this._body], { type: this.headers.get('content-type') || '' })) }
    arrayBuffer() { this.bodyUsed = true; return Promise.resolve(this._bytes ? this._bytes.slice(0) : new TextEncoder().encode(this._body).buffer) }
    bytes() { return this.arrayBuffer().then(b => new Uint8Array(b)) }
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
  // A request body as text, and the type it implies (FormData goes as multipart).
  const encodeBody = b => {
    if (b == null) return [undefined, null]
    if (typeof b === 'string') return [b, 'text/plain;charset=UTF-8']
    if (b instanceof URLSearchParams) return [b.toString(), 'application/x-www-form-urlencoded;charset=UTF-8']
    if (b instanceof FormData) {
      const boundary = '----VolantFormBoundary' + Math.random().toString(36).slice(2, 14)
      let out = ''
      for (const [k, v] of b._e) {
        const file = v instanceof g.Blob
        out += '--' + boundary + '\r\nContent-Disposition: form-data; name="' + String(k).replace(/"/g, '%22') + '"' +
          (file ? '; filename="' + (v.name || 'blob') + '"\r\nContent-Type: ' + (v.type || 'application/octet-stream') : '') +
          '\r\n\r\n' + (file ? (v._t || '') : v) + '\r\n'
      }
      return [out + '--' + boundary + '--\r\n', 'multipart/form-data; boundary=' + boundary]
    }
    if (b instanceof g.Blob) return [b._t || '', b.type || null]
    if (b instanceof ArrayBuffer || ArrayBuffer.isView(b)) return [new TextDecoder().decode(b), null]
    return [String(b), 'text/plain;charset=UTF-8']
  }
  // Volant's request: [status, contentType, text, finalUrl, headers] or null.
  const send = (method, url, headers, body, credentials, mode) => {
    const [text, type] = encodeBody(body)
    const h = []
    headers.forEach((v, k) => h.push([k, v]))
    if (type && !headers.has('content-type')) h.push(['content-type', type])
    return __fetch(method, url, text, h, credentials || 'same-origin', mode || 'cors')
  }
  const parseHeaders = raw => {
    const h = new Headers()
    for (const line of String(raw || '').split('\r\n')) {
      const i = line.indexOf(':')
      if (i > 0) h.append(line.slice(0, i).trim(), line.slice(i + 1).trim())
    }
    return h
  }
  g.fetch = (input, init) => {
    init = init || {}
    const req = input instanceof Request ? input : null
    const url = req ? req.url : String(input)
    const method = (init.method || (req && req.method) || 'GET').toUpperCase()
    if (init.signal && init.signal.aborted) return Promise.reject(init.signal.reason)
    const headers = new Headers(init.headers || (req && req.headers))
    const credentials = init.credentials || (req && req.credentials) || 'same-origin'
    const mode = init.mode || (req && req.mode) || 'cors'
    const r = send(method, url, headers, init.body !== undefined ? init.body : req && req.body, credentials, mode)
    if (!r) return Promise.reject(new TypeError('Failed to fetch'))
    const res = new Response(r[2], { status: r[0], headers: parseHeaders(r[4]), url: r[3] })
    if (r[5]) Object.defineProperty(res, '_bytes', { value: r[5] })
    if (r[0] === 0) { res.type = 'opaque'; res.url = ''; return Promise.resolve(res) }
    if (__origin(r[3]) !== __origin(location.href)) res.type = 'cors'
    if (!res.headers.has('content-type') && r[1]) res.headers.set('content-type', r[1])
    res.redirected = r[3] !== __resolve(url)
    return Promise.resolve(res)
  }

  class XMLHttpRequest extends EventTarget {
    constructor() {
      super()
      this.readyState = 0; this.status = 0; this.statusText = ''; this.responseText = ''; this.response = ''
      this.responseType = ''; this.responseURL = ''; this.timeout = 0; this.withCredentials = false
      this.upload = new EventTarget()
      this._type = ''
    }
    open(method, url) { this._method = String(method).toUpperCase(); this._url = String(url); if (!__urlParse(this._url, document.baseURI)) throw new DOMException(this._url + ' is not a valid URL.', 'SyntaxError'); this._headers = new Headers(); this.readyState = 1 }
    setRequestHeader(k, v) { (this._headers || (this._headers = new Headers())).append(k, v) }
    overrideMimeType() {}
    getResponseHeader(k) { return this._res ? this._res.get(k) : null }
    getAllResponseHeaders() { let out = ''; if (this._res) this._res.forEach((v, k) => { out += k + ': ' + v + '\r\n' }); return out }
    abort() { this._aborted = true }
    send(body) {
      const r = send(this._method || 'GET', this._url, this._headers || new Headers(), body, this.withCredentials ? 'include' : 'same-origin', 'cors')
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
        this._res = parseHeaders(r[4])
        if (!this._res.has('content-type') && r[1]) this._res.set('content-type', r[1])
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
  g.TextEncoder = class TextEncoder {
    get encoding() { return 'utf-8' }
    encode(s) { return __encode(s === undefined ? '' : String(s)) }
    encodeInto(s, dest) {
      if (!(dest instanceof Uint8Array)) throw new TypeError('encodeInto needs a Uint8Array')
      s = String(s)
      let read = 0, written = 0
      for (const ch of s) {
        const b = __encode(ch)
        if (written + b.length > dest.length) break
        dest.set(b, written)
        written += b.length
        read += ch.length
      }
      return { read, written }
    }
  }
  g.TextDecoder = class TextDecoder {
    constructor(label, options) {
      const enc = __encodingFor(label === undefined ? 'utf-8' : String(label))
      if (enc === null || enc === 'replacement') throw new RangeError('The encoding label provided (' + label + ') is invalid.')
      options = options == null ? {} : options
      if (typeof options !== 'object' && typeof options !== 'function') throw new TypeError('options must be an object')
      Object.defineProperty(this, '_s', { value: { enc, fatal: !!options.fatal, ignoreBOM: !!options.ignoreBOM, pending: [], bomSeen: false } })
    }
    get encoding() { return this._s.enc.toLowerCase() }
    get fatal() { return this._s.fatal }
    get ignoreBOM() { return this._s.ignoreBOM }
    decode(input, options) {
      const s = this._s
      const stream = !!(options && options.stream)
      let bytes
      if (input === undefined) bytes = new Uint8Array(0)
      else if (input instanceof ArrayBuffer || (typeof SharedArrayBuffer !== 'undefined' && input instanceof SharedArrayBuffer)) bytes = new Uint8Array(input)
      else if (ArrayBuffer.isView(input)) bytes = new Uint8Array(input.buffer, input.byteOffset, input.byteLength)
      else throw new TypeError('decode needs an ArrayBuffer or a view of one')
      if (s.pending.length) { const all = new Uint8Array(s.pending.length + bytes.length); all.set(s.pending); all.set(bytes, s.pending.length); bytes = all }
      const [text, used] = __decode(s.enc, bytes, s.fatal, stream)
      s.pending = stream ? Array.from(bytes.subarray(used)) : []
      let out = text
      // A byte order mark at the very start is dropped (unless ignoreBOM).
      if (!s.bomSeen && out.length) {
        s.bomSeen = true
        if (!s.ignoreBOM && ['UTF-8', 'UTF-16LE', 'UTF-16BE'].includes(s.enc) && out.charCodeAt(0) === 0xFEFF) out = out.slice(1)
      }
      if (!stream) s.bomSeen = false
      return out
    }
  }
  g.unescape = g.unescape || (s => String(s).replace(/%([0-9a-fA-F]{2})/g, (_, h) => String.fromCharCode(parseInt(h, 16))))
  g.escape = g.escape || (s => String(s).replace(/[^A-Za-z0-9@*_+\-./]/g, c => { const n = c.charCodeAt(0); return n < 256 ? '%' + (n < 16 ? '0' : '') + n.toString(16).toUpperCase() : '%u' + n.toString(16).toUpperCase().padStart(4, '0') }))
  // A Blob keeps text as text until its bytes are wanted, and bytes as bytes.
  g.Blob = class Blob {
    constructor(parts, opts) {
      parts = parts == null ? [] : parts
      if (typeof parts !== 'object' || typeof parts[Symbol.iterator] !== 'function') throw new TypeError("Failed to construct 'Blob': The provided value cannot be converted to a sequence.")
      const list = [...parts]
      let text = true
      for (const p of list) if (p instanceof ArrayBuffer || ArrayBuffer.isView(p) || (p instanceof Blob && !('_tx' in p._d))) text = false
      const d = {}
      if (text) d._tx = list.map(p => p instanceof Blob ? p._d._tx : String(p)).join('')
      else {
        const chunks = list.map(p => p instanceof ArrayBuffer ? new Uint8Array(p) : ArrayBuffer.isView(p) ? new Uint8Array(p.buffer, p.byteOffset, p.byteLength) : p instanceof Blob ? p._u8() : __encode(String(p)))
        const out = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0))
        let at = 0
        for (const c of chunks) { out.set(c, at); at += c.length }
        d._b = out
      }
      Object.defineProperty(this, '_d', { value: d })
      const type = opts && opts.type !== undefined ? String(opts.type) : ''
      this.type = /[^\x20-\x7e]/.test(type) ? '' : type.toLowerCase()
    }
    get size() { return this._u8().length }
    get _t() { const d = this._d; return '_tx' in d ? d._tx : (d._tx = new TextDecoder().decode(d._b)) }
    _u8() { const d = this._d; return d._b || (d._b = __encode(d._tx)) }
    text() { return Promise.resolve(this._t) }
    arrayBuffer() { const u = this._u8(); return Promise.resolve(u.buffer.slice(u.byteOffset, u.byteOffset + u.length)) }
    bytes() { return Promise.resolve(new Uint8Array(this._u8())) }
    slice(a, b, type) {
      const u = this._u8(), n = u.length
      const at = v => v === undefined ? undefined : v < 0 ? Math.max(n + v, 0) : Math.min(v, n)
      const s = at(a) || 0, e = b === undefined ? n : at(b)
      return new Blob([u.slice(s, Math.max(s, e))], { type: type === undefined ? '' : type })
    }
    stream() { const u = this._u8(); return new ReadableStream({ start(c) { c.enqueue(new Uint8Array(u)); c.close() } }) }
  }
  g.File = class File extends g.Blob { constructor(parts, name, opts) { super(parts, opts); this.name = name; this.lastModified = Date.now() } }
  g.FileReader = class FileReader extends EventTarget {
    readAsText(b) { b.text().then(t => { this.result = t; this.readyState = 2; this.dispatchEvent(new Event('load')); this.dispatchEvent(new Event('loadend')) }) }
    readAsDataURL(b) {
      const u = b._u8()
      let bin = ''
      for (let i = 0; i < u.length; i += 8192) bin += String.fromCharCode.apply(null, u.subarray(i, i + 8192))
      this._done('data:' + (b.type || 'application/octet-stream') + ';base64,' + btoa(bin))
    }
    readAsArrayBuffer(b) { b.arrayBuffer().then(r => this._done(r)) }
    readAsBinaryString(b) { const u = b._u8(); let s = ''; for (let i = 0; i < u.length; i++) s += String.fromCharCode(u[i]); this._done(s) }
    _done(r) { this.readyState = 1; setTimeout(() => { this.result = r; this.readyState = 2; const e = k => { const ev = new Event(k); if (typeof this['on' + k] === 'function') this['on' + k](ev); this.dispatchEvent(ev) }; e('load'); e('loadend') }, 0) }
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
  // ---------------------------------------------------------------- more documents
  const NS = { html: 'http://www.w3.org/1999/xhtml', svg: 'http://www.w3.org/2000/svg' }
  class DOMImplementation {
    constructor(doc) { Object.defineProperty(this, '_doc', { value: doc }) }
    hasFeature() { return true }
    createHTMLDocument(title) {
      const d = __parseDocument('<!DOCTYPE html><html><head></head><body></body></html>')
      if (title !== undefined) { const t = d.createElement('title'); t.appendChild(d.createTextNode(String(title))); d.head.appendChild(t) }
      return d
    }
    createDocument(namespace, qualifiedName, doctype) {
      if (arguments.length < 2) throw new TypeError('createDocument needs 2 arguments')
      const ns = namespace == null || namespace === '' ? null : String(namespace)
      const d = __newDocument(ns === NS.html ? 'application/xhtml+xml' : ns === NS.svg ? 'image/svg+xml' : 'application/xml')
      if (doctype != null) d.appendChild(doctype)
      const q = qualifiedName === null ? '' : String(qualifiedName)
      if (q !== '') d.appendChild(d.createElementNS(ns, q))
      return d
    }
    createDocumentType(name, publicId, systemId) {
      if (arguments.length < 3) throw new TypeError('createDocumentType needs 3 arguments')
      return __createDocumentType(name, publicId, systemId, this._doc)
    }
  }
  g.DOMImplementation = DOMImplementation
  const impls = new WeakMap()
  Object.defineProperty(Document.prototype, 'implementation', {
    get() { let m = impls.get(this); if (!m) { m = new DOMImplementation(this); impls.set(this, m) } return m },
    configurable: true, enumerable: true,
  })
  {
    const P = Document.prototype
    const D = function Document() { if (!new.target) throw new TypeError("Constructor Document requires 'new'"); const d = __newDocument('application/xml'); Object.setPrototypeOf(d, P); return d }
    D.prototype = P
    Object.defineProperty(P, 'constructor', { value: D, writable: true, configurable: true })
    Object.setPrototypeOf(D, g.Node)
    g.Document = D
  }
  g.Audio = function (src) { const a = document.createElement('audio'); if (src) a.src = src; return a }
  g.Option = function (text, value, def, selected) {
    const o = document.createElement('option')
    if (text !== undefined) o.textContent = text
    if (value !== undefined) o.value = value
    if (selected) o.selected = true
    return o
  }
  g.DOMParser = class DOMParser {
    parseFromString(markup, type) {
      if (arguments.length < 2) throw new TypeError('parseFromString needs 2 arguments')
      type = String(type)
      if (!['text/html', 'text/xml', 'application/xml', 'application/xhtml+xml', 'image/svg+xml'].includes(type)) throw new TypeError('unsupported type ' + type)
      return __parseDocument(String(markup), type)
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

  // ---------------------------------------------------------------- geometry
  // DOMPoint, DOMRect, DOMQuad and DOMMatrix (CSS Geometry Interfaces).
  {
    const num = v => +v
    const dict = (v, what) => { if (v != null && typeof v !== 'object' && typeof v !== 'function') throw new TypeError("Failed to execute '" + what + "': The provided value is not of type 'Dictionary'."); return v || {} }
    const def = (o, k, v) => Object.defineProperty(o, k, { value: v, writable: true, configurable: true })
    const accessors = (C, names, writable, store) => {
      for (const n of names) Object.defineProperty(C.prototype, n, { get() { return this[store][n] }, set: writable ? function (v) { this[store][n] = num(v) } : undefined, enumerable: true, configurable: true })
    }
    class DOMPointReadOnly {
      constructor(x = 0, y = 0, z = 0, w = 1) { def(this, '_p', { x: num(x), y: num(y), z: num(z), w: num(w) }) }
      static fromPoint(...a) { const p = dict(a[0], 'fromPoint'); return new this(p.x, p.y, p.z, p.w) }
      matrixTransform(...a) {
        const m = DOMMatrixReadOnly.fromMatrix(a[0])
        const v = m._m, { x, y, z, w } = this._p
        return new DOMPoint(v[0] * x + v[4] * y + v[8] * z + v[12] * w, v[1] * x + v[5] * y + v[9] * z + v[13] * w, v[2] * x + v[6] * y + v[10] * z + v[14] * w, v[3] * x + v[7] * y + v[11] * z + v[15] * w)
      }
      toJSON() { const { x, y, z, w } = this._p; return { x, y, z, w } }
    }
    class DOMPoint extends DOMPointReadOnly {}
    accessors(DOMPointReadOnly, ['x', 'y', 'z', 'w'], false, '_p')
    accessors(DOMPoint, ['x', 'y', 'z', 'w'], true, '_p')
    const nanMin = (...v) => v.some(Number.isNaN) ? NaN : Math.min(...v)
    const nanMax = (...v) => v.some(Number.isNaN) ? NaN : Math.max(...v)
    class DOMRectReadOnly {
      constructor(x = 0, y = 0, width = 0, height = 0) { def(this, '_r', { x: num(x), y: num(y), width: num(width), height: num(height) }) }
      static fromRect(...a) { const r = dict(a[0], 'fromRect'); return new this(r.x, r.y, r.width, r.height) }
      get top() { const r = this._r; return nanMin(r.y, r.y + r.height) }
      get bottom() { const r = this._r; return nanMax(r.y, r.y + r.height) }
      get left() { const r = this._r; return nanMin(r.x, r.x + r.width) }
      get right() { const r = this._r; return nanMax(r.x, r.x + r.width) }
      toJSON() { const { x, y, width, height } = this._r; return { x, y, width, height, top: this.top, right: this.right, bottom: this.bottom, left: this.left } }
    }
    class DOMRect extends DOMRectReadOnly {}
    accessors(DOMRectReadOnly, ['x', 'y', 'width', 'height'], false, '_r')
    accessors(DOMRect, ['x', 'y', 'width', 'height'], true, '_r')
    for (const k of ['top', 'bottom', 'left', 'right']) Object.defineProperty(DOMRectReadOnly.prototype, k, { enumerable: true })
    class DOMQuad {
      constructor(p1, p2, p3, p4) { def(this, '_q', [p1, p2, p3, p4].map(p => DOMPoint.fromPoint(dict(p, 'DOMQuad')))) }
      static fromRect(r) { r = dict(r, 'fromRect'); const x = num(r.x ?? 0), y = num(r.y ?? 0), w = num(r.width ?? 0), h = num(r.height ?? 0); return new DOMQuad({ x, y }, { x: x + w, y }, { x: x + w, y: y + h }, { x, y: y + h }) }
      static fromQuad(q) { q = dict(q, 'fromQuad'); return new DOMQuad(q.p1, q.p2, q.p3, q.p4) }
      get p1() { return this._q[0] }
      get p2() { return this._q[1] }
      get p3() { return this._q[2] }
      get p4() { return this._q[3] }
      getBounds() {
        const xs = this._q.map(p => p.x), ys = this._q.map(p => p.y)
        const x = nanMin(...xs), y = nanMin(...ys)
        return new DOMRect(x, y, nanMax(...xs) - x, nanMax(...ys) - y)
      }
      toJSON() { return { p1: this.p1, p2: this.p2, p3: this.p3, p4: this.p4 } }
    }
    for (const k of ['p1', 'p2', 'p3', 'p4']) Object.defineProperty(DOMQuad.prototype, k, { enumerable: true })

    // Matrices are 16 numbers, column by column: [m11, m12, m13, m14, m21, …, m44].
    const ID = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]
    const mul = (A, B) => {
      const C = new Array(16)
      for (let c = 0; c < 4; c++) for (let r = 0; r < 4; r++) C[c * 4 + r] = A[r] * B[c * 4] + A[4 + r] * B[c * 4 + 1] + A[8 + r] * B[c * 4 + 2] + A[12 + r] * B[c * 4 + 3]
      return C
    }
    const two = (a, b, c, d, e, f) => [a, b, 0, 0, c, d, 0, 0, 0, 0, 1, 0, e, f, 0, 1]
    const NAMES = ['m11', 'm12', 'm13', 'm14', 'm21', 'm22', 'm23', 'm24', 'm31', 'm32', 'm33', 'm34', 'm41', 'm42', 'm43', 'm44']
    const ALIAS = { a: 0, b: 1, c: 4, d: 5, e: 12, f: 13 }
    const rad = d => d * Math.PI / 180
    const axisAngle = (x, y, z, deg) => {
      const len = Math.hypot(x, y, z)
      if (len === 0) return ID.slice()
      x /= len; y /= len; z /= len
      const half = rad(deg) / 2, sc = Math.sin(half) * Math.cos(half), sq = Math.sin(half) ** 2
      return [1 - 2 * (y * y + z * z) * sq, 2 * (x * y * sq + z * sc), 2 * (x * z * sq - y * sc), 0,
        2 * (x * y * sq - z * sc), 1 - 2 * (x * x + z * z) * sq, 2 * (y * z * sq + x * sc), 0,
        2 * (x * z * sq + y * sc), 2 * (y * z * sq - x * sc), 1 - 2 * (x * x + y * y) * sq, 0, 0, 0, 0, 1]
    }
    // A CSS transform list as a matrix and whether it's 2D (absolute lengths only).
    const parseTransform = str => {
      str = String(str).replace(/\/\*[\s\S]*?\*\//g, ' ').trim()
      if (str === '' || str.toLowerCase() === 'none') return [ID.slice(), true]
      const bad = () => { throw new DOMException("Failed to parse '" + str + "'.", 'SyntaxError') }
      let m = ID.slice(), is2D = true, rest = str
      const len = s => { const r = /^([+-]?(?:\d+\.?\d*|\.\d+)(?:e[+-]?\d+)?)(px)?$/i.exec(s); if (!r || (!r[2] && +r[1] !== 0)) bad(); return +r[1] }
      const ang = s => { const r = /^([+-]?(?:\d+\.?\d*|\.\d+)(?:e[+-]?\d+)?)(deg|rad|grad|turn)?$/i.exec(s); if (!r || (!r[2] && +r[1] !== 0)) bad(); const u = (r[2] || 'deg').toLowerCase(); return +r[1] * (u === 'rad' ? 180 / Math.PI : u === 'grad' ? 0.9 : u === 'turn' ? 360 : 1) }
      const n = s => { const r = /^[+-]?(?:\d+\.?\d*|\.\d+)(?:e[+-]?\d+)?$/i.exec(s); if (!r) bad(); return +s }
      while (rest.length) {
        const f = /^([a-zA-Z0-9]+)\(([^)]*)\)\s*/.exec(rest)
        if (!f) bad()
        rest = rest.slice(f[0].length)
        const name = f[1].toLowerCase(), args = f[2].split(',').map(a => a.trim())
        if (args.some(a => a === '')) bad()
        const k = cnt => { if (!(Array.isArray(cnt) ? cnt.includes(args.length) : args.length === cnt)) bad() }
        let t
        switch (name) {
          case 'matrix': k(6); t = two(...args.map(n)); break
          case 'matrix3d': k(16); t = args.map(n); is2D = false; break
          case 'translate': k([1, 2]); t = two(1, 0, 0, 1, len(args[0]), args[1] ? len(args[1]) : 0); break
          case 'translatex': k(1); t = two(1, 0, 0, 1, len(args[0]), 0); break
          case 'translatey': k(1); t = two(1, 0, 0, 1, 0, len(args[0])); break
          case 'translatez': k(1); t = ID.slice(); t[14] = len(args[0]); is2D = false; break
          case 'translate3d': k(3); t = ID.slice(); t[12] = len(args[0]); t[13] = len(args[1]); t[14] = len(args[2]); is2D = false; break
          case 'scale': k([1, 2]); { const x = n(args[0]); t = two(x, 0, 0, args[1] ? n(args[1]) : x, 0, 0) } break
          case 'scalex': k(1); t = two(n(args[0]), 0, 0, 1, 0, 0); break
          case 'scaley': k(1); t = two(1, 0, 0, n(args[0]), 0, 0); break
          case 'scalez': k(1); t = ID.slice(); t[10] = n(args[0]); is2D = false; break
          case 'scale3d': k(3); t = ID.slice(); t[0] = n(args[0]); t[5] = n(args[1]); t[10] = n(args[2]); is2D = false; break
          case 'rotate': case 'rotatez': k(1); { const a = rad(ang(args[0])); t = two(Math.cos(a), Math.sin(a), -Math.sin(a), Math.cos(a), 0, 0) } if (name === 'rotatez') is2D = false; break
          case 'rotatex': k(1); t = axisAngle(1, 0, 0, ang(args[0])); is2D = false; break
          case 'rotatey': k(1); t = axisAngle(0, 1, 0, ang(args[0])); is2D = false; break
          case 'rotate3d': k(4); t = axisAngle(n(args[0]), n(args[1]), n(args[2]), ang(args[3])); is2D = false; break
          case 'skew': k([1, 2]); t = two(1, Math.tan(rad(args[1] ? ang(args[1]) : 0)), Math.tan(rad(ang(args[0]))), 1, 0, 0); break
          case 'skewx': k(1); t = two(1, 0, Math.tan(rad(ang(args[0]))), 1, 0, 0); break
          case 'skewy': k(1); t = two(1, Math.tan(rad(ang(args[0]))), 0, 1, 0, 0); break
          case 'perspective': k(1); { const d = len(args[0]); t = ID.slice(); if (d !== 0) t[11] = -1 / d } is2D = false; break
          default: bad()
        }
        m = mul(m, t)
      }
      return [m, is2D]
    }
    const same = (a, b) => a === b || (Number.isNaN(a) && Number.isNaN(b))
    // Checks a DOMMatrixInit dictionary and fills in its gaps.
    const fixup = (o, only2D) => {
      o = dict(o, 'fromMatrix')
      for (const [al, idx] of Object.entries(ALIAS)) {
        const nm = NAMES[idx]
        if (o[al] !== undefined && o[nm] !== undefined && !same(+o[al], +o[nm])) throw new TypeError("Failed to execute 'fromMatrix': The '" + al + "' property should equal '" + nm + "'.")
      }
      const v = ID.slice()
      for (const [al, idx] of Object.entries(ALIAS)) { const x = o[NAMES[idx]] !== undefined ? o[NAMES[idx]] : o[al]; if (x !== undefined) v[idx] = +x }
      if (only2D) return [v, true]
      const third = [2, 3, 6, 7, 8, 9, 11, 14]
      let flat = true
      for (const k of [2, 3, 6, 7, 8, 9, 10, 11, 14, 15]) { if (o[NAMES[k]] !== undefined) { v[k] = +o[NAMES[k]]; if (third.includes(k) ? v[k] !== 0 : v[k] !== 1) flat = false } }
      if (o.is2D === true && !flat) throw new TypeError("Failed to execute 'fromMatrix': The is2D member is set to true but the input matrix is a 3D matrix.")
      return [v, o.is2D === undefined ? flat : !!o.is2D]
    }
    class DOMMatrixReadOnly {
      constructor(init) {
        let v = ID.slice(), is2D = true
        if (init === undefined) {} else if (typeof init === 'string') [v, is2D] = parseTransform(init)
        else if (init !== null && typeof init === 'object' && typeof init[Symbol.iterator] === 'function') {
          const a = [...init].map(num)
          if (a.length === 6) v = two(...a)
          else if (a.length === 16) { v = a; is2D = false }
          else throw new TypeError("Failed to construct 'DOMMatrix': The sequence must contain 6 elements for a 2D matrix or 16 elements for a 3D matrix.")
        } else [v, is2D] = parseTransform(String(init))
        def(this, '_m', v)
        def(this, '_2d', is2D)
      }
      static fromMatrix(...a) { const o = a[0]; if (o instanceof DOMMatrixReadOnly) return this._of(o._m.slice(), o._2d); const [v, d] = fixup(o); return this._of(v, d) }
      static fromFloat32Array(a) { if (!(a instanceof Float32Array)) throw new TypeError('Not a Float32Array'); return new this([...a]) }
      static fromFloat64Array(a) { if (!(a instanceof Float64Array)) throw new TypeError('Not a Float64Array'); return new this([...a]) }
      static _of(v, is2D) { const m = new this(); m._m = v; m._2d = is2D; return m }
      get is2D() { return this._2d }
      get isIdentity() { return this._m.every((x, k) => x === ID[k]) }
      _copy() { return DOMMatrix._of(this._m.slice(), this._2d) }
      translate(...a) { return this._copy().translateSelf(...a) }
      scale(...a) { return this._copy().scaleSelf(...a) }
      scaleNonUniform(...a) { return this._copy().scaleSelf(a[0] ?? 1, a[1] ?? 1, 1, 0, 0, 0) }
      scale3d(...a) { return this._copy().scale3dSelf(...a) }
      rotate(...a) { return this._copy().rotateSelf(...a) }
      rotateFromVector(...a) { return this._copy().rotateFromVectorSelf(...a) }
      rotateAxisAngle(...a) { return this._copy().rotateAxisAngleSelf(...a) }
      skewX(...a) { return this._copy().skewXSelf(...a) }
      skewY(...a) { return this._copy().skewYSelf(...a) }
      multiply(...a) { return this._copy().multiplySelf(...a) }
      flipX() { return this._copy().multiplySelf(DOMMatrix._of(two(-1, 0, 0, 1, 0, 0), true)) }
      flipY() { return this._copy().multiplySelf(DOMMatrix._of(two(1, 0, 0, -1, 0, 0), true)) }
      inverse() { return this._copy().invertSelf() }
      transformPoint(...a) { return DOMPoint.fromPoint(a[0]).matrixTransform(this) }
      toFloat32Array() { return new Float32Array(this._m) }
      toFloat64Array() { return new Float64Array(this._m) }
      toString() {
        if (!this._m.every(Number.isFinite)) throw new DOMException('Cannot serialize a matrix with non-finite values.', 'InvalidStateError')
        const v = this._m
        return this._2d ? 'matrix(' + [v[0], v[1], v[4], v[5], v[12], v[13]].join(', ') + ')' : 'matrix3d(' + v.join(', ') + ')'
      }
      toJSON() { const o = {}; for (const k of ['a', 'b', 'c', 'd', 'e', 'f', ...NAMES]) o[k] = this[k]; o.is2D = this.is2D; o.isIdentity = this.isIdentity; return o }
    }
    class DOMMatrix extends DOMMatrixReadOnly {
      multiplySelf(...a) { const o = DOMMatrixReadOnly.fromMatrix(a[0]); this._m = mul(this._m, o._m); if (!o._2d) this._2d = false; return this }
      preMultiplySelf(...a) { const o = DOMMatrixReadOnly.fromMatrix(a[0]); this._m = mul(o._m, this._m); if (!o._2d) this._2d = false; return this }
      translateSelf(...a) { const x = +(a[0] ?? 0), y = +(a[1] ?? 0), z = +(a[2] ?? 0); const t = ID.slice(); t[12] = x; t[13] = y; t[14] = z; this._m = mul(this._m, t); if (z !== 0) this._2d = false; return this }
      scaleSelf(...a) {
        const sx = +(a[0] ?? 1), sy = a[1] === undefined ? sx : +a[1], sz = +(a[2] ?? 1), ox = +(a[3] ?? 0), oy = +(a[4] ?? 0), oz = +(a[5] ?? 0)
        this.translateSelf(ox, oy, oz)
        const t = ID.slice(); t[0] = sx; t[5] = sy; t[10] = sz
        this._m = mul(this._m, t)
        this.translateSelf(-ox, -oy, -oz)
        if (sz !== 1 || oz !== 0) this._2d = false
        return this
      }
      scale3dSelf(...a) { const s = a[0] ?? 1; this.scaleSelf(s, s, s, a[1] ?? 0, a[2] ?? 0, a[3] ?? 0); if (+s !== 1) this._2d = false; return this }
      rotateSelf(rx = 0, ry, rz) {
        if (ry === undefined && rz === undefined) { rz = rx; rx = 0; ry = 0 }
        rx = +rx; ry = +(ry ?? 0); rz = +(rz ?? 0)
        if (rx !== 0 || ry !== 0) this._2d = false
        if (rz) this._m = mul(this._m, axisAngle(0, 0, 1, rz))
        if (ry) this._m = mul(this._m, axisAngle(0, 1, 0, ry))
        if (rx) this._m = mul(this._m, axisAngle(1, 0, 0, rx))
        return this
      }
      rotateFromVectorSelf(...a) { const x = +(a[0] ?? 0), y = +(a[1] ?? 0); const a = x === 0 && y === 0 ? 0 : Math.atan2(y, x) * 180 / Math.PI; if (a) this._m = mul(this._m, axisAngle(0, 0, 1, a)); return this }
      rotateAxisAngleSelf(...a) { const x = +(a[0] ?? 0), y = +(a[1] ?? 0), z = +(a[2] ?? 0), angle = +(a[3] ?? 0); this._m = mul(this._m, axisAngle(x, y, z, angle)); if (x !== 0 || y !== 0) this._2d = false; return this }
      skewXSelf(...a) { const s = a[0] ?? 0; this._m = mul(this._m, two(1, 0, Math.tan(rad(+s)), 1, 0, 0)); return this }
      skewYSelf(...a) { const s = a[0] ?? 0; this._m = mul(this._m, two(1, Math.tan(rad(+s)), 0, 1, 0, 0)); return this }
      invertSelf() {
        const m = this._m, inv = new Array(16)
        inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15] + m[9] * m[7] * m[14] + m[13] * m[6] * m[11] - m[13] * m[7] * m[10]
        inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15] - m[8] * m[7] * m[14] - m[12] * m[6] * m[11] + m[12] * m[7] * m[10]
        inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15] + m[8] * m[7] * m[13] + m[12] * m[5] * m[11] - m[12] * m[7] * m[9]
        inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14] - m[8] * m[6] * m[13] - m[12] * m[5] * m[10] + m[12] * m[6] * m[9]
        inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15] - m[9] * m[3] * m[14] - m[13] * m[2] * m[11] + m[13] * m[3] * m[10]
        inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15] + m[8] * m[3] * m[14] + m[12] * m[2] * m[11] - m[12] * m[3] * m[10]
        inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15] - m[8] * m[3] * m[13] - m[12] * m[1] * m[11] + m[12] * m[3] * m[9]
        inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14] + m[8] * m[2] * m[13] + m[12] * m[1] * m[10] - m[12] * m[2] * m[9]
        inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15] + m[5] * m[3] * m[14] + m[13] * m[2] * m[7] - m[13] * m[3] * m[6]
        inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15] - m[4] * m[3] * m[14] - m[12] * m[2] * m[7] + m[12] * m[3] * m[6]
        inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15] + m[4] * m[3] * m[13] + m[12] * m[1] * m[7] - m[12] * m[3] * m[5]
        inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14] - m[4] * m[2] * m[13] - m[12] * m[1] * m[6] + m[12] * m[2] * m[5]
        inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11] - m[5] * m[3] * m[10] - m[9] * m[2] * m[7] + m[9] * m[3] * m[6]
        inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11] + m[4] * m[3] * m[10] + m[8] * m[2] * m[7] - m[8] * m[3] * m[6]
        inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11] - m[4] * m[3] * m[9] - m[8] * m[1] * m[7] + m[8] * m[3] * m[5]
        inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10] + m[4] * m[2] * m[9] + m[8] * m[1] * m[6] - m[8] * m[2] * m[5]
        const det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12]
        if (det === 0 || !Number.isFinite(det)) { this._m = new Array(16).fill(NaN); this._2d = false; return this }
        this._m = inv.map(x => x / det)
        return this
      }
      setMatrixValue(str) { const [v, d] = parseTransform(str); this._m = v; this._2d = d; return this }
    }
    for (const [k, idx] of [...Object.entries(ALIAS), ...NAMES.map((n, i) => [n, i])]) {
      Object.defineProperty(DOMMatrixReadOnly.prototype, k, { get() { return this._m[idx] }, enumerable: true, configurable: true })
      const third = [2, 3, 6, 7, 8, 9, 11, 14].includes(idx), diag = idx === 10 || idx === 15
      Object.defineProperty(DOMMatrix.prototype, k, {
        get() { return this._m[idx] },
        set(v) { v = +v; this._m[idx] = v; if ((third && v !== 0) || (diag && v !== 1)) this._2d = false },
        enumerable: true, configurable: true,
      })
    }
    const all = { DOMPointReadOnly, DOMPoint, DOMRectReadOnly, DOMRect, DOMQuad, DOMMatrixReadOnly, DOMMatrix }
    for (const [k, C] of Object.entries(all)) {
      Object.defineProperty(C.prototype, Symbol.toStringTag, { value: k, configurable: true })
      Object.defineProperty(g, k, { value: C, writable: true, configurable: true })
    }
    Object.defineProperty(g, 'WebKitCSSMatrix', { value: DOMMatrix, writable: true, configurable: true })
    // A DOMMatrix2DInit for canvases: [a, b, c, d, e, f], checked and filled in.
    Object.defineProperty(g, '__m2d', { value: o => { if (o instanceof DOMMatrixReadOnly) { const v = o._m; return [v[0], v[1], v[4], v[5], v[12], v[13]] } const [v] = fixup(o, true); return [v[0], v[1], v[4], v[5], v[12], v[13]] } })
  }

  // ---------------------------------------------------------------- fonts
  // FontFace and document.fonts: Canard reads TrueType, OpenType and WOFF files itself.
  {
    const weights = w => {
      const one = v => v === 'normal' ? 400 : v === 'bold' ? 700 : Math.min(1000, Math.max(1, parseFloat(v) || 400))
      const p = String(w == null ? 'normal' : w).trim().split(/\s+/)
      return [one(p[0]), one(p[1] || p[0])]
    }
    const bufferish = v => v instanceof ArrayBuffer || ArrayBuffer.isView(v)
    class FontFace {
      constructor(family, source, desc) {
        if (arguments.length < 2) throw new TypeError("Failed to construct 'FontFace': 2 arguments required.")
        desc = desc || {}
        this.family = String(family).replace(/^["']|["']$/g, '')
        this.style = desc.style || 'normal'
        this.weight = String(desc.weight || 'normal')
        this.stretch = desc.stretch || 'normal'
        this.unicodeRange = desc.unicodeRange || 'U+0-10FFFF'
        this.variant = desc.variant || 'normal'
        this.featureSettings = desc.featureSettings || 'normal'
        this.display = desc.display || 'auto'
        this.ascentOverride = this.descentOverride = this.lineGapOverride = 'normal'
        this.status = 'unloaded'
        let ok, no
        this.loaded = new Promise((a, b) => { ok = a; no = b })
        this.loaded.catch(() => {})
        Object.defineProperty(this, '_src', { value: source })
        Object.defineProperty(this, '_done', { value: [ok, no] })
        // Bytes load straight away; addresses when asked to (or when the set needs them).
        if (bufferish(source)) this._load()
        else if (typeof source !== 'string') { this.status = 'error'; no(new DOMException('The source is not a string or buffer.', 'SyntaxError')) }
      }
      _load() {
        if (this.status !== 'unloaded') return
        this.status = 'loading'
        const [lo, hi] = weights(this.weight)
        const ok = __fontload(this.family, this._src, lo, hi, /italic|oblique/.test(this.style))
        this.status = ok ? 'loaded' : 'error'
        if (ok) this._done[0](this)
        else this._done[1](new DOMException('A network error occurred.', 'NetworkError'))
        fonts._changed()
      }
      load() { this._load(); return this.loaded }
    }
    g.FontFace = FontFace
    class FontFaceSetLoadEvent extends Event {
      constructor(type, init) { super(type, init); this.fontfaces = (init && init.fontfaces) || [] }
    }
    g.FontFaceSetLoadEvent = FontFaceSetLoadEvent
    // The page's own @font-face rules load the first time anything asks.
    let sheetsLoaded = false
    const sheets = () => { if (!sheetsLoaded) { sheetsLoaded = true; __fontfaces() } }
    class FontFaceSet extends EventTarget {
      constructor() { super(); Object.defineProperty(this, '_faces', { value: new Set() }) }
      get size() { return this._faces.size }
      get status() { for (const f of this._faces) if (f.status === 'loading') return 'loading'; return 'loaded' }
      get ready() { sheets(); return Promise.resolve(this) }
      add(f) { if (!(f instanceof FontFace)) throw new TypeError("Failed to execute 'add' on 'FontFaceSet': parameter 1 is not of type 'FontFace'."); this._faces.add(f); return this }
      delete(f) { return this._faces.delete(f) }
      has(f) { return this._faces.has(f) }
      clear() { this._faces.clear() }
      forEach(cb, self) { for (const f of this._faces) cb.call(self, f, f, this) }
      values() { return this._faces.values() }
      keys() { return this._faces.values() }
      entries() { return this._faces.entries() }
      [Symbol.iterator]() { return this._faces.values() }
      _matching(font) {
        const m = /([^,]*?)\s*(\d+(?:\.\d+)?(?:px|pt|em|rem|%))(?:\/\S+)?\s+(.+)$/.exec(String(font).trim())
        if (!m) throw new DOMException('Could not resolve \'' + font + '\' as a font.', 'SyntaxError')
        const fams = m[3].split(',').map(f => f.trim().replace(/^["']|["']$/g, '').toLowerCase())
        return [...this._faces].filter(f => fams.includes(f.family.toLowerCase()))
      }
      load(font, text) {
        sheets()
        let faces
        try { faces = this._matching(font) } catch (e) { return Promise.reject(e) }
        for (const f of faces) f._load()
        return Promise.all(faces.map(f => f.loaded))
      }
      check(font, text) {
        sheets()
        return this._matching(font).every(f => f.status === 'loaded')
      }
      _changed() {}
    }
    g.FontFaceSet = FontFaceSet
    const fonts = new FontFaceSet()
    Object.defineProperty(document, 'fonts', { get: () => fonts, configurable: true })
  }

  // ---------------------------------------------------------------- canvas
  // A 2D context queues commands (the numbers in script/canvas_run.rs), and
  // Canard carries them out on the canvas's pixels when the page reads them
  // back or the page is drawn. Getters answer from the state kept here.
  const CV = { SAVE: 1, RESTORE: 2, RESET: 3, TRANSFORM: 10, SET_TRANSFORM: 11, SET: 20, DASH: 21, BEGIN: 30, MOVE: 31, LINE: 32, QUAD: 33, CUBIC: 34, ARC: 35, ELLIPSE: 36, ARC_TO: 37, RECT: 38, ROUND_RECT: 39, CLOSE: 40, SVG: 41, ADD_PATH: 42, FILL: 50, STROKE: 51, CLIP: 52, FILL_RECT: 53, STROKE_RECT: 54, CLEAR_RECT: 55, FILL_TEXT: 60, STROKE_TEXT: 61, DRAW_IMAGE: 70, GET: 80, PUT: 81, URL: 82, HIT: 83 }
  const MAX_QUEUE = 100000
  const domErr = (name, msg) => new DOMException(msg || name, name)
  const fin = (...a) => a.every(Number.isFinite)
  const needArgs = (n, args, what) => { if (args.length < n) throw new TypeError("Failed to execute '" + what + "': " + n + ' arguments required, but only ' + args.length + ' present.') }
  class CanvasGradient {
    constructor(k, a) { this._k = k; this._a = a; this._s = [] }
    addColorStop(o, c) {
      needArgs(2, arguments, 'addColorStop')
      o = +o
      if (!Number.isFinite(o)) throw new TypeError('The provided double value is non-finite.')
      if (o < 0 || o > 1) throw domErr('IndexSizeError', 'The provided value (' + o + ') is outside the range (0.0, 1.0).')
      if (__cvcolor(String(c)) === null) throw domErr('SyntaxError', "The value provided ('" + c + "') could not be parsed as a color.")
      this._s.push([o, String(c)])
    }
  }
  class CanvasPattern {
    constructor(img, rep) { this._k = 'pattern'; this._i = img; this._r = rep; this._m = null }
    setTransform(m) { this._m = __m2d(m) }
  }
  // Path commands, shared by contexts and Path2D.
  const pathMethods = {
    moveTo(x, y) { this._p(CV.MOVE, +x, +y) },
    lineTo(x, y) { this._p(CV.LINE, +x, +y) },
    quadraticCurveTo(cx, cy, x, y) { this._p(CV.QUAD, +cx, +cy, +x, +y) },
    bezierCurveTo(a, b, c, d, e, f) { this._p(CV.CUBIC, +a, +b, +c, +d, +e, +f) },
    arc(x, y, r, a0, a1, ccw) {
      needArgs(5, arguments, 'arc')
      if (fin(+x, +y, +r, +a0, +a1) && r < 0) throw domErr('IndexSizeError', 'The radius provided (' + r + ') is negative.')
      this._p(CV.ARC, +x, +y, +r, +a0, +a1, ccw ? 1 : 0)
    },
    ellipse(x, y, rx, ry, rot, a0, a1, ccw) {
      needArgs(7, arguments, 'ellipse')
      if (fin(+x, +y, +rx, +ry, +rot, +a0, +a1) && (rx < 0 || ry < 0)) throw domErr('IndexSizeError', 'The radius provided is negative.')
      this._p(CV.ELLIPSE, +x, +y, +rx, +ry, +rot, +a0, +a1, ccw ? 1 : 0)
    },
    arcTo(x1, y1, x2, y2, r) {
      needArgs(5, arguments, 'arcTo')
      if (fin(+x1, +y1, +x2, +y2, +r) && r < 0) throw domErr('IndexSizeError', 'The radius provided (' + r + ') is negative.')
      this._p(CV.ARC_TO, +x1, +y1, +x2, +y2, +r)
    },
    rect(x, y, w, h) { this._p(CV.RECT, +x, +y, +w, +h) },
    roundRect(x, y, w, h, radii) {
      if (radii === undefined) radii = 0
      const list = (typeof radii === 'object' && radii !== null && typeof radii[Symbol.iterator] === 'function') ? Array.from(radii) : [radii]
      if (list.length < 1 || list.length > 4) throw new RangeError(list.length + ' radii provided. Between one and four radii are necessary.')
      const flat = []
      for (const r of list) {
        const [rx, ry] = typeof r === 'object' && r !== null ? [+(r.x || 0), +(r.y || 0)] : [+r, +r]
        if (!fin(rx, ry)) return
        if (rx < 0 || ry < 0) throw new RangeError('Radius value ' + r + ' is negative.')
        flat.push(rx, ry)
      }
      this._p(CV.ROUND_RECT, +x, +y, +w, +h, flat)
    },
    closePath() { this._p(CV.CLOSE) },
  }
  class Path2D {
    constructor(src) {
      this._c = []
      if (src instanceof Path2D) this._c = src._c.slice()
      else if (src !== undefined) this._c.push(CV.SVG, String(src))
    }
    _p(...a) { for (const v of a) this._c.push(v) }
    addPath(p, m) {
      if (!(p instanceof Path2D)) throw new TypeError("Failed to execute 'addPath': parameter 1 is not of type 'Path2D'.")
      const [a, b, c, d, e, f] = __m2d(m)
      this._c.push(CV.ADD_PATH, p._c.slice(), a, b, c, d, e, f)
    }
  }
  Object.assign(Path2D.prototype, pathMethods)
  class ImageData {
    constructor(a, b, c, opts) {
      if (a instanceof Uint8ClampedArray) {
        const n = a.length
        if (n === 0 || n % 4) throw domErr('InvalidStateError', 'The input data length is not a multiple of 4.')
        b = b >>> 0
        if (!b) throw domErr('IndexSizeError', 'The source width is zero or not a number.')
        if ((n / 4) % b) throw domErr('IndexSizeError', 'The input data length is not a multiple of (4 * width).')
        const h = n / 4 / b
        if (c !== undefined && c >>> 0 !== h) throw domErr('IndexSizeError', 'The input data length is not equal to (4 * width * height).')
        Object.defineProperties(this, { width: { value: b, enumerable: true }, height: { value: h, enumerable: true }, data: { value: a, enumerable: true } })
      } else {
        needArgs(2, arguments, 'ImageData')
        const w = Math.trunc(+a), h = Math.trunc(+b)
        if (!w || !h || !Number.isFinite(w) || !Number.isFinite(h)) throw domErr('IndexSizeError', 'The source width or height is zero.')
        const [aw, ah] = [Math.abs(w), Math.abs(h)]
        if (aw * ah > 1 << 28) throw new RangeError('Out of memory at ImageData creation')
        Object.defineProperties(this, { width: { value: aw, enumerable: true }, height: { value: ah, enumerable: true }, data: { value: new Uint8ClampedArray(aw * ah * 4), enumerable: true } })
      }
      Object.defineProperty(this, 'colorSpace', { value: (opts && opts.colorSpace) || 'srgb', enumerable: true })
    }
  }
  class TextMetrics {}
  // Each style property: (index for the queue, default, how to check a new value; undefined means ignore it).
  const ops = ['source-over', 'source-in', 'source-out', 'source-atop', 'destination-over', 'destination-in', 'destination-out', 'destination-atop', 'lighter', 'copy', 'xor', 'multiply', 'screen', 'overlay', 'darken', 'lighten', 'color-dodge', 'color-burn', 'hard-light', 'soft-light', 'difference', 'exclusion', 'hue', 'saturation', 'color', 'luminosity']
  const oneOf = list => v => (v = String(v), list.includes(v) ? v : undefined)
  const positive = v => (v = +v, Number.isFinite(v) && v > 0 ? v : undefined)
  const finite = v => (v = +v, Number.isFinite(v) ? v : undefined)
  const style = (v, ctx) => {
    if (v instanceof CanvasGradient || v instanceof CanvasPattern) return v
    const c = __cvcolor(String(v), ctx && ctx.canvas)
    return c === null ? undefined : c
  }
  const props = {
    fillStyle: [0, '#000000', style], strokeStyle: [1, '#000000', style],
    lineWidth: [2, 1, positive], lineCap: [3, 'butt', oneOf(['butt', 'round', 'square'])], lineJoin: [4, 'miter', oneOf(['miter', 'round', 'bevel'])],
    miterLimit: [5, 10, positive], globalAlpha: [6, 1, v => (v = +v, Number.isFinite(v) && v >= 0 && v <= 1 ? v : undefined)],
    globalCompositeOperation: [7, 'source-over', oneOf(ops)],
    font: [8, '10px sans-serif', v => (v = String(v), __cvfont(v) ? v : undefined)],
    textAlign: [9, 'start', oneOf(['start', 'end', 'left', 'right', 'center'])],
    textBaseline: [10, 'alphabetic', oneOf(['top', 'hanging', 'middle', 'alphabetic', 'ideographic', 'bottom'])],
    shadowBlur: [11, 0, v => (v = +v, Number.isFinite(v) && v >= 0 ? v : undefined)],
    shadowColor: [12, 'rgba(0, 0, 0, 0)', (v, ctx) => { const c = __cvcolor(String(v), ctx && ctx.canvas); return c === null ? undefined : c }],
    shadowOffsetX: [13, 0, finite], shadowOffsetY: [14, 0, finite],
    imageSmoothingEnabled: [15, true, v => !!v], lineDashOffset: [16, 0, finite],
    direction: [17, 'inherit', oneOf(['ltr', 'rtl', 'inherit'])],
  }
  // Accepted and kept, but nothing draws differently for them yet.
  const quiet = { imageSmoothingQuality: ['low', oneOf(['low', 'medium', 'high'])], filter: ['none', v => String(v)], letterSpacing: ['0px', v => String(v)], wordSpacing: ['0px', v => String(v)], fontKerning: ['auto', oneOf(['auto', 'normal', 'none'])], fontStretch: ['normal', v => String(v)], fontVariantCaps: ['normal', v => String(v)], textRendering: ['auto', v => String(v)] }
  const fresh = () => {
    const s = { m: [1, 0, 0, 1, 0, 0], dash: [] }
    for (const k in props) s[k] = props[k][1]
    for (const k in quiet) s[k] = quiet[k][0]
    return s
  }
  // The pixels of an image source, checked as drawImage and createPattern check them: null when it isn't ready.
  const usable = (img, what) => {
    if (img && img.tagName === 'CANVAS') {
      if (!img.width || !img.height) throw domErr('InvalidStateError', "Failed to execute '" + what + "': The image argument is a canvas element with a width or height of 0.")
      if (img.__ctx) img.__ctx._flush()
      return true
    }
    if (img instanceof ImageData || (img && img._bitmap)) return true
    if (img && (img.tagName === 'IMG' || img.tagName === 'image')) return !!(img.getAttribute('src') || img.getAttribute('href'))
    if (img && img.tagName === 'VIDEO') return false
    throw new TypeError("Failed to execute '" + what + "': The provided value is not of type '(CSSImageValue or HTMLCanvasElement or HTMLImageElement or HTMLVideoElement or ImageBitmap or OffscreenCanvas or SVGImageElement or VideoFrame)'.")
  }
  class CanvasRenderingContext2D {
    constructor(canvas) { this.canvas = canvas; this._q = []; this._s = fresh(); this._stack = [] }
    _p(...a) { const q = this._q; for (const v of a) q.push(v); if (q.length > MAX_QUEUE) this._flush() }
    _flush() { if (this._q.length) { const q = this._q; this._q = []; __cv(this.canvas, q) } }
    _ask(...a) { const q = this._q; this._q = []; for (const v of a) q.push(v); return __cv(this.canvas, q) }
    _reset() { this._q = [CV.RESET]; this._s = fresh(); this._stack = [] }
    getContextAttributes() { return { alpha: true, desynchronized: false, colorSpace: 'srgb', willReadFrequently: false } }
    isContextLost() { return false }
    // state
    save() { this._stack.push(Object.assign({}, this._s, { m: this._s.m.slice(), dash: this._s.dash.slice() })); this._p(CV.SAVE) }
    restore() { if (this._stack.length) { this._s = this._stack.pop(); this._p(CV.RESTORE) } }
    reset() { this._reset() }
    // transforms
    transform(a, b, c, d, e, f) {
      needArgs(6, arguments, 'transform')
      a = +a; b = +b; c = +c; d = +d; e = +e; f = +f
      if (!fin(a, b, c, d, e, f)) return
      const m = this._s.m
      this._s.m = [m[0] * a + m[2] * b, m[1] * a + m[3] * b, m[0] * c + m[2] * d, m[1] * c + m[3] * d, m[0] * e + m[2] * f + m[4], m[1] * e + m[3] * f + m[5]]
      this._p(CV.TRANSFORM, a, b, c, d, e, f)
    }
    setTransform(a, b, c, d, e, f) {
      if (arguments.length <= 1) [a, b, c, d, e, f] = __m2d(a)
      a = +a; b = +b; c = +c; d = +d; e = +e; f = +f
      if (!fin(a, b, c, d, e, f)) return
      this._s.m = [a, b, c, d, e, f]
      this._p(CV.SET_TRANSFORM, a, b, c, d, e, f)
    }
    resetTransform() { this.setTransform(1, 0, 0, 1, 0, 0) }
    getTransform() { const [a, b, c, d, e, f] = this._s.m; return g.DOMMatrix ? new g.DOMMatrix([a, b, c, d, e, f]) : { a, b, c, d, e, f, is2D: true, isIdentity: a === 1 && b === 0 && c === 0 && d === 1 && e === 0 && f === 0 } }
    translate(x, y) { needArgs(2, arguments, 'translate'); this.transform(1, 0, 0, 1, x, y) }
    scale(x, y) { needArgs(2, arguments, 'scale'); this.transform(x, 0, 0, y, 0, 0) }
    rotate(r) { needArgs(1, arguments, 'rotate'); r = +r; if (Number.isFinite(r)) this.transform(Math.cos(r), Math.sin(r), -Math.sin(r), Math.cos(r), 0, 0) }
    // paths
    beginPath() { this._p(CV.BEGIN) }
    _rule(r) { return r === 'evenodd' ? 1 : 0 }
    _pathArgs(a) { return a[0] instanceof Path2D ? [a[0]._c.slice(), a[1]] : [null, a[0]] }
    fill(...a) { const [p, r] = this._pathArgs(a); this._p(CV.FILL, this._rule(r), p) }
    stroke(p) { this._p(CV.STROKE, p instanceof Path2D ? p._c.slice() : null) }
    clip(...a) { const [p, r] = this._pathArgs(a); this._p(CV.CLIP, this._rule(r), p) }
    isPointInPath(...a) {
      const p = a[0] instanceof Path2D ? a.shift()._c.slice() : null
      needArgs(2, a, 'isPointInPath')
      return this._ask(CV.HIT, 0, +a[0], +a[1], this._rule(a[2]), p)
    }
    isPointInStroke(...a) {
      const p = a[0] instanceof Path2D ? a.shift()._c.slice() : null
      needArgs(2, a, 'isPointInStroke')
      return this._ask(CV.HIT, 1, +a[0], +a[1], 0, p)
    }
    // rectangles
    fillRect(x, y, w, h) { needArgs(4, arguments, 'fillRect'); this._p(CV.FILL_RECT, +x, +y, +w, +h) }
    strokeRect(x, y, w, h) { needArgs(4, arguments, 'strokeRect'); this._p(CV.STROKE_RECT, +x, +y, +w, +h) }
    clearRect(x, y, w, h) {
      needArgs(4, arguments, 'clearRect')
      // Clearing the whole canvas with no transform or clip forgets what was queued.
      const m = this._s.m
      if (m[0] === 1 && m[1] === 0 && m[2] === 0 && m[3] === 1 && m[4] === 0 && m[5] === 0 && x <= 0 && y <= 0 && x + w >= this.canvas.width && y + h >= this.canvas.height && !this._stack.length && this._q.indexOf(CV.CLIP) < 0 && this._q.indexOf(CV.SAVE) < 0 && this._q.indexOf(CV.RESET) < 0 && this._q.indexOf(CV.SET) < 0) this._q = []
      this._p(CV.CLEAR_RECT, +x, +y, +w, +h)
    }
    // text
    fillText(t, x, y, mw) { needArgs(3, arguments, 'fillText'); this._p(CV.FILL_TEXT, String(t), +x, +y, mw === undefined ? null : +mw) }
    strokeText(t, x, y, mw) { needArgs(3, arguments, 'strokeText'); this._p(CV.STROKE_TEXT, String(t), +x, +y, mw === undefined ? null : +mw) }
    measureText(t) {
      needArgs(1, arguments, 'measureText')
      const [w, asc, desc] = __measure(String(t).replace(/[\t\n\f\r]/g, ' '), this._s.font)
      const m = new TextMetrics()
      const align = this._s.textAlign, rtl = this._s.direction === 'rtl'
      const left = align === 'center' ? w / 2 : (align === 'right' || (align === 'end' && !rtl) || (align === 'start' && rtl)) ? w : 0
      const bl = this._s.textBaseline
      const base = bl === 'top' ? asc : bl === 'hanging' ? asc * 0.8 : bl === 'middle' ? (asc - desc) / 2 : (bl === 'bottom' || bl === 'ideographic') ? -desc : 0
      Object.assign(m, {
        width: w, actualBoundingBoxLeft: left, actualBoundingBoxRight: w - left,
        actualBoundingBoxAscent: asc - base, actualBoundingBoxDescent: desc + base,
        fontBoundingBoxAscent: asc - base, fontBoundingBoxDescent: desc + base,
        emHeightAscent: asc - base, emHeightDescent: desc + base,
        hangingBaseline: asc * 0.8 - base, alphabeticBaseline: -base, ideographicBaseline: -desc - base,
      })
      return m
    }
    // images
    drawImage(img, ...a) {
      if (![2, 4, 8].includes(a.length)) throw new TypeError("Failed to execute 'drawImage': Valid arities are: [3, 5, 9], but " + (a.length + 1) + ' arguments provided.')
      if (!usable(img, 'drawImage')) return
      const n = a.map(v => +v)
      while (n.length < 8) n.push(0)
      this._p(CV.DRAW_IMAGE, img, a.length, ...n)
    }
    createPattern(img, rep) {
      needArgs(2, arguments, 'createPattern')
      rep = rep === null || rep === '' ? 'repeat' : String(rep)
      if (!['repeat', 'repeat-x', 'repeat-y', 'no-repeat'].includes(rep)) throw domErr('SyntaxError', "Failed to execute 'createPattern': The provided type ('" + rep + "') is not one of 'repeat', 'no-repeat', 'repeat-x', or 'repeat-y'.")
      if (!usable(img, 'createPattern')) return null
      // A canvas pattern keeps the canvas as it is now.
      if (img && img.tagName === 'CANVAS') { const copy = new ImageData(img.width, img.height); const d = img.getContext('2d').getImageData(0, 0, img.width, img.height); copy.data.set(d.data); img = copy }
      return new CanvasPattern(img, rep)
    }
    // gradients
    createLinearGradient(x0, y0, x1, y1) {
      needArgs(4, arguments, 'createLinearGradient')
      const a = [+x0, +y0, +x1, +y1]
      if (!fin(...a)) throw new TypeError("Failed to execute 'createLinearGradient': The provided double value is non-finite.")
      return new CanvasGradient('linear', a)
    }
    createRadialGradient(x0, y0, r0, x1, y1, r1) {
      needArgs(6, arguments, 'createRadialGradient')
      const a = [+x0, +y0, +r0, +x1, +y1, +r1]
      if (!fin(...a)) throw new TypeError("Failed to execute 'createRadialGradient': The provided double value is non-finite.")
      if (a[2] < 0 || a[5] < 0) throw domErr('IndexSizeError', 'The radius provided is negative.')
      return new CanvasGradient('radial', a)
    }
    createConicGradient(angle, x, y) {
      needArgs(3, arguments, 'createConicGradient')
      const a = [+angle, +x, +y]
      if (!fin(...a)) throw new TypeError("Failed to execute 'createConicGradient': The provided double value is non-finite.")
      return new CanvasGradient('conic', a)
    }
    // pixels
    createImageData(w, h) {
      if (w instanceof ImageData) return new ImageData(w.width, w.height)
      needArgs(2, arguments, 'createImageData')
      return new ImageData(w, h)
    }
    getImageData(x, y, w, h) {
      needArgs(4, arguments, 'getImageData')
      x = Math.trunc(+x); y = Math.trunc(+y); w = Math.trunc(+w); h = Math.trunc(+h)
      if (!fin(x, y, w, h)) throw new TypeError("Failed to execute 'getImageData': The provided double value is non-finite.")
      if (!w || !h) throw domErr('IndexSizeError', "Failed to execute 'getImageData': The source " + (w ? 'height' : 'width') + ' is 0.')
      if (w < 0) { x += w; w = -w }
      if (h < 0) { y += h; h = -h }
      if (w * h > 1 << 28) throw new RangeError('Out of memory at ImageData creation')
      return new ImageData(this._ask(CV.GET, x, y, w, h), w)
    }
    putImageData(d, dx, dy, x, y, w, h) {
      if (!(d instanceof ImageData)) throw new TypeError("Failed to execute 'putImageData': parameter 1 is not of type 'ImageData'.")
      needArgs(3, arguments, 'putImageData')
      if (arguments.length === 3) { x = 0; y = 0; w = d.width; h = d.height }
      const n = [dx, dy, x, y, w, h].map(v => Math.trunc(+v))
      if (!fin(...n)) throw new TypeError("Failed to execute 'putImageData': The provided double value is non-finite.")
      this._p(CV.PUT, d.data, d.width, d.height, ...n)
    }
    setLineDash(d) {
      const list = Array.from(d || [], v => +v)
      if (list.some(v => !Number.isFinite(v) || v < 0)) return
      if (list.length % 2) list.push(...list)
      this._s.dash = list
      this._p(CV.DASH, list.slice())
    }
    getLineDash() { return this._s.dash.slice() }
    drawFocusIfNeeded() {}
    scrollPathIntoView() {}
  }
  Object.assign(CanvasRenderingContext2D.prototype, pathMethods)
  for (const k in props) {
    const [idx, , check] = props[k]
    Object.defineProperty(CanvasRenderingContext2D.prototype, k, {
      get() { return this._s[k] },
      set(v) { v = check(v, this); if (v === undefined) return; this._s[k] = v; this._p(CV.SET, idx, v) },
      configurable: true, enumerable: true,
    })
  }
  for (const k in quiet) {
    const check = quiet[k][1]
    Object.defineProperty(CanvasRenderingContext2D.prototype, k, { get() { return this._s[k] }, set(v) { v = check(v); if (v !== undefined) this._s[k] = v }, configurable: true, enumerable: true })
  }
  g.CanvasRenderingContext2D = CanvasRenderingContext2D
  g.CanvasGradient = CanvasGradient
  g.CanvasPattern = CanvasPattern
  g.Path2D = Path2D
  g.ImageData = ImageData
  g.TextMetrics = TextMetrics
  class ImageBitmap {
    constructor(src, w, h) { this._bitmap = true; this._src = src; this.width = w; this.height = h }
    close() { this._src = null; this.width = 0; this.height = 0 }
  }
  g.ImageBitmap = ImageBitmap
  g.createImageBitmap = function (src) {
    return new Promise((resolve, reject) => {
      try {
        if (src instanceof ImageData) return resolve(Object.assign(new ImageBitmap(null, src.width, src.height), { data: src.data }))
        if (src && src.tagName === 'CANVAS') {
          const d = src.getContext('2d').getImageData(0, 0, src.width, src.height)
          return resolve(Object.assign(new ImageBitmap(null, d.width, d.height), { data: d.data }))
        }
        if (src instanceof ImageBitmap) return resolve(Object.assign(new ImageBitmap(src._src, src.width, src.height), { data: src.data }))
        if (src && src.tagName === 'IMG') return resolve(new ImageBitmap(src, src.naturalWidth || src.width, src.naturalHeight || src.height))
        if (src instanceof g.Blob) {
          const p = __picture(src._u8(), src.type)
          if (!p) return reject(new DOMException('The source image could not be decoded.', 'InvalidStateError'))
          return resolve(Object.assign(new ImageBitmap(null, p[0], p[1]), { data: p[2] }))
        }
        reject(new TypeError('The provided value is not of an image type.'))
      } catch (e) { reject(e) }
    })
  }
  {
    const EP = Object.getPrototypeOf(document.createElement('canvas'))
    EP.getContext = function (type) {
      if (this.tagName !== 'CANVAS') return null
      if (String(type) !== '2d') return null
      return this.__ctx || (this.__ctx = new CanvasRenderingContext2D(this))
    }
    EP.toDataURL = function () {
      if (!this.width || !this.height) return 'data:,'
      const ctx = this.__ctx || this.getContext('2d')
      return ctx._ask(CV.URL, 0)
    }
    EP.toBlob = function (cb, type) {
      if (typeof cb !== 'function') throw new TypeError("Failed to execute 'toBlob': The callback provided as parameter 1 is not a function.")
      const url = this.toDataURL(type)
      setTimeout(() => {
        if (url === 'data:,') return cb(null)
        const bin = atob(url.slice(url.indexOf(',') + 1))
        const u = new Uint8Array(bin.length)
        for (let i = 0; i < bin.length; i++) u[i] = bin.charCodeAt(i)
        cb(new g.Blob([u], { type: 'image/png' }))
      }, 0)
    }
    EP.transferControlToOffscreen = function () { throw domErr('NotSupportedError', 'OffscreenCanvas is not supported yet.') }
    EP.captureStream = function () { throw domErr('NotSupportedError', 'Canvas capture is not supported yet.') }
  }
  // ---------------------------------------------------------------- images
  // An <img>'s picture is fetched and decoded when the page needs to know about
  // it: a load or error listener, naturalWidth, complete, decode() or a canvas.
  // Otherwise the app loads pictures on its own, to show them.
  {
    const IP = Object.getPrototypeOf(document.createElement('img'))
    const find = (o, k) => { for (; o; o = Object.getPrototypeOf(o)) { const d = Object.getOwnPropertyDescriptor(o, k); if (d) return d } }
    const srcD = find(IP, 'src'), setAttr = IP.setAttribute, ael = IP.addEventListener
    const info = el => el.__pic || (el.__pic = { url: undefined, size: null, pending: false })
    // The picture's size, fetching it if its address changed ([w, h], or null if it can't be shown).
    const fetchPic = el => {
      const s = info(el), url = el.getAttribute('src')
      if (s.url !== url) { s.url = url; s.size = url ? __img(el) : null }
      return s.size
    }
    const wanted = el => el.__wantload || typeof el.onload === 'function' || typeof el.onerror === 'function' || el.hasAttribute('onload') || el.hasAttribute('onerror')
    const schedule = el => {
      const s = info(el)
      if (s.pending) return
      s.pending = true
      setTimeout(() => {
        s.pending = false
        if (el.getAttribute('src') === null || !wanted(el)) return
        el.dispatchEvent(new Event(fetchPic(el) ? 'load' : 'error'))
      }, 0)
    }
    Object.defineProperty(IP, 'src', { get() { return srcD.get.call(this) }, set(v) { srcD.set.call(this, v); if (this.tagName === 'IMG') schedule(this) }, configurable: true, enumerable: true })
    IP.setAttribute = function (k, v) { const r = setAttr.call(this, k, v); if (this.tagName === 'IMG' && String(k).toLowerCase() === 'src') schedule(this); return r }
    IP.addEventListener = function (t, ...rest) { if (this.tagName === 'IMG' && (t === 'load' || t === 'error')) this.__wantload = true; return ael.call(this, t, ...rest) }
    const sized = k => function () { if (this.tagName !== 'IMG') return 0; const s = fetchPic(this); return s ? s[k] : 0 }
    Object.defineProperty(IP, 'naturalWidth', { get: sized(0), configurable: true, enumerable: true })
    Object.defineProperty(IP, 'naturalHeight', { get: sized(1), configurable: true, enumerable: true })
    Object.defineProperty(IP, 'complete', { get() { if (this.tagName !== 'IMG') return true; if (!this.getAttribute('src')) return true; fetchPic(this); return true }, configurable: true, enumerable: true })
    IP.decode = function () {
      return new Promise((resolve, reject) => setTimeout(() => (this.getAttribute('src') && fetchPic(this) ? resolve() : reject(new DOMException('The source image cannot be decoded.', 'EncodingError'))), 0))
    }
    // Pictures in the page's markup with handlers: they load (and say so) once the page is parsed.
    document.addEventListener('DOMContentLoaded', () => {
      for (const img of document.getElementsByTagName('img')) if (img.getAttribute('src') !== null && wanted(img)) schedule(img)
    })
    g.Image = function Image(w, h) {
      const img = document.createElement('img')
      if (w !== undefined) img.setAttribute('width', String(w))
      if (h !== undefined) img.setAttribute('height', String(h))
      return img
    }
    g.Image.prototype = IP
  }

  // ------------------------------------------------- attribute nodes
  // Each element keeps one Attr per attribute, so the same one comes back;
  // an Attr whose attribute is gone has no owner and keeps its last value.
  const attrCache = new WeakMap()
  const attrKey = (ns, local) => (ns || '') + '\u0000' + local
  class Attr {
    constructor(name, value, owner, ns = null, prefix = null, local = name) { this._name = name; this._value = value; this._owner = owner || null; this._ns = ns; this._prefix = prefix; this._local = local }
    get ownerElement() {
      const o = this._owner
      if (!o) return null
      const m = attrCache.get(o)
      if (m && m.get(attrKey(this._ns, this._local)) === this && o.hasAttributeNS(this._ns, this._local)) return o
      this._owner = null
      return null
    }
    get name() { return this._name }
    get nodeName() { return this._name }
    get localName() { return this._local }
    get namespaceURI() { return this._ns }
    get prefix() { return this._prefix }
    get specified() { return true }
    get nodeType() { return 2 }
    get ownerDocument() { return this._owner ? this._owner.ownerDocument : document }
    get value() { const o = this.ownerElement; if (o) this._value = o.getAttributeNS(this._ns, this._local); return this._value }
    set value(v) { this._value = String(v); const o = this.ownerElement; if (o) o.setAttributeNS(this._ns, this._name, this._value) }
    get nodeValue() { return this.value }
    set nodeValue(v) { this.value = v === null ? '' : v }
    get textContent() { return this.value }
    set textContent(v) { this.value = v === null ? '' : v }
    cloneNode() { return new Attr(this._name, this.value, null, this._ns, this._prefix, this._local) }
    isEqualNode(o) { return !!o && o.nodeType === 2 && o.namespaceURI === this._ns && o.localName === this._local && o.value === this.value }
  }
  const attrFor = (el, a) => {
    let m = attrCache.get(el)
    if (!m) attrCache.set(el, (m = new Map()))
    const k = attrKey(a.namespaceURI, a.localName)
    let at = m.get(k)
    if (!at || at._name !== a.name || at._owner !== el) m.set(k, (at = new Attr(a.name, a.value, el, a.namespaceURI, a.prefix, a.localName)))
    else at._value = a.value
    return at
  }
  class NamedNodeMap {
    item(i) { return this[i] || null }
    getNamedItem(n) { n = String(n); if (this._lower) n = n.toLowerCase(); for (let i = 0; i < this.length; i++) if (this[i].name === n) return this[i]; return null }
    getNamedItemNS(ns, n) { ns = ns == null || ns === '' ? null : String(ns); n = String(n); for (let i = 0; i < this.length; i++) if (this[i].namespaceURI === ns && this[i].localName === n) return this[i]; return null }
    setNamedItem(a) { return this._el.setAttributeNode(a) }
    setNamedItemNS(a) { return this._el.setAttributeNodeNS(a) }
    removeNamedItem(n) { const a = this.getNamedItem(n); if (!a) throw new DOMException('no such attribute', 'NotFoundError'); return this._el.removeAttributeNode(a) }
    removeNamedItemNS(ns, n) { const a = this.getNamedItemNS(ns, n); if (!a) throw new DOMException('no such attribute', 'NotFoundError'); return this._el.removeAttributeNode(a) }
    [Symbol.iterator]() { return Array.prototype.values.call(this) }
  }
  g.Attr = Attr
  g.NamedNodeMap = NamedNodeMap
  {
    const EP = Element.prototype
    const lowers = el => el.namespaceURI === 'http://www.w3.org/1999/xhtml' && (!el.ownerDocument || el.ownerDocument.contentType === 'text/html')
    const pairs = Object.getOwnPropertyDescriptor(EP, 'attributes').get
    Object.defineProperty(EP, 'attributes', {
      configurable: true, enumerable: true,
      get() {
        const m = Object.create(NamedNodeMap.prototype), list = pairs.call(this)
        list.forEach((a, i) => {
          const at = attrFor(this, a)
          m[i] = at
          if (!(a.name in m)) Object.defineProperty(m, a.name, { value: at, configurable: true })
        })
        Object.defineProperty(m, 'length', { value: list.length })
        Object.defineProperty(m, '_el', { value: this })
        Object.defineProperty(m, '_lower', { value: lowers(this) })
        return m
      },
    })
    // Removing an attribute leaves its Attr with the value it had.
    const keep = el => { const m = attrCache.get(el); if (m) for (const at of m.values()) if (at._owner === el) void at.value }
    for (const k of ['removeAttribute', 'removeAttributeNS', 'toggleAttribute']) {
      const f = EP[k]
      EP[k] = function (...a) { keep(this); return f.apply(this, a) }
    }
    EP.getAttributeNode = function (n) { return this.attributes.getNamedItem(n) }
    EP.getAttributeNodeNS = function (ns, n) { return this.attributes.getNamedItemNS(ns, n) }
    EP.setAttributeNode = function (a) {
      if (!(a instanceof Attr)) throw new TypeError('not an Attr')
      const owner = a.ownerElement
      if (owner && owner !== this) throw new DOMException('the attribute is in use', 'InUseAttributeError')
      const old = this.getAttributeNodeNS(a.namespaceURI, a.localName)
      if (old === a) return a
      if (old) { void old.value; old._owner = null }
      const v = a._value == null ? '' : a._value
      if (old) this.setAttributeNS(a.namespaceURI, old.name, v)
      else this.setAttributeNS(a.namespaceURI, a.name, v)
      a._owner = this
      let m = attrCache.get(this)
      if (!m) attrCache.set(this, (m = new Map()))
      m.set(attrKey(a.namespaceURI, a.localName), a)
      return old
    }
    EP.setAttributeNodeNS = EP.setAttributeNode
    EP.removeAttributeNode = function (a) {
      if (!(a instanceof Attr) || a.ownerElement !== this) throw new DOMException('no such attribute', 'NotFoundError')
      void a.value
      this.removeAttributeNS(a.namespaceURI, a.localName)
      a._owner = null
      return a
    }
    const DP = Document.prototype
    DP.createAttribute = function (n) {
      n = String(n)
      if (!n || /[\t\n\f\r \0\/=>]/.test(n)) throw new DOMException('not a valid attribute name', 'InvalidCharacterError')
      return new Attr(this.contentType === 'text/html' ? n.toLowerCase() : n, '', null)
    }
    DP.createAttributeNS = function (ns, n) {
      ns = ns == null || ns === '' ? null : String(ns); n = String(n)
      // Reuses createElementNS's validation of the name against the namespace.
      const probe = this.createElementNS(ns, n)
      return new Attr(n, '', null, ns, probe.prefix, probe.localName)
    }
  }

  // ------------------------------------------------- interfaces, as in browsers
  // Nodes are EventTargets; window, navigator, history, screen and location are
  // instances of their interfaces, whose prototypes hold their members.
  Object.setPrototypeOf(Node.prototype, EventTarget.prototype)
  const iface = (name, obj, parent) => {
    const C = function () { throw new TypeError('Illegal constructor') }
    Object.defineProperty(C, 'name', { value: name })
    const proto = Object.create(parent ? parent.prototype : Object.prototype)
    Object.defineProperty(proto, 'constructor', { value: C, writable: true, configurable: true })
    Object.defineProperty(proto, Symbol.toStringTag, { value: name, configurable: true })
    C.prototype = proto
    if (obj) {
      // Methods move to the prototype; values stay on the object (they change).
      for (const k of Object.getOwnPropertyNames(obj)) {
        const d = Object.getOwnPropertyDescriptor(obj, k)
        if (typeof d.value === 'function' || d.get || d.set) {
          Object.defineProperty(proto, k, Object.assign(d, { configurable: true }))
          delete obj[k]
        }
      }
      Object.setPrototypeOf(obj, proto)
    }
    g[name] = C
    return C
  }
  {
    // navigator's fields are fixed: getters on Navigator.prototype, as in browsers.
    const nav = g.navigator, fields = {}
    for (const k of Object.keys(nav)) if (typeof nav[k] !== 'function') { fields[k] = nav[k]; delete nav[k] }
    const Navigator = iface('Navigator', nav)
    for (const k of Object.keys(fields)) Object.defineProperty(Navigator.prototype, k, { get() { return fields[k] }, configurable: true, enumerable: true })
  }
  iface('History', g.history)
  iface('Screen', g.screen)
  iface('Location', g.location)
  iface('Window', null, EventTarget)
  g.HTMLCollection = g.NodeList

  // ------------------------------------------------- tree walking
  const NodeFilter = {
    FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3,
    SHOW_ALL: 0xFFFFFFFF, SHOW_ELEMENT: 0x1, SHOW_ATTRIBUTE: 0x2, SHOW_TEXT: 0x4, SHOW_CDATA_SECTION: 0x8,
    SHOW_PROCESSING_INSTRUCTION: 0x40, SHOW_COMMENT: 0x80, SHOW_DOCUMENT: 0x100, SHOW_DOCUMENT_TYPE: 0x200, SHOW_DOCUMENT_FRAGMENT: 0x400,
  }
  g.NodeFilter = NodeFilter
  const judge = (w, n) => {
    if (!(w.whatToShow & (1 << (n.nodeType - 1)))) return 3
    const f = w.filter
    if (!f) return 1
    return (typeof f === 'function' ? f(n) : f.acceptNode(n)) || 1
  }
  const following = (n, root, skipKids) => {
    if (!skipKids && n.firstChild) return n.firstChild
    for (; n && n !== root; n = n.parentNode) if (n.nextSibling) return n.nextSibling
    return null
  }
  const preceding = (n, root) => {
    if (n === root) return null
    let p = n.previousSibling
    if (!p) return n.parentNode
    while (p.lastChild) p = p.lastChild
    return p
  }
  class TreeWalker {
    constructor(root, what, filter) { this.root = root; this.whatToShow = what === undefined ? 0xFFFFFFFF : what >>> 0; this.filter = filter || null; this.currentNode = root }
    nextNode() {
      let n = this.currentNode
      for (let skip = false; (n = following(n, this.root, skip));) {
        const r = judge(this, n)
        if (r === 1) return (this.currentNode = n)
        skip = r === 2
      }
      return null
    }
    previousNode() {
      for (let n = this.currentNode; (n = preceding(n, this.root));) if (judge(this, n) === 1) return (this.currentNode = n)
      return null
    }
    parentNode() {
      for (let n = this.currentNode; n && n !== this.root;) { n = n.parentNode; if (n && judge(this, n) === 1) return (this.currentNode = n) }
      return null
    }
    firstChild() { for (let n = this.currentNode.firstChild; n; n = n.nextSibling) if (judge(this, n) === 1) return (this.currentNode = n); return null }
    lastChild() { for (let n = this.currentNode.lastChild; n; n = n.previousSibling) if (judge(this, n) === 1) return (this.currentNode = n); return null }
    nextSibling() { for (let n = this.currentNode.nextSibling; n; n = n.nextSibling) if (judge(this, n) === 1) return (this.currentNode = n); return null }
    previousSibling() { for (let n = this.currentNode.previousSibling; n; n = n.previousSibling) if (judge(this, n) === 1) return (this.currentNode = n); return null }
  }
  class NodeIterator {
    constructor(root, what, filter) { this.root = root; this.whatToShow = what === undefined ? 0xFFFFFFFF : what >>> 0; this.filter = filter || null; this.referenceNode = root; this.pointerBeforeReferenceNode = true }
    nextNode() {
      let n = this.referenceNode
      if (this.pointerBeforeReferenceNode) { this.pointerBeforeReferenceNode = false; if (judge(this, n) === 1) return n }
      while ((n = following(n, this.root, false))) { this.referenceNode = n; if (judge(this, n) === 1) return n }
      return null
    }
    previousNode() {
      let n = this.referenceNode
      if (!this.pointerBeforeReferenceNode) { this.pointerBeforeReferenceNode = true; if (judge(this, n) === 1) return n }
      while ((n = preceding(n, this.root))) { this.referenceNode = n; if (judge(this, n) === 1) return n }
      return null
    }
    detach() {}
  }
  g.TreeWalker = TreeWalker
  g.NodeIterator = NodeIterator
  Document.prototype.createTreeWalker = (root, what, filter) => new TreeWalker(root, what, filter)
  Document.prototype.createNodeIterator = (root, what, filter) => new NodeIterator(root, what, filter)

  // ------------------------------------------------- node comparisons, hit tests
  Node.prototype.isEqualNode = function (o) {
    if (!o || o.nodeType !== this.nodeType || o.nodeName !== this.nodeName) return false
    if (this.nodeType === 10) return o.publicId === this.publicId && o.systemId === this.systemId
    if (this.nodeType === 1 && (o.namespaceURI !== this.namespaceURI || o.prefix !== this.prefix || o.localName !== this.localName)) return false
    if (this.nodeType !== 1) return o.nodeValue === this.nodeValue && (this.nodeType !== 11 && this.nodeType !== 9 || sameKids(this, o))
    const a = this.attributes, b = o.attributes
    if (a.length !== b.length) return false
    for (let i = 0; i < a.length; i++) if (o.getAttribute(a[i].name) !== a[i].value) return false
    return sameKids(this, o)
  }
  const sameKids = (x, y) => {
    const a = x.childNodes, b = y.childNodes
    if (a.length !== b.length) return false
    for (let i = 0; i < a.length; i++) if (!a[i].isEqualNode(b[i])) return false
    return true
  }
  // Without a laid-out hit test from scripts yet, the body is what's at any point on the page.
  Document.prototype.elementFromPoint = function (x, y) { return x >= 0 && y >= 0 && x <= g.innerWidth && y <= g.innerHeight ? this.body : null }
  Document.prototype.elementsFromPoint = function (x, y) { const e = this.elementFromPoint(x, y); return e ? [e, this.documentElement] : [] }
  Document.prototype.caretRangeFromPoint = () => null
  if (g.HTMLSelectElement) {
    const SP = HTMLSelectElement.prototype
    if (typeof SP.add !== 'function') SP.add = function (o, before) {
      const ref = typeof before === 'number' ? this.options[before] : before
      ref && ref.parentNode ? ref.parentNode.insertBefore(o, ref) : this.appendChild(o)
    }
  }

  // ------------------------------------------------- messaging
  class MessagePort extends EventTarget {
    constructor() { super(); this._other = null; this._open = false; this._queue = []; this.onmessage = null }
    postMessage(data) {
      const to = this._other
      if (!to) return
      const copy = typeof structuredClone === 'function' ? structuredClone(data) : data
      setTimeout(() => {
        if (!to._open && !to.onmessage) { to._queue.push(copy); return }
        to._deliver(copy)
      }, 0)
    }
    _deliver(data) { const e = new Event('message'); e.data = data; e.ports = []; this.dispatchEvent(e); if (typeof this.onmessage === 'function') this.onmessage(e) }
    start() { this._open = true; const q = this._queue; this._queue = []; for (const d of q) this._deliver(d) }
    close() { this._other = null }
    addEventListener(t, f, o) { super.addEventListener(t, f, o); if (t === 'message') this.start() }
  }
  g.MessagePort = MessagePort
  g.MessageChannel = class MessageChannel {
    constructor() { this.port1 = new MessagePort(); this.port2 = new MessagePort(); this.port1._other = this.port2; this.port2._other = this.port1 }
  }
  const channels = {}
  g.BroadcastChannel = class BroadcastChannel extends EventTarget {
    constructor(name) { super(); this.name = String(name); this.onmessage = null; (channels[this.name] = channels[this.name] || []).push(this) }
    postMessage(data) {
      for (const c of channels[this.name] || []) if (c !== this) setTimeout(() => { const e = new Event('message'); e.data = data; c.dispatchEvent(e); if (typeof c.onmessage === 'function') c.onmessage(e) }, 0)
    }
    close() { const l = channels[this.name] || []; const k = l.indexOf(this); if (k >= 0) l.splice(k, 1) }
  }

  // ------------------------------------------------- constructable style sheets
  class CSSRule { constructor(text, sheet) { this.cssText = text; this.parentStyleSheet = sheet; const m = /^\s*([^{]+)\{/.exec(text); this.selectorText = m ? m[1].trim() : ''; this.type = 1 } }
  g.CSSRule = CSSRule
  g.CSSStyleRule = CSSRule
  g.CSSStyleSheet = class CSSStyleSheet {
    constructor(opts) { this.cssRules = []; this.disabled = !!(opts && opts.disabled); this.media = { mediaText: (opts && opts.media) || '' }; this.ownerNode = null; this.href = null }
    get rules() { return this.cssRules }
    insertRule(text, at) { at = at === undefined ? 0 : at; this.cssRules.splice(at, 0, new CSSRule(String(text), this)); this._sync(); return at }
    deleteRule(at) { this.cssRules.splice(at, 1); this._sync() }
    addRule(sel, body, at) { return this.insertRule(sel + '{' + body + '}', at === undefined ? this.cssRules.length : at) }
    removeRule(at) { this.deleteRule(at || 0) }
    replaceSync(text) {
      this.cssRules = []
      let depth = 0, start = 0
      text = String(text)
      for (let i = 0; i < text.length; i++) {
        if (text[i] === '{') depth++
        else if (text[i] === '}' && --depth === 0) { this.cssRules.push(new CSSRule(text.slice(start, i + 1).trim(), this)); start = i + 1 }
      }
      this._sync()
    }
    replace(text) { this.replaceSync(text); return Promise.resolve(this) }
    // An adopted sheet is shown through a <style> Volant keeps for it.
    _sync() { if (this._style) this._style.textContent = this.disabled ? '' : this.cssRules.map(r => r.cssText).join('\n') }
  }
  {
    let adopted = []
    Object.defineProperty(Document.prototype, 'adoptedStyleSheets', {
      configurable: true,
      get() { return adopted },
      set(list) {
        for (const s of adopted) if (s._style && !list.includes(s)) { s._style.remove(); s._style = null }
        adopted = Array.from(list)
        for (const s of adopted) if (!s._style) { s._style = document.createElement('style'); (document.head || document.documentElement).appendChild(s._style); s._sync() }
      },
    })
  }

  // <a> and <area> expose the parts of their address, like location does.
  // HTMLHyperlinkElementUtils: the href attribute parsed against the document's base URL.
  const URL_PART = { origin: 1, protocol: 2, username: 3, password: 4, host: 5, hostname: 6, port: 7, pathname: 8, search: 9, hash: 10 }
  const linkURL = el => { const a = el.getAttribute('href'); return a === null ? null : __urlParse(a, document.baseURI) }
  for (const C of [g.HTMLAnchorElement, g.HTMLAreaElement]) {
    if (!C) continue
    for (const k of ['protocol', 'host', 'hostname', 'port', 'pathname', 'search', 'hash', 'origin', 'username', 'password']) {
      if (Object.getOwnPropertyDescriptor(C.prototype, k)) continue
      Object.defineProperty(C.prototype, k, {
        configurable: true, enumerable: true,
        get() { const r = linkURL(this); return r ? r[URL_PART[k]] : k === 'protocol' ? ':' : '' },
        set(v) { if (k === 'origin') return; const r = linkURL(this); if (r) this.setAttribute('href', __urlSet(r[0], k, String(v))[0]) },
      })
    }
  }

  // ---------------------------------------------------------------- CharacterData, Text, Comment
  const needs = (args, n, what) => { if (args.length < n) throw new TypeError(what + ' needs ' + n + ' argument' + (n > 1 ? 's' : '')) }
  const reconstruct = (name, parent, make) => {
    const P = g[name].prototype
    const C = function () { if (!new.target) throw new TypeError("Constructor " + name + " requires 'new'"); return make.apply(null, arguments) }
    C.prototype = P
    Object.defineProperty(P, 'constructor', { value: C, writable: true, configurable: true })
    Object.defineProperty(C, 'name', { value: name })
    if (parent) Object.setPrototypeOf(C, parent)
    g[name] = C
  }
  reconstruct('Text', g.CharacterData, (d) => document.createTextNode(d === undefined ? '' : d))
  reconstruct('Comment', g.CharacterData, (d) => document.createComment(d === undefined ? '' : d))
  reconstruct('DocumentFragment', g.Node, () => document.createDocumentFragment())
  Object.setPrototypeOf(g.CharacterData, g.Node)
  Object.setPrototypeOf(g.ProcessingInstruction, g.CharacterData)
  Object.setPrototypeOf(g.DocumentType, g.Node)
  const CD = g.CharacterData.prototype
  const offsetIn = (node, off) => {
    off = off >>> 0
    if (off > node.data.length) throw new DOMException('offset ' + off + ' is past the end', 'IndexSizeError')
    return off
  }
  CD.substringData = function (off, count) {
    needs(arguments, 2, 'substringData')
    off = offsetIn(this, off)
    return this.data.substring(off, off + (count >>> 0))
  }
  CD.replaceData = function (off, count, data) {
    needs(arguments, 3, 'replaceData')
    off = offsetIn(this, off)
    count = count >>> 0
    const d = this.data
    this.data = d.slice(0, off) + String(data) + d.slice(Math.min(d.length, off + count))
  }
  CD.appendData = function (data) { needs(arguments, 1, 'appendData'); this.data = this.data + String(data) }
  CD.insertData = function (off, data) { needs(arguments, 2, 'insertData'); this.replaceData(off, 0, data) }
  CD.deleteData = function (off, count) { needs(arguments, 2, 'deleteData'); this.replaceData(off, count, '') }
  g.Text.prototype.splitText = function (off) {
    needs(arguments, 1, 'splitText')
    off = offsetIn(this, off)
    const d = this.data
    const rest = document.createTextNode(d.slice(off))
    if (this.parentNode) this.parentNode.insertBefore(rest, this.nextSibling)
    this.data = d.slice(0, off)
    return rest
  }

  // ---------------------------------------------------------------- reflected attributes
  // HTML's IDL attributes that reflect content attributes, per interface:
  // [prop, type, attribute name (when not the lower-cased prop), extra]. Types:
  // s string, sn string ([LegacyNullToEmptyString]), u URL, b boolean, l long,
  // ul unsigned long, lul unsigned long > 0, cl clamped [min, max], d double,
  // e enumerated {k: keywords, m: missing default, i: invalid default, n: nullable}, t DOMTokenList.
  const REF = { k: ['', 'no-referrer', 'no-referrer-when-downgrade', 'same-origin', 'origin', 'strict-origin', 'origin-when-cross-origin', 'strict-origin-when-cross-origin', 'unsafe-url'] }
  const CORS = { k: ['anonymous', 'use-credentials'], m: null, i: 'anonymous', n: true }
  const LOADING = { k: ['lazy', 'eager'], m: 'eager', i: 'eager' }
  const ENCTYPE = { k: ['application/x-www-form-urlencoded', 'multipart/form-data', 'text/plain'], m: 'application/x-www-form-urlencoded', i: 'application/x-www-form-urlencoded' }
  const FMETHOD = { k: ['get', 'post', 'dialog'], m: 'get', i: 'get' }
  const CELL = [['align', 's'], ['ch', 's', 'char'], ['chOff', 's', 'charoff'], ['vAlign', 's']]
  const LINK = [['target', 's'], ['download', 's'], ['ping', 's'], ['rel', 's'], ['relList', 't', 'rel'], ['hreflang', 's'], ['type', 's'], ['referrerPolicy', 'e', 0, REF]]
  const REFLECT = {
    HTMLElement: [['title', 's'], ['lang', 's'], ['accessKey', 's'], ['dir', 'e', 0, { k: ['ltr', 'rtl', 'auto'] }], ['hidden', 'b'], ['inert', 'b'], ['autofocus', 'b'],
      ['nonce', 's'], ['enterKeyHint', 'e', 0, { k: ['enter', 'done', 'go', 'next', 'previous', 'search', 'send'] }],
      ['inputMode', 'e', 0, { k: ['none', 'text', 'tel', 'url', 'email', 'numeric', 'decimal', 'search'] }]],
    HTMLAnchorElement: LINK.concat([['coords', 's'], ['charset', 's'], ['name', 's'], ['rev', 's'], ['shape', 's']]),
    HTMLAreaElement: LINK.filter(a => a[0] !== 'hreflang' && a[0] !== 'type').concat([['alt', 's'], ['coords', 's'], ['shape', 's'], ['noHref', 'b']]),
    HTMLBaseElement: [['target', 's']],
    HTMLQuoteElement: [['cite', 'u']],
    HTMLModElement: [['cite', 'u'], ['dateTime', 's']],
    HTMLBodyElement: [['text', 'sn'], ['link', 'sn'], ['vLink', 'sn', 'vlink'], ['aLink', 'sn', 'alink'], ['bgColor', 'sn', 'bgcolor'], ['background', 's']],
    HTMLBRElement: [['clear', 's']],
    HTMLButtonElement: [['disabled', 'b'], ['formAction', 'u', 'formaction'], ['formEnctype', 'e', 'formenctype', Object.assign({}, ENCTYPE, { m: '' })],
      ['formMethod', 'e', 'formmethod', Object.assign({}, FMETHOD, { m: '' })], ['formNoValidate', 'b', 'formnovalidate'], ['formTarget', 's', 'formtarget'],
      ['name', 's'], ['type', 'e', 0, { k: ['submit', 'reset', 'button'], m: 'submit', i: 'submit' }], ['value', 's']],
    // HTMLCanvasElement's width and height stay native: setting them clears the canvas.
    HTMLDataElement: [['value', 's']],
    HTMLDetailsElement: [['open', 'b'], ['name', 's']],
    HTMLDialogElement: [['open', 'b']],
    HTMLDivElement: [['align', 's']],
    HTMLDListElement: [['compact', 'b']],
    HTMLDirectoryElement: [['compact', 'b']],
    HTMLMenuElement: [['compact', 'b']],
    HTMLEmbedElement: [['src', 'u'], ['type', 's'], ['width', 's'], ['height', 's'], ['align', 's'], ['name', 's']],
    HTMLFieldSetElement: [['disabled', 'b'], ['name', 's']],
    HTMLFontElement: [['color', 'sn'], ['face', 's'], ['size', 's']],
    HTMLFormElement: [['acceptCharset', 's', 'accept-charset'], ['autocomplete', 'e', 0, { k: ['on', 'off'], m: 'on', i: 'on' }], ['enctype', 'e', 0, ENCTYPE],
      ['encoding', 'e', 'enctype', ENCTYPE], ['method', 'e', 0, FMETHOD], ['name', 's'], ['noValidate', 'b', 'novalidate'], ['target', 's'], ['rel', 's'], ['relList', 't', 'rel']],
    HTMLHeadingElement: [['align', 's']],
    HTMLHRElement: [['align', 's'], ['color', 's'], ['noShade', 'b', 'noshade'], ['size', 's'], ['width', 's']],
    HTMLHtmlElement: [['version', 's']],
    HTMLIFrameElement: [['src', 'u'], ['srcdoc', 's'], ['name', 's'], ['sandbox', 't'], ['allow', 's'], ['allowFullscreen', 'b', 'allowfullscreen'], ['width', 's'], ['height', 's'],
      ['referrerPolicy', 'e', 0, REF], ['loading', 'e', 0, LOADING], ['align', 's'], ['scrolling', 's'], ['frameBorder', 's', 'frameborder'], ['longDesc', 'u', 'longdesc'],
      ['marginHeight', 'sn', 'marginheight'], ['marginWidth', 'sn', 'marginwidth']],
    HTMLImageElement: [['alt', 's'], ['src', 'u'], ['srcset', 's'], ['sizes', 's'], ['crossOrigin', 'e', 'crossorigin', CORS], ['useMap', 's', 'usemap'], ['isMap', 'b', 'ismap'],
      ['referrerPolicy', 'e', 0, REF], ['decoding', 'e', 0, { k: ['sync', 'async', 'auto'], m: 'auto', i: 'auto' }], ['loading', 'e', 0, LOADING], ['name', 's'],
      ['lowsrc', 'u'], ['align', 's'], ['hspace', 'ul'], ['vspace', 'ul'], ['longDesc', 'u', 'longdesc'], ['border', 'sn']],
    HTMLInputElement: [['accept', 's'], ['alt', 's'], ['defaultChecked', 'b', 'checked'], ['dirName', 's', 'dirname'], ['disabled', 'b'], ['formAction', 'u', 'formaction'],
      ['formNoValidate', 'b', 'formnovalidate'], ['formTarget', 's', 'formtarget'], ['height', 'ul'], ['max', 's'], ['min', 's'], ['multiple', 'b'], ['name', 's'],
      ['pattern', 's'], ['placeholder', 's'], ['readOnly', 'b', 'readonly'], ['required', 'b'], ['size', 'lul', 0, 20], ['src', 'u'], ['step', 's'],
      ['type', 'e', 0, { k: ['hidden', 'text', 'search', 'tel', 'url', 'email', 'password', 'date', 'month', 'week', 'time', 'datetime-local', 'number', 'range', 'color', 'checkbox', 'radio', 'file', 'submit', 'image', 'reset', 'button'], m: 'text', i: 'text' }],
      ['defaultValue', 's', 'value'], ['width', 'ul'], ['align', 's'], ['useMap', 's', 'usemap']],
    HTMLLabelElement: [['htmlFor', 's', 'for']],
    HTMLLegendElement: [['align', 's']],
    HTMLLIElement: [['value', 'l'], ['type', 's']],
    HTMLLinkElement: [['href', 'u'], ['crossOrigin', 'e', 'crossorigin', CORS], ['rel', 's'], ['relList', 't', 'rel'], ['media', 's'], ['integrity', 's'], ['hreflang', 's'],
      ['type', 's'], ['sizes', 't'], ['referrerPolicy', 'e', 0, REF], ['charset', 's'], ['rev', 's'], ['target', 's'], ['imageSrcset', 's', 'imagesrcset'],
      ['imageSizes', 's', 'imagesizes'], ['blocking', 't']],
    HTMLMapElement: [['name', 's']],
    HTMLMetaElement: [['name', 's'], ['httpEquiv', 's', 'http-equiv'], ['content', 's'], ['media', 's'], ['scheme', 's']],
    HTMLObjectElement: [['data', 'u'], ['type', 's'], ['name', 's'], ['useMap', 's', 'usemap'], ['width', 's'], ['height', 's'], ['align', 's'], ['archive', 's'], ['code', 's'],
      ['declare', 'b'], ['hspace', 'ul'], ['standby', 's'], ['vspace', 'ul'], ['codeBase', 'u', 'codebase'], ['codeType', 's', 'codetype'], ['border', 'sn']],
    HTMLOListElement: [['reversed', 'b'], ['start', 'l', 0, 1], ['type', 's'], ['compact', 'b']],
    HTMLOptGroupElement: [['disabled', 'b'], ['label', 's']],
    HTMLOptionElement: [['disabled', 'b'], ['defaultSelected', 'b', 'selected']],
    HTMLOutputElement: [['htmlFor', 't', 'for'], ['name', 's']],
    HTMLParagraphElement: [['align', 's']],
    HTMLParamElement: [['name', 's'], ['value', 's'], ['type', 's'], ['valueType', 's', 'valuetype']],
    HTMLPreElement: [['width', 'l']],
    HTMLScriptElement: [['src', 'u'], ['type', 's'], ['noModule', 'b', 'nomodule'], ['defer', 'b'], ['crossOrigin', 'e', 'crossorigin', CORS], ['integrity', 's'],
      ['referrerPolicy', 'e', 0, REF], ['charset', 's'], ['event', 's'], ['htmlFor', 's', 'for'], ['blocking', 't']],
    HTMLSelectElement: [['disabled', 'b'], ['multiple', 'b'], ['name', 's'], ['required', 'b'], ['size', 'ul', 0, 0]],
    HTMLSlotElement: [['name', 's']],
    HTMLSourceElement: [['src', 'u'], ['type', 's'], ['srcset', 's'], ['sizes', 's'], ['media', 's'], ['width', 'ul'], ['height', 'ul']],
    HTMLStyleElement: [['media', 's'], ['type', 's'], ['blocking', 't']],
    HTMLTableElement: [['align', 's'], ['border', 's'], ['frame', 's'], ['rules', 's'], ['summary', 's'], ['width', 's'], ['bgColor', 'sn', 'bgcolor'],
      ['cellPadding', 'sn', 'cellpadding'], ['cellSpacing', 'sn', 'cellspacing']],
    HTMLTableCaptionElement: [['align', 's']],
    HTMLTableColElement: [['span', 'cl', 0, [1, 1000, 1]], ['width', 's']].concat(CELL),
    HTMLTableSectionElement: CELL,
    HTMLTableRowElement: CELL.concat([['bgColor', 'sn', 'bgcolor']]),
    HTMLTableCellElement: [['colSpan', 'cl', 'colspan', [1, 1000, 1]], ['rowSpan', 'cl', 'rowspan', [0, 65534, 1]], ['headers', 's'], ['abbr', 's'],
      ['scope', 'e', 0, { k: ['row', 'col', 'rowgroup', 'colgroup'] }], ['axis', 's'], ['height', 's'], ['width', 's'], ['noWrap', 'b', 'nowrap'],
      ['bgColor', 'sn', 'bgcolor']].concat(CELL),
    HTMLTextAreaElement: [['cols', 'lul', 0, 20], ['dirName', 's', 'dirname'], ['disabled', 'b'], ['name', 's'], ['placeholder', 's'], ['readOnly', 'b', 'readonly'],
      ['required', 'b'], ['rows', 'lul', 0, 2], ['wrap', 's']],
    HTMLTimeElement: [['dateTime', 's', 'datetime']],
    HTMLTrackElement: [['kind', 'e', 0, { k: ['subtitles', 'captions', 'descriptions', 'chapters', 'metadata'], m: 'subtitles', i: 'metadata' }], ['src', 'u'],
      ['srclang', 's'], ['label', 's'], ['default', 'b']],
    HTMLUListElement: [['compact', 'b'], ['type', 's']],
    HTMLVideoElement: [['width', 'ul'], ['height', 'ul'], ['poster', 'u'], ['playsInline', 'b', 'playsinline']],
  }
  for (const m of ['HTMLVideoElement', 'HTMLAudioElement']) REFLECT[m] = (REFLECT[m] || []).concat([['src', 'u'], ['crossOrigin', 'e', 'crossorigin', CORS],
    ['autoplay', 'b'], ['loop', 'b'], ['controls', 'b'], ['defaultMuted', 'b', 'muted']])
  {
    const MAX = 2147483647
    const lower = v => v.replace(/[A-Z]/g, c => c.toLowerCase())
    // HTML's rules for parsing integers (and non-negative ones), and floating-point numbers.
    const int = v => { const m = /^[\t\n\f\r ]*([-+]?)([0-9]+)/.exec(v); return m ? (m[1] === '-' ? -1 : 1) * Number(m[2]) : null }
    const float = v => { const m = /^[\t\n\f\r ]*(-?(?:[0-9]+(?:\.[0-9]+)?|\.[0-9]+)(?:[eE][-+]?[0-9]+)?)/.exec(v); if (!m) return null; const n = Number(m[1]); return isFinite(n) ? (n === 0 ? 0 : n) : null }
    const str = (el, a) => { const v = el.getAttribute(a); return v === null ? '' : v }
    const kinds = {
      s: (a) => ({ get() { return str(this, a) }, set(v) { this.setAttribute(a, String(v)) } }),
      sn: (a) => ({ get() { return str(this, a) }, set(v) { this.setAttribute(a, v === null ? '' : String(v)) } }),
      u: (a) => ({
        get() { const v = this.getAttribute(a); if (v === null) return ''; try { return new URL(v, document.baseURI).href } catch (e) { return v } },
        set(v) { this.setAttribute(a, String(v)) },
      }),
      b: (a) => ({ get() { return this.hasAttribute(a) }, set(v) { if (v) this.setAttribute(a, ''); else this.removeAttribute(a) } }),
      l: (a, def = 0) => ({
        get() { const n = int(str(this, a)); return n === null || n < -MAX - 1 || n > MAX ? def : n },
        set(v) { this.setAttribute(a, String(+v | 0)) },
      }),
      ul: (a, def = 0) => ({
        get() { const n = int(str(this, a)); return n === null || n < 0 || n > MAX ? def : n },
        set(v) { let n = +v >>> 0; if (n > MAX) n = def; this.setAttribute(a, String(n)) },
      }),
      lul: (a, def = 1) => ({
        get() { const n = int(str(this, a)); return n === null || n <= 0 || n > MAX ? def : n },
        set(v) { let n = +v >>> 0; if (n === 0) throw new DOMException('the value must be greater than zero', 'IndexSizeError'); if (n > MAX) n = def; this.setAttribute(a, String(n)) },
      }),
      cl: (a, [min, max, def]) => ({
        get() { const n = int(str(this, a)); return n === null || n < 0 || n > MAX ? def : Math.min(max, Math.max(min, n)) },
        set(v) { let n = +v >>> 0; if (n > MAX) n = def; this.setAttribute(a, String(n)) },
      }),
      d: (a, def = 0) => ({
        get() { const n = float(str(this, a)); return n === null ? def : n },
        set(v) { const n = +v; if (!isFinite(n)) throw new TypeError('the value must be finite'); this.setAttribute(a, String(n)) },
      }),
      e: (a, spec) => ({
        get() {
          const v = this.getAttribute(a)
          if (v === null) return spec.m === undefined ? '' : spec.m
          const k = lower(v)
          if (spec.k.includes(k)) return k
          return spec.i === undefined ? '' : spec.i
        },
        set(v) { if (spec.n && v === null) this.removeAttribute(a); else this.setAttribute(a, String(v)) },
      }),
      t: (a) => ({ get() { return __tokenList(this, a) }, set(v) { this.setAttribute(a, String(v)) } }),
      // A long limited to non-negative numbers (maxLength, minLength).
      ll: (a, def = -1) => ({
        get() { const n = int(str(this, a)); return n === null || n < 0 || n > MAX ? def : n },
        set(v) { const n = +v | 0; if (n < 0) throw new DOMException('the value must not be negative', 'IndexSizeError'); this.setAttribute(a, String(n)) },
      }),
      // A nullable DOMString (role and the ARIA attributes).
      ns: (a) => ({ get() { return this.getAttribute(a) }, set(v) { if (v == null) this.removeAttribute(a); else this.setAttribute(a, String(v)) } }),
    }
    // tabIndex: 0 for elements that take focus by default, -1 for the rest.
    const focusable = el => {
      const t = el.localName
      if (el.namespaceURI === 'http://www.w3.org/2000/svg') return t === 'a' && (el.hasAttribute('href') || el.hasAttribute('xlink:href'))
      if ((t === 'a' || t === 'area') && el.hasAttribute('href')) return true
      if (['button', 'frame', 'iframe', 'input', 'object', 'select', 'textarea'].includes(t)) return true
      if (t === 'summary' && el.parentNode && el.parentNode.localName === 'details' && Array.prototype.find.call(el.parentNode.children, c => c.localName === 'summary') === el) return true
      return el.isContentEditable === true
    }
    const tabIndex = {
      get() { const n = int(str(this, 'tabindex')); return n === null || n < -MAX - 1 || n > MAX ? (focusable(this) ? 0 : -1) : n },
      set(v) { this.setAttribute('tabindex', String(+v | 0)) },
    }
    for (const C of [g.HTMLElement, g.SVGElement, g.MathMLElement]) if (C) Object.defineProperty(C.prototype, 'tabIndex', { get: tabIndex.get, set: tabIndex.set, configurable: true, enumerable: true })
    for (const C of ['HTMLInputElement', 'HTMLTextAreaElement']) REFLECT[C] = (REFLECT[C] || []).concat([['maxLength', 'll'], ['minLength', 'll']])
    // ARIA reflection (ARIAMixin), on every element.
    REFLECT.Element = [['role', 'ns'], ...('ActiveDescendant Atomic AutoComplete BrailleLabel BrailleRoleDescription Busy Checked ColCount ColIndex ColIndexText ColSpan Current ' +
      'Description Disabled Expanded HasPopup Hidden Invalid KeyShortcuts Label Level Live Modal MultiLine MultiSelectable Orientation Placeholder PosInSet Pressed ReadOnly ' +
      'Relevant Required RoleDescription RowCount RowIndex RowIndexText RowSpan Selected SetSize Sort ValueMax ValueMin ValueNow ValueText').split(' ')
      .filter(n => n !== 'ActiveDescendant').map(n => ['aria' + n, 'ns', 'aria-' + n.toLowerCase()])]
    // Native accessors that already reflect the attribute exactly stay as they are.
    const plain = { s: 1, u: 1, b: 1 }
    for (const iface in REFLECT) {
      const C = g[iface]
      if (!C || !C.prototype) continue
      for (const [prop, type, attrName, extra] of REFLECT[iface]) {
        if (plain[type] && Object.getOwnPropertyDescriptor(C.prototype, prop)) continue
        const d = kinds[type](attrName || prop.toLowerCase(), extra)
        Object.defineProperty(C.prototype, prop, { get: d.get, set: d.set, configurable: true, enumerable: true })
      }
    }
  }

  // ---------------------------------------------------------------- frames
  // Each iframe gets a document of its own: about:blank, its srcdoc, or its
  // src when that's same-origin (scripts in it don't run). Its window shares
  // this one's globals, with its own document and location.
  {
    const frames = new WeakMap()
    const frameOf = el => {
      let f = frames.get(el)
      if (f) return f
      f = { el, url: 'about:blank', doc: __parseDocument(''), blocked: false, gen: 0 }
      const win = Object.create(g)
      const loc = {
        get href() { return f.url },
        set href(v) { navigate(f, v) },
        get origin() { const p = __urlParse(f.url); return p ? p[1] : 'null' },
        get protocol() { const p = __urlParse(f.url); return p ? p[2] : '' },
        get pathname() { const p = __urlParse(f.url); return p ? p[8] : '' },
        get search() { const p = __urlParse(f.url); return p ? p[9] : '' },
        get hash() { const p = __urlParse(f.url); return p ? p[10] : '' },
        assign(v) { navigate(f, v) }, replace(v) { navigate(f, v) }, reload() {},
        toString() { return f.url },
      }
      Object.defineProperties(win, {
        document: { get: () => (f.blocked ? null : f.doc), configurable: true },
        window: { get: () => win, configurable: true },
        self: { get: () => win, configurable: true },
        frames: { get: () => win, configurable: true },
        parent: { get: () => g, configurable: true },
        top: { get: () => g.top, configurable: true },
        frameElement: { get: () => el, configurable: true },
        location: { get: () => loc, set: v => navigate(f, v), configurable: true },
      })
      f.win = win
      frames.set(el, f)
      return f
    }
    const fireLoad = f => setTimeout(() => f.el.dispatchEvent(new Event('load')), 0)
    const navigate = (f, v) => {
      v = String(v)
      const p = __urlParse(v, document.baseURI)
      if (!p) throw new DOMException(v + ' is not a valid URL.', 'SyntaxError')
      load(f, p[0])
    }
    const load = (f, href) => {
      const gen = ++f.gen
      if (href === 'about:blank') { f.url = href; f.blocked = false; f.doc = __parseDocument(''); fireLoad(f); return }
      const p = __urlParse(href)
      if (!p || p[1] !== location.origin) { f.url = href; f.blocked = true; fireLoad(f); return }
      fetch(href).then(r => r.text().then(t => {
        if (gen !== f.gen) return
        const ty = (r.headers.get('content-type') || 'text/html').split(';')[0].trim().toLowerCase()
        f.url = r.url || href
        f.blocked = false
        f.doc = __parseDocument(t, ty, f.url)
        f.el.dispatchEvent(new Event('load'))
      })).catch(() => { if (gen === f.gen) fireLoad(f) })
    }
    const start = el => {
      const f = frameOf(el)
      const sd = el.getAttribute('srcdoc')
      if (sd !== null) { f.gen++; f.url = 'about:srcdoc'; f.blocked = false; f.doc = __parseDocument(sd); fireLoad(f); return }
      const src = el.getAttribute('src')
      const p = src && src.trim() ? __urlParse(src, document.baseURI) : null
      load(f, p ? p[0] : 'about:blank')
    }
    g.__frameInserted = el => { frames.delete(el); start(el) }
    const IF = g.HTMLIFrameElement && g.HTMLIFrameElement.prototype
    if (IF) {
      Object.defineProperty(IF, 'contentDocument', { get() { if (!this.isConnected) return null; const f = frameOf(this); return f.blocked ? null : f.doc }, configurable: true, enumerable: true })
      Object.defineProperty(IF, 'contentWindow', { get() { return this.isConnected ? frameOf(this).win : null }, configurable: true, enumerable: true })
      for (const a of ['src', 'srcdoc']) {
        const d = Object.getOwnPropertyDescriptor(IF, a)
        if (d && d.set) Object.defineProperty(IF, a, { get: d.get, set(v) { d.set.call(this, v); if (this.isConnected) start(this) }, configurable: true, enumerable: true })
      }
    }
    // Frames already in the page when it was parsed: they load before the window's load event.
    g.__startFrames = () => { for (const el of document.getElementsByTagName('iframe')) if (!frames.has(el)) start(el) }
  }
})(window)
