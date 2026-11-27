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
    /// User-made group this mod is shown under (None = ungrouped). Display only: it doesn't
    /// change priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// Mods in priority order: later entries win file conflicts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModList {
    pub mods: Vec<ModEntry>,
    /// User-made groups, in display order.
    #[serde(default)]
    pub groups: Vec<String>,
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
                    group: None,
                });
            }
        }
        // Keep every group a mod refers to, even if the groups list lost it.
        let missing: Vec<String> = list.mods.iter().filter_map(|m| m.group.clone()).collect();
        for g in missing {
            if list.group_index(&g).is_none() {
                list.groups.push(g);
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

    pub fn group_index(&self, name: &str) -> Option<usize> {
        self.groups.iter().position(|g| g.eq_ignore_ascii_case(name))
    }

    /// Adds a group and returns its stored name, or None if the name is blank or taken.
    pub fn add_group(&mut self, name: &str) -> Option<String> {
        let name = name.trim();
        if name.is_empty() || self.group_index(name).is_some() {
            return None;
        }
        self.groups.push(name.to_string());
        Some(name.to_string())
    }

    /// Renames a group and the mods in it. False if the new name is blank or another group has it.
    pub fn rename_group(&mut self, old: &str, new: &str) -> bool {
        let new = new.trim();
        let Some(i) = self.group_index(old) else { return false };
        if new.is_empty() || self.group_index(new).is_some_and(|j| j != i) {
            return false;
        }
        let old = std::mem::replace(&mut self.groups[i], new.to_string());
        for m in self.mods.iter_mut().filter(|m| m.group.as_deref() == Some(old.as_str())) {
            m.group = Some(new.to_string());
        }
        true
    }

    /// Deletes a group; its mods become ungrouped (nothing is uninstalled).
    pub fn remove_group(&mut self, name: &str) {
        if let Some(i) = self.group_index(name) {
            let g = self.groups.remove(i);
            for m in self.mods.iter_mut().filter(|m| m.group.as_deref() == Some(g.as_str())) {
                m.group = None;
            }
        }
    }

    /// Indices of the mods in `group` (None = ungrouped), in priority order.
    pub fn in_group(&self, group: Option<&str>) -> Vec<usize> {
        (0..self.mods.len()).filter(|&i| self.mods[i].group.as_deref() == group).collect()
    }

    /// Bytes on disk of each mod's staging folder, keyed by mod name.
    pub fn sizes(&self) -> HashMap<String, u64> {
        self.mods
            .iter()
            .map(|m| {
                let size = walkdir::WalkDir::new(self.mod_dir(&m.name))
                    .into_iter()
                    .flatten()
                    .filter(|e| e.file_type().is_file())
                    .filter_map(|e| e.metadata().ok())
                    .map(|md| md.len())
                    .sum();
                (m.name.clone(), size)
            })
            .collect()
    }

    /// File conflicts between enabled mods, keyed by mod name. Mods without conflicts are absent.
    pub fn conflicts(&self) -> HashMap<String, Conflicts> {
        // Every enabled mod that ships each file, in priority order.
        let mut owners: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, m) in self.mods.iter().enumerate().filter(|(_, m)| m.enabled) {
            for sub in ["data", "root"] {
                let dir = self.mod_dir(&m.name).join(sub);
                for f in paths::list_files(&dir) {
                    owners.entry(format!("{sub}/{}", paths::key(&f))).or_default().push(i);
                }
            }
        }
        let mut pairs: HashMap<(usize, usize), usize> = HashMap::new();
        let mut result: HashMap<usize, Conflicts> = HashMap::new();
        for list in owners.values().filter(|l| l.len() > 1) {
            for (k, &lo) in list.iter().enumerate() {
                let c = result.entry(lo).or_default();
                if k > 0 {
                    c.overrides += 1;
                }
                if k + 1 < list.len() {
                    c.overridden += 1;
                }
                for &hi in &list[k + 1..] {
                    *pairs.entry((lo, hi)).or_default() += 1;
                }
            }
        }
        for ((lo, hi), n) in pairs {
            result.entry(hi).or_default().winning.push((self.mods[lo].name.clone(), n));
            result.entry(lo).or_default().losing.push((self.mods[hi].name.clone(), n));
        }
        for c in result.values_mut() {
            for v in [&mut c.winning, &mut c.losing] {
                v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            }
        }
        result.into_iter().map(|(i, c)| (self.mods[i].name.clone(), c)).collect()
    }
}

/// One mod's file conflicts with the other enabled mods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conflicts {
    /// Files this mod overrides from lower-priority mods.
    pub overrides: usize,
    /// Files of this mod that higher-priority mods override.
    pub overridden: usize,
    /// Lower-priority mods this mod overrides, with the number of shared files, most first.
    pub winning: Vec<(String, usize)>,
    /// Higher-priority mods that override this mod, with the number of shared files, most first.
    pub losing: Vec<(String, usize)>,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, group: Option<&str>) -> ModEntry {
        ModEntry { name: name.into(), enabled: true, installer: "Simple".into(), version: None, author: None, source: None, installed_at: 0, group: group.map(Into::into) }
    }

    #[test]
    fn groups_add_rename_remove() {
        let mut l = ModList { mods: vec![entry("A", None), entry("B", None), entry("C", None)], ..Default::default() };
        assert_eq!(l.add_group(" Visuals "), Some("Visuals".into()));
        assert_eq!(l.add_group("visuals"), None);
        assert_eq!(l.add_group("  "), None);
        l.add_group("Audio");
        l.mods[0].group = Some("Visuals".into());
        l.mods[2].group = Some("Visuals".into());
        assert_eq!(l.in_group(Some("Visuals")), [0, 2]);
        assert_eq!(l.in_group(None), [1]);

        assert!(!l.rename_group("Visuals", "AUDIO"));
        assert!(l.rename_group("Visuals", "visuals"));
        assert_eq!(l.groups, ["visuals", "Audio"]);
        assert_eq!(l.in_group(Some("visuals")), [0, 2]);

        l.remove_group("VISUALS");
        assert_eq!(l.groups, ["Audio"]);
        assert_eq!(l.in_group(None), [0, 1, 2]);
    }

    #[test]
    fn load_keeps_old_files_and_adopts_groups() {
        let t = tempfile::tempdir().unwrap();
        for n in ["A", "B"] {
            fs::create_dir_all(t.path().join("mods").join(n)).unwrap();
        }
        // An old modlist.json, from before groups, plus one mod naming a group the list lacks.
        fs::write(
            t.path().join("modlist.json"),
            r#"{"mods":[{"name":"A","enabled":true,"installer":"Simple"},{"name":"B","enabled":true,"installer":"Simple","group":"Extra"}]}"#,
        )
        .unwrap();
        let l = ModList::load(t.path()).unwrap();
        assert_eq!(l.mods[0].group, None);
        assert_eq!(l.groups, ["Extra"]);
    }

    #[test]
    fn sizes_sum_each_mod_folder() {
        let t = tempfile::tempdir().unwrap();
        let l = ModList { mods: vec![entry("A", None), entry("B", None)], base: t.path().into(), ..Default::default() };
        let a = t.path().join("mods/A");
        fs::create_dir_all(a.join("data/meshes")).unwrap();
        fs::create_dir_all(a.join("root")).unwrap();
        fs::write(a.join("data/a.esp"), [0u8; 100]).unwrap();
        fs::write(a.join("data/meshes/b.nif"), [0u8; 50]).unwrap();
        fs::write(a.join("root/c.dll"), [0u8; 7]).unwrap();
        let s = l.sizes();
        assert_eq!(s["A"], 157);
        assert_eq!(s["B"], 0);
    }

    #[test]
    fn conflicts_list_the_other_mods() {
        let t = tempfile::tempdir().unwrap();
        let mut l = ModList { mods: vec![entry("A", None), entry("B", None), entry("C", None), entry("D", None)], base: t.path().into(), ..Default::default() };
        let put = |m: &str, f: &str| {
            let p = t.path().join("mods").join(m).join("data").join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        };
        put("A", "x.esp");
        put("A", "y.esp");
        put("B", "x.esp");
        put("C", "X.ESP");
        put("C", "y.esp");
        put("D", "z.esp");
        let c = l.conflicts();
        assert!(!c.contains_key("D"));
        assert_eq!((c["A"].overrides, c["A"].overridden), (0, 2));
        assert_eq!(c["A"].losing, [("C".to_string(), 2), ("B".to_string(), 1)]);
        assert_eq!((c["B"].overrides, c["B"].overridden), (1, 1));
        assert_eq!(c["B"].winning, [("A".to_string(), 1)]);
        assert_eq!(c["B"].losing, [("C".to_string(), 1)]);
        assert_eq!((c["C"].overrides, c["C"].overridden), (2, 0));
        assert_eq!(c["C"].winning, [("A".to_string(), 2), ("B".to_string(), 1)]);

        l.mods[2].enabled = false;
        let c = l.conflicts();
        assert_eq!(c["A"].losing, [("B".to_string(), 1)]);
        assert!(!c.contains_key("C"));
    }
}
