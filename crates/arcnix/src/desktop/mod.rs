pub(crate) mod app_icon;
pub(crate) mod app_settings;
pub(crate) mod desktop_appearance_ui;
pub(crate) mod desktop_apps;
pub(crate) mod desktop_customization_ui;
pub(crate) mod desktop_power;
pub(crate) mod desktop_shortcuts_ui;
pub(crate) mod desktop_system_settings;
pub(crate) mod device_info_ui;
#[cfg(target_os = "linux")]
pub(crate) mod linux_icon;
pub(crate) mod server_apps_ui;
pub(crate) mod server_apps_worker;
pub(crate) mod server_desktop;
pub(crate) mod wallpaper_ui;
pub(crate) mod window_tabs;
#[cfg(windows)]
mod windows_shortcuts;
