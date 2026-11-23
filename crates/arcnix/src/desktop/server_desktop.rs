//! Browser-style server tabs and the desktop application shell.
use std::collections::BTreeMap;

use arcnix_core::server_apps::{DesktopApp, DesktopWindow, WindowBounds};

use crate::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Home,
    Desktop,
    TransferFiles,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Desktop => "Desktop",
            Self::TransferFiles => "Transfer files",
        }
    }
    fn icon(self) -> Icon {
        match self {
            Self::Home => Icon::Server,
            Self::Desktop => Icon::Grid,
            Self::TransferFiles => Icon::Folder,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Workbench {
    pub active: Tab,
    pub tabs: Vec<Tab>,
    desktop_page: WorkspacePage,
    pub apps: Vec<WorkspacePage>,
    stack: Vec<WorkspacePage>,
    desktop_restore: Vec<WorkspacePage>,
    slots: BTreeMap<WorkspacePage, usize>,
    detached: BTreeMap<WorkspacePage, DesktopWindow>,
    pub files: files_tabs::Windows,
    pub windows: BTreeMap<DesktopApp, DesktopWindow>,
    pub fullscreen: bool,
}

impl Default for Workbench {
    fn default() -> Self {
        Self {
            active: Tab::Home,
            tabs: vec![Tab::Home],
            desktop_page: WorkspacePage::Desktop,
            apps: Vec::new(),
            stack: Vec::new(),
            desktop_restore: Vec::new(),
            slots: BTreeMap::new(),
            detached: BTreeMap::new(),
            files: Default::default(),
            windows: BTreeMap::new(),
            fullscreen: false,
        }
    }
}

pub(crate) fn app_id(page: WorkspacePage) -> Option<DesktopApp> {
    Some(match page {
        WorkspacePage::Detached(app, _) => app,
        WorkspacePage::Terminal => DesktopApp::Terminal,
        WorkspacePage::Monitor => DesktopApp::Monitor,
        WorkspacePage::AppCenter => DesktopApp::AppCenter,
        WorkspacePage::Files(_) => DesktopApp::Files,
        WorkspacePage::Docker => DesktopApp::Docker,
        WorkspacePage::Editor => DesktopApp::Editor,
        WorkspacePage::Code => DesktopApp::Code,
        WorkspacePage::Player => DesktopApp::Player,
        WorkspacePage::ImageViewer => DesktopApp::ImageViewer,
        WorkspacePage::PdfReader => DesktopApp::PdfReader,
        WorkspacePage::Browser => DesktopApp::Browser,
        WorkspacePage::Settings => DesktopApp::Settings,
        WorkspacePage::PgAdmin => DesktopApp::PgAdmin,
        _ => return None,
    })
}

pub(crate) fn app_page(app: DesktopApp) -> WorkspacePage {
    match app {
        DesktopApp::Terminal => WorkspacePage::Terminal,
        DesktopApp::Monitor | DesktopApp::Disks | DesktopApp::Services | DesktopApp::Logs => {
            WorkspacePage::Monitor
        }
        DesktopApp::AppCenter => WorkspacePage::AppCenter,
        DesktopApp::Files | DesktopApp::Trash => WorkspacePage::Files(0),
        DesktopApp::Docker => WorkspacePage::Docker,
        DesktopApp::Editor => WorkspacePage::Editor,
        DesktopApp::Code => WorkspacePage::Code,
        DesktopApp::Player => WorkspacePage::Player,
        DesktopApp::ImageViewer => WorkspacePage::ImageViewer,
        DesktopApp::PdfReader => WorkspacePage::PdfReader,
        DesktopApp::Browser => WorkspacePage::Browser,
        DesktopApp::Settings => WorkspacePage::Settings,
        DesktopApp::PgAdmin => WorkspacePage::PgAdmin,
    }
}

impl Workbench {
    pub(crate) fn window(&self, page: WorkspacePage) -> DesktopWindow {
        if page.tab_window().is_some_and(|id| id != 0) {
            return self.detached.get(&page).copied().unwrap_or_default();
        }
        app_id(page)
            .and_then(|app| self.windows.get(&app).copied())
            .unwrap_or_default()
    }

    fn open_window(&mut self, page: WorkspacePage) {
        if !self.apps.contains(&page) {
            self.apps.push(page);
            let slot = (0..)
                .find(|slot| !self.slots.values().any(|saved| saved == slot))
                .unwrap();
            self.slots.insert(page, slot);
        }
        self.desktop_restore.clear();
        self.stack.retain(|&open| open != page);
        self.stack.push(page);
    }

    pub(crate) fn window_visible(&self, page: WorkspacePage) -> bool {
        self.stack.contains(&page)
    }

    pub(crate) fn reset_open_windows(&mut self) {
        self.apps.clear();
        self.stack.clear();
        self.desktop_restore.clear();
        self.slots.clear();
        self.detached.clear();
        self.files = Default::default();
        self.desktop_page = WorkspacePage::Desktop;
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundsEvent {
    epoch: u64,
    app: DesktopApp,
    #[serde(default)]
    window: Option<u32>,
    bounds: WindowBounds,
}

fn save_bounds(ui: &Ui, input: &str) {
    let Ok(event) = serde_json::from_str::<BoundsEvent>(input) else {
        return;
    };
    if event.bounds.validate().is_err() {
        return;
    }
    let page = event.window.map_or_else(
        || app_page(event.app),
        |id| window_tabs::page(event.app, id),
    );
    let changed = ui.state.revise(|s| {
        if !visible(s)
            || s.dialog.is_some()
            || s.epoch != event.epoch
            || !s.workbench.window_visible(page)
            || app_id(page) != Some(event.app)
        {
            return false;
        }
        let window = window_mut(s, page, event.app);
        if window.maximized || window.bounds == Some(event.bounds) {
            return false;
        }
        window.bounds = Some(event.bounds);
        true
    });
    if changed && page.tab_window().is_none_or(|id| id == 0) {
        ui.remember_desktop_layout();
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FocusEvent {
    epoch: u64,
    app: DesktopApp,
    #[serde(default)]
    window: Option<u32>,
}

fn focus_window(ui: &Ui, input: &str) {
    let Ok(event) = serde_json::from_str::<FocusEvent>(input) else {
        return;
    };
    let page = event.window.map_or_else(
        || app_page(event.app),
        |id| window_tabs::page(event.app, id),
    );
    if ui.state.with(|s| {
        visible(s)
            && s.epoch == event.epoch
            && s.dialog.is_none()
            && s.fingerprint.is_none()
            && page != s.workspace_page
            && s.workbench.window_visible(page)
            && app_id(page) == Some(event.app)
    }) {
        ui.focus_desktop_app(page);
    }
}

pub(crate) const APP_LAUNCHERS: [WorkspacePage; 13] = [
    WorkspacePage::Files(0),
    WorkspacePage::Terminal,
    WorkspacePage::Code,
    WorkspacePage::Browser,
    WorkspacePage::Player,
    WorkspacePage::ImageViewer,
    WorkspacePage::PdfReader,
    WorkspacePage::Monitor,
    WorkspacePage::Docker,
    WorkspacePage::PgAdmin,
    WorkspacePage::AppCenter,
    WorkspacePage::Settings,
    WorkspacePage::Editor,
];

fn local_app(s: &State, page: WorkspacePage) -> bool {
    if page.base() == WorkspacePage::Terminal && !s.terminal_available() {
        return false;
    }
    if page.base() == WorkspacePage::Monitor {
        return s
            .local_desktop
            .as_ref()
            .is_some_and(local_desktop::Target::supports_metrics);
    }
    matches!(
        page.base(),
        WorkspacePage::Files(_)
            | WorkspacePage::Code
            | WorkspacePage::ImageViewer
            | WorkspacePage::PdfReader
            | WorkspacePage::Player
            | WorkspacePage::Editor
            | WorkspacePage::Terminal
            | WorkspacePage::Settings
    )
}

pub(crate) fn available_apps(s: &State) -> Vec<WorkspacePage> {
    APP_LAUNCHERS
        .into_iter()
        .filter(|page| s.local_desktop.is_none() || local_app(s, *page))
        .filter(|page| match page {
            WorkspacePage::Browser => cfg!(windows),
            // Only this platform's component has been tested.
            WorkspacePage::PgAdmin => cfg!(target_os = "macos"),
            WorkspacePage::Docker => s.server_apps.applications.docker,
            _ => true,
        })
        .collect()
}
pub(crate) fn app_label(page: WorkspacePage) -> &'static str {
    match page.base() {
        WorkspacePage::Docker => "Docker",
        WorkspacePage::PgAdmin => "pgAdmin",
        WorkspacePage::Files(_) => "文件管理",
        WorkspacePage::Code => "Code Editor",
        WorkspacePage::Editor => "Text Editor",
        WorkspacePage::Player => "Player",
        WorkspacePage::ImageViewer => "Image Viewer",
        WorkspacePage::PdfReader => "PDF Reader",
        WorkspacePage::Browser => "Browser",
        WorkspacePage::Terminal => "终端",
        WorkspacePage::Monitor => "Monitor",
        WorkspacePage::AppCenter => "App Center",
        WorkspacePage::Settings => "Settings",
        _ => "Desktop",
    }
}

fn launcher_page(s: &State, page: WorkspacePage) -> WorkspacePage {
    if page == WorkspacePage::Files(0) {
        s.workbench
            .files
            .current()
            .map_or(page, WorkspacePage::Files)
    } else {
        window_tabs::current(s, page)
    }
}

pub(crate) fn can_close_app(s: &State, page: WorkspacePage) -> bool {
    page == WorkspacePage::Code
        || (!s.system_task_running
            && (page.base() != WorkspacePage::Editor || !text_editor::desktop_busy(s)))
}

pub(crate) fn show_desktop_label(s: &State) -> &'static str {
    if s.workbench.stack.is_empty() && !s.workbench.desktop_restore.is_empty() {
        "Restore windows"
    } else {
        "Show desktop"
    }
}

fn window_class(page: WorkspacePage) -> String {
    match page {
        WorkspacePage::Files(id) => format!("files-window-{id}"),
        _ if page.tab_window().is_some() => format!(
            "tab-window-{}-{}",
            app_class(page),
            page.tab_window().unwrap()
        ),
        _ => app_class(page).into(),
    }
}

fn window_mut(s: &mut State, page: WorkspacePage, app: DesktopApp) -> &mut DesktopWindow {
    if page.tab_window().is_some_and(|id| id != 0) {
        s.workbench.detached.entry(page).or_default()
    } else {
        s.workbench.windows.entry(app).or_default()
    }
}

pub(crate) fn app_class(page: WorkspacePage) -> &'static str {
    match page.base() {
        WorkspacePage::Docker => "app-docker",
        WorkspacePage::PgAdmin => "app-pgadmin",
        WorkspacePage::Files(_) => "app-files",
        WorkspacePage::Code => "app-code",
        WorkspacePage::Editor => "app-editor",
        WorkspacePage::Player => "app-player",
        WorkspacePage::ImageViewer => "app-image",
        WorkspacePage::PdfReader => "app-document",
        WorkspacePage::Browser => "app-browser",
        WorkspacePage::Terminal => "app-terminal",
        WorkspacePage::Monitor => "app-monitor",
        WorkspacePage::AppCenter => "app-center",
        WorkspacePage::Settings => "app-settings",
        _ => "app-desktop",
    }
}

pub(crate) fn reader_fullscreen(s: &State, page: WorkspacePage) -> bool {
    match page {
        WorkspacePage::PdfReader => s.pdf.fullscreen,
        WorkspacePage::ImageViewer => s.images.fullscreen,
        _ => false,
    }
}

pub(crate) fn app_icon(page: WorkspacePage) -> Icon {
    match page.base() {
        WorkspacePage::Terminal => Icon::Terminal,
        WorkspacePage::Monitor => Icon::Monitor,
        WorkspacePage::AppCenter => Icon::AppCenter,
        WorkspacePage::Settings => Icon::Settings,
        WorkspacePage::Files(_) => Icon::Folder,
        WorkspacePage::Code => Icon::Code,
        WorkspacePage::Editor => Icon::Edit,
        WorkspacePage::Player => Icon::Play,
        WorkspacePage::ImageViewer => Icon::Image,
        WorkspacePage::PdfReader => Icon::File,
        WorkspacePage::Browser => Icon::Browser,
        WorkspacePage::Docker => Icon::Docker,
        WorkspacePage::PgAdmin => Icon::Database,
        _ => Icon::Grid,
    }
}

pub(crate) fn select_tab(s: &mut State, tab: Tab) {
    if tab != Tab::Desktop {
        s.workbench.fullscreen = false;
    }
    observability::metrics_leave(s);
    s.terminal.menu = None;
    s.file_context = None;
    s.file_menu = None;
    s.file_browser.cancel_drag();
    if s.workbench.active == Tab::Desktop {
        s.workbench.desktop_page = s.workspace_page;
    }
    if !s.workbench.tabs.contains(&tab) {
        s.workbench.tabs.push(tab);
    }
    s.workbench.active = tab;
    s.workspace_page = match tab {
        Tab::Home => WorkspacePage::Desktop,
        Tab::Desktop => s.workbench.desktop_page,
        Tab::TransferFiles => WorkspacePage::TransferFiles,
    };
}

pub(crate) fn visible(s: &State) -> bool {
    !s.background
        && s.navigation == navigation::Page::Servers
        && s.connected
        && s.workbench.active == Tab::Desktop
}

pub(crate) fn fullscreen(s: &State) -> bool {
    s.workbench.fullscreen && visible(s)
}

pub(crate) fn owns_dialog(s: &State) -> bool {
    #[cfg(windows)]
    if s.dialog == Some(Dialog::BrowserForwardingRepair) {
        return visible(s);
    }
    visible(s)
        && matches!(
            s.dialog,
            Some(
                Dialog::SoftwareInstall
                    | Dialog::OpenDesktopFile(_)
                    | Dialog::DesktopTimeZone
                    | Dialog::DesktopIcons
                    | Dialog::DesktopApps
                    | Dialog::DockShortcuts
                    | Dialog::FileAssociation { .. }
                    | Dialog::OpenWith { .. }
                    | Dialog::SoftwareAction(..)
                    | Dialog::DesktopTask(..)
                    | Dialog::ScriptRefresh(_, _, true)
                    | Dialog::ConfigureNetwork
                    | Dialog::StartupService
                    | Dialog::NativeServiceConfirm
                    | Dialog::ScriptRun
                    | Dialog::DockerAction
                    | Dialog::DockerConfigure
                    | Dialog::TerminalRename { .. }
                    | Dialog::TerminatePortProcess
                    | Dialog::CustomizeFolder { .. }
                    | Dialog::AppSettings(_)
                    | Dialog::FilesSettings
                    | Dialog::FileProperties
                    | Dialog::Trash { remote: true }
                    | Dialog::FileColumns { remote: true }
                    | Dialog::DeleteFiles { remote: true, .. }
                    | Dialog::FileElevation
                    | Dialog::TransferProgress(_)
                    | Dialog::FilesDownload { .. }
                    | Dialog::TransferDefaults
                    | Dialog::TransferConflict
            )
        )
}

/// Also used by Terminal here and service-log links so they open the right tab.
pub(crate) fn select_app(s: &mut State, page: WorkspacePage) {
    if app_id(page).is_none() {
        return;
    }
    select_tab(s, Tab::Desktop);
    if let Some(id) = page.files_window_id() {
        files_tabs::focus(s, id);
    }
    window_tabs::focus(s, page);
    s.workbench.open_window(page);
    s.workspace_page = page;
    s.workbench.desktop_page = page;
}

fn front_window(s: &mut State) {
    let page = s.workbench.stack.last().copied().unwrap_or_default();
    if page == s.workbench.desktop_page {
        return;
    }
    s.workbench.desktop_page = page;
    if s.workbench.active == Tab::Desktop {
        if let Some(id) = page.files_window_id() {
            files_tabs::focus(s, id);
        }
        observability::metrics_leave(s);
        s.terminal.menu = None;
        s.file_context = None;
        s.file_menu = None;
        s.file_browser.cancel_drag();
        window_tabs::focus(s, page);
        s.workspace_page = page;
    }
}

pub(crate) fn has_siblings(s: &State, page: WorkspacePage) -> bool {
    s.workbench
        .apps
        .iter()
        .any(|&other| other != page && app_id(other) == app_id(page))
}

pub(crate) fn remove_window(s: &mut State, page: WorkspacePage) {
    if page == WorkspacePage::Settings {
        desktop_customization_ui::close_settings_search(s);
    }
    s.workbench.apps.retain(|&open| open != page);
    s.workbench.stack.retain(|&open| open != page);
    s.workbench.desktop_restore.retain(|&open| open != page);
    s.workbench.slots.remove(&page);
    s.workbench.detached.remove(&page);
    if let Some(id) = page.files_window_id() {
        files_tabs::remove(s, id);
    }
    window_tabs::forget(s, page);
    front_window(s);
}

impl Ui {
    pub(crate) fn toggle_show_desktop(&self) {
        let hiding = self.state.revise(|s| {
            let workbench = &mut s.workbench;
            let hiding = !workbench.stack.is_empty();
            if hiding {
                workbench.desktop_restore = std::mem::take(&mut workbench.stack);
            } else {
                workbench.stack = std::mem::take(&mut workbench.desktop_restore);
            }
            front_window(s);
            hiding
        });
        self.desktop_enter();
        if hiding {
            self.refresh_desktop_items(true);
        }
    }

    pub(crate) fn toggle_desktop_fullscreen(&self) {
        let changed_tab = self.state.with(|s| s.workbench.active != Tab::Desktop);
        self.state.revise(|s| {
            if s.navigation != navigation::Page::Servers || !s.connected {
                return;
            }
            let was_fullscreen = fullscreen(s);
            if s.workbench.active != Tab::Desktop {
                select_tab(s, Tab::Desktop);
            }
            s.workbench.fullscreen = !was_fullscreen;
        });
        if changed_tab {
            self.desktop_enter();
        }
    }

    /// Opens a workspace from an explicit entry point, connecting if needed.
    pub(crate) fn open_workbench(&self, tab: Tab) {
        self.enter_workbench(tab, true);
    }

    /// Tab buttons only switch views. While disconnected they show the
    /// reconnect page, and open the credential dialog only when the server
    /// needs one, so switching tabs never connects by itself.
    fn switch_workbench(&self, tab: Tab) {
        self.enter_workbench(tab, false);
    }

    fn enter_workbench(&self, tab: Tab, connect: bool) {
        if tab == Tab::TransferFiles && self.state.with(|s| s.local_desktop.is_some()) {
            return;
        }
        if self.state.with(|s| s.workbench.active != tab) {
            self.state.revise(|s| select_tab(s, tab));
        }
        if self
            .state
            .with(|s| !s.connected && !s.connecting && tab != Tab::Home)
        {
            if connect {
                self.request_connect();
            } else if self
                .state
                .with(|s| s.local_desktop.is_none() && connect_prompt_needed(s))
            {
                self.open_dialog(Dialog::Connect);
            }
        }
        self.desktop_enter();
    }

    pub(crate) fn desktop_enter(&self) {
        self.close_desktop_menu();
        if self.state.with(visible) {
            self.refresh_desktop_items(false);
        }
        self.docker_enter();
        self.pgadmin_enter();
        self.terminal_enter();
        self.observability_enter();
        self.app_center_enter();
        self.files_enter();
        if self.state.with(|s| {
            visible(s)
                && s.workspace_page.is_files()
                && !files_tabs::apps_active(s)
                && s.sftp_available
                && s.remote_request == 0
                && !s.remote_ready
        }) {
            self.refresh(true);
        }
        trash_ui::enter(self);
        network_ui::enter(self);
        if self.state.with(|s| {
            navigation::workspace_visible(s)
                && s.workspace_page == WorkspacePage::AppCenter
                && s.software.environment.is_none()
                && s.software.pending.is_none()
        }) {
            self.software_detect();
        }
    }

    pub(crate) fn open_desktop_app(&self, page: WorkspacePage) {
        let page = self.state.with(|s| launcher_page(s, page));
        self.focus_desktop_app(page);
    }

    pub(crate) fn close_desktop_launcher(&self, page: WorkspacePage) {
        if let Some(target) = self.state.with(|s| {
            (launcher_running(s, page) && can_close_app(s, page)).then(|| launcher_page(s, page))
        }) {
            self.close_desktop_app(target);
        }
    }

    pub(crate) fn focus_desktop_app(&self, page: WorkspacePage) {
        if self
            .state
            .with(|s| s.local_desktop.is_some() && !local_app(s, page))
        {
            return;
        }
        if page.is_files()
            && self
                .state
                .with(|s| s.workspace_page != page && s.file_pending.is_some())
        {
            return;
        }
        self.state.revise(|s| select_app(s, page));
        #[cfg(windows)]
        if page.base() == WorkspacePage::Browser {
            self.open_web_browser();
        }
        self.desktop_enter();
    }

    pub(crate) fn cycle_desktop_apps(&self, backwards: bool) {
        let next = self.state.with(|s| {
            let apps = &s.workbench.apps;
            if apps.is_empty() {
                return None;
            }
            let current = apps.iter().position(|&app| app == s.workspace_page);
            let index = match (current, backwards) {
                (Some(index), true) => (index + apps.len() - 1) % apps.len(),
                (Some(index), false) => (index + 1) % apps.len(),
                (None, true) => apps.len() - 1,
                (None, false) => 0,
            };
            (apps[index] != s.workspace_page).then_some(apps[index])
        });
        if let Some(page) = next {
            self.focus_desktop_app(page);
        }
    }

    pub(crate) fn minimize_desktop_window(&self) {
        self.minimize_window(self.state.with(|s| s.workspace_page));
    }

    fn minimize_window(&self, page: WorkspacePage) {
        let changed = self.state.revise(|s| {
            let previous = s.workspace_page;
            s.workbench.stack.retain(|&open| open != page);
            front_window(s);
            s.workspace_page != previous
        });
        if changed {
            self.desktop_enter();
        }
    }

    pub(crate) fn toggle_desktop_maximize(&self) {
        self.toggle_window_maximize(self.state.with(|s| s.workspace_page));
    }

    fn toggle_window_maximize(&self, page: WorkspacePage) {
        let Some(app) = app_id(page) else {
            return;
        };
        let changed = self.state.revise(|s| {
            if !s.workbench.window_visible(page) {
                return false;
            }
            let window = window_mut(s, page, app);
            window.maximized = !window.maximized;
            true
        });
        if changed && page.tab_window().is_none_or(|id| id == 0) {
            self.remember_desktop_layout();
        }
    }

    pub(super) fn close_active_desktop(&self, tab: bool) {
        let Some(page) = self.state.with(|s| {
            let page = s.workspace_page;
            (s.workbench.window_visible(page)
                && can_close_app(s, page)
                && !(page == WorkspacePage::Code && s.code.confirm)
                && !(page.base() == WorkspacePage::Editor && s.desktop_editor.confirming_close()))
            .then_some(page)
        }) else {
            return;
        };
        if !tab {
            self.close_desktop_app(page);
            return;
        }
        match page.base() {
            WorkspacePage::Files(id) => files_tabs::close_selected(self, id),
            WorkspacePage::Terminal => {
                let id = self.state.with(|s| s.terminal.id);
                if id > 0 {
                    self.terminal_close_tab(id);
                }
            }
            WorkspacePage::Editor => text_editor::close_selected(self),
            WorkspacePage::Code => self.state.revise(|s| {
                s.code.commands.push("workbench.action.closeActiveEditor");
            }),
            #[cfg(windows)]
            WorkspacePage::Browser => self.state.revise(|s| {
                if let Some(id) = s.web_browser.active {
                    web_browser::close_tab(s, id);
                }
            }),
            _ => {}
        }
    }

    fn close_desktop_app(&self, page: WorkspacePage) {
        if page.base() == WorkspacePage::Editor && !text_editor::close_desktop(self, page) {
            return;
        }
        if page == WorkspacePage::Code && !code_workbench::close(self) {
            return;
        }
        self.finish_desktop_close(page);
    }

    pub(crate) fn finish_desktop_close(&self, page: WorkspacePage) {
        if page == WorkspacePage::Code {
            crate::code_terminal::close_all(self);
            self.state.revise(|s| s.code = Default::default());
        }
        if page == WorkspacePage::PgAdmin {
            self.state.revise(pgadmin_ui::reset);
        }
        if page == WorkspacePage::PdfReader {
            self.state.revise(|s| s.pdf = Default::default());
        }
        if page == WorkspacePage::ImageViewer {
            self.state.revise(|s| s.images = Default::default());
        }
        if page == WorkspacePage::Player {
            self.player.stop(self);
        }
        let siblings = self.state.with(|s| has_siblings(s, page));
        #[cfg(windows)]
        if page.base() == WorkspacePage::Browser {
            if siblings {
                self.state.revise(|s| {
                    let tabs = s
                        .web_browser
                        .groups
                        .tabs(page.tab_window().unwrap())
                        .to_vec();
                    for id in tabs {
                        s.web_browser.close_tab(id);
                    }
                });
            } else {
                self.close_web_browser();
            }
        }
        if page.base() == WorkspacePage::Terminal {
            if siblings {
                let tabs = self
                    .state
                    .with(|s| s.terminal.groups.tabs(page.tab_window().unwrap()).to_vec());
                for id in tabs {
                    self.terminal_remove_tab(id);
                }
                self.remember_desktop_layout();
            } else {
                self.terminal_close_all();
            }
        }
        let changed = self.state.revise(|s| {
            let previous = s.workspace_page;
            remove_window(s, page);
            previous != s.workspace_page
        });
        if changed {
            self.desktop_enter();
        }
    }
}

pub(crate) fn tabs(ui: Ui) -> impl Widget {
    let state = ui.state;
    row().class("workbench-tabs").fill(
        each(state.map(|s| s.workbench.tabs.clone()))
            .key(|tab| *tab as u8)
            .children(move |item| {
                let tab = item.value.get();
                let choose = ui.clone();
                let close = ui.clone();
                let fullscreen_ui = ui.clone();
                row()
                    .class(state.map(move |s| {
                        if s.workbench.active == tab {
                            "workbench-tab active"
                        } else {
                            "workbench-tab"
                        }
                        .to_owned()
                    }))
                    .fill((
                        sapid::button()
                            .class("workbench-tab-select")
                            .name(i18n::bind(tab.label()))
                            .pressed(state.map(move |s| s.workbench.active == tab))
                            .fill((
                                icons::glyph(tab.icon()),
                                text().content(i18n::bind(tab.label())),
                            ))
                            .on_press(move || choose.switch_workbench(tab)),
                        when(tab == Tab::Desktop, move || {
                            fullscreen_action(fullscreen_ui.clone(), false)
                        }),
                        when(tab != Tab::Home, move || {
                            close_action(match tab {
                                Tab::Desktop => "Close Desktop tab",
                                _ => "Close Transfer files tab",
                            })
                            .class("icon-button workbench-tab-close")
                            .on_press({
                                let close = close.clone();
                                move || {
                                    close.state.revise(|s| {
                                        if s.workbench.active == tab {
                                            select_tab(s, Tab::Home);
                                        }
                                        s.workbench.tabs.retain(|open| *open != tab);
                                    });
                                }
                            })
                        }),
                    ))
            }),
    )
}

fn fullscreen_action(ui: Ui, exit: bool) -> impl Widget {
    let state = ui.state;
    let label = if exit {
        "Exit desktop fullscreen"
    } else {
        "Fullscreen desktop"
    };
    sapid::button()
        .name(i18n::bind(label))
        .fill(icons::glyph(if exit {
            Icon::ExitFullscreen
        } else {
            Icon::Fullscreen
        }))
        .class(if exit {
            "icon-button desktop-fullscreen-exit"
        } else {
            "icon-button desktop-fullscreen-toggle"
        })
        .enabled(state.map(|s| s.connected))
        .on_press(move || ui.toggle_desktop_fullscreen())
}

fn fullscreen_bar(ui: Ui) -> impl Widget {
    widget(move |scope| {
        let state = ui.state;
        let pinned = scope.cage(true);
        let ui = ui.clone();
        when(state.map(fullscreen), move || {
            view().class("desktop-menubar-overlay").fill((
                view().class("desktop-menubar-edge"),
                row().class("desktop-menubar").fill((
                    sapid::button()
                        .class("icon-button desktop-menubar-pin")
                        .name(i18n::bind("Pin fullscreen bar"))
                        .fill(icons::glyph(Icon::Pin))
                        .pressed(pinned)
                        .on_press(move || pinned.revise(|pinned| *pinned = !*pinned)),
                    text()
                        .class("desktop-menubar-title")
                        .content(state.map(selected_name)),
                    workspace::transfer_progress_buttons(ui.clone()),
                    connection_target(state),
                    text()
                        .class("desktop-secure")
                        .content(i18n::desktop::bind(state.map(|s| {
                            if s.local_desktop.is_some() {
                                "已直连本机"
                            } else {
                                "SSH 已连接"
                            }
                            .to_owned()
                        }))),
                    fullscreen_action(ui.clone(), true),
                )),
            ))
        })
        .show_in(scope)
    })
}

pub(crate) fn entry(ui: Ui, tab: Tab) -> impl Widget {
    let description = match tab {
        Tab::Desktop => "终端、资源、软件与服务，尽在服务器桌面。",
        _ => "在本地与服务器之间浏览、上传和下载文件。",
    };
    sapid::button()
        .class(if tab == Tab::Desktop {
            "workspace-entry overview-connect entry-desktop"
        } else {
            "workspace-entry overview-transfer entry-files"
        })
        .name(i18n::bind(tab.label()))
        .fill((
            icons::glyph(tab.icon()),
            text().class("entry-title").content(i18n::bind(tab.label())),
            text()
                .class("entry-description")
                .content(i18n::bind(description)),
            icons::glyph(Icon::Go),
        ))
        .on_press(move || ui.open_workbench(tab))
}

pub(crate) fn launcher_running(s: &State, page: WorkspacePage) -> bool {
    s.workbench
        .apps
        .iter()
        .any(|open| app_id(*open) == app_id(page))
}

fn launcher_active(s: &State, page: WorkspacePage) -> bool {
    app_id(s.workspace_page) == app_id(page)
}

pub(crate) fn launcher(ui: Ui, page: WorkspacePage, dock: bool) -> impl Widget {
    let state = ui.state;
    let label = app_label(page);
    sapid::button()
        .class(state.map(move |s| {
            format!(
                "{} {}{}{}",
                if dock {
                    "desktop-dock-app"
                } else {
                    "desktop-shortcut"
                },
                app_class(page),
                if launcher_running(s, page) {
                    " running"
                } else {
                    ""
                },
                if launcher_active(s, page) {
                    " active"
                } else {
                    ""
                }
            )
        }))
        .name(i18n::desktop::bind(label))
        .pressed(state.map(move |s| launcher_active(s, page)))
        .fill((
            text().class("desktop-app-icon").content(""),
            text()
                .class("desktop-app-label")
                .content(i18n::desktop::bind(label)),
        ))
        .on_press(move || ui.open_desktop_app(page))
}

pub(crate) fn tab_overflow() -> impl Widget {
    row().class("window-tabs-overflow").fill((
        sapid::button()
            .name(i18n::desktop::bind("All tabs"))
            .class("icon-button window-tabs-more")
            .fill(icons::glyph(Icon::ChevronRight)),
        col().class("window-tabs-menu"),
    ))
}

fn app_window(ui: Ui, page: WorkspacePage) -> impl Widget {
    let files = page.files_window_id();
    let state = ui.state;
    let minimize = ui.clone();
    let maximize = ui.clone();
    let close = ui.clone();
    let add_tab = ui.clone();
    let files_settings = ui.clone();
    let id = page.tab_window().unwrap_or_default();
    let browse_video = ui.clone();
    let title = if page == WorkspacePage::Player {
        text().content(state.map(|s| {
            s.video
                .as_ref()
                .map(|v| v.title.clone())
                .unwrap_or_else(|| i18n::desktop::translate("Player"))
        }))
    } else {
        text().content(i18n::desktop::bind(app_label(page)))
    };
    let tools = match page.base() {
        WorkspacePage::Terminal => fragment(terminal::header(ui.clone(), id)),
        WorkspacePage::Code => fragment(code_workbench::title_tools(ui.clone())),
        WorkspacePage::PgAdmin => fragment(pgadmin_ui::title_tools(ui.clone())),
        WorkspacePage::Files(id) => fragment((
            files_ui::sidebar_toggle(ui.clone()),
            files_tabs::tab_strip(ui.clone(), id),
            tab_overflow(),
        )),
        WorkspacePage::Editor => fragment(text_editor::menubar(ui.clone())),
        WorkspacePage::Monitor => fragment(monitor_ui::window_tools(ui.clone())),
        #[cfg(windows)]
        WorkspacePage::Browser => fragment(web_browser::address_bar(ui.clone(), id)),
        _ => fragment(()),
    };
    let mark = row()
        .class("desktop-window-mark")
        .fill(icons::glyph(app_icon(page)));
    let title = title.class("desktop-window-title");
    let titlebar = if page == WorkspacePage::Settings {
        fragment((
            row().class("desktop-settings-sidebar-heading").fill((
                mark,
                title,
                desktop_customization_ui::settings_search_toggle(ui.clone()),
            )),
            text()
                .class("desktop-settings-section-title")
                .content(i18n::desktop::bind(
                    state.map(desktop_customization_ui::settings_section_label),
                )),
            text()
                .class("desktop-settings-saving")
                .content(i18n::desktop::bind(
                    state.map(server_apps_ui::settings_save_status),
                )),
        ))
    } else {
        fragment((mark, title, tools))
    };
    let content = match page.base() {
        WorkspacePage::Terminal => fragment(terminal::window_page(ui, id)),
        WorkspacePage::Files(id) => fragment(files_tabs::window_content(ui, id)),
        WorkspacePage::Monitor => fragment(monitor_ui::page(ui)),
        WorkspacePage::AppCenter => fragment(app_center_ui::page(ui)),
        WorkspacePage::Settings => fragment(server_apps_ui::settings(ui)),
        WorkspacePage::Docker => fragment(docker_ui::page(ui)),
        WorkspacePage::PgAdmin => fragment(pgadmin_ui::page(ui)),
        WorkspacePage::Code => fragment(code_workbench::page(ui)),
        WorkspacePage::Editor => fragment(text_editor::page(ui, id)),
        WorkspacePage::Player => fragment(video_player::panel(ui)),
        WorkspacePage::ImageViewer => fragment(image_viewer::panel(ui)),
        WorkspacePage::PdfReader => fragment(pdf_reader::panel(ui)),
        #[cfg(windows)]
        WorkspacePage::Browser => fragment(web_browser::window_page(ui, id)),
        _ => fragment(()),
    };
    col()
        .class(state.map(move |s| {
            let mut class = format!("desktop-window {}", app_class(page));
            if page.tab_window().is_some() {
                class.push(' ');
                class.push_str(&window_class(page));
                if window_tabs::movable(s, page) {
                    class.push_str(" window-tabs-movable");
                }
            }
            if !s.workbench.window_visible(page) {
                class.push_str(" hidden");
            } else {
                if s.workbench.desktop_page == page {
                    class.push_str(" active");
                    if reader_fullscreen(s, page) {
                        class.push_str(" viewer-expanded");
                    }
                }
                if s.workbench.window(page).maximized {
                    class.push_str(" maximized");
                }
            }
            class
        }))
        .fill((
            row()
                .class(if page.is_files() {
                    "desktop-window-titlebar files-titlebar"
                } else if page == WorkspacePage::Settings {
                    "desktop-window-titlebar desktop-settings-titlebar"
                } else {
                    "desktop-window-titlebar"
                })
                .fill((
                    titlebar,
                    row().class("desktop-window-actions").fill((
                        when(page == WorkspacePage::Player, move || {
                            let ui = browse_video.clone();
                            i18n::desktop::icon_action("Open video", Icon::OpenVideo)
                                .class("icon-button player-open-video")
                                .on_press(move || ui.player.browse(&ui))
                        }),
                        when(files.is_some(), move || {
                            let ui = add_tab.clone();
                            i18n::desktop::icon_action("New tab", Icon::Plus)
                                .class("icon-button files-new-tab")
                                .on_press(move || {
                                    if let Some(id) = files {
                                        files_tabs::new_tab(&ui, id);
                                    }
                                })
                        }),
                        when(
                            files.is_some()
                                || matches!(
                                    page.base(),
                                    WorkspacePage::Terminal
                                        | WorkspacePage::Player
                                        | WorkspacePage::Editor
                                ),
                            move || {
                                let ui = files_settings.clone();
                                sapid::button()
                                    .name(i18n::desktop::bind("Settings"))
                                    .fill(icons::glyph(Icon::Settings))
                                    .class(if files.is_some() {
                                        "icon-button files-settings"
                                    } else {
                                        "icon-button app-settings-button"
                                    })
                                    .on_press(move || {
                                        ui.open_dialog(if files.is_some() {
                                            Dialog::FilesSettings
                                        } else {
                                            Dialog::AppSettings(page.base())
                                        })
                                    })
                            },
                        ),
                        i18n::desktop::icon_action("Minimize window", Icon::WindowMinimize)
                            .class("icon-button window-minimize")
                            .on_press(move || minimize.minimize_window(page)),
                        sapid::button()
                            .name(i18n::desktop::bind("Toggle maximize"))
                            .class("icon-button window-maximize")
                            .pressed(state.map(move |s| s.workbench.window(page).maximized))
                            .fill((
                                icons::glyph(Icon::WindowMaximize),
                                icons::glyph(Icon::WindowRestore),
                            ))
                            .on_press(move || maximize.toggle_window_maximize(page)),
                        i18n::desktop::close_action("Close application")
                            .class("icon-button window-close")
                            .enabled(state.map(move |s| can_close_app(s, page)))
                            .on_press(move || close.close_desktop_app(page)),
                    )),
                )),
            col().class("desktop-window-content").fill(content),
            resize_handles(),
        ))
}

fn app_windows(ui: Ui) -> impl Widget {
    let state = ui.state;
    col().class("desktop-window-layer").fill(
        each(state.map(|s| {
            // The terminal bridge stays mounted until its session metadata is cleared.
            std::iter::once(WorkspacePage::Terminal)
                .chain(
                    s.workbench
                        .apps
                        .iter()
                        .copied()
                        .filter(|&page| page != WorkspacePage::Terminal),
                )
                .collect::<Vec<_>>()
        }))
        .key(|page| window_class(*page))
        .children(move |item| app_window(ui.clone(), item.value.get())),
    )
}

pub(crate) fn selected_name(s: &State) -> String {
    if let Some(target) = &s.local_desktop {
        let name = navigation::local_desktop_name(target, s.config.settings.language);
        let mode = i18n::translate(navigation::local_desktop_mode(target));
        return format!("{name} · {mode}");
    }
    s.config
        .servers
        .iter()
        .find(|p| Some(&p.id) == s.selected.as_ref())
        .map(|p| {
            if p.name.trim().is_empty() {
                &p.host
            } else {
                &p.name
            }
        })
        .cloned()
        .unwrap_or_default()
}

fn desktop_address(s: &State) -> String {
    let Some(profile) = s
        .config
        .servers
        .iter()
        .find(|p| Some(&p.id) == s.selected.as_ref())
    else {
        return String::new();
    };
    if profile.port == 22 {
        profile.host.clone()
    } else if profile.host.contains(':') {
        format!("[{}]:{}", profile.host, profile.port)
    } else {
        format!("{}:{}", profile.host, profile.port)
    }
}

fn dock_clock() -> impl Widget {
    desktop_system_settings::clock_view("desktop-panel-clock")
}

pub(crate) fn desktop(ui: Ui) -> impl Widget {
    let state = ui.state;
    let apps = ui.clone();
    let geometry = ui.clone();

    let focus = ui.clone();
    col()
        .class(state.map(|s| {
            if visible(s) {
                return server_apps_ui::themed(s, "server-desktop");
            } else {
                "server-desktop hidden"
            }
            .to_owned()
        }))
        .fill((
            desktop_customization_ui::gesture_input(ui.clone()),
            text()
                .class("desktop-theme-data hidden")
                .content(state.map(desktop_appearance_ui::variables)),
            desktop_shortcuts_ui::bridge(ui.clone()),
            text()
                .class("desktop-clock-options hidden")
                .content(state.map(desktop_system_settings::clock_options_data)),
            text()
                .class("desktop-clock-sample hidden")
                .content(state.map(|s| {
                    s.server_apps
                        .clock
                        .as_ref()
                        .map_or_else(String::new, |(serial, clock)| {
                            serde_json::json!([
                                serial,
                                clock.unix_seconds,
                                clock.offset_minutes,
                                clock.zone
                            ])
                            .to_string()
                        })
                })),
            input().class("desktop-bounds-input hidden").value("").on(
                value_changed,
                move |event| {
                    if let Event::ValueChanged { value, .. } = event {
                        save_bounds(&geometry, &value);
                    }
                },
            ),
            input()
                .class("desktop-focus-input hidden")
                .value("")
                .on(value_changed, move |event| {
                    if let Event::ValueChanged { value, .. } = event {
                        focus_window(&focus, &value);
                    }
                }),
            files_tabs::bridge(ui.clone()),
            window_tabs::bridge(ui.clone()),
            text().class("desktop-motion-state").content(state.map(|s| {
                let shown = visible(s);
                let windows: Vec<_> = s
                    .workbench
                    .stack
                    .iter()
                    .enumerate()
                    .filter_map(|(index, &page)| {
                        let app = app_id(page)?;
                        let layout = s.workbench.window(page);
                        Some(serde_json::json!({
                            "id": app,
                            "window": page.tab_window(),
                            "class": window_class(page),
                            "order": index + 1,
                            "slot": s.workbench.slots.get(&page).copied().unwrap_or_default(),
                            "bounds": layout.bounds,
                            "maximized": layout.maximized,
                        }))
                    })
                    .collect();
                serde_json::json!({
                    "shown": shown,
                    "epoch": s.epoch,
                    "app": if shown && s.workspace_page != WorkspacePage::Desktop {
                        window_class(s.workspace_page)
                    } else { String::new() },
                    "fullscreen": fullscreen(s),
                    "windows": windows,
                })
                .to_string()
            })),
            fullscreen_bar(ui.clone()),
            col()
                .class("desktop-canvas")
                .name(i18n::desktop::bind("Desktop"))
                .focus(FocusMode::Programmatic)
                .fill((
                    desktop_customization_ui::wallpaper(ui.clone()),
                    when(
                        state.map(|s| desktop_customization_ui::appearance(s).show_server),
                        move || {
                            col().class("desktop-wallpaper-copy").fill((
                                text()
                                    .class("desktop-wallpaper-name")
                                    .content(state.map(|s| match &s.local_desktop {
                                        Some(target) => navigation::local_desktop_name(
                                            target,
                                            desktop_system_settings::language(s),
                                        ),
                                        None => selected_name(s),
                                    })),
                                text()
                                    .class("desktop-wallpaper-mode")
                                    .content(state.map(|s| {
                                        s.local_desktop.as_ref().map_or_else(
                                            String::new,
                                            |target| {
                                                i18n::translate_for(
                                                    navigation::local_desktop_mode(target),
                                                    desktop_system_settings::language(s),
                                                )
                                            },
                                        )
                                    })),
                                text()
                                    .class("desktop-wallpaper-address")
                                    .content(state.map(|s| {
                                        if s.local_desktop.is_some() {
                                            return i18n::translate_for(
                                                "本机体验提供部分功能，连接服务器可使用更多功能。",
                                                desktop_system_settings::language(s),
                                            );
                                        }
                                        let address = desktop_address(s);
                                        if address == selected_name(s) {
                                            String::new()
                                        } else {
                                            address
                                        }
                                    })),
                            ))
                        },
                    ),
                    col()
                        .class(state.map(desktop_customization_ui::icon_grid_class))
                        .fill(desktop_customization_ui::launchers(ui.clone(), false)),
                    app_windows(ui.clone()),
                    col()
                        .class(state.map(desktop_customization_ui::dock_zone_class))
                        .fill(
                            row()
                                .class(state.map(desktop_customization_ui::dock_class))
                                .fill((
                                    row().class("desktop-dock-leading").fill((
                                        sapid::button()
                                            .class("desktop-dock-home")
                                            .name(i18n::desktop::bind("Apps"))
                                            .pressed(
                                                state
                                                    .map(|s| s.dialog == Some(Dialog::DesktopApps)),
                                            )
                                            .fill((
                                                icons::glyph(Icon::Grid),
                                                text()
                                                    .class("desktop-app-label")
                                                    .content(i18n::desktop::bind("Apps")),
                                            ))
                                            .on_press(move || {
                                                apps.open_dialog(Dialog::DesktopApps)
                                            }),
                                        text().class("desktop-dock-divider").content(""),
                                    )),
                                    row().class("desktop-dock-apps").fill(
                                        desktop_customization_ui::launchers(ui.clone(), true),
                                    ),
                                    when(
                                        state.map(desktop_customization_ui::panel_mode),
                                        dock_clock,
                                    ),
                                    when(
                                        state.map(desktop_customization_ui::floating_dock_status),
                                        || {
                                            row().class("desktop-dock-status").fill((
                                                text().class("desktop-dock-divider").content(""),
                                                dock_clock(),
                                            ))
                                        },
                                    ),
                                )),
                        ),
                    desktop_customization_ui::context_menu(ui),
                )),
        ))
}

pub(crate) fn reconnect(ui: Ui) -> impl Widget {
    let state = ui.state;
    let connect = ui.clone();
    col().class("workspace-reconnect").fill((
        icons::glyph(Icon::Server),
        text().class("page-title").content(i18n::bind(
            state.map(|s| s.workbench.active.label().to_owned()),
        )),
        text()
            .class("home-description")
            .content(i18n::bind(state.map(|s| {
                if s.local_desktop.is_some() {
                    if s.connecting {
                        "正在打开本机桌面…"
                    } else {
                        "体验本机桌面"
                    }
                } else if s.connecting {
                    "正在建立安全连接…"
                } else {
                    "连接服务器以打开此工作区。"
                }
                .to_owned()
            }))),
        btn("连接", move || connect.request_connect())
            .class("button primary")
            .enabled(state.map(|s| !s.connecting)),
        btn("Home", move || ui.open_workbench(Tab::Home)).class("button quiet"),
    ))
}
