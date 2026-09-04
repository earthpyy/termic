// GH #276: the store half of "one turn interrupts the user once".
//
// The loop that produced a banner per stage boundary runs through two store
// actions, and this file drives the real ones rather than a model of them:
//
//   markAttention("done")            -> the dot appears; the notifier sees news
//   setWorkState("working")          -> the dot is CLEARED, on purpose
//   markAttention("done", _, true)   -> the dot returns, marked as a repeat
//
// The third step is the fix. What must not regress is the second: clearing a
// premature done when the agent goes back to work is deliberate (leaving "this
// finished" on a visibly working tab is a lie), so a fix that kept the dot
// instead would have traded one wrong badge for another.

import { describe, it, expect, beforeEach } from "vitest";
import { useApp } from "@/store/app";
import { isNewsworthyUnread } from "@/lib/attentionNotify";
import { STICKY_DONE_MS } from "@/lib/agents";
import type { Task, Tab, TerminalTab } from "@/lib/types";

const task = (id: string): Task => ({
  id, project_id: "p", name: id, path: `/tmp/${id}`, branch: "main",
  cli: "claude", created_at: "", archived: false,
} as unknown as Task);

const tab = (id: string): Tab => ({
  id, type: "terminal", cli: "claude", title: id, workState: "working",
} as unknown as Tab);

const agentTab = () =>
  useApp.getState().tabs.t1.find(t => t.id === "a") as TerminalTab;

describe("a repeated done marks the dot without re-notifying", () => {
  beforeEach(() => {
    useApp.setState({
      tasks: [task("t1")],
      activeTaskId: null,      // nobody watching, or a done never badges
      tabs: { t1: [tab("a")] },
      activeTab: { t1: "a" },
      splitTree: {}, activePaneId: {},
    } as never);
  });

  it("stores `repeat` only when asked, and never as false", () => {
    const app = useApp.getState();
    app.markAttention("t1", "a", "done");
    // Absent rather than `false`: an unread record is compared and snapshotted
    // all over the place, and a field that never changes behaviour should not
    // appear in any of it.
    expect(agentTab().unread).toEqual({ reason: "done" });
    expect("repeat" in (agentTab().unread as object)).toBe(false);

    app.markAttention("t1", "a", "done", undefined, true);
    expect(agentTab().unread).toEqual({ reason: "done", repeat: true });
  });

  it("keeps carrying the agent's own wording alongside the flag", () => {
    useApp.getState().markAttention("t1", "a", "attention", "Claude needs your permission", true);
    expect(agentTab().unread).toEqual({
      reason: "attention",
      message: "Claude needs your permission",
      repeat: true,
    });
  });

  it("reproduces the whole two-stage turn: two dots, one notification", () => {
    const app = () => useApp.getState();
    const news: boolean[] = [];
    let prev: Tab | undefined;
    const observe = () => {
      const now = agentTab();
      news.push(isNewsworthyUnread(now) && !isNewsworthyUnread(prev));
      prev = { ...now };
    };

    // Stage 1 looks finished.
    app().setWorkState("t1", "a", "done");
    app().markAttention("t1", "a", "done");
    observe();
    expect(agentTab().unread).toBeTruthy();

    // Age the done past STICKY_DONE_MS. Inside that window a "back to working"
    // is REFUSED as spinner flicker (see the sticky-done gate in setWorkState),
    // which is exactly why the storm needs a long turn: the lap time is
    // STICKY_DONE_MS + however long the agent then stays quiet. Rewriting
    // `workDoneAt` is how the age is faked without a fake clock, and it is the
    // real field the gate reads.
    useApp.setState({
      tabs: {
        t1: useApp.getState().tabs.t1.map(t =>
          t.id === "a" ? { ...t, workDoneAt: Date.now() - (STICKY_DONE_MS + 1_000) } : t),
      },
    } as never);

    // Back to work. The store clears the premature done ITSELF - this is the
    // step that makes the next mark a fresh rising edge, and the reason the
    // notifier could not simply dedupe on truthiness.
    app().setWorkState("t1", "a", "working");
    observe();
    expect(agentTab().unread).toBeNull();

    // The turn really ends. Dot back, banner not.
    app().setWorkState("t1", "a", "done");
    app().markAttention("t1", "a", "done", undefined, true);
    observe();
    expect(agentTab().unread).toEqual({ reason: "done", repeat: true });

    expect(news.filter(Boolean).length).toBe(1);
  });

  it("still clears a repeat dot when the user visits the tab", () => {
    // A suppressed banner must not leave an un-clearable dot behind.
    useApp.getState().markAttention("t1", "a", "done", undefined, true);
    expect(agentTab().unread).toBeTruthy();
    useApp.getState().clearAttention("t1", "a");
    expect(agentTab().unread).toBeNull();
  });
});
