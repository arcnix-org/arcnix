//! The pgAdmin window: component installation, the database connection and the
//! area that hosts pgAdmin's own interface.
use arcnix_core::browser_proxy::Diagnosis;
use arcnix_core::pgadmin::{self, Connection};

use crate::i18n::desktop as i18n;
use crate::i18n::desktop::{btn, icon_action};
use crate::*;

#[derive(Clone, Default)]
pub(crate) enum Stage {
    #[default]
    Closed,
    Checking(u64),
    Missing {
        size: Option<u64>,
    },
    Installing {
        serial: u64,
        cancel: Arc<AtomicBool>,
        percent: u64,
    },
    Connect,
    Starting {
        serial: u64,
        stop: Arc<AtomicBool>,
    },
    Running {
        serial: u64,
        stop: Arc<AtomicBool>,
        url: String,
    },
}

#[derive(Clone, Default)]
pub(crate) struct PgAdmin {
    stage: Stage,
    pub viewport: Option<native_viewport::Viewport>,
    pub error: String,
    pub reload: bool,
    pub download: Option<Download>,
    connection: Connection,
    port: String,
}

impl PgAdmin {
    pub fn url(&self) -> Option<&str> {
        match &self.stage {
            Stage::Running { url, .. } => Some(url),
            _ => None,
        }
    }

    fn stop(&self) {
        match &self.stage {
            Stage::Installing { cancel: flag, .. }
            | Stage::Starting { stop: flag, .. }
            | Stage::Running { stop: flag, .. } => flag.store(true, Ordering::Release),
            _ => {}
        }
    }

    /// Replies belong to the request that started the current stage.
    fn awaits(&self, reply: u64) -> bool {
        match self.stage {
            Stage::Checking(serial)
            | Stage::Installing { serial, .. }
            | Stage::Starting { serial, .. }
            | Stage::Running { serial, .. } => serial == reply,
            _ => false,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Download {
    pub path: std::path::PathBuf,
    pub success: bool,
}

impl Download {
    fn message(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if self.success {
            format!("Saved {name} to Downloads")
        } else {
            format!("Could not save {name}")
        }
    }
}

/// Select the file in Finder; the folder opens even if the file was moved.
fn reveal(download: &Download) {
    #[cfg(target_os = "macos")]
    {
        let mut command = std::process::Command::new("/usr/bin/open");
        if download.success && download.path.exists() {
            command.arg("-R").arg(&download.path);
        } else if let Some(folder) = download.path.parent() {
            command.arg(folder);
        }
        let _ = command.spawn();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = download;
}

pub(crate) enum Reply {
    Opened {
        installed: bool,
        size: Option<u64>,
        connection: Connection,
    },
    Progress(u64),
    Installed(std::result::Result<(), String>),
    Ready(String),
    Unreachable(Diagnosis),
    Ended(std::result::Result<(), String>),
}

pub(crate) fn reset(s: &mut State) {
    s.pgadmin.stop();
    s.pgadmin = PgAdmin::default();
}

impl Ui {
    /// Check the component whenever an open window has not done so yet.
    pub(crate) fn pgadmin_enter(&self) {
        if !self.state.with(|s| {
            s.connected
                && s.local_desktop.is_none()
                && s.workbench.apps.contains(&WorkspacePage::PgAdmin)
                && matches!(s.pgadmin.stage, Stage::Closed)
        }) {
            return;
        }
        match pgadmin::base(&self.store) {
            Ok(base) => {
                let serial = self.send(Action::PgAdminOpen(base));
                self.state
                    .revise(|s| s.pgadmin.stage = Stage::Checking(serial));
            }
            Err(error) => self
                .state
                .revise(|s| s.pgadmin.error = format!("{error:#}")),
        }
    }

    fn pgadmin_install(&self) {
        let Ok(base) = pgadmin::base(&self.store) else {
            return;
        };
        if !self
            .state
            .with(|s| matches!(s.pgadmin.stage, Stage::Missing { size: Some(_) }))
        {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let serial = self.send(Action::PgAdminInstall(base, cancel.clone()));
        self.state.revise(|s| {
            s.pgadmin.error.clear();
            s.pgadmin.stage = Stage::Installing {
                serial,
                cancel,
                percent: 0,
            };
        });
    }

    fn pgadmin_start(&self) {
        let connection = self.state.revise(|s| {
            if !s.connected || !matches!(s.pgadmin.stage, Stage::Connect) {
                return None;
            }
            let mut connection = s.pgadmin.connection.clone();
            connection.host = connection.host.trim().to_owned();
            connection.port = s.pgadmin.port.trim().parse().unwrap_or(0);
            match connection.validate() {
                Ok(()) => {
                    s.pgadmin.error.clear();
                    s.pgadmin.connection = connection.clone();
                    Some(connection)
                }
                Err(error) => {
                    s.pgadmin.error = error.to_string();
                    None
                }
            }
        });
        let (Some(connection), Ok(base)) = (connection, pgadmin::base(&self.store)) else {
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let serial = self.send(Action::PgAdminStart {
            base,
            connection,
            stop: stop.clone(),
        });
        self.state
            .revise(|s| s.pgadmin.stage = Stage::Starting { serial, stop });
    }

    /// Return to the connection form, or to the install prompt.
    fn pgadmin_stop(&self) {
        let reopen = self.state.revise(|s| {
            s.pgadmin.stop();
            s.pgadmin.error.clear();
            let installing = matches!(s.pgadmin.stage, Stage::Installing { .. });
            s.pgadmin.stage = if installing {
                Stage::Closed
            } else {
                Stage::Connect
            };
            installing
        });
        if reopen {
            self.pgadmin_enter();
        }
    }
}

pub(crate) fn reply(ui: &Ui, serial: u64, reply: Reply) {
    let reopen = ui.state.revise(|s| {
        let pgadmin = &mut s.pgadmin;
        if !pgadmin.awaits(serial) {
            return false;
        }
        match reply {
            Reply::Opened {
                installed,
                size,
                connection,
            } => {
                pgadmin.port = connection.port.to_string();
                pgadmin.connection = connection;
                pgadmin.stage = if installed {
                    Stage::Connect
                } else {
                    Stage::Missing { size }
                };
            }
            Reply::Progress(value) => {
                if let Stage::Installing { percent, .. } = &mut pgadmin.stage {
                    *percent = value;
                }
            }
            Reply::Installed(result) => {
                pgadmin.stage = Stage::Closed;
                pgadmin.error = result.err().unwrap_or_default();
                return true;
            }
            Reply::Ready(url) => {
                if let Stage::Starting { serial, stop } = &pgadmin.stage {
                    pgadmin.stage = Stage::Running {
                        serial: *serial,
                        stop: stop.clone(),
                        url,
                    };
                }
            }
            Reply::Unreachable(diagnosis) => {
                pgadmin.error = match diagnosis {
                    Diagnosis::ForwardingDenied => "The SSH server does not allow forwarding to this PostgreSQL address.",
                    Diagnosis::DestinationUnreachable => "The server could not reach PostgreSQL. Check that it is running and the address is correct.",
                    _ => "The PostgreSQL connection could not be opened through SSH.",
                }
                .into();
            }
            Reply::Ended(result) => {
                pgadmin.stop();
                pgadmin.stage = Stage::Connect;
                pgadmin.error = result.err().unwrap_or_default();
            }
        }
        false
    });
    if reopen {
        ui.pgadmin_enter();
    }
}

fn field(
    ui: &Ui,
    label: &'static str,
    read: fn(&PgAdmin) -> String,
    write: fn(&mut PgAdmin, String),
) -> impl Widget + use<> {
    let state = ui.state;
    let submit = ui.clone();
    col().class("field").fill((
        text().class("field-label").content(i18n::bind(label)),
        input()
            .name(i18n::bind(label))
            .value(state.map(move |s| read(&s.pgadmin)))
            .on(value_changed, move |event| {
                if let Event::ValueChanged { value, .. } = event
                    && value.len() <= 255
                {
                    state.revise(|s| write(&mut s.pgadmin, value));
                }
            })
            .on(commit, move |_| submit.pgadmin_start()),
    ))
}

fn panel(ui: Ui) -> impl Widget {
    let state = ui.state;
    let install = ui.clone();
    let cancel = ui.clone();
    let connect = ui.clone();
    let stop = ui.clone();
    col().class("pgadmin-panel form-stack").fill((
        when(
            state.map(|s| matches!(s.pgadmin.stage, Stage::Closed | Stage::Checking(_))),
            || text().class("subtle").content(i18n::bind("Checking pgAdmin…")),
        ),
        when(
            state.map(|s| matches!(s.pgadmin.stage, Stage::Missing { .. })),
            move || {
                let install = install.clone();
                fragment((
                    text()
                        .class("section-title")
                        .content(i18n::bind("pgAdmin is not installed")),
                    text().class("subtle").content(i18n::bind(
                        "Arcnix keeps its own copy of pgAdmin on this computer. Nothing is installed on the server.",
                    )),
                    when(
                        state.map(|s| matches!(s.pgadmin.stage, Stage::Missing { size: None })),
                        || {
                            text().class("subtle").content(i18n::bind(
                                "This version of Arcnix has no pgAdmin download for this system yet.",
                            ))
                        },
                    ),
                    text().class("subtle").content(state.map(|s| match s.pgadmin.stage {
                        Stage::Missing { size: Some(size) } if size > 0 => {
                            format!("{} MB", size.div_ceil(1_000_000))
                        }
                        _ => String::new(),
                    })),
                    btn("Install pgAdmin", move || install.pgadmin_install()).enabled(
                        state.map(|s| {
                            matches!(s.pgadmin.stage, Stage::Missing { size: Some(_) })
                        }),
                    ),
                ))
            },
        ),
        when(
            state.map(|s| matches!(s.pgadmin.stage, Stage::Installing { .. })),
            move || {
                let cancel = cancel.clone();
                fragment((
                    text()
                        .class("section-title")
                        .content(i18n::bind("Installing pgAdmin…")),
                    text().class("subtle").content(state.map(|s| {
                        match s.pgadmin.stage {
                            Stage::Installing { percent, .. } if percent < 100 => {
                                format!("{percent}%")
                            }
                            _ => String::new(),
                        }
                    })),
                    btn("Cancel", move || cancel.pgadmin_stop()),
                ))
            },
        ),
        when(
            state.map(|s| matches!(s.pgadmin.stage, Stage::Connect)),
            move || {
                let start = connect.clone();
                fragment((
                    text()
                        .class("section-title")
                        .content(i18n::bind("Connect to PostgreSQL")),
                    text().class("subtle").content(i18n::bind(
                        "The address is resolved on the server and reached through SSH. pgAdmin asks for the database password.",
                    )),
                    field(
                        &connect,
                        "PostgreSQL host",
                        |p| p.connection.host.clone(),
                        |p, value| p.connection.host = value,
                    ),
                    field(
                        &connect,
                        "PostgreSQL port",
                        |p| p.port.clone(),
                        |p, value| p.port = value,
                    ),
                    field(
                        &connect,
                        "Database name",
                        |p| p.connection.database.clone(),
                        |p, value| p.connection.database = value,
                    ),
                    field(
                        &connect,
                        "Database user",
                        |p| p.connection.user.clone(),
                        |p, value| p.connection.user = value,
                    ),
                    btn("Open pgAdmin", move || start.pgadmin_start()),
                ))
            },
        ),
        when(
            state.map(|s| matches!(s.pgadmin.stage, Stage::Starting { .. })),
            move || {
                let stop = stop.clone();
                fragment((
                    text()
                        .class("section-title")
                        .content(i18n::bind("Starting pgAdmin…")),
                    btn("Cancel", move || stop.pgadmin_stop()),
                ))
            },
        ),
    ))
}

pub(crate) fn title_tools(ui: Ui) -> impl Widget {
    let state = ui.state;
    when(state.map(|s| s.pgadmin.url().is_some()), move || {
        let stop = ui.clone();
        row().class("desktop-app-tools").fill((
            text()
                .class("pgadmin-download subtle")
                .content(i18n::bind(state.map(|s| {
                    s.pgadmin
                        .download
                        .as_ref()
                        .map(Download::message)
                        .unwrap_or_default()
                }))),
            when(state.map(|s| s.pgadmin.download.is_some()), move || {
                icon_action("Show in Finder", Icon::Folder).on_press(move || {
                    if let Some(download) = state.with(|s| s.pgadmin.download.clone()) {
                        reveal(&download);
                    }
                })
            }),
            icon_action("Reload pgAdmin", Icon::Refresh)
                .on_press(move || state.revise(|s| s.pgadmin.reload = true)),
            icon_action("Close database connection", Icon::Pause)
                .on_press(move || stop.pgadmin_stop()),
        ))
    })
}

pub(crate) fn page(ui: Ui) -> impl Widget {
    let state = ui.state;
    col().class("pgadmin-page").fill((
        input()
            .class("browser-viewport-input hidden")
            .value("")
            .on(value_changed, move |event| {
                if let Event::ValueChanged { value, .. } = event
                    && value.len() < 8192
                    && let Ok(v) = serde_json::from_str::<native_viewport::Viewport>(&value)
                    && v.valid()
                    && state.with(|s| s.epoch == v.epoch && s.pgadmin.viewport.as_ref() != Some(&v))
                {
                    state.revise(|s| s.pgadmin.viewport = Some(v));
                }
            }),
        text()
            .class("browser-epoch hidden")
            .content(state.map(|s| s.epoch.to_string())),
        text()
            .class("pgadmin-error form-error")
            .content(i18n::bind(state.map(|s| s.pgadmin.error.clone()))),
        when(state.map(|s| s.pgadmin.url().is_some()), || {
            col().class("browser-surface").fill(
                text()
                    .class("subtle")
                    .content(i18n::bind("pgAdmin shows here when nothing covers it.")),
            )
        })
        .otherwise(move || panel(ui.clone())),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_from_an_earlier_request_are_ignored() {
        let stop = Arc::new(AtomicBool::new(false));
        let pgadmin = PgAdmin {
            stage: Stage::Starting {
                serial: 7,
                stop: stop.clone(),
            },
            ..Default::default()
        };
        assert!(pgadmin.awaits(7));
        assert!(!pgadmin.awaits(6));
        assert!(pgadmin.url().is_none());
        pgadmin.stop();
        assert!(stop.load(Ordering::Acquire));
        assert!(!PgAdmin::default().awaits(0));
    }
}
