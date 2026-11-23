use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Theme;
use crate::browser::Bookmark;
use crate::desktop_customization::{
    Appearance, Dock, Layout, Shortcut, Wallpaper, known_apps, known_shortcuts,
};
use crate::desktop_shortcuts::Shortcuts;
use crate::desktop_theme::{Overrides, Preset, Themes, appearance_value, built_in_wallpaper};
use crate::server_apps::{
    DesktopApp as App, DesktopClock, DesktopPreferences, DesktopWindow, EditorSettings,
    FilesPreferences, PlayerSettings, TerminalAppearance, TerminalLayout,
};

pub(crate) const HEADER: &[u8] = b"arcnix-desktop-v1\n";

#[derive(Clone, Copy)]
enum Domain {
    System,
    Shell,
    Dock,
    Files,
    Browser,
    Terminal,
    Player,
    Editor,
    Monitor,
    App(&'static str, App),
}

const DOMAINS: &[Domain] = &[
    Domain::System,
    Domain::Shell,
    Domain::Dock,
    Domain::Files,
    Domain::Browser,
    Domain::Terminal,
    Domain::Monitor,
    Domain::App("apps/app_center.json", App::AppCenter),
    Domain::App("apps/code.json", App::Code),
    Domain::App("apps/docker.json", App::Docker),
    Domain::Player,
    Domain::App("apps/settings.json", App::Settings),
    Domain::App("apps/disks.json", App::Disks),
    Domain::App("apps/services.json", App::Services),
    Domain::App("apps/logs.json", App::Logs),
    Domain::App("apps/trash.json", App::Trash),
    Domain::Editor,
];

#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct System {
    version: u8,
    theme: Theme,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<crate::Language>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_zone: Option<String>,
    #[serde(skip_serializing_if = "DesktopClock::is_default")]
    clock: DesktopClock,
    shortcuts: Shortcuts,
}

impl Default for System {
    fn default() -> Self {
        Self {
            version: 1,
            theme: Theme::System,
            language: None,
            time_zone: None,
            clock: DesktopClock::default(),
            shortcuts: Shortcuts::default(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Shell {
    show_server: bool,
    show_apps: bool,
    #[serde(deserialize_with = "known_shortcuts")]
    order: Vec<Shortcut>,
    #[serde(deserialize_with = "known_apps")]
    hidden: Vec<App>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "appearance_value")]
    theme: Option<Preset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "built_in_wallpaper")]
    wallpaper: Option<Wallpaper>,
    #[serde(skip_serializing_if = "Overrides::is_default")]
    overrides: Box<Overrides>,
    #[serde(skip_serializing_if = "Themes::is_default")]
    themes: Box<Themes>,
    #[serde(skip_serializing_if = "Layout::is_default")]
    layout: Layout,
}

impl From<&Appearance> for Shell {
    fn from(value: &Appearance) -> Self {
        Self {
            show_server: value.show_server,
            show_apps: value.show_apps,
            order: value.order.clone(),
            hidden: value.hidden.clone(),
            theme: value.theme,
            wallpaper: value.wallpaper.clone(),
            overrides: value.overrides.clone(),
            themes: value.themes.clone(),
            layout: value.layout,
        }
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::from(&Appearance::default())
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Application<T: Default> {
    #[serde(skip_serializing_if = "Option::is_none")]
    window: Option<DesktopWindow>,
    settings: T,
}

// Keep the existing terminal layout under "settings" when reading older snapshots.
#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TerminalFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    window: Option<DesktopWindow>,
    settings: TerminalLayout,
    appearance: TerminalAppearance,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Monitor {
    resource_interval_seconds: Option<u64>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Browser {
    bookmarks: Vec<Bookmark>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WindowOnly {
    #[serde(skip_serializing_if = "Option::is_none")]
    window: Option<DesktopWindow>,
}

fn application<T: Default>(prefs: &DesktopPreferences, app: App, settings: T) -> Application<T> {
    Application {
        window: prefs.windows.get(&app).copied(),
        settings,
    }
}

fn read_application<T: Default + DeserializeOwned>(
    prefs: &mut DesktopPreferences,
    app: App,
    bytes: &[u8],
) -> Result<T> {
    let Application { window, settings } = read_json(bytes)?;
    prefs.windows.extend(window.map(|window| (app, window)));
    Ok(settings)
}

fn read_json<T: Default + DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.is_empty() {
        Ok(T::default())
    } else {
        Ok(serde_json::from_slice(bytes)?)
    }
}

impl Domain {
    fn path(self) -> &'static str {
        match self {
            Self::System => "system.json",
            Self::Shell => "preferences/desktop.json",
            Self::Dock => "apps/dock.json",
            Self::Files => "apps/files.json",
            Self::Browser => "apps/web_browser.json",
            Self::Terminal => "apps/terminal.json",
            Self::Monitor => "apps/monitor.json",
            Self::Editor => "apps/text_editor.json",
            Self::Player => "apps/player.json",
            Self::App(path, _) => path,
        }
    }

    fn encode(self, prefs: &DesktopPreferences) -> Result<Vec<u8>> {
        Ok(match self {
            Self::System => serde_json::to_vec_pretty(&System {
                version: prefs.version,
                theme: prefs.theme,
                language: prefs.language,
                time_zone: prefs.time_zone.clone(),
                clock: prefs.clock,
                shortcuts: prefs.shortcuts.clone(),
            })?,
            Self::Shell => serde_json::to_vec_pretty(&Shell::from(&prefs.appearance))?,
            Self::Dock => serde_json::to_vec_pretty(&prefs.appearance.dock)?,
            Self::Files => {
                serde_json::to_vec_pretty(&application(prefs, App::Files, prefs.files.clone()))?
            }
            Self::Browser => serde_json::to_vec_pretty(&application(
                prefs,
                App::Browser,
                Browser {
                    bookmarks: prefs.browser_bookmarks.clone(),
                },
            ))?,
            Self::Terminal => serde_json::to_vec_pretty(&TerminalFile {
                window: prefs.windows.get(&App::Terminal).copied(),
                settings: prefs.terminal.clone(),
                appearance: prefs.app_settings.terminal,
            })?,
            Self::Editor => serde_json::to_vec_pretty(&application(
                prefs,
                App::Editor,
                prefs.app_settings.editor,
            ))?,
            Self::Player => serde_json::to_vec_pretty(&application(
                prefs,
                App::Player,
                prefs.app_settings.player,
            ))?,
            Self::Monitor => serde_json::to_vec_pretty(&application(
                prefs,
                App::Monitor,
                Monitor {
                    resource_interval_seconds: prefs.resource_interval_seconds,
                },
            ))?,
            Self::App(_, app) => serde_json::to_vec_pretty(&WindowOnly {
                window: prefs.windows.get(&app).copied(),
            })?,
        })
    }

    fn decode(self, prefs: &mut DesktopPreferences, bytes: &[u8]) -> Result<()> {
        match self {
            Self::System => {
                let System {
                    version,
                    theme,
                    language,
                    time_zone,
                    clock,
                    shortcuts,
                } = read_json(bytes)?;
                prefs.version = version;
                prefs.theme = theme;
                prefs.language = language;
                prefs.time_zone = time_zone;
                prefs.clock = clock;
                prefs.shortcuts = shortcuts;
            }
            Self::Shell => {
                let Shell {
                    show_server,
                    show_apps,
                    order,
                    hidden,
                    theme,
                    wallpaper,
                    overrides,
                    themes,
                    layout,
                } = read_json(bytes)?;
                prefs.appearance.show_server = show_server;
                prefs.appearance.show_apps = show_apps;
                prefs.appearance.order = order;
                prefs.appearance.hidden = hidden;
                prefs.appearance.theme = theme;
                prefs.appearance.wallpaper = wallpaper;
                prefs.appearance.overrides = overrides;
                prefs.appearance.themes = themes;
                prefs.appearance.layout = layout;
            }
            Self::Dock => prefs.appearance.dock = read_json::<Dock>(bytes)?,
            Self::Files => {
                prefs.files = read_application::<FilesPreferences>(prefs, App::Files, bytes)?
            }
            Self::Browser => {
                prefs.browser_bookmarks =
                    read_application::<Browser>(prefs, App::Browser, bytes)?.bookmarks
            }
            Self::Terminal => {
                let file: TerminalFile = read_json(bytes)?;
                prefs
                    .windows
                    .extend(file.window.map(|w| (App::Terminal, w)));
                prefs.terminal = file.settings;
                prefs.app_settings.terminal = file.appearance;
            }
            Self::Editor => {
                prefs.app_settings.editor =
                    read_application::<EditorSettings>(prefs, App::Editor, bytes)?
            }
            Self::Player => {
                prefs.app_settings.player =
                    read_application::<PlayerSettings>(prefs, App::Player, bytes)?
            }
            Self::Monitor => {
                prefs.resource_interval_seconds =
                    read_application::<Monitor>(prefs, App::Monitor, bytes)?
                        .resource_interval_seconds
            }
            Self::App(_, app) => {
                let WindowOnly { window } = read_json(bytes)?;
                prefs.windows.extend(window.map(|window| (app, window)));
            }
        }
        Ok(())
    }
}

pub fn encode(prefs: &DesktopPreferences) -> Result<Vec<u8>> {
    prefs.validate()?;
    let mut packet = HEADER.to_vec();
    for &domain in DOMAINS {
        let data = domain.encode(prefs)?;
        let size = data.len() + 1;
        packet.extend_from_slice(format!("{size}\n").as_bytes());
        packet.extend_from_slice(&data);
        packet.push(b'\n');
    }
    Ok(packet)
}

/// The packet with one file's contents replaced.
#[cfg(test)]
pub(crate) fn replace_for_test(packet: &[u8], path: &str, contents: &[u8]) -> Result<Vec<u8>> {
    let mut replaced = HEADER.to_vec();
    for record in records(packet)? {
        let data = if record.path == path {
            contents
        } else {
            record.contents
        };
        replaced.extend_from_slice(format!("{}\n", data.len()).as_bytes());
        replaced.extend_from_slice(data);
    }
    Ok(replaced)
}

struct Record<'a> {
    path: &'static str,
    length_offset: usize,
    contents_offset: usize,
    contents: &'a [u8],
}

fn records(packet: &[u8]) -> Result<Vec<Record<'_>>> {
    let mut rest = packet
        .strip_prefix(HEADER)
        .context("Invalid desktop configuration header")?;
    let mut files = Vec::with_capacity(DOMAINS.len());
    for &domain in DOMAINS {
        let newline = rest
            .iter()
            .position(|byte| *byte == b'\n')
            .context("Missing desktop file length")?;
        let (size, data) = (&rest[..newline], &rest[newline + 1..]);
        let size: usize = std::str::from_utf8(size)?.parse()?;
        ensure!(size <= data.len(), "Truncated desktop configuration");
        let (contents, remaining) = data.split_at(size);
        files.push(Record {
            path: domain.path(),
            length_offset: packet.len() - rest.len(),
            contents_offset: packet.len() - data.len(),
            contents,
        });
        rest = remaining;
    }
    ensure!(
        rest.is_empty(),
        "Unexpected data after desktop configuration"
    );
    Ok(files)
}

pub(crate) fn decode(packet: &[u8]) -> Result<DesktopPreferences> {
    let mut prefs = DesktopPreferences::default();
    for (&domain, record) in DOMAINS.iter().zip(records(packet)?) {
        domain
            .decode(&mut prefs, record.contents)
            .with_context(|| format!("Invalid {}", record.path))?;
    }
    prefs.validate()?;
    Ok(prefs)
}

pub(crate) struct FileUpdate {
    path: &'static str,
    expected_snapshot: String,
    pub contents: Vec<u8>,
}

pub(crate) fn single_file_update(
    packet: &[u8],
    remote: &DesktopPreferences,
    merged: &DesktopPreferences,
    limit: usize,
) -> Result<Option<FileUpdate>> {
    merged.validate()?;
    let mut changed = None;
    for (&domain, record) in DOMAINS.iter().zip(records(packet)?) {
        let contents = domain.encode(merged)?;
        // Compare canonical values so differences in formatting and omitted defaults
        // do not turn an unrelated file into part of this update.
        if contents != domain.encode(remote)? {
            if changed.is_some() {
                return Ok(None);
            }
            changed = Some((record, contents));
        }
    }
    let Some((record, mut contents)) = changed else {
        return Ok(None);
    };
    contents.push(b'\n');
    let prefix = &packet[..record.length_offset];
    let length = format!("{}\n", contents.len());
    let suffix = &packet[record.contents_offset + record.contents.len()..];
    ensure!(
        prefix.len() + length.len() + contents.len() + suffix.len() <= limit,
        "Invalid sync document size"
    );
    let mut digest = Sha256::new();
    digest.update(prefix);
    digest.update(length.as_bytes());
    digest.update(&contents);
    digest.update(suffix);
    Ok(Some(FileUpdate {
        path: record.path,
        expected_snapshot: format!("{:x}", digest.finalize()),
        contents,
    }))
}

fn script(limit: usize, operation: &str) -> String {
    let paths = DOMAINS
        .iter()
        .map(|domain| domain.path())
        .collect::<Vec<_>>()
        .join(" ");
    let mut script =
        format!("# arcnix:desktop:domains:{operation}\nlimit={limit}\ndomains='{paths}'\n");
    script.push_str(include_str!("desktop_storage.sh"));
    script
}

pub(crate) fn file_command(limit: usize, base: &str, update: &FileUpdate) -> String {
    let mut script = script(limit, "file");
    let digest = Sha256::digest(&update.contents);
    let FileUpdate {
        path,
        expected_snapshot,
        ..
    } = update;
    script.push_str(&format!(
        "\nprepare_file '{digest:x}'\npublish_file '{base}' '{expected_snapshot}' '{path}'\n"
    ));
    script
}

pub(crate) fn command(limit: usize, commit: Option<(&str, &str, &[u8])>) -> Result<String> {
    use std::fmt::Write;

    let operation = if commit.is_some() { "commit" } else { "read" };
    let mut script = script(limit, operation);
    if let Some((base, next, packet)) = commit {
        writeln!(script, "\nprepare_commit '{next}'")?;
        for record in records(packet)? {
            ensure!(
                !record.contents.is_empty(),
                "Cannot publish an empty desktop file"
            );
            writeln!(
                script,
                "stage_file '{}' {} {}",
                record.path,
                record.contents_offset,
                record.contents.len()
            )?;
        }
        writeln!(script, "publish_commit '{base}' '{next}'")?;
    } else {
        script.push_str("\nread_desktop\n");
    }
    Ok(script)
}
