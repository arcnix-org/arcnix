use std::result::Result;

use crate::files_ui::TagFilter;
use crate::i18n::desktop as i18n;
use crate::server_apps_worker::Action as AppAction;
use crate::*;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Purpose {
    Video,
    Image,
    Wallpaper,
    Pdf,
    Office,
    TextEditor,
    SaveText(u64),
    SaveOffice(u64),
    Code,
}

#[derive(Clone, Copy)]
struct Formats {
    label: &'static str,
    empty: &'static str,
    includes: fn(&str) -> bool,
    extensions: fn() -> Vec<&'static str>,
}

impl Purpose {
    fn title(self) -> &'static str {
        match self {
            Self::Video => "Open video",
            Self::Image => "Open image",
            Self::Wallpaper => "Add Wallpaper",
            Self::Pdf => "Open PDF",
            Self::Office => "Open Office document",
            Self::SaveText(_) | Self::SaveOffice(_) => "Save as…",
            _ => "Open file",
        }
    }

    fn formats(self) -> Formats {
        match self {
            Self::Office | Self::SaveOffice(_) => Formats {
                label: "Office documents",
                empty: "No Office documents in this folder.",
                includes: office_reader::supports,
                extensions: || office_reader::EXTENSIONS.to_vec(),
            },
            Self::Pdf => Formats {
                label: "PDF documents",
                empty: "No PDFs in this folder.",
                includes: |path| {
                    arcnix_core::file_associations::extension(path).as_deref() == Some("pdf")
                },
                extensions: || vec!["pdf"],
            },
            Self::Wallpaper => Formats {
                label: "All supported images",
                empty: "No images in this folder.",
                includes: arcnix_core::wallpaper_cache::supports,
                extensions: || arcnix_core::wallpaper_cache::EXTENSIONS.to_vec(),
            },
            Self::Image => Formats {
                label: "All supported images",
                empty: "No images in this folder.",
                includes: image_viewer::supports,
                extensions: || image_viewer::extensions().collect(),
            },
            Self::Video => Formats {
                label: "All supported videos",
                empty: "No playable videos in this folder.",
                includes: video_player::supports,
                extensions: || video_player::supported_extensions().collect(),
            },
            _ => Formats {
                label: "All supported text files",
                empty: "No text files in this folder.",
                includes: file_open::is_text_file,
                extensions: || {
                    vec![
                        "txt", "md", "log", "json", "yaml", "toml", "ini", "conf", "csv", "html",
                        "xml",
                    ]
                },
            },
        }
    }
}

pub(crate) fn current(s: &State) -> Option<Purpose> {
    match s.dialog {
        Some(Dialog::OpenDesktopFile(purpose)) => Some(purpose),
        _ => None,
    }
}

pub(crate) fn open(ui: &Ui, purpose: Purpose, start: Option<String>) {
    if !ui.state.with(|s| {
        s.connected
            && s.sftp_available
            && server_desktop::visible(s)
            && s.dialog.is_none()
            && !s.system_task_running
    }) {
        return;
    }
    ui.open_dialog(Dialog::OpenDesktopFile(purpose));
    if ui.state.with(current) == Some(purpose) {
        let path = start.unwrap_or_else(|| ui.state.with(|s| s.remote_path.clone()));
        read_directory(ui, path);
        if matches!(purpose, Purpose::SaveText(_) | Purpose::SaveOffice(_)) {
            ui.state.revise(|s| {
                if let Some(d) = &mut s.desktop_file_picker {
                    d.filter = Filter::All;
                }
            });
        }
        ui.files_enter();
    }
}

pub(crate) fn dismiss(s: &mut State) {
    if current(s).is_some() {
        s.desktop_file_picker = None;
        s.dialog = None;
        s.dialog_error.clear();
    }
}

fn confirm(ui: &Ui, path: &str) {
    if ui.state.with(|s| office_sync::uploading(s, path)) {
        return;
    }
    let Some(purpose) = ui.state.with(current) else {
        return;
    };
    let open: fn(&Ui, &str) = match purpose {
        Purpose::SaveOffice(id) => return office_reader::submit_save(ui, id, path),
        Purpose::SaveText(id) => return submit_save(ui, id, path),
        Purpose::Video => |ui, path| ui.player.open_from_picker(ui, path),
        Purpose::Image => image_viewer::open,
        Purpose::Wallpaper => wallpaper_ui::select,
        Purpose::Pdf => pdf_reader::open,
        Purpose::Office => office_reader::open,
        Purpose::TextEditor => text_editor::open,
        Purpose::Code => code_workbench::open,
    };
    ui.close_dialog();
    if !ui
        .state
        .with(|s| s.connected && s.sftp_available && s.dialog.is_none())
    {
        return;
    }
    open(ui, path);
}

pub(crate) fn dialog(ui: Ui) -> impl Widget {
    let state = ui.state;
    each(state.map(|s| current(s).into_iter().collect::<Vec<_>>()))
        .key(|purpose| *purpose)
        .children(move |item| {
            let purpose = item.value.get();
            let formats = purpose.formats();
            let cancel = ui.clone();
            let choose = ui.clone();
            modal(
                ui.clone(),
                purpose.title(),
                panel(ui.clone(), formats),
                (
                    match purpose {
                        Purpose::SaveText(_) | Purpose::SaveOffice(_) => fragment(
                            row().class("file-picker-save-name").fill((
                                text().content(i18n::bind("File name")),
                                input()
                                    .name(i18n::bind("File name"))
                                    .value(state.map(|s| {
                                        s.desktop_file_picker
                                            .as_ref()
                                            .map_or_else(String::new, |d| d.name.clone())
                                    }))
                                    .on(value_changed, {
                                        let ui = ui.clone();
                                        move |event| {
                                            if let Event::ValueChanged { value, .. } = event {
                                                set_save_name(&ui, value);
                                            }
                                        }
                                    })
                                    .on(commit, {
                                        let ui = ui.clone();
                                        move |_| {
                                            if let Some(path) = state.with(save_path) {
                                                confirm(&ui, &path);
                                            }
                                        }
                                    }),
                            )),
                        ),
                        _ => fragment(format_filter(state, formats)),
                    },
                    row().class("file-picker-actions").fill((
                        i18n::btn("取消", move || cancel.close_dialog()),
                        sapid::button()
                            .label(i18n::bind(state.map(move |s| {
                                match purpose {
                                    Purpose::SaveText(_)
                                        if s.desktop_file_picker
                                            .as_ref()
                                            .is_some_and(|d| d.replace.is_some()) =>
                                    {
                                        "Replace"
                                    }
                                    Purpose::SaveText(_) | Purpose::SaveOffice(_) => "Save",
                                    Purpose::Wallpaper => "Add Wallpaper",
                                    _ => "Open",
                                }
                                .to_owned()
                            })))
                            .on_press(move || {
                                if let Some(path) = state.with(chosen_path) {
                                    confirm(&choose, &path);
                                }
                            })
                            .class("button primary")
                            .enabled(state.map(|s| {
                                chosen_path(s).is_some_and(|path| !office_sync::uploading(s, &path))
                            })),
                    )),
                ),
            )
        })
}

#[derive(Clone, Default)]
pub(crate) struct Directory {
    path: String,
    collection: Option<Collection>,
    request: Option<u64>,
    entries: Vec<FileEntry>,
    error: String,
    filter: Filter,
    selected: Option<String>,
    name: String,
    replace: Option<String>,
}

pub(crate) fn set_save_name(ui: &Ui, name: String) {
    ui.state.revise(|s| {
        if let Some(d) = &mut s.desktop_file_picker {
            d.name = name;
            d.replace = None;
            s.dialog_error.clear();
        }
    });
}

fn save_path(s: &State) -> Option<String> {
    let d = s.desktop_file_picker.as_ref()?;
    if d.request.is_some()
        || !d.error.is_empty()
        || matches!(d.name.as_str(), "" | "." | "..")
        || d.name.contains(['/', '\\'])
        || d.name.chars().any(char::is_control)
    {
        return None;
    }
    if d.collection.is_some() {
        let parent = parent(d.selected.as_deref()?)?;
        return Some(remote_join(&parent, &d.name));
    }
    Some(remote_join(&d.path, &d.name))
}

fn chosen_path(s: &State) -> Option<String> {
    if matches!(
        current(s),
        Some(Purpose::SaveText(_) | Purpose::SaveOffice(_))
    ) {
        save_path(s)
    } else {
        selected_path(s).map(str::to_owned)
    }
}

fn submit_save(ui: &Ui, id: u64, path: &str) {
    let ready = ui.state.revise(|s| {
        if text_editor::open_in_another_tab(s, id, path) {
            s.dialog_error = i18n::translate("This file is open in another tab.");
            return None;
        }
        let d = s.desktop_file_picker.as_mut()?;
        let entry = d.entries.iter().find(|e| e.path == path);
        if let Some(e) = entry {
            if e.is_dir || e.is_symlink {
                s.dialog_error = i18n::translate("Choose a regular file name.");
                return None;
            }
            if d.replace.as_deref() != Some(path) {
                d.replace = Some(path.to_owned());
                s.dialog_error = i18n::translate(&format!("Replace existing file {}?", e.name));
                return None;
            }
        }
        Some(entry.map(|e| arcnix_core::text_files::TextSnapshot {
            size: e.size,
            modified: e.modified,
        }))
    });
    if let Some(replace) = ready {
        ui.close_dialog();
        text_editor::save_as(ui, id, path.to_owned(), replace);
    }
}

#[derive(Clone)]
enum Collection {
    Favorite(String),
    Tag(TagFilter),
    Trash,
}

#[derive(Clone, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    Supported,
    All,
    Extension(&'static str),
    Custom(Vec<String>),
}

impl Filter {
    fn label(&self, formats: Formats) -> String {
        match self {
            Self::Supported => formats.label.into(),
            Self::All => "All files".into(),
            Self::Extension(ext) => format!("*.{ext}"),
            Self::Custom(_) => "Custom extensions".into(),
        }
    }

    fn includes(&self, entry: &FileEntry, formats: Formats) -> bool {
        use arcnix_core::file_associations::extension;
        entry.is_dir
            || match self {
                Self::Supported => (formats.includes)(&entry.path),
                Self::All => true,
                Self::Extension(ext) => extension(&entry.path).as_deref() == Some(*ext),
                Self::Custom(suffixes) => {
                    let name = entry.name.to_lowercase();
                    suffixes.iter().any(|suffix| {
                        name.strip_suffix(suffix)
                            .is_some_and(|stem| !stem.is_empty())
                    })
                }
            }
    }
}

fn custom_extensions(input: &str) -> Option<Vec<String>> {
    let mut suffixes = Vec::new();
    for token in input
        .split(|c: char| c.is_whitespace() || ",;，；、".contains(c))
        .filter(|token| !token.is_empty())
    {
        let extension = token
            .strip_prefix("*.")
            .or_else(|| token.strip_prefix('.'))
            .unwrap_or(token);
        if !extension.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_alphanumeric() || "_+-".contains(c))
        }) {
            return None;
        }
        let suffix = format!(".{}", extension.to_lowercase());
        if !suffixes.contains(&suffix) {
            suffixes.push(suffix);
        }
    }
    (!suffixes.is_empty()).then_some(suffixes)
}

pub(crate) fn parent(path: &str) -> Option<String> {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) => Some("/".into()),
        Some((parent, _)) => Some(parent.to_owned()),
        None if !matches!(path, "" | "/" | ".") => Some(".".into()),
        None => None,
    }
}

fn read_directory(ui: &Ui, path: String) {
    let serial = ui.send(Action::Children {
        remote: true,
        path: path.clone(),
    });
    load(ui, serial, Some(path), None);
}

fn read_collection(ui: &Ui, collection: Collection) {
    let action = match &collection {
        Collection::Favorite(path) => AppAction::PickerEntries(vec![path.clone()]),
        Collection::Tag(tag) => {
            AppAction::PickerEntries(ui.state.with(|s| files_ui::paths_for_tag(s, *tag)))
        }
        Collection::Trash => AppAction::PickerTrash,
    };
    let serial = ui.send(Action::ServerApps(action));
    load(ui, serial, None, Some(collection));
}

fn load(ui: &Ui, serial: u64, path: Option<String>, collection: Option<Collection>) {
    ui.state.revise(|s| {
        let directory = s.desktop_file_picker.get_or_insert_with(Directory::default);
        if let Some(path) = path {
            directory.path = path;
        }
        directory.collection = collection;
        directory.request = Some(serial);
        directory.entries.clear();
        directory.error.clear();
        directory.selected = None;
        directory.replace = None;
        s.dialog_error.clear();
    });
}

pub(crate) fn awaiting_directory(ui: &Ui, serial: u64) -> bool {
    ui.state.with(|s| {
        current(s).is_some()
            && s.desktop_file_picker
                .as_ref()
                .is_some_and(|d| d.request == Some(serial))
    })
}

pub(crate) fn listed(ui: &Ui, result: Result<Vec<FileEntry>, String>) {
    let favorite = ui
        .state
        .with(|s| matches!(collection(s), Some(Collection::Favorite(_))));
    if favorite
        && let Ok(entries) = &result
        && let [entry] = entries.as_slice()
        && entry.is_dir
    {
        read_directory(ui, entry.path.clone());
        return;
    }
    ui.state.revise(|s| {
        let Some(directory) = &mut s.desktop_file_picker else {
            return;
        };
        directory.request = None;
        match result {
            Ok(mut entries) => {
                entries.retain(|e| !e.is_symlink);
                directory.entries = entries;
            }
            Err(error) => directory.error = error,
        }
    });
}

fn selected_path(s: &State) -> Option<&str> {
    s.desktop_file_picker.as_ref()?.selected.as_deref()
}

fn location(s: &State) -> &str {
    s.desktop_file_picker
        .as_ref()
        .filter(|d| d.collection.is_none())
        .map_or("", |d| d.path.as_str())
}

fn collection(s: &State) -> Option<&Collection> {
    s.desktop_file_picker.as_ref()?.collection.as_ref()
}

fn up_path(s: &State) -> Option<String> {
    let directory = s.desktop_file_picker.as_ref()?;
    if directory.collection.is_some() {
        Some(directory.path.clone())
    } else {
        parent(&directory.path)
    }
}

fn tag(s: &State) -> Option<TagFilter> {
    match collection(s)? {
        Collection::Tag(tag) => Some(*tag),
        _ => None,
    }
}

fn format_filter(state: Cage<State>, formats: Formats) -> impl Widget {
    widget(move |scope| {
        let expanded = scope.cage(false);
        let trigger = scope.cage(None);
        let focus = scope.clone();
        let draft = scope.cage(String::new());
        let choose = move |filter| {
            state.revise(|s| {
                if let Some(directory) = &mut s.desktop_file_picker
                    && directory.filter != filter
                {
                    directory.filter = filter;
                    directory.selected = None;
                }
            });
            expanded.revise(|open| *open = false);
            if let Some(node) = trigger.get() {
                let _ = focus.action(glory::render::Action::Focus(node));
            }
        };
        row()
            .class("file-picker-filter")
            .fill((
                sapid::button()
                    .class("button with-icon files-filter-choice")
                    .node_ref(trigger)
                    .name(i18n::bind("文件类型"))
                    .pressed(expanded)
                    .fill((
                        icons::glyph(Icon::Filter),
                        text().content(i18n::bind(state.map(move |s| {
                            s.desktop_file_picker.as_ref().map_or_else(
                                || Filter::Supported.label(formats),
                                |d| d.filter.label(formats),
                            )
                        }))),
                    ))
                    .on_press(move || expanded.revise(|open| *open = !*open)),
                when(expanded, move || {
                    let choose_custom = choose.clone();
                    let apply = move || {
                        if let Some(suffixes) = draft.with(|value| custom_extensions(value)) {
                            choose_custom(Filter::Custom(suffixes));
                        }
                    };
                    let apply_on_commit = apply.clone();
                    col().class("file-picker-formats").fill((
                        row().class("choice-list").fill(fragment(
                            [Filter::Supported, Filter::All]
                                .into_iter()
                                .chain((formats.extensions)().into_iter().map(Filter::Extension))
                                .map(|option| {
                                    let choose = choose.clone();
                                    let current = option.clone();
                                    sapid::button()
                                        .class("button compact files-filter-choice")
                                        .label(i18n::bind(option.label(formats)))
                                        .pressed(state.map(move |s| {
                                            s.desktop_file_picker
                                                .as_ref()
                                                .is_some_and(|d| d.filter == current)
                                        }))
                                        .on_press(move || choose(option.clone()))
                                })
                                .collect::<Vec<_>>(),
                        )),
                        text()
                            .class("field-label")
                            .content(i18n::bind("Custom extensions")),
                        row().class("file-picker-custom-format").fill((
                            input()
                                .name(i18n::bind("Custom extensions"))
                                .value(draft)
                                .on(value_changed, move |event| {
                                    if let Event::ValueChanged { value, .. } = event {
                                        draft.revise(|current| *current = value);
                                    }
                                })
                                .on(commit, move |_| apply_on_commit()),
                            i18n::btn("应用", apply)
                                .class("button compact")
                                .enabled(draft.map(|value| custom_extensions(value).is_some())),
                        )),
                        text()
                            .class("subtle")
                            .content(i18n::bind("Enter extensions such as .video, .backup")),
                    ))
                }),
            ))
            .show_in(scope)
    })
}

fn sidebar(ui: Ui) -> impl Widget {
    let state = ui.state;
    let places = ui.clone();
    let favorites = ui.clone();
    let trash = ui.clone();
    col()
        .class("files-sidebar")
        .name(i18n::bind("Files sidebar"))
        .fill((
            fragment(files_ui::location_links(state, location, false, move |path| {
                read_directory(&places, path);
            })),
            sapid::button()
                .class("files-place files-trash-place").enabled(state.map(|s|s.local_desktop.is_none()))
                .name(i18n::bind("回收站"))
                .pressed(state.map(|s| matches!(collection(s), Some(Collection::Trash))))
                .fill((icons::glyph(Icon::Clear), text().content(i18n::bind("回收站"))))
                .on_press(move || read_collection(&trash, Collection::Trash)),
            text().class("files-sidebar-heading").content(i18n::bind("Favorites")),
            each(state.map(|s| files_ui::preferences(s).favorites.clone()))
                .key(String::clone)
                .children(move |item| {
                    let path = item.value;
                    let open = favorites.clone();
                    sapid::button()
                        .class("files-place")
                        .name(i18n::bind(path.map(|path| format!("Open favorite {path}"))))
                        .pressed(state.map(move |s| {
                            path.with(|path| location(s) == path
                                || matches!(collection(s), Some(Collection::Favorite(selected)) if selected == path))
                        }))
                        .fill((icons::glyph(Icon::Star), text().content(path.map(|path| files_ui::item_name(path).to_owned()))))
                        .on_press(move || read_collection(&open, Collection::Favorite(path.get())))
                }),
            fragment(files_ui::tag_links(state, tag, move |tag| {
                read_collection(&ui, Collection::Tag(tag));
            })),
        ))
}

fn panel(ui: Ui, formats: Formats) -> impl Widget {
    let state = ui.state;
    let up = ui.clone();
    let refresh = ui.clone();
    let jump = ui.clone();
    let places = ui.clone();
    let enabled = state.map(|s| !location(s).is_empty());
    col().class("file-picker-browser").fill((
        row().class("pathbar").fill((
            sapid::button()
                .class("icon-button files-sidebar-toggle")
                .name(i18n::bind("Toggle file picker sidebar"))
                .pressed(state.map(|s| !s.desktop_file_picker_sidebar_hidden))
                .fill(icons::glyph(Icon::Sidebar))
                .on_press(move || {
                    state.revise(|s| {
                        s.desktop_file_picker_sidebar_hidden = !s.desktop_file_picker_sidebar_hidden
                    })
                }),
            i18n::icon_action("Parent folder", Icon::Up)
                .enabled(state.map(|s| up_path(s).is_some()))
                .on_press(move || {
                    if let Some(path) = state.with(up_path) {
                        read_directory(&up, path);
                    }
                }),
            col().class("path-location").fill((
                when(state.map(|s| collection(s).is_none()), move || {
                    let jump = jump.clone();
                    file_browser::breadcrumb_bar(
                        state,
                        location,
                        true,
                        i18n::bind("远程路径"),
                        |_| false,
                        enabled,
                        move |path| read_directory(&jump, path),
                    )
                }),
                when(state.map(|s| collection(s).is_some()), move || {
                    text()
                        .class("breadcrumb current")
                        .content(i18n::bind(state.map(|s| {
                            match collection(s) {
                                Some(Collection::Favorite(path)) => files_ui::item_name(path),
                                Some(Collection::Tag(tag)) => tag.label(),
                                Some(Collection::Trash) => "回收站",
                                None => "",
                            }
                            .to_owned()
                        })))
                }),
            )),
            i18n::icon_action("Refresh", Icon::Refresh)
                .enabled(state.map(|s| s.desktop_file_picker.is_some()))
                .on_press(move || {
                    if let Some(selected) = state.with(|s| collection(s).cloned()) {
                        read_collection(&refresh, selected);
                    } else {
                        let path = state.with(|s| location(s).to_owned());
                        read_directory(&refresh, path);
                    }
                }),
        )),
        splitter::files_body(
            ui.clone(),
            splitter::FileSidebar::Picker,
            move || sidebar(places.clone()),
            listing(ui, formats),
        ),
    ))
}

fn listing(ui: Ui, formats: Formats) -> impl Widget {
    let state = ui.state;
    let entries = state
        .map(move |s| {
            s.desktop_file_picker.as_ref().map_or_else(Vec::new, |d| {
                d.entries
                    .iter()
                    .filter(|entry| s.config.settings.show_hidden_files || !entry.is_hidden())
                    .filter(|entry| d.filter.includes(entry, formats))
                    .cloned()
                    .collect()
            })
        })
        .with_partial_eq();
    col().class("file-picker-list-pane").fill((
        row().class("file-header").fill((
            text().class("column-name").content(i18n::bind("名称")),
            text().class("column-size").content(i18n::bind("大小")),
        )),
        col().class("file-list").fill((
            each(entries)
                .key(|entry| entry.path.clone())
                .children(move |item| {
                    let entry = item.value;
                    let browse = ui.clone();
                    let open = ui.clone();
                    row()
                        .class(state.map(move |s| {
                            if entry.with(|e| selected_path(s) == Some(e.path.as_str())) {
                                "file-row file-picker-row row-selected"
                            } else {
                                "file-row file-picker-row"
                            }
                            .to_owned()
                        }))
                        .fill((
                            button()
                                .class(file_browser::entry_name_class(
                                    state,
                                    entry,
                                    |file| file,
                                    true,
                                ))
                                .semantics(state.map(move |s| {
                                    entry.with(|e| {
                                        Some(Box::new(Semantics {
                                            pressed: (!e.is_dir)
                                                .then(|| selected_path(s) == Some(e.path.as_str())),
                                            ..Default::default()
                                        }))
                                    })
                                }))
                                .name(entry.map(|e| e.name.clone()))
                                .enabled(state.map(move |s| {
                                    entry.with(|e| !office_sync::uploading(s, &e.path))
                                }))
                                .fill(file_browser::entry_name(
                                    state,
                                    entry,
                                    |file| file,
                                    true,
                                    true,
                                ))
                                .on(activate, move |_| {
                                    let e = entry.get();
                                    if e.is_dir {
                                        read_directory(&browse, e.path);
                                    } else {
                                        state.revise(|s| {
                                            let saving = matches!(
                                                current(s),
                                                Some(Purpose::SaveText(_) | Purpose::SaveOffice(_))
                                            );
                                            if let Some(directory) = &mut s.desktop_file_picker {
                                                directory.selected = Some(e.path);
                                                if saving {
                                                    directory.name = e.name;
                                                    directory.replace = None;
                                                    s.dialog_error.clear();
                                                }
                                            }
                                        });
                                    }
                                }),
                            sapid::button()
                                .class("hidden file-picker-open-entry")
                                .on_press(move || {
                                    let file = entry.get();
                                    if !file.is_dir
                                        && state.with(|s| {
                                            s.desktop_file_picker.as_ref().is_some_and(
                                                |directory| {
                                                    directory.request.is_none()
                                                        && directory.entries.contains(&file)
                                                        && directory.filter.includes(&file, formats)
                                                },
                                            )
                                        })
                                    {
                                        if matches!(
                                            state.with(current),
                                            Some(Purpose::SaveOffice(_))
                                        ) {
                                            set_save_name(&open, file.name);
                                        } else {
                                            confirm(&open, &file.path);
                                        }
                                    }
                                }),
                            text().class("file-size").content(i18n::bind(
                                entry.map(|e| if e.is_dir { "—".into() } else { size(e.size) }),
                            )),
                        ))
                }),
            text()
                .class("file-picker-message")
                .content(i18n::bind(state.map(move |s| {
                    let Some(d) = &s.desktop_file_picker else {
                        return String::new();
                    };
                    if d.request.is_some() {
                        "Loading…".into()
                    } else if !d.error.is_empty() {
                        d.error.clone()
                    } else if entries.with(Vec::is_empty) {
                        if d.collection.is_some() {
                            return "没有匹配的项目".into();
                        }
                        match d.filter {
                            Filter::Supported => formats.empty.into(),
                            Filter::All => "此文件夹为空".into(),
                            _ => "没有匹配的项目".into(),
                        }
                    } else {
                        String::new()
                    }
                }))),
        )),
    ))
}
