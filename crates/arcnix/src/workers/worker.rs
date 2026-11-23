use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use anyhow::Context;
use arcnix_core::file_capabilities::{
    BoundedFileReader, ClientFiles, DirectoryReader, DocumentDestinationWriter, DocumentReader,
    DocumentWriter, EntryKind, FileDeleter, FileEntryCreator, FilePermissionWriter, FileRenamer,
    FileTreeOperator, ReviewedDocument, ReviewedFileOperations, ReviewedPageOpener,
    SeekableFileReader, TextPage, TextPageReader, TrashAccess,
};
use arcnix_core::host_capabilities::{PortController, PortInventoryReader, SystemLogReader};
use arcnix_core::metrics::{ResourceReader, ResourceReading};
use arcnix_core::packages::{self, SystemEnvironment};
use arcnix_core::remote::{PendingConnection, RemoteSession};
use arcnix_core::software_capabilities::PackageInventoryReader;
use arcnix_core::system_logs::{LogCapabilities, LogQuery, LogSnapshot};
use arcnix_core::{local, *};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictScope {
    ThisFile,
    ThisQueue,
    ThisSession,
    FutureConnections,
}
#[derive(Clone, Copy, Debug)]
pub struct ConflictDecision {
    pub action: arcnix_core::transfer::ConflictAction,
    pub scope: ConflictScope,
}

pub enum Login {
    Manual {
        secret: zeroize::Zeroizing<String>,
        remember: bool,
    },
    Saved,
}

pub struct FileFailure {
    pub message: String,
    pub permission_denied: bool,
    pub text_retry: Option<remote::AdminTextRetry>,
}

impl FileFailure {
    pub(crate) fn text_message(&self, reading: bool, language: arcnix_core::Language) -> String {
        let detail = crate::i18n::translate_for(&self.message, language);
        if !self.permission_denied {
            return detail;
        }
        let denied = crate::i18n::translate_for(
            if reading {
                "You do not have permission to read this file."
            } else {
                "You do not have permission to save this file."
            },
            language,
        );
        if detail.is_empty() {
            denied
        } else {
            format!("{denied} {detail}")
        }
    }

    pub(super) fn ordinary(message: String) -> Self {
        Self {
            message,
            permission_denied: false,
            text_retry: None,
        }
    }
}

pub(crate) type TextAuthorization = (remote::AdminTextRetry, zeroize::Zeroizing<String>);

pub enum Action {
    LocalDesktop(local_desktop::Target, PathBuf),
    OpenPdf(String, async_channel::Receiver<crate::pdf_reader::Command>),
    PlayVideo {
        path: String,
        prefer_content: bool,
        active: Arc<AtomicBool>,
    },
    #[cfg(windows)]
    ReadHtml(String),
    CodeFile(crate::code_workbench::FileRequest),
    AttachCodePage {
        path: String,
        snapshot: arcnix_core::text_files::TextSnapshot,
        sudo: Option<TextAuthorization>,
    },
    TextRead {
        path: String,
        sudo: Option<TextAuthorization>,
        cancel: Arc<AtomicBool>,
    },
    TextSave {
        document: arcnix_core::text_files::Document,
        content: String,
        cancel: Arc<AtomicBool>,
        sudo: Option<TextAuthorization>,
    },
    TextSaveAs {
        path: String,
        bytes: zeroize::Zeroizing<Vec<u8>>,
        replace: Option<arcnix_core::text_files::TextSnapshot>,
        cancel: Arc<AtomicBool>,
    },
    Keepalive,
    ServerApps(crate::server_apps_worker::Action),
    Operations(crate::operations_worker::Action),
    Terminal(crate::terminal_worker::Action),
    Probe(Box<ServerProfile>, Option<String>),
    ProbeSaved(Box<ServerProfile>, String),
    Authenticate {
        login: Login,
        fingerprint: String,
    },
    ForgetCredentials(Vec<crate::ssh_credentials::Key>, bool),
    Disconnect,
    Local(PathBuf),
    Remote(String),
    Children {
        remote: bool,
        path: String,
    },
    CreateEntry {
        remote: bool,
        parent: String,
        default_name: String,
        directory: bool,
    },
    Trash {
        remote: bool,
        operation: recycle::Operation,
    },
    LocalRename(PathBuf, PathBuf),
    Delete {
        remote: bool,
        targets: Vec<arcnix_core::deletion::Target>,
        cancel: Arc<AtomicBool>,
        sudo: Option<zeroize::Zeroizing<String>>,
    },
    ImportSshConfig(PathBuf),
    #[cfg(windows)]
    WslTerminal(String),
    #[cfg(windows)]
    WslFiles(String, crate::wsl_workspace::Operation),
    RemoteRename(String, String),
    ManageRemoteFiles(arcnix_core::remote::FileAction, Vec<String>),
    ManageFilesAsAdmin(remote::ManagedFileRetry, zeroize::Zeroizing<String>),
    ResolveFileAlias(String),
    FileMetadata(String),
    Permissions {
        targets: Vec<arcnix_core::permissions::Target>,
        mode: u32,
        cancel: Arc<AtomicBool>,
    },
    TransferInteractive {
        job: TransferJob,
        cancel: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
        policy: Option<arcnix_core::transfer::ConflictAction>,
    },
    #[cfg(windows)]
    StartBrowserProxy(Arc<BrowserProxyControl>),
    #[cfg(windows)]
    DiagnoseBrowser(String),
    #[cfg(windows)]
    RepairBrowserForwarding {
        sudo: zeroize::Zeroizing<String>,
    },
    PgAdminOpen(PathBuf),
    PgAdminInstall(PathBuf, Arc<AtomicBool>),
    PgAdminStart {
        base: PathBuf,
        connection: arcnix_core::pgadmin::Connection,
        stop: Arc<AtomicBool>,
    },
    PackageDetect,
    PackageSearch(String),
    PackageInstalled,
    MetricsSample,
    PortsRead {
        administrator: bool,
        sudo: zeroize::Zeroizing<String>,
        cancel: Arc<AtomicBool>,
    },
    PortTerminate {
        target: Box<arcnix_core::resource_ports::Target>,
        administrator: bool,
        sudo: zeroize::Zeroizing<String>,
        cancel: Arc<AtomicBool>,
    },
    LogsDiscover,
    LogsRead(LogQuery),
}
pub struct Request {
    pub epoch: u64,
    pub serial: u64,
    pub action: Action,
}
#[cfg(test)]
impl Action {
    fn transfer(job: TransferJob, cancel: Arc<AtomicBool>) -> Self {
        Self::TransferInteractive {
            job,
            cancel,
            paused: Arc::default(),
            policy: None,
        }
    }
}
#[cfg(windows)]
#[derive(Default)]
pub struct BrowserProxyControl {
    pub stop: AtomicBool,
    pub reconnect: AtomicBool,
    refused: std::sync::Mutex<Vec<String>>,
}

#[cfg(windows)]
impl BrowserProxyControl {
    fn remember_refusal(&self, host: &str, port: u16) {
        let target = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        if let Ok(mut refused) = self.refused.lock()
            && !refused.contains(&target)
        {
            if refused.len() == 64 {
                refused.remove(0);
            }
            refused.push(target);
        }
    }

    pub fn was_refused(&self, target: &str) -> bool {
        self.refused
            .lock()
            .is_ok_and(|refused| refused.iter().any(|item| item == target))
    }
}

pub enum Update {
    LocalDesktop(Result<String, String>),
    Pdf(crate::pdf_reader::Reply),
    PgAdmin(crate::pgadmin_ui::Reply),
    Video(Result<Arc<crate::video_player::Source>, String>),
    #[cfg(windows)]
    Html(Result<String, String>),
    CodeFile(Result<serde_json::Value, String>),
    TextFile(Result<arcnix_core::text_files::Document, FileFailure>),
    #[cfg(windows)]
    BrowserProxy(Result<(u16, String), String>),
    #[cfg(windows)]
    BrowserDiagnosis(arcnix_core::browser_proxy::Diagnosis),
    #[cfg(windows)]
    BrowserForwardingRepaired(Result<(), String>),
    ServerApps(Box<Result<crate::server_apps_worker::Reply, String>>),
    Alive,
    FileMetadata(Result<arcnix_core::permissions::Metadata, String>),
    Permissions(Result<usize, String>),
    DeletionProgress(arcnix_core::deletion::Progress),
    Operations(crate::operations_worker::Event),
    Terminal(crate::terminal_worker::Event),
    Fingerprint(String),
    Connected(bool),
    Disconnected,
    Local(PathBuf, Vec<FileEntry>),
    Remote(String, Vec<FileEntry>),
    Created {
        remote: bool,
        directory: bool,
        name: String,
    },
    Children {
        remote: bool,
        parent: String,
        result: Result<Vec<FileEntry>, String>,
    },
    Progress(String, u64, u64, u64),
    TransferPlan(String, arcnix_core::transfer::TransferPlan),
    TransferFile(String, arcnix_core::transfer::FileProgress),
    Finished(String, Result<(), String>),
    Renamed {
        remote: bool,
        name: String,
    },
    ManagedRemoteFiles(Result<(), String>),
    FileAdminRequired(remote::ManagedFileRetry),
    FileAlias(Result<(String, bool), String>),
    Trash {
        remote: bool,
        result: Result<Vec<recycle::Item>, String>,
    },
    Deleted {
        remote: bool,
        result: Result<usize, FileFailure>,
    },
    Error(String),
    ConnectionError(String),
    CredentialPrompt(Option<&'static str>),
    CredentialNotice(&'static str),
    Imported(arcnix_core::ssh_config::ImportReport),
    #[cfg(windows)]
    WslFiles(Result<crate::wsl_workspace::ResultView, String>),
    BrowseError {
        remote: bool,
        message: String,
    },
    ConnectionLost(String),
    PackageEnvironment(Result<SystemEnvironment, String>),
    PackageList(Result<Vec<packages::Package>, String>),
    Metrics(Result<ResourceReading, String>),
    Ports(Result<arcnix_core::resource_ports::Inventory, String>),
    PortTerminated(Result<(), String>),
    LogCapabilities(Result<LogCapabilities, String>),
    SystemLogs(Result<LogSnapshot, String>),
    TransferConflict(
        String,
        arcnix_core::transfer::TransferConflict,
        mpsc::Sender<ConflictDecision>,
    ),
    TransferReport(String, arcnix_core::transfer::TransferSummary),
}
pub struct Response {
    pub epoch: u64,
    pub serial: u64,
    pub update: Update,
}
fn pgadmin_install(base: &Path, cancel: &AtomicBool, reply: impl Fn(crate::pgadmin_ui::Reply)) {
    use arcnix_core::{pgadmin, updates};

    use crate::pgadmin_ui::Reply;
    let check = || {
        anyhow::ensure!(!cancel.load(Ordering::Acquire), "Installation cancelled");
        Ok(())
    };
    let mut shown = 0;
    let result = if let Some(package) = pgadmin::local_package() {
        pgadmin::install_local(base, &package, check)
    } else if let Some(asset) = pgadmin::package() {
        pgadmin::install(
            base,
            &updates::Source::github(),
            &asset,
            check,
            |done, total| {
                let percent = done * 100 / total.max(1);
                if percent != shown {
                    shown = percent;
                    reply(Reply::Progress(percent));
                }
            },
        )
    } else {
        Err(anyhow::anyhow!("pgAdmin is not available for this system"))
    };
    reply(Reply::Installed(
        result.map(|_| ()).map_err(|error| format!("{error:#}")),
    ));
}
fn publish_file_result<T>(
    result: anyhow::Result<T>,
    send: impl FnOnce(std::result::Result<T, String>),
) -> anyhow::Result<()> {
    match result {
        Ok(value) => {
            send(Ok(value));
            Ok(())
        }
        Err(error) => {
            let lost = arcnix_core::transport_error::is_connection_error(&error);
            send(Err(format!("{error:#}")));
            if lost { Err(error) } else { Ok(()) }
        }
    }
}
fn publish_text_result<T>(
    result: anyhow::Result<T>,
    sudo: Option<TextAuthorization>,
    active: bool,
    prepare: impl FnOnce() -> anyhow::Result<remote::AdminTextRetry>,
    send: impl FnOnce(Result<T, FileFailure>),
) -> anyhow::Result<()> {
    let prepared = match &result {
        Err(error) if active && sudo.is_none() && remote::file_permission_denied(error) => {
            prepare()
        }
        Err(error)
            if active
                && error.is::<remote::SudoPasswordRequired>()
                && let Some((mut retry, _)) = sudo =>
        {
            retry.needs_password = true;
            Ok(retry)
        }
        _ => return publish_elevatable_result(result, false, send),
    };
    let (text_retry, message) = match prepared {
        Ok(retry) => (Some(retry), String::new()),
        Err(reason) => (None, format!("{reason:#}")),
    };
    send(Err(FileFailure {
        permission_denied: true,
        message,
        text_retry,
    }));
    Ok(())
}

fn publish_elevatable_result<T>(
    result: anyhow::Result<T>,
    allow_elevation: bool,
    send: impl FnOnce(Result<T, FileFailure>),
) -> anyhow::Result<()> {
    match result {
        Ok(value) => send(Ok(value)),
        Err(error) => {
            let lost = arcnix_core::transport_error::is_connection_error(&error);
            send(Err(FileFailure {
                message: format!("{error:#}"),
                text_retry: None,
                permission_denied: allow_elevation
                    && !lost
                    && remote::file_permission_denied(&error),
            }));
            if lost {
                return Err(error);
            }
        }
    }
    Ok(())
}
fn deletion_progress(
    events: async_channel::Sender<Response>,
    epoch: u64,
    serial: u64,
) -> impl FnMut(arcnix_core::deletion::Progress) {
    let mut previous = None;
    let mut last = std::time::Instant::now();
    move |progress| {
        if previous != Some(progress.phase)
            || last.elapsed().as_millis() >= 100
            || (progress.total > 0 && progress.removed == progress.total)
        {
            previous = Some(progress.phase);
            last = std::time::Instant::now();
            let _ = events.try_send(Response {
                epoch,
                serial,
                update: Update::DeletionProgress(progress),
            });
        }
    }
}
pub(super) fn create_named_entry(
    default_name: &str,
    mut try_create: impl FnMut(&str) -> anyhow::Result<bool>,
) -> anyhow::Result<String> {
    for index in 1u64.. {
        let name = if index == 1 {
            default_name.to_owned()
        } else {
            format!("{default_name} ({index})")
        };
        if try_create(&name)? {
            return Ok(name);
        }
    }
    anyhow::bail!("No available name")
}

#[cfg(test)]
pub fn start() -> (mpsc::Sender<Request>, async_channel::Receiver<Response>) {
    start_with_limiter(Arc::new(crate::transfer_worker::Limiter::default()))
}

pub(crate) fn start_with_limiter(
    limiter: Arc<crate::transfer_worker::Limiter>,
) -> (mpsc::Sender<Request>, async_channel::Receiver<Response>) {
    let (tx, rx) = mpsc::channel::<Request>();
    let (events, receive) = async_channel::bounded(128);
    let (local_tx, local_rx) = mpsc::channel::<Request>();
    let (remote_tx, remote_rx) = mpsc::channel::<Request>();
    let generation = Arc::new(AtomicU64::new(0));
    let router_generation = generation.clone();
    let terminal = crate::terminal_worker::start(events.clone(), generation.clone());
    let routed_terminal = terminal.clone();
    let router_events = events.clone();
    // Local filesystem work must stay available during a large SFTP transfer.
    thread::spawn(move || {
        let mut read_only_demo = false;
        for request in rx {
            match &request.action {
                Action::LocalDesktop(target, _) => read_only_demo = target.read_only(),
                Action::Probe(..) | Action::ProbeSaved(..) | Action::Disconnect => {
                    read_only_demo = false
                }
                _ => {}
            }
            // The transfer pane has a separate local-files worker; it must honor
            // the same access choice even if a queued UI action reaches it.
            if read_only_demo
                && matches!(
                    request.action,
                    Action::CreateEntry { remote: false, .. }
                        | Action::LocalRename(..)
                        | Action::Trash { remote: false, .. }
                        | Action::Delete { remote: false, .. }
                )
            {
                let _ = router_events.send_blocking(Response {
                    epoch: request.epoch,
                    serial: request.serial,
                    update: Update::Error("This local desktop is read-only".into()),
                });
                continue;
            }
            match request.action {
                #[cfg(windows)]
                Action::WslTerminal(name) => {
                    router_generation.store(request.epoch, Ordering::Release);
                    let _ = routed_terminal.send(crate::terminal_worker::Command::Attach {
                        epoch: request.epoch,
                        target: crate::terminal_worker::Target::Local(local_desktop::Target::Wsl(
                            name,
                        )),
                    });
                }
                #[cfg(windows)]
                Action::WslFiles(..) => {
                    if local_tx.send(request).is_err() {
                        break;
                    }
                }
                Action::Terminal(action) => {
                    let _ = routed_terminal.send(crate::terminal_worker::Command::Request {
                        epoch: request.epoch,
                        serial: request.serial,
                        action,
                    });
                }
                Action::Local(_)
                | Action::Children { remote: false, .. }
                | Action::CreateEntry { remote: false, .. }
                | Action::LocalRename(..)
                | Action::Trash { remote: false, .. }
                | Action::Delete { remote: false, .. }
                | Action::ImportSshConfig(_) => {
                    if local_tx.send(request).is_err() {
                        break;
                    }
                }
                _ => {
                    if matches!(
                        request.action,
                        Action::LocalDesktop(..)
                            | Action::Probe(..)
                            | Action::ProbeSaved(..)
                            | Action::Disconnect
                    ) {
                        router_generation.store(request.epoch, Ordering::Release);
                        let _ = routed_terminal.send(crate::terminal_worker::Command::Reset);
                    }
                    if remote_tx.send(request).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let local_events = events.clone();
    thread::spawn(move || {
        for Request {
            epoch,
            serial,
            action,
        } in local_rx
        {
            let browsing = matches!(action, Action::Local(_));
            let result = match action {
                Action::Local(path) => {
                    local::list_dir(&path).map(|(path, entries)| Update::Local(path, entries))
                }
                Action::Children {
                    remote: false,
                    path,
                } => Ok(Update::Children {
                    remote: false,
                    parent: path.clone(),
                    result: local::list_dir(Path::new(&path))
                        .map(|(_, entries)| entries)
                        .map_err(|e| format!("{e:#}")),
                }),
                Action::CreateEntry {
                    remote: false,
                    parent,
                    default_name,
                    directory,
                } => create_named_entry(&default_name, |name| {
                    let path = Path::new(&parent).join(name);
                    let result = if directory {
                        local::mkdir(&path)
                    } else {
                        local::create_file(&path)
                    };
                    match result {
                        Ok(()) => Ok(true),
                        Err(_) if std::fs::symlink_metadata(&path).is_ok() => Ok(false),
                        Err(error) => Err(error),
                    }
                })
                .map(|name| Update::Created {
                    remote: false,
                    directory,
                    name,
                }),
                Action::Trash {
                    remote: false,
                    operation,
                } => Ok(Update::Trash {
                    remote: false,
                    result: ClientFiles
                        .access_trash(&operation, &|| Ok(()))
                        .map_err(|e| format!("{e:#}")),
                }),
                Action::LocalRename(from, to) => {
                    ClientFiles
                        .rename_entry(&from, &to)
                        .map(|_| Update::Renamed {
                            remote: false,
                            name: to
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned(),
                        })
                }
                Action::Delete {
                    remote: false,
                    targets,
                    cancel,
                    ..
                } => Ok(Update::Deleted {
                    remote: false,
                    result: ClientFiles
                        .delete_entries(
                            &targets,
                            &|| {
                                anyhow::ensure!(
                                    !cancel.load(Ordering::Relaxed),
                                    "Deletion canceled"
                                );
                                Ok(())
                            },
                            &mut deletion_progress(local_events.clone(), epoch, serial),
                        )
                        .map_err(|e| FileFailure::ordinary(format!("{e:#}"))),
                }),
                #[cfg(windows)]
                Action::WslFiles(name, op) => Ok(Update::WslFiles(
                    op.run(&name).map_err(|e| format!("{e:#}")),
                )),
                Action::ImportSshConfig(path) => {
                    arcnix_core::ssh_config::import_file(&path).map(Update::Imported)
                }
                _ => unreachable!(),
            };
            let update = result.unwrap_or_else(|error| {
                if browsing {
                    Update::BrowseError {
                        remote: false,
                        message: format!("{error:#}"),
                    }
                } else {
                    Update::Error(format!("{error:#}"))
                }
            });
            if local_events
                .send_blocking(Response {
                    epoch,
                    serial,
                    update,
                })
                .is_err()
            {
                break;
            }
        }
    });
    thread::spawn(move || {
        let mut transfers =
            crate::transfer_worker::Pool::with_limiter(events.clone(), generation.clone(), limiter);
        let mut local_desktop: Option<super::local_desktop_worker::Session> = None;
        let mut pending: Option<PendingConnection> = None;
        let mut session: Option<RemoteSession> = None;
        let mut saved_login: Option<(crate::ssh_credentials::Key, zeroize::Zeroizing<String>)> =
            None;
        let mut active_epoch = 0;
        let mut active_server = String::new();
        let mut active_profile: Option<ServerProfile> = None;
        let mut session_access: Option<(String, zeroize::Zeroizing<String>)> = None;
        #[cfg(windows)]
        let mut browser_listeners = std::collections::HashMap::new();
        let mut package_environment: Option<SystemEnvironment> = None;
        for Request {
            epoch,
            serial,
            action,
        } in remote_rx
        {
            let send = |update| {
                let _ = events.send_blocking(Response {
                    epoch,
                    serial,
                    update,
                });
            };
            if matches!(
                action,
                Action::LocalDesktop(..)
                    | Action::Probe(..)
                    | Action::ProbeSaved(..)
                    | Action::Disconnect
            ) {
                local_desktop = None;
            }
            if let Action::LocalDesktop(target, prefs) = action {
                if epoch != generation.load(Ordering::Acquire) {
                    continue;
                }
                session = None;
                pending = None;
                active_profile = None;
                session_access = None;
                saved_login = None;
                transfers.reset();
                active_epoch = epoch;
                let result = target
                    .home()
                    .inspect(|home| {
                        let _ = terminal.send(crate::terminal_worker::Command::Attach {
                            epoch,
                            target: crate::terminal_worker::Target::Local(target.clone()),
                        });
                        local_desktop = Some(super::local_desktop_worker::Session {
                            target,
                            home: home.clone(),
                            prefs,
                        });
                    })
                    .map_err(|e| format!("{e:#}"));
                send(Update::LocalDesktop(result));
                continue;
            }
            let action = if let Some(desktop) = &local_desktop {
                if epoch != active_epoch || epoch != generation.load(Ordering::Acquire) {
                    continue;
                }
                if let Action::OpenPdf(path, commands) = action {
                    match desktop
                        .target
                        .read_file_bytes(&path, crate::pdf_reader::LIMIT)
                    {
                        Ok(bytes) => {
                            crate::pdf_reader::start(bytes, commands, events.clone(), epoch, serial)
                        }
                        Err(error) => send(Update::Pdf(crate::pdf_reader::Reply::Failed(format!(
                            "{error:#}"
                        )))),
                    }
                    continue;
                }
                match desktop.dispatch(action) {
                    std::ops::ControlFlow::Break(update) => {
                        send(update);
                        continue;
                    }
                    std::ops::ControlFlow::Continue(action) => action,
                }
            } else {
                action
            };
            let connection = || {
                active_profile
                    .as_ref()
                    .zip(session_access.as_ref())
                    .filter(|_| {
                        session.is_some()
                            && epoch == active_epoch
                            && epoch == generation.load(Ordering::Acquire)
                    })
            };
            if let Action::PlayVideo {
                path,
                prefer_content,
                active,
            } = action
            {
                let events = events.clone();
                let ready = move |result| {
                    let _ = events.send_blocking(Response {
                        epoch,
                        serial,
                        update: Update::Video(result),
                    });
                };
                if let Some(desktop) = &local_desktop {
                    let target = desktop.target.clone();
                    thread::spawn(move || {
                        crate::video_player::serve(
                            &path,
                            prefer_content,
                            active,
                            || {
                                let (file, size) = target.open_seekable_file(&path)?;
                                Ok(((), file, size))
                            },
                            ready,
                        )
                    });
                } else if let Some((profile, (fingerprint, secret))) = connection() {
                    let profile = profile.clone();
                    let fingerprint = fingerprint.clone();
                    let secret = secret.clone();
                    thread::spawn(move || {
                        crate::video_player::serve(
                            &path,
                            prefer_content,
                            active,
                            || {
                                let session =
                                    PendingConnection::open_for_known_host(&profile, &fingerprint)?
                                        .authenticate(&secret, &fingerprint)?;
                                let (file, size) = session.open_seekable_file(&path)?;
                                Ok((session, file, size))
                            },
                            ready,
                        )
                    });
                } else {
                    ready(Err("Connect a server first".into()));
                }
                continue;
            }
            #[cfg(windows)]
            if let Action::ReadHtml(path) = action {
                let result = session
                    .as_ref()
                    .filter(|_| epoch == active_epoch)
                    .ok_or_else(|| anyhow::anyhow!("Connect a server first"))
                    .and_then(|remote| {
                        remote.read_document(&path, &|| {
                            anyhow::ensure!(
                                generation.load(Ordering::Acquire) == epoch,
                                "Connection changed"
                            );
                            Ok(())
                        })
                    })
                    .map(|document| document.content)
                    .map_err(|error| format!("{error:#}"));
                send(Update::Html(result));
                continue;
            }
            #[cfg(windows)]
            if let Action::RepairBrowserForwarding { sudo } = action {
                let result = match session.as_ref().filter(|_| {
                    epoch == active_epoch && epoch == generation.load(Ordering::Acquire)
                }) {
                    Some(remote) => arcnix_core::browser_proxy::repair_forwarding(remote, &sudo)
                        .map_err(|error| {
                            arcnix_core::tasks::redact_log(&format!("{error:#}"), &[sudo.as_str()])
                        }),
                    None => Err("Connect a server first".into()),
                };
                send(Update::BrowserForwardingRepaired(result));
                continue;
            }
            #[cfg(windows)]
            if let Action::DiagnoseBrowser(address) = action {
                if let Some((profile, access)) = connection() {
                    let profile = profile.clone();
                    let (fingerprint, secret) = access.clone();
                    let events = events.clone();
                    thread::spawn(move || {
                        let diagnosis = arcnix_core::browser_proxy::diagnose(
                            &profile,
                            &fingerprint,
                            secret,
                            &address,
                        );
                        let _ = events.send_blocking(Response {
                            epoch,
                            serial,
                            update: Update::BrowserDiagnosis(diagnosis),
                        });
                    });
                } else {
                    send(Update::BrowserDiagnosis(
                        arcnix_core::browser_proxy::Diagnosis::TunnelUnavailable,
                    ));
                }
                continue;
            }
            #[cfg(windows)]
            if let Action::StartBrowserProxy(control) = action {
                if let Some((profile, access)) = connection() {
                    let profile = profile.clone();
                    let (fingerprint, secret) = access.clone();
                    let key = arcnix_core::browser::profile_key(&profile, &fingerprint);
                    let listener = match browser_listeners.entry(key.clone()) {
                        std::collections::hash_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            arcnix_core::browser_proxy::listener()
                                .map(|listener| entry.insert(Arc::new(listener)))
                        }
                    }
                    .map(|listener| Arc::clone(listener));
                    let listener = match listener {
                        Ok(listener) => listener,
                        Err(error) => {
                            send(Update::BrowserProxy(Err(format!("{error:#}"))));
                            continue;
                        }
                    };
                    let events = events.clone();
                    let generation = generation.clone();
                    thread::spawn(move || {
                        let stopped = || {
                            control.stop.load(Ordering::Acquire)
                                || generation.load(Ordering::Acquire) != epoch
                        };
                        let publish = |result| {
                            let _ = events.send_blocking(Response {
                                epoch,
                                serial,
                                update: Update::BrowserProxy(result),
                            });
                        };
                        while !stopped() {
                            let result = arcnix_core::browser_proxy::run(
                                &listener,
                                &profile,
                                &fingerprint,
                                secret.clone(),
                                || stopped() || control.reconnect.load(Ordering::Acquire),
                                |port| {
                                    if let Ok(mut refused) = control.refused.lock() {
                                        refused.clear();
                                    }
                                    publish(Ok((port, key.clone())));
                                },
                                |host, port| control.remember_refusal(host, port),
                            );
                            if stopped() {
                                break;
                            }
                            if control.reconnect.swap(false, Ordering::AcqRel) {
                                continue;
                            }
                            if let Err(error) = result {
                                publish(Err(format!("{error:#}")));
                            }
                            break;
                        }
                    });
                } else {
                    send(Update::BrowserProxy(Err("Connect a server first".into())));
                }
                continue;
            }
            let action = match action {
                Action::PgAdminOpen(base) => {
                    use arcnix_core::pgadmin;
                    let connection = connection()
                        .map(|(profile, (fingerprint, _))| {
                            pgadmin::saved_connection(&base, profile, fingerprint)
                        })
                        .unwrap_or_default();
                    send(Update::PgAdmin(crate::pgadmin_ui::Reply::Opened {
                        installed: pgadmin::installed(&base).is_some(),
                        size: pgadmin::local_package()
                            .map(|_| 0)
                            .or_else(|| pgadmin::package().map(|asset| asset.size)),
                        connection,
                    }));
                    continue;
                }
                Action::PgAdminInstall(base, cancel) => {
                    let events = events.clone();
                    thread::spawn(move || {
                        pgadmin_install(&base, &cancel, |reply| {
                            let _ = events.send_blocking(Response {
                                epoch,
                                serial,
                                update: Update::PgAdmin(reply),
                            });
                        });
                    });
                    continue;
                }
                Action::PgAdminStart {
                    base,
                    connection: target,
                    stop,
                } => {
                    let access = connection().map(|(profile, access)| {
                        let (fingerprint, secret) = access.clone();
                        (profile.clone(), fingerprint, secret)
                    });
                    let events = events.clone();
                    let generation = generation.clone();
                    thread::spawn(move || {
                        use arcnix_core::pgadmin;

                        use crate::pgadmin_ui::Reply;
                        let reply = |reply| {
                            let _ = events.send_blocking(Response {
                                epoch,
                                serial,
                                update: Update::PgAdmin(reply),
                            });
                        };
                        let result = (|| {
                            let (profile, fingerprint, secret) =
                                access.context("Connect a server first")?;
                            let component =
                                pgadmin::installed(&base).context("pgAdmin is not installed")?;
                            pgadmin::run(
                                &base,
                                &component,
                                &profile,
                                &fingerprint,
                                secret,
                                &target,
                                || {
                                    stop.load(Ordering::Acquire)
                                        || generation.load(Ordering::Acquire) != epoch
                                },
                                |event| {
                                    reply(match event {
                                        pgadmin::Event::Ready(url) => Reply::Ready(url),
                                        pgadmin::Event::Unreachable(diagnosis) => {
                                            Reply::Unreachable(diagnosis)
                                        }
                                    })
                                },
                            )
                        })();
                        reply(Reply::Ended(result.map_err(|error| format!("{error:#}"))));
                    });
                    continue;
                }
                action => action,
            };
            if let Action::ServerApps(action) = action {
                let outcome = (|| {
                    anyhow::ensure!(
                        epoch == generation.load(Ordering::Acquire) && epoch == active_epoch,
                        "Server connection has changed"
                    );
                    let remote = session
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Connect a server first"))?;
                    crate::server_apps_worker::handle(remote, action)
                })();
                if let Err(error) = &outcome
                    && arcnix_core::transport_error::is_connection_error(error)
                {
                    session = None;
                    pending = None;
                    transfers.reset();
                    package_environment = None;
                    send(Update::ConnectionLost(format!("{error:#}")));
                }
                send(Update::ServerApps(Box::new(
                    outcome.map_err(|e| format!("{e:#}")),
                )));
                continue;
            }
            if let Action::Operations(action) = action {
                let kind = action.kind();
                let result: anyhow::Result<()> = (|| {
                    anyhow::ensure!(
                        epoch == generation.load(Ordering::Acquire) && epoch == active_epoch,
                        "Server connection has changed"
                    );
                    let remote = session
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Connect a server first"))?;
                    crate::operations_worker::handle(remote, action, |e| {
                        send(Update::Operations(e))
                    })
                })();
                if let Err(error) = result {
                    if arcnix_core::transport_error::is_connection_error(&error) {
                        session = None;
                        pending = None;
                        transfers.reset();
                        send(Update::ConnectionLost(format!("{error:#}")));
                    }
                    send(Update::Operations(crate::operations_worker::Event::Failed(
                        kind,
                        format!("{error:#}"),
                    )));
                }
                continue;
            }
            if matches!(
                action,
                Action::PortsRead { .. } | Action::PortTerminate { .. }
            ) {
                let terminating = matches!(action, Action::PortTerminate { .. });
                let outcome: anyhow::Result<()> = (|| {
                    anyhow::ensure!(
                        epoch == generation.load(Ordering::Acquire) && epoch == active_epoch,
                        "Server connection has changed"
                    );
                    let remote = session
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Connect a server first"))?;
                    match action {
                        Action::PortsRead {
                            administrator,
                            sudo,
                            cancel,
                        } => {
                            let snapshot = remote.read_listening_ports(
                                administrator,
                                (!sudo.is_empty()).then_some(sudo.as_str()),
                                &cancel,
                            )?;
                            send(Update::Ports(Ok(snapshot)));
                        }
                        Action::PortTerminate {
                            target,
                            administrator,
                            sudo,
                            cancel,
                        } => {
                            remote.terminate_port_holder(
                                &target,
                                administrator,
                                (!sudo.is_empty()).then_some(sudo.as_str()),
                                &cancel,
                            )?;
                            send(Update::PortTerminated(Ok(())));
                        }
                        _ => unreachable!(),
                    }
                    Ok(())
                })();
                if let Err(error) = outcome {
                    if arcnix_core::transport_error::is_connection_error(&error) {
                        session = None;
                        pending = None;
                        transfers.reset();
                        package_environment = None;
                        send(Update::ConnectionLost(format!("{error:#}")));
                    }
                    let message = format!("{error:#}");
                    send(if terminating {
                        Update::PortTerminated(Err(message))
                    } else {
                        Update::Ports(Err(message))
                    });
                }
                continue;
            }
            if matches!(
                action,
                Action::MetricsSample | Action::LogsDiscover | Action::LogsRead(_)
            ) {
                let kind = match &action {
                    Action::MetricsSample => 0,
                    Action::LogsDiscover => 1,
                    _ => 2,
                };
                let outcome: anyhow::Result<()> = (|| {
                    anyhow::ensure!(
                        epoch == generation.load(Ordering::Acquire) && epoch == active_epoch,
                        "连接已切换，采集已停止"
                    );
                    let remote = session
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("请先连接服务器"))?;
                    send(match action {
                        Action::MetricsSample => Update::Metrics(Ok(remote.sample_resources()?)),
                        Action::LogsDiscover => {
                            Update::LogCapabilities(Ok(remote.discover_system_logs()?))
                        }
                        Action::LogsRead(query) => {
                            Update::SystemLogs(Ok(remote.read_system_logs(&query)?))
                        }
                        _ => unreachable!(),
                    });
                    Ok(())
                })();
                if let Err(error) = outcome {
                    if arcnix_core::transport_error::is_connection_error(&error) {
                        session = None;
                        pending = None;
                        transfers.reset();
                        package_environment = None;
                        send(Update::ConnectionLost(format!("{error:#}")));
                    }
                    let message = format!("{error:#}");
                    send(match kind {
                        0 => Update::Metrics(Err(message)),
                        1 => Update::LogCapabilities(Err(message)),
                        _ => Update::SystemLogs(Err(message)),
                    });
                }
                continue;
            }
            if matches!(
                action,
                Action::PackageDetect | Action::PackageSearch(_) | Action::PackageInstalled
            ) {
                let detect = matches!(action, Action::PackageDetect);
                let outcome: anyhow::Result<()> = (|| {
                    anyhow::ensure!(
                        epoch == generation.load(Ordering::Acquire) && epoch == active_epoch,
                        "Server connection has changed"
                    );
                    let remote = session
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Connect a server first"))?;
                    if package_environment.is_none() || detect {
                        package_environment = Some(remote.read_package_environment()?);
                    }
                    let environment = package_environment.as_ref().expect("environment detected");
                    send(match action {
                        Action::PackageDetect => {
                            Update::PackageEnvironment(Ok(environment.clone()))
                        }
                        Action::PackageSearch(query) => {
                            Update::PackageList(Ok(remote.search_packages(environment, &query)?))
                        }
                        Action::PackageInstalled => {
                            Update::PackageList(Ok(remote.read_installed_packages(environment)?))
                        }
                        _ => unreachable!(),
                    });
                    Ok(())
                })();
                if let Err(error) = outcome {
                    if arcnix_core::transport_error::is_connection_error(&error) {
                        session = None;
                        pending = None;
                        transfers.reset();
                        package_environment = None;
                        send(Update::ConnectionLost(format!("{error:#}")));
                    }
                    let message = format!("{error:#}");
                    send(if detect {
                        Update::PackageEnvironment(Err(message))
                    } else {
                        Update::PackageList(Err(message))
                    });
                }
                continue;
            }
            if let Action::TransferInteractive {
                job,
                cancel,
                paused,
                policy,
            } = action
            {
                transfers.submit(epoch, serial, job, cancel, paused, policy);
                continue;
            }
            let browsing = matches!(action, Action::Remote(_));
            let file_kind = match &action {
                Action::OpenPdf(..) => Some(8),
                Action::Trash { .. } => Some(4),
                Action::Delete { .. } => Some(0),
                Action::Permissions { .. } => Some(1),
                Action::FileMetadata(_) => Some(2),
                Action::CodeFile(_) | Action::AttachCodePage { .. } => Some(3),
                Action::TextRead { .. } | Action::TextSave { .. } | Action::TextSaveAs { .. } => {
                    Some(7)
                }
                Action::ManageRemoteFiles(..) | Action::ManageFilesAsAdmin(..) => Some(5),
                Action::ResolveFileAlias(_) => Some(6),
                _ => None,
            };
            let mut file_completed = false;
            let child_path = match &action {
                Action::Children { remote: true, path } => Some(path.clone()),
                _ => None,
            };
            let action = match action {
                Action::ForgetCredentials(keys, report_success) => {
                    if saved_login
                        .as_ref()
                        .is_some_and(|(key, _)| keys.contains(key))
                    {
                        saved_login = None;
                    }
                    let mut failed = false;
                    for key in keys {
                        failed |= key.forget().is_err();
                    }
                    if failed {
                        send(Update::CredentialNotice(
                            "Could not remove the SSH credential from the system credential store.",
                        ));
                    } else if report_success {
                        send(Update::CredentialNotice(
                            "No saved SSH credential remains for this server.",
                        ));
                    }
                    continue;
                }
                action => action,
            };
            let try_saved = matches!(&action, Action::ProbeSaved(..));
            let expected = match &action {
                Action::Probe(_, expected) => expected.clone(),
                Action::ProbeSaved(_, expected) => Some(expected.clone()),
                _ => None,
            };
            let connecting =
                try_saved || matches!(action, Action::Probe(..) | Action::Authenticate { .. });
            let had_session = session.is_some();
            let result: anyhow::Result<()> = (|| {
                anyhow::ensure!(
                    epoch == generation.load(Ordering::Acquire),
                    "连接已切换，操作已忽略"
                );
                match action {
                    Action::Probe(profile, ..) | Action::ProbeSaved(profile, ..) => {
                        session_access = None;
                        saved_login = None;
                        let profile = *profile;
                        package_environment = None;
                        active_epoch = epoch;
                        active_server = profile.id.clone();
                        active_profile = Some(profile.clone());
                        session = None;
                        pending = None;
                        transfers.reset();
                        if try_saved {
                            let expected = expected
                                .as_deref()
                                .context("Saved SSH credentials require a trusted host")?;
                            let key = crate::ssh_credentials::Key::new(&profile, expected);
                            match key.load() {
                                Ok(Some(secret)) => saved_login = Some((key, secret)),
                                Ok(None) => {
                                    send(Update::CredentialPrompt(None));
                                    return Ok(());
                                }
                                Err(()) => {
                                    send(Update::CredentialPrompt(Some(
                                        "Could not read the system credential store. Enter your SSH credential to connect.",
                                    )));
                                    return Ok(());
                                }
                            }
                            if epoch != generation.load(Ordering::Acquire) {
                                saved_login = None;
                                return Ok(());
                            }
                        }
                        let connection = match expected {
                            Some(expected) => {
                                PendingConnection::open_for_known_host(&profile, &expected)?
                            }
                            None => PendingConnection::open(&profile)?,
                        };
                        let fingerprint = connection.fingerprint().to_string();
                        pending = Some(connection);
                        send(Update::Fingerprint(fingerprint));
                    }
                    Action::Authenticate { login, fingerprint } => {
                        if epoch != active_epoch {
                            return Ok(());
                        }
                        let connection = pending
                            .take()
                            .ok_or_else(|| anyhow::anyhow!("连接已失效，请重新连接"))?;
                        let profile = active_profile.as_ref().context("Missing SSH profile")?;
                        let (secret, remember, saved_key) = match login {
                            Login::Manual { secret, remember } => (secret, remember, None),
                            Login::Saved => {
                                let Some((key, secret)) = saved_login.take() else {
                                    send(Update::CredentialPrompt(None));
                                    return Ok(());
                                };
                                // The item was loaded for the saved trust record. Verify that
                                // this is still the fingerprint associated with that item.
                                if key != crate::ssh_credentials::Key::new(profile, &fingerprint) {
                                    send(Update::CredentialPrompt(None));
                                    return Ok(());
                                }
                                (secret, false, Some(key))
                            }
                        };
                        match connection.authenticate(&secret, &fingerprint) {
                            Ok(remote) => session = Some(remote),
                            Err(error) => {
                                if let Some(key) = saved_key
                                    && crate::ssh_credentials::rejected(&error, &profile.auth)
                                    && key.forget().is_err()
                                {
                                    send(Update::CredentialNotice(
                                        "Could not remove the SSH credential from the system credential store.",
                                    ));
                                }
                                let mut message = format!("{error:#}");
                                if !secret.is_empty() {
                                    message = message.replace(secret.as_str(), "[redacted]");
                                }
                                send(Update::ConnectionError(message));
                                return Ok(());
                            }
                        }
                        if remember
                            && !secret.is_empty()
                            && epoch == generation.load(Ordering::Acquire)
                            && crate::ssh_credentials::Key::new(profile, &fingerprint)
                                .save(&secret)
                                .is_err()
                        {
                            send(Update::CredentialNotice(
                                "Connected, but could not save the SSH credential to the system credential store.",
                            ));
                        }
                        session_access = Some((fingerprint.clone(), secret.clone()));
                        if let Some(profile) = active_profile.clone() {
                            transfers.attach(
                                epoch,
                                profile.clone(),
                                fingerprint.clone(),
                                secret.clone(),
                            );
                            let _ = terminal.send(crate::terminal_worker::Command::Attach {
                                epoch,
                                target: crate::terminal_worker::Target::Ssh {
                                    profile: Box::new(profile),
                                    fingerprint,
                                    secret,
                                },
                            });
                        }
                        send(Update::Connected(
                            session.as_ref().is_some_and(|s| s.sftp_available()),
                        ));
                    }
                    Action::Disconnect => {
                        saved_login = None;
                        session_access = None;
                        transfers.reset();
                        package_environment = None;
                        active_epoch = epoch;
                        session = None;
                        pending = None;
                        transfers.reset();
                        send(Update::Disconnected);
                    }
                    action => {
                        if epoch != active_epoch {
                            anyhow::bail!("连接已切换，操作已忽略");
                        }
                        let remote = session
                            .as_mut()
                            .ok_or_else(|| anyhow::anyhow!("请先连接服务器"))?;
                        match action {
                            Action::OpenPdf(path, commands) => {
                                let bytes =
                                    remote.read_file_bytes(&path, crate::pdf_reader::LIMIT)?;
                                crate::pdf_reader::start(
                                    bytes,
                                    commands,
                                    events.clone(),
                                    epoch,
                                    serial,
                                );
                                file_completed = true;
                            }
                            Action::TextRead { path, cancel, sudo } => {
                                file_completed = true;
                                let check = || {
                                    anyhow::ensure!(
                                        generation.load(Ordering::Acquire) == epoch
                                            && !cancel.load(Ordering::Relaxed),
                                        "File read canceled or connection changed"
                                    );
                                    Ok(())
                                };
                                let result = match &sudo {
                                    Some((retry, password)) => ReviewedDocument {
                                        session: remote,
                                        retry,
                                        password,
                                        cancel: &cancel,
                                    }
                                    .read_document(&path, &check),
                                    None => remote.read_document(&path, &check),
                                };
                                publish_text_result(
                                    result,
                                    sudo,
                                    check().is_ok(),
                                    || remote.prepare_admin_read(&path),
                                    |r| send(Update::TextFile(r)),
                                )?;
                            }
                            Action::TextSaveAs {
                                path,
                                bytes,
                                replace,
                                cancel,
                            } => {
                                file_completed = true;
                                publish_elevatable_result(
                                    remote.write_document_destination(
                                        &path,
                                        &bytes,
                                        replace,
                                        &|| {
                                            anyhow::ensure!(
                                                generation.load(Ordering::Acquire) == epoch
                                                    && !cancel.load(Ordering::Relaxed),
                                                "File save canceled or connection changed"
                                            );
                                            Ok(())
                                        },
                                    ),
                                    false,
                                    |result| send(Update::TextFile(result)),
                                )?;
                            }
                            Action::TextSave {
                                document,
                                content,
                                cancel,
                                sudo,
                            } => {
                                file_completed = true;
                                let content = zeroize::Zeroizing::new(content);
                                let check = || {
                                    anyhow::ensure!(
                                        generation.load(Ordering::Acquire) == epoch
                                            && !cancel.load(Ordering::Relaxed),
                                        "File save canceled or connection changed"
                                    );
                                    Ok(())
                                };
                                let result = match &sudo {
                                    Some((retry, password)) => ReviewedDocument {
                                        session: remote,
                                        retry,
                                        password,
                                        cancel: &cancel,
                                    }
                                    .save_document(&document, &content, &check),
                                    None => remote.save_document(&document, &content, &check),
                                };
                                publish_text_result(
                                    result,
                                    sudo,
                                    check().is_ok(),
                                    || remote.prepare_admin_save(&document, &content),
                                    |r| send(Update::TextFile(r)),
                                )?;
                            }
                            Action::AttachCodePage {
                                path,
                                snapshot,
                                sudo,
                            } => {
                                file_completed = true;
                                let handle = sudo.as_ref().map(|_| serial);
                                let page = TextPage {
                                    path: &path,
                                    snapshot,
                                    reviewed_handle: handle,
                                };
                                let result = match &sudo {
                                    Some((retry, password)) => {
                                        remote.attach_reviewed_page(page, retry, password)
                                    }
                                    None => remote.sample_page_tail(page, None),
                                };
                                publish_text_result(
                                    result,
                                    sudo,
                                    generation.load(Ordering::Acquire) == epoch,
                                    || remote.prepare_admin_page(&path),
                                    |result| match result {
                                        Ok((snapshot, stamp)) => send(Update::CodeFile(Ok(
                                            serde_json::json!({ "snapshot":snapshot, "stamp":stamp, "handle":handle }),
                                        ))),
                                        Err(error) => send(Update::TextFile(Err(error))),
                                    },
                                )?;
                            }
                            Action::CodeFile(request) => {
                                let result = request.execute(
                                    crate::code_workbench::Files::Remote(remote),
                                    || {
                                        anyhow::ensure!(
                                            generation.load(Ordering::Acquire) == epoch,
                                            "Connection changed"
                                        );
                                        Ok(())
                                    },
                                );
                                file_completed = true;
                                match result {
                                    Ok(value) => send(Update::CodeFile(Ok(value))),
                                    Err(error) => {
                                        send(Update::CodeFile(Err(format!("{error:#}"))));
                                        if arcnix_core::transport_error::is_connection_error(&error)
                                        {
                                            return Err(error);
                                        }
                                    }
                                }
                            }
                            Action::Keepalive => {
                                remote.keepalive()?;
                                send(Update::Alive);
                            }
                            Action::Remote(path) => {
                                let (path, entries) = remote.list_entries(&path)?;
                                send(Update::Remote(path, entries));
                            }
                            Action::Children { remote: true, path } => {
                                let result = remote.list_entries(&path).map(|(_, entries)| entries);
                                match result {
                                    Ok(entries) => send(Update::Children {
                                        remote: true,
                                        parent: path,
                                        result: Ok(entries),
                                    }),
                                    Err(error)
                                        if arcnix_core::transport_error::is_connection_error(
                                            &error,
                                        ) =>
                                    {
                                        return Err(error);
                                    }
                                    Err(error) => send(Update::Children {
                                        remote: true,
                                        parent: path,
                                        result: Err(format!("{error:#}")),
                                    }),
                                }
                            }
                            Action::CreateEntry {
                                remote: true,
                                parent,
                                default_name,
                                directory,
                            } => {
                                let name = create_named_entry(&default_name, |name| {
                                    remote.try_create_named(
                                        &parent,
                                        name,
                                        EntryKind::from_directory(directory),
                                    )
                                })?;
                                send(Update::Created {
                                    remote: true,
                                    directory,
                                    name,
                                });
                            }
                            Action::Trash {
                                remote: true,
                                operation,
                            } => {
                                file_completed = true;
                                publish_file_result(
                                    remote.access_trash(&operation, &|| {
                                        anyhow::ensure!(
                                            generation.load(Ordering::Acquire) == epoch,
                                            "Connection changed"
                                        );
                                        Ok(())
                                    }),
                                    |result| {
                                        send(Update::Trash {
                                            remote: true,
                                            result,
                                        })
                                    },
                                )?;
                            }
                            Action::RemoteRename(from, to) => {
                                remote.rename_entry(&from, &to)?;
                                send(Update::Renamed {
                                    remote: true,
                                    name: to.rsplit('/').next().unwrap_or(&to).to_owned(),
                                });
                            }
                            Action::ManageRemoteFiles(action, paths) => {
                                file_completed = true;
                                let check = || {
                                    anyhow::ensure!(
                                        generation.load(Ordering::Acquire) == epoch,
                                        "Connection changed"
                                    );
                                    Ok(())
                                };
                                let result =
                                    remote.apply_file_action(action.clone(), &paths, &check);
                                if let Err(error) = &result
                                    && let Some(remaining) = action.retry_sources(&paths, error)
                                    && check().is_ok()
                                {
                                    match remote.prepare_managed_retry(action, remaining.to_vec()) {
                                        Ok(retry) => send(Update::FileAdminRequired(retry)),
                                        Err(reason) => send(Update::ManagedRemoteFiles(Err(
                                            format!("{error:#}. {reason:#}"),
                                        ))),
                                    }
                                } else {
                                    publish_file_result(result, |result| {
                                        send(Update::ManagedRemoteFiles(result))
                                    })?;
                                }
                            }
                            Action::ManageFilesAsAdmin(mut retry, password) => {
                                file_completed = true;
                                let result =
                                    remote.apply_reviewed_file_action(&retry, &password, &|| {
                                        anyhow::ensure!(
                                            generation.load(Ordering::Acquire) == epoch,
                                            "Connection changed"
                                        );
                                        Ok(())
                                    });
                                if password.is_empty()
                                    && result
                                        .as_ref()
                                        .is_err_and(|e| e.is::<remote::SudoPasswordRequired>())
                                {
                                    retry.needs_password = true;
                                    send(Update::FileAdminRequired(retry));
                                } else {
                                    publish_file_result(result, |result| {
                                        send(Update::ManagedRemoteFiles(result))
                                    })?;
                                }
                            }
                            Action::ResolveFileAlias(path) => {
                                file_completed = true;
                                publish_file_result(remote.resolve_file_alias(&path), |result| {
                                    send(Update::FileAlias(result))
                                })?;
                            }
                            Action::FileMetadata(path) => {
                                file_completed = true;
                                publish_file_result(remote.file_properties(&path), |result| {
                                    send(Update::FileMetadata(result))
                                })?;
                            }
                            Action::Permissions {
                                targets,
                                mode,
                                cancel,
                            } => {
                                file_completed = true;
                                publish_file_result(
                                    remote.update_file_permissions(&targets, mode, &|| {
                                        anyhow::ensure!(
                                            generation.load(Ordering::Acquire) == epoch
                                                && !cancel.load(Ordering::Relaxed),
                                            "Permission update canceled or connection changed"
                                        );
                                        Ok(())
                                    }),
                                    |result| send(Update::Permissions(result)),
                                )?;
                            }
                            Action::Delete {
                                remote: true,
                                targets,
                                cancel,
                                sudo,
                            } => {
                                file_completed = true;
                                let check = || {
                                    anyhow::ensure!(
                                        generation.load(Ordering::Acquire) == epoch
                                            && !cancel.load(Ordering::Relaxed),
                                        "Deletion canceled or connection changed"
                                    );
                                    Ok(())
                                };
                                let mut removed = 0;
                                let mut progress = deletion_progress(events.clone(), epoch, serial);
                                let result = match sudo.as_deref() {
                                    Some(password) => remote.delete_reviewed_entries(
                                        &targets, password, &cancel, &check,
                                    ),
                                    None => remote.delete_entries(&targets, &check, &mut |step| {
                                        removed = step.removed;
                                        progress(step);
                                    }),
                                };
                                publish_elevatable_result(
                                    result,
                                    sudo.is_none() && removed == 0 && check().is_ok(),
                                    |result| {
                                        send(Update::Deleted {
                                            remote: true,
                                            result,
                                        })
                                    },
                                )?;
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                if let Some(kind) = file_kind {
                    if !file_completed {
                        let message = format!("{error:#}");
                        send(match kind {
                            0 => Update::Deleted {
                                remote: true,
                                result: Err(FileFailure::ordinary(message)),
                            },
                            1 => Update::Permissions(Err(message)),
                            3 => Update::CodeFile(Err(message)),
                            4 => Update::Trash {
                                remote: true,
                                result: Err(message),
                            },
                            5 => Update::ManagedRemoteFiles(Err(message)),
                            6 => Update::FileAlias(Err(message)),
                            7 => Update::TextFile(Err(FileFailure::ordinary(message))),
                            8 => Update::Pdf(crate::pdf_reader::Reply::Failed(message)),
                            _ => Update::FileMetadata(Err(message)),
                        });
                    }
                    if had_session && arcnix_core::transport_error::is_connection_error(&error) {
                        session = None;
                        pending = None;
                        transfers.reset();
                        send(Update::ConnectionLost(format!("{error:#}")));
                    }
                    continue;
                }
                if had_session && arcnix_core::transport_error::is_connection_error(&error) {
                    session = None;
                    pending = None;
                    transfers.reset();
                    send(Update::ConnectionLost(format!("{error:#}")));
                } else if let Some(parent) = child_path {
                    send(Update::Children {
                        remote: true,
                        parent,
                        result: Err(format!("{error:#}")),
                    });
                } else if browsing {
                    send(Update::BrowseError {
                        remote: true,
                        message: format!("{error:#}"),
                    });
                } else if connecting {
                    saved_login = None;
                    send(Update::ConnectionError(format!("{error:#}")));
                } else {
                    send(Update::Error(format!("{error:#}")));
                }
            }
        }
    });
    (tx, receive)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result};

    use super::*;

    fn receive(rx: &async_channel::Receiver<Response>) -> Result<Response> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match rx.try_recv() {
                Ok(response) => return Ok(response),
                Err(async_channel::TryRecvError::Closed) => {
                    anyhow::bail!("worker event channel closed")
                }
                Err(async_channel::TryRecvError::Empty) if Instant::now() >= deadline => {
                    anyhow::bail!("worker event timed out")
                }
                Err(async_channel::TryRecvError::Empty) => thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    #[test]
    fn disconnected_observation_requests_have_terminal_results() -> Result<()> {
        let (tx, rx) = start();
        for (serial, action) in [
            Action::MetricsSample,
            Action::LogsDiscover,
            Action::LogsRead(LogQuery::default()),
            Action::PortsRead {
                administrator: false,
                sudo: zeroize::Zeroizing::new(String::new()),
                cancel: Arc::new(AtomicBool::new(false)),
            },
        ]
        .into_iter()
        .enumerate()
        {
            tx.send(Request {
                epoch: 0,
                serial: serial as u64 + 1,
                action,
            })
            .map_err(|_| anyhow::anyhow!("worker stopped"))?;
            let response = receive(&rx)?;
            assert_eq!(response.serial, serial as u64 + 1);
            assert!(matches!(
                response.update,
                Update::Metrics(Err(_))
                    | Update::LogCapabilities(Err(_))
                    | Update::SystemLogs(Err(_))
                    | Update::Ports(Err(_))
            ));
        }
        Ok(())
    }

    #[test]
    fn port_termination_requests_finish_without_a_session_and_reject_expired_epochs() -> Result<()>
    {
        let (tx, rx) = start();
        let target: arcnix_core::resource_ports::Target =
            serde_json::from_value(serde_json::json!({
                "boot_id":"12345678-1234-1234-1234-123456789abc", "namespace":123,
                "listener":{"protocol":"tcp","family":4,"address":"127.0.0.1","port":45678,
                    "inode":321,"uid":1000,"holders":[],"holders_limited":false},
                "holder":{"pid":1234,"start_ticks":9876,"uid":1000,"fd":3,"command":"owned-fixture",
                    "executable":"/usr/bin/owned-fixture","unit":null}
            }))?;
        for (epoch, serial) in [(0, 1), (99, 2)] {
            tx.send(Request {
                epoch,
                serial,
                action: Action::PortTerminate {
                    target: Box::new(target.clone()),
                    administrator: false,
                    sudo: zeroize::Zeroizing::new(String::new()),
                    cancel: Arc::new(AtomicBool::new(false)),
                },
            })?;
            let response = receive(&rx)?;
            assert_eq!((response.epoch, response.serial), (epoch, serial));
            match response.update {
                Update::PortTerminated(Err(error)) => assert!(error.contains(if epoch == 0 {
                    "Connect a server first"
                } else {
                    "Server connection has changed"
                })),
                _ => anyhow::bail!("Port termination did not return a terminal error"),
            }
        }
        Ok(())
    }

    #[test]
    fn file_tasks_finish_when_disconnected_or_queued_on_an_expired_connection() -> Result<()> {
        let (tx, rx) = start();
        for (serial, action) in [
            (
                1,
                Action::Delete {
                    remote: true,
                    targets: vec![],
                    cancel: Arc::new(AtomicBool::new(false)),
                    sudo: None,
                },
            ),
            (
                2,
                Action::Permissions {
                    targets: vec![],
                    mode: 0o644,
                    cancel: Arc::new(AtomicBool::new(false)),
                },
            ),
            (3, Action::FileMetadata("/file".into())),
        ] {
            tx.send(Request {
                epoch: 0,
                serial,
                action,
            })?;
            let response = receive(&rx)?;
            assert_eq!(response.serial, serial);
            assert!(matches!(
                response.update,
                Update::Deleted { result: Err(_), .. }
                    | Update::Permissions(Err(_))
                    | Update::FileMetadata(Err(_))
            ));
        }
        tx.send(Request {
            epoch: 12,
            serial: 4,
            action: Action::Disconnect,
        })?;
        assert!(matches!(receive(&rx)?.update, Update::Disconnected));
        tx.send(Request {
            epoch: 0,
            serial: 5,
            action: Action::Delete {
                remote: true,
                targets: vec![],
                cancel: Arc::new(AtomicBool::new(false)),
                sudo: None,
            },
        })?;
        assert!(matches!(
            receive(&rx)?.update,
            Update::Deleted { result: Err(_), .. }
        ));
        Ok(())
    }

    #[test]
    #[ignore = "requires loopback SSH fixture"]
    fn observation_bridge_uses_ssh_without_sftp_and_rejects_stale_session() -> Result<()> {
        let address: SocketAddr = std::env::var("ARCNIX_TEST_SSH_ADDR")?.parse()?;
        anyhow::ensure!(address.ip().is_loopback(), "fixture must be loopback");
        let (tx, rx) = start();
        let send = |epoch, serial, action| {
            tx.send(Request {
                epoch,
                serial,
                action,
            })
            .map_err(|_| anyhow::anyhow!("worker stopped"))
        };
        send(
            88,
            1,
            Action::Probe(
                ServerProfile {
                    host: address.ip().to_string(),
                    port: address.port(),
                    username: "arcnix-nosftp".into(),
                    ..Default::default()
                }
                .into(),
                None,
            ),
        )?;
        let fingerprint = match receive(&rx)?.update {
            Update::Fingerprint(value) => value,
            _ => anyhow::bail!("expected fingerprint"),
        };
        send(
            88,
            2,
            Action::Authenticate {
                login: crate::worker::Login::Manual {
                    secret: zeroize::Zeroizing::new("arcnix-test".into()),
                    remember: false,
                },
                fingerprint,
            },
        )?;
        assert!(matches!(receive(&rx)?.update, Update::Connected(false)));
        send(88, 3, Action::MetricsSample)?;
        let first = match receive(&rx)?.update {
            Update::Metrics(Ok(snapshot)) => snapshot,
            _ => anyhow::bail!("expected resource snapshot"),
        };
        send(88, 4, Action::MetricsSample)?;
        let second = match receive(&rx)?.update {
            Update::Metrics(Ok(snapshot)) => snapshot,
            _ => anyhow::bail!("expected second resource snapshot"),
        };
        let usage = second.usage(Some(&first.snapshot));
        assert_eq!(usage.cpu_percent, Some(30.0));
        assert_eq!(usage.receive_bytes_per_second, Some(1024.0));
        send(88, 5, Action::LogsDiscover)?;
        assert!(matches!(
            receive(&rx)?.update,
            Update::LogCapabilities(Ok(_))
        ));
        send(88, 6, Action::LogsRead(LogQuery::default()))?;
        match receive(&rx)?.update {
            Update::SystemLogs(Ok(snapshot)) => assert_eq!(snapshot.entries.len(), 2),
            _ => anyhow::bail!("expected journal snapshot"),
        }
        send(89, 7, Action::Disconnect)?;
        assert!(matches!(receive(&rx)?.update, Update::Disconnected));
        send(88, 8, Action::MetricsSample)?;
        let stale = receive(&rx)?;
        assert_eq!(stale.epoch, 88);
        assert!(matches!(stale.update, Update::Metrics(Err(_))));
        Ok(())
    }

    #[test]
    #[ignore = "requires tools/sftp_fixture.py and ARCNIX_TEST_SSH_ADDR"]
    fn worker_bridge_authenticates_browses_and_rejects_wrong_server() -> Result<()> {
        let addr: SocketAddr = std::env::var("ARCNIX_TEST_SSH_ADDR")
            .context("start loopback fixture and set ARCNIX_TEST_SSH_ADDR")?
            .parse()?;
        anyhow::ensure!(addr.ip().is_loopback(), "fixture must be loopback-only");
        let profile = ServerProfile {
            id: "worker-fixture".into(),
            name: "Worker integration".into(),
            host: addr.ip().to_string(),
            port: addr.port(),
            username: "arcnix".into(),
            auth: AuthMethod::Password,
            ..Default::default()
        };
        let (tx, rx) = start();
        let send = |epoch, action| {
            tx.send(Request {
                epoch,
                serial: epoch,
                action,
            })
            .map_err(|_| anyhow::anyhow!("worker request channel closed"))
        };
        send(77, Action::Probe(profile.into(), None))?;
        let response = receive(&rx)?;
        assert_eq!(response.epoch, 77);
        let fingerprint = match response.update {
            Update::Fingerprint(value) => value,
            Update::Error(error) => anyhow::bail!("probe: {error}"),
            _ => anyhow::bail!("expected pending fingerprint before authentication"),
        };
        assert!(fingerprint.starts_with("SHA256:"));
        send(
            77,
            Action::Authenticate {
                login: crate::worker::Login::Manual {
                    secret: zeroize::Zeroizing::new("arcnix-test".into()),
                    remember: false,
                },
                fingerprint,
            },
        )?;
        assert!(matches!(receive(&rx)?.update, Update::Connected(true)));
        send(77, Action::Remote("/".into()))?;
        match receive(&rx)?.update {
            Update::Remote(path, entries) => {
                assert_eq!(path, "/");
                assert!(entries.iter().any(|entry| entry.name == "中文 空格.txt"));
            }
            Update::Error(error) => anyhow::bail!("browse: {error}"),
            _ => anyhow::bail!("expected directory listing"),
        }
        let id = new_id();
        send(
            77,
            Action::transfer(
                TransferJob {
                    id: id.clone(),
                    server_id: "different-server".into(),
                    direction: TransferDirection::Upload,
                    local_path: "nonexistent-worker-source".into(),
                    remote_path: "/must-not-be-written".into(),
                    total_bytes: 0,
                    transferred_bytes: 0,
                    status: TransferStatus::Queued,
                },
                Arc::new(AtomicBool::new(false)),
            ),
        )?;
        match receive(&rx)?.update {
            Update::Finished(received, Err(error)) => {
                assert_eq!(received, id);
                assert!(error.contains("服务器会话已变化"));
            }
            _ => anyhow::bail!("wrong-server transfer was not rejected"),
        }
        send(
            76,
            Action::CreateEntry {
                remote: true,
                parent: "/".into(),
                default_name: "must-not-be-created".into(),
                directory: true,
            },
        )?;
        let stale = receive(&rx)?;
        assert_eq!(stale.epoch, 76);
        assert!(matches!(stale.update, Update::Error(_)));
        send(77, Action::Remote("/__fixture_disconnect__".into()))?;
        let lost = receive(&rx)?;
        assert!(
            matches!(lost.update, Update::ConnectionLost(_)),
            "socket loss must clear connected state"
        );
        send(
            77,
            Action::transfer(
                TransferJob {
                    id: "after-loss".into(),
                    server_id: "worker-fixture".into(),
                    direction: TransferDirection::Download,
                    local_path: "must-not-be-written".into(),
                    remote_path: "/welcome.txt".into(),
                    total_bytes: 0,
                    transferred_bytes: 0,
                    status: TransferStatus::Queued,
                },
                Arc::new(AtomicBool::new(false)),
            ),
        )?;
        assert!(matches!(receive(&rx)?.update, Update::Finished(id, Err(_)) if id == "after-loss"));
        send(78, Action::Disconnect)?;
        let disconnected = receive(&rx)?;
        assert_eq!(disconnected.epoch, 78);
        assert!(matches!(disconnected.update, Update::Disconnected));
        Ok(())
    }

    #[test]
    fn disconnected_and_stale_transfers_always_finish() -> Result<()> {
        let (tx, rx) = start();
        tx.send(Request {
            epoch: 3,
            serial: 1,
            action: Action::Disconnect,
        })
        .map_err(|_| anyhow::anyhow!("worker stopped"))?;
        let job = TransferJob {
            id: "stale-job".into(),
            server_id: "missing".into(),
            direction: TransferDirection::Upload,
            local_path: "absent".into(),
            remote_path: "/absent".into(),
            total_bytes: 0,
            transferred_bytes: 0,
            status: TransferStatus::Queued,
        };
        tx.send(Request {
            epoch: 2,
            serial: 9,
            action: Action::transfer(job.clone(), Arc::new(AtomicBool::new(false))),
        })
        .map_err(|_| anyhow::anyhow!("worker stopped"))?;
        assert!(matches!(receive(&rx)?.update, Update::Disconnected));
        let response = receive(&rx)?;
        assert_eq!(response.serial, 9);
        assert!(matches!(response.update, Update::Finished(id, Err(_)) if id == "stale-job"));
        tx.send(Request {
            epoch: 3,
            serial: 10,
            action: Action::transfer(job, Arc::new(AtomicBool::new(false))),
        })
        .map_err(|_| anyhow::anyhow!("worker stopped"))?;
        assert!(matches!(receive(&rx)?.update, Update::Finished(_, Err(_))));
        Ok(())
    }

    #[test]
    fn local_browsing_stays_available_during_ssh_handshake() -> Result<()> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            if let Ok((socket, _)) = listener.accept() {
                let _ = accepted_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
                drop(socket);
            }
        });
        let (tx, rx) = start();
        let profile = ServerProfile {
            host: address.ip().to_string(),
            port: address.port(),
            ..Default::default()
        };
        tx.send(Request {
            epoch: 1,
            serial: 1,
            action: Action::Probe(profile.into(), None),
        })
        .map_err(|_| anyhow::anyhow!("worker stopped"))?;
        accepted_rx.recv_timeout(Duration::from_secs(2))?;
        let began = Instant::now();
        tx.send(Request {
            epoch: 1,
            serial: 2,
            action: Action::Local(std::env::current_dir()?),
        })
        .map_err(|_| anyhow::anyhow!("worker stopped"))?;
        let response = receive(&rx)?;
        let _ = release_tx.send(());
        server
            .join()
            .map_err(|_| anyhow::anyhow!("test server panicked"))?;
        assert!(matches!(response.update, Update::Local(_, _)));
        assert_eq!(response.serial, 2);
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "local browsing was blocked by network work"
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires loopback SSH fixture; never runs actual apt"]
    fn workspace_package_queries_remain_read_only_and_reject_stale_connections() -> Result<()> {
        let address: SocketAddr = std::env::var("ARCNIX_TEST_SSH_ADDR")?.parse()?;
        anyhow::ensure!(address.ip().is_loopback(), "fixture must be loopback");
        let profile = ServerProfile {
            id: "package-worker".into(),
            host: address.ip().to_string(),
            port: address.port(),
            username: "arcnix".into(),
            ..Default::default()
        };
        let (tx, rx) = start();
        let send = |serial, action| {
            tx.send(Request {
                epoch: 100,
                serial,
                action,
            })
            .map_err(|_| anyhow::anyhow!("worker stopped"))
        };
        send(1, Action::Probe(profile.into(), None))?;
        let fingerprint = match receive(&rx)?.update {
            Update::Fingerprint(f) => f,
            _ => anyhow::bail!("expected fingerprint"),
        };
        send(
            2,
            Action::Authenticate {
                login: crate::worker::Login::Manual {
                    secret: zeroize::Zeroizing::new("arcnix-test".into()),
                    remember: false,
                },
                fingerprint,
            },
        )?;
        assert!(matches!(receive(&rx)?.update, Update::Connected(true)));
        send(3, Action::PackageDetect)?;
        assert!(matches!(
            receive(&rx)?.update,
            Update::PackageEnvironment(Ok(_))
        ));
        send(4, Action::PackageSearch("nginx".into()))?;
        match receive(&rx)?.update {
            Update::PackageList(Ok(items)) => assert!(items.iter().any(|p| p.name == "nginx")),
            _ => anyhow::bail!("expected package search results"),
        }
        send(5, Action::PackageInstalled)?;
        assert!(matches!(receive(&rx)?.update, Update::PackageList(Ok(_))));
        tx.send(Request {
            epoch: 101,
            serial: 6,
            action: Action::Disconnect,
        })
        .map_err(|_| anyhow::anyhow!("worker stopped"))?;
        assert!(matches!(receive(&rx)?.update, Update::Disconnected));
        send(7, Action::PackageInstalled)?;
        assert!(matches!(receive(&rx)?.update, Update::PackageList(Err(_))));
        Ok(())
    }
}
