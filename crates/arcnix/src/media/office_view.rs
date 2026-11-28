//! Isolated Office document WebView with Syntra menus, reader and Writer surface.
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};
use std::task::Waker;

use anyhow::Result;
use glory::wry::shell::NativeWindow;
use wry::{WebView, WebViewBuilder};

use crate::native_viewport::Viewport;
use crate::office_reader::{self, Document};
use crate::{Ui, desktop_system_settings, server_desktop, window_tabs};

#[cfg(windows)]
const ORIGIN: &str = "http://arcnixoffice.localhost";
#[cfg(not(windows))]
const ORIGIN: &str = "arcnixoffice://localhost";

// WebView2's UI language can differ from the Windows regional format.
fn regional_locale() -> Option<String> {
    #[cfg(windows)]
    {
        let mut name = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH includes the terminator.
        let len = unsafe { windows::Win32::Globalization::GetUserDefaultLocaleName(&mut name) };
        if len > 1 {
            return String::from_utf16(&name[..len as usize - 1]).ok();
        }
    }
    None
}

fn resource(
    req: &wry::http::Request<Vec<u8>>,
    source: &Document,
    draft: Option<&str>,
) -> wry::http::Response<Cow<'static, [u8]>> {
    let mut response = wry::http::Response::builder()
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff");
    let (mime, data): (_, Cow<'static, [u8]>) =
        match (req.method().as_str(), req.uri().path(), draft) {
            ("GET", "/document", None) => (
                "text/html; charset=utf-8",
                Cow::Owned(source.html.as_bytes().to_vec()),
            ),
            ("GET", "/edit", Some(_)) => (
                "text/html; charset=utf-8",
                Cow::Borrowed(include_bytes!("../../assets/office-editor.html")),
            ),
            ("GET", "/office-editor.js", Some(_)) => (
                "text/javascript",
                Cow::Borrowed(include_bytes!("../../assets/office-editor.js")),
            ),
            ("GET", "/writer-surface.js", Some(_)) => (
                "text/javascript",
                Cow::Borrowed(syntra_office_ui::WRITER_SURFACE_JS),
            ),
            ("GET", "/embedded-writer.js", Some(_)) => (
                "text/javascript",
                Cow::Borrowed(syntra_office_ui::EMBEDDED_WRITER_JS),
            ),
            ("GET", "/model.json", Some(model)) => {
                ("application/json", Cow::Owned(model.as_bytes().to_vec()))
            }
            _ => {
                response = response.status(404);
                ("text/plain", Cow::Borrowed(b""))
            }
        };
    let policy = if draft.is_some() {
        "default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'"
    } else {
        "default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:; base-uri 'none'; form-action 'none'"
    };
    response
        .header("Content-Type", mime)
        .header("Content-Security-Policy", policy)
        .body(data)
        .unwrap()
}

#[derive(Clone, PartialEq, Eq)]
struct OfficeTheme {
    dark: bool,
    css: String,
}

impl OfficeTheme {
    fn current(s: &crate::State) -> Self {
        let theme = crate::desktop_appearance_ui::theme(s);
        let dark = theme
            .mode
            .is_dark(s.config.settings.theme.is_dark(s.system_dark));
        let mut css = String::from(":root{");
        for (role, color) in theme.palette(dark) {
            css.push_str(&format!(
                "--office-{}:{color};",
                role.variable().trim_start_matches('-')
            ));
        }
        css.push_str(&format!(
            "--office-fill:{};--office-scheme:{}}}",
            theme.accent_fill(dark),
            if dark { "dark" } else { "light" }
        ));
        Self { dark, css }
    }

    #[cfg(windows)]
    fn native(&self) -> wry::Theme {
        if self.dark {
            wry::Theme::Dark
        } else {
            wry::Theme::Light
        }
    }
}

struct View {
    theme: OfficeTheme,
    web: WebView,
    #[cfg(target_os = "macos")]
    surface: crate::mac_surface::Surface,
    document: Arc<Document>,
    editing: bool,
    messages: mpsc::Receiver<String>,
    bounds: Option<Viewport>,
}

impl View {
    fn new(
        window: &NativeWindow,
        document: Arc<Document>,
        ui: &Ui,
        id: u64,
        draft: Option<Arc<str>>,
        wake: Option<Waker>,
        theme: &OfficeTheme,
    ) -> Result<Self> {
        let source = document.clone();
        let (send, messages) = mpsc::channel();
        let editing = draft.is_some();
        let limit = syntra_doc_engine::MAX_DOCUMENT_CONTENT_BYTES;
        let menus = syntra_office_ui::embedded_menus(document.kind);
        let (chinese, dirty, remote, editable) = ui.state.with(|s| {
            let reader = &s.office.files[&id];
            (
                desktop_system_settings::language(s) == arcnix_core::Language::SimplifiedChinese,
                reader.dirty,
                reader.remote().is_some(),
                office_reader::can_edit(s, reader),
            )
        });
        let options = serde_json::json!({"zh":chinese,"locale":regional_locale(),"menus":menus,"kind":document.kind,"width":document.width_px,
            "editable":editable,"limit":limit,"dirty":dirty,"remote":remote});
        let css = serde_json::to_string(&format!(
            "{}\n{}",
            syntra_office_ui::COMMAND_BAR_CSS,
            include_str!("../../assets/office-theme.css")
        ))?;
        let palette = serde_json::to_string(&theme.css)?;
        let reader = if editing {
            ""
        } else {
            syntra_office_ui::READER_JS
        };
        let script = format!(
            "window.officeOptions = {options}; window.officePalette = {palette};\n{}\n{reader}\n(() => {{
                const mount = window.SyntraOffice.mount;
                const open = () => window.ipc.postMessage('open');
                window.SyntraOffice.mount = options => {{
                    options.actions.open = {{label: ['Open…', '打开…'], shortcut: 'Ctrl+O', run: open}};
                    return mount(options);
                }};
                addEventListener('DOMContentLoaded', () => {{
                    const style = document.createElement('style'); style.textContent = {css}; document.head.append(style);
                    const palette = document.createElement('style'); palette.id = 'office-theme';
                    palette.textContent = window.officePalette; document.head.append(palette);
                    if (window.SyntraOffice.mountReader) window.SyntraOffice.mountReader({{...officeOptions,
                        open,
                        edit: officeOptions.editable ? () => window.ipc.postMessage('edit') : null}});
                }});
            }})(); addEventListener('keydown', e => {{
                if (e.key === 'F5' || ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'r')) e.preventDefault();
            }}); addEventListener('pointerdown', () => window.ipc.postMessage('focus'), true);",
            syntra_office_ui::COMMAND_BAR_JS
        );
        let entry = if editing { "/edit" } else { "/document" };
        let url = format!("{ORIGIN}{entry}");
        let allowed = url.clone();
        #[cfg(not(windows))]
        let builder = WebViewBuilder::new();
        #[cfg(windows)]
        let mut context = ui.webview_context("office");
        #[cfg(windows)]
        let builder = {
            use wry::WebViewBuilderExtWindows;
            WebViewBuilder::new_with_web_context(&mut context)
                .with_browser_accelerator_keys(false)
                .with_theme(theme.native())
        };
        let web = builder
            .with_visible(false)
            .with_focused(false)
            .with_incognito(true)
            .with_custom_protocol("arcnixoffice".into(), move |_, req| {
                resource(&req, &source, draft.as_deref())
            })
            .with_initialization_script(script)
            .with_navigation_handler(move |url| url == allowed)
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_permission_handler(|_| wry::PermissionResponse::Deny)
            .with_download_started_handler(|_, _| false)
            .with_ipc_handler(move |req| {
                if req.body().len() <= limit + 64
                    && (matches!(req.body().as_str(), "focus" | "open" | "edit") || editing)
                    && send.send(req.into_body()).is_ok()
                    && let Some(wake) = &wake
                {
                    wake.wake_by_ref();
                }
            })
            .with_url(url)
            .build_as_child(window)?;
        #[cfg(target_os = "macos")]
        let surface = {
            use wry::WebViewExtMacOS;
            crate::mac_surface::Surface::attach(&web.webview())?
        };
        Ok(Self {
            theme: theme.clone(),
            web,
            #[cfg(target_os = "macos")]
            surface,
            document,
            messages,
            editing,
            bounds: None,
        })
    }

    fn sync_theme(&mut self, theme: &OfficeTheme) -> Result<()> {
        if &self.theme != theme {
            #[cfg(windows)]
            if self.theme.dark != theme.dark {
                use wry::WebViewExtWindows;
                self.web.set_theme(theme.native())?;
            }
            let css = serde_json::to_string(&theme.css)?;
            self.web.evaluate_script(&format!(
                "window.officePalette = {css}; if (document.getElementById('office-theme')) document.getElementById('office-theme').textContent = window.officePalette;"
            ))?;
            self.theme = theme.clone();
        }
        Ok(())
    }

    fn position(&self, bounds: &Viewport) -> Result<()> {
        #[cfg(windows)]
        return crate::web_browser_native::position(&self.web, bounds);
        #[cfg(target_os = "macos")]
        {
            use wry::WebViewExtMacOS;
            self.surface.position(&self.web.webview(), bounds)
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let area = bounds
                .area
                .zip(bounds.frame)
                .and_then(|(a, f)| a.intersect(f));
            if let Some(area) = area {
                self.web.set_bounds(wry::Rect {
                    position: wry::dpi::PhysicalPosition::new(area.left, area.top).into(),
                    size: wry::dpi::PhysicalSize::new(area.width() as u32, area.height() as u32)
                        .into(),
                })?;
            }
            self.web
                .set_visible(area.is_some() && bounds.clip.is_empty())?;
            Ok(())
        }
    }
}

#[derive(Default)]
pub(crate) struct Host {
    views: BTreeMap<u64, View>,
}

impl Host {
    pub fn hide(&mut self) {
        for view in self.views.values_mut() {
            if view.bounds.take().is_some() {
                let _ = view.web.set_visible(false);
            }
        }
    }

    pub fn poll(&mut self, window: &NativeWindow, ui: &Ui, wake: Option<&Waker>) {
        let documents = ui.state.with(|s| {
            self.views.retain(|id, _| {
                s.office
                    .files
                    .get(id)
                    .is_some_and(|doc| doc.document.is_some())
            });
            s.office
                .files
                .iter()
                .filter_map(|(&id, doc)| {
                    let owner = s.office.groups.owner(id)?;
                    let page =
                        window_tabs::page(arcnix_core::server_apps::DesktopApp::Office, owner);
                    let bounds = (s.office.groups.selected(owner) == Some(id)
                        && server_desktop::visible(s)
                        && s.workbench.window_visible(page)
                        && s.workbench.overview.is_none()
                        && s.dialog.is_none()
                        && s.fingerprint.is_none()
                        && !doc.is_saving())
                    .then(|| s.office_viewports.get(&owner))
                    .flatten()
                    .filter(|v| v.epoch == s.epoch && v.area.is_some())
                    .cloned();
                    Some((id, page, doc.document.clone()?, doc.draft.clone(), bounds))
                })
                .collect::<Vec<_>>()
        });
        if documents.is_empty() {
            return;
        }
        let theme = ui.state.with(OfficeTheme::current);
        for (id, page, document, draft, bounds) in documents {
            if self.views.get(&id).is_none_or(|v| {
                !Arc::ptr_eq(&v.document, &document) || v.editing != draft.is_some()
            }) {
                self.views.remove(&id);
                if bounds.is_none() {
                    continue;
                }
                match View::new(window, document, ui, id, draft, wake.cloned(), &theme) {
                    Ok(view) => {
                        self.views.insert(id, view);
                    }
                    Err(error) => {
                        Self::failed(ui, id, error);
                        continue;
                    }
                }
            }
            let view = self.views.get_mut(&id).unwrap();
            let _ = view.sync_theme(&theme);
            for message in view.messages.try_iter() {
                if message == "focus" && bounds.is_some() {
                    if ui.state.with(|s| s.workspace_page != page) {
                        ui.focus_desktop_app(page);
                    }
                } else if bounds.is_some() && message == "open" {
                    ui.focus_desktop_app(page);
                    office_reader::browse(ui);
                } else if bounds.is_some() && message == "edit" {
                    office_reader::begin_edit(ui, id);
                } else if view.editing
                    && (bounds.is_some()
                        || message.starts_with("model\n")
                        || message.starts_with("plain\n"))
                    && ui.state.with(|s| {
                        s.office
                            .files
                            .get(&id)
                            .is_some_and(|d| d.draft.is_some() && !d.is_saving())
                    })
                {
                    office_reader::edit_message(ui, id, &view.web, &message);
                }
            }
            if bounds != view.bounds {
                let result = match &bounds {
                    Some(port) => view.position(port),
                    None => view.web.set_visible(false).map_err(Into::into),
                };
                if let Err(error) = result {
                    Self::failed(ui, id, error);
                }
                view.bounds = bounds;
            }
        }
    }

    fn failed(ui: &Ui, id: u64, error: anyhow::Error) {
        ui.state.revise(|s| {
            if let Some(doc) = s.office.files.get_mut(&id) {
                doc.document = None;
                doc.error = format!("{error:#}");
            }
        });
    }
}
