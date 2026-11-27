# ADR 0001: Goal, scope and structure

Date: 2026-11-26. Status: accepted.

## Context

Skald started as a personal mod manager and grew to about 5,500 lines with no written rules on what
it is for or how the code is split. These decisions record the answers so later changes can be
checked against them.

## Goal and audience

- Skald is the mod manager the author uses every day for Skyrim Special Edition on Linux. It is also
  public as a portfolio project, so the code and docs should be readable by someone reviewing it.
  When the two goals conflict, daily use wins.
- Only the author runs it. Skald is built from source with `cargo run --release`. There are no
  releases or tags, and the version stays at `0.1.0`.

## Scope

- Skyrim SE only. If Skald ever supports more games, they will be Creation Engine games (Fallout 4,
  Skyrim LE, Starfield, Oblivion Remastered), because those share the `Data` folder, plugin files,
  `Plugins.txt`, LOOT and FOMOD.
- No groundwork for other games now. Skyrim SE assumptions get gathered into one place when a second
  game is actually being added, not before.
- Not planned: nxm:// "Download with Manager" links. Possible later: profiles, with no preparation
  now. BSA unpacking only if a mod needs it.
- `examples/inspect.rs` stays a read-only diagnostics tool. There is no command-line frontend.

## Decisions

1. **The GUI shows, the library does.** `app.rs`, `install_ui.rs` and `theme.rs` show information
   and pass on the user's actions and decisions. The library (`lib.rs` and its modules) holds state
   and does the work. Logic that has ended up in `app.rs` moves to the library over time, starting
   with code that writes to disk: the commit step of `run_install`, the `tmp/` cleanup, and Remove
   unmanaged.
2. **Library functions are synchronous.** The GUI owns threads, channels and job polling. If
   progress reporting is ever needed, the library takes a callback.
3. **The game folder is written in three places only.** Deploy, Purge and Remove unmanaged are the
   only code that writes to the game folder. Deploy records every file it places or backs up in
   `deployment.json`, so Purge can always return the game to its state before Skald. New features
   that touch `Data` or the game root go through `deploy.rs` and the manifest.
4. **Library code that writes to disk has a test.** Each such path has a `tempfile`-based test. The
   GUI has no tests by design. Pure helpers get tests when they are cheap to write.
5. **Docs change with the code.** A change to the project layout, the files Skald writes on disk,
   or the deploy and purge behaviour updates `README-TECHNICAL.md` in the same commit.

## Consequences

- `app.rs` (about 1,700 lines) is structural debt to pay down, not a pattern to follow.
- A new feature that needs disk access starts as a library function with a test, then gets a GUI.
- Adding a second game will take a refactor of `config.rs`, `plugins.rs`, `loot.rs` and `launch.rs`.
  That cost is accepted in exchange for not designing an abstraction around one game.
