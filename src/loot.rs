//! Plugin sorting with libloot and the official Skyrim SE masterlist.

use anyhow::{Context, Result, anyhow, bail};
use libloot::metadata::{Message, MessageContent, MessageType, select_message_content};
use libloot::{EvalMode, Game, GameType, MergeMode};
use std::fs;
use std::path::{Path, PathBuf};

pub const MASTERLIST_URL: &str = "https://github.com/loot/skyrimse/raw/refs/heads/v0.29/masterlist.yaml";
pub const PRELUDE_URL: &str = "https://github.com/loot/prelude/raw/refs/heads/v0.29/prelude.yaml";
pub const MASTERLIST: &str = "masterlist.yaml";
pub const PRELUDE: &str = "prelude.yaml";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// Plugins to sort, in the current order: (file name, absolute path).
#[derive(Debug, Clone)]
pub struct SortInput {
    pub game_dir: PathBuf,
    pub appdata_dir: PathBuf,
    pub plugins: Vec<(String, PathBuf)>,
}

#[derive(Debug, Clone, Default)]
pub struct SortOutcome {
    pub order: Vec<String>,
    /// (plugin name, or "" for general messages; level; text).
    pub messages: Vec<(String, Level, String)>,
}

fn fetch(url: &str, dest: &Path) -> Result<()> {
    let body = ureq::get(url)
        .call()
        .with_context(|| format!("downloading {url}"))?
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .with_context(|| format!("reading {url}"))?;
    let tmp = dest.with_extension("yaml.tmp");
    fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, dest).with_context(|| format!("replacing {}", dest.display()))?;
    Ok(())
}

/// Download the masterlist and prelude into `dir`. Returns `Ok(true)` when both are fresh,
/// `Ok(false)` when the download failed but cached copies exist, and an error otherwise.
pub fn update_masterlist(dir: &Path) -> Result<bool> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let res = fetch(MASTERLIST_URL, &dir.join(MASTERLIST)).and_then(|()| fetch(PRELUDE_URL, &dir.join(PRELUDE)));
    match res {
        Ok(()) => Ok(true),
        Err(_) if dir.join(MASTERLIST).is_file() && dir.join(PRELUDE).is_file() => Ok(false),
        Err(e) => Err(e.context("no cached masterlist to fall back on")),
    }
}

/// LOOT messages are Markdown: drop `**`/`__` and turn `[text](url)` into `text (url)`.
pub fn plain(md: &str) -> String {
    let s = md.replace("**", "").replace("__", "");
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        if let Some(close) = after.find("](")
            && let Some(end) = after[close + 2..].find(')')
        {
            out.push_str(&rest[..open]);
            out.push_str(&after[..close]);
            out.push_str(" (");
            out.push_str(&after[close + 2..close + 2 + end]);
            out.push(')');
            rest = &after[close + 3 + end..];
        } else {
            out.push_str(&rest[..=open]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn text(content: &[MessageContent]) -> String {
    select_message_content(content, MessageContent::DEFAULT_LANGUAGE).map(|c| plain(c.text())).unwrap_or_default()
}

fn message(plugin: &str, m: &Message) -> (String, Level, String) {
    let level = match m.message_type() {
        MessageType::Error => Level::Error,
        MessageType::Warn => Level::Warn,
        _ => Level::Info,
    };
    (plugin.to_string(), level, text(m.content()))
}

/// Sort `input.plugins` with the masterlist in `masterlist_dir`. Nothing is written to disk.
pub fn sort(input: &SortInput, masterlist_dir: &Path) -> Result<SortOutcome> {
    let mut game = Game::with_local_path(GameType::SkyrimSE, &input.game_dir, &input.appdata_dir).context("setting up LOOT")?;
    game.database()
        .write()
        .map_err(|_| anyhow!("LOOT database lock poisoned"))?
        .load_masterlist_with_prelude(&masterlist_dir.join(MASTERLIST), &masterlist_dir.join(PRELUDE))
        .context("loading the LOOT masterlist")?;

    let mut out = SortOutcome::default();
    let (valid, invalid): (Vec<_>, Vec<_>) = input.plugins.iter().partition(|(_, p)| game.is_valid_plugin(p));
    for (n, _) in invalid {
        out.messages.push((n.clone(), Level::Warn, "not a valid plugin, left where it was".into()));
    }
    let paths: Vec<&Path> = valid.iter().map(|(_, p)| p.as_path()).collect();
    game.load_plugins(&paths).context("reading plugins")?;
    let names: Vec<&str> = valid.iter().map(|(n, _)| n.as_str()).filter(|n| game.plugin(n).is_some()).collect();
    if names.is_empty() {
        bail!("no plugins could be read");
    }
    out.order = game.sort_plugins(&names).context("sorting")?;

    let db = game.database();
    let db = db.read().map_err(|_| anyhow!("LOOT database lock poisoned"))?;
    for m in db.general_messages(MergeMode::WithoutUserMetadata, EvalMode::Evaluate).unwrap_or_default() {
        out.messages.push(message("", &m));
    }
    for name in &out.order {
        let Ok(Some(meta)) = db.plugin_metadata(name, MergeMode::WithoutUserMetadata, EvalMode::Evaluate) else { continue };
        out.messages.extend(meta.messages().iter().map(|m| message(name, m)));
        let crc = game.plugin(name).and_then(|p| p.crc());
        for d in meta.dirty_info().iter().filter(|d| Some(d.crc()) == crc) {
            out.messages.push((
                name.clone(),
                Level::Warn,
                format!(
                    "needs cleaning with {}: {} ITM records, {} deleted references, {} deleted navmeshes",
                    plain(d.cleaning_utility()),
                    d.itm_count(),
                    d.deleted_reference_count(),
                    d.deleted_navmesh_count()
                ),
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal Skyrim plugin: a TES4 record with HEDR and optional masters.
    fn plugin(path: &Path, flags: u32, masters: &[&str]) {
        let mut sub = vec![];
        sub.extend(b"HEDR");
        sub.extend(12u16.to_le_bytes());
        sub.extend(1.7f32.to_le_bytes());
        sub.extend(0u32.to_le_bytes());
        sub.extend(0x800u32.to_le_bytes());
        for m in masters {
            sub.extend(b"MAST");
            sub.extend((m.len() as u16 + 1).to_le_bytes());
            sub.extend(m.as_bytes());
            sub.push(0);
            sub.extend(b"DATA");
            sub.extend(8u16.to_le_bytes());
            sub.extend(0u64.to_le_bytes());
        }
        let mut rec = vec![];
        rec.extend(b"TES4");
        rec.extend((sub.len() as u32).to_le_bytes());
        rec.extend(flags.to_le_bytes());
        rec.extend([0u8; 8]);
        rec.extend(44u16.to_le_bytes());
        rec.extend(0u16.to_le_bytes());
        rec.extend(sub);
        fs::write(path, rec).unwrap();
    }

    #[test]
    fn markdown_to_plain() {
        assert_eq!(plain("Your **SKSE** needs [SSEEdit v4](https://x.y/1) [not a link"), "Your SKSE needs SSEEdit v4 (https://x.y/1) [not a link");
    }

    #[test]
    fn sorts_with_masterlist_rules() {
        let t = tempfile::tempdir().unwrap();
        let (game, data, app, ml, staged) = (t.path().join("game"), t.path().join("game/Data"), t.path().join("app"), t.path().join("ml"), t.path().join("mod"));
        for d in [&data, &app, &ml, &staged] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(game.join("SkyrimSE.exe"), b"").unwrap();
        plugin(&data.join("Skyrim.esm"), 1, &[]);
        plugin(&data.join("A.esp"), 0, &["Skyrim.esm"]);
        // Not deployed: lives in a mod's staging folder.
        plugin(&staged.join("B.esp"), 0, &["Skyrim.esm"]);
        fs::write(data.join("Broken.esp"), b"junk").unwrap();
        fs::write(ml.join(PRELUDE), "common: []\n").unwrap();
        fs::write(
            ml.join(MASTERLIST),
            "plugins:\n  - name: 'A.esp'\n    after: ['B.esp']\n    msg:\n      - type: warn\n        content: 'Check A.'\n",
        )
        .unwrap();

        let input = SortInput {
            game_dir: game,
            appdata_dir: app,
            plugins: vec![
                ("Skyrim.esm".into(), data.join("Skyrim.esm")),
                ("A.esp".into(), data.join("A.esp")),
                ("B.esp".into(), staged.join("B.esp")),
                ("Broken.esp".into(), data.join("Broken.esp")),
            ],
        };
        let out = sort(&input, &ml).unwrap();
        assert_eq!(out.order, ["Skyrim.esm", "B.esp", "A.esp"]);
        assert!(out.messages.contains(&("A.esp".into(), Level::Warn, "Check A.".into())));
        assert!(out.messages.iter().any(|(n, l, _)| n == "Broken.esp" && *l == Level::Warn));
    }
}
