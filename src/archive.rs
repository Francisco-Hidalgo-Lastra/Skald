//! Archive extraction (zip, 7z, rar) or plain directory import.

use anyhow::{Context, Result, bail};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    SevenZip,
    Rar,
    Directory,
}

pub fn sniff(path: &Path) -> Result<ArchiveKind> {
    if path.is_dir() {
        return Ok(ArchiveKind::Directory);
    }
    let mut magic = [0u8; 8];
    let n = File::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .read(&mut magic)?;
    let m = &magic[..n];
    if m.starts_with(b"PK\x03\x04") || m.starts_with(b"PK\x05\x06") {
        return Ok(ArchiveKind::Zip);
    }
    if m.starts_with(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        return Ok(ArchiveKind::SevenZip);
    }
    if m.starts_with(b"Rar!\x1A\x07") {
        return Ok(ArchiveKind::Rar);
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "zip" => Ok(ArchiveKind::Zip),
        "7z" => Ok(ArchiveKind::SevenZip),
        "rar" => Ok(ArchiveKind::Rar),
        _ => bail!("{} is not a supported archive (zip, 7z, rar)", path.display()),
    }
}

/// Extract `archive` into the (empty) directory `dest`.
pub fn extract(archive: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    match sniff(archive)? {
        ArchiveKind::Directory => {
            crate::paths::copy_merge(archive, dest, Path::new(""))?;
        }
        ArchiveKind::Zip => {
            let mut zip = zip::ZipArchive::new(File::open(archive)?)?;
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i)?;
                // Normalise backslashes that Windows zippers sometimes emit.
                let rel = crate::paths::normalize_rel(&entry.name()?);
                if rel.as_os_str().is_empty() {
                    continue;
                }
                let out = dest.join(&rel);
                if entry.is_dir() {
                    fs::create_dir_all(&out)?;
                } else {
                    if let Some(p) = out.parent() {
                        fs::create_dir_all(p)?;
                    }
                    let mut f = File::create(&out)?;
                    std::io::copy(&mut entry, &mut f)?;
                }
            }
        }
        ArchiveKind::SevenZip => {
            sevenz_rust2::decompress_file(archive, dest)
                .map_err(|e| anyhow::anyhow!("7z extraction failed: {e}"))?;
        }
        ArchiveKind::Rar => {
            let status = Command::new("unrar")
                .arg("x")
                .arg("-o+")
                .arg("-y")
                .arg("-idq")
                .arg(archive)
                .arg(format!("{}/", dest.display()))
                .status();
            match status {
                Ok(s) if s.success() => {}
                Ok(s) => bail!("unrar exited with {s}"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    bail!("RAR archives need the `unrar` program installed")
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}
