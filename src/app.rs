use crate::install_ui::{GameEnv, InstallRequest, Outcome, Session};
use crate::theme;
use egui::{Color32, RichText};
use egui_extras::Column;
use skald::config::{self, Config, DeployMethod};
use skald::deploy::{Deployer, Report};
use skald::installer::{self, InstallerKind};
use skald::launch::{self, CustomRunner, Runner};
use skald::loot::{self, SortInput, SortOutcome};
use skald::mods::{self, ModEntry, ModList};
use skald::plugins::LoadOrder;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

const BAR_H: f32 = 40.0;
const ROW_H: f32 = 28.0;

enum JobResult {
    Extracted { archive: PathBuf, temp: PathBuf, extracted: PathBuf, kind: InstallerKind },
    Installed { entry: ModEntry, replace_at: Option<usize>, files: usize, temp: PathBuf },
    Deployed(Report),
    Purged(Report),
    Sorted { outcome: SortOutcome, fresh_masterlist: bool },
    Failed { what: String, error: String, temp: Option<PathBuf> },
}

struct Job {
    label: String,
    rx: Receiver<JobResult>,
}

/// Which list fills the main area.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Mods,
    Plugins,
}

#[derive(Default)]
struct SettingsForm {
    open: bool,
    game_dir: String,
    appdata_dir: String,
    method: DeployMethod,
    proton: String,
    runners: Vec<CustomRunner>,
}

pub struct App {
    cfg: Config,
    base: PathBuf,
    mods: ModList,
    deployer: Deployer,
    load_order: Option<LoadOrder>,
    selected: Option<usize>,
    conflicts: std::collections::HashMap<String, (usize, usize)>,
    job: Option<Job>,
    install: Option<Session>,
    queue: Vec<PathBuf>,
    log: Vec<(bool, String)>,
    settings: SettingsForm,
    dirty: bool,
    rename: Option<(usize, String)>,
    confirm_remove: Option<usize>,
    confirm_unmanaged: bool,
    tab: Tab,
    /// Search text for the mod and plugin lists, kept separately per tab.
    mod_search: String,
    plugin_search: String,
    /// Icons read from the tools' exes (None = the exe has no icon).
    tool_icons: std::collections::HashMap<PathBuf, Option<egui::TextureHandle>>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
        let base = config::data_dir();
        let _ = std::fs::remove_dir_all(base.join("tmp"));
        let cfg = Config::load();
        let mut log = vec![];
        let mods = ModList::load(&base).unwrap_or_else(|e| {
            log.push((true, format!("Failed to load mod list: {e:#}")));
            ModList { base: base.clone(), ..Default::default() }
        });
        let mut app = Self {
            deployer: Deployer::new(&base),
            cfg,
            base,
            mods,
            load_order: None,
            selected: None,
            conflicts: Default::default(),
            job: None,
            install: None,
            tool_icons: Default::default(),
            queue: vec![],
            log,
            settings: SettingsForm::default(),
            dirty: false,
            rename: None,
            confirm_remove: None,
            confirm_unmanaged: false,
            tab: Tab::default(),
            mod_search: String::new(),
            plugin_search: String::new(),
        };
        match &app.cfg.game_dir {
            Some(g) => app.info(format!("Skyrim SE: {}", g.display())),
            None => {
                app.error("Skyrim Special Edition was not found. Set its folder in Settings.");
                app.open_settings();
            }
        }
        app.refresh();
        app
    }

    fn info(&mut self, s: impl Into<String>) {
        self.log.push((false, s.into()));
    }
    fn error(&mut self, s: impl Into<String>) {
        self.log.push((true, s.into()));
    }

    fn open_settings(&mut self) {
        let show = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        self.settings = SettingsForm {
            open: true,
            game_dir: show(&self.cfg.game_dir),
            appdata_dir: show(&self.cfg.appdata_dir),
            method: self.cfg.deploy_method,
            proton: show(&self.cfg.proton),
            runners: self.cfg.runners.clone(),
        };
    }

    /// Recompute conflicts and the plugin list after the mod list changed.
    fn refresh(&mut self) {
        self.conflicts = self.mods.conflicts();
        let (Some(game), Some(data), Some(appdata)) = (self.cfg.game_dir.clone(), self.cfg.data_path(), self.cfg.appdata_dir.clone())
        else {
            self.load_order = None;
            return;
        };
        let extra: Vec<(String, PathBuf)> = self
            .mods
            .mods
            .iter()
            .filter(|m| m.enabled)
            .flat_map(|m| {
                let d = self.mods.mod_dir(&m.name).join("data");
                installer::top_level_plugins(&d).into_iter().map(move |p| (p.clone(), d.join(&p)))
            })
            .collect();
        let mut lo = LoadOrder::load(&appdata, &data, &game, &extra);
        // Managed = placed by the current deployment, or provided by an enabled mod.
        let managed: std::collections::HashSet<String> = self
            .deployer
            .placed()
            .iter()
            .filter(|t| t.parent() == Some(data.as_path()))
            .filter_map(|t| t.file_name())
            .map(|n| n.to_string_lossy().to_lowercase())
            .chain(extra.iter().map(|(n, _)| n.to_lowercase()))
            .collect();
        lo.mark_unmanaged(&managed);
        self.load_order = Some(lo);
    }

    fn save_load_order(&mut self) {
        if let Some(lo) = &self.load_order
            && let Err(e) = lo.save() {
                self.error(format!("Saving Plugins.txt failed: {e:#}"));
            }
    }

    fn mods_changed(&mut self) {
        if let Err(e) = self.mods.save() {
            self.error(format!("Saving mod list failed: {e:#}"));
        }
        self.dirty = true;
        self.refresh();
        self.save_load_order();
    }

    fn game_env(&self) -> GameEnv {
        let mut roots: Vec<PathBuf> = self.cfg.data_path().into_iter().collect();
        roots.extend(self.mods.mods.iter().filter(|m| m.enabled).map(|m| self.mods.mod_dir(&m.name).join("data")));
        let active_plugins = self
            .load_order
            .as_ref()
            .map(|lo| lo.plugins.iter().filter(|p| p.enabled).map(|p| p.name.to_lowercase()).collect())
            .unwrap_or_default();
        GameEnv { roots, active_plugins }
    }

    // ------------------------------------------------------------- jobs ----

    fn start_install(&mut self, archive: PathBuf) {
        let temp = self.base.join("tmp").join(format!("{}-{}", mods::now(), rand_suffix()));
        let extracted = temp.join("x");
        let (tx, rx) = channel();
        let label = format!("Extracting {}", archive.file_name().unwrap_or_default().to_string_lossy());
        std::thread::spawn(move || {
            let r = skald::archive::extract(&archive, &extracted).and_then(|_| installer::detect(&extracted));
            let _ = tx.send(match r {
                Ok(kind) => JobResult::Extracted { archive, temp, extracted, kind },
                Err(e) => JobResult::Failed { what: format!("Installing {}", archive.display()), error: format!("{e:#}"), temp: Some(temp) },
            });
        });
        self.job = Some(Job { label, rx });
    }

    fn run_install(&mut self, req: InstallRequest) {
        let replace_at = if req.replace { self.mods.mods.iter().position(|m| m.name.eq_ignore_ascii_case(&req.name)) } else { None };
        let name = match replace_at {
            Some(i) => self.mods.mods[i].name.clone(),
            None => self.mods.unique_name(&req.name),
        };
        let mod_dir = self.mods.mod_dir(&name);
        let staging = self.base.join("tmp").join(format!("staging-{}", rand_suffix()));
        let (tx, rx) = channel();
        let label = format!("Installing {name}");
        std::thread::spawn(move || {
            let r = installer::execute(&req.ops, &staging).and_then(|n| {
                if mod_dir.exists() {
                    std::fs::remove_dir_all(&mod_dir)?;
                }
                std::fs::create_dir_all(mod_dir.parent().unwrap())?;
                if std::fs::rename(&staging, &mod_dir).is_err() {
                    skald::paths::copy_merge(&staging, &mod_dir, Path::new(""))?;
                }
                Ok(n)
            });
            let _ = tx.send(match r {
                Ok(files) => JobResult::Installed {
                    entry: ModEntry {
                        name,
                        enabled: true,
                        installer: req.kind_label.to_string(),
                        version: req.version,
                        author: req.author,
                        source: Some(req.source.display().to_string()),
                        installed_at: mods::now(),
                    },
                    replace_at,
                    files,
                    temp: req.temp,
                },
                Err(e) => JobResult::Failed { what: format!("Installing {name}"), error: format!("{e:#}"), temp: Some(req.temp) },
            });
        });
        self.job = Some(Job { label, rx });
    }

    fn run_deploy(&mut self, purge_only: bool) {
        let (Some(game), Some(data)) = (self.cfg.game_dir.clone(), self.cfg.data_path()) else {
            self.error("Set the game folder in Settings first.");
            return;
        };
        let mods = self.mods.clone();
        let deployer = Deployer::new(&self.base);
        let method = self.cfg.deploy_method;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let r = if purge_only { deployer.purge().map(JobResult::Purged) } else { deployer.deploy(&mods, &game, &data, method).map(JobResult::Deployed) };
            let _ = tx.send(r.unwrap_or_else(|e| JobResult::Failed {
                what: if purge_only { "Purge".into() } else { "Deploy".into() },
                error: format!("{e:#}"),
                temp: None,
            }));
        });
        self.job = Some(Job { label: if purge_only { "Purging…".into() } else { "Deploying…".into() }, rx });
    }

    /// Sorts the plugins with LOOT on a background thread (downloads the masterlist first).
    fn sort_plugins(&mut self) {
        let (Some(game_dir), Some(appdata_dir), Some(lo)) = (self.cfg.game_dir.clone(), self.cfg.appdata_dir.clone(), &self.load_order) else {
            self.error("Set the game folder in Settings first.");
            return;
        };
        let input = SortInput { game_dir, appdata_dir, plugins: lo.plugins.iter().map(|p| (p.name.clone(), p.path.clone())).collect() };
        let dir = self.base.join("loot");
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let r = loot::update_masterlist(&dir).and_then(|fresh| Ok(JobResult::Sorted { outcome: loot::sort(&input, &dir)?, fresh_masterlist: fresh }));
            let _ = tx.send(r.unwrap_or_else(|e| JobResult::Failed { what: "Sorting".into(), error: format!("{e:#}"), temp: None }));
        });
        self.job = Some(Job { label: "Sorting…".into(), rx });
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else {
            if self.install.is_none()
                && let Some(next) = (!self.queue.is_empty()).then(|| self.queue.remove(0)) {
                    self.start_install(next);
                }
            return;
        };
        let Ok(result) = job.rx.try_recv() else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        self.job = None;
        match result {
            JobResult::Extracted { archive, temp, extracted, kind } => {
                self.info(format!("Detected {} installer in {}", kind.label(), archive.display()));
                let session = Session::new(archive, temp, &extracted, kind, self.game_env());
                // Simple mods install straight away, unless the name clashes and we need to ask.
                let taken = self.mods.mods.iter().any(|m| m.name.eq_ignore_ascii_case(session.name.trim()));
                match session.quick_install().filter(|_| !taken) {
                    Some(req) => self.run_install(req),
                    None => self.install = Some(session),
                }
            }
            JobResult::Installed { entry, replace_at, files, temp } => {
                let _ = std::fs::remove_dir_all(temp);
                self.info(format!("Installed {} ({files} files)", entry.name));
                match replace_at {
                    Some(i) => {
                        let enabled = self.mods.mods[i].enabled;
                        self.mods.mods[i] = ModEntry { enabled, ..entry };
                        self.selected = Some(i);
                    }
                    None => {
                        self.mods.mods.push(entry);
                        self.selected = Some(self.mods.mods.len() - 1);
                    }
                }
                self.mods_changed();
            }
            JobResult::Deployed(r) => {
                self.info(format!("Deployed {} files ({} existing files backed up)", r.deployed, r.backed_up));
                for w in r.warnings {
                    self.error(w);
                }
                self.dirty = false;
                self.refresh();
                self.save_load_order();
            }
            JobResult::Purged(r) => {
                self.info(format!("Purged {} deployed files; game folder restored", r.deployed));
                for w in r.warnings {
                    self.error(w);
                }
                self.dirty = true;
                self.refresh();
            }
            JobResult::Sorted { outcome, fresh_masterlist } => {
                if !fresh_masterlist {
                    self.error("Couldn't download the LOOT masterlist; used the saved copy");
                }
                // Base game and Creation Club plugins can't be changed here, so skip their notes (mostly "needs cleaning").
                let locked: std::collections::HashSet<String> = self
                    .load_order
                    .iter()
                    .flat_map(|lo| &lo.plugins)
                    .filter(|p| p.locked)
                    .map(|p| p.name.to_lowercase())
                    .collect();
                for (plugin, level, text) in outcome.messages.into_iter().filter(|(p, ..)| !locked.contains(&p.to_lowercase())) {
                    let line = if plugin.is_empty() { format!("LOOT: {text}") } else { format!("LOOT: {plugin}: {text}") };
                    match level {
                        loot::Level::Error => self.error(line),
                        _ => self.info(line),
                    }
                }
                if let Some(lo) = &mut self.load_order {
                    lo.apply_order(&outcome.order);
                    self.info(format!("Sorted {} plugins with LOOT", outcome.order.len()));
                    self.save_load_order();
                }
            }
            JobResult::Failed { what, error, temp } => {
                if let Some(t) = temp {
                    let _ = std::fs::remove_dir_all(t);
                }
                self.error(format!("{what} failed: {error}"));
            }
        }
    }

    // --------------------------------------------------------------- ui ----

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let busy = self.job.is_some() || self.install.is_some();
        ui.horizontal(|ui| {
            // Tall buttons sit edge to edge, with no gap between them.
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.spacing_mut().interact_size.y = BAR_H;
            ui.spacing_mut().button_padding.x = 16.0;
            ui.add_enabled_ui(!busy, |ui| {
                self.play_controls(ui);
                let deploy = theme::accent_button("Deploy", theme::GREEN);
                if ui.add(deploy.min_size(egui::vec2(0.0, BAR_H))).on_hover_text("Link all enabled mods into the game folder").clicked() {
                    self.run_deploy(false);
                }
                if ui.add(theme::accent_button("Purge", theme::RED).min_size(egui::vec2(0.0, BAR_H))).on_hover_text("Remove deployed mods from the game folder").clicked() {
                    self.run_deploy(true);
                }
            });
            ui.add_space(16.0);
            self.tools_tray(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(!busy, egui::Button::new("⚙  Settings").min_size(egui::vec2(0.0, BAR_H))).clicked() {
                    self.open_settings();
                }
                ui.add_space(16.0);
                if let Some(job) = &self.job {
                    ui.label(RichText::new(&job.label).color(theme::TEXT_WEAK));
                    ui.add_space(8.0);
                    ui.spinner();
                } else if self.dirty {
                    theme::pill(ui, "●  CHANGES NOT DEPLOYED", theme::YELLOW);
                } else if self.deployer.is_deployed() {
                    theme::pill(ui, "✔  DEPLOYED", theme::GREEN);
                }
            });
        });
    }

    fn play_controls(&mut self, ui: &mut egui::Ui) {
        let current = launch::selected(&self.cfg);
        let hover = match &current {
            Runner::Steam => "Launch Skyrim SE through Steam".to_string(),
            Runner::Exe { exe, .. } => format!("Run {} with Proton", exe.display()),
        };
        let play = theme::accent_button(format!("Play {}", current.name()), theme::GOLD).min_size(egui::vec2(140.0, BAR_H));
        if ui.add(play).on_hover_text(hover).clicked() {
            self.play(&current);
        }
        let mut pick = None;
        let mut add = false;
        egui::ComboBox::from_id_salt("runner")
            .selected_text("")
            .width(22.0)
            .show_ui(ui, |ui| {
                for r in launch::runners(&self.cfg) {
                    if ui.selectable_label(r == current, r.name()).clicked() {
                        pick = Some(r);
                    }
                }
                ui.separator();
                if ui.button("Add program…").clicked() {
                    add = true;
                }
            })
            .response
            .on_hover_text("Choose what Play starts");
        if let Some(r) = pick {
            self.cfg.runner = (r != Runner::Steam).then(|| r.name().to_string());
            self.save_config();
        }
        if add {
            self.add_runner();
        }
    }

    /// One icon button per tool exe, plus a button to add another.
    fn tools_tray(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.spacing_mut().button_padding = egui::vec2(4.0, 4.0);
        let size = egui::vec2(BAR_H, BAR_H);
        let (mut run, mut remove) = (None, None);
        for (i, t) in self.cfg.tools.iter().enumerate() {
            let missing = !t.exe.is_file();
            if missing {
                // Not deployed yet: read the icon again once it appears.
                self.tool_icons.remove(&t.exe);
            }
            let icon = if missing { &mut None } else { self.tool_icons.entry(t.exe.clone()).or_insert_with(|| {
                skald::exeicon::extract(&t.exe, 32).map(|ic| {
                    let img = egui::ColorImage::from_rgba_unmultiplied([ic.width as usize, ic.height as usize], &ic.rgba);
                    ui.ctx().load_texture(format!("tool-{}", t.exe.display()), img, egui::TextureOptions::LINEAR)
                })
            }) };
            let button = match icon {
                Some(tex) => egui::Button::image(egui::Image::new(&*tex).fit_to_exact_size(egui::vec2(28.0, 28.0))),
                None => {
                    let letter = t.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                    egui::Button::new(RichText::new(letter).size(18.0).strong().color(theme::FROST))
                }
            };
            let mut hover = format!("{}\n{}", t.name, t.exe.display());
            if missing {
                hover.push_str("\n\nNot found. Deploy first if it comes from a mod.");
            }
            let resp = ui.add(button.frame_when_inactive(false).min_size(size)).on_hover_text(hover);
            if missing {
                ui.painter().line_segment([resp.rect.left_bottom(), resp.rect.right_top()], egui::Stroke::new(1.5, theme::RED));
            }
            if resp.clicked() {
                run = Some(i);
            }
            resp.context_menu(|ui| {
                if ui.button("Remove from tray").clicked() {
                    remove = Some(i);
                }
            });
        }
        if ui.add(egui::Button::new(RichText::new("+").size(18.0).color(theme::TEXT_WEAK)).frame_when_inactive(false).min_size(size)).on_hover_text("Add a program to the tray").clicked() {
            self.add_tool();
        }
        if let Some(i) = run {
            let t = &self.cfg.tools[i];
            let runner = Runner::Exe { name: t.name.clone(), exe: t.exe.clone() };
            match launch::launch(&self.cfg, &runner) {
                Ok(()) => self.info(format!("Started {}", runner.name())),
                Err(e) => self.error(format!("Couldn't start {}: {e:#}", runner.name())),
            }
        }
        if let Some(i) = remove {
            let t = self.cfg.tools.remove(i);
            self.info(format!("Removed {} from the tray", t.name));
            self.save_config();
        }
    }

    fn add_tool(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Choose a program for the tray").add_filter("Windows program", &["exe", "EXE"]);
        if let Some(d) = self.cfg.data_path().or(self.cfg.game_dir.clone()) {
            dialog = dialog.set_directory(d);
        }
        let Some(exe) = dialog.pick_file() else { return };
        if self.cfg.tools.iter().any(|t| t.exe == exe) {
            self.info(format!("{} is already in the tray", exe.display()));
            return;
        }
        let name = exe.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Program".into());
        self.info(format!("Added {name} to the tray"));
        self.cfg.tools.push(CustomRunner { name, exe });
        self.save_config();
    }

    fn play(&mut self, runner: &Runner) {
        if matches!(runner, Runner::Exe { .. }) && !launch::steam_running() {
            self.error("Steam doesn't seem to be running. Start it first, or the game will close right away.");
        }
        match launch::launch(&self.cfg, runner) {
            Ok(()) => self.info(format!("Started {}", runner.name())),
            Err(e) => self.error(format!("Couldn't start {}: {e:#}", runner.name())),
        }
    }

    fn add_runner(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Choose a program to run with Proton").add_filter("Windows program", &["exe", "EXE"]);
        if let Some(g) = &self.cfg.game_dir {
            dialog = dialog.set_directory(g);
        }
        let Some(exe) = dialog.pick_file() else { return };
        let stem = exe.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Program".into());
        let taken = |n: &str| launch::runners(&self.cfg).iter().any(|r| r.name() == n);
        let mut name = stem.clone();
        let mut i = 2;
        while taken(&name) {
            name = format!("{stem} ({i})");
            i += 1;
        }
        self.info(format!("Added {name} to the Play menu"));
        self.cfg.runners.push(CustomRunner { name: name.clone(), exe });
        self.cfg.runner = Some(name);
        self.save_config();
    }

    fn save_config(&mut self) {
        if let Err(e) = self.cfg.save() {
            self.error(format!("Saving settings failed: {e:#}"));
        }
    }

    fn mod_list(&mut self, ui: &mut egui::Ui) {
        if self.mods.mods.is_empty() {
            empty_state(ui, "📦", "No mods installed", "Drag mod archives (.zip, .7z, .rar) or extracted folders onto this window.");
            return;
        }
        let busy = self.job.is_some();
        let mut changed = false;
        let mut action: Option<(usize, &'static str)> = None;
        let n = self.mods.mods.len();
        let filtering = !self.mod_search.trim().is_empty();
        let shown: Vec<usize> = (0..n).filter(|&i| matches_search(&self.mods.mods[i].name, &self.mod_search)).collect();
        if shown.is_empty() {
            empty_state(ui, "🔍", "No matching mods", "Nothing in the mod list matches the search.");
            return;
        }
        let (mods, conflicts, selected) = (&mut self.mods.mods, &self.conflicts, &mut self.selected);
        // Drag-to-reorder: the dragged row index lives in egui memory; the drop slot is found while drawing rows.
        let ctx = ui.ctx().clone();
        let drag_id = ui.scope_id().with("mods_drag_from");
        let can_drag = !busy && !filtering;
        let mut drop_to: Option<usize> = None;
        let painter = ui.painter().clone();
        egui_extras::TableBuilder::new(ui)
            .id_salt("mods")
            .striped(true)
            .sense(egui::Sense::click_and_drag())
            .auto_shrink([false, false])
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(26.0))
            .column(Column::exact(30.0))
            .column(Column::remainder().at_least(180.0).clip(true))
            .column(Column::initial(110.0).at_least(70.0).resizable(true))
            .column(Column::initial(90.0).at_least(50.0).resizable(true))
            .column(Column::initial(90.0).at_least(70.0))
            .column(Column::exact(52.0))
            .header(26.0, |mut h| {
                for t in ["", "#", "NAME", "INSTALLER", "VERSION", "CONFLICTS", ""] {
                    h.col(|ui| {
                        ui.label(RichText::new(t).small().strong().color(theme::TEXT_WEAK));
                    });
                }
            })
            .body(|body| {
                body.rows(ROW_H, shown.len(), |mut row| {
                    let i = shown[row.index()];
                    let m = &mut mods[i];
                    row.set_selected(*selected == Some(i));
                    row.col(|ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            if ui.checkbox(&mut m.enabled, "").changed() {
                                changed = true;
                            }
                        });
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(format!("{i}")).monospace().color(theme::TEXT_WEAK));
                    });
                    row.col(|ui| {
                        let t = RichText::new(&m.name);
                        ui.add(egui::Label::new(if m.enabled { t.color(theme::TEXT) } else { t.color(theme::TEXT_WEAK) }).selectable(false).truncate());
                    });
                    row.col(|ui| {
                        if !m.installer.is_empty() {
                            theme::pill(ui, m.installer.to_uppercase(), installer_color(&m.installer));
                        }
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(m.version.as_deref().unwrap_or("")).color(theme::TEXT_WEAK));
                    });
                    row.col(|ui| {
                        if let Some((over, under)) = conflicts.get(&m.name) {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if *over > 0 {
                                ui.label(RichText::new(format!("▲{over}")).color(theme::GREEN))
                                    .on_hover_text(format!("Overrides {over} files from mods above"));
                            }
                            if *under > 0 {
                                ui.label(RichText::new(format!("▼{under}")).color(theme::RED))
                                    .on_hover_text(format!("{under} files overridden by mods below"));
                            }
                        }
                    });
                    row.col(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        // Moving while filtered would swap with hidden neighbours, so it waits for a clear search.
                        let hint = if filtering { "Clear the search to reorder" } else { "" };
                        ui.add_enabled_ui(!busy && !filtering, |ui| {
                            if ui.add_enabled(i > 0, theme::icon_button("⏶")).on_hover_text("Move up").on_disabled_hover_text(hint).clicked() {
                                action = Some((i, "up"));
                            }
                            if ui.add_enabled(i + 1 < n, theme::icon_button("⏷")).on_hover_text("Move down").on_disabled_hover_text(hint).clicked() {
                                action = Some((i, "down"));
                            }
                        });
                    });
                    let r = row.response();
                    if can_drag && r.drag_started_by(egui::PointerButton::Primary) {
                        ctx.data_mut(|d| d.insert_temp(drag_id, i));
                        *selected = Some(i);
                    }
                    if let Some(from) = ctx.data(|d| d.get_temp::<usize>(drag_id))
                        && let Some(pos) = ctx.pointer_interact_pos()
                        && r.rect.y_range().contains(pos.y)
                    {
                        drop_to = Some(i);
                        let y = if i >= from { r.rect.bottom() } else { r.rect.top() };
                        painter.hline(r.rect.x_range(), y, egui::Stroke::new(2.0, theme::FROST));
                    }
                    if r.clicked() || r.secondary_clicked() {
                        *selected = Some(i);
                    }
                    r.context_menu(|ui| {
                        if ui.button("✏  Rename…").clicked() {
                            action = Some((i, "rename"));
                            ui.close();
                        }
                        if ui.button("📂  Open folder").clicked() {
                            action = Some((i, "open"));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button(RichText::new("🗑  Remove").color(theme::RED)).clicked() {
                            action = Some((i, "remove"));
                            ui.close();
                        }
                    });
                });
            });
        if let Some(from) = ctx.data(|d| d.get_temp::<usize>(drag_id)) {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            if ctx.input(|i| i.pointer.any_released()) {
                ctx.data_mut(|d| d.remove::<usize>(drag_id));
                if let Some(to) = drop_to
                    && to != from
                    && to < self.mods.mods.len()
                {
                    let m = self.mods.mods.remove(from);
                    self.mods.mods.insert(to, m);
                    self.selected = Some(to);
                    changed = true;
                }
            }
        }
        match action {
            Some((i, "up")) => {
                self.mods.mods.swap(i, i - 1);
                self.selected = Some(i - 1);
                changed = true;
            }
            Some((i, "down")) => {
                self.mods.mods.swap(i, i + 1);
                self.selected = Some(i + 1);
                changed = true;
            }
            Some((i, "rename")) => self.rename = Some((i, self.mods.mods[i].name.clone())),
            Some((i, "open")) => {
                let _ = std::process::Command::new("xdg-open").arg(self.mods.mod_dir(&self.mods.mods[i].name)).spawn();
            }
            Some((i, "remove")) => self.confirm_remove = Some(i),
            _ => {}
        }
        if changed {
            self.mods_changed();
        }
    }

    /// Tab strip above the main area, with a summary of the open list on the right.
    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let enabled = self.mods.mods.iter().filter(|m| m.enabled).count();
        let (active, light) = self.load_order.as_ref().map_or((0, 0), |lo| {
            (lo.plugins.iter().filter(|p| p.enabled).count(), lo.plugins.iter().filter(|p| p.enabled && p.light).count())
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (tab, label, count) in [(Tab::Mods, "Mods", enabled), (Tab::Plugins, "Plugins", active)] {
                if theme::tab(ui, self.tab == tab, label, count).clicked() {
                    self.tab = tab;
                }
            }
            ui.add_space(12.0);
            let (query, hint) = match self.tab {
                Tab::Mods => (&mut self.mod_search, "🔍 Search mods  (Ctrl+F)"),
                Tab::Plugins => (&mut self.plugin_search, "🔍 Search plugins  (Ctrl+F)"),
            };
            let r = ui.add(egui::TextEdit::singleline(query).id_salt("list_search").hint_text(hint).desired_width(220.0));
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
                r.request_focus();
            }
            if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                query.clear();
            }
            if !query.is_empty() {
                ui.spacing_mut().item_spacing.x = 2.0;
                if ui.add(theme::icon_button("✖")).on_hover_text("Clear search").clicked() {
                    query.clear();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let usable = match self.tab {
                    Tab::Mods => self.job.is_none() && !self.mods.mods.is_empty(),
                    Tab::Plugins => self.load_order.as_ref().is_some_and(|lo| lo.plugins.iter().any(|p| !p.locked)),
                };
                let mut set_all = None;
                let (on_label, off_label) = if self.search().trim().is_empty() { ("Enable all", "Disable all") } else { ("Enable matches", "Disable matches") };
                if ui.add_enabled(usable, egui::Button::new(RichText::new(off_label).small())).clicked() {
                    set_all = Some(false);
                }
                if ui.add_enabled(usable, egui::Button::new(RichText::new(on_label).small())).clicked() {
                    set_all = Some(true);
                }
                if let Some(on) = set_all {
                    self.set_all_enabled(on);
                }
                if self.tab == Tab::Plugins {
                    let unmanaged = self.unmanaged_plugins().len();
                    if ui
                        .add_enabled(unmanaged > 0 && self.job.is_none(), egui::Button::new(RichText::new(format!("Remove unmanaged ({unmanaged})")).small()))
                        .on_hover_text("Move plugins that Skald didn't install out of the Data folder")
                        .clicked()
                    {
                        self.confirm_unmanaged = true;
                    }
                    if ui
                        .add_enabled(usable && self.job.is_none(), egui::Button::new(RichText::new("Sort (LOOT)").small()))
                        .on_hover_text("Sort plugins using LOOT's masterlist (downloads the latest one)")
                        .clicked()
                    {
                        self.sort_plugins();
                    }
                }
                ui.add_space(8.0);
                let summary = match self.tab {
                    Tab::Mods => format!("{enabled} of {} enabled  ·  lower in the list wins conflicts", self.mods.mods.len()),
                    Tab::Plugins if self.load_order.is_some() => format!("{active} active  ·  {} full, {light} light", active - light),
                    Tab::Plugins => String::new(),
                };
                ui.label(RichText::new(summary).color(theme::TEXT_WEAK).small());
            });
        });
        let r = ui.min_rect();
        ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, theme::BORDER));
        ui.add_space(8.0);
    }

    fn unmanaged_plugins(&self) -> Vec<String> {
        self.load_order.as_ref().map(|lo| lo.plugins.iter().filter(|p| p.unmanaged).map(|p| p.name.clone()).collect()).unwrap_or_default()
    }

    /// Moves unmanaged plugins from `Data` to `unmanaged/<time>/` and drops them from the load order.
    fn remove_unmanaged(&mut self) {
        let Some(data) = self.cfg.data_path() else { return };
        let names = self.unmanaged_plugins();
        match skald::plugins::move_out(&data, &names, &self.base.join("unmanaged")) {
            Ok((dest, moved)) => self.info(format!("Moved {} unmanaged plugins to {}: {}", moved.len(), dest.display(), moved.join(", "))),
            Err(e) => self.error(format!("Removing unmanaged plugins failed: {e:#}")),
        }
        self.refresh();
        self.save_load_order();
    }

    /// Search text of the open tab.
    fn search(&self) -> &str {
        match self.tab {
            Tab::Mods => &self.mod_search,
            Tab::Plugins => &self.plugin_search,
        }
    }

    /// Enables or disables every entry in the open tab that matches the search (all of them when it is empty).
    /// Base game plugins stay as they are.
    fn set_all_enabled(&mut self, on: bool) {
        match self.tab {
            Tab::Mods => {
                let q = &self.mod_search;
                let mut hit = self.mods.mods.iter_mut().filter(|m| matches_search(&m.name, q) && m.enabled != on).peekable();
                if hit.peek().is_some() {
                    hit.for_each(|m| m.enabled = on);
                    self.mods_changed();
                }
            }
            Tab::Plugins => {
                if let Some(lo) = &mut self.load_order {
                    let q = &self.plugin_search;
                    lo.plugins.iter_mut().filter(|p| !p.locked && matches_search(&p.name, q)).for_each(|p| p.enabled = on);
                }
                self.save_load_order();
            }
        }
    }

    fn plugin_list(&mut self, ui: &mut egui::Ui) {
        let Some(lo) = &mut self.load_order else {
            empty_state(ui, "🧩", "No plugin list", "Set the game folder and Proton prefix in Settings to manage plugins.");
            return;
        };
        let mut changed = false;
        let mut mv: Option<(usize, bool)> = None;
        // Locked plugins are always sorted first, so the two groups are contiguous.
        let split = lo.plugins.iter().take_while(|p| p.locked).count();
        let n = lo.plugins.len();
        let q = &self.plugin_search;
        let filtering = !q.trim().is_empty();
        let pick = |r: std::ops::Range<usize>| -> Vec<usize> { r.filter(|&i| matches_search(&lo.plugins[i].name, q)).collect() };
        let groups = [("plugins_base", "Base game & DLC", pick(0..split), false), ("plugins_mods", "Mods", pick(split..n), true)];
        egui::ScrollArea::vertical().id_salt("plugins_scroll").auto_shrink([false, false]).show(ui, |ui| {
            for (salt, title, rows, open) in groups {
                let active = rows.iter().filter(|&&i| lo.plugins[i].enabled).count();
                let header = format!("{title}  ({active}/{})", rows.len());
                // While searching, groups with matches open by themselves so results are never hidden.
                egui::CollapsingHeader::new(RichText::new(header).strong().color(theme::TEXT))
                    .id_salt(salt)
                    .default_open(open)
                    .open(filtering.then_some(!rows.is_empty()))
                    .show_unindented(ui, |ui| plugin_table(ui, lo, salt, &rows, filtering, &mut changed, &mut mv));
            }
        });
        if let Some((i, up)) = mv {
            lo.move_by(i, up);
            changed = true;
        }
        if changed {
            self.save_load_order();
        }
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let Some(m) = self.selected.and_then(|i| self.mods.mods.get(i)) else {
            ui.label(RichText::new("Select a mod to see its details.").color(theme::TEXT_WEAK));
            return;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(&m.name).size(16.0).strong().color(theme::TEXT));
            if let Some(v) = &m.version {
                ui.label(RichText::new(format!("v{v}")).color(theme::TEXT_WEAK));
            }
            if let Some(a) = &m.author {
                ui.label(RichText::new(format!("by {a}")).color(theme::TEXT_WEAK));
            }
            theme::pill(ui, m.installer.to_uppercase(), installer_color(&m.installer));
            if !m.enabled {
                theme::pill(ui, "DISABLED", theme::TEXT_WEAK);
            }
        });
        if let Some(s) = &m.source {
            ui.add(egui::Label::new(RichText::new(s).small().color(theme::TEXT_WEAK)).truncate());
        }
    }

    fn log_view(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (err, line) in &self.log {
                ui.horizontal(|ui| {
                    let (icon, color, text) = if *err { ("✖", theme::RED, theme::RED) } else { ("›", theme::FROST, theme::TEXT_WEAK) };
                    ui.label(RichText::new(icon).monospace().color(color));
                    ui.add(egui::Label::new(RichText::new(line).monospace().color(text)).wrap());
                });
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        // Install wizard.
        if let Some(session) = &mut self.install {
            let wanted = session.name.trim().to_string();
            let taken = self.mods.mods.iter().any(|m| m.name.eq_ignore_ascii_case(&wanted));
            match session.show(ctx, taken) {
                Outcome::Pending => {}
                Outcome::Cancel => {
                    let s = self.install.take().unwrap();
                    let _ = std::fs::remove_dir_all(&s.temp);
                    self.info("Installation cancelled");
                }
                Outcome::Install(req) => {
                    self.install = None;
                    self.run_install(req);
                }
            }
        }

        if let Some(job) = &self.job
            && self.install.is_none() {
                egui::Modal::new(egui::Id::unique("busy")).show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.add_space(6.0);
                        ui.label(RichText::new(&job.label).size(15.0));
                    });
                });
            }

        if let Some((i, name)) = &mut self.rename {
            let mut close = false;
            let mut apply = None;
            egui::Modal::new(egui::Id::unique("rename")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.heading("Rename mod");
                ui.add_space(4.0);
                let r = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
                ui.add_space(8.0);
                r.request_focus();
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui.add(theme::accent_button("Rename", theme::FROST)).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                        apply = Some((*i, name.clone()));
                    }
                });
            });
            if let Some((i, n)) = apply {
                match self.mods.rename(i, &n) {
                    Ok(()) => self.mods_changed(),
                    Err(e) => self.error(format!("Rename failed: {e:#}")),
                }
                close = true;
            }
            if close {
                self.rename = None;
            }
        }

        if let Some(i) = self.confirm_remove {
            let mut close = false;
            let name = self.mods.mods.get(i).map(|m| m.name.clone()).unwrap_or_default();
            egui::Modal::new(egui::Id::unique("remove")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.heading("Remove mod");
                ui.add_space(4.0);
                ui.label(format!("Delete “{name}” and its files? This can't be undone."));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui.add(theme::accent_button("Remove", theme::RED)).clicked() {
                        match self.mods.remove(i) {
                            Ok(()) => {
                                self.info(format!("Removed {name}"));
                                self.selected = None;
                                self.mods_changed();
                            }
                            Err(e) => self.error(format!("Remove failed: {e:#}")),
                        }
                        close = true;
                    }
                });
            });
            if close {
                self.confirm_remove = None;
            }
        }

        if self.confirm_unmanaged {
            let mut close = false;
            let names = self.unmanaged_plugins();
            let dest = self.base.join("unmanaged");
            egui::Modal::new(egui::Id::unique("remove_unmanaged")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading("Remove unmanaged plugins");
                ui.add_space(4.0);
                ui.label("These plugins are in the Data folder but weren't installed by Skald:");
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    for n in &names {
                        ui.label(RichText::new(format!("•  {n}")).color(theme::YELLOW));
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new(format!("They'll be moved to {}, not deleted. Other files from those mods (meshes, textures, archives) stay in Data.", dest.display())).color(theme::TEXT_WEAK));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui.add(theme::accent_button("Remove", theme::RED)).clicked() {
                        self.remove_unmanaged();
                        close = true;
                    }
                });
            });
            if close {
                self.confirm_unmanaged = false;
            }
        }

        if self.settings.open {
            self.settings_dialog(ctx);
        }
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let mut save = false;
        let mods_dir = self.mods.mods_dir();
        let f = &mut self.settings;
        egui::Modal::new(egui::Id::unique("settings")).show(ctx, |ui| {
            ui.set_width(640.0);
            ui.heading("Settings");
            ui.add_space(10.0);
            egui::Grid::new("settings_grid").num_columns(3).spacing([8.0, 8.0]).show(ui, |ui| {
                ui.label(RichText::new("Skyrim SE folder").color(theme::TEXT_WEAK));
                ui.add(egui::TextEdit::singleline(&mut f.game_dir).desired_width(420.0));
                if ui.button("Browse…").clicked()
                    && let Some(d) = rfd::FileDialog::new().set_title("Folder containing SkyrimSE.exe").pick_folder() {
                        f.game_dir = d.display().to_string();
                        if f.appdata_dir.is_empty()
                            && let Some(a) = config::appdata_from_game_dir(&d) {
                                f.appdata_dir = a.display().to_string();
                            }
                    }
                ui.end_row();
                ui.label(RichText::new("Plugins.txt folder").color(theme::TEXT_WEAK));
                ui.add(egui::TextEdit::singleline(&mut f.appdata_dir).desired_width(420.0))
                    .on_hover_text(".../compatdata/489830/pfx/drive_c/users/steamuser/AppData/Local/Skyrim Special Edition");
                if ui.button("Browse…").clicked()
                    && let Some(d) = rfd::FileDialog::new().set_title("Proton prefix AppData/Local/Skyrim Special Edition").pick_folder() {
                        f.appdata_dir = d.display().to_string();
                    }
                ui.end_row();
                ui.label(RichText::new("Mods folder").color(theme::TEXT_WEAK));
                ui.label(RichText::new(mods_dir.display().to_string()).weak());
                if ui.button("Open").on_hover_text("Open the folder where Skald keeps installed mods").clicked() {
                    let _ = std::process::Command::new("xdg-open").arg(&mods_dir).spawn();
                }
                ui.end_row();
                ui.label(RichText::new("Deploy method").color(theme::TEXT_WEAK));
                egui::ComboBox::from_id_salt("method")
                    .selected_text(format!("{:?}", f.method))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut f.method, DeployMethod::Hardlink, "Hardlink (recommended)");
                        ui.selectable_value(&mut f.method, DeployMethod::Symlink, "Symlink");
                        ui.selectable_value(&mut f.method, DeployMethod::Copy, "Copy");
                    });
                ui.end_row();
                ui.label(RichText::new("Proton").color(theme::TEXT_WEAK));
                ui.add(egui::TextEdit::singleline(&mut f.proton).desired_width(420.0).hint_text("Automatic (the one Steam uses for Skyrim)"))
                    .on_hover_text("Used when Play starts something other than Steam, e.g. SKSE");
                ui.menu_button("Choose…", |ui| {
                    for p in launch::installed_protons() {
                        let label = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        if ui.button(label).clicked() {
                            f.proton = p.display().to_string();
                        }
                    }
                    if ui.button("Automatic").clicked() {
                        f.proton.clear();
                    }
                });
                ui.end_row();
                if !f.runners.is_empty() {
                    ui.label(RichText::new("Play menu programs").color(theme::TEXT_WEAK));
                    ui.vertical(|ui| {
                        let mut remove = None;
                        for (i, r) in f.runners.iter().enumerate() {
                            ui.horizontal(|ui| {
                                if ui.small_button("✖").on_hover_text("Remove from the Play menu").clicked() {
                                    remove = Some(i);
                                }
                                ui.label(&r.name);
                                ui.label(RichText::new(r.exe.display().to_string()).weak().small());
                            });
                        }
                        if let Some(i) = remove {
                            f.runners.remove(i);
                        }
                    });
                    ui.end_row();
                }
            });
            let proton = f.proton.trim();
            if !proton.is_empty() && !Path::new(proton).join("proton").is_file() {
                ui.colored_label(theme::YELLOW, "No `proton` script in that Proton folder.");
            }
            let valid = Config::is_valid_game_dir(Path::new(&f.game_dir));
            if !valid {
                ui.colored_label(theme::YELLOW, "SkyrimSE.exe not found in that folder.");
            }
            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Auto-detect").clicked()
                    && let Some(d) = config::detect() {
                        f.game_dir = d.game_dir.display().to_string();
                        f.appdata_dir = d.appdata_dir.map(|p| p.display().to_string()).unwrap_or_default();
                    }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui.add_enabled(valid, theme::accent_button("Save", theme::FROST)).clicked() {
                    save = true;
                }
            });
        });
        if save {
            let f = &self.settings;
            let method_changed = self.cfg.deploy_method != f.method;
            self.cfg.game_dir = Some(PathBuf::from(f.game_dir.trim()));
            self.cfg.appdata_dir = Some(f.appdata_dir.trim()).filter(|s| !s.is_empty()).map(PathBuf::from);
            self.cfg.deploy_method = f.method;
            self.cfg.proton = Some(f.proton.trim()).filter(|s| !s.is_empty()).map(PathBuf::from);
            self.cfg.runners = f.runners.clone();
            match self.cfg.save() {
                Ok(()) => self.info("Settings saved"),
                Err(e) => self.error(format!("Saving settings failed: {e:#}")),
            }
            if method_changed && self.deployer.is_deployed() {
                self.dirty = true;
            }
            self.refresh();
            close = true;
        }
        if close {
            self.settings.open = false;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_job(&ctx);

        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.queue.extend(dropped);
        }

        // No margin, so the buttons sit flush against the edges of the bar.
        let top_frame = egui::Frame::NONE.fill(theme::BG_DEEP).stroke(egui::Stroke::new(1.0, theme::BORDER));
        egui::Panel::top("top").frame(top_frame).show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("log").resizable(true).default_size(150.0).frame(theme::panel_frame(theme::BG_DEEP)).show(ui, |ui| {
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.details(ui);
            });
            ui.add_space(6.0);
            self.log_view(ui);
        });
        egui::CentralPanel::default().frame(theme::panel_frame(theme::PANEL)).show(ui, |ui| {
            self.tab_bar(ui);
            match self.tab {
                Tab::Mods => self.mod_list(ui),
                Tab::Plugins => self.plugin_list(ui),
            }
        });

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            drop_overlay(&ctx);
        }
        self.dialogs(&ctx);
    }
}

/// Colour for an installer-type tag.
fn installer_color(kind: &str) -> Color32 {
    match kind {
        k if k.starts_with("FOMOD") => theme::FROST,
        "BAIN" => theme::PURPLE,
        "Game root" => theme::GOLD,
        "Simple" => theme::GREEN,
        _ => theme::TEXT_WEAK,
    }
}

fn empty_state(ui: &mut egui::Ui, icon: &str, title: &str, hint: &str) {
    ui.add_space(ui.available_height() * 0.25);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(icon).size(42.0).color(theme::TEXT_WEAK));
        ui.add_space(6.0);
        ui.label(RichText::new(title).size(18.0).strong().color(theme::TEXT));
        ui.label(RichText::new(hint).color(theme::TEXT_WEAK));
    });
}

/// Full-window highlight while files are dragged over Skald.
fn drop_overlay(ctx: &egui::Context) {
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop_overlay")));
    painter.rect_filled(screen, 0.0, Color32::from_black_alpha(200));
    painter.rect_stroke(screen.shrink(16.0), 0.0, egui::Stroke::new(2.0, theme::FROST), egui::StrokeKind::Inside);
    let c = screen.center();
    painter.text(c - egui::vec2(0.0, 18.0), egui::Align2::CENTER_CENTER, "⬇  Drop to install", egui::FontId::proportional(28.0), theme::FROST);
    painter.text(
        c + egui::vec2(0.0, 18.0),
        egui::Align2::CENTER_CENTER,
        "Archives (.zip, .7z, .rar) or extracted mod folders",
        egui::FontId::proportional(14.0),
        theme::TEXT_WEAK,
    );
    ctx.request_repaint();
}

fn rand_suffix() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(mods::now());
    format!("{:x}", h.finish() & 0xffff_ffff)
}

/// Case-insensitive substring match of a list entry against the search text (an empty search matches everything).
fn matches_search(name: &str, query: &str) -> bool {
    let q = query.trim();
    q.is_empty() || name.to_lowercase().contains(&q.to_lowercase())
}

/// One plugin group (`rows`: indices into the load order) as a table without its own scroll area.
/// Reordering is off while `filtering`, since a move would swap with a hidden neighbour.
fn plugin_table(ui: &mut egui::Ui, lo: &mut LoadOrder, salt: &str, rows: &[usize], filtering: bool, changed: &mut bool, mv: &mut Option<(usize, bool)>) {
    if rows.is_empty() {
        ui.label(RichText::new(if filtering { "No matching plugins." } else { "No plugins." }).color(theme::TEXT_WEAK));
        return;
    }
    egui_extras::TableBuilder::new(ui)
        .id_salt(salt)
        .striped(true)
        .vscroll(false)
        .auto_shrink([false, true])
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::exact(26.0))
        .column(Column::remainder().at_least(120.0).clip(true))
        .column(Column::exact(160.0))
        .column(Column::exact(52.0))
        .body(|body| {
            body.rows(ROW_H, rows.len(), |mut row| {
                let i = rows[row.index()];
                let (up, down) = (!filtering && lo.can_move(i, true), !filtering && lo.can_move(i, false));
                let p = &mut lo.plugins[i];
                row.col(|ui| {
                    ui.add_enabled_ui(!p.locked, |ui| {
                        if ui.checkbox(&mut p.enabled, "").changed() {
                            *changed = true;
                        }
                    });
                });
                row.col(|ui| {
                    let color = if p.locked {
                        theme::TEXT_WEAK
                    } else if !p.enabled {
                        theme::TEXT_WEAK.gamma_multiply(0.8)
                    } else if p.master {
                        theme::FROST
                    } else {
                        theme::TEXT
                    };
                    let label = ui.add(egui::Label::new(RichText::new(&p.name).color(color)).truncate());
                    if p.locked {
                        label.on_hover_text("Base game plugin, always loaded first");
                    } else if p.unmanaged {
                        label.on_hover_text("Not installed by Skald: left over from a manual install or another mod manager");
                    }
                });
                row.col(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if p.master {
                        theme::pill(ui, "ESM", theme::FROST);
                    }
                    if p.light {
                        theme::pill(ui, "ESL", theme::PURPLE);
                    }
                    if p.unmanaged {
                        theme::pill(ui, "UNMANAGED", theme::YELLOW)
                            .on_hover_text("Not installed by Skald: left over from a manual install or another mod manager");
                    }
                });
                row.col(|ui| {
                    if p.locked {
                        return;
                    }
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let hint = if filtering { "Clear the search to reorder" } else { "" };
                    if ui.add_enabled(up, theme::icon_button("⏶")).on_disabled_hover_text(hint).clicked() {
                        *mv = Some((i, true));
                    }
                    if ui.add_enabled(down, theme::icon_button("⏷")).on_disabled_hover_text(hint).clicked() {
                        *mv = Some((i, false));
                    }
                });
            });
        });
}
