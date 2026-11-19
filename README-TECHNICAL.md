# Skald: Technical Reference

How Skald is put together, where it keeps things, and what it does to the game folder.
For a plain-language guide, see [README.md](README.md).

Skald is a mod manager for **Skyrim Special Edition on Linux** (Steam + Proton). It's written in
Rust (edition 2024) with an [egui](https://github.com/emilk/egui)/eframe desktop GUI.

---

## 1. Project layout

```
skald/
├── Cargo.toml
├── src/
│   ├── main.rs          # binary entry: creates the eframe window (1280×800, drag-and-drop on)
│   ├── app.rs           # GUI: mod list, plugin list, top bar, log, settings, background jobs
│   ├── install_ui.rs    # GUI: install dialog, one menu per installer type
│   ├── theme.rs         # GUI: dark palette, egui style, small shared widgets (pills, cards)
│   ├── lib.rs           # library crate `skald` (everything below is GUI-free)
│   ├── archive.rs       # zip / 7z / rar / folder extraction
│   ├── installer.rs     # installer-type detection + install plans
│   ├── fomod.rs         # FOMOD XML parser, condition evaluator, wizard state machine
│   ├── mods.rs          # installed mod list, ordering, conflict counting
│   ├── deploy.rs        # deploy (link into game) / purge (undo), manifest + backups
│   ├── launch.rs        # Play runners: Steam, SKSE, custom .exe via Proton
│   ├── exeicon.rs       # reads the icon embedded in a Windows .exe (tools tray)
│   ├── plugins.rs       # Plugins.txt / loadorder.txt, ESM/ESL header flags
│   ├── loot.rs          # LOOT sorting: masterlist download, libloot sort, messages
│   ├── config.rs        # settings + Steam/Proton auto-detection
│   └── paths.rs         # case-insensitive path helpers
├── tests/end_to_end.rs  # zip → FOMOD wizard → install → deploy → purge
└── examples/inspect.rs  # read-only CLI diagnostics
```

The split matters: `lib.rs` exposes only the GUI-free modules, so tests and `examples/inspect.rs` use
the same code as the GUI without needing a window. `app.rs`, `install_ui.rs` and `theme.rs` belong only to the binary.

### Dependencies

| Crate | Used for |
|---|---|
| `eframe`, `egui`, `egui_extras` | GUI; `egui_extras` image loaders show FOMOD option images |
| `image` | PNG/JPEG/BMP/WebP/GIF decoding for those images |
| `rfd` | Native file and folder pickers |
| `zip`, `sevenz-rust2` | Archive extraction (pure Rust) |
| `unrar` (external program) | RAR extraction, called as a subprocess |
| `roxmltree` | FOMOD XML parsing |
| `serde`, `serde_json` | Config, mod list and deployment manifest |
| `walkdir` | Recursive file walks |
| `dirs` | XDG config/data directories |
| `anyhow` | Error handling |
| `libloot` | LOOT plugin sorting and masterlist messages (pure Rust; **GPL-3.0-or-later**, so distributed builds must be GPL-compatible) |
| `ureq` | Downloading the LOOT masterlist and prelude |
| `tempfile` (dev) | Tests |

---

## 2. Files on disk

| Path | Contents |
|---|---|
| `~/.config/skald/config.json` | `game_dir`, `appdata_dir`, `deploy_method`, `runner`, `runners`, `tools`, `proton` |
| `~/.local/share/skald/modlist.json` | Ordered mod list (`name`, `enabled`, `installer`, `version`, `author`, `source`, `installed_at`) |
| `~/.local/share/skald/mods/<name>/data/` | Files that go into `Skyrim Special Edition/Data` |
| `~/.local/share/skald/mods/<name>/root/` | Files that go next to `SkyrimSE.exe` (SKSE, ENB, …) |
| `~/.local/share/skald/deployment.json` | Manifest of every file Skald put in the game folder |
| `~/.local/share/skald/backups/{data,root}/` | Original game files that a deployed mod replaced |
| `~/.local/share/skald/unmanaged/<unix time>/` | Unmanaged plugins moved out of `Data` by **Remove unmanaged** (never deleted automatically) |
| `~/.local/share/skald/loot/` | Cached `masterlist.yaml` and `prelude.yaml`, replaced on every sort when the download works |
| `~/.local/share/skald/tmp/` | Extraction and staging scratch space; wiped on startup |

Writes to `modlist.json` and `deployment.json` go to a `.tmp` file first and are then renamed over
the original, so a crash never leaves a half-written file.

When `modlist.json` loads, it is checked against `mods/`. Entries whose folder no longer exists are
dropped. Folders that aren't listed are added at the end, disabled, with installer `Unknown`. This
means you can put a folder into `mods/` by hand.

---

## 3. Game detection (`config.rs`)

1. **Steam roots** checked: `~/.local/share/Steam`, `~/.steam/steam`, `~/.steam/root`,
   Flatpak (`~/.var/app/com.valvesoftware.Steam/.local/share/Steam`) and Snap
   (`~/snap/steam/common/.local/share/Steam`). A root counts only if it has `steamapps/`.
2. **Libraries**: each root, plus every absolute `"path"` in `steamapps/libraryfolders.vdf`,
   with duplicates removed after canonicalising.
3. **Game folder**: `<library>/steamapps/common/Skyrim Special Edition`. A folder with
   `SkyrimSE.exe` is preferred. Failing that, one that still has a `Data` folder is accepted, which
   covers the time while Steam is verifying or re-downloading and the exe is missing.
4. **Proton AppData** (where `Plugins.txt` lives):
   `<library>/steamapps/compatdata/489830/pfx/drive_c/users/steamuser/AppData/Local/Skyrim Special Edition`.
   If that folder doesn't exist yet but the prefix does (the game has never been launched), Skald
   uses that path anyway and creates it when it saves.

Auto-detection only fills in settings that are empty. Anything set in **Settings** takes priority.
The Settings dialog only allows saving when the chosen folder contains `SkyrimSE.exe`.

---

## 4. Case-insensitive paths (`paths.rs`)

Skyrim and mod authors assume Windows: case-insensitive names and `\` separators. Linux has
neither, so every path that touches mod files goes through these helpers:

| Function | Purpose |
|---|---|
| `normalize_rel` | `\` → `/`, drops empty, `.` and **`..`** parts, so an archive or FOMOD can't write outside its target |
| `key` | Lower-cased `/`-joined form, used to compare paths and find conflicts |
| `find_child_ci` | Finds a direct child by name, ignoring case (exact match tried first) |
| `resolve_ci` | Resolves a relative path, matching each part that already exists case-insensitively |
| `resolve_ci_create` | Same, but creates missing folders and records them so purge can remove them |
| `copy_merge` | Recursive copy that merges into existing folders, ignoring case |
| `sanitize_name` | Makes a mod name safe as a folder name |

The result: a mod's `textures/actors/` merges into an existing `Textures/Actors/` instead of
creating a second folder that the game (through Wine) might or might not read.

---

## 5. Install pipeline

```
archive or folder
   │  archive::extract           (background thread)
   ▼
~/.local/share/skald/tmp/<id>/x/
   │  installer::detect          → InstallerKind
   ▼
install dialog (install_ui.rs)    user makes choices
   │  build_ops                  → Vec<CopyOp>
   ▼
installer::execute               → tmp/staging-<id>/{data,root}
   │  rename (or copy) into place
   ▼
mods/<name>/{data,root}          + entry in modlist.json (enabled)
```

### 5.1 Extraction (`archive.rs`)

The format is identified by its first bytes, with the file extension as a fallback:

| Format | Magic bytes | Extractor |
|---|---|---|
| Zip | `PK\x03\x04` / `PK\x05\x06` | `zip` crate; `\` in entry names is normalised |
| 7z | `7z\xBC\xAF\x27\x1C` | `sevenz-rust2` |
| RAR | `Rar!\x1A\x07` | `unrar x -o+ -y -idq` subprocess; clear error if `unrar` is missing |
| Folder | (is a directory) | `copy_merge` |

Several dropped archives are queued and installed one at a time.

### 5.2 Detection (`installer::detect`)

Checks run in this order. The first match wins.

1. **FOMOD**: a `fomod/` folder (any case) at the top or up to 3 folders deep.
   - Contains `ModuleConfig.xml` → parse it. If it has steps or required files →
     `Fomod`. If it's empty (some archives ship only `info.xml`) → treated as a plain archive.
     If it can't be parsed → the install fails with an error.
   - Contains only `script.cs` → `FomodScript`.
2. **Unwrap**: go down through single wrapper folders (`MyMod-1234-1-0/MyMod/…`) up to 8 levels.
   Docs and images (`txt md pdf url html rtf doc(x) jpg png webp`) don't count when deciding
   whether a folder only wraps another. The descent stops at `Data` or `fomod`.
3. **Game root**: a known root file (`skse64_loader.exe`, `d3d11.dll`, `dxgi.dll`, `dinput8.dll`,
   `enbseries.ini`, `reshade.ini`, `version.dll`, `winhttp.dll`, …) or any `.exe`/`.dll` at the top.
4. **Data under a `Data` folder**: the top doesn't look like data, but it has a `Data/` subfolder.
5. **Data**: the top contains a known Data folder (`meshes textures scripts interface sound skse
   strings seq …`, about 40 names) or a file ending `.esp .esm .esl .bsa .ini .seq .modgroups`.
6. **BAIN**: 2 or more subfolders named like `NN<sep>…` (at least two digits, then a space, `_`, `-`
   or `.`).
7. **Unknown**: anything else.

| `InstallerKind` | Label in mod list | Menu |
|---|---|---|
| `Fomod` | FOMOD | Wizard |
| `FomodScript` | FOMOD (C# script) | Manual folder picker (C# scripts aren't run) |
| `Bain` | BAIN | Package checklist; packages starting `00` are preselected |
| `Data` | Simple | None: installed straight away (`Session::quick_install`). File list + confirm only when the name clashes with an existing mod |
| `GameRoot` | Game root | File list + confirm; `Data/` → data, everything else → root |
| `Unknown` | Manual | Folder tree (4 levels deep, `fomod` hidden); first folder that looks like Data is preselected and shown green |

### 5.3 Install plans

Each menu produces a list of `CopyOp { src, dest_rel, target: Data | GameRoot }`.
`installer::execute` copies them into a staging folder, then **deletes any `fomod/` folder that
ended up in `data/`** so the installer files are never deployed. The staging folder is renamed to
`mods/<name>`, falling back to a copy if the rename fails. The dialog can replace a mod that has the
same name or install a second copy (`Name (2)`).

The default mod name comes from the Nexus filename: `SkyUI_5_2_SE-12604-5-2SE-1573147237.7z` →
`SkyUI 5 2 SE`. A FOMOD's `info.xml` adds its version and author.

---

## 6. FOMOD support (`fomod.rs`)

### Parsing
- Config encoding: UTF-8, UTF-16 LE/BE, with or without a BOM. Many FOMODs are UTF-16.
- `info.xml`: name, author, version.
- `moduleName`, `moduleImage`, `moduleDependencies`, `requiredInstallFiles`,
  `installSteps` (`order` = Ascending / Descending / Explicit), `optionalFileGroups`, `plugins`,
  `conditionalFileInstalls`.
- Groups: `SelectExactlyOne`, `SelectAtMostOne`, `SelectAtLeastOne`, `SelectAll`, `SelectAny`.
- Plugins: description, image, `conditionFlags`, files/folders, `typeDescriptor`
  (simple `type`, or `dependencyType` with `defaultType` + `patterns`).
- Files: `source`, `destination`, `priority`, `alwaysInstall`, `installIfUsable`.

### Conditions
`Dependency` is a tree: `Composite { And | Or }`, `File { file, state }`, `Flag { flag, value }`,
`Version` (version checks always pass).

File states come from `GameEnv`. It looks for the file in the game's `Data` folder and in the
`data/` folder of every **enabled** mod, ignoring case. A plugin file that exists but isn't active
in the load order counts as `Inactive`. This lets wizards that ask "do you have mod X?" answer
correctly before anything has been deployed.

### Wizard state machine
- Choices are stored per step (`selected[step][group][plugin]`), so going **Back** keeps them.
- Flags are computed from the steps visited so far. A step's `visible` condition uses the flags from
  steps **before** it.
- `init_step` applies defaults the first time a step is shown. Required and Recommended options are
  ticked. `SelectAll` gets every option. `SelectExactlyOne`/`SelectAtMostOne` keep only the first
  ticked option, and `SelectExactlyOne`/`SelectAtLeastOne` fall back to the first usable option if
  nothing is ticked.
- **Locked** options: `Required` (always on), `NotUsable` (always off), and every option in a
  `SelectAll` group.
- `validation_errors` stops **Next** until each group's rule is met.
- `files_to_install` gathers required files and, from the steps actually visited, the files of selected options, `alwaysInstall` files,
  `installIfUsable` files whose option isn't `NotUsable`, and matching `conditionalFileInstalls`.
  These are then sorted by `priority`, keeping document order within the same priority, so
  higher-priority files overwrite lower ones.
- Files the XML lists but that are missing from the archive are skipped and logged to stderr.
  If nothing at all would be installed, the install is refused.

---

## 7. Mod list and conflicts (`mods.rs`)

- Order = priority. **Mods lower in the list overwrite mods above them.**
- `conflicts()` walks enabled mods in order and records which mod owns each `data/…` and `root/…`
  path (by `key`). Each mod gets a pair `(overrides, overridden)`, shown as `+N` (green) and `-N`
  (red) in the **Conflicts** column.
- Any change to the mod list (enable, reorder, install, remove) saves `modlist.json`, rebuilds the
  plugin list, and shows **Changes not deployed**. The game folder isn't touched until **Deploy**.

---

## 8. Deploy and purge (`deploy.rs`)

### Deploy
1. **Purge** the previous deployment first, so every deploy starts clean.
2. For each enabled mod in order, list `data/` and `root/`. Later mods replace earlier ones for the
   same path (compared ignoring case).
3. For each winning file:
   - Resolve the target in `Data/` or the game folder with `resolve_ci_create`, which reuses
     existing folders and records the ones it creates.
   - If a file already exists there (a vanilla file, or something you put there by hand), **move**
     it to `backups/{data|root}/<rel>`.
   - **Add the entry to the manifest before creating the link**, so a deploy that crashes halfway
     can still be purged.
   - Link it using the chosen method:

| Method | Behaviour |
|---|---|
| Hardlink (default) | `hard_link`; falls back to a symlink when the game is on a different filesystem from `~/.local/share` |
| Symlink | Always a symlink |
| Copy | Full copy (uses more disk space, fully independent of the staging folder) |

4. Write `deployment.json` (`method`, `files[{target, source, backup}]`, `created_dirs`), even if
   the deploy failed partway.

### Purge
In reverse order, for each manifest entry:
- **Is the target still ours?** For a symlink: it points to the recorded source. For a hard link:
  same device and inode as the source. For Copy: same size.
  - Yes → delete it and restore the backup, if there is one.
  - No (something else changed it, e.g. a Steam update) → **leave it in place**, rename the backup to
    `<file>.skald-original`, and log a warning. Skald never deletes a file it didn't put there.
- Remove the folders Skald created (only if they're empty), then delete `backups/` and the manifest.

Staging folders under `mods/` are never changed by deploy or purge.

---

## 9. Plugins (`plugins.rs`)

- Files: `Plugins.txt` and `loadorder.txt` in the Proton AppData folder (existing files are found
  ignoring case).
- **Building the list**:
  1. Locked first: `Skyrim.esm Update.esm Dawnguard.esm HearthFires.esm Dragonborn.esm`, then every
     Creation Club plugin listed in `Skyrim.ccc`.
  2. Then the order from `loadorder.txt`, then `Plugins.txt`.
  3. Then any new plugins (in `Data/` or at the top of an enabled mod's `data/`, even before it's
     deployed), sorted by name and **enabled by default**.
  4. Locked plugins, then masters, then regular plugins. This matches what the engine enforces.
- **Flags**: from the TES4 header. `0x1` or `.esm`/`.esl` = master (ESM), `0x200` or `.esl` = light (ESL).
  The panel shows active / full / light counts.
- **Reordering**: a plugin can only swap with a neighbour of the same kind (master with master,
  regular with regular). Locked plugins can't be moved or disabled.
- **Saving**: happens immediately on every change (no Deploy needed). Output is CRLF, `*` marks an
  active plugin, `Plugins.txt` gets Bethesda's comment header, and `loadorder.txt` lists every plugin.

- **Unmanaged plugins**: after building the list, `App::refresh` calls `LoadOrder::mark_unmanaged`
  with the plugins Skald manages: the file names of `deployment.json` targets directly in `Data/`,
  plus the top-level plugins of enabled mods. Every other non-locked plugin is flagged `unmanaged`
  and shown with an **UNMANAGED** pill.
- **Remove unmanaged**: `plugins::move_out` moves those plugin files (looked up ignoring case) into a
  new `~/.local/share/skald/unmanaged/<unix time>/` folder. It falls back to copy and then remove
  when `rename` fails across filesystems. Then the list is rebuilt and saved, which drops them from
  `Plugins.txt`/`loadorder.txt`. Only the plugin files are moved, not their BSAs or loose assets.
  This folder is separate from `backups/` because purge deletes `backups/`.

- **Paths**: `PluginEntry::path` is the plugin file: in `Data`, or in an enabled mod's staging
  `data/` folder when it isn't deployed yet.

### Sorting (`loot.rs`)

**Sort (LOOT)** on the Plugins tab runs a background job (`App::sort_plugins`, `JobResult::Sorted`):

1. `loot::update_masterlist` downloads `MASTERLIST_URL`
   (`github.com/loot/skyrimse`, branch `v0.29`) and `PRELUDE_URL` (`github.com/loot/prelude`, `v0.29`)
   into `~/.local/share/skald/loot/`. Each file goes through `.yaml.tmp` and a rename. If the download
   fails but both cached files exist, it carries on with them and the log says so. With no cached
   copy, the sort fails. The `v0.29` branch matches the libloot 0.29 metadata syntax. Change both
   together.
2. `loot::sort` creates `Game::with_local_path(GameType::SkyrimSE, game_dir, appdata_dir)`. It doesn't
   use `Game::new`, because that can't find the local folder on Linux. It loads the masterlist and
   prelude, drops files that fail `Game::is_valid_plugin` (with a warning, because one bad header would
   fail the whole `load_plugins` call), and then fully loads every plugin by **absolute path**. A full
   load, not headers only, gives LOOT the record overlaps it needs, and absolute paths mean undeployed
   plugins are sorted too. Then `Game::sort_plugins` runs with the current Skald order as the starting order.
   libloot writes nothing. `set_load_order` isn't used.
3. Messages: general messages, then each plugin's `messages()` and the `dirty_info()` entries whose CRC
   matches the loaded plugin. All are evaluated, without user metadata, and in English through
   `select_message_content`. `loot::plain` strips the Markdown (`**`, `[text](url)` → `text (url)`).
   The GUI logs errors in red and everything else as info. It skips messages for locked plugins,
   mostly "needs cleaning" notes on the base game and Creation Club, which can't be changed here.
4. `LoadOrder::apply_order` reorders to match the result, ignoring case. Unknown names keep their
   relative order at the end. Then `sort_masters_first` (a stable sort) keeps locked plugins first and
   masters before regular plugins. Enabled flags aren't touched. The order is then saved like any other change.

The full plugin load takes about a second for 80 plugins in a release build.

### Launching (`launch.rs`)

The menu next to **Play** lists the runners: **Steam** (always), **SKSE** (when
`skse64_loader.exe` is in the game folder, matched ignoring case), then the programs in
`config.runners`. The choice is saved as `config.runner` (`None` = Steam). If the saved runner is
no longer available, Steam is used.

- **Steam**: `xdg-open steam://rungameid/489830`.
- **Anything else**: `<proton>/proton run <exe>`, with the working directory set to the exe's
  folder and these environment variables: `STEAM_COMPAT_DATA_PATH=<lib>/steamapps/compatdata/489830`,
  `STEAM_COMPAT_CLIENT_INSTALL_PATH=<steam root>`, and `SteamAppId`/`SteamGameId=489830`. Output
  is discarded, and a thread waits on the child so it doesn't become a zombie.
- **Proton folder**: `config.proton` if set. Otherwise it's read from `compatdata/489830/config_info`,
  where Proton writes its own `<proton>/files/...` paths (this is the build Steam last used for
  Skyrim). Otherwise the first installed one found in `compatibilitytools.d/` or `steamapps/common/`.
- **Prefix**: an ancestor of `appdata_dir` named `489830` that contains `pfx/`. Otherwise it's
  searched for in the Steam libraries.
- Skyrim needs the Steam client running. Before starting a non-Steam runner, Skald checks
  `~/.steam/steam.pid` and logs a warning if Steam isn't running.
- Runs outside the Steam Linux Runtime container. This is fine for SKSE on most distros.

**Tools tray**: `config.tools` (same `CustomRunner { name, exe }` shape) is a separate list from the
Play runners. Each tool is run with `launch::launch` as a `Runner::Exe`, so it gets the same Proton,
prefix and environment as above, but without the Steam-running warning (tools don't need it).
The tray's file picker opens in the game's `Data` folder, where mod tools such as BodySlide end up
after deploy. The tray stores the exe path as given and doesn't track which mod it came from.

Icons come from `exeicon::extract`. It parses the PE headers to find the resource section,
takes the first `RT_GROUP_ICON`, picks the entry closest to 32 px (higher bit depth breaks ties),
loads the matching `RT_ICON`, and wraps it in a one-image `.ico` that the `image` crate decodes
(PNG or BMP entries). Exes with no icon show the first letter of their name. The textures are
cached per exe path in `App::tool_icons`. While an exe is missing, nothing is cached and the button
gets a red slash, so the icon shows up once the mod is deployed.

---

## 10. GUI (`app.rs`, `install_ui.rs`)

- **Layout**: top bar (Play and its runner menu, Deploy, Purge, then the tools tray on the left; job spinner or deploy
  status, then Settings on the far right; square buttons with no gaps, groups split by spacing), then the
  main area with a **Mods** / **Plugins** tab strip (`Tab` in `app.rs`; each tab shows its active count, and
  the open list's summary sits on the right of the strip, next to **Enable all** / **Disable all**:
  `set_all_enabled` acts on the open tab; for mods it goes through `mods_changed` (so the state becomes
  "Changes not deployed") and is disabled while a job runs; for plugins it skips locked base-game plugins and saves `Plugins.txt` straight away). The Plugins tab
  shows two `CollapsingHeader` groups in one scroll area: the locked plugins (always a prefix of the load
  order, so `0..split`) as "Base game & DLC", closed by default, and the rest as "Mods". Each group is its
  own `plugin_table` (no inner scroll) indexing into the shared `LoadOrder`, and the header shows active/total. At the bottom the selected mod's details plus a log (errors in red).
- **Look**: `theme.rs` sets up a dark theme for every system theme (`ctx.set_theme(Dark)`). The
  palette is slate panels with frost-blue (selection, ESM, FOMOD) and gold (Play, Install) accents.
  It also sets square corners, larger spacing and text sizes, and provides helpers: `pill` (tags such as
  ESM/ESL, installer type, deploy status), `tab`, `card`, `accent_button`, `icon_button`.
  The mod and plugin lists are `egui_extras::TableBuilder` tables: striped, with row selection
  (click a mod row to select it, right-click for its menu) and virtual scrolling. Dragging files over
  the window shows a full-window overlay painted on the foreground layer.
- **Background jobs**: extraction, install, deploy and purge each run on their own thread and send
  back a `JobResult` over an `mpsc` channel. Only one job runs at a time, and buttons that would
  start another are disabled meanwhile. Queued archives start when the previous install finishes.
- **Play**: starts the runner picked in the menu next to it (Steam, SKSE or a custom `.exe`).
  See *Launching* in §9.
- **Installing**: drag and drop is the only way in. Archives or extracted folders dropped on the
  window go into the install queue.
- **Settings dialog**: also shows the mods folder with an **Open** button (`xdg-open`).
- **Right-click a mod**: Rename…, Open folder (`xdg-open`), Remove (asks first, deletes the staging
  folder).

---

## 11. Building, tests and diagnostics

```sh
cargo run --release                          # GUI
cargo test                                   # unit + end-to-end tests
cargo run --example inspect -- a.7z b.zip    # read-only: game setup + installer type per archive
```

| Test | Covers |
|---|---|
| `paths::normalize_strips_junk` | `\` handling, `..` removal, keys |
| `paths::ci_resolution_merges` | Merging into existing folders regardless of case, created-dir tracking |
| `installer::detects_kinds` | Data-under-wrapper, game root, BAIN, FOMOD (lower-case `moduleconfig.xml`), Unknown |
| `installer::guesses_nexus_names` | Nexus filename → mod name |
| `fomod::parses_utf16_and_structure` | UTF-16 config, steps, groups, plugin types |
| `plugins::apply_order_keeps_locked_and_masters_first` | LOOT result applied, locked and master blocks kept |
| `loot::sorts_with_masterlist_rules` | Real libloot sort with a local masterlist: `after` rule, undeployed plugin by absolute path, invalid plugin skipped, `warn` message |
| `loot::markdown_to_plain` | Markdown cleanup of LOOT messages |
| `fomod::wizard_flow_with_flags` | Defaults, flags, visibility conditions, conditional installs |
| `deploy::deploy_and_purge_roundtrip` | All three methods, conflict winner, case merge, backups restored |
| `end_to_end::fomod_zip_install_and_deploy` | Real zip → detect → wizard → execute → deploy → purge |

`inspect` never writes to the game or Skald's data. It extracts to `$TMPDIR/skald-inspect-<pid>`
and deletes that folder afterwards.

---

## 12. Known limitations

- Skyrim SE only (Steam app 489830). No other games, no GOG or Epic builds.
- C# script FOMODs aren't run; you pick the data folder manually.
- FOMOD `version` conditions always pass.
- No Nexus "Download with Manager" (nxm://) links, no profiles, no BSA unpacking.
- LOOT sorting uses the masterlist only: no user metadata, groups editor or Bash Tags, and the
  masterlist is always fetched from the fixed `v0.29` URLs (no setting for another source).
- Plugins are only found at the top level of `Data` (that's also the only place the game loads
  them from).
- If Steam updates the game while mods are deployed, the changed files are kept on purge and the
  originals are saved as `.skald-original` (see §8).
