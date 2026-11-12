//! Volant's own downloads: filter lists, uBlock Origin's release, updates,
//! weather and site icons. The HTTP is volant-core's (`http`: requests,
//! redirects, chunked, gzip, limits, deadlines). Underneath, the encrypted
//! connection comes from macOS's Network framework, so certificate checks and
//! TLS stay the system's. Call these from background threads: they block.

use block2::RcBlock;
use std::ffi::{c_char, c_void, CString};
use std::sync::mpsc;
use std::time::Duration;
use volant_core::http::{self, Connection, Options, Response, Transport};

/// Network framework and libdispatch objects.
type Obj = *mut c_void;

#[link(name = "Network", kind = "framework")]
extern "C" {
    fn nw_endpoint_create_host(host: *const c_char, port: *const c_char) -> Obj;
    fn nw_parameters_create_secure_tcp(configure_tls: *const c_void, configure_tcp: *const c_void) -> Obj;
    static _nw_parameters_configure_protocol_default_configuration: *const c_void;
    static _nw_parameters_configure_protocol_disable: *const c_void;
    static _nw_content_context_default_message: Obj;
    fn nw_connection_create(endpoint: Obj, parameters: Obj) -> Obj;
    fn nw_connection_set_queue(connection: Obj, queue: Obj);
    fn nw_connection_set_state_changed_handler(connection: Obj, handler: *const c_void);
    fn nw_connection_start(connection: Obj);
    fn nw_connection_send(connection: Obj, content: Obj, context: Obj, is_complete: bool, completion: *const c_void);
    fn nw_connection_receive(connection: Obj, minimum: u32, maximum: u32, completion: *const c_void);
    fn nw_connection_cancel(connection: Obj);
    fn nw_error_get_error_domain(error: Obj) -> u32;
    fn nw_error_get_error_code(error: Obj) -> i32;
    fn nw_release(object: Obj);
}

extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: Obj) -> Obj;
    fn dispatch_data_create(buffer: *const c_void, size: usize, queue: Obj, destructor: *const c_void) -> Obj;
    fn dispatch_data_create_map(data: Obj, buffer: *mut *const c_void, size: *mut usize) -> Obj;
    fn dispatch_release(object: Obj);
}

// nw_connection_state_t
const WAITING: u32 = 1;
const READY: u32 = 3;
const FAILED: u32 = 4;
const CANCELLED: u32 = 5;
// nw_error_domain_t
const DOMAIN_POSIX: u32 = 1;
const DOMAIN_DNS: u32 = 2;
const DOMAIN_TLS: u32 = 3;

/// (domain, code) of an nw_error_t, if there is one.
fn error_of(e: Obj) -> Option<(u32, i32)> {
    (!e.is_null()).then(|| unsafe { (nw_error_get_error_domain(e), nw_error_get_error_code(e)) })
}

fn describe(host: &str, (domain, code): (u32, i32)) -> String {
    match domain {
        DOMAIN_DNS => format!("couldn't find {host} (DNS {code})"),
        DOMAIN_TLS => format!("the secure connection to {host} failed (TLS {code})"),
        DOMAIN_POSIX => format!("couldn't connect to {host} (error {code})"),
        _ => format!("couldn't connect to {host} ({domain}/{code})"),
    }
}

/// The peer closing the stream, as opposed to a real failure.
fn is_close((domain, code): (u32, i32)) -> bool {
    (domain == DOMAIN_POSIX && (code == 57 || code == 96)) || (domain == DOMAIN_TLS && code == -9816)
}

fn block_ptr<T: ?Sized>(b: &T) -> *const c_void {
    b as *const T as *const c_void
}

pub struct NwTransport;

pub struct NwConn {
    conn: Obj,
    queue: Obj,
    host: String,
    eof: bool,
}

// The handles are only touched from the thread that owns the NwConn; the
// Network framework itself is thread-safe.
unsafe impl Send for NwConn {}

impl Transport for NwTransport {
    type Conn = NwConn;

    fn connect(&self, host: &str, port: u16, tls: bool, timeout: Duration) -> Result<NwConn, String> {
        let c_host = CString::new(host).map_err(|_| "bad host name".to_string())?;
        let c_port = CString::new(port.to_string()).expect("digits");
        let (tx, rx) = mpsc::channel::<(u32, Option<(u32, i32)>)>();
        unsafe {
            let queue = dispatch_queue_create(c"app.volant.net".as_ptr(), std::ptr::null_mut());
            let endpoint = nw_endpoint_create_host(c_host.as_ptr(), c_port.as_ptr());
            let tls_config = if tls { _nw_parameters_configure_protocol_default_configuration } else { _nw_parameters_configure_protocol_disable };
            let params = nw_parameters_create_secure_tcp(tls_config, _nw_parameters_configure_protocol_default_configuration);
            let conn = nw_connection_create(endpoint, params);
            nw_release(endpoint);
            nw_release(params);
            if conn.is_null() {
                dispatch_release(queue);
                return Err(format!("couldn't connect to {host}"));
            }
            let state = RcBlock::new(move |s: u32, err: *mut c_void| {
                let _ = tx.send((s, error_of(err)));
            });
            nw_connection_set_queue(conn, queue);
            nw_connection_set_state_changed_handler(conn, block_ptr(&*state));
            nw_connection_start(conn);
            let me = NwConn { conn, queue, host: host.to_string(), eof: false };
            let deadline = std::time::Instant::now() + timeout;
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                match rx.recv_timeout(left) {
                    Ok((READY, _)) => return Ok(me),
                    // Waiting means no route right now (offline, DNS failing): don't hang on it.
                    Ok((WAITING, Some(e))) | Ok((FAILED, Some(e))) => return Err(describe(host, e)),
                    Ok((FAILED, None)) | Ok((CANCELLED, _)) => return Err(format!("couldn't connect to {host}")),
                    Ok(_) => continue,
                    Err(_) => return Err(format!("{host} didn't answer in time")),
                }
            }
        }
    }
}

impl Connection for NwConn {
    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        let (tx, rx) = mpsc::channel::<Option<(u32, i32)>>();
        let done = RcBlock::new(move |err: *mut c_void| {
            let _ = tx.send(error_of(err));
        });
        unsafe {
            // A null destructor means dispatch copies the bytes.
            let content = dispatch_data_create(data.as_ptr().cast(), data.len(), self.queue, std::ptr::null());
            nw_connection_send(self.conn, content, _nw_content_context_default_message, true, block_ptr(&*done));
            dispatch_release(content);
        }
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(None) => Ok(()),
            Ok(Some(e)) => Err(describe(&self.host, e)),
            Err(_) => Err(format!("{} didn't take the request in time", self.host)),
        }
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String> {
        if self.eof {
            return Ok(None);
        }
        let (tx, rx) = mpsc::channel::<(Vec<u8>, bool, Option<(u32, i32)>)>();
        // C `bool` is one byte; Rust's bool isn't a block argument type, so u8.
        let done = RcBlock::new(move |content: *mut c_void, _context: *mut c_void, complete: u8, err: *mut c_void| {
            let mut bytes = Vec::new();
            if !content.is_null() {
                unsafe {
                    let mut ptr: *const c_void = std::ptr::null();
                    let mut len = 0usize;
                    let map = dispatch_data_create_map(content, &mut ptr, &mut len);
                    if !ptr.is_null() {
                        bytes.extend_from_slice(std::slice::from_raw_parts(ptr.cast::<u8>(), len));
                    }
                    if !map.is_null() {
                        dispatch_release(map);
                    }
                }
            }
            let _ = tx.send((bytes, complete != 0, error_of(err)));
        });
        unsafe { nw_connection_receive(self.conn, 1, 256 * 1024, block_ptr(&*done)) };
        match rx.recv_timeout(timeout) {
            Ok((bytes, complete, err)) => {
                match err {
                    Some(e) if is_close(e) => self.eof = true,
                    Some(e) => return Err(describe(&self.host, e)),
                    None => {}
                }
                if complete {
                    self.eof = true;
                }
                if bytes.is_empty() && self.eof {
                    Ok(None)
                } else {
                    Ok(Some(bytes))
                }
            }
            Err(_) => Err(format!("{} stopped sending", self.host)),
        }
    }
}

impl Drop for NwConn {
    fn drop(&mut self) {
        unsafe {
            nw_connection_cancel(self.conn);
            nw_release(self.conn);
            dispatch_release(self.queue);
        }
    }
}

/// Options with Volant's User-Agent (GitHub's API requires one).
pub fn options() -> Options {
    Options { user_agent: format!("Volant/{}", super::VERSION), ..Default::default() }
}

/// GETs `url` (blocking).
pub fn get(url: &str, opts: &Options) -> Result<Response, String> {
    http::get(&NwTransport, url, opts)
}

/// GETs `url` and returns the body of a 2xx answer.
pub fn fetch(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let r = get(url, &Options { limit, ..options() })?;
    if r.ok() {
        Ok(r.body)
    } else {
        Err(format!("{} answered {}", http::Url::parse(url).map(|u| u.host).unwrap_or_default(), r.status))
    }
}

/// [`fetch`] as UTF-8 text.
pub fn fetch_text(url: &str, limit: usize) -> Result<String, String> {
    fetch(url, limit).map(|b| String::from_utf8_lossy(&b).into_owned())
}
