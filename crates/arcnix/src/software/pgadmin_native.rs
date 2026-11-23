//! pgAdmin's interface in a child WebView limited to one loopback address.
use std::sync::mpsc;
use std::task::Waker;

use anyhow::{Context, Result};
use glory::wry::shell::NativeWindow;
use wry::{WebView, WebViewBuilder};

use crate::*;

/// Navigation stays on the backend this window started. pgAdmin opens each
/// tool in an inline frame (`about:srcdoc`) that then posts to the backend.
fn same_origin(origin: &str, url: &str) -> bool {
    url == "about:blank"
        || url == "about:srcdoc"
        || url
            .strip_prefix(origin)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']))
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
            bounds: None,
        }
    }

    fn create(&mut self, window: &NativeWindow, url: &str) -> Result<View> {
        let origin = origin(url).context("Invalid pgAdmin address")?.to_owned();
        let wake = self.wake.clone().context("Desktop not ready")?;
        let (send, receive) = mpsc::channel();
        self.focus = receive;
        let allowed = origin.clone();
        let web = WebViewBuilder::new()
            .with_visible(false)
            .with_focused(false)
            .with_incognito(true)
            .with_initialization_script(
                "addEventListener('pointerdown', () => window.ipc.postMessage('focus'), true);",
            )
            .with_navigation_handler(move |target| same_origin(&allowed, &target))
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_permission_handler(|_| wry::PermissionResponse::Deny)
            .with_download_started_handler(|_, _| false)
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
            assert!(same_origin(origin, url), "{url}");
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
            assert!(!same_origin(origin, url), "{url}");
        }
        assert!(super::origin("http://example.com/").is_none());
    }
}
