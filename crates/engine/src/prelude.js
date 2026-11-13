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
    blob() { return Promise.resolve(new Blob([this._body], { type: this.headers.get('content-type') || '' })) }
    arrayBuffer() { return Promise.resolve(new TextEncoder().encode(this._body).buffer) }
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
  g.Blob = class Blob {
    constructor(parts, opts) { this._t = (parts || []).map(String).join(''); this.type = (opts && opts.type) || ''; this.size = this._t.length }
    text() { return Promise.resolve(this._t) }
    arrayBuffer() { return Promise.resolve(new TextEncoder().encode(this._t).buffer) }
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

  // ---------------------------------------------------------------- canvas
  // A 2D context records what's drawn as a display list (transformed, curves
  // made into short lines); Volant draws the list into the canvas's box.
  const MAX_OPS = 20000
  class CanvasGradient { constructor() { this._stops = [] } addColorStop(o, c) { this._stops.push([o, String(c)]) } }
  class CanvasPattern { setTransform() {} }
  const paint = v => (v instanceof CanvasGradient ? (v._stops.length ? v._stops[0][1] : 'transparent') : v instanceof CanvasPattern ? '#888' : String(v))
  class CanvasRenderingContext2D {
    constructor(canvas) { this.canvas = canvas; this._reset() }
    _reset() {
      this._ops = []; this._stack = []; this._m = [1, 0, 0, 1, 0, 0]; this._path = []; this._sub = null
      this.fillStyle = '#000'; this.strokeStyle = '#000'; this.lineWidth = 1; this.globalAlpha = 1
      this.font = '10px sans-serif'; this.textAlign = 'start'; this.textBaseline = 'alphabetic'
      this.lineCap = 'butt'; this.lineJoin = 'miter'; this.miterLimit = 10; this.globalCompositeOperation = 'source-over'
      this.shadowBlur = 0; this.shadowColor = 'transparent'; this.shadowOffsetX = 0; this.shadowOffsetY = 0
      this.imageSmoothingEnabled = true; this.direction = 'inherit'; this.filter = 'none'; this._dash = []
    }
    _push(op) { if (this._ops.length < MAX_OPS) this._ops.push(op) }
    _pt(x, y) { const m = this._m; return [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]] }
    _scale() { const m = this._m; return Math.sqrt(Math.abs(m[0] * m[3] - m[1] * m[2])) || 1 }
    // state
    save() { this._stack.push([this._m.slice(), this.fillStyle, this.strokeStyle, this.lineWidth, this.globalAlpha, this.font, this.textAlign, this.textBaseline]) }
    restore() { const s = this._stack.pop(); if (s) [this._m, this.fillStyle, this.strokeStyle, this.lineWidth, this.globalAlpha, this.font, this.textAlign, this.textBaseline] = s }
    reset() { this._reset() }
    // transforms
    transform(a, b, c, d, e, f) { const m = this._m; this._m = [m[0] * a + m[2] * b, m[1] * a + m[3] * b, m[0] * c + m[2] * d, m[1] * c + m[3] * d, m[0] * e + m[2] * f + m[4], m[1] * e + m[3] * f + m[5]] }
    setTransform(a, b, c, d, e, f) { if (typeof a === 'object' && a) { ({ a, b, c, d, e, f } = a) } this._m = a === undefined ? [1, 0, 0, 1, 0, 0] : [a, b, c, d, e, f] }
    resetTransform() { this._m = [1, 0, 0, 1, 0, 0] }
    getTransform() { const [a, b, c, d, e, f] = this._m; return { a, b, c, d, e, f, is2D: true, isIdentity: a === 1 && b === 0 && c === 0 && d === 1 && e === 0 && f === 0 } }
    translate(x, y) { this.transform(1, 0, 0, 1, x, y) }
    scale(x, y) { this.transform(x, 0, 0, y, 0, 0) }
    rotate(r) { const c = Math.cos(r), s = Math.sin(r); this.transform(c, s, -s, c, 0, 0) }
    // paths
    beginPath() { this._path = []; this._sub = null }
    _start(x, y) { this._sub = [false, ...this._pt(x, y)]; this._path.push(this._sub); this._last = [x, y] }
    moveTo(x, y) { this._start(+x, +y) }
    lineTo(x, y) { x = +x; y = +y; if (!this._sub) this._start(x, y); else { this._sub.push(...this._pt(x, y)); this._last = [x, y] } }
    closePath() { if (this._sub) { this._sub[0] = true; const [x, y] = this._sub.length > 2 ? [this._sub[1], this._sub[2]] : [0, 0]; this._sub = null; this._closedAt = [x, y] } }
    rect(x, y, w, h) { this.moveTo(x, y); this.lineTo(x + w, y); this.lineTo(x + w, y + h); this.lineTo(x, y + h); this.closePath() }
    roundRect(x, y, w, h, r) {
      r = Math.min(Math.abs(Array.isArray(r) ? r[0] || 0 : +r || 0), Math.abs(w) / 2, Math.abs(h) / 2)
      if (!r) return this.rect(x, y, w, h)
      this.moveTo(x + r, y); this.arc(x + w - r, y + r, r, -Math.PI / 2, 0); this.arc(x + w - r, y + h - r, r, 0, Math.PI / 2)
      this.arc(x + r, y + h - r, r, Math.PI / 2, Math.PI); this.arc(x + r, y + r, r, Math.PI, Math.PI * 1.5); this.closePath()
    }
    arc(x, y, r, a0, a1, ccw) { this.ellipse(x, y, r, r, 0, a0, a1, ccw) }
    ellipse(x, y, rx, ry, rot, a0, a1, ccw) {
      let sweep = a1 - a0
      if (!ccw && sweep < 0) sweep = sweep % (2 * Math.PI) + 2 * Math.PI
      if (ccw && sweep > 0) sweep = sweep % (2 * Math.PI) - 2 * Math.PI
      if (Math.abs(a1 - a0) >= 2 * Math.PI) sweep = ccw ? -2 * Math.PI : 2 * Math.PI
      const n = Math.max(8, Math.min(128, Math.ceil(Math.abs(sweep) * Math.max(rx, ry) * this._scale() / 3)))
      const cr = Math.cos(rot || 0), sr = Math.sin(rot || 0)
      for (let k = 0; k <= n; k++) {
        const t = a0 + sweep * k / n, ex = rx * Math.cos(t), ey = ry * Math.sin(t)
        const px = x + ex * cr - ey * sr, py = y + ex * sr + ey * cr
        if (k === 0 && !this._sub) this._start(px, py); else this.lineTo(px, py)
      }
    }
    arcTo(x1, y1, x2, y2) { this.lineTo(x1, y1); this.lineTo(x2, y2) }
    quadraticCurveTo(cx, cy, x, y) {
      const [x0, y0] = this._last || [cx, cy]
      for (let k = 1; k <= 16; k++) { const t = k / 16, u = 1 - t; this.lineTo(u * u * x0 + 2 * u * t * cx + t * t * x, u * u * y0 + 2 * u * t * cy + t * t * y) }
    }
    bezierCurveTo(c1x, c1y, c2x, c2y, x, y) {
      const [x0, y0] = this._last || [c1x, c1y]
      for (let k = 1; k <= 20; k++) {
        const t = k / 20, u = 1 - t
        this.lineTo(u * u * u * x0 + 3 * u * u * t * c1x + 3 * u * t * t * c2x + t * t * t * x, u * u * u * y0 + 3 * u * u * t * c1y + 3 * u * t * t * c2y + t * t * t * y)
      }
    }
    _draw(fill, stroke) {
      const subs = this._path.filter(s => s.length >= 5).map(s => s.slice())
      if (subs.length) this._push(['p', fill ? paint(this.fillStyle) : null, stroke ? paint(this.strokeStyle) : null, this.lineWidth * this._scale(), this.globalAlpha, subs])
    }
    fill() { this._draw(true, false) }
    stroke() { this._draw(false, true) }
    clip() {}
    isPointInPath() { return false }
    isPointInStroke() { return false }
    // rectangles
    _rectPath(x, y, w, h) { const a = this._pt(x, y), b = this._pt(x + w, y), c = this._pt(x + w, y + h), d = this._pt(x, y + h); return [[true, ...a, ...b, ...c, ...d]] }
    fillRect(x, y, w, h) { this._push(['p', paint(this.fillStyle), null, 0, this.globalAlpha, this._rectPath(x, y, w, h)]) }
    strokeRect(x, y, w, h) { this._push(['p', null, paint(this.strokeStyle), this.lineWidth * this._scale(), this.globalAlpha, this._rectPath(x, y, w, h)]) }
    clearRect(x, y, w, h) {
      const cw = this.canvas.width, ch = this.canvas.height, [x0, y0] = this._pt(x, y), [x1, y1] = this._pt(x + w, y + h)
      // Clearing it all starts the list again; a smaller clear paints the page's background.
      if (Math.min(x0, x1) <= 0 && Math.min(y0, y1) <= 0 && Math.max(x0, x1) >= cw && Math.max(y0, y1) >= ch) this._ops = []
      else this._push(['c', ...this._rectPath(x, y, w, h)[0].slice(1)])
    }
    // text
    _text(t, x, y, fill) {
      const [px, py] = this._pt(x, y)
      this._push(['t', px, py, String(t), this.font, paint(fill ? this.fillStyle : this.strokeStyle), this.textAlign, this.textBaseline, this.globalAlpha, this._scale()])
    }
    fillText(t, x, y) { this._text(t, x, y, true) }
    strokeText(t, x, y) { this._text(t, x, y, false) }
    measureText(t) {
      const w = __measure(String(t), this.font), size = parseFloat((/(\d+(?:\.\d+)?)px/.exec(this.font) || [0, 10])[1])
      return { width: w, actualBoundingBoxLeft: 0, actualBoundingBoxRight: w, actualBoundingBoxAscent: size * 0.8, actualBoundingBoxDescent: size * 0.2, fontBoundingBoxAscent: size * 0.9, fontBoundingBoxDescent: size * 0.25 }
    }
    // images
    drawImage(img, ...a) {
      const src = img && (img.currentSrc || img.src || (img.getAttribute && img.getAttribute('src')))
      if (!src) return
      let [dx, dy, dw, dh] = a.length >= 8 ? a.slice(4) : a
      if (dw === undefined) { dw = img.naturalWidth || img.width || 0; dh = img.naturalHeight || img.height || 0 }
      const [x0, y0] = this._pt(dx, dy), [x1, y1] = this._pt(dx + dw, dy + dh)
      this._push(['i', String(src), Math.min(x0, x1), Math.min(y0, y1), Math.abs(x1 - x0), Math.abs(y1 - y0), this.globalAlpha])
    }
    // pixels (not kept: Volant draws vectors)
    createImageData(w, h) { if (typeof w === 'object') ({ width: w, height: h } = w); return { width: w, height: h, data: new Uint8ClampedArray(Math.max(0, w * h * 4)) } }
    getImageData(x, y, w, h) { return this.createImageData(w, h) }
    putImageData() {}
    createLinearGradient() { return new CanvasGradient() }
    createRadialGradient() { return new CanvasGradient() }
    createConicGradient() { return new CanvasGradient() }
    createPattern() { return new CanvasPattern() }
    setLineDash(d) { this._dash = Array.isArray(d) ? d.slice() : [] }
    getLineDash() { return this._dash.slice() }
    drawFocusIfNeeded() {}
  }
  g.CanvasRenderingContext2D = CanvasRenderingContext2D
  g.CanvasGradient = CanvasGradient
  g.CanvasPattern = CanvasPattern
  g.Path2D = class Path2D { constructor() { this._calls = [] } }
  {
    const EP = Object.getPrototypeOf(document.createElement('canvas'))
    EP.getContext = function (type) {
      if (this.tagName !== 'CANVAS' || String(type).toLowerCase() !== '2d') return null
      return this.__ctx || (this.__ctx = new CanvasRenderingContext2D(this))
    }
    EP.toDataURL = function () { return 'data:,' }
    EP.toBlob = function (cb) { setTimeout(() => cb && cb(new g.Blob([], { type: 'image/png' })), 0) }
    EP.transferControlToOffscreen = function () { return null }
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
