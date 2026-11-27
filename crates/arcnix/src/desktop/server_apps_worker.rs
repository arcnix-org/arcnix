use arcnix_core::host_capabilities::{
    DiskInventoryReader, NetworkInventoryReader, ProcessInventoryReader,
};
use arcnix_core::remote::RemoteSession;
use arcnix_core::settings_capabilities::{
    DesktopPreferencesReader, DesktopPreferencesWriter, HostClockReader, SystemTimeZoneReader,
    SystemTimeZoneWriter,
};
use arcnix_core::software_capabilities::{
    ComposeProjectReader, ContainerController, ContainerInventoryReader, SoftwareCatalogReader,
};
use arcnix_core::{desktop_customization, desktop_time_zone, docker, server_apps, sync};

pub enum Action {
    Discover,
    Clock,
    DeviceInfo,
    SystemPower(arcnix_core::host::PowerAction),
    ReadTimeZone,
    SetTimeZone(String),
    FileLocations,
    FileItem(String),
    ReadImage(String),
    PickerEntries(Vec<String>),
    PickerTrash,
    CheckDesktopPaths(Vec<String>),
    DesktopItems,
    SoftwareInventory,
    Disks,
    NetworkSettings,
    Processes,
    SoftwareQuery {
        source: arcnix_core::software_install::Source,
        query: Option<String>,
    },
    ComposeList,
    ReadPreferences,
    #[cfg(windows)]
    ReadCookies(String),
    #[cfg(windows)]
    WriteCookies(String, sync::Revision, Vec<u8>),
    WritePreferences(Box<sync::Pending<server_apps::DesktopPreferences>>),
    DockerSnapshot,
    DockerImages,
    DockerStats(String),
    DockerInspect(String),
    DockerVolume(String),
    DockerLogs(String, u16),
    DockerOperate(String, docker::Operation),
    DockerDaemon,
    DockerMirrorTest(String),
    DockerImageConfig(String),
    DockerDiskUsage,
    DockerRemoveImage {
        target: String,
        id: String,
    },
    DockerPrune(docker::Prune),
    DockerLogins,
    /// The password goes to `docker login` on standard input. This enum is
    /// never printed, so it doesn't reach logs.
    DockerLogin {
        registry: String,
        username: String,
        password: zeroize::Zeroizing<String>,
    },
    DockerLogout(String),
    DockerNetworks,
    NginxRead,
    /// A command from `nginx::logs_command`.
    NginxLogs(String),
    /// A command from `nginx::certificates_command`.
    NginxCertificates(String),
    RedisRead(std::collections::BTreeMap<String, arcnix_core::redis::Auth>),
    RedisScan {
        endpoint: arcnix_core::redis::Endpoint,
        auth: Option<arcnix_core::redis::Auth>,
        db: u8,
        cursor: String,
        pattern: String,
    },
    RedisReadKey {
        endpoint: arcnix_core::redis::Endpoint,
        auth: Option<arcnix_core::redis::Auth>,
        db: u8,
        key: String,
    },
    RedisDiagnostics {
        endpoint: arcnix_core::redis::Endpoint,
        auth: Option<arcnix_core::redis::Auth>,
    },
    RedisWrite {
        mode: arcnix_core::redis::WriteMode,
        endpoint: arcnix_core::redis::Endpoint,
        auth: Option<arcnix_core::redis::Auth>,
        db: u8,
        op: arcnix_core::redis::WriteOp,
    },
}
pub enum Reply {
    Applications(server_apps::Applications),
    Clock(server_apps::ClockSample),
    DeviceInfo(arcnix_core::device_info::DeviceInfo),
    TimeZone(desktop_time_zone::TimeZones),
    FileLocations(Vec<arcnix_core::remote::FileLocation>),
    FileItem(arcnix_core::permissions::Metadata),
    Image(crate::image_viewer::Picture),
    PickerEntries(Vec<arcnix_core::FileEntry>),
    MissingDesktopPaths {
        paths: Vec<String>,
        unchecked: usize,
    },
    DesktopItems(desktop_customization::DesktopItems),
    SoftwareInventory(arcnix_core::software_catalog::Inventory),
    Disks(arcnix_core::disks::Snapshot),
    NetworkSettings(arcnix_core::host::HostOs, arcnix_core::network::Snapshot),
    Processes(Vec<arcnix_core::processes::Process>),
    SoftwarePackages(Vec<arcnix_core::packages::Package>),
    ComposeProjects(arcnix_core::compose_apps::Projects),
    Preferences(server_apps::DesktopPreferences),
    #[cfg(windows)]
    Cookies(sync::Snapshot),
    #[cfg(windows)]
    CookiesSaved(bool),
    SavedPreferences(sync::DesktopResult),
    Snapshot(docker::Snapshot),
    Images(Vec<docker::Image>),
    ContainerStats(docker::Stats),
    Container(docker::ContainerDetails),
    Volume(docker::VolumeDetails),
    Logs(String),
    Operated,
    DockerDaemon(arcnix_core::docker_daemon::Daemon),
    MirrorTest(arcnix_core::docker_daemon::MirrorTest),
    ImageConfig(arcnix_core::docker::ImageConfig),
    DiskUsage(Vec<docker::Usage>),
    Cleaned(String),
    Logins(Vec<docker::Login>),
    LoggedIn(String),
    Networks(Vec<docker::Network>),
    Nginx(arcnix_core::nginx::Reading),
    NginxLogs(String),
    NginxCertificates(arcnix_core::nginx::Certificates),
    Redis(arcnix_core::redis::Reading),
    RedisKeys(arcnix_core::redis::ScanPage),
    RedisKeyValue(arcnix_core::redis::KeyValue),
    RedisDiagnostics(arcnix_core::redis::Diagnostics),
    RedisWritten,
}
pub fn handle(session: &RemoteSession, action: Action) -> anyhow::Result<Reply> {
    Ok(match action {
        Action::SystemPower(action) => {
            session.system_power(action)?;
            Reply::Operated
        }
        Action::SoftwareInventory => Reply::SoftwareInventory(session.read_software_catalog()?),
        Action::SoftwareQuery { source, query } => {
            Reply::SoftwarePackages(session.query_software_source(source, query.as_deref())?)
        }
        Action::ComposeList => Reply::ComposeProjects(session.read_compose_projects()?),
        Action::Disks => Reply::Disks(session.read_disk_inventory()?),
        Action::NetworkSettings => {
            Reply::NetworkSettings(session.host_os()?, session.read_network_inventory()?)
        }
        Action::Processes => Reply::Processes(session.read_process_inventory()?),
        Action::Discover => Reply::Applications(server_apps::discover(session)?),
        Action::Clock => Reply::Clock(session.read_host_clock()?),
        Action::DeviceInfo => {
            Reply::DeviceInfo(arcnix_core::device_info::DeviceInfo::read_remote(session)?)
        }
        Action::ReadTimeZone => Reply::TimeZone(session.read_system_time_zone()?),
        Action::SetTimeZone(zone) => Reply::TimeZone(session.change_system_time_zone(&zone)?),
        Action::FileLocations => Reply::FileLocations(session.file_locations()?),
        Action::ReadImage(path) => {
            Reply::Image(desktop_customization::read_wallpaper(session, &path)?.into())
        }
        Action::FileItem(path) => Reply::FileItem(session.file_metadata(&path)?),
        Action::PickerEntries(paths) => Reply::PickerEntries(picker_entries(
            session,
            paths.into_iter().map(|path| {
                let name = arcnix_core::file_associations::filename(&path).to_owned();
                (name, path)
            }),
        )?),
        Action::PickerTrash => Reply::PickerEntries(picker_entries(
            session,
            session
                .recycle(&arcnix_core::recycle::Operation::List, || Ok(()))?
                .into_iter()
                .filter_map(|item| {
                    let name = arcnix_core::file_associations::filename(&item.original).to_owned();
                    Some((name, item.payload?.path))
                }),
        )?),
        Action::CheckDesktopPaths(paths) => {
            let (paths, unchecked) = session.find_missing_desktop_paths(paths)?;
            Reply::MissingDesktopPaths { paths, unchecked }
        }
        Action::DesktopItems => Reply::DesktopItems(desktop_customization::items(session)?),
        #[cfg(windows)]
        Action::ReadCookies(client) => {
            Reply::Cookies(sync::Document::Cookies(&client).read(session)?)
        }
        #[cfg(windows)]
        Action::WriteCookies(client, base, data) => {
            Reply::CookiesSaved(sync::Document::Cookies(&client).commit(session, &base, &data)?)
        }
        Action::ReadPreferences => Reply::Preferences(session.read_desktop_preferences()?),
        Action::WritePreferences(pending) => {
            Reply::SavedPreferences(session.save_desktop_preferences(&pending)?)
        }
        Action::DockerSnapshot => Reply::Snapshot(session.read_containers()?),
        Action::DockerImages => Reply::Images(session.read_container_images()?),
        Action::DockerStats(id) => Reply::ContainerStats(session.read_container_stats(&id)?),
        Action::DockerInspect(id) => Reply::Container(session.inspect_container(&id)?),
        Action::DockerVolume(name) => Reply::Volume(session.inspect_container_volume(&name)?),
        Action::DockerLogs(id, lines) => Reply::Logs(session.read_container_logs(&id, lines)?),
        Action::DockerDaemon => Reply::DockerDaemon(arcnix_core::docker_daemon::read(session)?),
        Action::DockerMirrorTest(mirror) => {
            Reply::MirrorTest(arcnix_core::docker_daemon::test_mirror(session, &mirror)?)
        }
        Action::DockerImageConfig(image) => {
            Reply::ImageConfig(arcnix_core::docker::image_config(session, &image)?)
        }
        Action::DockerDiskUsage => Reply::DiskUsage(docker::disk_usage(session)?),
        Action::DockerRemoveImage { target, id } => {
            Reply::Cleaned(docker::remove_image(session, &target, &id)?)
        }
        Action::DockerPrune(prune) => Reply::Cleaned(docker::prune(session, prune)?),
        Action::DockerLogins => Reply::Logins(docker::logins(session)?),
        Action::DockerLogin {
            registry,
            username,
            password,
        } => Reply::LoggedIn(docker::login(session, &registry, &username, &password)?),
        Action::DockerLogout(registry) => Reply::LoggedIn(docker::logout(session, &registry)?),
        Action::DockerNetworks => Reply::Networks(docker::networks(session)?),
        Action::NginxRead => Reply::Nginx(arcnix_core::nginx::read(session)?),
        Action::NginxLogs(command) => {
            Reply::NginxLogs(arcnix_core::nginx::read_logs(session, &command)?)
        }
        Action::NginxCertificates(command) => {
            Reply::NginxCertificates(arcnix_core::nginx::read_certificates(session, &command)?)
        }
        Action::RedisRead(auth) => Reply::Redis(arcnix_core::redis::read(session, &auth)?),
        Action::RedisScan {
            endpoint,
            auth,
            db,
            cursor,
            pattern,
        } => Reply::RedisKeys(arcnix_core::redis::scan_keys(
            session,
            &endpoint,
            auth.as_ref(),
            db,
            &cursor,
            &pattern,
        )?),
        Action::RedisReadKey {
            endpoint,
            auth,
            db,
            key,
        } => Reply::RedisKeyValue(arcnix_core::redis::read_key(
            session,
            &endpoint,
            auth.as_ref(),
            db,
            &key,
        )?),
        Action::RedisDiagnostics { endpoint, auth } => Reply::RedisDiagnostics(
            arcnix_core::redis::read_diagnostics(session, &endpoint, auth.as_ref())?,
        ),
        Action::RedisWrite {
            mode,
            endpoint,
            auth,
            db,
            op,
        } => {
            arcnix_core::redis::write_key(session, &endpoint, auth.as_ref(), db, mode, &op)?;
            Reply::RedisWritten
        }
        Action::DockerOperate(id, operation) => {
            session.apply_container_operation(&id, &operation)?;
            Reply::Operated
        }
    })
}

fn picker_entries(
    session: &RemoteSession,
    paths: impl IntoIterator<Item = (String, String)>,
) -> anyhow::Result<Vec<arcnix_core::FileEntry>> {
    let mut entries = Vec::new();
    for (name, path) in paths {
        match session.file_metadata(&path) {
            Ok(meta) if !meta.symlink => entries.push(arcnix_core::FileEntry {
                hidden_attribute: meta.hidden_attribute,
                name,
                path,
                is_dir: meta.directory,
                is_symlink: false,
                size: meta.size,
                modified: meta.modified,
                permissions: meta.mode,
            }),
            Err(error) if arcnix_core::transport_error::is_connection_error(&error) => {
                return Err(error);
            }
            _ => {}
        }
    }
    Ok(entries)
}
