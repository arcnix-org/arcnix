use arcnix_core::preferences::*;

use crate::*;

pub(crate) fn tick(ui: &Ui) {
    if let Some(error) = ui.logger.error() {
        ui.state.revise(|s| s.settings.log_error = error);
    }
    keepalive(ui);
}

pub(crate) fn keepalive_seconds(config: &AppConfig, server: Option<&str>) -> u32 {
    config
        .servers
        .iter()
        .find(|p| Some(p.id.as_str()) == server)
        .and_then(|p| p.connection)
        .unwrap_or(config.settings.connection)
        .keepalive_seconds
}

pub(crate) fn keepalive(ui: &Ui) {
    let due = ui.state.revise(|s| {
        let seconds = keepalive_seconds(&s.config, s.selected.as_deref());
        if seconds == 0
            || !s.connected
            || s.keepalive_pending
            || s.system_task_running
            || s.file_pending.is_some()
            || !s.cancels.is_empty()
            || s.keepalive_at.elapsed().as_secs() < u64::from(seconds)
        {
            return false;
        }
        s.keepalive_at = std::time::Instant::now();
        s.keepalive_pending = true;
        true
    });
    if due {
        ui.send(Action::Keepalive);
    }
}
fn label(value: &'static str) -> impl Widget {
    text().class("field-label").content(i18n::bind(value))
}
fn check(
    ui: Ui,
    name: &'static str,
    read: fn(&AppSettings) -> bool,
    write: fn(&mut AppSettings, bool),
) -> sapid::Checkbox {
    sapid::checkbox()
        .class("preference-check")
        .name(i18n::bind(name))
        .label(i18n::bind(name))
        .checked(ui.state.map(move |s| read(&s.config.settings)))
        .on_checked_change(move |checked| ui.update_preferences(|s| write(s, checked)))
}
pub(crate) fn preference_select<T, L, O>(
    ui: &Ui,
    desktop: bool,
    title: &'static str,
    options: O,
    read: fn(&AppSettings) -> T,
    write: fn(&mut AppSettings, T),
) -> impl Widget + use<T, L, O>
where
    T: Copy + PartialEq + 'static,
    L: Into<String>,
    O: IntoIterator<Item = (T, L)>,
{
    settings::select(
        ui,
        desktop,
        title,
        options,
        move |s| read(&s.config.settings),
        |_| true,
        move |ui, value| ui.update_preferences(|s| write(s, value)),
    )
}

pub(crate) fn display(ui: Ui) -> impl Widget {
    fragment((
        settings::panel_with_action(
            &ui,
            navigation::SettingsSection::Files,
            "File sizes",
            restore_file_size_defaults(&ui, false),
            size_options(&ui, false),
        ),
        settings::panel_with_action(
            &ui,
            navigation::SettingsSection::Files,
            "Dates and times",
            settings::restore_local_defaults(
                &ui,
                false,
                "Restore date and time defaults",
                |s, defaults| {
                    s.display.dates = defaults.display.dates;
                    s.display.timezone = defaults.display.timezone;
                },
            ),
            date_options(&ui),
        ),
    ))
}

pub(crate) fn restore_file_size_defaults(ui: &Ui, desktop: bool) -> sapid::Button {
    settings::restore_local_defaults(ui, desktop, "Restore file size defaults", |s, defaults| {
        s.display.units = defaults.display.units;
        s.display.decimals = defaults.display.decimals;
        s.display.group_digits = defaults.display.group_digits;
    })
}

fn setting_field(
    desktop: bool,
    title: &'static str,
    control: impl Widget + 'static,
) -> impl Widget {
    row()
        .class("preference-select-row")
        .fill((text().content(i18n::bind_in(desktop, title)), control))
}

pub(crate) fn size_options(ui: &Ui, desktop: bool) -> impl Widget + use<> {
    col().class("preferences-stack").fill((
        setting_field(
            desktop,
            "File size units",
            preference_select(
                ui,
                desktop,
                "File size units",
                [
                    (SizeUnits::Decimal, "Decimal · KB / MB"),
                    (SizeUnits::Iec, "IEC · KiB / MiB"),
                ],
                |s| s.display.units,
                |s, units| s.display.units = units,
            ),
        ),
        setting_field(
            desktop,
            "Decimal places",
            preference_select(
                ui,
                desktop,
                "Decimal places",
                (0..=DisplaySettings::MAX_DECIMALS).map(|n| (n, n.to_string())),
                |s| s.display.decimals,
                |s, places| s.display.decimals = places,
            ),
        ),
        check(
            ui.clone(),
            "Use thousands separators",
            |s| s.display.group_digits,
            |s, v| s.display.group_digits = v,
        )
        .name(i18n::bind_in(desktop, "Use thousands separators"))
        .label(i18n::bind_in(desktop, "Use thousands separators")),
        text().class("preference-preview").content(i18n::bind_in(
            desktop,
            ui.state.map(|s| {
                format!(
                    "Preview: {}",
                    formatting::bytes_with(123456789, s.config.settings.display)
                )
            }),
        )),
    ))
}

pub(crate) fn date_options(ui: &Ui) -> impl Widget + use<> {
    col().class("preferences-stack").fill((
        setting_field(
            false,
            "Date format",
            preference_select(
                ui,
                false,
                "Date format",
                [
                    (DateStyle::Relative, "Relative time"),
                    (DateStyle::Iso, "ISO 8601"),
                    (DateStyle::System, "System format"),
                ],
                |s| s.display.dates,
                |s, dates| s.display.dates = dates,
            ),
        ),
        setting_field(
            false,
            "Time zone",
            preference_select(
                ui,
                false,
                "Time zone",
                [
                    (DisplayTimezone::Local, "Local timezone"),
                    (DisplayTimezone::Utc, "UTC"),
                ],
                |s| s.display.timezone,
                |s, zone| s.display.timezone = zone,
            ),
        ),
        text()
            .class("preference-preview")
            .content(ui.state.map(|_| {
                let example = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .saturating_sub(90);
                format!(
                    "{}: {}",
                    i18n::translate("Preview"),
                    formatting::modified(Some(example))
                )
            })),
    ))
}
pub(crate) fn connection(ui: Ui) -> impl Widget {
    settings::panel_with_action(&ui,navigation::SettingsSection::Connection,"Timeout and keepalive",
        settings::restore_local_defaults(&ui, false, "Restore connection defaults", |s, defaults| s.connection = defaults.connection), col().class("preferences-stack").fill((
        setting_field(false, "Connection timeout", preference_select(&ui, false, "Connection timeout",
            [5u32,15,30,60,120].map(|n| (n,format!("{n} seconds"))), |s| s.connection.timeout_seconds, |s,n| s.connection.timeout_seconds=n)),
        setting_field(false, "SSH keepalive", preference_select(&ui, false, "SSH keepalive",
            [0u32,15,30,60].map(|n| (n,if n==0 {"Disabled".into()} else {format!("{n} seconds")})), |s| s.connection.keepalive_seconds, |s,n| s.connection.keepalive_seconds=n)),
        text().class("subtle").content(i18n::bind("Connection changes apply on the next connection. Commands and file changes are never replayed automatically.")),
    )))
}
fn policy(ui: Ui, direction: TransferDirection) -> impl Widget {
    let uploads = direction == TransferDirection::Upload;
    let title = if uploads {
        "Upload policy"
    } else {
        "Download policy"
    };
    setting_field(
        false,
        title,
        settings::select(
            &ui,
            false,
            title,
            [
                (None, "Ask every time"),
                (Some(ExistingFileAction::Overwrite), "Overwrite"),
                (Some(ExistingFileAction::Skip), "Skip"),
            ],
            move |s| {
                if uploads {
                    s.config.settings.upload_conflict
                } else {
                    s.config.settings.download_conflict
                }
            },
            |_| true,
            move |ui, action| {
                ui.update_preferences(|s| {
                    if uploads {
                        s.upload_conflict = action
                    } else {
                        s.download_conflict = action
                    }
                })
            },
        ),
    )
}
pub(crate) fn transfers(ui: Ui) -> impl Widget {
    let edit = ui.clone();
    let apply = ui.clone();
    let restore = ui.clone();
    fragment((
        settings::panel_with_action(
            &ui,
            navigation::SettingsSection::Transfers,
            "Concurrent transfers",
            settings::restore_button(false, "Restore default transfer limit", move || {
                restore.update_preferences(|s| s.transfer_concurrency = AppSettings::default().transfer_concurrency);
                if restore.state.with(|s| s.settings.error.is_empty()) {
                    restore.state.revise(|s| s.settings.transfer_concurrency_draft = None);
                    restore.schedule();
                }
            }),
            col().class("preferences-stack").fill((
                label("Maximum simultaneous files"),
                row().class("script-actions").fill((
                    input()
                        .class("transfer-concurrency-input")
                        .name(i18n::bind("Maximum simultaneous files"))
                        .value(ui.state.map(|s| {
                            s.settings.transfer_concurrency_draft.clone().unwrap_or_else(|| {
                                s.config.settings.transfer_concurrency.to_string()
                            })
                        }))
                        .on(value_changed, move |event| {
                            if let Event::ValueChanged { value, .. } = event {
                                edit.state.revise(|s| s.settings.transfer_concurrency_draft = Some(value));
                            }
                        }),
                    sapid::button()
                        .class("button")
                        .label(i18n::bind("Apply transfer limit"))
                        .enabled(ui.state.map(|s| s.settings.transfer_concurrency_draft.is_some()))
                        .on_press(move || {
                            let value = apply.state.with(|s| s.settings.transfer_concurrency_draft.clone().unwrap_or_default());
                            let Ok(limit) = value.trim().parse::<u8>() else {
                                apply.state.revise(|s| s.settings.error = "Concurrent transfers must be between 1 and 16".into());
                                return;
                            };
                            apply.update_preferences(|s| s.transfer_concurrency = limit);
                            if apply.state.with(|s| s.settings.error.is_empty()) {
                                apply.state.revise(|s| s.settings.transfer_concurrency_draft = None);
                                apply.schedule();
                            }
                        }),
                )),
                text().class("subtle").content(i18n::bind("Choose 1–16 simultaneous files (default 5). Uploads and downloads share this limit. Lowering it lets active files finish before more start.")),
            )),
        ),
        settings::panel_with_action(&ui,navigation::SettingsSection::Transfers,"Existing files",
            settings::restore_local_defaults(&ui, false, "Restore file conflict defaults", |s, defaults| {
                s.upload_conflict = defaults.upload_conflict;
                s.download_conflict = defaults.download_conflict;
            }), col().class("preferences-stack").fill((
            text().class("subtle").content(i18n::bind("Default behavior when a destination already exists. Server and session choices override these defaults.")),
            policy(ui.clone(),TransferDirection::Upload),policy(ui.clone(),TransferDirection::Download),
        ))),
    ))
}
pub(crate) fn logging(ui: Ui) -> impl Widget {
    let export = ui.clone();
    let activity = ui.clone();
    let state = ui.state;
    let restore = ui.clone();
    fragment((
        settings::panel(
            &ui,
            navigation::SettingsSection::Logging,
            "Session activity",
            col().class("preferences-stack").fill((
                text().class("subtle").content(i18n::bind(
                    "Review recent connections, transfers, and other actions from this session. The activity list is cleared when you quit Arcnix.",
                )),
                check(
                    ui.clone(),
                    "Include the time in new entries",
                    |s| s.logging.timestamps,
                    |s, v| s.logging.timestamps = v,
                ).enabled(ui.writable),
                row().class("choice-list").fill((
                    btn("View session activity", move || activity.navigate(navigation::Page::Notifications)),
                    sapid::button()
                        .class("button export-log")
                        .label(i18n::bind("Export session log…"))
                        .on_press(move || {
                            match file_picker::save(
                                &i18n::translate("Export session log"),
                                "arcnix-operation.log",
                            )
                            .and_then(|p| {
                                if let Some(p) = p {
                                    std::fs::write(p, export.state.with(|s| s.logs.clone()))?;
                                }
                                Ok(())
                            }) {
                                Ok(()) => export.state.revise(|s| s.settings.log_export_error.clear()),
                                Err(e) => export
                                    .state
                                    .revise(|s| s.settings.log_export_error = e.to_string()),
                            }
                        }),
                )),
                settings::feedback(&ui, |s| s.settings.log_export_error.clone(), "software-error"),
            )),
        ),
        log_files(ui),
        when(state.map(|s| s.settings_section == navigation::SettingsSection::Logging), move || {
            let restore = restore.clone();
            let writable = restore.writable;
            row().class("choice-list").fill(
                settings::restore_button(false, "Restore logging defaults", move || {
                    restore.update_preferences(|s| s.logging = LogSettings::default());
                    if restore.state.with(|s| s.settings.error.is_empty()) {
                        restore.state.revise(|s| {
                            s.settings.log_directory_draft = None;
                            s.settings.log_error.clear();
                            s.settings.open_log_feedback.clear();
                        });
                    }
                }).enabled(state.map(move |s| writable.get() &&
                    (s.config.settings.logging != LogSettings::default() || s.settings.log_directory_draft.is_some())
                )),
            )
        }),
    ))
}
pub(crate) fn open_local_log_button(ui: &Ui) -> sapid::Button {
    let ui = ui.clone();
    btn("Open local log", move || {
        let (path, enabled) = ui.state.with(|s| {
            let logging = &s.config.settings.logging;
            (ui.logger.active_path(logging), logging.write_file)
        });
        let feedback = match client_logs::open_existing(&path) {
            Ok(true) => String::new(),
            Ok(false) if enabled => "The local log will be created with the next activity.".into(),
            Ok(false) => {
                "No local log file yet. Turn on “Automatically save logs to this computer” in Settings → Logging."
                    .into()
            }
            Err(error) => format!(
                "Could not open local log at {}: {error:#}",
                local_display(&path)
            ),
        };
        ui.state.revise(|s| s.settings.open_log_feedback = feedback);
    })
}

pub(crate) fn open_log_feedback(ui: &Ui) -> impl Widget + use<> {
    settings::feedback(
        ui,
        |s| s.settings.open_log_feedback.clone(),
        "subtle local-log-feedback",
    )
}

fn log_files(ui: Ui) -> impl Widget {
    let directory = ui.clone();
    let apply_directory = ui.clone();
    let browse = ui.clone();
    let save = ui.clone();
    let fallback = local_display(ui.logger.directory());
    settings::panel(
        &ui,
        navigation::SettingsSection::Logging,
        "Local log",
        col().class("preferences-stack").fill((
            row().class("choice-list settings-choices log-save-switch").fill(
                sapid::switch()
                    .size(sapid::Size::Small)
                    .label_position(sapid::SwitchLabelPosition::Before)
                    .label(i18n::bind("Automatically save logs to this computer"))
                    .checked(ui.state.map(|s| s.config.settings.logging.write_file).with_partial_eq())
                    .enabled(ui.writable)
                    .on_checked_change(move |enabled| save.update_preferences(|s| s.logging.write_file = enabled)),
            ),
            text().class("subtle").content(i18n::bind(
                "Save new entries to arcnix.log so you can read them after quitting Arcnix.",
            )),
            row().class("choice-list").fill(open_local_log_button(&ui)),
            open_log_feedback(&ui),
            col()
                .class(ui.state.map(|s| {
                    if s.config.settings.logging.write_file {
                        "preferences-stack log-file-options"
                    } else {
                        "hidden"
                    }
                    .to_owned()
                }))
                .fill((
                    label("Save logs in"),
                    row().class("file-picker-field").fill((
                        input()
                            .name(i18n::bind("Save logs in"))
                            .value(ui.state.map(move |s| {
                                s.settings.log_directory_draft.clone().unwrap_or_else(|| {
                                    s.config
                                        .settings
                                        .logging
                                        .directory
                                        .as_ref()
                                        .map(|p| local_display(p))
                                        .unwrap_or_else(|| fallback.clone())
                                })
                            }))
                            .on(value_changed, move |e| {
                                if let Event::ValueChanged { value, .. } = e {
                                    directory
                                        .state
                                        .revise(|s| s.settings.log_directory_draft = Some(value));
                                }
                            }),
                        sapid::button()
                            .class("button")
                            .label(i18n::bind("Apply"))
                            .enabled(ui.state.map(|s| s.settings.log_directory_draft.is_some()))
                            .on_press(move || {
                                let value = apply_directory.state.with(|s| {
                                    s.settings.log_directory_draft.clone().unwrap_or_default()
                                });
                                apply_directory.update_preferences(|s| {
                                    s.logging.directory = if value.trim().is_empty() {
                                        None
                                    } else {
                                        Some(value.into())
                                    }
                                });
                                if apply_directory.state.with(|s| s.settings.error.is_empty()) {
                                    apply_directory.state.revise(|s| {
                                        s.settings.log_directory_draft = None;
                                        s.settings.log_error.clear();
                                    });
                                }
                            }),
                        sapid::button()
                            .class("button browse-button")
                            .label(i18n::bind("Browse…"))
                            .on_press(move || {
                                let path = browse
                                    .state
                                    .with(|s| s.config.settings.logging.directory.clone());
                                match file_picker::folder(
                                    path.as_deref(),
                                    &i18n::translate("Save logs in"),
                                ) {
                                    Ok(Some(path)) => {
                                        browse
                                            .state
                                            .revise(|s| s.settings.log_directory_draft = None);
                                        browse.update_preferences(|s| {
                                            s.logging.directory = Some(path)
                                        })
                                    }
                                    Ok(None) => {}
                                    Err(e) => browse
                                        .state
                                        .revise(|s| s.settings.log_error = e.to_string()),
                                }
                            }),
                    )),
                    text()
                        .class("subtle")
                        .content(i18n::bind("Clear the folder and select Apply to use the default location.")),
                    setting_field(
                        false,
                        "Maximum size per file",
                        preference_select(
                            &ui,
                            false,
                            "Maximum size per file",
                            [1u16, 5, 10, 50, 100].map(|n| (n, format!("{n} MiB"))),
                            |s| s.logging.max_size_mib,
                            |s, n| s.logging.max_size_mib = n,
                        ),
                    ),
                    setting_field(
                        false,
                        "Old log files to keep",
                        preference_select(
                            &ui,
                            false,
                            "Old log files to keep",
                            [1u8, 3, 5, 10].map(|n| (n, n.to_string())),
                            |s| s.logging.retained_files,
                            |s, n| s.logging.retained_files = n,
                        ),
                    ),
                    text().class("subtle").content(i18n::bind(
                        "When a file fills up, Arcnix starts a new one and removes the oldest files beyond this limit.",
                    )),
                )),
            settings::feedback(
                &ui,
                |s| s.settings.log_error.clone(),
                "software-error log-error",
            ),
        )),
    )
}
pub(crate) fn server_files(ui: Ui) -> impl Widget {
    let browse = ui.clone();
    let remember = ui.clone();
    let clear = ui.clone();
    col().class(ui.state.map(|s|if s.server_editor_tab==1{"form-stack"}else{"hidden"}.to_owned())).fill((
        field("Default local directory",&ui,|s|s.editor.locations.local_directory.as_ref().map(|p|local_display(p)).unwrap_or_default(),|s,v|s.editor.locations.local_directory=if v.trim().is_empty(){None}else{Some(v.into())}),
        sapid::button().class("button").label(i18n::bind("Browse…")).on_press(move||{let path=browse.state.with(|s|s.editor.locations.local_directory.clone());match file_picker::folder(path.as_deref(),&i18n::translate("Default local directory")){Ok(Some(path))=>browse.state.revise(|s|s.editor.locations.local_directory=Some(path)),Ok(None)=>{},Err(e)=>browse.state.revise(|s|s.dialog_error=e.to_string())}}),
        field("初始远程目录",&ui,|s|s.editor.initial_remote_path.clone(),|s,v|s.editor.initial_remote_path=v),
        sapid::checkbox().class("preference-check").label(i18n::bind("Remember browsing locations for this server")).checked(ui.state.map(|s|s.editor.locations.remember_paths)).on_checked_change(move|v|remember.state.revise(|s|s.editor.locations.remember_paths=v)),
        text().class("subtle").content(i18n::bind("Remembered locations take precedence over the default directories. Clear them to start at the defaults again.")),
        sapid::button().class("button").label(i18n::bind("Clear remembered locations")).on_press(move||clear.state.revise(|s|{s.editor.locations.last_local=None;s.editor.locations.last_remote=None;})),
    ))
}
pub(crate) fn server_connection(ui: Ui) -> impl Widget {
    let inherit = ui.clone();
    col()
        .class(ui.state.map(|s| {
            if s.server_editor_tab == 2 {
                "form-stack"
            } else {
                "hidden"
            }
            .to_owned()
        }))
        .fill((
            sapid::checkbox()
                .class("preference-check")
                .label(i18n::bind("Use application connection defaults"))
                .checked(ui.state.map(|s| s.editor.connection.is_none()))
                .on_checked_change(move |v| {
                    inherit.state.revise(|s| {
                        s.editor.connection = if v {
                            None
                        } else {
                            Some(s.config.settings.connection)
                        }
                    })
                }),
            label("Connection timeout"),
            row().class("choice-list").fill(
                each(vec![5u32, 15, 30, 60, 120]).key(|n| *n).children({
                    let ui = ui.clone();
                    move |item| {
                        let n = item.value.get();
                        let set = ui.clone();
                        sapid::button()
                            .class(ui.state.map(move |s| {
                                if s.editor
                                    .connection
                                    .unwrap_or(s.config.settings.connection)
                                    .timeout_seconds
                                    == n
                                {
                                    "choice active"
                                } else {
                                    "choice"
                                }
                                .to_owned()
                            }))
                            .enabled(ui.state.map(|s| s.editor.connection.is_some()))
                            .label(i18n::bind(format!("{n} seconds")))
                            .on_press(move || {
                                set.state.revise(|s| {
                                    if let Some(v) = &mut s.editor.connection {
                                        v.timeout_seconds = n;
                                    }
                                })
                            })
                    }
                }),
            ),
            label("SSH keepalive"),
            row()
                .class("choice-list")
                .fill(each(vec![0u32, 15, 30, 60]).key(|n| *n).children({
                    let ui = ui.clone();
                    move |item| {
                        let n = item.value.get();
                        let set = ui.clone();
                        sapid::button()
                            .class(ui.state.map(move |s| {
                                if s.editor
                                    .connection
                                    .unwrap_or(s.config.settings.connection)
                                    .keepalive_seconds
                                    == n
                                {
                                    "choice active"
                                } else {
                                    "choice"
                                }
                                .to_owned()
                            }))
                            .enabled(ui.state.map(|s| s.editor.connection.is_some()))
                            .label(i18n::bind(if n == 0 {
                                "Disabled".into()
                            } else {
                                format!("{n} seconds")
                            }))
                            .on_press(move || {
                                set.state.revise(|s| {
                                    if let Some(v) = &mut s.editor.connection {
                                        v.keepalive_seconds = n;
                                    }
                                })
                            })
                    }
                })),
        ))
}
pub(crate) fn server_tabs(ui: Ui) -> impl Widget {
    row().class("server-editor-tabs").fill(
        each(vec![
            (0, "General"),
            (1, "Files"),
            (2, "Connection"),
            (3, "Labels"),
        ])
        .key(|(id, _)| *id)
        .children(move |item| {
            let (id, name) = item.value.get();
            let set = ui.clone();
            sapid::button()
                .class(ui.state.map(move |s| {
                    if s.server_editor_tab == id {
                        "module-tab active"
                    } else {
                        "module-tab"
                    }
                    .to_owned()
                }))
                .label(i18n::bind(name))
                .on_press(move || set.state.revise(|s| s.server_editor_tab = id))
        }),
    )
}
fn server_policy(ui: Ui, direction: TransferDirection) -> impl Widget {
    row().class("choice-list").fill(
        each(vec![
            (None, "Inherit application default"),
            (Some(ExistingFileAction::Ask), "Ask every time"),
            (Some(ExistingFileAction::Overwrite), "Overwrite"),
            (Some(ExistingFileAction::Skip), "Skip"),
        ])
        .key(|(a, _)| format!("{a:?}"))
        .children(move |item| {
            let (action, label) = item.value.get();
            let change = ui.clone();
            sapid::button()
                .class(ui.state.map(move |s| {
                    if s.config
                        .transfer_rules
                        .iter()
                        .find(|r| {
                            Some(&r.server_id) == s.selected.as_ref() && r.direction == direction
                        })
                        .map(|r| r.action)
                        == action
                    {
                        "choice active"
                    } else {
                        "choice"
                    }
                    .to_owned()
                }))
                .label(i18n::bind(label))
                .on_press(move || {
                    let result = change.state.revise(|s| {
                        let id = s
                            .selected
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("Select a server first"))?;
                        s.config.set_transfer_rule(&id, direction, action)?;
                        s.session_rules.remove(&direction);
                        s.queue_rules.retain(|(_, d), _| *d != direction);
                        Ok::<_, anyhow::Error>(())
                    });
                    match result {
                        Ok(()) => change.save(),
                        Err(e) => change.state.revise(|s| s.dialog_error = e.to_string()),
                    }
                })
        }),
    )
}
pub(crate) fn server_transfer_dialog(ui: Ui) -> impl Widget {
    let state = ui.state;
    when(
        state.map(|s| s.dialog == Some(Dialog::TransferDefaults)),
        move || {
            let close = ui.clone();
            let restore = ui.clone();
            modal_with_header(ui.clone(),"Transfer defaults",settings::restore_button(false, "Use application transfer defaults", move || {
                if let Some(server) = state.with(|s| s.selected.clone()) {
                    restore.reset_conflict_rules(&server);
                }
            }),col().class("preferences-stack").fill((
        text().class("section-title").content(state.map(|s|s.config.servers.iter().find(|p|Some(&p.id)==s.selected.as_ref()).map(|p|p.name.clone()).unwrap_or_default())),
        text().class("subtle").content(i18n::bind("Changes are saved for this server and affect future transfers. Active transfers keep their current choices.")),
        label("Upload policy"),server_policy(ui.clone(),TransferDirection::Upload),label("Download policy"),server_policy(ui.clone(),TransferDirection::Download),
    )),sapid::button().class("button primary").label(i18n::bind("Done")).on_press(move||close.close_dialog()))
        },
    )
}
