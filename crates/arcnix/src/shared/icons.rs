//! Sapid action primitives with decorative SVG masks for the Wry host.
//! Wry does not render Glory Graphic yet; CSS masks retain currentColor.
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use glory::prelude::*;

use crate::i18n;

// Keep the CSS name, bundled SVG and optional color artwork in one registry.
macro_rules! icons {
    ($($variant:ident => $line:literal $(as $class:literal)? $(, $color:literal)?;)+) => {
        #[derive(Clone, Copy)]
        pub enum Icon { $($variant),+ }

        impl Icon {
            const ALL: &[Self] = &[$(Self::$variant),+];

            pub(crate) fn name(self) -> &'static str {
                match self { $(Self::$variant => icons!(@name $line $(as $class)?)),+ }
            }

            fn svg(self) -> &'static str {
                match self { $(Self::$variant => include_str!(concat!("../../assets/icons/", $line, ".svg"))),+ }
            }

            pub(crate) fn color(self) -> Option<(&'static str, &'static str)> {
                match self { $(Self::$variant => icons!(@color $($color)?)),+ }
            }
        }
    };
    (@name $line:literal as $class:literal) => { $class };
    (@name $line:literal) => { $line };
    (@color $name:literal) => { Some(($name, include_str!(concat!("../../assets/icons/color/", $name, ".svg")))) };
    (@color) => { None };
}

icons! {
    Search => "search";
    Tag => "tag";
    Browser => "browser", "browser";
    ChevronRight => "chevron-right";
    Server => "server";
    Bell => "bell";
    Paste => "paste";
    Home => "home", "home";
    Folder => "folder", "files";
    File => "file", "document";
    Syntra => "syntra", "office";
    Grid => "grid", "grid";
    List => "list";
    Sidebar => "sidebar";
    Copy => "copy";
    Edit => "edit", "editor";
    Code => "code", "code";
    Settings => "settings", "settings";
    Info => "info";
    Close => "close";
    More => "more";
    Up => "arrow-up" as "up";
    Go => "arrow-right" as "go";
    Refresh => "refresh";
    Power => "power";
    Save => "save";
    Filter => "filter";
    Collapse => "chevron-down" as "collapse";
    Expand => "chevron-up" as "expand";
    Pause => "pause";
    Volume => "volume";
    VolumeOff => "volume-off";
    Play => "play", "player";
    OpenVideo => "open-video";
    OpenDocument => "open-document";
    ZoomIn => "zoom-in";
    ZoomOut => "zoom-out";
    Clear => "trash" as "clear", "trash";
    Plus => "plus";
    Minus => "minus";
    Upload => "upload";
    Download => "download", "download";
    Star => "star", "star";
    StarFilled => "star-filled";
    Fullscreen => "fullscreen";
    ExitFullscreen => "exit-fullscreen";
    Pin => "pin";
    Docker => "docker", "docker";
    Container => "box";
    Layers => "layers";
    Plug => "plug";
    Key => "key";
    Stop => "stop";
    Database => "database", "pgadmin";
    WebServer => "web-server", "nginx";
    Redis => "redis", "redis";
    Disks => "disks", "disks";
    Monitor => "monitor", "monitor";
    AppCenter => "app-center", "center";
    Terminal => "terminal", "terminal";
    Services => "services";
    Logs => "logs";
    WindowMinimize => "window-minimize";
    WindowMaximize => "window-maximize";
    WindowRestore => "window-restore";
    Desktop => "desktop", "desktop";
    Image => "image", "image";
    Video => "video", "video";
    Audio => "audio", "audio";
}

pub(crate) fn glyph(icon: Icon) -> impl Widget {
    text()
        .class(format!("action-glyph glyph-{}", icon.name()))
        .content("")
}

pub(crate) fn disclosure_icon(expanded: impl Into<Vary<bool>>) -> impl Widget {
    let expanded = expanded.into();
    text()
        .class_fn(move || {
            if expanded.get() {
                "action-glyph disclosure-icon sapid-chevron-down"
            } else {
                "action-glyph disclosure-icon sapid-chevron-right"
            }
            .to_owned()
        })
        .content("")
}

fn disclosure_svg(icon: sapid::Icon) -> String {
    use glory::render::PathCommand;
    // Wry 0.7 rejects Graphic nodes. Keep Sapid's own geometry and stroke, and
    // render it through the same SVG mask path as the other desktop icons.
    let drawing = icon.drawing(glory::style::Color(0, 0, 0, 255));
    let path = drawing
        .commands
        .iter()
        .map(|command| match command {
            PathCommand::Move(point) => format!("M{} {}", point.x, point.y),
            PathCommand::Line(point) => format!("L{} {}", point.x, point.y),
            PathCommand::Cubic { first, second, end } => format!(
                "C{} {} {} {} {} {}",
                first.x, first.y, second.x, second.y, end.x, end.y
            ),
            PathCommand::Close => "Z".to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    let stroke = drawing.stroke.expect("Sapid chevrons have a stroke");
    let cap = match stroke.cap {
        glory::render::StrokeCap::Butt => "butt",
        glory::render::StrokeCap::Round => "round",
        glory::render::StrokeCap::Square => "square",
    };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {} {}\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"{}\" stroke-linecap=\"{cap}\" stroke-linejoin=\"round\"><path d=\"{path}\"/></svg>",
        drawing.width, drawing.height, stroke.width
    )
}

pub fn icon_action(name: impl Into<String>, icon: Icon) -> sapid::ActionIcon {
    let name = name.into();
    sapid::action_icon(i18n::translate(&name))
        .icon(glyph(icon))
        .variant(sapid::ActionIconVariant::Subtle)
        .size(sapid::Size::Small)
        .class("icon-button")
}

pub fn close_action(name: impl Into<String>) -> sapid::CloseButton {
    sapid::close_button()
        .name(i18n::translate(&name.into()))
        .icon(glyph(Icon::Close))
        .size(sapid::Size::Small)
        .class("icon-button")
}

pub fn text_action(name: impl Into<String>, icon: Icon) -> sapid::Button {
    let name = name.into();
    sapid::button()
        .name(i18n::bind(name.clone()))
        .class("button with-icon")
        .fill((glyph(icon), text().content(i18n::bind(name))))
}

pub fn css() -> String {
    let mut css = String::new();
    for (selector, svg) in [
        (".brand-icon", include_str!("../../assets/arcnix-dark.svg")),
        (
            ".theme-light .brand-icon",
            include_str!("../../assets/arcnix-light.svg"),
        ),
    ] {
        let encoded = svg
            .bytes()
            .map(|byte| format!("%{byte:02X}"))
            .collect::<String>();
        css.push_str(&format!(
            "{selector}{{background-image:url(\"data:image/svg+xml,{encoded}\")}}\n"
        ));
    }
    for &icon in Icon::ALL {
        let name = icon.name();
        let line = STANDARD.encode(icon.svg());
        css.push_str(&format!(
            ".glyph-{name}{{mask-image:url(\"data:image/svg+xml;base64,{line}\")}}\n"
        ));
        if matches!(icon, Icon::Upload) {
            css.push_str(&format!(".file-upload-badge::before{{mask-image:url(\"data:image/svg+xml;base64,{line}\")}}\n"));
        }
        if let Some((art, svg)) = icon.color() {
            let color = STANDARD.encode(svg);
            css.push_str(&format!(
                ".app-{art} .desktop-app-icon::before{{mask-image:url(\"data:image/svg+xml;base64,{line}\")}}\n\
                 .app-{art},.glyph-{name}{{--color-app-icon:url(\"data:image/svg+xml;base64,{color}\")}}\n"
            ));
        }
    }
    for (name, icon) in [
        ("right", sapid::Icon::ChevronRight),
        ("down", sapid::Icon::ChevronDown),
    ] {
        let encoded = disclosure_svg(icon)
            .bytes()
            .map(|byte| format!("%{byte:02X}"))
            .collect::<String>();
        css.push_str(&format!(
            ".sapid-chevron-{name}{{mask-image:url(\"data:image/svg+xml,{encoded}\")}}\n"
        ));
    }
    css
}
