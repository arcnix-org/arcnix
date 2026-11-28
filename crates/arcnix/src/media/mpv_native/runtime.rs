//! Windows release executables carry the same compressed runtime, including
//! single-file updates. Publish a complete cache directory in one rename.
use std::fs::{self, File};
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};

const DLL: &str = "libmpv-2.dll";

pub(super) fn library_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let sidecar = exe.with_file_name(DLL);
    if sidecar.is_file() {
        return Ok(sidecar);
    }
    let mut archive = zip::ZipArchive::new(File::open(exe)?).context(
        "Missing bundled libmpv runtime. For a development build, stage libmpv-2.dll beside Arcnix",
    )?;
    let member = archive.by_name(DLL)?;
    let cache_name = format!("libmpv-{:08x}-{}", member.crc32(), member.size());
    drop(member);
    let cache = arcnix_core::paths::media_cache_dir()?;
    let installed = cache.join(cache_name);
    let dll = installed.join(DLL);
    if dll.is_file() {
        return Ok(dll);
    }
    fs::create_dir_all(&cache)?;
    let staging = tempfile::tempdir_in(&cache)?;
    for (member, filename) in [
        (DLL, DLL),
        ("LICENSES/libmpv/LICENSE.LGPL", "LICENSE.LGPL"),
        ("LICENSES/libmpv/SOURCE.txt", "SOURCE.txt"),
    ] {
        let mut entry = archive.by_name(member)?;
        let mut output = File::create_new(staging.path().join(filename))?;
        let len = io::copy(&mut entry, &mut output)?;
        ensure!(len == entry.size(), "Incomplete libmpv runtime");
    }
    if let Err(error) = fs::rename(staging.path(), installed)
        && !dll.is_file()
    {
        return Err(error.into());
    }
    Ok(dll)
}
