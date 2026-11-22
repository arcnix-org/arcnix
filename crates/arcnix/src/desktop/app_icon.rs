//! Native window presentation for the server desktop.
use std::collections::HashMap;
use std::task::Waker;

use glory::wry::shell::{Fullscreen, NativeLifecycle, NativeWindow, ShellError};
#[cfg(target_os = "macos")]
use tao::platform::macos::WindowExtMacOS;

use crate::{Ui, WorkspacePage, code_native};

pub(crate) struct DesktopWindow {
    ui: Ui,
    #[cfg(not(target_os = "macos"))]
    fullscreen: bool,
    #[cfg(target_os = "macos")]
    previous_fullscreen: Option<Fullscreen>,
    wake: Option<Waker>,
    epoch: Option<u64>,
    code: HashMap<u64, code_native::Host>,
    #[cfg(target_os = "macos")]
    icon_installed: bool,
    #[cfg(windows)]
    browsers: HashMap<u64, crate::web_browser_native::Host>,
    #[cfg(windows)]
    video: crate::mpv_native::Host,
    #[cfg(windows)]
    exiting: bool,
    #[cfg(windows)]
    shortcuts: super::windows_shortcuts::Capture,
}

impl DesktopWindow {
    pub(crate) fn new(ui: Ui) -> Self {
        Self {
            #[cfg(windows)]
            video: crate::mpv_native::Host::new(ui.clone()),
            ui,
            #[cfg(not(target_os = "macos"))]
            fullscreen: false,
            #[cfg(target_os = "macos")]
            previous_fullscreen: None,
            wake: None,
            epoch: None,
            code: HashMap::new(),
            #[cfg(target_os = "macos")]
            icon_installed: false,
            #[cfg(windows)]
            browsers: HashMap::new(),
            #[cfg(windows)]
            exiting: false,
            #[cfg(windows)]
            shortcuts: Default::default(),
        }
    }

    #[cfg(target_os = "macos")]
    fn sync_fullscreen(&mut self, window: &NativeWindow, fullscreen: bool) {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{
            NSApplication, NSApplicationPresentationOptions, NSWindow, NSWindowStyleMask,
        };

        if !fullscreen {
            if window.simple_fullscreen() {
                window.set_simple_fullscreen(false);
            }
            if let Some(previous) = self.previous_fullscreen.take() {
                window.set_fullscreen(Some(previous));
            }
            return;
        }
        if window.simple_fullscreen() {
            return;
        }
        if let Some(previous) = window.fullscreen() {
            if self.previous_fullscreen.is_none() {
                self.previous_fullscreen = Some(previous);
                window.set_fullscreen(None);
            }
            return;
        }
        // The native fullscreen exit animates after Tao clears its fullscreen state.
        let native = unsafe { &*window.ns_window().cast::<NSWindow>() };
        if native.styleMask().contains(NSWindowStyleMask::FullScreen) {
            return;
        }
        // A titleless fullscreen window lets the desktop handle the top edge.
        // Tao restores the window frame and previous presentation options on exit.
        if window.set_simple_fullscreen(true) {
            let mtm = MainThreadMarker::new().expect("native lifecycle runs on the main thread");
            NSApplication::sharedApplication(mtm).setPresentationOptions(
                NSApplicationPresentationOptions::HideMenuBar
                    | NSApplicationPresentationOptions::HideDock,
            );
        }
    }
}

impl NativeLifecycle for DesktopWindow {
    fn poll(&mut self, window: &NativeWindow) -> Result<bool, ShellError> {
        #[cfg(target_os = "macos")]
        if !self.icon_installed {
            // The event loop applies the macOS activation policy before the first poll.
            install_macos_icon();
            self.icon_installed = true;
        }
        let state = self.ui.state;
        let fullscreen = state.with(crate::server_desktop::fullscreen);
        self.ui.player.sync_surface(&self.ui);
        #[cfg(target_os = "macos")]
        self.sync_fullscreen(window, fullscreen);
        #[cfg(not(target_os = "macos"))]
        if self.fullscreen != fullscreen {
            window.set_fullscreen(
                fullscreen.then(|| Fullscreen::Borderless(window.current_monitor())),
            );
            self.fullscreen = fullscreen;
        }
        let (epoch, code_open) = state.with(|s| {
            (
                s.connected.then_some(s.epoch),
                s.workbench.apps.contains(&WorkspacePage::Code),
            )
        });
        if self.epoch != epoch {
            if let Some(old) = self.epoch {
                if let Some(code) = self.code.get_mut(&old) {
                    code.hide();
                }
                #[cfg(windows)]
                if let Some(browser) = self.browsers.get_mut(&old) {
                    browser.hide();
                }
            }
            self.epoch = epoch;
        }
        // Code remains available in read-only sessions where Terminal is disabled.
        state.with(|s| {
            self.code.retain(|&id, _| {
                epoch == Some(id)
                    || s.parked
                        .values()
                        .any(|session| session.connected && session.epoch == id)
            });
        });
        if let Some(epoch) = epoch {
            if code_open {
                self.code
                    .entry(epoch)
                    .or_insert_with(|| {
                        let mut code = code_native::Host::new(self.ui.clone());
                        code.wake = self.wake.clone();
                        code
                    })
                    .poll(window);
            } else if let Some(mut code) = self.code.remove(&epoch) {
                code.poll(window);
            }
        }
        #[cfg(windows)]
        {
            self.shortcuts.poll(&self.ui, window, self.wake.as_ref());
            self.video.poll(window);
            if self.exiting {
                return Ok(self
                    .browsers
                    .values_mut()
                    .all(|browser| browser.exit_ready()));
            }
            let live = state.with(|s| s.connected_epochs());
            self.browsers.retain(|epoch, _| live.contains(epoch));
            if let Some(epoch) = epoch {
                if state
                    .with(|s| crate::server_desktop::launcher_running(s, WorkspacePage::Browser))
                {
                    self.browsers.entry(epoch).or_insert_with(|| {
                        let mut browser = crate::web_browser_native::Host::new(self.ui.clone());
                        if let Some(wake) = &self.wake {
                            browser.set_wake(wake.clone());
                        }
                        browser
                    });
                }
                if let Some(browser) = self.browsers.get_mut(&epoch) {
                    browser.poll(window);
                }
            }
        }
        Ok(false)
    }

    #[cfg(windows)]
    fn close_requested(&mut self, _window: &NativeWindow) -> Result<bool, ShellError> {
        self.exiting = true;
        let mut ready = true;
        for browser in self.browsers.values_mut() {
            ready &= browser.close_requested();
        }
        Ok(ready)
    }

    fn closed(&mut self, _window: &NativeWindow) {
        #[cfg(target_os = "macos")]
        if _window.simple_fullscreen() {
            _window.set_simple_fullscreen(false);
        }
        #[cfg(windows)]
        for browser in self.browsers.values_mut() {
            browser.close();
        }
    }

    #[cfg(not(windows))]
    fn opened(
        &mut self,
        _window: &NativeWindow,
        wake: &std::task::Waker,
    ) -> Result<(), ShellError> {
        self.wake = Some(wake.clone());
        Ok(())
    }

    #[cfg(windows)]
    fn opened(&mut self, window: &NativeWindow, wake: &std::task::Waker) -> Result<(), ShellError> {
        self.wake = Some(wake.clone());
        if let Err(error) = super::windows_shortcuts::use_window_keyboard_input() {
            self.ui.log(format!("Windows shortcut setup: {error}"));
        }
        use glory::wry::shell::NativeWindowExtWindows;
        use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_SHARED, LoadImageW, SM_CXICON,
            SM_CXSMICON, SM_CYICON, SM_CYSMICON, SendMessageW, WM_SETICON,
        };
        use windows::core::PCWSTR;

        // LR_SHARED gives the module ownership of these resource-backed handles.
        unsafe {
            let instance = GetModuleHandleW(None)
                .map_err(|error| glory::wry::shell::ShellError(error.to_string()))?;
            for (kind, width, height) in [
                (ICON_SMALL, SM_CXSMICON, SM_CYSMICON),
                (ICON_BIG, SM_CXICON, SM_CYICON),
            ] {
                let icon = LoadImageW(
                    Some(HINSTANCE(instance.0)),
                    PCWSTR(std::ptr::without_provenance(1)),
                    IMAGE_ICON,
                    GetSystemMetrics(width),
                    GetSystemMetrics(height),
                    LR_SHARED,
                )
                .map_err(|error| glory::wry::shell::ShellError(error.to_string()))?;
                SendMessageW(
                    HWND(window.hwnd() as *mut _),
                    WM_SETICON,
                    Some(WPARAM(kind as usize)),
                    Some(LPARAM(icon.0 as isize)),
                );
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn install_macos_icon() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let mtm = MainThreadMarker::new().expect("the desktop event loop runs on the main thread");
    let data = NSData::with_bytes(include_bytes!("../../assets/arcnix.png"));
    if let Some(icon) = NSImage::initWithData(NSImage::alloc(), &data) {
        // SAFETY: The setter receives a valid, non-null image on the main thread.
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&icon)) };
    }
}
