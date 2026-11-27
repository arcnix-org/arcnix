//! The Redis app: overview, key browser (bounded SCAN + safe editing),
//! connected clients, slowlog, and service control, for Redis / Valkey on the
//! server and in Docker containers.
use std::collections::BTreeMap;
use std::rc::Rc;

use arcnix_core::redis::{
    self, Auth, AuthStatus, Control, Diagnostics, Instance, KeySummary, KeyValue, WriteMode,
    WriteOp,
};
use zeroize::Zeroizing;

use crate::i18n::desktop as i18n;
use crate::i18n::desktop::{btn, icon_action, text_action};
use crate::server_apps_worker::{Action as AppAction, Reply};
use crate::*;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum View {
    #[default]
    Overview,
    Keys,
    Diagnostics,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum NewKeyKind {
    #[default]
    String,
    Hash,
    List,
    Set,
    ZSet,
}

#[derive(Clone, Default)]
struct NewKey {
    kind: NewKeyKind,
    key: String,
    field: String,
    score: String,
    value: String,
    ttl: String,
}

#[derive(Clone, PartialEq, Eq)]
struct PendingData {
    serial: u64,
    instance: String,
    db: u8,
}

#[derive(Clone, Default)]
pub(crate) struct RedisState {
    instances: Vec<Rc<Instance>>,
    instance: String,
    snapshot: Option<Rc<Instance>>,
    docker_error: String,
    /// Per-instance Redis credentials kept only in memory for this session.
    auth: BTreeMap<String, Auth>,
    auth_username: String,
    auth_password: Zeroizing<String>,
    pending: Option<u64>,
    keys_pending: Option<PendingData>,
    key_pending: Option<PendingData>,
    diag_pending: Option<PendingData>,
    write_pending: Option<PendingData>,
    /// Whether the pending scan is loading a next page (append) vs fresh scan.
    scan_append: bool,
    /// Key to re-read after a write completes (None when deleting).
    after_write_key: Option<String>,
    error: String,
    notice: String,
    info: String,
    view: View,
    db: u8,
    pattern: String,
    cursor: String,
    keys: Vec<KeySummary>,
    keys_loaded: bool,
    keys_truncated: bool,
    selected_key: Option<Rc<KeyValue>>,
    edit_value: String,
    edit_ttl: String,
    pretty_json: bool,
    add_field: String,
    add_score: String,
    add_value: String,
    new_key: NewKey,
    delete_target: String,
    diagnostics: Option<Rc<Diagnostics>>,
    client_filter: String,
}

impl RedisState {
    fn clear_instance_views(&mut self) {
        self.keys_pending = None;
        self.key_pending = None;
        self.diag_pending = None;
        self.write_pending = None;
        self.after_write_key = None;
        self.auth_username.clear();
        self.auth_password = Default::default();
        self.db = 0;
        self.pattern.clear();
        self.client_filter.clear();
        self.keys.clear();
        self.keys_loaded = false;
        self.keys_truncated = false;
        self.cursor.clear();
        self.selected_key = None;
        self.edit_value.clear();
        self.edit_ttl.clear();
        self.pretty_json = false;
        self.add_field.clear();
        self.add_score.clear();
        self.add_value.clear();
        self.diagnostics = None;
        self.notice.clear();
        self.info.clear();
    }
}

pub(crate) fn ready(s: &State) -> bool {
    s.connected && s.server_apps.applications.redis && s.redis.pending.is_none()
}

fn busy_data(s: &State) -> bool {
    s.redis.keys_pending.is_some()
        || s.redis.key_pending.is_some()
        || s.redis.write_pending.is_some()
}

fn can_change_service(s: &State) -> bool {
    s.connected
        && !scripts_ui::busy(s)
        && s.redis.snapshot.as_ref().is_some_and(|i| i.can_control())
}

fn data_ready(s: &State) -> bool {
    s.connected
        && s.server_apps.applications.redis
        && s.redis
            .snapshot
            .as_ref()
            .is_some_and(|i| i.ready_for_data())
}

fn needs_auth(s: &State) -> bool {
    s.redis.snapshot.as_ref().is_some_and(|inst| {
        inst.running
            && matches!(
                inst.auth_status,
                AuthStatus::Required | AuthStatus::Failed(_)
            )
    })
}

fn status(s: &State) -> (&'static str, &'static str) {
    let Some(inst) = &s.redis.snapshot else {
        return ("Checking…", "nginx-status");
    };
    if let Some(container) = &inst.docker {
        return match container.state.as_str() {
            "running" => match &inst.auth_status {
                AuthStatus::Ok => ("Running", "nginx-status running"),
                AuthStatus::Required | AuthStatus::Failed(_) => {
                    ("Authentication required", "nginx-status")
                }
                AuthStatus::Unreachable(_) => ("Unreachable", "nginx-status failed"),
            },
            "restarting" => ("Restarting", "nginx-status"),
            "paused" => ("Paused", "nginx-status stopped"),
            "dead" => ("Failed", "nginx-status failed"),
            _ => ("Stopped", "nginx-status stopped"),
        };
    }
    if inst.running {
        return match &inst.auth_status {
            AuthStatus::Ok => ("Running", "nginx-status running"),
            AuthStatus::Required | AuthStatus::Failed(_) => {
                ("Authentication required", "nginx-status")
            }
            AuthStatus::Unreachable(_) => ("Unreachable", "nginx-status failed"),
        };
    }
    match &inst.service {
        Some(svc) => match svc.active.as_str() {
            "failed" => ("Failed", "nginx-status failed"),
            "activating" => ("Starting", "nginx-status"),
            "deactivating" => ("Stopping", "nginx-status"),
            _ => ("Stopped", "nginx-status stopped"),
        },
        None => ("Stopped", "nginx-status stopped"),
    }
}

fn uptime(seconds: u64) -> String {
    match seconds {
        0..60 => "up less than a minute".into(),
        60..3600 => format!("up {} min", seconds / 60),
        3600..86400 => format!("up {} h", seconds / 3600),
        _ => format!("up {} days", seconds / 86400),
    }
}

fn format_ttl(ttl: i64) -> String {
    if ttl < 0 {
        i18n::translate("No expiry")
    } else if ttl < 60 {
        format!("{ttl} s")
    } else if ttl < 3600 {
        format!("{} m {} s", ttl / 60, ttl % 60)
    } else if ttl < 86400 {
        format!("{} h {} m", ttl / 3600, (ttl % 3600) / 60)
    } else {
        format!("{} d {} h", ttl / 86400, (ttl % 86400) / 3600)
    }
}

fn format_duration_us(us: u64) -> String {
    if us < 1000 {
        format!("{us} µs")
    } else if us < 1_000_000 {
        format!("{:.2} ms", us as f64 / 1000.0)
    } else {
        format!("{:.2} s", us as f64 / 1_000_000.0)
    }
}

fn details(s: &State) -> String {
    let Some(inst) = &s.redis.snapshot else {
        return String::new();
    };
    let mut parts = Vec::new();
    let version = inst
        .overview
        .as_ref()
        .filter(|o| !o.version.is_empty())
        .map(|o| format!("{} {}", o.product_name(), o.version));
    parts.push(version.unwrap_or_else(|| "Redis".to_owned()));
    if let Some(secs) = inst.uptime() {
        parts.push(i18n::translate(&uptime(secs)));
    }
    match &inst.docker {
        Some(container) => parts.push(i18n::translate(if container.starts_at_boot() {
            "restarts automatically"
        } else {
            "does not restart automatically"
        })),
        None if inst.service.is_some() => {
            parts.push(i18n::translate(if inst.starts_at_boot() {
                "starts at boot"
            } else {
                "does not start at boot"
            }));
        }
        None => {}
    }
    if let Some(endpoint) = &inst.endpoint {
        parts.push(endpoint.display_address());
    }
    parts.join(" · ")
}

fn origin(s: &State) -> String {
    let Some(container) = s.redis.snapshot.as_ref().and_then(|i| i.docker.as_ref()) else {
        return String::new();
    };
    let mut parts = vec![if container.compose() {
        i18n::translate(&format!(
            "Compose service {} in project {}",
            container.service, container.project
        ))
    } else {
        i18n::translate(&format!("Docker container {}", container.name))
    }];
    parts.push(container.image.clone());
    if !container.directory.is_empty() {
        parts.push(container.directory.clone());
    }
    parts.join(" · ")
}

fn tail(output: &str) -> String {
    let lines: Vec<&str> = output.trim_end().lines().collect();
    lines[lines.len().saturating_sub(8)..].join("\n")
}

fn maybe_pretty_json(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    serde_json::to_string_pretty(&parsed).ok()
}

fn kind_badge_class(kind: &str) -> &'static str {
    match kind {
        "string" => "redis-type-badge type-string",
        "hash" => "redis-type-badge type-hash",
        "list" => "redis-type-badge type-list",
        "set" => "redis-type-badge type-set",
        "zset" => "redis-type-badge type-zset",
        "stream" => "redis-type-badge type-stream",
        _ => "redis-type-badge",
    }
}

impl Ui {
    pub(crate) fn redis_enter(&self) {
        if self.state.with(|s| {
            navigation::workspace_visible(s)
                && s.workspace_page == WorkspacePage::Redis
                && s.redis.snapshot.is_none()
                && s.redis.error.is_empty()
                && ready(s)
        }) {
            self.redis_refresh();
        }
    }

    fn redis_refresh(&self) {
        if !self.state.with(ready) {
            return;
        }
        let auth = self.state.with(|s| s.redis.auth.clone());
        let serial = self.send(Action::ServerApps(AppAction::RedisRead(auth)));
        self.state.revise(|s| {
            s.redis.pending = Some(serial);
            s.redis.error.clear();
        });
    }

    pub(crate) fn redis_task_finished(&self, task: &str) {
        let finished = self.state.with(|s| {
            let record = s.scripts.tasks.iter().find(|t| t.id == task)?;
            if !record.script_id.starts_with("builtin:redis:")
                && record.script_id != "builtin:service-operation"
            {
                return None;
            }
            let target = record
                .targets
                .iter()
                .find(|t| s.selected.as_ref() == Some(&t.id))?;
            Some((
                record.name.clone(),
                target.status == arcnix_core::tasks::Status::Failed,
                target.output.clone(),
            ))
        });
        if let Some((name, failed, output)) = finished {
            self.state.revise(|s| {
                if failed {
                    s.redis.notice = format!(
                        "{}\n{}",
                        i18n::translate(&format!("{name} failed")),
                        tail(&output)
                    );
                } else {
                    s.redis.notice.clear();
                }
            });
            if self.state.with(|s| {
                s.server_apps.applications.redis && s.workbench.apps.contains(&WorkspacePage::Redis)
            }) {
                self.state.revise(|s| s.redis.pending = None);
                self.redis_refresh();
            }
        }
    }

    fn redis_control(&self, control: Control) {
        let template = self.state.with(|s| {
            s.redis
                .snapshot
                .as_ref()
                .filter(|_| can_change_service(s))
                .map(|inst| redis::control(inst, control))
        });
        if let Some(result) = template {
            self.state.revise(|s| s.redis.notice.clear());
            match result {
                Ok(template) => self.choose_server_script(template),
                Err(error) => self
                    .state
                    .revise(|s| s.redis.error = i18n::translate(&format!("{error:#}"))),
            }
        }
    }

    fn redis_unlock(&self) {
        let updated = self.state.revise(|s| {
            let key = s.redis.instance.clone();
            if key.is_empty() || s.redis.auth_password.is_empty() {
                return false;
            }
            let auth = Auth {
                username: s.redis.auth_username.trim().to_owned(),
                password: s.redis.auth_password.clone(),
            };
            if let Err(error) = auth.validate() {
                s.redis.error = i18n::translate(&format!("{error:#}"));
                return false;
            }
            s.redis.auth_username.clear();
            s.redis.auth_password = Default::default();
            s.redis.auth.insert(key, auth);
            s.redis.pending = None;
            s.redis.clear_instance_views();
            s.redis.error.clear();
            true
        });
        if updated {
            self.redis_refresh();
        }
    }

    fn redis_clear_auth(&self) {
        self.state.revise(|s| {
            let key = s.redis.instance.clone();
            s.redis.auth.remove(&key);
            s.redis.auth_username.clear();
            s.redis.auth_password = Default::default();
            s.redis.pending = None;
            s.redis.clear_instance_views();
        });
        self.redis_refresh();
    }

    fn redis_scan(&self, append: bool, clear_info: bool) {
        let req = self.state.with(|s| {
            if !data_ready(s) || s.redis.keys_pending.is_some() {
                return None;
            }
            let inst = s.redis.snapshot.as_ref()?;
            let cursor = if append {
                if s.redis.cursor.is_empty() || s.redis.cursor == "0" {
                    return None;
                }
                s.redis.cursor.clone()
            } else {
                "0".to_owned()
            };
            Some((
                inst.key.clone(),
                inst.endpoint.clone()?,
                s.redis.auth.get(&inst.key).cloned(),
                s.redis.db,
                cursor,
                s.redis.pattern.clone(),
            ))
        });
        let Some((instance, endpoint, auth, db, cursor, pattern)) = req else {
            return;
        };
        let serial = self.send(Action::ServerApps(AppAction::RedisScan {
            endpoint,
            auth,
            db,
            cursor,
            pattern,
        }));
        self.state.revise(|s| {
            s.redis.keys_pending = Some(PendingData {
                serial,
                instance,
                db,
            });
            s.redis.scan_append = append;
            s.redis.error.clear();
            if clear_info {
                s.redis.info.clear();
            }
        });
    }

    fn redis_select_key(&self, key: String, clear_info: bool) {
        let req = self.state.with(|s| {
            if !data_ready(s) {
                return None;
            }
            let inst = s.redis.snapshot.as_ref()?;
            Some((
                inst.key.clone(),
                inst.endpoint.clone()?,
                s.redis.auth.get(&inst.key).cloned(),
                s.redis.db,
                key,
            ))
        });
        let Some((instance, endpoint, auth, db, key)) = req else {
            return;
        };
        let serial = self.send(Action::ServerApps(AppAction::RedisReadKey {
            endpoint,
            auth,
            db,
            key,
        }));
        self.state.revise(|s| {
            s.redis.key_pending = Some(PendingData {
                serial,
                instance,
                db,
            });
            s.redis.error.clear();
            if clear_info {
                s.redis.info.clear();
            }
        });
    }

    fn redis_diagnostics(&self) {
        let req = self.state.with(|s| {
            if !data_ready(s) || s.redis.diag_pending.is_some() {
                return None;
            }
            let inst = s.redis.snapshot.as_ref()?;
            Some((
                inst.key.clone(),
                inst.endpoint.clone()?,
                s.redis.auth.get(&inst.key).cloned(),
            ))
        });
        let Some((instance, endpoint, auth)) = req else {
            return;
        };
        let serial = self.send(Action::ServerApps(AppAction::RedisDiagnostics {
            endpoint,
            auth,
        }));
        self.state.revise(|s| {
            s.redis.diag_pending = Some(PendingData {
                serial,
                instance,
                db: 0,
            });
            s.redis.error.clear();
        });
    }

    fn redis_write(&self, mode: WriteMode, op: WriteOp, after_write_key: Option<String>) {
        if let Err(error) = op.validate() {
            self.state
                .revise(|s| s.redis.error = i18n::translate(&format!("{error:#}")));
            return;
        }
        let req = self.state.with(|s| {
            if !data_ready(s) || s.redis.write_pending.is_some() {
                return None;
            }
            let inst = s.redis.snapshot.as_ref()?;
            Some((
                inst.key.clone(),
                inst.endpoint.clone()?,
                s.redis.auth.get(&inst.key).cloned(),
                s.redis.db,
            ))
        });
        let Some((instance, endpoint, auth, db)) = req else {
            return;
        };
        let serial = self.send(Action::ServerApps(AppAction::RedisWrite {
            mode,
            endpoint,
            auth,
            db,
            op,
        }));
        self.state.revise(|s| {
            s.redis.write_pending = Some(PendingData {
                serial,
                instance,
                db,
            });
            s.redis.after_write_key = after_write_key;
            s.redis.error.clear();
            s.redis.info.clear();
        });
    }

    fn redis_save_string(&self) {
        let data = self.state.with(|s| {
            let kv = s.redis.selected_key.as_ref()?;
            if kv.kind != "string" || kv.binary || kv.truncated {
                return None;
            }
            Some((kv.name.clone(), s.redis.edit_value.clone()))
        });
        if let Some((key, value)) = data {
            self.redis_write(
                WriteMode::Edit,
                WriteOp::SetString {
                    key: key.clone(),
                    value,
                    ttl_seconds: None,
                    keep_ttl: true,
                },
                Some(key),
            );
        }
    }

    fn redis_apply_ttl(&self, persist: bool) {
        let result = self
            .state
            .with(|s| -> anyhow::Result<(String, Option<u64>)> {
                let kv = s
                    .redis
                    .selected_key
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Select a key first"))?;
                let ttl = if persist {
                    None
                } else {
                    let raw = s.redis.edit_ttl.trim();
                    let secs: u64 = raw
                        .parse()
                        .map_err(|_| anyhow::anyhow!("Enter TTL in whole seconds"))?;
                    anyhow::ensure!(secs > 0, "TTL must be greater than 0 seconds");
                    Some(secs)
                };
                Ok((kv.name.clone(), ttl))
            });
        match result {
            Ok((key, ttl_seconds)) => {
                self.redis_write(
                    WriteMode::Edit,
                    WriteOp::SetTtl {
                        key: key.clone(),
                        ttl_seconds,
                    },
                    Some(key),
                );
            }
            Err(error) => self
                .state
                .revise(|s| s.redis.error = i18n::translate(&format!("{error:#}"))),
        }
    }

    fn redis_add_collection_item(&self) {
        let result = self.state.with(|s| -> anyhow::Result<(WriteOp, String)> {
            let kv = s
                .redis
                .selected_key
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Select a key first"))?;
            let key = kv.name.clone();
            let op = match kv.kind.as_str() {
                "hash" => WriteOp::SetHashField {
                    key: key.clone(),
                    field: s.redis.add_field.clone(),
                    value: s.redis.add_value.clone(),
                },
                "list" => WriteOp::PushList {
                    key: key.clone(),
                    value: s.redis.add_value.clone(),
                },
                "set" => WriteOp::AddSetMember {
                    key: key.clone(),
                    member: s.redis.add_value.clone(),
                },
                "zset" => {
                    let score: f64 = s
                        .redis
                        .add_score
                        .trim()
                        .parse()
                        .map_err(|_| anyhow::anyhow!("Enter a valid numeric score"))?;
                    WriteOp::AddZSetMember {
                        key: key.clone(),
                        score,
                        member: s.redis.add_value.clone(),
                    }
                }
                _ => anyhow::bail!("Adding entries is not supported for this type"),
            };
            op.validate()?;
            Ok((op, key))
        });
        match result {
            Ok((op, key)) => {
                self.state.revise(|s| {
                    s.redis.add_field.clear();
                    s.redis.add_score.clear();
                    s.redis.add_value.clear();
                });
                self.redis_write(WriteMode::Edit, op, Some(key));
            }
            Err(error) => self
                .state
                .revise(|s| s.redis.error = i18n::translate(&format!("{error:#}"))),
        }
    }

    fn redis_open_new_key(&self) {
        self.state.revise(|s| {
            s.redis.new_key = NewKey::default();
            s.dialog_error.clear();
        });
        self.open_dialog(Dialog::RedisNewKey);
    }

    fn redis_submit_new_key(&self) {
        let result = self.state.with(|s| -> anyhow::Result<(WriteOp, String)> {
            let n = &s.redis.new_key;
            let key = n.key.clone();
            let op = match n.kind {
                NewKeyKind::String => {
                    let ttl_seconds = if n.ttl.trim().is_empty() {
                        None
                    } else {
                        let secs: u64 = n
                            .ttl
                            .trim()
                            .parse()
                            .map_err(|_| anyhow::anyhow!("Enter TTL in whole seconds"))?;
                        anyhow::ensure!(secs > 0, "TTL must be greater than 0 seconds");
                        Some(secs)
                    };
                    WriteOp::SetString {
                        key: key.clone(),
                        value: n.value.clone(),
                        ttl_seconds,
                        keep_ttl: false,
                    }
                }
                NewKeyKind::Hash => WriteOp::SetHashField {
                    key: key.clone(),
                    field: n.field.clone(),
                    value: n.value.clone(),
                },
                NewKeyKind::List => WriteOp::PushList {
                    key: key.clone(),
                    value: n.value.clone(),
                },
                NewKeyKind::Set => WriteOp::AddSetMember {
                    key: key.clone(),
                    member: n.value.clone(),
                },
                NewKeyKind::ZSet => {
                    let score: f64 = n
                        .score
                        .trim()
                        .parse()
                        .map_err(|_| anyhow::anyhow!("Enter a valid numeric score"))?;
                    WriteOp::AddZSetMember {
                        key: key.clone(),
                        score,
                        member: n.value.clone(),
                    }
                }
            };
            op.validate()?;
            Ok((op, key))
        });
        match result {
            Ok((op, key)) => {
                self.close_dialog();
                self.redis_write(WriteMode::Create, op, Some(key));
            }
            Err(error) => self
                .state
                .revise(|s| s.dialog_error = i18n::translate(&format!("{error:#}"))),
        }
    }

    fn redis_open_delete_key(&self) {
        let key = self
            .state
            .with(|s| s.redis.selected_key.as_ref().map(|k| k.name.clone()));
        if let Some(key) = key {
            self.state.revise(|s| {
                s.redis.delete_target = key;
                s.dialog_error.clear();
            });
            self.open_dialog(Dialog::RedisDeleteKey);
        }
    }

    fn redis_confirm_delete_key(&self) {
        let key = self.state.with(|s| s.redis.delete_target.clone());
        if key.is_empty() {
            return;
        }
        self.close_dialog();
        self.redis_write(WriteMode::Edit, WriteOp::DeleteKey { key }, None);
    }

    fn redis_open_db(&self, db: u8) {
        self.state.revise(|s| {
            s.redis.view = View::Keys;
            s.redis.db = db;
            s.redis.keys_pending = None;
            s.redis.key_pending = None;
            s.redis.write_pending = None;
            s.redis.after_write_key = None;
            s.redis.selected_key = None;
            s.redis.keys.clear();
            s.redis.keys_loaded = false;
            s.redis.cursor.clear();
        });
        self.redis_scan(false, true);
    }
}

pub(crate) fn event(
    ui: &Ui,
    serial: u64,
    result: std::result::Result<Reply, String>,
) -> Option<std::result::Result<Reply, String>> {
    let matched = ui.state.with(|s| {
        if s.redis.pending == Some(serial) {
            return Some((1u8, None));
        }
        [
            (2u8, &s.redis.keys_pending),
            (3u8, &s.redis.key_pending),
            (4u8, &s.redis.diag_pending),
            (5u8, &s.redis.write_pending),
        ]
        .into_iter()
        .find_map(|(kind, slot)| {
            slot.as_ref()
                .filter(|p| p.serial == serial)
                .map(|p| (kind, Some(p.clone())))
        })
    });
    let Some((kind, pending_data)) = matched else {
        return Some(result);
    };

    let mut follow_scan = false;
    let mut follow_diag = false;
    let mut follow_key: Option<String> = None;

    let matches_instance = |s: &State| {
        pending_data
            .as_ref()
            .is_some_and(|p| p.instance == s.redis.instance)
    };
    let matches_db = |s: &State| {
        pending_data
            .as_ref()
            .is_some_and(|p| p.instance == s.redis.instance && p.db == s.redis.db)
    };

    ui.state.revise(|s| match (kind, result) {
        (1, Ok(Reply::Redis(reading))) => {
            s.redis.pending = None;
            let instances: Vec<Rc<Instance>> = reading.instances.into_iter().map(Rc::new).collect();
            let prev = s.redis.instance.clone();
            let selected = instances
                .iter()
                .find(|i| i.key == s.redis.instance)
                .or(instances.first())
                .cloned();
            s.redis.instance = selected.as_ref().map(|i| i.key.clone()).unwrap_or_default();
            if s.redis.instance != prev {
                s.redis.clear_instance_views();
            }
            s.redis.snapshot = selected;
            s.redis.instances = instances;
            s.redis.docker_error = reading.docker_error;
            if s.redis
                .snapshot
                .as_ref()
                .is_some_and(|i| i.ready_for_data())
            {
                if s.redis.view == View::Keys && !s.redis.keys_loaded {
                    follow_scan = true;
                } else if s.redis.view == View::Diagnostics && s.redis.diagnostics.is_none() {
                    follow_diag = true;
                }
            }
        }
        (1, Err(error)) => {
            s.redis.pending = None;
            s.redis.error = i18n::translate(&error);
        }
        (2, Ok(Reply::RedisKeys(page))) => {
            s.redis.keys_pending = None;
            if matches_db(s) && page.db == s.redis.db {
                if s.redis.scan_append {
                    for key in page.keys {
                        if !s
                            .redis
                            .keys
                            .iter()
                            .any(|existing| existing.name == key.name)
                        {
                            s.redis.keys.push(key);
                        }
                    }
                } else {
                    s.redis.keys = page.keys;
                }
                s.redis.cursor = page.cursor;
                s.redis.keys_truncated = page.truncated;
                s.redis.keys_loaded = true;
            }
        }
        (2, Err(error)) => {
            s.redis.keys_pending = None;
            if matches_db(s) {
                s.redis.error = i18n::translate(&error);
            }
        }
        (3, Ok(Reply::RedisKeyValue(kv))) => {
            s.redis.key_pending = None;
            if matches_db(s) {
                s.redis.edit_value = kv.string_value.clone().unwrap_or_default();
                s.redis.edit_ttl = if kv.ttl >= 0 {
                    kv.ttl.to_string()
                } else {
                    String::new()
                };
                s.redis.pretty_json = false;
                s.redis.selected_key = Some(Rc::new(kv));
            }
        }
        (3, Err(error)) => {
            s.redis.key_pending = None;
            if matches_db(s) {
                s.redis.error = i18n::translate(&error);
            }
        }
        (4, Ok(Reply::RedisDiagnostics(diag))) => {
            s.redis.diag_pending = None;
            if matches_instance(s) {
                s.redis.diagnostics = Some(Rc::new(diag));
            }
        }
        (4, Err(error)) => {
            s.redis.diag_pending = None;
            if matches_instance(s) {
                s.redis.error = i18n::translate(&error);
            }
        }
        (5, Ok(Reply::RedisWritten)) => {
            s.redis.write_pending = None;
            let key = s.redis.after_write_key.take();
            if matches_db(s) {
                if key.is_none() {
                    s.redis.selected_key = None;
                    s.redis.edit_value.clear();
                    s.redis.edit_ttl.clear();
                    s.redis.info = i18n::translate("Key deleted");
                } else {
                    s.redis.info = i18n::translate("Saved");
                }
                // Cancel any stale in-flight scan so the post-write refresh is never dropped.
                s.redis.keys_pending = None;
                follow_key = key;
                follow_scan = true;
            }
        }
        (5, Err(error)) => {
            s.redis.write_pending = None;
            s.redis.after_write_key = None;
            if matches_db(s) {
                s.redis.error = i18n::translate(&error);
            }
        }
        (1, _) => {
            s.redis.pending = None;
            s.redis.error = i18n::translate("Invalid Redis response");
        }
        (2, _) => {
            s.redis.keys_pending = None;
            s.redis.error = i18n::translate("Invalid Redis key list response");
        }
        (3, _) => {
            s.redis.key_pending = None;
            s.redis.error = i18n::translate("Invalid Redis key value response");
        }
        (4, _) => {
            s.redis.diag_pending = None;
            s.redis.error = i18n::translate("Invalid Redis diagnostics response");
        }
        _ => {
            s.redis.write_pending = None;
            s.redis.error = i18n::translate("Invalid Redis response");
        }
    });

    if follow_scan {
        ui.redis_scan(false, false);
    }
    if let Some(key) = follow_key {
        ui.redis_select_key(key, false);
    }
    if follow_diag {
        ui.redis_diagnostics();
    }
    None
}

fn navigation(ui: Ui) -> impl Widget {
    let state = ui.state;
    let items = [
        ("Overview", Icon::Grid, View::Overview),
        ("Keys", Icon::Key, View::Keys),
        ("Clients & slowlog", Icon::Logs, View::Diagnostics),
    ]
    .into_iter()
    .map({
        let ui = ui.clone();
        move |(label, icon, view)| {
            let ui = ui.clone();
            text_action(label, icon)
                .class("button with-icon nginx-nav-item")
                .pressed(state.map(move |s| s.redis.view == view))
                .on_press(move || {
                    state.revise(|s| s.redis.view = view);
                    match view {
                        View::Keys => {
                            if ui.state.with(|s| !s.redis.keys_loaded) {
                                ui.redis_scan(false, true);
                            }
                        }
                        View::Diagnostics => {
                            if ui.state.with(|s| s.redis.diagnostics.is_none()) {
                                ui.redis_diagnostics();
                            }
                        }
                        View::Overview => {}
                    }
                })
        }
    })
    .collect::<Vec<_>>();

    col().class("nginx-nav").fill((
        col().class("nginx-views").fill(items),
        when(state.map(|s| s.redis.instances.len() > 1), move || {
            let ui = ui.clone();
            col().class("nginx-instances").fill((
                text()
                    .class("nginx-nav-label")
                    .content(i18n::bind("Instances")),
                each(state.map(|s| {
                    s.redis
                        .instances
                        .iter()
                        .map(|i| {
                            (
                                i.key.clone(),
                                i.title.clone(),
                                i.docker.is_some(),
                                i.running,
                            )
                        })
                        .collect::<Vec<_>>()
                }))
                .key(|i| format!("{}\n{}\n{}", i.0, i.1, i.3))
                .children(move |item| {
                    let (key, title, docker, running) = item.value.get();
                    let chosen = key.clone();
                    let switch_ui = ui.clone();
                    text_action(title, if docker { Icon::Docker } else { Icon::Redis })
                        .class(if running {
                            "button with-icon nginx-nav-item nginx-instance"
                        } else {
                            "button with-icon nginx-nav-item nginx-instance stopped"
                        })
                        .pressed(state.map(move |s| s.redis.instance == key))
                        .on_press(move || {
                            let view = state.revise(|s| {
                                if s.redis.instance == chosen {
                                    return None;
                                }
                                s.redis.snapshot =
                                    s.redis.instances.iter().find(|i| i.key == chosen).cloned();
                                s.redis.instance = chosen.clone();
                                s.redis.clear_instance_views();
                                Some(s.redis.view)
                            });
                            if let Some(View::Keys) = view {
                                switch_ui.redis_scan(false, true);
                            } else if let Some(View::Diagnostics) = view {
                                switch_ui.redis_diagnostics();
                            }
                        })
                }),
            ))
        }),
    ))
}

fn control_button(
    ui: &Ui,
    label: &'static str,
    class: &'static str,
    control: Control,
    shown: fn(&State) -> bool,
) -> impl Widget + use<> {
    let state = ui.state;
    let ui = ui.clone();
    sapid::button()
        .class(state.map(move |s| {
            if shown(s) {
                class.to_owned()
            } else {
                format!("{class} hidden")
            }
        }))
        .label(i18n::bind(label))
        .enabled(state.map(can_change_service))
        .on_press(move || ui.redis_control(control))
}

fn auth_banner(ui: Ui) -> impl Widget {
    let state = ui.state;
    let unlock = ui.clone();
    let unlock_enter = ui.clone();
    when(state.map(needs_auth), move || {
        let unlock = unlock.clone();
        let unlock_enter = unlock_enter.clone();
        col().class("redis-auth-card").fill((
            text()
                .class("section-title")
                .content(i18n::bind("Redis authentication")),
            text().class("hint").content(state.map(|s| {
                match s.redis.snapshot.as_ref().map(|i| &i.auth_status) {
                    Some(AuthStatus::Failed(msg)) if !msg.is_empty() => msg.clone(),
                    _ => i18n::translate(
                        "This Redis instance requires a password. Credentials stay only in memory for this session and are never saved or placed on command lines.",
                    ),
                }
            })),
            row().class("redis-auth-fields").fill((
                col().class("field").fill((
                    text()
                        .class("field-label")
                        .content(i18n::bind("ACL username (optional)")),
                    input()
                        .name(i18n::bind("ACL username (optional)"))
                        .value(state.map(|s| s.redis.auth_username.clone()))
                        .on(value_changed, move |event| {
                            if let Event::ValueChanged { value, .. } = event {
                                state.revise(|s| s.redis.auth_username = value);
                            }
                        }),
                )),
                col().class("field").fill((
                    text()
                        .class("field-label")
                        .content(i18n::bind("Password")),
                    input()
                        .purpose(Purpose::Password)
                        .name(i18n::bind("Password"))
                        .value(state.map(|s| s.redis.auth_password.to_string()))
                        .on(value_changed, move |event| {
                            if let Event::ValueChanged { value, .. } = event {
                                state.revise(|s| s.redis.auth_password = Zeroizing::new(value));
                            }
                        })
                        .on(commit, move |_| unlock_enter.redis_unlock()),
                )),
                sapid::button()
                    .class("button primary redis-auth-submit")
                    .label(i18n::bind("Unlock"))
                    .enabled(state.map(|s| ready(s) && !s.redis.auth_password.is_empty()))
                    .on_press(move || unlock.redis_unlock()),
            )),
        ))
    })
}

fn stat(state: Cage<State>, label: &'static str, value: fn(&State) -> String) -> impl Widget {
    col().class("nginx-stat").fill((
        text().class("nginx-stat-label").content(i18n::bind(label)),
        text().class("nginx-stat-value").content(state.map(value)),
    ))
}

fn overview(ui: Ui) -> impl Widget {
    let state = ui.state;
    let refresh = ui.clone();
    let clear_auth = ui.clone();
    let open_db = ui.clone();
    let running: fn(&State) -> bool = |s| s.redis.snapshot.as_ref().is_some_and(|i| i.running);

    col().class("nginx-overview").fill((
        col().class("nginx-header").fill((
            row().class("nginx-title-row").fill((
                text().class("nginx-title").content(i18n::bind("Overview")),
                text()
                    .class(state.map(|s| status(s).1.to_owned()))
                    .content(i18n::bind(state.map(|s| status(s).0.to_owned()))),
                icon_action("Refresh Redis", Icon::Refresh)
                    .class("icon-button")
                    .enabled(state.map(ready))
                    .on_press(move || refresh.redis_refresh()),
            )),
            text()
                .class("nginx-details subtle")
                .content(state.map(details)),
            text()
                .class(state.map(|s| {
                    if origin(s).is_empty() {
                        "hidden"
                    } else {
                        "nginx-details subtle"
                    }
                    .to_owned()
                }))
                .selectable(true)
                .content(state.map(origin)),
            row().class("nginx-actions").fill((
                control_button(&ui, "Restart…", "button", Control::Restart, running),
                control_button(&ui, "Start…", "button primary", Control::Start, |s| {
                    s.redis.snapshot.as_ref().is_some_and(|i| !i.running)
                }),
                control_button(&ui, "Stop…", "button danger", Control::Stop, running),
                when(
                    state.map(|s| s.redis.auth.contains_key(&s.redis.instance)),
                    move || {
                        let clear_auth = clear_auth.clone();
                        btn("Lock", move || clear_auth.redis_clear_auth()).class("button")
                    },
                ),
            )),
        )),
        when(state.map(|s| !s.redis.docker_error.is_empty()), move || {
            text().class("nginx-info").content(state.map(|s| {
                i18n::translate(&format!(
                    "Docker containers could not be checked: {}",
                    s.redis.docker_error
                ))
            }))
        }),
        auth_banner(ui.clone()),
        when(
            state.map(|s| {
                matches!(
                    s.redis.snapshot.as_ref().map(|i| &i.auth_status),
                    Some(AuthStatus::Unreachable(_))
                ) && s.redis.snapshot.as_ref().is_some_and(|i| i.running)
            }),
            move || {
                text().class("nginx-error").content(state.map(|s| {
                    match s.redis.snapshot.as_ref().map(|i| &i.auth_status) {
                        Some(AuthStatus::Unreachable(msg)) => i18n::translate(msg),
                        _ => String::new(),
                    }
                }))
            },
        ),
        when(
            state.map(|s| {
                s.redis
                    .snapshot
                    .as_ref()
                    .is_some_and(|i| i.overview.is_some())
            }),
            move || {
                let open_db = open_db.clone();
                col().class("redis-overview-body").fill((
                    row().class("nginx-stats").fill((
                        stat(state, "Total keys", |s| {
                            s.redis
                                .snapshot
                                .as_ref()
                                .and_then(|i| i.overview.as_ref())
                                .map(|o| o.total_keys.to_string())
                                .unwrap_or_else(|| "—".into())
                        }),
                        stat(state, "Memory", |s| {
                            s.redis
                                .snapshot
                                .as_ref()
                                .and_then(|i| i.overview.as_ref())
                                .map(|o| {
                                    let used = formatting::bytes(o.used_memory);
                                    if o.maxmemory > 0 {
                                        format!("{used} / {}", formatting::bytes(o.maxmemory))
                                    } else {
                                        used
                                    }
                                })
                                .unwrap_or_else(|| "—".into())
                        }),
                        stat(state, "Connected clients", |s| {
                            s.redis
                                .snapshot
                                .as_ref()
                                .and_then(|i| i.overview.as_ref())
                                .map(|o| {
                                    if o.blocked_clients > 0 {
                                        i18n::translate(&format!(
                                            "{} ({} blocked)",
                                            o.connected_clients, o.blocked_clients
                                        ))
                                    } else {
                                        o.connected_clients.to_string()
                                    }
                                })
                                .unwrap_or_else(|| "—".into())
                        }),
                        stat(state, "Ops / sec", |s| {
                            s.redis
                                .snapshot
                                .as_ref()
                                .and_then(|i| i.overview.as_ref())
                                .map(|o| {
                                    if let Some(rate) = o.hit_rate_percent() {
                                        i18n::translate(&format!(
                                            "{} · {:.1}% hit",
                                            o.instantaneous_ops_per_sec, rate
                                        ))
                                    } else {
                                        o.instantaneous_ops_per_sec.to_string()
                                    }
                                })
                                .unwrap_or_else(|| "—".into())
                        }),
                        stat(state, "Persistence", |s| {
                            s.redis
                                .snapshot
                                .as_ref()
                                .and_then(|i| i.overview.as_ref())
                                .map(|o| {
                                    let aof = i18n::translate(if o.aof_enabled {
                                        "AOF on"
                                    } else {
                                        "AOF off"
                                    });
                                    if o.rdb_last_bgsave_status.is_empty() {
                                        aof
                                    } else {
                                        format!("RDB {} · {aof}", o.rdb_last_bgsave_status)
                                    }
                                })
                                .unwrap_or_else(|| "—".into())
                        }),
                        stat(state, "Mode & role", |s| {
                            s.redis
                                .snapshot
                                .as_ref()
                                .and_then(|i| i.overview.as_ref())
                                .map(|o| {
                                    let mut parts = Vec::new();
                                    if !o.mode.is_empty() {
                                        parts.push(o.mode.as_str());
                                    }
                                    if !o.role.is_empty() {
                                        parts.push(o.role.as_str());
                                    }
                                    if parts.is_empty() {
                                        "—".into()
                                    } else {
                                        parts.join(" · ")
                                    }
                                })
                                .unwrap_or_else(|| "—".into())
                        }),
                    )),
                    col().class("nginx-section").fill((
                        text()
                            .class("section-title")
                            .content(i18n::bind("Keyspace")),
                        when(
                            state.map(|s| {
                                s.redis
                                    .snapshot
                                    .as_ref()
                                    .and_then(|i| i.overview.as_ref())
                                    .is_some_and(|o| o.databases.is_empty())
                            }),
                            || {
                                text()
                                    .class("subtle")
                                    .content(i18n::bind("No keys in any database."))
                            },
                        ),
                        col().class("nginx-sites").fill(
                            each(state.map(|s| {
                                s.redis
                                    .snapshot
                                    .as_ref()
                                    .and_then(|i| i.overview.as_ref())
                                    .map(|o| o.databases.clone())
                                    .unwrap_or_default()
                            }))
                            .key(|db| format!("{}:{}:{}", db.index, db.keys, db.expires))
                            .children(move |item| {
                                let db = item.value.get();
                                let open_db = open_db.clone();
                                let detail = match db.avg_ttl_ms {
                                    Some(ms) if ms > 0 => i18n::translate(&format!(
                                        "{} keys · {} expiring · avg TTL {}",
                                        db.keys,
                                        db.expires,
                                        format_ttl((ms / 1000) as i64)
                                    )),
                                    _ => i18n::translate(&format!(
                                        "{} keys · {} expiring",
                                        db.keys, db.expires
                                    )),
                                };
                                row().class("nginx-site-row").fill((
                                    sapid::button()
                                        .class("nginx-site-open")
                                        .name(i18n::bind(format!("Open db{}", db.index)))
                                        .fill((
                                            text()
                                                .class("nginx-site-name")
                                                .content(format!("db{}", db.index)),
                                            text().class("nginx-site-detail").content(detail),
                                        ))
                                        .on_press(move || open_db.redis_open_db(db.index)),
                                    text().class("nginx-badge").content(format!("{}", db.keys)),
                                ))
                            }),
                        ),
                    )),
                ))
            },
        ),
    ))
}

fn keys_view(ui: Ui) -> impl Widget {
    let state = ui.state;
    let scan_btn = ui.clone();
    let scan_enter = ui.clone();
    let load_more = ui.clone();
    let new_key_ui = ui.clone();
    let select_row_ui = ui.clone();
    let db_pick_ui = ui.clone();

    col().class("redis-keys-view").fill((
        auth_banner(ui.clone()),
        row().class("redis-keys-toolbar").fill((
            row().class("redis-db-strip").fill(
                each(state.map(|s| {
                    let mut dbs: Vec<u8> = s
                        .redis
                        .snapshot
                        .as_ref()
                        .and_then(|i| i.overview.as_ref())
                        .map(|o| o.databases.iter().map(|d| d.index).collect())
                        .unwrap_or_default();
                    if !dbs.contains(&0) {
                        dbs.insert(0, 0);
                    }
                    if !dbs.contains(&s.redis.db) {
                        dbs.push(s.redis.db);
                    }
                    dbs.sort_unstable();
                    dbs.dedup();
                    dbs
                }))
                .key(|d| d.to_string())
                .children(move |item| {
                    let db_index = item.value.get();
                    let db_pick_ui = db_pick_ui.clone();
                    sapid::button()
                        .class("nginx-segment")
                        .label(format!("db{db_index}"))
                        .pressed(state.map(move |s| s.redis.db == db_index))
                        .enabled(state.map(|s| data_ready(s) && !busy_data(s)))
                        .on_press(move || db_pick_ui.redis_open_db(db_index))
                }),
            ),
            input()
                .class("redis-pattern-input")
                .name(i18n::bind("Key pattern"))
                .value(state.map(|s| s.redis.pattern.clone()))
                .on(value_changed, move |event| {
                    if let Event::ValueChanged { value, .. } = event {
                        state.revise(|s| s.redis.pattern = value);
                    }
                })
                .on(commit, move |_| {
                    if scan_enter.state.with(|s| !busy_data(s)) {
                        scan_enter.redis_scan(false, true);
                    }
                }),
            btn("Scan", move || scan_btn.redis_scan(false, true))
                .class("button")
                .enabled(state.map(|s| data_ready(s) && !busy_data(s))),
            btn("New key…", move || new_key_ui.redis_open_new_key())
                .class("button primary")
                .enabled(state.map(|s| data_ready(s) && !busy_data(s))),
        )),
        when(state.map(|s| !data_ready(s) && !needs_auth(s)), || {
            text()
                .class("subtle")
                .content(i18n::bind("Start or connect to Redis to browse keys."))
        }),
        when(state.map(data_ready), move || {
            let select_row_ui = select_row_ui.clone();
            let load_more = load_more.clone();
            let detail_ui = ui.clone();
            row().class("redis-keys-split").fill((
                col().class("redis-keys-list-pane").fill((
                    when(
                        state.map(|s| s.redis.keys_pending.is_some() && !s.redis.keys_loaded),
                        || text().class("subtle").content(i18n::bind("Scanning keys…")),
                    ),
                    when(
                        state.map(|s| {
                            s.redis.keys_loaded
                                && s.redis.keys.is_empty()
                                && s.redis.keys_pending.is_none()
                        }),
                        move || {
                            text().class("subtle").content(i18n::bind(state.map(|s| {
                                if !s.redis.cursor.is_empty() && s.redis.cursor != "0" {
                                    "No matches in this batch yet. Load more to keep scanning."
                                } else {
                                    "No matching keys in this database."
                                }
                                .to_owned()
                            })))
                        },
                    ),
                    col().class("redis-keys-list").fill(
                        each(state.map(|s| s.redis.keys.clone()))
                            .key(|k| format!("{}\n{}\n{}\n{}", k.name, k.kind, k.ttl, k.binary))
                            .children(move |item| {
                                let k = item.value.get();
                                let key_name = k.name.clone();
                                let select_name = k.name.clone();
                                let select_row_ui = select_row_ui.clone();
                                let is_binary_name = k.binary;
                                let meta = if is_binary_name {
                                    i18n::translate("Binary key name (read-only)")
                                } else {
                                    match k.size {
                                        Some(bytes) => {
                                            format!(
                                                "{} · {}",
                                                format_ttl(k.ttl),
                                                formatting::bytes(bytes)
                                            )
                                        }
                                        None => format_ttl(k.ttl),
                                    }
                                };
                                row()
                                    .class(state.map(move |s| {
                                        if s.redis
                                            .selected_key
                                            .as_ref()
                                            .is_some_and(|sel| sel.name == key_name)
                                        {
                                            "redis-key-row selected"
                                        } else {
                                            "redis-key-row"
                                        }
                                        .to_owned()
                                    }))
                                    .fill((
                                        sapid::button()
                                            .class("redis-key-open")
                                            .name(k.name.clone())
                                            .enabled(!is_binary_name)
                                            .fill((
                                                text()
                                                    .class("redis-key-name")
                                                    .content(k.name.clone()),
                                                text().class("redis-key-meta subtle").content(meta),
                                            ))
                                            .on_press(move || {
                                                select_row_ui
                                                    .redis_select_key(select_name.clone(), true)
                                            }),
                                        text()
                                            .class(kind_badge_class(&k.kind))
                                            .content(k.kind.clone()),
                                    ))
                            }),
                    ),
                    when(
                        state.map(|s| !s.redis.cursor.is_empty() && s.redis.cursor != "0"),
                        move || {
                            let load_more = load_more.clone();
                            btn("Load more keys", move || load_more.redis_scan(true, true))
                                .class("button redis-load-more")
                                .enabled(state.map(|s| data_ready(s) && !busy_data(s)))
                        },
                    ),
                )),
                key_inspector(detail_ui),
            ))
        }),
    ))
}

fn key_inspector(ui: Ui) -> impl Widget {
    let state = ui.state;
    let save_str = ui.clone();
    let set_ttl = ui.clone();
    let persist_ttl = ui.clone();
    let delete_btn = ui.clone();
    let refresh_key = ui.clone();
    let add_item = ui.clone();
    let remove_item_ui = ui.clone();

    col().class("redis-key-detail").fill((
        when(
            state.map(|s| s.redis.selected_key.is_none() && s.redis.key_pending.is_none()),
            || {
                text()
                    .class("subtle")
                    .content(i18n::bind("Select a key to inspect or edit its value."))
            },
        ),
        when(
            state.map(|s| s.redis.key_pending.is_some() && s.redis.selected_key.is_none()),
            || text().class("subtle").content(i18n::bind("Loading key…")),
        ),
        when(state.map(|s| s.redis.selected_key.is_some()), move || {
            let save_str = save_str.clone();
            let set_ttl = set_ttl.clone();
            let persist_ttl = persist_ttl.clone();
            let delete_btn = delete_btn.clone();
            let refresh_key = refresh_key.clone();
            let add_item = add_item.clone();
            let remove_item_ui = remove_item_ui.clone();

            col().class("redis-detail-inner").fill((
                row().class("redis-detail-head").fill((
                    col().class("redis-detail-title").fill((
                        text()
                            .class("redis-detail-name")
                            .selectable(true)
                            .content(state.map(|s| {
                                s.redis
                                    .selected_key
                                    .as_ref()
                                    .map(|k| k.name.clone())
                                    .unwrap_or_default()
                            })),
                        text()
                            .class("subtle redis-detail-meta")
                            .content(state.map(|s| {
                                let Some(k) = &s.redis.selected_key else {
                                    return String::new();
                                };
                                let mut parts = vec![k.kind.clone()];
                                if !k.encoding.is_empty() {
                                    parts.push(k.encoding.clone());
                                }
                                if let Some(len) = k.length {
                                    if k.kind == "string" {
                                        parts.push(i18n::translate(&format!("{len} bytes")));
                                    } else {
                                        parts.push(i18n::translate(&format!("{len} items")));
                                    }
                                }
                                if let Some(mem) = k.memory_bytes {
                                    parts.push(formatting::bytes(mem));
                                }
                                parts.push(format!("TTL: {}", format_ttl(k.ttl)));
                                parts.join(" · ")
                            })),
                    )),
                    icon_action("Refresh key", Icon::Refresh)
                        .class("icon-button")
                        .enabled(state.map(|s| !busy_data(s)))
                        .on_press(move || {
                            if let Some(name) = refresh_key
                                .state
                                .with(|s| s.redis.selected_key.as_ref().map(|k| k.name.clone()))
                            {
                                refresh_key.redis_select_key(name, true);
                            }
                        }),
                    btn("Delete key…", move || delete_btn.redis_open_delete_key())
                        .class("button danger")
                        .enabled(state.map(|s| !busy_data(s))),
                )),
                row().class("redis-ttl-bar").fill((
                    text()
                        .class("field-label")
                        .content(i18n::bind("TTL (seconds)")),
                    input()
                        .class("redis-ttl-input")
                        .name(i18n::bind("TTL (seconds)"))
                        .value(state.map(|s| s.redis.edit_ttl.clone()))
                        .on(value_changed, move |event| {
                            if let Event::ValueChanged { value, .. } = event {
                                state.revise(|s| s.redis.edit_ttl = value);
                            }
                        }),
                    btn("Set TTL", move || set_ttl.redis_apply_ttl(false))
                        .class("button")
                        .enabled(state.map(|s| !busy_data(s))),
                    btn("Persist", move || persist_ttl.redis_apply_ttl(true))
                        .class("button")
                        .enabled(state.map(|s| {
                            !busy_data(s)
                                && s.redis.selected_key.as_ref().is_some_and(|k| k.ttl >= 0)
                        })),
                )),
                when(
                    state.map(|s| {
                        s.redis
                            .selected_key
                            .as_ref()
                            .is_some_and(|k| k.truncated || k.binary)
                    }),
                    move || {
                        text()
                            .class("nginx-info")
                            .content(i18n::bind(state.map(|s| {
                                match &s.redis.selected_key {
                                    Some(k) if k.binary => {
                                        "Binary value shown as hex preview. Editing is disabled."
                                    }
                                    Some(k) if k.truncated => {
                                        "Large value is truncated for preview. Editing is disabled."
                                    }
                                    _ => "",
                                }
                                .to_owned()
                            })))
                    },
                ),
                // String editor
                when(
                    state.map(|s| {
                        s.redis
                            .selected_key
                            .as_ref()
                            .is_some_and(|k| k.kind == "string")
                    }),
                    move || {
                        let save_str = save_str.clone();
                        col().class("redis-string-editor").fill((
                            row().class("redis-string-tools").fill((
                                btn("Format JSON", move || {
                                    state.revise(|s| {
                                        if let Some(pretty) = maybe_pretty_json(&s.redis.edit_value)
                                        {
                                            s.redis.edit_value = pretty;
                                            s.redis.pretty_json = true;
                                        }
                                    })
                                })
                                .class("button")
                                .enabled(
                                    state.map(|s| maybe_pretty_json(&s.redis.edit_value).is_some()),
                                ),
                                btn("Save value", move || save_str.redis_save_string())
                                    .class("button primary")
                                    .enabled(state.map(|s| {
                                        !busy_data(s)
                                            && s.redis
                                                .selected_key
                                                .as_ref()
                                                .is_some_and(|k| !k.binary && !k.truncated)
                                    })),
                            )),
                            input()
                                .multiline(true)
                                .class("redis-value-area")
                                .name(i18n::bind("Value"))
                                .enabled(state.map(|s| {
                                    s.redis
                                        .selected_key
                                        .as_ref()
                                        .is_some_and(|k| !k.binary && !k.truncated)
                                }))
                                .value(state.map(|s| s.redis.edit_value.clone()))
                                .on(value_changed, move |event| {
                                    if let Event::ValueChanged { value, .. } = event {
                                        state.revise(|s| s.redis.edit_value = value);
                                    }
                                }),
                        ))
                    },
                ),
                // Collection entries (hash / list / set / zset / stream)
                when(
                    state.map(|s| {
                        s.redis
                            .selected_key
                            .as_ref()
                            .is_some_and(|k| k.kind != "string")
                    }),
                    move || {
                        let add_item = add_item.clone();
                        let remove_item_ui = remove_item_ui.clone();
                        col().class("redis-collection").fill((
                            when(
                                state.map(|s| {
                                    s.redis.selected_key.as_ref().is_some_and(|k| {
                                        matches!(k.kind.as_str(), "hash" | "list" | "set" | "zset")
                                    })
                                }),
                                move || {
                                    let add_item = add_item.clone();
                                    row().class("redis-add-row").fill((
                                        when(
                                            state.map(|s| {
                                                s.redis
                                                    .selected_key
                                                    .as_ref()
                                                    .is_some_and(|k| k.kind == "hash")
                                            }),
                                            move || {
                                                input()
                                                    .class("redis-add-key")
                                                    .name(i18n::bind("Field"))
                                                    .value(state.map(|s| s.redis.add_field.clone()))
                                                    .on(value_changed, move |event| {
                                                        if let Event::ValueChanged {
                                                            value, ..
                                                        } = event
                                                        {
                                                            state.revise(|s| {
                                                                s.redis.add_field = value
                                                            });
                                                        }
                                                    })
                                            },
                                        ),
                                        when(
                                            state.map(|s| {
                                                s.redis
                                                    .selected_key
                                                    .as_ref()
                                                    .is_some_and(|k| k.kind == "zset")
                                            }),
                                            move || {
                                                input()
                                                    .class("redis-add-score")
                                                    .name(i18n::bind("Score"))
                                                    .value(state.map(|s| s.redis.add_score.clone()))
                                                    .on(value_changed, move |event| {
                                                        if let Event::ValueChanged {
                                                            value, ..
                                                        } = event
                                                        {
                                                            state.revise(|s| {
                                                                s.redis.add_score = value
                                                            });
                                                        }
                                                    })
                                            },
                                        ),
                                        input()
                                            .class("redis-add-val")
                                            .name(i18n::bind("Value"))
                                            .value(state.map(|s| s.redis.add_value.clone()))
                                            .on(value_changed, move |event| {
                                                if let Event::ValueChanged { value, .. } = event {
                                                    state.revise(|s| s.redis.add_value = value);
                                                }
                                            }),
                                        btn("Add", move || add_item.redis_add_collection_item())
                                            .class("button primary")
                                            .enabled(state.map(|s| {
                                                !busy_data(s)
                                                    && s.redis
                                                        .selected_key
                                                        .as_ref()
                                                        .is_some_and(|k| !k.binary && !k.truncated)
                                            })),
                                    ))
                                },
                            ),
                            col().class("redis-entries").fill(
                                each(state.map(|s| {
                                    let Some(k) = &s.redis.selected_key else {
                                        return Vec::new();
                                    };
                                    k.entries
                                        .iter()
                                        .enumerate()
                                        .map(|(idx, e)| {
                                            (
                                                idx,
                                                k.name.clone(),
                                                k.kind.clone(),
                                                e.label.clone(),
                                                e.value.clone(),
                                            )
                                        })
                                        .collect()
                                }))
                                .key(|(idx, k, _, l, v)| format!("{idx}:{k}:{l}:{v}"))
                                .children(move |item| {
                                    let (_, key_name, kind, label, val) = item.value.get();
                                    let remove_ui = remove_item_ui.clone();
                                    let removable =
                                        matches!(kind.as_str(), "hash" | "set" | "zset");
                                    let del_key = key_name.clone();
                                    let del_kind = kind.clone();
                                    let del_label = label.clone();
                                    let del_val = val.clone();
                                    row().class("redis-entry-row").fill((
                                        when(!label.is_empty(), move || {
                                            text()
                                                .class("redis-entry-label")
                                                .selectable(true)
                                                .content(label.clone())
                                        }),
                                        text()
                                            .class("redis-entry-value")
                                            .selectable(true)
                                            .content(val.clone()),
                                        when(removable, move || {
                                            let remove_ui = remove_ui.clone();
                                            let del_key = del_key.clone();
                                            let del_kind = del_kind.clone();
                                            let del_label = del_label.clone();
                                            let del_val = del_val.clone();
                                            icon_action("Remove entry", Icon::Clear)
                                                .class("icon-button")
                                                .enabled(state.map(|s| {
                                                    !busy_data(s)
                                                        && s.redis
                                                            .selected_key
                                                            .as_ref()
                                                            .is_some_and(|k| {
                                                                !k.binary && !k.truncated
                                                            })
                                                }))
                                                .on_press(move || {
                                                    let op = match del_kind.as_str() {
                                                        "hash" => WriteOp::DeleteHashField {
                                                            key: del_key.clone(),
                                                            field: del_label.clone(),
                                                        },
                                                        "set" => WriteOp::RemoveSetMember {
                                                            key: del_key.clone(),
                                                            member: del_val.clone(),
                                                        },
                                                        "zset" => WriteOp::RemoveZSetMember {
                                                            key: del_key.clone(),
                                                            member: del_val.clone(),
                                                        },
                                                        _ => return,
                                                    };
                                                    remove_ui.redis_write(
                                                        WriteMode::Edit,
                                                        op,
                                                        Some(del_key.clone()),
                                                    );
                                                })
                                        }),
                                    ))
                                }),
                            ),
                        ))
                    },
                ),
            ))
        }),
    ))
}

fn diagnostics_view(ui: Ui) -> impl Widget {
    let state = ui.state;
    let refresh = ui.clone();

    col().class("redis-diag-view").fill((
        row().class("nginx-title-row").fill((
            text()
                .class("nginx-title")
                .content(i18n::bind("Clients & slowlog")),
            icon_action("Refresh diagnostics", Icon::Refresh)
                .class("icon-button")
                .enabled(state.map(|s| data_ready(s) && s.redis.diag_pending.is_none()))
                .on_press(move || refresh.redis_diagnostics()),
        )),
        auth_banner(ui.clone()),
        when(
            state.map(|s| s.redis.diag_pending.is_some() && s.redis.diagnostics.is_none()),
            || text().class("subtle").content(i18n::bind("Loading…")),
        ),
        when(state.map(|s| s.redis.diagnostics.is_some()), move || {
            col().class("redis-diag-sections").fill((
                col().class("nginx-section").fill((
                    row().class("nginx-title-row").fill((
                        text()
                            .class("section-title")
                            .content(i18n::bind("Connected clients")),
                        input()
                            .class("redis-client-filter")
                            .name(i18n::bind("Filter clients"))
                            .value(state.map(|s| s.redis.client_filter.clone()))
                            .on(value_changed, move |event| {
                                if let Event::ValueChanged { value, .. } = event {
                                    state.revise(|s| s.redis.client_filter = value);
                                }
                            }),
                    )),
                    when(
                        state.map(|s| {
                            s.redis
                                .diagnostics
                                .as_ref()
                                .is_some_and(|d| d.clients_truncated)
                        }),
                        || {
                            text()
                                .class("hint")
                                .content(i18n::bind("Showing the first 200 connected clients."))
                        },
                    ),
                    col().class("redis-entries").fill(
                        each(state.map(|s| {
                            let filter = s.redis.client_filter.to_lowercase();
                            s.redis
                                .diagnostics
                                .as_ref()
                                .map(|d| {
                                    d.clients
                                        .iter()
                                        .filter(|c| {
                                            filter.is_empty()
                                                || [&c.addr, &c.name, &c.cmd, &c.id]
                                                    .iter()
                                                    .any(|f| f.to_lowercase().contains(&filter))
                                        })
                                        .cloned()
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default()
                        }))
                        .key(|c| format!("{}:{}:{}", c.id, c.addr, c.idle))
                        .children(|item| {
                            let c = item.value.get();
                            let title = if c.name.is_empty() {
                                c.addr.clone()
                            } else {
                                format!("{} ({})", c.addr, c.name)
                            };
                            let meta = format!(
                                "db{} · cmd={} · age {} s · idle {} s",
                                c.db, c.cmd, c.age, c.idle
                            );
                            row().class("redis-entry-row").fill((
                                text()
                                    .class("redis-entry-label")
                                    .selectable(true)
                                    .content(title),
                                text().class("redis-entry-value subtle").content(meta),
                            ))
                        }),
                    ),
                )),
                col().class("nginx-section").fill((
                    text().class("section-title").content(i18n::bind("Slowlog")),
                    when(
                        state.map(|s| {
                            s.redis
                                .diagnostics
                                .as_ref()
                                .is_some_and(|d| d.slowlog.is_empty())
                        }),
                        || {
                            text()
                                .class("subtle")
                                .content(i18n::bind("No slow queries recorded."))
                        },
                    ),
                    col().class("redis-entries").fill(
                        each(state.map(|s| {
                            s.redis
                                .diagnostics
                                .as_ref()
                                .map(|d| d.slowlog.clone())
                                .unwrap_or_default()
                        }))
                        .key(|e| format!("{}:{}", e.id, e.timestamp))
                        .children(|item| {
                            let e = item.value.get();
                            let meta = format!(
                                "{} · {}",
                                format_duration_us(e.duration_us),
                                formatting::timestamp(Some(e.timestamp))
                            );
                            row().class("redis-entry-row").fill((
                                text()
                                    .class("redis-entry-value")
                                    .selectable(true)
                                    .content(e.command),
                                text().class("nginx-badge").content(meta),
                            ))
                        }),
                    ),
                )),
            ))
        }),
    ))
}

pub(crate) fn page(ui: Ui) -> impl Widget {
    let state = ui.state;
    let content = col().class("nginx-main").fill((
        text()
            .class(state.map(|s| {
                if s.redis.error.is_empty() && s.redis.notice.is_empty() {
                    "hidden"
                } else {
                    "nginx-error"
                }
                .to_owned()
            }))
            .selectable(true)
            .content(i18n::bind(state.map(|s| {
                if s.redis.error.is_empty() {
                    s.redis.notice.clone()
                } else {
                    s.redis.error.clone()
                }
            }))),
        text()
            .class(state.map(|s| {
                if s.redis.info.is_empty() {
                    "hidden"
                } else {
                    "nginx-info"
                }
                .to_owned()
            }))
            .content(i18n::bind(state.map(|s| s.redis.info.clone()))),
        when(
            state.map(|s| s.redis.snapshot.is_none() && s.redis.pending.is_some()),
            || text().class("subtle").content(i18n::bind("Loading…")),
        ),
        when(state.map(|s| s.redis.view == View::Overview), {
            let ui = ui.clone();
            move || overview(ui.clone())
        }),
        when(state.map(|s| s.redis.view == View::Keys), {
            let ui = ui.clone();
            move || keys_view(ui.clone())
        }),
        when(state.map(|s| s.redis.view == View::Diagnostics), {
            let ui = ui.clone();
            move || diagnostics_view(ui.clone())
        }),
    ));
    row()
        .class("nginx-page redis-page")
        .fill((navigation(ui), content))
}

fn redis_field(
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
                    state.revise(|s| write(s, value));
                }
            }),
    ))
}

fn new_key_kind_button(ui: &Ui, label: &'static str, kind: NewKeyKind) -> impl Widget + use<> {
    let state = ui.state;
    sapid::button()
        .class("nginx-segment")
        .label(i18n::bind(label))
        .pressed(state.map(move |s| s.redis.new_key.kind == kind))
        .on_press(move || {
            state.revise(|s| {
                s.redis.new_key.kind = kind;
                s.dialog_error.clear();
            })
        })
}

pub(crate) fn dialogs(ui: Ui) -> impl Widget {
    let state = ui.state;
    let create_ui = ui.clone();
    let delete_ui = ui.clone();
    fragment((
        when(
            state.map(|s| s.dialog == Some(Dialog::RedisNewKey)),
            move || {
                let ui = create_ui.clone();
                let submit = ui.clone();
                modal(
                    ui.clone(),
                    "New key",
                    col().class("nginx-new-site").fill((
                        row().class("nginx-segments").fill((
                            new_key_kind_button(&ui, "String", NewKeyKind::String),
                            new_key_kind_button(&ui, "Hash", NewKeyKind::Hash),
                            new_key_kind_button(&ui, "List", NewKeyKind::List),
                            new_key_kind_button(&ui, "Set", NewKeyKind::Set),
                            new_key_kind_button(&ui, "Sorted set", NewKeyKind::ZSet),
                        )),
                        redis_field(
                            "Key name",
                            &ui,
                            |s| s.redis.new_key.key.clone(),
                            |s, v| s.redis.new_key.key = v,
                        ),
                        when(state.map(|s| s.redis.new_key.kind == NewKeyKind::Hash), {
                            let ui = ui.clone();
                            move || {
                                redis_field(
                                    "Field",
                                    &ui,
                                    |s| s.redis.new_key.field.clone(),
                                    |s, v| s.redis.new_key.field = v,
                                )
                            }
                        }),
                        when(state.map(|s| s.redis.new_key.kind == NewKeyKind::ZSet), {
                            let ui = ui.clone();
                            move || {
                                redis_field(
                                    "Score",
                                    &ui,
                                    |s| s.redis.new_key.score.clone(),
                                    |s, v| s.redis.new_key.score = v,
                                )
                            }
                        }),
                        col().class("field").fill((
                            text().class("field-label").content(i18n::bind("Value")),
                            input()
                                .multiline(true)
                                .class("redis-new-value")
                                .name(i18n::bind("Value"))
                                .value(state.map(|s| s.redis.new_key.value.clone()))
                                .on(value_changed, move |event| {
                                    if let Event::ValueChanged { value, .. } = event {
                                        state.revise(|s| s.redis.new_key.value = value);
                                    }
                                }),
                        )),
                        when(state.map(|s| s.redis.new_key.kind == NewKeyKind::String), {
                            let ui = ui.clone();
                            move || {
                                redis_field(
                                    "TTL in seconds (optional)",
                                    &ui,
                                    |s| s.redis.new_key.ttl.clone(),
                                    |s, v| s.redis.new_key.ttl = v,
                                )
                            }
                        }),
                    )),
                    sapid::button()
                        .class("button primary")
                        .label(i18n::bind("Create"))
                        .enabled(state.map(|s| data_ready(s) && !busy_data(s)))
                        .on_press(move || submit.redis_submit_new_key()),
                )
            },
        ),
        when(
            state.map(|s| s.dialog == Some(Dialog::RedisDeleteKey)),
            move || {
                let ui = delete_ui.clone();
                let confirm = ui.clone();
                modal(
                    ui.clone(),
                    "Delete key",
                    col()
                        .class("nginx-new-site")
                        .fill(text().content(state.map(|s| {
                            i18n::translate(&format!(
                                "Delete key {} from db{}? This cannot be undone.",
                                s.redis.delete_target, s.redis.db
                            ))
                        }))),
                    sapid::button()
                        .class("button danger")
                        .label(i18n::bind("Delete"))
                        .enabled(state.map(|s| data_ready(s) && !busy_data(s)))
                        .on_press(move || confirm.redis_confirm_delete_key()),
                )
            },
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_ttl_and_duration_naturally() {
        assert_eq!(format_ttl(-1), "No expiry");
        assert_eq!(format_ttl(45), "45 s");
        assert_eq!(format_ttl(125), "2 m 5 s");
        assert_eq!(format_duration_us(420), "420 µs");
        assert_eq!(format_duration_us(12_500), "12.50 ms");
    }

    #[test]
    fn formats_valid_json_only() {
        assert!(maybe_pretty_json(r#"{"a":1,"b":[2,3]}"#).is_some());
        assert!(maybe_pretty_json("plain text").is_none());
        assert!(maybe_pretty_json("{not valid").is_none());
    }

    #[test]
    #[ignore = "requires loopback SSH + Redis fixture"]
    fn live_ssh_ui_e2e_flow() -> anyhow::Result<()> {
        use arcnix_core::remote::PendingConnection;
        use glory::render::memory::Memory;

        let addr: std::net::SocketAddr = std::env::var("ARCNIX_TEST_SSH_ADDR")?.parse()?;
        anyhow::ensure!(addr.ip().is_loopback(), "loopback only");
        let profile = arcnix_core::ServerProfile {
            id: "redis-e2e".into(),
            name: "Redis E2E".into(),
            host: addr.ip().to_string(),
            port: addr.port(),
            username: "arcnix".into(),
            auth: arcnix_core::AuthMethod::Password,
            ..Default::default()
        };
        let pending = PendingConnection::open(&profile)?;
        let fp = pending.fingerprint().to_owned();
        let session = pending.authenticate("arcnix-test", &fp)?;

        // Build full application UI state + worker queue connected to live SSH session.
        let root = std::env::temp_dir().join(format!("arcnix-redis-ui-e2e-{}", new_id()));
        std::fs::create_dir_all(&root)?;
        let store = Rc::new(ConfigStore::at(root.join("config.json")));
        let config = AppConfig {
            servers: vec![profile],
            ..Default::default()
        };
        let (tx, rx) = std::sync::mpsc::channel::<Request>();
        let host_window = WindowApp::new();
        let renderer = glory::App::new(Memory::new);
        let scope = renderer.scope().clone();
        crate::i18n::init(&scope, config.settings.language);
        let state = scope.cage(State::new(
            config,
            root.to_string_lossy().into_owned(),
            String::new(),
        ));
        crate::i18n::desktop::init(&scope, state);
        let (update_controller, _) = updates_ui::start()?;
        let (batch_events, _) = async_channel::bounded(16);
        let ui = Ui {
            player: video_player::Player::new(host_window.renderer().clone(), &scope),
            update_controller,
            transfer_limiter: Default::default(),
            batch_controller: Rc::new(std::cell::RefCell::new(None)),
            batch_events,
            logger: Rc::new(client_logs::Logger::new(store.path())),
            state,
            store,
            writable: scope.cage(true),
            scope,
            shell_focus: renderer.scope().cage(None),
        };
        ui.state.revise(|s| {
            s.selected = Some("redis-e2e".into());
            s.connected = true;
            s.epoch = 1;
            s.worker = Some((1, tx));
            s.workbench.active = server_desktop::Tab::Desktop;
            s.workspace_page = WorkspacePage::Redis;
            s.server_apps.applications.redis = true;
        });

        // Mount real Redis page + dialogs in Glory's in-memory renderer.
        renderer
            .mount(fragment((page(ui.clone()), dialogs(ui.clone()))))
            .map_err(|e| anyhow::anyhow!("Mount failed: {e:?}"))?;

        let visible_text = || -> String {
            let tree = renderer.tree();
            let mut pending: Vec<_> = tree.roots().iter().rev().copied().collect();
            let mut out = Vec::new();
            while let Some(id) = pending.pop() {
                let node = tree.node(id).unwrap();
                if !node.placed || node.suspended || node.descriptor.view().hidden() {
                    continue;
                }
                if let glory::render::Props::Text(t) = node.descriptor.view().props
                    && !t.content.is_empty()
                {
                    out.push(t.content.clone());
                }
                pending.extend(node.children.iter().rev().copied());
            }
            out.join("\n")
        };

        // Helper: drain every queued ServerApps action through real SSH session.
        let pump = || -> anyhow::Result<()> {
            while let Ok(req) = rx.try_recv() {
                if let Action::ServerApps(action) = req.action {
                    let res =
                        server_apps_worker::handle(&session, action).map_err(|e| format!("{e:#}"));
                    let unhandled = event(&ui, req.serial, res);
                    assert!(unhandled.is_none(), "Redis UI must handle its own replies");
                    renderer
                        .flush()
                        .map_err(|e| anyhow::anyhow!("Flush failed: {e:?}"))?;
                }
            }
            Ok(())
        };

        // 1. Enter Redis app -> Overview loads both instances and renders real UI text.
        ui.redis_enter();
        pump()?;
        assert!(ui.state.with(|s| s.redis.instances.len() == 2));
        assert_eq!(ui.state.with(|s| status(s).0), "Running");
        let ov_text = visible_text();
        assert!(
            ov_text.contains("Redis 7.2.5"),
            "Overview missing version: {ov_text}"
        );
        assert!(
            ov_text.contains("db0") && ov_text.contains("db1"),
            "Overview missing keyspace"
        );

        // 2. Switch to auth-protected Docker container -> needs_auth -> unlock with wrong then
        //    right password -> lock.
        let docker_key = ui.state.with(|s| {
            s.redis
                .instances
                .iter()
                .find(|i| i.docker.is_some())
                .unwrap()
                .key
                .clone()
        });
        ui.state.revise(|s| {
            s.redis.snapshot = s
                .redis
                .instances
                .iter()
                .find(|i| i.key == docker_key)
                .cloned();
            s.redis.instance = docker_key.clone();
            s.redis.clear_instance_views();
        });
        renderer.flush().ok();
        assert!(ui.state.with(needs_auth));
        assert!(visible_text().contains("Redis authentication"));

        ui.state
            .revise(|s| s.redis.auth_password = Zeroizing::new("bad".into()));
        ui.redis_unlock();
        pump()?;
        assert!(ui.state.with(needs_auth));
        assert!(visible_text().contains("WRONGPASS"));

        ui.state.revise(|s| {
            s.redis.auth_username = "default".into();
            s.redis.auth_password = Zeroizing::new("s3cr3t".into());
        });
        ui.redis_unlock();
        pump()?;
        assert!(!ui.state.with(needs_auth));
        assert!(ui.state.with(data_ready));
        assert!(ui.state.with(|s| s.redis.auth_username.is_empty()));
        assert!(visible_text().contains("Redis 7.4.1"));

        ui.redis_clear_auth();
        pump()?;
        assert!(ui.state.with(needs_auth));
        assert!(ui.state.with(|s| s.redis.auth_username.is_empty()));

        // 3. Switch back to host instance -> open db0 -> scan -> load more -> edit string -> TTL ->
        //    Hash add -> New/Delete key.
        ui.state.revise(|s| {
            s.redis.snapshot = s.redis.instances.iter().find(|i| i.key == "host").cloned();
            s.redis.instance = "host".into();
            s.redis.clear_instance_views();
        });
        ui.redis_open_db(0);
        pump()?;
        assert_eq!(ui.state.with(|s| s.redis.keys.len()), 100);
        ui.redis_scan(true, true);
        pump()?;
        assert_eq!(ui.state.with(|s| s.redis.keys.len()), 123);

        // Select JSON string key, pretty-print, save.
        ui.redis_select_key("app:config".into(), true);
        pump()?;
        ui.state.revise(|s| {
            let pretty = maybe_pretty_json(&s.redis.edit_value).unwrap();
            s.redis.edit_value = pretty;
        });
        ui.redis_save_string();
        pump()?;
        assert_eq!(ui.state.with(|s| s.redis.error.clone()), "");
        assert_eq!(ui.state.with(|s| s.redis.info.clone()), "Saved");

        // Set TTL & Persist via UI actions.
        ui.redis_select_key("user:1001".into(), true);
        pump()?;
        ui.state.revise(|s| s.redis.edit_ttl = "240".into());
        ui.redis_apply_ttl(false);
        pump()?;
        assert!(
            ui.state
                .with(|s| s.redis.selected_key.as_ref().unwrap().ttl > 0)
        );
        ui.redis_apply_ttl(true);
        pump()?;
        assert_eq!(
            ui.state
                .with(|s| s.redis.selected_key.as_ref().unwrap().ttl),
            -1
        );

        // Add Hash field via UI action.
        ui.state.revise(|s| {
            s.redis.add_field = "ui_added".into();
            s.redis.add_value = "yes".into();
        });
        ui.redis_add_collection_item();
        pump()?;
        assert!(ui.state.with(|s| {
            s.redis
                .selected_key
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .any(|e| e.label == "ui_added" && e.value == "yes")
        }));

        // New key dialog -> submit -> verify -> Delete key dialog -> confirm -> verify removed.
        ui.redis_open_new_key();
        renderer.flush().ok();
        ui.state.revise(|s| {
            s.redis.new_key.kind = NewKeyKind::String;
            s.redis.new_key.key = "ui:created".into();
            s.redis.new_key.value = "hello-ui".into();
            s.redis.new_key.ttl = "90".into();
        });
        ui.redis_submit_new_key();
        pump()?;
        assert_eq!(
            ui.state
                .with(|s| s.redis.selected_key.as_ref().unwrap().name.clone()),
            "ui:created"
        );
        ui.redis_open_new_key();
        ui.state.revise(|s| {
            s.redis.new_key.key = "ui:created".into();
            s.redis.new_key.value = "replacement".into();
        });
        ui.redis_submit_new_key();
        pump()?;
        assert_eq!(
            ui.state.with(|s| s.redis.error.clone()),
            "A key with this name already exists"
        );
        ui.redis_select_key("ui:created".into(), true);
        pump()?;
        assert_eq!(ui.state.with(|s| s.redis.edit_value.clone()), "hello-ui");
        ui.redis_open_delete_key();
        renderer.flush().ok();
        ui.redis_confirm_delete_key();
        pump()?;
        assert!(ui.state.with(|s| s.redis.selected_key.is_none()));
        assert_eq!(ui.state.with(|s| s.redis.info.clone()), "Key deleted");

        // Diagnostics view via UI action.
        ui.state.revise(|s| s.redis.view = View::Diagnostics);
        ui.redis_diagnostics();
        pump()?;
        let diag_text = visible_text();
        assert!(
            diag_text.contains("api-worker"),
            "Clients view missing api-worker"
        );
        assert!(
            diag_text.contains("HGETALL user:1001"),
            "Slowlog view missing entry"
        );

        let _ = std::fs::remove_dir_all(&root);
        println!("[PASS] Live Arcnix UI + Real SSH Worker E2E flow passed");
        Ok(())
    }
}
