use arcnix_core::desktop_customization::{
    self as desktop, Appearance, DesktopItems, Dock, DockItem, DockPosition, IconDirection,
    IconFlow, Layout, MAX_ICON_SIZE, MEDIUM_ICON_SIZE, MIN_ICON_SIZE, Shortcut, SystemIcon,
    Wallpaper,
};
use arcnix_core::desktop_shortcuts::Action as KeyAction;
use arcnix_core::server_apps::DesktopApp;
use serde::Deserialize;

use crate::i18n::desktop as i18n;
use crate::server_apps_worker::{Action as AppAction, Reply};
use crate::*;

#[derive(Clone, Default)]
pub(crate) struct Runtime {
    items: Option<DesktopItems>,
    items_pending: Option<u64>,
    menu: Option<Menu>,
    settings_page: SettingsPage,
    settings_search: Option<String>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SettingsPage {
    #[default]
    General,
    Network,
    DefaultApps,
    Desktop,
    Dock,
    Appearance,
    Region,
    DateTime,
    Shortcuts,
    About,
}

pub(crate) fn settings_page(s: &State) -> SettingsPage {
    runtime(s).settings_page
}

#[cfg(windows)]
pub(super) fn menu_open(s: &State) -> bool {
    runtime(s).menu.is_some()
}

fn runtime(s: &State) -> &Runtime {
    &s.server_apps.customization
}
pub(crate) fn appearance(s: &State) -> &Appearance {
    &server_apps_ui::current(s).appearance
}

pub(crate) fn update(ui: &Ui, change: impl FnOnce(&mut Appearance)) {
    if !ui.state.with(server_apps_ui::preferences_ready) {
        return;
    }
    let mut prefs = ui.state.with(server_apps_ui::desired);
    change(&mut prefs.appearance);
    if let Err(error) = prefs.appearance.validate() {
        ui.error(error.to_string());
    } else if ui.state.with(|s| &prefs != server_apps_ui::current(s)) {
        ui.queue_desktop_preferences(prefs);
    }
}

impl Ui {
    pub(crate) fn refresh_desktop_items(&self, force: bool) {
        if !self.state.with(|s| {
            s.connected
                && s.sftp_available
                && runtime(s).items_pending.is_none()
                && (force || runtime(s).items.is_none())
        }) {
            return;
        }
        let serial = self.send(Action::ServerApps(AppAction::DesktopItems));
        self.state
            .revise(|s| s.server_apps.customization.items_pending = Some(serial));
    }

    pub(crate) fn close_desktop_menu(&self) {
        self.state
            .revise(|s| s.server_apps.customization.menu = None);
    }

    fn open_desktop_file(&self, name: &str) {
        let entry = self.state.with(|s| find_file(s, name).cloned());
        if let Some(entry) = entry {
            if entry.is_symlink {
                file_actions::open_alias(self, &entry.path);
            } else if entry.is_dir {
                self.open_desktop_app(WorkspacePage::Files(0));
                file_browser::navigate(self, true, entry.path);
            } else {
                file_browser::open_remote_file(self, &entry.path);
            }
        }
    }
}

pub(crate) fn pending(ui: &Ui, serial: u64) -> bool {
    ui.state.with(|s| runtime(s).items_pending == Some(serial))
}

pub(crate) fn event(ui: &Ui, result: std::result::Result<Reply, String>) {
    ui.state.revise(|s| {
        let desktop = &mut s.server_apps.customization;
        desktop.items_pending = None;
        if let Ok(Reply::DesktopItems(items)) = result {
            desktop.items = Some(items);
        } else if desktop.items.is_none() {
            desktop.items = Some(DesktopItems::default());
        }
    });
}

fn find_file<'a>(s: &'a State, name: &str) -> Option<&'a FileEntry> {
    runtime(s)
        .items
        .as_ref()?
        .entries
        .iter()
        .find(|entry| entry.name == name)
}

fn desktop_icons(s: &State) -> Vec<Shortcut> {
    let settings = appearance(s);
    let mut icons: Vec<_> = server_desktop::available_apps(s)
        .into_iter()
        .filter_map(server_desktop::app_id)
        .filter(|&app| shortcut_visible(s, app))
        .map(Shortcut::App)
        .collect();
    if s.sftp_available {
        if settings.layout.show_home {
            icons.insert(0, Shortcut::System(SystemIcon::Home));
        }
        if settings.layout.show_trash && s.local_desktop.is_none() {
            icons.push(Shortcut::System(SystemIcon::Trash));
        }
    }
    if let Some(items) = &runtime(s).items {
        icons.extend(
            items
                .entries
                .iter()
                .map(|entry| Shortcut::File(entry.name.clone())),
        );
    }
    ordered(icons, &settings.order)
}

fn system_app(app: DesktopApp) -> bool {
    app != DesktopApp::PgAdmin
}

pub(super) fn shortcut_visible(s: &State, app: DesktopApp) -> bool {
    let prefs = appearance(s);
    prefs.show_apps
        && !prefs.hidden.contains(&app)
        && (default_app_shortcut(s, app) || prefs.extra_shortcuts.contains(&app))
}

fn default_app_shortcut(s: &State, app: DesktopApp) -> bool {
    match app {
        DesktopApp::Code | DesktopApp::Player | DesktopApp::ImageViewer | DesktopApp::PdfReader => {
            s.local_desktop.is_some()
        }
        // Older Arcnix versions reject desktop settings that name Nginx or
        // Redis, so they are saved only when someone pins them.
        DesktopApp::Editor | DesktopApp::PgAdmin | DesktopApp::Nginx | DesktopApp::Redis => false,
        _ => true,
    }
}

pub(super) fn set_shortcut_visible(ui: &Ui, app: DesktopApp, visible: bool) {
    if ui.state.with(|s| shortcut_visible(s, app)) == visible {
        return;
    }
    update(ui, |prefs| {
        if !prefs.show_apps {
            // Convert the legacy master switch into per-app choices first.
            prefs.extra_shortcuts.clear();
            prefs.hidden = server_desktop::APP_LAUNCHERS
                .into_iter()
                .filter_map(server_desktop::app_id)
                .filter(|&id| ui.state.with(|s| default_app_shortcut(s, id)))
                .collect();
            prefs.show_apps = true;
        }
        prefs.hidden.retain(|&id| id != app);
        prefs.extra_shortcuts.retain(|&id| id != app);
        match (ui.state.with(|s| default_app_shortcut(s, app)), visible) {
            (false, true) => prefs.extra_shortcuts.push(app),
            (true, false) => prefs.hidden.push(app),
            _ => {}
        }
    });
}

fn available_shortcuts(s: &State) -> Vec<DockItem> {
    let apps = server_desktop::available_apps(s)
        .into_iter()
        .filter_map(server_desktop::app_id)
        .map(DockItem::App);
    let system_icons = [SystemIcon::Home, SystemIcon::Trash]
        .into_iter()
        .filter(|&icon| icon != SystemIcon::Trash || s.local_desktop.is_none())
        .map(|system| DockItem::System { system });
    apps.chain(system_icons).collect()
}

fn dock_choices(s: &State) -> Vec<DockItem> {
    ordered(available_shortcuts(s), &appearance(s).dock.order)
}

fn dock_icons(s: &State) -> Vec<Shortcut> {
    let mut icons: Vec<_> = dock_choices(s)
        .into_iter()
        .filter(|&item| {
            dock_pinned(s, item) && (s.sftp_available || matches!(item, DockItem::App(_)))
        })
        .map(Shortcut::from)
        .collect();
    for app in s
        .workbench
        .apps
        .iter()
        .copied()
        .filter_map(server_desktop::app_id)
    {
        let icon = Shortcut::App(app);
        if !icons.contains(&icon) {
            icons.push(icon);
        }
    }
    icons
}

pub(super) fn dock_pinned(s: &State, item: DockItem) -> bool {
    let dock = &appearance(s).dock;
    match item {
        DockItem::App(app) => {
            !dock.unpinned.contains(&app)
                && (default_app_shortcut(s, app) || dock.extra_pins.contains(&app))
        }
        DockItem::System { .. } => dock.order.contains(&item),
    }
}

fn set_app_pin(s: &State, dock: &mut Dock, app: DesktopApp, pinned: bool) {
    dock.unpinned.retain(|&id| id != app);
    dock.extra_pins.retain(|&id| id != app);
    match (default_app_shortcut(s, app), pinned) {
        (true, false) => dock.unpinned.push(app),
        (false, true) => dock.extra_pins.push(app),
        _ => {}
    }
}

pub(super) fn toggle_dock_pin(ui: &Ui, item: DockItem) {
    update(ui, |prefs| {
        let dock = &mut prefs.dock;
        let pinned = ui.state.with(|s| dock_pinned(s, item));
        match item {
            DockItem::App(app) => ui.state.with(|s| set_app_pin(s, dock, app, !pinned)),
            _ if pinned => dock.order.retain(|&id| id != item),
            _ => ui.state.with(|s| {
                for candidate in available_shortcuts(s) {
                    if (candidate == item || dock_pinned(s, candidate))
                        && !dock.order.contains(&candidate)
                    {
                        dock.order.push(candidate);
                    }
                }
            }),
        }
    });
}

fn system_icon(icon: SystemIcon) -> (&'static str, Icon, &'static str) {
    match icon {
        SystemIcon::Home => ("主目录", Icon::Home, "app-home"),
        SystemIcon::Trash => ("Trash", Icon::Clear, "app-trash"),
    }
}

fn system_visible(prefs: &Appearance, icon: SystemIcon) -> bool {
    match icon {
        SystemIcon::Home => prefs.layout.show_home,
        SystemIcon::Trash => prefs.layout.show_trash,
    }
}

fn set_system_visible(ui: &Ui, icon: SystemIcon, visible: bool) {
    update(ui, |prefs| match icon {
        SystemIcon::Home => prefs.layout.show_home = visible,
        SystemIcon::Trash => prefs.layout.show_trash = visible,
    });
}

fn open_system_icon(ui: &Ui, icon: SystemIcon) {
    let home = ui.state.with(|s| {
        runtime(s)
            .items
            .as_ref()
            .map(|items| items.home.clone())
            .filter(|home| !home.is_empty())
            .or_else(|| {
                s.server_apps
                    .files_locations
                    .as_ref()?
                    .iter()
                    .find(|location| location.name == "Home")
                    .map(|location| location.path.clone())
            })
    });
    ui.open_desktop_app(WorkspacePage::Files(0));
    match icon {
        SystemIcon::Home => {
            if let Some(path) = home {
                file_browser::navigate(ui, true, path);
            }
        }
        SystemIcon::Trash => trash_ui::open(ui, true),
    }
}

fn ordered<T: Clone + Eq>(mut icons: Vec<T>, order: &[T]) -> Vec<T> {
    let mut sorted = Vec::with_capacity(icons.len());
    for item in order {
        if let Some(index) = icons.iter().position(|icon| icon == item) {
            sorted.push(icons.remove(index));
        }
    }
    sorted.extend(icons);
    sorted
}

pub(crate) fn launchers(ui: Ui, dock: bool) -> impl Widget {
    let state = ui.state;
    each(state.map(move |s| {
        if dock {
            dock_icons(s)
        } else {
            desktop_icons(s)
        }
    }))
    .key(Clone::clone)
    .children(move |item| {
        let id = item.value.get();
        let key = serde_json::to_string(&id).expect("valid desktop item ID");
        let body = match id {
            Shortcut::App(app) => fragment(server_desktop::launcher(
                ui.clone(),
                server_desktop::app_page(app),
                dock,
            )),
            Shortcut::System(icon) => {
                let (label, _, tile) = system_icon(icon);
                let class = if dock {
                    "desktop-dock-app"
                } else {
                    "desktop-shortcut"
                };
                let open = ui.clone();
                fragment(
                    sapid::button()
                        .class(format!("{class} {tile}"))
                        .name(i18n::bind(label))
                        .fill((
                            text().class("desktop-app-icon").content(""),
                            text().class("desktop-app-label").content(i18n::bind(label)),
                        ))
                        .on_press(move || open_system_icon(&open, icon)),
                )
            }
            Shortcut::File(name) => {
                let open = ui.clone();
                let current = name.clone();
                let appearance = name.clone();
                let busy_name = name.clone();
                let busy = move |s: &State| {
                    find_file(s, &busy_name).is_some_and(|e| office_sync::uploading(s, &e.path))
                };
                let badge = state.map(busy.clone());
                fragment(
                    sapid::button()
                        .class(state.map(move |s| {
                            let icon = find_file(s, &current).map_or(Icon::File, |file| {
                                files_search::file_icon(&file.name, file.is_dir)
                            });
                            let art = icon.color().map_or("document", |(name, _)| name);
                            format!("desktop-shortcut desktop-file app-{art}")
                        }))
                        .name(name.clone())
                        .enabled(state.map(move |s| !busy(s)))
                        .fill((
                            text()
                                .class(state.map(move |s| {
                                    let mut class = "desktop-app-icon".to_owned();
                                    if find_file(s, &appearance)
                                        .is_some_and(|entry| entry.is_symlink)
                                    {
                                        class.push_str(" desktop-alias-mark");
                                    }
                                    if let Some(entry) =
                                        find_file(s, &appearance).filter(|entry| !entry.is_symlink)
                                    {
                                        if let Some(tag) = files_ui::tag_class(s, &entry.path) {
                                            class.push_str(&format!(" desktop-tagged {tag}"));
                                        }
                                        if entry.is_dir
                                            && let Some(icon) =
                                                files_ui::folder_icon_class(s, &entry.path)
                                        {
                                            class.push_str(&format!(" folder-custom-mark {icon}"));
                                        }
                                    }
                                    class
                                }))
                                .content(""),
                            office_sync::upload_badge(badge),
                            text().class("desktop-app-label").content(name.clone()),
                        ))
                        .on_press(move || open.open_desktop_file(&name)),
                )
            }
        };
        col()
            .class("desktop-icon-slot")
            .fill((text().class("desktop-item-key hidden").content(key), body))
    })
}

const BUILT_IN_WALLPAPERS: [(Wallpaper, &str, &str); 8] = [
    (Wallpaper::Prism, "Prism", "wallpaper-prism"),
    (Wallpaper::Valley, "Valley", "wallpaper-valley"),
    (Wallpaper::Tide, "Tide", "wallpaper-tide"),
    (Wallpaper::Clay, "Clay", "wallpaper-clay"),
    (Wallpaper::Meadow, "Meadow", "wallpaper-meadow"),
    (Wallpaper::Fjord, "Fjord", "wallpaper-fjord"),
    (Wallpaper::Aurora, "Aurora", "wallpaper-aurora"),
    (Wallpaper::Seaside, "Seaside", "wallpaper-seaside"),
];

pub(crate) fn customization_css() -> String {
    use std::fmt::Write;

    let mut css = format!(":root {{ --medium-icon-size: {MEDIUM_ICON_SIZE}px; }}\n");
    for size in desktop::ICON_SIZE_RANGE {
        writeln!(
            css,
            ".desktop-dock.dock-size-{size} {{ --dock-ideal: {size}px; }}"
        )
        .unwrap();
        writeln!(
            css,
            ".desktop-shortcuts.icons-size-{size} {{ --desktop-icon-ideal: {size}px; }}"
        )
        .unwrap();
    }
    css
}

pub(crate) fn wallpaper(ui: Ui) -> impl Widget {
    col().class(ui.state.map(|s| {
        format!(
            "desktop-wallpaper-art {}",
            wallpaper_class(&desktop_appearance_ui::background(s))
        )
    }))
}

fn wallpaper_class(wallpaper: &Wallpaper) -> &'static str {
    BUILT_IN_WALLPAPERS
        .iter()
        .find(|(picture, ..)| picture == wallpaper)
        .map_or("wallpaper-prism", |(_, _, class)| *class)
}

pub(crate) fn wallpaper_preview_class(wallpaper: &Wallpaper, dark: bool) -> &'static str {
    match wallpaper {
        Wallpaper::Prism if dark => "wallpaper-prism-dark",
        Wallpaper::Prism => "wallpaper-prism-light",
        Wallpaper::Valley if dark => "wallpaper-valley-dark",
        Wallpaper::Valley => "wallpaper-valley-light",
        Wallpaper::Tide if dark => "wallpaper-tide-dark",
        Wallpaper::Tide => "wallpaper-tide-light",
        Wallpaper::Clay if dark => "wallpaper-clay-dark",
        Wallpaper::Clay => "wallpaper-clay-light",
        Wallpaper::Meadow if dark => "wallpaper-meadow-dark",
        Wallpaper::Meadow => "wallpaper-meadow-light",
        Wallpaper::Fjord if dark => "wallpaper-fjord-dark",
        Wallpaper::Fjord => "wallpaper-fjord-light",
        Wallpaper::Aurora if dark => "wallpaper-aurora-dark",
        Wallpaper::Aurora => "wallpaper-aurora-light",
        Wallpaper::Seaside if dark => "wallpaper-seaside-moonrise",
        Wallpaper::Seaside => "wallpaper-seaside-sunrise",
        _ => wallpaper_class(wallpaper),
    }
}

pub(crate) fn dock_class(s: &State) -> String {
    let dock = &appearance(s).dock;
    let pixels = dock.icon_pixels();
    format!(
        "desktop-dock dock-size-{pixels}{}{}{}",
        if dock.auto_hide {
            " dock-auto-hide"
        } else {
            ""
        },
        if dock.panel_mode && dock.panel_centered {
            " dock-centered"
        } else {
            ""
        },
        if floating_dock_status(s) {
            " dock-show-status"
        } else {
            ""
        }
    )
}

pub(crate) fn dock_zone_class(s: &State) -> String {
    let dock = &appearance(s).dock;
    let position = match dock.position {
        DockPosition::Left => " dock-side-zone dock-left",
        DockPosition::Right => " dock-side-zone dock-right",
        DockPosition::Bottom => "",
    };
    let panel = if dock.panel_mode {
        " dock-panel-zone"
    } else {
        ""
    };
    format!("desktop-dock-zone{position}{panel}")
}

pub(crate) fn panel_mode(s: &State) -> bool {
    appearance(s).dock.panel_mode
}

pub(crate) fn floating_dock_status(s: &State) -> bool {
    let dock = &appearance(s).dock;
    !dock.panel_mode && dock.floating_status
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Zone {
    Desktop,
    Dock,
}

#[derive(Clone)]
struct Menu {
    zone: Zone,
    item: Option<Shortcut>,
    x: i32,
    y: i32,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Gesture {
    DockOverview {
        epoch: u64,
        app: DesktopApp,
    },
    RemoveFile {
        epoch: u64,
        name: String,
        permanent: bool,
    },
    OpenInTextEditor {
        window: u32,
        epoch: u64,
        name: String,
    },
    Context {
        epoch: u64,
        zone: Zone,
        item: Option<Shortcut>,
        x: i32,
        y: i32,
    },
    Dismiss {
        epoch: u64,
    },
    Reorder {
        epoch: u64,
        zone: Zone,
        item: Shortcut,
        anchor: Shortcut,
        after: bool,
    },
}

fn icons(s: &State, zone: Zone) -> Vec<Shortcut> {
    match zone {
        Zone::Desktop => desktop_icons(s),
        Zone::Dock => dock_icons(s),
    }
}

fn dock_item(shortcut: Shortcut) -> Option<DockItem> {
    match shortcut {
        Shortcut::App(app) => Some(DockItem::App(app)),
        Shortcut::System(system) => Some(DockItem::System { system }),
        Shortcut::File(_) => None,
    }
}

fn gesture(ui: &Ui, value: &str) {
    if value.len() > 4096 {
        return;
    }
    let Ok(gesture) = serde_json::from_str(value) else {
        return;
    };
    let epoch = match &gesture {
        Gesture::DockOverview { epoch, .. }
        | Gesture::RemoveFile { epoch, .. }
        | Gesture::OpenInTextEditor { epoch, .. }
        | Gesture::Context { epoch, .. }
        | Gesture::Dismiss { epoch }
        | Gesture::Reorder { epoch, .. } => *epoch,
    };
    if !ui
        .state
        .with(|s| s.epoch == epoch && server_desktop::visible(s) && s.dialog.is_none())
    {
        return;
    }
    match gesture {
        Gesture::DockOverview { app, .. } => ui.open_window_overview(app),
        Gesture::OpenInTextEditor { window, name, .. } => {
            let file = ui.state.with(|s| {
                find_file(s, &name)
                    .filter(|entry| {
                        text_editor::accepts_drop(s, window) && !entry.is_dir && !entry.is_symlink
                    })
                    .map(|entry| entry.path.clone())
            });
            if let Some(path) = file {
                ui.focus_desktop_app(text_editor::window_page(window));
                text_editor::open(ui, &path);
            }
        }
        Gesture::RemoveFile {
            name, permanent, ..
        } => {
            if let Some(file) = ui.state.with(|s| find_file(s, &name).cloned()) {
                trash_ui::desktop_file(ui, &file, permanent);
            }
        }
        Gesture::Dismiss { .. } => ui.close_desktop_menu(),
        Gesture::Context {
            zone, item, x, y, ..
        } => ui.state.revise(|s| {
            s.workbench.overview = None;
            let item = item.filter(|item| icons(s, zone).contains(item));
            s.server_apps.customization.menu = Some(Menu { zone, item, x, y });
        }),
        Gesture::Reorder {
            zone,
            item,
            anchor,
            after,
            ..
        } => {
            if item == anchor {
                return;
            }
            let mut icons = ui.state.with(|s| icons(s, zone));
            let Some(from) = icons.iter().position(|icon| *icon == item) else {
                return;
            };
            icons.remove(from);
            let Some(at) = icons.iter().position(|icon| *icon == anchor) else {
                return;
            };
            icons.insert(at + usize::from(after), item.clone());
            update(ui, |prefs| match zone {
                Zone::Desktop => {
                    let old = std::mem::take(&mut prefs.order);
                    let others = old
                        .into_iter()
                        .filter(|id| !icons.contains(id))
                        .collect::<Vec<_>>();
                    let remaining = desktop::MAX_ICONS - icons.len();
                    icons.extend(others.into_iter().take(remaining));
                    prefs.order = icons;
                }
                Zone::Dock => {
                    let Some(item) = dock_item(item) else {
                        return;
                    };
                    let dock = &mut prefs.dock;
                    let mut order: Vec<_> = icons
                        .into_iter()
                        .filter_map(dock_item)
                        .filter(|&entry| entry == item || ui.state.with(|s| dock_pinned(s, entry)))
                        .collect();
                    for &entry in &dock.order {
                        if !order.contains(&entry) {
                            order.push(entry);
                        }
                    }
                    dock.order = order;
                    if let DockItem::App(app) = item {
                        ui.state.with(|s| set_app_pin(s, dock, app, true));
                    }
                }
            });
        }
    }
}

pub(crate) fn gesture_input(ui: Ui) -> impl Widget {
    input()
        .class("desktop-customization-input hidden")
        .on(value_changed, move |event| {
            if let Event::ValueChanged { value, .. } = event {
                gesture(&ui, &value);
            }
        })
}

fn menu_action<F: FnMut(&Ui) + 'static>(
    ui: &Ui,
    label: impl Into<Vary<String>>,
    mut action: F,
) -> sapid::Button {
    let ui = ui.clone();
    sapid::button()
        .class("menu-item")
        .label(i18n::bind(label))
        .on_press(move || {
            ui.close_desktop_menu();
            action(&ui);
        })
}

fn menu_action_with_key(
    ui: &Ui,
    label: impl Into<Vary<String>>,
    shortcut: KeyAction,
    applies: impl Fn(&State) -> bool + 'static,
    action: impl FnMut(&Ui) + 'static,
) -> sapid::Button {
    let label = label.into();
    let title = i18n::bind(label.clone());
    menu_action(ui, label, action)
        .label("")
        .name(title.clone())
        .fill((
            text().content(title),
            text()
                .class("menu-key-hint")
                .content(ui.state.map(move |s| {
                    if applies(s) {
                        desktop_shortcuts_ui::menu_hint(s, shortcut)
                    } else {
                        String::new()
                    }
                })),
        ))
}

pub(crate) fn open_settings(ui: &Ui, page: SettingsPage) {
    ui.state.revise(|s| {
        s.server_apps.customization.settings_page = page;
        close_settings_search(s);
        s.server_apps.customization.menu = None;
    });
    ui.open_desktop_app(WorkspacePage::Settings);
}

fn dock_pin_action(ui: &Ui, item: DockItem) -> sapid::Button {
    menu_action(
        ui,
        ui.state.map(move |s| {
            if dock_pinned(s, item) {
                "Remove from dock"
            } else {
                "Keep in dock"
            }
            .to_owned()
        }),
        move |ui| toggle_dock_pin(ui, item),
    )
    .enabled(ui.state.map(server_apps_ui::preferences_ready))
}

fn system_icon_menu(ui: Ui, system: SystemIcon, zone: Zone) -> impl Widget {
    fragment((
        menu_action(&ui, "Open", move |ui| open_system_icon(ui, system)),
        dock_pin_action(&ui, DockItem::System { system }),
        if zone == Zone::Desktop {
            fragment(
                menu_action(&ui, "Hide desktop shortcut", move |ui| {
                    set_system_visible(ui, system, false);
                })
                .enabled(ui.state.map(server_apps_ui::preferences_ready)),
            )
        } else {
            fragment(())
        },
    ))
}

#[derive(Clone, PartialEq)]
struct DockDocument {
    page: WorkspacePage,
    tab: Option<u64>,
    label: String,
    selected: bool,
}

fn open_documents(s: &State, app: DesktopApp) -> Vec<DockDocument> {
    let mut entries = Vec::new();
    for &page in &s.workbench.apps {
        if server_desktop::app_id(page) != Some(app) {
            continue;
        }
        let window = page.tab_window().unwrap_or_default();
        let mut push = |tab, label: String, selected| {
            entries.push(DockDocument {
                page,
                tab,
                label,
                selected: selected && s.workspace_page == page && s.workbench.window_visible(page),
            })
        };
        match page.base() {
            WorkspacePage::PdfReader | WorkspacePage::Office => {
                let (tabs, selected) = if page.base() == WorkspacePage::Office {
                    (s.office.tabs(window), s.office.groups.selected(window))
                } else {
                    (s.pdf.tabs(window), s.pdf.groups.selected(window))
                };
                if tabs.is_empty() {
                    push(None, i18n::translate(server_desktop::app_label(page)), true);
                }
                for (id, path) in tabs {
                    push(
                        Some(id),
                        arcnix_core::file_associations::filename(&path).to_owned(),
                        selected == Some(id),
                    );
                }
            }
            WorkspacePage::Files(_) => {
                let groups = &s.workbench.files.groups;
                for &id in groups.tabs(window) {
                    push(
                        Some(id),
                        files_tabs::title(s, window, id),
                        groups.selected(window) == Some(id),
                    );
                }
            }
            WorkspacePage::Editor => {
                let groups = &s.desktop_editor.groups;
                for &id in groups.tabs(window) {
                    push(
                        Some(id),
                        s.desktop_editor.tab_title(id),
                        groups.selected(window) == Some(id),
                    );
                }
            }
            #[cfg(windows)]
            WorkspacePage::Browser => {
                let browser = &s.web_browser;
                for &id in browser.groups.tabs(window) {
                    if let Some(tab) = browser.tab(id) {
                        let title = if tab.title.is_empty() {
                            &tab.url
                        } else {
                            &tab.title
                        };
                        push(
                            Some(id),
                            title.clone(),
                            browser.groups.selected(window) == Some(id),
                        );
                    }
                }
            }
            WorkspacePage::ImageViewer | WorkspacePage::Player | WorkspacePage::Code => {
                if let Some(title) = server_desktop::window_title(s, page) {
                    push(None, title, true);
                }
            }
            _ => {}
        }
    }
    let mut counts = std::collections::BTreeMap::new();
    for entry in &entries {
        *counts.entry(entry.label.clone()).or_insert(0usize) += 1;
    }
    for entry in &mut entries {
        if counts[&entry.label] > 1 {
            let window = entry.page.tab_window().unwrap_or_default();
            let groups = match app {
                DesktopApp::Office => &s.office.groups,
                DesktopApp::PdfReader => &s.pdf.groups,
                DesktopApp::Files => &s.workbench.files.groups,
                DesktopApp::Editor => &s.desktop_editor.groups,
                #[cfg(windows)]
                DesktopApp::Browser => &s.web_browser.groups,
                _ => continue,
            };
            let window_name = i18n::translate("Window");
            entry
                .label
                .push_str(&format!(" · {window_name} {}", window + 1));
            if groups.tabs(window).len() > 1
                && let Some(index) = groups
                    .tabs(window)
                    .iter()
                    .position(|id| Some(*id) == entry.tab)
            {
                let tab_name = i18n::translate("Tab");
                entry
                    .label
                    .push_str(&format!(" · {tab_name} {}", index + 1));
            }
        }
    }
    entries
}

fn dock_documents(ui: Ui, app: DesktopApp) -> impl Widget {
    let state = ui.state;
    let entries = state.map(move |s| open_documents(s, app));
    when(state.map(move |_| !entries.get().is_empty()), move || {
        let ui = ui.clone();
        fragment((
            text().class("menu-divider").content(""),
            each(entries)
                .key(|entry| (entry.page.tab_window(), entry.tab))
                .children(move |item| {
                    let ui = ui.clone();
                    let DockDocument { page, tab, .. } = item.value.get();
                    sapid::button()
                        .class("menu-item dock-document")
                        .name(item.value.map(|entry| entry.label.clone()))
                        .fill((
                            text().class("dock-document-check").content(
                                item.value
                                    .map(|entry| if entry.selected { "✓" } else { "" }.to_owned()),
                            ),
                            text()
                                .class("dock-document-name")
                                .content(item.value.map(|entry| entry.label.clone())),
                        ))
                        .pressed(item.value.map(|entry| entry.selected))
                        .on_press(move || {
                            ui.close_desktop_menu();
                            ui.focus_desktop_app(page);
                            if let Some(id) = tab {
                                match page.base() {
                                    WorkspacePage::PdfReader => {
                                        ui.state.revise(|s| s.pdf.groups.select(id))
                                    }
                                    WorkspacePage::Office => {
                                        ui.state.revise(|s| s.office.groups.select(id))
                                    }
                                    WorkspacePage::Editor => {
                                        ui.state.revise(|s| s.desktop_editor.select(id))
                                    }
                                    WorkspacePage::Files(window) => {
                                        files_tabs::select(&ui, window, id)
                                    }
                                    #[cfg(windows)]
                                    WorkspacePage::Browser => {
                                        ui.state.revise(|s| s.web_browser.select(id))
                                    }
                                    _ => {}
                                }
                            }
                        })
                }),
        ))
    })
}

fn launcher_menu(ui: Ui, app: DesktopApp, zone: Zone) -> impl Widget {
    let state = ui.state;
    let page = server_desktop::app_page(app);
    let running = state.map(move |s| server_desktop::launcher_running(s, page));
    let files = page.is_files();
    let extra = match page {
        WorkspacePage::Files(_) => fragment(
            menu_action_with_key(
                &ui,
                "New window",
                KeyAction::OpenFiles,
                files_tabs::available,
                |ui| {
                    files_tabs::new_window(ui, files_tabs::WindowStart::Default);
                },
            )
            .enabled(state.map(files_tabs::available)),
        ),
        WorkspacePage::Terminal => fragment(when(running, {
            let ui = ui.clone();
            move || {
                menu_action(&ui, "New window", |ui| ui.terminal_new_window()).enabled(
                    state.map(|s| s.terminal.session_count() < terminal_worker::SESSION_LIMIT),
                )
            }
        })),
        #[cfg(windows)]
        WorkspacePage::Browser => fragment(when(running, {
            let ui = ui.clone();
            move || {
                menu_action(&ui, "New web tab", move |ui| {
                    ui.open_desktop_app(page);
                    ui.state.revise(|s| {
                        s.web_browser.add_tab(None);
                    });
                })
                .enabled(state.map(|s| s.web_browser.tabs.len() < web_browser::TAB_LIMIT))
            }
        })),
        WorkspacePage::Office => fragment(menu_action(&ui, "Open Office document", move |ui| {
            ui.open_desktop_app(page);
            office_reader::browse(ui);
        })),
        WorkspacePage::PdfReader => fragment(menu_action(&ui, "Open PDF", move |ui| {
            ui.open_desktop_app(page);
            pdf_reader::browse(ui);
        })),
        WorkspacePage::ImageViewer => fragment(menu_action(&ui, "Open image", move |ui| {
            ui.open_desktop_app(page);
            image_viewer::browse(ui);
        })),
        WorkspacePage::Player => fragment(menu_action(&ui, "Open video", move |ui| {
            ui.open_desktop_app(page);
            ui.player.browse(ui);
        })),
        _ => fragment(()),
    };
    fragment((
        menu_action(
            &ui,
            state.map(move |s| {
                if server_desktop::launcher_running(s, page) {
                    "Show window"
                } else {
                    "Open"
                }
                .to_owned()
            }),
            move |ui| ui.open_desktop_app(page),
        ),
        extra,
        when(matches!(app, DesktopApp::PdfReader | DesktopApp::Office), {
            let ui = ui.clone();
            move || {
                menu_action(&ui, "New window", move |ui| {
                    let id = ui.state.revise(|s| {
                        if !server_desktop::launcher_running(s, page) {
                            0
                        } else if app == DesktopApp::Office {
                            s.office.groups.new_window()
                        } else {
                            s.pdf.groups.new_window()
                        }
                    });
                    ui.focus_desktop_app(window_tabs::page(app, id));
                })
            }
        }),
        if zone == Zone::Dock {
            fragment(dock_documents(ui.clone(), app))
        } else {
            fragment(())
        },
        when(files, {
            let ui = ui.clone();
            move || {
                let open = ui.clone();
                fragment((
                    text().class("menu-divider").content(""),
                    each(state.map(|s| s.server_apps.files_locations.clone().unwrap_or_default()))
                        .key(|location| location.name)
                        .children(move |item| {
                            let location = item.value.get();
                            menu_action(&open, location.name, move |ui| {
                                files_tabs::new_window(
                                    ui,
                                    files_tabs::WindowStart::Directory(location.path.clone()),
                                );
                            })
                            .enabled(state.map(files_tabs::available))
                        }),
                    menu_action(&ui, "Filesystem", |ui| {
                        files_tabs::new_window(ui, files_tabs::WindowStart::Directory("/".into()));
                    })
                    .enabled(state.map(files_tabs::available)),
                ))
            }
        }),
        text().class("menu-divider").content(""),
        dock_pin_action(&ui, DockItem::App(app)),
        if zone == Zone::Desktop {
            fragment(
                menu_action(&ui, "Hide desktop shortcut", move |ui| {
                    set_shortcut_visible(ui, app, false);
                })
                .enabled(state.map(server_apps_ui::preferences_ready)),
            )
        } else if page == WorkspacePage::Settings {
            fragment(menu_action(&ui, "Dock settings", |ui| {
                open_settings(ui, SettingsPage::Dock)
            }))
        } else {
            fragment(())
        },
        when(running, move || {
            fragment((
                text().class("menu-divider").content(""),
                menu_action_with_key(
                    &ui,
                    if files
                        || matches!(
                            app,
                            DesktopApp::Office | DesktopApp::PdfReader | DesktopApp::Terminal
                        )
                    {
                        "Close window"
                    } else {
                        "Close application"
                    },
                    KeyAction::CloseWindow,
                    move |s| server_desktop::app_id(s.workspace_page) == Some(app),
                    move |ui| ui.close_desktop_launcher(page),
                )
                .enabled(state.map(move |s| server_desktop::can_close_app(s, page))),
            ))
        }),
    ))
}

pub(crate) fn context_menu(ui: Ui) -> impl Widget {
    let state = ui.state;
    when(
        state
            .map(|s| runtime(s).menu.is_some() && server_desktop::visible(s) && s.dialog.is_none()),
        move || {
            let close = ui.clone();
            let coordinates = text()
                .class("desktop-menu-position hidden")
                .content(state.map(|s| {
                    runtime(s)
                        .menu
                        .as_ref()
                        .map(|menu| format!("{},{}", menu.x, menu.y))
                        .unwrap_or_default()
                }));
            let entries = each(state.map(|s| {
                runtime(s)
                    .menu
                    .as_ref()
                    .map(|menu| (menu.zone, menu.item.clone()))
                    .into_iter()
                    .collect::<Vec<_>>()
            }))
            .key(|(zone, item)| serde_json::to_string(&(matches!(zone, Zone::Dock), item)).unwrap())
            .children({
                let ui = ui.clone();
                move |key| {
                    let (zone, item) = key.value.get();
                    let dock = zone == Zone::Dock;
                    let file = match item {
                        Some(Shortcut::App(app)) => {
                            return fragment(launcher_menu(ui.clone(), app, zone));
                        }
                        Some(Shortcut::File(name)) => Some(name),
                        Some(Shortcut::System(system)) => {
                            return fragment(system_icon_menu(ui.clone(), system, zone));
                        }
                        None => None,
                    };
                    let directory = file.as_deref().and_then(|name| {
                        state.with(|s| {
                            find_file(s, name)
                                .filter(|entry| {
                                    entry.is_dir && terminal::directory_supported(&entry.path)
                                })
                                .map(|entry| entry.path.clone())
                        })
                    });
                    fragment((
                        file_open::menu(
                            ui.clone(),
                            {
                                let file = file.clone();
                                move |s| {
                                    let entry = find_file(s, file.as_ref()?)?;
                                    (!dock && !entry.is_dir && !entry.is_symlink)
                                        .then(|| entry.path.clone())
                                }
                            },
                            Ui::close_desktop_menu,
                            false,
                        ),
                        when(
                            !dock && file.is_some() && state.with(|s| s.local_desktop.is_none()),
                            {
                                let ui = ui.clone();
                                let name = file.clone().unwrap_or_default();
                                move || {
                                    let buttons = [("移入回收站", false), ("永久删除", true)]
                                        .into_iter()
                                        .map(|(label, permanent)| {
                                            let name = name.clone();
                                            menu_action(&ui, label, move |ui| {
                                                if let Some(file) =
                                                    ui.state.with(|s| find_file(s, &name).cloned())
                                                {
                                                    trash_ui::desktop_file(ui, &file, permanent);
                                                }
                                            })
                                        })
                                        .collect::<Vec<_>>();
                                    fragment(buttons)
                                }
                            },
                        ),
                        when(directory.is_some(), {
                            let ui = ui.clone();
                            move || {
                                let path = directory.clone();
                                menu_action(&ui, "在此处打开终端", move |ui| {
                                    if let Some(path) = &path {
                                        ui.terminal_here(path.clone());
                                    }
                                })
                            }
                        }),
                        files_ui::appearance_menu(
                            ui.clone(),
                            {
                                let file = file.clone();
                                move |s| {
                                    let entry = find_file(s, file.as_ref()?)?;
                                    (!dock && !entry.is_symlink)
                                        .then(|| (entry.path.clone(), entry.is_dir))
                                }
                            },
                            Ui::close_desktop_menu,
                            false,
                        ),
                        when(!dock, {
                            let ui = ui.clone();
                            move || {
                                fragment((
                                    menu_action(&ui, "Desktop settings", |ui| {
                                        open_settings(ui, SettingsPage::Desktop)
                                    }),
                                    menu_action(&ui, "Refresh desktop", |ui| {
                                        ui.refresh_desktop_items(true)
                                    }),
                                    when(
                                        state.map(|s| {
                                            runtime(s)
                                                .items
                                                .as_ref()
                                                .is_some_and(|items| items.exists)
                                        }),
                                        {
                                            let ui = ui.clone();
                                            move || {
                                                menu_action(&ui, "Open Desktop folder", move |ui| {
                                                    if let Some(path) = ui.state.with(|s| {
                                                        runtime(s)
                                                            .items
                                                            .as_ref()
                                                            .map(|items| items.path.clone())
                                                    }) {
                                                        ui.open_desktop_app(WorkspacePage::Files(
                                                            0,
                                                        ));
                                                        file_browser::navigate(ui, true, path);
                                                    }
                                                })
                                            }
                                        },
                                    ),
                                ))
                            }
                        }),
                        when(dock, {
                            let ui = ui.clone();
                            move || {
                                menu_action_with_key(
                                    &ui,
                                    state.map(|s| server_desktop::show_desktop_label(s).to_owned()),
                                    KeyAction::ShowDesktop,
                                    |_| true,
                                    Ui::toggle_show_desktop,
                                )
                            }
                        }),
                    ))
                }
            });
            fragment((
                sapid::button()
                    .class("desktop-menu-backdrop")
                    .name(i18n::bind("Close menu"))
                    .on_press(move || close.close_desktop_menu()),
                col().class("pane-menu desktop-context-menu").fill((
                    coordinates,
                    col().class("desktop-context-items").fill(entries),
                )),
            ))
        },
    )
}

const SETTINGS_SECTIONS: [(SettingsPage, &str, Icon, &[&str]); 10] = [
    (
        SettingsPage::General,
        "General",
        Icon::Settings,
        &[
            "Resource refresh interval",
            "Server desktop",
            "Detect applications",
            "Device",
            "CPU model",
            "Physical cores",
            "Logical processors",
            "Memory",
            "GPU model",
            "System information",
            "Operating system",
            "OS version",
            "Architecture",
            "Kernel version",
        ],
    ),
    (
        SettingsPage::Network,
        "Network",
        Icon::Server,
        &["Network interfaces", "DNS", "Proxy"],
    ),
    (
        SettingsPage::Desktop,
        "Desktop",
        Icon::Grid,
        &[
            "Desktop layout",
            "Desktop icon size",
            "System app icons",
            "Show server info",
        ],
    ),
    (
        SettingsPage::Dock,
        "Dock",
        Icon::Sidebar,
        &[
            "Dock layout",
            "App window overview",
            "Click the active app",
            "Launcher",
            "Show locations",
            "Show power menu",
            "Dock position",
            "Dock style",
            "Status bar",
            "Dock icons",
            "Dock icon size",
            "App icon alignment",
            "Automatically hide the dock",
            "Dock clock",
            "Time format",
            "Date",
            "Seconds",
            "System app icons",
        ],
    ),
    (
        SettingsPage::Appearance,
        "Appearance",
        Icon::File,
        &[
            "Themes",
            "Wallpaper",
            "Accent color",
            "Icon family",
            "Tile background",
            "Light",
            "Dark",
        ],
    ),
    (
        SettingsPage::Region,
        "Region & language",
        Icon::Browser,
        &["Desktop language", "Numbers and file sizes"],
    ),
    (
        SettingsPage::DateTime,
        "Date & time",
        Icon::Monitor,
        &["Time zone", "Time format"],
    ),
    (
        SettingsPage::DefaultApps,
        "Default apps",
        Icon::Folder,
        &["File extensions", "File associations"],
    ),
    (
        SettingsPage::Shortcuts,
        "Keyboard shortcuts",
        Icon::Terminal,
        &["Key bindings"],
    ),
    (
        SettingsPage::About,
        "About",
        Icon::Info,
        &["Version", "License", "Website"],
    ),
];

pub(crate) fn settings_section_label(s: &State) -> String {
    SETTINGS_SECTIONS
        .iter()
        .find(|(page, ..)| *page == settings_page(s))
        .map_or("", |(_, label, ..)| *label)
        .to_owned()
}

fn settings_sections(s: &State) -> Vec<(SettingsPage, &'static str, Icon)> {
    let query = runtime(s)
        .settings_search
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    let language = desktop_system_settings::language(s);
    SETTINGS_SECTIONS
        .into_iter()
        .filter(|(page, ..)| {
            s.local_desktop.is_none()
                || !matches!(page, SettingsPage::Network | SettingsPage::DefaultApps)
        })
        .filter(|(_, label, _, terms)| {
            query.split_whitespace().all(|word| {
                std::iter::once(label).chain(terms.iter()).any(|term| {
                    term.to_lowercase().contains(word)
                        || crate::i18n::translate_for(term, language)
                            .to_lowercase()
                            .contains(word)
                })
            })
        })
        .map(|(page, label, icon, _)| (page, label, icon))
        .collect()
}

pub(crate) fn close_settings_search(s: &mut State) {
    s.server_apps.customization.settings_search = None;
}

pub(crate) fn settings_search_toggle(ui: Ui) -> impl Widget {
    let state = ui.state;
    sapid::button()
        .class("icon-button desktop-settings-search-toggle")
        .name(i18n::bind("Search settings"))
        .fill(icons::glyph(Icon::Search))
        .pressed(state.map(|s| runtime(s).settings_search.is_some()))
        .on_press(move || {
            state.revise(|s| {
                let query = &mut s.server_apps.customization.settings_search;
                *query = query.is_none().then(String::new);
            })
        })
}

pub(crate) fn settings_tabs(ui: Ui) -> impl Widget {
    let state = ui.state;
    let sections = state.map(settings_sections);
    col().class("desktop-settings-sidebar").fill((
        when(
            state.map(|s| runtime(s).settings_search.is_some()),
            move || {
                widget(move |scope| {
                    let field = scope.cage(None);
                    let focus = scope.clone();
                    let mounted = scope.mount_with(|| {
                        row().class("desktop-settings-search").fill((
                            icons::glyph(Icon::Search),
                            input()
                                .name(i18n::bind("Search settings"))
                                .node_ref(field)
                                .value(state.map(|s| {
                                    runtime(s).settings_search.clone().unwrap_or_default()
                                }))
                                .on(value_changed, move |event| {
                                    if let Event::ValueChanged { value, .. } = event {
                                        state.revise(|s| {
                                            if let Some(query) =
                                                &mut s.server_apps.customization.settings_search
                                            {
                                                *query = value;
                                            }
                                        });
                                    }
                                }),
                            when(
                                state.map(|s| {
                                    runtime(s)
                                        .settings_search
                                        .as_ref()
                                        .is_some_and(|query| !query.is_empty())
                                }),
                                move || {
                                    let focus = focus.clone();
                                    sapid::button()
                                        .class("desktop-settings-search-clear")
                                        .name(i18n::bind("Clear search"))
                                        .fill(icons::glyph(Icon::Close))
                                        .on_press(move || {
                                            state.revise(|s| {
                                                if let Some(query) =
                                                    &mut s.server_apps.customization.settings_search
                                                {
                                                    query.clear();
                                                }
                                            });
                                            if let Some(id) = field.get() {
                                                let _ =
                                                    focus.action(glory::render::Action::Focus(id));
                                            }
                                        })
                                },
                            ),
                        ))
                    })?;
                    scope.retain(mounted)?;
                    if let Some(id) = field.get() {
                        scope.action(glory::render::Action::Focus(id))?;
                    }
                    Ok(())
                })
            },
        ),
        col()
            .class("desktop-settings-tabs")
            .name(i18n::bind("Settings sections"))
            .fill((
                each(sections)
                    .key(|(page, ..)| *page as u8)
                    .children(move |item| {
                        let (page, label, icon) = item.value.get();
                        let choose = ui.clone();
                        sapid::button()
                            .class("desktop-settings-section")
                            .name(i18n::bind(label))
                            .fill((icons::glyph(icon), text().content(i18n::bind(label))))
                            .pressed(state.map(move |s| settings_page(s) == page))
                            .on_press(move || {
                                state.revise(|s| {
                                    s.server_apps.customization.settings_page = page;
                                });
                                network_ui::enter(&choose);
                                device_info_ui::enter(&choose);
                                if page == SettingsPage::DateTime {
                                    choose.read_desktop_time_zone();
                                    choose.tick_desktop_clock();
                                }
                            })
                    }),
                when(state.map(|s| settings_sections(s).is_empty()), || {
                    text()
                        .class("desktop-settings-search-empty")
                        .content(i18n::bind("No matching settings"))
                }),
            )),
    ))
}

fn toggle(
    ui: &Ui,
    label: &'static str,
    selected: fn(&Appearance) -> bool,
    change: fn(&mut Appearance, bool),
) -> impl Widget + use<> {
    let ui = ui.clone();
    sapid::switch()
        .size(sapid::Size::Small)
        .name(i18n::bind(label))
        .checked(ui.state.map(move |s| selected(appearance(s))))
        .enabled(ui.state.map(server_apps_ui::preferences_ready))
        .on_checked_change(move |checked| update(&ui, |prefs| change(prefs, checked)))
}

fn settings_dialog_link(ui: Ui, title: &'static str, dialog: Dialog) -> impl Widget {
    let label = i18n::bind(title);
    row().class("desktop-setting-link-row").fill(
        sapid::button()
            .class("desktop-setting-link")
            .name(label.clone())
            .enabled(ui.state.map(server_apps_ui::preferences_ready))
            .fill((
                text().class("desktop-setting-label").content(label),
                icons::glyph(Icon::ChevronRight),
            ))
            .on_press(move || ui.open_dialog(dialog.clone())),
    )
}

pub(crate) fn shortcut_settings_dialog(ui: Ui) -> impl Widget {
    let state = ui.state;
    when(
        state.map(|s| matches!(s.dialog, Some(Dialog::DesktopIcons | Dialog::DockShortcuts))),
        move || {
            let dock = state.with(|s| s.dialog == Some(Dialog::DockShortcuts));
            let change = ui.clone();
            modal_with_header(
                ui.clone(),
                "System app icons",
                settings::desktop_reset(&ui, move |prefs| {
                    let appearance = &mut prefs.appearance;
                    if dock {
                        reset_system_pins(&mut appearance.dock);
                        appearance
                            .dock
                            .order
                            .retain(|item| matches!(item, DockItem::App(_)));
                    } else {
                        reset_desktop_app_icons(appearance);
                    }
                }),
                col().class("desktop-shortcut-options").fill(
                    each(state.map(move |s| {
                        let choices = available_shortcuts(s)
                            .into_iter()
                            .filter(|item| !matches!(item, DockItem::App(app) if !system_app(*app)))
                            .collect();
                        ordered(
                            choices,
                            &[DockItem::System {
                                system: SystemIcon::Home,
                            }],
                        )
                    }))
                    .key(|item| *item)
                    .children(move |item| {
                        let item = item.value.get();
                        let (label, glyph) = match item {
                            DockItem::App(app) => {
                                let page = server_desktop::app_page(app);
                                (
                                    server_desktop::app_label(page),
                                    server_desktop::app_icon(page),
                                )
                            }
                            DockItem::System { system } => {
                                let (label, glyph, _) = system_icon(system);
                                (label, glyph)
                            }
                        };
                        let label = i18n::bind(label);
                        let change = change.clone();
                        row().class("desktop-shortcut-option").fill((
                            icons::glyph(glyph),
                            sapid::checkbox()
                                .class("preference-check")
                                .name(label.clone())
                                .label(label)
                                .checked(state.map(move |s| {
                                    let prefs = appearance(s);
                                    if dock {
                                        dock_pinned(s, item)
                                    } else {
                                        match item {
                                            DockItem::App(app) => shortcut_visible(s, app),
                                            DockItem::System { system } => {
                                                system_visible(prefs, system)
                                            }
                                        }
                                    }
                                }))
                                .enabled(state.map(server_apps_ui::preferences_ready))
                                .on_checked_change(move |visible| {
                                    if dock {
                                        if state.with(|s| dock_pinned(s, item)) != visible {
                                            toggle_dock_pin(&change, item);
                                        }
                                    } else {
                                        match item {
                                            DockItem::App(app) => {
                                                set_shortcut_visible(&change, app, visible)
                                            }
                                            DockItem::System { system } => {
                                                set_system_visible(&change, system, visible)
                                            }
                                        }
                                    }
                                }),
                        ))
                    }),
                ),
                (),
            )
        },
    )
}

fn reset_system_pins(dock: &mut Dock) {
    dock.unpinned.retain(|&app| !system_app(app));
    dock.extra_pins.retain(|&app| !system_app(app));
}

fn reset_desktop_app_icons(prefs: &mut Appearance) {
    let defaults = Appearance::default();
    prefs.hidden.retain(|&app| !system_app(app));
    prefs
        .extra_shortcuts
        .retain(|&app| prefs.show_apps && !system_app(app));
    prefs.show_apps = defaults.show_apps;
    prefs.layout.show_home = defaults.layout.show_home;
    prefs.layout.show_trash = defaults.layout.show_trash;
}

pub(crate) fn settings_row(
    title: &'static str,
    controls: impl glory::ui::IntoChildren,
) -> impl Widget {
    row().class("desktop-setting-row").fill((
        text()
            .class("desktop-setting-label")
            .content(i18n::bind(title)),
        row()
            .class("desktop-setting-controls choice-list")
            .fill(controls),
    ))
}

fn choices<T: Copy + PartialEq + 'static>(
    ui: &Ui,
    title: &'static str,
    options: &[(T, &'static str)],
    read: fn(&Appearance) -> T,
    write: fn(&mut Appearance, T),
) -> impl Widget + use<T> {
    settings::select(
        ui,
        true,
        title,
        options.to_vec(),
        move |s| read(appearance(s)),
        server_apps_ui::preferences_ready,
        move |ui, value| update(ui, |prefs| write(prefs, value)),
    )
}

pub(crate) fn layout_controls(ui: Ui) -> impl Widget {
    col()
        .class("desktop-settings-card desktop-settings-rows")
        .fill((
            settings::section_heading(
                "Desktop layout",
                true,
                settings::desktop_reset(&ui, |prefs| {
                    let defaults = Layout::default();
                    prefs.appearance.layout.flow = defaults.flow;
                    prefs.appearance.layout.direction = defaults.direction;
                }),
            ),
            settings_row(
                "Icon arrangement",
                choices(
                    &ui,
                    "Icon arrangement",
                    &[
                        (IconFlow::Columns, "Vertical"),
                        (IconFlow::Rows, "Horizontal"),
                    ],
                    |prefs| prefs.layout.flow,
                    |prefs, value| prefs.layout.flow = value,
                ),
            ),
            settings_row(
                "Starting edge",
                choices(
                    &ui,
                    "Starting edge",
                    &[
                        (IconDirection::LeftToRight, "Left to right"),
                        (IconDirection::RightToLeft, "Right to left"),
                    ],
                    |prefs| prefs.layout.direction,
                    |prefs, value| prefs.layout.direction = value,
                ),
            ),
        ))
}

pub(crate) fn icon_settings(ui: Ui) -> impl Widget {
    col().class("desktop-settings-card").fill((
        settings::section_heading(
            "Desktop icons",
            true,
            settings::desktop_reset(&ui, |prefs| {
                let appearance = &mut prefs.appearance;
                let defaults = Appearance::default();
                reset_desktop_app_icons(appearance);
                appearance.show_server = defaults.show_server;
                appearance.order = defaults.order;
                appearance.layout.size = defaults.layout.size;
                appearance.layout.icon_size = defaults.layout.icon_size;
            }),
        ),
        settings_row(
            "Desktop icon size",
            icon_size_control(
                &ui,
                "Desktop icon size",
                "icon-size-control desktop-size-control",
                |prefs| prefs.layout.icon_pixels(),
                |prefs, value| prefs.layout.set_icon_pixels(value),
            ),
        ),
        settings_dialog_link(ui.clone(), "System app icons", Dialog::DesktopIcons),
        settings_row(
            "Show server info",
            toggle(
                &ui,
                "Show server info",
                |p| p.show_server,
                |p, show| p.show_server = show,
            ),
        ),
        text().class("subtle").content(i18n::bind(
            "Drag from Files. Ctrl copies, Shift moves, and Alt or Ctrl+Shift creates a shortcut.",
        )),
    ))
}

pub(crate) fn icon_grid_class(s: &State) -> String {
    let layout = appearance(s).layout;
    let flow = match layout.flow {
        IconFlow::Columns => "",
        IconFlow::Rows => " icons-by-row",
    };
    let direction = match layout.direction {
        IconDirection::LeftToRight => "",
        IconDirection::RightToLeft => " icons-from-right",
    };
    let pixels = layout.icon_pixels();
    format!("desktop-shortcuts{flow}{direction} icons-size-{pixels}")
}

fn icon_size_control(
    ui: &Ui,
    label: &'static str,
    class: &'static str,
    read: fn(&Appearance) -> u8,
    write: fn(&mut Appearance, u8),
) -> impl Widget + use<> {
    let state = ui.state;
    let resize_icons = ui.clone();
    col().class("icon-size-options").fill((
        row().class(class).fill((
            input()
                .class("icon-size-bridge hidden")
                .name(i18n::bind(label))
                .enabled(state.map(server_apps_ui::preferences_ready))
                .on(value_changed, move |event| {
                    if let Event::ValueChanged { value, .. } = event
                        && let Ok(pixels) = value.parse::<u8>()
                        && desktop::ICON_SIZE_RANGE.contains(&pixels)
                    {
                        update(&resize_icons, |prefs| write(prefs, pixels));
                    }
                }),
            text().class("icon-size-state hidden").content(
                state.map(move |s| {
                    format!("{MIN_ICON_SIZE},{MAX_ICON_SIZE},{}", read(appearance(s)))
                }),
            ),
        )),
        choices(
            ui,
            label,
            &[
                (MIN_ICON_SIZE, "Small"),
                (MEDIUM_ICON_SIZE, "Medium"),
                (MAX_ICON_SIZE, "Large"),
            ],
            read,
            write,
        ),
    ))
}

pub(crate) fn dock_settings(ui: Ui) -> impl Widget {
    col().class("desktop-settings-pane").fill((
        dock_layout(ui.clone()),
        dock_overview_settings(ui.clone()),
        dock_icons_settings(ui.clone()),
        dock_status_settings(ui.clone()),
        col()
            .class("desktop-settings-card desktop-settings-rows")
            .fill((
                settings::section_heading(
                    "Launcher",
                    true,
                    settings::desktop_reset(&ui, |prefs| {
                        let defaults = Dock::default();
                        prefs.appearance.dock.launcher_locations = defaults.launcher_locations;
                        prefs.appearance.dock.launcher_power = defaults.launcher_power;
                    }),
                ),
                settings_row(
                    "Show locations",
                    toggle(
                        &ui,
                        "Show locations",
                        |p| p.dock.launcher_locations,
                        |p, show| p.dock.launcher_locations = show,
                    ),
                ),
                when(ui.state.map(desktop_power::supported), move || {
                    settings_row(
                        "Show power menu",
                        toggle(
                            &ui,
                            "Show power menu",
                            |p| p.dock.launcher_power,
                            |p, show| p.dock.launcher_power = show,
                        ),
                    )
                }),
            )),
    ))
}

fn dock_layout(ui: Ui) -> impl Widget {
    col()
        .class("desktop-settings-card desktop-settings-rows dock-layout-settings")
        .fill((
            settings::section_heading(
                "Dock layout",
                true,
                settings::desktop_reset(&ui, |prefs| {
                    let defaults = Dock::default();
                    let dock = &mut prefs.appearance.dock;
                    dock.position = defaults.position;
                    dock.panel_mode = defaults.panel_mode;
                    dock.auto_hide = defaults.auto_hide;
                }),
            ),
            settings_row(
                "Dock position",
                choices(
                    &ui,
                    "Dock position",
                    &[
                        (DockPosition::Left, "Left side"),
                        (DockPosition::Right, "Right side"),
                        (DockPosition::Bottom, "Bottom"),
                    ],
                    |prefs| prefs.dock.position,
                    |prefs, value| prefs.dock.position = value,
                ),
            ),
            settings_row(
                "Dock style",
                choices(
                    &ui,
                    "Dock style",
                    &[(false, "Floating dock"), (true, "Panel mode")],
                    |prefs| prefs.dock.panel_mode,
                    |prefs, value| prefs.dock.panel_mode = value,
                ),
            ),
            settings_row(
                "Automatically hide the dock",
                toggle(
                    &ui,
                    "Automatically hide the dock",
                    |p| p.dock.auto_hide,
                    |p, hide| p.dock.auto_hide = hide,
                ),
            ),
        ))
}

fn dock_overview_settings(ui: Ui) -> impl Widget {
    col()
        .class("desktop-settings-card desktop-settings-rows")
        .fill((
            settings::section_heading(
                "App window overview",
                true,
                settings::desktop_reset(&ui, |prefs| {
                    let defaults = Dock::default();
                    prefs.appearance.dock.overview_active_click = defaults.overview_active_click;
                }),
            ),
            settings_row(
                "Click the active app",
                toggle(
                    &ui,
                    "Click the active app",
                    |p| p.dock.overview_active_click,
                    |p, value| p.dock.overview_active_click = value,
                ),
            ),
        ))
}

fn dock_icons_settings(ui: Ui) -> impl Widget {
    let alignment = ui.clone();
    let state = ui.state;
    col()
        .class("desktop-settings-card desktop-settings-rows dock-icons-settings")
        .fill((
            settings::section_heading(
                "Dock icons",
                true,
                settings::desktop_reset(&ui, |prefs| {
                    let defaults = Dock::default();
                    let dock = &mut prefs.appearance.dock;
                    dock.order = defaults.order;
                    reset_system_pins(dock);
                    dock.size = defaults.size;
                    dock.icon_size = defaults.icon_size;
                    dock.panel_centered = defaults.panel_centered;
                }),
            ),
            when(state.map(panel_mode), move || {
                settings_row(
                    "App icon alignment",
                    each(state.map(|s| vec![appearance(s).dock.position == DockPosition::Bottom]))
                        .key(|bottom| *bottom)
                        .children({
                            let alignment = alignment.clone();
                            move |item| {
                                let edge = if item.value.get() {
                                    "Align left"
                                } else {
                                    "Align top"
                                };
                                settings::select(
                                    &alignment,
                                    true,
                                    "App icon alignment",
                                    [(false, edge), (true, "Center icons")],
                                    |s| appearance(s).dock.panel_centered,
                                    server_apps_ui::preferences_ready,
                                    |ui, centered| {
                                        update(ui, |prefs| prefs.dock.panel_centered = centered)
                                    },
                                )
                            }
                        }),
                )
            }),
            settings_row(
                "Dock icon size",
                icon_size_control(
                    &ui,
                    "Dock icon size",
                    "icon-size-control dock-size-control",
                    |prefs| prefs.dock.icon_pixels(),
                    |prefs, value| prefs.dock.set_icon_pixels(value),
                ),
            ),
            settings_dialog_link(ui, "System app icons", Dialog::DockShortcuts),
        ))
}

fn dock_status_settings(ui: Ui) -> impl Widget {
    let state = ui.state;
    let status = ui.clone();
    col()
        .class("desktop-settings-card desktop-settings-rows dock-status-settings")
        .fill((
            settings::section_heading(
                "Status bar",
                true,
                settings::desktop_reset(&ui, |prefs| {
                    prefs.appearance.dock.floating_status = Dock::default().floating_status;
                    prefs.clock = Default::default();
                }),
            ),
            when(state.map(|s| !panel_mode(s)), move || {
                settings_row(
                    "Show dock status bar",
                    toggle(
                        &status,
                        "Show dock status bar",
                        |p| p.dock.floating_status,
                        |p, show| p.dock.floating_status = show,
                    ),
                )
            }),
            when(
                state.map(|s| panel_mode(s) || floating_dock_status(s)),
                move || desktop_system_settings::dock_clock_settings(ui.clone()),
            ),
        ))
}

pub(crate) fn wallpaper_controls(ui: Ui) -> impl Widget {
    let state = ui.state;
    let reset = ui.clone();
    let ready = state.map(server_apps_ui::preferences_ready);
    col()
        .class("desktop-settings-card wallpaper-settings")
        .role(Role::Group)
        .name(i18n::bind("Wallpaper"))
        .fill((
            settings::section_heading(
                "Wallpaper",
                true,
                settings::reset_button(true, move || {
                    desktop_appearance_ui::set_background(&reset, None)
                })
                .enabled(ready),
            ),
            row().class("wallpaper-choices").fill(
                BUILT_IN_WALLPAPERS
                    .into_iter()
                    .map(|(wallpaper, label, class)| {
                        let choose = ui.clone();
                        let selected = wallpaper.clone();
                        sapid::button()
                            .class("button wallpaper-choice")
                            .name(i18n::bind(format!("{label} wallpaper")))
                            .pressed(
                                state
                                    .map(move |s| desktop_appearance_ui::background(s) == selected),
                            )
                            .enabled(ready)
                            .fill((
                                text()
                                    .class(format!("wallpaper-swatch {class}"))
                                    .content(""),
                                text().class("wallpaper-name").content(i18n::bind(label)),
                            ))
                            .on_press(move || {
                                desktop_appearance_ui::set_background(
                                    &choose,
                                    Some(wallpaper.clone()),
                                )
                            })
                    })
                    .collect::<Vec<_>>(),
            ),
        ))
}
