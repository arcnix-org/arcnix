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

pub(crate) fn migrate_config(target: &Path) -> Result<()> {
    let legacy = legacy_project()?;
    if legacy.config_dir() == target || !legacy.config_dir().try_exists()? {
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
    move_contents(legacy.config_dir(), target)?;
    remove_empty_parents(legacy.config_dir().parent(), target.parent());
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
        move_contents(&source, &target).unwrap();
        move_contents(&source, &target).unwrap();
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
    fn migration_refuses_conflicts_before_moving_any_files() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("old");
        let target = temp.path().join("new");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(source.join("config.json"), b"old").unwrap();
        fs::write(source.join("other.json"), b"other").unwrap();
        fs::write(target.join("config.json"), b"new").unwrap();
        assert!(move_contents(&source, &target).is_err());
        assert_eq!(fs::read(source.join("config.json")).unwrap(), b"old");
        assert_eq!(fs::read(target.join("config.json")).unwrap(), b"new");
        assert!(source.join("other.json").exists());
        assert!(!target.join("other.json").exists());
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
        }
    }
}
