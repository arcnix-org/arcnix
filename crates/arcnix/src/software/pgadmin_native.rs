//! pgAdmin's interface in a child WebView limited to one loopback address.
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::task::Waker;

use anyhow::{Context, Result};
use glory::wry::shell::NativeWindow;
use wry::{WebView, WebViewBuilder};

use crate::*;

/// Navigation stays on the backend this window started. pgAdmin opens each
/// tool in an inline frame (`about:srcdoc`) that then posts to the backend.
fn same_origin(origin: &str, url: &str) -> bool {
    url.strip_prefix(origin)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']))
}

fn navigable(origin: &str, url: &str) -> bool {
    matches!(url, "about:blank" | "about:srcdoc") || same_origin(origin, url)
}

/// The WebView and, on macOS, the surface that clips it to what the desktop
/// exposes. The WebView is dropped first, as in the Code window.
struct View {
    web: WebView,
    #[cfg(target_os = "macos")]
    surface: mac_surface::Surface,
}

#[cfg(target_os = "macos")]
fn position(view: &View, viewport: &native_viewport::Viewport) -> Result<()> {
    use wry::WebViewExtMacOS;
    view.surface.position(&view.web.webview(), viewport)
}

/// pgAdmin is offered on macOS only. Elsewhere a covered view is hidden.
#[cfg(not(target_os = "macos"))]
fn position(view: &View, viewport: &native_viewport::Viewport) -> Result<()> {
    use wry::dpi::{PhysicalPosition, PhysicalSize};
    let Some(area) = viewport
        .area
        .zip(viewport.frame)
        .and_then(|(a, f)| a.intersect(f))
    else {
        view.web.set_visible(false)?;
        return Ok(());
    };
    view.web.set_bounds(wry::Rect {
        position: PhysicalPosition::new(area.left, area.top).into(),
        size: PhysicalSize::new(area.width() as u32, area.height() as u32).into(),
    })?;
    view.web.set_visible(viewport.clip.is_empty())?;
    Ok(())
}

/// Files pgAdmin saves: links to its own blobs and data, or backend attachments.
fn downloadable(origin: &str, url: &str) -> bool {
    url.starts_with("data:") || same_origin(origin, url.strip_prefix("blob:").unwrap_or(url))
}

/// Keep only the name WebKit suggested, inside the Downloads folder, without
/// replacing an existing file.
fn destination(folder: &Path, suggested: &Path, pending: &[(String, PathBuf)]) -> Option<PathBuf> {
    let name = suggested.file_name()?.to_str()?;
    if name.starts_with('.') || name.chars().any(|c| c.is_control() || c == ':') {
        return None;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    let mut path = folder.join(name);
    for counter in 1..1000 {
        if path
            .symlink_metadata()
            .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
            && !pending.iter().any(|(_, other)| *other == path)
        {
            return Some(path);
        }
        path = folder.join(format!("{stem} ({counter}){extension}"));
    }
    None
}

pub(crate) fn downloads_folder() -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join("Downloads"))
}

fn origin(url: &str) -> Option<&str> {
    let start = "http://127.0.0.1:".len();
    url.starts_with("http://127.0.0.1:").then_some(())?;
    let end = url[start..].find('/')? + start;
    Some(&url[..end])
}

pub(crate) struct Host {
    ui: Ui,
    wake: Option<Waker>,
    url: Option<String>,
    view: Option<View>,
    focus: mpsc::Receiver<()>,
    downloads: mpsc::Receiver<pgadmin_ui::Download>,
    bounds: Option<native_viewport::Viewport>,
}

impl Host {
    pub fn new(ui: Ui, wake: Option<Waker>) -> Self {
        Self {
            ui,
            wake,
            url: None,
            view: None,
            focus: mpsc::channel().1,
            downloads: mpsc::channel().1,
            bounds: None,
        }
    }

    fn create(&mut self, window: &NativeWindow, url: &str) -> Result<View> {
        let origin = origin(url).context("Invalid pgAdmin address")?.to_owned();
        let wake = self.wake.clone().context("Desktop not ready")?;
        let (send, receive) = mpsc::channel();
        self.focus = receive;
        let allowed = origin.clone();
        let source = origin.clone();
        let (saved, downloads) = mpsc::channel();
        self.downloads = downloads;
        let started: Rc<RefCell<Vec<(String, PathBuf)>>> = Rc::default();
        let finished = started.clone();
        let completed_wake = wake.clone();
        let web = WebViewBuilder::new()
            .with_visible(false)
            .with_focused(false)
            .with_incognito(true)
            .with_initialization_script(
                "addEventListener('pointerdown', () => window.ipc.postMessage('focus'), true);",
            )
            .with_navigation_handler(move |target| navigable(&allowed, &target))
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_permission_handler(|_| wry::PermissionResponse::Deny)
            .with_download_started_handler(move |url, path| {
                let mut started = started.borrow_mut();
                let Some(target) = downloads_folder()
                    .filter(|_| downloadable(&source, &url))
                    .and_then(|folder| destination(&folder, path, &started))
                else {
                    return false;
                };
                *path = target.clone();
                started.push((url, target));
                true
            })
            .with_download_completed_handler(move |url, _, success| {
                // macOS reports no path, so match the request instead.
                let mut started = finished.borrow_mut();
                if let Some(index) = started.iter().position(|(started, _)| *started == url) {
                    let (_, path) = started.remove(index);
                    if saved.send(pgadmin_ui::Download { path, success }).is_ok() {
                        completed_wake.wake_by_ref();
                    }
                }
            })
            .with_ipc_handler(move |request| {
                // The page can only ask for its own window to come forward.
                if same_origin(&origin, &request.uri().to_string())
                    && request.body() == "focus"
                    && send.send(()).is_ok()
                {
                    wake.wake_by_ref();
                }
            })
            .with_url(url)
            .build_as_child(window)?;
        #[cfg(target_os = "macos")]
        let surface = {
            use wry::WebViewExtMacOS;
            mac_surface::Surface::attach(&web.webview())?
        };
        Ok(View {
            web,
            #[cfg(target_os = "macos")]
            surface,
        })
    }

    pub fn hide(&mut self) {
        if let Some(view) = &self.view {
            let _ = view.web.set_visible(false);
        }
        self.bounds = None;
    }

    pub fn poll(&mut self, window: &NativeWindow) {
        let url = self
            .ui
            .state
            .with(|s| s.pgadmin.url().filter(|_| s.connected).map(str::to_owned));
        if self.url != url {
            self.view = None;
            self.bounds = None;
            self.url = url.clone();
            if let Some(url) = &url {
                match self.create(window, url) {
                    Ok(view) => self.view = Some(view),
                    Err(error) => self
                        .ui
                        .state
                        .revise(|s| s.pgadmin.error = format!("{error:#}")),
                }
            }
        }
        let (Some(view), Some(url)) = (&self.view, &self.url) else {
            return;
        };
        if self.focus.try_iter().count() > 0
            && self.ui.state.with(|s| {
                server_desktop::visible(s)
                    && s.workbench.window_visible(WorkspacePage::PgAdmin)
                    && s.workspace_page != WorkspacePage::PgAdmin
                    && s.dialog.is_none()
            })
        {
            self.ui.open_desktop_app(WorkspacePage::PgAdmin);
        }
        if let Some(download) = self.downloads.try_iter().last() {
            self.ui
                .state
                .revise(|s| s.pgadmin.download = Some(download));
        }
        if self.ui.state.with(|s| s.pgadmin.reload) {
            self.ui.state.revise(|s| s.pgadmin.reload = false);
            let _ = view.web.load_url(url);
        }
        let next = self.ui.state.with(|s| {
            if !server_desktop::visible(s)
                || !s.workbench.window_visible(WorkspacePage::PgAdmin)
                || s.dialog.is_some()
                || s.fingerprint.is_some()
            {
                return None;
            }
            s.pgadmin
                .viewport
                .clone()
                .filter(|v| v.area.is_some() && v.epoch == s.epoch)
        });
        if self.bounds != next {
            let shown = match &next {
                Some(port) => position(view, port),
                None => view.web.set_visible(false).map_err(Into::into),
            };
            if let Err(error) = shown {
                self.ui
                    .state
                    .revise(|s| s.pgadmin.error = format!("{error:#}"));
            }
            self.bounds = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_stays_on_the_started_backend() {
        let origin = origin("http://127.0.0.1:5050/?key=abc").unwrap();
        assert_eq!(origin, "http://127.0.0.1:5050");
        for url in [
            "http://127.0.0.1:5050",
            "http://127.0.0.1:5050/browser/",
            "http://127.0.0.1:5050?key=abc",
            "about:blank",
            "about:srcdoc",
        ] {
            assert!(navigable(origin, url), "{url}");
        }
        for url in [
            "http://127.0.0.1:50501/",
            "http://127.0.0.1:5050@example.com/",
            "https://127.0.0.1:5050/",
            "https://www.pgadmin.org/",
            "file:///etc/hosts",
            "about:srcdoc#x",
            "data:text/html,x",
        ] {
            assert!(!navigable(origin, url), "{url}");
        }
        assert!(super::origin("http://example.com/").is_none());
        for url in [
            "blob:http://127.0.0.1:5050/0c7e",
            "data:text/csv;base64,YQ==",
            "http://127.0.0.1:5050/misc/download",
        ] {
            assert!(downloadable(origin, url), "{url}");
        }
        for url in [
            "blob:http://127.0.0.1:5051/0c7e",
            "blob:about:blank",
            "blob:about:srcdoc",
            "https://example.com/file.csv",
            "file:///etc/hosts",
        ] {
            assert!(!downloadable(origin, url), "{url}");
        }
    }

    #[test]
    fn downloads_stay_in_the_folder_and_keep_existing_files() {
        let folder =
            std::env::temp_dir().join(format!("arcnix-downloads-{}", arcnix_core::new_id()));
        std::fs::create_dir(&folder).unwrap();
        let first = destination(&folder, Path::new("/elsewhere/orders.csv"), &[]).unwrap();
        assert_eq!(first, folder.join("orders.csv"));
        assert_eq!(
            destination(
                &folder,
                Path::new("orders.csv"),
                &[(String::new(), first.clone())]
            )
            .unwrap(),
            folder.join("orders (1).csv")
        );
        std::fs::write(&first, "").unwrap();
        assert_eq!(
            destination(&folder, Path::new("orders.csv"), &[]).unwrap(),
            folder.join("orders (1).csv")
        );
        assert_eq!(
            destination(&folder, Path::new("README"), &[]).unwrap(),
            folder.join("README")
        );
        for name in [".profile", "..", "a\nb.csv"] {
            assert!(
                destination(&folder, Path::new(name), &[]).is_none(),
                "{name}"
            );
        }
        std::fs::remove_dir_all(folder).unwrap();
    }
}
