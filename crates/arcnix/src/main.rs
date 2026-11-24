#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod automation;
mod desktop;
mod editor;
mod files;
mod maintenance;
mod media;
mod monitoring;
mod shared;
mod shell;
mod software;
mod terminals;
mod workers;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use arcnix_core::store::ConfigStore;
use arcnix_core::*;
use automation::{operations, *};
use browser::{Sort, visible};
use desktop::*;
use editor::*;
use files::{browser, *};
use glory::prelude::*;
use glory::ui::{IntoChildren, fragment};
use glory::wry::shell::{Config, Shell, WindowApp};
use icons::{Icon, close_action, icon_action, text_action};
use maintenance::*;
use media::*;
use monitoring::*;
use shared::*;
use shell::*;
use software::{
    app_center_ui, compose_ui, docker_ui, installation_ui, pgadmin_native, pgadmin_ui,
    software_install_ui,
};
use terminals::{terminal, *};
use worker::{Action, Request, Update};
use workers::*;

#[derive(Clone, PartialEq, Eq)]
enum Dialog {
    OpenDesktopFile(desktop_file_picker::Purpose),
    TextEditor,
    DesktopTimeZone,
    DesktopIcons,
    Launcher,
    SystemPower(u64, arcnix_core::host::PowerAction),
    DockShortcuts,
    FileAssociation {
        extension: String,
        add: bool,
    },
    OpenWith {
        path: String,
        selected: Option<arcnix_core::file_associations::FileApp>,
    },
    SoftwareInstall,
    SoftwareAction(
        arcnix_core::software_install::Action,
        String,
        arcnix_core::software_install::Source,
        Vec<String>,
    ),
    DesktopTask(String),
    ConfigureNetwork,
    #[cfg(windows)]
    BrowserForwardingRepair,
    StartupService,
    NativeServiceConfirm,
    CustomizeFolder {
        path: String,
    },
    DockerAction,
    DockerConfigure,
    TerminalRename {
        id: u64,
    },
    TransferDefaults,
    TransferProgress(TransferDirection),
    FilesDownload {
        epoch: u64,
        destination: String,
        entries: Vec<FileEntry>,
    },
    Server,
    LocalDemo,
    Group {
        id: Option<String>,
    },
    Import,
    Connect,
    AppSettings(WorkspacePage),
    FilesSettings,
    FileProperties,
    FileColumns {
        remote: bool,
    },
    Trash {
        remote: bool,
    },
    DeleteFiles {
        remote: bool,
        targets: Vec<arcnix_core::deletion::Target>,
    },
    FileElevation,
    DeleteServer(String),
    DissolveGroup(String),
    DeleteGroupAndServers(String),
    SnippetEdit,
    ScriptRun,
    ScriptImport,
    ScriptExport,
    TaskRecordConnect,
    TaskRecordLog,
    NativeTask,
    TaskStop,
    TerminatePortProcess,
    ConfigurationBackup,
    ConfigurationRestore,
    RecordStorageConnect,
    CleanupReportConnect,
    ExportDownload,
    BackupEdit,
    BackupDelete(String),
    BackupBrowse,
    BackupFiles,
    BackupRestore,
    BackupDatabaseRestore,
    ScriptRefresh(String, String, bool),
    ScriptBatchRefresh(String),
    SnippetDelete(String),
    TransferConflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
enum WorkspacePage {
    #[default]
    Desktop,
    TransferFiles,
    AppCenter,
    Monitor,
    Terminal,
    Files(u32),
    Detached(arcnix_core::server_apps::DesktopApp, u32),
    Docker,
    Editor,
    Code,
    Player,
    ImageViewer,
    PdfReader,
    Office,
    Browser,
    Settings,
    PgAdmin,
}

impl WorkspacePage {
    fn files_window_id(self) -> Option<u32> {
        if let Self::Files(id) = self {
            Some(id)
        } else {
            None
        }
    }

    fn is_files(self) -> bool {
        self.files_window_id().is_some()
    }
}

// Keep the active workspace fields directly on State so existing bindings
// can borrow them together. The same list defines what moves with a server.
macro_rules! define_state {
    (
        |$config:ident, $local_path:ident, $notice:ident|
        session { $($(#[$session_attr:meta])* $field:ident: $ty:ty = $value:expr,)* }
        shared { $($shared:ident: $shared_ty:ty = $shared_value:expr,)* }
    ) => {
        #[derive(Clone)]
        struct State {
            $($(#[$session_attr])* $field: $ty,)*
            $($shared: $shared_ty,)*
        }

        #[derive(Clone)]
        struct ServerSession {
            $($(#[$session_attr])* $field: $ty,)*
        }

        impl State {
            fn new($config: AppConfig, $local_path: String, $notice: String) -> Self {
                Self {
                    $($(#[$session_attr])* $field: $value,)*
                    $($shared: $shared_value,)*
                }
            }

            fn swap_session(&mut self, session: &mut ServerSession) {
                $($(#[$session_attr])* std::mem::swap(&mut self.$field, &mut session.$field);)*
            }
        }

        impl ServerSession {
            fn new($local_path: String) -> Self {
                let $notice = String::new();
                Self { $($(#[$session_attr])* $field: $value,)* }
            }
        }
    };
}

define_state! {
    |config, local_path, notice|
    session {
        selected: Option<String> = None,
        local_desktop: Option<local_desktop::Target> = None,
        local_terminal_supported: bool = false,
        #[cfg(windows)]
        wsl: wsl_workspace::Workspace = Default::default(),
        dialog: Option<Dialog> = None,
        dialog_error: String = String::new(),
        file_menu: Option<bool> = None,
        file_context: Option<file_context::Target> = None,
        file_pending: Option<u64> = None,
        trash: [trash_ui::Bin; 2] = Default::default(),
        deletion: Option<file_context::Deletion> = None,
        file_elevation: Option<file_elevation::Prompt> = None,
        properties: Option<file_properties::Editor> = None,
        code: code_workbench::Runtime = Default::default(),
        remote_editor: Option<text_editor::Editor> = None,
        desktop_editor: text_editor::Tabs = Default::default(),
        office: office_reader::Reader = Default::default(),
        pdf: pdf_reader::Reader = Default::default(),
        pgadmin: pgadmin_ui::PgAdmin = Default::default(),
        images: image_viewer::Viewer = Default::default(),
        video: Option<video_player::Playback> = None,
        desktop_file_picker: Option<desktop_file_picker::Directory> = None,
        desktop_file_picker_sidebar_hidden: bool = false,
        activity_tab: u8 = 0,
        activity_open: bool = true,
        local_filter_open: bool = false,
        remote_filter_open: bool = false,
        notice: String = notice,
        secret: String = String::new(),
        remember_ssh_credential: bool = false,
        connecting_with_saved_credential: bool = false,
        connect_prompt_skipped: bool = false,
        connected: bool = false,
        sftp_available: bool = false,
        connecting: bool = false,
        keepalive_at: std::time::Instant = std::time::Instant::now(),
        keepalive_pending: bool = false,
        fingerprint: Option<String> = None,
        sync_scope: Option<String> = None,
        epoch: u64 = 0,
        local_request: u64 = 0,
        remote_request: u64 = 0,
        local_filter: String = String::new(),
        remote_filter: String = String::new(),
        queue_paused: bool = false,
        local_path: String = local_path,
        remote_path: String = ".".into(),
        local_ready: bool = false,
        remote_ready: bool = false,
        local: Vec<FileEntry> = vec![],
        remote: Vec<FileEntry> = vec![],
        local_selection: HashSet<String> = HashSet::new(),
        remote_selection: HashSet<String> = HashSet::new(),
        file_browser: file_browser::BrowserState = Default::default(),
        terminal: terminal::TerminalState = Default::default(),
        name: String = String::new(),
        cancels: HashMap<String, Arc<AtomicBool>> = HashMap::new(),
        files_uploads: files_transfer::Uploads = Default::default(),
        workspace_page: WorkspacePage = WorkspacePage::Desktop,
        workbench: server_desktop::Workbench = Default::default(),
        #[cfg(windows)]
        web_browser: web_browser::Browser = Default::default(),
        server_apps: server_apps_ui::DesktopState = Default::default(),
        docker: docker_ui::DockerState = Default::default(),
        disks: disks_ui::DiskState = Default::default(),
        compose: compose_ui::ComposeState = Default::default(),
        install: software_install_ui::InstallState = Default::default(),
        software: software::SoftwareState = Default::default(),
        operations: operations::OperationsState = Default::default(),
        metrics: observability::MetricState = Default::default(),
        ports: resource_ports_ui::PortState = Default::default(),
        system_logs: observability::LogsState = Default::default(),
        system_task_running: bool = false,
        file_split: f64 = 0.5,
        transfer_conflict: Option<transfer_ui::PendingConflict> = None,
        transfer_conflicts: std::collections::VecDeque<transfer_ui::PendingConflict> = Default::default(),
        queue_rules: HashMap<(String, TransferDirection), arcnix_core::transfer::ConflictAction> = HashMap::new(),
        session_rules: HashMap<TransferDirection, arcnix_core::transfer::ConflictAction> = HashMap::new(),
        worker: Option<(u64, Sender<Request>)> = None,
    }
    shared {
        config: AppConfig = config,
        group: Option<String> = None,
        collapsed_groups: HashSet<String> = HashSet::new(),
        search: String = String::new(),
        connected_servers_only: bool = false,
        editor: ServerProfile = ServerProfile::default(),
        server_editor_original: Option<ServerProfile> = None,
        server_tags: String = String::new(),
        server_editor_tab: u8 = 0,
        host_menu: Option<String> = None,
        group_menu: Option<String> = None,
        updates: updates_ui::UpdateState = Default::default(),
        import_pending: Option<u64> = None,
        group_name: String = String::new(),
        port: String = "22".into(),
        request_serial: u64 = 0,
        queue_filter: u8 = 0,
        import_path: String = String::new(),
        jobs: Vec<TransferJob> = vec![],
        transfer_pauses: HashMap<String, Arc<AtomicBool>> = HashMap::new(),
        inflight_transfers: HashSet<String> = HashSet::new(),
        logs: String = String::new(),
        navigation: navigation::Page = Default::default(),
        settings_section: navigation::SettingsSection = Default::default(),
        unread_notifications: usize = 0,
        scripts: scripts_ui::ScriptsState = Default::default(),
        task_records: task_records_ui::RecordState = Default::default(),
        record_storage: record_maintenance_ui::StorageState = Default::default(),
        cleanup_reports: cleanup_records_ui::ReportsState = Default::default(),
        backups: backups_ui::BackupState = Default::default(),
        settings: settings::SettingsState = Default::default(),
        system_dark: bool = true,
        transfer_details: HashMap<String, transfer_details::Details> = Default::default(),
        batch_ids: HashMap<String, String> = HashMap::new(),
        transfer_now: std::time::Instant = std::time::Instant::now(),
        transfer_meters: HashMap<String, transfer_progress::Meter> = HashMap::new(),
        transfer_summaries: HashMap<String, arcnix_core::transfer::TransferSummary> = HashMap::new(),
        parked: HashMap<Option<String>, ServerSession> = HashMap::new(),
        next_epoch: u64 = 0,
        background: bool = false,
    }
}
impl ServerSession {
    fn connection_active(&self) -> bool {
        #[cfg(windows)]
        if self.wsl.directory.is_some() {
            return true;
        }
        self.connected
    }
}

impl State {
    fn demo_read_only(&self) -> bool {
        self.local_desktop
            .as_ref()
            .is_some_and(local_desktop::Target::read_only)
    }
    fn advance_epoch(&mut self) {
        file_elevation::reset(self);
        self.next_epoch += 1;
        self.epoch = self.next_epoch;
    }

    fn connected_epochs(&self) -> Vec<u64> {
        let mut epochs = self
            .parked
            .values()
            .filter(|session| session.connection_active())
            .map(|session| session.epoch)
            .collect::<Vec<_>>();
        if self.connection_active() {
            epochs.push(self.epoch);
        }
        epochs.sort_unstable();
        epochs
    }

    fn active_connections(&self) -> usize {
        usize::from(self.connection_active())
            + self
                .parked
                .values()
                .filter(|s| s.connection_active())
                .count()
    }

    fn transfer_cancel(&self, id: &str) -> Option<&Arc<AtomicBool>> {
        self.cancels.get(id).or_else(|| {
            self.parked
                .values()
                .find_map(|session| session.cancels.get(id))
        })
    }

    fn local_wsl_selected(&self) -> bool {
        cfg!(windows)
            && selected_profile(self)
                .and_then(ServerProfile::direct_wsl)
                .is_some()
    }
    fn terminal_available(&self) -> bool {
        self.connection_active() && (self.local_desktop.is_none() || self.local_terminal_supported)
    }

    fn connection_active(&self) -> bool {
        #[cfg(windows)]
        if self.wsl.directory.is_some() {
            return true;
        }
        self.connected
    }
    fn server_connection(&self, id: &str) -> (bool, bool) {
        if self.selected.as_deref() == Some(id) {
            (self.connection_active(), self.connecting)
        } else {
            self.parked
                .get(&Some(id.to_owned()))
                .map(|session| (session.connection_active(), session.connecting))
                .unwrap_or_default()
        }
    }
}

#[derive(Clone)]
struct Ui {
    player: video_player::Player,
    update_controller: Arc<updates_ui::Controller>,
    transfer_limiter: Arc<transfer_worker::Limiter>,
    batch_controller: Rc<std::cell::RefCell<Option<batch_worker::Controller>>>,
    batch_events: async_channel::Sender<batch_worker::Event>,
    logger: Rc<client_logs::Logger>,
    state: Cage<State>,
    store: Rc<ConfigStore>,
    writable: Cage<bool>,
    scope: Scope,
    shell_focus: Cage<Option<glory::render::NodeId>>,
}
impl Ui {
    fn apply_conflict_rule(
        &self,
        job_id: &str,
        direction: TransferDirection,
        decision: worker::ConflictDecision,
    ) {
        use arcnix_core::transfer::ConflictAction;
        if decision.action == ConflictAction::Cancel {
            self.cancel_all();
            return;
        }
        self.state.revise(|s| match decision.scope {
            worker::ConflictScope::ThisFile => {}
            worker::ConflictScope::ThisQueue => {
                if let Some(batch) = s.batch_ids.get(job_id) {
                    s.queue_rules
                        .insert((batch.clone(), direction), decision.action);
                }
            }
            worker::ConflictScope::ThisSession | worker::ConflictScope::FutureConnections => {
                s.session_rules.insert(direction, decision.action);
            }
        });
        if decision.scope == worker::ConflictScope::FutureConnections {
            let action = match decision.action {
                ConflictAction::Overwrite => ExistingFileAction::Overwrite,
                _ => ExistingFileAction::Skip,
            };
            let result = self.state.revise(|s| {
                s.config.set_transfer_rule(
                    s.selected.as_deref().unwrap_or(""),
                    direction,
                    Some(action),
                )
            });
            if let Err(error) = result {
                self.error(error.to_string());
            } else {
                self.save();
            }
        }
    }
    fn reset_conflict_rules(&self, server_id: &str) {
        self.state.revise(|s| {
            s.config
                .transfer_rules
                .retain(|rule| rule.server_id != server_id);
            if s.selected.as_deref() == Some(server_id) {
                s.session_rules.clear();
                s.queue_rules.clear();
            } else if let Some(session) = s.parked.get_mut(&Some(server_id.to_owned())) {
                session.session_rules.clear();
                session.queue_rules.clear();
            }
            s.host_menu = None;
        });
        self.save();
        self.log("该服务器的文件冲突处理已恢复为全局默认设置");
    }
    fn select_server(&self, id: String, connect_now: bool) {
        if self.state.with(|s| {
            s.selected.as_ref() != Some(&id)
                && (s.system_task_running || !s.config.available_servers().any(|p| p.id == id))
        }) {
            return;
        }
        if self.state.with(|s| s.selected.as_ref() != Some(&id)) {
            self.switch_session(Some(id));
        }
        self.state.revise(|s| {
            navigation::change_page(s, navigation::Page::Servers, "select-server");
            dismiss_menus(s);
        });
        self.desktop_enter();
        if connect_now && self.state.with(|s| !s.connection_active() && !s.connecting) {
            self.request_connect();
        }
    }

    fn enter_local_desktop(&self, target: local_desktop::Target) {
        if self.state.with(|s| s.system_task_running) {
            return;
        }
        let scope = target.scope();
        let prefs = match self.store.demo_preferences(&scope) {
            Ok(path) => path,
            Err(error) => {
                self.error(format!("{error:#}"));
                return;
            }
        };
        let terminal_supported = terminals::check_local_terminal(&target).is_ok();
        let session_scope = if target.read_only() {
            format!("{scope}-readonly")
        } else {
            scope.clone()
        };
        let mut id = format!("@{session_scope}");
        while self
            .state
            .with(|s| s.config.servers.iter().any(|p| p.id == id))
        {
            id.push('@');
        }
        if self.state.with(|s| s.selected.as_ref() != Some(&id)) {
            self.switch_session(Some(id));
        }
        let connect = self.state.revise(|s| {
            navigation::change_page(s, navigation::Page::Servers, "local-desktop");
            s.dialog = None;
            dismiss_menus(s);
            server_desktop::select_tab(s, server_desktop::Tab::Desktop);
            s.local_desktop = Some(target.clone());
            if s.connected || s.connecting {
                return false;
            }
            s.local_terminal_supported = terminal_supported;
            s.advance_epoch();
            s.sync_scope = Some(session_scope.clone());
            s.connecting = true;
            s.notice.clear();
            true
        });
        if connect {
            self.send(Action::LocalDesktop(target, prefs));
        } else {
            self.desktop_enter();
        }
    }

    fn switch_session(&self, target: Option<String>) {
        let fresh = self.state.revise(|s| {
            let saved = s.parked.remove(&target);
            let fresh = saved.is_none();
            let mut incoming = saved.unwrap_or_else(|| {
                let profile = s
                    .config
                    .servers
                    .iter()
                    .find(|p| Some(&p.id) == target.as_ref());
                let location = profile
                    .and_then(|p| {
                        p.locations
                            .remember_paths
                            .then_some(p.locations.last_local.as_ref())
                            .flatten()
                            .or(p.locations.local_directory.as_ref())
                    })
                    .or(s.config.local_directory.as_ref());
                let mut session = ServerSession::new(
                    location
                        .map(|path| local_display(path))
                        .unwrap_or_else(|| s.local_path.clone()),
                );
                session.file_split =
                    f64::from(profile.and_then(|p| p.file_pane_ratio).unwrap_or(500)) / 1000.0;
                session.selected = target;
                session
            });
            observability::metrics_leave(s);
            s.terminal.menu = None;
            s.file_context = None;
            s.file_menu = None;
            s.file_browser.cancel_drag();
            if let Some(prompt) = &mut s.file_elevation {
                prompt.clear_password();
            }
            s.swap_session(&mut incoming);
            s.code.viewport = None;
            s.office.viewport = None;
            #[cfg(windows)]
            {
                s.web_browser.viewports.clear();
            }
            s.parked.insert(incoming.selected.clone(), incoming);
            fresh
        });
        if fresh {
            self.refresh(false);
        }
    }

    // Replies run synchronously on the UI thread. Restore the selected
    // workspace before the renderer processes the queued state changes.
    fn with_parked(&self, key: &Option<String>, run: impl FnOnce()) {
        let Some(mut session) = self.state.revise(|s| {
            let mut session = s.parked.remove(key)?;
            s.swap_session(&mut session);
            s.background = true;
            Some(session)
        }) else {
            return;
        };
        run();
        self.state.revise(|s| {
            s.swap_session(&mut session);
            s.parked.insert(key.clone(), session);
            s.background = false;
        });
    }

    fn tick_parked(&self) {
        let due = self.state.with(|s| {
            s.parked
                .iter()
                .filter_map(|(key, session)| {
                    if !session.connected
                        || session.keepalive_pending
                        || session.system_task_running
                        || session.file_pending.is_some()
                        || !session.cancels.is_empty()
                    {
                        return None;
                    }
                    let seconds = preferences_ui::keepalive_seconds(&s.config, key.as_deref());
                    (seconds > 0 && session.keepalive_at.elapsed().as_secs() >= u64::from(seconds))
                        .then(|| key.clone())
                })
                .collect::<Vec<_>>()
        });
        for key in due {
            self.with_parked(&key, || preferences_ui::keepalive(self));
        }
    }

    fn discard_parked(&self, key: &Option<String>) {
        if self
            .state
            .with(|s| s.parked.get(key).is_some_and(|p| p.worker.is_some()))
        {
            self.with_parked(key, || self.disconnect());
        }
        self.state.revise(|s| s.parked.remove(key));
    }

    fn disconnect_server(&self, id: String) {
        if self.state.with(|s| s.selected.as_ref() == Some(&id)) {
            self.disconnect();
        } else {
            self.with_parked(&Some(id), || self.disconnect());
        }
    }
    fn save_file_split(&self, ratio: f64) {
        let ratio = if ratio.is_finite() {
            ratio.clamp(0.2, 0.8)
        } else {
            0.5
        };
        let saved = self.state.revise(|s| {
            s.file_split = ratio;
            s.selected.clone().is_some_and(|id| {
                s.config
                    .set_file_pane_ratio(&id, (ratio * 1000.0).round() as u16)
                    .is_ok()
            })
        });
        if saved {
            self.save();
        }
    }
    fn file_task_blocks_dialog(&self) -> bool {
        let busy = self.state.with(file_task_busy);
        if busy {
            self.error("文件任务仍在处理当前请求，请稍后重试。");
        }
        busy
    }
    fn open_dialog(&self, dialog: Dialog) {
        if self.file_task_blocks_dialog() {
            return;
        }
        self.state.revise(|s| show_dialog(s, dialog));
    }
    fn close_dialog(&self) {
        if self.state.with(|s| matches!(&s.dialog, Some(Dialog::DesktopTask(id)) if s.scripts.active.as_ref() == Some(id) && !s.scripts.submitted)) {
            self.stop_script_queue();
        }
        let terminal_rename = self
            .state
            .with(|s| matches!(s.dialog, Some(Dialog::TerminalRename { .. })));
        if file_elevation::dismiss(self)
            || text_editor::dismiss(self)
            || file_context::dismiss_deletion(self)
        {
            return;
        }
        if file_properties::dismiss(self) {
            return;
        }
        if self
            .state
            .with(|s| s.dialog == Some(Dialog::TransferConflict))
            && let Some(pending) = self.state.revise(|s| s.transfer_conflict.take())
        {
            self.cancel_all();
            let _ = pending.reply.send(worker::ConflictDecision {
                action: arcnix_core::transfer::ConflictAction::Cancel,
                scope: worker::ConflictScope::ThisFile,
            });
        }
        self.state.revise(|s| {
            if s.file_pending.is_some() || (!s.background && s.import_pending.is_some()) {
                return;
            }
            if s.background {
                s.dialog = None;
                s.dialog_error.clear();
                return;
            }
            backups_ui::dismiss(s);
            resource_ports_ui::dismiss(s);
            #[cfg(windows)]
            if s.dialog == Some(Dialog::BrowserForwardingRepair) {
                web_browser::cancel_forwarding_review(s);
            }
            if matches!(s.dialog, Some(Dialog::ScriptImport | Dialog::ScriptExport)) {
                script_exchange_ui::forget(s);
            }
            desktop_file_picker::dismiss(s);
            cleanup_records_ui::dismiss(s);
            record_maintenance_ui::dismiss(s);
            if s.dialog == Some(Dialog::Connect) {
                s.secret.clear();
            }
            if let Some(Dialog::ScriptRefresh(id, _, true)) = &s.dialog {
                s.scripts.refresh_secret.clear();
                s.dialog = Some(Dialog::DesktopTask(id.clone()));
                s.dialog_error.clear();
                return;
            }
            if matches!(s.dialog, Some(Dialog::ScriptRefresh(..))) {
                s.scripts.refresh_secret.clear();
            }
            task_refresh_ui::dismiss(s);
            task_records_ui::dismiss(s);
            if matches!(
                s.dialog,
                Some(Dialog::ConfigurationBackup | Dialog::ConfigurationRestore)
            ) {
                configuration_backup_ui::forget(s);
            }
            if s.dialog == Some(Dialog::ScriptRun) {
                s.scripts.credentials.clear();
                s.scripts.values.clear();
                s.scripts.preset_selected.clear();
                s.scripts.preset_name.clear();
            }
            if matches!(
                s.dialog,
                Some(Dialog::SoftwareInstall | Dialog::SoftwareAction(..))
            ) {
                s.scripts.credentials.clear();
            }
            if s.dialog == Some(Dialog::TaskRecordConnect) {
                s.task_records.secrets.clear();
            }
            if s.dialog == Some(Dialog::SnippetEdit) {
                s.operations.editor = None;
                s.scripts.parameter_editor = Default::default();
            }

            if s.dialog == Some(Dialog::FileProperties) {
                s.properties = None;
            }
            s.dialog = None;
            s.dialog_error.clear();
        });
        if self.state.with(|s| !s.background && s.dialog.is_none())
            && let Some(id) = self.shell_focus.get()
        {
            let _ = self.scope.action(glory::render::Action::Focus(id));
        }
        if self
            .state
            .with(|s| s.dialog.is_none() && s.transfer_conflict.is_some())
        {
            self.open_dialog(Dialog::TransferConflict);
        }
        if terminal_rename
            && self.state.with(|s| {
                s.dialog.is_none()
                    && navigation::workspace_visible(s)
                    && s.workspace_page.base() == WorkspacePage::Terminal
            })
        {
            let id = self.state.with(|s| s.terminal.id);
            self.terminal_ui_command(id, "focus");
        }
    }
    fn error(&self, message: impl AsRef<str>) {
        self.state
            .revise(|s| s.notice = message.as_ref().to_string());
        self.log(message);
    }
    fn log(&self, message: impl AsRef<str>) {
        let mut message = i18n::translate(message.as_ref());
        let settings = self.state.with(|s| {
            if !s.secret.is_empty() {
                message = message.replace(s.secret.as_str(), "[redacted]");
            }
            s.config.settings.logging.clone()
        });
        if settings.timestamps {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            message = format!("[{}] {message}", formatting::timestamp(Some(now)));
        }
        self.logger.record(message.clone(), &settings);
        self.state.revise(|s| {
            if !s.logs.is_empty() && !s.logs.ends_with('\n') {
                s.logs.push('\n');
            }
            if s.navigation != navigation::Page::Notifications {
                s.unread_notifications = s.unread_notifications.saturating_add(1);
            }
            s.logs.push_str(&message);
            s.logs.push('\n');
            if s.logs.len() > 60000 {
                let boundary = s
                    .logs
                    .char_indices()
                    .map(|(i, _)| i)
                    .find(|i| *i >= 20000)
                    .unwrap_or(0);
                s.logs.drain(..boundary);
            }
        });
    }
    fn send(&self, action: Action) -> u64 {
        let (epoch, serial, worker, server) = self.state.revise(|s| {
            s.request_serial += 1;
            (
                s.epoch,
                s.request_serial,
                s.worker.clone(),
                s.selected.clone(),
            )
        });
        let tx = if let Some((_, tx)) = worker {
            tx
        } else {
            let (tx, rx) = worker::start_with_limiter(self.transfer_limiter.clone());
            let ui = self.clone();
            if self
                .scope
                .spawn(async move {
                    while let Ok(response) = rx.recv().await {
                        ui.route_worker_response(&server, serial, response);
                    }
                })
                .is_err()
            {
                self.log("后台任务已停止");
                return serial;
            }
            self.state.revise(|s| s.worker = Some((serial, tx.clone())));
            tx
        };
        if tx
            .send(Request {
                epoch,
                serial,
                action,
            })
            .is_err()
        {
            self.log("后台任务已停止");
        }
        serial
    }

    fn route_worker_response(
        &self,
        server: &Option<String>,
        worker_id: u64,
        response: worker::Response,
    ) {
        let finished = if let Update::Finished(id, _) = &response.update {
            self.state.revise(|s| s.inflight_transfers.remove(id))
        } else {
            false
        };
        let (active, parked) = self.state.with(|s| {
            (
                s.worker.as_ref().is_some_and(|(id, _)| *id == worker_id),
                s.parked
                    .get(server)
                    .and_then(|session| session.worker.as_ref())
                    .is_some_and(|(id, _)| *id == worker_id),
            )
        });
        if active {
            self.worker_response(response);
        } else if parked {
            self.with_parked(server, || self.worker_response(response));
        }
        if finished {
            self.schedule();
        }
    }

    #[rustfmt::skip]
    fn worker_response(&self, response: worker::Response) {
        let state = self.state;
        let events = self;
        if !accepts_response(state.with(|s|s.epoch),response.epoch) && !matches!(&response.update, Update::Children {remote:false,..}|Update::Trash{..}|Update::Deleted{..}|Update::Permissions(_)|Update::CredentialNotice(_)){return;}
        match response.update {
            #[cfg(windows)]
            Update::WslFiles(result)=>wsl_workspace::receive(events,response.serial,result),
            Update::PgAdmin(reply)=>pgadmin_ui::reply(events,response.serial,reply),
            Update::OfficeSaved(result)=>office_reader::saved(events,response.serial,result),
            Update::Office(result)=>office_reader::received(events,response.serial,result),
            Update::Pdf(event)=>pdf_reader::received(events,response.serial,event),
            Update::Video(result)=>events.player.ready(events,response.serial,result),
            #[cfg(windows)]
            Update::Html(result)=>web_browser::html_result(events,response.serial,result),
            Update::Alive=>state.revise(|s|s.keepalive_pending=false),
            Update::ServerApps(result)=>server_apps_ui::event(events,response.serial,*result),
            Update::Operations(event)=>operations::event(events,response.serial,event),
            Update::Terminal(event)=>terminal::update(events,event),
            Update::Fingerprint(fingerprint)=>{
                let known=state.with(|s|s.config.servers.iter().find(|p|Some(&p.id)==s.selected.as_ref()).and_then(|p|s.config.trusted_fingerprint(&p.host,p.port)).map(str::to_owned));
                if known.as_ref().is_some_and(|f|f!=&fingerprint){
                    events.disconnect();events.log(format!("拒绝连接：主机密钥发生变化。已保存：{}；当前：{fingerprint}。请通过可信渠道核实后手动更新配置。",known.unwrap()));
                }else{state.revise(|s|s.fingerprint=Some(fingerprint));if known.is_some(){events.authenticate(false);}}
            }
            Update::LocalDesktop(result)=>match result {
                Ok(home)=>{state.revise(|s| {s.connected=true;s.connecting=false;s.sftp_available=true;s.remote_path=home;});events.server_apps_connected();events.refresh(true);events.desktop_enter();}
                Err(message)=>{state.revise(|s|{s.connecting=false;s.notice=message;});}
            },
            Update::Connected(sftp,address)=>{state.revise(|s|{s.server_apps.connected_address=address;s.connected=true;s.connecting=false;s.connecting_with_saved_credential=false;s.keepalive_pending=false;s.sftp_available=sftp;if s.workbench.active==server_desktop::Tab::Home{server_desktop::select_tab(s,server_desktop::Tab::Desktop);}});
                #[cfg(windows)]
                if state.with(|s| server_desktop::launcher_running(s, WorkspacePage::Browser)) { events.open_web_browser(); }
                events.log("SSH 认证成功");events.server_apps_connected();if sftp{events.refresh(true);events.files_enter();}else{events.log("服务器未启用 SFTP；SSH 系统软件管理仍可使用。");}events.terminal_enter();events.pgadmin_enter();events.observability_enter();if state.with(|s|s.workspace_page==WorkspacePage::AppCenter){events.software_detect();}}
            Update::Disconnected=>events.log("连接已断开"),
            Update::Local(path,entries)=>{if !accepts_listing(state.with(|s|s.local_request),response.serial){return;}state.revise(|s|{s.local_path=local_display(&path);s.local_ready=true;s.local=entries;file_browser::listed(s,false);if !s.background{s.config.local_directory=Some(path.clone());};if let Some(profile)=s.config.servers.iter_mut().find(|p|Some(&p.id)==s.selected.as_ref())&& profile.locations.remember_paths{profile.locations.last_local=Some(path);}});events.save();}
            Update::Remote(path,entries)=>{if !accepts_listing(state.with(|s|s.remote_request),response.serial){return;}state.revise(|s|{file_browser::remote_listed(s,&path);s.remote_path=path.clone();if let Some(profile)=s.config.servers.iter_mut().find(|p|Some(&p.id)==s.selected.as_ref())&& profile.locations.remember_paths{profile.locations.last_remote=Some(path);}s.remote_ready=true;s.remote=entries;file_browser::listed(s,true);});events.save();},
            Update::Children{remote:true,result,..} if state.with(|s|s.images.directory_request == Some(response.serial)) =>
                state.revise(|s|image_viewer::listed(s,result.unwrap_or_default())),
            Update::Children{remote:true,result,..} if desktop_file_picker::awaiting_directory(events, response.serial) =>
                desktop_file_picker::listed(events,result),
            Update::Children{remote:true,parent,result} if files_tabs::awaiting_refresh(events, response.serial) =>
                files_tabs::refreshed(events, response.serial, &parent, result),
            Update::Children{remote,parent,result}=>file_browser::accept_children(events,remote,parent,response.serial,result),
            Update::BrowseError{remote,message}=>{let latest=state.with(|s|if remote{s.remote_request}else{s.local_request});if accepts_listing(latest,response.serial){if remote{state.revise(file_browser::remote_browse_failed);}events.error(format!("读取目录失败：{message}"));}},
            #[cfg(windows)]
            Update::BrowserProxy(result)=>web_browser::proxy_event(events,response.serial,result),
            #[cfg(windows)]
            Update::BrowserDiagnosis(diagnosis)=>web_browser::diagnosis_event(events,response.serial,diagnosis),
            #[cfg(windows)]
            Update::BrowserForwardingRepaired(result)=>web_browser::forwarding_repaired(events,response.serial,result),
            Update::ConnectionLost(message)=>{
                state.revise(|s| {
                    s.images = Default::default();
                    s.pdf = Default::default();
                    s.office = Default::default();
                    desktop_file_picker::dismiss(s);
                    file_elevation::reset(s);
                    if matches!(s.dialog, Some(Dialog::DesktopTimeZone | Dialog::DesktopIcons | Dialog::DockShortcuts | Dialog::Launcher | Dialog::SystemPower(..))) {
                        s.dialog = None;
                    }
                });
                events.player.close(events);
                #[cfg(windows)]
                events.close_web_browser();operations::connection_lost(events);terminal::disconnected(events);software::on_connection_lost(events,&message);state.revise(|s|{s.metrics=Default::default();resource_ports_ui::reset(s);s.system_logs=Default::default();s.session_rules.clear();s.queue_rules.clear();s.transfer_conflict=None;s.transfer_conflicts.clear();s.files_uploads.abort();for (id,c) in &s.cancels{c.store(true,Ordering::Relaxed);s.transfer_pauses.remove(id);}if s.dialog==Some(Dialog::TransferConflict){s.dialog=None;}s.connected=false;s.trash[1]=Default::default();s.file_browser.reset(true);s.code=Default::default();s.sync_scope=None;s.server_apps=Default::default();s.docker=Default::default();pgadmin_ui::reset(s);s.disks=Default::default();s.compose=Default::default();s.install=Default::default();s.workbench.fullscreen=false;s.connecting=false;s.remote_ready=false;s.remote.clear();s.remote_selection.clear();s.remote_request=0;s.fingerprint=None;for j in &mut s.jobs {if Some(&j.server_id)==s.selected.as_ref() && j.status==TransferStatus::Queued{j.status=TransferStatus::Cancelled;}}});events.log(format!("SSH 连接已断开：{message}"));},
            Update::TransferConflict(id,info,reply)=>transfer_ui::show(events,id,info,reply),
            Update::TransferPlan(id,plan)=>state.revise(|s|transfer_details::plan(s,&id,&plan)),
            Update::TransferFile(id,progress)=>state.revise(|s|{transfer_details::update(s,&id,progress);s.transfer_now=std::time::Instant::now();let waiting=transfer_details::waiting(s,&id)||workspace::transfer_paused(s,&id);if let Some(m)=s.transfer_meters.get_mut(&id){if waiting{m.pause(s.transfer_now);}else{m.resume(s.transfer_now);}}}),
            Update::TransferReport(id,summary)=>{events.log(format!("传输结果：写入 {} 个文件，跳过 {} 个文件",summary.transferred_files,summary.skipped_files));state.revise(|s|{s.transfer_summaries.insert(id,summary);});},
            Update::Progress(id,done,total,actual)=>state.revise(|s|{s.transfer_now=std::time::Instant::now();if let Some(m)=s.transfer_meters.get_mut(&id){m.observe(actual,s.transfer_now);}
                if let Some(job)=s.jobs.iter_mut().find(|j|j.id==id){job.transferred_bytes=done;job.total_bytes=total;}}),
            Update::Finished(id,result)=>{
                transfer_ui::finished(events,&id);
                if let Err(error)=&result{events.log(format!("传输未完成：{error}"));}
                state.revise(|s|{
                    s.transfer_now=std::time::Instant::now();
                    if let Some(m)=s.transfer_meters.get_mut(&id){m.finish(s.transfer_now);}
                    s.transfer_pauses.remove(&id);
                    let cancelled=s.cancels.remove(&id).is_some_and(|c|c.load(Ordering::Relaxed));
                    if let Some(job)=s.jobs.iter_mut().find(|j|j.id==id){job.status=finished_status(result,cancelled);let status=job.status.clone();s.files_uploads.finish(&id,&status);transfer_details::finish(s,&id,&status);}
                });
                if state.with(|s|s.cancels.is_empty()) {events.refresh(false);if state.with(|s|s.connected){events.refresh(true);}}
            }
            Update::Created{remote,name,directory}=>{
                events.log(match (remote,directory) {
                    (true,true)=>"远程目录已创建", (true,false)=>"远程文件已创建",
                    (false,true)=>"本地目录已创建", (false,false)=>"本地文件已创建",
                });
                file_browser::refresh_changed_entry(events,remote,response.serial,file_browser::NewEntryFocus::Edit(name));
            }
            Update::Renamed{remote,name}=>{
                events.log(if remote {"远程文件已重命名"} else {"本地文件已重命名"});
                file_browser::refresh_changed_entry(events,remote,response.serial,file_browser::NewEntryFocus::Select(name));
            }
            Update::Trash{remote,result}=>trash_ui::completed(events,response.serial,remote,result),
            Update::FileAdminRequired(retry)=>file_actions::offer_retry(events,response.serial,retry),
            Update::ManagedRemoteFiles(result)=>file_actions::completed(events,response.serial,result),
            Update::FileAlias(result)=>file_actions::alias_resolved(events,response.serial,result),
            Update::Deleted{remote,result}=>file_context::deleted(events,response.serial,remote,result),
            Update::DeletionProgress(progress)=>file_context::deletion_progress(events,response.serial,progress),
            Update::FileMetadata(result)=>file_properties::metadata(events,response.serial,result),
            Update::Permissions(result)=>file_properties::finished(events,response.serial,result),
            Update::CodeFile(reply)=>state.revise(|s|code_workbench::reply(s,response.serial,reply)),
            Update::TextFile(reply) if state.with(|s|s.code.pending.contains_key(&response.serial))=>state.revise(|s|code_workbench::document_reply(s,response.serial,reply)),
            Update::TextFile(reply)=>text_editor::event(events,response.serial,reply),
            Update::Error(message)=>{
                if file_browser::mutation_failed(events,response.serial,&message) {return;}
                let pending=state.with(|s|s.file_pending==Some(response.serial)||(!s.background&&s.import_pending==Some(response.serial)));
                if pending {state.revise(|s|{if s.file_pending==Some(response.serial){s.file_pending=None;};if s.import_pending==Some(response.serial){s.import_pending=None;}s.dialog_error=message.clone();});events.log(format!("操作失败：{message}"));}
                else {events.error(format!("错误：{message}"));}
            },
            Update::ConnectionError(message)=>events.connection_failed(Some(&format!("连接失败：{message}")),false),
            Update::CredentialPrompt(detail)=>events.connection_failed(detail,true),
            Update::CredentialNotice(message)=>events.error(message),
            Update::Metrics(result)=>observability::metrics_event(events,response.serial,result),
            Update::Ports(result)=>resource_ports_ui::event(events,response.serial,result),
            Update::PortTerminated(result)=>resource_ports_ui::terminated(events,response.serial,result),
            Update::LogCapabilities(result)=>observability::logs_capabilities_event(events,response.serial,result),
            Update::SystemLogs(result)=>observability::logs_event(events,response.serial,result),
            Update::PackageEnvironment(result)=>software::event(events,response.serial,software::Event::Environment(result)),
            Update::PackageList(result)=>software::event(events,response.serial,software::Event::Packages(result)),
            Update::Imported(report)=>{
                let warning_count=report.warnings.len();
                for warning in &report.warnings {events.log(format!("SSH 配置导入：{warning}"));}
                let result=state.with(|s|{let (config,imported,skipped)=import_profiles(&s.config,report.servers)?;anyhow::ensure!(events.writable.get(),"配置读取失败，禁止覆盖");events.store.save(&config)?;Ok::<_,anyhow::Error>((config,imported,skipped))});
                state.revise(|s|s.import_pending=None);
                match result {
                    Ok((config,imported,skipped))=>{
                        let summary=format!("导入完成：新增 {imported} 台，跳过 {skipped} 台重复站点；{warning_count} 条提示");
                        state.revise(|s|{s.config=config;s.dialog=None;s.dialog_error.clear();s.notice=if warning_count==0{String::new()}else{format!("{summary}。{}",report.warnings[0])};});events.log(summary);
                    },
                    Err(e)=>{state.revise(|s|s.dialog_error=format!("导入失败：{e:#}"));events.log(format!("导入失败：{e:#}"));}
                }
            }
        }
    }
    fn retry(&self, id: Option<String>) {
        if !self.state.with(|s| s.connected) {
            self.log("请先连接原服务器再重试传输");
            return;
        }
        self.state.revise(|s| {
            let active = s.cancels.keys().cloned().collect();
            let previous: Vec<_> = s.jobs.iter().map(|job| job.id.clone()).collect();
            browser::retry(&mut s.jobs, &active, s.selected.as_deref(), id.as_deref());
            let changes: Vec<_> = previous
                .into_iter()
                .zip(s.jobs.iter().map(|job| job.id.clone()))
                .filter(|(old, new)| old != new)
                .collect();
            for (old, new) in changes {
                transfer_details::reset(s, &old, &new);
                if let Some(batch) = s.batch_ids.remove(&old) {
                    s.batch_ids.insert(new.clone(), batch);
                }
                s.files_uploads.retry(&old, new);
            }
            s.transfer_meters
                .retain(|id, _| s.jobs.iter().any(|j| &j.id == id));
        });
        self.schedule();
    }
    fn save(&self) {
        if !self.writable.get() {
            self.log("配置读取失败，已禁止覆盖。请修复配置文件后重新启动。");
            return;
        }
        let config = self.state.with(|s| s.config.clone());
        if let Err(e) = self.store.save(&config) {
            self.log(format!("保存失败: {e:#}"));
        }
    }
    fn request_connect(&self) {
        if self.state.with(|s| s.local_desktop.is_some()) {
            self.open_dialog(Dialog::LocalDemo);
            return;
        }
        #[cfg(windows)]
        if self.state.with(State::local_wsl_selected) {
            wsl_workspace::connect(self);
            return;
        }
        if self.file_task_blocks_dialog() {
            return;
        }
        let try_saved = self.state.with(|s| {
            ssh_credentials::SUPPORTED
                && !s.connected
                && !s.connecting
                && selected_profile(s).is_some_and(|profile| {
                    !needs_no_credentials(profile)
                        && s.config
                            .trusted_fingerprint(&profile.host, profile.port)
                            .is_some()
                })
        });
        if !try_saved && self.state.with(connect_prompt_needed) {
            self.open_dialog(Dialog::Connect);
            return;
        }
        self.state.revise(|s| {
            s.secret.clear();
            s.remember_ssh_credential = false;
            dismiss_menus(s);
        });
        self.connect(false, try_saved);
    }
    /// If the connection starts before a prompt, allow manual entry on failure.
    fn connect(&self, prompted: bool, try_saved: bool) {
        if self.state.with(|s| s.system_task_running) {
            self.error("系统软件任务正在执行，请等待完成后切换服务器");
            return;
        }
        let profile = self.state.with(|s| {
            s.config
                .available_servers()
                .find(|p| Some(&p.id) == s.selected.as_ref())
                .cloned()
        });
        if let Some(mut profile) = profile {
            if profile.connection.is_none() {
                profile.connection = Some(self.state.with(|s| s.config.settings.connection));
            }
            self.cancel_all();
            self.player.close(self);
            #[cfg(windows)]
            self.close_web_browser();
            self.state.revise(|s| {
                s.advance_epoch();
                s.cancels.clear();
                s.connected = false;
                s.connecting = true;
                s.connect_prompt_skipped = !prompted;
                s.connecting_with_saved_credential = try_saved;
                s.sync_scope = None;
                s.terminal = Default::default();
                s.fingerprint = None;
                s.remote_ready = false;
                s.remote.clear();
                s.remote_selection.clear();
                s.file_browser.reset_session();
                s.trash[1] = Default::default();
                s.remote_path = if profile.locations.remember_paths {
                    profile.locations.last_remote.clone()
                } else {
                    None
                }
                .unwrap_or_else(|| profile.initial_remote_path.clone());
                s.software = Default::default();
                s.server_apps = Default::default();
                s.docker = Default::default();
                pgadmin_ui::reset(s);
                s.disks = Default::default();
                s.compose = Default::default();
                s.install = Default::default();
                s.operations = Default::default();
                s.metrics = Default::default();
                resource_ports_ui::reset(s);
                s.system_logs = Default::default();
            });
            let known = self.state.with(|s| {
                s.config
                    .trusted_fingerprint(&profile.host, profile.port)
                    .map(str::to_owned)
            });
            self.send(match known {
                Some(fingerprint) if try_saved => Action::ProbeSaved(profile.into(), fingerprint),
                known => Action::Probe(profile.into(), known),
            });
        } else {
            self.log("请先新建并选择服务器");
        }
    }
    fn disconnect(&self) {
        text_editor::cancel(self);
        file_context::cancel_deletion(self);
        file_properties::cancel(self);
        if self.state.with(|s| s.system_task_running) {
            self.error("系统软件任务正在执行，请等待完成后断开连接");
            return;
        }
        self.cancel_all();
        self.player.close(self);
        #[cfg(windows)]
        self.close_web_browser();
        if self.state.with(|s| {
            matches!(
                s.dialog,
                Some(
                    Dialog::TransferConflict
                        | Dialog::DesktopTimeZone
                        | Dialog::DesktopIcons
                        | Dialog::DockShortcuts
                )
            )
        }) {
            self.close_dialog();
        }
        self.state.revise(|s| {
            desktop_file_picker::dismiss(s);
            #[cfg(windows)]
            {
                s.wsl.cancel();
                s.wsl = Default::default();
            }
            s.advance_epoch();
            s.terminal = Default::default();
            s.cancels.clear();
            s.connected = false;
            s.sftp_available = false;
            s.sync_scope = None;
            s.connecting = false;
            s.keepalive_pending = false;
            s.fingerprint = None;
            s.secret.clear();
            s.remember_ssh_credential = false;
            s.connecting_with_saved_credential = false;
            s.remote_ready = false;
            s.remote.clear();
            s.remote_selection.clear();
            s.remote_request = 0;
            s.file_browser.reset_session();
            s.trash[1] = Default::default();
            s.software = Default::default();
            s.server_apps = Default::default();
            s.docker = Default::default();
            pgadmin_ui::reset(s);
            s.disks = Default::default();
            s.compose = Default::default();
            s.install = Default::default();
            s.code = Default::default();
            s.images = Default::default();
            s.pdf = Default::default();
            s.office = Default::default();
            text_editor::disconnect(s);
            s.operations = Default::default();
            s.metrics = Default::default();
            resource_ports_ui::reset(s);
            s.system_logs = Default::default();
            s.session_rules.clear();
            s.queue_rules.clear();
            s.workbench.reset_open_windows();
            s.workbench.fullscreen = false;
            s.workspace_page = if s.workbench.active == server_desktop::Tab::TransferFiles {
                WorkspacePage::TransferFiles
            } else {
                WorkspacePage::Desktop
            };
        });
        self.send(Action::Disconnect);
    }
    fn cancel_all(&self) {
        self.state.revise(|s| {
            s.transfer_now = std::time::Instant::now();

            s.files_uploads.abort();
            for (id, cancel) in &s.cancels {
                cancel.store(true, Ordering::Relaxed);
                s.transfer_pauses.remove(id);
                if let Some(meter) = s.transfer_meters.get_mut(id) {
                    meter.finish(s.transfer_now);
                }
            }
            for j in &mut s.jobs {
                if Some(&j.server_id) == s.selected.as_ref()
                    && matches!(j.status, TransferStatus::Queued | TransferStatus::Running)
                {
                    j.status = TransferStatus::Cancelled;
                }
            }
            let cancelled: Vec<_> = s
                .jobs
                .iter()
                .filter(|job| {
                    Some(&job.server_id) == s.selected.as_ref()
                        && job.status == TransferStatus::Cancelled
                })
                .map(|job| job.id.clone())
                .collect();
            for id in cancelled {
                s.transfer_pauses.remove(&id);
                transfer_details::finish(s, &id, &TransferStatus::Cancelled);
            }
            s.transfer_conflict = None;
            s.transfer_conflicts.clear();
        });
    }
    fn authenticate(&self, save_trust: bool) {
        let details = self.state.with(|s| {
            let profile = selected_profile(s)?.clone();
            let fingerprint = s.fingerprint.clone()?;
            let known =
                s.config.trusted_fingerprint(&profile.host, profile.port) == Some(&fingerprint);
            Some((profile, fingerprint, known))
        });
        let Some((profile, fingerprint, mut trusted)) = details else {
            return;
        };
        if save_trust {
            let mut config = self.state.with(|s| s.config.clone());
            let result = config
                .trust_host(&profile.host, profile.port, &fingerprint)
                .and_then(|()| {
                    anyhow::ensure!(self.writable.get(), "配置读取失败，禁止覆盖");
                    self.store.save(&config)
                });
            match result {
                Ok(()) => {
                    self.state.revise(|s| s.config = config);
                    trusted = true;
                }
                Err(error) => self.error(error.to_string()),
            }
        }
        if self.state.with(|s| s.connecting_with_saved_credential) && !trusted {
            self.disconnect();
            self.open_dialog(Dialog::Connect);
            return;
        }
        let wants_to_remember = self.state.with(|s| s.remember_ssh_credential);
        if wants_to_remember && !trusted {
            self.error(
                "Save the SSH host fingerprint to remember the credential on this computer.",
            );
        }
        let login = self.state.revise(|s| {
            s.sync_scope = Some(arcnix_core::browser::profile_key(&profile, &fingerprint));
            s.fingerprint = None;
            let secret = zeroize::Zeroizing::new(std::mem::take(&mut s.secret));
            s.remember_ssh_credential = false;
            if s.connecting_with_saved_credential {
                worker::Login::Saved
            } else {
                worker::Login::Manual {
                    secret,
                    remember: ssh_credentials::SUPPORTED && wants_to_remember && trusted,
                }
            }
        });
        self.send(Action::Authenticate { login, fingerprint });
    }
    fn forget_changed_credentials(&self, replacement: &AppConfig) {
        if !ssh_credentials::SUPPORTED {
            return;
        }
        let keys = self.state.with(|s| {
            let retained: HashSet<_> = replacement
                .servers
                .iter()
                .filter_map(|p| ssh_credentials::Key::for_profile(replacement, p))
                .collect();
            s.config
                .servers
                .iter()
                .filter_map(|p| ssh_credentials::Key::for_profile(&s.config, p))
                .filter(|key| !retained.contains(key))
                .collect::<Vec<_>>()
        });
        if !keys.is_empty() {
            self.send(Action::ForgetCredentials(keys, false));
        }
    }
    fn connection_failed(&self, detail: Option<&str>, prompt: bool) {
        self.state.revise(|s| {
            s.connecting = false;
            s.fingerprint = None;
            s.sync_scope = None;
            s.secret.clear();
            s.connecting_with_saved_credential = false;
            s.remember_ssh_credential = false;
            let skipped = std::mem::take(&mut s.connect_prompt_skipped);
            if (prompt || skipped) && s.dialog.is_none() && !file_task_busy(s) {
                show_dialog(s, Dialog::Connect);
                if let Some(detail) = detail {
                    s.dialog_error = detail.to_owned();
                }
            }
        });
        if let Some(detail) = detail {
            self.error(detail);
        }
    }
    fn refresh(&self, remote: bool) {
        let path = self.state.revise(|s| {
            s.file_menu = None;
            s.file_context = None;
            s.file_browser.reset(remote);
            if remote {
                s.remote_ready = false;
                s.remote_selection.clear();
                s.remote_path.clone()
            } else {
                s.local_ready = false;
                s.local_selection.clear();
                s.local_path.clone()
            }
        });
        let serial = self.send(if remote {
            Action::Remote(path)
        } else {
            Action::Local(path.into())
        });
        self.state.revise(|s| {
            if remote {
                s.remote_request = serial
            } else {
                s.local_request = serial
            }
        });
    }
    fn transfer(&self, direction: TransferDirection) {
        let entries = self
            .state
            .with(|s| file_browser::selected_entries(s, direction == TransferDirection::Download));
        self.transfer_entries(direction, entries);
    }
    fn transfer_entries(&self, direction: TransferDirection, entries: Vec<FileEntry>) {
        let local = self.state.with(|s| PathBuf::from(&s.local_path));
        self.transfer_entries_to(direction, entries, &local);
    }
    fn transfer_entries_to(
        &self,
        direction: TransferDirection,
        entries: Vec<FileEntry>,
        local: &Path,
    ) {
        if direction == TransferDirection::Download
            && entries.iter().any(|f| !safe_local_name(&f.name))
        {
            self.log("下载已拒绝：所选远程名称无法安全保存为本地文件名");
            return;
        }
        let jobs = self.state.with(|s| {
            if !file_browser::transfer_ready(s) {
                return Vec::new();
            }
            let Some(server_id) = s.selected.clone() else {
                return Vec::new();
            };
            entries
                .iter()
                .map(|f| TransferJob {
                    id: new_id(),
                    server_id: server_id.clone(),
                    direction,
                    local_path: if direction == TransferDirection::Upload {
                        PathBuf::from(&f.path)
                    } else {
                        local.join(&f.name)
                    },
                    remote_path: if direction == TransferDirection::Download {
                        f.path.clone()
                    } else {
                        remote_join(&s.remote_path, &f.name)
                    },
                    total_bytes: if f.is_dir { 0 } else { f.size },
                    transferred_bytes: 0,
                    status: TransferStatus::Queued,
                })
                .collect::<Vec<_>>()
        });
        if jobs.is_empty() {
            self.log("请先连接并成功读取两个目录，再选择文件或目录；不传输符号链接");
            return;
        }
        self.state.revise(|s| enqueue_transfers(s, jobs));
        self.schedule();
    }
    fn schedule(&self) {
        let limit = self.state.with(|s| s.config.settings.transfer_concurrency);
        self.transfer_limiter.set_limit(limit);
        self.schedule_session(limit);
        let parked = self.state.with(|s| {
            if s.background {
                return Vec::new();
            }
            s.parked
                .iter()
                .filter(|(key, session)| {
                    session.connected
                        && !session.queue_paused
                        && s.jobs.iter().any(|job| {
                            job.status == TransferStatus::Queued
                                && Some(&job.server_id) == key.as_ref()
                        })
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>()
        });
        for key in parked {
            self.with_parked(&key, || self.schedule_session(limit));
        }
    }

    fn schedule_session(&self, limit: u8) {
        let next = self.state.revise(|s| {
            let mut next = Vec::new();
            if s.system_task_running
                || s.deletion.is_some()
                || !s.code.pending.is_empty()
                || s.properties.as_ref().is_some_and(|p| p.is_running())
                || !s.connected
                || s.queue_paused
            {
                return next;
            }
            while s.cancels.len() < usize::from(limit) {
                let index = s.jobs.iter().position(|job| {
                    job.status == TransferStatus::Queued
                        && !workspace::transfer_paused(s, &job.id)
                        && Some(&job.server_id) == s.selected.as_ref()
                        && !s.jobs.iter().any(|active| {
                            s.inflight_transfers.contains(&active.id)
                                && transfer_worker::overlap(job, active)
                        })
                });
                let Some(index) = index else {
                    break;
                };
                let job = &mut s.jobs[index];
                job.status = TransferStatus::Running;
                s.inflight_transfers.insert(job.id.clone());
                s.transfer_now = std::time::Instant::now();
                s.transfer_meters.insert(
                    job.id.clone(),
                    transfer_progress::Meter::new(s.transfer_now),
                );
                let cancel = Arc::new(AtomicBool::new(false));
                s.cancels.insert(job.id.clone(), cancel.clone());
                let paused = s.transfer_pauses.entry(job.id.clone()).or_default().clone();
                next.push((job.clone(), cancel, paused));
            }
            next
        });
        for (job, cancel, paused) in next {
            let policy = self
                .state
                .with(|s| transfer_ui::policy(s, &job.id, job.direction));
            self.send(Action::TransferInteractive {
                job,
                cancel,
                paused,
                policy,
            });
        }
    }
}
fn rename_action(
    s: &State,
    remote: bool,
    from: &str,
    name: &str,
) -> std::result::Result<Action, &'static str> {
    if !workspace::ready(s, remote) {
        return Err("请先成功读取目标目录");
    }
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err("请输入不含路径分隔符的文件名");
    }
    if remote {
        let parent = from.rsplit_once('/').map_or(".", |(parent, _)| parent);
        Ok(Action::RemoteRename(from.into(), remote_join(parent, name)))
    } else {
        if !safe_local_name(name) {
            return Err("本地名称包含无效字符、设备名或路径前缀");
        }
        Ok(Action::LocalRename(
            from.into(),
            Path::new(from).with_file_name(name),
        ))
    }
}
fn import_profiles(
    existing: &AppConfig,
    profiles: Vec<ServerProfile>,
) -> anyhow::Result<(AppConfig, usize, usize)> {
    let mut config = existing.clone();
    let mut imported = 0;
    let mut skipped = 0;
    for mut profile in profiles {
        if config.servers.iter().any(|p| {
            p.host.eq_ignore_ascii_case(&profile.host)
                && p.port == profile.port
                && p.username == profile.username
                && p.auth == profile.auth
                && p.private_key == profile.private_key
        }) {
            skipped += 1;
            continue;
        }
        let group = if let Some(g) = config
            .groups
            .iter()
            .find(|g| g.name.eq_ignore_ascii_case("SSH Config"))
        {
            g.id.clone()
        } else {
            config.add_group("SSH Config")?
        };
        profile.group_id = Some(group);
        config.upsert_server(profile)?;
        imported += 1;
    }
    config.validate()?;
    Ok((config, imported, skipped))
}
fn pane_entries(s: &State, remote: bool) -> Vec<FileEntry> {
    file_browser::entries(s, remote)
}
fn safe_local_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':', '\0']) {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        if name.ends_with(['.', ' ']) || name.chars().any(|c| c < ' ' || "<>\"|?*".contains(c)) {
            return false;
        }
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())
        {
            return false;
        }
    }
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
}
fn remote_join(base: &str, name: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), name)
}
fn local_display(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(unc) = value.strip_prefix("\\\\?\\UNC\\") {
        return format!("\\\\{unc}");
    }
    value.strip_prefix("\\\\?\\").unwrap_or(&value).to_string()
}
fn accepts_listing(latest: u64, received: u64) -> bool {
    latest != 0 && latest == received
}
/// A running file task keeps the dialog slot until it finishes.
fn file_task_busy(s: &State) -> bool {
    s.file_pending.is_some()
        && (s.deletion.is_some()
            || s.properties.as_ref().is_some_and(|p| p.is_running())
            || text_editor::busy(s))
}
fn dismiss_menus(s: &mut State) {
    if !s.background {
        s.host_menu = None;
        s.group_menu = None;
    }
    s.file_menu = None;
}
/// Shows `dialog` in place of any open menu. Callers check [`file_task_busy`].
fn show_dialog(s: &mut State, dialog: Dialog) {
    if dialog != Dialog::FileElevation {
        file_elevation::reset(s);
    }
    if dialog == Dialog::Connect {
        s.remember_ssh_credential = false;
    }
    if dialog == Dialog::Server {
        s.server_editor_tab = 0;
    }
    s.dialog = Some(dialog);
    s.dialog_error.clear();
    dismiss_menus(s);
}
fn selected_profile(s: &State) -> Option<&ServerProfile> {
    s.config
        .available_servers()
        .find(|p| Some(&p.id) == s.selected.as_ref())
}
/// Whether connecting to the selected server must go through the credential
/// dialog: always while a connection exists or is starting, and otherwise
/// unless [`needs_no_credentials`] holds for its profile.
fn connect_prompt_needed(s: &State) -> bool {
    s.connecting
        || s.connected
        || !s
            .config
            .servers
            .iter()
            .find(|p| Some(&p.id) == s.selected.as_ref())
            .is_some_and(needs_no_credentials)
}
/// Whether connecting to `profile` needs no password or key passphrase.
///
/// Only private keys recognised as unencrypted qualify; passwords, encrypted
/// keys and keys that cannot be inspected keep the credential dialog.
fn needs_no_credentials(profile: &ServerProfile) -> bool {
    use arcnix_core::private_keys::{KeyProtection, key_protection};
    profile.auth == AuthMethod::PrivateKey
        && profile
            .private_key
            .as_deref()
            .is_some_and(|key| key_protection(key) == KeyProtection::Unencrypted)
}
fn accepts_response(current: u64, received: u64) -> bool {
    current == received
}
fn finished_status(result: std::result::Result<(), String>, cancelled: bool) -> TransferStatus {
    match result {
        Ok(()) => TransferStatus::Completed,
        Err(_) if cancelled => TransferStatus::Cancelled,
        Err(e) => TransferStatus::Failed(e),
    }
}
fn modified(timestamp: Option<u64>) -> String {
    formatting::modified(timestamp)
}
fn size(bytes: u64) -> String {
    formatting::bytes(bytes)
}
fn open_url(url: &str) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{HSTRING, w};
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(url),
                None,
                None,
                SW_SHOWNORMAL,
            )
        };
        anyhow::ensure!(result.0 as isize > 32, "Could not open the link");
    }
    #[cfg(not(windows))]
    {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener).arg(url).spawn()?;
    }
    Ok(())
}

fn btn(label: impl Into<String>, action: impl FnMut() + 'static) -> sapid::Button {
    sapid::button()
        .label(i18n::bind(label.into()))
        .class("button")
        .on_press(action)
}
fn field(
    label: &'static str,
    ui: &Ui,
    read: fn(&State) -> String,
    write: fn(&mut State, String),
) -> impl Widget + use<> {
    let state = ui.state;
    col().class("field").fill((
        text().class("field-label").content(i18n::bind(label)),
        input()
            .name(i18n::bind(label))
            .value(state.map(read))
            .on(value_changed, move |event| {
                if let Event::ValueChanged { value, .. } = event {
                    state.revise(|s| {
                        write(s, value);
                        s.dialog_error.clear();
                    });
                }
            }),
    ))
}
fn enqueue_transfers(s: &mut State, jobs: Vec<TransferJob>) {
    let batch = new_id();
    for job in &jobs {
        s.batch_ids.insert(job.id.clone(), batch.clone());
    }
    s.jobs.extend(jobs);
}

fn resize_handles() -> impl Widget {
    fragment(
        ["n", "s", "e", "w", "ne", "nw", "se", "sw"]
            .into_iter()
            .map(|edge| view().class(format!("desktop-resize-handle edge-{edge}")))
            .collect::<Vec<_>>(),
    )
}

fn modal(
    ui: Ui,
    title: impl Into<String>,
    body: impl IntoChildren + 'static,
    footer: impl IntoChildren + 'static,
) -> impl Widget {
    modal_with_header(ui, title, (), body, footer)
}

fn modal_with_header(
    ui: Ui,
    title: impl Into<String>,
    header: impl IntoChildren + 'static,
    body: impl IntoChildren + 'static,
    footer: impl IntoChildren + 'static,
) -> impl Widget {
    let mut content = Some((title.into(), header, body, footer));
    widget(move |scope| {
        let (title, header, body, footer) = content
            .take()
            .expect("dialog builds once per mounted scope");
        let close = ui.clone();
        let scrim = ui.clone();
        let focus = scope.cage(None);
        let desktop_language = ui.state.with(|s| {
            (matches!(
                s.dialog,
                Some(
                    Dialog::FileProperties
                        | Dialog::FilesSettings
                        | Dialog::FileElevation
                        | Dialog::DeleteFiles { .. }
                )
            ) && server_desktop::owns_dialog(s))
                || matches!(
                    s.dialog,
                    Some(
                        Dialog::DesktopTimeZone
                            | Dialog::AppSettings(_)
                            | Dialog::Launcher
                            | Dialog::SystemPower(..)
                            | Dialog::OpenDesktopFile(_)
                            | Dialog::DesktopIcons
                            | Dialog::DockShortcuts
                    )
                )
        });
        let title = i18n::bind_in(desktop_language, title);
        let mounted = scope.mount_with(|| {
            let mut backdrop = col();
            // Glory bounds the number of nodes in a focus scope. These app catalogues
            // have a fixed set of entries, unlike a remote directory listing.
            if ui.state.with(|s| {
                matches!(
                    s.dialog,
                    Some(Dialog::Launcher | Dialog::DesktopIcons | Dialog::DockShortcuts)
                )
            }) {
                backdrop = backdrop
                    .focus(FocusMode::Programmatic)
                    .semantics(Some(Box::new(Semantics {
                        focus_scope: Some(glory::render::FocusScopeConfig {
                            auto_focus: false,
                            restore_focus: false,
                            ..Default::default()
                        }),
                        ..Default::default()
                    })));
            }
            backdrop
                .class(ui.state.map(|s| {
                    if server_desktop::owns_dialog(s) && s.fingerprint.is_none() {
                        server_apps_ui::themed(s, "modal-backdrop desktop-modal-backdrop")
                    } else {
                        "modal-backdrop".to_owned()
                    }
                }))
                .fill((
                    button()
                        .class("modal-scrim")
                        .label(i18n::bind_in(desktop_language, "关闭对话框"))
                        .focus(FocusMode::Programmatic)
                        .enabled(ui.state.map(|s| {
                            (s.file_pending.is_none()
                                || file_context::deletion_dialog(s)
                                || file_properties::running(s)
                                || s.dialog == Some(Dialog::TextEditor))
                                && s.import_pending.is_none()
                        }))
                        .on(activate, move |_| scrim.close_dialog()),
                    col()
                        .class(ui.state.map(|s| {
                            match s.dialog {
                                Some(Dialog::TransferProgress(_)) => {
                                    "modal-card transfer-progress-modal"
                                }
                                Some(Dialog::DesktopTimeZone) => {
                                    "modal-card settings-surface time-zone-dialog"
                                }
                                Some(Dialog::DesktopIcons) => {
                                    "modal-card settings-surface shortcut-settings-dialog"
                                }
                                Some(Dialog::Launcher) => "modal-card desktop-apps-dialog",
                                Some(Dialog::DockShortcuts) => {
                                    "modal-card settings-surface shortcut-settings-dialog dock-shortcut-dialog"
                                }
                                Some(
                                    Dialog::ConfigureNetwork
                                    | Dialog::FileAssociation { .. }
                                    | Dialog::FilesSettings
                                    | Dialog::AppSettings(_)
                                    | Dialog::TransferDefaults
                                    | Dialog::ConfigurationBackup
                                    | Dialog::ConfigurationRestore,
                                ) => "modal-card settings-surface",
                                Some(Dialog::DeleteFiles { .. }) => "modal-card file-delete-dialog",
                                Some(Dialog::OpenDesktopFile(_)) => "modal-card file-picker-dialog",
                                Some(Dialog::TextEditor) => "modal-card remote-editor-modal",
                                _ => "modal-card",
                            }
                            .to_owned()
                        }))
                        .role(Role::Dialog)
                        .name(title.clone())
                        .focus(FocusMode::Programmatic)
                        .node_ref(focus)
                        .fill((
                            row()
                                .class("modal-header")
                                .fill(text().class("dialog-title").content(title))
                                .fill(header)
                                .fill(
                                    (if desktop_language { i18n::desktop::close_action("关闭对话框") } else { close_action("关闭对话框") })
                                        .enabled(ui.state.map(|s| {
                                            (s.file_pending.is_none()
                                                || file_context::deletion_dialog(s)
                                                || file_properties::running(s)
                                                || s.dialog == Some(Dialog::TextEditor))
                                                && s.import_pending.is_none()
                                        }))
                                        .on_press(move || close.close_dialog()),
                                ),
                            col()
                                .class("modal-body")
                                .semantics(ui.state.map(|s| {
                                    Some(Box::new(Semantics {
                                        inert: s.file_pending.is_some()
                                            || s.import_pending.is_some(),
                                        ..Default::default()
                                    }))
                                }))
                                .fill(body),
                            text()
                                .class(ui.state.map(|s| {
                                    if s.dialog_error.is_empty() {
                                        "hidden"
                                    } else {
                                        "form-error"
                                    }
                                    .to_string()
                                }))
                                .content(i18n::bind_in(desktop_language, ui.state.map(|s| s.dialog_error.clone()))),
                            row().class("modal-footer").fill(footer),
                            resize_handles(),
                        )),
                ))
        })?;
        scope.retain(mounted)?;
        if let Some(id) = focus.get() {
            scope.action(glory::render::Action::Focus(id))?;
        }
        Ok(())
    })
}

fn connection_target(state: Cage<State>) -> impl Widget {
    text().class("connection-target").content(state.map(|s| {
        if !s.connected {
            return String::new();
        }
        s.config
            .servers
            .iter()
            .find(|p| Some(&p.id) == s.selected.as_ref())
            .map(|p| format!("{}@{}", p.username, p.host))
            .unwrap_or_default()
    }))
}

fn connection_controls(ui: Ui) -> impl Widget {
    let state = ui.state;
    row().class("connection-controls").fill((
        connection_target(state),
        text()
            .class(state.map(|s| {
                if s.connected {
                    "connection-state online"
                } else if s.connecting {
                    "connection-state connecting"
                } else {
                    "connection-state"
                }
                .to_string()
            }))
            .content(i18n::bind(state.map(|s| {
                if s.local_desktop.is_some() {
                    if s.connected {
                        "已直连本机"
                    } else if s.connecting {
                        "正在打开本机桌面…"
                    } else {
                        "本机桌面已断开"
                    }
                } else if s.connected {
                    "SSH 已连接"
                } else if s.connecting {
                    "SSH 正在连接…"
                } else if s.config.available_servers().next().is_none() {
                    "尚未添加服务器"
                } else if s.selected.is_none() {
                    "请选择左侧服务器"
                } else {
                    "SSH 未连接"
                }
                .to_string()
            }))),
        sapid::button()
            .class(state.map(|s| {
                if s.connected || s.connecting {
                    "button connection-button"
                } else {
                    "button primary connection-button"
                }
                .to_string()
            }))
            .label(i18n::bind(state.map(|s| {
                if s.connected {
                    "断开连接"
                } else if s.connecting {
                    "取消连接"
                } else if s.local_desktop.is_none() && s.config.available_servers().next().is_none()
                {
                    "添加服务器"
                } else {
                    "Desktop"
                }
                .to_string()
            })))
            .enabled(state.map(|s| {
                !s.system_task_running
                    && (s.selected.is_some() || s.config.available_servers().next().is_none())
            }))
            .on_press(move || {
                if state.with(|s| s.connected || s.connecting) {
                    ui.disconnect();
                } else if state.with(|s| {
                    s.local_desktop.is_none() && s.config.available_servers().next().is_none()
                }) {
                    servers::new_server(&ui);
                } else {
                    ui.open_workbench(server_desktop::Tab::Desktop);
                }
            }),
    ))
}

fn app(ui: Ui) -> impl Widget {
    let state = ui.state;
    let menus = ui.clone();
    let trust = ui.clone();
    let once = ui.clone();
    let reject = ui.clone();
    let keyboard_ui = ui.clone();
    col()
        .class(state.map(|s| {
            if s.config.settings.theme.is_dark(s.system_dark) {
                "app theme-dark"
            } else {
                "app theme-light"
            }
            .to_string()
        }))
        .on(keyboard, move |event| {
            if let Event::Keyboard {
                command: glory::render::KeyboardCommand::Dismiss,
                ..
            } = event
            {
                if keyboard_ui.state.with(|s| s.fingerprint.is_some()) {
                    keyboard_ui.disconnect();
                } else if keyboard_ui.state.with(|s| {
                    server_desktop::fullscreen(s)
                        && s.dialog.is_none()
                        && s.host_menu.is_none()
                        && s.group_menu.is_none()
                        && s.file_menu.is_none()
                        && s.file_context.is_none()
                        && s.terminal.menu.is_none()
                }) {
                    keyboard_ui.state.revise(|s| s.workbench.fullscreen = false);
                } else {
                    keyboard_ui.close_dialog();
                    keyboard_ui.state.revise(|s| {
                        s.host_menu = None;
                        s.group_menu = None;
                        s.file_menu = None;
                    });
                }
            }
        })
        .fill((
            text().class("terminal-workspace-epochs hidden").content(
                state.map(|s| serde_json::to_string(&s.connected_epochs()).unwrap_or_default()),
            ),
            input()
                .class("system-theme-input hidden")
                .value("")
                .on(value_changed, move |event| {
                    if let Event::ValueChanged { value, .. } = event {
                        let dark = match value.as_str() {
                            "dark" => true,
                            "light" => false,
                            _ => return,
                        };
                        if state.with(|s| s.system_dark != dark) {
                            state.revise(|s| s.system_dark = dark);
                        }
                    }
                }),
            col()
                .class("app-shell")
                .focus(FocusMode::Programmatic)
                .node_ref(ui.shell_focus)
                .semantics(state.map(|s| {
                    Some(Box::new(Semantics {
                        inert: s.dialog.is_some()
                            || s.fingerprint.is_some()
                            || updates_ui::applying(s),
                        ..Default::default()
                    }))
                }))
                .fill(navigation::shell(ui.clone())),
            button()
                .class(state.map(|s| {
                    if s.dialog.is_none()
                        && (s.host_menu.is_some()
                            || s.group_menu.is_some()
                            || (s.file_menu.is_some() && !file_context::desktop_menu_open(s)))
                    {
                        "menu-dismiss"
                    } else {
                        "hidden"
                    }
                    .to_string()
                }))
                .label(i18n::bind("关闭操作菜单"))
                .focus(FocusMode::Programmatic)
                .on(activate, move |_| {
                    menus.state.revise(|s| {
                        s.host_menu = None;
                        s.group_menu = None;
                        s.file_menu = None;
                    })
                }),
            fragment((
                updates_ui::banner(ui.clone()),
                servers::dialogs(ui.clone()),
                updates_ui::overlay(ui.clone()),
                settings::files_dialog(ui.clone()),
                desktop_file_picker::dialog(ui.clone()),
                files_ui::folder_icon_dialog(ui.clone()),
                network_ui::dialog(ui.clone()),
                fragment((
                    fragment((
                        desktop_system_settings::time_zone_dialog(ui.clone()),
                        app_settings::dialog(ui.clone()),
                        desktop_customization_ui::shortcut_settings_dialog(ui.clone()),
                        desktop_apps::dialog(ui.clone()),
                        desktop_power::dialog(ui.clone()),
                        text_editor::dialogs(ui.clone()),
                        file_context::dialogs(ui.clone()),
                        file_elevation::dialog(ui.clone()),
                        file_open::dialog(ui.clone()),
                        file_open::settings_dialog(ui.clone()),
                        trash_ui::dialog(ui.clone()),
                    )),
                    software_install_ui::dialogs(ui.clone()),
                    fragment(docker_ui::dialogs(ui.clone())),
                    terminal_menu::dialogs(ui.clone()),
                    resource_ports_ui::dialog(ui.clone()),
                )),
                file_columns::dialog(ui.clone()),
                cleanup_records_ui::dialog(ui.clone()),
                cleanup_records_ui::download_dialog(ui.clone()),
                script_exchange_ui::dialogs(ui.clone()),
            )),
            operations::dialogs(ui.clone()),
            scripts_ui::dialogs(ui.clone()),
            fragment((
                task_records_ui::dialogs(ui.clone()),
                configuration_backup_ui::dialogs(ui.clone()),
                #[cfg(windows)]
                web_browser::forwarding_dialog(ui.clone()),
            )),
            record_maintenance_ui::dialog(ui.clone()),
            backups_ui::dialogs(ui.clone()),
            fragment((
                transfer_ui::dialogs(ui.clone()),
                workspace::transfer_progress_dialog(ui.clone()),
                files_transfer::download_dialog(ui.clone()),
            )),
            when(state.map(|s| s.fingerprint.is_some()), move || {
                let trust = trust.clone();
                let once = once.clone();
                let reject = reject.clone();
                modal(
                    ui.clone(),
                    "确认服务器身份",
                    col().class("dialog-stack").fill((
                        text().content(i18n::bind("首次连接此服务器，请核对 SSH 主机指纹。")),
                        text()
                            .class("fingerprint")
                            .selectable(true)
                            .content(state.map(|s| s.fingerprint.clone().unwrap_or_default())),
                        text().class("hint").content(i18n::bind(
                            "确认指纹后再继续认证。可选择仅本次信任，或保存供下次连接校验。",
                        )),
                    )),
                    (
                        btn("取消", move || reject.disconnect()),
                        btn("仅本次信任", move || once.authenticate(false)),
                        sapid::button()
                            .class("button primary")
                            .label(i18n::bind("信任并保存"))
                            .on_press(move || trust.authenticate(true)),
                    ),
                )
            }),
        ))
}

#[cfg(windows)]
const APP_CSS: &str = concat!(
    include_str!("style.css"),
    "\n",
    include_str!("shell/wsl.css")
);
#[cfg(not(windows))]
const APP_CSS: &str = include_str!("style.css");

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().is_some_and(|arg| arg == "--apply-update") {
        if arguments.len() != 2 {
            return Err("--apply-update requires one prepared plan".into());
        }
        update_install::apply(Path::new(&arguments[1]))?;
        return Ok(());
    }
    if arguments == ["--version"] {
        println!("arcnix {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut config_path = None;
    let mut health_marker = None;
    let mut args = arguments.into_iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--config" if config_path.is_none() => {
                config_path = Some(PathBuf::from(
                    args.next().ok_or("--config requires a path")?,
                ))
            }
            "--update-health" if health_marker.is_none() => {
                health_marker = Some(PathBuf::from(
                    args.next().ok_or("--update-health requires a path")?,
                ))
            }
            "--help" => {
                println!("Arcnix desktop\nUsage: arcnix [--config <path>] [--version]");
                return Ok(());
            }
            _ => return Err("Usage: arcnix [--config <path>] [--version]".into()),
        }
    }
    let store = Rc::new(if let Some(path) = config_path {
        ConfigStore::at(path)
    } else {
        ConfigStore::default_location()?
    });
    if let Some(path) = &health_marker {
        let root = std::fs::canonicalize(
            store
                .path()
                .parent()
                .ok_or("Missing configuration directory")?
                .join("updates"),
        )?;
        if !path.is_absolute()
            || !path
                .parent()
                .and_then(|p| std::fs::canonicalize(p).ok())
                .is_some_and(|p| p.starts_with(&root))
            || !path.file_name().is_some_and(|p| {
                p.to_string_lossy().starts_with("startup-") && p.to_string_lossy().ends_with(".ok")
            })
        {
            return Err("Invalid update startup acknowledgement path".into());
        }
    }
    let (config, error) = match store.load() {
        Ok(c) => (c, None),
        Err(e) => (
            AppConfig::default(),
            Some(format!("配置读取失败，禁止覆盖: {e:#}")),
        ),
    };
    if health_marker.is_some() && error.is_some() {
        return Err("Updated application could not load the saved configuration".into());
    }
    let path = config
        .local_directory
        .clone()
        .unwrap_or(std::env::current_dir()?);
    let (update_controller, update_rx) = updates_ui::start()?;
    formatting::configure(config.settings.display);
    let logger = Rc::new(client_logs::Logger::new(store.path()));
    let window = WindowApp::new();
    window.renderer().enable_local_media();
    let scope = window.app().scope();
    i18n::init(scope, config.settings.language);
    let state = scope.cage(State::new(
        config,
        local_display(&path),
        error.clone().unwrap_or_default(),
    ));
    i18n::desktop::init(scope, state);
    let (batch_events, batch_rx) = async_channel::bounded(128);
    let ui = Ui {
        player: video_player::Player::new(window.renderer().clone(), scope),
        update_controller,
        transfer_limiter: Default::default(),
        batch_controller: Rc::new(std::cell::RefCell::new(None)),
        batch_events,
        logger,
        state,
        store,
        writable: scope.cage(error.is_none()),
        scope: scope.clone(),
        shell_focus: scope.cage(None),
    };
    ui.log(
        error
            .as_deref()
            .unwrap_or("Arcnix 已就绪。新建服务器并连接，或浏览本地目录。"),
    );
    ui.refresh(false);
    scripts_ui::restore(&ui);
    let batch_ui = ui.clone();
    scope.spawn(async move {
        while let Ok(event) = batch_rx.recv().await {
            scripts_ui::event(&batch_ui, event);
        }
    })?;
    updates_ui::restore(&ui);
    let updates = ui.clone();
    scope.spawn(async move {
        while let Ok(event) = update_rx.recv().await {
            updates_ui::event(&updates, event);
        }
    })?;
    // Coalesce ticks while the UI is busy. Sampling itself is also gated by
    // the active page and a single pending request in observability state.
    let (ticks, tick_rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if let Err(async_channel::TrySendError::Closed(_)) = ticks.try_send(()) {
                break;
            }
        }
    });
    let polling = ui.clone();
    scope.spawn(async move {
        let mut health_marker = health_marker;
        while tick_rx.recv().await.is_ok() {
            if let Some(path) = health_marker.take() {
                use std::io::Write;
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(path)
                {
                    let _ = writeln!(file, "arcnix {}", env!("CARGO_PKG_VERSION"));
                    let _ = file.sync_all();
                }
            }
            polling.observability_tick();
            polling.tick_desktop_clock();
            polling.tick_desktop_sync();
            #[cfg(windows)]
            browser_cookies::tick(&polling);
            polling.transfer_tick();
            preferences_ui::tick(&polling);
            polling.tick_parked();
            updates_ui::tick(&polling);
        }
    })?;
    // Locale bindings patch the existing document. Remounting it would retire
    // live button identities while their browser activations are still queued.
    window.mount(app(ui.clone()))?;
    let shell = Shell::new();
    // Tao creates its NSApplication subclass when it constructs the event loop.
    #[cfg(target_os = "macos")]
    macos_menu::install();
    shell.run(
        window,
        Config {
            native: Some(Box::new(app_icon::DesktopWindow::new(ui.clone()))),
            assets: arcnix_core::desktop_theme::BUNDLED_WALLPAPERS.iter()
            .map(|(name, bytes)| {
                (
                    format!("/wallpapers/{name}.webp"),
                    glory::wry::shell::Asset {
                        mime: "image/webp".into(),
                        bytes: bytes.to_vec(),
                    },
                )
            })
            .collect(),
            asset_provider: Some(Arc::new(video_player::Assets {
                player: ui.player.source.clone(),
            })),
            initialization_script: Some(
                [
                    include_str!("../assets/system-theme.js"),
                    include_str!("../assets/desktop-frame.js"),
                    include_str!("../assets/shortcut-chords.js"),
                    include_str!("../assets/file-list-events.js"),
                    include_str!("../assets/desktop-customization.js"),
                    include_str!("../assets/settings-controls.js"),
                    include_str!("../assets/help-events.js"),
                    include_str!("../assets/resource-trends.js"),
                    include_str!("../assets/server-tree-events.js"),
                    include_str!("../assets/desktop-geometry.js"),
                    include_str!("../assets/modal-drag.js"),
                    include_str!("../assets/window-tabs.js"),
                    include_str!("../assets/files-tabs.js"),
                    if cfg!(windows) {
                        "const nativeSurfaceRegionClipping = () => true;"
                    } else if cfg!(target_os = "macos") {
                        "const nativeSurfaceRegionClipping = app => ['code','pgadmin','office'].includes(app);"
                    } else {
                        "const nativeSurfaceRegionClipping = () => false;"
                    },
                    include_str!("../assets/browser-viewport.js"),
                    include_str!("../assets/video-player.js"),
                    include_str!("../assets/image-viewer.js"),
                    include_str!("../assets/pdf-reader.js"),
                    include_str!("../assets/vendor/codemirror/modes.js"),
                    include_str!("../assets/text-editor-events.js"),
                    include_str!("../assets/desktop-motion.js"),
                    include_str!("../assets/desktop-shortcuts.js"),
                    include_str!("../assets/vendor/xterm/xterm.js"),
                    include_str!("../assets/vendor/addon-fit/addon-fit.js"),
                    include_str!("../assets/terminal.js"),
                ]
                .join("\n;\n"),
            ),
            title: i18n::translate("Arcnix — 服务器管理工作台"),
            width: 1440.,
            height: 980.,
            css: format!(
                "@layer sapid-settings {{ @scope (.settings-segment-field, .settings-select-field) {{ {} }} }}\n{}\n{}\n{}\n{}\n{}",
                sapid::STRUCTURAL_CSS,
                APP_CSS,
                icons::css(),
                folder_icons::css(),
                desktop_customization_ui::customization_css(),
                include_str!("../assets/vendor/xterm/xterm.css")
            ),
            ..Default::default()
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_launchers_follow_the_session_capability() {
        let mut state = State::new(AppConfig::default(), String::new(), String::new());
        state.connected = true;
        state.local_desktop = Some(local_desktop::Target::Native);
        for supported in [false, true] {
            state.local_terminal_supported = supported;
            assert_eq!(state.terminal_available(), supported);
            assert_eq!(
                server_desktop::available_apps(&state).contains(&WorkspacePage::Terminal),
                supported
            );
        }
        state.local_terminal_supported = false;
        state.local_desktop = None;
        assert!(state.terminal_available());
    }

    #[test]
    fn read_only_desktop_stays_connected_without_terminal_access() {
        let mut state = State::new(AppConfig::default(), String::new(), String::new());
        state.epoch = 1;
        state.connected = true;
        state.local_desktop = Some(local_desktop::Target::ReadOnlyHost);
        assert!(!state.terminal_available());
        assert_eq!(state.connected_epochs(), [1]);

        let mut session = ServerSession::new(String::new());
        state.swap_session(&mut session);
        state.parked.insert(None, session);
        assert_eq!(state.connected_epochs(), [1]);
        assert_eq!(state.active_connections(), 1);
        state.parked.get_mut(&None).unwrap().connected = false;
        assert!(state.connected_epochs().is_empty());
    }

    #[test]
    fn only_unencrypted_private_keys_skip_the_credential_dialog() {
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = std::env::temp_dir().join(format!("arcnix-connect-prompt-{}", new_id()));
        std::fs::create_dir(&directory).unwrap();
        let _cleanup = Cleanup(directory.clone());
        let pem = |label: &str, headers: &str| {
            format!("-----BEGIN {label}-----\n{headers}QUJDRA==\n-----END {label}-----\n")
        };
        let unencrypted = directory.join("plain.pem");
        std::fs::write(&unencrypted, pem("PRIVATE KEY", "")).unwrap();
        let encrypted = directory.join("encrypted.pem");
        std::fs::write(&encrypted, pem("ENCRYPTED PRIVATE KEY", "")).unwrap();
        let unknown = directory.join("unknown.ppk");
        std::fs::write(&unknown, "PuTTY-User-Key-File-3: ssh-ed25519\n").unwrap();

        let with_key = |path: Option<PathBuf>| ServerProfile {
            auth: AuthMethod::PrivateKey,
            private_key: path,
            ..Default::default()
        };
        assert!(needs_no_credentials(&with_key(Some(unencrypted.clone()))));
        assert!(!needs_no_credentials(&with_key(Some(encrypted))));
        assert!(!needs_no_credentials(&with_key(Some(unknown))));
        assert!(!needs_no_credentials(&with_key(Some(
            directory.join("missing")
        ))));
        assert!(!needs_no_credentials(&with_key(None)));
        // A leftover key path must not bypass the prompt for password logins.
        let password = ServerProfile {
            auth: AuthMethod::Password,
            private_key: Some(unencrypted),
            ..Default::default()
        };
        assert!(!needs_no_credentials(&password));
    }
    #[test]
    fn import_preserves_alternate_identities_and_reuses_group_case() {
        let mut existing = AppConfig::default();
        existing.add_group("ssh config").unwrap();
        let first = ServerProfile {
            name: "first".into(),
            host: "localhost".into(),
            username: "user".into(),
            ..Default::default()
        };
        let mut alternate = first.clone();
        alternate.id = new_id();
        alternate.name = "alternate".into();
        alternate.auth = AuthMethod::PrivateKey;
        alternate.private_key = Some("key.pem".into());
        let (config, added, skipped) =
            import_profiles(&existing, vec![first.clone(), first, alternate]).unwrap();
        assert_eq!((added, skipped), (2, 1));
        assert_eq!(config.groups.len(), 1);
        assert_eq!(config.servers.len(), 2);
    }
    #[test]
    fn failed_import_leaves_original_untouched() {
        let existing = AppConfig::default();
        let good = ServerProfile {
            name: "valid".into(),
            host: "localhost".into(),
            username: "user".into(),
            ..Default::default()
        };
        assert!(import_profiles(&existing, vec![good, ServerProfile::default()]).is_err());
        assert!(existing.servers.is_empty());
        assert!(existing.groups.is_empty());
    }
    #[test]
    fn directory_response_cannot_restore_invalidated_draft() {
        assert!(!accepts_listing(0, 42));
        assert!(!accepts_listing(43, 42));
        assert!(accepts_listing(43, 43));
    }
    #[test]
    fn old_server_responses_cannot_update_new_session() {
        assert!(!accepts_response(3, 2));
        assert!(accepts_response(3, 3));
        assert!(!accepts_response(3, 4));
    }
    #[test]
    fn late_cancel_cannot_hide_successful_transfer() {
        assert_eq!(finished_status(Ok(()), true), TransferStatus::Completed);
        assert_eq!(
            finished_status(Err("cancelled".into()), true),
            TransferStatus::Cancelled
        );
        assert_eq!(
            finished_status(Err("denied".into()), false),
            TransferStatus::Failed("denied".into())
        );
    }
    #[test]
    fn remote_paths_use_posix_separators() {
        assert_eq!(
            remote_join("/var/log/", "arcnix.log"),
            "/var/log/arcnix.log"
        );
        assert_eq!(remote_join("/", "中文.txt"), "/中文.txt");
    }
    #[test]
    fn downloaded_names_cannot_escape_local_directory() {
        for name in [
            "../escape",
            "..",
            "C:escape",
            "C:\\escape",
            "a/b",
            "a\\b",
            "",
        ] {
            assert!(!safe_local_name(name), "{name}");
        }
        assert!(safe_local_name("中文 file.txt"));
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_special_names_rejected() {
        for name in [
            "CON",
            "nul.txt",
            "COM1.log",
            "LPT3",
            "file.",
            "file ",
            "stream:secret",
        ] {
            assert!(!safe_local_name(name), "{name}");
        }
    }
}
