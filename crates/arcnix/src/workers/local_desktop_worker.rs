use std::path::PathBuf;
use std::sync::atomic::Ordering;

use anyhow::{Result, ensure};
use arcnix_core::FileEntry;
use arcnix_core::desktop_customization::DesktopItems;
use arcnix_core::file_capabilities::{
    DirectoryReader, DocumentDestinationWriter, DocumentReader, DocumentWriter, EntryKind,
    FileEntryCreator, FileRenamer, TextPage, TextPageReader,
};
use arcnix_core::host_capabilities::DiskInventoryReader;
use arcnix_core::local_desktop::{Target, join};
use arcnix_core::metrics::ResourceReader;
use arcnix_core::settings_capabilities::{
    DesktopPreferencesReader, DesktopPreferencesWriter, HostClockReader, LocalDesktopPreferences,
};

use super::worker::{Action, FileFailure, Update, create_named_entry};
use crate::server_apps_worker::{Action as AppAction, Reply};

pub(super) struct Session {
    pub target: Target,
    pub home: String,
    pub prefs: PathBuf,
}

fn message<T>(result: Result<T>) -> Result<T, String> {
    result.map_err(|e| format!("{e:#}"))
}

impl Session {
    fn apps(&self, action: AppAction) -> Result<Reply> {
        let target = &self.target;
        Ok(match action {
            AppAction::SystemPower(action) => {
                target.system_power(action)?;
                Reply::Operated
            }
            AppAction::Discover => Reply::Applications(Default::default()),
            AppAction::Clock => Reply::Clock(target.read_host_clock()?),
            AppAction::DeviceInfo => Reply::DeviceInfo(target.device_info()?),
            AppAction::Disks => Reply::Disks(target.read_disk_inventory()?),
            AppAction::ReadTimeZone => Reply::TimeZone(arcnix_core::desktop_time_zone::TimeZones {
                system: false,
                name: None,
                choices: Vec::new(),
            }),
            AppAction::FileLocations => Reply::FileLocations(target.locations(&self.home)?),
            AppAction::RemoveWallpaperCache(request) => {
                request.remove_cached()?;
                Reply::WallpaperRemoved
            }
            AppAction::ReadWallpaper(request) => Reply::Wallpaper(request.local(target)?),
            AppAction::ReadImage(path) => Reply::Image(target.read_image(&path)?.into()),
            AppAction::FileItem(path) => Reply::FileItem(target.metadata(&path)?),
            AppAction::PickerEntries(paths) => {
                let mut entries = Vec::new();
                for path in paths {
                    if let Ok(m) = target.metadata(&path)
                        && !m.symlink
                    {
                        entries.push(FileEntry {
                            hidden_attribute: m.hidden_attribute,
                            name: arcnix_core::file_associations::filename(&path).into(),
                            path,
                            is_dir: m.directory,
                            is_symlink: false,
                            size: m.size,
                            modified: m.modified,
                            permissions: m.mode,
                        });
                    }
                }
                Reply::PickerEntries(entries)
            }
            AppAction::DesktopItems => {
                let location = target
                    .locations(&self.home)?
                    .into_iter()
                    .find(|loc| loc.name == "Desktop");
                let exists = location.is_some();
                let path = location.map_or_else(|| join(&self.home, "Desktop"), |loc| loc.path);
                let mut entries = if exists {
                    target.list_entries(&path)?.1
                } else {
                    Vec::new()
                };
                entries.retain(|e| !e.is_hidden() && !e.name.chars().any(char::is_control));
                entries.truncate(128);
                Reply::DesktopItems(DesktopItems {
                    home: self.home.clone(),
                    path,
                    exists,
                    entries,
                })
            }
            AppAction::ReadPreferences => {
                Reply::Preferences(LocalDesktopPreferences(&self.prefs).read_desktop_preferences()?)
            }
            AppAction::WritePreferences(pending) => Reply::SavedPreferences(
                LocalDesktopPreferences(&self.prefs).save_desktop_preferences(&pending)?,
            ),
            _ => anyhow::bail!("This action is unavailable on the local desktop"),
        })
    }

    pub fn dispatch(&self, action: Action) -> std::ops::ControlFlow<Update, Action> {
        let target = &self.target;
        let file_result = |r| message(r).map_err(FileFailure::ordinary);
        std::ops::ControlFlow::Break(match action {
            Action::AttachCodePage {
                path,
                snapshot,
                sudo,
            } => Update::CodeFile(message((|| {
                ensure!(sudo.is_none(), "Local editing uses the current account");
                let mut pages = target;
                let (snapshot, stamp) = pages.sample_page_tail(
                    TextPage {
                        path: &path,
                        snapshot,
                        reviewed_handle: None,
                    },
                    None,
                )?;
                Ok(serde_json::json!({ "snapshot":snapshot, "stamp":stamp }))
            })())),
            Action::CodeFile(request) => Update::CodeFile(message(
                request.execute(crate::code_workbench::Files::Local(target), || Ok(())),
            )),
            Action::MetricsSample(gpu) => Update::Metrics(message(target.sample_monitor(gpu))),
            Action::Keepalive => Update::Alive,
            Action::Remote(path) => match target.list_entries(&path) {
                Ok((path, entries)) => Update::Remote(path, entries, 0),
                Err(error) => Update::BrowseError {
                    remote: true,
                    message: format!("{error:#}"),
                },
            },
            Action::Children { remote: true, path } => Update::Children {
                result: message(target.list_entries(&path).map(|(_, v)| v)),
                parent: path,
                remote: true,
            },
            Action::CreateEntry {
                remote: true,
                parent,
                default_name,
                directory,
            } => {
                match create_named_entry(&default_name, |name| {
                    target.try_create_named(&parent, name, EntryKind::from_directory(directory))
                }) {
                    Ok(name) => Update::Created {
                        remote: true,
                        name,
                        directory,
                    },
                    Err(error) => Update::Error(format!("{error:#}")),
                }
            }
            Action::RemoteRename(from, to) => match target.rename_entry(&from, &to) {
                Ok(()) => Update::Renamed {
                    remote: true,
                    name: arcnix_core::file_associations::filename(&to).into(),
                },
                Err(error) => Update::Error(format!("{error:#}")),
            },
            Action::ResolveFileAlias(path) => {
                Update::FileAlias(message(target.resolve_alias(&path)))
            }
            Action::FileMetadata(path) => {
                Update::FileMetadata(message(target.file_properties(&path)))
            }
            Action::TextRead { path, cancel, sudo } => Update::TextFile(file_result((|| {
                ensure!(sudo.is_none(), "Local editing uses the current account");
                target.read_document(&path, &|| {
                    ensure!(!cancel.load(Ordering::Acquire), "Read cancelled");
                    Ok(())
                })
            })(
            ))),
            Action::TextSave {
                document,
                content,
                cancel,
                sudo,
            } => Update::TextFile(file_result((|| {
                ensure!(sudo.is_none(), "Local editing uses the current account");
                target.save_document(&document, &content, &|| {
                    ensure!(!cancel.load(Ordering::Acquire), "Save cancelled");
                    Ok(())
                })
            })())),
            Action::TextSaveAs {
                path,
                bytes,
                replace,
                cancel,
            } => Update::TextFile(file_result(target.write_document_destination(
                &path,
                &bytes,
                replace,
                &|| {
                    ensure!(!cancel.load(Ordering::Acquire), "Save cancelled");
                    Ok(())
                },
            ))),
            Action::ServerApps(action) => Update::ServerApps(Box::new(message(self.apps(action)))),
            other => return std::ops::ControlFlow::Continue(other),
        })
    }
}
