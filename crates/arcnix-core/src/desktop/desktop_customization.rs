use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize};

use crate::FileEntry;
use crate::desktop_theme::{
    CustomTheme, Overrides, Preset, Themes, appearance_value, built_in_wallpaper,
};
use crate::remote::RemoteSession;
use crate::server_apps::DesktopApp;

const MAX_FILES: usize = 128;
pub const MIN_ICON_SIZE: u8 = 32;
pub const MEDIUM_ICON_SIZE: u8 = 48;
pub const MAX_ICON_SIZE: u8 = 64;
pub const ICON_SIZE_RANGE: std::ops::RangeInclusive<u8> = MIN_ICON_SIZE..=MAX_ICON_SIZE;
pub const MAX_ICONS: usize = MAX_FILES + DesktopApp::COUNT + 2;
pub(crate) const DESKTOP_NAME_LIMIT: usize = 255;
pub(crate) const WALLPAPER_PATH_LIMIT: usize = 1536;
const MAX_WALLPAPER: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Shortcut {
    App(DesktopApp),
    File(String),
    System(SystemIcon),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemIcon {
    Home,
    Trash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum DockItem {
    App(DesktopApp),
    System { system: SystemIcon },
}

impl From<DockItem> for Shortcut {
    fn from(item: DockItem) -> Self {
        match item {
            DockItem::App(app) => Self::App(app),
            DockItem::System { system } => Self::System(system),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wallpaper {
    #[serde(alias = "terracotta", alias = "arcnix", alias = "afterglow")]
    Seaside,
    Aurora,
    #[default]
    #[serde(alias = "dunes")]
    Fjord,
    Remote(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconSize {
    Small,
    #[default]
    Medium,
    Large,
}

pub use IconSize as DockSize;
pub use IconSize as DesktopIconSize;

impl IconSize {
    pub const fn pixels(self) -> u8 {
        match self {
            Self::Small => MIN_ICON_SIZE,
            Self::Medium => MEDIUM_ICON_SIZE,
            Self::Large => MAX_ICON_SIZE,
        }
    }

    fn from_pixels(pixels: u8) -> Option<Self> {
        [Self::Small, Self::Medium, Self::Large]
            .into_iter()
            .find(|size| size.pixels() == pixels)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockPosition {
    Left,
    Right,
    #[default]
    Bottom,
}

impl DockPosition {
    fn is_bottom(&self) -> bool {
        *self == Self::Bottom
    }
}

fn discard_magnification<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<(), D::Error> {
    bool::deserialize(deserializer).map(|_| ())
}

/// Read a list that names desktop apps, skipping apps this version does not
/// know. Settings saved by a newer Arcnix then still load here.
pub(crate) fn known_apps<'de, D, T>(deserializer: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    read_app_list(deserializer, unknown_app_name)
}

pub(crate) fn known_shortcuts<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Shortcut>, D::Error> {
    read_app_list(deserializer, |value| {
        value.get("kind").and_then(serde_json::Value::as_str) == Some("app")
            && value.get("id").is_some_and(unknown_app_name)
    })
}

fn unknown_app_name(value: &serde_json::Value) -> bool {
    value.is_string() && DesktopApp::deserialize(value).is_err()
}

fn read_app_list<'de, D, T>(
    deserializer: D,
    skip: impl Fn(&serde_json::Value) -> bool,
) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    Vec::<serde_json::Value>::deserialize(deserializer)?
        .into_iter()
        .filter(|value| !skip(value))
        .map(|value| serde_json::from_value(value).map_err(serde::de::Error::custom))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(
    clippy::manual_non_exhaustive,
    reason = "The unit field accepts legacy serialized dock settings"
)]
pub struct Dock {
    #[serde(deserialize_with = "known_apps")]
    pub order: Vec<DockItem>,
    #[serde(deserialize_with = "known_apps")]
    pub unpinned: Vec<DesktopApp>,
    /// Explicit pins for applications outside the default set. Legacy order is only a layout hint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(deserialize_with = "known_apps")]
    pub extra_pins: Vec<DesktopApp>,
    #[serde(deserialize_with = "appearance_value")]
    pub size: DockSize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "dock_icon_size")]
    pub icon_size: Option<u8>,
    #[serde(skip_serializing_if = "DockPosition::is_bottom")]
    #[serde(deserialize_with = "appearance_value")]
    pub position: DockPosition,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub panel_mode: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub panel_centered: bool,
    pub floating_status: bool,
    #[serde(
        rename = "magnify",
        skip_serializing,
        deserialize_with = "discard_magnification"
    )]
    legacy_magnify: (),
    pub auto_hide: bool,
}

impl Default for Dock {
    fn default() -> Self {
        Self {
            order: vec![],
            unpinned: vec![],
            extra_pins: vec![],
            size: DockSize::Medium,
            icon_size: None,
            position: DockPosition::Bottom,
            panel_mode: false,
            panel_centered: false,
            floating_status: true,
            legacy_magnify: (),
            auto_hide: false,
        }
    }
}

impl Dock {
    pub const MIN_ICON_SIZE: u8 = MIN_ICON_SIZE;
    pub const MAX_ICON_SIZE: u8 = MAX_ICON_SIZE;

    pub fn icon_pixels(&self) -> u8 {
        self.icon_size
            .unwrap_or(self.size.pixels())
            .clamp(Self::MIN_ICON_SIZE, Self::MAX_ICON_SIZE)
    }

    pub fn set_icon_pixels(&mut self, pixels: u8) {
        let pixels = pixels.clamp(Self::MIN_ICON_SIZE, Self::MAX_ICON_SIZE);
        if let Some(preset) = IconSize::from_pixels(pixels) {
            self.size = preset;
            self.icon_size = None;
        } else {
            self.icon_size = Some(pixels);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconFlow {
    #[default]
    Columns,
    Rows,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconDirection {
    #[default]
    LeftToRight,
    RightToLeft,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    #[serde(deserialize_with = "appearance_value")]
    pub flow: IconFlow,
    #[serde(deserialize_with = "appearance_value")]
    pub direction: IconDirection,
    #[serde(deserialize_with = "appearance_value")]
    pub size: DesktopIconSize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "desktop_icon_size")]
    pub icon_size: Option<u8>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub show_home: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub show_trash: bool,
}

fn dock_icon_size<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u8>, D::Error> {
    let size: Option<u8> = appearance_value(deserializer)?;
    Ok(size.filter(|size| (24..=Dock::MAX_ICON_SIZE).contains(size)))
}

fn desktop_icon_size<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u8>, D::Error> {
    let size: Option<u8> = appearance_value(deserializer)?;
    Ok(size.filter(|size| (Layout::MIN_ICON_SIZE..=88).contains(size)))
}

impl Layout {
    pub const MIN_ICON_SIZE: u8 = MIN_ICON_SIZE;
    pub const MAX_ICON_SIZE: u8 = MAX_ICON_SIZE;

    pub fn icon_pixels(&self) -> u8 {
        self.icon_size
            .unwrap_or(self.size.pixels())
            .clamp(Self::MIN_ICON_SIZE, Self::MAX_ICON_SIZE)
    }

    pub fn set_icon_pixels(&mut self, pixels: u8) {
        let pixels = pixels.clamp(Self::MIN_ICON_SIZE, Self::MAX_ICON_SIZE);
        if let Some(preset) = IconSize::from_pixels(pixels) {
            self.size = preset;
            self.icon_size = None;
        } else {
            self.icon_size = Some(pixels);
        }
    }

    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub show_server: bool,
    pub show_apps: bool,
    #[serde(deserialize_with = "known_shortcuts")]
    pub order: Vec<Shortcut>,
    #[serde(deserialize_with = "known_apps")]
    pub hidden: Vec<DesktopApp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "appearance_value")]
    pub theme: Option<Preset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "built_in_wallpaper")]
    pub wallpaper: Option<Wallpaper>,
    #[serde(skip_serializing_if = "Overrides::is_default")]
    pub overrides: Box<Overrides>,
    #[serde(skip_serializing_if = "Themes::is_default")]
    pub themes: Box<Themes>,
    #[serde(skip_serializing_if = "Layout::is_default")]
    pub layout: Layout,
    pub dock: Dock,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            show_server: true,
            show_apps: true,
            order: vec![],
            hidden: vec![],
            theme: None,
            wallpaper: None,
            overrides: Box::default(),
            themes: Box::default(),
            layout: Layout::default(),
            dock: Dock::default(),
        }
    }
}

impl Appearance {
    pub fn apply_theme_preset(&mut self, preset: Preset) {
        self.theme = Some(preset);
        self.wallpaper = None;
        *self.overrides = Overrides::default();
    }

    pub fn theme_settings(&self, legacy_mode: crate::Theme) -> CustomTheme {
        if self.theme.is_none() && self.overrides.is_default() {
            return self.themes.editable(self.wallpaper.as_ref(), legacy_mode);
        }
        let base = self.theme.unwrap_or_default();
        let mut overrides = self.overrides.as_ref().clone();
        if let Some(wallpaper) = &self.wallpaper {
            overrides.wallpaper = Some(wallpaper.clone());
            overrides.light.wallpaper = None;
            overrides.dark.wallpaper = None;
        }
        CustomTheme { base, overrides }
    }

    pub fn edit_theme_settings(&mut self, legacy_mode: crate::Theme) {
        if self.theme.is_some() {
            return;
        }
        let CustomTheme {
            base,
            mut overrides,
        } = self.theme_settings(legacy_mode);
        self.theme = Some(base);
        self.wallpaper =
            if overrides.light.wallpaper.is_none() && overrides.dark.wallpaper.is_none() {
                overrides.wallpaper.take()
            } else {
                None
            };
        *self.overrides = overrides;
    }

    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.order.len() <= MAX_ICONS && unique(&self.order),
            "Invalid desktop icon order"
        );
        for icon in &self.order {
            if let Shortcut::File(name) = icon {
                ensure!(valid_name(name), "Invalid desktop filename");
            }
        }
        for apps in [&self.hidden, &self.dock.unpinned, &self.dock.extra_pins] {
            ensure!(
                apps.len() <= DesktopApp::COUNT && unique(apps),
                "Invalid application list"
            );
        }
        ensure!(
            self.dock.order.len() <= DesktopApp::COUNT + 2 && unique(&self.dock.order),
            "Invalid dock shortcut order"
        );
        // Continue accepting sizes saved by earlier clients. Display and edits use the current
        // range.
        ensure!(
            self.dock
                .icon_size
                .is_none_or(|size| (24..=Dock::MAX_ICON_SIZE).contains(&size)),
            "Invalid dock icon size"
        );
        ensure!(
            self.layout
                .icon_size
                .is_none_or(|size| (Layout::MIN_ICON_SIZE..=88).contains(&size)),
            "Invalid desktop icon size"
        );
        if let Some(Wallpaper::Remote(path)) = &self.wallpaper {
            validate_image_path(path)?;
        }
        self.overrides.validate()?;
        self.themes.validate()?;
        Ok(())
    }
}

fn unique<T: Eq + std::hash::Hash>(values: &[T]) -> bool {
    values.iter().collect::<HashSet<_>>().len() == values.len()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= DESKTOP_NAME_LIMIT
        && !matches!(name, "." | "..")
        && !name.contains('/')
        && !name.chars().any(char::is_control)
}

#[derive(Clone, Debug, Default)]
pub struct DesktopItems {
    pub home: String,
    pub path: String,
    pub exists: bool,
    pub entries: Vec<FileEntry>,
}

pub fn items(session: &RemoteSession) -> Result<DesktopItems> {
    let home = session.login_home()?;
    let path = format!("{}/Desktop", home.trim_end_matches('/'));
    if !session.directory_exists(&path)? {
        return Ok(DesktopItems {
            home,
            path,
            ..Default::default()
        });
    }
    let (path, mut entries) = session.list_dir(&path)?;
    entries.retain(|entry| valid_name(&entry.name) && !entry.is_hidden());
    entries.sort_by(|a, b| (!a.is_dir, &a.name).cmp(&(!b.is_dir, &b.name)));
    entries.truncate(MAX_FILES);
    Ok(DesktopItems {
        home,
        path,
        exists: true,
        entries,
    })
}

const FORMATS: [(&str, &str); 8] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
    ("bmp", "image/bmp"),
    ("avif", "image/avif"),
    ("svg", "image/svg+xml"),
];

pub fn image_extensions() -> impl Iterator<Item = &'static str> {
    FORMATS.iter().map(|&(ext, _)| ext)
}

fn format(path: &Path) -> Result<(&'static str, &'static str)> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    FORMATS
        .into_iter()
        .find(|(extension, _)| *extension == ext)
        .context("Supported image formats: PNG, JPEG, WebP, GIF, BMP, AVIF and SVG")
}

pub(crate) fn validate_image_path(path: &str) -> Result<()> {
    ensure!(
        path.starts_with('/')
            && path.len() <= WALLPAPER_PATH_LIMIT
            && !path.chars().any(char::is_control),
        "Invalid server wallpaper path"
    );
    format(Path::new(path))?;
    Ok(())
}

fn validate_image(extension: &str, bytes: &[u8]) -> Result<()> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_WALLPAPER,
        "Images must be nonempty and at most 8 MiB"
    );
    let valid = match extension {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" | "jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "bmp" => bytes.starts_with(b"BM"),
        "avif" => {
            bytes.get(4..8) == Some(b"ftyp")
                && bytes.get(8..32.min(bytes.len())).is_some_and(|brands| {
                    brands
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|b| matches!(b, b"avif" | b"avis"))
                })
        }
        "svg" => std::str::from_utf8(bytes).is_ok_and(|contents| {
            contents.contains("<svg")
                && !contents.contains("<!ENTITY")
                && !contents.contains("<!DOCTYPE")
        }),
        _ => false,
    };
    ensure!(valid, "Image content does not match its file format");
    Ok(())
}

#[derive(Debug)]
pub struct WallpaperImage {
    pub path: String,
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

pub fn read_wallpaper(session: &RemoteSession, path: &str) -> Result<WallpaperImage> {
    let path = if let Some(relative) = path.strip_prefix("~/") {
        format!("{}/{relative}", session.login_home()?.trim_end_matches('/'))
    } else {
        path.to_owned()
    };
    read_image(&path, |limit| session.read_bounded_file(&path, limit))
}

pub fn read_image(
    path: &str,
    read: impl FnOnce(usize) -> Result<Vec<u8>>,
) -> Result<WallpaperImage> {
    validate_image_path(path)?;
    let (extension, mime) = format(Path::new(path))?;
    let bytes = read(MAX_WALLPAPER)?;
    validate_image(extension, &bytes)?;
    Ok(WallpaperImage {
        path: path.into(),
        mime,
        bytes,
    })
}
