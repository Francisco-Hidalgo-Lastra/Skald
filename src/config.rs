//! Settings and Skyrim SE (Steam/Proton) auto-detection.

use crate::paths::find_child_ci;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const APP_ID: u32 = 489830;
pub const GAME_FOLDER: &str = "Skyrim Special Edition";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DeployMethod {
    /// Hard links (falls back to symlinks across filesystems). Invisible to the game.
    #[default]
    Hardlink,
    Symlink,
    Copy,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Folder containing SkyrimSE.exe.
    pub game_dir: Option<PathBuf>,
    /// `.../AppData/Local/Skyrim Special Edition` inside the Proton prefix
    /// (where Plugins.txt lives).
    pub appdata_dir: Option<PathBuf>,
    #[serde(default)]
    pub deploy_method: DeployMethod,
    /// Name of the Play runner (None = Steam).
    #[serde(default)]
    pub runner: Option<String>,
    /// Programs added to the Play menu.
    #[serde(default)]
    pub runners: Vec<crate::launch::CustomRunner>,
    /// Programs in the tools tray of the top bar.
    #[serde(default)]
    pub tools: Vec<crate::launch::CustomRunner>,
    /// Proton folder override for non-Steam runners (None = auto).
    #[serde(default)]
    pub proton: Option<PathBuf>,
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("skald")
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("skald")
}

impl Config {
    pub fn load() -> Self {
        let path = config_dir().join("config.json");
        let mut cfg: Config = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if (cfg.game_dir.is_none() || cfg.appdata_dir.is_none())
            && let Some(d) = detect()
        {
            cfg.game_dir.get_or_insert(d.game_dir);
            if cfg.appdata_dir.is_none() {
                cfg.appdata_dir = d.appdata_dir;
            }
        }
        cfg
    }

    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(config_dir())?;
        fs::write(config_dir().join("config.json"), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn data_path(&self) -> Option<PathBuf> {
        let g = self.game_dir.as_ref()?;
        Some(find_child_ci(g, "Data").unwrap_or_else(|| g.join("Data")))
    }

    pub fn is_valid_game_dir(p: &Path) -> bool {
        find_child_ci(p, "SkyrimSE.exe").is_some()
    }
}

pub struct Detected {
    pub game_dir: PathBuf,
    pub appdata_dir: Option<PathBuf>,
}

pub fn steam_roots() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    [
        ".local/share/Steam",
        ".steam/steam",
        ".steam/root",
        ".var/app/com.valvesoftware.Steam/.local/share/Steam",
        "snap/steam/common/.local/share/Steam",
    ]
    .iter()
    .map(|p| home.join(p))
    .filter(|p| p.join("steamapps").is_dir())
    .collect()
}

/// All Steam library folders (each containing `steamapps`).
pub fn libraries() -> Vec<PathBuf> {
    let mut libs = vec![];
    for root in steam_roots() {
        libs.push(root.clone());
        if let Ok(vdf) = fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
            for line in vdf.lines() {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("\"path\"") {
                    let p = rest.trim().trim_matches('"').replace("\\\\", "/");
                    if p.starts_with('/') {
                        libs.push(PathBuf::from(p));
                    }
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    libs.retain(|l| seen.insert(fs::canonicalize(l).unwrap_or(l.clone())));
    libs
}

pub fn appdata_for_library(lib: &Path) -> PathBuf {
    lib.join(format!(
        "steamapps/compatdata/{APP_ID}/pfx/drive_c/users/steamuser/AppData/Local/{GAME_FOLDER}"
    ))
}

pub fn detect() -> Option<Detected> {
    let libs = libraries();
    let candidates: Vec<PathBuf> = libs.iter().map(|l| l.join("steamapps/common").join(GAME_FOLDER)).collect();
    // Prefer a complete install; fall back to a folder that still has Data
    // (e.g. while Steam is updating/verifying and the exe is temporarily gone).
    let game_dir = candidates
        .iter()
        .find(|p| Config::is_valid_game_dir(p))
        .or_else(|| candidates.iter().find(|p| find_child_ci(p, "Data").is_some_and(|d| d.is_dir())))?
        .clone();
    // The prefix normally lives in the same library as the game, but check all.
    let appdata_dir = libs
        .iter()
        .map(|l| appdata_for_library(l))
        .find(|p| p.is_dir())
        // Fresh prefix (game never launched yet): use where it will be created.
        .or_else(|| {
            libs.iter()
                .find(|l| l.join(format!("steamapps/compatdata/{APP_ID}/pfx")).is_dir())
                .map(|l| appdata_for_library(l))
        });
    Some(Detected { game_dir, appdata_dir })
}

/// Given a game dir, guess where its Proton prefix AppData is.
pub fn appdata_from_game_dir(game_dir: &Path) -> Option<PathBuf> {
    // <lib>/steamapps/common/<game>  ->  <lib>
    let lib = game_dir.parent()?.parent()?.parent()?;
    let p = appdata_for_library(lib);
    p.is_dir().then_some(p).or_else(|| detect().and_then(|d| d.appdata_dir))
}
