// @vitest-environment happy-dom
// prefsPersist keeps module-level state (the pending patch, the debounce
// timer, the suppression flag), so every test gets a fresh module instance
// via vi.resetModules() + dynamic import. localStorage is stubbed for the
// same reason store/prefs.test.ts stubs it — see the note there.
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

const prefsSave = vi.fn(async (_patch: Record<string, unknown>) => {});
const settingsLoad = vi.fn(async () => ({}) as Record<string, unknown>);

vi.mock("@/lib/ipc", () => ({
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

const load = () => import("./prefsPersist");

beforeEach(() => {
  vi.stubGlobal("localStorage", fakeLocalStorage());
  vi.resetModules();
  prefsSave.mockClear();
  settingsLoad.mockReset();
  settingsLoad.mockResolvedValue({});
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("savePref", () => {
  it("writes the mirror synchronously, before any flush", async () => {
    const { savePref } = await load();
    savePref("branchPrefix", "wip");
    expect(localStorage.getItem("branchPrefix")).toBe("wip");
    expect(prefsSave).not.toHaveBeenCalled();
  });

  it("debounces a burst into one disk write carrying the last value", async () => {
    vi.useFakeTimers();
    const { savePref } = await load();
    // A slider drag: many writes, one file rewrite.
    for (const px of [12, 13, 14, 15]) savePref("terminalFontSize", String(px));
    await vi.advanceTimersByTimeAsync(500);
    expect(prefsSave).toHaveBeenCalledTimes(1);
    expect(prefsSave).toHaveBeenCalledWith({ terminalFontSize: 15 });
  });

  it("encodes each kind as a real JSON type, not the mirror's string", async () => {
    const { savePref, flushPrefs } = await load();
    savePref("terminalGpuEnabled", "0");
    savePref("uiScale", "120");
    savePref("themeMode", "solarized");
    savePref("shortcutBindings", JSON.stringify({ "task-next": { key: "j" } }));
    await flushPrefs();
    expect(prefsSave).toHaveBeenCalledWith({
      terminalGpuEnabled: false,
      uiScale: 120,
      themeMode: "solarized",
      shortcutBindings: { "task-next": { key: "j" } },
    });
  });

  it("keeps the mirror correct when the disk write fails", async () => {
    prefsSave.mockRejectedValueOnce(new Error("read-only fs"));
    const { savePref, flushPrefs } = await load();
    savePref("branchPrefix", "wip");
    await expect(flushPrefs()).resolves.toBeUndefined();
    expect(localStorage.getItem("branchPrefix")).toBe("wip");
  });

  it("drops a failed patch instead of resending it on the next flush", async () => {
    prefsSave.mockRejectedValueOnce(new Error("read-only fs"));
    const { savePref, flushPrefs } = await load();
    savePref("branchPrefix", "wip");
    await flushPrefs();
    savePref("uiScale", "110");
    await flushPrefs();
    expect(prefsSave).toHaveBeenLastCalledWith({ uiScale: 110 });
  });
});

describe("withoutPersist", () => {
  it("mirrors but does not write to disk", async () => {
    const { savePref, withoutPersist, flushPrefs } = await load();
    withoutPersist(() => savePref("branchPrefix", "from-disk"));
    await flushPrefs();
    expect(localStorage.getItem("branchPrefix")).toBe("from-disk");
    expect(prefsSave).not.toHaveBeenCalled();
  });

  it("resumes persisting after the callback throws", async () => {
    const { savePref, withoutPersist, flushPrefs } = await load();
    expect(() => withoutPersist(() => { throw new Error("boom"); })).toThrow("boom");
    savePref("branchPrefix", "after");
    await flushPrefs();
    expect(prefsSave).toHaveBeenCalledWith({ branchPrefix: "after" });
  });
});

describe("hydratePrefs", () => {
  it("reports nothing when the mirror already matches disk", async () => {
    settingsLoad.mockResolvedValue({ prefs: { branchPrefix: "wip" } });
    const { hydratePrefs } = await load();
    localStorage.setItem("branchPrefix", "wip");
    expect(await hydratePrefs()).toEqual({});
  });

  it("overwrites a stale mirror and reports the changed keys", async () => {
    // The synced-in case: another machine's settings.json lands on disk.
    settingsLoad.mockResolvedValue({
      prefs: { branchPrefix: "feat", uiScale: 130, terminalGpuEnabled: false },
    });
    const { hydratePrefs } = await load();
    localStorage.setItem("branchPrefix", "wip");
    localStorage.setItem("uiScale", "100");
    localStorage.setItem("terminalGpuEnabled", "1");
    const changed = await hydratePrefs();
    expect(changed).toEqual({ branchPrefix: "feat", uiScale: "130", terminalGpuEnabled: "0" });
    expect(localStorage.getItem("uiScale")).toBe("130");
    expect(localStorage.getItem("terminalGpuEnabled")).toBe("0");
  });

  it("ignores values stored with the wrong type", async () => {
    // A hand-mangled or foreign file must leave that pref at its default
    // rather than poison the mirror with an unparseable value.
    settingsLoad.mockResolvedValue({
      prefs: { uiScale: "130", terminalGpuEnabled: "yes", branchPrefix: 7, shortcutBindings: 3 },
    });
    const { hydratePrefs } = await load();
    localStorage.setItem("uiScale", "100");
    expect(await hydratePrefs()).toEqual({});
    expect(localStorage.getItem("uiScale")).toBe("100");
  });

  it("ignores keys it doesn't know", async () => {
    // Forward compatibility: a newer build's pref must not reach the store,
    // and prefs_save merges rather than replaces, so it survives on disk.
    settingsLoad.mockResolvedValue({ prefs: { someFuturePref: true } });
    const { hydratePrefs } = await load();
    expect(await hydratePrefs()).toEqual({});
    expect(localStorage.getItem("someFuturePref")).toBeNull();
  });

  it("degrades to mirror-only when the backend is unreachable", async () => {
    settingsLoad.mockRejectedValue(new Error("no backend"));
    const { hydratePrefs } = await load();
    localStorage.setItem("branchPrefix", "wip");
    expect(await hydratePrefs()).toEqual({});
    expect(localStorage.getItem("branchPrefix")).toBe("wip");
    expect(prefsSave).not.toHaveBeenCalled();
  });

  describe("first launch of a profile with no prefs on disk", () => {
    it("seeds the file from the mirror instead of wiping it", async () => {
      const { hydratePrefs } = await load();
      localStorage.setItem("branchPrefix", "wip");
      localStorage.setItem("terminalFontSize", "15");
      expect(await hydratePrefs()).toEqual({});
      expect(prefsSave).toHaveBeenCalledWith({ branchPrefix: "wip", terminalFontSize: 15 });
      expect(localStorage.getItem("branchPrefix")).toBe("wip");
    });

    it("seeds only the keys actually set, leaving the rest to code defaults", async () => {
      const { hydratePrefs } = await load();
      localStorage.setItem("branchPrefix", "wip");
      await hydratePrefs();
      expect(prefsSave).toHaveBeenCalledWith({ branchPrefix: "wip" });
    });

    it("writes nothing at all on a fresh install", async () => {
      const { hydratePrefs } = await load();
      expect(await hydratePrefs()).toEqual({});
      expect(prefsSave).not.toHaveBeenCalled();
    });

    it("does not seed once the file has prefs, even unrelated ones", async () => {
      settingsLoad.mockResolvedValue({ prefs: { uiScale: 100 } });
      const { hydratePrefs } = await load();
      localStorage.setItem("uiScale", "100");
      localStorage.setItem("branchPrefix", "wip");
      await hydratePrefs();
      expect(prefsSave).not.toHaveBeenCalled();
    });
  });
});
