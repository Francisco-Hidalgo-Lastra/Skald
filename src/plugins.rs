//! Plugin load order (`Plugins.txt` / `loadorder.txt` in the Proton prefix).

use crate::paths::find_child_ci;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Always loaded first, in this order, by the engine itself.
pub const BASE_MASTERS: &[&str] = &["Skyrim.esm", "Update.esm", "Dawnguard.esm", "HearthFires.esm", "Dragonborn.esm"];

#[derive(Debug, Clone)]
pub struct PluginEntry {
    pub name: String,
    pub enabled: bool,
    pub master: bool,
    pub light: bool,
    /// Base game / Creation Club: can't be disabled or reordered.
    pub locked: bool,
    /// In `Data` but not placed by Skald or provided by an enabled mod (left over from
    /// a manual install or another mod manager). Set by the caller via `mark_unmanaged`.
    pub unmanaged: bool,
    /// Where the plugin file is: in `Data`, or in an enabled mod's staging folder.
    pub path: PathBuf,
}

/// Read the TES4 header flags of a plugin: (master, light).
pub fn header_flags(path: &Path) -> (bool, bool) {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut buf = [0u8; 12];
    let flags = fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut buf))
        .ok()
        .filter(|_| &buf[..4] == b"TES4")
        .map(|_| u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]))
        .unwrap_or(0);
    let master = flags & 0x1 != 0 || ext == "esm" || ext == "esl";
    let light = flags & 0x200 != 0 || ext == "esl";
    (master, light)
}

fn read_list(path: Option<PathBuf>) -> Vec<(String, bool)> {
    let Some(text) = path.and_then(|p| fs::read(p).ok()) else { return vec![] };
    String::from_utf8_lossy(&text)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.strip_prefix('*') {
            Some(n) => (n.trim().to_string(), true),
            None => (l.to_string(), false),
        })
        .collect()
}

pub struct LoadOrder {
    pub plugins: Vec<PluginEntry>,
    appdata: PathBuf,
}

impl LoadOrder {
    /// Build the load order from what's in `data_dir` plus `extra` plugins
    /// (name, path) provided by enabled-but-not-yet-deployed mods.
    pub fn load(appdata: &Path, data_dir: &Path, game_dir: &Path, extra: &[(String, PathBuf)]) -> Self {
        // Every plugin that exists, keyed case-insensitively.
        let mut available: Vec<(String, PathBuf)> = fs::read_dir(data_dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
                    .filter(|(n, _)| crate::installer::is_plugin_name(n))
                    .collect()
            })
            .unwrap_or_default();
        for (n, p) in extra {
            if !available.iter().any(|(a, _)| a.eq_ignore_ascii_case(n)) {
                available.push((n.clone(), p.clone()));
            }
        }
        let find = |n: &str| available.iter().find(|(a, _)| a.eq_ignore_ascii_case(n)).cloned();

        let active: HashSet<String> = read_list(find_child_ci(appdata, "Plugins.txt"))
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| n.to_lowercase())
            .collect();
        let mut order: Vec<String> = read_list(find_child_ci(appdata, "loadorder.txt")).into_iter().map(|(n, _)| n).collect();
        order.extend(read_list(find_child_ci(appdata, "Plugins.txt")).into_iter().map(|(n, _)| n));

        // Locked entries: base masters, then Creation Club content from Skyrim.ccc.
        let ccc: Vec<String> = find_child_ci(game_dir, "Skyrim.ccc")
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|t| t.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
            .unwrap_or_default();
        let locked: Vec<String> = BASE_MASTERS.iter().map(|s| s.to_string()).chain(ccc).collect();

        let mut seen = HashSet::new();
        let mut plugins = vec![];
        let mut push = |name: &str, locked: bool, plugins: &mut Vec<PluginEntry>| {
            let Some((real, path)) = find(name) else { return };
            if !seen.insert(real.to_lowercase()) {
                return;
            }
            let (master, light) = header_flags(&path);
            plugins.push(PluginEntry {
                enabled: locked || active.contains(&real.to_lowercase()),
                name: real,
                master,
                light,
                locked,
                unmanaged: false,
                path,
            });
        };
        for n in &locked {
            push(n, true, &mut plugins);
        }
        for n in &order {
            push(n, false, &mut plugins);
        }
        // New plugins: append, enabled by default.
        let mut new: Vec<String> = available.iter().map(|(n, _)| n.clone()).collect();
        new.sort_by_key(|n| n.to_lowercase());
        let before = plugins.len();
        for n in &new {
            push(n, false, &mut plugins);
        }
        for p in &mut plugins[before..] {
            p.enabled = true;
        }
        let mut lo = Self { plugins, appdata: appdata.to_path_buf() };
        lo.sort_masters_first();
        lo
    }

    /// The engine forces masters before regular plugins; reflect that.
    pub fn sort_masters_first(&mut self) {
        self.plugins.sort_by_key(|p| (!p.locked, !p.master));
    }

    /// Reorder to follow `names` (matched ignoring case). Plugins not in `names` go after, in
    /// their current order. Locked plugins stay first and masters stay before regular plugins.
    pub fn apply_order(&mut self, names: &[String]) {
        let pos = |p: &PluginEntry| names.iter().position(|n| n.eq_ignore_ascii_case(&p.name)).unwrap_or(usize::MAX);
        self.plugins.sort_by_key(|p| pos(p));
        self.sort_masters_first();
    }

    /// Flag every non-locked plugin whose lowercase name isn't in `managed`.
    pub fn mark_unmanaged(&mut self, managed: &HashSet<String>) {
        for p in &mut self.plugins {
            p.unmanaged = !p.locked && !managed.contains(&p.name.to_lowercase());
        }
    }

    pub fn can_move(&self, i: usize, up: bool) -> bool {
        let j = if up { i.checked_sub(1) } else { Some(i + 1) };
        let Some(j) = j.filter(|j| *j < self.plugins.len()) else { return false };
        let (a, b) = (&self.plugins[i], &self.plugins[j]);
        !a.locked && !b.locked && a.master == b.master
    }

    pub fn move_by(&mut self, i: usize, up: bool) {
        if self.can_move(i, up) {
            let j = if up { i - 1 } else { i + 1 };
            self.plugins.swap(i, j);
        }
    }

    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(&self.appdata)?;
        let plugins_txt = find_child_ci(&self.appdata, "Plugins.txt").unwrap_or_else(|| self.appdata.join("Plugins.txt"));
        let loadorder_txt = find_child_ci(&self.appdata, "loadorder.txt").unwrap_or_else(|| self.appdata.join("loadorder.txt"));
        let mut p = String::from("# This file is used by Skyrim to keep track of your downloaded content.\r\n# Please do not modify this file.\r\n");
        let mut l = String::new();
        for e in &self.plugins {
            if e.enabled {
                p.push('*');
            }
            p.push_str(&e.name);
            p.push_str("\r\n");
            l.push_str(&e.name);
            l.push_str("\r\n");
        }
        fs::write(plugins_txt, p)?;
        fs::write(loadorder_txt, l)?;
        Ok(())
    }
}

/// Move the named plugin files out of `data_dir` into a fresh folder under `dest_root`
/// (nothing is deleted). Returns that folder and the names actually moved.
pub fn move_out(data_dir: &Path, names: &[String], dest_root: &Path) -> Result<(PathBuf, Vec<String>)> {
    let stamp = crate::mods::now();
    let mut dest = dest_root.join(stamp.to_string());
    for n in 1.. {
        if !dest.exists() {
            break;
        }
        dest = dest_root.join(format!("{stamp}-{n}"));
    }
    fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;
    let mut moved = vec![];
    // Linux is case-sensitive, so Data can hold several spellings of one plugin; the load
    // order shows them as one entry, so move them all or the plugin seems to stay.
    let entries: Vec<PathBuf> = fs::read_dir(data_dir)
        .with_context(|| format!("reading {}", data_dir.display()))?
        .flatten()
        .map(|e| e.path())
        .collect();
    let targets = entries.iter().filter(|p| {
        let n = p.file_name().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
        names.iter().any(|w| w.to_lowercase() == n)
    });
    for src in targets {
        let real = src.file_name().unwrap().to_os_string();
        let to = dest.join(&real);
        if fs::rename(src, &to).is_err() {
            // Different filesystem: copy, then remove the original only once the copy exists.
            fs::copy(src, &to).with_context(|| format!("copying {}", src.display()))?;
            fs::remove_file(src).with_context(|| format!("removing {}", src.display()))?;
        }
        moved.push(real.to_string_lossy().into_owned());
    }
    Ok((dest, moved))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmanaged_marking_and_move_out() {
        let t = tempfile::tempdir().unwrap();
        let (data, appdata, game, stash) = (t.path().join("Data"), t.path().join("app"), t.path().to_path_buf(), t.path().join("unmanaged"));
        fs::create_dir_all(&data).unwrap();
        for n in ["Skyrim.esm", "Mine.esp", "Old.esp", "old.esp"] {
            fs::write(data.join(n), b"").unwrap();
        }
        let mut lo = LoadOrder::load(&appdata, &data, &game, &[]);
        lo.mark_unmanaged(&HashSet::from(["mine.esp".to_string()]));
        let un: Vec<_> = lo.plugins.iter().filter(|p| p.unmanaged).map(|p| p.name.clone()).collect();
        assert_eq!(un.len(), 1);
        assert!(un[0].eq_ignore_ascii_case("old.esp"));

        // Both spellings go, even though the load order lists only one.
        let (dest, mut moved) = move_out(&data, &["old.ESP".to_string()], &stash).unwrap();
        moved.sort();
        assert_eq!(moved, ["Old.esp", "old.esp"]);
        assert!(dest.join("Old.esp").exists() && dest.join("old.esp").exists());
        assert!(!data.join("Old.esp").exists() && !data.join("old.esp").exists());
        assert!(data.join("Mine.esp").exists());
    }

    #[test]
    fn apply_order_keeps_locked_and_masters_first() {
        let t = tempfile::tempdir().unwrap();
        let (data, appdata, game) = (t.path().join("Data"), t.path().join("app"), t.path().to_path_buf());
        fs::create_dir_all(&data).unwrap();
        for n in ["Skyrim.esm", "M.esm", "A.esp", "B.esp", "C.esp"] {
            fs::write(data.join(n), b"").unwrap();
        }
        let mut lo = LoadOrder::load(&appdata, &data, &game, &[]);
        lo.apply_order(&["c.esp".into(), "B.esp".into(), "M.esm".into()]);
        let names: Vec<_> = lo.plugins.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Skyrim.esm", "M.esm", "C.esp", "B.esp", "A.esp"]);
        assert_eq!(lo.plugins[2].path, data.join("C.esp"));
    }
}
