use arcnix_core::server_apps::{FilesPreferences, FolderColor};

use crate::i18n::desktop as i18n;
use crate::i18n::desktop::{btn, icon_action};
use crate::*;

const COLORS: [(FolderColor, &str); 7] = [
    (FolderColor::Red, "Red"),
    (FolderColor::Orange, "Orange"),
    (FolderColor::Yellow, "Yellow"),
    (FolderColor::Green, "Green"),
    (FolderColor::Blue, "Blue"),
    (FolderColor::Purple, "Purple"),
    (FolderColor::Gray, "Gray"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagFilter {
    Color(FolderColor),
    All,
}

impl TagFilter {
    pub(crate) fn dot_class(self) -> String {
        let color = match self {
            Self::Color(color) => color_class(color),
            Self::All => "tag-all",
        };
        format!("folder-color-dot {color}")
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Color(color) => COLORS
                .iter()
                .find(|(value, _)| *value == color)
                .map_or("Tags", |(_, label)| *label),
            Self::All => "All tags",
        }
    }
}

#[derive(Clone)]
pub(crate) struct ItemRequest {
    pub serial: u64,
    path: String,
}

#[derive(serde::Deserialize)]
struct FavoriteDrop {
    epoch: u64,
    request: u64,
    path: String,
    from_favorites: bool,
    anchor: Option<String>,
    after: bool,
}

fn color_class(color: FolderColor) -> &'static str {
    match color {
        FolderColor::Red => "tag-red",
        FolderColor::Orange => "tag-orange",
        FolderColor::Yellow => "tag-yellow",
        FolderColor::Green => "tag-green",
        FolderColor::Blue => "tag-blue",
        FolderColor::Purple => "tag-purple",
        FolderColor::Gray => "tag-gray",
    }
}

pub(crate) fn tag_class(s: &State, path: &str) -> Option<&'static str> {
    preferences(s).colors.get(path).copied().map(color_class)
}

pub(crate) fn folder_icon_class(s: &State, path: &str) -> Option<String> {
    preferences(s)
        .folder_icons
        .get(path)
        .and_then(|id| folder_icons::class(id))
}

fn color_dot(s: &State, path: &str) -> String {
    tag_class(s, path).map_or_else(
        || "hidden".into(),
        |class| format!("folder-color-dot {class}"),
    )
}

pub(crate) fn preferences(s: &State) -> &FilesPreferences {
    &server_apps_ui::current(s).files
}

fn editable(s: &State) -> bool {
    // The desktop outbox queues edits while a preferences read is in flight.
    s.sftp_available && server_apps_ui::preferences_ready(s)
}

#[derive(Clone, Default)]
pub(crate) struct Cleanup {
    pending: std::collections::BTreeMap<u64, (FilesPreferences, bool)>,
    status: String,
}

impl Cleanup {
    fn checking_manually(&self) -> bool {
        self.pending.values().any(|(_, report)| *report)
    }
}

fn check_missing_paths(ui: &Ui, deleted: Option<&[arcnix_core::deletion::Target]>) {
    let report = deleted.is_none();
    let Some(snapshot) = ui.state.with(|s| {
        (editable(s) && (!report || !s.server_apps.files_cleanup.checking_manually()))
            .then(|| preferences(s).clone())
    }) else {
        return;
    };
    let paths: Vec<_> = snapshot
        .marked_paths()
        .into_iter()
        .filter(|path| {
            deleted.is_none_or(|targets| {
                targets.iter().any(|target| {
                    path == &target.path
                        || target.directory
                            && !target.symlink
                            && path
                                .strip_prefix(&target.path)
                                .is_some_and(|rest| rest.starts_with('/'))
                })
            })
        })
        .collect();
    if paths.is_empty() {
        if report {
            ui.state.revise(|s| {
                s.server_apps.files_cleanup.status =
                    i18n::translate("No missing Files entries to remove")
            });
        }
        return;
    }
    let serial = ui.send(Action::ServerApps(
        server_apps_worker::Action::CheckDesktopPaths(paths),
    ));
    ui.state.revise(|s| {
        let cleanup = &mut s.server_apps.files_cleanup;
        cleanup.pending.insert(serial, (snapshot, report));
        if report {
            cleanup.status.clear();
        }
    });
}

pub(crate) fn cleanup_pending(ui: &Ui, serial: u64) -> bool {
    ui.state
        .with(|s| s.server_apps.files_cleanup.pending.contains_key(&serial))
}

pub(crate) fn cleanup_completed(
    ui: &Ui,
    serial: u64,
    result: std::result::Result<server_apps_worker::Reply, String>,
) {
    let Some((snapshot, report)) = ui
        .state
        .revise(|s| s.server_apps.files_cleanup.pending.remove(&serial))
    else {
        return;
    };
    let status = match result {
        Ok(server_apps_worker::Reply::MissingDesktopPaths { paths, unchecked }) => {
            let mut prefs = ui.state.with(server_apps_ui::desired);
            let files = &mut prefs.files;
            let missing = paths
                .into_iter()
                .filter(|path| {
                    // Keep any edits made after the check was submitted.
                    files.favorites.contains(path) == snapshot.favorites.contains(path)
                        && files.colors.get(path) == snapshot.colors.get(path)
                        && files.folder_icons.get(path) == snapshot.folder_icons.get(path)
                })
                .collect::<std::collections::BTreeSet<_>>();
            let removed = missing.len();
            if removed > 0 {
                files.remove_path_metadata(&missing);
                ui.queue_desktop_preferences(prefs);
            }
            let summary = if removed == 0 {
                i18n::translate("No missing Files entries to remove")
            } else {
                i18n::translate(&format!("Cleaned up {removed} missing Files paths"))
            };
            if unchecked == 0 {
                summary
            } else {
                format!(
                    "{summary}. {}",
                    i18n::translate(&format!("Kept {unchecked} paths that could not be checked"))
                )
            }
        }
        Ok(_) => i18n::translate("Could not check Files entries"),
        Err(error) => format!(
            "{} {error}",
            i18n::translate("Could not check Files entries")
        ),
    };
    if report {
        ui.state
            .revise(|s| s.server_apps.files_cleanup.status = status);
    }
}

pub(crate) fn cleanup_settings(ui: Ui) -> impl Widget {
    let state = ui.state;
    col().class("desktop-settings-card").fill((
        text().class("section-title").content(i18n::bind("Files metadata")),
        text().class("subtle").content(i18n::bind("Remove favorites, color tags and custom icons for missing paths. Items in Trash are kept for restoration.")),
        row().class("choice-list").fill(
            btn("Clean up missing entries", move || check_missing_paths(&ui, None)).class("button")
                .enabled(state.map(|s| editable(s) && !s.server_apps.files_cleanup.checking_manually())),
        ),
        when(state.map(|s| s.server_apps.files_cleanup.checking_manually() || !s.server_apps.files_cleanup.status.is_empty()), move || {
            text().class("subtle files-cleanup-status").content(state.map(|s| {
                let cleanup = &s.server_apps.files_cleanup;
                if cleanup.checking_manually() {
                    i18n::translate("Checking Files entries…")
                } else {
                    cleanup.status.clone()
                }
            }))
        }),
    ))
}

pub(crate) fn check_permanently_deleted(ui: &Ui, targets: &[arcnix_core::deletion::Target]) {
    if !targets.is_empty() {
        check_missing_paths(ui, Some(targets));
    }
}

fn active(s: &State) -> bool {
    server_desktop::visible(s)
        && s.workspace_page.is_files()
        && s.sftp_available
        && s.dialog.is_none()
}

fn saved_item(s: &State, path: &str) -> bool {
    preferences(s).favorites.iter().any(|item| item == path)
        || preferences(s).colors.contains_key(path)
}

pub(crate) fn item_opened(ui: &Ui, result: std::result::Result<server_apps_worker::Reply, String>) {
    let request = ui.state.revise(|s| s.server_apps.files_open_pending.take());
    let Some(request) =
        request.filter(|request| ui.state.with(|s| active(s) && saved_item(s, &request.path)))
    else {
        return;
    };
    match result {
        Ok(server_apps_worker::Reply::FileItem(item)) if !item.symlink => {
            if item.directory {
                file_browser::navigate(ui, true, request.path);
            } else {
                file_browser::open_remote_file(ui, &request.path);
            }
        }
        Ok(_) => ui.error("Could not open this item"),
        Err(error) => ui.error(error),
    }
}

impl Ui {
    pub(crate) fn files_enter(&self) {
        if !self.state.with(|s| {
            server_desktop::visible(s)
                && s.sftp_available
                && s.server_apps.files_locations.is_none()
                && s.server_apps.files_locations_pending.is_none()
        }) {
            return;
        }
        let serial = self.send(Action::ServerApps(
            server_apps_worker::Action::FileLocations,
        ));
        self.state
            .revise(|s| s.server_apps.files_locations_pending = Some(serial));
    }

    fn change_files(&self, change: impl FnOnce(&mut FilesPreferences)) {
        if !self.state.with(editable) {
            return;
        }
        let mut prefs = self.state.with(server_apps_ui::desired);
        change(&mut prefs.files);
        if let Err(error) = prefs.files.validate() {
            self.error(format!("{error:#}"));
        } else if self.state.with(|s| &prefs != server_apps_ui::current(s)) {
            self.queue_desktop_preferences(prefs);
        }
    }

    pub(crate) fn toggle_files_favorite(&self, path: String) {
        if FilesPreferences::validate_path(&path).is_err() {
            return;
        }
        self.change_files(|prefs| {
            if let Some(index) = prefs.favorites.iter().position(|p| p == &path) {
                prefs.favorites.remove(index);
            } else {
                prefs.favorites.push(path);
            }
        });
    }

    pub(crate) fn set_file_colors(&self, paths: Vec<String>, color: FolderColor) {
        if paths.is_empty() {
            return;
        }
        self.change_files(|prefs| {
            for path in paths {
                prefs.colors.insert(path, color);
            }
        });
    }

    pub(crate) fn insert_file_favorite(&self, path: String, anchor: Option<&str>, after: bool) {
        self.change_files(|prefs| {
            let favorites = &mut prefs.favorites;
            if anchor.is_some_and(|anchor| {
                anchor == path || !favorites.iter().any(|item| item == anchor)
            }) {
                return;
            }
            favorites.retain(|item| item != &path);
            let index = anchor
                .and_then(|anchor| favorites.iter().position(|item| item == anchor))
                .map_or(favorites.len(), |index| index + usize::from(after));
            favorites.insert(index, path);
        });
    }

    fn open_files_item(&self, path: String) {
        if !self.state.with(|s| active(s) && saved_item(s, &path)) {
            return;
        }
        let serial = self.send(Action::ServerApps(server_apps_worker::Action::FileItem(
            path.clone(),
        )));
        self.state
            .revise(|s| s.server_apps.files_open_pending = Some(ItemRequest { serial, path }));
    }

    fn filter_files_tags(&self, filter: Option<TagFilter>) {
        self.state.revise(|s| {
            s.workbench.files.location = files_tabs::Location::Directory;
            if s.server_apps.files_tag != filter {
                s.server_apps.files_search.tag_query.clear();
            }
            s.server_apps.files_tag = filter;
            s.server_apps.files_open_pending = None;
        });
    }

    fn drop_files_favorite(&self, drop: FavoriteDrop) {
        let valid = self.state.with(|s| {
            if !active(s)
                || !editable(s)
                || preferences(s).sidebar_hidden
                || drop.epoch != s.epoch
                || drop.request != s.remote_request
                || FilesPreferences::validate_path(&drop.path).is_err()
            {
                return false;
            }
            let favorites = &preferences(s).favorites;
            if let Some(anchor) = &drop.anchor
                && (anchor == &drop.path || !favorites.contains(anchor))
            {
                return false;
            }
            if drop.from_favorites {
                favorites.contains(&drop.path)
            } else {
                s.server_apps.files_tag.is_none()
                    && s.remote_ready
                    && file_browser::entries(s, true)
                        .iter()
                        .any(|entry| entry.path == drop.path && !entry.is_symlink)
            }
        });
        if valid {
            self.insert_file_favorite(drop.path, drop.anchor.as_deref(), drop.after);
        }
    }
}

pub(crate) fn item_target(s: &State) -> Option<(String, bool)> {
    if !editable(s) || !s.workspace_page.is_files() || !s.remote_ready {
        return None;
    }
    let (path, directory) = if s.remote_selection.is_empty() {
        (s.remote_path.clone(), true)
    } else {
        if s.remote_selection.len() != 1 {
            return None;
        }
        let items = file_context::chosen(s, true);
        match items.as_slice() {
            [entry] if !entry.is_symlink => (entry.path.clone(), entry.is_dir),
            _ => return None,
        }
    };
    FilesPreferences::validate_path(&path)
        .ok()
        .map(|()| (path, directory))
}

pub(crate) fn drag_bridge(ui: Ui) -> impl Widget {
    fragment((
        text().class("files-favorites-state hidden").content(
            ui.state
                .map(|s| format!(r#"{{"epoch":{},"request":{}}}"#, s.epoch, s.remote_request)),
        ),
        input()
            .class("files-favorites-input hidden")
            .on(value_changed, move |event| {
                if let Event::ValueChanged { value, .. } = event
                    && value.len() <= 4096
                    && let Ok(drop) = serde_json::from_str(&value)
                {
                    ui.drop_files_favorite(drop);
                }
            }),
    ))
}

pub(crate) fn sidebar_toggle(ui: Ui) -> impl Widget {
    let state = ui.state;
    sapid::button()
        .class("icon-button files-sidebar-toggle")
        .name(i18n::bind("Toggle Files sidebar"))
        .pressed(state.map(|s| !preferences(s).sidebar_hidden))
        .enabled(state.map(editable))
        .fill(icons::glyph(Icon::Sidebar))
        .on_press(move || ui.change_files(|prefs| prefs.sidebar_hidden = !prefs.sidebar_hidden))
}

pub(crate) fn favorite_button(ui: Ui) -> impl Widget {
    let state = ui.state;
    sapid::button()
        .class("icon-button files-favorite")
        .name(i18n::bind(state.map(|s| {
            if preferences(s).favorites.contains(&s.remote_path) {
                "Remove current folder from favorites"
            } else {
                "Add current folder to favorites"
            }
            .to_owned()
        })))
        .pressed(state.map(|s| {
            s.server_apps.files_tag.is_none() && preferences(s).favorites.contains(&s.remote_path)
        }))
        .enabled(state.map(|s| {
            editable(s)
                && s.server_apps.files_tag.is_none()
                && s.remote_ready
                && FilesPreferences::validate_path(&s.remote_path).is_ok()
        }))
        .fill((icons::glyph(Icon::Star), icons::glyph(Icon::StarFilled)))
        .on_press(move || ui.toggle_files_favorite(state.with(|s| s.remote_path.clone())))
}

pub(crate) fn drop_location(s: &State, path: &str) -> bool {
    path == "/"
        || s.server_apps
            .files_locations
            .as_ref()
            .is_some_and(|locations| locations.iter().any(|location| location.path == path))
}

pub(crate) fn activate_window(ui: &Ui, window: u32) -> bool {
    let page = WorkspacePage::Files(window);
    if ui.state.with(|s| s.workspace_page != page) {
        ui.focus_desktop_app(page);
    }
    ui.state.with(|s| s.workspace_page == page)
}

fn place(
    state: Cage<State>,
    label: &'static str,
    path: String,
    current: impl Fn(&State) -> &str + 'static,
    drop_location: bool,
    open: impl Fn(String) + 'static,
) -> impl Widget {
    let current_path = path.clone();
    let name = i18n::bind(if label == "Home" { "主目录" } else { label });
    sapid::button()
        .class(if drop_location {
            "files-place files-drop-location"
        } else {
            "files-place"
        })
        .name(name.clone())
        .pressed(state.map(move |s| current(s) == current_path))
        .fill((
            icons::glyph(match label {
                "Home" => Icon::Home,
                "Desktop" => Icon::Desktop,
                "Pictures" => Icon::Image,
                "Music" => Icon::Audio,
                "Videos" => Icon::Video,
                "Documents" => Icon::File,
                "Downloads" => Icon::Download,
                "Filesystem" => Icon::Disks,
                _ => Icon::Folder,
            }),
            text().content(name),
            text().class("files-drop-path hidden").content(path.clone()),
        ))
        .on_press(move || open(path.clone()))
}

pub(crate) fn location_links(
    state: Cage<State>,
    current: impl Fn(&State) -> &str + Copy + 'static,
    drop_location: bool,
    open: impl Fn(String) + Clone + 'static,
) -> impl IntoChildren {
    let open_location = open.clone();
    (
        text()
            .class("files-sidebar-heading")
            .content(i18n::bind("Locations")),
        each(state.map(|s| s.server_apps.files_locations.clone().unwrap_or_default()))
            .key(|location| (location.name, location.path.clone()))
            .children(move |item| {
                let location = item.value.get();
                place(
                    state,
                    location.name,
                    location.path,
                    current,
                    drop_location,
                    open_location.clone(),
                )
            }),
        place(
            state,
            "Filesystem",
            "/".into(),
            current,
            drop_location,
            open,
        ),
    )
}

pub(crate) fn item_name(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
}

fn tag_link(
    state: Cage<State>,
    filter: TagFilter,
    current: impl Fn(&State) -> Option<TagFilter> + 'static,
    open: impl Fn(TagFilter) + 'static,
) -> impl Widget {
    sapid::button()
        .class("files-place files-tag-link")
        .name(i18n::bind(filter.label()))
        .pressed(state.map(move |s| current(s) == Some(filter)))
        .fill((
            text().class(filter.dot_class()).content(""),
            text().content(i18n::bind(filter.label())),
            text()
                .class("files-drop-color hidden")
                .content(match filter {
                    TagFilter::Color(color) => color_class(color).trim_start_matches("tag-"),
                    TagFilter::All => "",
                }),
        ))
        .on_press(move || open(filter))
}

pub(crate) fn tag_links(
    state: Cage<State>,
    current: impl Fn(&State) -> Option<TagFilter> + Copy + 'static,
    open: impl Fn(TagFilter) + Clone + 'static,
) -> impl IntoChildren {
    (
        text()
            .class("files-sidebar-heading")
            .content(i18n::bind("Tags")),
        fragment(
            COLORS
                .into_iter()
                .map(|(color, _)| TagFilter::Color(color))
                .chain([TagFilter::All])
                .map(|filter| tag_link(state, filter, current, open.clone()))
                .collect::<Vec<_>>(),
        ),
    )
}

pub(crate) fn sidebar(ui: Ui, window: u32) -> impl Widget {
    let state = ui.state;
    let place_ui = ui.clone();
    let favorite_ui = ui.clone();
    col()
        .class("files-sidebar")
        .name(i18n::bind("Files sidebar"))
        .fill((
            fragment(location_links(
                state,
                move |s| files_tabs::directory(s, window).unwrap_or_default(),
                true,
                move |path| {
                    if activate_window(&place_ui, window) {
                        file_browser::navigate(&place_ui, true, path);
                    }
                },
            )),
            sapid::button()
                .class("files-place files-apps-place")
                .name(i18n::bind("Apps"))
                .pressed(state.map(move |s| files_tabs::showing_apps(s, window)))
                .fill((icons::glyph(Icon::Grid), text().content(i18n::bind("Apps"))))
                .on_press({
                    let ui = ui.clone();
                    move || {
                        if activate_window(&ui, window) {
                            files_tabs::open_apps(&ui);
                        }
                    }
                }),
            sapid::button()
                .class("files-place files-trash-place")
                .enabled(state.map(|s| s.local_desktop.is_none()))
                .name(i18n::bind("回收站"))
                .pressed(state.map(move |s| files_tabs::showing_trash(s, window)))
                .fill((
                    icons::glyph(Icon::Clear),
                    text().content(i18n::bind("回收站")),
                ))
                .on_press({
                    let ui = ui.clone();
                    move || {
                        if activate_window(&ui, window) {
                            trash_ui::open(&ui, true);
                        }
                    }
                }),
            col()
                .class(state.map(|s| {
                    if editable(s) && active(s) {
                        "files-favorites editable"
                    } else {
                        "files-favorites"
                    }
                    .to_owned()
                }))
                .fill((
                    text()
                        .class("files-sidebar-heading")
                        .content(i18n::bind("Favorites")),
                    each(state.map(|s| preferences(s).favorites.clone()))
                        .key(String::clone)
                        .children(move |item| {
                            let path = item.value;
                            let open = favorite_ui.clone();
                            let remove = favorite_ui.clone();
                            row().class("files-favorite-row").name(path).fill((
                                sapid::button()
                                    .class("files-place")
                                    .name(i18n::bind(path.map(|p| format!("Open favorite {p}"))))
                                    .pressed(state.map(move |s| {
                                        files_tabs::directory(s, window)
                                            == Some(path.get().as_str())
                                    }))
                                    .fill((
                                        icons::glyph(Icon::Star),
                                        text().content(path.map(|p| item_name(p).to_owned())),
                                        text()
                                            .class(state.map(move |s| color_dot(s, &path.get())))
                                            .content(""),
                                    ))
                                    .on_press(move || {
                                        if activate_window(&open, window) {
                                            open.open_files_item(path.get());
                                        }
                                    }),
                                sapid::button()
                                    .class("icon-button files-favorite-remove")
                                    .name(i18n::bind(path.map(|p| format!("Remove favorite {p}"))))
                                    .enabled(state.map(editable))
                                    .fill(icons::glyph(Icon::Close))
                                    .on_press(move || remove.toggle_files_favorite(path.get())),
                            ))
                        }),
                )),
            fragment(tag_links(
                state,
                move |s| files_tabs::showing_tag(s, window),
                {
                    let ui = ui.clone();
                    move |filter| {
                        if activate_window(&ui, window) {
                            ui.filter_files_tags(Some(filter));
                        }
                    }
                },
            )),
        ))
}

pub(crate) fn empty_state(icon: Icon, label: &'static str, class: &'static str) -> impl Widget {
    col().class(format!("files-empty-state {class}")).fill((
        icons::glyph(icon),
        text().class("files-empty-title").content(i18n::bind(label)),
    ))
}

pub(crate) fn empty_listing(filtered: impl Into<Vary<bool>>) -> impl Widget {
    when(filtered, || {
        empty_state(Icon::Search, "No matching items", "file-empty")
    })
    .otherwise(|| empty_state(Icon::Folder, "This folder is empty", "file-empty"))
}

pub(crate) fn paths_for_tag(s: &State, filter: TagFilter) -> Vec<String> {
    preferences(s)
        .colors
        .iter()
        .filter(|(_, color)| filter == TagFilter::All || filter == TagFilter::Color(**color))
        .map(|(path, _)| path.clone())
        .collect()
}

pub(crate) fn tag_results(ui: Ui, window: u32) -> impl Widget {
    let state = ui.state;
    when(
        state.map(move |s| files_tabs::showing_tag(s, window).is_some()),
        move || {
            let back = ui.clone();
            let item_ui = ui.clone();
            let search = ui.clone();
            let query = state.map(move |s| files_tabs::tag_query(s, window).to_owned());
            let paths = state.map(move |s| {
                let Some(tag) = files_tabs::showing_tag(s, window) else {
                    return Vec::new();
                };
                let mut paths = paths_for_tag(s, tag);
                let needle = files_tabs::tag_query(s, window).trim().to_lowercase();
                if !needle.is_empty() {
                    paths.retain(|p| p.to_lowercase().contains(&needle));
                }
                paths
            });
            col().class("files-tags-view").fill((
                row()
                    .class("files-window-tools files-location-toolbar")
                    .fill((
                        icon_action("Back to folder", Icon::ChevronRight)
                            .class("icon-button files-history-back")
                            .on_press(move || {
                                if activate_window(&back, window) {
                                    back.filter_files_tags(None);
                                }
                            }),
                        row().class("files-location-search").fill((
                            icons::glyph(Icon::Search),
                            sapid::text_field()
                                .unstyled()
                                .name(i18n::bind("Search tagged items"))
                                .placeholder(i18n::bind("Search tagged items"))
                                .value(query)
                                .on_value_change(move |value| {
                                    if activate_window(&search, window) {
                                        state.revise(|s| {
                                            s.server_apps.files_search.tag_query = value
                                        });
                                    }
                                }),
                        )),
                    )),
                when(state.map(move |_| paths.with(Vec::is_empty)), move || {
                    when(
                        state.map(move |_| query.with(|q| q.trim().is_empty())),
                        || empty_state(Icon::Tag, "No tagged items", "files-tags-empty"),
                    )
                    .otherwise(|| {
                        empty_state(Icon::Search, "No matching items", "files-tags-empty")
                    })
                }),
                col().class("files-tags-list").fill((
                    when(
                        state.map(move |_| paths.with(|rows| !rows.is_empty())),
                        || {
                            row().class("files-tag-columns").fill((
                                text().content(i18n::bind("名称")),
                                text().content(i18n::bind("Original path")),
                            ))
                        },
                    ),
                    each(paths).key(String::clone).children(move |item| {
                        let path = item.value;
                        let open = item_ui.clone();
                        let clear = item_ui.clone();
                        row().class("files-tag-result").fill((
                            sapid::button()
                                .class("files-tag-item")
                                .name(path)
                                .fill((
                                    text()
                                        .class(state.map(move |s| color_dot(s, &path.get())))
                                        .content(""),
                                    text()
                                        .class("files-tag-item-name")
                                        .content(path.map(|p| item_name(p).to_owned())),
                                    text().class("files-tag-item-path subtle").content(path),
                                ))
                                .on_press(move || {
                                    if activate_window(&open, window) {
                                        open.open_files_item(path.get());
                                    }
                                }),
                            icon_action("Clear tag", Icon::Close)
                                .enabled(state.map(editable))
                                .on_press(move || {
                                    if !activate_window(&clear, window) {
                                        return;
                                    }
                                    clear.change_files(|prefs| {
                                        prefs.colors.remove(&path.get());
                                    });
                                }),
                        ))
                    }),
                )),
            ))
        },
    )
}

pub(crate) fn appearance_menu(
    ui: Ui,
    target: impl Fn(&State) -> Option<(String, bool)> + Clone + 'static,
    close: fn(&Ui),
    favorites: bool,
) -> impl Widget {
    let state = ui.state;
    let target = move |s: &State| {
        let item = target(s)?;
        (editable(s) && FilesPreferences::validate_path(&item.0).is_ok()).then_some(item)
    };
    when(
        state.map({
            let target = target.clone();
            move |s| target(s).is_some()
        }),
        move || {
            let swatches = COLORS
                .into_iter()
                .map(|(color, label)| {
                    let current = target.clone();
                    let apply = target.clone();
                    let ui = ui.clone();
                    sapid::button()
                        .class("file-color-swatch")
                        .name(crate::i18n::workspace_label(state, label))
                        .pressed(state.map(move |s| {
                            current(s).is_some_and(|(path, _)| {
                                preferences(s).colors.get(&path) == Some(&color)
                            })
                        }))
                        .fill(
                            text()
                                .class(format!("folder-color-dot {}", color_class(color)))
                                .content(""),
                        )
                        .on_press(move || {
                            if let Some((path, _)) = state.with(&apply) {
                                close(&ui);
                                ui.change_files(|prefs| {
                                    prefs.colors.insert(path, color);
                                });
                            }
                        })
                })
                .collect::<Vec<_>>();
            let can_clear = target.clone();
            let clear_target = target.clone();
            let clear = ui.clone();
            let customize = ui.clone();
            let folder = target.clone();
            let edit_target = target.clone();
            let favorite_target = target.clone();
            let favorite = ui.clone();
            col().class("file-menu-group file-appearance-menu").fill((
                when(favorites, move || {
                    let ui = favorite.clone();
                    let target = favorite_target.clone();
                    sapid::button()
                        .class("menu-item")
                        .label(crate::i18n::workspace_label(state, "Toggle favorite"))
                        .on_press(move || {
                            if let Some((path, _)) = state.with(&target) {
                                close(&ui);
                                ui.toggle_files_favorite(path);
                            }
                        })
                }),
                row()
                    .class("file-color-menu")
                    .name(crate::i18n::workspace_label(state, "Tags"))
                    .fill((
                        fragment(swatches),
                        sapid::button()
                            .class("file-color-swatch file-color-clear")
                            .name(crate::i18n::workspace_label(state, "Clear tag"))
                            .enabled(state.map(move |s| {
                                can_clear(s).is_some_and(|(path, _)| {
                                    preferences(s).colors.contains_key(&path)
                                })
                            }))
                            .fill(icons::glyph(Icon::Close))
                            .on_press(move || {
                                if let Some((path, _)) = state.with(&clear_target) {
                                    close(&clear);
                                    clear.change_files(|prefs| {
                                        prefs.colors.remove(&path);
                                    });
                                }
                            }),
                    )),
                when(
                    state.map(move |s| folder(s).is_some_and(|(_, directory)| directory)),
                    move || {
                        let customize = customize.clone();
                        let edit_target = edit_target.clone();
                        sapid::button()
                            .class("menu-item")
                            .label(crate::i18n::workspace_label(state, "Customize folder…"))
                            .on_press(move || {
                                if let Some((path, true)) = state.with(&edit_target) {
                                    close(&customize);
                                    customize.open_dialog(Dialog::CustomizeFolder { path });
                                }
                            })
                    },
                ),
            ))
        },
    )
}

pub(crate) fn folder_icon_dialog(ui: Ui) -> impl Widget {
    let state = ui.state;
    when(
        state.map(|s| matches!(s.dialog, Some(Dialog::CustomizeFolder { .. }))),
        move || {
            let Some(path) = state.with(|s| {
                let Some(Dialog::CustomizeFolder { path }) = &s.dialog else {
                    return None;
                };
                Some(path.clone())
            }) else {
                return fragment(());
            };
            fragment(folder_icon_picker(ui.clone(), path))
        },
    )
}

fn folder_icon_picker(ui: Ui, path: String) -> impl Widget {
    let state = ui.state;
    widget(move |scope| {
        let query = scope.cage(String::new());
        let search_ref = scope.cage(None);
        let preview = path.clone();
        let tagged = path.clone();
        let folder_path = path.clone();
        let current = path.clone();
        let restore = ui.clone();
        let done = ui.clone();
        let mounted = scope.mount_with(|| {
            modal_with_header(
                ui.clone(),
                "Customize folder",
                settings::restore_button(true, "Restore default folder icon", move || {
                    restore.change_files(|prefs| {
                        prefs.folder_icons.remove(&folder_path);
                    });
                })
                .enabled(state.map(move |s| {
                    editable(s) && preferences(s).folder_icons.contains_key(&current)
                })),
                (
                    row().class("folder-customize-preview").fill((
                        row()
                            .class(state.map(move |s| {
                                tag_class(s, &tagged).map_or_else(
                                    || "folder-icon-preview".into(),
                                    |tag| format!("folder-icon-preview {tag}"),
                                )
                            }))
                            .fill(
                                text()
                                    .class(state.map(move |s| {
                                        folder_icon_class(s, &preview).map_or_else(
                                            || "action-glyph glyph-folder".into(),
                                            |class| {
                                                format!("action-glyph folder-custom-mark {class}")
                                            },
                                        )
                                    }))
                                    .content(""),
                            ),
                        col().class("folder-customize-name").fill((
                            text()
                                .class("section-title")
                                .content(item_name(&path).to_owned()),
                            text().class("subtle").content(path.clone()),
                        )),
                    )),
                    row().class("folder-icon-controls").fill(
                        row().class("folder-icon-search").fill((
                            icons::glyph(Icon::Search),
                            input()
                                .node_ref(search_ref)
                                .name(i18n::bind("Search icons"))
                                .semantics(state.map(|_| {
                                    Some(Box::new(glory::render::Semantics {
                                        placeholder: Some(i18n::translate("Search icons…")),
                                        ..Default::default()
                                    }))
                                }))
                                .on(value_changed, move |event| {
                                    if let Event::ValueChanged { value, .. } = event {
                                        query.revise(|q| *q = value.to_lowercase());
                                    }
                                }),
                        )),
                    ),
                    folder_icon_gallery(ui.clone(), path.clone(), query),
                ),
                btn("Done", move || done.close_dialog()).class("button primary"),
            )
        })?;
        scope.retain(mounted)?;
        if let Some(id) = search_ref.get() {
            scope.action(glory::render::Action::Focus(id))?;
        }
        Ok(())
    })
}

fn folder_icon_gallery(ui: Ui, path: String, query: Cage<String>) -> impl Widget {
    use folder_icons::GROUPS;

    let state = ui.state;
    let groups = query.map(|query| {
        GROUPS
            .iter()
            .filter(|group| group.matches(query))
            .collect::<Vec<_>>()
    });
    col().class("folder-icon-gallery").fill((
        when(
            query.map(|query| !GROUPS.iter().any(|group| group.matches(query))),
            || {
                text()
                    .class("folder-icon-empty")
                    .content(i18n::bind("No matching icons"))
            },
        ),
        each(groups).key(|group| group.label).children(move |item| {
            let group = item.value.get();
            let path = path.clone();
            let ui = ui.clone();
            col().class("folder-icon-group").fill((
                text()
                    .class("folder-icon-group-label")
                    .content(i18n::bind(group.label)),
                row().class("folder-icon-options").fill(
                    each(query.map(move |query| {
                        group
                            .icons
                            .iter()
                            .filter(|icon| icon.matches(query))
                            .collect::<Vec<_>>()
                    }))
                    .key(|icon| icon.id)
                    .children(move |item| {
                        let icon = item.value.get();
                        let path = path.clone();
                        let current = path.clone();
                        let ui = ui.clone();
                        sapid::button()
                            .class("folder-icon-option")
                            .name(i18n::bind(icon.label))
                            .enabled(state.map(editable))
                            .pressed(state.map(move |s| {
                                preferences(s)
                                    .folder_icons
                                    .get(&current)
                                    .is_some_and(|id| folder_icons::name(id) == icon.id)
                            }))
                            .fill(
                                text()
                                    .class(format!(
                                        "action-glyph folder-custom-mark {}",
                                        icon.class()
                                    ))
                                    .content(""),
                            )
                            .on_press(move || {
                                ui.change_files(|prefs| {
                                    prefs.folder_icons.insert(path.clone(), icon.id.to_owned());
                                });
                            })
                    }),
                ),
            ))
        }),
    ))
}
