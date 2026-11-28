//! Content-addressed, server-scoped local wallpaper files and small gallery previews.
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

impl Request {
    fn directory(&self) -> PathBuf {
        self.root
            .join(hash(self.scope.as_bytes()))
            .join(hash(self.path.as_bytes()))
    }

    /// Remove this source's local cached versions, never the source image or another server's
    /// files.
    pub fn remove_cached(&self) -> Result<()> {
        let directory = self.directory();
        let metadata = match std::fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            metadata.is_dir() && !metadata.is_symlink(),
            "Invalid wallpaper cache directory"
        );
        let root = self.root.canonicalize()?;
        let resolved = directory.canonicalize()?;
        ensure!(
            resolved
                == root
                    .join(hash(self.scope.as_bytes()))
                    .join(hash(self.path.as_bytes())),
            "Invalid wallpaper cache directory"
        );
        std::fs::remove_dir_all(resolved)?;
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
        let directory = self.directory();
        let image_name = format!("{digest}.{extension}");
        let thumbnail_name = format!("{digest}-thumb.png");
        let image_path = directory.join(&image_name);
        let thumbnail_path = directory.join(&thumbnail_name);
        let cached = read_bounded(&image_path)
            .ok()
            .filter(|bytes| hash(bytes) == digest);
        if cached.is_none() || !valid_thumbnail(&thumbnail_path) {
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
            let thumbnail = thumbnail(&bytes, &extension)?;
            std::fs::create_dir_all(&directory)?;
            // Each file is published only after validation. Content-specific URLs avoid stale
            // WebView images.
            publish(&image_path, &bytes)?;
            publish(&thumbnail_path, &thumbnail)?;
        }
        let prefix = format!(
            "/wallpaper-cache/{}/{}/",
            hash(self.scope.as_bytes()),
            hash(self.path.as_bytes())
        );
        Ok(Cached {
            path: self.path.clone(),
            image: format!("{prefix}{image_name}"),
            thumbnail: format!("{prefix}{thumbnail_name}"),
        })
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
    let Ok(mut reader) = ImageReader::open(path) else {
        return false;
    };
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
    let mut segments = url.strip_prefix("/wallpaper-cache/")?.split('/');
    let scope = segments.next()?;
    let source = segments.next()?;
    let name = segments.next()?;
    if segments.next().is_some() || !checked_hash(scope) || !checked_hash(source) {
        return None;
    }
    let (stem, extension) = name.rsplit_once('.')?;
    if !checked_hash(stem.strip_suffix("-thumb").unwrap_or(stem)) {
        return None;
    }
    let mime = match extension {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => return None,
    };
    let path = root.join(scope).join(source).join(name);
    if !path
        .canonicalize()
        .ok()?
        .starts_with(root.canonicalize().ok()?)
    {
        return None;
    }
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > LIMIT as u64 {
        return None;
    }
    Some((mime, read_bounded(&path).ok()?))
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
        let replacement = request.load(|| Ok(hash(&changed)), || Ok(changed.clone()))?;
        assert_ne!(original.image, replacement.image);
        assert_ne!(original.thumbnail, replacement.thumbnail);
        assert_eq!(asset(root.path(), &replacement.image).unwrap().1, changed);

        let other = Request {
            scope: "server-b".into(),
            ..request.clone()
        };
        let isolated = other.load(|| Ok(hash(&first)), || Ok(first.clone()))?;
        assert_ne!(isolated.image, original.image);
        assert!(asset(root.path(), "/wallpaper-cache/../../config.json").is_none());
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
    fn removal_is_scoped_to_one_server_and_source() -> Result<()> {
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
        let load = |request: &Request| request.load(|| Ok(hash(&bytes)), || Ok(bytes.clone()));
        let removed = load(&selected)?;
        let retained_server = load(&other_server)?;
        let retained_image = load(&other_image)?;
        selected.remove_cached()?;
        selected.remove_cached()?;
        assert!(asset(&selected.root, &removed.image).is_none());
        assert!(asset(&selected.root, &removed.thumbnail).is_none());
        assert!(asset(&selected.root, &retained_server.image).is_some());
        assert!(asset(&selected.root, &retained_image.thumbnail).is_some());
        assert_eq!(std::fs::read(original)?, bytes);
        Ok(())
    }
}
