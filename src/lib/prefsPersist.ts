// Disk persistence for the UI preferences in store/prefs.ts.
//
// `settings.json` (`prefs` object, Rust-owned file) is the SOURCE OF TRUTH —
// it's the thing you sync between machines. localStorage is a synchronous
// MIRROR of it, and exists for exactly one reason: prefs.ts computes ~40
// initial values at module-eval time (theme, uiScale, fonts are applied
// before the first frame) and `invoke()` is async. Reading the mirror keeps
// first paint free of both a default-theme flash and an IPC round trip.
//
// The flow:
//   write  →  mirror synchronously, disk debounced (see FLUSH_MS)
//   boot   →  paint from mirror, then hydrate from disk and apply any diff
//
// Same first-paint-cache trick lib/customTheme.ts already uses for the active
// custom theme payload.
//
// The mirror keeps prefs.ts's existing string encoding ("1"/"0", String(n)),
// so every reader in that file works unchanged and old profiles still parse.
// Disk gets real JSON types instead — a synced config file should read
// `"terminalGpuEnabled": true`, not `"1"`. PREF_KINDS is what maps between
// the two, and is the single registry of which keys are disk-backed.

import { prefsSave, settingsLoad } from "@/lib/ipc";

export type PrefKind = "bool" | "num" | "str" | "json";

/** Every disk-backed pref key, and how it encodes. Keys are the localStorage
 *  names from store/prefs.ts, reused verbatim as the `prefs` object's keys so
 *  the two sides never need a translation table.
 *
 *  Adding a pref: add its key here and route its setter through `savePref`.
 *  A key absent from this map stays localStorage-only (per-machine cache:
 *  layout widths, viewed-file marks, update watermarks). */
export const PREF_KINDS = {
  // Appearance
  editorFont:             "str",
  editorThemeId:          "str",
  editorThemeIdLight:     "str",
  terminalFont:           "str",
  terminalFontSize:       "num",
  editorFontSize:         "num",
  uiScale:                "num",
  codeLigatures:          "bool",
  showAllInstalledFonts:  "bool",
  themeMode:              "str",
  // Terminal
  terminalLetterSpacing:  "num",
  terminalScrollback:     "num",
  terminalOptionAsMeta:   "bool",
  terminalGpuEnabled:     "bool",
  terminalRenderer:       "str",
  terminalCopyOnSelect:   "bool",
  // Notifications
  desktopNotifications:   "bool",
  completionSound:        "bool",
  completionSoundId:      "str",
  settledHighlight:       "bool",
  workingIndicator:       "bool",
  // Behavior
  confirmBeforeCloseAgentTab: "bool",
  loadRemoteImages:       "bool",
  taskExpandMode:         "str",
  hideInactiveProjects:   "bool",
  markdownDefaultView:    "str",
  branchPrefix:           "str",
  queueMinIntervalMs:     "num",
  splitPaneDim:           "bool",
  splitPaneDimAmount:     "num",
  // Sandbox defaults
  globalDefaultSandbox:   "bool",
  sandboxBypassPermissions: "bool",
  sandboxAllowScope:      "str",
  // Keybindings (whole BindingMap as one object)
  shortcutBindings:       "json",
} as const satisfies Record<string, PrefKind>;

export type PrefKey = keyof typeof PREF_KINDS;

const isPrefKey = (k: string): k is PrefKey => k in PREF_KINDS;

/** Mirror encoding -> JSON value for the file. */
function toDisk(kind: PrefKind, raw: string): unknown {
  switch (kind) {
    case "bool": return raw === "1";
    case "num":  return Number(raw);
    case "str":  return raw;
    case "json": try { return JSON.parse(raw); } catch { return undefined; }
  }
}

/** JSON value from the file -> mirror encoding. Returns undefined when the
 *  stored value has the wrong shape, so a hand-mangled or foreign file leaves
 *  that one pref at its default instead of poisoning the mirror. */
function toMirror(kind: PrefKind, v: unknown): string | undefined {
  switch (kind) {
    case "bool": return typeof v === "boolean" ? (v ? "1" : "0") : undefined;
    case "num":  return typeof v === "number" && Number.isFinite(v) ? String(v) : undefined;
    case "str":  return typeof v === "string" ? v : undefined;
    case "json": return v !== null && typeof v === "object" ? JSON.stringify(v) : undefined;
  }
}

// ── Writing ──

// Trailing debounce. Font size, letter spacing and UI scale are slider-driven
// and fire per tick; the mirror absorbs those for free but each disk write
// rewrites the whole settings.json. Kept short because the mirror is only a
// cache of disk (see hydratePrefs): anything not flushed before the process
// dies is lost, not recovered from localStorage.
const FLUSH_MS = 250;

const pending = new Map<PrefKey, unknown>();
let timer: ReturnType<typeof setTimeout> | null = null;
/** Set while hydratePrefs applies disk values, so replaying them through the
 *  store's setters doesn't echo straight back to the file it just read. */
let suppressed = false;

/** Persist one pref: mirror now, disk shortly. `raw` is the localStorage
 *  encoding, exactly what store/prefs.ts already writes. */
export function savePref(key: PrefKey, raw: string): void {
  try { localStorage.setItem(key, raw); } catch {}
  if (suppressed) return;
  const encoded = toDisk(PREF_KINDS[key], raw);
  if (encoded === undefined) return;
  pending.set(key, encoded);
  if (timer === null) timer = setTimeout(() => { void flushPrefs(); }, FLUSH_MS);
}

/** Write every queued pref now. Safe to call with nothing pending. */
export async function flushPrefs(): Promise<void> {
  if (timer !== null) { clearTimeout(timer); timer = null; }
  if (pending.size === 0) return;
  const patch = Object.fromEntries(pending);
  pending.clear();
  // Dropping the patch on failure is deliberate: the mirror still holds the
  // value, so the UI stays correct, and the next pref change re-flushes.
  // Retrying here would just pile writes onto an already-failing file.
  try { await prefsSave(patch); } catch {}
}

// ── Boot ──

/** Read `settings.json`'s prefs and reconcile them with the mirror.
 *
 *  Returns the keys whose disk value differs from what the mirror held at
 *  boot, already written into the mirror, for the caller to apply to the live
 *  store. Normally empty (mirror was already current), so a launch does no
 *  visible work. It's non-empty when the file was synced in from another
 *  machine, which is the case this whole module exists to serve.
 *
 *  First run on an existing profile finds no `prefs` on disk and seeds it from
 *  the mirror, so nobody loses their settings to the move. */
export async function hydratePrefs(): Promise<Partial<Record<PrefKey, string>>> {
  let disk: Record<string, unknown>;
  try {
    disk = (await settingsLoad()).prefs ?? {};
  } catch {
    return {}; // Backend unreachable: mirror-only is a fine degraded mode.
  }

  const keys = Object.keys(disk).filter(isPrefKey);
  if (keys.length === 0) {
    seedFromMirror();
    return {};
  }

  const changed: Partial<Record<PrefKey, string>> = {};
  for (const key of keys) {
    const next = toMirror(PREF_KINDS[key], disk[key]);
    if (next === undefined) continue;
    let current: string | null = null;
    try { current = localStorage.getItem(key); } catch {}
    if (current === next) continue;
    try { localStorage.setItem(key, next); } catch {}
    changed[key] = next;
  }
  return changed;
}

/** Upload whatever the mirror already holds. Runs once, when a profile that
 *  predates disk-backed prefs first launches. */
function seedFromMirror(): void {
  for (const key of Object.keys(PREF_KINDS) as PrefKey[]) {
    let raw: string | null = null;
    try { raw = localStorage.getItem(key); } catch {}
    if (raw === null) continue; // never set: leave it to the code defaults
    const encoded = toDisk(PREF_KINDS[key], raw);
    if (encoded !== undefined) pending.set(key, encoded);
  }
  void flushPrefs();
}

/** Replay disk values through the store's own setters without echoing them
 *  back to disk. */
export function withoutPersist(fn: () => void): void {
  suppressed = true;
  try { fn(); } finally { suppressed = false; }
}

/** Don't lose the last few hundred ms of edits when the window goes away.
 *  Best effort — `invoke` is async and teardown may outrun it, which is why
 *  FLUSH_MS is short rather than relying on this. */
if (typeof window !== "undefined") {
  window.addEventListener("pagehide", () => { void flushPrefs(); });
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") void flushPrefs();
  });
}
