// @vitest-environment happy-dom
// syncPrefsFromDisk: the boot reconciliation between settings.json (source of
// truth, the file you sync between machines) and the localStorage mirror the
// store was built from. Kept out of prefs.test.ts because it needs @/lib/ipc
// partially mocked, and that file exercises the real module.
//
// localStorage is stubbed for the reason documented in prefs.test.ts.
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

const prefsSave = vi.fn(async (_patch: Record<string, unknown>) => {});
const settingsLoad = vi.fn(async () => ({}) as Record<string, unknown>);

vi.mock("@/lib/ipc", async importOriginal => ({
  ...(await importOriginal<typeof import("@/lib/ipc")>()),
  prefsSave: (patch: Record<string, unknown>) => prefsSave(patch),
  settingsLoad: () => settingsLoad(),
}));

function fakeLocalStorage() {
  const store = new Map<string, string>();
  return {
    getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
    setItem: (k: string, v: string) => { store.set(k, v); },
    removeItem: (k: string) => { store.delete(k); },
    clear: () => { store.clear(); },
  };
}

/** Seed the mirror, then load a fresh store from it — the module-eval read
 *  that happens before first paint on a real launch. */
async function bootWith(mirror: Record<string, string>) {
  vi.stubGlobal("localStorage", fakeLocalStorage());
  for (const [k, v] of Object.entries(mirror)) localStorage.setItem(k, v);
  vi.resetModules();
  return import("./prefs");
}

beforeEach(() => {
  prefsSave.mockClear();
  settingsLoad.mockReset();
  settingsLoad.mockResolvedValue({});
});
afterEach(() => { vi.unstubAllGlobals(); });

describe("syncPrefsFromDisk", () => {
  it("applies prefs synced in from another machine to the live store", async () => {
    settingsLoad.mockResolvedValue({
      prefs: { themeMode: "solarized", terminalFontSize: 16, branchPrefix: "feat" },
    });
    const { usePrefs, syncPrefsFromDisk } = await bootWith({
      themeMode: "claude", terminalFontSize: "13", branchPrefix: "wip",
    });
    // Boot state comes from the mirror, before disk is consulted.
    expect(usePrefs.getState().themeMode).toBe("claude");

    await syncPrefsFromDisk();

    expect(usePrefs.getState().themeMode).toBe("solarized");
    expect(usePrefs.getState().terminalFontSize).toBe(16);
    expect(usePrefs.getState().branchPrefix).toBe("feat");
  });

  it("does not echo the applied values back to the file they came from", async () => {
    settingsLoad.mockResolvedValue({ prefs: { branchPrefix: "feat" } });
    const { syncPrefsFromDisk } = await bootWith({ branchPrefix: "wip" });
    await syncPrefsFromDisk();
    expect(prefsSave).not.toHaveBeenCalled();
  });

  it("persists normally again after the sync", async () => {
    settingsLoad.mockResolvedValue({ prefs: { branchPrefix: "feat" } });
    const { usePrefs, syncPrefsFromDisk } = await bootWith({ branchPrefix: "wip" });
    await syncPrefsFromDisk();
    usePrefs.getState().setBranchPrefix("later");
    const { flushPrefs } = await import("@/lib/prefsPersist");
    await flushPrefs();
    expect(prefsSave).toHaveBeenCalledWith({ branchPrefix: "later" });
  });

  it("leaves the store untouched when the mirror already matches disk", async () => {
    settingsLoad.mockResolvedValue({ prefs: { terminalFontSize: 13 } });
    const { usePrefs, syncPrefsFromDisk } = await bootWith({ terminalFontSize: "13" });
    await syncPrefsFromDisk();
    expect(usePrefs.getState().terminalFontSize).toBe(13);
    expect(prefsSave).not.toHaveBeenCalled();
  });

  it("clamps a hand-edited value that's out of range", async () => {
    settingsLoad.mockResolvedValue({ prefs: { uiScale: 900, terminalScrollback: 10 } });
    const { usePrefs, syncPrefsFromDisk } = await bootWith({ uiScale: "100" });
    await syncPrefsFromDisk();
    expect(usePrefs.getState().uiScale).toBe(200);
    expect(usePrefs.getState().terminalScrollback).toBe(1000);
  });

  it("falls back to the default theme for an unknown theme id", async () => {
    settingsLoad.mockResolvedValue({ prefs: { themeMode: "nonesuch" } });
    const { usePrefs, syncPrefsFromDisk } = await bootWith({ themeMode: "dark" });
    await syncPrefsFromDisk();
    expect(usePrefs.getState().themeMode).toBe("claude");
  });

  it("lets the renderer pref win when it and the GPU boolean both moved", async () => {
    // Both keys describe one setting and each setter writes both, so applying
    // the boolean afterwards would silently undo the renderer choice.
    settingsLoad.mockResolvedValue({
      prefs: { terminalRenderer: "canvas", terminalGpuEnabled: false },
    });
    const { usePrefs, syncPrefsFromDisk } = await bootWith({
      terminalRenderer: "webgl", terminalGpuEnabled: "1",
    });
    await syncPrefsFromDisk();
    expect(usePrefs.getState().terminalRenderer).toBe("canvas");
    expect(usePrefs.getState().terminalGpuEnabled).toBe(false);
  });

  it("honours a lone GPU boolean from a file predating the renderer pref", async () => {
    settingsLoad.mockResolvedValue({ prefs: { terminalGpuEnabled: false } });
    const { usePrefs, syncPrefsFromDisk } = await bootWith({
      terminalRenderer: "webgl", terminalGpuEnabled: "1",
    });
    await syncPrefsFromDisk();
    expect(usePrefs.getState().terminalGpuEnabled).toBe(false);
    expect(usePrefs.getState().terminalRenderer).toBe("dom");
  });

  it("restores shortcut overrides, merged onto the current defaults", async () => {
    settingsLoad.mockResolvedValue({
      prefs: { shortcutBindings: { "task-next": { cmd: true, shift: true, alt: false, key: "j" } } },
    });
    const { usePrefs, syncPrefsFromDisk } = await bootWith({ shortcutBindings: "{}" });
    await syncPrefsFromDisk();
    const { shortcuts } = usePrefs.getState();
    expect(shortcuts["task-next"]).toEqual({ cmd: true, shift: true, alt: false, key: "j" });
    // Commands absent from the blob keep their factory binding.
    expect(shortcuts["task-prev"]).toBeDefined();
  });

  it("maps a bad allow-scope back to 'never chosen' rather than a bogus scope", async () => {
    settingsLoad.mockResolvedValue({ prefs: { sandboxAllowScope: "wat" } });
    const { usePrefs, syncPrefsFromDisk } = await bootWith({ sandboxAllowScope: "repo" });
    await syncPrefsFromDisk();
    expect(usePrefs.getState().allowScope).toBeNull();
  });

  it("survives an unreachable backend with the mirror's values intact", async () => {
    settingsLoad.mockRejectedValue(new Error("no backend"));
    const { usePrefs, syncPrefsFromDisk } = await bootWith({ branchPrefix: "wip" });
    await expect(syncPrefsFromDisk()).resolves.toBeUndefined();
    expect(usePrefs.getState().branchPrefix).toBe("wip");
  });
});
