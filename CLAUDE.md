# Skald

Skyrim Special Edition mod manager for Linux (Steam + Proton). Rust 2024, egui/eframe GUI.

- `README.md` is the user guide, in plain language.
- `README-TECHNICAL.md` is the full design reference: files on disk, install pipeline, FOMOD,
  deploy/purge, plugins, launching, GUI. Read the relevant section there before changing a module.

## Commands

```sh
cargo build
cargo test                                   # unit tests in each module + tests/end_to_end.rs
cargo clippy --all-targets                   # fomod.rs has some older warnings; don't add new ones
cargo run --release                          # GUI
cargo run --example inspect -- a.7z b.zip    # read-only: detected game setup + installer type
```

## Layout

- Library crate (`src/lib.rs`): `archive`, `config`, `deploy`, `fomod`, `installer`, `launch`,
  `mods`, `paths`, `plugins`. Logic lives here and is unit-testable without a GUI.
- Binary only: `main.rs`, `app.rs` (main window, background jobs over `mpsc`, settings dialog),
  `install_ui.rs` (FOMOD wizard / manual install dialogs), `theme.rs` (palette, style, shared widgets).
- Data: `~/.config/skald/config.json`, plus `~/.local/share/skald/` (`mods/<name>/{data,root}`,
  `modlist.json`, `deployment.json`, `backups/`).

## Rules that must hold

- **The game is case-insensitive, Linux is not.** Always use the `paths.rs` helpers
  (`find_child_ci`, `resolve_ci`, `resolve_ci_create`, `key`) when matching or creating game paths.
  Never `join` a mod-relative path onto the game folder directly.
- **Never delete a file Skald didn't place.** Deploy moves existing files to `backups/`. Purge
  removes only targets that pass `is_ours`, and anything changed outside Skald is kept (the original
  is saved as `.skald-original`). Staging folders under `mods/` are never changed by deploy or purge.
- Write the manifest entry *before* making the link, so a crashed deploy can still be purged.
  Write JSON state through a `.tmp` file and rename it into place.
- Changes to the mod list mark the state as "Changes not deployed". Plugin order is saved straight
  away to `Plugins.txt`/`loadorder.txt` (CRLF, `*` = active).
- New `Config` fields need `#[serde(default)]` so existing config files still load.

## Environment gotchas

- `~/.steam/steam` and `~/.steam/root` usually link to `~/.local/share/Steam`. Remove duplicates
  from anything collected across `config::steam_roots()` by comparing `fs::canonicalize` results.
- Non-Steam runners (SKSE, custom exes) run as `<proton>/proton run <exe>` outside Steam's runtime
  container, and need the Steam client running. The Proton build is read from
  `compatdata/489830/config_info`.
- Don't click Play or call `launch::launch` while testing: it starts the real game. Test detection
  with a throwaway example that prints `launch::runners` / `find_proton` instead.
- Tests use `tempfile` and must not touch the real game folder or `~/.local/share/skald`.

## Conventions

- Error handling uses `anyhow` with `.context(...)`. Show errors to the user in the GUI log with
  `self.error(...)` and `format!("{e:#}")`.
- Use let-chains (`if let ... && ...`) and keep the existing compact style.
- When behaviour changes, update both READMEs in the same change. The user README stays plain and
  task-oriented; implementation detail goes in the technical one.
