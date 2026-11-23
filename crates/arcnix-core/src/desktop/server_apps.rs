//! Finite software discovery and remote-user desktop preferences over SSH.
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::desktop_shortcuts::Shortcuts;
use crate::remote::{CommandStream, RemoteSession};
use crate::{CommandOutput, Theme};

pub const TERMINAL_TAB_LIMIT: usize = 32;
const TERMINAL_TAB_NAME_LIMIT: usize = 128;

// Bound untrusted transport input using the existing field limits. JSON can
// encode an ASCII byte as six bytes (\u00XX); the remaining space covers keys,
// numeric fields, and formatting. Saving uses the typed schema's limits.
pub(crate) const DESKTOP_JSON_READ_LIMIT: usize = 6
    * (FilesPreferences::PATHS_LIMIT
        + FilesPreferences::MAX_FOLDER_ICONS * FilesPreferences::ICON_ID_LIMIT
        + TERMINAL_TAB_LIMIT
            * (crate::terminal::DIRECTORY_LIMIT
                + crate::terminal::SESSION_ID_LIMIT
                + TERMINAL_TAB_NAME_LIMIT)
        + crate::terminal::SESSION_ID_LIMIT
        + crate::browser::BOOKMARK_LIMIT
            * (crate::browser::BOOKMARK_TITLE_LIMIT + crate::browser::URL_LIMIT)
        + crate::desktop_customization::MAX_ICONS
            * crate::desktop_customization::DESKTOP_NAME_LIMIT
        + crate::desktop_customization::WALLPAPER_PATH_LIMIT)
    + 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applications {
    pub docker: bool,
    pub compose: bool,
    pub snap: bool,
    pub tmux: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopPreferences {
    pub version: u8,
    pub theme: Theme,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<crate::Language>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    #[serde(skip_serializing_if = "DesktopClock::is_default")]
    pub clock: DesktopClock,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_interval_seconds: Option<u64>,
    pub app_settings: DesktopAppSettings,
    pub shortcuts: Shortcuts,
    #[serde(skip_serializing_if = "TerminalLayout::is_empty")]
    pub terminal: TerminalLayout,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub windows: BTreeMap<DesktopApp, DesktopWindow>,
    #[serde(
        alias = "explorer",
        skip_serializing_if = "FilesPreferences::is_default"
    )]
    pub files: FilesPreferences,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub browser_bookmarks: Vec<crate::browser::Bookmark>,
    #[serde(skip_serializing_if = "crate::desktop_customization::Appearance::is_default")]
    pub appearance: crate::desktop_customization::Appearance,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopAppSettings {
    pub terminal: TerminalAppearance,
    pub editor: EditorSettings,
    pub player: PlayerSettings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalAppearance {
    pub font_size: u8,
    pub font_family: TerminalFont,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    Block,
    Bar,
    Underline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalFont {
    Automatic,
    System,
    Consolas,
    Menlo,
}

impl TerminalFont {
    pub fn css(self) -> &'static str {
        match self {
            Self::Automatic => r#""Cascadia Mono", Consolas, "Liberation Mono", monospace"#,
            Self::System => "ui-monospace, monospace",
            Self::Consolas => "Consolas, monospace",
            Self::Menlo => "Menlo, monospace",
        }
    }
}

impl Default for TerminalAppearance {
    fn default() -> Self {
        Self {
            font_size: 13,
            font_family: TerminalFont::Automatic,
            cursor_style: CursorStyle::Block,
            cursor_blink: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EditorSettings {
    pub font_size: u8,
    pub line_numbers: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_size: 13,
            line_numbers: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerSettings {
    pub volume: u8,
    pub speed: u16,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            volume: 100,
            speed: 100,
        }
    }
}

impl DesktopAppSettings {
    pub const FONT_SIZES: [u8; 7] = [12, 13, 14, 16, 18, 20, 24];
    pub const SPEEDS: [u16; 6] = [50, 75, 100, 125, 150, 200];

    fn validate(&self) -> Result<()> {
        ensure!(
            Self::FONT_SIZES.contains(&self.terminal.font_size)
                && Self::FONT_SIZES.contains(&self.editor.font_size),
            "Invalid app font size"
        );
        ensure!(
            self.player.volume <= 100 && Self::SPEEDS.contains(&self.player.speed),
            "Invalid playback defaults"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
// Older desktop snapshots include a timezone field. The clock now follows the server.
#[serde(default)]
pub struct DesktopClock {
    pub format: ClockFormat,
    pub show_weekday: bool,
    pub show_date: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_seconds: Option<bool>,
}

impl Default for DesktopClock {
    fn default() -> Self {
        Self {
            format: ClockFormat::default(),
            show_weekday: false,
            show_date: true,
            show_seconds: None,
        }
    }
}

impl DesktopClock {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockFormat {
    #[default]
    Automatic,
    TwelveHour,
    TwentyFourHour,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopApp {
    Terminal,
    Monitor,
    #[serde(alias = "store")]
    AppCenter,
    // Retained so existing desktop and Dock preferences still deserialize.
    Services,
    Logs,
    #[serde(alias = "explorer")]
    Files,
    Disks,
    Docker,
    Editor,
    Code,
    #[serde(alias = "video")]
    Player,
    ImageViewer,
    PdfReader,
    Trash,
    Browser,
    Settings,
}

impl DesktopApp {
    pub const COUNT: usize = 16;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<WindowBounds>,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<WindowWorkArea>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowWorkArea {
    pub width: u32,
    pub height: u32,
}

impl WindowBounds {
    pub fn validate(self) -> Result<()> {
        ensure!(
            (-32768..=32768).contains(&self.x)
                && (-32768..=32768).contains(&self.y)
                && (1..=32768).contains(&self.width)
                && (1..=32768).contains(&self.height)
                && self.reference.is_none_or(|area| {
                    (1..=32768).contains(&area.width) && (1..=32768).contains(&area.height)
                }),
            "Invalid desktop window geometry"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesPreferences {
    #[serde(default)]
    pub sidebar_hidden: bool,
    #[serde(default)]
    pub favorites: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub colors: BTreeMap<String, FolderColor>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub folder_icons: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Gray,
}

impl FilesPreferences {
    pub const MAX_FAVORITES: usize = 24;
    pub const MAX_COLORS: usize = 64;
    pub const MAX_FOLDER_ICONS: usize = 64;
    const PATHS_LIMIT: usize = 24 * 1024;
    const ICON_ID_LIMIT: usize = 48;
    pub fn marked_paths(&self) -> std::collections::BTreeSet<String> {
        self.favorites
            .iter()
            .chain(self.colors.keys())
            .chain(self.folder_icons.keys())
            .cloned()
            .collect()
    }

    pub fn remove_path_metadata(&mut self, paths: &std::collections::BTreeSet<String>) {
        self.favorites.retain(|path| !paths.contains(path));
        self.colors.retain(|path, _| !paths.contains(path));
        self.folder_icons.retain(|path, _| !paths.contains(path));
    }

    fn is_default(&self) -> bool {
        !self.sidebar_hidden
            && self.favorites.is_empty()
            && self.colors.is_empty()
            && self.folder_icons.is_empty()
    }
    pub fn validate_path(path: &str) -> Result<()> {
        ensure!(
            path.starts_with('/') && path.len() <= 1536 && !path.chars().any(char::is_control),
            "Invalid favorite folder path"
        );
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.favorites.len() <= Self::MAX_FAVORITES,
            "Too many favorite folders"
        );
        ensure!(
            self.colors.len() <= Self::MAX_COLORS,
            "Too many folder colors"
        );
        ensure!(
            self.folder_icons.len() <= Self::MAX_FOLDER_ICONS,
            "Too many custom folder icons"
        );
        ensure!(
            self.favorites
                .iter()
                .chain(self.colors.keys())
                .chain(self.folder_icons.keys())
                .map(String::len)
                .sum::<usize>()
                <= Self::PATHS_LIMIT,
            "Files preferences exceed the path limit"
        );
        let mut seen = HashSet::new();
        for path in &self.favorites {
            Self::validate_path(path)?;
            ensure!(seen.insert(path), "Duplicate favorite folder");
        }
        for path in self.colors.keys().chain(self.folder_icons.keys()) {
            Self::validate_path(path)?;
        }
        for icon in self.folder_icons.values() {
            // An optional style prefix and a semantic name identify a client-bundled icon.
            let valid_part = |part: &str| {
                !part.is_empty()
                    && part.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            };
            ensure!(
                icon.len() <= Self::ICON_ID_LIMIT
                    && icon.split_once(':').map_or_else(
                        || valid_part(icon),
                        |(style, name)| valid_part(style) && valid_part(name),
                    ),
                "Invalid folder icon identifier"
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalLayout {
    pub tabs: Vec<TerminalTab>,
    pub active: String,
    pub maximized: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalTab {
    pub id: String,
    pub name: Option<String>,
    pub directory: Option<String>,
}
impl TerminalLayout {
    fn is_empty(&self) -> bool {
        self.tabs.is_empty() && !self.maximized
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.tabs.len() <= TERMINAL_TAB_LIMIT,
            "At most {TERMINAL_TAB_LIMIT} terminal tabs can be saved"
        );
        let mut ids = std::collections::HashSet::new();
        for tab in &self.tabs {
            crate::terminal::validate_session(&tab.id)?;
            ensure!(ids.insert(tab.id.clone()), "Duplicate terminal session");
            if let Some(name) = &tab.name {
                ensure!(
                    name.len() <= TERMINAL_TAB_NAME_LIMIT && !name.chars().any(char::is_control),
                    "Invalid terminal tab name"
                );
            }
            if let Some(directory) = &tab.directory {
                crate::terminal::validate_directory(directory)?;
            }
        }
        ensure!(
            self.active.is_empty() || ids.contains(&self.active),
            "Invalid active terminal tab"
        );
        Ok(())
    }
}
impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            version: 1,
            theme: Theme::System,
            language: None,
            time_zone: None,
            clock: DesktopClock::default(),
            resource_interval_seconds: None,
            app_settings: DesktopAppSettings::default(),
            shortcuts: Shortcuts::default(),
            terminal: Default::default(),
            windows: Default::default(),
            files: Default::default(),
            browser_bookmarks: Vec::new(),
            appearance: Default::default(),
        }
    }
}

impl DesktopPreferences {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.time_zone
                .as_deref()
                .is_none_or(crate::desktop_time_zone::valid_name),
            "Invalid desktop time zone"
        );
        ensure!(self.version == 1, "Unsupported desktop preferences version");
        ensure!(
            self.resource_interval_seconds
                .is_none_or(|seconds| crate::AppSettings::RESOURCE_INTERVALS.contains(&seconds)),
            "Invalid desktop resource refresh interval"
        );
        self.app_settings.validate()?;
        self.terminal.validate()?;
        self.shortcuts.validate()?;
        self.files.validate()?;
        crate::browser::validate_bookmarks(&self.browser_bookmarks)?;
        self.appearance.validate()?;
        for bounds in self.windows.values().filter_map(|window| window.bounds) {
            bounds.validate()?;
        }
        Ok(())
    }
}

pub(crate) fn execute(
    session: &RemoteSession,
    command: &str,
    limit: usize,
) -> Result<CommandOutput> {
    let (stdout, stderr, exit_code) = execute_bytes(session, command, limit)?;
    Ok(CommandOutput {
        stdout: String::from_utf8(stdout)?,
        stderr: lossy(stderr),
        exit_code,
    })
}

/// Like [`execute`], for output that is displayed rather than parsed: invalid
/// UTF-8 in either stream becomes U+FFFD instead of failing the request.
pub(crate) fn execute_lossy(
    session: &RemoteSession,
    command: &str,
    limit: usize,
) -> Result<CommandOutput> {
    let (stdout, stderr, exit_code) = execute_bytes(session, command, limit)?;
    Ok(CommandOutput {
        stdout: lossy(stdout),
        stderr: lossy(stderr),
        exit_code,
    })
}

/// Reuses the buffer when it is already valid UTF-8.
fn lossy(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

fn execute_bytes(
    session: &RemoteSession,
    command: &str,
    limit: usize,
) -> Result<(Vec<u8>, Vec<u8>, i32)> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let output = session.exec_stream_checked(
        command,
        None,
        Duration::from_secs(90),
        None,
        false,
        |stream, bytes| {
            ensure!(
                stdout.len() + stderr.len() + bytes.len() <= limit,
                "Server response exceeded the size limit"
            );
            match stream {
                CommandStream::Stdout => stdout.extend_from_slice(bytes),
                CommandStream::Stderr => stderr.extend_from_slice(bytes),
            }
            Ok(())
        },
    )?;
    Ok((stdout, stderr, output.exit_code))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockSample {
    pub unix_seconds: i64,
    pub offset_minutes: i16,
    pub zone: Option<String>,
}

pub fn clock(session: &RemoteSession) -> Result<ClockSample> {
    let output = execute(
        session,
        "# arcnix:desktop:clock\nunset TZ\nLC_ALL=C date '+%s %z %Z'",
        256,
    )?;
    ensure!(output.exit_code == 0, "Cannot read the server clock");
    parse_clock(&output.stdout)
}

pub(crate) fn parse_clock(output: &str) -> Result<ClockSample> {
    let mut fields = output.trim().splitn(3, ' ');
    let seconds = fields
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing server time"))?;
    let offset = fields
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing server time zone"))?;
    let zone = fields
        .next()
        .map(str::trim)
        .filter(|zone| !zone.is_empty() && zone.len() <= 64 && !zone.chars().any(char::is_control));
    let [
        sign @ (b'+' | b'-'),
        hours_tens @ b'0'..=b'9',
        hours_units @ b'0'..=b'9',
        minutes_tens @ b'0'..=b'9',
        minutes_units @ b'0'..=b'9',
    ] = *offset.as_bytes()
    else {
        anyhow::bail!("Invalid server UTC offset");
    };
    let hours = i16::from(hours_tens - b'0') * 10 + i16::from(hours_units - b'0');
    let minutes = i16::from(minutes_tens - b'0') * 10 + i16::from(minutes_units - b'0');
    ensure!(hours < 24 && minutes < 60, "Invalid server UTC offset");
    let multiplier = if sign == b'-' { -1 } else { 1 };
    Ok(ClockSample {
        unix_seconds: seconds.parse()?,
        offset_minutes: multiplier * (hours * 60 + minutes),
        zone: zone.map(str::to_owned),
    })
}

pub fn discover_command() -> &'static str {
    "# arcnix:apps:discover\nprintf 'ARCNIX_APPS_V1\\n'; if ! command -v docker >/dev/null 2>&1 && test -x /snap/bin/docker; then export PATH=\"$PATH:/snap/bin\"; fi; if command -v docker >/dev/null 2>&1; then printf 'docker\\n'; if command -v timeout >/dev/null 2>&1 && timeout -k 1s 5s docker compose version --short >/dev/null 2>&1; then printf 'compose\\n'; fi; fi; if command -v snap >/dev/null 2>&1; then printf 'snap\\n'; fi; if command -v tmux >/dev/null 2>&1; then printf 'tmux\\n'; fi"
}
pub fn discover(session: &RemoteSession) -> Result<Applications> {
    use crate::host::HostOs;

    let darwin_command;
    let command = match session.host_os()? {
        HostOs::Windows => {
            return Ok(Applications {
                docker: crate::docker::windows_available(session)?,
                ..Default::default()
            });
        }
        HostOs::MacOs => {
            darwin_command = format!(
                "# arcnix:apps:darwin-discover\n{}\nprintf 'ARCNIX_APPS_V1\\n'; if command -v docker >/dev/null 2>&1; then printf 'docker\\n'; fi; if command -v tmux >/dev/null 2>&1; then printf 'tmux\\n'; fi",
                crate::docker::DARWIN_CLI_PATH
            );
            &darwin_command
        }
        _ => discover_command(),
    };
    let output = execute(session, command, 4096)?;
    ensure!(
        output.exit_code == 0,
        "Application detection failed: {}",
        output.stderr.trim()
    );
    let mut lines = output.stdout.lines();
    ensure!(
        lines.next() == Some("ARCNIX_APPS_V1"),
        "Invalid application detection response"
    );
    let mut apps = Applications::default();
    for line in lines {
        let target = match line {
            "docker" => &mut apps.docker,
            "compose" => &mut apps.compose,
            "snap" => &mut apps.snap,
            "tmux" => &mut apps.tmux,
            _ => anyhow::bail!("Invalid application detection response"),
        };
        ensure!(!*target, "Duplicate application detection response");
        *target = true;
    }
    Ok(apps)
}

pub fn parse_preferences(text: &str) -> Result<DesktopPreferences> {
    let prefs: Option<DesktopPreferences> = serde_json::from_str(text)?;
    let prefs = prefs.unwrap_or_default();
    prefs.validate()?;
    Ok(prefs)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_customization::DockItem;

    #[test]
    fn settings_naming_newer_apps_still_load() -> Result<()> {
        use crate::desktop_customization::{Shortcut, SystemIcon};
        // Saved by a newer Arcnix with an app this version does not have.
        let prefs = parse_preferences(
            r#"{"version":1,"appearance":{
                "order":[{"kind":"app","id":"files"},{"kind":"app","id":"later_app"},
                         {"kind":"file","id":"notes.txt"}],
                "hidden":["later_app","docker"],
                "dock":{"order":["later_app","terminal",{"system":"home"}],
                        "unpinned":["later_app"],"extra_pins":["later_app","editor"]}}}"#,
        )?;
        let appearance = &prefs.appearance;
        assert_eq!(
            appearance.order,
            [
                Shortcut::App(DesktopApp::Files),
                Shortcut::File("notes.txt".into())
            ]
        );
        assert_eq!(appearance.hidden, [DesktopApp::Docker]);
        assert_eq!(
            appearance.dock.order,
            [
                DockItem::App(DesktopApp::Terminal),
                DockItem::System {
                    system: SystemIcon::Home
                }
            ]
        );
        assert!(appearance.dock.unpinned.is_empty());
        assert_eq!(appearance.dock.extra_pins, [DesktopApp::Editor]);
        // The per-file format reads the desktop file separately.
        let packet = crate::desktop_storage::encode(&DesktopPreferences::default())?;
        let shell = br#"{"order":[{"kind":"app","id":"later_app"}],"hidden":["later_app"]}"#;
        let packet =
            crate::desktop_storage::replace_for_test(&packet, "preferences/desktop.json", shell)?;
        assert_eq!(
            crate::desktop_storage::decode(&packet)?,
            DesktopPreferences::default()
        );
        // Other unreadable entries are still errors.
        for broken in [
            r#"{"appearance":{"order":[{"kind":"widget","id":"x"}]}}"#,
            r#"{"appearance":{"order":[{"kind":"app","id":7}]}}"#,
            r#"{"appearance":{"order":["later_app"]}}"#,
            r#"{"appearance":{"hidden":[{"kind":"app","id":"later_app"}]}}"#,
            r#"{"appearance":{"dock":{"order":[{"kind":"app","id":"later_app"}]}}}"#,
            r#"{"appearance":{"hidden":[7]}}"#,
            r#"{"appearance":{"dock":{"order":[{"system":"nowhere"}]}}}"#,
        ] {
            assert!(parse_preferences(broken).is_err(), "{broken}");
        }
        Ok(())
    }

    #[test]
    fn app_preferences_round_trip_through_desktop_storage() -> Result<()> {
        let mut prefs = parse_preferences(r#"{"version":1}"#)?;
        assert_eq!(prefs.app_settings, DesktopAppSettings::default());
        prefs.app_settings.terminal.font_family = TerminalFont::Consolas;
        prefs.app_settings.terminal.cursor_blink = false;
        prefs.app_settings.editor.font_size = 18;
        prefs.app_settings.editor.line_numbers = false;
        prefs.app_settings.player.volume = 40;
        prefs.app_settings.player.speed = 150;
        let packet = crate::desktop_storage::encode(&prefs)?;
        assert_eq!(crate::desktop_storage::decode(&packet)?, prefs);
        prefs.app_settings.player.speed = 0;
        assert!(prefs.validate().is_err());
        Ok(())
    }

    #[test]
    fn desktop_clock_settings_round_trip_with_legacy_defaults() -> Result<()> {
        let mut prefs = parse_preferences(r#"{"version":1,"theme":"System"}"#)?;
        assert_eq!(prefs.clock, DesktopClock::default());
        let legacy = parse_preferences(
            r#"{"version":1,"theme":"System","clock":{"timezone":"local","format":"twelve_hour"}}"#,
        )?;
        assert_eq!(legacy.clock.format, ClockFormat::TwelveHour);
        assert!(legacy.clock.show_date);
        assert!(!serde_json::to_string(&legacy)?.contains("timezone"));
        prefs.clock = DesktopClock {
            format: ClockFormat::TwentyFourHour,
            show_weekday: true,
            show_date: false,
            show_seconds: Some(false),
        };
        let packet = crate::desktop_storage::encode(&prefs)?;
        assert_eq!(crate::desktop_storage::decode(&packet)?, prefs);
        Ok(())
    }

    #[test]
    fn loads_legacy_desktop_app_names_and_saves_canonical_names() {
        let prefs = parse_preferences(r#"{
            "version":1,"theme":"Dark",
            "terminal":{"tabs":[],"active":"","maximized":true},
            "windows":{
                "explorer":{"bounds":{"x":34,"y":52,"width":820,"height":480}},
                "store":{"maximized":true},
                "video":{"maximized":false}
            },
            "explorer":{
                "sidebar_hidden":true,"favorites":["/srv/网站"],"colors":{"/srv/网站":"purple"},
                "folder_icons":{"/srv/照片":"photos","/srv/cloud":"filled:cloud","/srv/calendar":"duotone:calendar"}
            },
            "appearance":{
                "order":[{"kind":"app","id":"store"}],
                "hidden":["video"],
                "dock":{"order":["explorer","store","video"],"unpinned":["video"]}
            }
        }"#).unwrap();
        assert!(!prefs.windows[&DesktopApp::Files].maximized);
        assert!(prefs.windows[&DesktopApp::AppCenter].maximized);
        assert!(prefs.windows.contains_key(&DesktopApp::Player));
        assert_eq!(prefs.files.colors["/srv/网站"], FolderColor::Purple);
        assert_eq!(prefs.files.folder_icons["/srv/照片"], "photos");
        assert_eq!(prefs.files.folder_icons["/srv/cloud"], "filled:cloud");
        assert_eq!(
            prefs.files.folder_icons["/srv/calendar"],
            "duotone:calendar"
        );
        assert!(prefs.files.sidebar_hidden);
        assert_eq!(prefs.files.favorites, ["/srv/网站"]);
        assert_eq!(
            prefs.appearance.dock.order,
            [DesktopApp::Files, DesktopApp::AppCenter, DesktopApp::Player].map(DockItem::App)
        );
        let saved = serde_json::to_value(&prefs).unwrap();
        assert_eq!(saved["windows"]["files"]["bounds"]["width"], 820);
        assert_eq!(saved["windows"]["app_center"]["maximized"], true);
        assert!(saved["windows"].get("player").is_some());
        assert_eq!(saved["files"]["sidebar_hidden"], true);
        assert!(saved.get("explorer").is_none());
        assert_eq!(saved["appearance"]["order"][0]["id"], "app_center");
        assert_eq!(saved["appearance"]["hidden"][0], "player");
        assert_eq!(saved["appearance"]["dock"]["unpinned"][0], "player");
        assert_eq!(parse_preferences(&saved.to_string()).unwrap(), prefs);
        for field in [
            r#""windows":{"explorer":{"bounds":{"x":0,"y":0,"width":0,"height":900}}}"#,
            r#""explorer":{"favorites":["relative"]}"#,
            r#""explorer":{"colors":{"/srv":"chartreuse"}}"#,
            r#""files":{"folder_icons":{"relative":"photos"}}"#,
            r#""files":{"folder_icons":{"/srv":"filled:"}}"#,
            r#""files":{"folder_icons":{"/srv":"<svg>"}}"#,
        ] {
            assert!(
                parse_preferences(&format!(r#"{{"version":1,"theme":"Dark",{field}}}"#)).is_err()
            );
        }
    }
    #[test]
    fn editor_and_code_keep_independent_layouts() -> Result<()> {
        use DesktopApp::{Code, Editor};
        let mut prefs = DesktopPreferences::default();
        prefs.windows.insert(
            Editor,
            DesktopWindow {
                bounds: None,
                maximized: true,
            },
        );
        prefs.windows.insert(Code, DesktopWindow::default());
        prefs.appearance.dock.order = vec![DockItem::App(Editor), DockItem::App(Code)];
        let legacy = parse_preferences(&serde_json::to_string(&prefs)?)?;
        assert_eq!(legacy, prefs);
        assert_eq!(
            crate::desktop_storage::decode(&crate::desktop_storage::encode(&prefs)?)?,
            prefs
        );
        Ok(())
    }

    #[test]
    fn dock_order_keeps_existing_app_ids_alongside_system_shortcuts() -> Result<()> {
        let prefs = parse_preferences(
            r#"{"appearance":{"dock":{"order":["explorer",{"system":"home"},"editor",{"system":"trash"}]}}}"#,
        )?;
        let saved = serde_json::to_value(&prefs)?;
        assert_eq!(
            saved["appearance"]["dock"]["order"],
            serde_json::json!(["files", {"system":"home"}, "editor", {"system":"trash"}])
        );
        assert_eq!(
            crate::desktop_storage::decode(&crate::desktop_storage::encode(&prefs)?)?,
            prefs
        );
        assert!(
            parse_preferences(
                r#"{"appearance":{"dock":{"order":[{"system":"home"},{"system":"home"}]}}}"#,
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn desktop_and_dock_share_all_icon_presets() -> Result<()> {
        for (preset, expected) in [("small", 32), ("medium", 48), ("large", 64)] {
            let json = serde_json::json!({
                "appearance": {"dock": {"size": preset}, "layout": {"size": preset}}
            });
            let prefs = parse_preferences(&json.to_string())?;
            assert_eq!(prefs.appearance.dock.icon_pixels(), expected);
            assert_eq!(prefs.appearance.layout.icon_pixels(), expected);
        }
        let prefs =
            parse_preferences(r#"{"appearance":{"layout":{"size":"small","icon_size":40}}}"#)?;
        assert_eq!(prefs.appearance.layout.icon_pixels(), 40);
        let custom =
            parse_preferences(r#"{"appearance":{"layout":{"size":"medium","icon_size":55}}}"#)?;
        assert_eq!(custom.appearance.layout.icon_pixels(), 55);
        Ok(())
    }

    #[test]
    fn legacy_icon_sizes_fit_the_current_controls() -> Result<()> {
        let mut prefs = parse_preferences(
            r#"{"appearance":{"dock":{"icon_size":24},"layout":{"icon_size":88}}}"#,
        )?;
        assert_eq!(prefs.appearance.dock.icon_pixels(), 32);
        assert_eq!(prefs.appearance.layout.icon_pixels(), 64);
        assert_eq!(
            crate::desktop_storage::decode(&crate::desktop_storage::encode(&prefs)?)?,
            prefs
        );
        prefs.appearance.dock.set_icon_pixels(20);
        prefs.appearance.layout.set_icon_pixels(80);
        assert_eq!(prefs.appearance.dock.icon_size, None);
        assert_eq!(prefs.appearance.layout.icon_size, None);
        assert_eq!(
            prefs.appearance.dock.size,
            crate::desktop_customization::DockSize::Small
        );
        assert_eq!(
            prefs.appearance.layout.size,
            crate::desktop_customization::DesktopIconSize::Large
        );
        Ok(())
    }

    #[test]
    fn preferences_require_a_known_schema_and_closed_theme() {
        for empty in ["null", "{}"] {
            assert_eq!(
                parse_preferences(empty).unwrap(),
                DesktopPreferences::default()
            );
        }
        assert_eq!(
            parse_preferences(r#"{"version":1,"theme":"Light"}"#)
                .unwrap()
                .theme,
            Theme::Light
        );
        for invalid in [
            r#"{"version":2,"theme":"Dark"}"#,
            r#"{"version":1,"theme":"$(echo bad)"}"#,
            r#"{"theme":"Dark","unknown":true}"#,
        ] {
            assert!(parse_preferences(invalid).is_err());
        }
        assert!(parse_preferences(&" ".repeat(4097)).is_err());
    }
}
