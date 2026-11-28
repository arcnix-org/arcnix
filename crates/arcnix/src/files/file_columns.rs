use crate::*;

pub(crate) fn visible(s: &State, remote: bool) -> VisibleFileColumns {
    files_view::current(s, remote).columns
}

fn set(ui: &Ui, remote: bool, edit: impl FnOnce(&mut VisibleFileColumns)) {
    let view = ui.state.with(|s| files_view::current(s, remote));
    let mut columns = view.columns;
    edit(&mut columns);
    files_view::change(ui, remote, |v| {
        v.columns = Some(columns);
        if (view.sort == Sort::Size && !columns.size)
            || (view.sort == Sort::Modified && !columns.modified)
        {
            v.sort = Some(Sort::Name);
            v.descending = Some(false);
        }
    });
}
pub(crate) fn open(ui: &Ui, remote: bool) {
    if ui.state.with(|s| s.file_pending.is_none()) {
        ui.open_dialog(Dialog::FileColumns { remote });
    }
}
fn choice(
    ui: Ui,
    remote: bool,
    desktop: bool,
    label: &'static str,
    get: fn(VisibleFileColumns) -> bool,
    write: fn(&mut VisibleFileColumns, bool),
) -> impl Widget {
    sapid::checkbox()
        .class(ui.state.map(move |s| {
            if get(visible(s, remote)) {
                "column-option active"
            } else {
                "column-option"
            }
            .to_owned()
        }))
        .name(i18n::bind_in(desktop, label))
        .label(i18n::bind_in(desktop, label))
        .checked(ui.state.map(move |s| get(visible(s, remote))))
        .on_checked_change(move |checked| set(&ui, remote, |columns| write(columns, checked)))
}
pub(crate) fn dialog(ui: Ui) -> impl Widget {
    let state = ui.state;
    when(
        state.map(|s| matches!(s.dialog, Some(Dialog::FileColumns { .. }))),
        move || {
            let remote =
                state.with(|s| matches!(s.dialog, Some(Dialog::FileColumns { remote: true })));
            let desktop = state.with(server_desktop::owns_dialog);
            let restore = ui.clone();
            let close = ui.clone();
            modal_with_header(
                ui.clone(),
                "显示列",
                settings::restore_button(desktop, "Restore default columns", move || {
                    files_view::change(&restore, remote, |v| v.columns = None)
                }),
                col().class("column-settings").fill((
                    text().class("subtle").content(state.map(move |s| {
                        if remote {
                            s.remote_path.clone()
                        } else {
                            s.local_path.clone()
                        }
                    })),
                    text().class("column-hint").content(i18n::bind_in(
                        desktop,
                        "名称始终显示。更改仅应用于此文件夹。",
                    )),
                    fragment(
                        files_view::COLUMN_CHOICES
                            .iter()
                            .map(|&(label, get, write)| {
                                choice(ui.clone(), remote, desktop, label, get, write)
                            })
                            .collect::<Vec<_>>(),
                    ),
                )),
                sapid::button()
                    .class("button primary")
                    .label(i18n::bind_in(desktop, "完成"))
                    .on_press(move || close.close_dialog()),
            )
        },
    )
}
