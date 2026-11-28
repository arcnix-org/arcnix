//! Versioned, atomic configuration storage. Credentials are deliberately absent
//! from the persisted model; callers keep passwords/passphrases in session state.
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::{AppConfig, AuthMethod, ServerGroup, ServerProfile, TrustedHost, new_id};

const SCHEMA_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    config: AppConfig,
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

pub struct StartupConfig {
    pub config: AppConfig,
    pub diagnostic: Option<String>,
    pub writable: bool,
}

impl ConfigStore {
    pub fn default_location() -> Result<Self> {
        Ok(Self::at(super::paths::config_dir()?.join("config.json")))
    }

    /// Called only for normal startup; explicit --config paths stay portable.
    pub fn migrate_default_location() -> Result<Self> {
        let store = Self::default_location()?;
        super::paths::migrate_config(
            store
                .path
                .parent()
                .context("Missing configuration directory")?,
        )?;
        Ok(store)
    }

    /// Local application data follows the configuration for portable installs.
    pub fn local_data_root(&self) -> Result<PathBuf> {
        if self.path != Self::default_location()?.path {
            return Ok(self
                .path
                .parent()
                .context("Configuration directory is unavailable")?
                .to_owned());
        }
        super::paths::local_data_dir()
    }

    /// A real SSH session writes its Linux user data under ~/.arcnix. Keep
    /// this host's simulated server state in its own namespace. Portable
    /// configurations and test runs keep their demo data beside the config.
    pub fn demo_preferences(&self, scope: &str) -> Result<PathBuf> {
        anyhow::ensure!(
            matches!(scope, "local-windows" | "local-linux" | "local-macos")
                || scope
                    .strip_prefix("local-wsl-")
                    .is_some_and(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())),
            "Invalid demo identity"
        );
        let parent = if self.path == Self::default_location()?.path {
            std::env::home_dir()
                .context("Home directory is unavailable")?
                .join(".arcnix")
        } else {
            self.path
                .parent()
                .context("Configuration directory is unavailable")?
                .to_owned()
        };
        let path = parent
            .join("self-demo-server")
            .join(scope)
            .join("desktop.json");
        let legacy = self
            .path
            .with_file_name("local-desktops")
            .join(scope)
            .join("desktop.json");
        if legacy.is_file() && !path.try_exists()? {
            let _lock = crate::sync::lock_local_desktop(&path)?;
            if !path.try_exists()? {
                let prefs = crate::sync::read_local_desktop(&legacy)?;
                replace_document(&path, &serde_json::to_vec(&prefs)?)?;
            }
        }
        Ok(path)
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Only a missing file is treated as first launch. Invalid or unreadable
    /// configuration remains untouched and is reported to the caller.
    pub fn load(&self) -> Result<AppConfig> {
        Ok(self
            .read_document()?
            .as_deref()
            .map(decoded)
            .transpose()?
            .unwrap_or_default())
    }

    fn read_document(&self) -> Result<Option<Vec<u8>>> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(err) => return Err(err).context("读取配置文件失败"),
        };
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("读取配置文件失败")?;
        ensure!(bytes.len() as u64 <= MAX_CONFIG_BYTES, "配置文件过大");
        Ok(Some(bytes))
    }

    pub fn load_for_startup(&self) -> StartupConfig {
        let mut debug_mode = false;
        let result = self.load().map(|config| (config, None)).or_else(|error| {
            if error.downcast_ref::<std::io::Error>().is_some() {
                return Err(error);
            }
            // Re-read under the same lock used by save before repairing anything.
            let _lock = self.write_lock()?;
            let Some(bytes) = self.read_document()? else {
                return Ok((AppConfig::default(), None));
            };
            let original_error = match decoded(&bytes) {
                Ok(config) => return Ok((config, None)),
                Err(error) => error,
            };
            let Some((fixed, removed)) = repair_file_preferences(&bytes, &mut debug_mode) else {
                return Err(original_error);
            };
            let config = decoded(&fixed)?;
            let backup = self.path.canonicalize()?
                .with_added_extension(format!("before-repair-{}.bak", new_id()));
            crate::configuration_backups::publish(&backup, &bytes)?;
            replace_document(&self.path, &fixed)?;
            let diagnostic = format!(
                "Removed {removed} unsupported file association entries. Original configuration: {}\n{}",
                backup.display(), original_error.root_cause()
            );
            Ok((config, Some(diagnostic)))
        });
        match result {
            Ok((config, diagnostic)) => StartupConfig {
                config,
                diagnostic,
                writable: true,
            },
            Err(error) => {
                let mut config = AppConfig::default();
                config.settings.debug_mode = debug_mode;
                StartupConfig {
                    config,
                    diagnostic: Some(format!("{error:#}")),
                    writable: false,
                }
            }
        }
    }

    fn write_lock(&self) -> Result<fs::File> {
        let path = self.path.with_added_extension("lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        lock.lock()?;
        Ok(lock)
    }

    /// Writes a unique file beside the destination, flushes it, then renames it
    /// over the destination. A failed write never truncates the old config.
    pub fn save(&self, config: &AppConfig) -> Result<()> {
        let bytes = encoded(config)?;
        let _lock = self.write_lock()?;
        replace_document(&self.path, &bytes)
    }

    /// Check the same schema and serialized limit used by save, without I/O.
    pub fn check(config: &AppConfig) -> Result<()> {
        encoded(config).map(|_| ())
    }
}

fn repair_file_preferences(bytes: &[u8], debug_mode: &mut bool) -> Option<(Vec<u8>, usize)> {
    // Replace only the file association object. Re-encoding the entire JSON value
    // could silently collapse duplicate fields in server profiles or trust records.
    type RawObject<'a> = BTreeMap<String, &'a serde_json::value::RawValue>;
    let document: RawObject = serde_json::from_slice(bytes).ok()?;
    let config: RawObject = serde_json::from_str(document.get("config")?.get()).ok()?;
    let settings: RawObject = serde_json::from_str(config.get("settings")?.get()).ok()?;
    *debug_mode = settings
        .get("debug_mode")
        .is_some_and(|v| v.get() == "true");
    let original_rules = settings.get("file_associations")?.get();
    let mut rules = serde_json::from_str(original_rules).ok()?;
    let removed = crate::file_associations::repair_handlers(&mut rules);
    if removed == 0 {
        return None;
    }
    let start = original_rules.as_ptr() as usize - bytes.as_ptr() as usize;
    let mut fixed = bytes[..start].to_vec();
    serde_json::to_writer(&mut fixed, &rules).ok()?;
    fixed.extend_from_slice(&bytes[start + original_rules.len()..]);
    Some((fixed, removed))
}

pub(crate) fn replace_document(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).context("创建配置目录失败")?;
    let temporary = parent.join(format!(".arcnix-config-{}.tmp", new_id()));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).context("创建临时配置文件失败")?;
        file.write_all(bytes).context("写入配置失败")?;
        file.write_all(b"\n")?;
        file.sync_all().context("刷新配置文件失败")?;
        drop(file);
        fs::rename(&temporary, path).context("替换配置文件失败，原配置已保留")?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn decoded(bytes: &[u8]) -> Result<AppConfig> {
    ensure!(bytes.len() as u64 <= MAX_CONFIG_BYTES, "配置文件过大");
    let document: Document =
        serde_json::from_slice(bytes).context("配置文件格式错误，原文件已保留")?;
    ensure!(
        document.version == SCHEMA_VERSION,
        "不支持的配置版本 {}，需要版本 {}；原文件已保留",
        document.version,
        SCHEMA_VERSION
    );
    document.config.validate().context("配置内容无效")?;
    Ok(document.config)
}
pub(crate) fn encoded(config: &AppConfig) -> Result<Vec<u8>> {
    config.validate()?;
    let document = Document {
        version: SCHEMA_VERSION,
        config: config.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&document)?;
    ensure!((bytes.len() as u64) < MAX_CONFIG_BYTES, "配置文件过大");
    Ok(bytes)
}

fn nonempty(value: &str, label: &str, limit: usize) -> Result<()> {
    ensure!(!value.trim().is_empty(), "{label}不能为空");
    ensure!(value.len() <= limit, "{label}过长");
    ensure!(!value.chars().any(char::is_control), "{label}包含控制字符");
    Ok(())
}

fn validate_host(host: &str) -> Result<()> {
    nonempty(host, "主机地址", 1024)?;
    ensure!(
        !host
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '@')),
        "主机地址应只包含主机名或 IP 地址"
    );
    if host.contains(':') || host.contains('[') || host.contains(']') {
        let address = if host.starts_with('[') {
            host.strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .context("IPv6 地址括号格式无效")?
        } else {
            host
        };
        address
            .parse::<std::net::Ipv6Addr>()
            .context("IPv6 地址格式无效；端口请填入单独的端口字段")?;
    }
    Ok(())
}

pub(crate) fn host_key(host: &str) -> String {
    let address = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    match address.parse::<std::net::IpAddr>() {
        Ok(address) => address.to_string(),
        Err(_) => host.to_ascii_lowercase(),
    }
}

fn validate_fingerprint(fingerprint: &str) -> Result<()> {
    let encoded = fingerprint
        .strip_prefix("SHA256:")
        .context("主机指纹须为 SHA256 格式")?;
    let decoded = STANDARD_NO_PAD
        .decode(encoded)
        .context("主机指纹编码无效")?;
    ensure!(decoded.len() == 32, "主机指纹长度无效");
    Ok(())
}

impl ServerProfile {
    pub fn validate(&self) -> Result<()> {
        crate::server_labels::validate(&self.environment, &self.tags)?;
        nonempty(&self.id, "服务器标识", 256)?;
        nonempty(&self.name, "服务器名称", 256)?;
        validate_host(&self.host)?;
        #[cfg(windows)]
        if let Some(name) = &self.wsl_distribution {
            ensure!(
                self.host == crate::wsl::profile_host(name)?,
                "Invalid WSL profile address"
            );
        }
        ensure!(self.port != 0, "SSH 端口必须介于 1 和 65535 之间");
        if let Some(connection) = self.connection {
            connection.validate()?;
        }
        for path in [&self.locations.local_directory, &self.locations.last_local]
            .into_iter()
            .flatten()
        {
            ensure!(
                !self.available_on_this_platform() || path.is_absolute(),
                "Local server directories must be absolute"
            );
        }
        if let Some(path) = &self.locations.last_remote {
            nonempty(path, "Last remote directory", 32768)?;
        }
        ensure!(
            self.file_pane_ratio
                .is_none_or(|ratio| (200..=800).contains(&ratio)),
            "文件分栏比例应介于 20% 和 80% 之间"
        );
        nonempty(&self.username, "用户名", 256)?;
        ensure!(
            !self.username.chars().any(char::is_whitespace),
            "用户名不能包含空白字符"
        );
        nonempty(&self.initial_remote_path, "初始远程目录", 32_768)?;
        if self.auth == AuthMethod::PrivateKey {
            ensure!(
                self.private_key
                    .as_ref()
                    .is_some_and(|p| !p.as_os_str().is_empty()),
                "请选择私钥文件"
            );
        }
        Ok(())
    }
}

impl AppConfig {
    pub fn validate(&self) -> Result<()> {
        self.settings.connection.validate()?;
        crate::file_associations::validate(&self.settings.file_associations)?;
        ensure!(
            self.backup_evidence.len() <= 512,
            "Too many backup confirmation records"
        );
        ensure!(
            self.backup_policies.len() <= 100,
            "At most 100 backup policies are supported"
        );
        let mut policy_ids = HashSet::new();
        for policy in &self.backup_policies {
            policy.validate()?;
            ensure!(
                policy_ids.insert(&policy.id),
                "Duplicate backup policy identifier"
            );
        }
        self.settings.logging.validate()?;
        ensure!(
            self.settings.display.decimals <= crate::preferences::DisplaySettings::MAX_DECIMALS,
            "Use 0–2 decimal places"
        );
        ensure!(
            [3, 6, 12, 24, 168, 720].contains(&self.settings.updates.interval_hours),
            "Invalid update check interval"
        );
        ensure!(
            crate::AppSettings::RESOURCE_INTERVALS
                .contains(&self.settings.resource_interval_seconds),
            "资源采样间隔应为 3、5、10 或 30 秒"
        );
        ensure!(
            (1..=16).contains(&self.settings.transfer_concurrency),
            "Concurrent transfers must be between 1 and 16"
        );
        ensure!(
            self.snippets.len() <= 200,
            "Too many saved commands (200 maximum)"
        );
        let mut snippet_ids = HashSet::new();
        for snippet in &self.snippets {
            snippet.validate()?;
            ensure!(
                snippet_ids.insert(&snippet.id),
                "Duplicate command identifier"
            );
        }
        crate::script_parameters::validate_presets(self)?;
        crate::script_favorites::validate(&self.script_favorites)?;
        let mut group_ids = HashSet::new();
        let mut group_names = HashSet::new();
        for group in &self.groups {
            nonempty(&group.id, "分组标识", 256)?;
            nonempty(&group.name, "分组名称", 256)?;
            ensure!(group_ids.insert(group.id.as_str()), "分组标识重复");
            ensure!(
                group_names.insert(group.name.trim().to_lowercase()),
                "分组名称重复"
            );
        }
        let mut server_ids = HashSet::new();
        for server in &self.servers {
            server.validate()?;
            ensure!(server_ids.insert(server.id.as_str()), "服务器标识重复");
            if let Some(group) = &server.group_id {
                ensure!(
                    group_ids.contains(group.as_str()),
                    "服务器引用了不存在的分组"
                );
            }
        }
        let mut hosts = HashSet::new();
        let mut rules = HashSet::new();
        for rule in &self.transfer_rules {
            ensure!(
                server_ids.contains(rule.server_id.as_str()),
                "文件冲突规则引用了不存在的服务器"
            );
            ensure!(
                rules.insert((&rule.server_id, rule.direction)),
                "文件冲突规则重复"
            );
        }
        for host in &self.trusted_hosts {
            validate_host(&host.host)?;
            ensure!(host.port != 0, "信任主机端口无效");
            validate_fingerprint(&host.fingerprint)?;
            ensure!(
                hosts.insert((host_key(&host.host), host.port)),
                "信任主机记录重复"
            );
        }
        Ok(())
    }

    pub fn upsert_server(&mut self, mut server: ServerProfile) -> Result<()> {
        server.validate()?;
        if let Some(group) = &server.group_id {
            ensure!(self.groups.iter().any(|g| &g.id == group), "所选分组不存在");
        }
        if let Some(existing) = self.servers.iter_mut().find(|s| s.id == server.id) {
            if existing.host != server.host
                || existing.port != server.port
                || existing.username != server.username
            {
                self.transfer_rules
                    .retain(|rule| rule.server_id != server.id);
                server.locations.last_local = None;
                server.locations.last_remote = None;
            }
            *existing = server;
        } else {
            self.servers.push(server);
        }
        Ok(())
    }

    pub fn delete_server(&mut self, id: &str) -> bool {
        let before = self.servers.len();
        self.servers.retain(|s| s.id != id);
        self.transfer_rules.retain(|rule| rule.server_id != id);
        before != self.servers.len()
    }

    pub fn set_transfer_rule(
        &mut self,
        server_id: &str,
        direction: crate::TransferDirection,
        action: Option<crate::ExistingFileAction>,
    ) -> Result<()> {
        ensure!(
            self.servers.iter().any(|server| server.id == server_id),
            "服务器不存在"
        );
        self.transfer_rules
            .retain(|rule| rule.server_id != server_id || rule.direction != direction);
        if let Some(action) = action {
            self.transfer_rules.push(crate::TransferRule {
                server_id: server_id.to_owned(),
                direction,
                action,
            });
        }
        Ok(())
    }

    pub fn set_file_pane_ratio(&mut self, server_id: &str, ratio: u16) -> Result<()> {
        ensure!(
            (200..=800).contains(&ratio),
            "文件分栏比例应介于 20% 和 80% 之间"
        );
        let server = self
            .servers
            .iter_mut()
            .find(|server| server.id == server_id)
            .context("服务器不存在")?;
        server.file_pane_ratio = Some(ratio);
        Ok(())
    }

    pub fn add_group(&mut self, name: impl Into<String>) -> Result<String> {
        let name = name.into().trim().to_owned();
        nonempty(&name, "分组名称", 256)?;
        ensure!(
            !self
                .groups
                .iter()
                .any(|g| g.name.trim().to_lowercase() == name.to_lowercase()),
            "分组名称已存在"
        );
        let id = new_id();
        self.groups.push(ServerGroup {
            id: id.clone(),
            name,
        });
        Ok(id)
    }

    pub fn rename_group(&mut self, id: &str, name: impl Into<String>) -> Result<()> {
        let name = name.into().trim().to_owned();
        nonempty(&name, "分组名称", 256)?;
        ensure!(
            !self
                .groups
                .iter()
                .any(|g| g.id != id && g.name.trim().to_lowercase() == name.to_lowercase()),
            "分组名称已存在"
        );
        let group = self
            .groups
            .iter_mut()
            .find(|g| g.id == id)
            .context("分组不存在")?;
        group.name = name;
        Ok(())
    }

    pub fn delete_group(&mut self, id: &str) -> bool {
        let before = self.groups.len();
        self.groups.retain(|g| g.id != id);
        if self.groups.len() == before {
            return false;
        }
        for server in &mut self.servers {
            if server.group_id.as_deref() == Some(id) {
                server.group_id = None;
            }
        }
        true
    }

    /// Move a folder relative to another folder, preserving membership and IDs.
    pub fn move_group(&mut self, id: &str, target: &str, after: bool) -> Result<bool> {
        let source = self
            .groups
            .iter()
            .position(|g| g.id == id)
            .context("分组不存在")?;
        ensure!(self.groups.iter().any(|g| g.id == target), "目标分组不存在");
        if id == target {
            return Ok(false);
        }
        let group = self.groups.remove(source);
        let destination = self
            .groups
            .iter()
            .position(|g| g.id == target)
            .expect("validated target")
            + usize::from(after);
        self.groups.insert(destination, group);
        Ok(source != destination)
    }

    /// Delete local profiles in a group and their profile-specific transfer rules.
    pub fn delete_group_and_servers(&mut self, id: &str) -> bool {
        if !self.groups.iter().any(|g| g.id == id) {
            return false;
        }
        let removed: HashSet<_> = self
            .available_servers()
            .filter(|s| s.group_id.as_deref() == Some(id))
            .map(|s| s.id.clone())
            .collect();
        self.servers.retain(|s| !removed.contains(&s.id));
        self.transfer_rules
            .retain(|rule| !removed.contains(&rule.server_id));
        self.delete_group(id)
    }

    pub fn trusted_fingerprint(&self, host: &str, port: u16) -> Option<&str> {
        self.trusted_hosts
            .iter()
            .find(|h| host_key(&h.host) == host_key(host) && h.port == port)
            .map(|h| h.fingerprint.as_str())
    }

    /// Call only after explicit user confirmation, particularly on key changes.
    pub fn trust_host(&mut self, host: &str, port: u16, fingerprint: &str) -> Result<()> {
        validate_host(host)?;
        ensure!(port != 0, "SSH 端口不能为 0");
        validate_fingerprint(fingerprint)?;
        let host = host_key(host);
        if let Some(record) = self
            .trusted_hosts
            .iter_mut()
            .find(|h| host_key(&h.host) == host && h.port == port)
        {
            record.fingerprint = fingerprint.to_owned();
        } else {
            self.trusted_hosts.push(TrustedHost {
                host,
                port,
                fingerprint: fingerprint.to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("arcnix-store-test-{}", new_id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn store(&self) -> ConfigStore {
            ConfigStore::at(self.0.join("nested").join("config.json"))
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn demo_state_is_isolated_and_imported_only_once() {
        let dir = TestDir::new();
        let store = dir.store();
        let parent = store.path().parent().unwrap();
        let path = store.demo_preferences("local-linux").unwrap();
        assert_eq!(
            path,
            parent.join("self-demo-server/local-linux/desktop.json")
        );
        assert!(!path.exists());
        assert!(store.demo_preferences("../desktop").is_err());
        let legacy = parent.join("local-desktops/local-linux/desktop.json");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(
            &legacy,
            serde_json::to_vec(&crate::server_apps::DesktopPreferences::default()).unwrap(),
        )
        .unwrap();
        assert_eq!(store.demo_preferences("local-linux").unwrap(), path);
        assert!(crate::sync::read_local_desktop(&path).is_ok());
        assert!(path.is_file());
        fs::write(legacy, b"invalid legacy config").unwrap();
        assert_eq!(store.demo_preferences("local-linux").unwrap(), path);
        assert!(!parent.join("desktop").exists());
    }

    fn server() -> ServerProfile {
        ServerProfile {
            name: "测试服务器".into(),
            host: "example.org".into(),
            username: "chris".into(),
            ..Default::default()
        }
    }
    #[test]
    fn foreign_local_profiles_round_trip_without_becoming_ssh_targets() {
        let ssh = server();
        let group = ServerGroup {
            id: "local".into(),
            name: "Local".into(),
        };
        let mut local = ServerProfile {
            host: "Ubuntu.wsl.localhost".into(),
            wsl_distribution: Some("Ubuntu".into()),
            group_id: Some(group.id.clone()),
            ..server()
        };
        local.locations.last_local = Some(PathBuf::from(r"C:\Users\chris"));
        for use_ssh in [false, true] {
            local.wsl_use_ssh = use_ssh;
            let config = AppConfig {
                groups: vec![group.clone()],
                servers: vec![ssh.clone(), local.clone()],
                ..Default::default()
            };
            let mut loaded = decoded(&encoded(&config).unwrap()).unwrap();
            assert_eq!(loaded, config);
            assert_eq!(
                loaded.available_servers().count(),
                if cfg!(windows) { 2 } else { 1 }
            );
            #[cfg(not(windows))]
            {
                assert!(loaded.available_servers().all(|p| p.id == ssh.id));
                let error = crate::remote::PendingConnection::open(&local)
                    .err()
                    .unwrap();
                assert_eq!(error.to_string(), "Local WSL connections require Windows");
            }
            assert!(loaded.delete_group_and_servers(&group.id));
            assert!(loaded.servers.iter().any(|p| p.id == ssh.id));
            assert_eq!(
                loaded.servers.iter().any(|p| p.id == local.id),
                !cfg!(windows)
            );
            loaded.validate().unwrap();
        }
    }

    fn fingerprint() -> String {
        format!("SHA256:{}", STANDARD_NO_PAD.encode([42_u8; 32]))
    }

    #[test]
    fn application_preferences_default_to_english_and_survive_reload() {
        use crate::{Language, Theme};
        let mut config: AppConfig = serde_json::from_str(
            r#"{"groups":[],"servers":[],"trusted_hosts":[],"local_directory":null}"#,
        )
        .unwrap();
        assert_eq!(config.settings.language, Language::English);
        assert_eq!(config.settings.theme, Theme::System);
        config.settings.language = Language::SimplifiedChinese;
        config.settings.theme = Theme::Light;
        config.settings.show_hidden_files = false;
        config.settings.resource_interval_seconds = 10;
        let dir = TestDir::new();
        let store = dir.store();
        store.save(&config).unwrap();
        assert_eq!(store.load().unwrap(), config);
        let original = fs::read(store.path()).unwrap();
        config.settings.resource_interval_seconds = 0;
        assert!(store.save(&config).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), original);
    }

    #[test]
    fn theme_modes_resolve_without_overwriting_saved_preferences() {
        use crate::{AppSettings, Theme};
        assert_eq!(Theme::default(), Theme::System);
        let missing_theme: AppSettings = serde_json::from_str(
            r#"{"language":"English","show_hidden_files":true,"resource_interval_seconds":3}"#,
        )
        .unwrap();
        assert_eq!(missing_theme.theme, Theme::System);
        let dir = TestDir::new();
        let store = dir.store();
        for mode in [Theme::System, Theme::Dark, Theme::Light] {
            let mut config = AppConfig::default();
            config.settings.theme = mode;
            store.save(&config).unwrap();
            let loaded = store.load().unwrap();
            assert_eq!(loaded.settings.theme, mode);
            assert_eq!(mode.is_dark(false), mode == Theme::Dark);
            assert_eq!(mode.is_dark(true), mode != Theme::Light);
            assert_eq!(store.load().unwrap(), loaded);
        }
    }

    #[test]
    fn transfer_concurrency_defaults_for_old_configs_and_rejects_invalid_saved_limits() {
        let old: AppConfig = serde_json::from_str(
            r#"{"groups":[],"servers":[],"trusted_hosts":[],"local_directory":null,"settings":{"language":"English","show_hidden_files":true,"resource_interval_seconds":3}}"#,
        )
        .unwrap();
        assert_eq!(old.settings.transfer_concurrency, 5);
        assert_eq!(AppConfig::default().settings.transfer_concurrency, 5);
        let dir = TestDir::new();
        let store = dir.store();
        let mut config = old;
        for limit in [1, 5, 16] {
            config.settings.transfer_concurrency = limit;
            store.save(&config).unwrap();
            assert_eq!(store.load().unwrap().settings.transfer_concurrency, limit);
        }
        let original = fs::read(store.path()).unwrap();
        for invalid in [0, 17, u8::MAX] {
            config.settings.transfer_concurrency = invalid;
            assert!(store.save(&config).is_err());
            assert_eq!(fs::read(store.path()).unwrap(), original);
        }
    }

    #[test]
    fn per_server_layout_and_conflict_rules_roundtrip_and_keep_identity_boundaries() {
        use crate::{ExistingFileAction, TransferDirection};
        let dir = TestDir::new();
        let store = dir.store();
        let mut config = AppConfig::default();
        let first = server();
        let mut second = server();
        second.name = "另一台".into();
        second.host = "another.example.org".into();
        config.upsert_server(first.clone()).unwrap();
        config.upsert_server(second.clone()).unwrap();
        config.set_file_pane_ratio(&first.id, 650).unwrap();
        config.set_file_pane_ratio(&second.id, 350).unwrap();
        assert!(config.set_file_pane_ratio(&first.id, 199).is_err());
        config
            .set_transfer_rule(
                &first.id,
                TransferDirection::Upload,
                Some(ExistingFileAction::Overwrite),
            )
            .unwrap();
        config
            .set_transfer_rule(
                &first.id,
                TransferDirection::Download,
                Some(ExistingFileAction::Skip),
            )
            .unwrap();
        store.save(&config).unwrap();
        let restored = store.load().unwrap();
        assert_eq!(restored, config);
        assert_eq!(restored.servers[0].file_pane_ratio, Some(650));
        assert_eq!(restored.servers[1].file_pane_ratio, Some(350));
        let mut renamed = config.servers[0].clone();
        renamed.name = "改名".into();
        config.upsert_server(renamed.clone()).unwrap();
        assert_eq!(config.transfer_rules.len(), 2);
        renamed.host = "changed.example.org".into();
        config.upsert_server(renamed).unwrap();
        assert!(config.transfer_rules.is_empty());
        config
            .set_transfer_rule(
                &second.id,
                TransferDirection::Upload,
                Some(ExistingFileAction::Skip),
            )
            .unwrap();
        config.delete_server(&second.id);
        assert!(config.transfer_rules.is_empty());
        assert!(config.validate().is_ok());
        let old: crate::ServerProfile =
            serde_json::from_value(serde_json::to_value(server()).unwrap()).unwrap();
        assert_eq!(old.file_pane_ratio, None);
    }

    #[test]
    fn first_launch_and_atomic_replacement_roundtrip() {
        let dir = TestDir::new();
        let store = dir.store();
        assert_eq!(store.load().unwrap(), AppConfig::default());
        let mut config = AppConfig::default();
        let group = config.add_group("生产环境").unwrap();
        let mut profile = server();
        profile.group_id = Some(group);
        config.upsert_server(profile.clone()).unwrap();
        config
            .trust_host("EXAMPLE.ORG", 22, &fingerprint())
            .unwrap();
        store.save(&config).unwrap();
        assert_eq!(store.load().unwrap(), config);
        profile.name = "重命名".into();
        profile.environment = "Production".into();
        profile.tags = vec!["Web".into(), "中文".into()];
        config.upsert_server(profile).unwrap();
        store.save(&config).unwrap();
        assert_eq!(store.load().unwrap(), config);
        let before = fs::read(store.path()).unwrap();
        let mut invalid = config.clone();
        invalid.servers[0].tags = vec!["Web".into(), "WEB".into()];
        assert!(store.save(&invalid).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), before);
        assert_eq!(store.load().unwrap(), config);
        let contents = fs::read_to_string(store.path()).unwrap();
        assert!(!contents.contains("password"));
        let files: HashSet<_> = fs::read_dir(store.path().parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(
            files,
            HashSet::from([store.path.clone(), store.path.with_added_extension("lock")])
        );
    }

    #[test]
    fn startup_repairs_unknown_file_apps_once_and_preserves_other_preferences() {
        let dir = TestDir::new();
        let store = dir.store();
        let mut config = AppConfig::default();
        config.upsert_server(server()).unwrap();
        config
            .trust_host("example.org", 22, &fingerprint())
            .unwrap();
        store.save(&config).unwrap();
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        let settings = &mut document["config"]["settings"];
        settings["future_setting"] = true.into();
        settings["file_associations"] = serde_json::json!({
            "docx": {"default":"future_app","added_apps":["office","future_app"],"opened_with":["future_app","office"]},
            "pdf": {"default":"pdf_reader","opened_with":["office"]}
        });
        let original = serde_json::to_vec(&document).unwrap();
        fs::write(store.path(), &original).unwrap();
        let loaded = store.load_for_startup();
        assert!(loaded.writable);
        assert!(loaded.diagnostic.is_some());
        assert!(!loaded.config.settings.debug_mode);
        assert_eq!(loaded.config.servers, config.servers);
        assert_eq!(loaded.config.trusted_hosts, config.trusted_hosts);
        let fixed: serde_json::Value =
            serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        document["config"]["settings"]["file_associations"]["docx"] = serde_json::json!({
            "added_apps":["office"],"opened_with":["office"]
        });
        assert_eq!(fixed, document);
        assert!(store.load_for_startup().diagnostic.is_none());
        let backups: Vec<_> = fs::read_dir(store.path().parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "bak"))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), original);
    }

    #[test]
    fn startup_keeps_unrecoverable_documents_read_only() {
        let dir = TestDir::new();
        let store = dir.store();
        let mut config = AppConfig::default();
        config.settings.debug_mode = true;
        store.save(&config).unwrap();
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        document["config"]["settings"]["file_associations"] =
            serde_json::json!({"docx":{"default":"future_app"}});
        document["config"]["trusted_hosts"] =
            serde_json::json!([{"host":"example.org","port":22,"fingerprint":"broken"}]);
        let invalid_host = serde_json::to_vec(&document).unwrap();
        document["config"]["trusted_hosts"] = serde_json::json!([]);
        let duplicate_hosts = serde_json::to_string(&document).unwrap().replace(
            "\"trusted_hosts\":[]",
            "\"trusted_hosts\":[],\"trusted_hosts\":[]",
        );
        document["version"] = 99.into();
        for (bytes, debug_mode) in [
            (invalid_host, true),
            (duplicate_hosts.into_bytes(), true),
            (serde_json::to_vec(&document).unwrap(), true),
            (b"{".to_vec(), false),
        ] {
            fs::write(store.path(), &bytes).unwrap();
            let loaded = store.load_for_startup();
            assert!(!loaded.writable);
            assert!(loaded.diagnostic.is_some());
            assert_eq!(loaded.config.settings.debug_mode, debug_mode);
            assert_eq!(fs::read(store.path()).unwrap(), bytes);
        }
    }

    #[test]
    fn file_columns_are_isolated_per_server_and_pane_and_survive_old_configs() {
        let dir = TestDir::new();
        let store = dir.store();
        let mut config = AppConfig::default();
        let mut first = server();
        let second = server();
        first.file_columns.remote.permissions = true;
        first.file_columns.remote.size = false;
        first.file_columns.local.kind = true;
        config.upsert_server(first.clone()).unwrap();
        config.upsert_server(second.clone()).unwrap();
        store.save(&config).unwrap();
        let restored = store.load().unwrap();
        assert_eq!(restored.servers[0].file_columns, first.file_columns);
        assert_eq!(
            restored.servers[1].file_columns,
            crate::FileColumnSettings::default()
        );
        assert!(restored.servers[0].file_columns.local.size);
        let mut old = serde_json::to_value(first).unwrap();
        old.as_object_mut().unwrap().remove("file_columns");
        let old: crate::ServerProfile = serde_json::from_value(old).unwrap();
        assert_eq!(old.file_columns, crate::FileColumnSettings::default());
    }

    #[test]
    fn corrupted_or_future_documents_remain_untouched() {
        let dir = TestDir::new();
        let store = dir.store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        for data in [
            "broken",
            r#"{"version":99,"config":{"groups":[],"servers":[],"trusted_hosts":[],"local_directory":null}}"#,
        ] {
            fs::write(store.path(), data).unwrap();
            assert!(store.load().is_err());
            assert_eq!(fs::read_to_string(store.path()).unwrap(), data);
        }
    }

    #[test]
    fn failed_validation_preserves_previous_document() {
        let dir = TestDir::new();
        let store = dir.store();
        store.save(&AppConfig::default()).unwrap();
        let prior = fs::read(store.path()).unwrap();
        let mut config = AppConfig::default();
        let mut invalid = server();
        invalid.port = 0;
        config.servers.push(invalid);
        assert!(store.save(&config).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), prior);
    }

    #[test]
    fn group_order_survives_reload_and_invalid_moves_are_atomic() {
        let dir = TestDir::new();
        let store = dir.store();
        let mut config = AppConfig::default();
        let a = config.add_group("A").unwrap();
        let b = config.add_group("B").unwrap();
        let c = config.add_group("C").unwrap();
        let mut profile = server();
        profile.group_id = Some(a.clone());
        config.upsert_server(profile).unwrap();
        let profiles = config.servers.clone();
        assert!(config.move_group(&a, &c, true).unwrap());
        assert_eq!(
            config.groups.iter().map(|g| &g.id).collect::<Vec<_>>(),
            vec![&b, &c, &a]
        );
        assert!(!config.move_group(&a, &c, true).unwrap());
        let prior = config.clone();
        assert!(config.move_group(&a, "missing", false).is_err());
        assert!(config.move_group("missing", &a, false).is_err());
        assert_eq!(config, prior);
        assert!(config.move_group(&a, &b, false).unwrap());
        assert_eq!(config.servers, profiles);
        store.save(&config).unwrap();
        assert_eq!(store.load().unwrap(), config);
    }

    #[test]
    fn deleting_group_profiles_cleans_rules_and_preserves_other_servers() {
        use crate::{ExistingFileAction, TransferDirection};
        let mut config = AppConfig::default();
        let group = config.add_group("delete").unwrap();
        let other = config.add_group("keep").unwrap();
        let mut removed_ids = Vec::new();
        for _ in 0..2 {
            let mut profile = server();
            profile.group_id = Some(group.clone());
            removed_ids.push(profile.id.clone());
            config.upsert_server(profile).unwrap();
        }
        let mut kept = server();
        kept.group_id = Some(other.clone());
        let kept_id = kept.id.clone();
        config.upsert_server(kept.clone()).unwrap();
        let root = server();
        config.upsert_server(root.clone()).unwrap();
        for id in removed_ids.iter().chain(std::iter::once(&kept_id)) {
            config
                .set_transfer_rule(
                    id,
                    TransferDirection::Upload,
                    Some(ExistingFileAction::Skip),
                )
                .unwrap();
        }
        let prior = config.clone();
        assert!(!config.delete_group_and_servers("missing"));
        assert_eq!(config, prior);
        assert!(config.delete_group_and_servers(&group));
        assert_eq!(config.servers, vec![kept, root]);
        assert_eq!(config.transfer_rules.len(), 1);
        assert_eq!(config.transfer_rules[0].server_id, kept_id);
        assert_eq!(config.groups[0].id, other);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn group_deletion_ungroups_servers_and_preserves_profiles() {
        let mut config = AppConfig::default();
        let group = config.add_group("production").unwrap();
        assert!(config.add_group(" Production ").is_err());
        let mut profile = server();
        profile.group_id = Some(group.clone());
        let id = profile.id.clone();
        config.upsert_server(profile).unwrap();
        config.rename_group(&group, "研发").unwrap();
        assert!(config.delete_group(&group));
        assert_eq!(config.servers.len(), 1);
        assert_eq!(config.servers[0].group_id, None);
        assert!(config.validate().is_ok());
        assert!(!config.delete_group(&group));
        assert!(config.delete_server(&id));
        assert!(!config.delete_server(&id));
    }

    #[test]
    fn rejects_invalid_profiles_references_and_duplicate_ids() {
        for host in [
            "",
            "ssh://host",
            "user@host",
            "host:22",
            "host\nname",
            "a b",
        ] {
            let mut profile = server();
            profile.host = host.into();
            assert!(profile.validate().is_err(), "accepted {host:?}");
        }
        let mut profile = server();
        profile.auth = AuthMethod::PrivateKey;
        assert!(profile.validate().is_err());
        profile.private_key = Some(PathBuf::from("id_ed25519"));
        profile.host = "[::1]".into();
        assert!(profile.validate().is_ok());
        let mut config = AppConfig::default();
        profile.group_id = Some("missing".into());
        assert!(config.upsert_server(profile.clone()).is_err());
        profile.group_id = None;
        config.servers = vec![profile.clone(), profile];
        assert!(config.validate().is_err());
    }

    #[test]
    fn trust_matches_host_and_port_and_replaces_explicitly() {
        let mut config = AppConfig::default();
        let first = fingerprint();
        config.trust_host("Example.ORG", 22, &first).unwrap();
        assert_eq!(
            config.trusted_fingerprint("example.org", 22),
            Some(first.as_str())
        );
        assert_eq!(config.trusted_fingerprint("example.org", 2222), None);
        let next = format!("SHA256:{}", STANDARD_NO_PAD.encode([43_u8; 32]));
        config.trust_host("EXAMPLE.org", 22, &next).unwrap();
        assert_eq!(config.trusted_hosts.len(), 1);
        assert_eq!(
            config.trusted_fingerprint("example.org", 22),
            Some(next.as_str())
        );
        assert!(config.trust_host("host", 22, "SHA256:invalid").is_err());
        config.trust_host("[::1]", 22, &first).unwrap();
        assert_eq!(config.trusted_fingerprint("::1", 22), Some(first.as_str()));
    }
    #[test]
    fn retired_file_size_options_migrate_when_loading_a_configuration() {
        use crate::preferences::SizeUnits;
        let mut document: serde_json::Value =
            serde_json::from_slice(&encoded(&AppConfig::default()).unwrap()).unwrap();
        let display = &mut document["config"]["settings"]["display"];
        display["units"] = "Bytes".into();
        display["decimals"] = 3.into();
        display["group_digits"] = true.into();
        let loaded = decoded(&serde_json::to_vec(&document).unwrap()).unwrap();
        assert_eq!(loaded.settings.display.units, SizeUnits::Decimal);
        assert_eq!(loaded.settings.display.decimals, 2);
        assert!(loaded.settings.display.group_digits);
        let saved: serde_json::Value = serde_json::from_slice(&encoded(&loaded).unwrap()).unwrap();
        assert_eq!(saved["config"]["settings"]["display"]["units"], "Decimal");
        document["config"]["settings"]["display"]["decimals"] = 4.into();
        assert!(decoded(&serde_json::to_vec(&document).unwrap()).is_err());
    }

    #[test]
    fn workstation_preferences_and_locations_roundtrip_without_leaking_between_servers() {
        use crate::preferences::*;
        let dir = TestDir::new();
        let store = dir.store();
        let mut config = AppConfig::default();
        let mut first = server();
        first.locations.local_directory = Some(dir.0.clone());
        first.locations.last_local = Some(dir.0.clone());
        first.locations.last_remote = Some("/srv/first".into());
        first.connection = Some(ConnectionSettings {
            timeout_seconds: 5,
            keepalive_seconds: 0,
        });
        let second = server();
        config.upsert_server(first.clone()).unwrap();
        config.upsert_server(second).unwrap();
        config.settings.display.units = SizeUnits::Iec;
        config.settings.display.decimals = 2;
        config.settings.display.group_digits = true;
        config.settings.display.dates = DateStyle::Iso;
        config.settings.display.timezone = DisplayTimezone::Utc;
        config.settings.logging.timestamps = false;
        config.settings.logging.max_size_mib = 50;
        config.settings.updates.interval_hours = 168;
        config.settings.upload_conflict = Some(crate::ExistingFileAction::Skip);
        store.save(&config).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, config);
        assert_eq!(loaded.servers[1].locations.last_remote, None);
        first.host = "other.example.org".into();
        config.upsert_server(first).unwrap();
        assert!(config.servers[0].locations.last_remote.is_none());
        assert!(config.servers[0].locations.last_local.is_none());
        config.settings.connection.timeout_seconds = 0;
        assert!(store.save(&config).is_err());
    }
}
