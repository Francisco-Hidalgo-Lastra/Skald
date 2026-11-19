//! The install dialog. Shows a different menu depending on the installer type
//! that was detected in the archive.

use crate::theme;
use egui::{Color32, RichText};
use skald::fomod::{Environment, FileState, FomodPackage, GroupType, PluginType, Wizard};
use skald::installer::{self, CopyOp, InstallerKind};
use skald::paths::{self, normalize_rel, resolve_ci};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Answers FOMOD `fileDependency` checks against the game's Data folder and
/// the staging folders of enabled mods.
pub struct GameEnv {
    pub roots: Vec<PathBuf>,
    pub active_plugins: HashSet<String>,
}

impl Environment for GameEnv {
    fn file_state(&self, file: &str) -> FileState {
        let rel = normalize_rel(file);
        if !self.roots.iter().any(|r| resolve_ci(r, &rel).exists()) {
            return FileState::Missing;
        }
        let name = rel.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if installer::is_plugin_name(&name) && !self.active_plugins.contains(&name) {
            FileState::Inactive
        } else {
            FileState::Active
        }
    }
}

pub enum Menu {
    Fomod { pkg: Box<FomodPackage>, wizard: Wizard, focus: Option<(usize, usize)> },
    Bain { root: PathBuf, packages: Vec<String>, checked: Vec<bool> },
    Simple { root: PathBuf, game_root: bool, files: Vec<PathBuf> },
    /// Unknown layout or C# FOMOD: user picks the folder that maps to Data.
    Manual { root: PathBuf, dirs: Vec<PathBuf>, chosen: usize, reason: String },
}

pub struct Session {
    pub archive: PathBuf,
    pub temp: PathBuf,
    pub name: String,
    pub kind_label: &'static str,
    pub version: Option<String>,
    pub author: Option<String>,
    pub replace_existing: bool,
    pub menu: Menu,
    pub env: GameEnv,
    error: Option<String>,
}

pub struct InstallRequest {
    pub name: String,
    pub replace: bool,
    pub ops: Vec<CopyOp>,
    pub kind_label: &'static str,
    pub version: Option<String>,
    pub author: Option<String>,
    pub source: PathBuf,
    pub temp: PathBuf,
}

pub enum Outcome {
    Pending,
    Cancel,
    Install(InstallRequest),
}

impl Session {
    pub fn new(archive: PathBuf, temp: PathBuf, extracted: &Path, kind: InstallerKind, env: GameEnv) -> Self {
        let mut name = installer::guess_name(&archive);
        let kind_label = kind.label();
        let (mut version, mut author) = (None, None);
        let menu = match kind {
            InstallerKind::Fomod(pkg) => {
                if let Some(n) = pkg.info.name.clone().or(Some(pkg.config.name.clone())).filter(|n| !n.trim().is_empty()) {
                    name = paths::sanitize_name(&n);
                }
                version = pkg.info.version.clone();
                author = pkg.info.author.clone();
                let wizard = Wizard::new(pkg.config.clone(), &env);
                Menu::Fomod { pkg, wizard, focus: None }
            }
            InstallerKind::Bain { root, packages } => {
                let checked = packages.iter().map(|p| p.starts_with("00")).collect();
                Menu::Bain { root, packages, checked }
            }
            InstallerKind::Data { root } => {
                let files = paths::list_files(&root);
                Menu::Simple { root, game_root: false, files }
            }
            InstallerKind::GameRoot { root } => {
                let files = paths::list_files(&root);
                Menu::Simple { root, game_root: true, files }
            }
            InstallerKind::FomodScript { root } => manual(
                root,
                "This archive uses a C# script FOMOD installer, which can't be run. Pick the folder that should become the Data folder.",
            ),
            InstallerKind::Unknown { root } => manual(
                root,
                "Couldn't recognise this archive's layout. Pick the folder whose contents belong in Skyrim's Data folder.",
            ),
        };
        let _ = extracted;
        Self { archive, temp, name, kind_label, version, author, replace_existing: false, menu, env, error: None }
    }

    pub fn show(&mut self, ctx: &egui::Context, name_taken: bool) -> Outcome {
        let mut outcome = Outcome::Pending;
        let screen = ctx.content_rect();
        egui::Modal::new(egui::Id::unique("install_modal")).show(ctx, |ui| {
            ui.set_width((screen.width() - 120.0).clamp(500.0, 1100.0));
            let max_h = (screen.height() - 160.0).max(300.0);

            ui.horizontal(|ui| {
                ui.heading("Install mod");
                theme::pill(ui, self.kind_label.to_uppercase(), theme::FROST);
            });
            ui.add(egui::Label::new(RichText::new(self.archive.display().to_string()).small().color(theme::TEXT_WEAK)).truncate());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Name").color(theme::TEXT_WEAK));
                ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(360.0));
            });
            if name_taken {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::YELLOW, "A mod with this name exists.");
                    ui.radio_value(&mut self.replace_existing, true, "Replace it");
                    ui.radio_value(&mut self.replace_existing, false, "Install as a new copy");
                });
            }
            ui.separator();

            let ready = match &mut self.menu {
                Menu::Fomod { pkg, wizard, focus } => fomod_ui(ui, pkg, wizard, focus, &self.env, max_h),
                Menu::Bain { packages, checked, .. } => bain_ui(ui, packages, checked, max_h),
                Menu::Simple { game_root, files, .. } => simple_ui(ui, *game_root, files, max_h),
                Menu::Manual { root, dirs, chosen, reason } => manual_ui(ui, root, dirs, chosen, reason, max_h),
            };

            if let Some(e) = &self.error {
                ui.colored_label(theme::RED, e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    outcome = Outcome::Cancel;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let is_wizard_mid = matches!(&self.menu, Menu::Fomod { wizard, .. } if wizard.has_next(&self.env));
                    if let Menu::Fomod { wizard, focus, .. } = &mut self.menu {
                        let errs = wizard.validation_errors();
                        if is_wizard_mid
                            && ui.add_enabled(errs.is_empty(), egui::Button::new("Next ▶")).clicked() {
                                wizard.next(&self.env);
                                *focus = None;
                            }
                        if ui.add_enabled(wizard.has_back(), egui::Button::new("◀ Back")).clicked() {
                            wizard.back();
                            *focus = None;
                        }
                    }
                    if !is_wizard_mid
                        && ui.add_enabled(ready && !self.name.trim().is_empty(), theme::accent_button("Install", theme::GOLD)).clicked()
                    {
                        match self.build_ops() {
                            Ok(ops) => {
                                outcome = Outcome::Install(InstallRequest {
                                    name: self.name.trim().to_string(),
                                    replace: name_taken && self.replace_existing,
                                    ops,
                                    kind_label: self.kind_label,
                                    version: self.version.clone(),
                                    author: self.author.clone(),
                                    source: self.archive.clone(),
                                    temp: self.temp.clone(),
                                })
                            }
                            Err(e) => self.error = Some(e),
                        }
                    }
                });
            });
        });
        outcome
    }

    /// A plain Data-folder mod has nothing to choose, so it can be installed
    /// without showing the dialog.
    pub fn quick_install(&self) -> Option<InstallRequest> {
        if !matches!(self.menu, Menu::Simple { game_root: false, .. }) || self.name.trim().is_empty() {
            return None;
        }
        Some(InstallRequest {
            name: self.name.trim().to_string(),
            replace: false,
            ops: self.build_ops().ok()?,
            kind_label: self.kind_label,
            version: self.version.clone(),
            author: self.author.clone(),
            source: self.archive.clone(),
            temp: self.temp.clone(),
        })
    }

    fn build_ops(&self) -> Result<Vec<CopyOp>, String> {
        let ops = match &self.menu {
            Menu::Fomod { pkg, wizard, .. } => {
                let files = wizard.files_to_install(&self.env);
                let (ops, missing) = installer::plan_fomod(pkg, &files);
                if !missing.is_empty() {
                    eprintln!("FOMOD references missing files: {missing:?}");
                }
                if ops.is_empty() {
                    return Err("The selected options don't install any files.".into());
                }
                ops
            }
            Menu::Bain { root, packages, checked } => {
                let sel: Vec<String> = packages.iter().zip(checked).filter(|(_, c)| **c).map(|(p, _)| p.clone()).collect();
                if sel.is_empty() {
                    return Err("Select at least one package.".into());
                }
                installer::plan_bain(root, &sel)
            }
            Menu::Simple { root, game_root: false, .. } => installer::plan_data_dir(root),
            Menu::Simple { root, game_root: true, .. } => installer::plan_game_root(root),
            Menu::Manual { root, dirs, chosen, .. } => installer::plan_data_dir(&root.join(&dirs[*chosen])),
        };
        Ok(ops)
    }
}

fn manual(root: PathBuf, reason: &str) -> Menu {
    let mut dirs = vec![PathBuf::new()];
    dirs.extend(
        walkdir::WalkDir::new(&root)
            .min_depth(1)
            .max_depth(4)
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_dir())
            .map(|e| e.path().strip_prefix(&root).unwrap().to_path_buf())
            .filter(|p| !p.components().any(|c| c.as_os_str().eq_ignore_ascii_case("fomod"))),
    );
    // Preselect the shallowest folder that looks like Data.
    let chosen = dirs.iter().position(|d| installer::looks_like_data(&root.join(d))).unwrap_or(0);
    Menu::Manual { root, dirs, chosen, reason: reason.into() }
}

fn type_badge(t: PluginType) -> Option<(&'static str, Color32)> {
    match t {
        PluginType::Required => Some(("required", theme::GOLD)),
        PluginType::Recommended => Some(("recommended", theme::GREEN)),
        PluginType::NotUsable => Some(("not usable", theme::RED)),
        PluginType::CouldBeUsable => Some(("could be usable", theme::TEXT_WEAK)),
        PluginType::Optional => None,
    }
}

fn image_widget(ui: &mut egui::Ui, path: &Path, max_h: f32) {
    let uri = format!("file://{}", path.display());
    ui.add(egui::Image::new(uri).max_height(max_h).max_width(ui.available_width()).maintain_aspect_ratio(true));
}

fn fomod_ui(
    ui: &mut egui::Ui,
    pkg: &FomodPackage,
    wizard: &mut Wizard,
    focus: &mut Option<(usize, usize)>,
    env: &GameEnv,
    max_h: f32,
) -> bool {
    let title = if pkg.config.name.is_empty() { "FOMOD installer".to_string() } else { pkg.config.name.clone() };
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(18.0).strong());
        if let Some(a) = &pkg.info.author {
            ui.label(RichText::new(format!("by {a}")).weak());
        }
        if let Some(v) = &pkg.info.version {
            ui.label(RichText::new(format!("v{v}")).weak());
        }
    });

    let Some(step) = wizard.current else {
        ui.label("This FOMOD has no options to choose; only its required files will be installed.");
        return true;
    };
    let visited = wizard.visited_steps().len();
    ui.horizontal(|ui| {
        theme::pill(ui, format!("STEP {visited}"), theme::GOLD);
        ui.label(RichText::new(&wizard.config.steps[step].name).size(15.0).color(theme::TEXT));
    });
    ui.add_space(4.0);

    let body_h = (max_h - 140.0).max(240.0);
    ui.columns(2, |cols| {
        // Left: groups and options.
        egui::ScrollArea::vertical().id_salt("fomod_opts").max_height(body_h).auto_shrink([false, false]).show(&mut cols[0], |ui| {
            let groups = wizard.config.steps[step].groups.clone();
            for (gi, g) in groups.iter().enumerate() {
                let hint = match g.kind {
                    GroupType::SelectExactlyOne => "choose one",
                    GroupType::SelectAtMostOne => "choose one or none",
                    GroupType::SelectAtLeastOne => "choose at least one",
                    GroupType::SelectAll => "all included",
                    GroupType::SelectAny => "choose any",
                };
                theme::card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&g.name).strong().color(theme::TEXT));
                        ui.label(RichText::new(format!("({hint})")).weak().small());
                    });
                    for (pi, p) in g.plugins.iter().enumerate() {
                        let ty = wizard.plugin_type(step, gi, pi, env);
                        let locked = wizard.is_locked(step, gi, pi, env);
                        let mut on = wizard.selected[step][gi][pi];
                        ui.horizontal(|ui| {
                            let resp = ui.add_enabled_ui(!locked, |ui| {
                                if g.kind == GroupType::SelectExactlyOne {
                                    ui.radio(on, &p.name)
                                } else {
                                    ui.checkbox(&mut on, &p.name)
                                }
                            });
                            let r = resp.inner;
                            if r.clicked() {
                                let value = if g.kind == GroupType::SelectExactlyOne { true } else { on };
                                wizard.set(step, gi, pi, value, env);
                                *focus = Some((gi, pi));
                            }
                            if r.hovered() {
                                *focus = Some((gi, pi));
                            }
                            if let Some((txt, col)) = type_badge(ty) {
                                theme::pill(ui, txt.to_uppercase(), col);
                            }
                        });
                    }
                });
                ui.add_space(4.0);
            }
        });

        // Right: description and image of the focused option.
        let right = &mut cols[1];
        let (gi, pi) = focus.unwrap_or_else(|| {
            let groups = &wizard.config.steps[step].groups;
            groups
                .iter()
                .enumerate()
                .find_map(|(gi, _)| wizard.selected[step][gi].iter().position(|s| *s).map(|pi| (gi, pi)))
                .unwrap_or((0, 0))
        });
        if let Some(p) = wizard.config.steps[step].groups.get(gi).and_then(|g| g.plugins.get(pi)) {
            egui::ScrollArea::vertical().id_salt("fomod_desc").max_height(body_h).auto_shrink([false, false]).show(right, |ui| {
                ui.label(RichText::new(&p.name).size(16.0).strong().color(theme::FROST));
                if let Some(img) = p.image.as_deref().and_then(|i| pkg.image_path(i)) {
                    image_widget(ui, &img, body_h * 0.6);
                } else if let Some(img) = pkg.config.image.as_deref().and_then(|i| pkg.image_path(i)) {
                    image_widget(ui, &img, body_h * 0.4);
                }
                ui.add_space(6.0);
                if p.description.is_empty() {
                    ui.label(RichText::new("No description.").weak());
                } else {
                    ui.label(&p.description);
                }
            });
        }
    });

    let errs = wizard.validation_errors();
    for e in &errs {
        ui.colored_label(theme::YELLOW, e);
    }
    errs.is_empty()
}

fn bain_ui(ui: &mut egui::Ui, packages: &[String], checked: &mut [bool], max_h: f32) -> bool {
    ui.label("BAIN package: choose which sub-packages to install. They are applied in order; later packages overwrite earlier ones.");
    ui.horizontal(|ui| {
        if ui.small_button("Select all").clicked() {
            checked.iter_mut().for_each(|c| *c = true);
        }
        if ui.small_button("Select none").clicked() {
            checked.iter_mut().for_each(|c| *c = false);
        }
    });
    egui::ScrollArea::vertical().max_height(max_h - 120.0).show(ui, |ui| {
        for (p, c) in packages.iter().zip(checked.iter_mut()) {
            ui.checkbox(c, p);
        }
    });
    checked.iter().any(|c| *c)
}

fn simple_ui(ui: &mut egui::Ui, game_root: bool, files: &[PathBuf], max_h: f32) -> bool {
    if game_root {
        ui.label(
            "This mod installs into the game folder (next to SkyrimSE.exe), e.g. SKSE or ENB. Its Data subfolder, if any, goes into Data.",
        );
    } else {
        ui.label("Simple mod: its contents go straight into the Data folder.");
    }
    ui.label(RichText::new(format!("{} files", files.len())).weak());
    egui::CollapsingHeader::new("File list").default_open(files.len() <= 30).show(ui, |ui| {
        egui::ScrollArea::vertical().max_height(max_h - 160.0).show(ui, |ui| {
            for f in files.iter().take(2000) {
                ui.label(RichText::new(f.display().to_string()).monospace().small());
            }
            if files.len() > 2000 {
                ui.label(format!("… and {} more", files.len() - 2000));
            }
        });
    });
    !files.is_empty()
}

fn manual_ui(ui: &mut egui::Ui, root: &Path, dirs: &[PathBuf], chosen: &mut usize, reason: &str, max_h: f32) -> bool {
    ui.label(reason);
    ui.add_space(4.0);
    ui.columns(2, |cols| {
        egui::ScrollArea::vertical().id_salt("manual_dirs").max_height(max_h - 160.0).auto_shrink([false, false]).show(&mut cols[0], |ui| {
            for (i, d) in dirs.iter().enumerate() {
                let depth = d.components().count();
                let label = if d.as_os_str().is_empty() {
                    "<archive root>".to_string()
                } else {
                    format!("{}{}", "    ".repeat(depth.saturating_sub(1)), d.file_name().unwrap().to_string_lossy())
                };
                let looks = installer::looks_like_data(&root.join(d));
                let text = if looks { RichText::new(label).color(theme::GREEN) } else { RichText::new(label) };
                if ui.selectable_label(*chosen == i, text).clicked() {
                    *chosen = i;
                }
            }
        });
        let ui = &mut cols[1];
        ui.label(RichText::new("Will be installed into Data:").strong());
        egui::ScrollArea::vertical().id_salt("manual_preview").max_height(max_h - 180.0).show(ui, |ui| {
            let dir = root.join(&dirs[*chosen]);
            let mut entries: Vec<_> = std::fs::read_dir(&dir).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
            entries.sort();
            for e in entries {
                let n = e.file_name().unwrap().to_string_lossy().into_owned();
                ui.label(RichText::new(if e.is_dir() { format!("📁 {n}/") } else { n }).monospace());
            }
        });
        ui.label(RichText::new("Green folders look like a Data folder.").weak().small());
    });
    true
}
