//! Native child WebViews for untrusted pages. The desktop supplies bounds and
//! occlusion rectangles; the child never shares the Arcnix document or bridge.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::task::Waker;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use arcnix_core::browser::parse_address;
use glory::wry::shell::NativeWindow;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    CallDevToolsProtocolMethodCompletedHandler, CoTaskMemPWSTR,
    DevToolsProtocolEventReceivedEventHandler, FocusChangedEventHandler,
    HistoryChangedEventHandler, NavigationCompletedEventHandler, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler, SourceChangedEventHandler,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, DeleteObject, NULLREGION, RGN_DIFF, SetWindowRgn,
};
use windows::core::{BOOL, HSTRING, Interface, PCWSTR, PWSTR, w};
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{
    PermissionResponse, Rect, WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows,
    WebViewExtWindows,
};

use crate::browser_cookies::{Cookie, Profile};
use crate::web_browser::{self, BoxPx, Command, Proxy, Viewport};
use crate::*;

// Chrome Stable for Windows, 2026-11-17.
const CHROME_VERSION: &str = "158.0.8127.37";

fn chrome_major() -> &'static str {
    CHROME_VERSION
        .split('.')
        .next()
        .expect("Chrome major version")
}

fn chrome_user_agent() -> String {
    format!(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{}.0.0.0 Safari/537.36",
        chrome_major()
    )
}

struct PageView {
    view: WebView,
    identity_pending: bool,
    pending_url: Option<(String, Option<u64>)>,
    events: Events,
}

impl PageView {
    fn navigate(&self, url: &str, attempt: Option<u64>) -> Result<()> {
        let Some(attempt) = attempt else {
            return Ok(self.view.load_url(url)?);
        };
        let events = self.events.clone();
        let parameters = HSTRING::from(serde_json::json!({"url": url}).to_string());
        let original = url.to_owned();
        unsafe {
            let started = self.view.webview().CallDevToolsProtocolMethod(
                w!("Page.navigate"),
                &parameters,
                &CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
                    move |status, result| {
                        let reply = serde_json::from_str::<serde_json::Value>(&result).ok();
                        let error = reply
                            .as_ref()
                            .and_then(|r| r.get("errorText"))
                            .and_then(|s| s.as_str());
                        // A certificate failure or a response from an HTTPS site is final.
                        let retry = matches!(
                            error,
                            Some(
                                "net::ERR_CONNECTION_REFUSED"
                                    | "net::ERR_CONNECTION_TIMED_OUT"
                                    | "net::ERR_TIMED_OUT"
                                    | "net::ERR_CONNECTION_CLOSED"
                                    | "net::ERR_CONNECTION_RESET"
                                    | "net::ERR_SSL_PROTOCOL_ERROR"
                                    | "net::ERR_SOCKS_CONNECTION_FAILED"
                            )
                        );
                        let outcome = if status.is_err() || reply.is_none() {
                            AddressOutcome::NativeHttps(original.clone())
                        } else if retry {
                            AddressOutcome::RetryHttp
                        } else {
                            AddressOutcome::Finished
                        };
                        events.send(BrowserEvent::AddressResult(attempt, outcome));
                        Ok(())
                    },
                )),
            );
            if started.is_err() {
                self.events.send(BrowserEvent::AddressResult(
                    attempt,
                    AddressOutcome::NativeHttps(url.to_owned()),
                ));
            }
        }
        Ok(())
    }
}

struct HtmlView {
    serial: u64,
    view: WebView,
    viewport: Option<Viewport>,
}

fn html_child(ui: &Ui, window: &NativeWindow, source: Arc<str>) -> Result<WebView> {
    const URL: &str = "http://arcnixpreview.localhost/index.html";
    // Remote HTML has neither the main shell's origin nor its IPC bridge.
    // A sandboxed response permits inline formatting while blocking scripts,
    // forms, frames, and network subresources.
    let started = std::cell::Cell::new(false);
    let mut context = ui.webview_context("preview");
    let view = WebViewBuilder::new_with_web_context(&mut context)
        .with_visible(false)
        .with_focused(false)
        .with_incognito(true)
        .with_custom_protocol("arcnixpreview".into(), move |_, request| {
            let response = wry::http::Response::builder();
            if request.method() == "GET" && request.uri().path_and_query().is_some_and(|p| p.as_str() == "/index.html") {
                response
                    .header("Content-Type", "text/html; charset=utf-8")
                    .header("Content-Security-Policy", "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:; base-uri 'none'; form-action 'none'")
                    .header("X-Content-Type-Options", "nosniff")
                    .body(std::borrow::Cow::Owned(source.as_bytes().to_vec()))
                    .expect("valid HTML preview response")
            } else {
                response.status(404).body(std::borrow::Cow::Borrowed(&[][..])).expect("valid error response")
            }
        })
        .with_navigation_handler(move |url| url == "about:blank" || (url == URL && !started.replace(true)))
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_permission_handler(|_| wry::PermissionResponse::Deny)
        .with_download_started_handler(|_, _| false)
        .with_url(URL)
        .build_as_child(window)?;
    unsafe {
        view.webview()
            .Settings()?
            .SetAreDefaultContextMenusEnabled(false)?;
    }
    Ok(view)
}

pub(crate) struct Host {
    ui: Ui,
    session: Option<(u64, u16)>,
    context: Option<WebContext>,
    applied_dark: Option<bool>,
    views: BTreeMap<u64, PageView>,
    current: BTreeMap<u64, Viewport>,
    tx: Sender<(u64, BrowserEvent)>,
    rx: Receiver<(u64, BrowserEvent)>,
    wake: Option<Waker>,
    cookie_profile: Option<Profile>,
    cookies_restored: bool,
    save_cookies: bool,
    capture: Option<(u64, Instant, Rc<Cell<bool>>)>,
    capture_id: u64,
    last_capture: Option<Instant>,
    retiring: bool,
    exiting: bool,
    html_preview: BTreeMap<u64, HtmlView>,
}

enum AddressOutcome {
    Finished,
    RetryHttp,
    NativeHttps(String),
}

enum BrowserEvent {
    CookieCapture(u64, Result<bool, ()>),
    IdentityReady,
    Location {
        url: String,
        back: bool,
        forward: bool,
    },
    Title(String),
    Started(u64, bool),
    AddressResult(u64, AddressOutcome),
    Finished(u64, Option<&'static str>),
    ResourceFailed(String),
    Popup(String),
    Focus,
}

#[derive(Clone)]
struct Events {
    id: u64,
    tx: Sender<(u64, BrowserEvent)>,
    wake: Waker,
}

impl Events {
    fn send(&self, event: BrowserEvent) {
        if self.tx.send((self.id, event)).is_ok() {
            self.wake.wake_by_ref();
        }
    }

    fn location(&self, view: ICoreWebView2) -> windows::core::Result<()> {
        let mut source = PWSTR::null();
        let mut back = BOOL::default();
        let mut forward = BOOL::default();
        unsafe {
            view.Source(&mut source)?;
            let source = CoTaskMemPWSTR::from(source);
            view.CanGoBack(&mut back)?;
            view.CanGoForward(&mut forward)?;
            let url = source.to_string();
            self.send(BrowserEvent::Location {
                url: if parse_address(&url).is_ok() {
                    url
                } else {
                    String::new()
                },
                back: back.as_bool(),
                forward: forward.as_bool(),
            });
        }
        Ok(())
    }

    fn title(&self, view: &ICoreWebView2) -> windows::core::Result<()> {
        let mut title = PWSTR::null();
        unsafe {
            view.DocumentTitle(&mut title)?;
        }
        self.send(BrowserEvent::Title(CoTaskMemPWSTR::from(title).to_string()));
        Ok(())
    }
}

impl Host {
    pub fn new(ui: Ui) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            ui,
            session: None,
            context: None,
            applied_dark: None,
            views: BTreeMap::new(),
            current: Default::default(),
            tx,
            rx,
            wake: None,
            cookie_profile: None,
            cookies_restored: false,
            save_cookies: true,
            capture: None,
            capture_id: 0,
            last_capture: None,
            retiring: false,
            exiting: false,
            html_preview: Default::default(),
        }
    }

    pub fn set_wake(&mut self, wake: Waker) {
        self.wake = Some(wake);
    }

    pub fn close(&mut self) {
        self.ui.close_web_browser();
        self.html_preview.clear();
        self.reset();
    }

    fn reset(&mut self) {
        self.current.clear();
        self.views.clear();
        self.context = None;
        self.applied_dark = None;
        self.session = None;
        self.cookie_profile = None;
        self.cookies_restored = false;
        self.save_cookies = true;
        self.cancel_cookie_capture();
        self.last_capture = None;
        self.retiring = false;
    }

    pub fn close_requested(&mut self) -> bool {
        self.exiting = true;
        let ready = self.retire();
        if !ready && let Some(wake) = self.wake.clone() {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(2));
                wake.wake();
            });
        }
        ready
    }

    pub fn exit_ready(&mut self) -> bool {
        self.exiting && self.retire()
    }

    fn retire(&mut self) -> bool {
        if !self.retiring {
            self.retiring = true;
            for page in self.views.values() {
                let _ = page.view.set_visible(false);
            }
            self.capture_cookies();
        }
        self.capture
            .as_ref()
            .is_none_or(|(_, since, _)| since.elapsed() >= Duration::from_secs(2))
    }

    fn cancel_cookie_capture(&mut self) {
        if let Some((_, _, canceled)) = self.capture.take() {
            canceled.set(true);
        }
    }

    fn capture_cookies(&mut self) {
        if self.capture.is_some() || !self.cookies_restored || !self.save_cookies {
            return;
        }
        let (Some(profile), Some(page), Some(wake)) = (
            self.cookie_profile.clone(),
            self.views.values().next(),
            self.wake.clone(),
        ) else {
            return;
        };
        self.capture_id = self.capture_id.wrapping_add(1);
        let id = self.capture_id;
        let events = Events {
            id: 0,
            tx: self.tx.clone(),
            wake,
        };
        let canceled = Rc::new(Cell::new(false));
        let is_canceled = canceled.clone();
        let callback = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
            move |status, response| {
                let response = zeroize::Zeroizing::new(response);
                if is_canceled.get() {
                    return Ok(());
                }
                let success = status
                    .map_err(anyhow::Error::from)
                    .and_then(|_| collect_cookies(&response))
                    .and_then(|jar| profile.record(&jar))
                    .map_err(|_| ());
                events.send(BrowserEvent::CookieCapture(id, success));
                Ok(())
            },
        ));
        let started = unsafe {
            page.view.webview().CallDevToolsProtocolMethod(
                w!("Storage.getCookies"),
                w!("{}"),
                &callback,
            )
        };
        if started.is_ok() {
            self.capture = Some((id, Instant::now(), canceled));
        } else {
            self.cookie_error();
        }
        self.last_capture = Some(Instant::now());
    }

    fn cookie_error(&self) {
        self.ui.state.revise(|s| {
            s.server_apps.cookies.error =
                "Could not save or restore encrypted browser cookies".into();
        });
    }

    fn drain_events(&mut self) {
        while let Ok((id, event)) = self.rx.try_recv() {
            if let BrowserEvent::CookieCapture(capture, success) = event {
                if self.capture.as_ref().is_some_and(|(id, ..)| *id == capture) {
                    self.capture = None;
                    match success {
                        Ok(true) => {
                            if let Some(profile) = &self.cookie_profile {
                                browser_cookies::captured(&self.ui, &profile.scope);
                            }
                        }
                        Ok(false) => {}
                        Err(()) => self.cookie_error(),
                    }
                }
                continue;
            }
            if matches!(event, BrowserEvent::Finished(_, _)) {
                self.last_capture = None;
            }
            // The native view of an old tab can still emit events while it closes.
            if !self.views.contains_key(&id)
                || !self.ui.state.with(|s| s.web_browser.tab(id).is_some())
            {
                continue;
            }
            if matches!(event, BrowserEvent::IdentityReady) {
                let page = self.views.get_mut(&id).expect("Browser tab exists");
                page.identity_pending = false;
                if let Some((url, attempt)) = page.pending_url.take()
                    && let Err(error) = page.navigate(&url, attempt)
                {
                    self.tab_error(id, error.to_string());
                }
                continue;
            }
            if matches!(event, BrowserEvent::Focus) {
                if let Some(page) = self.ui.state.with(|s| {
                    web_browser::visible_tabs(s)
                        .find(|(_, tab, _)| tab.id == id)
                        .map(|(window, ..)| {
                            window_tabs::page(arcnix_core::server_apps::DesktopApp::Browser, window)
                        })
                        .filter(|page| *page != s.workspace_page)
                }) {
                    self.ui.focus_desktop_app(page);
                }
                continue;
            }
            let diagnose = self.ui.state.revise(|s| {
                let browser = &mut s.web_browser;
                match event {
                    BrowserEvent::Location { url, back, forward } => {
                        browser.update_location(id, url, back, forward);
                    }
                    BrowserEvent::Title(title) => {
                        if let Some(tab) = browser.tab_mut(id) {
                            tab.title = title
                                .chars()
                                .filter(|c| !c.is_control())
                                .take(192)
                                .collect();
                        }
                    }
                    BrowserEvent::AddressResult(attempt, outcome) => {
                        if let Some(tab) = browser.tab_mut(id)
                            && tab
                                .http_fallback
                                .as_ref()
                                .is_some_and(|(pending, _)| *pending == attempt)
                            && let Some((_, url)) = tab.http_fallback.take()
                        {
                            match outcome {
                                AddressOutcome::Finished => {}
                                AddressOutcome::RetryHttp => browser.load_tab(id, url, None),
                                AddressOutcome::NativeHttps(https) => {
                                    browser.load_tab(id, https, None)
                                }
                            }
                        }
                    }
                    BrowserEvent::Started(navigation, redirected) => {
                        if let Some(tab) = browser.tab_mut(id) {
                            if redirected && tab.navigation == Some(navigation) {
                                tab.http_fallback = None;
                            }
                            tab.navigation = Some(navigation);
                            tab.loading = true;
                            tab.error = None;
                            tab.clear_diagnostic();
                            tab.resources_blocked = false;
                        }
                    }
                    BrowserEvent::Finished(navigation, error) => {
                        if let Some(tab) = browser.tab_mut(id)
                            && tab.navigation == Some(navigation)
                        {
                            tab.loading = false;
                            tab.error = error.map(str::to_owned);
                            return error
                                .is_some_and(|message| message != web_browser::CERTIFICATE_ERROR);
                        }
                    }
                    BrowserEvent::ResourceFailed(destination) => {
                        if let Proxy::Ready { control, .. } = &browser.proxy
                            && control.was_refused(&destination)
                            && let Some(tab) = browser.tab_mut(id)
                        {
                            tab.resources_blocked = true;
                        }
                    }
                    BrowserEvent::Popup(url) => {
                        browser.add_tab(Some(url));
                    }
                    BrowserEvent::Focus
                    | BrowserEvent::IdentityReady
                    | BrowserEvent::CookieCapture(..) => {}
                }
                false
            });
            if diagnose {
                self.ui.diagnose_web_tab(id);
            }
        }
    }

    fn tab_error(&self, id: u64, error: String) {
        self.ui.state.revise(|s| {
            if let Some(tab) = s.web_browser.tab_mut(id) {
                tab.error = Some(error);
                tab.loading = false;
            }
        });
    }

    fn sync_theme(&mut self) {
        let Some(page) = self.views.values().next() else {
            return;
        };
        let dark = self.ui.state.with(server_apps_ui::desktop_dark);
        if self.applied_dark != Some(dark)
            && page
                .view
                .set_theme(if dark {
                    wry::Theme::Dark
                } else {
                    wry::Theme::Light
                })
                .is_ok()
        {
            // WebView2 applies the preference to every tab sharing this isolated profile.
            self.applied_dark = Some(dark);
        }
    }

    pub fn hide(&mut self) {
        for id in std::mem::take(&mut self.current).keys() {
            if let Some(page) = self.views.get(id) {
                let _ = page.view.set_visible(false);
            }
        }
        for preview in self.html_preview.values_mut() {
            if preview.viewport.take().is_some() {
                let _ = preview.view.set_visible(false);
            }
        }
    }

    fn sync_html_preview(&mut self, window: &NativeWindow) {
        let next = self.ui.state.with(web_browser::html_viewports);
        self.html_preview.retain(|id, preview| {
            if !self.ui.state.with(|s| {
                s.web_browser
                    .tab(*id)
                    .is_some_and(|tab| tab.html_serial() == Some(preview.serial))
            }) {
                return false;
            }
            if !next.iter().any(|(tab, ..)| id == tab) && preview.viewport.take().is_some() {
                let _ = preview.view.set_visible(false);
            }
            true
        });
        for (id, serial, source, viewport) in next {
            if self
                .html_preview
                .get(&id)
                .is_none_or(|preview| preview.serial != serial)
            {
                match html_child(&self.ui, window, source) {
                    Ok(view) => {
                        self.html_preview.insert(
                            id,
                            HtmlView {
                                serial,
                                view,
                                viewport: None,
                            },
                        );
                    }
                    Err(error) => {
                        self.tab_error(id, format!("{error:#}"));
                        continue;
                    }
                }
            }
            let preview = self.html_preview.get_mut(&id).unwrap();
            if preview.viewport.as_ref() != Some(&viewport) {
                if let Err(error) = position(&preview.view, &viewport) {
                    self.html_preview.remove(&id);
                    self.tab_error(id, format!("{error:#}"));
                } else {
                    preview.viewport = Some(viewport);
                }
            }
        }
    }

    pub fn poll(&mut self, window: &NativeWindow) {
        self.drain_events();
        if self.exiting {
            return;
        }
        self.sync_html_preview(window);
        let connection = self.ui.state.with(|s| {
            if !s.connected || !server_desktop::launcher_running(s, WorkspacePage::Browser) {
                return None;
            }
            match &s.web_browser.proxy {
                Proxy::Ready {
                    serial, port, key, ..
                } => Some((
                    *serial,
                    *port,
                    key.clone(),
                    s.server_apps.cookies.ready,
                    s.server_apps.cookies.profile.clone(),
                )),
                _ => None,
            }
        });
        let Some((serial, port, key, cookies_ready, profile)) = connection else {
            if self.session.is_some() && self.retire() {
                self.reset();
            }
            return;
        };
        let state = self.ui.state;
        let tabs_closed = !self.views.is_empty()
            && !state.with(|s| self.views.keys().any(|id| s.web_browser.tab(*id).is_some()));
        if self.session != Some((serial, port)) || tabs_closed || self.retiring {
            if !self.retire() {
                return;
            }
            self.reset();
            if !cookies_ready {
                return;
            }
            self.session = Some((serial, port));
            self.cookie_profile = profile.filter(|profile| profile.scope == key);
            self.context = Some(self.ui.webview_context(&format!("browser/{key}")));
        }
        self.views
            .retain(|id, _| state.with(|s| s.web_browser.tab(*id).is_some()));
        self.sync_theme();
        let commands =
            if state.with(|s| s.web_browser.reconnecting || s.web_browser.commands.is_empty()) {
                Default::default()
            } else {
                state.revise(|s| std::mem::take(&mut s.web_browser.commands))
            };
        for (id, command) in commands {
            if !state.with(|s| s.web_browser.tab(id).is_some()) {
                continue;
            }
            if let Err(error) = self.run_command(window, id, port, command) {
                self.tab_error(id, format!("{error:#}"));
            }
        }

        if self
            .capture
            .as_ref()
            .is_some_and(|(_, since, _)| since.elapsed() >= Duration::from_secs(5))
        {
            self.cancel_cookie_capture();
            self.cookie_error();
        }
        if self
            .last_capture
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(3))
        {
            self.capture_cookies();
        }

        let mut next = state.with(web_browser::native_viewports);
        next.retain(|id, _| {
            self.views
                .get(id)
                .is_some_and(|page| !page.identity_pending)
        });
        self.current.retain(|id, _| {
            if next.contains_key(id) {
                return true;
            }
            if let Some(page) = self.views.get(id) {
                let _ = page.view.set_visible(false);
            }
            false
        });
        for (id, viewport) in next {
            if self.current.get(&id) == Some(&viewport) {
                continue;
            }
            let view = &self.views[&id].view;
            match position(view, &viewport) {
                Ok(()) => {
                    self.current.insert(id, viewport);
                }
                Err(error) => {
                    let _ = view.set_visible(false);
                    self.current.remove(&id);
                    self.tab_error(id, format!("{error:#}"));
                }
            }
        }
    }

    fn run_command(
        &mut self,
        window: &NativeWindow,
        id: u64,
        port: u16,
        command: Command,
    ) -> Result<()> {
        if let Command::Load(url, attempt) = command {
            parse_address(&url)?;
            if !self.views.contains_key(&id) {
                let events = Events {
                    id,
                    tx: self.tx.clone(),
                    wake: self.wake.clone().context("Browser host is not ready")?,
                };
                let environment = self
                    .views
                    .values()
                    .next()
                    .map(|page| page.view.environment());
                let context = self
                    .context
                    .as_mut()
                    .context("Browser profile is not ready")?;
                let directory = context
                    .data_directory()
                    .context("Browser profile is missing")?;
                let key = directory
                    .file_name()
                    .context("Browser profile key is missing")?;
                let previous = self
                    .ui
                    .store
                    .path()
                    .with_file_name("browser-data")
                    .join(key);
                arcnix_core::paths::prepare_webview_profile(&previous, directory)?;
                let view = child(window, context, environment, port, events.clone())?;
                if !self.cookies_restored {
                    if let Some(profile) = &self.cookie_profile
                        && restore_cookies(&view, profile).is_err()
                    {
                        self.save_cookies = false;
                        self.cookie_error();
                    }
                    self.cookies_restored = true;
                }
                let identity_pending = configure_chrome_identity(&view, events.clone()).is_ok();
                self.views.insert(
                    id,
                    PageView {
                        view,
                        identity_pending,
                        pending_url: None,
                        events,
                    },
                );
                self.sync_theme();
            }
            let page = self.views.get_mut(&id).expect("Browser tab created");
            if page.identity_pending {
                page.pending_url = Some((url, attempt));
            } else {
                page.navigate(&url, attempt)?;
            }
        } else if let Some(page) = self.views.get_mut(&id) {
            if page.identity_pending {
                if matches!(command, Command::Stop) {
                    page.pending_url = None;
                }
                return Ok(());
            }
            let core = page.view.webview();
            unsafe {
                match command {
                    Command::Back => core.GoBack()?,
                    Command::Forward => core.GoForward()?,
                    Command::Reload => core.Reload()?,
                    Command::Stop => core.Stop()?,
                    Command::Load(..) => unreachable!(),
                }
            }
        }
        Ok(())
    }
}

fn cookie_manager(view: &WebView) -> Result<ICoreWebView2CookieManager> {
    Ok(unsafe { view.webview().cast::<ICoreWebView2_2>()?.CookieManager()? })
}

fn collect_cookies(response: &str) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    use serde::Deserialize;
    use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

    #[derive(Deserialize)]
    struct CookieList {
        cookies: Vec<ChromeCookie>,
    }

    #[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
    #[serde(rename_all = "camelCase")]
    struct ChromeCookie {
        name: String,
        value: String,
        domain: String,
        path: String,
        expires: f64,
        session: bool,
        secure: bool,
        http_only: bool,
        same_site: Option<String>,
        #[zeroize(skip)]
        partition_key: Option<serde::de::IgnoredAny>,
        #[serde(default)]
        partition_key_opaque: bool,
    }

    ensure!(
        response.len() <= 8 * 1024 * 1024,
        "Browser cookie response is too large"
    );
    let CookieList { cookies } = serde_json::from_str(response)?;
    ensure!(cookies.len() <= 4096, "Too many browser cookies to sync");
    let mut snapshot = Vec::new();
    for mut cookie in cookies {
        // WebView2's restore API has no partition attribute. Keep these solely
        // in the live browser profile to preserve their original isolation.
        if cookie.partition_key.is_some() || cookie.partition_key_opaque {
            continue;
        }
        let expires = if cookie.session {
            None
        } else if cookie.expires.is_finite() && cookie.expires > 0.0 {
            Some(cookie.expires)
        } else {
            continue;
        };
        let same_site = match cookie.same_site.as_deref() {
            Some("Strict") => COREWEBVIEW2_COOKIE_SAME_SITE_KIND_STRICT,
            Some("None") if cookie.secure => COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE,
            _ => COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX,
        };
        snapshot.push(Cookie {
            name: std::mem::take(&mut cookie.name),
            value: std::mem::take(&mut cookie.value),
            domain: std::mem::take(&mut cookie.domain),
            path: std::mem::take(&mut cookie.path),
            expires,
            secure: cookie.secure,
            http_only: cookie.http_only,
            same_site: same_site.0,
        });
    }
    snapshot.sort_by(|a, b| (&a.domain, &a.path, &a.name).cmp(&(&b.domain, &b.path, &b.name)));
    Ok(Zeroizing::new(serde_json::to_vec(&snapshot)?))
}

fn restore_cookies(view: &WebView, profile: &Profile) -> Result<()> {
    let Some(jar) = profile.restore()? else {
        return Ok(());
    };
    let cookies: Vec<Cookie> = serde_json::from_slice(&jar)?;
    let manager = cookie_manager(view)?;
    let now = time::OffsetDateTime::now_utc().unix_timestamp() as f64;
    let mut restore = Vec::new();
    for record in cookies
        .into_iter()
        .filter(|cookie| cookie.expires.is_none_or(|expires| expires > now))
    {
        let name = HSTRING::from(&record.name);
        let value = HSTRING::from(&record.value);
        let domain = HSTRING::from(&record.domain);
        let path = HSTRING::from(&record.path);
        unsafe {
            let cookie = manager.CreateCookie(&name, &value, &domain, &path)?;
            cookie.SetIsSecure(record.secure)?;
            cookie.SetIsHttpOnly(record.http_only)?;
            cookie.SetSameSite(COREWEBVIEW2_COOKIE_SAME_SITE_KIND(record.same_site))?;
            if let Some(expires) = record.expires {
                cookie.SetExpires(expires)?;
            }
            restore.push(cookie);
        }
    }
    for cookie in restore {
        unsafe {
            manager.AddOrUpdateCookie(&cookie)?;
        }
    }
    Ok(())
}

fn child(
    window: &NativeWindow,
    context: &mut WebContext,
    environment: Option<ICoreWebView2Environment>,
    port: u16,
    events: Events,
) -> Result<WebView> {
    let directory = context
        .data_directory()
        .context("Browser profile is missing")?
        .to_owned();
    std::fs::create_dir_all(&directory).context("Unable to create the browser profile")?;
    let title_events = events.clone();
    let mut builder = WebViewBuilder::new_with_web_context(context)
        .with_visible(false)
        .with_focused(false)
        .with_devtools(false)
        // The WebView setting also applies the legacy UA to its workers and
        // supplies the fallback when the runtime cannot configure Client Hints.
        .with_user_agent(chrome_user_agent())
        // Disable Chromium's implicit loopback bypass so SSH receives localhost too.
        .with_additional_browser_args(format!(
            "--proxy-server=socks5://127.0.0.1:{port} --proxy-bypass-list=<-loopback> \
             --host-resolver-rules=\"MAP * ~NOTFOUND, EXCLUDE 127.0.0.1\" \
             --disable-quic --force-webrtc-ip-handling-policy=disable_non_proxied_udp \
             --disable-features=msWebOOUI,msPdfOOUI"
        ))
        .with_navigation_handler(|url| url == "about:blank" || parse_address(&url).is_ok())
        .with_permission_handler(|_| PermissionResponse::Deny)
        .with_download_started_handler(|_, _| false)
        .with_document_title_changed_handler(move |title| {
            title_events.send(BrowserEvent::Title(title))
        });
    if let Some(environment) = environment {
        builder = builder.with_environment(environment);
    }
    let view = builder
        .build_as_child(window)
        .context("Unable to open the isolated browser")?;
    // WebView2 environment overrides can redirect an application-supplied folder.
    // Verify the actual profile before allowing any navigation.
    let environment = view.environment().cast::<ICoreWebView2Environment7>()?;
    let mut actual = PWSTR::null();
    unsafe {
        environment.UserDataFolder(&mut actual)?;
    }
    let actual = PathBuf::from(CoTaskMemPWSTR::from(actual).to_string());
    ensure!(
        std::fs::canonicalize(actual)? == std::fs::canonicalize(directory)?,
        "The browser profile could not be isolated from the Arcnix interface"
    );
    initialize(&view, events)?;
    Ok(view)
}

fn configure_chrome_identity(view: &WebView, events: Events) -> windows::core::Result<()> {
    let version_list = |version: &str| {
        serde_json::json!([
            { "brand": "Chromium", "version": version },
            { "brand": "Google Chrome", "version": version },
        ])
    };
    let parameters = HSTRING::from(
        serde_json::json!({
            "userAgent": chrome_user_agent(),
            "userAgentMetadata": {
                "brands": version_list(chrome_major()),
                "fullVersionList": version_list(CHROME_VERSION),
                "fullVersion": CHROME_VERSION,
                "platform": "Windows",
                "platformVersion": "",
                "architecture": if cfg!(target_arch = "aarch64") { "arm" } else { "x86" },
                "bitness": usize::BITS.to_string(),
                "model": "",
                "mobile": false,
                "wow64": false,
            },
        })
        .to_string(),
    );
    let core = view.webview();
    let network = core.clone();
    unsafe {
        core.CallDevToolsProtocolMethod(
            w!("Emulation.setUserAgentOverride"),
            &parameters,
            &CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |_, _| {
                let ready = events.clone();
                if network
                    .CallDevToolsProtocolMethod(
                        w!("Network.enable"),
                        w!("{\"maxPostDataSize\":0}"),
                        &CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
                            move |_, _| {
                                ready.send(BrowserEvent::IdentityReady);
                                Ok(())
                            },
                        )),
                    )
                    .is_err()
                {
                    events.send(BrowserEvent::IdentityReady);
                }
                Ok(())
            })),
        )
    }
}

fn observe_event(
    core: &ICoreWebView2,
    name: PCWSTR,
    on_json: impl Fn(&str) + 'static,
) -> windows::core::Result<()> {
    unsafe {
        let receiver = core.GetDevToolsProtocolEventReceiver(name)?;
        receiver.add_DevToolsProtocolEventReceived(
            &DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut payload = PWSTR::null();
                    args.ParameterObjectAsJson(&mut payload)?;
                    let json = CoTaskMemPWSTR::from(payload);
                    on_json(&json.to_string());
                }
                Ok(())
            })),
            &mut 0,
        )?;
    }
    Ok(())
}

fn observe_resources(
    core: &ICoreWebView2,
    events: Events,
    requests: Rc<RefCell<BTreeMap<String, String>>>,
) -> windows::core::Result<()> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Request<'a> {
        request_id: &'a str,
        #[serde(rename = "type")]
        kind: Option<&'a str>,
        request: Resource<'a>,
    }
    #[derive(serde::Deserialize)]
    struct Resource<'a> {
        url: &'a str,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Completion<'a> {
        request_id: &'a str,
        error_text: Option<&'a str>,
    }

    let started = requests.clone();
    observe_event(core, w!("Network.requestWillBeSent"), move |json| {
        if let Ok(request) = serde_json::from_str::<Request<'_>>(json)
            && matches!(
                request.kind,
                Some("Stylesheet" | "Script" | "Font" | "Image")
            )
            && let Ok(destination) =
                arcnix_core::browser_proxy::forwarding_destination(request.request.url)
        {
            let mut pending = started.borrow_mut();
            if pending.len() < 512 {
                pending.insert(request.request_id.to_owned(), destination);
            }
        }
    })?;
    let finished = requests.clone();
    observe_event(core, w!("Network.loadingFinished"), move |json| {
        if let Ok(result) = serde_json::from_str::<Completion<'_>>(json) {
            finished.borrow_mut().remove(result.request_id);
        }
    })?;
    observe_event(core, w!("Network.loadingFailed"), move |json| {
        if let Ok(result) = serde_json::from_str::<Completion<'_>>(json)
            && let Some(destination) = requests.borrow_mut().remove(result.request_id)
            && result
                .error_text
                .is_some_and(|error| error.starts_with("net::ERR_SOCKS_"))
        {
            events.send(BrowserEvent::ResourceFailed(destination));
        }
    })
}

fn initialize(view: &WebView, events: Events) -> windows::core::Result<()> {
    let core = view.webview();
    let controller = view.controller();
    let requests = Rc::new(RefCell::new(BTreeMap::new()));
    let _ = observe_resources(&core, events.clone(), requests.clone());
    unsafe {
        let settings = core.Settings()?;
        settings.SetAreHostObjectsAllowed(false)?;
        settings.SetIsWebMessageEnabled(false)?;
        settings.SetIsBuiltInErrorPageEnabled(false)?;
        settings.SetIsStatusBarEnabled(false)?;
        settings.SetAreDefaultContextMenusEnabled(false)?;

        let popup = events.clone();
        core.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    args.SetHandled(true)?;
                    let mut initiated = BOOL::default();
                    args.IsUserInitiated(&mut initiated)?;
                    if initiated.as_bool() {
                        let mut source = PWSTR::null();
                        args.Uri(&mut source)?;
                        let url = CoTaskMemPWSTR::from(source).to_string();
                        if parse_address(&url).is_ok() {
                            popup.send(BrowserEvent::Popup(url));
                        }
                    }
                }
                Ok(())
            })),
            &mut 0,
        )?;

        let started = events.clone();
        core.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut id = 0;
                    args.NavigationId(&mut id)?;
                    let mut source = PWSTR::null();
                    args.Uri(&mut source)?;
                    if parse_address(&CoTaskMemPWSTR::from(source).to_string()).is_ok() {
                        requests.borrow_mut().clear();
                        let mut redirected = BOOL::default();
                        args.IsRedirected(&mut redirected)?;
                        started.send(BrowserEvent::Started(id, redirected.as_bool()));
                    }
                }
                Ok(())
            })),
            &mut 0,
        )?;

        let finished = events.clone();
        core.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |sender, args| {
                if let Some(args) = args {
                    let mut id = 0;
                    let mut success = BOOL::default();
                    let mut status = COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN;
                    args.NavigationId(&mut id)?;
                    args.IsSuccess(&mut success)?;
                    args.WebErrorStatus(&mut status)?;
                    if success.as_bool()
                        && let Some(view) = sender.as_ref()
                    {
                        let _ = finished.title(view);
                    }
                    let error = if success.as_bool()
                        || status == COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED
                    {
                        None
                    } else {
                        Some(web_error(status))
                    };
                    finished.send(BrowserEvent::Finished(id, error));
                }
                if let Some(sender) = sender {
                    finished.location(sender)?;
                }
                Ok(())
            })),
            &mut 0,
        )?;

        let source = events.clone();
        core.add_SourceChanged(
            &SourceChangedEventHandler::create(Box::new(move |sender, _| {
                if let Some(sender) = sender {
                    source.location(sender)?;
                }
                Ok(())
            })),
            &mut 0,
        )?;

        let history = events.clone();
        core.add_HistoryChanged(
            &HistoryChangedEventHandler::create(Box::new(move |sender, _| {
                if let Some(sender) = sender {
                    history.location(sender)?;
                }
                Ok(())
            })),
            &mut 0,
        )?;

        controller.add_GotFocus(
            &FocusChangedEventHandler::create(Box::new(move |_, _| {
                events.send(BrowserEvent::Focus);
                Ok(())
            })),
            &mut 0,
        )?;
    }
    Ok(())
}

fn web_error(status: COREWEBVIEW2_WEB_ERROR_STATUS) -> &'static str {
    match status {
        COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT
        | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED
        | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID
        | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED => web_browser::CERTIFICATE_ERROR,
        COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT => "The website took too long to respond",
        _ => "The server could not reach this page or SSH forwarding was refused",
    }
}

pub(crate) fn position(view: &WebView, viewport: &Viewport) -> Result<()> {
    let area = viewport.area.context("Missing browser content bounds")?;
    view.set_bounds(Rect {
        position: PhysicalPosition::new(area.left, area.top).into(),
        size: PhysicalSize::new(area.width() as u32, area.height() as u32).into(),
    })?;
    view.set_visible(install_region(
        view.hwnd(),
        area,
        viewport.frame,
        &viewport.clip,
    )?)?;
    Ok(())
}

pub(crate) fn install_region(
    window: HWND,
    area: BoxPx,
    frame: Option<BoxPx>,
    occlusion: &[BoxPx],
) -> Result<bool> {
    let Some(exposed) = frame.and_then(|frame| area.intersect(frame)) else {
        return Ok(false);
    };
    // SetWindowRgn takes ownership of the assigned region. Temporary regions
    // remain ours and are released whether or not the operation succeeds.
    unsafe {
        let region = CreateRectRgn(
            exposed.left - area.left,
            exposed.top - area.top,
            exposed.right - area.left,
            exposed.bottom - area.top,
        );
        ensure!(!region.is_invalid(), "Unable to clip the browser view");
        let mut visible = true;
        for rect in occlusion.iter().filter_map(|rect| exposed.intersect(*rect)) {
            let cover = CreateRectRgn(
                rect.left - area.left,
                rect.top - area.top,
                rect.right - area.left,
                rect.bottom - area.top,
            );
            if cover.is_invalid() {
                let _ = DeleteObject(region.into());
                anyhow::bail!("Unable to clip the browser view");
            }
            let kind = CombineRgn(Some(region), Some(region), Some(cover), RGN_DIFF);
            let _ = DeleteObject(cover.into());
            if kind.0 == 0 {
                let _ = DeleteObject(region.into());
                anyhow::bail!("Unable to calculate browser window clipping");
            }
            if kind == NULLREGION {
                visible = false;
                break;
            }
        }
        if !visible || SetWindowRgn(window, Some(region), true) == 0 {
            let _ = DeleteObject(region.into());
            ensure!(!visible, "Unable to apply browser window clipping");
        }
        Ok(visible)
    }
}
#[cfg(test)]
mod cookie_tests {
    use super::*;

    #[test]
    fn partitioned_cookies_stay_in_the_live_webview_profile() -> Result<()> {
        let mut plain = serde_json::json!({
            "name": "session", "value": "fixture", "domain": "example.test", "path": "/",
            "expires": -1, "session": true, "secure": true, "httpOnly": true, "sameSite": "Strict"
        });
        let mut partitioned = plain.clone();
        partitioned["partitionKey"] = serde_json::json!({"topLevelSite": "https://other.test"});
        let mut opaque = plain.clone();
        opaque["partitionKeyOpaque"] = serde_json::json!(true);
        let response =
            serde_json::json!({"cookies": [&mut plain, &partitioned, &opaque]}).to_string();
        let captured: Vec<Cookie> = serde_json::from_slice(&collect_cookies(&response)?)?;
        assert_eq!(captured.len(), 1);
        let cookie = &captured[0];
        assert!(cookie.secure && cookie.http_only && cookie.expires.is_none());
        assert_eq!(
            cookie.same_site,
            COREWEBVIEW2_COOKIE_SAME_SITE_KIND_STRICT.0
        );
        Ok(())
    }
}
