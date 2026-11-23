//! Trusted, local WebView for the Code workbench. Only bundled resources are served.
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read;
use std::sync::{OnceLock, mpsc};
use std::task::Waker;

use anyhow::{Context, Result};
use base64::Engine;
use glory::wry::shell::NativeWindow;
use serde_json::{Value, json};
#[cfg(target_os = "macos")]
use wry::WebViewExtMacOS;
use wry::{WebView, WebViewBuilder};

use crate::*;

#[cfg(windows)]
const ORIGIN: &str = "http://arcnixcode.localhost";
#[cfg(not(windows))]
const ORIGIN: &str = "arcnixcode://localhost";

#[cfg(target_os = "macos")]
use crate::mac_surface;

struct CodeView {
    web: WebView,
    #[cfg(target_os = "macos")]
    surface: mac_surface::Surface,
}

#[cfg(any(windows, target_os = "macos"))]
fn position(
    view: &CodeView,
    viewport: &native_viewport::Viewport,
    picker: bool,
    menu_width: Option<i32>,
) -> Result<()> {
    use native_viewport::BoxPx;
    let mut port = viewport.clone();
    let body = port.area.context("Missing Code content bounds")?;
    let slot = port.command_center.filter(|slot| {
        slot.top < body.top
            && slot.bottom <= body.top
            && slot.left >= body.left
            && slot.right <= body.right
    });
    let menu = port
        .menu_slot
        .filter(|m| m.left >= body.left && slot.is_some_and(|s| m.right <= s.left));
    if let Some(slot) = slot {
        // Expose only the native menu and the active picker in the host titlebar.
        port.area = Some(BoxPx {
            top: slot.top,
            ..body
        });
        let strip = BoxPx {
            top: slot.top,
            bottom: body.top,
            ..body
        };
        // Keep the full slot for responsive menu layout. Only the part with
        // menu buttons should intercept clicks in the desktop titlebar.
        let menu_hit = menu.and_then(|menu| {
            let width = menu_width.unwrap_or(menu.width()).min(menu.width());
            (width > 0).then_some(BoxPx {
                right: menu.left + width,
                ..menu
            })
        });
        let mut left = strip.left;
        for visible in [menu_hit, picker.then_some(slot)].into_iter().flatten() {
            if visible.left > left {
                port.clip.push(BoxPx {
                    left,
                    right: visible.left,
                    ..strip
                });
            }
            left = visible.right;
        }
        if left < strip.right {
            port.clip.push(BoxPx { left, ..strip });
        }
    }
    let geometry = slot.map(|slot| {
        [
            slot.left - body.left,
            slot.width(),
            slot.height(),
            body.top - slot.top,
            menu.map_or(0, |m| m.left - body.left),
            menu.map_or(0, |m| m.width()),
        ]
    });
    view.web.evaluate_script(&format!(
        "window.arcnixCommandSlot?.({}, {})",
        json!(geometry),
        json!(viewport.title_colors)
    ))?;
    #[cfg(windows)]
    return web_browser_native::position(&view.web, &port);
    #[cfg(target_os = "macos")]
    view.surface.position(&view.web.webview(), &port)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn position(
    view: &CodeView,
    viewport: &native_viewport::Viewport,
    _picker: bool,
    _menu_width: Option<i32>,
) -> Result<()> {
    use wry::dpi::{PhysicalPosition, PhysicalSize};
    let view = &view.web;
    let Some(area) = viewport
        .area
        .zip(viewport.frame)
        .and_then(|(area, frame)| area.intersect(frame))
    else {
        view.set_visible(false)?;
        return Ok(());
    };
    view.set_bounds(wry::Rect {
        position: PhysicalPosition::new(area.left, area.top).into(),
        size: PhysicalSize::new(area.width() as u32, area.height() as u32).into(),
    })?;
    view.set_visible(viewport.clip.is_empty())?;
    Ok(())
}

fn assets() -> &'static HashMap<String, Vec<u8>> {
    static FILES: OnceLock<HashMap<String, Vec<u8>>> = OnceLock::new();
    FILES.get_or_init(|| {
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(
            &include_bytes!("../../assets/vendor/workbench/workbench.gz")[..],
        )
        .read_to_end(&mut decoded)
        .expect("embedded workbench archive");
        let packed: HashMap<String, String> =
            serde_json::from_slice(&decoded).expect("embedded workbench manifest");
        packed
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    base64::engine::general_purpose::STANDARD
                        .decode(v)
                        .expect("embedded workbench asset"),
                )
            })
            .collect()
    })
}
fn asset(req: wry::http::Request<Vec<u8>>) -> wry::http::Response<Cow<'static, [u8]>> {
    let name = req.uri().path().trim_start_matches('/');
    let response=wry::http::Response::builder().header("Content-Security-Policy",
        "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; font-src 'self' data:; img-src 'self' data: blob:; connect-src 'self'; worker-src 'self' blob:; frame-src 'none'; base-uri 'none'; form-action 'none'");
    if req.method() == "GET"
        && let Some(bytes) = assets().get(name)
    {
        let mime = match name.rsplit('.').next().unwrap_or("") {
            "js" => "text/javascript",
            "css" => "text/css",
            "html" => "text/html; charset=utf-8",
            "json" => "application/json",
            "ttf" => "font/ttf",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "wasm" => "application/wasm",
            _ => "application/octet-stream",
        };
        return response
            .header("Content-Type", mime)
            .body(Cow::Borrowed(bytes.as_slice()))
            .unwrap();
    }
    response.status(404).body(Cow::Borrowed(&[][..])).unwrap()
}
pub(crate) struct Host {
    ui: Ui,
    pub wake: Option<Waker>,
    rx: mpsc::Receiver<(u64, String)>,
    tx: mpsc::Sender<(u64, String)>,
    serial: u64,
    epoch: Option<u64>,
    view: Option<CodeView>,
    ready: bool,
    save_shortcut: Option<Vec<String>>,
    picker: bool,
    menu_width: Option<i32>,
    bounds: Option<native_viewport::Viewport>,
}
impl Host {
    pub fn new(ui: Ui) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            ui,
            wake: None,
            rx,
            tx,
            serial: 0,
            epoch: None,
            view: None,
            ready: false,
            save_shortcut: None,
            picker: false,
            menu_width: None,
            bounds: None,
        }
    }
    fn create(&mut self, window: &NativeWindow) -> Result<CodeView> {
        let opts = self.ui.state.revise(|s| {
            let root = if s.remote_path.starts_with('/') {
                s.remote_path.as_str()
            } else {
                "/"
            };
            s.code.root = root.to_owned();
            s.code.folder = root
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or("/")
                .to_owned();
            json!({
                "root": root,
                "readOnly": s.demo_read_only(),
                "terminalAvailable": s.terminal_available(),
                "terminalLabel": if s.local_desktop.is_none() {"SSH"} else if cfg!(windows) {"PowerShell"} else {"Terminal"},
                "windowsHost": cfg!(windows) && s.local_desktop.is_some(),
                "pageBytes": arcnix_core::text_files::PAGE_BYTES,
                "zh": desktop_system_settings::language(s) == arcnix_core::Language::SimplifiedChinese,
                "dark": server_apps_ui::desktop_dark(s),
                "saveShortcut": desktop_shortcuts_ui::keys(s).chords(arcnix_core::desktop_shortcuts::Action::CodeSave).collect::<Vec<_>>(),
            })
        });
        let tx = self.tx.clone();
        let wake = self.wake.clone().context("Desktop not ready")?;
        let serial = self.serial;
        let script = format!(
            "window.arcnixWorkspace={opts};\n{}",
            include_str!("../../assets/shortcut-chords.js")
        );
        #[cfg(any(windows, target_os = "macos"))]
        let script = format!("{script}\n{}", include_str!("../../assets/code-menubar.js"));
        #[cfg(not(windows))]
        let script = format!(
            "{script}\nwindow.addEventListener('contextmenu', event => event.preventDefault(), true);"
        );
        let view = WebViewBuilder::new()
            .with_visible(false)
            .with_focused(false)
            .with_incognito(true)
            .with_custom_protocol("arcnixcode".into(), |_, req| asset(req))
            .with_initialization_script(script)
            .with_navigation_handler(|url| {
                url == format!("{ORIGIN}/index.html") || url == "about:blank"
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_permission_handler(|_| wry::PermissionResponse::Deny)
            .with_download_started_handler(|_, _| false)
            .with_ipc_handler(move |request| {
                if request
                    .uri()
                    .to_string()
                    .strip_prefix(ORIGIN)
                    .is_some_and(|path| path.is_empty() || path.starts_with('/'))
                    && request.body().len() < 7 * 1024 * 1024
                    && tx.send((serial, request.into_body())).is_ok()
                {
                    wake.wake_by_ref();
                }
            })
            .with_url(format!("{ORIGIN}/index.html"))
            .build_as_child(window)?;
        #[cfg(windows)]
        {
            use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings9;
            use windows::core::Interface;
            use wry::WebViewExtWindows;

            // The bundled workbench declares a native drag region for its
            // standalone window. In a child WebView that would drag Arcnix.
            unsafe {
                let settings = view.controller().CoreWebView2()?.Settings()?;
                settings.SetAreDefaultContextMenusEnabled(false)?;
                if let Ok(settings) = settings.cast::<ICoreWebView2Settings9>() {
                    settings.SetIsNonClientRegionSupportEnabled(false)?;
                }
            }
        }
        Ok(CodeView {
            #[cfg(target_os = "macos")]
            surface: mac_surface::Surface::attach(&view.webview())?,
            web: view,
        })
    }
    pub fn hide(&mut self) {
        if let Some(view) = &self.view {
            let _ = view.web.set_visible(false);
        }
        self.bounds = None;
    }
    pub fn poll(&mut self, window: &NativeWindow) {
        let epoch = self.ui.state.with(|s| {
            (s.connected && s.workbench.apps.contains(&WorkspacePage::Code)).then_some(s.epoch)
        });
        if self.epoch != epoch {
            if epoch.is_none()
                && self
                    .ui
                    .state
                    .with(|s| s.connected && Some(s.epoch) == self.epoch)
            {
                crate::code_terminal::close_all(&self.ui);
                self.ui
                    .send(Action::CodeFile(code_workbench::FileRequest::ClosePage {
                        handle: None,
                    }));
                self.ui
                    .send(Action::CodeFile(code_workbench::FileRequest::CloseRange {
                        cursor: None,
                    }));
            }
            self.view = None;
            self.bounds = None;
            self.ready = false;
            self.save_shortcut = None;
            self.picker = false;
            self.menu_width = None;
            self.epoch = epoch;
            self.serial += 1;
            if epoch.is_some() {
                match self.create(window) {
                    Ok(view) => self.view = Some(view),
                    Err(e) => self.ui.state.revise(|s| s.code.error = format!("{e:#}")),
                }
            }
        }
        let Some(code) = &self.view else { return };
        let view = &code.web;
        let mut layout_changed = false;
        while let Ok((serial, raw)) = self.rx.try_recv() {
            if serial != self.serial {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            match msg["op"].as_str() {
                Some("ready") => {
                    self.ready = true;
                    layout_changed = true;
                }
                Some("titlePicker") => {
                    let visible = msg["visible"] == true;
                    layout_changed |= visible != self.picker;
                    self.picker = visible;
                }
                Some("menuWidth") => {
                    if let Some(width) = msg["width"].as_i64().filter(|n| (0..=32768).contains(n)) {
                        let width = Some(width as i32);
                        layout_changed |= width != self.menu_width;
                        self.menu_width = width;
                    }
                }
                Some("error") => self.ui.state.revise(|s| {
                    s.code.error = msg["message"]
                        .as_str()
                        .unwrap_or("Code initialization failed")
                        .to_owned()
                }),
                Some("dirty") => self
                    .ui
                    .state
                    .revise(|s| s.code.dirty = msg["value"] == true),
                Some("terminal") => crate::code_terminal::request(&self.ui, msg),
                Some("browseFiles") => {
                    if self
                        .ui
                        .state
                        .with(|s| s.workspace_page == WorkspacePage::Code)
                    {
                        let start = self
                            .ui
                            .state
                            .with(|s| (!s.code.root.is_empty()).then(|| s.code.root.clone()));
                        desktop_file_picker::open(
                            &self.ui,
                            desktop_file_picker::Purpose::Code,
                            start,
                        );
                    }
                }
                Some("fileOpened") => {
                    if let Some(path) = msg["path"].as_str().filter(|path| path.len() <= 4096) {
                        file_open::remember(
                            &self.ui,
                            path,
                            arcnix_core::file_associations::FileApp::Code,
                        );
                    }
                }
                Some("attachPage") => {
                    if let (Some(id), Some(path), Ok(snapshot)) = (
                        msg["id"].as_u64(),
                        msg["path"].as_str(),
                        serde_json::from_value(msg["snapshot"].clone()),
                    ) {
                        code_workbench::attach_page(&self.ui, id, path.into(), snapshot, None);
                    }
                }
                Some("openText") => {
                    if let (Some(id), Some(path)) = (msg["id"].as_u64(), msg["path"].as_str()) {
                        code_workbench::read(&self.ui, id, path.to_owned(), None);
                    }
                }
                Some("write") => {
                    if let Some(id) = msg["id"].as_u64()
                        && let Ok(write) = serde_json::from_value(msg)
                    {
                        code_workbench::save(&self.ui, id, Arc::new(write), None);
                    }
                }
                Some("focus") => {
                    if self.ui.state.with(|s| {
                        server_desktop::visible(s)
                            && s.workbench.window_visible(WorkspacePage::Code)
                            && s.workspace_page != WorkspacePage::Code
                            && s.dialog.is_none()
                    }) {
                        self.ui.open_desktop_app(WorkspacePage::Code);
                    }
                }
                _ => {
                    if let Some(id) = msg["id"].as_u64()
                        && let Ok(request) = serde_json::from_value(msg)
                    {
                        let background = matches!(
                            request,
                            code_workbench::FileRequest::PollTail { .. }
                                | code_workbench::FileRequest::CountLines { .. }
                                | code_workbench::FileRequest::CloseRange { .. }
                        );
                        let serial = self.ui.send(Action::CodeFile(request));
                        self.ui.state.revise(|s| {
                            if background {
                                s.code.background.insert(serial, id);
                            } else {
                                s.code
                                    .pending
                                    .insert(serial, code_workbench::Pending::File(id));
                            }
                        });
                    }
                }
            }
        }
        if self.ui.state.with(|s| !s.code.replies.is_empty()) {
            let replies = self
                .ui
                .state
                .revise(|s| std::mem::take(&mut s.code.replies));
            let _ = view.evaluate_script(&format!("window.arcnixReply({})", json!(replies)));
        }
        if self.ready {
            let shortcut = self.ui.state.with(|s| {
                desktop_shortcuts_ui::keys(s)
                    .chords(arcnix_core::desktop_shortcuts::Action::CodeSave)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            });
            if self.save_shortcut.as_ref() != Some(&shortcut) {
                let _ = view.evaluate_script(&format!(
                    "window.arcnixSetSaveShortcut({});",
                    json!(shortcut)
                ));
                self.save_shortcut = Some(shortcut);
            }
        }
        if self.ui.state.with(|s| !s.code.terminals.events.is_empty()) {
            let events = self
                .ui
                .state
                .revise(|s| std::mem::take(&mut s.code.terminals.events));
            let _ =
                view.evaluate_script(&format!("window.arcnixTerminalEvents({})", json!(events)));
        }
        if self.ready && self.ui.state.with(|s| !s.code.commands.is_empty()) {
            for command in self
                .ui
                .state
                .revise(|s| std::mem::take(&mut s.code.commands))
            {
                let _ = view.focus();
                let _ = view.evaluate_script(&format!("window.arcnixCommand({})", json!(command)));
            }
        }
        if self.ready && self.ui.state.with(|s| !s.code.open.is_empty()) {
            for path in self.ui.state.revise(|s| std::mem::take(&mut s.code.open)) {
                let _ = view.evaluate_script(&format!(
                    "((path) => window.arcnixOpen(path).then(editor => {{ if (editor) window.ipc.postMessage(JSON.stringify({{op:'fileOpened',path}})); }}))({})",
                    json!(path)
                ));
            }
        }
        let next = self.ui.state.with(|s| {
            if !server_desktop::visible(s)
                || !s.workbench.window_visible(WorkspacePage::Code)
                || s.dialog.is_some()
                || s.fingerprint.is_some()
                || s.code.confirm
                || !s.code.error.is_empty()
            {
                return None;
            }
            s.code
                .viewport
                .clone()
                .filter(|v| v.area.is_some() && Some(v.epoch) == epoch)
        });
        if self.bounds != next || layout_changed {
            if let Some(port) = &next {
                if let Err(error) = position(code, port, self.picker, self.menu_width) {
                    self.ui.state.revise(|s| s.code.error = error.to_string());
                }
            } else {
                let _ = view.set_visible(false);
            }
            self.bounds = next;
        }
    }
}
