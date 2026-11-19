//! The installed mod list: staging folders plus ordering/enabled state.

use crate::paths;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModEntry {
    pub name: String,
    pub enabled: bool,
    pub installer: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub installed_at: u64,
}

/// Mods in priority order: later entries win file conflicts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModList {
    pub mods: Vec<ModEntry>,
    #[serde(skip)]
    pub base: PathBuf,
}

impl ModList {
    pub fn load(base: &Path) -> Result<Self> {
        fs::create_dir_all(base.join("mods"))?;
        let path = base.join("modlist.json");
        let mut list: ModList = match fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).context("modlist.json is corrupt")?,
            Err(_) => ModList::default(),
        };
        list.base = base.to_path_buf();
        // Drop entries whose folder vanished; adopt folders we don't know.
        list.mods.retain(|m| list.base.join("mods").join(&m.name).is_dir());
        if let Ok(rd) = fs::read_dir(base.join("mods")) {
            let mut extra: Vec<String> = rd
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !list.mods.iter().any(|m| &m.name == n))
                .collect();
            extra.sort();
            for name in extra {
                list.mods.push(ModEntry {
                    name,
                    enabled: false,
                    installer: "Unknown".into(),
                    version: None,
                    author: None,
                    source: None,
                    installed_at: 0,
                });
            }
        }
        Ok(list)
    }

    pub fn save(&self) -> Result<()> {
        let tmp = self.base.join("modlist.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(tmp, self.base.join("modlist.json"))?;
        Ok(())
    }

    pub fn mods_dir(&self) -> PathBuf {
        self.base.join("mods")
    }

    pub fn mod_dir(&self, name: &str) -> PathBuf {
        self.mods_dir().join(name)
    }

    pub fn unique_name(&self, wanted: &str) -> String {
        let base = paths::sanitize_name(wanted);
        let mut name = base.clone();
        let mut i = 2;
        while self.mods.iter().any(|m| m.name.eq_ignore_ascii_case(&name)) || self.mod_dir(&name).exists() {
            name = format!("{base} ({i})");
            i += 1;
        }
        name
    }

    pub fn remove(&mut self, idx: usize) -> Result<()> {
        let m = self.mods.remove(idx);
        let dir = self.mod_dir(&m.name);
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        self.save()
    }

    pub fn rename(&mut self, idx: usize, new: &str) -> Result<()> {
        let new = self.unique_name(new);
        fs::rename(self.mod_dir(&self.mods[idx].name), self.mod_dir(&new))?;
        self.mods[idx].name = new;
        self.save()
    }

    /// For each enabled mod: (files it overrides from lower mods, files
    /// overridden by higher mods).
    pub fn conflicts(&self) -> HashMap<String, (usize, usize)> {
        let mut owner: HashMap<String, usize> = HashMap::new();
        let mut result: HashMap<String, (usize, usize)> = HashMap::new();
        for (i, m) in self.mods.iter().enumerate().filter(|(_, m)| m.enabled) {
            for sub in ["data", "root"] {
                let dir = self.mod_dir(&m.name).join(sub);
                for f in paths::list_files(&dir) {
                    let k = format!("{sub}/{}", paths::key(&f));
                    if let Some(prev) = owner.insert(k, i) {
                        result.entry(m.name.clone()).or_default().0 += 1;
                        result.entry(self.mods[prev].name.clone()).or_default().1 += 1;
                    }
                }
            }
        }
        result
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
