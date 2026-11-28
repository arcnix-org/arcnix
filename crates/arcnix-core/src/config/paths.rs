//! Product-only application directories. Organization paths are read only for migration.
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
#[cfg(windows)]
use directories::BaseDirs;
use directories::ProjectDirs;

pub(crate) fn project() -> Result<ProjectDirs> {
    ProjectDirs::from("", "", "Arcnix").context("Cannot find the application directory")
}

pub(crate) fn config_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        Ok(BaseDirs::new()
            .context("Cannot find the configuration directory")?
            .config_dir()
            .join("Arcnix"))
    }
    #[cfg(not(windows))]
    {
        Ok(project()?.config_dir().to_owned())
    }
}

pub(crate) fn local_data_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        Ok(BaseDirs::new()
            .context("Cannot find the local application data directory")?
            .data_local_dir()
            .join("Arcnix"))
    }
    #[cfg(not(windows))]
    {
        Ok(project()?.data_local_dir().to_owned())
    }
}

/// The media runtime can be regenerated, but reuse an existing verified cache.
pub fn media_cache_dir() -> Result<PathBuf> {
    let target = project()?.cache_dir().join("media");
    let legacy = legacy_project()?.cache_dir().join("media");
    if !target.try_exists()? && legacy.try_exists()? {
        fs::create_dir_all(target.parent().context("Missing cache directory")?)?;
        fs::rename(&legacy, &target).context("Cannot move the media runtime cache")?;
        remove_empty_parents(legacy.parent(), legacy_project()?.cache_dir().parent());
    }
    Ok(target)
}

pub(crate) fn legacy_project() -> Result<ProjectDirs> {
    ProjectDirs::from("org", "arcnix", "Arcnix")
        .context("Cannot find the previous application directory")
}

/// Adopt an existing SSH browser profile when that connection first opens.
/// The UI starts independently of potentially locked browser files.
#[cfg(windows)]
pub fn prepare_webview_profile(previous: &Path, current: &Path) -> Result<()> {
    if current.try_exists()? || !previous.try_exists()? {
        return Ok(());
    }
    use std::os::windows::fs::MetadataExt;

    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    let metadata = fs::symlink_metadata(previous)?;
    ensure!(
        metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "Cannot migrate a linked or invalid browser profile"
    );
    let parent = current
        .parent()
        .context("Missing WebView2 profile directory")?;
    fs::create_dir_all(parent)?;
    match fs::rename(previous, current) {
        Ok(()) => Ok(()),
        Err(_) if current.is_dir() => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            // Roaming AppData or a portable configuration can be on a different volume.
            // Publish only a complete copy; other sessions may be opening the same profile.
            let staged = tempfile::tempdir_in(parent)?;
            copy_profile(previous, staged.path())?;
            match fs::rename(staged.path(), current) {
                Ok(()) => Ok(()),
                Err(_) if current.is_dir() => Ok(()),
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "Cannot move browser profile from {} to {}",
                previous.display(),
                current.display()
            )
        }),
    }
}

#[cfg(windows)]
fn copy_profile(source: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::fs::MetadataExt;

    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let to = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(entry.path())?;
        // Never traverse links or Windows junctions in the cached profile.
        ensure!(
            metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "Cannot copy a linked browser profile entry"
        );
        if metadata.is_dir() {
            fs::create_dir(&to)?;
            copy_profile(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

pub(crate) fn migrate_config(target: &Path) -> Result<()> {
    let legacy = legacy_project()?;
    migrate_config_from(legacy.config_dir(), target)
}

fn migrate_config_from(source: &Path, target: &Path) -> Result<()> {
    let config = target.join("config.json");
    if config.try_exists()? || source == target || !source.try_exists()? {
        return Ok(());
    }
    fs::create_dir_all(target)?;
    let lock = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(target.join(".directory-migration.lock"))?;
    lock.lock()?;
    if !config.try_exists()? {
        move_contents(source, target)?;
        remove_empty_parents(source.parent(), target.parent());
    }
    Ok(())
}

// Preflight the entire tree before moving anything. Conflicting files are never
// overwritten; an interrupted move can be resumed from the remaining entries.
fn move_contents(source: &Path, target: &Path) -> Result<()> {
    if source == target || !source.try_exists()? {
        return Ok(());
    }
    let mut moves = Vec::new();
    plan_moves(source, target, &mut moves)?;
    create_directories(source, target)?;
    // Publish the active configuration last so an interrupted migration can resume.
    let config = target.join("config.json");
    moves.sort_by_key(|(_, destination)| destination == &config);
    for (source, target) in moves {
        fs::create_dir_all(target.parent().context("Missing destination directory")?)?;
        fs::rename(&source, &target).with_context(|| {
            format!(
                "Cannot move {} to {}; close older Arcnix sessions and retry",
                source.display(),
                target.display()
            )
        })?;
    }
    remove_empty_tree(source)?;
    Ok(())
}

fn plan_moves(source: &Path, target: &Path, moves: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    ensure!(
        !fs::symlink_metadata(source)?.file_type().is_symlink(),
        "Cannot migrate a linked application directory: {}",
        source.display()
    );
    if target.try_exists()? {
        let metadata = fs::symlink_metadata(target)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Application directory is unavailable: {}",
            target.display()
        );
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source = entry.path();
        let target = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            plan_moves(&source, &target, moves)?;
        } else {
            ensure!(
                !target.try_exists()?,
                "Both old and new Arcnix data exist at {}; resolve the conflict before migrating",
                target.display()
            );
            moves.push((source, target));
        }
    }
    Ok(())
}

fn create_directories(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            create_directories(&entry.path(), &target.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn remove_empty_tree(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            remove_empty_tree(&entry.path())?;
        }
    }
    fs::remove_dir(path)?;
    Ok(())
}

fn remove_empty_parents(mut path: Option<&Path>, stop: Option<&Path>) {
    while let Some(current) = path {
        if Some(current) == stop || fs::remove_dir(current).is_err() {
            break;
        }
        path = current.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_moves_all_data_into_the_product_directory_and_can_resume() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("Arcnix");
        let source = target.join("Arcnix/config");
        fs::create_dir_all(source.join("browser-data/profile")).unwrap();
        fs::create_dir_all(source.join("logs")).unwrap();
        fs::write(source.join("config.json"), b"saved config").unwrap();
        fs::write(source.join("browser-data/profile/cookies"), b"cookies").unwrap();
        fs::create_dir_all(target.join("browser-data/profile")).unwrap();
        fs::write(target.join("browser-data/profile/already-moved"), b"kept").unwrap();
        migrate_config_from(&source, &target).unwrap();
        migrate_config_from(&source, &target).unwrap();
        assert_eq!(
            fs::read(target.join("config.json")).unwrap(),
            b"saved config"
        );
        assert_eq!(
            fs::read(target.join("browser-data/profile/cookies")).unwrap(),
            b"cookies"
        );
        assert_eq!(
            fs::read(target.join("browser-data/profile/already-moved")).unwrap(),
            b"kept"
        );
        assert!(!source.exists());
        assert!(target.join("logs").is_dir());
    }

    #[test]
    fn existing_configuration_wins_without_touching_the_previous_tree() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("old");
        let target = temp.path().join("new");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(source.join("config.json"), b"old").unwrap();
        fs::write(source.join("other.json"), b"other").unwrap();
        fs::write(target.join("config.json"), b"new").unwrap();
        migrate_config_from(&source, &target).unwrap();
        assert_eq!(fs::read(source.join("config.json")).unwrap(), b"old");
        assert_eq!(fs::read(target.join("config.json")).unwrap(), b"new");
        assert!(source.join("other.json").exists());
        assert!(!target.join("other.json").exists());
        assert!(!target.join(".directory-migration.lock").exists());
    }

    #[test]
    fn current_paths_have_only_the_product_name() {
        assert_eq!(
            project().unwrap().project_path(),
            Path::new(if cfg!(target_os = "linux") {
                "arcnix"
            } else {
                "Arcnix"
            })
        );
        #[cfg(windows)]
        {
            let base = BaseDirs::new().unwrap();
            assert_eq!(config_dir().unwrap(), base.config_dir().join("Arcnix"));
            assert_eq!(
                local_data_dir().unwrap(),
                base.data_local_dir().join("Arcnix")
            );
            assert_eq!(
                crate::store::ConfigStore::default_location()
                    .unwrap()
                    .webview_data_root()
                    .unwrap(),
                base.data_local_dir().join("Arcnix/WebView2")
            );
        }
    }

    #[test]
    fn interrupted_migration_preserves_conflicting_auxiliary_data() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("old");
        let target = temp.path().join("new");
        fs::create_dir_all(&source)?;
        fs::create_dir_all(&target)?;
        fs::write(source.join("config.json"), b"active")?;
        fs::write(source.join("config.tasks.json"), b"old tasks")?;
        fs::write(target.join("config.tasks.json"), b"new tasks")?;
        assert!(migrate_config_from(&source, &target).is_err());
        assert!(!target.join("config.json").exists());
        assert_eq!(fs::read(source.join("config.json"))?, b"active");
        assert_eq!(fs::read(source.join("config.tasks.json"))?, b"old tasks");
        assert_eq!(fs::read(target.join("config.tasks.json"))?, b"new tasks");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn ssh_profile_migrates_once_and_keeps_a_recreated_old_profile() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let old = temp.path().join("browser-data/server");
        let new = temp.path().join("WebView2/browser/server");
        fs::create_dir_all(&old)?;
        fs::write(old.join("Local State"), b"active")?;
        prepare_webview_profile(&old, &new)?;
        assert_eq!(fs::read(new.join("Local State"))?, b"active");
        fs::create_dir_all(&old)?;
        fs::write(old.join("Local State"), b"older client")?;
        prepare_webview_profile(&old, &new)?;
        assert_eq!(fs::read(new.join("Local State"))?, b"active");
        assert_eq!(fs::read(old.join("Local State"))?, b"older client");
        Ok(())
    }
}
