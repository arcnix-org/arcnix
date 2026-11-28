//! pgAdmin's interface in a child WebView limited to one loopback address.
use std::cell::RefCell;
use std::collections::VecDeque;
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

/// Windows clips the view to the exposed region, as for Code.
#[cfg(windows)]
fn position(view: &View, viewport: &native_viewport::Viewport) -> Result<()> {
    crate::web_browser_native::position(&view.web, viewport)
}

/// pgAdmin is not offered here yet. A covered view is hidden.
#[cfg(not(any(target_os = "macos", windows)))]
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

/// Keep only the name the WebView suggested, inside `folder`, without
/// replacing an existing file.
fn destination(folder: &Path, suggested: &Path) -> Option<PathBuf> {
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
        {
            return Some(path);
        }
        path = folder.join(format!("{stem} ({counter}){extension}"));
    }
    None
}

pub(crate) fn downloads_folder() -> Option<PathBuf> {
    // Windows lets people move Downloads; ask for its known folder.
    #[cfg(windows)]
    if let Some(folder) =
        directories::UserDirs::new().and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
    {
        return Some(folder);
    }
    std::env::home_dir().map(|home| home.join("Downloads"))
}

/// Move a finished download to where the person chose. The staging folder
/// may be on another volume; a staged copy left behind is removed with it.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => return Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices => {}
        Err(err) => return Err(err),
    }
    let folder = to.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    let tmp = copy_into(from, folder)?;
    tmp.persist(to)?;
    Ok(())
}

fn copy_into(from: &Path, folder: &Path) -> std::io::Result<tempfile::NamedTempFile> {
    let mut tmp = tempfile::NamedTempFile::new_in(folder)?;
    std::io::copy(&mut std::fs::File::open(from)?, tmp.as_file_mut())?;
    Ok(tmp)
}

/// An unattended save must never replace an existing export, even when two
/// windows finish downloads with the same name at the same time.
fn keep_in_folder(staged: &Path, folder: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let mut tmp = copy_into(staged, folder)?;
    loop {
        let target = destination(folder, staged).ok_or(std::io::ErrorKind::AlreadyExists)?;
        match tmp.persist_noclobber(&target) {
            Ok(_) => return Ok(target),
            Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => tmp = err.file,
            Err(err) => return Err(err.error),
        }
    }
}

struct Staged {
    folder: tempfile::TempDir,
    path: PathBuf,
}

impl Staged {
    fn new(suggested: &Path) -> Option<Self> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("arcnix-pgadmin-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let folder = builder.tempdir().ok()?;
        let path = destination(folder.path(), suggested)?;
        Some(Self { folder, path })
    }

    fn finish(self, choice: Choice) -> Option<pgadmin_ui::Download> {
        let saved = match choice {
            Ok(None) => return None,
            Ok(Some(target)) => move_file(&self.path, &target).map(|()| target),
            Err(_) => downloads_folder()
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
                .and_then(|folder| keep_in_folder(&self.path, &folder)),
        };
        Some(match saved {
            Ok(path) => pgadmin_ui::Download {
                path,
                success: true,
            },
            Err(_) => {
                // Keep the completed export recoverable via the title bar.
                let _ = self.folder.keep();
                pgadmin_ui::Download {
                    path: self.path,
                    success: false,
                }
            }
        })
    }
}

type Choice = Result<Option<PathBuf>>;
type Answer = (Staged, Choice);

fn completed_index(pending: &[(String, Staged)], url: &str, path: Option<&Path>) -> Option<usize> {
    match path {
        Some(path) => pending.iter().position(|(_, staged)| staged.path == path),
        None => pending.iter().position(|(address, _)| address == url),
    }
}

/// Ask where to save `name`. Windows runs its dialog to completion here.
#[cfg(windows)]
fn ask(
    window: &NativeWindow,
    name: &str,
    folder: Option<&Path>,
    reply: impl FnOnce(Choice) + 'static,
) {
    use glory::wry::shell::NativeWindowExtWindows;
    // Owned by Arcnix's window even when another app is in front.
    reply(file_picker::save_in(
        &i18n::desktop::translate("Save file"),
        name,
        folder,
        Some(window.hwnd()),
    ));
}

/// A sheet on the Arcnix window; AppKit answers later on the main thread.
#[cfg(target_os = "macos")]
fn ask(
    window: &NativeWindow,
    name: &str,
    folder: Option<&Path>,
    reply: impl FnOnce(Choice) + 'static,
) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponse, NSModalResponseOK, NSSavePanel, NSWindow};
    use objc2_foundation::{NSString, NSURL};
    use tao::platform::macos::WindowExtMacOS;

    let Some(mtm) = MainThreadMarker::new() else {
        reply(Err(anyhow::anyhow!("The save panel needs the main thread")));
        return;
    };
    let panel = NSSavePanel::savePanel(mtm);
    panel.setNameFieldStringValue(&NSString::from_str(name));
    panel.setCanCreateDirectories(true);
    if let Some(folder) = folder.and_then(Path::to_str) {
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(folder), true);
        panel.setDirectoryURL(Some(&url));
    }
    let answered = panel.clone();
    let reply = RefCell::new(Some(reply));
    let handler = block2::RcBlock::new(move |response: NSModalResponse| {
        let path = (response == NSModalResponseOK)
            .then(|| answered.URL())
            .flatten()
            .and_then(|url| url.path())
            .map(|path| PathBuf::from(path.to_string()));
        if let Some(reply) = reply.borrow_mut().take() {
            reply(Ok(path));
        }
    });
    // SAFETY: the shell's window is an NSWindow that outlives this call.
    let native = unsafe { &*window.ns_window().cast::<NSWindow>() };
    panel.beginSheetModalForWindow_completionHandler(native, &handler);
}

#[cfg(not(any(target_os = "macos", windows)))]
fn ask(_: &NativeWindow, _: &str, _: Option<&Path>, reply: impl FnOnce(Choice) + 'static) {
    reply(Err(anyhow::anyhow!("No save dialog on this platform")));
}

pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
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
    /// Completed downloads retain their own staging directories.
    downloads: mpsc::Receiver<(Staged, bool)>,
    /// Downloads waiting for the person to choose where they go.
    waiting: VecDeque<Staged>,
    /// A save dialog owns its staged file until it replies.
    asking: bool,
    choices: (mpsc::Sender<Answer>, mpsc::Receiver<Answer>),
    /// Copies to another volume run off the UI thread.
    moves: (
        mpsc::Sender<Option<pgadmin_ui::Download>>,
        mpsc::Receiver<Option<pgadmin_ui::Download>>,
    ),
    moving: bool,
    /// The folder of the last saved file, where the next dialog starts.
    folder: Option<PathBuf>,
    bounds: Option<native_viewport::Viewport>,
}

impl Drop for Host {
    /// pgAdmin closed before every download was saved: keep those files in
    /// Downloads rather than losing them with the staging folder.
    fn drop(&mut self) {
        let finished = self
            .downloads
            .try_iter()
            .filter(|(_, ok)| *ok)
            .map(|(file, _)| file);
        let pending: Vec<_> = self
            .choices
            .1
            .try_iter()
            .chain(
                self.waiting
                    .drain(..)
                    .chain(finished)
                    .map(|file| (file, Err(anyhow::anyhow!("Window closed")))),
            )
            .collect();
        if !pending.is_empty() {
            std::thread::spawn(move || {
                for (file, choice) in pending {
                    let _ = file.finish(choice);
                }
            });
        }
    }
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
            waiting: VecDeque::new(),
            asking: false,
            choices: mpsc::channel(),
            moves: mpsc::channel(),
            moving: false,
            folder: None,
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
        // A replaced view may have finished downloads not yet asked about.
        let earlier = std::mem::replace(&mut self.downloads, downloads);
        self.waiting.extend(
            earlier
                .try_iter()
                .filter(|(_, success)| *success)
                .map(|(staged, _)| staged),
        );
        let started: Rc<RefCell<Vec<(String, Staged)>>> = Rc::default();
        let finished = started.clone();
        let completed_wake = wake.clone();
        #[cfg(not(windows))]
        let builder = WebViewBuilder::new();
        #[cfg(windows)]
        let mut context = self.ui.webview_context("pgadmin");
        #[cfg(windows)]
        let builder = WebViewBuilder::new_with_web_context(&mut context);
        let web = builder
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
                // Each file gets its own folder, so the name stays as
                // suggested for the save dialog.
                if !downloadable(&source, &url) {
                    return false;
                }
                let Some(staged) = Staged::new(path) else {
                    return false;
                };
                *path = staged.path.clone();
                started.borrow_mut().push((url, staged));
                true
            })
            .with_download_completed_handler(move |url, reported, success| {
                // WebView2 reports the exact staged file, even for duplicate URLs.
                // WebKit only reports the request address.
                let mut started = finished.borrow_mut();
                if let Some(index) = completed_index(&started, &url, reported.as_deref()) {
                    let (_, path) = started.remove(index);
                    if saved.send((path, success)).is_ok() {
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

    /// Ask where each finished download goes, one dialog at a time.
    fn save_downloads(&mut self, window: &NativeWindow) {
        for (staged, success) in self.downloads.try_iter() {
            if success {
                self.waiting.push_back(staged);
            } else {
                let folder = self.folder.clone().or_else(downloads_folder);
                let path = folder.unwrap_or_default().join(file_name(&staged.path));
                self.report(pgadmin_ui::Download {
                    path,
                    success: false,
                });
            }
        }
        while let Ok(result) = self.moves.1.try_recv() {
            self.moving = false;
            if let Some(download) = result {
                if download.success {
                    self.folder = download.path.parent().map(Path::to_path_buf);
                }
                self.report(download);
            }
        }
        if let Ok((file, choice)) = self.choices.1.try_recv() {
            self.asking = false;
            self.moving = true;
            let moved = self.moves.0.clone();
            let wake = self.wake.clone();
            std::thread::spawn(move || {
                if moved.send(file.finish(choice)).is_ok()
                    && let Some(wake) = wake
                {
                    wake.wake();
                }
            });
        }
        if self.asking || self.moving {
            return;
        }
        let Some(file) = self.waiting.pop_front() else {
            return;
        };
        self.asking = true;
        let folder = self.folder.clone().or_else(downloads_folder);
        let reply = self.choices.0.clone();
        let wake = self.wake.clone();
        ask(
            window,
            &file_name(&file.path),
            folder.as_deref(),
            move |choice| {
                if let Err(err) = reply.send((file, choice)) {
                    let (file, choice) = err.0;
                    std::thread::spawn(move || file.finish(choice));
                } else if let Some(wake) = wake {
                    wake.wake();
                }
            },
        );
    }

    fn report(&self, download: pgadmin_ui::Download) {
        self.ui
            .state
            .revise(|s| s.pgadmin.download = Some(download));
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
        self.save_downloads(window);
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
        let first = destination(&folder, Path::new("/elsewhere/orders.csv")).unwrap();
        assert_eq!(first, folder.join("orders.csv"));
        assert_eq!(
            destination(&folder, Path::new("orders.csv")).unwrap(),
            folder.join("orders.csv")
        );
        std::fs::write(&first, "").unwrap();
        assert_eq!(
            destination(&folder, Path::new("orders.csv")).unwrap(),
            folder.join("orders (1).csv")
        );
        assert_eq!(
            destination(&folder, Path::new("README")).unwrap(),
            folder.join("README")
        );
        for name in [".profile", "..", "a\nb.csv"] {
            assert!(destination(&folder, Path::new(name)).is_none(), "{name}");
        }
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn overlapping_exports_match_the_reported_file_and_keep_both_fallback_copies() {
        let folder = tempfile::tempdir().unwrap();
        let first = Staged::new(Path::new("orders.csv")).unwrap();
        let second = Staged::new(Path::new("orders.csv")).unwrap();
        std::fs::write(&first.path, "first").unwrap();
        std::fs::write(&second.path, "second").unwrap();
        let pending = vec![("same".into(), first), ("same".into(), second)];
        assert_eq!(
            completed_index(&pending, "same", Some(&pending[1].1.path)),
            Some(1)
        );
        assert_eq!(completed_index(&pending, "same", None), Some(0));
        let paths = std::thread::scope(|scope| {
            let save = |i: usize| keep_in_folder(&pending[i].1.path, folder.path()).unwrap();
            let first = scope.spawn(move || save(0));
            let second = save(1);
            (first.join().unwrap(), second)
        });
        assert_ne!(paths.0, paths.1);
        assert_eq!(std::fs::read_to_string(paths.0).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(paths.1).unwrap(), "second");
    }

    #[test]
    fn finished_downloads_move_to_the_chosen_file() {
        let root = std::env::temp_dir().join(format!("arcnix-move-{}", arcnix_core::new_id()));
        let staged = root.join("staging/orders.csv");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::create_dir_all(root.join("chosen")).unwrap();
        std::fs::write(&staged, "id\n1\n").unwrap();
        let chosen = root.join("chosen/march.csv");
        std::fs::write(&chosen, "replaced").unwrap();
        move_file(&staged, &chosen).unwrap();
        assert!(!staged.exists());
        assert_eq!(std::fs::read_to_string(&chosen).unwrap(), "id\n1\n");
        assert_eq!(file_name(&chosen), "march.csv");
        std::fs::remove_dir_all(root).unwrap();
    }
}
