//! Seekable SFTP media source shared by the web and native players.
use std::io::{Read, Seek, SeekFrom};
use std::result::Result;
use std::sync::{Mutex, mpsc};

use glory::wry::shell::{AssetProvider, AssetSource};
use glory::wry::{LocalMedia, MediaKind, Renderer};

use crate::i18n::desktop as i18n;
use crate::i18n::desktop::text_action;
use crate::*;

const WEB_FORMATS: &[(&str, &str)] = &[
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("webm", "video/webm"),
    ("ogv", "video/ogg"),
];
// Pin native demuxers so opening a remote file cannot load a playlist.
const NATIVE_FORMATS: &[(&str, &str)] = &[
    ("mkv", "mkv"),
    ("mov", "mov"),
    ("avi", "avi"),
    ("mpeg", "mpeg"),
    ("mpg", "mpeg"),
    ("ts", "mpegts"),
    ("m2ts", "mpegts"),
    ("flv", "flv"),
    ("wmv", "asf"),
    ("vob", "mpeg"),
    ("3gp", "mov"),
    ("asf", "asf"),
];
const NATIVE_AVAILABLE: bool = cfg!(all(windows, target_arch = "x86_64"));

type ReadResult = Result<Vec<u8>, String>;

fn web_format(path: &str) -> Option<(&'static str, &'static str)> {
    let extension = arcnix_core::file_associations::extension(path)?;
    WEB_FORMATS
        .iter()
        .copied()
        .find(|(ext, _)| *ext == extension)
}

fn native_demuxer(path: &str) -> Option<&'static str> {
    let extension = arcnix_core::file_associations::extension(path)?;
    NATIVE_FORMATS
        .iter()
        .find_map(|(ext, demuxer)| (*ext == extension).then_some(*demuxer))
}

pub(crate) fn supports(path: &str) -> bool {
    web_format(path).is_some() || (NATIVE_AVAILABLE && native_demuxer(path).is_some())
}

pub(crate) fn supported_extensions() -> impl Iterator<Item = &'static str> {
    WEB_FORMATS.iter().map(|(ext, _)| *ext).chain(
        NATIVE_FORMATS
            .iter()
            .filter(|_| NATIVE_AVAILABLE)
            .map(|(ext, _)| *ext),
    )
}

struct StreamFormat {
    extension: &'static str,
    mime: &'static str,
    #[cfg(windows)]
    native_demuxer: Option<&'static str>,
}

impl StreamFormat {
    fn web(extension: &'static str, mime: &'static str) -> Self {
        Self {
            extension,
            mime,
            #[cfg(windows)]
            native_demuxer: None,
        }
    }

    fn native(_demuxer: &'static str) -> Self {
        Self {
            extension: "media",
            mime: "application/octet-stream",
            #[cfg(windows)]
            native_demuxer: Some(_demuxer),
        }
    }
}

fn stream_format(
    path: &str,
    file: &mut (impl Read + Seek),
    size: u64,
    prefer_content: bool,
) -> anyhow::Result<StreamFormat> {
    let named = || {
        web_format(path)
            .map(|(extension, mime)| StreamFormat::web(extension, mime))
            .or_else(|| {
                native_demuxer(path)
                    .filter(|_| NATIVE_AVAILABLE)
                    .map(StreamFormat::native)
            })
    };
    if !prefer_content && let Some(format) = named() {
        return Ok(format);
    }
    let mut header = [0; 4096];
    let len = size.min(header.len() as u64) as usize;
    file.read_exact(&mut header[..len])?;
    file.seek(SeekFrom::Start(0))?;
    detected_format(&header[..len], path)
        .or_else(named)
        .ok_or_else(|| anyhow::anyhow!("Player could not identify a supported video format"))
}

fn detected_format(header: &[u8], path: &str) -> Option<StreamFormat> {
    let native = |demuxer| NATIVE_AVAILABLE.then(|| StreamFormat::native(demuxer));
    match header {
        [_, _, _, _, b'f', b't', b'y', b'p', _, _, _, _, ..] => {
            // ISO base media and QuickTime share the same box structure.
            let brand = &header[8..12];
            if NATIVE_AVAILABLE
                && (brand == b"qt  "
                    || brand.starts_with(b"3g")
                    || native_demuxer(path) == Some("mov"))
            {
                native("mov")
            } else {
                Some(StreamFormat::web("mp4", "video/mp4"))
            }
        }
        [0x1a, 0x45, 0xdf, 0xa3, ..] => match ebml_doctype(header) {
            Some(b"webm") => Some(StreamFormat::web("webm", "video/webm")),
            Some(b"matroska") => native("mkv"),
            _ => None,
        },
        [b'O', b'g', b'g', b'S', ..] => Some(StreamFormat::web("ogv", "video/ogg")),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'A',
            b'V',
            b'I',
            b' ',
            ..,
        ] => native("avi"),
        [b'F', b'L', b'V', ..] => native("flv"),
        [0, 0, 1, 0xba, ..] => native("mpeg"),
        [
            0x30,
            0x26,
            0xb2,
            0x75,
            0x8e,
            0x66,
            0xcf,
            0x11,
            0xa6,
            0xd9,
            0,
            0xaa,
            0,
            0x62,
            0xce,
            0x6c,
            ..,
        ] => native("asf"),
        _ if [(0, 188), (4, 192)].into_iter().any(|(offset, stride)| {
            (0..3).all(|packet| header.get(offset + packet * stride) == Some(&0x47))
        }) =>
        {
            native("mpegts")
        }
        _ => None,
    }
}

fn ebml_doctype(header: &[u8]) -> Option<&[u8]> {
    fn element(bytes: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
        let id_width = bytes.first()?.leading_zeros() as usize + 1;
        if id_width > 4 {
            return None;
        }
        let (id, bytes) = bytes.split_at_checked(id_width)?;
        let size_width = bytes.first()?.leading_zeros() as usize + 1;
        if size_width > 8 {
            return None;
        }
        let (size, bytes) = bytes.split_at_checked(size_width)?;
        let marker = 0x80 >> (size_width - 1);
        let size = size[1..]
            .iter()
            .fold(u64::from(size[0] & (marker - 1)), |size, byte| {
                (size << 8) | u64::from(*byte)
            });
        let (value, tail) = bytes.split_at_checked(usize::try_from(size).ok()?)?;
        Some((id, value, tail))
    }

    let (_, mut fields, _) = element(header)?;
    while let Some((id, value, tail)) = element(fields) {
        if id == [0x42, 0x82] {
            return Some(value);
        }
        fields = tail;
    }
    None
}
struct RangeRead {
    offset: u64,
    length: usize,
    reply: mpsc::Sender<ReadResult>,
}
pub(crate) struct Source {
    url: String,
    format: StreamFormat,
    size: u64,
    active: Arc<AtomicBool>,
    reads: mpsc::SyncSender<RangeRead>,
}
impl AssetSource for Source {
    fn len(&self) -> u64 {
        self.size
    }
    fn mime(&self) -> &str {
        self.format.mime
    }
    fn current(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    fn read_range(&self, offset: u64, length: usize) -> ReadResult {
        self.read(offset, length, || false)
    }
}
impl Source {
    #[cfg(windows)]
    pub(crate) fn native_demuxer(&self) -> Option<&'static str> {
        self.format.native_demuxer
    }

    pub(crate) fn read(
        &self,
        offset: u64,
        length: usize,
        cancelled: impl Fn() -> bool,
    ) -> ReadResult {
        if !self.current()
            || length > 1024 * 1024
            || length as u64 > self.size.saturating_sub(offset)
        {
            return Err("Invalid video range".into());
        }
        let (reply, result) = mpsc::channel();
        self.reads
            .send(RangeRead {
                offset,
                length,
                reply,
            })
            .map_err(|_| "Video closed")?;
        while self.current() && !cancelled() {
            match result.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(reply) => return reply,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            }
        }
        Err("Video closed".into())
    }
}

pub(crate) fn serve<Session, File: Read + Seek>(
    path: &str,
    prefer_content: bool,
    active: Arc<AtomicBool>,
    open_file: impl FnOnce() -> anyhow::Result<(Session, File, u64)>,
    ready: impl FnOnce(Result<Arc<Source>, String>),
) {
    // This thread owns both the connection and the handle, including their
    // potentially blocking destructors. UI and directory listings never wait on it.
    let open = || {
        anyhow::ensure!(active.load(Ordering::Acquire), "Video closed");
        let opened = open_file()?;
        anyhow::ensure!(opened.2 > 0, "Video file is empty");
        Ok::<_, anyhow::Error>(opened)
    };
    let (_session, mut file, size) = match open() {
        Ok(value) => value,
        Err(error) => {
            ready(Err(format!("{error:#}")));
            return;
        }
    };
    let format = match stream_format(path, &mut file, size, prefer_content) {
        Ok(format) => format,
        Err(error) => {
            ready(Err(format!("{error:#}")));
            return;
        }
    };
    let (reads, requests) = mpsc::sync_channel(4);
    ready(Ok(Arc::new(Source {
        url: format!("/media/{}.{}", new_id(), format.extension),
        format,
        size,
        active: active.clone(),
        reads,
    })));
    for RangeRead {
        offset,
        length,
        reply,
    } in requests
    {
        let result = (|| {
            anyhow::ensure!(active.load(Ordering::Acquire), "Video closed");
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = vec![0; length];
            for chunk in bytes.chunks_mut(128 * 1024) {
                anyhow::ensure!(active.load(Ordering::Acquire), "Video closed");
                file.read_exact(chunk)?;
            }
            Ok::<_, anyhow::Error>(bytes)
        })();
        let _ = reply.send(result.map_err(|e| e.to_string()));
        if !active.load(Ordering::Acquire) {
            break;
        }
    }
}

type CurrentSource = Arc<Mutex<Option<Arc<Source>>>>;
pub(crate) struct Assets {
    pub player: CurrentSource,
    pub wallpapers: PathBuf,
}
impl AssetProvider for Assets {
    fn get(&self, path: &str) -> Option<Arc<dyn AssetSource>> {
        if path.starts_with("/media/")
            && let Some(source) = self
                .player
                .lock()
                .unwrap()
                .as_ref()
                .filter(|s| s.url == path)
        {
            return Some(source.clone());
        }
        wallpaper_ui::asset(&self.wallpapers, path)
    }
}

#[derive(Clone)]
pub(crate) struct Playback {
    pub(crate) serial: u64,
    pub(crate) title: String,
    path: String,
    active: Arc<AtomicBool>,
    source: Option<Arc<Source>>,
    pub(crate) status: String,
    #[cfg(windows)]
    pub(crate) native: Option<crate::mpv_native::Playback>,
}
#[derive(Clone)]
pub(crate) struct Player {
    #[cfg(windows)]
    pub viewport: Cage<Option<native_viewport::Viewport>>,
    renderer: Renderer,
    node: Cage<Option<glory::render::NodeId>>,
    mounted: Rc<std::cell::RefCell<Option<(glory::render::NodeId, u64)>>>,
    pub source: CurrentSource,
}
impl Player {
    pub fn new(renderer: Renderer, scope: &Scope) -> Self {
        Self {
            #[cfg(windows)]
            viewport: scope.cage(None),
            renderer,
            node: scope.cage(None),
            mounted: Default::default(),
            source: Default::default(),
        }
    }
    pub fn open(&self, ui: &Ui, path: &str) {
        self.open_media(ui, path, false);
    }

    pub(crate) fn browse(&self, ui: &Ui) {
        let start = ui.state.with(|s| {
            s.video
                .as_ref()
                .and_then(|video| desktop_file_picker::parent(&video.path))
        });
        desktop_file_picker::open(ui, desktop_file_picker::Purpose::Video, start);
    }

    pub(crate) fn open_from_picker(&self, ui: &Ui, path: &str) {
        self.open_media(ui, path, true);
    }

    fn open_media(&self, ui: &Ui, path: &str, prefer_content: bool) {
        self.stop(ui);
        let active = Arc::new(AtomicBool::new(true));
        let serial = ui.send(Action::PlayVideo {
            path: path.to_owned(),
            prefer_content,
            active: active.clone(),
        });
        ui.state.revise(|s| {
            s.video = Some(Playback {
                serial,
                title: path.rsplit('/').next().unwrap_or(path).to_owned(),
                path: path.to_owned(),
                active,
                source: None,
                status: i18n::translate("正在打开视频…"),
                #[cfg(windows)]
                native: None,
            })
        });
        ui.open_desktop_app(WorkspacePage::Player);
    }
    pub fn close(&self, ui: &Ui) {
        self.stop(ui);
        ui.state
            .revise(|s| server_desktop::remove_window(s, WorkspacePage::Player));
    }
    pub fn stop(&self, ui: &Ui) {
        ui.state.revise(|s| {
            if desktop_file_picker::current(s) == Some(desktop_file_picker::Purpose::Video) {
                desktop_file_picker::dismiss(s);
            }
            if let Some(video) = s.video.take() {
                video.active.store(false, Ordering::Release);
            }
        });
    }

    pub fn ready(&self, ui: &Ui, serial: u64, result: Result<Arc<Source>, String>) {
        ui.state.revise(|s| {
            let Some(video) = s.video.as_mut().filter(|v| v.serial == serial) else {
                return;
            };
            match result {
                Ok(source) => {
                    #[cfg(windows)]
                    if source.native_demuxer().is_some() {
                        video.native.get_or_insert_with(Default::default).source = Some(source);
                        return;
                    }
                    video.source = Some(source);
                    video.status.clear();
                }
                Err(error) => video.status = i18n::translate(&error),
            }
        });
    }

    pub(crate) fn opened(&self, ui: &Ui, serial: u64) {
        if let Some(path) = ui.state.with(|s| {
            s.video
                .as_ref()
                .filter(|video| video.serial == serial)
                .map(|video| video.path.clone())
        }) {
            file_open::remember(ui, &path, arcnix_core::file_associations::FileApp::Player);
        }
    }

    pub(crate) fn sync_surface(&self, ui: &Ui) {
        let next = self.node.get().and_then(|node| {
            ui.state.with(|s| {
                let video = s.video.as_ref()?;
                Some((
                    node,
                    video.serial,
                    video.source.clone()?,
                    video.title.clone(),
                ))
            })
        });
        let key = next.as_ref().map(|(node, serial, ..)| (*node, *serial));
        let mut mounted = self.mounted.borrow_mut();
        if *mounted == key {
            return;
        }
        if let Some((node, _)) = *mounted {
            let _ = self.renderer.set_local_media(node, None);
        }
        *mounted = key;
        *self.source.lock().unwrap() = next.as_ref().map(|(_, _, source, _)| source.clone());
        if let Some((node, serial, source, title)) = next
            && let Err(error) = self.renderer.set_local_media(
                node,
                Some(LocalMedia {
                    kind: MediaKind::Video,
                    path: &source.url,
                    title: &title,
                }),
            )
        {
            ui.state.revise(|s| {
                if let Some(video) = s.video.as_mut().filter(|v| v.serial == serial) {
                    video.status = error.to_string();
                }
            });
        }
    }
}
fn empty_player(ui: Ui) -> impl Widget {
    let browse = ui.clone();
    col().class("player-idle").fill((
        col().class("player-idle-stage").fill((
            sapid::button()
                .class("player-open-disc")
                .name(i18n::bind("Open video"))
                .fill(icons::glyph(Icon::Play))
                .on_press(move || browse.player.browse(&browse)),
            text()
                .class("player-idle-title")
                .content(i18n::bind("Open video")),
        )),
        col().class("player-idle-controls").fill((
            view().class("player-empty-timeline"),
            row().class("player-idle-transport").fill((
                icons::glyph(Icon::Play),
                text().class("player-clock").content("0:00 / 0:00"),
                text_action("Browse files…", Icon::OpenVideo)
                    .class("button with-icon player-browse")
                    .on_press(move || ui.player.browse(&ui)),
            )),
        )),
    ))
}

pub(crate) fn panel(ui: Ui) -> impl Widget {
    let state = ui.state;
    let idle = ui.clone();
    col().class("remote-player").fill((
        text()
            .class("player-preferences hidden")
            .content(state.map(|s| {
                let prefs = &server_apps_ui::current(s).app_settings;
                serde_json::json!([prefs.player.volume, prefs.player.speed]).to_string()
            })),
        #[cfg(windows)]
        crate::mpv_native::bridge(ui.clone()),
        col()
            .class("remote-player-screen")
            .node_ref(ui.player.node)
            .fill(
                row()
                    .class("player-feedback")
                    .fill((icons::glyph(Icon::Play), icons::glyph(Icon::Pause))),
            ),
        input()
            .class("player-opened-input hidden")
            .on(value_changed, {
                let ui = ui.clone();
                move |event| {
                    if let Event::ValueChanged { value, .. } = event
                        && let Some(serial) = ui.state.with(|s| {
                            let video = s.video.as_ref()?;
                            (video.source.as_ref()?.url == value).then_some(video.serial)
                        })
                    {
                        ui.player.opened(&ui, serial);
                    }
                }
            }),
        when(state.map(|s| s.video.is_none()), move || {
            empty_player(idle.clone())
        }),
        #[cfg(windows)]
        crate::mpv_native::transport_bridge(ui.clone()),
        each(state.map(|s| s.video.iter().map(|video| video.serial).collect::<Vec<_>>()))
            .key(|serial| *serial)
            .children(move |_| {
                fragment((
                    text().class("player-labels hidden").content(state.map(|s| {
                        let language = desktop_system_settings::language(s);
                        let labels = [
                            "Play",
                            "Pause playback",
                            "Playback position",
                            "Volume",
                            "Mute",
                            "Unmute",
                        ]
                        .map(|label| crate::i18n::translate_for(label, language));
                        serde_json::json!(labels).to_string()
                    })),
                    view().class("player-control-bar"),
                ))
            }),
        text().class("remote-player-status").content(state.map(|s| {
            s.video
                .as_ref()
                .map(|v| v.status.clone())
                .unwrap_or_default()
        })),
        text().class("remote-player-error").content(i18n::bind(
            "Could not play this video. Check the connection and video codec.",
        )),
    ))
}
