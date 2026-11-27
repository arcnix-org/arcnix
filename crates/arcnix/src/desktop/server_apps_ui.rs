use std::time::{Duration, Instant};

use arcnix_core::server_apps::{
    Applications, ClockSample, DesktopApp, DesktopPreferences, DesktopWindow,
};
use arcnix_core::sync::{self, DesktopResult, Pending};

use crate::i18n::desktop as i18n;
use crate::i18n::desktop::text_action;
use crate::server_apps_worker::{Action as AppAction, Reply};
use crate::*;

#[derive(Clone, Default)]
pub(crate) struct DesktopState {
    pub applications: Applications,
    #[cfg(windows)]
    pub cookies: browser_cookies::Runtime,
    pub clock: Option<(u64, ClockSample)>,
    clock_pending: Option<u64>,
    clock_checked_at: Option<Instant>,
    pub time_zone: desktop_system_settings::TimeZoneState,
    pub device: device_info_ui::Runtime,
    pub power_pending: Option<(u64, arcnix_core::host::PowerAction)>,
    pub connected_address: String,
    pub discover_pending: Option<u64>,
    pub discover_error: String,
    pub preferences: DesktopPreferences,
    pub preferences_loaded: bool,
    pub preferences_pending: Option<u64>,
    pub save_pending: Option<u64>,
    save_started: Option<Instant>,
    show_saving: bool,
    pub preferences_error: String,
    writing: Option<DesktopPreferences>,
    pending: Option<Pending<DesktopPreferences>>,
    conflict: Option<(DesktopPreferences, Vec<String>)>,
    sync_after: Option<Instant>,
    journal_error: bool,
    pub files_locations: Option<Vec<arcnix_core::remote::FileLocation>>,
    pub files_locations_pending: Option<u64>,
    pub files_tag: Option<files_ui::TagFilter>,
    pub files_open_pending: Option<files_ui::ItemRequest>,
    pub files_search: files_search::Search,
    pub files_cleanup: files_ui::Cleanup,
    pub customization: desktop_customization_ui::Runtime,
    pub network: network_ui::Runtime,
    pub processes: processes_ui::Runtime,
    pub shortcut_error: String,
    pub recording_shortcut: bool,
}

pub(crate) fn desktop_dark(s: &State) -> bool {
    desktop_appearance_ui::dark(s)
}

pub(crate) fn themed(s: &State, base: &str) -> String {
    if s.connected
        && s.navigation == navigation::Page::Servers
        && s.workbench.active == server_desktop::Tab::Desktop
    {
        format!(
            "{base} {}",
            if desktop_dark(s) {
                "theme-dark desktop-dark"
            } else {
                "theme-light desktop-light"
            }
        )
    } else {
        base.to_owned()
    }
}

impl Ui {
    pub(crate) fn server_apps_connected(&self) {
        self.discover_server_apps();
        self.read_desktop_time_zone();
        #[cfg(windows)]
        browser_cookies::tick(self);
        if let Some(path) = self.desktop_sync_path() {
            match sync::load_desktop_pending(&path) {
                Ok(pending) => self.state.revise(|s| s.server_apps.pending = pending),
                Err(error) => self.state.revise(|s| {
                    s.server_apps.journal_error = true;
                    s.server_apps.preferences_error = error.to_string();
                }),
            }
        }
        self.read_desktop_preferences();
        if self.state.with(server_desktop::visible) {
            self.refresh_desktop_items(false);
        }
    }
    pub(crate) fn tick_desktop_clock(&self) {
        let due = self.state.with(|s| {
            server_desktop::visible(s)
                && (desktop_customization_ui::panel_mode(s)
                    || desktop_customization_ui::floating_dock_status(s)
                    || (settings_visible(s)
                        && desktop_customization_ui::settings_page(s)
                            == desktop_customization_ui::SettingsPage::DateTime))
                && s.server_apps.clock_pending.is_none()
                && s.server_apps
                    .clock_checked_at
                    .is_none_or(|checked| checked.elapsed() >= Duration::from_secs(60))
        });
        if due {
            let serial = self.send(Action::ServerApps(AppAction::Clock));
            self.state
                .revise(|s| s.server_apps.clock_pending = Some(serial));
        }
    }
    pub(crate) fn refresh_desktop_clock(&self) {
        self.state.revise(|s| {
            s.server_apps.clock_checked_at = None;
            s.server_apps.clock_pending = None;
        });
        self.tick_desktop_clock();
    }
    pub(crate) fn discover_server_apps(&self) {
        if !self
            .state
            .with(|s| s.connected && s.server_apps.discover_pending.is_none())
        {
            return;
        }
        let serial = self.send(Action::ServerApps(AppAction::Discover));
        self.state.revise(|s| {
            s.server_apps.discover_pending = Some(serial);
            s.server_apps.discover_error.clear();
        });
    }
    fn read_desktop_preferences(&self) {
        if !self.state.with(|s| {
            s.connected
                && s.server_apps.preferences_pending.is_none()
                && s.server_apps.save_pending.is_none()
                && s.server_apps.conflict.is_none()
        }) {
            return;
        }
        let serial = self.send(Action::ServerApps(AppAction::ReadPreferences));
        self.state.revise(|s| {
            s.server_apps.preferences_pending = Some(serial);
            if !s.server_apps.journal_error {
                s.server_apps.preferences_error.clear();
            }
            s.server_apps.sync_after = Some(Instant::now() + Duration::from_secs(15));
        });
    }
    fn save_desktop_interval(&self, seconds: Option<u64>) {
        if self.state.with(preferences_ready) {
            let mut prefs = self.state.with(desired);
            prefs.resource_interval_seconds = seconds;
            self.queue_desktop_preferences(prefs);
        }
    }
    fn desktop_sync_path(&self) -> Option<PathBuf> {
        self.state.with(|s| {
            s.sync_scope.as_ref().map(|scope| {
                self.store
                    .path()
                    .with_file_name("sync-outbox")
                    .join(scope)
                    .join("desktop.json")
            })
        })
    }
    fn persist_desktop_sync(
        &self,
        pending: Option<&Pending<DesktopPreferences>>,
    ) -> anyhow::Result<()> {
        let path = self
            .desktop_sync_path()
            .ok_or_else(|| anyhow::anyhow!("Missing SSH sync identity"))?;
        if let Some(pending) = pending {
            sync::save_desktop_pending(&path, pending)
        } else {
            match std::fs::remove_file(path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
                _ => Ok(()),
            }
        }
    }
    pub(crate) fn tick_desktop_sync(&self) {
        let update = self.state.with(|s| {
            let show = slow_settings_save(s);
            (show != s.server_apps.show_saving).then_some(show)
        });
        if let Some(show) = update {
            self.state.revise(|s| s.server_apps.show_saving = show);
        }
        if !self.state.with(|s| {
            let desktop = &s.server_apps;
            s.connected
                && desktop.preferences_pending.is_none()
                && desktop.save_pending.is_none()
                && desktop.conflict.is_none()
                && desktop.sync_after.is_none_or(|due| Instant::now() >= due)
        }) {
            return;
        }
        if self
            .state
            .with(|s| s.server_apps.preferences_loaded && s.server_apps.pending.is_some())
        {
            self.send_desktop_sync();
        } else {
            self.read_desktop_preferences();
        }
    }
    fn send_desktop_sync(&self) {
        let pending = self.state.with(|s| {
            let desktop = &s.server_apps;
            if preferences_ready(s)
                && desktop.preferences_pending.is_none()
                && desktop.save_pending.is_none()
                && desktop.conflict.is_none()
            {
                desktop.pending.clone()
            } else {
                None
            }
        });
        if let Some(pending) = pending {
            let desired = pending.desired.clone();
            let mut journal = pending.clone();
            if journal.attempted.is_none() {
                journal.attempted = Some(desired.clone());
                if let Err(error) = self.persist_desktop_sync(Some(&journal)) {
                    self.state
                        .revise(|s| s.server_apps.preferences_error = error.to_string());
                    return;
                }
            }
            let serial = self.send(Action::ServerApps(AppAction::WritePreferences(Box::new(
                pending,
            ))));
            self.state.revise(|s| {
                s.server_apps.save_pending = Some(serial);
                s.server_apps.save_started = Some(Instant::now());
                s.server_apps.writing = Some(desired);
                s.server_apps.pending = Some(journal);
            });
        }
    }
    pub(crate) fn queue_desktop_preferences(&self, prefs: DesktopPreferences) {
        if !self.state.with(preferences_ready) {
            return;
        }
        let pending = self.state.with(|s| {
            let desktop = &s.server_apps;
            let base = desktop.pending.as_ref().map_or_else(
                || desktop.preferences.clone(),
                |pending| pending.base.clone(),
            );
            let attempted = desktop.writing.clone().or_else(|| {
                desktop
                    .pending
                    .as_ref()
                    .and_then(|pending| pending.attempted.clone())
            });
            Pending {
                base,
                desired: prefs,
                attempted,
            }
        });
        self.stage_desktop_sync(pending);
    }
    fn stage_desktop_sync(&self, pending: Pending<DesktopPreferences>) {
        if let Err(error) = self.persist_desktop_sync(Some(&pending)) {
            self.state
                .revise(|s| s.server_apps.preferences_error = error.to_string());
            return;
        }
        self.state.revise(|s| {
            s.server_apps.pending = Some(pending);
            s.server_apps.preferences_error.clear();
            s.server_apps.sync_after = None;
        });
        self.send_desktop_sync();
    }
    fn resolve_desktop_sync(&self, keep_local: bool) {
        let input = self.state.with(|s| {
            Some((
                s.server_apps.pending.clone()?,
                s.server_apps.conflict.clone()?.0,
            ))
        });
        if let Some((pending, remote)) = input {
            match sync::resolve_desktop(&pending, &remote, keep_local) {
                Ok((desired, _)) => {
                    let pending = Pending {
                        base: remote,
                        desired,
                        attempted: None,
                    };
                    match self.persist_desktop_sync(Some(&pending)) {
                        Ok(()) => {
                            self.state.revise(|s| {
                                s.server_apps.conflict = None;
                                s.server_apps.pending = Some(pending);
                                s.server_apps.preferences_error.clear();
                            });
                            self.send_desktop_sync();
                        }
                        Err(error) => self
                            .state
                            .revise(|s| s.server_apps.preferences_error = error.to_string()),
                    }
                }
                Err(error) => self
                    .state
                    .revise(|s| s.server_apps.preferences_error = error.to_string()),
            }
        }
    }
    pub(crate) fn remember_desktop_layout(&self) {
        let prefs = self.state.with(|s| {
            let mut prefs = desired(s);
            prefs.terminal = terminal::layout(s);
            prefs.windows = s.workbench.windows.clone();
            prefs
        });
        if self.state.with(|s| desired(s) != prefs) {
            self.queue_desktop_preferences(prefs);
        }
    }
}
pub(crate) fn desired(s: &State) -> DesktopPreferences {
    current(s).clone()
}

fn slow_settings_save(s: &State) -> bool {
    let desktop = &s.server_apps;
    [desktop.save_started, desktop.time_zone.write_started]
        .into_iter()
        .flatten()
        .any(|started| started.elapsed() >= Duration::from_secs(5))
}

pub(crate) fn settings_save_status(s: &State) -> String {
    if s.server_apps.show_saving && slow_settings_save(s) {
        "Saving…".into()
    } else {
        String::new()
    }
}
pub(crate) fn current(s: &State) -> &DesktopPreferences {
    s.server_apps
        .pending
        .as_ref()
        .map(|pending| &pending.desired)
        .unwrap_or(&s.server_apps.preferences)
}
pub(crate) fn preferences_ready(s: &State) -> bool {
    s.connected && s.server_apps.preferences_loaded && !s.server_apps.journal_error
}
pub(crate) fn resource_interval(s: &State) -> u64 {
    s.server_apps
        .preferences
        .resource_interval_seconds
        .unwrap_or(s.config.settings.resource_interval_seconds)
}

pub(crate) fn settings_visible(s: &State) -> bool {
    server_desktop::visible(s) && s.workspace_page == WorkspacePage::Settings
}
pub(crate) fn event(ui: &Ui, serial: u64, result: std::result::Result<Reply, String>) {
    if let Some((_, action)) = ui
        .state
        .with(|s| s.server_apps.power_pending.filter(|(id, _)| *id == serial))
    {
        ui.state.revise(|s| s.server_apps.power_pending = None);
        match result {
            Ok(Reply::Operated) => ui.log(i18n::translate(match action {
                arcnix_core::host::PowerAction::Restart => "System restart requested",
                arcnix_core::host::PowerAction::ShutDown => "System shutdown requested",
            })),
            Err(error) => ui.error(error),
            _ => ui.error(i18n::translate("Unexpected power response")),
        }
        return;
    }
    if ui.state.with(|s| s.images.request == Some(serial)) {
        image_viewer::received(ui, serial, result);
        return;
    }
    if desktop_file_picker::awaiting_directory(ui, serial) {
        desktop_file_picker::listed(
            ui,
            result.and_then(|reply| match reply {
                Reply::PickerEntries(entries) => Ok(entries),
                _ => Err("Invalid file picker response".into()),
            }),
        );
        return;
    }
    if ui
        .state
        .with(|s| s.server_apps.time_zone.pending == Some(serial))
    {
        desktop_system_settings::time_zone_reply(ui, result);
        return;
    }
    if ui
        .state
        .with(|s| s.server_apps.device.pending == Some(serial))
    {
        device_info_ui::received(ui, result);
        return;
    }
    #[cfg(windows)]
    if ui.state.with(|s| browser_cookies::pending(s, serial)) {
        browser_cookies::event(ui, result);
        return;
    }
    if ui
        .state
        .with(|s| s.server_apps.clock_pending == Some(serial))
    {
        ui.state.revise(|s| {
            s.server_apps.clock_pending = None;
            s.server_apps.clock_checked_at = Some(Instant::now());
            if let Ok(Reply::Clock(sample)) = result {
                s.server_apps.clock = Some((serial, sample));
            }
        });
        return;
    }
    if desktop_customization_ui::pending(ui, serial) {
        desktop_customization_ui::event(ui, result);
        return;
    }
    if files_ui::cleanup_pending(ui, serial) {
        files_ui::cleanup_completed(ui, serial, result);
        return;
    }
    if ui.state.with(|s| {
        s.server_apps
            .files_open_pending
            .as_ref()
            .is_some_and(|pending| pending.serial == serial)
    }) {
        files_ui::item_opened(ui, result);
        return;
    }
    let kind = ui.state.with(|s| {
        if s.server_apps.discover_pending == Some(serial) {
            0
        } else if s.server_apps.preferences_pending == Some(serial) {
            1
        } else if s.server_apps.save_pending == Some(serial) {
            2
        } else if s.server_apps.files_locations_pending == Some(serial) {
            4
        } else {
            3
        }
    });
    if kind == 3 {
        if let Some(result) = software::server_reply(ui, serial, result)
            && let Some(result) = compose_ui::event(ui, serial, result)
            && let Some(result) = disks_ui::event(ui, serial, result)
            && let Some(result) = network_ui::event(ui, serial, result)
            && let Some(result) = processes_ui::event(ui, serial, result)
            && let Some(result) = nginx_ui::event(ui, serial, result)
            && let Some(result) = redis_ui::event(ui, serial, result)
        {
            docker_ui::event(ui, serial, result);
        }
        return;
    }
    let mut restore = None;
    let mut save_local_geometry = false;
    if kind == 2 {
        finish_desktop_sync(ui, result);
        return;
    }
    ui.state.revise(|s| match (kind, result) {
        (0, reply) => {
            s.server_apps.discover_pending = None;
            match reply {
                Ok(Reply::Applications(apps)) => {
                    s.server_apps.applications = apps;
                    if !s.server_apps.applications.docker {
                        server_desktop::remove_window(s, WorkspacePage::Docker);
                        s.docker = Default::default();
                    }
                    if !s.server_apps.applications.nginx {
                        server_desktop::remove_window(s, WorkspacePage::Nginx);
                        s.nginx = Default::default();
                    }
                    if !s.server_apps.applications.redis {
                        server_desktop::remove_window(s, WorkspacePage::Redis);
                        s.redis = Default::default();
                    }
                }
                Err(error) => s.server_apps.discover_error = error,
                _ => s.server_apps.discover_error = "Invalid application detection response".into(),
            }
        }
        (1, reply) => {
            s.server_apps.preferences_pending = None;
            match reply {
                Ok(Reply::Preferences(prefs)) => {
                    let prefs_for_layout = s
                        .server_apps
                        .pending
                        .as_ref()
                        .map_or(&prefs, |pending| &pending.desired);
                    if !s.server_apps.preferences_loaded {
                        restore = Some(prefs_for_layout.terminal.clone());
                    }
                    let mut windows = prefs_for_layout.windows.clone();
                    for app in [DesktopApp::Disks, DesktopApp::Services, DesktopApp::Logs] {
                        if let Some(window) = windows.remove(&app) {
                            windows.entry(DesktopApp::Monitor).or_insert(window);
                        }
                    }
                    if prefs_for_layout.terminal.maximized {
                        windows
                            .entry(DesktopApp::Terminal)
                            .or_insert(DesktopWindow {
                                maximized: true,
                                ..Default::default()
                            });
                    }
                    if !s.server_apps.preferences_loaded && !s.workbench.windows.is_empty() {
                        windows.append(&mut s.workbench.windows);
                        save_local_geometry = true;
                    }
                    if s.server_apps.preferences_loaded {
                        for app in s
                            .workbench
                            .apps
                            .iter()
                            .filter_map(|page| server_desktop::app_id(*page))
                        {
                            if let Some(window) = s.workbench.windows.get(&app) {
                                windows.insert(app, *window);
                            }
                        }
                    }
                    s.workbench.windows = windows;
                    s.server_apps.preferences = prefs;
                    s.server_apps.preferences_loaded = true;
                }
                Err(error) => {
                    if error.starts_with("Invalid desktop preferences") {
                        s.server_apps.preferences_loaded = false;
                    }
                    s.server_apps.preferences_error = error;
                }
                _ => {
                    s.server_apps.preferences_error = "Invalid desktop preferences response".into()
                }
            }
        }
        (4, reply) => {
            s.server_apps.files_locations_pending = None;
            s.server_apps.files_locations = Some(match reply {
                Ok(Reply::FileLocations(locations)) => locations,
                _ => Vec::new(),
            });
        }
        _ => {}
    });
    if let Some(layout) = restore {
        ui.state.revise(|s| {
            terminal::restore(s, &layout);
        });
    }
    if kind == 0 {
        ui.docker_enter();
        ui.nginx_enter();
        ui.redis_enter();
    }
    if kind <= 1 {
        ui.terminal_enter();
    }
    if save_local_geometry {
        ui.remember_desktop_layout();
    }
    if kind == 1 {
        ui.send_desktop_sync();
    }
}

fn finish_desktop_sync(ui: &Ui, result: std::result::Result<Reply, String>) {
    let data = ui.state.revise(|s| {
        s.server_apps.save_pending = None;
        s.server_apps.save_started = None;
        s.server_apps.sync_after = Some(Instant::now() + Duration::from_secs(5));
        Some((
            s.server_apps.writing.take()?,
            s.server_apps.pending.clone()?,
        ))
    });
    let Some((sent, pending)) = data else { return };
    match result {
        Ok(Reply::SavedPreferences(DesktopResult::Saved(saved))) => {
            let outcome = (|| -> anyhow::Result<_> {
                let newer = if pending.desired != sent {
                    let (desired, _) = sync::merge_desktop(&sent, &pending.desired, &saved, true)?;
                    (desired != saved).then(|| Pending {
                        base: saved.clone(),
                        desired,
                        attempted: None,
                    })
                } else {
                    None
                };
                ui.persist_desktop_sync(newer.as_ref())?;
                Ok(newer)
            })();
            match outcome {
                Ok(newer) => {
                    ui.state.revise(|s| {
                        s.server_apps.preferences = saved;
                        s.server_apps.pending = newer;
                        s.server_apps.preferences_error.clear();
                    });
                    ui.send_desktop_sync();
                }
                Err(error) => ui
                    .state
                    .revise(|s| s.server_apps.preferences_error = error.to_string()),
            }
        }
        Ok(Reply::SavedPreferences(DesktopResult::Conflict { remote, paths })) => {
            ui.state
                .revise(|s| s.server_apps.conflict = Some((remote, paths)));
            if !ui.state.with(settings_visible) {
                ui.error(i18n::translate(
                    "Desktop sync conflict. Open Settings to resolve.",
                ));
            }
        }
        Err(error) => ui.state.revise(|s| s.server_apps.preferences_error = error),
        _ => ui
            .state
            .revise(|s| s.server_apps.preferences_error = "Invalid desktop sync response".into()),
    }
}

pub(crate) fn settings(ui: Ui) -> impl Widget {
    use desktop_customization_ui::{SettingsPage, settings_page};

    let state = ui.state;
    row().class("desktop-settings-page settings-surface").fill((
        desktop_customization_ui::settings_tabs(ui.clone()),
        each(state.map(|s| vec![settings_page(s)]))
            .key(|page| *page as u8)
            .children(move |item| {
                let page = item.value.get();
                let content = match page {
                    SettingsPage::General => fragment((
                        device_info_ui::details(ui.clone()),
                        general_settings(ui.clone()),
                    )),
                    SettingsPage::Network => fragment(network_ui::page(ui.clone())),
                    SettingsPage::DefaultApps => fragment(
                        col().class("desktop-settings-pane").fill(
                            col().class("desktop-settings-card").fill((
                                settings::section_heading("Default apps", true, file_open::reset_settings(&ui, true)),
                                text().class("subtle").content(i18n::bind(
                                    "Manage default and alternative apps for remote file extensions.",
                                )),
                                file_open::settings(ui.clone(), true),
                                text()
                                    .class("error")
                                    .content(state.map(|s| s.settings.error.clone())),
                            )),
                        ),
                    ),
                    SettingsPage::Desktop => fragment(desktop_settings(ui.clone())),
                    SettingsPage::Dock => fragment(desktop_customization_ui::dock_settings(ui.clone())),
                    SettingsPage::Appearance => fragment(desktop_appearance_ui::controls(ui.clone())),
                    SettingsPage::Region => fragment(desktop_system_settings::region(ui.clone())),
                    SettingsPage::DateTime => fragment(desktop_system_settings::date_time(ui.clone())),
                    SettingsPage::Shortcuts => fragment(desktop_shortcuts_ui::controls(ui.clone())),
                    SettingsPage::About => fragment(settings::about(ui.clone(), true)),
                };
                let keep_local = ui.clone();
                let keep_remote = ui.clone();
                col().class("desktop-preferences").fill((
                    content,
                    when(
                        !matches!(
                            page,
                            SettingsPage::Network | SettingsPage::DefaultApps | SettingsPage::About
                        ),
                        move || {
                            let keep_local = keep_local.clone();
                            let keep_remote = keep_remote.clone();
                            fragment((
                                when(state.map(|s| s.server_apps.conflict.is_some()), move || {
                                    let local = keep_local.clone();
                                    let remote = keep_remote.clone();
                                    col()
                                        .class("desktop-settings-pane desktop-settings-card")
                                        .fill((
                                            text()
                                                .class("section-title")
                                                .content(i18n::bind("Desktop sync conflict")),
                                            text().class("subtle settings-copyable").content(state.map(|s| {
                                                s.server_apps
                                                    .conflict
                                                    .as_ref()
                                                    .map(|(_, paths)| paths.join(", "))
                                                    .unwrap_or_default()
                                            })),
                                            row().fill((
                                                sapid::button()
                                                    .class("button")
                                                    .label(i18n::bind("Keep local changes"))
                                                    .on_press(move || {
                                                        local.resolve_desktop_sync(true)
                                                    }),
                                                sapid::button()
                                                    .class("button")
                                                    .label(i18n::bind("Keep server changes"))
                                                    .on_press(move || {
                                                        remote.resolve_desktop_sync(false)
                                                    }),
                                            )),
                                        ))
                                }),
                                text()
                                    .class("subtle desktop-settings-status")
                                    .content(i18n::bind(state.map(|s| {
                                        if s.server_apps.save_pending.is_none()
                                            && s.server_apps.pending.is_some()
                                            && s.server_apps.conflict.is_none()
                                        {
                                            "Pending sync"
                                        } else if s.server_apps.preferences_pending.is_some()
                                            && !s.server_apps.preferences_loaded
                                        {
                                            "Loading…"
                                        } else {
                                            ""
                                        }
                                        .to_owned()
                                    }))),
                                text().class("error").content(
                                    state.map(|s| s.server_apps.preferences_error.clone()),
                                ),
                            ))
                        },
                    ),
                ))
            }),
    ))
}

fn general_settings(ui: Ui) -> impl Widget {
    let state = ui.state;
    let reload = ui.clone();
    let detect = ui.clone();
    col().class("desktop-settings-pane").fill((
      col().class("desktop-settings-card").fill((
        settings::section_heading("Resource refresh interval", true, (
            help_ui::tip("Resource refresh interval", "This server uses the application default until you select an interval here. Sampling runs while the resource views in Monitor are open."),
            settings::desktop_reset(&ui, |prefs| prefs.resource_interval_seconds = None),
        )),
        settings::select(&ui, true, "Resource refresh interval",
            std::iter::once((None, "Follow application".to_string())).chain(
                AppSettings::RESOURCE_INTERVALS.into_iter().map(|n| (Some(n), format!("Every {n} s")))),
            |s| current(s).resource_interval_seconds, preferences_ready,
            |ui, seconds| ui.save_desktop_interval(seconds)),
      )),
      col().class("desktop-settings-card").fill((
        row().class("section-heading").fill((
            text().class("section-title").content(i18n::bind("Server desktop")),
            help_ui::tip("Server desktop settings", "Saved under ~/.arcnix/desktop/ for this SSH user. The Auto color scheme follows the client app. Other servers and users keep their own settings."),
        )),
        row().class("choice-list").fill((
            text_action("Reload settings", Icon::Refresh)
                .enabled(state.map(|s| s.connected && s.server_apps.preferences_pending.is_none() && s.server_apps.save_pending.is_none()))
                .on_press(move || reload.read_desktop_preferences()),
            text_action("Detect applications", Icon::Search)
                .enabled(state.map(|s| s.connected && s.server_apps.discover_pending.is_none()))
                .on_press(move || detect.discover_server_apps()),
        )),
        text().class("subtle").content(i18n::bind(state.map(|s| {
            if s.server_apps.applications.docker { "Docker detected" } else { "Docker not detected" }.to_owned()
        }))),
        text().class("error").content(i18n::bind(state.map(|s| s.server_apps.discover_error.clone()))),
        #[cfg(windows)]
        text().class("error").content(i18n::bind(state.map(|s| s.server_apps.cookies.error.clone()))),
      )),
    ))
}

fn desktop_settings(ui: Ui) -> impl Widget {
    col().class("desktop-settings-pane").fill((
        desktop_customization_ui::layout_controls(ui.clone()),
        desktop_customization_ui::icon_settings(ui.clone()),
        files_ui::cleanup_settings(ui),
    ))
}
