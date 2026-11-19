//! Starting the game: through Steam, or with another program (SKSE, custom
//! .exe) run directly with Proton inside the game's prefix.

use crate::config::{self, APP_ID, Config};
use crate::paths::find_child_ci;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A program the user added to the Play menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomRunner {
    pub name: String,
    pub exe: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runner {
    /// `steam://rungameid/489830` (the normal Skyrim launcher).
    Steam,
    /// A Windows program run with Proton in the game's prefix.
    Exe { name: String, exe: PathBuf },
}

pub const SKSE: &str = "SKSE";

impl Runner {
    pub fn name(&self) -> &str {
        match self {
            Runner::Steam => "Steam",
            Runner::Exe { name, .. } => name,
        }
    }
}

/// Steam, then SKSE if its loader is in the game folder, then custom programs.
pub fn runners(cfg: &Config) -> Vec<Runner> {
    let mut v = vec![Runner::Steam];
    if let Some(exe) = cfg.game_dir.as_deref().and_then(|g| find_child_ci(g, "skse64_loader.exe")) {
        v.push(Runner::Exe { name: SKSE.into(), exe });
    }
    for c in &cfg.runners {
        if !v.iter().any(|r| r.name() == c.name) {
            v.push(Runner::Exe { name: c.name.clone(), exe: c.exe.clone() });
        }
    }
    v
}

/// The runner chosen in the config, or Steam if it's no longer available.
pub fn selected(cfg: &Config) -> Runner {
    let all = runners(cfg);
    cfg.runner
        .as_deref()
        .and_then(|n| all.iter().find(|r| r.name() == n).cloned())
        .unwrap_or(Runner::Steam)
}

/// `<lib>/steamapps/compatdata/489830`.
pub fn compat_data(cfg: &Config) -> Option<PathBuf> {
    let id = APP_ID.to_string();
    let from_appdata = cfg.appdata_dir.as_deref().and_then(|a| {
        a.ancestors().find(|p| p.file_name().is_some_and(|n| n == id.as_str()) && p.join("pfx").is_dir()).map(Path::to_path_buf)
    });
    from_appdata.or_else(|| {
        let mut libs = vec![];
        if let Some(lib) = cfg.game_dir.as_deref().and_then(|g| g.parent()?.parent()?.parent()) {
            libs.push(lib.to_path_buf());
        }
        libs.extend(config::libraries());
        libs.into_iter().map(|l| l.join("steamapps/compatdata").join(&id)).find(|p| p.join("pfx").is_dir())
    })
}

fn is_proton_dir(p: &Path) -> bool {
    p.join("proton").is_file()
}

/// The Proton folder (containing the `proton` script) to run programs with:
/// the configured one, else the one Steam last used for Skyrim, else any installed.
pub fn find_proton(cfg: &Config) -> Option<PathBuf> {
    if let Some(p) = cfg.proton.as_ref().filter(|p| is_proton_dir(p)) {
        return Some(p.clone());
    }
    // Proton writes its own paths (".../<proton>/files/...") into config_info.
    if let Some(info) = compat_data(cfg).and_then(|c| fs::read_to_string(c.join("config_info")).ok()) {
        for line in info.lines() {
            if let Some((dir, _)) = line.split_once("/files/") {
                let dir = PathBuf::from(dir);
                if is_proton_dir(&dir) {
                    return Some(dir);
                }
            }
        }
    }
    installed_protons().into_iter().next()
}

/// Every Proton build Steam knows about, newest-looking names first.
pub fn installed_protons() -> Vec<PathBuf> {
    let mut found = vec![];
    for root in config::steam_roots() {
        for sub in ["compatibilitytools.d", "steamapps/common"] {
            let Ok(rd) = fs::read_dir(root.join(sub)) else { continue };
            found.extend(rd.flatten().map(|e| e.path()).filter(|p| is_proton_dir(p)));
        }
    }
    let mut seen = std::collections::HashSet::new();
    found.retain(|p| seen.insert(fs::canonicalize(p).unwrap_or(p.clone())));
    found.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    found
}

/// Is the Steam client running? (Skyrim exits immediately without it.)
pub fn steam_running() -> bool {
    let Some(home) = dirs::home_dir() else { return true };
    let pid = fs::read_to_string(home.join(".steam/steam.pid")).unwrap_or_default();
    let pid = pid.trim();
    !pid.is_empty() && Path::new("/proc").join(pid).exists()
}

/// Start the game with `runner`. Returns once the process has been spawned.
pub fn launch(cfg: &Config, runner: &Runner) -> Result<()> {
    match runner {
        Runner::Steam => {
            Command::new("xdg-open")
                .arg(format!("steam://rungameid/{APP_ID}"))
                .spawn()
                .context("couldn't run xdg-open")?;
        }
        Runner::Exe { exe, .. } => {
            if !exe.is_file() {
                bail!("{} doesn't exist", exe.display());
            }
            let proton = find_proton(cfg).context("no Proton found; set it in Settings")?;
            let compat = compat_data(cfg).context("Skyrim's Proton prefix wasn't found; start the game once through Steam")?;
            let steam = config::steam_roots().into_iter().next().context("Steam wasn't found")?;
            let mut child = Command::new(proton.join("proton"))
                .arg("run")
                .arg(exe)
                .current_dir(exe.parent().unwrap_or(Path::new("/")))
                .env("STEAM_COMPAT_DATA_PATH", &compat)
                .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", &steam)
                .env("SteamAppId", APP_ID.to_string())
                .env("SteamGameId", APP_ID.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .with_context(|| format!("couldn't start {}", proton.join("proton").display()))?;
            // Reap it so it doesn't linger as a zombie.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"").unwrap();
    }

    #[test]
    fn runners_and_selection() {
        let t = tempfile::tempdir().unwrap();
        let game = t.path().join("game");
        touch(&game.join("SkyrimSE.exe"));
        let mut cfg = Config { game_dir: Some(game.clone()), ..Default::default() };
        assert_eq!(runners(&cfg), vec![Runner::Steam]);

        touch(&game.join("SKSE64_Loader.exe"));
        cfg.runners.push(CustomRunner { name: "xEdit".into(), exe: t.path().join("xEdit.exe") });
        let names: Vec<_> = runners(&cfg).iter().map(|r| r.name().to_string()).collect();
        assert_eq!(names, ["Steam", SKSE, "xEdit"]);

        cfg.runner = Some(SKSE.into());
        assert_eq!(selected(&cfg), Runner::Exe { name: SKSE.into(), exe: game.join("SKSE64_Loader.exe") });
        cfg.runner = Some("gone".into());
        assert_eq!(selected(&cfg), Runner::Steam);
    }

    #[test]
    fn proton_from_config_info() {
        let t = tempfile::tempdir().unwrap();
        let proton = t.path().join("tools/GE-Proton");
        touch(&proton.join("proton"));
        let compat = t.path().join("lib/steamapps/compatdata/489830");
        let appdata = compat.join("pfx/drive_c/users/steamuser/AppData/Local/Skyrim Special Edition");
        fs::create_dir_all(&appdata).unwrap();
        fs::write(compat.join("config_info"), format!("11.0-100\n{}/files/share/fonts/\n", proton.display())).unwrap();
        let mut cfg = Config { appdata_dir: Some(appdata), ..Default::default() };
        assert_eq!(compat_data(&cfg), Some(compat));
        assert_eq!(find_proton(&cfg), Some(proton.clone()));

        let other = t.path().join("tools/Other");
        touch(&other.join("proton"));
        cfg.proton = Some(other.clone());
        assert_eq!(find_proton(&cfg), Some(other));
    }
}
