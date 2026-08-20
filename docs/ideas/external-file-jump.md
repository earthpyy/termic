# In-terminal jumps to out-of-project files

Not approved, not built. Discussion in
[#240](https://github.com/simion/termic/issues/240).

## The idea

Cmd+click resolves file-path references in a terminal today (#117), but only
inside the active task. Extend it to any **absolute** path on disk, opening in
the in-app editor, fully editable.

It also fixes a live bug. `PATH_TOKEN_RE` (`src/lib/termLinkOpener.ts:36`)
cannot capture a leading `/`, so `/Users/me/…/a.ts` is scraped as the *token*
`Users/me/…/a.ts`. That underlines, so it looks live, and then every fallback in
`handlePathTarget` misses (the clicked path is a superset of `src/lib/a.ts`, not
a suffix) and the user gets "No matches". This hits absolute paths pointing
**inside the current worktree** too, which is the most common thing an agent
prints.

## Decisions

**Detection.** Optional leading `/` or `~/`. The mandatory extension stays: it
is what stopped "and/or" and "TCP/IP" underlining in #117, so `/etc/hosts` still
will not match. Extension-less absolute paths are separate tuning.

**Inside the task wins.** A path under `task.path` or a `composition[].path`
relativizes and opens as a normal `EditTab`, keeping git, diff, review comments
and tree highlight. The containment test lives in **Rust**, not TS: macOS hands
out `/tmp/…` for worktrees that canonicalize to `/private/tmp/…`, so a
`startsWith` in the frontend would mis-route them.

**Full parity outside.** Text → editor, image/PDF → `PreviewPane`, directory →
`DirTab`. Punting non-text to `open_file_external` was rejected: it would open
an external `.png` in Preview.app while the identical in-project one renders
in-app.

**Relative escapes (`../x.ts`) stay unhandled.** No trustworthy CWD exists
(the PTY spawns at `task.path`, nothing follows `cd`), and guessing would trade
a visible failure for a silent wrong-file open.

**No new setting.** #240 asks for an editor-vs-default-app pref. Declined: a
pref is easier to add than remove, and the file tree's double-click already
opens externally.

**Out of scope.** `AuxTerminal` stays `urlsOnly`; enabling it needs
`handlePathTarget` lifted into a shared hook, which should not land in the same
diff as the Rust work.

## Containment

Every file IPC is hard-scoped to the worktree by `safe_task_path`, and the
webview sits *outside* the sandbox (the "Known gap" in
[sandbox.md](../sandbox.md)). What makes that gap tolerable today is precisely
that a compromised page cannot reach past the task. This punches a hole in it.

- **Session allowlist.** A `HashSet<PathBuf>`, process lifetime, never
  persisted. Admitted only via a user gesture, canonicalized on admit and on
  use. Preserves the property that the filesystem reachable from the webview is
  only what a human pointed at. It does *not* stop an agent printing
  `~/.ssh/id_rsa` and hoping you click it. Only the click does.
- **Read and write bound differently.** Read recursively under an admitted
  path's directory (external markdown reads siblings and `./assets/*.png` with
  no gesture behind it); write only at an exactly admitted path. One rule for
  both verbs would mean a single click in `~`, where agents routinely run,
  admits write access to everything you own.
- **Six commands go dual-mode** (`task_file_read`, `_write`, `_read_base64`,
  `_fp`, `task_dir_list`, `task_path_stat`) behind an explicit `external: bool`.
  Separate `ext_*` siblings would have kept `grep safe_task_path` a complete
  audit; the maintainer chose dual-mode, so `sandbox.md` has to carry that list
  instead. **Implicit signalling must not be built**: `normalizePath` already
  strips leading slashes elsewhere, so an absolute-looking string is something
  the codebase produces by accident. The explicit flag keeps the failure mode
  "rejected" rather than "escaped".

## Surface

`EditTab` gains `external?: true`. One wrapper module owns the
scoped-vs-absolute branch so the nine IPC call sites do not each grow an `if`.
Review comments are disabled (they key repo-relative), tree highlight no-ops,
git chrome hides, `fsRevision` re-read still fires.

`EditorBreadcrumb` discloses the path, so no new chrome: plain-text segments,
`$HOME` as `~`, copy-path and open-in-file-manager stay, locate-in-tree goes.
No tab-level marker, accepting that background tabs named `config.ts` give no
signal until focused.

## Known costs

- `grep safe_task_path` stops being a complete audit. The mitigation is a doc,
  which decays.
- No git safety net outside the worktree: no diff, no Git panel, no
  `git checkout` to undo a bad save. With same-basename collisions and no tab
  marker, saving over the wrong file is the realistic failure mode.
