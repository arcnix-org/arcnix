use glory::render::NodeId;

use crate::i18n::desktop as i18n;
use crate::*;

fn matching_apps(s: &State, query: &str) -> Vec<WorkspacePage> {
    let query = query.to_lowercase();
    let language = desktop_system_settings::language(s);
    server_desktop::available_apps(s)
        .into_iter()
        .filter(|&page| {
            let label = server_desktop::app_label(page);
            let english = crate::i18n::translate_for(label, Language::English).to_lowercase();
            let localized = crate::i18n::translate_for(label, language).to_lowercase();
            query
                .split_whitespace()
                .all(|word| english.contains(word) || localized.contains(word))
        })
        .collect()
}

fn launch(ui: &Ui, page: WorkspacePage) {
    if !ui.state.with(|s| {
        server_desktop::visible(s)
            && matches!(s.dialog, None | Some(Dialog::DesktopApps))
            && server_desktop::available_apps(s).contains(&page)
    }) {
        return;
    }
    if ui.state.with(|s| s.dialog == Some(Dialog::DesktopApps)) {
        ui.close_dialog();
    }
    if ui.state.with(|s| s.dialog.is_none()) {
        ui.open_desktop_app(page);
        if page.is_files() {
            files_tabs::leave_apps(ui);
        }
    }
}

fn search(ui: Ui, query: Cage<String>, field: Option<Cage<Option<NodeId>>>) -> impl Widget {
    let state = ui.state;
    let mut input = input()
        .name(i18n::bind("Search apps"))
        .on(value_changed, move |event| {
            if let Event::ValueChanged { value, .. } = event {
                query.revise(|current| *current = value);
            }
        })
        .on(commit, move |event| {
            if let Event::Commit { value, .. } = event
                && !value.trim().is_empty()
                && let Some(page) = state.with(|s| matching_apps(s, &value).first().copied())
            {
                launch(&ui, page);
            }
        });
    if let Some(field) = field {
        input = input.node_ref(field);
    }
    row()
        .class("desktop-app-catalog-search")
        .fill((icons::glyph(Icon::Search), input))
}

fn catalog(ui: Ui, query: Cage<String>) -> impl Widget {
    widget(move |scope| {
        let state = ui.state;
        let matches = scope.bond(move || state.with(|s| matching_apps(s, &query.get())));
        col()
            .class("desktop-app-catalog-scroll")
            .fill((
                col().class("desktop-app-catalog-grid").fill(
                    each(matches)
                        .key(|page| server_desktop::app_class(*page))
                        .children({
                            let ui = ui.clone();
                            move |item| {
                                let page = item.value.get();
                                let label = i18n::bind(server_desktop::app_label(page));
                                let open = ui.clone();
                                sapid::button()
                                    .class(format!(
                                        "desktop-app-catalog-item {}",
                                        server_desktop::app_class(page)
                                    ))
                                    .name(label.clone())
                                    .fill((
                                        text().class("desktop-app-icon").content(""),
                                        text().class("desktop-app-label").content(label),
                                    ))
                                    .on_press(move || launch(&open, page))
                            }
                        }),
                ),
                when(scope.bond(move || matches.with(Vec::is_empty)), || {
                    text()
                        .class("desktop-app-catalog-empty")
                        .content(i18n::bind("No matching apps"))
                }),
            ))
            .show_in(scope)
    })
}

pub(crate) fn files(ui: Ui) -> impl Widget {
    widget(move |scope| {
        let query = scope.cage(String::new());
        col()
            .class("desktop-app-catalog")
            .fill((search(ui.clone(), query, None), catalog(ui.clone(), query)))
            .show_in(scope)
    })
}

pub(crate) fn dialog(ui: Ui) -> impl Widget {
    when(
        ui.state.map(|s| s.dialog == Some(Dialog::DesktopApps)),
        move || {
            let ui = ui.clone();
            widget(move |scope| {
                let query = scope.cage(String::new());
                let field = scope.cage(None);
                let mounted = scope.mount_with(|| {
                    modal_with_header(
                        ui.clone(),
                        "Apps",
                        search(ui.clone(), query, Some(field)),
                        col()
                            .class("desktop-app-catalog")
                            .fill(catalog(ui.clone(), query)),
                        (),
                    )
                })?;
                scope.retain(mounted)?;
                if let Some(id) = field.get() {
                    scope.action(glory::render::Action::Focus(id))?;
                }
                Ok(())
            })
        },
    )
}
