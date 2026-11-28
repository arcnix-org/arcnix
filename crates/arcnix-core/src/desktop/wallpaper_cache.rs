//! Shared content-addressed wallpapers and previews, with per-source ownership.
use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use image::{ImageFormat, ImageReader};
use sha2::{Digest, Sha256};

const LIMIT: usize = 8 * 1024 * 1024;
pub const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];

#[derive(Clone, Debug)]
pub struct Request {
    pub root: PathBuf,
    pub scope: String,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct Cached {
    pub path: String,
    pub image: String,
    pub thumbnail: String,
}

pub fn supports(path: &str) -> bool {
    crate::file_associations::extension(path).is_some_and(|ext| EXTENSIONS.contains(&ext.as_str()))
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn checked_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// Only these names can be served or collected. Source paths never become local filenames.
fn asset_name(name: &str) -> Option<(&str, &'static str)> {
    let (stem, extension) = name.rsplit_once('.')?;
    let digest = stem.strip_suffix("-thumb").unwrap_or(stem);
    if !checked_hash(digest) || (stem != digest && extension != "png") {
        return None;
    }
    let mime = match extension {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => return None,
    };
    Some((digest, mime))
}

type Sources = BTreeMap<String, BTreeMap<String, String>>;

struct Cache<'a> {
    root: &'a Path,
    _lock: File,
}

impl<'a> Cache<'a> {
    fn lock(root: &'a Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("sources.lock"))?;
        lock.lock()?;
        Ok(Self { root, _lock: lock })
    }

    fn set_source(&self, request: &Request, digest: Option<&str>) -> Result<()> {
        let index = self.root.join("sources.json");
        let mut sources: Sources = match File::open(&index) {
            Ok(file) => serde_json::from_reader(file).context("Invalid wallpaper source index")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
        let current = sources
            .get(&request.scope)
            .and_then(|paths| paths.get(&request.path));
        if current.map(String::as_str) != digest {
            if let Some(digest) = digest {
                sources
                    .entry(request.scope.clone())
                    .or_default()
                    .insert(request.path.clone(), digest.into());
            } else if let Some(paths) = sources.get_mut(&request.scope) {
                paths.remove(&request.path);
                if paths.is_empty() {
                    sources.remove(&request.scope);
                }
            }
            publish(&index, &serde_json::to_vec(&sources)?)?;
        } else if digest.is_some() {
            return Ok(());
        }
        // Commit references before collection. A later source change also collects objects
        // left between a file publication and an index update by an interrupted process.
        let retained: HashSet<&str> = sources
            .values()
            .flat_map(|paths| paths.values().map(String::as_str))
            .collect();
        for entry in std::fs::read_dir(self.root)? {
            let entry = entry?;
            if let Some((digest, _)) = entry.file_name().to_str().and_then(asset_name)
                && !retained.contains(digest)
                && !entry.file_type()?.is_dir()
            {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }
}

impl Request {
    /// Forget this origin. Other origins can continue using the same local files.
    pub fn remove_cached(&self) -> Result<()> {
        if self.root.try_exists()? {
            Cache::lock(&self.root)?.set_source(self, None)?;
        }
        Ok(())
    }

    pub fn remote(&self, session: &crate::remote::RemoteSession) -> Result<Cached> {
        use std::cell::RefCell;
        crate::desktop_customization::validate_image_path(&self.path)?;
        // Hash on the server, without transferring the image when its content is unchanged.
        // O_NOFOLLOW/O_NONBLOCK reject symlinks and special files before reading.
        let script = "import os,stat,sys,hashlib\nf=os.open(sys.argv[1],os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)\nwith os.fdopen(f,'rb') as s:\n m=os.fstat(s.fileno());assert stat.S_ISREG(m.st_mode) and 0<m.st_size<=8388608\n b=s.read(8388609);assert len(b)==m.st_size\n print(hashlib.sha256(b).hexdigest())";
        let downloaded = RefCell::new(None);
        self.load(
            || {
                if let Ok(output) = session.exec(&format!(
                    "# arcnix:wallpaper:sha256\npython3 -c {} {}",
                    crate::remote_tasks::quote(script),
                    crate::remote_tasks::quote(&self.path)
                )) && output.exit_code == 0
                    && checked_hash(output.stdout.trim())
                {
                    return Ok(output.stdout.trim().to_owned());
                }
                // A server only needs SSH/SFTP. Without the optional hash helper,
                // validate a bounded download and reuse it if the cache needs rebuilding.
                let bytes = session.read_bounded_file(&self.path, LIMIT)?;
                let digest = hash(&bytes);
                *downloaded.borrow_mut() = Some(bytes);
                Ok(digest)
            },
            || {
                let bytes = downloaded.borrow_mut().take();
                bytes.map_or_else(|| session.read_bounded_file(&self.path, LIMIT), Ok)
            },
        )
    }

    pub fn local(&self, target: &crate::local_desktop::Target) -> Result<Cached> {
        self.load(
            || {
                let (file, stamp) = target.open_binary(&self.path)?;
                ensure!(
                    stamp.size > 0 && stamp.size <= LIMIT as u64,
                    "Images must be nonempty and at most 8 MiB"
                );
                let mut digest = Sha256::new();
                let count = std::io::copy(&mut file.take(LIMIT as u64 + 1), &mut digest)?;
                ensure!(count == stamp.size, "Wallpaper changed while reading");
                Ok(format!("{:x}", digest.finalize()))
            },
            || target.read_bounded_file(&self.path, LIMIT),
        )
    }

    fn load(
        &self,
        fingerprint: impl FnOnce() -> Result<String>,
        read: impl FnOnce() -> Result<Vec<u8>>,
    ) -> Result<Cached> {
        ensure!(
            supports(&self.path),
            "Supported wallpaper formats: PNG, JPEG, WebP, GIF and BMP"
        );
        let digest = fingerprint()?;
        ensure!(checked_hash(&digest), "Invalid wallpaper content hash");
        let extension = crate::file_associations::extension(&self.path).unwrap();
        let extension = if extension == "jpeg" {
            "jpg"
        } else {
            &extension
        };
        let image_name = format!("{digest}.{extension}");
        let thumbnail_name = format!("{digest}-thumb.png");
        let image_path = self.root.join(&image_name);
        let thumbnail_path = self.root.join(&thumbnail_name);
        let result = Cached {
            path: self.path.clone(),
            image: format!("/wallpaper-cache/{image_name}"),
            thumbnail: format!("/wallpaper-cache/{thumbnail_name}"),
        };
        let cached = {
            let cache = Cache::lock(&self.root)?;
            let cached = read_bounded(&image_path)
                .ok()
                .filter(|bytes| hash(bytes) == digest);
            if cached.is_some() && valid_thumbnail(&thumbnail_path) {
                cache.set_source(self, Some(&digest))?;
                return Ok(result);
            }
            cached
        };
        // Leave the shared lock free during network I/O and decoding.
        let bytes = match cached {
            Some(bytes) => bytes,
            None => read()?,
        };
        ensure!(
            !bytes.is_empty() && bytes.len() <= LIMIT,
            "Images must be nonempty and at most 8 MiB"
        );
        ensure!(
            hash(&bytes) == digest,
            "Wallpaper changed while downloading. Try again."
        );
        let preview = thumbnail(&bytes, extension)?;
        let cache = Cache::lock(&self.root)?;
        publish(&image_path, &bytes)?;
        publish(&thumbnail_path, &preview)?;
        cache.set_source(self, Some(&digest))?;
        Ok(result)
    }
}

fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    // Canonicalizing gives Windows its extended-length path before tempfile's native rename.
    let parent = path
        .parent()
        .context("Missing cache directory")?
        .canonicalize()?;
    let destination = parent.join(path.file_name().context("Missing cache filename")?);
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary
        .persist(destination)
        .map_err(|error| error.error)?;
    Ok(())
}

fn thumbnail(bytes: &[u8], extension: &str) -> Result<Vec<u8>> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    ensure!(
        reader.format() == ImageFormat::from_extension(extension),
        "Image content does not match its file format"
    );
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().context("Could not decode this wallpaper")?;
    let image = image.resize_to_fill(320, 180, image::imageops::FilterType::Triangle);
    let mut output = Cursor::new(Vec::new());
    image.write_to(&mut output, ImageFormat::Png)?;
    Ok(output.into_inner())
}

fn valid_thumbnail(path: &Path) -> bool {
    let Ok(bytes) = read_bounded(path) else {
        return false;
    };
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(320);
    limits.max_image_height = Some(180);
    limits.max_alloc = Some(1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .is_ok_and(|image| image.width() == 320 && image.height() == 180)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "Invalid wallpaper cache file"
    );
    let file = std::fs::File::open(path)?;
    ensure!(
        file.metadata()?.len() <= LIMIT as u64,
        "Wallpaper cache file is too large"
    );
    let mut bytes = Vec::new();
    file.take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= LIMIT, "Wallpaper cache file is too large");
    Ok(bytes)
}

/// Only generated cache URLs are readable; no arbitrary file paths enter the asset provider.
pub fn asset(root: &Path, url: &str) -> Option<(&'static str, Vec<u8>)> {
    let name = url.strip_prefix("/wallpaper-cache/")?;
    let (_, mime) = asset_name(name)?;
    Some((mime, read_bounded(&root.join(name)).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(color: [u8; 3]) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(640, 360, image::Rgb(color));
        let mut output = Cursor::new(Vec::new());
        image.write_to(&mut output, ImageFormat::Bmp).unwrap();
        output.into_inner()
    }

    #[test]
    fn reuses_content_and_refreshes_changed_bytes_even_at_the_same_size() -> Result<()> {
        let root = tempfile::tempdir()?;
        let request = Request {
            root: root.path().into(),
            scope: "server-a".into(),
            path: "/home/user/picture.bmp".into(),
        };
        let first = picture([80, 20, 10]);
        let changed = picture([20, 80, 10]);
        assert_eq!(first.len(), changed.len());
        let original = request.load(|| Ok(hash(&first)), || Ok(first.clone()))?;
        let same = request.load(
            || Ok(hash(&first)),
            || panic!("unchanged content must not download"),
        )?;
        assert_eq!(same.image, original.image);
        let (_, small) = asset(root.path(), &original.thumbnail).unwrap();
        let decoded = image::load_from_memory(&small)?;
        assert_eq!((decoded.width(), decoded.height()), (320, 180));
        let other = Request {
            scope: "server-b".into(),
            ..request.clone()
        };
        let shared = other.load(
            || Ok(hash(&first)),
            || panic!("reuse the original from server A"),
        )?;
        assert_eq!(shared.image, original.image);
        let replacement = request.load(|| Ok(hash(&changed)), || Ok(changed.clone()))?;
        assert_ne!(original.image, replacement.image);
        assert_ne!(original.thumbnail, replacement.thumbnail);
        assert_eq!(asset(root.path(), &replacement.image).unwrap().1, changed);

        assert_eq!(asset(root.path(), &original.image).unwrap().1, first);
        other.remove_cached()?;
        assert!(asset(root.path(), &original.image).is_none());
        assert!(asset(root.path(), &original.thumbnail).is_none());
        assert!(asset(root.path(), &replacement.thumbnail).is_some());
        assert!(asset(root.path(), "/wallpaper-cache/../../config.json").is_none());
        assert!(asset(root.path(), "/wallpaper-cache/sources.json").is_none());
        Ok(())
    }

    #[test]
    fn repairs_corrupt_cache_and_rejects_invalid_or_racing_images() -> Result<()> {
        let root = tempfile::tempdir()?;
        let request = Request {
            root: root.path().into(),
            scope: "server".into(),
            path: "/picture.bmp".into(),
        };
        let bytes = picture([3, 4, 5]);
        let cached = request.load(|| Ok(hash(&bytes)), || Ok(bytes.clone()))?;
        let thumbnail_path = root
            .path()
            .join(cached.thumbnail.strip_prefix("/wallpaper-cache/").unwrap());
        std::fs::write(thumbnail_path, b"broken")?;
        request.load(
            || Ok(hash(&bytes)),
            || panic!("repair the thumbnail from local cached content"),
        )?;
        let image_path = root
            .path()
            .join(cached.image.strip_prefix("/wallpaper-cache/").unwrap());
        std::fs::write(image_path, b"corrupt")?;
        request.load(|| Ok(hash(&bytes)), || Ok(bytes.clone()))?;
        assert!(
            request
                .load(|| Ok(hash(b"different")), || Ok(bytes.clone()))
                .is_err()
        );
        assert!(
            request
                .load(|| Ok(hash(b"broken")), || Ok(b"broken".to_vec()))
                .is_err()
        );
        let disguised = Request {
            path: "/disguised.png".into(),
            ..request.clone()
        };
        assert!(
            disguised
                .load(|| Ok(hash(&bytes)), || Ok(bytes.clone()))
                .is_err()
        );
        assert_eq!(asset(root.path(), &cached.image).unwrap().1, bytes);
        let sources: Sources =
            serde_json::from_reader(File::open(root.path().join("sources.json"))?)?;
        assert_eq!(
            sources[&request.scope],
            BTreeMap::from([(request.path.clone(), hash(&bytes))])
        );
        let svg = Request {
            path: "/external.svg".into(),
            ..request
        };
        assert!(
            svg.load(
                || panic!("unsupported format must not be read"),
                || unreachable!()
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn custom_wallpapers_round_trip_and_survive_theme_reset() -> Result<()> {
        use crate::desktop_customization::{Appearance, Wallpaper};
        let path = "/home/user/picture.png".to_owned();
        let mut appearance = Appearance {
            wallpaper: Some(Wallpaper::Remote(path.clone())),
            wallpapers: vec![path.clone()],
            ..Default::default()
        };
        appearance.validate()?;
        let saved: Appearance = serde_json::from_slice(&serde_json::to_vec(&appearance)?)?;
        assert_eq!(saved, appearance);
        let prefs = crate::server_apps::DesktopPreferences {
            appearance: appearance.clone(),
            ..Default::default()
        };
        assert_eq!(
            crate::desktop_storage::decode(&crate::desktop_storage::encode(&prefs)?)?,
            prefs
        );
        appearance.apply_theme_preset(Default::default());
        assert_eq!(appearance.wallpapers, [path]);
        assert!(appearance.wallpaper.is_none());
        Ok(())
    }

    #[test]
    fn supports_deep_cache_paths() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("long-workspace-directory-".repeat(6));
        let request = Request {
            root: root.clone(),
            scope: "server".into(),
            path: "/picture.bmp".into(),
        };
        let bytes = picture([3, 4, 5]);
        let cached = request.load(|| Ok(hash(&bytes)), || Ok(bytes.clone()))?;
        assert_eq!(asset(&root, &cached.image).unwrap().1, bytes);
        assert!(asset(&root, &cached.thumbnail).is_some());
        Ok(())
    }

    #[test]
    fn identical_sources_share_assets_until_the_last_reference_is_removed() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("cache");
        let original = temporary.path().join("source.bmp");
        let bytes = picture([40, 50, 60]);
        std::fs::write(&original, &bytes)?;
        let selected = Request {
            root,
            scope: "server-a".into(),
            path: "/source.bmp".into(),
        };
        let other_server = Request {
            scope: "server-b".into(),
            ..selected.clone()
        };
        let other_image = Request {
            path: "/other.bmp".into(),
            ..selected.clone()
        };
        let removed = selected.load(|| Ok(hash(&bytes)), || Ok(bytes.clone()))?;
        let reuse =
            |request: &Request| request.load(|| Ok(hash(&bytes)), || panic!("duplicate download"));
        let retained_server = reuse(&other_server)?;
        let retained_image = reuse(&other_image)?;
        assert_eq!(removed.image, retained_server.image);
        assert_eq!(removed.thumbnail, retained_image.thumbnail);
        assert_eq!(
            removed.image,
            format!("/wallpaper-cache/{}.bmp", hash(&bytes))
        );
        selected.remove_cached()?;
        selected.remove_cached()?;
        assert!(asset(&selected.root, &retained_server.image).is_some());
        other_server.remove_cached()?;
        assert!(asset(&selected.root, &retained_image.thumbnail).is_some());
        other_image.remove_cached()?;
        assert!(asset(&selected.root, &removed.image).is_none());
        assert!(asset(&selected.root, &removed.thumbnail).is_none());
        assert_eq!(std::fs::read(original)?, bytes);
        Ok(())
    }

    #[test]
    fn jpeg_aliases_and_concurrent_imports_keep_both_references() -> Result<()> {
        let root = tempfile::tempdir()?;
        let first = Request {
            root: root.path().into(),
            scope: "server".into(),
            path: "/one.jpeg".into(),
        };
        let second = Request {
            path: "/two.JPG".into(),
            ..first.clone()
        };
        let mut output = Cursor::new(Vec::new());
        image::RgbImage::from_pixel(16, 9, image::Rgb([1u8, 2, 3]))
            .write_to(&mut output, ImageFormat::Jpeg)?;
        let bytes = output.into_inner();
        let barrier = std::sync::Barrier::new(2);
        let load = |request: &Request| {
            barrier.wait();
            request.load(|| Ok(hash(&bytes)), || Ok(bytes.clone()))
        };
        let (a, b) = std::thread::scope(|scope| {
            let a = scope.spawn(|| load(&first));
            let b = load(&second);
            (a.join().unwrap(), b)
        });
        let (a, b) = (a?, b?);
        assert_eq!(a.image, b.image);
        assert!(a.image.ends_with(".jpg"));
        assert_eq!(a.thumbnail, b.thumbnail);
        second.remove_cached()?;
        assert!(asset(root.path(), &a.image).is_some());
        first.remove_cached()?;
        assert!(asset(root.path(), &b.thumbnail).is_none());
        Ok(())
    }
}
