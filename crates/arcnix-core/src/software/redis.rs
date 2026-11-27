//! Redis / Valkey inspection, key browsing, safe write operations, and service
//! control tasks over SSH.
//!
//! A server can run Redis itself (systemd service), in Docker containers, or
//! both; each is an instance. Inspection and key operations speak RESP2 over
//! loopback TCP or Unix sockets via an embedded Python 3 standard-library
//! helper. Credentials and key values are sent only on stdin JSON, never in
//! command lines or logs. Service start/stop/restart run through reviewed
//! administrator tasks.
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::operations::Snippet;
use crate::remote::{CommandStream, RemoteSession};
use crate::remote_tasks::quote;
use crate::scripts::Template;

const HELPER: &str = include_str!("redis_helper.py");
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const STDIN_LIMIT: usize = 2 * 1024 * 1024;
const INSTANCE_LIMIT: usize = 21;
pub const MAX_KEY_BYTES: usize = 4096;
pub const MAX_VALUE_BYTES: usize = 256 * 1024;
pub const MAX_PATTERN_BYTES: usize = 512;
pub const CLIENTS_LIMIT: usize = 200;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Service {
    pub unit: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub enabled: String,
    pub uptime: Option<u64>,
}

impl Service {
    pub fn running(&self) -> bool {
        self.active == "active"
    }
    pub fn starts_at_boot(&self) -> bool {
        matches!(
            self.enabled.as_str(),
            "enabled" | "enabled-runtime" | "alias"
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub uptime: Option<u64>,
    pub restart: String,
    pub project: String,
    pub service: String,
    pub directory: String,
    pub ports: Vec<String>,
}

impl Container {
    pub fn running(&self) -> bool {
        self.state == "running"
    }
    pub fn starts_at_boot(&self) -> bool {
        matches!(self.restart.as_str(), "always" | "unless-stopped")
    }
    pub fn compose(&self) -> bool {
        !self.project.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub socket: Option<String>,
    #[serde(default)]
    pub container_id: Option<String>,
}

impl Endpoint {
    pub fn validate(&self) -> Result<()> {
        if let Some(socket) = &self.socket {
            ensure!(
                socket.starts_with('/')
                    && socket.len() <= 512
                    && !socket.chars().any(char::is_control),
                "Invalid Redis Unix socket path"
            );
        } else {
            ensure!(
                !self.host.is_empty()
                    && self.host.len() <= 253
                    && self
                        .host
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':')),
                "Invalid Redis host"
            );
            ensure!(self.port > 0, "Invalid Redis port");
        }
        if let Some(cid) = &self.container_id {
            validate_container_id(cid)?;
        }
        Ok(())
    }

    pub fn display_address(&self) -> String {
        if let Some(socket) = &self.socket {
            socket.clone()
        } else if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Auth {
    pub username: String,
    pub password: Zeroizing<String>,
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl Auth {
    pub fn is_empty(&self) -> bool {
        self.password.is_empty()
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.username.len() <= 256 && !self.username.chars().any(char::is_control),
            "Invalid Redis username"
        );
        ensure!(
            self.password.len() <= 4096 && !self.password.contains(['\r', '\n', '\0']),
            "Invalid Redis password"
        );
        Ok(())
    }
    fn wire(&self) -> Option<WireAuth<'_>> {
        (!self.password.is_empty()).then_some(WireAuth {
            username: self.username.trim(),
            password: self.password.as_str(),
        })
    }
}

#[derive(Serialize)]
struct WireAuth<'a> {
    username: &'a str,
    password: &'a str,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AuthStatus {
    #[default]
    Ok,
    Required,
    Failed(String),
    Unreachable(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DatabaseInfo {
    pub index: u8,
    pub keys: u64,
    pub expires: u64,
    pub avg_ttl_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overview {
    pub engine: String,
    pub version: String,
    pub mode: String,
    pub role: String,
    pub uptime_seconds: Option<u64>,
    pub connected_clients: u64,
    pub blocked_clients: u64,
    pub used_memory: u64,
    pub used_memory_rss: u64,
    pub maxmemory: u64,
    pub maxmemory_policy: String,
    pub mem_fragmentation_ratio: String,
    pub instantaneous_ops_per_sec: u64,
    pub keyspace_hits: u64,
    pub keyspace_misses: u64,
    pub rdb_last_save_time: Option<u64>,
    pub rdb_last_bgsave_status: String,
    pub aof_enabled: bool,
    pub databases: Vec<DatabaseInfo>,
    pub total_keys: u64,
}

impl Overview {
    pub fn hit_rate_percent(&self) -> Option<f64> {
        let total = self.keyspace_hits.saturating_add(self.keyspace_misses);
        (total > 0).then(|| (self.keyspace_hits as f64) * 100.0 / (total as f64))
    }
    pub fn product_name(&self) -> &str {
        if self.engine.is_empty() {
            "Redis"
        } else {
            &self.engine
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Instance {
    pub key: String,
    pub title: String,
    pub service: Option<Service>,
    pub docker: Option<Container>,
    pub endpoint: Option<Endpoint>,
    pub installed: bool,
    pub running: bool,
    pub auth_status: AuthStatus,
    pub overview: Option<Overview>,
    pub config_path: Option<String>,
}

impl Instance {
    pub fn uptime(&self) -> Option<u64> {
        self.overview
            .as_ref()
            .and_then(|o| o.uptime_seconds)
            .or_else(|| self.docker.as_ref().and_then(|c| c.uptime))
            .or_else(|| self.service.as_ref().and_then(|s| s.uptime))
    }
    pub fn starts_at_boot(&self) -> bool {
        self.docker
            .as_ref()
            .map(Container::starts_at_boot)
            .or_else(|| self.service.as_ref().map(Service::starts_at_boot))
            .unwrap_or(false)
    }
    pub fn can_control(&self) -> bool {
        self.docker.is_some() || self.service.is_some()
    }
    pub fn ready_for_data(&self) -> bool {
        self.running && self.endpoint.is_some() && self.auth_status == AuthStatus::Ok
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    pub instances: Vec<Instance>,
    pub docker_error: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct KeySummary {
    pub name: String,
    pub kind: String,
    pub ttl: i64,
    pub size: Option<u64>,
    #[serde(default)]
    pub binary: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct ScanPage {
    pub db: u8,
    pub cursor: String,
    pub keys: Vec<KeySummary>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct KeyEntry {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct KeyValue {
    pub name: String,
    pub kind: String,
    pub ttl: i64,
    pub encoding: String,
    pub memory_bytes: Option<u64>,
    pub length: Option<u64>,
    pub truncated: bool,
    pub binary: bool,
    pub string_value: Option<String>,
    pub entries: Vec<KeyEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientEntry {
    pub id: String,
    pub addr: String,
    pub name: String,
    pub db: u8,
    pub age: u64,
    pub idle: u64,
    pub cmd: String,
    pub flags: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct SlowlogEntry {
    pub id: u64,
    pub timestamp: u64,
    pub duration_us: u64,
    pub command: String,
    pub client_addr: String,
    pub client_name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub clients: Vec<ClientEntry>,
    pub slowlog: Vec<SlowlogEntry>,
    pub clients_truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMode {
    Create,
    Edit,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WriteOp {
    SetString {
        key: String,
        value: String,
        ttl_seconds: Option<u64>,
        keep_ttl: bool,
    },
    SetHashField {
        key: String,
        field: String,
        value: String,
    },
    DeleteHashField {
        key: String,
        field: String,
    },
    PushList {
        key: String,
        value: String,
    },
    AddSetMember {
        key: String,
        member: String,
    },
    RemoveSetMember {
        key: String,
        member: String,
    },
    AddZSetMember {
        key: String,
        score: f64,
        member: String,
    },
    RemoveZSetMember {
        key: String,
        member: String,
    },
    SetTtl {
        key: String,
        ttl_seconds: Option<u64>,
    },
    DeleteKey {
        key: String,
    },
}

impl WriteOp {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::SetString {
                key,
                value,
                ttl_seconds,
                keep_ttl,
            } => {
                validate_key(key)?;
                ensure!(
                    value.len() <= MAX_VALUE_BYTES,
                    "Value exceeds the 256 KiB editing limit"
                );
                if !keep_ttl {
                    validate_optional_ttl(*ttl_seconds)?;
                }
            }
            Self::SetHashField { key, field, value } => {
                validate_key(key)?;
                validate_field(field, "Field")?;
                ensure!(
                    value.len() <= MAX_VALUE_BYTES,
                    "Value exceeds the 256 KiB editing limit"
                );
            }
            Self::DeleteHashField { key, field } => {
                validate_key(key)?;
                ensure!(
                    field.len() <= MAX_KEY_BYTES && !field.contains('\0'),
                    "Field is invalid or too long"
                );
            }
            Self::PushList { key, value } => {
                validate_key(key)?;
                ensure!(
                    value.len() <= MAX_VALUE_BYTES,
                    "Value exceeds the 256 KiB editing limit"
                );
            }
            Self::AddSetMember { key, member } => {
                validate_key(key)?;
                ensure!(
                    !member.is_empty() && member.len() <= MAX_VALUE_BYTES,
                    "Enter a member value"
                );
            }
            Self::RemoveSetMember { key, member } | Self::RemoveZSetMember { key, member } => {
                validate_key(key)?;
                ensure!(
                    member.len() <= MAX_VALUE_BYTES,
                    "Value exceeds the 256 KiB editing limit"
                );
            }
            Self::AddZSetMember { key, score, member } => {
                validate_key(key)?;
                ensure!(score.is_finite(), "Enter a valid numeric score");
                ensure!(
                    !member.is_empty() && member.len() <= MAX_VALUE_BYTES,
                    "Enter a member value"
                );
            }
            Self::SetTtl { key, ttl_seconds } => {
                validate_key(key)?;
                validate_optional_ttl(*ttl_seconds)?;
            }
            Self::DeleteKey { key } => {
                validate_key(key)?;
            }
        }
        Ok(())
    }

    fn to_json(&self) -> serde_json::Value {
        match self {
            Self::SetString {
                key,
                value,
                ttl_seconds,
                keep_ttl,
            } => serde_json::json!({
                "kind": "set_string",
                "key": key,
                "value": value,
                "ttl_seconds": ttl_seconds,
                "keep_ttl": keep_ttl,
            }),
            Self::SetHashField { key, field, value } => serde_json::json!({
                "kind": "set_hash_field",
                "key": key,
                "field": field,
                "value": value,
            }),
            Self::DeleteHashField { key, field } => serde_json::json!({
                "kind": "delete_hash_field",
                "key": key,
                "field": field,
            }),
            Self::PushList { key, value } => serde_json::json!({
                "kind": "push_list",
                "key": key,
                "value": value,
            }),
            Self::AddSetMember { key, member } => serde_json::json!({
                "kind": "add_set_member",
                "key": key,
                "member": member,
            }),
            Self::RemoveSetMember { key, member } => serde_json::json!({
                "kind": "remove_set_member",
                "key": key,
                "member": member,
            }),
            Self::AddZSetMember { key, score, member } => serde_json::json!({
                "kind": "add_zset_member",
                "key": key,
                "score": score,
                "member": member,
            }),
            Self::RemoveZSetMember { key, member } => serde_json::json!({
                "kind": "remove_zset_member",
                "key": key,
                "member": member,
            }),
            Self::SetTtl { key, ttl_seconds } => serde_json::json!({
                "kind": "set_ttl",
                "key": key,
                "ttl_seconds": ttl_seconds,
            }),
            Self::DeleteKey { key } => serde_json::json!({
                "kind": "delete_key",
                "key": key,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Start,
    Stop,
    Restart,
}

fn validate_container_id(cid: &str) -> Result<()> {
    ensure!(
        !cid.is_empty() && cid.len() <= 128 && cid.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid Docker container ID"
    );
    Ok(())
}

fn validate_key(key: &str) -> Result<()> {
    ensure!(!key.is_empty(), "Enter a key name");
    ensure!(
        key.len() <= MAX_KEY_BYTES && !key.contains('\0'),
        "Key name is invalid or too long"
    );
    Ok(())
}

fn validate_field(field: &str, label: &str) -> Result<()> {
    ensure!(!field.is_empty(), "{label} cannot be empty");
    ensure!(
        field.len() <= MAX_KEY_BYTES && !field.contains('\0'),
        "{label} is invalid or too long"
    );
    Ok(())
}

fn validate_optional_ttl(ttl: Option<u64>) -> Result<()> {
    if let Some(seconds) = ttl {
        ensure!(seconds > 0, "TTL must be greater than 0 seconds");
    }
    Ok(())
}

fn validate_pattern(pattern: &str) -> Result<String> {
    let trimmed = pattern.trim();
    let effective = if trimmed.is_empty() { "*" } else { trimmed };
    ensure!(
        effective.len() <= MAX_PATTERN_BYTES && !effective.contains(['\0', '\r', '\n']),
        "Invalid key pattern"
    );
    Ok(effective.to_owned())
}

fn validate_cursor(cursor: &str) -> Result<String> {
    let trimmed = cursor.trim();
    let effective = if trimmed.is_empty() { "0" } else { trimmed };
    ensure!(
        effective.len() <= 32 && effective.bytes().all(|b| b.is_ascii_digit()),
        "Invalid scan cursor"
    );
    Ok(effective.to_owned())
}

fn helper_command() -> &'static str {
    static COMMAND: OnceLock<String> = OnceLock::new();
    COMMAND.get_or_init(|| {
        use std::io::Write;

        use base64::Engine as _;

        let script = HELPER.replace("\r\n", "\n");
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        encoder
            .write_all(script.as_bytes())
            .expect("in-memory zlib compression");
        let compressed = encoder.finish().expect("in-memory zlib finish");
        let encoded = base64::engine::general_purpose::STANDARD.encode(compressed);
        let inner = format!(
            "cd /; set -- python3 -I -c \"import base64,zlib;exec(zlib.decompress(base64.b64decode('{encoded}')).decode(),{{'__name__':'__main__'}})\"; if command -v timeout >/dev/null 2>&1; then set -- timeout -k 2 28 \"$@\"; fi; exec \"$@\""
        );
        format!("# arcnix:redis:helper\nexec /bin/sh -c {}", quote(&inner))
    })
}

fn run_helper(session: &RemoteSession, payload: &[u8]) -> Result<String> {
    session.require_linux_capability("Redis")?;
    ensure!(
        payload.len() <= STDIN_LIMIT,
        "Redis request exceeds the size limit"
    );
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let output = session.exec_stream_controlled(
        helper_command(),
        payload,
        Duration::from_secs(30),
        false,
        None,
        false,
        |stream, bytes| {
            ensure!(
                stdout.len() + stderr.len() + bytes.len() <= RESPONSE_LIMIT,
                "Redis response exceeded the size limit"
            );
            match stream {
                CommandStream::Stdout => stdout.extend_from_slice(bytes),
                CommandStream::Stderr => stderr.extend_from_slice(bytes),
            }
            Ok(())
        },
    )?;
    let stderr = String::from_utf8_lossy(&stderr).trim().to_owned();
    ensure!(
        output.exit_code != 127,
        "Reading Redis needs Python 3 on the server"
    );
    if output.exit_code != 0 {
        let message = stderr
            .strip_prefix("REDIS_ERROR:")
            .map(str::trim)
            .unwrap_or(&stderr);
        anyhow::bail!(
            "{}",
            if message.is_empty() {
                format!("Redis request failed (exit {})", output.exit_code)
            } else {
                message.to_owned()
            }
        );
    }
    String::from_utf8(stdout).context("Invalid UTF-8 in Redis response")
}

#[derive(Deserialize)]
struct RawAuthStatus {
    kind: String,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct RawInstance {
    key: String,
    title: String,
    service: Option<Service>,
    docker: Option<Container>,
    endpoint: Option<Endpoint>,
    installed: bool,
    running: bool,
    auth_status: RawAuthStatus,
    info: Option<String>,
    config_path: Option<String>,
}

#[derive(Deserialize)]
struct RawReading {
    instances: Vec<RawInstance>,
    #[serde(default)]
    docker_error: String,
}

pub fn read(session: &RemoteSession, auth_by_instance: &BTreeMap<String, Auth>) -> Result<Reading> {
    let mut auth_json = serde_json::Map::new();
    for (key, auth) in auth_by_instance {
        auth.validate()?;
        if let Some(wire) = auth.wire() {
            auth_json.insert(key.clone(), serde_json::to_value(wire)?);
        }
    }
    let payload = Zeroizing::new(serde_json::to_vec(&serde_json::json!({
        "op": "discover",
        "auth": auth_json,
    }))?);
    let stdout = run_helper(session, &payload)?;
    parse_reading(&stdout)
}

pub fn scan_keys(
    session: &RemoteSession,
    endpoint: &Endpoint,
    auth: Option<&Auth>,
    db: u8,
    cursor: &str,
    pattern: &str,
) -> Result<ScanPage> {
    endpoint.validate()?;
    let cursor = validate_cursor(cursor)?;
    let pattern = validate_pattern(pattern)?;
    if let Some(auth) = auth {
        auth.validate()?;
    }
    let payload = Zeroizing::new(serde_json::to_vec(&serde_json::json!({
        "op": "scan",
        "endpoint": endpoint,
        "auth": auth.and_then(Auth::wire),
        "db": db,
        "cursor": cursor,
        "pattern": pattern,
    }))?);
    let stdout = run_helper(session, &payload)?;
    serde_json::from_str(&stdout).context("Invalid Redis key list response")
}

pub fn read_key(
    session: &RemoteSession,
    endpoint: &Endpoint,
    auth: Option<&Auth>,
    db: u8,
    key: &str,
) -> Result<KeyValue> {
    endpoint.validate()?;
    validate_key(key)?;
    if let Some(auth) = auth {
        auth.validate()?;
    }
    let payload = Zeroizing::new(serde_json::to_vec(&serde_json::json!({
        "op": "get_key",
        "endpoint": endpoint,
        "auth": auth.and_then(Auth::wire),
        "db": db,
        "key": key,
    }))?);
    let stdout = run_helper(session, &payload)?;
    serde_json::from_str(&stdout).context("Invalid Redis key value response")
}

#[derive(Deserialize)]
struct RawDiagnostics {
    clients_text: String,
    slowlog: Vec<SlowlogEntry>,
}

pub fn read_diagnostics(
    session: &RemoteSession,
    endpoint: &Endpoint,
    auth: Option<&Auth>,
) -> Result<Diagnostics> {
    endpoint.validate()?;
    if let Some(auth) = auth {
        auth.validate()?;
    }
    let payload = Zeroizing::new(serde_json::to_vec(&serde_json::json!({
        "op": "diagnostics",
        "endpoint": endpoint,
        "auth": auth.and_then(Auth::wire),
    }))?);
    let stdout = run_helper(session, &payload)?;
    let raw: RawDiagnostics =
        serde_json::from_str(&stdout).context("Invalid Redis diagnostics response")?;
    let (clients, clients_truncated) = parse_client_list(&raw.clients_text);
    Ok(Diagnostics {
        clients,
        slowlog: raw.slowlog,
        clients_truncated,
    })
}

pub fn write_key(
    session: &RemoteSession,
    endpoint: &Endpoint,
    auth: Option<&Auth>,
    db: u8,
    mode: WriteMode,
    op: &WriteOp,
) -> Result<()> {
    endpoint.validate()?;
    op.validate()?;
    if let Some(auth) = auth {
        auth.validate()?;
    }
    let payload = Zeroizing::new(serde_json::to_vec(&serde_json::json!({
        "op": "write",
        "endpoint": endpoint,
        "auth": auth.and_then(Auth::wire),
        "db": db,
        "create_only": mode == WriteMode::Create,
        "write_op": op.to_json(),
    }))?);
    let _ = run_helper(session, &payload)?;
    Ok(())
}

pub fn control(instance: &Instance, control: Control) -> Result<Template> {
    let (id, name, description, command) = match (&instance.docker, &instance.service, control) {
        (Some(container), _, Control::Start) => {
            validate_container_id(&container.id)?;
            (
                "start",
                "Start Redis container",
                "Starts the Redis Docker container.",
                format!("docker start {}", quote(&container.id)),
            )
        }
        (Some(container), _, Control::Stop) => {
            validate_container_id(&container.id)?;
            (
                "stop",
                "Stop Redis container",
                "Stops the Redis Docker container. Clients connected to this container will disconnect.",
                format!("docker stop {}", quote(&container.id)),
            )
        }
        (Some(container), _, Control::Restart) => {
            validate_container_id(&container.id)?;
            (
                "restart",
                "Restart Redis container",
                "Restarts the Redis Docker container. Open connections are dropped.",
                format!("docker restart {}", quote(&container.id)),
            )
        }
        (None, Some(service), Control::Start) => {
            validate_unit(&service.unit)?;
            (
                "start",
                "Start Redis",
                "Starts the Redis system service.",
                format!("systemctl start -- {}", quote(&service.unit)),
            )
        }
        (None, Some(service), Control::Stop) => {
            validate_unit(&service.unit)?;
            (
                "stop",
                "Stop Redis",
                "Stops the Redis system service. Applications using this instance will lose access until it is started again.",
                format!("systemctl stop -- {}", quote(&service.unit)),
            )
        }
        (None, Some(service), Control::Restart) => {
            validate_unit(&service.unit)?;
            (
                "restart",
                "Restart Redis",
                "Restarts the Redis system service. Open connections are dropped.",
                format!("systemctl restart -- {}", quote(&service.unit)),
            )
        }
        (None, None, _) => anyhow::bail!("This Redis instance has no controllable service"),
    };

    let description = match &instance.docker {
        Some(container) => format!("{} · {description}", container.name),
        None => description.to_owned(),
    };
    let prelude = if instance.docker.is_some() {
        "unset DOCKER_HOST DOCKER_CONTEXT DOCKER_TLS_VERIFY DOCKER_CERT_PATH\nif ! command -v docker >/dev/null 2>&1 && test -x /snap/bin/docker; then export PATH=\"$PATH:/snap/bin\"; fi\n"
    } else {
        ""
    };
    Ok(Template::custom(Snippet {
        parameters: Vec::new(),
        id: format!("builtin:redis:{id}"),
        name: name.to_owned(),
        description,
        command: format!("# arcnix:redis:{id}\nset -eu\n{prelude}{command}\n"),
        administrator: true,
    }))
}

fn validate_unit(unit: &str) -> Result<()> {
    ensure!(
        !unit.is_empty()
            && !unit.starts_with('-')
            && unit.len() <= 128
            && unit.ends_with(".service")
            && unit
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@')),
        "Invalid systemd unit name"
    );
    let lower = unit.to_ascii_lowercase();
    ensure!(
        lower == "redis.service"
            || lower == "redis-server.service"
            || lower == "valkey.service"
            || lower == "valkey-server.service"
            || lower.starts_with("redis@")
            || lower.starts_with("redis-server@")
            || lower.starts_with("valkey@")
            || lower.starts_with("valkey-server@"),
        "Unsupported Redis systemd unit"
    );
    Ok(())
}

pub fn parse_reading(text: &str) -> Result<Reading> {
    let raw: RawReading = serde_json::from_str(text).context("Invalid Redis response")?;
    ensure!(
        raw.instances.len() <= INSTANCE_LIMIT,
        "Server reported too many Redis instances"
    );
    let mut seen_keys = HashSet::new();
    let mut instances = Vec::with_capacity(raw.instances.len());
    for item in raw.instances {
        ensure!(
            !item.key.is_empty()
                && item.key.len() <= 160
                && !item.key.chars().any(char::is_control)
                && seen_keys.insert(item.key.clone()),
            "Invalid or duplicate Redis instance key"
        );
        if let Some(endpoint) = &item.endpoint {
            endpoint.validate()?;
        }
        if let Some(container) = &item.docker {
            validate_container_id(&container.id)?;
        }
        if let Some(service) = &item.service {
            validate_unit(&service.unit)?;
        }
        let auth_status = match item.auth_status.kind.as_str() {
            "ok" => AuthStatus::Ok,
            "required" => AuthStatus::Required,
            "failed" => AuthStatus::Failed(item.auth_status.message),
            _ => AuthStatus::Unreachable(item.auth_status.message),
        };
        let overview = item.info.as_deref().map(parse_info);
        instances.push(Instance {
            key: item.key,
            title: item.title,
            service: item.service,
            docker: item.docker,
            endpoint: item.endpoint,
            installed: item.installed,
            running: item.running,
            auth_status,
            overview,
            config_path: item.config_path,
        });
    }
    Ok(Reading {
        instances,
        docker_error: raw.docker_error,
    })
}

pub fn parse_info(text: &str) -> Overview {
    let mut overview = Overview::default();
    let mut total_keys = 0u64;
    let mut redis_version = String::new();
    let mut valkey_version = String::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "valkey_version" if !value.is_empty() => {
                valkey_version = value.to_owned();
            }
            "redis_version" if !value.is_empty() => {
                redis_version = value.to_owned();
            }
            "valkey_mode" | "redis_mode" if overview.mode.is_empty() => {
                overview.mode = value.to_owned();
            }
            "role" => overview.role = value.to_owned(),
            "uptime_in_seconds" => overview.uptime_seconds = value.parse().ok(),
            "connected_clients" => overview.connected_clients = value.parse().unwrap_or(0),
            "blocked_clients" => overview.blocked_clients = value.parse().unwrap_or(0),
            "used_memory" => overview.used_memory = value.parse().unwrap_or(0),
            "used_memory_rss" => overview.used_memory_rss = value.parse().unwrap_or(0),
            "maxmemory" => overview.maxmemory = value.parse().unwrap_or(0),
            "maxmemory_policy" => overview.maxmemory_policy = value.to_owned(),
            "mem_fragmentation_ratio" => overview.mem_fragmentation_ratio = value.to_owned(),
            "instantaneous_ops_per_sec" => {
                overview.instantaneous_ops_per_sec = value.parse().unwrap_or(0)
            }
            "keyspace_hits" => overview.keyspace_hits = value.parse().unwrap_or(0),
            "keyspace_misses" => overview.keyspace_misses = value.parse().unwrap_or(0),
            "rdb_last_save_time" => overview.rdb_last_save_time = value.parse().ok(),
            "rdb_last_bgsave_status" => overview.rdb_last_bgsave_status = value.to_owned(),
            "aof_enabled" => overview.aof_enabled = value == "1",
            _ if key.starts_with("db") => {
                if let Ok(index) = key[2..].parse::<u8>() {
                    let db = parse_db_stats(index, value);
                    total_keys = total_keys.saturating_add(db.keys);
                    overview.databases.push(db);
                }
            }
            _ => {}
        }
    }
    if !valkey_version.is_empty() {
        overview.engine = "Valkey".to_owned();
        overview.version = valkey_version;
    } else {
        overview.engine = "Redis".to_owned();
        overview.version = redis_version;
    }
    overview.databases.sort_by_key(|db| db.index);
    overview.total_keys = total_keys;
    overview
}

fn parse_db_stats(index: u8, value: &str) -> DatabaseInfo {
    let mut keys = 0;
    let mut expires = 0;
    let mut avg_ttl_ms = None;
    for part in value.split(',') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k.trim() {
            "keys" => keys = v.trim().parse().unwrap_or(0),
            "expires" => expires = v.trim().parse().unwrap_or(0),
            "avg_ttl" => avg_ttl_ms = v.trim().parse().ok(),
            _ => {}
        }
    }
    DatabaseInfo {
        index,
        keys,
        expires,
        avg_ttl_ms,
    }
}

pub fn parse_client_list(text: &str) -> (Vec<ClientEntry>, bool) {
    let mut clients = Vec::new();
    let mut truncated = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if clients.len() >= CLIENTS_LIMIT {
            truncated = true;
            break;
        }
        let mut entry = ClientEntry::default();
        for token in line.split_whitespace() {
            let Some((k, v)) = token.split_once('=') else {
                continue;
            };
            match k {
                "id" => entry.id = v.to_owned(),
                "addr" => entry.addr = v.to_owned(),
                "name" => entry.name = v.to_owned(),
                "db" => entry.db = v.parse().unwrap_or(0),
                "age" => entry.age = v.parse().unwrap_or(0),
                "idle" => entry.idle = v.parse().unwrap_or(0),
                "cmd" => entry.cmd = v.to_owned(),
                "flags" => entry.flags = v.to_owned(),
                _ => {}
            }
        }
        if !entry.addr.is_empty() || !entry.id.is_empty() {
            clients.push(entry);
        }
    }
    (clients, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_debug_redacts_password() {
        let auth = Auth {
            username: "ops".into(),
            password: Zeroizing::new("super-secret-token".into()),
        };
        let rendered = format!("{auth:?}");
        assert!(rendered.contains("ops"));
        assert!(rendered.contains("[REDACTED]"));
        assert!(!rendered.contains("super-secret-token"));
    }

    #[test]
    fn parses_redis_and_valkey_info() {
        let sample = "\
# Server\r\n\
redis_version:7.2.4\r\n\
valkey_version:8.0.1\r\n\
redis_mode:standalone\r\n\
uptime_in_seconds:86420\r\n\
# Clients\r\n\
connected_clients:14\r\n\
blocked_clients:1\r\n\
# Memory\r\n\
used_memory:2097152\r\n\
used_memory_rss:3145728\r\n\
maxmemory:1073741824\r\n\
maxmemory_policy:allkeys-lru\r\n\
mem_fragmentation_ratio:1.50\r\n\
# Persistence\r\n\
rdb_last_save_time:1730000000\r\n\
rdb_last_bgsave_status:ok\r\n\
aof_enabled:1\r\n\
# Stats\r\n\
instantaneous_ops_per_sec:420\r\n\
keyspace_hits:900\r\n\
keyspace_misses:100\r\n\
# Replication\r\n\
role:master\r\n\
# Keyspace\r\n\
db2:keys=12,expires=3,avg_ttl=60000\r\n\
db0:keys=100,expires=40,avg_ttl=12000\r\n";

        let info = parse_info(sample);
        assert_eq!(info.product_name(), "Valkey");
        assert_eq!(info.version, "8.0.1");
        assert_eq!(info.mode, "standalone");
        assert_eq!(info.role, "master");
        assert_eq!(info.uptime_seconds, Some(86420));
        assert_eq!(info.connected_clients, 14);
        assert_eq!(info.blocked_clients, 1);
        assert_eq!(info.used_memory, 2097152);
        assert_eq!(info.maxmemory, 1073741824);
        assert_eq!(info.maxmemory_policy, "allkeys-lru");
        assert!(info.aof_enabled);
        assert_eq!(info.rdb_last_bgsave_status, "ok");
        assert_eq!(info.total_keys, 112);
        assert_eq!(info.databases.len(), 2);
        assert_eq!(info.databases[0].index, 0);
        assert_eq!(info.databases[0].keys, 100);
        assert_eq!(info.databases[1].index, 2);
        assert!((info.hit_rate_percent().unwrap() - 90.0).abs() < 1e-6);
    }

    #[test]
    fn parses_client_list_entries() {
        let sample = "id=42 addr=127.0.0.1:54321 laddr=127.0.0.1:6379 fd=8 name=worker-1 age=120 idle=3 flags=N db=2 cmd=hgetall\n\
id=43 addr=10.0.0.5:40000 fd=9 name= age=10 idle=0 flags=N db=0 cmd=client|list\n";
        let (clients, truncated) = parse_client_list(sample);
        assert!(!truncated);
        assert_eq!(clients.len(), 2);
        assert_eq!(clients[0].id, "42");
        assert_eq!(clients[0].name, "worker-1");
        assert_eq!(clients[0].db, 2);
        assert_eq!(clients[0].cmd, "hgetall");
        assert_eq!(clients[1].addr, "10.0.0.5:40000");
    }

    #[test]
    fn validates_write_ops_endpoints_and_units() {
        assert!(
            WriteOp::SetString {
                key: "".into(),
                value: "v".into(),
                ttl_seconds: None,
                keep_ttl: false,
            }
            .validate()
            .is_err()
        );
        assert!(
            WriteOp::SetString {
                key: "session:1".into(),
                value: "ok".into(),
                ttl_seconds: Some(0),
                keep_ttl: false,
            }
            .validate()
            .is_err()
        );
        assert!(
            WriteOp::SetString {
                key: "session:1".into(),
                value: "ok".into(),
                ttl_seconds: None,
                keep_ttl: true,
            }
            .validate()
            .is_ok()
        );
        assert!(
            WriteOp::RemoveSetMember {
                key: "tags".into(),
                member: String::new(),
            }
            .validate()
            .is_ok()
        );
        assert!(
            WriteOp::AddZSetMember {
                key: "scores".into(),
                score: f64::NAN,
                member: "alice".into(),
            }
            .validate()
            .is_err()
        );
        let ipv6 = Endpoint {
            host: "::1".into(),
            port: 6379,
            socket: None,
            container_id: None,
        };
        assert!(ipv6.validate().is_ok());
        assert_eq!(ipv6.display_address(), "[::1]:6379");

        assert!(validate_unit("redis-server.service").is_ok());
        assert!(validate_unit("valkey@cache.service").is_ok());
        assert!(validate_unit("sshd.service").is_err());
        assert!(validate_unit("-redis.service").is_err());
        assert!(
            helper_command().len() < 30 * 1024,
            "SSH exec command must stay below the 32 KiB SSH packet limit"
        );
        // Heavily JSON-escaped 256 KiB value must still fit within STDIN_LIMIT.
        let escaped = WriteOp::SetString {
            key: "big".into(),
            value: "\x01".repeat(MAX_VALUE_BYTES),
            ttl_seconds: None,
            keep_ttl: false,
        };
        assert!(escaped.validate().is_ok());
        let wire = serde_json::to_vec(&serde_json::json!({
            "op": "write",
            "write_op": escaped.to_json(),
        }))
        .unwrap();
        assert!(wire.len() <= STDIN_LIMIT);
    }

    #[cfg(unix)]
    #[test]
    fn helper_command_runs_under_sh_and_zsh() -> Result<()> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        for shell in ["/bin/sh", "/bin/zsh"] {
            if !std::path::Path::new(shell).is_file() {
                continue;
            }
            let mut child = Command::new(shell)
                .args(["-c", helper_command()])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(br#"{"op":"unknown_probe"}"#)?;
            let out = child.wait_with_output()?;
            assert_eq!(out.status.code(), Some(1), "shell={shell}");
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                err.contains("Unknown Redis helper operation: unknown_probe"),
                "shell={shell} stderr={err}"
            );
        }
        Ok(())
    }

    #[test]
    fn rejects_untrusted_parse_reading_payloads() {
        // Compromised server claiming sshd.service as Redis unit must be rejected.
        let bad_unit = r#"{
            "instances": [{
                "key": "host",
                "title": "On the server",
                "service": {"unit": "sshd.service", "load": "loaded", "active": "active", "sub": "running", "enabled": "enabled", "uptime": 10},
                "docker": null,
                "endpoint": {"host": "127.0.0.1", "port": 6379},
                "installed": true,
                "running": true,
                "auth_status": {"kind": "ok", "message": ""},
                "info": null,
                "config_path": null
            }]
        }"#;
        assert!(parse_reading(bad_unit).is_err());
    }

    #[test]
    fn builds_service_and_container_control_templates() -> Result<()> {
        let host = Instance {
            key: "host".into(),
            title: "On the server".into(),
            service: Some(Service {
                unit: "redis-server.service".into(),
                load: "loaded".into(),
                active: "active".into(),
                sub: "running".into(),
                enabled: "enabled".into(),
                uptime: Some(60),
            }),
            ..Default::default()
        };
        let tpl = control(&host, Control::Restart)?;
        assert!(tpl.snippet.administrator);
        assert!(
            tpl.snippet
                .command
                .contains("systemctl restart -- 'redis-server.service'")
        );

        let docker = Instance {
            key: "docker:abcdef123456".into(),
            title: "redis".into(),
            docker: Some(Container {
                id: "abcdef123456".into(),
                name: "redis".into(),
                state: "running".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let tpl = control(&docker, Control::Stop)?;
        assert!(tpl.snippet.administrator);
        assert!(tpl.snippet.command.contains("unset DOCKER_HOST"));
        assert!(tpl.snippet.command.contains("docker stop 'abcdef123456'"));
        Ok(())
    }
}
