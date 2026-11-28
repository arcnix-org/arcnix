use std::result::Result;

use arcnix_core::desktop_customization::Wallpaper;
use arcnix_core::wallpaper_cache::{Cached, Request};

use crate::server_apps_worker::{Action as AppAction, Reply};
use crate::*;

pub(crate) const EMPTY_IMAGE: &str =
    "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

#[derive(Clone, Default)]
pub(crate) struct Runtime {
    pub pictures: HashMap<String, Cached>,
    pub selecting: Option<String>,
    request: Option<(u64, String, bool)>,
    removal: Option<(u64, String)>,
    failed: HashSet<String>,
    pub error: String,
}

pub(crate) fn cache_root(ui: &Ui) -> PathBuf {
    ui.store.path().with_file_name("wallpaper-cache")
}

pub(crate) fn busy(s: &State) -> bool {
    s.server_apps.wallpapers.request.is_some() || s.server_apps.wallpapers.removal.is_some()
}

pub(crate) fn status(s: &State) -> &'static str {
    if s.server_apps.wallpapers.removal.is_some() {
        "Removing wallpaper…"
    } else if s.server_apps.wallpapers.request.is_some() {
        "Loading wallpaper…"
    } else {
        ""
    }
}

pub(crate) fn remove(ui: &Ui, path: &str) {
    let Some((scope, active, mode)) = ui.state.with(|s| {
        if !server_apps_ui::preferences_ready(s)
            || busy(s)
            || !server_apps_ui::current(s)
                .appearance
                .wallpapers
                .iter()
                .any(|p| p == path)
        {
            return None;
        }
        Some((
            s.sync_scope.clone()?,
            desktop_appearance_ui::background(s) == Wallpaper::Remote(path.into()),
            server_apps_ui::current(s).theme,
        ))
    }) else {
        return;
    };
    desktop_customization_ui::update(ui, |prefs| {
        prefs.wallpapers.retain(|picture| picture != path);
        if active {
            prefs.edit_theme_settings(mode);
            prefs.wallpaper = None;
            prefs.overrides.wallpaper = None;
            prefs.overrides.light.wallpaper = None;
            prefs.overrides.dark.wallpaper = None;
        }
    });
    if ui.state.with(|s| {
        server_apps_ui::current(s)
            .appearance
            .wallpapers
            .iter()
            .any(|p| p == path)
    }) {
        return;
    }
    let serial = ui.send(Action::ServerApps(AppAction::RemoveWallpaperCache(
        Request {
            root: cache_root(ui),
            scope,
            path: path.into(),
        },
    )));
    ui.state.revise(|s| {
        let runtime = &mut s.server_apps.wallpapers;
        runtime.selecting = None;
        runtime.pictures.remove(path);
        runtime.failed.remove(path);
        runtime.error.clear();
        runtime.removal = Some((serial, path.into()));
    });
}

pub(crate) fn browse(ui: &Ui) {
    desktop_file_picker::open(ui, desktop_file_picker::Purpose::Wallpaper, None);
}

pub(crate) fn select(ui: &Ui, path: &str) {
    if !ui
        .state
        .with(|s| server_apps_ui::preferences_ready(s) && s.sftp_available && !busy(s))
    {
        return;
    }
    if ui.state.with(|s| {
        let catalog = &server_apps_ui::current(s).appearance.wallpapers;
        catalog.len() >= 32 && !catalog.iter().any(|p| p == path)
    }) {
        ui.state
            .revise(|s| s.server_apps.wallpapers.error = "You can add up to 32 wallpapers.".into());
        return;
    }
    ui.state
        .revise(|s| s.server_apps.wallpapers.selecting = Some(path.to_owned()));
    request(ui, path, true);
}

fn request(ui: &Ui, path: &str, add: bool) {
    let Some(scope) = ui.state.with(|s| s.sync_scope.clone()) else {
        return;
    };
    let serial = ui.send(Action::ServerApps(AppAction::ReadWallpaper(Request {
        root: cache_root(ui),
        scope,
        path: path.to_owned(),
    })));
    ui.state.revise(|s| {
        s.server_apps.wallpapers.request = Some((serial, path.to_owned(), add));
        s.server_apps.wallpapers.error.clear();
    });
}

pub(crate) fn tick(ui: &Ui) {
    let next = ui.state.with(|s| {
        if !server_apps_ui::preferences_ready(s) || !s.sftp_available || busy(s) {
            return None;
        }
        let mut paths = server_apps_ui::current(s).appearance.wallpapers.clone();
        if let Wallpaper::Remote(path) = desktop_appearance_ui::background(s) {
            paths.insert(0, path);
        }
        paths.into_iter().find(|path| {
            !s.server_apps.wallpapers.pictures.contains_key(path)
                && !s.server_apps.wallpapers.failed.contains(path)
        })
    });
    if let Some(path) = next {
        request(ui, &path, false);
    }
}

pub(crate) fn pending(ui: &Ui, serial: u64) -> bool {
    ui.state.with(|s| {
        s.server_apps
            .wallpapers
            .removal
            .as_ref()
            .is_some_and(|(id, _)| *id == serial)
            || s.server_apps
                .wallpapers
                .request
                .as_ref()
                .is_some_and(|(id, ..)| *id == serial)
    })
}

pub(crate) fn received(ui: &Ui, result: Result<Reply, String>) {
    if ui
        .state
        .with(|s| s.server_apps.wallpapers.removal.is_some())
    {
        ui.state.revise(|s| {
            let runtime = &mut s.server_apps.wallpapers;
            runtime.removal = None;
            runtime.error = match result {
                Ok(Reply::WallpaperRemoved) => String::new(),
                Err(error) => error,
                _ => "Could not remove cached wallpaper.".into(),
            };
        });
        return;
    }
    let select = ui.state.revise(|s| {
        let runtime = &mut s.server_apps.wallpapers;
        let (_, path, add) = runtime.request.take()?;
        match result {
            Ok(Reply::Wallpaper(picture)) if picture.path == path => {
                runtime.pictures.insert(path.clone(), picture);
                runtime.failed.remove(&path);
                let selected = runtime.selecting.take().as_ref() == Some(&path);
                add.then_some((path, selected))
            }
            result => {
                runtime.failed.insert(path);
                runtime.selecting = None;
                runtime.error = match result {
                    Err(error) => error,
                    _ => "Could not load this wallpaper.".into(),
                };
                None
            }
        }
    });
    if let Some((path, selected)) = select {
        desktop_customization_ui::update(ui, |prefs| {
            if !prefs.wallpapers.contains(&path) {
                prefs.wallpapers.push(path.clone());
            }
        });
        if selected {
            desktop_appearance_ui::set_background(ui, Some(Wallpaper::Remote(path)));
        }
    }
}

pub(crate) fn selected_image(s: &State) -> Option<String> {
    let Wallpaper::Remote(path) = desktop_appearance_ui::background(s) else {
        return None;
    };
    s.server_apps
        .wallpapers
        .pictures
        .get(&path)
        .map(|picture| picture.image.clone())
}

struct Asset {
    mime: &'static str,
    bytes: Vec<u8>,
}

impl glory::wry::shell::AssetSource for Asset {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn mime(&self) -> &str {
        self.mime
    }
    fn current(&self) -> bool {
        true
    }
    fn read_range(&self, offset: u64, length: usize) -> Result<Vec<u8>, String> {
        let start = usize::try_from(offset).map_err(|_| "Invalid wallpaper range")?;
        let end = start.checked_add(length).ok_or("Invalid wallpaper range")?;
        self.bytes
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| "Invalid wallpaper range".into())
    }
}

pub(crate) fn asset(root: &Path, path: &str) -> Option<Arc<dyn glory::wry::shell::AssetSource>> {
    let (mime, bytes) = arcnix_core::wallpaper_cache::asset(root, path)?;
    Some(Arc::new(Asset { mime, bytes }))
}
