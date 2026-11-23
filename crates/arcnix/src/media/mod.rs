#[cfg(windows)]
pub(crate) mod browser_cookies;
pub(crate) mod image_viewer;
#[cfg(target_os = "macos")]
pub(crate) mod mac_surface;
#[cfg(windows)]
pub(crate) mod mpv_native;
pub(crate) mod native_viewport;
pub(crate) mod pdf_reader;
pub(crate) mod video_player;
#[cfg(windows)]
pub(crate) mod web_browser;
#[cfg(windows)]
pub(crate) mod web_browser_native;

use crate::*;

pub(crate) fn reader_control(label: &'static str, class: &str, icon: Icon) -> sapid::Button {
    sapid::button()
        .class(format!("icon-button viewer-{class}"))
        .name(i18n::desktop::bind(label))
        .fill(icons::glyph(icon))
}

pub(crate) fn viewer_fullscreen_button(state: Cage<State>, page: WorkspacePage) -> impl Widget {
    let expanded = state.map(move |s| server_desktop::reader_fullscreen(s, page));
    sapid::button()
        .class("icon-button viewer-fullscreen")
        .name(i18n::desktop::bind(state.map(move |s| {
            if !server_desktop::reader_fullscreen(s, page) {
                "View fullscreen in Desktop"
            } else if page == WorkspacePage::ImageViewer {
                "Exit image fullscreen"
            } else {
                "Exit PDF fullscreen"
            }
            .to_owned()
        })))
        .pressed(expanded)
        .fill(
            when(expanded, || icons::glyph(Icon::ExitFullscreen))
                .otherwise(|| icons::glyph(Icon::Fullscreen)),
        )
        .on_press(move || {
            state.revise(|s| match page {
                WorkspacePage::ImageViewer => s.images.fullscreen = !s.images.fullscreen,
                WorkspacePage::PdfReader => s.pdf.fullscreen = !s.pdf.fullscreen,
                _ => {}
            })
        })
}
