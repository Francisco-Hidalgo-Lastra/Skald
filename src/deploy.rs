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
            if fs::symlink_metadata(&f.target).is_ok() {
                if Self::is_ours(f, m.method) {
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
                    fs::rename(b, &f.target).with_context(|| format!("restoring {}", f.target.display()))?;
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
                if fs::symlink_metadata(&target).is_ok() {
                    let b = self.backup_dir.join(if k.0 { "root" } else { "data" }).join(rel);
                    fs::create_dir_all(b.parent().unwrap())?;
                    fs::rename(&target, &b).with_context(|| format!("backing up {}", target.display()))?;
                    backup = Some(b);
                    report.backed_up += 1;
                }
                // Record before linking so a crash can still be purged.
                manifest.files.push(DeployedFile { target: target.clone(), source: src.clone(), backup });
                link(src, &target, method).with_context(|| format!("deploying {}", target.display()))?;
                report.deployed += 1;
            }
            Ok(())
        })();
        self.save_manifest(&manifest)?;
        result.map(|_| report)
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
        ModEntry { name: name.into(), enabled: true, installer: "Simple".into(), version: None, author: None, source: None, installed_at: 0 }
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
}
