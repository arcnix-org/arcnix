//! pgAdmin as a managed local component: a verified, private Python runtime
//! whose desktop-mode backend reaches PostgreSQL through the SSH connection.
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component as PathPart, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::ServerProfile;
use crate::browser_proxy::{self, Diagnosis};
use crate::store::{ConfigStore, replace_document};
use crate::updates::{self, Asset, Source};

/// The only pgAdmin release whose desktop-mode contract this client supports.
pub const VERSION: &str = "9.19";

const LAUNCHER: &str = include_str!("pgadmin_launch.py");
const MANIFEST: &str = "component.json";
const MAX_FILES: usize = 60_000;
const MAX_UNPACKED: u64 = 1024 * 1024 * 1024;
const START_TIMEOUT: Duration = Duration::from_secs(120);
const LOG_TAIL: u64 = 2048;

/// Components are built from official pgAdmin releases in this repository's
/// `arcnix-build` branch and published as release `pgadmin-<version>-<build>`.
const PACKAGES: &str = "https://github.com/arcnix-org/pgadmin4/releases/download/pgadmin-9.19-2";

/// (OS, architecture, file, size, SHA-256) for each published platform.
const PLATFORMS: &[(&str, &str, &str, u64, &str)] = &[(
    "macos",
    "aarch64",
    "pgadmin-9.19-2-macos-arm64.zip",
    94_755_232,
    "cf725ea7a4e21b606cb65564f175adc03d897ec2109ae096104c98c268cf882f",
)];

/// The published package for this platform, if one has been built.
pub fn package() -> Option<Asset> {
    PLATFORMS
        .iter()
        .find(|(os, arch, ..)| *os == std::env::consts::OS && *arch == std::env::consts::ARCH)
        .map(|(_, _, file, size, digest)| Asset {
            url: format!("{PACKAGES}/{file}"),
            size: *size,
            digest: (*digest).to_owned(),
        })
}

/// A package on this computer, for testing before one is published.
pub fn local_package() -> Option<PathBuf> {
    std::env::var_os("ARCNIX_PGADMIN_PACKAGE")
        .filter(|_| cfg!(debug_assertions))
        .map(PathBuf::from)
}

/// Arcnix-managed storage, separate from any pgAdmin the user installed.
/// Portable configurations and test runs keep it beside their configuration.
pub fn base(store: &ConfigStore) -> Result<PathBuf> {
    if store.path() != ConfigStore::default_location()?.path() {
        return Ok(store
            .path()
            .parent()
            .context("Configuration directory is unavailable")?
            .join("components")
            .join("pgadmin"));
    }
    Ok(ProjectDirs::from("org", "arcnix", "Arcnix")
        .context("Cannot find the application data directory")?
        .data_local_dir()
        .join("components")
        .join("pgadmin"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    python: PathBuf,
    root: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: String,
    python: String,
    pgadmin: String,
}

fn relative(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    ensure!(
        !value.is_empty()
            && path
                .components()
                .all(|part| matches!(part, PathPart::Normal(_))),
        "Invalid path in the pgAdmin component"
    );
    Ok(path)
}

fn read_component(directory: &Path) -> Result<Component> {
    let mut manifest = Vec::new();
    fs::File::open(directory.join(MANIFEST))?
        .take(4096)
        .read_to_end(&mut manifest)?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest).context("Invalid pgAdmin component manifest")?;
    ensure!(
        manifest.version == VERSION,
        "This client supports pgAdmin {VERSION}, not {}",
        manifest.version
    );
    let component = Component {
        python: directory.join(relative(&manifest.python)?),
        root: directory.join(relative(&manifest.pgadmin)?),
    };
    ensure!(
        component.python.is_file()
            && component.root.join("pgAdmin4.py").is_file()
            && component.root.join("setup.py").is_file()
            // PyPI has published a release without its compiled interface.
            && component
                .root
                .join("pgadmin/static/js/generated/app.bundle.js")
                .is_file(),
        "The pgAdmin component is incomplete"
    );
    Ok(component)
}

pub fn installed(base: &Path) -> Option<Component> {
    read_component(&base.join(VERSION)).ok()
}

/// Download, verify, unpack and activate the component.
pub fn install(
    base: &Path,
    source: &Source,
    asset: &Asset,
    check: impl Fn() -> Result<()>,
    progress: impl FnMut(u64, u64),
) -> Result<Component> {
    fs::create_dir_all(base)?;
    let stage = Stage::new(base)?;
    let archive = updates::download(
        source,
        asset,
        &stage.0.join("component.zip"),
        &check,
        progress,
    )?;
    activate(base, &stage, archive, &check)
}

/// Install from a package already on this computer. Development builds only:
/// the caller vouches for the file because there is no published digest.
pub fn install_local(
    base: &Path,
    package: &Path,
    check: impl Fn() -> Result<()>,
) -> Result<Component> {
    fs::create_dir_all(base)?;
    let stage = Stage::new(base)?;
    activate(base, &stage, fs::File::open(package)?, &check)
}

struct Stage(PathBuf);

impl Stage {
    fn new(base: &Path) -> Result<Self> {
        let path = base.join(format!(".installing-{}", crate::new_id()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn activate(
    base: &Path,
    stage: &Stage,
    archive: fs::File,
    check: &impl Fn() -> Result<()>,
) -> Result<Component> {
    let unpacked = stage.0.join("component");
    unpack(archive, &unpacked, check)?;
    read_component(&unpacked)?;
    check()?;
    let target = base.join(VERSION);
    if target.exists() {
        // Keep a working copy; replace only one that cannot start.
        if let Some(component) = installed(base) {
            return Ok(component);
        }
        fs::rename(&target, stage.0.join("replaced"))?;
    }
    fs::rename(&unpacked, &target)?;
    let component = read_component(&target)?;
    // Not worth failing a working install over; the next upgrade retries.
    let _ = remove_superseded(base);
    Ok(component)
}

/// `major.minor`, as component folders are named.
fn release_number(name: &str) -> Option<(u32, u32)> {
    let (major, minor) = name.split_once('.')?;
    let number = |part: &str| {
        (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    };
    Some((number(major)?, number(minor)?))
}

/// True while a pgAdmin backend holds one of the data folders.
fn backend_running(base: &Path) -> Result<bool> {
    let Ok(servers) = fs::read_dir(base.join("servers")) else {
        return Ok(false);
    };
    for server in servers {
        let server = server?.path();
        if !server.is_dir() {
            continue;
        }
        for data in fs::read_dir(server)? {
            let lock = data?.path().join("arcnix.lock");
            let Ok(file) = OpenOptions::new().write(true).open(lock) else {
                continue;
            };
            match file.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => return Ok(true),
                Err(fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }
    Ok(false)
}

/// Delete component versions older than this one. Newer ones stay for the
/// Arcnix that installed them. Skipped while any pgAdmin backend runs, since
/// another Arcnix may be running an older version.
fn remove_superseded(base: &Path) -> Result<()> {
    let current = release_number(VERSION).context("Invalid pgAdmin version")?;
    if backend_running(base)? {
        return Ok(());
    }
    for entry in fs::read_dir(base)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if release_number(&name).is_some_and(|release| release < current) {
            // Moved aside first, so nothing sees a partly deleted version.
            let aside = base.join(format!(".removing-{}", crate::new_id()));
            fs::rename(entry.path(), &aside)?;
            fs::remove_dir_all(aside)?;
        } else if name.starts_with(".removing-") {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

fn unpack(archive: fs::File, target: &Path, check: &impl Fn() -> Result<()>) -> Result<()> {
    let mut zip = zip::ZipArchive::new(archive).context("Invalid pgAdmin component package")?;
    ensure!(
        zip.len() <= MAX_FILES,
        "The pgAdmin component has too many files"
    );
    fs::create_dir(target)?;
    let mut total = 0u64;
    for index in 0..zip.len() {
        check()?;
        let mut entry = zip.by_index(index)?;
        let path = target.join(relative(entry.name().trim_end_matches('/'))?);
        if entry.is_dir() {
            fs::create_dir_all(&path)?;
            continue;
        }
        ensure!(
            entry.is_file() && !entry.is_symlink(),
            "The pgAdmin component contains a link"
        );
        total = total.saturating_add(entry.size());
        ensure!(total <= MAX_UNPACKED, "The pgAdmin component is too large");
        fs::create_dir_all(path.parent().context("Invalid component path")?)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let executable = entry.unix_mode().is_some_and(|mode| mode & 0o100 != 0);
            options.mode(if executable { 0o755 } else { 0o644 });
        }
        let mut file = options.open(&path)?;
        let size = entry.size();
        let copied = std::io::copy(&mut entry.by_ref().take(size + 1), &mut file)?;
        ensure!(copied == size, "The pgAdmin component is damaged");
    }
    Ok(())
}

/// Where PostgreSQL listens, as seen from the SSH server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 5432,
            database: "postgres".into(),
            user: "postgres".into(),
        }
    }
}

impl Connection {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.host.is_empty()
                && self.host.len() <= 255
                && self
                    .host
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".-_:".contains(&byte)),
            "Enter a PostgreSQL host name or IP address"
        );
        ensure!(self.port != 0, "Enter a PostgreSQL port");
        // PostgreSQL limits identifiers to 63 bytes.
        ensure!(
            [&self.database, &self.user].into_iter().all(|name| {
                !name.is_empty() && name.len() <= 63 && !name.chars().any(char::is_control)
            }),
            "Enter a database and user name"
        );
        Ok(())
    }

    fn name(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn server_dir(base: &Path, profile: &ServerProfile, fingerprint: &str) -> PathBuf {
    base.join("servers")
        .join(crate::browser::profile_key(profile, fingerprint))
}

pub fn saved_connection(base: &Path, profile: &ServerProfile, fingerprint: &str) -> Connection {
    let path = server_dir(base, profile, fingerprint).join("connection.json");
    fs::File::open(path)
        .ok()
        .and_then(|file| serde_json::from_reader::<_, Connection>(file.take(8192)).ok())
        .filter(|connection| connection.validate().is_ok())
        .unwrap_or_default()
}

fn private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .context("Cannot create the pgAdmin data folder")
}

pub enum Event {
    /// The interface is ready. The address carries the window's private key.
    Ready(String),
    /// A database connection could not be opened through SSH.
    Unreachable(Diagnosis),
}

/// Run one pgAdmin backend and its tunnel until stopped or either ends.
#[allow(clippy::too_many_arguments)]
pub fn run(
    base: &Path,
    component: &Component,
    profile: &ServerProfile,
    fingerprint: &str,
    secret: Zeroizing<String>,
    connection: &Connection,
    stopped: impl Fn() -> bool + Sync,
    event: impl Fn(Event) + Sync,
) -> Result<()> {
    connection.validate()?;
    let directory = server_dir(base, profile, fingerprint);
    private_dir(&directory)?;
    let data = directory.join(&format!("{:x}", Sha256::digest(connection.name()))[..16]);
    private_dir(&data)?;
    let _lock = lock(&data, &stopped)?;
    if stopped() {
        return Ok(());
    }
    replace_document(
        &directory.join("connection.json"),
        &serde_json::to_vec(connection)?,
    )?;

    let listener = browser_proxy::listener()?;
    let tunnel_port = listener.local_addr()?.port();
    let ended = AtomicBool::new(false);
    let connected = AtomicBool::new(false);
    let failure = Mutex::new(None);
    let finished = || stopped() || ended.load(Ordering::Acquire);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let result = browser_proxy::run_fixed(
                &listener,
                profile,
                fingerprint,
                secret,
                (&connection.host, connection.port),
                finished,
                || connected.store(true, Ordering::Release),
                |diagnosis| event(Event::Unreachable(diagnosis)),
            );
            if let Err(error) = result
                && let Ok(mut failure) = failure.lock()
            {
                *failure = Some(error.context("The SSH tunnel to PostgreSQL closed"));
            }
            ended.store(true, Ordering::Release);
        });
        // The tunnel authenticates first, so a failed login starts nothing.
        while !connected.load(Ordering::Acquire) && !finished() {
            std::thread::sleep(Duration::from_millis(20));
        }
        let result = if finished() {
            Ok(())
        } else {
            backend(
                component,
                &data,
                connection,
                tunnel_port,
                &finished,
                |url| event(Event::Ready(url)),
            )
        };
        ended.store(true, Ordering::Release);
        result
    })?;
    match failure.into_inner() {
        Ok(Some(error)) => Err(error),
        _ => Ok(()),
    }
}

/// One backend per data folder: pgAdmin's own database is not shared.
fn lock(data: &Path, stopped: &impl Fn() -> bool) -> Result<fs::File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(data.join("arcnix.lock"))?;
    // A window that just closed needs a moment to release it.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) => {
                ensure!(
                    Instant::now() < deadline,
                    "pgAdmin is already open for this database in another Arcnix window"
                );
                if stopped() {
                    return Ok(file);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

struct Backend(Child);

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn backend(
    component: &Component,
    data: &Path,
    connection: &Connection,
    tunnel_port: u16,
    finished: &impl Fn() -> bool,
    ready: impl FnOnce(String),
) -> Result<()> {
    let mut random = [0u8; 32];
    getrandom::fill(&mut random)
        .map_err(|error| anyhow::anyhow!("Cannot create a private window key: {error}"))?;
    let key = Zeroizing::new(
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    );
    let launch = crate::new_id();
    let request = Zeroizing::new(serde_json::to_vec(&serde_json::json!({
        "root": component.root,
        "data": data,
        "launch": launch,
        "key": key.as_str(),
        "server": {
            "name": connection.name(),
            "port": tunnel_port,
            "database": connection.database,
            "user": connection.user,
        },
    }))?);

    let log_path = data.join("backend.log");
    let log = fs::File::create(&log_path)?;
    let mut command = Command::new(&component.python);
    command
        .args(["-I", "-B", "-c", LAUNCHER])
        .current_dir(data)
        .stdin(Stdio::piped())
        .stdout(log.try_clone()?)
        .stderr(log)
        .env_clear();
    // No inherited PG*, PYTHON* or PGADMIN_* settings.
    for name in [
        "HOME",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "LANG",
        "SystemRoot",
        "SYSTEMDRIVE",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "CommonProgramFiles",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("PYTHONDONTWRITEBYTECODE", "1")
        // A matching line in ~/.pgpass belongs to a different server.
        .env("PGPASSFILE", data.join("no-pgpass"));
    #[cfg(unix)]
    command.env("PATH", "/usr/bin:/bin");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = Backend(command.spawn().context("Cannot start pgAdmin")?);
    // Held open for the child's lifetime: closing it is the stop signal.
    let mut input = child.0.stdin.take().context("Cannot start pgAdmin")?;
    input.write_all(&request)?;
    input.write_all(b"\n")?;
    input.flush()?;

    let ready_path = data.join("backend.ready");
    let deadline = Instant::now() + START_TIMEOUT;
    let port = loop {
        if finished() {
            return Ok(());
        }
        if let Some(status) = child.0.try_wait()? {
            bail!(
                "pgAdmin stopped while starting ({status}). {}",
                tail(&log_path)
            );
        }
        if let Ok(record) = fs::read_to_string(&ready_path)
            && let Some((id, value)) = record.split_once(' ')
            && id == launch
        {
            break value
                .parse::<u16>()
                .context("Invalid pgAdmin backend port")?;
        }
        ensure!(
            Instant::now() < deadline,
            "pgAdmin did not start in time. {}",
            tail(&log_path)
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    ready(format!("http://127.0.0.1:{port}/?key={}", key.as_str()));
    while !finished() {
        if let Some(status) = child.0.try_wait()? {
            bail!("pgAdmin stopped ({status}). {}", tail(&log_path));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

fn tail(path: &Path) -> String {
    use std::io::{Seek, SeekFrom};
    let read = || -> Result<String> {
        let mut file = fs::File::open(path)?;
        let size = file.metadata()?.len();
        file.seek(SeekFrom::Start(size.saturating_sub(LOG_TAIL)))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
    };
    read().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn published_packages_match_this_version() {
        let prefix = format!("pgadmin-{}-", super::VERSION);
        let tag = super::PACKAGES.rsplit('/').next().unwrap();
        assert!(tag.starts_with(&prefix), "{tag}");
        for (_, _, file, size, digest) in super::PLATFORMS {
            assert!(file.starts_with(tag) && file.ends_with(".zip"), "{file}");
            assert!(*size > 0 && *size <= crate::updates::MAX_ASSET);
            assert!(crate::updates::valid_digest(digest), "{digest}");
        }
    }

    use super::*;

    struct Temporary(PathBuf);

    impl Temporary {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("arcnix-pgadmin-{}", crate::new_id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn manifest(version: &str) -> String {
        serde_json::json!({"version": version, "python": "bin/python3", "pgadmin": "pgadmin4"})
            .to_string()
    }

    fn package(directory: &Path, entries: &[(&str, &str)]) -> Result<PathBuf> {
        let path = directory.join("package.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path)?);
        for (name, content) in entries {
            let mode = if *name == "bin/python3" { 0o755 } else { 0o644 };
            zip.start_file(
                *name,
                zip::write::SimpleFileOptions::default().unix_permissions(mode),
            )?;
            zip.write_all(content.as_bytes())?;
        }
        zip.finish()?;
        Ok(path)
    }

    const FILES: [(&str, &str); 4] = [
        ("bin/python3", "#!/bin/sh\n"),
        ("pgadmin4/pgAdmin4.py", ""),
        ("pgadmin4/setup.py", ""),
        ("pgadmin4/pgadmin/static/js/generated/app.bundle.js", ""),
    ];

    fn install_entries(entries: &[(&str, &str)]) -> (Temporary, Result<Component>) {
        let directory = Temporary::new();
        let package = package(&directory.0, entries).unwrap();
        let result = install_local(&directory.0.join("base"), &package, || Ok(()));
        (directory, result)
    }

    #[test]
    fn installs_a_complete_component_and_removes_its_staging_folder() {
        let manifest = manifest(VERSION);
        let mut entries = vec![(MANIFEST, manifest.as_str())];
        entries.extend(FILES);
        let (directory, result) = install_entries(&entries);
        let component = result.unwrap();
        let base = directory.0.join("base");
        assert_eq!(installed(&base), Some(component.clone()));
        assert_eq!(fs::read_dir(&base).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode();
            assert_ne!(mode(&component.python) & 0o100, 0);
            assert_eq!(mode(&component.root.join("setup.py")) & 0o111, 0);
        }
    }

    #[test]
    fn installing_removes_older_versions_unless_pgadmin_is_running() {
        let directory = Temporary::new();
        let base = directory.0.join("base");
        let folders = [
            "0.1",
            "999.0",
            "servers/server/data",
            ".installing-other",
            ".removing-old",
            "9.x",
            "notes.1",
        ];
        for name in folders {
            fs::create_dir_all(base.join(name)).unwrap();
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(base.join("servers/server/data/arcnix.lock"))
            .unwrap();
        lock.lock().unwrap();
        remove_superseded(&base).unwrap();
        assert!(base.join("0.1").is_dir(), "removed while pgAdmin runs");
        drop(lock);

        let manifest = manifest(VERSION);
        let mut entries = vec![(MANIFEST, manifest.as_str())];
        entries.extend(FILES);
        let package = package(&directory.0, &entries).unwrap();
        install_local(&base, &package, || Ok(())).unwrap();
        let mut left: Vec<_> = fs::read_dir(&base)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        let mut expected = vec![
            VERSION,
            "999.0",
            "servers",
            ".installing-other",
            "9.x",
            "notes.1",
        ];
        expected.sort();
        assert_eq!(left, expected);
        assert_eq!(release_number(VERSION).map(|_| ()), Some(()));
        assert_eq!(release_number("10.2"), Some((10, 2)));
        for name in ["9", "9.", ".9", "9.x", "+9.1", "9.1.2", "notes.1"] {
            assert_eq!(release_number(name), None, "{name}");
        }
    }

    #[test]
    fn rejects_incomplete_unsupported_and_escaping_components() {
        let current = manifest(VERSION);
        let other = manifest("1.0");
        let escaping = serde_json::json!({
            "version": VERSION, "python": "../python3", "pgadmin": "pgadmin4"
        })
        .to_string();
        for (manifest, files, extra) in [
            (&current, &FILES[..3], None),
            (&other, &FILES[..], None),
            (&escaping, &FILES[..], None),
            (&current, &FILES[..], Some(("../escaped", ""))),
            (&current, &FILES[..], Some(("/absolute", ""))),
        ] {
            let mut entries = vec![(MANIFEST, manifest.as_str())];
            entries.extend(files);
            entries.extend(extra);
            let (directory, result) = install_entries(&entries);
            assert!(result.is_err());
            assert!(installed(&directory.0.join("base")).is_none());
            assert!(!directory.0.join("escaped").exists());
            assert_eq!(fs::read_dir(directory.0.join("base")).unwrap().count(), 0);
        }
    }

    #[test]
    fn validates_connections() {
        assert!(Connection::default().validate().is_ok());
        let invalid = [
            Connection {
                host: String::new(),
                ..Default::default()
            },
            Connection {
                host: "a b".into(),
                ..Default::default()
            },
            Connection {
                host: "/var/run/postgresql".into(),
                ..Default::default()
            },
            Connection {
                port: 0,
                ..Default::default()
            },
            Connection {
                user: String::new(),
                ..Default::default()
            },
            Connection {
                database: "a\nb".into(),
                ..Default::default()
            },
            Connection {
                database: "a".repeat(64),
                ..Default::default()
            },
        ];
        for connection in invalid {
            assert!(connection.validate().is_err(), "{connection:?}");
        }
        let ipv6 = Connection {
            host: "::1".into(),
            ..Default::default()
        };
        assert_eq!(ipv6.name(), "[::1]:5432");
    }

    #[test]
    fn only_one_backend_uses_a_data_folder() {
        let directory = Temporary::new();
        let held = lock(&directory.0, &|| false).unwrap();
        assert!(lock(&directory.0, &|| false).is_err());
        drop(held);
        assert!(lock(&directory.0, &|| false).is_ok());
    }
}
