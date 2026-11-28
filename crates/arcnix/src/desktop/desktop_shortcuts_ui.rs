use arcnix_core::desktop_shortcuts::{
    Action as Shortcut, GROUPS, Keymap, Preset, Profile, Scheme, Scope as ShortcutScope,
};
use serde::Deserialize;

use crate::i18n::desktop as i18n;
use crate::i18n::desktop::close_action;
use crate::*;

pub(crate) fn keys(s: &State) -> Keymap<'_> {
    server_apps_ui::current(s)
        .shortcuts
        .resolved(s.config.settings.shortcut_scheme.preset())
}

pub(crate) fn menu_hint(s: &State, action: Shortcut) -> String {
    let keys = keys(s);
    keys.chords(action)
        .next()
        .map(|chord| keys.preset.display(chord))
        .unwrap_or_default()
}

pub(super) fn can_run(s: &State, action: Shortcut) -> bool {
    if s.workbench.overview.is_some() {
        return false;
    }
    match action {
        Shortcut::NextApp | Shortcut::PreviousApp => !s.workbench.apps.is_empty(),
        Shortcut::Maximize | Shortcut::Minimize => {
            server_desktop::app_id(s.workspace_page).is_some()
        }
        Shortcut::Fullscreen | Shortcut::ShowDesktop => true,
        Shortcut::CloseWindow | Shortcut::CloseTab => s.workbench.window_visible(s.workspace_page),
        Shortcut::ExitFullscreen => {
            server_desktop::reader_fullscreen(s, s.workspace_page) || server_desktop::fullscreen(s)
        }
        Shortcut::OpenFiles => files_tabs::available(s),
        Shortcut::FilesDuplicateWindow => s.workspace_page.is_files() && files_tabs::available(s),
        _ => false,
    }
}

pub(super) fn desktop_active(s: &State) -> bool {
    server_desktop::visible(s) && s.dialog.is_none() && s.fingerprint.is_none()
}

fn editing(s: &State) -> bool {
    desktop_active(s)
        && s.workspace_page == WorkspacePage::Settings
        && desktop_customization_ui::settings_page(s)
            == desktop_customization_ui::SettingsPage::Shortcuts
        && server_apps_ui::preferences_ready(s)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Packet {
    epoch: u64,
    intent: Intent,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Intent {
    Recording {
        active: bool,
    },
    Run {
        action: Shortcut,
        preset: Preset,
        chord: String,
    },
    Record {
        action: Shortcut,
        chord: String,
        preset: Preset,
        add: bool,
        previous: Option<String>,
    },
}

fn receive(ui: &Ui, value: &str) {
    if value.len() > 512 {
        return;
    }
    let Ok(packet) = serde_json::from_str::<Packet>(value) else {
        return;
    };
    if !ui.state.with(|s| s.epoch == packet.epoch) {
        return;
    }
    match packet.intent {
        Intent::Recording { active } => {
            ui.state
                .revise(|s| s.server_apps.recording_shortcut = active && editing(s));
        }
        _ if !ui.state.with(desktop_active) => {}
        Intent::Run {
            action,
            preset,
            chord,
        } => {
            if !ui.state.with(|s| {
                let keys = keys(s);
                keys.preset == preset
                    && can_run(s, action)
                    && keys.chords(action).any(|key| key == chord)
            }) {
                return;
            }
            run(ui, action);
        }
        Intent::Record {
            action,
            chord,
            preset,
            add,
            previous,
        } => {
            if ui.state.with(|s| keys(s).preset == preset) {
                record(ui, action, chord, add, previous);
            }
        }
    }
}

pub(super) fn run(ui: &Ui, action: Shortcut) {
    match action {
        Shortcut::NextApp => ui.cycle_desktop_apps(false),
        Shortcut::PreviousApp => ui.cycle_desktop_apps(true),
        Shortcut::Maximize => ui.toggle_desktop_maximize(),
        Shortcut::Minimize => ui.minimize_desktop_window(),
        Shortcut::ExitFullscreen
            if ui
                .state
                .with(|s| server_desktop::reader_fullscreen(s, s.workspace_page)) =>
        {
            ui.state.revise(|s| match s.workspace_page.base() {
                WorkspacePage::PdfReader => {
                    s.pdf
                        .selected_mut(s.workspace_page.tab_window().unwrap_or_default())
                        .fullscreen = false
                }
                WorkspacePage::ImageViewer => s.images.fullscreen = false,
                _ => {}
            });
        }
        Shortcut::Fullscreen | Shortcut::ExitFullscreen => ui.toggle_desktop_fullscreen(),
        Shortcut::ShowDesktop => ui.toggle_show_desktop(),
        Shortcut::CloseWindow | Shortcut::CloseTab => {
            ui.close_active_desktop(action == Shortcut::CloseTab)
        }
        Shortcut::OpenFiles => files_tabs::new_window(ui, files_tabs::WindowStart::Default),
        Shortcut::FilesDuplicateWindow => {
            files_tabs::new_window(ui, files_tabs::WindowStart::Current)
        }
        _ => {}
    }
}

fn record(ui: &Ui, action: Shortcut, chord: String, add: bool, previous: Option<String>) {
    change(ui, |keys, preset| {
        if cfg!(windows)
            && preset == Preset::Windows
            && !matches!(
                (action, chord.as_str()),
                (Shortcut::ShowDesktop, "Meta+D") | (Shortcut::CloseWindow, "Alt+F4")
            )
            && (chord.split('+').any(|part| part == "Meta")
                || matches!(
                    chord.as_str(),
                    "Alt+Tab"
                        | "Alt+Shift+Tab"
                        | "Alt+F4"
                        | "Ctrl+Alt+Delete"
                        | "Ctrl+Escape"
                        | "Ctrl+Shift+Escape"
                        | "Alt+Space"
                        | "Ctrl+Alt+Tab"
                ))
        {
            anyhow::bail!("This shortcut is reserved by Windows.");
        }
        // Fixed editor/file commands use the primary modifier of the selected scheme.
        let portable = if preset == Preset::MacOs {
            chord
                .replace("Shift+Meta+", "Ctrl+Shift+")
                .replace("Meta+", "Ctrl+")
        } else {
            chord.clone()
        };
        let occupied = match action.scope() {
            ShortcutScope::Files => matches!(
                portable.as_str(),
                "Ctrl+W"
                    | "Ctrl+F"
                    | "F2"
                    | "F5"
                    | "Delete"
                    | "Shift+Delete"
                    | "Alt+Enter"
                    | "Ctrl+Shift+C"
                    | "Shift+F10"
            ),
            ShortcutScope::Code => matches!(
                portable.as_str(),
                "Ctrl+P"
                    | "Ctrl+F"
                    | "Ctrl+H"
                    | "Ctrl+Z"
                    | "Ctrl+Y"
                    | "Ctrl+A"
                    | "Ctrl+X"
                    | "Ctrl+C"
                    | "Ctrl+V"
                    | "Ctrl+Shift+S"
                    | "Ctrl+Shift+P"
                    | "Ctrl+K"
                    | "F2"
            ),
            _ => false,
        };
        anyhow::ensure!(!occupied, "This shortcut is already in use.");
        if let Some(previous) = previous {
            keys.replace_extra(preset, action, &previous, chord)
        } else if add {
            keys.add(preset, action, chord)
        } else {
            keys.set(preset, action, Some(chord))
        }
    });
}

pub(crate) fn bridge(ui: Ui) -> impl Widget {
    let state = ui.state;
    fragment((
        text()
            .class("desktop-shortcut-state hidden")
            .content(state.map(|s| {
                let keys = keys(s);
                let actions = GROUPS
                    .iter()
                    .flat_map(|(_, definitions)| definitions.iter());
                let bindings = actions
                    .clone()
                    .map(|definition| {
                        (
                            definition.action,
                            keys.chords(definition.action).collect::<Vec<_>>(),
                        )
                    })
                    .collect::<std::collections::BTreeMap<_, _>>();
                let commands = actions
                    .filter(|definition| can_run(s, definition.action))
                    .map(|definition| definition.action)
                    .collect::<Vec<_>>();
                serde_json::json!({
                    "epoch": s.epoch,
                    "active": desktop_active(s),
                    "preset": keys.preset,
                    "windowsHost": cfg!(windows),
                    "bindings": bindings,
                    "commands": commands,
                })
                .to_string()
            })),
        input()
            .class("desktop-shortcut-input hidden")
            .value("")
            .on(value_changed, move |event| {
                if let Event::ValueChanged { value, .. } = event {
                    receive(&ui, &value);
                }
            }),
    ))
}

fn change(ui: &Ui, apply: impl FnOnce(&mut Profile, Preset) -> anyhow::Result<()>) {
    let Some(mut prefs) = ui
        .state
        .with(|s| editing(s).then(|| server_apps_ui::desired(s)))
    else {
        return;
    };
    let preset = ui.state.with(|s| keys(s).preset);
    let result = apply(prefs.shortcuts.profile_mut(preset), preset);
    ui.state.revise(|s| {
        s.server_apps.shortcut_error = result
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default();
    });
    if result.is_ok() && ui.state.with(|s| server_apps_ui::current(s) != &prefs) {
        ui.queue_desktop_preferences(prefs);
    }
}

fn matching_shortcut(s: &State, query: &str, group: &str, label: &str, action: Shortcut) -> bool {
    if query.is_empty() {
        return true;
    }
    let language = desktop_system_settings::language(s);
    let text = format!(
        "{} {label} {} {}",
        crate::i18n::translate_for(group, Language::English),
        crate::i18n::translate_for(group, language),
        crate::i18n::translate_for(label, language)
    )
    .to_lowercase();
    let keys = keys(s);
    let chords: Vec<_> = keys
        .binding(action)
        .into_iter()
        .chain(keys.additions(action).iter().map(String::as_str))
        .map(|chord| keys.preset.display(chord).to_lowercase())
        .collect();
    query.split_whitespace().all(|word| {
        text.contains(word)
            || chords.iter().any(|chord| {
                if word.contains('+') {
                    chord == word || (word.ends_with('+') && chord.starts_with(word))
                } else {
                    chord.split('+').any(|key| key.starts_with(word))
                }
            })
    })
}

pub(crate) fn controls(ui: Ui) -> impl Widget {
    widget(move |scope| {
        let query = scope.cage(String::new());
        let field = scope.cage(None);
        let focus = scope.clone();
        let search = row()
            .class("desktop-settings-search shortcut-search")
            .fill((
                icons::glyph(Icon::Search),
                sapid::text_field()
                    .unstyled()
                    .name(i18n::bind("Search shortcuts"))
                    .placeholder(i18n::bind("Search shortcuts"))
                    .node_ref(field)
                    .value(query)
                    .on_value_change(move |value| query.revise(|current| *current = value)),
                when(query.map(|q| !q.is_empty()), move || {
                    let focus = focus.clone();
                    close_action("Clear search")
                        .class("desktop-settings-search-clear")
                        .on_press(move || {
                            query.revise(String::clear);
                            if let Some(id) = field.get() {
                                let _ = focus.action(glory::render::Action::Focus(id));
                            }
                        })
                }),
            ));
        let terms = scope.bond(move || {
            query
                .get()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
                .replace(" +", "+")
                .replace("+ ", "+")
        });
        shortcut_controls(ui.clone(), terms, search).show_in(scope)
    })
}

fn shortcut_controls(ui: Ui, terms: Bond<String>, search: impl Widget) -> impl Widget {
    let state = ui.state;
    let capture = ui.clone();
    let error = text()
        .class("error desktop-shortcut-error")
        .content(i18n::bind(
            state.map(|s| s.server_apps.shortcut_error.clone()),
        ));
    let pane_class = state.map(move |_| {
        if terms.with(String::is_empty) {
            "desktop-settings-pane desktop-custom-keys"
        } else {
            "desktop-settings-pane desktop-custom-keys shortcut-filtered"
        }
        .to_owned()
    });
    col().class(pane_class).fill((
        row().class("section-heading shortcut-search-heading").fill((
            text().class("section-title").content(i18n::bind("Keyboard shortcuts")),
            help_ui::tip("Change shortcut", "Click a shortcut to change it, or + to add another. Esc cancels. Changes to this preset are saved on the server."),
            search,
        )),
        col().class("desktop-settings-card desktop-shortcut-scheme").fill((
            row().class("desktop-setting-row").fill((
                text().class("section-title").content(i18n::bind("Shortcut scheme")),
                row().class("desktop-setting-controls").fill(settings::select(&ui, true, "Shortcut scheme",
                    [(Scheme::Auto, "Auto"), (Scheme::Windows, "Windows"), (Scheme::MacOs, "macOS")],
                    |s| s.config.settings.shortcut_scheme, |_| true, |ui, scheme| {
                        ui.state.revise(|s| {
                            s.config.settings.shortcut_scheme = scheme;
                            s.server_apps.recording_shortcut = false;
                            s.server_apps.shortcut_error.clear();
                        });
                        ui.save();
                    })),
            )),
            when(state.map(|s| s.config.settings.shortcut_scheme == Scheme::Auto), move || {
                text().class("subtle").content(i18n::bind(match Scheme::Auto.preset() {
                    Preset::Windows => "Active preset: Windows",
                    Preset::MacOs => "Active preset: macOS",
                }))
            }),
        )),
        when(state.map(|s| cfg!(windows) && keys(s).preset == Preset::Windows), move || {
            let capture = capture.clone();
            col().class("desktop-settings-card shortcut-capture-card").fill((
                sapid::checkbox().class("preference-check desktop-capture-shortcuts")
                    .name(i18n::bind("Use Windows shortcuts on the desktop"))
                    .label(i18n::bind("Use Windows shortcuts on the desktop"))
                    .checked(state.map(|s| s.config.settings.capture_desktop_shortcuts))
                    .on_checked_change(move |on| {
                        state.revise(|s| s.config.settings.capture_desktop_shortcuts = on);
                        capture.save();
                    }),
                text().class("subtle").content(i18n::bind("When the desktop is active, Win+D shows the desktop. Press it again to restore windows. Alt+F4 closes the active window and Ctrl+F4 closes the active tab. Alt+Tab and Alt+Shift+Tab switch apps, Win+E opens Files, and Win+Up / Win+Down maximize or minimize a window. Changing an action’s shortcut also releases its Windows shortcut.")),
            ))
        }),
        error,
        text().class("subtle shortcut-search-empty").content(i18n::bind("No matching shortcuts.")),
        fragment(GROUPS.iter().map(move |(scope, definitions)| {
            let scope = *scope;
            let (title, restore_label) = match scope {
                ShortcutScope::Desktop => ("Desktop", "Restore default shortcuts for Desktop"),
                ShortcutScope::Terminal => ("Terminal", "Restore default shortcuts for Terminal"),
                ShortcutScope::Files => (server_desktop::app_label(WorkspacePage::Files(0)), "Restore default shortcuts for Files"),
                ShortcutScope::Code => (server_desktop::app_label(WorkspacePage::Code), "Restore default shortcuts for Code Editor"),
            };
            let restore = ui.clone();
            col().class("desktop-settings-card desktop-shortcut-group").fill((
                settings::section_heading(title, true, settings::restore_button(true, restore_label, move || {
                    change(&restore, |keys, _| {
                        keys.reset_scope(scope);
                        Ok(())
                    });
                }).enabled(state.map(move |s| editing(s) && definitions.iter().any(|definition| keys(s).modified(definition.action))))),
                fragment(definitions.iter().map(|definition| {
                    let action = definition.action;
                    let label = definition.label;
                    let enable = ui.clone();
                    let restore = ui.clone();
                    let modified = state.map(move |s| keys(s).modified(action));
                    row().class(state.map(move |s| {
                        if terms.with(|q| matching_shortcut(s, q, title, label, action)) {
                            "desktop-key-binding".to_owned()
                        } else {
                            "desktop-key-binding hidden".to_owned()
                        }
                    })).fill((
                        row().class("desktop-shortcut-label").fill((
                            sapid::checkbox()
                                .class("preference-check desktop-shortcut-enabled")
                                .name(i18n::bind(label))
                                .label(i18n::bind(label))
                                .enabled(state.map(editing))
                                .checked(state.map(move |s| keys(s).enabled(action)))
                                .on_checked_change(move |on| change(&enable, |keys, preset| {
                                    keys.set_enabled(preset, action, on)
                                })),
                            when(modified, || {
                                text().class("desktop-shortcut-modified").content(i18n::bind("Customized"))
                            }),
                        )),
                        row().class("desktop-key-controls").fill((
                            text().class("desktop-shortcut-id hidden").content(
                                serde_json::to_string(&action).expect("valid desktop shortcut action"),
                            ),
                            sapid::button()
                                .class("button desktop-shortcut-record")
                                .name(i18n::bind(format!("Change shortcut: {label}")))
                                .enabled(state.map(editing))
                                .fill((
                                    text().class("desktop-key-combination").content(i18n::bind(state.map(move |s| {
                                        keys(s).binding(action).map(|chord| keys(s).preset.display(chord)).unwrap_or_else(|| "Unassigned".into())
                                    }))),
                                    text().class("desktop-key-recording").content(i18n::bind("Press shortcut…")),
                                )),
                            each(state.map(move |s| keys(s).additions(action).to_vec()))
                                .key(|chord| chord.clone())
                                .children({
                                    let ui = ui.clone();
                                    move |item| {
                                        let chord = item.value.get();
                                        let remove = ui.clone();
                                        let old = chord.clone();
                                        row().class("desktop-shortcut-extra").fill((
                                            text().class("desktop-shortcut-previous hidden").content(chord.clone()),
                                            sapid::button().class("button desktop-shortcut-record")
                                                .name(i18n::bind(format!("Change shortcut: {label} ({chord})")))
                                                .enabled(state.map(editing))
                                                .fill((
                                                    text().class("desktop-key-combination").content(state.map(move |s| keys(s).preset.display(&chord))),
                                                    text().class("desktop-key-recording").content(i18n::bind("Press shortcut…")),
                                                )),
                                            close_action("Remove shortcut").class("icon-button")
                                                .enabled(state.map(editing))
                                                .on_press(move || change(&remove, |keys, _| { keys.remove_extra(action, &old); Ok(()) })),
                                        ))
                                    }
                                }),
                            sapid::button().class("button desktop-shortcut-record desktop-shortcut-add")
                                .name(i18n::bind(format!("Add shortcut: {label}")))
                                .enabled(state.map(editing))
                                .fill((
                                    text().class("desktop-key-combination").content("+"),
                                    text().class("desktop-key-recording").content(i18n::bind("Press shortcut…")),
                                )),
                            when(modified, move || {
                                let restore = restore.clone();
                                settings::restore_button(true, state.map(move |s| {
                                    let preset = keys(s).preset;
                                    let chord = preset.display(preset.default_key(action));
                                    format!("Restore shortcut: {label} (Default: {chord})")
                                }), move || change(&restore, |keys, preset| keys.reset(preset, action)))
                                .enabled(state.map(editing))
                            }),
                        )),
                    ))
                }).collect::<Vec<_>>()),
            ))
        }).collect::<Vec<_>>()),
    ))
}
