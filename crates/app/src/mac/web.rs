//! Web views and everything WebKit tells us about them.

use super::{actions, ns, pages, rect, theme, ui, with, Browser, Download};
use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAutoresizingMaskOptions, NSEventModifierFlags, NSModalResponseOK, NSOpenPanel, NSWorkspace};
use objc2_foundation::{
    NSArray, NSData, NSDictionary, NSObjectNSKeyValueObserverRegistration, NSDistributedNotificationCenter, NSError, NSHTTPURLResponse, NSKeyValueChangeKey, NSKeyValueObservingOptions,
    NSString, NSURL, NSURLResponse,
};
use objc2_web_kit::{
    WKDownload, WKDownloadDelegate, WKFrameInfo, WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
    WKNavigationResponse, WKNavigationResponsePolicy, WKNavigationType, WKOpenPanelParameters, WKScriptMessage, WKScriptMessageHandler,
    WKUIDelegate, WKURLSchemeHandler, WKURLSchemeTask, WKUserContentController, WKWebView, WKWebViewConfiguration, WKWindowFeatures,
};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use volant_core::tabs::TabId;

/// Properties we watch on every web view.
const OBSERVED: &[&str] = &["title", "URL", "estimatedProgress", "loading", "canGoBack", "canGoForward"];

/// Present ourselves as Safari so sites send what they'd send Safari.
const UA_SUFFIX: &str = "Version/18.4 Safari/605.1.15";

pub struct WebTab {
    pub web: Retained<WKWebView>,
    /// Last URL written to history for this tab.
    pub recorded: String,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; WebDelegate does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    pub struct WebDelegate;

    unsafe impl NSObjectProtocol for WebDelegate {}

    impl WebDelegate {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe(&self, key: Option<&NSString>, object: Option<&AnyObject>, _change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>, _ctx: *mut c_void) {
            let (Some(key), Some(obj)) = (key, object) else { return };
            let key = key.to_string();
            let ptr = obj as *const AnyObject as usize;
            with(move |b| changed(b, ptr, &key));
        }
    }

    unsafe impl WKNavigationDelegate for WebDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_action(&self, web: &WKWebView, action: &WKNavigationAction, handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>) {
            handler.call((decide_navigation(web, action),));
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(&self, _web: &WKWebView, response: &WKNavigationResponse, handler: &DynBlock<dyn Fn(WKNavigationResponsePolicy)>) {
            handler.call((decide_response(response),));
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, web: &WKWebView, _nav: Option<&WKNavigation>) {
            let ptr = ptr_of(web);
            with(move |b| {
                if let Some(id) = tab_of(b, ptr) {
                    fetch_icon(b, id);
                    ui::update_row(b, id);
                    ui::update_chrome(b);
                }
            });
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(&self, web: &WKWebView, _nav: Option<&WKNavigation>, error: &NSError) {
            load_failed(web, error);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(&self, web: &WKWebView, _nav: Option<&WKNavigation>, error: &NSError) {
            let _ = (web, error); // A page that started showing keeps what it has.
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn process_died(&self, web: &WKWebView) {
            // The page's process crashed or was killed for memory: bring it back.
            unsafe { web.reload() };
        }

        #[unsafe(method(webView:navigationAction:didBecomeDownload:))]
        fn action_download(&self, _web: &WKWebView, _action: &WKNavigationAction, download: &WKDownload) {
            self.adopt_download(download);
        }

        #[unsafe(method(webView:navigationResponse:didBecomeDownload:))]
        fn response_download(&self, _web: &WKWebView, _response: &WKNavigationResponse, download: &WKDownload) {
            self.adopt_download(download);
        }
    }

    unsafe impl WKUIDelegate for WebDelegate {
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(&self, _web: &WKWebView, config: &WKWebViewConfiguration, action: &WKNavigationAction, _f: &WKWindowFeatures) -> Option<Retained<WKWebView>> {
            // target=_blank links and window.open(): a new tab, in front.
            let url = unsafe { action.request().URL() }.and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
            super::try_with(|b| actions::adopt_popup(b, config, &url))
        }

        #[unsafe(method(webViewDidClose:))]
        fn did_close(&self, web: &WKWebView) {
            let ptr = ptr_of(web);
            with(move |b| {
                if let Some(id) = tab_of(b, ptr) {
                    actions::close_tab(b, id);
                }
            });
        }

        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn js_alert(&self, _web: &WKWebView, message: &NSString, frame: &WKFrameInfo, done: &DynBlock<dyn Fn()>) {
            alert(&frame_host(frame), &message.to_string(), &["OK"]);
            done.call(());
        }

        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn js_confirm(&self, _web: &WKWebView, message: &NSString, frame: &WKFrameInfo, done: &DynBlock<dyn Fn(objc2::runtime::Bool)>) {
            let ok = alert(&frame_host(frame), &message.to_string(), &["OK", "Cancel"]) == 0;
            done.call((objc2::runtime::Bool::new(ok),));
        }

        #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
        fn js_prompt(&self, _web: &WKWebView, prompt: &NSString, default: Option<&NSString>, frame: &WKFrameInfo, done: &DynBlock<dyn Fn(*mut NSString)>) {
            let mtm = self.mtm();
            let a = NSAlert::new(mtm);
            a.setMessageText(&ns(&frame_host(frame)));
            a.setInformativeText(prompt);
            a.addButtonWithTitle(&ns("OK"));
            a.addButtonWithTitle(&ns("Cancel"));
            let field = objc2_app_kit::NSTextField::initWithFrame(objc2_app_kit::NSTextField::alloc(mtm), rect(0.0, 0.0, 280.0, 24.0));
            field.setStringValue(&ns(&default.map(|d| d.to_string()).unwrap_or_default()));
            a.setAccessoryView(Some(&field));
            a.window().setInitialFirstResponder(Some(&field));
            if a.runModal() == NSAlertFirstButtonReturn {
                let v = field.stringValue();
                done.call((Retained::as_ptr(&v) as *mut NSString,));
            } else {
                done.call((std::ptr::null_mut(),));
            }
        }

        #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
        fn open_panel(&self, _web: &WKWebView, params: &WKOpenPanelParameters, _frame: &WKFrameInfo, done: &DynBlock<dyn Fn(*mut NSArray<NSURL>)>) {
            let panel = NSOpenPanel::openPanel(self.mtm());
            unsafe {
                panel.setAllowsMultipleSelection(params.allowsMultipleSelection());
                panel.setCanChooseDirectories(params.allowsDirectories());
            }
            panel.setCanChooseFiles(true);
            if panel.runModal() == NSModalResponseOK {
                let urls = panel.URLs();
                done.call((Retained::as_ptr(&urls) as *mut NSArray<NSURL>,));
            } else {
                done.call((std::ptr::null_mut(),));
            }
        }
    }

    unsafe impl WKDownloadDelegate for WebDelegate {
        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        fn download_destination(&self, download: &WKDownload, _response: &NSURLResponse, name: &NSString, done: &DynBlock<dyn Fn(*mut NSURL)>) {
            let name = sanitize(&name.to_string());
            let ptr = download as *const WKDownload as usize;
            let path = super::try_with(|b| {
                let path = unique_path(&b.paths.downloads, &name);
                if let Some(d) = b.downloads.iter_mut().find(|d| Retained::as_ptr(&d.obj) as usize == ptr) {
                    d.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name.clone());
                    d.path = path.clone();
                }
                ui::update_downloads(b);
                path
            });
            match path {
                Some(p) => {
                    let url = NSURL::fileURLWithPath(&ns(&p.to_string_lossy()));
                    done.call((Retained::as_ptr(&url) as *mut NSURL,));
                }
                None => done.call((std::ptr::null_mut(),)),
            }
        }

        #[unsafe(method(downloadDidFinish:))]
        fn download_finished(&self, download: &WKDownload) {
            let ptr = download as *const WKDownload as usize;
            with(move |b| {
                if let Some(d) = b.downloads.iter_mut().find(|d| Retained::as_ptr(&d.obj) as usize == ptr) {
                    d.done = true;
                    d.finished_at = super::now();
                    // Makes the Downloads stack in the Dock bounce, like Safari.
                    let center = NSDistributedNotificationCenter::defaultCenter();
                    let path = ns(&d.path.to_string_lossy());
                    unsafe { center.postNotificationName_object(&ns("com.apple.DownloadFileFinished"), Some(&path)) };
                }
                ui::update_downloads(b);
            });
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        fn download_failed(&self, download: &WKDownload, _error: &NSError, _resume: Option<&NSData>) {
            let ptr = download as *const WKDownload as usize;
            with(move |b| {
                if let Some(d) = b.downloads.iter_mut().find(|d| Retained::as_ptr(&d.obj) as usize == ptr) {
                    d.failed = true;
                    d.finished_at = super::now();
                }
                ui::update_downloads(b);
            });
        }
    }

    unsafe impl WKScriptMessageHandler for WebDelegate {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn message(&self, _c: &WKUserContentController, msg: &WKScriptMessage) {
            // Only Volant's own pages may talk to the browser.
            let frame = unsafe { msg.frameInfo() };
            let origin = unsafe { frame.securityOrigin() };
            if unsafe { origin.protocol() }.to_string() != "volant" || !unsafe { frame.isMainFrame() } {
                return;
            }
            let body = unsafe { msg.body() };
            let Some(text) = body.downcast_ref::<NSString>().map(|s| s.to_string()) else { return };
            let (cmd, value) = text.split_once('\n').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((text.clone(), String::new()));
            let ptr = unsafe { msg.webView() }.map(|w| Retained::as_ptr(&w) as usize).unwrap_or(0);
            with(move |b| {
                let id = tab_of(b, ptr);
                pages::handle(b, id, &cmd, &value);
            });
        }
    }

    unsafe impl WKURLSchemeHandler for WebDelegate {
        #[unsafe(method(webView:startURLSchemeTask:))]
        fn start_task(&self, _web: &WKWebView, task: &ProtocolObject<dyn WKURLSchemeTask>) {
            let request = unsafe { task.request() };
            let Some(url) = request.URL() else { return };
            let s = url.absoluteString().map(|s| s.to_string()).unwrap_or_default();
            let (mime, body) = super::try_with(|b| pages::serve(b, &s))
                .unwrap_or_else(|| ("text/html", b"<meta http-equiv=refresh content=0>".to_vec()));
            let response = {
                NSURLResponse::initWithURL_MIMEType_expectedContentLength_textEncodingName(NSURLResponse::alloc(), &url, Some(&ns(mime)), body.len() as isize, Some(&ns("utf-8")))
            };
            unsafe {
                task.didReceiveResponse(&response);
                task.didReceiveData(&NSData::with_bytes(&body));
                task.didFinish();
            }
        }

        #[unsafe(method(webView:stopURLSchemeTask:))]
        fn stop_task(&self, _web: &WKWebView, _task: &ProtocolObject<dyn WKURLSchemeTask>) {}
    }
);

impl WebDelegate {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }

    fn adopt_download(&self, download: &WKDownload) {
        unsafe { download.setDelegate(Some(ProtocolObject::from_ref(self))) };
        let obj: Retained<WKDownload> = unsafe { Retained::retain(download as *const WKDownload as *mut WKDownload) }.expect("download");
        with(move |b| {
            b.downloads.push(Download { obj, name: "file".into(), path: PathBuf::new(), done: false, failed: false, finished_at: 0 });
            ui::watch_downloads(b);
        });
    }
}

fn ptr_of(web: &WKWebView) -> usize {
    web as *const WKWebView as usize
}

pub fn tab_of(b: &Browser, ptr: usize) -> Option<TabId> {
    b.webs.iter().find(|(_, t)| Retained::as_ptr(&t.web) as usize == ptr).map(|(id, _)| *id)
}

fn frame_host(frame: &WKFrameInfo) -> String {
    let h = unsafe { frame.securityOrigin().host() }.to_string();
    if h.is_empty() {
        "This page says".into()
    } else {
        format!("{h} says")
    }
}

/// Shows a modal alert and returns the index of the button pressed.
pub fn alert(title: &str, text: &str, buttons: &[&str]) -> usize {
    let Some(mtm) = MainThreadMarker::new() else { return 0 };
    let a = NSAlert::new(mtm);
    a.setMessageText(&ns(title));
    a.setInformativeText(&ns(text));
    for b in buttons {
        a.addButtonWithTitle(&ns(b));
    }
    let r = a.runModal();
    (r - NSAlertFirstButtonReturn).max(0) as usize
}

pub fn make_config(del: &WebDelegate, mtm: MainThreadMarker) -> (Retained<WKWebViewConfiguration>, Retained<WKUserContentController>) {
    unsafe {
        let config = WKWebViewConfiguration::new(mtm);
        let content = WKUserContentController::new(mtm);
        content.addScriptMessageHandler_name(ProtocolObject::from_ref(del), &ns("volant"));
        config.setUserContentController(&content);
        config.setURLSchemeHandler_forURLScheme(Some(ProtocolObject::from_ref(del)), &ns("volant"));
        config.setApplicationNameForUserAgent(Some(&ns(UA_SUFFIX)));
        let prefs = config.preferences();
        if prefs.respondsToSelector(sel!(setElementFullscreenEnabled:)) {
            prefs.setElementFullscreenEnabled(true);
        }
        (config, content)
    }
}

pub fn paint_background(web: &WKWebView) {
    unsafe {
        if web.respondsToSelector(sel!(setUnderPageBackgroundColor:)) {
            web.setUnderPageBackgroundColor(Some(&theme::ns(theme::current().content)));
        }
    }
}

/// Creates the web view for a tab (or adopts one WebKit made for a popup).
pub fn create(b: &mut Browser, id: TabId, config: Option<&WKWebViewConfiguration>) -> Retained<WKWebView> {
    let cfg = config.unwrap_or(&b.config);
    let web = unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(b.mtm), b.ui.content.bounds(), cfg) };
    unsafe {
        web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*b.web_delegate)));
        web.setUIDelegate(Some(ProtocolObject::from_ref(&*b.web_delegate)));
        web.setAllowsBackForwardNavigationGestures(true);
        web.setAllowsMagnification(true);
        if web.respondsToSelector(sel!(setInspectable:)) {
            web.setInspectable(true);
        }
    }
    web.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    paint_background(&web);
    for key in OBSERVED {
        unsafe { web.addObserver_forKeyPath_options_context(&b.web_delegate, &ns(key), NSKeyValueObservingOptions::New, std::ptr::null_mut()) };
    }
    b.webs.insert(id, WebTab { web: web.clone(), recorded: String::new() });
    web
}

/// Tears down a tab's web view, freeing its memory (closing or sleeping a tab).
pub fn release(b: &mut Browser, id: TabId) {
    if let Some(t) = b.webs.remove(&id) {
        unsafe {
            for key in OBSERVED {
                t.web.removeObserver_forKeyPath(&b.web_delegate, &ns(key));
            }
            t.web.stopLoading();
            t.web.setNavigationDelegate(None);
            t.web.setUIDelegate(None);
        }
        t.web.removeFromSuperview();
    }
}

pub fn load(web: &WKWebView, url: &str) {
    let Some(u) = NSURL::URLWithString(&ns(url)).or_else(|| {
        // Percent-encode anything NSURL won't take as-is (spaces, non-ASCII paths).
        let enc: String = url
            .chars()
            .map(|c| if c.is_ascii() && !c.is_ascii_whitespace() && c != '"' && c != '<' && c != '>' { c.to_string() } else { c.to_string().bytes().map(|b| format!("%{b:02X}")).collect() })
            .collect();
        NSURL::URLWithString(&ns(&enc))
    }) else {
        return;
    };
    let req = objc2_foundation::NSURLRequest::requestWithURL(&u);
    unsafe { web.loadRequest(&req) };
}

fn decide_navigation(web: &WKWebView, action: &WKNavigationAction) -> WKNavigationActionPolicy {
    let req = unsafe { action.request() };
    let url = req.URL().and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
    let scheme = url.split(':').next().unwrap_or("").to_ascii_lowercase();
    let main_frame = unsafe { action.targetFrame() }.map(|f| unsafe { f.isMainFrame() }).unwrap_or(true);
    let source_internal = unsafe { action.sourceFrame() }.map(|f| unsafe { f.securityOrigin().protocol() }.to_string() == "volant").unwrap_or(false);
    let kind = unsafe { action.navigationType() };

    // Web pages can't frame or push you into Volant's own pages.
    if scheme == "volant" && (!main_frame || (!source_internal && kind == WKNavigationType::LinkActivated && !current_is_internal(web))) {
        return WKNavigationActionPolicy::Cancel;
    }
    // ⌘-click or middle-click: open in a new tab.
    if kind == WKNavigationType::LinkActivated && (scheme == "http" || scheme == "https") {
        let mods = unsafe { action.modifierFlags() };
        let middle = unsafe { action.buttonNumber() } == 2;
        if mods.contains(NSEventModifierFlags::Command) || middle {
            let front = mods.contains(NSEventModifierFlags::Shift);
            with(move |b| {
                actions::open_tab(b, &url, front);
            });
            return WKNavigationActionPolicy::Cancel;
        }
    }
    // mailto:, tel:, zoommtg:, and other app links go to the app that handles them.
    if !matches!(scheme.as_str(), "http" | "https" | "about" | "data" | "blob" | "file" | "volant" | "javascript" | "") {
        if let Some(u) = req.URL() {
            NSWorkspace::sharedWorkspace().openURL(&u);
        }
        return WKNavigationActionPolicy::Cancel;
    }
    if unsafe { action.respondsToSelector(sel!(shouldPerformDownload)) && action.shouldPerformDownload() } {
        return WKNavigationActionPolicy::Download;
    }
    WKNavigationActionPolicy::Allow
}

fn current_is_internal(web: &WKWebView) -> bool {
    unsafe { web.URL() }.and_then(|u| u.scheme()).map(|s| s.to_string() == "volant").unwrap_or(false)
}

fn decide_response(response: &WKNavigationResponse) -> WKNavigationResponsePolicy {
    unsafe {
        if !response.isForMainFrame() {
            return WKNavigationResponsePolicy::Allow;
        }
        let r = response.response();
        if let Some(http) = r.downcast_ref::<NSHTTPURLResponse>() {
            if let Some(cd) = http.valueForHTTPHeaderField(&ns("Content-Disposition")) {
                if cd.to_string().trim_start().to_ascii_lowercase().starts_with("attachment") {
                    return WKNavigationResponsePolicy::Download;
                }
            }
        }
        if !response.canShowMIMEType() {
            return WKNavigationResponsePolicy::Download;
        }
    }
    WKNavigationResponsePolicy::Allow
}

fn load_failed(web: &WKWebView, error: &NSError) {
    let code = error.code();
    let domain = error.domain().to_string();
    // Cancelled (-999), or turned into a download / handled elsewhere (WebKit 102, 204).
    if code == -999 || (domain == "WebKitErrorDomain" && (code == 102 || code == 204)) {
        return;
    }
    let info = error.userInfo();
    let failing = info
        .objectForKey(&ns("NSErrorFailingURLStringKey"))
        .and_then(|o| o.downcast::<NSString>().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let message = error.localizedDescription().to_string();
    let ptr = ptr_of(web);
    with(move |b| {
        let Some(id) = tab_of(b, ptr) else { return };
        let html = pages::error_page(&failing, &message, code);
        if let Some(t) = b.webs.get(&id) {
            let base = NSURL::URLWithString(&ns("volant://error"));
            unsafe { t.web.loadHTMLString_baseURL(&ns(&html), base.as_deref()) };
        }
        if let Some(tab) = b.tabs.get_mut(id) {
            if !failing.is_empty() {
                tab.url = failing.clone();
            }
        }
        ui::update_row(b, id);
        ui::update_chrome(b);
    });
}

/// A watched property changed on one of our web views.
fn changed(b: &mut Browser, ptr: usize, key: &str) {
    let Some(id) = tab_of(b, ptr) else { return };
    let active = b.tabs.active == Some(id);
    let web = b.webs[&id].web.clone();
    match key {
        "title" => {
            let title = unsafe { web.title() }.map(|t| t.to_string()).unwrap_or_default();
            if let Some(tab) = b.tabs.get_mut(id) {
                if !title.is_empty() {
                    tab.title = title.clone();
                }
                let url = tab.url.clone();
                b.history.set_title(&url, &title);
            }
            ui::update_row(b, id);
            super::mark_dirty(b);
            if active {
                ui::update_chrome(b);
            }
        }
        "URL" => {
            let url = unsafe { web.URL() }.and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
            if url.is_empty() || url.starts_with("volant://error") || url.starts_with("volant://reader") {
                // Error and Reader pages stand in for the real page; keep its address.
                ui::update_chrome(b);
                return;
            }
            if let Some(tab) = b.tabs.get_mut(id) {
                if tab.url != url {
                    tab.url = url.clone();
                    super::mark_dirty(b);
                }
            }
            if let Some(w) = b.webs.get_mut(&id) {
                if w.recorded != url {
                    w.recorded = url.clone();
                    let title = unsafe { web.title() }.map(|t| t.to_string()).unwrap_or_default();
                    b.history.visit(&url, &title, super::now());
                }
            }
            ui::update_row(b, id);
            if active {
                ui::update_chrome(b);
                super::find::page_changed(b);
            }
        }
        "estimatedProgress" | "loading" => {
            if active {
                ui::update_progress(b);
                if key == "loading" {
                    ui::update_chrome(b);
                }
            }
        }
        _ => {
            if active {
                ui::update_chrome(b);
            }
        }
    }
}

const ICON_JS: &str = r#"(() => {
  const pick = [...document.querySelectorAll('link[rel~="icon" i], link[rel="shortcut icon" i], link[rel="apple-touch-icon" i]')]
    .filter((l) => l.href && !/\.svg(\?|$)/i.test(l.href))
    .sort((a, b) => (parseInt((b.sizes && b.sizes.value) || '0') >= 32 ? 1 : 0) - (parseInt((a.sizes && a.sizes.value) || '0') >= 32 ? 1 : 0))[0];
  return pick ? pick.href : location.origin + '/favicon.ico';
})()"#;

/// Finds the page's icon and downloads it in the background (once per site).
fn fetch_icon(b: &mut Browser, id: TabId) {
    let Some(tab) = b.tabs.get(id) else { return };
    if !tab.url.starts_with("http") {
        return;
    }
    let host = volant_core::omnibox::host(&tab.url).to_string();
    if b.icons.contains_key(&host) || !b.icon_requests.insert(host.clone()) {
        return;
    }
    let Some(w) = b.webs.get(&id) else { return };
    let block = RcBlock::new(move |res: *mut AnyObject, _err: *mut NSError| {
        let href = unsafe { res.as_ref() }.and_then(|o| o.downcast_ref::<NSString>()).map(|s| s.to_string());
        let Some(href) = href else { return };
        let host = host.clone();
        std::thread::spawn(move || {
            let Some(url) = NSURL::URLWithString(&ns(&href)) else { return };
            let Some(data) = NSData::dataWithContentsOfURL(&url) else { return };
            if data.length() == 0 || data.length() > 512 * 1024 {
                return;
            }
            let bytes = data.to_vec();
            super::on_main(move || {
                with(move |b| {
                    let data = NSData::with_bytes(&bytes);
                    let Some(img) = objc2_app_kit::NSImage::initWithData(objc2_app_kit::NSImage::alloc(), &data) else { return };
                    if !img.isValid() {
                        return;
                    }
                    b.icons.insert(host.clone(), img);
                    let ids: Vec<TabId> = b.tabs.list.iter().filter(|t| volant_core::omnibox::host(&t.url) == host).map(|t| t.id).collect();
                    for id in ids {
                        ui::update_row(b, id);
                    }
                });
            });
        });
    });
    unsafe { w.web.evaluateJavaScript_completionHandler(&ns(ICON_JS), Some(&block)) };
}

fn sanitize(name: &str) -> String {
    let n: String = name.chars().map(|c| if c == '/' || c == ':' || c.is_control() { '-' } else { c }).collect();
    let n = n.trim().trim_start_matches('.').to_string();
    if n.is_empty() {
        "download".into()
    } else {
        n
    }
}

/// `name.zip`, or `name 2.zip` if that's taken, like Safari.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..10_000).map(|i| dir.join(format!("{stem} {i}{ext}"))).find(|p| !p.exists()).unwrap_or(p)
}
