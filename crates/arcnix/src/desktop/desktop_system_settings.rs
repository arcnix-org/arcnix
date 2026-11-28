use std::time::Instant;

use arcnix_core::desktop_customization::DockPosition;
use arcnix_core::desktop_time_zone::{TimeZoneChoice, TimeZones};
use arcnix_core::server_apps::{ClockFormat, DesktopClock};

use crate::desktop_customization_ui::settings_row;
use crate::i18n::desktop as i18n;
use crate::server_apps_worker::{Action as AppAction, Reply};
use crate::*;

#[derive(Clone, Default)]
pub(crate) struct TimeZoneState {
    pub pending: Option<u64>,
    pub write_started: Option<Instant>,
    observed: Option<TimeZones>,
    error: String,
}

pub(crate) fn language(s: &State) -> Language {
    server_apps_ui::current(s)
        .language
        .unwrap_or(s.config.settings.language)
}

impl Ui {
    fn change_desktop_language(&self, value: Language) {
        let mut prefs = self.state.with(server_apps_ui::desired);
        if prefs.language != Some(value) {
            prefs.language = Some(value);
            self.queue_desktop_preferences(prefs);
        }
    }

    pub(crate) fn read_desktop_time_zone(&self) {
        if self
            .state
            .with(|s| s.connected && s.server_apps.time_zone.pending.is_none())
        {
            self.request_desktop_time_zone(AppAction::ReadTimeZone);
        }
    }

    fn request_desktop_time_zone(&self, action: AppAction) {
        let write_started = matches!(action, AppAction::SetTimeZone(_)).then(Instant::now);
        let serial = self.send(Action::ServerApps(action));
        self.state.revise(|s| {
            let zone = &mut s.server_apps.time_zone;
            zone.pending = Some(serial);
            zone.write_started = write_started;
            zone.error.clear();
        });
    }

    fn change_desktop_time_zone(&self, value: String) {
        let allowed = self.state.with(|s| {
            s.dialog == Some(Dialog::DesktopTimeZone)
                && s.server_apps.time_zone.pending.is_none()
                && system_time_zone(s).is_some_and(|zone| {
                    zone.name.as_deref() == Some(value.as_str())
                        || zone.choices.iter().any(|choice| choice.name == value)
                })
        });
        if !allowed {
            return;
        }
        self.close_dialog();
        if self
            .state
            .with(|s| selected_time_zone(s) == Some(value.as_str()))
        {
            return;
        }
        self.request_desktop_time_zone(AppAction::SetTimeZone(value));
    }
}

pub(crate) fn time_zone_reply(ui: &Ui, result: std::result::Result<Reply, String>) {
    let refresh = ui.state.revise(|s| {
        let zone = &mut s.server_apps.time_zone;
        zone.pending = None;
        let writing = zone.write_started.take().is_some();
        match result {
            Ok(Reply::TimeZone(observed)) => {
                let refresh = zone
                    .observed
                    .as_ref()
                    .is_some_and(|prior| prior.name != observed.name);
                zone.observed = Some(observed);
                refresh
            }
            Err(error) => {
                if !writing {
                    zone.observed = None;
                }
                zone.error = error;
                false
            }
            _ => {
                zone.error = "Invalid server time zone response".into();
                false
            }
        }
    });
    if refresh {
        ui.refresh_desktop_clock();
    }
}

pub(crate) fn region(ui: Ui) -> impl Widget {
    col().class("desktop-settings-pane desktop-region").fill((
        col().class("desktop-settings-card").fill((
            settings::section_heading("Desktop language", true, settings::restore_desktop_defaults(&ui, "Use application language", |prefs| prefs.language = None)),
            settings::select(&ui, true, "Desktop language",
                [(Language::English, "English"), (Language::SimplifiedChinese, "简体中文")],
                language, server_apps_ui::preferences_ready, |ui, lang| ui.change_desktop_language(lang)),
            text().class("subtle").content(i18n::bind(
                "Changes the display language of this Arcnix desktop only. The server manages its system language separately.",
            )),
        )),
        col().class("desktop-settings-card").fill((
            settings::section_heading("Numbers and file sizes", true, preferences_ui::restore_file_size_defaults(&ui, true)),
            preferences_ui::size_options(&ui, true),
        )),
        settings::feedback(&ui, |s| s.settings.error.clone(), "error"),
    ))
}

fn time_format_choices(ui: &Ui) -> impl Widget + use<> {
    settings::select(
        ui,
        true,
        "Time format",
        [
            (ClockFormat::TwentyFourHour, "24-hour"),
            (ClockFormat::TwelveHour, "AM / PM"),
        ],
        clock_format,
        server_apps_ui::preferences_ready,
        |ui, value| {
            let mut prefs = ui.state.with(server_apps_ui::desired);
            if prefs.clock.format != value {
                prefs.clock.format = value;
                ui.queue_desktop_preferences(prefs);
            }
        },
    )
}

fn clock_switch(
    ui: &Ui,
    label: &'static str,
    checked: impl Fn(&State) -> bool + 'static,
    update: fn(&mut DesktopClock, bool),
) -> sapid::Switch {
    let state = ui.state;
    let ui = ui.clone();
    sapid::switch()
        .size(sapid::Size::Small)
        .name(i18n::bind(label))
        .checked(state.map(checked))
        .enabled(state.map(server_apps_ui::preferences_ready))
        .on_checked_change(move |value| {
            let mut prefs = state.with(server_apps_ui::desired);
            update(&mut prefs.clock, value);
            ui.queue_desktop_preferences(prefs);
        })
}

pub(crate) fn side_dock(s: &State) -> bool {
    desktop_customization_ui::appearance(s).dock.position != DockPosition::Bottom
}

fn clock_format(s: &State) -> ClockFormat {
    match server_apps_ui::current(s).clock.format {
        ClockFormat::Automatic if !side_dock(s) && language(s) == Language::English => {
            ClockFormat::TwelveHour
        }
        ClockFormat::Automatic => ClockFormat::TwentyFourHour,
        format => format,
    }
}

fn clock_shows_seconds(s: &State) -> bool {
    server_apps_ui::current(s)
        .clock
        .show_seconds
        .unwrap_or_else(|| !side_dock(s))
}

pub(crate) fn clock_options_data(s: &State) -> String {
    let clock = server_apps_ui::current(s).clock;
    let format = if clock_format(s) == ClockFormat::TwelveHour {
        "h12"
    } else {
        "h23"
    };
    let locale = match language(s) {
        Language::English => "en-US",
        Language::SimplifiedChinese => "zh-CN",
    };
    format!(
        "{locale},{format},{},{},{}",
        clock.show_weekday,
        clock.show_date,
        clock_shows_seconds(s),
    )
}

pub(crate) fn clock_view(class: &'static str) -> impl Widget {
    col().class(class).name(i18n::bind("Server time")).fill((
        text().class("desktop-panel-time").content(""),
        text().class("desktop-panel-weekday").content(""),
        text().class("desktop-panel-date").content(""),
        text().class("desktop-clock-tooltip").content(""),
    ))
}

fn system_time_zone(s: &State) -> Option<&TimeZones> {
    s.server_apps
        .time_zone
        .observed
        .as_ref()
        .filter(|zone| zone.system && zone.name.is_some())
}

fn selected_time_zone(s: &State) -> Option<&str> {
    system_time_zone(s)?.name.as_deref()
}

fn time_zone_city(zone: &str) -> &str {
    zone.rsplit('/').next().unwrap_or(zone)
}

fn time_zone_choices(s: &State) -> Vec<TimeZoneChoice> {
    let Some(observed) = system_time_zone(s) else {
        return Vec::new();
    };
    let mut choices = observed.choices.clone();
    if let Some(name) = &observed.name
        && !choices.iter().any(|choice| &choice.name == name)
    {
        choices.push(TimeZoneChoice {
            name: name.clone(),
            country: None,
        });
    }
    choices.sort_by(|a, b| {
        time_zone_city(&a.name)
            .cmp(time_zone_city(&b.name))
            .then_with(|| a.name.cmp(&b.name))
    });
    choices
}

fn time_zone_offset(s: &State) -> String {
    let Some((_, clock)) = &s.server_apps.clock else {
        return "Server clock unavailable".to_owned();
    };
    let offset = clock.offset_minutes.unsigned_abs();
    let sign = if clock.offset_minutes < 0 { '-' } else { '+' };
    format!("UTC{sign}{:02}:{:02}", offset / 60, offset % 60)
}

fn time_zone_summary(s: &State) -> String {
    if s.server_apps.clock.is_some() {
        format!("Server time zone · {}", time_zone_offset(s))
    } else {
        time_zone_offset(s)
    }
}

fn time_zone_controls(ui: Ui) -> impl Widget {
    let state = ui.state;
    let label = state.map(|s| selected_time_zone(s).unwrap_or_default().to_owned());
    col().class("desktop-time-zone-controls").fill((
        sapid::button()
            .class("button desktop-time-zone-current")
            .name(label)
            .fill((text().content(label), icons::glyph(Icon::ChevronRight)))
            .enabled(state.map(|s| {
                let zone = &s.server_apps.time_zone;
                zone.pending.is_none()
                    && system_time_zone(s).is_some_and(|observed| !observed.choices.is_empty())
            }))
            .on_press(move || ui.open_dialog(Dialog::DesktopTimeZone)),
        text()
            .class("subtle time-zone-summary")
            .content(i18n::bind(state.map(time_zone_summary))),
        text().class("error").content(i18n::bind(
            state.map(|s| s.server_apps.time_zone.error.clone()),
        )),
    ))
}

pub(crate) fn time_zone_dialog(ui: Ui) -> impl Widget {
    let state = ui.state;
    when(
        state.map(|s| s.dialog == Some(Dialog::DesktopTimeZone)),
        move || {
            let select = ui.clone();
            modal(
                ui.clone(),
                "Select time zone",
                (
                    row().class("time-zone-search").fill((
                        icons::glyph(Icon::Search),
                        input()
                            .class("time-zone-query")
                            .name(i18n::bind("Search cities and time zones")),
                    )),
                    col().class("time-zone-options").fill((
                        each(state.map(time_zone_choices))
                            .key(|choice| choice.name.clone())
                            .children(move |item| {
                                let choice = item.value.get();
                                let name = choice.name;
                                let selected = name.clone();
                                let label = name.clone();
                                let action = select.clone();
                                sapid::button()
                                    .class("time-zone-option")
                                    .name(label)
                                    .pressed(state.map(move |s| {
                                        selected_time_zone(s) == Some(selected.as_str())
                                    }))
                                    .fill((
                                        text()
                                            .class("time-zone-place")
                                            .content(time_zone_city(&name).replace('_', " ")),
                                        text().class("time-zone-detail").content(name.clone()),
                                        text().class("time-zone-local-time").content(""),
                                        text()
                                            .class("time-zone-country-code hidden")
                                            .content(choice.country.unwrap_or_default()),
                                    ))
                                    .on_press(move || action.change_desktop_time_zone(name.clone()))
                            }),
                        text()
                            .class("time-zone-empty hidden")
                            .content(i18n::bind("No matching time zones")),
                    )),
                ),
                (),
            )
        },
    )
}

pub(crate) fn date_time(ui: Ui) -> impl Widget {
    let state = ui.state;
    let choose_zone = ui.clone();
    col()
        .class("desktop-settings-pane desktop-date-time")
        .fill((
            col()
                .class("desktop-settings-card desktop-settings-rows")
                .fill((
                    settings_row(
                        "Current date and time",
                        clock_view("desktop-panel-clock desktop-time-preview"),
                    ),
                    when(state.map(|s| system_time_zone(s).is_some()), move || {
                        settings_row("Time zone", time_zone_controls(choose_zone.clone()))
                    }),
                )),
            col()
                .class("desktop-settings-card desktop-settings-rows")
                .fill((
                    settings::section_heading(
                        "Time format",
                        true,
                        settings::restore_desktop_defaults(
                            &ui,
                            "Restore default time format",
                            |prefs| prefs.clock.format = ClockFormat::default(),
                        ),
                    ),
                    row()
                        .class("desktop-setting-controls choice-list")
                        .fill(time_format_choices(&ui)),
                )),
        ))
}

pub(crate) fn dock_clock_settings(ui: Ui) -> impl Widget {
    col()
        .class("dock-clock-settings desktop-settings-rows")
        .fill((
            settings_row("Time format", time_format_choices(&ui)),
            settings_row(
                "Weekday",
                row().class("dock-clock-weekday").fill(clock_switch(
                    &ui,
                    "Weekday",
                    |s| server_apps_ui::current(s).clock.show_weekday,
                    |clock, show| clock.show_weekday = show,
                )),
            ),
            settings_row(
                "Date",
                clock_switch(
                    &ui,
                    "Date",
                    |s| server_apps_ui::current(s).clock.show_date,
                    |clock, show| clock.show_date = show,
                ),
            ),
            settings_row(
                "Seconds",
                row().class("dock-clock-seconds").fill(clock_switch(
                    &ui,
                    "Seconds",
                    clock_shows_seconds,
                    |clock, show| clock.show_seconds = Some(show),
                )),
            ),
        ))
}
