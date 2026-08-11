# Data model

## Directories

Three directories, different owners:
- `~/Library/Application Support/termic/` — app-owned: `projects.json`, `tasks/`, `settings.json`. Path via `dirs::data_local_dir().join("termic")` in `lib.rs#data_dir()`.
- `~/Library/Application Support/com.simion.termic/` — tauri-plugin-window-state owned (window position/size). Path from `tauri.conf.json#identifier`.
- `~/.config/termic/themes/` — user-owned, hand-authored custom theme files ([docs/themes.md](themes.md)). `$XDG_CONFIG_HOME` respected; shared by release + dev builds (no `termic_dev` split). Path via `lib.rs#themes_dir_path()`.

## Entities

- **Project** (`projects.json`, single JSON array) — git repo path + scripts + `preview_url` template + `files_to_copy` globs + `default_cli` + optional `group` label (UI-only collapsible folder in the sidebar; no filesystem effect; a group exists iff ≥1 project carries the label. All group reads go through `groupOf()` in `src/lib/projectGroups.ts`, THE normalization point: trim + ALL-CAPS, so mixed-case labels on disk converge to one group. Collapse state + folder color live in `localStorage` keyed by normalized name, pruned when a group disappears).
- **Task** (`tasks/<uuid>.json`) — git worktree branched from project's `base_branch`. Worktrees live at `~/termic/tasks/<project>/<name>/`. `is_main_checkout=true` tasks point at the project's live checkout (no worktree, archive skips `rm -rf`). Optional `order` holds the sidebar position within the project, written by drag-to-reorder (`task_reorder`). Projects get their order from the `projects.json` array; tasks are a file each, so they need the explicit key. `load_tasks` sorts on `(order, created)` with a missing `order` LAST, which is why a project nobody has dragged still reads oldest-first and a new task appends at the bottom of a reordered one.
- **Settings** (`settings.json`) — `repos_dir`, `welcomed`, `agents[]` (claude/gemini/codex defaults + customs; each has `command`/`args`/`yolo_args`/`runtime_yolo_command`). Defaults seeded if `agents` is empty. `schema_version` gates one-time on-disk migrations.
- **UI preferences** (`settings.json`, `prefs` object) — fonts, sizes, theme, terminal renderer/scrollback, notification choices, sandbox defaults, shortcut overrides. Untyped in Rust (`serde_json::Map`, so keys stay sorted and the file diffs cleanly across machines); parsed and clamped in TS only. Written ONLY by `prefs_save`, which merges key by key, so an older or newer build never drops a key it doesn't recognize. `settings_save` deliberately discards the `prefs` it is sent and keeps disk's copy, so an unrelated Settings toggle can't roll back a pref changed since the UI loaded. See "Preferences" below.
- **Tab** (per task, in `useApp`) — `terminal` (PTY running a CLI), `edit` (CodeMirror), `diff` (vs HEAD). PTYs die with the app.

## Preferences: disk vs localStorage

`settings.json`'s `prefs` object is the source of truth (it exists to be synced
between machines). `localStorage` is a synchronous **mirror** of it, and exists
for one reason: `store/prefs.ts` computes ~40 initial values at module-eval
time and applies theme / `uiScale` / fonts before the first frame, while
`invoke()` is async. Reading the mirror keeps first paint free of both a
default-theme flash and an IPC round trip. Same trick `lib/customTheme.ts` uses
for the active custom theme payload.

- **Write** — `savePref` (`lib/prefsPersist.ts`) mirrors synchronously, then
  flushes to disk on a 250 ms trailing debounce. The debounce is what keeps a
  font-size drag from rewriting the whole file per tick.
- **Boot** — paint from the mirror, then `syncPrefsFromDisk` reconciles against
  the file and replays any difference through the store's real setters (so
  `applyTheme` / clamps still run). A no-op on a normal launch; it does work
  when the file synced in from another machine.
- **Encoding** — the mirror keeps prefs.ts's legacy string form (`"1"`/`"0"`,
  `String(n)`); disk gets real JSON types. `PREF_KINDS` maps between them and is
  the single registry of which keys are disk-backed. Adding a pref means adding
  it there and routing its setter through `savePref`; `tsc` enforces both ends
  (`savePref` takes a `PrefKey`, and the apply switch has a `never` guard).
- **Deliberately NOT disk-backed** — per-machine state that would be noise in a
  synced file: layout widths and collapse state (`store/app.ts`), viewed-file
  marks, agent races, update watermarks, prompt library. These stay
  localStorage-only, which is why a key absent from `PREF_KINDS` is a choice,
  not an oversight.
- A profile that predates this seeds the file from its mirror on first launch,
  so nothing is lost in the move. There is no file watcher: a change made to
  `settings.json` while the app runs applies on next launch.

## Migrations

The "Task" entity was called "Workspace" before, on disk and in code. A one-time
startup migration (`migrate_workspaces_to_tasks` in `lib.rs`, gated by
`settings.schema_version`) renames the metadata dir `workspaces/` → `tasks/` and
rewrites the `is_repo_root` field to `is_main_checkout` (serde `alias` still reads
the old name). It is **metadata-only**: it deliberately does NOT move worktree
directories or rewrite each task's `path`. CWD-resume agents (Claude Code's
`--continue`) resume the most recent session by working directory, so relocating a
worktree would silently orphan its history. Existing worktrees stay under
`~/termic/workspaces/…`; NEW worktrees are created under `~/termic/tasks/…`
(`worktrees_base()`), and the two roots coexist while the old one empties out
lazily as tasks are archived/recreated. The metadata rename is atomic (stage in
`tasks.tmp/`, then one `rename` into place), guarded by a `tasks-migration.lock`,
backs up to `backups/pre-tasks-<ts>/`, and prunes-on-corruption (an unparseable
record, or an active worktree whose dir was deleted externally, is dropped +
logged to `tasks-migration.log`, never carried forward). The JS half
(`src/lib/lsMigration.ts`) renames the persisted `localStorage` pref keys
(`workspaceExpandMode` → `taskExpandMode`, `collapsedWorkspaces` → `collapsedTasks`,
plus the two `newWorkspaceLast*` keys); everything else in `localStorage` is keyed
by task UUID, which never changes.
