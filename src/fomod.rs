//! FOMOD (`fomod/ModuleConfig.xml`) parsing, condition evaluation and a
//! UI-independent wizard state machine.
//!
//! Implements the XML schema used by Nexus/MO2/Vortex (ModuleConfig 5.x):
//! install steps, groups, plugin type descriptors, condition flags,
//! visibility conditions, conditional file installs and priorities.

use anyhow::{Context, Result, anyhow};
use roxmltree::{Document, Node};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- model ----

#[derive(Debug, Clone, Default)]
pub struct ModuleConfig {
    pub name: String,
    pub image: Option<String>,
    pub module_dependencies: Option<Dependency>,
    pub required_files: Vec<FileItem>,
    pub steps: Vec<Step>,
    pub conditional_installs: Vec<ConditionalInstall>,
}

#[derive(Debug, Clone, Default)]
pub struct ModuleInfo {
    pub name: Option<String>,
    pub author: Option<String>,
    pub version: Option<String>,
    pub website: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileItem {
    pub source: String,
    /// `None` when the attribute was omitted (defaults to the source path).
    pub destination: Option<String>,
    pub is_folder: bool,
    pub priority: i32,
    pub always_install: bool,
    pub install_if_usable: bool,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub name: String,
    pub visible: Option<Dependency>,
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupType {
    SelectExactlyOne,
    SelectAtMostOne,
    SelectAtLeastOne,
    SelectAll,
    SelectAny,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub name: String,
    pub kind: GroupType,
    pub plugins: Vec<Plugin>,
}

#[derive(Debug, Clone)]
pub struct Plugin {
    pub name: String,
    pub description: String,
    pub image: Option<String>,
    pub files: Vec<FileItem>,
    pub flags: Vec<(String, String)>,
    pub type_descriptor: TypeDescriptor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginType {
    Required,
    Optional,
    Recommended,
    NotUsable,
    CouldBeUsable,
}

#[derive(Debug, Clone)]
pub enum TypeDescriptor {
    Simple(PluginType),
    Dependent { default: PluginType, patterns: Vec<(Dependency, PluginType)> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Missing,
    Inactive,
    Active,
}

#[derive(Debug, Clone)]
pub enum Dependency {
    Composite { op: Operator, children: Vec<Dependency> },
    File { file: String, state: FileState },
    Flag { flag: String, value: String },
    /// Game / mod-manager / script-extender version checks. We are always
    /// "new enough", so these evaluate to true.
    Version,
}

#[derive(Debug, Clone)]
pub struct ConditionalInstall {
    pub dependency: Dependency,
    pub files: Vec<FileItem>,
}

// ----------------------------------------------------------- environment ----

/// What the condition evaluator needs to know about the user's setup.
pub trait Environment {
    fn file_state(&self, file: &str) -> FileState;
}

/// Environment that reports every file as missing (useful for tests).
pub struct NoFiles;
impl Environment for NoFiles {
    fn file_state(&self, _: &str) -> FileState {
        FileState::Missing
    }
}

pub type Flags = HashMap<String, String>;

impl Dependency {
    pub fn eval(&self, flags: &Flags, env: &dyn Environment) -> bool {
        match self {
            Dependency::Composite { op, children } => match op {
                Operator::And => children.iter().all(|c| c.eval(flags, env)),
                Operator::Or => children.is_empty() || children.iter().any(|c| c.eval(flags, env)),
            },
            Dependency::File { file, state } => env.file_state(file) == *state,
            Dependency::Flag { flag, value } => {
                flags.get(flag).map(String::as_str).unwrap_or("") == value
            }
            Dependency::Version => true,
        }
    }
}

impl TypeDescriptor {
    pub fn resolve(&self, flags: &Flags, env: &dyn Environment) -> PluginType {
        match self {
            TypeDescriptor::Simple(t) => *t,
            TypeDescriptor::Dependent { default, patterns } => patterns
                .iter()
                .find(|(d, _)| d.eval(flags, env))
                .map(|(_, t)| *t)
                .unwrap_or(*default),
        }
    }
}

// ---------------------------------------------------------------- parsing ----

/// Decode an XML file that may be UTF-8, UTF-16 LE/BE (with or without BOM).
pub fn decode_text(bytes: &[u8]) -> String {
    fn utf16(b: &[u8], le: bool) -> String {
        let units: Vec<u16> = b
            .as_chunks::<2>().0.iter()
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        String::from_utf16_lossy(&units)
    }
    let s = match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        // BOM-less UTF-16: `<` followed by NUL (LE) or NUL followed by `<` (BE).
        [b'<', 0, ..] => utf16(bytes, true),
        [0, b'<', ..] => utf16(bytes, false),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    };
    s.trim_start_matches('\u{feff}').to_string()
}

/// roxmltree refuses DTDs and doesn't care about the declared encoding (we've
/// already decoded), so strip the XML declaration and any DOCTYPE.
fn prepare_xml(text: &str) -> String {
    let mut t = text.trim_start();
    if t.starts_with("<?xml")
        && let Some(end) = t.find("?>") {
            t = &t[end + 2..];
        }
    let mut out = t.to_string();
    if let Some(start) = out.find("<!DOCTYPE")
        && let Some(len) = out[start..].find('>') {
            out.replace_range(start..start + len + 1, "");
        }
    out
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name().eq_ignore_ascii_case(name))
}

fn children<'a, 'i>(n: Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = Node<'a, 'i>> + 'a
where
    'i: 'a,
{
    n.children().filter(move |c| c.is_element() && c.tag_name().name().eq_ignore_ascii_case(name))
}

fn attr<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name().eq_ignore_ascii_case(name)).map(|a| a.value())
}

fn text(n: Option<Node>) -> String {
    n.map(|n| n.descendants().filter(|d| d.is_text()).map(|d| d.text().unwrap_or("")).collect::<String>())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn parse_bool(v: Option<&str>) -> bool {
    matches!(v.map(str::trim), Some("true") | Some("1"))
}

fn parse_files(n: Option<Node>) -> Vec<FileItem> {
    let Some(n) = n else { return vec![] };
    n.children()
        .filter(|c| c.is_element())
        .filter_map(|c| {
            let is_folder = match c.tag_name().name().to_ascii_lowercase().as_str() {
                "file" => false,
                "folder" => true,
                _ => return None,
            };
            Some(FileItem {
                source: attr(c, "source")?.to_string(),
                destination: attr(c, "destination").map(str::to_string),
                is_folder,
                priority: attr(c, "priority").and_then(|p| p.trim().parse().ok()).unwrap_or(0),
                always_install: parse_bool(attr(c, "alwaysInstall")),
                install_if_usable: parse_bool(attr(c, "installIfUsable")),
            })
        })
        .collect()
}

fn parse_operator(n: Node) -> Operator {
    match attr(n, "operator") {
        Some(o) if o.eq_ignore_ascii_case("or") => Operator::Or,
        _ => Operator::And,
    }
}

/// Parse a `<dependencies>`/`<visible>`/`<moduleDependencies>` element.
fn parse_composite(n: Node) -> Dependency {
    let children = n
        .children()
        .filter(|c| c.is_element())
        .filter_map(|c| match c.tag_name().name().to_ascii_lowercase().as_str() {
            "filedependency" => Some(Dependency::File {
                file: attr(c, "file").unwrap_or_default().to_string(),
                state: match attr(c, "state").unwrap_or("Active").to_ascii_lowercase().as_str() {
                    "missing" => FileState::Missing,
                    "inactive" => FileState::Inactive,
                    _ => FileState::Active,
                },
            }),
            "flagdependency" => Some(Dependency::Flag {
                flag: attr(c, "flag").unwrap_or_default().to_string(),
                value: attr(c, "value").unwrap_or_default().to_string(),
            }),
            "dependencies" => Some(parse_composite(c)),
            // gameDependency, fommDependency, foseDependency, ... : version checks.
            _ => Some(Dependency::Version),
        })
        .collect();
    Dependency::Composite { op: parse_operator(n), children }
}

fn parse_plugin_type(s: Option<&str>) -> PluginType {
    match s.unwrap_or("").to_ascii_lowercase().as_str() {
        "required" => PluginType::Required,
        "recommended" => PluginType::Recommended,
        "notusable" => PluginType::NotUsable,
        "couldbeusable" => PluginType::CouldBeUsable,
        _ => PluginType::Optional,
    }
}

fn parse_type_descriptor(n: Option<Node>) -> TypeDescriptor {
    let Some(n) = n else { return TypeDescriptor::Simple(PluginType::Optional) };
    if let Some(t) = child(n, "type") {
        return TypeDescriptor::Simple(parse_plugin_type(attr(t, "name")));
    }
    if let Some(dt) = child(n, "dependencyType") {
        let default = parse_plugin_type(child(dt, "defaultType").and_then(|d| attr(d, "name")));
        let patterns = child(dt, "patterns")
            .map(|ps| {
                children(ps, "pattern")
                    .map(|p| {
                        let dep = child(p, "dependencies")
                            .map(parse_composite)
                            .unwrap_or(Dependency::Composite { op: Operator::And, children: vec![] });
                        let ty = parse_plugin_type(child(p, "type").and_then(|t| attr(t, "name")));
                        (dep, ty)
                    })
                    .collect()
            })
            .unwrap_or_default();
        return TypeDescriptor::Dependent { default, patterns };
    }
    TypeDescriptor::Simple(PluginType::Optional)
}

/// Apply a schema `order` attribute (Ascending is the schema default).
fn apply_order<T>(items: &mut Vec<T>, order: Option<&str>, name: impl Fn(&T) -> String) {
    match order.unwrap_or("Ascending").to_ascii_lowercase().as_str() {
        "explicit" => {}
        "descending" => items.sort_by_key(|i| std::cmp::Reverse(name(i).to_lowercase())),
        _ => items.sort_by_key(|i| name(i).to_lowercase()),
    }
}

fn parse_group_type(s: Option<&str>) -> GroupType {
    match s.unwrap_or("").to_ascii_lowercase().as_str() {
        "selectexactlyone" => GroupType::SelectExactlyOne,
        "selectatmostone" => GroupType::SelectAtMostOne,
        "selectatleastone" => GroupType::SelectAtLeastOne,
        "selectall" => GroupType::SelectAll,
        _ => GroupType::SelectAny,
    }
}

pub fn parse_module_config(xml: &str) -> Result<ModuleConfig> {
    let prepared = prepare_xml(xml);
    let doc = Document::parse(&prepared).context("ModuleConfig.xml is not valid XML")?;
    let root = doc.root_element();
    if !root.tag_name().name().eq_ignore_ascii_case("config") {
        return Err(anyhow!("ModuleConfig.xml root element is <{}>, expected <config>", root.tag_name().name()));
    }

    let mut cfg = ModuleConfig {
        name: text(child(root, "moduleName")),
        image: child(root, "moduleImage").and_then(|i| attr(i, "path")).map(str::to_string),
        module_dependencies: child(root, "moduleDependencies").map(parse_composite),
        required_files: parse_files(child(root, "requiredInstallFiles")),
        ..Default::default()
    };

    if let Some(steps_node) = child(root, "installSteps") {
        let mut steps: Vec<Step> = children(steps_node, "installStep")
            .map(|s| {
                let mut groups: Vec<Group> = child(s, "optionalFileGroups")
                    .map(|gs| {
                        let mut groups: Vec<Group> = children(gs, "group")
                            .map(|g| {
                                let plugins_node = child(g, "plugins");
                                let mut plugins: Vec<Plugin> = plugins_node
                                    .map(|ps| {
                                        children(ps, "plugin")
                                            .map(|p| Plugin {
                                                name: attr(p, "name").unwrap_or_default().to_string(),
                                                description: text(child(p, "description")),
                                                image: child(p, "image")
                                                    .and_then(|i| attr(i, "path"))
                                                    .map(str::to_string),
                                                files: parse_files(child(p, "files")),
                                                flags: child(p, "conditionFlags")
                                                    .map(|cf| {
                                                        children(cf, "flag")
                                                            .map(|f| {
                                                                (
                                                                    attr(f, "name").unwrap_or_default().to_string(),
                                                                    text(Some(f)),
                                                                )
                                                            })
                                                            .collect()
                                                    })
                                                    .unwrap_or_default(),
                                                type_descriptor: parse_type_descriptor(child(p, "typeDescriptor")),
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                apply_order(&mut plugins, plugins_node.and_then(|n| attr(n, "order")), |p| p.name.clone());
                                Group {
                                    name: attr(g, "name").unwrap_or_default().to_string(),
                                    kind: parse_group_type(attr(g, "type")),
                                    plugins,
                                }
                            })
                            .collect();
                        apply_order(&mut groups, attr(gs, "order"), |g| g.name.clone());
                        groups
                    })
                    .unwrap_or_default();
                groups.retain(|g| !g.plugins.is_empty());
                Step {
                    name: attr(s, "name").unwrap_or_default().to_string(),
                    visible: child(s, "visible").map(parse_composite),
                    groups,
                }
            })
            .collect();
        apply_order(&mut steps, attr(steps_node, "order"), |s| s.name.clone());
        cfg.steps = steps;
    }

    if let Some(ci) = child(root, "conditionalFileInstalls")
        && let Some(ps) = child(ci, "patterns") {
            cfg.conditional_installs = children(ps, "pattern")
                .map(|p| ConditionalInstall {
                    dependency: child(p, "dependencies")
                        .map(parse_composite)
                        .unwrap_or(Dependency::Composite { op: Operator::And, children: vec![] }),
                    files: parse_files(child(p, "files")),
                })
                .collect();
        }
    Ok(cfg)
}

pub fn parse_info(xml: &str) -> ModuleInfo {
    let prepared = prepare_xml(xml);
    let Ok(doc) = Document::parse(&prepared) else { return ModuleInfo::default() };
    let root = doc.root_element();
    let get = |n: &str| Some(text(child(root, n))).filter(|s| !s.is_empty());
    ModuleInfo {
        name: get("Name"),
        author: get("Author"),
        version: get("Version"),
        website: get("Website"),
        description: get("Description"),
    }
}

/// A located FOMOD inside an extracted archive.
#[derive(Debug, Clone)]
pub struct FomodPackage {
    /// Directory that contains the `fomod` folder; file sources are relative to it.
    pub root: PathBuf,
    pub config: ModuleConfig,
    pub info: ModuleInfo,
}

impl FomodPackage {
    pub fn load(root: &Path, config_path: &Path) -> Result<Self> {
        let bytes = std::fs::read(config_path)
            .with_context(|| format!("reading {}", config_path.display()))?;
        let config = parse_module_config(&decode_text(&bytes))?;
        let info = config_path
            .parent()
            .and_then(|d| crate::paths::find_child_ci(d, "info.xml"))
            .and_then(|p| std::fs::read(p).ok())
            .map(|b| parse_info(&decode_text(&b)))
            .unwrap_or_default();
        Ok(Self { root: root.to_path_buf(), config, info })
    }

    /// Resolve an image path from the XML to a file on disk, if it exists.
    pub fn image_path(&self, rel: &str) -> Option<PathBuf> {
        let p = crate::paths::resolve_ci(&self.root, &crate::paths::normalize_rel(rel));
        p.is_file().then_some(p)
    }
}

// ----------------------------------------------------------------- wizard ----

/// Drives a user through the install steps. Holds selections for every step
/// so going back and forth keeps choices.
pub struct Wizard {
    pub config: ModuleConfig,
    /// `selected[step][group][plugin]`
    pub selected: Vec<Vec<Vec<bool>>>,
    initialized: Vec<bool>,
    /// Steps visited before the current one, in order.
    history: Vec<usize>,
    /// Index of the current step; `None` if there are no visible steps.
    pub current: Option<usize>,
}

impl Wizard {
    pub fn new(config: ModuleConfig, env: &dyn Environment) -> Self {
        let selected = config
            .steps
            .iter()
            .map(|s| s.groups.iter().map(|g| vec![false; g.plugins.len()]).collect())
            .collect();
        let initialized = vec![false; config.steps.len()];
        let mut w = Self { config, selected, initialized, history: vec![], current: None };
        w.current = w.next_visible_from(0, &Flags::new(), env);
        if let Some(c) = w.current {
            w.init_step(c, env);
        }
        w
    }

    fn next_visible_from(&self, start: usize, flags: &Flags, env: &dyn Environment) -> Option<usize> {
        (start..self.config.steps.len()).find(|&i| {
            let s = &self.config.steps[i];
            !s.groups.is_empty() && s.visible.as_ref().is_none_or(|v| v.eval(flags, env))
        })
    }

    /// Flags produced by the selections in `steps`.
    fn flags_of(&self, steps: impl Iterator<Item = usize>) -> Flags {
        let mut flags = Flags::new();
        for s in steps {
            for (gi, g) in self.config.steps[s].groups.iter().enumerate() {
                for (pi, p) in g.plugins.iter().enumerate() {
                    if self.selected[s][gi][pi] {
                        for (k, v) in &p.flags {
                            flags.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
        }
        flags
    }

    /// Flags as seen by the current step (from previous steps only).
    pub fn flags_before_current(&self) -> Flags {
        self.flags_of(self.history.iter().copied())
    }

    /// Flags including the current step's selections.
    pub fn flags_through_current(&self) -> Flags {
        self.flags_of(self.history.iter().copied().chain(self.current))
    }

    pub fn plugin_type(&self, step: usize, group: usize, plugin: usize, env: &dyn Environment) -> PluginType {
        let flags = self.flags_before_current();
        self.config.steps[step].groups[group].plugins[plugin]
            .type_descriptor
            .resolve(&flags, env)
    }

    fn init_step(&mut self, step: usize, env: &dyn Environment) {
        if self.initialized[step] {
            return;
        }
        self.initialized[step] = true;
        let flags = self.flags_before_current();
        for (gi, g) in self.config.steps[step].groups.iter().enumerate() {
            let types: Vec<PluginType> =
                g.plugins.iter().map(|p| p.type_descriptor.resolve(&flags, env)).collect();
            let sel = &mut self.selected[step][gi];
            for (i, t) in types.iter().enumerate() {
                sel[i] = match g.kind {
                    GroupType::SelectAll => true,
                    _ => matches!(t, PluginType::Required | PluginType::Recommended),
                };
            }
            let first_usable = types.iter().position(|t| *t != PluginType::NotUsable);
            match g.kind {
                GroupType::SelectExactlyOne | GroupType::SelectAtMostOne => {
                    // Keep at most the first pre-selected one.
                    if let Some(first) = sel.iter().position(|s| *s) {
                        for (i, s) in sel.iter_mut().enumerate() {
                            *s = i == first;
                        }
                    } else if g.kind == GroupType::SelectExactlyOne
                        && let Some(f) = first_usable {
                            sel[f] = true;
                        }
                }
                GroupType::SelectAtLeastOne => {
                    if !sel.iter().any(|s| *s)
                        && let Some(f) = first_usable {
                            sel[f] = true;
                        }
                }
                _ => {}
            }
        }
    }

    /// Whether the user may change this plugin's checkbox.
    pub fn is_locked(&self, step: usize, group: usize, plugin: usize, env: &dyn Environment) -> bool {
        let g = &self.config.steps[step].groups[group];
        g.kind == GroupType::SelectAll
            || matches!(self.plugin_type(step, group, plugin, env), PluginType::Required | PluginType::NotUsable)
    }

    /// Toggle/select a plugin respecting the group's rules.
    pub fn set(&mut self, step: usize, group: usize, plugin: usize, value: bool, env: &dyn Environment) {
        if self.is_locked(step, group, plugin, env) {
            return;
        }
        let kind = self.config.steps[step].groups[group].kind;
        let sel = &mut self.selected[step][group];
        match kind {
            GroupType::SelectExactlyOne => {
                if value {
                    for (i, s) in sel.iter_mut().enumerate() {
                        *s = i == plugin;
                    }
                }
            }
            GroupType::SelectAtMostOne => {
                for (i, s) in sel.iter_mut().enumerate() {
                    *s = if i == plugin { value } else { false };
                }
            }
            _ => sel[plugin] = value,
        }
    }

    /// Problems that prevent leaving the current step.
    pub fn validation_errors(&self) -> Vec<String> {
        let Some(s) = self.current else { return vec![] };
        let mut errs = vec![];
        for (gi, g) in self.config.steps[s].groups.iter().enumerate() {
            let n = self.selected[s][gi].iter().filter(|x| **x).count();
            match g.kind {
                GroupType::SelectExactlyOne if n != 1 => errs.push(format!("Select exactly one option in \"{}\"", g.name)),
                GroupType::SelectAtLeastOne if n == 0 => errs.push(format!("Select at least one option in \"{}\"", g.name)),
                _ => {}
            }
        }
        errs
    }

    pub fn has_next(&self, env: &dyn Environment) -> bool {
        match self.current {
            Some(c) => self.next_visible_from(c + 1, &self.flags_through_current(), env).is_some(),
            None => false,
        }
    }

    pub fn has_back(&self) -> bool {
        !self.history.is_empty()
    }

    pub fn next(&mut self, env: &dyn Environment) -> bool {
        let Some(c) = self.current else { return false };
        let flags = self.flags_through_current();
        match self.next_visible_from(c + 1, &flags, env) {
            Some(n) => {
                self.history.push(c);
                self.current = Some(n);
                self.init_step(n, env);
                true
            }
            None => false,
        }
    }

    pub fn back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.current = Some(prev);
        }
    }

    pub fn visited_steps(&self) -> Vec<usize> {
        self.history.iter().copied().chain(self.current).collect()
    }

    /// Everything to install given the current selections, sorted by
    /// ascending priority (later entries overwrite earlier ones).
    pub fn files_to_install(&self, env: &dyn Environment) -> Vec<FileItem> {
        let mut out: Vec<FileItem> = self.config.required_files.clone();
        let visited = self.visited_steps();
        for (k, &s) in visited.iter().enumerate() {
            let flags_before = self.flags_of(visited[..k].iter().copied());
            for (gi, g) in self.config.steps[s].groups.iter().enumerate() {
                for (pi, p) in g.plugins.iter().enumerate() {
                    let selected = self.selected[s][gi][pi];
                    let ty = p.type_descriptor.resolve(&flags_before, env);
                    for f in &p.files {
                        if selected || f.always_install || (f.install_if_usable && ty != PluginType::NotUsable) {
                            out.push(f.clone());
                        }
                    }
                }
            }
        }
        let flags = self.flags_through_current();
        for ci in &self.config.conditional_installs {
            if ci.dependency.eval(&flags, env) {
                out.extend(ci.files.iter().cloned());
            }
        }
        out.sort_by_key(|f| f.priority); // stable: document order within a priority
        out
    }
}

impl FileItem {
    /// Destination path relative to Data.
    pub fn dest_rel(&self) -> PathBuf {
        use crate::paths::normalize_rel;
        match &self.destination {
            None => normalize_rel(&self.source),
            Some(d) => {
                let d_norm = normalize_rel(d);
                if self.is_folder {
                    d_norm
                } else if d.trim().is_empty() || d.ends_with('/') || d.ends_with('\\') {
                    let name = normalize_rel(&self.source);
                    d_norm.join(name.file_name().unwrap_or_default())
                } else {
                    d_norm
                }
            }
        }
    }
}

// ------------------------------------------------------------------ tests ----

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-16"?>
<config xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:noNamespaceSchemaLocation="http://qconsulting.ca/fo3/ModConfig5.0.xsd">
  <moduleName>Test Mod</moduleName>
  <requiredInstallFiles><folder source="Core" destination=""/></requiredInstallFiles>
  <installSteps order="Explicit">
    <installStep name="Body">
      <optionalFileGroups order="Explicit">
        <group name="Body type" type="SelectExactlyOne">
          <plugins order="Explicit">
            <plugin name="CBBE">
              <description>Curvy</description>
              <image path="fomod\cbbe.png"/>
              <files><folder source="Bodies\CBBE" destination=""/></files>
              <conditionFlags><flag name="body">cbbe</flag></conditionFlags>
              <typeDescriptor><type name="Optional"/></typeDescriptor>
            </plugin>
            <plugin name="UNP">
              <description>Slim</description>
              <conditionFlags><flag name="body">unp</flag></conditionFlags>
              <typeDescriptor><type name="Recommended"/></typeDescriptor>
            </plugin>
          </plugins>
        </group>
      </optionalFileGroups>
    </installStep>
    <installStep name="CBBE extras">
      <visible><flagDependency flag="body" value="cbbe"/></visible>
      <optionalFileGroups>
        <group name="Extras" type="SelectAny">
          <plugins>
            <plugin name="Zeta"><description/><files><file source="z.esp"/></files>
              <typeDescriptor><type name="Optional"/></typeDescriptor></plugin>
            <plugin name="Alpha"><description/><files><file source="opt\a.esp" destination="a.esp"/></files>
              <typeDescriptor><dependencyType><defaultType name="Optional"/>
                <patterns><pattern><dependencies operator="And"><fileDependency file="SKSE.esp" state="Active"/></dependencies><type name="NotUsable"/></pattern></patterns>
              </dependencyType></typeDescriptor></plugin>
          </plugins>
        </group>
      </optionalFileGroups>
    </installStep>
  </installSteps>
  <conditionalFileInstalls><patterns>
    <pattern><dependencies><flagDependency flag="body" value="unp"/></dependencies>
      <files><file source="unp.esp" destination="Body.esp" priority="5"/></files></pattern>
  </patterns></conditionalFileInstalls>
</config>"#;

    fn utf16le(s: &str) -> Vec<u8> {
        let mut v = vec![0xFF, 0xFE];
        for u in s.encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v
    }

    #[test]
    fn parses_utf16_and_structure() {
        let cfg = parse_module_config(&decode_text(&utf16le(XML))).unwrap();
        assert_eq!(cfg.name, "Test Mod");
        assert_eq!(cfg.steps.len(), 2);
        assert_eq!(cfg.steps[0].groups[0].plugins[0].name, "CBBE");
        // Second step's plugins use default Ascending order.
        assert_eq!(cfg.steps[1].groups[0].plugins[0].name, "Alpha");
        assert_eq!(cfg.required_files[0].destination.as_deref(), Some(""));
    }

    #[test]
    fn wizard_flow_with_flags() {
        let cfg = parse_module_config(XML).unwrap();
        let mut w = Wizard::new(cfg, &NoFiles);
        assert_eq!(w.current, Some(0));
        // Recommended UNP preselected, so the CBBE step is hidden.
        assert_eq!(w.selected[0][0], vec![false, true]);
        assert!(!w.has_next(&NoFiles));
        let files = w.files_to_install(&NoFiles);
        assert_eq!(files.last().unwrap().dest_rel(), PathBuf::from("Body.esp"));

        w.set(0, 0, 0, true, &NoFiles);
        assert_eq!(w.selected[0][0], vec![true, false]);
        assert!(w.has_next(&NoFiles));
        assert!(w.next(&NoFiles));
        assert_eq!(w.current, Some(1));
        w.set(1, 0, 0, true, &NoFiles);
        let dests: Vec<_> = w.files_to_install(&NoFiles).iter().map(|f| f.dest_rel()).collect();
        assert_eq!(dests, vec![PathBuf::from(""), PathBuf::from(""), PathBuf::from("a.esp")]);
        w.back();
        assert_eq!(w.current, Some(0));
    }
}
