//! Figures out what kind of installer an extracted mod archive contains, and
//! turns the user's choices into a concrete list of copy operations.

use crate::fomod::{FileItem, FomodPackage};
use crate::paths::{self, find_child_ci, normalize_rel, resolve_ci};
use anyhow::{Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

/// Top-level names that only appear inside a Skyrim `Data` folder.
const DATA_DIRS: &[&str] = &[
    "meshes", "textures", "scripts", "interface", "sound", "music", "skse", "seq", "strings",
    "video", "shadersfx", "lodsettings", "grass", "materials", "source", "dialogueviews",
    "calientetools", "nemesis_engine", "tools", "dyndolod", "netscriptframework", "platform",
    "mcm", "lsdata", "trees", "shaders", "fonts", "terrain", "facegen", "bashtags", "skyproc patchers",
    "actors", "distantlod", "lodgen", "textures_", "sksevr", "config", "_spidinis", "parallax",
];
const DATA_EXTS: &[&str] = &["esp", "esm", "esl", "bsa", "ini", "seq", "modgroups"];
const PLUGIN_EXTS: &[&str] = &["esp", "esm", "esl"];
/// Files that only belong next to SkyrimSE.exe.
const ROOT_MARKERS: &[&str] = &[
    "skse64_loader.exe", "skyrimse.exe", "d3d11.dll", "d3dcompiler_46e.dll", "dxgi.dll",
    "dinput8.dll", "enblocal.ini", "enbseries.ini", "enbseries", "reshade.ini", "binkw64.dll",
    "version.dll", "winhttp.dll",
];
/// Ignored when deciding whether an archive is just "a folder wrapping the mod".
const DOC_EXTS: &[&str] = &["txt", "md", "pdf", "url", "html", "htm", "rtf", "doc", "docx", "jpg", "jpeg", "png", "webp"];

#[derive(Debug, Clone)]
pub enum InstallerKind {
    /// XML FOMOD: show the step-by-step wizard.
    Fomod(Box<FomodPackage>),
    /// C# script FOMOD (`fomod/script.cs`) — cannot be run; user picks the data folder.
    FomodScript { root: PathBuf },
    /// Wrye Bash BAIN package with numbered sub-packages (`00 Core`, `10 Option`, ...).
    Bain { root: PathBuf, packages: Vec<String> },
    /// Plain mod: `root` maps straight onto `Data`.
    Data { root: PathBuf },
    /// Things like SKSE or ENB: `root` maps onto the game folder (its `Data`
    /// subfolder, if any, onto `Data`).
    GameRoot { root: PathBuf },
    /// Couldn't tell: the user picks which folder is the data root.
    Unknown { root: PathBuf },
}

impl InstallerKind {
    pub fn label(&self) -> &'static str {
        match self {
            InstallerKind::Fomod(_) => "FOMOD",
            InstallerKind::FomodScript { .. } => "FOMOD (C# script)",
            InstallerKind::Bain { .. } => "BAIN",
            InstallerKind::Data { .. } => "Simple",
            InstallerKind::GameRoot { .. } => "Game root",
            InstallerKind::Unknown { .. } => "Manual",
        }
    }
}

fn ext_of(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    v.sort();
    v
}

fn name_lc(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Does this directory look like the contents of a `Data` folder?
pub fn looks_like_data(dir: &Path) -> bool {
    entries(dir).iter().any(|e| {
        if e.is_dir() {
            DATA_DIRS.contains(&name_lc(e).as_str())
        } else {
            DATA_EXTS.contains(&ext_of(e).as_str())
        }
    })
}

fn has_root_markers(dir: &Path) -> bool {
    entries(dir).iter().any(|e| {
        let n = name_lc(e);
        ROOT_MARKERS.contains(&n.as_str()) || (e.is_file() && (ext_of(e) == "exe" || ext_of(e) == "dll"))
    })
}

fn is_bain_name(name: &str) -> bool {
    let digits = name.chars().take_while(|c| c.is_ascii_digit()).count();
    digits >= 2 && name[digits..].starts_with([' ', '_', '-', '.'])
}

/// Descend through folders that just wrap the real content
/// (`MyMod-1234-1-0/MyMod/...`).
fn unwrap_dir(mut dir: PathBuf) -> PathBuf {
    for _ in 0..8 {
        if looks_like_data(&dir) || has_root_markers(&dir) {
            break;
        }
        let meaningful: Vec<PathBuf> = entries(&dir)
            .into_iter()
            .filter(|e| e.is_dir() || !DOC_EXTS.contains(&ext_of(e).as_str()))
            .collect();
        match meaningful.as_slice() {
            [only] if only.is_dir() && name_lc(only) != "data" && name_lc(only) != "fomod" => dir = only.clone(),
            _ => break,
        }
    }
    dir
}

fn find_fomod(dir: &Path, depth: usize) -> Option<(PathBuf, Option<PathBuf>)> {
    if let Some(fdir) = find_child_ci(dir, "fomod").filter(|p| p.is_dir()) {
        if let Some(cfg) = find_child_ci(&fdir, "ModuleConfig.xml") {
            return Some((dir.to_path_buf(), Some(cfg)));
        }
        if find_child_ci(&fdir, "script.cs").is_some() {
            return Some((dir.to_path_buf(), None));
        }
    }
    if depth == 0 {
        return None;
    }
    entries(dir).iter().filter(|e| e.is_dir()).find_map(|e| find_fomod(e, depth - 1))
}

/// Inspect an extracted archive and decide what installer to show.
pub fn detect(extracted: &Path) -> Result<InstallerKind> {
    if let Some((root, cfg)) = find_fomod(extracted, 3) {
        return Ok(match cfg {
            Some(cfg) => match FomodPackage::load(&root, &cfg) {
                Ok(pkg) if !pkg.config.steps.is_empty() || !pkg.config.required_files.is_empty() => {
                    InstallerKind::Fomod(Box::new(pkg))
                }
                // A FOMOD with only info.xml/empty config: treat as a plain archive.
                Ok(_) => detect_plain(&root),
                Err(e) => bail!("FOMOD installer is broken: {e:#}"),
            },
            None => InstallerKind::FomodScript { root },
        });
    }
    Ok(detect_plain(extracted))
}

fn detect_plain(extracted: &Path) -> InstallerKind {
    let root = unwrap_dir(extracted.to_path_buf());

    if has_root_markers(&root) {
        return InstallerKind::GameRoot { root };
    }
    if !looks_like_data(&root)
        && let Some(data) = find_child_ci(&root, "data").filter(|d| d.is_dir()) {
            return InstallerKind::Data { root: data };
        }
    if looks_like_data(&root) {
        return InstallerKind::Data { root };
    }
    let packages: Vec<String> = entries(&root)
        .iter()
        .filter(|e| e.is_dir())
        .map(|e| e.file_name().unwrap().to_string_lossy().into_owned())
        .filter(|n| is_bain_name(n))
        .collect();
    if packages.len() >= 2 {
        return InstallerKind::Bain { root, packages };
    }
    InstallerKind::Unknown { root }
}

// ------------------------------------------------------------ install plan ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Data,
    GameRoot,
}

#[derive(Debug, Clone)]
pub struct CopyOp {
    pub src: PathBuf,
    pub dest_rel: PathBuf,
    pub target: Target,
}

pub fn plan_data_dir(root: &Path) -> Vec<CopyOp> {
    vec![CopyOp { src: root.to_path_buf(), dest_rel: PathBuf::new(), target: Target::Data }]
}

pub fn plan_game_root(root: &Path) -> Vec<CopyOp> {
    entries(root)
        .into_iter()
        .map(|e| {
            if e.is_dir() && name_lc(&e) == "data" {
                CopyOp { src: e, dest_rel: PathBuf::new(), target: Target::Data }
            } else {
                let name = PathBuf::from(e.file_name().unwrap());
                CopyOp { src: e, dest_rel: name, target: Target::GameRoot }
            }
        })
        .collect()
}

pub fn plan_bain(root: &Path, selected: &[String]) -> Vec<CopyOp> {
    selected
        .iter()
        .map(|p| {
            let dir = root.join(p);
            // Some BAIN packages wrap their contents in Data/.
            let src = match find_child_ci(&dir, "data") {
                Some(d) if d.is_dir() && !looks_like_data(&dir) => d,
                _ => dir,
            };
            CopyOp { src, dest_rel: PathBuf::new(), target: Target::Data }
        })
        .collect()
}

/// Turn the FOMOD wizard's file list into copy ops. Missing sources are
/// reported rather than silently skipped.
pub fn plan_fomod(pkg: &FomodPackage, files: &[FileItem]) -> (Vec<CopyOp>, Vec<String>) {
    let mut ops = vec![];
    let mut missing = vec![];
    for f in files {
        let src = resolve_ci(&pkg.root, &normalize_rel(&f.source));
        if !src.exists() {
            missing.push(f.source.clone());
            continue;
        }
        ops.push(CopyOp { src, dest_rel: f.dest_rel(), target: Target::Data });
    }
    (ops, missing)
}

/// Execute a plan into a mod's staging directory (`<mod>/data`, `<mod>/root`).
pub fn execute(ops: &[CopyOp], mod_dir: &Path) -> Result<usize> {
    let data = mod_dir.join("data");
    let root = mod_dir.join("root");
    fs::create_dir_all(&data)?;
    let mut n = 0;
    for op in ops {
        let base = match op.target {
            Target::Data => &data,
            Target::GameRoot => {
                fs::create_dir_all(&root)?;
                &root
            }
        };
        n += paths::copy_merge(&op.src, base, &op.dest_rel)?;
    }
    // Never ship the installer itself into Data.
    if let Some(f) = find_child_ci(&data, "fomod").filter(|p| p.is_dir()) {
        let _ = fs::remove_dir_all(f);
    }
    Ok(n)
}

/// Plugins (`.esp/.esm/.esl`) at the top level of a directory.
pub fn top_level_plugins(dir: &Path) -> Vec<String> {
    entries(dir)
        .into_iter()
        .filter(|e| e.is_file() && PLUGIN_EXTS.contains(&ext_of(e).as_str()))
        .map(|e| e.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

pub fn is_plugin_name(name: &str) -> bool {
    PLUGIN_EXTS.contains(&ext_of(Path::new(name)).as_str())
}

/// Guess a mod name from a Nexus download filename like
/// `SkyUI_5_2_SE-12604-5-2SE-1573147237.7z`.
pub fn guess_name(archive: &Path) -> String {
    let stem = if archive.is_dir() {
        archive.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        archive.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    };
    // Strip the Nexus "-<modid>-<version...>-<timestamp>" suffix.
    let parts: Vec<&str> = stem.split('-').collect();
    let name = match parts.iter().position(|p| p.len() >= 2 && p.chars().all(|c| c.is_ascii_digit())) {
        Some(i) if i > 0 && parts.len() - i >= 2 => parts[..i].join("-"),
        _ => stem.clone(),
    };
    paths::sanitize_name(&name.replace('_', " "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    #[test]
    fn detects_kinds() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        touch(&a.join("MyMod/Data/Meshes/x.nif"));
        assert!(matches!(detect(&a).unwrap(), InstallerKind::Data { root } if root.ends_with("Data")));

        let b = t.path().join("b");
        touch(&b.join("skse64_2_02/skse64_loader.exe"));
        touch(&b.join("skse64_2_02/Data/Scripts/a.pex"));
        assert!(matches!(detect(&b).unwrap(), InstallerKind::GameRoot { .. }));

        let c = t.path().join("c");
        touch(&c.join("00 Core/x.esp"));
        touch(&c.join("10 Option/textures/a.dds"));
        assert!(matches!(detect(&c).unwrap(), InstallerKind::Bain { packages, .. } if packages.len() == 2));

        let d = t.path().join("d");
        fs::create_dir_all(d.join("Pack/fomod")).unwrap();
        fs::write(
            d.join("Pack/fomod/moduleconfig.xml"),
            "<config><moduleName>X</moduleName><requiredInstallFiles><file source=\"a.esp\"/></requiredInstallFiles></config>",
        )
        .unwrap();
        assert!(matches!(detect(&d).unwrap(), InstallerKind::Fomod(p) if p.config.name == "X"));

        let e = t.path().join("e");
        touch(&e.join("stuff/thing.bin"));
        assert!(matches!(detect(&e).unwrap(), InstallerKind::Unknown { .. }));
    }

    #[test]
    fn guesses_nexus_names() {
        assert_eq!(guess_name(Path::new("SkyUI_5_2_SE-12604-5-2SE-1573147237.7z")), "SkyUI 5 2 SE");
        assert_eq!(guess_name(Path::new("Cool Mod.zip")), "Cool Mod");
    }
}
