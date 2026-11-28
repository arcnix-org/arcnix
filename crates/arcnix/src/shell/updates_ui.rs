use std::time::{Duration, Instant};

use arcnix_core::updates::{self, Check, Prepared, Release, Source};

use crate::*;
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    #[default]
    Idle,
    Checking,
    Available,
    Downloading,
    Ready,
    Applying,
    Current,
    NoRelease,
    Error,
}
#[derive(Clone)]
pub(crate) struct UpdateState {
    phase: Phase,
    serial: u64,
    release: Option<Release>,
    prepared: Option<Prepared>,
    done: u64,
    total: u64,
    error: String,
    cancel: Arc<AtomicBool>,
    next_check: Instant,
    failed_version: Option<String>,
    auto_at: Option<Instant>,
    deferred: bool,
    pending_hint: bool,
}
impl Default for UpdateState {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            serial: 0,
            release: None,
            prepared: None,
            done: 0,
            total: 0,
            error: String::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            next_check: Instant::now() + Duration::from_secs(5),
            failed_version: None,
            auto_at: None,
            deferred: false,
            pending_hint: false,
        }
    }
}
pub(crate) enum Event {
    Checked(u64, std::result::Result<Check, String>),
    Progress(u64, u64, u64),
    Prepared(u64, std::result::Result<Prepared, String>),
    Helper(std::result::Result<(), String>),
    Cleaned,
}
pub(crate) struct Controller {
    source: Source,
    events: async_channel::Sender<Event>,
}
pub(crate) fn start() -> anyhow::Result<(Arc<Controller>, async_channel::Receiver<Event>)> {
    let source = Source::github();
    #[cfg(debug_assertions)]
    let source = if let Ok(url) = std::env::var("ARCNIX_TEST_UPDATE_URL") {
        Source::loopback(&url)?
    } else {
        source
    };
    let (events, rx) = async_channel::bounded(32);
    Ok((Arc::new(Controller { source, events }), rx))
}
fn supported() -> bool {
    cfg!(all(windows, target_arch = "x86_64"))
}
pub(crate) fn applying(s: &State) -> bool {
    s.updates.phase == Phase::Applying
}
fn idle(s: &State) -> bool {
    s.dialog.is_none() && restart_safe(s)
}
fn restart_safe(s: &State) -> bool {
    if s.pgadmin_component.installing() || !s.inflight_transfers.is_empty() {
        return false;
    }
    !s.connected
        && !s.connecting
        && !s.parked.values().any(|p| {
            p.connected
                || p.connecting
                || p.file_pending.is_some()
                || p.system_task_running
                || p.office
                    .files
                    .values()
                    .any(|doc| doc.dirty || doc.is_saving())
                || p.code.dirty
                || !p.code.pending.is_empty()
                || text_editor::has_pending_work(&p.desktop_editor, p.remote_editor.as_ref())
        })
        && !s.system_task_running
        && !scripts_ui::busy(s)
        && s.scripts.refreshing.is_empty()
        && s.file_pending.is_none()
        && !s
            .office
            .files
            .values()
            .any(|doc| doc.dirty || doc.is_saving())
        && s.code.pending.is_empty()
        && !s.code.dirty
        && !text_editor::has_pending_work(&s.desktop_editor, s.remote_editor.as_ref())
        && s.cancels.is_empty()
        && !s
            .jobs
            .iter()
            .any(|j| matches!(j.status, TransferStatus::Queued | TransferStatus::Running))
}
fn root(ui: &Ui) -> PathBuf {
    ui.store
        .path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("updates")
}
pub(crate) fn restore(ui: &Ui) {
    let config = ui.store.path().to_owned();
    let controller = ui.update_controller.clone();
    std::thread::spawn(move || {
        for _ in 0..3 {
            std::thread::sleep(Duration::from_secs(2));
            if update_install::cleanup(&config).unwrap_or(false) {
                let _ = controller.events.send_blocking(Event::Cleaned);
                break;
            }
        }
    });
    ui.state
        .revise(|s| s.updates.pending_hint = root(ui).join("ready.json").is_file());
    if let Ok(bytes) = std::fs::read(root(ui).join("failure.json"))
        && bytes.len() < 32768
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
    {
        ui.state.revise(|s| {
            s.updates.failed_version = value["version"].as_str().map(str::to_owned);
            s.updates.error = value["message"]
                .as_str()
                .unwrap_or("Previous update failed")
                .to_owned();
        });
    }
}
fn check_due(now: u64, last: u64, hours: u16) -> bool {
    last > now || now - last >= u64::from(hours) * 3600
}

fn schedule_due(s: &State, now: u64, at: Instant) -> bool {
    s.config.settings.updates.automatic_checks
        && matches!(
            s.updates.phase,
            Phase::Idle
                | Phase::Current
                | Phase::NoRelease
                | Phase::Error
                | Phase::Available
                | Phase::Ready
        )
        && s.dialog != Some(Dialog::Update)
        && at >= s.updates.next_check
        && (s.updates.pending_hint
            || s.config
                .settings
                .updates
                .last_check
                .is_none_or(|last| check_due(now, last, s.config.settings.updates.interval_hours)))
}

pub(crate) fn tick(ui: &Ui) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let due = ui.writable.get() && ui.state.with(|s| schedule_due(s, now, Instant::now()));
    if due {
        check(ui);
    }
    let install = ui.state.revise(|s| {
        let eligible = ui.writable.get()
            && supported()
            && s.config.settings.updates.install_when_idle
            && s.updates.phase == Phase::Ready
            && idle(s)
            && !s.updates.deferred
            && s.updates
                .release
                .as_ref()
                .is_some_and(|r| s.updates.failed_version.as_ref() != Some(&r.version));
        if !eligible {
            s.updates.auto_at = None;
            return false;
        }
        let deadline = *s
            .updates
            .auto_at
            .get_or_insert_with(|| Instant::now() + Duration::from_secs(30));
        Instant::now() >= deadline
    });
    if install {
        install_update(ui);
    }
}
fn check(ui: &Ui) {
    let serial = ui.state.revise(|s| {
        if matches!(
            s.updates.phase,
            Phase::Checking | Phase::Downloading | Phase::Applying
        ) {
            return None;
        }
        s.updates.serial += 1;
        s.updates.phase = Phase::Checking;
        if s.updates.failed_version.is_none() {
            s.updates.error.clear();
        }
        s.updates.next_check = Instant::now() + Duration::from_secs(900);
        Some(s.updates.serial)
    });
    let Some(serial) = serial else {
        return;
    };
    let controller = ui.update_controller.clone();
    std::thread::spawn(move || {
        let result = updates::check(&controller.source, env!("CARGO_PKG_VERSION"))
            .map_err(|e| format!("{e:#}"));
        let _ = controller
            .events
            .send_blocking(Event::Checked(serial, result));
    });
}
fn download(ui: &Ui) {
    let task = ui.state.revise(|s| {
        if s.updates.phase != Phase::Available || !supported() {
            return None;
        }
        let release = s.updates.release.clone()?;
        release.asset.as_ref()?;
        s.updates.serial += 1;
        s.updates.phase = Phase::Downloading;
        s.updates.error.clear();
        s.updates.done = 0;
        s.updates.total = release.asset.as_ref()?.size;
        s.updates.cancel = Arc::new(AtomicBool::new(false));
        Some((s.updates.serial, release, s.updates.cancel.clone()))
    });
    let Some((serial, release, cancel)) = task else {
        return;
    };
    let controller = ui.update_controller.clone();
    let root = root(ui);
    std::thread::spawn(move || {
        let mut last = Instant::now();
        let result = updates::prepare(
            &controller.source,
            &release,
            &root,
            || {
                anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Update download canceled");
                Ok(())
            },
            |done, total| {
                if done == 0 || done == total || last.elapsed() >= Duration::from_millis(100) {
                    last = Instant::now();
                    let _ = controller
                        .events
                        .try_send(Event::Progress(serial, done, total));
                }
            },
        )
        .map_err(|e| format!("{e:#}"));
        let _ = controller
            .events
            .send_blocking(Event::Prepared(serial, result));
    });
}
fn install_update(ui: &Ui) {
    let prepared = ui.state.revise(|s| {
        if !supported() || s.updates.phase != Phase::Ready || !idle(s) {
            return None;
        }
        let prepared = s.updates.prepared.clone()?;
        s.updates.phase = Phase::Applying;
        s.updates.error.clear();
        Some(prepared)
    });
    if let Some(prepared) = prepared {
        if ui.writable.get() {
            ui.save();
        }
        let config = ui.store.path().to_owned();
        let controller = ui.update_controller.clone();
        std::thread::spawn(move || {
            let result = update_install::launch(&prepared, &config).map_err(|e| format!("{e:#}"));
            let _ = controller.events.send_blocking(Event::Helper(result));
        });
    }
}
pub(crate) fn event(ui: &Ui, event: Event) {
    let mut automatic_download = false;
    match event {
        Event::Cleaned => ui.state.revise(|s| s.updates.pending_hint = false),
        Event::Checked(serial, result) => {
            if !ui
                .state
                .with(|s| s.updates.serial == serial && s.updates.phase == Phase::Checking)
            {
                return;
            }
            let success = result.is_ok();
            ui.state.revise(|s| s.updates.pending_hint = false);
            ui.state.revise(|s| match result {
                Ok(Check::Current) => {
                    s.updates.phase = Phase::Current;
                    s.updates.prepared = None;
                    s.updates.release = None;
                }
                Ok(Check::NoRelease) => {
                    s.updates.phase = Phase::NoRelease;
                    s.updates.prepared = None;
                    s.updates.release = None;
                }
                Ok(Check::Available(release)) => {
                    if s.updates
                        .failed_version
                        .as_ref()
                        .is_some_and(|failed| failed != &release.version)
                    {
                        s.updates.failed_version = None;
                        s.updates.error.clear();
                    }
                    let unchanged = s.updates.release.as_ref().is_some_and(|known| {
                        known.version == release.version
                            && known.asset.as_ref().map(|a| &a.digest)
                                == release.asset.as_ref().map(|a| &a.digest)
                    });
                    if !unchanged {
                        s.updates.prepared = None;
                        s.updates.deferred = false;
                    }
                    automatic_download = ui.writable.get()
                        && supported()
                        && s.updates.prepared.is_none()
                        && s.config.settings.updates.automatic_download
                        && release.asset.is_some()
                        && s.updates.failed_version.as_ref() != Some(&release.version);
                    s.updates.phase = if s.updates.prepared.is_some() {
                        Phase::Ready
                    } else {
                        Phase::Available
                    };
                    s.updates.release = Some(release);
                }
                Err(error) => {
                    s.updates.phase = if s.updates.prepared.is_some() {
                        Phase::Ready
                    } else if s.updates.release.is_some() {
                        Phase::Available
                    } else {
                        Phase::Error
                    };
                    s.updates.error = error;
                }
            });
            if success && ui.writable.get() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                ui.update_preferences(|s| s.updates.last_check = Some(now));
            }
        }
        Event::Progress(serial, done, total) => ui.state.revise(|s| {
            if s.updates.serial == serial && s.updates.phase == Phase::Downloading {
                s.updates.done = done;
                s.updates.total = total;
            }
        }),
        Event::Prepared(serial, result) => ui.state.revise(|s| {
            if s.updates.serial != serial || s.updates.phase != Phase::Downloading {
                return;
            }
            match result {
                Ok(prepared) => {
                    s.updates.prepared = Some(prepared);
                    s.updates.phase = Phase::Ready;
                    s.updates.auto_at = None;
                }
                Err(error) => {
                    s.updates.error = error;
                    s.updates.phase = Phase::Available;
                }
            }
        }),
        Event::Helper(result) => match result {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                let failure=ui.state.with(|s|serde_json::json!({"version":s.updates.release.as_ref().map(|r|r.version.clone()),"message":error}));
                let _ = std::fs::write(
                    root(ui).join("failure.json"),
                    serde_json::to_vec(&failure).unwrap_or_default(),
                );
                ui.state.revise(|s| {
                    s.updates.phase = Phase::Ready;
                    s.updates.error = error;
                    s.updates.deferred = true;
                    s.updates.auto_at = None;
                    s.updates.failed_version =
                        s.updates.release.as_ref().map(|r| r.version.clone());
                })
            }
        },
    }
    if automatic_download {
        download(ui);
    }
}
fn status(s: &State) -> &'static str {
    match s.updates.phase {
        Phase::Idle => "Updates have not been checked",
        Phase::Checking => "Checking for updates…",
        Phase::Current => "You are using the latest stable version",
        Phase::NoRelease => "No published release is available yet",
        Phase::Available => "A new version is available",
        Phase::Downloading => "Downloading and verifying update…",
        Phase::Ready => "Update verified and ready to install",
        Phase::Applying => "Preparing restart…",
        Phase::Error => "Update check failed",
    }
}
pub(crate) fn panel(ui: Ui) -> impl Widget {
    settings::panel_with_action(
        &ui,
        navigation::SettingsSection::Updates,
        "Updates",
        settings::restore_local_defaults(&ui, false, "Restore update defaults", |s, defaults| {
            let last_check = s.updates.last_check;
            s.updates = defaults.updates.clone();
            s.updates.last_check = last_check;
        })
        .enabled(ui.writable),
        controls(ui.clone()),
    )
}

fn automatic_preferences(ui: Ui) -> impl Widget {
    let state = ui.state;
    let automatic = ui.clone();
    let fetch = ui.clone();
    let install = ui.clone();
    col().class("preferences-stack").fill((
        sapid::checkbox()
            .class("preference-check")
            .label(i18n::bind("Check for updates automatically"))
            .checked(state.map(|s| s.config.settings.updates.automatic_checks))
            .on_checked_change(move |v| {
                automatic.update_preferences(|s| s.updates.automatic_checks = v)
            }),
        preferences_ui::preference_select(
            &ui,
            false,
            "Update check interval",
            [
                (3, "Every 3 hours"),
                (6, "Every 6 hours"),
                (12, "Every 12 hours"),
                (24, "Daily"),
                (168, "Weekly"),
                (720, "Monthly"),
            ],
            |s| s.updates.interval_hours,
            |s, hours| s.updates.interval_hours = hours,
        ),
        when(supported(), move || {
            let fetch = fetch.clone();
            let install = install.clone();
            col().fill((
                sapid::checkbox()
                    .class("preference-check")
                    .label(i18n::bind("Download verified updates automatically"))
                    .checked(state.map(|s| s.config.settings.updates.automatic_download))
                    .on_checked_change(move |v| {
                        fetch.update_preferences(|s| s.updates.automatic_download = v)
                    }),
                sapid::checkbox()
                    .class("preference-check")
                    .label(i18n::bind("Restart and install automatically when idle"))
                    .checked(state.map(|s| s.config.settings.updates.install_when_idle))
                    .on_checked_change(move |v| {
                        install.update_preferences(|s| s.updates.install_when_idle = v)
                    }),
            ))
        }),
    ))
}

fn controls(ui: Ui) -> impl Widget {
    let state = ui.state;
    let preferences = ui.clone();
    let check_ui = ui.clone();
    col().class("preferences-stack update-panel").fill((
        when(ui.writable, move || {
            automatic_preferences(preferences.clone())
        }),
        when(ui.writable.map(|writable| !*writable), || {
            text().class("subtle").content(i18n::bind(
                "Settings are read-only. You can still update Arcnix manually.",
            ))
        }),
        when(
            state.map(|s| s.config.settings.updates.last_check.is_some()),
            move || {
                text().class("subtle").content(state.map(|s| {
                    s.config
                        .settings
                        .updates
                        .last_check
                        .map(|t| {
                            format!(
                                "{} {}",
                                i18n::translate("Last checked:"),
                                formatting::timestamp(Some(t))
                            )
                        })
                        .unwrap_or_default()
                }))
            },
        ),
        sapid::button()
            .class("button")
            .label(i18n::bind("Check now"))
            .enabled(state.map(|s| {
                !matches!(
                    s.updates.phase,
                    Phase::Checking | Phase::Downloading | Phase::Applying
                )
            }))
            .on_press(move || check(&check_ui)),
        release_controls(ui, false),
    ))
}

fn open_confirmation(ui: &Ui) {
    if ui
        .state
        .with(|s| s.dialog.is_none() && s.updates.release.is_some())
    {
        ui.open_dialog(Dialog::Update);
        ui.state.revise(|s| {
            if s.dialog == Some(Dialog::Update) {
                s.updates.auto_at = None;
                s.updates.deferred = true;
            }
        });
    }
}

pub(crate) fn rail_item(ui: Ui) -> impl Widget {
    when(
        ui.state
            .map(|s| s.updates.release.is_some() && !applying(s)),
        move || {
            let ui = ui.clone();
            sapid::button()
                .class("rail-item rail-update")
                .name(i18n::bind("Update available"))
                .fill(icons::glyph(Icon::Download))
                .on_press(move || open_confirmation(&ui))
        },
    )
}

pub(crate) fn dialog(ui: Ui) -> impl Widget {
    when(
        ui.state.map(|s| s.dialog == Some(Dialog::Update)),
        move || {
            let later = ui.clone();
            modal(
                ui.clone(),
                "Software update",
                release_controls(ui.clone(), true),
                btn("Later", move || later.close_dialog()),
            )
        },
    )
}

fn release_controls(ui: Ui, confirming: bool) -> impl Widget {
    let state = ui.state;
    let download_ui = ui.clone();
    let apply = ui.clone();
    let cancel = ui.clone();
    let page = ui.clone();
    col().class("preferences-stack update-details").fill((
        text()
            .class("update-status")
            .content(i18n::bind(state.map(|s| status(s).to_owned()))),
        when(state.map(|s| s.updates.release.is_some()), move || {
            col().class("update-release").fill((
                text().class("section-title").content(state.map(|s| {
                    s.updates
                        .release
                        .as_ref()
                        .map(|r| format!("Arcnix {}", r.version))
                        .unwrap_or_default()
                })),
                text()
                    .class("update-notes")
                    .selectable(true)
                    .content(state.map(|s| {
                        s.updates
                            .release
                            .as_ref()
                            .map(|r| r.notes.clone())
                            .unwrap_or_default()
                    })),
                text().class("subtle").content(i18n::bind(state.map(|s| {
                    if supported() {
                        s.updates
                            .release
                            .as_ref()
                            .map(|r| r.reason.clone())
                            .unwrap_or_default()
                    } else {
                        "Download the update for this platform from the releases page.".into()
                    }
                }))),
            ))
        }),
        when(
            state.map(|s| s.updates.phase == Phase::Downloading),
            move || {
                let cancel = cancel.clone();
                col().class("update-download").fill((
                    text().content(state.map(|s| {
                        format!(
                            "{} / {}",
                            formatting::bytes(s.updates.done),
                            formatting::bytes(s.updates.total)
                        )
                    })),
                    view()
                        .class("update-progress")
                        .fill(view().style(state.map(|s| {
                            glory::style::Style::new().width(glory::style::Length::Percent(
                                if s.updates.total == 0 {
                                    0.
                                } else {
                                    s.updates.done as f64 / s.updates.total as f64 * 100.
                                },
                            ))
                        }))),
                    btn("Cancel download", move || {
                        cancel
                            .state
                            .with(|s| s.updates.cancel.store(true, Ordering::Relaxed))
                    }),
                ))
            },
        ),
        settings::feedback(
            &ui,
            |s| s.updates.error.clone(),
            "software-error update-error",
        ),
        when(
            state.map(|s| s.updates.phase == Phase::Ready && !restart_safe(s)),
            || {
                text().class("subtle").content(i18n::bind(
                    "Waiting for connections and pending work to finish",
                ))
            },
        ),
        when(
            state.map(move |s| confirming && s.updates.phase == Phase::Ready),
            || text().content(i18n::bind("Arcnix will restart to install this update.")),
        ),
        row().class("choice-list").fill((
            btn("Open releases", move || {
                if let Err(error) = open_url(concat!(env!("CARGO_PKG_REPOSITORY"), "/releases")) {
                    page.state.revise(|s| s.updates.error = error.to_string());
                }
            }),
            when(
                state.map(|s| {
                    supported()
                        && s.updates.phase == Phase::Available
                        && s.updates
                            .release
                            .as_ref()
                            .is_some_and(|r| r.asset.is_some())
                }),
                move || {
                    let ui = download_ui.clone();
                    btn("Download update", move || download(&ui))
                },
            ),
            when(
                state.map(|s| supported() && s.updates.phase == Phase::Ready),
                move || {
                    let ui = apply.clone();
                    btn("Restart and install", move || {
                        if confirming {
                            ui.close_dialog();
                            install_update(&ui);
                        } else {
                            open_confirmation(&ui);
                        }
                    })
                    .class("button primary")
                    .enabled(state.map(restart_safe))
                },
            ),
        )),
    ))
}
pub(crate) fn overlay(ui: Ui) -> impl Widget {
    when(ui.state.map(applying), || {
        col().class("update-install-overlay").fill((
            text()
                .class("section-title")
                .content(i18n::bind("Preparing restart…")),
            text().content(i18n::bind(
                "The new version is being verified. The application will restart automatically.",
            )),
        ))
    })
}
pub(crate) fn banner(ui: Ui) -> impl Widget {
    let state = ui.state;
    when(
        state.map(|s| {
            s.updates.auto_at.is_some()
                || (s.updates.failed_version.is_some() && !s.updates.error.is_empty())
        }),
        move || {
            let later = ui.clone();
            let show = ui.clone();
            row().class("update-banner").fill((
                text().content(state.map(|s| {
                    if let Some(at) = s.updates.auto_at {
                        let seconds = at.saturating_duration_since(Instant::now()).as_secs() + 1;
                        i18n::translate(&format!(
                            "Update verified. Restarting in {seconds} seconds."
                        ))
                    } else {
                        i18n::translate(
                            "The last update could not be completed. The previous version is still in use.",
                        )
                    }
                })),
                sapid::button()
                    .class("button")
                    .label(i18n::bind("View updates"))
                    .on_press(move || {
                        show.state.revise(|s| {
                            navigation::change_page(s, navigation::Page::Settings, "update-banner");
                            s.settings_section = navigation::SettingsSection::Updates;
                        })
                    }),
                sapid::button()
                    .class("button")
                    .label(i18n::bind("Later"))
                    .on_press(move || {
                        later.state.revise(|s| {
                            s.updates.auto_at = None;
                            s.updates.deferred = true;
                            s.updates.error.clear();
                        })
                    }),
            ))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduled_checks_repeat_and_respect_active_operations() {
        let mut s = State::new(AppConfig::default(), String::new(), String::new());
        let at = s.updates.next_check;
        let period = 6 * 3600;
        assert!(schedule_due(&s, 0, at));
        for last in [0, period, 2 * period] {
            s.config.settings.updates.last_check = Some(last);
            for phase in [
                Phase::Current,
                Phase::NoRelease,
                Phase::Error,
                Phase::Available,
                Phase::Ready,
            ] {
                s.updates.phase = phase;
                assert!(!schedule_due(&s, last + period - 1, at));
                assert!(schedule_due(&s, last + period, at));
            }
        }
        assert!(schedule_due(&s, 1, at)); // Recover if the system clock moved backwards.
        assert!(!schedule_due(&s, 1, at - Duration::from_secs(1)));
        s.dialog = Some(Dialog::Update);
        assert!(!schedule_due(&s, 1, at));
        s.dialog = None;
        for phase in [Phase::Checking, Phase::Downloading, Phase::Applying] {
            s.updates.phase = phase;
            assert!(!schedule_due(&s, 1, at));
        }
        s.updates.phase = Phase::Idle;
        s.config.settings.updates.automatic_checks = false;
        assert!(!schedule_due(&s, 1, at));
    }
}
