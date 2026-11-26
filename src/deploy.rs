//! Deploys enabled mods into the game folder via links, and reverses it.
//!
//! Mods stay untouched in their staging folders. A manifest records every
//! file placed in the game folder; any pre-existing file that would be
//! replaced is moved to a backup folder and restored on purge.

use crate::config::DeployMethod;
use crate::mods::ModList;
use crate::paths::{self, key, resolve_ci_create};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub method: Option<DeployMethod>,
    pub files: Vec<DeployedFile>,
    pub created_dirs: Vec<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeployedFile {
    pub target: PathBuf,
    pub source: PathBuf,
    pub backup: Option<PathBuf>,
    /// What the placed file looked like right after deploying, so it can be
    /// recognised even after its source (staging folder) is gone.
    #[serde(default)]
    pub stamp: Option<Stamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub dev: u64,
    pub ino: u64,
    pub len: u64,
    pub mtime: i64,
}

impl Stamp {
    fn of(meta: &fs::Metadata) -> Self {
        Self { dev: meta.dev(), ino: meta.ino(), len: meta.len(), mtime: meta.mtime() }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub deployed: usize,
    pub backed_up: usize,
    pub warnings: Vec<String>,
}

pub struct Deployer {
    pub manifest_path: PathBuf,
    pub backup_dir: PathBuf,
}

impl Deployer {
    pub fn new(base: &Path) -> Self {
        Self { manifest_path: base.join("deployment.json"), backup_dir: base.join("backups") }
    }

    fn load_manifest(&self) -> Result<Manifest> {
        match fs::read(&self.manifest_path) {
            Ok(b) => serde_json::from_slice(&b).context("deployment.json is corrupt"),
            Err(_) => Ok(Manifest::default()),
        }
    }

    fn save_manifest(&self, m: &Manifest) -> Result<()> {
        if let Some(p) = self.manifest_path.parent() {
            fs::create_dir_all(p)?;
        }
        let tmp = self.manifest_path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(m)?)?;
        fs::rename(tmp, &self.manifest_path)?;
        Ok(())
    }

    pub fn is_deployed(&self) -> bool {
        self.load_manifest().map(|m| !m.files.is_empty()).unwrap_or(false)
    }

    /// Every file the current deployment placed in the game folder.
    pub fn placed(&self) -> Vec<PathBuf> {
        self.load_manifest().map(|m| m.files.into_iter().map(|f| f.target).collect()).unwrap_or_default()
    }

    /// Is `target` still the file we put there?
    fn is_ours(f: &DeployedFile, method: Option<DeployMethod>) -> bool {
        let Ok(meta) = fs::symlink_metadata(&f.target) else { return false };
        if meta.file_type().is_symlink() {
            return fs::read_link(&f.target).map(|l| l == f.source).unwrap_or(false);
        }
        if let Some(st) = f.stamp {
            let now = Stamp::of(&meta);
            // Hardlinks share mtime with the source, so only the identity counts.
            return if method == Some(DeployMethod::Copy) { now == st } else { now.dev == st.dev && now.ino == st.ino };
        }
        // Manifests written before stamps existed: compare with the source.
        match fs::metadata(&f.source) {
            Ok(src) if src.ino() == meta.ino() && src.dev() == meta.dev() => true,
            Ok(src) => method == Some(DeployMethod::Copy) && src.len() == meta.len(),
            Err(_) => method == Some(DeployMethod::Copy),
        }
    }

    /// Remove everything we deployed and restore backups.
    pub fn purge(&self) -> Result<Report> {
        let m = self.load_manifest()?;
        let mut report = Report::default();
        for f in m.files.iter().rev() {
            // A "backup" identical to what we deployed is a stale copy of the mod's own file
            // (e.g. left behind by an older Skald or a reinstall), not an original: restoring it
            // would keep the file in Data after the mod is disabled.
            let mut stale_backup = false;
            if fs::symlink_metadata(&f.target).is_ok() {
                if Self::is_ours(f, m.method) {
                    stale_backup = f.backup.as_ref().is_some_and(|b| same_content(b, &f.target));
                    fs::remove_file(&f.target)
                        .with_context(|| format!("removing {}", f.target.display()))?;
                } else {
                    let mut msg = format!("{} was changed outside Skald; left in place", f.target.display());
                    if let Some(b) = f.backup.as_ref().filter(|b| b.exists()) {
                        let mut keep = f.target.clone().into_os_string();
                        keep.push(".skald-original");
                        fs::rename(b, &keep)?;
                        msg.push_str(&format!(" (original kept as {})", Path::new(&keep).display()));
                    }
                    report.warnings.push(msg);
                    continue;
                }
            }
            if let Some(b) = &f.backup
                && b.exists() {
                    if stale_backup {
                        let _ = fs::remove_file(b);
                    } else {
                        fs::rename(b, &f.target).with_context(|| format!("restoring {}", f.target.display()))?;
                    }
                }
            report.deployed += 1;
        }
        for d in m.created_dirs.iter().rev() {
            let _ = fs::remove_dir(d); // only succeeds when empty
        }
        let _ = fs::remove_dir_all(&self.backup_dir);
        let _ = fs::remove_file(&self.manifest_path);
        Ok(report)
    }

    /// Purge, then link all enabled mods into `game_dir` (and its Data folder).
    pub fn deploy(&self, mods: &ModList, game_dir: &Path, data_dir: &Path, method: DeployMethod) -> Result<Report> {
        let mut report = self.purge()?;
        report.deployed = 0;
        fs::create_dir_all(data_dir)?;

        // Winner for every relative path (later mods override earlier ones).
        let mut winners: HashMap<(bool, String), (PathBuf, PathBuf)> = HashMap::new();
        let mut order: Vec<(bool, String)> = vec![];
        for m in mods.mods.iter().filter(|m| m.enabled) {
            for (is_root, sub) in [(false, "data"), (true, "root")] {
                let dir = mods.mod_dir(&m.name).join(sub);
                for rel in paths::list_files(&dir) {
                    let k = (is_root, key(&rel));
                    if winners.insert(k.clone(), (dir.join(&rel), rel)).is_none() {
                        order.push(k);
                    }
                }
            }
        }

        let mut manifest = Manifest { method: Some(method), ..Default::default() };
        let result = (|| -> Result<()> {
            for k in &order {
                let (src, rel) = &winners[k];
                let base = if k.0 { game_dir } else { data_dir };
                let target = resolve_ci_create(base, rel, &mut manifest.created_dirs)?;
                let mut backup = None;
                // An existing exact copy of the file we're about to place isn't worth keeping:
                // backing it up would bring it back on purge, even once its mod is disabled.
                if same_content(&target, src) {
                    fs::remove_file(&target).with_context(|| format!("replacing {}", target.display()))?;
                }
                if fs::symlink_metadata(&target).is_ok() {
                    let b = self.backup_dir.join(if k.0 { "root" } else { "data" }).join(rel);
                    fs::create_dir_all(b.parent().unwrap())?;
                    fs::rename(&target, &b).with_context(|| format!("backing up {}", target.display()))?;
                    backup = Some(b);
                    report.backed_up += 1;
                }
                // Record before linking so a crash can still be purged.
                manifest.files.push(DeployedFile { target: target.clone(), source: src.clone(), backup, stamp: None });
                link(src, &target, method).with_context(|| format!("deploying {}", target.display()))?;
                manifest.files.last_mut().unwrap().stamp = fs::symlink_metadata(&target)
                    .ok()
                    .filter(|m| !m.file_type().is_symlink())
                    .map(|m| Stamp::of(&m));
                report.deployed += 1;
            }
            Ok(())
        })();
        self.save_manifest(&manifest)?;
        result.map(|_| report)
    }
}

/// Are `a` and `b` both regular files with identical bytes?
fn same_content(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let (Ok(ma), Ok(mb)) = (fs::symlink_metadata(a), fs::symlink_metadata(b)) else { return false };
    if !ma.is_file() || !mb.is_file() || ma.len() != mb.len() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (fs::File::open(a), fs::File::open(b)) else { return false };
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let n = match fa.read(&mut ba) {
            Ok(0) => return true,
            Ok(n) => n,
            Err(_) => return false,
        };
        if fb.read_exact(&mut bb[..n]).is_err() || ba[..n] != bb[..n] {
            return false;
        }
    }
}

fn link(src: &Path, dst: &Path, method: DeployMethod) -> std::io::Result<()> {
    match method {
        DeployMethod::Hardlink => fs::hard_link(src, dst).or_else(|_| std::os::unix::fs::symlink(src, dst)),
        DeployMethod::Symlink => std::os::unix::fs::symlink(src, dst),
        DeployMethod::Copy => fs::copy(src, dst).map(|_| ()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::ModEntry;

    fn entry(name: &str) -> ModEntry {
        ModEntry { name: name.into(), enabled: true, installer: "Simple".into(), version: None, author: None, source: None, installed_at: 0, group: None }
    }

    #[test]
    fn deploy_and_purge_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().join("skald");
        let game = t.path().join("game");
        let data = game.join("Data");
        fs::create_dir_all(data.join("Textures")).unwrap();
        fs::write(data.join("Textures/vanilla.dds"), b"vanilla").unwrap();

        let mut list = ModList::load(&base).unwrap();
        for (name, content) in [("A", "a"), ("B", "b")] {
            let d = base.join("mods").join(name).join("data/textures");
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("Vanilla.dds"), content).unwrap();
            fs::write(d.join(format!("{name}.dds")), content).unwrap();
            list.mods.push(entry(name));
        }
        fs::create_dir_all(base.join("mods/B/root")).unwrap();
        fs::write(base.join("mods/B/root/loader.dll"), b"dll").unwrap();

        let dep = Deployer::new(&base);
        for method in [DeployMethod::Hardlink, DeployMethod::Symlink, DeployMethod::Copy] {
            let r = dep.deploy(&list, &game, &data, method).unwrap();
            assert_eq!(r.deployed, 4);
            // Merged into the existing "Textures" (case-insensitive), B wins.
            assert_eq!(fs::read_to_string(data.join("Textures/vanilla.dds")).unwrap(), "b");
            assert!(!data.join("textures").exists());
            assert!(game.join("loader.dll").exists());

            dep.purge().unwrap();
            assert_eq!(fs::read_to_string(data.join("Textures/vanilla.dds")).unwrap(), "vanilla");
            assert!(!data.join("Textures/A.dds").exists());
            assert!(!game.join("loader.dll").exists());
        }
    }

    #[test]
    fn stale_copies_are_not_restored() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().join("skald");
        let game = t.path().join("game");
        let data = game.join("Data");
        fs::create_dir_all(&data).unwrap();
        let d = base.join("mods/A/data");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("A.esp"), b"mod").unwrap();
        fs::write(d.join("B.esp"), b"mod b").unwrap();
        // Leftover copies in Data: one identical to the mod's file, one genuinely different.
        fs::write(data.join("A.esp"), b"mod").unwrap();
        fs::write(data.join("B.esp"), b"original").unwrap();
        let mut list = ModList::load(&base).unwrap(); // picks up the "A" folder
        assert_eq!(list.mods.len(), 1);
        list.mods[0].enabled = true;

        let dep = Deployer::new(&base);
        let r = dep.deploy(&list, &game, &data, DeployMethod::Hardlink).unwrap();
        assert_eq!(r.backed_up, 1);
        dep.purge().unwrap();
        assert!(!data.join("A.esp").exists());
        assert_eq!(fs::read_to_string(data.join("B.esp")).unwrap(), "original");

        // Same, for a backup taken by an older version (identical copy already in backups).
        let dep2 = Deployer::new(&base);
        dep2.deploy(&list, &game, &data, DeployMethod::Hardlink).unwrap();
        fs::create_dir_all(dep2.backup_dir.join("data")).unwrap();
        fs::write(dep2.backup_dir.join("data/A.esp"), b"mod").unwrap();
        let mut m = dep2.load_manifest().unwrap();
        m.files.iter_mut().find(|f| f.target.ends_with("A.esp")).unwrap().backup = Some(dep2.backup_dir.join("data/A.esp"));
        dep2.save_manifest(&m).unwrap();
        list.mods[0].enabled = false;
        dep2.deploy(&list, &game, &data, DeployMethod::Hardlink).unwrap();
        assert!(!data.join("A.esp").exists());
        assert_eq!(fs::read_to_string(data.join("B.esp")).unwrap(), "original");
    }

    #[test]
    fn purge_after_mod_folder_removed() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().join("skald");
        let game = t.path().join("game");
        let data = game.join("Data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("shared.esp"), b"vanilla").unwrap();

        let dep = Deployer::new(&base);
        for method in [DeployMethod::Hardlink, DeployMethod::Symlink, DeployMethod::Copy] {
            let mut list = ModList::load(&base).unwrap();
            let d = base.join("mods/A/data/meshes");
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("a.nif"), b"a").unwrap();
            fs::write(base.join("mods/A/data/shared.esp"), b"mod").unwrap();
            list.mods.push(entry("A"));

            dep.deploy(&list, &game, &data, method).unwrap();
            list.remove(0).unwrap(); // staging folder deleted while deployed
            let r = dep.purge().unwrap();
            assert!(r.warnings.is_empty(), "{method:?}: {:?}", r.warnings);
            assert!(!data.join("meshes").exists(), "{method:?}");
            assert_eq!(fs::read_to_string(data.join("shared.esp")).unwrap(), "vanilla");
        }
    }
}
