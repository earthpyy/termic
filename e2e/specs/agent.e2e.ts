// Agent work-state, attention and queue flows.
//
// These cases assert on the BADGE the user sees (`[data-testid="work-badge"]`
// in the tab strip and in the sidebar row), not on `tab.workState` in the
// store. The store field is an implementation detail of the detector; the
// badge is the feature. Reading the store here used to let the whole visual
// layer break silently — the spinner could stop rendering and every one of
// these tests would still pass.
//
// Submits go through `submitToAgent`, which drives xterm's own input path, so
// no spec stamps `lastInputAt` by hand any more. Terminal OUTPUT is still read
// from the store (`lastOutputAt`) — xterm paints to a WebGL canvas, so PTY
// bytes are genuinely not in the DOM.

import {
  archiveTask,
  clickByText,
  clickWhenVisible,
  ensureActiveTask,
  openTask,
  queuedCount,
  requireTermicApi,
  requireWorkBadges,
  sidebarBadge,
  snap,
  submitToAgent,
  taskViewBadge,
  waitForAgentReady,
  waitForAppShell,
  pressEscape,
  setHooksOwnState,
  waitForWorkBadge,
  waitForWorkBadgeGone,
  setWindowPresence,
} from "../helpers";

/** ms since the task's agent tab last produced PTY bytes. Not in the DOM. */
const quietFor = (taskId: string) =>
  browser.execute((id) => {
    const t = window.__termic!.useApp.getState().tabs[id][0];
    return Date.now() - (t.lastOutputAt ?? 0);
  }, taskId);

// Pasting an image into an agent terminal.
//
// This is the one gesture that cannot reach an agent as text: xterm.js sends
// bytes down the PTY and nothing else, and in Docker mode the agent is a
// Linux process with no route to the Mac's pasteboard, so its own clipboard
// reader finds nothing. termic writes the bytes to a file and types the path
// instead. Two halves, both covered here: the backend actually persisting a
// file (sniffing the format from the bytes, not from a claimed name), and the
// capture-phase listener on the terminal actually consuming an image paste
// while leaving an ordinary text paste alone.
describe("image paste", () => {
  let taskId!: string;
  after(async () => {
    if (taskId) await archiveTask(taskId);
  });

  const PNG = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4];

  type Pastes = { image: boolean; text: boolean };

  /** Put the task in (or out of) Docker mode and wait for the store to agree.
   *  The paste listener reads the LIVE task on every event, so this is the
   *  only state the two cases below differ by. */
  const setDocker = async (id: string, on: boolean) => {
    await browser.execute(async (taskId, enabled) => {
      const t = window.__termic!;
      await t.ipc.taskSetDocker(taskId, enabled, [], []);
      await t.useApp.getState().loadAll();
    }, id, on);
    await browser.waitUntil(
      async () => (await browser.execute((taskId) =>
        !!window.__termic!.useApp.getState().tasks.find((t: { id: string }) => t.id === taskId)?.docker_sandbox_enabled,
      id)) === on,
      { timeoutMsg: `the task never settled to docker=${on}` },
    );
  };

  /** Fire a synthetic image paste and a synthetic text paste at the task's
   *  terminal, and report which of them was CONSUMED. That is the
   *  deterministic signal for whether the capture-phase listener stepped in:
   *  xterm paints to a canvas, so the pasted path itself is not in the DOM.
   *  The three fields are exactly what a real ClipboardEvent carries. */
  const firePastes = (id: string) => browser.execute((taskId) => {
    const host = document.querySelector(`[data-task-id="${taskId}"] [data-terminal-host]`);
    if (!host) return "no terminal host";
    const fire = (data: Record<string, unknown>) => {
      const ev = new Event("paste", { bubbles: true, cancelable: true });
      Object.defineProperty(ev, "clipboardData", { value: data });
      return !host.dispatchEvent(ev);
    };
    const file = new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], "shot.png", { type: "image/png" });
    return {
      image: fire({ types: ["Files"], files: [file], items: [] }),
      text: fire({ types: ["text/plain"], files: [], items: [] }),
    };
  }, id) as Promise<Pastes | string>;

  it("writes a pasted image to disk and names it for its real format", async () => {
    await waitForAppShell();
    await requireTermicApi();
    const path = await browser.execute(async (bytes) =>
      window.__termic!.ipc.clipboardImageSave(new Uint8Array(bytes as number[])), PNG) as string;
    // Under the shared clipboard dir, which Docker mode mounts read-only at
    // this same absolute path, so what gets typed resolves in both worlds.
    expect(path).toContain("/clipboard/");
    expect(path).toMatch(/\/pasted-\d+-[0-9a-f]{8}\.png$/);
  });

  it("refuses bytes that are not an image, rather than writing a fake .png", async () => {
    const err = await browser.execute(async () => {
      try {
        await window.__termic!.ipc.clipboardImageSave(new TextEncoder().encode("just some text"));
        return null;
      } catch (e) { return String(e); }
    });
    expect(err).toBeTruthy();
  });

  it("reads an image straight off the Mac clipboard for ctrl+V", async () => {
    // ctrl+V is not a paste event (it is byte 0x16 down the PTY, and the
    // gesture claude binds its own image-attach to), so there are no bytes to
    // hand over and the pasteboard is read natively. That read is the part
    // worth covering; the keystroke that triggers it is one `if`.
    //
    // This does clobber the clipboard, so whatever text was on it is put back
    // afterwards. An image cannot be restored, which is the honest cost of
    // covering a clipboard feature at all.
    const { execFileSync } = await import("node:child_process");
    const before = execFileSync("pbpaste", { encoding: "utf8" });
    const png = "/tmp/termic-e2e-clipboard.png";
    execFileSync("/usr/bin/python3", ["-c",
      `import base64,pathlib;pathlib.Path(${JSON.stringify(png)}).write_bytes(base64.b64decode(` +
      `"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="))`]);
    try {
      execFileSync("osascript", ["-e", `set the clipboard to (read (POSIX file "${png}") as «class PNGf»)`]);
      const path = await browser.execute(async () => {
        try { return await window.__termic!.ipc.clipboardImageCapture(); }
        catch (e) { return "ERR " + String(e); }
      }) as string;
      expect(path).toMatch(/\/pasted-\d+-[0-9a-f]{8}\.png$/);

      // Text on the clipboard is an ordinary refusal, not a crash: the caller
      // forwards the keystroke to the agent and lets it answer for itself.
      execFileSync("osascript", ["-e", 'set the clipboard to "not an image"']);
      const err = await browser.execute(async () => {
        try { await window.__termic!.ipc.clipboardImageCapture(); return null; }
        catch (e) { return String(e); }
      });
      expect(err).toBeTruthy();
    } finally {
      execFileSync("pbcopy", { input: before });
    }
  });

  it("leaves a paste alone in a terminal that is NOT in Docker", async () => {
    // The important half. Outside a container the agent reads the Mac
    // clipboard itself and gets the real image, so stepping in would hand it
    // a file path instead - strictly worse. This task is an ordinary one, so
    // BOTH pastes must reach xterm untouched.
    taskId = await openTask("e2e-image-paste");
    await waitForAgentReady(taskId);
    // Pin the mode rather than trusting the fixture's default. A new task
    // inherits the profile's global sandbox selection, which an earlier spec
    // may have left on Docker - that is exactly how this case passed on one
    // run and failed on the next.
    await setDocker(taskId, false);
    const r = await firePastes(taskId);
    expect(typeof r).not.toBe("string");
    expect((r as Pastes).image).toBe(false);
    expect((r as Pastes).text).toBe(false);
  });

  it("consumes an image paste once the task runs in Docker, but still lets text through", async () => {
    await setDocker(taskId, true);

    const r = await firePastes(taskId);
    expect(typeof r).not.toBe("string");
    expect((r as Pastes).image).toBe(true);
    // The common paste must still reach xterm untouched, in either mode.
    // Swallowing this one would break every ordinary ⌘V in the app.
    expect((r as Pastes).text).toBe(false);
  });
});

// P0: after a real submit, termic must SHOW the agent as working. Work
// detection is gated on the tab having been submitted-to since spawn (guards
// against cold-start false positives), which is exactly why the submit goes in
// through the terminal's input path: it arms the detector the way a keystroke
// does, so this covers the arming too.
describe("agent working state", () => {
  let taskId!: string;
  after(async () => {
    if (taskId) await archiveTask(taskId);
  });

  it("shows the working badge after a submit", async () => {
    await waitForAppShell();
    await requireTermicApi();
    await requireWorkBadges();
    taskId = await openTask("e2e-agent-working");
    await waitForAgentReady(taskId);

    await submitToAgent(taskId, "do something");

    await waitForWorkBadge(taskId, "working", {
      timeout: 10_000,
      message: "the tab never showed a working badge after a submit",
    });
    await snap("agent-working.png");
  });
});

describe("inline images", () => {
  let taskId!: string;

  after(async () => {
    if (taskId) await archiveTask(taskId);
  });

  it("keeps IIP images visible through Pi's alternate-screen redraw", async () => {
    await waitForAppShell();
    await requireTermicApi();
    taskId = await openTask("e2e-iip");
    await waitForAgentReady(taskId);

    const tabId = await browser.execute((id) => window.__termic!.useApp.getState().tabs[id][0].id, taskId);
    const opaquePixelCount = () => browser.execute((id) => {
      const layer = document.querySelector(`[data-terminal-host="${id}"] .xterm-image-layer`);
      if (!(layer instanceof HTMLCanvasElement) || !layer.width || !layer.height) return 0;
      const context = layer.getContext("2d");
      if (!context) return 0;
      const pixels = context.getImageData(0, 0, layer.width, layer.height).data;
      let count = 0;
      for (let i = 3; i < pixels.length; i += 4) if (pixels[i]) count++;
      return count;
    }, tabId);
    await submitToAgent(taskId, "#iip");

    await browser.waitUntil(
      () => browser.execute((id) => (window.__termic!.useApp.getState().tabs[id][0].liveTitle ?? "").endsWith("iip-after"), taskId),
      { timeout: 10_000, timeoutMsg: "terminal parsing never resumed after the inline image" },
    );
    let previousOpaquePixels = 0;
    let settledFrames = 0;
    await browser.waitUntil(
      async () => {
        const opaquePixels = await opaquePixelCount();
        settledFrames = opaquePixels > 0 && opaquePixels === previousOpaquePixels ? settledFrames + 1 : 0;
        previousOpaquePixels = opaquePixels;
        return settledFrames >= 3;
      },
      { timeout: 10_000, timeoutMsg: "Pi's redraw erased the inline image after rendering settled" },
    );
    await browser.pause(500);
    expect(await opaquePixelCount()).toBeGreaterThan(0);
    await snap("inline-image.png");
  });
});

// P0: when an agent you're NOT watching finishes, termic must raise attention
// on its tab. Start an agent working, switch to another task so it's
// backgrounded (still mounted), and assert its SIDEBAR row flags completion —
// that row is the only surface a user can see it on while looking elsewhere.
describe("agent attention", () => {
  let a: string | undefined;
  let b: string | undefined;
  after(async () => {
    if (a) await archiveTask(a);
    if (b) await archiveTask(b);
  });

  it("flags a backgrounded agent's completion", async () => {
    await waitForAppShell();
    await requireTermicApi();
    await requireWorkBadges();

    a = await openTask("e2e-attn-a");
    await waitForAgentReady(a);
    await submitToAgent(a, "do something");
    await waitForWorkBadge(a, "working", {
      timeout: 10_000,
      message: "agent A never showed a working badge",
    });

    // Switch to a second task so A is backgrounded (kept mounted).
    b = await openTask("e2e-attn-b");

    await browser.waitUntil(
      async () => {
        const badge = await sidebarBadge(a!);
        return badge === "done" || badge === "attention";
      },
      {
        timeout: 15_000,
        interval: 300,
        timeoutMsg: "the backgrounded agent's sidebar row never flagged completion",
      },
    );

    await snap("agent-attention.png");
  });
});

// P1: the message queue lets you line up input while an agent is busy; it
// sends on idle. Cases: a message enqueued while working is HELD (the footer
// chip keeps counting it), then DRAINS once the agent goes idle (chip empties
// + the PTY receives it).
describe("message queue", () => {
  let taskId!: string;
  after(async () => {
    if (taskId) await archiveTask(taskId);
  });

  it("holds a message while working, then drains it when idle", async () => {
    await waitForAppShell();
    await requireTermicApi();
    await requireWorkBadges();
    taskId = await openTask("e2e-queue");
    await waitForAgentReady(taskId);

    // Put the agent to work.
    await submitToAgent(taskId, "work");
    await waitForWorkBadge(taskId, "working", {
      timeout: 10_000,
      message: "agent never showed a working badge",
    });

    // Enqueue a message WHILE working — it must be held, not sent. Adding it
    // is a store call (the composer lives in a popover; the queue engine, not
    // the popover, is what this case is about), but the assertion is the
    // footer chip's count.
    await browser.execute((id) => {
      const s = window.__termic!.useApp.getState();
      s.enqueueAgentMessage(id, s.tabs[id][0].id, "queued-msg");
    }, taskId);
    await browser.waitUntil(async () => (await queuedCount(taskId!)) === 1, {
      timeout: 8_000,
      timeoutMsg: "the queue chip never counted the held message",
    });

    const before = await browser.execute(
      (id) => window.__termic!.useApp.getState().tabs[id][0].lastOutputAt ?? 0,
      taskId,
    );

    // Once the agent settles to idle, the queue drains: the chip empties and
    // the PTY receives the queued line (new output — canvas, so store-read).
    await browser.waitUntil(
      async () => {
        if ((await queuedCount(taskId!)) !== 0) return false;
        const now = await browser.execute(
          (id) => window.__termic!.useApp.getState().tabs[id][0].lastOutputAt ?? 0,
          taskId,
        );
        return now !== before;
      },
      { timeout: 15_000, interval: 300, timeoutMsg: "queue never drained on idle" },
    );

    await snap("message-queue.png");
  });
});

// P2: per-task agent extras. Cases: toggling YOLO mode; opening an aux (bottom)
// terminal for a task.
describe("agent extras", () => {
  let taskId!: string;
  after(async () => {
    if (taskId) await archiveTask(taskId);
  });

  const task = () =>
    browser.execute(
      (id) =>
        window.__termic!.useApp.getState().tasks.find((t: any) => t.id === id),
      taskId,
    );

  it("toggles YOLO mode on a task", async () => {
    await waitForAppShell();
    await requireTermicApi();
    taskId = await openTask("e2e-extras");
    const before = !!(await task())?.yolo;
    await browser.execute(
      (id, b) => window.__termic!.useApp.getState().setTaskYolo(id, !b),
      taskId,
      before,
    );
    await browser.waitUntil(async () => !!(await task())?.yolo !== before, {
      timeout: 8_000,
      timeoutMsg: "YOLO never toggled",
    });
    // restore
    await browser.execute(
      (id, b) => window.__termic!.useApp.getState().setTaskYolo(id, b),
      taskId,
      before,
    );

    // Each toggle asks the live agent's pane whether to restart, and the window
    // has ONE confirm slot, so leaving it up blocks every later dialog in this
    // file (it used to sit over the whole suite). Answer it: "Later".
    //
    // Waited for, not required: toggling and restoring back-to-back can land in
    // a single React commit, and then `effYolo` never changes as far as the
    // pane is concerned, so nothing is asked. The invariant worth asserting is
    // that nothing is left standing, not that something appeared.
    await browser
      .waitUntil(() => browser.execute(() => !!window.__termic!.useUI.getState().confirm),
        { timeout: 5_000, interval: 200 })
      .catch(() => {});
    await browser.execute(() => {
      const ui = window.__termic!.useUI.getState();
      if (ui.confirm) ui.resolveConfirm(false);
    });
    expect(await browser.execute(() => !!window.__termic!.useUI.getState().confirm)).toBe(false);
  });

  // Driven through the REAL footer button, not through `addBottomTab`. The
  // button used to call `toggleTerminalSplit` alone, which opens the split and
  // lets TaskView seed an UNFOCUSED shell, so the user had to click the
  // terminal before typing. A store-level drive cannot see that wiring.
  it("opens an aux (bottom) terminal from the footer button and focuses it", async () => {
    await ensureActiveTask(taskId!);
    await clickByText("Terminal");

    await browser.waitUntil(
      () =>
        browser.execute(
          (id) => (window.__termic!.useApp.getState().bottomTabs[id] ?? []).length >= 1,
          taskId,
        ),
      { timeout: 8_000, timeoutMsg: "aux terminal was not added" },
    );

    // AuxTerminal focuses itself only once its PTY is live and the grid has
    // rendered, so this is a wait, not an immediate read.
    await browser.waitUntil(
      () =>
        browser.execute(
          () => !!document.activeElement?.closest("[data-bottom-split]"),
        ),
      { timeout: 20_000, interval: 250, timeoutMsg: "aux terminal never took focus" },
    );

    await snap("agent-extras.png");
  });

  // The chevron drives `toggleTerminalSplitCollapsed` directly, so the focus
  // move has to live in that action. Collapsing hides the shell with
  // display:none, which drops focus to <body> and swallows every keystroke.
  it("hands focus back on collapse and takes it again on expand", async () => {
    await clickWhenVisible(
      `[data-task-id="${taskId}"] button[title="Collapse terminal"]`,
    );
    await browser.waitUntil(
      () =>
        browser.execute(
          () => !document.activeElement?.closest("[data-bottom-split]"),
        ),
      { timeout: 10_000, interval: 250, timeoutMsg: "focus stayed in the collapsed split" },
    );

    await clickWhenVisible(
      `[data-task-id="${taskId}"] button[title="Expand terminal"]`,
    );
    await browser.waitUntil(
      () =>
        browser.execute(
          () => !!document.activeElement?.closest("[data-bottom-split]"),
        ),
      { timeout: 10_000, interval: 250, timeoutMsg: "expanding did not focus the shell" },
    );
  });

  // Clicking a pill only set the active tab; AuxTerminal never self-focuses on
  // becoming active, so focus stayed in the shell the user just left.
  it("focuses the shell whose pill is clicked", async () => {
    const first: string = await browser.execute(
      (id) => window.__termic!.useApp.getState().bottomTabs[id][0].id as string,
      taskId,
    );
    // A second shell, so the click is a real switch. addBottomTab focuses it.
    await browser.execute(
      (id) => window.__termic!.useApp.getState().addBottomTab(id),
      taskId,
    );
    await browser.waitUntil(
      () =>
        browser.execute(
          (id, f) => window.__termic!.useApp.getState().activeBottomTab[id] !== f,
          taskId,
          first,
        ),
      { timeout: 8_000, timeoutMsg: "the second shell never became active" },
    );

    await clickWhenVisible(
      `[data-bottom-split] [data-scroll-strip] [data-tab-id="${first}"]`,
    );
    await browser.waitUntil(
      () =>
        // The pill carries the same data-tab-id as the terminal host, so the
        // assertion pins the xterm textarea specifically, not just the id.
        browser.execute(
          (f) =>
            !!document.activeElement?.classList.contains("xterm-helper-textarea") &&
            document.activeElement.closest("[data-tab-id]")?.getAttribute("data-tab-id") === f,
          first,
        ),
      { timeout: 10_000, interval: 250, timeoutMsg: "clicking the pill did not focus its shell" },
    );

    // Back to one shell, so the next case closes the last one.
    await browser.execute(
      (id) => {
        const st = window.__termic!.useApp.getState();
        const extra = st.bottomTabs[id].filter((t: any) => t.id !== st.bottomTabs[id][0].id);
        extra.forEach((t: any) => st.closeBottomTab(id, t.id));
      },
      taskId,
    );
    await browser.waitUntil(
      () =>
        browser.execute(
          (id) => window.__termic!.useApp.getState().bottomTabs[id].length === 1,
          taskId,
        ),
      { timeout: 8_000, timeoutMsg: "the extra shell never closed" },
    );
  });

  // Closing the last shell closes the split, so the footer button comes back
  // and the next open starts from the same state as the first.
  it("closes the split when the last shell closes", async () => {
    const tabId = await browser.execute(
      (id) => window.__termic!.useApp.getState().bottomTabs[id][0].id,
      taskId,
    );
    await browser.execute(
      (id, tid) => window.__termic!.useApp.getState().closeBottomTab(id, tid),
      taskId,
      tabId,
    );
    await browser.waitUntil(
      () =>
        browser.execute(
          (id) => !window.__termic!.useApp.getState().terminalSplit[id],
          taskId,
        ),
      { timeout: 8_000, timeoutMsg: "split stayed open after the last shell closed" },
    );
  });
});

// P1: the agent registry (Settings → Agent CLIs). Guards disabling/enabling an
// agent CLI through agentsSave. Uses "gemini" (not the test agents) and always
// restores it.
describe("agent settings", () => {
  const AGENT = "gemini";

  const setDisabled = (disabled: boolean) =>
    browser.execute(
      async (id, dis) => {
        const st = window.__termic!.useApp.getState();
        const next = st.agents.map((a: any) =>
          a.id === id ? { ...a, disabled: dis } : a,
        );
        await window.__termic!.ipc.agentsSave(next);
        await st.loadAll();
      },
      AGENT,
      disabled,
    );
  const isDisabled = () =>
    browser.execute(
      (id) =>
        !!window.__termic!.useApp
          .getState()
          .agents.find((a: any) => a.id === id)?.disabled,
      AGENT,
    );

  after(async () => {
    await setDisabled(false);
  });

  it("disables an agent CLI", async () => {
    await waitForAppShell();
    await requireTermicApi();
    expect(await isDisabled()).toBe(false);
    await setDisabled(true);
    await browser.waitUntil(async () => (await isDisabled()) === true, {
      timeout: 8_000,
      timeoutMsg: "agent never became disabled",
    });
  });

  it("re-enables an agent CLI", async () => {
    await setDisabled(false);
    await browser.waitUntil(async () => (await isDisabled()) === false, {
      timeout: 8_000,
      timeoutMsg: "agent never re-enabled",
    });
    await snap("agent-settings.png");
  });
});

// P0: an agent that backgrounds work ends its own turn while the work runs, so
// its title goes to the idle glyph and every byte-stream signal reads
// "finished". Measured against real claude: a done badge held for 617s while
// three subagents worked. The only thing that says otherwise is the agent's own
// status line, so the done is held back while that line is on screen.
describe("pending work defers done", () => {
  let taskId!: string;
  after(async () => {
    if (taskId) await archiveTask(taskId);
  });

  it("holds the done badge while the agent reports work outstanding", async () => {
    await waitForAppShell();
    await requireTermicApi();
    await requireWorkBadges();
    taskId = await openTask("e2e-pending-work");
    await waitForAgentReady(taskId);

    await submitToAgent(taskId, "#pending 2");
    await waitForWorkBadge(taskId, "working", {
      timeout: 10_000,
      message: "agent never showed a working badge",
    });

    // Give every done path a real chance to fire before asserting it did not.
    // Not a fixed sleep: this waits on app state (bytes stopped arriving) past
    // the two thresholds that would otherwise fire — byte-quiet at 4s and the
    // settle timer at 5s. Without clearing those, "still working" would prove
    // nothing.
    await browser.waitUntil(async () => (await quietFor(taskId!)) > 9_000, {
      timeout: 25_000,
      interval: 500,
      timeoutMsg: "PTY never went quiet",
    });

    // Still spinning, and no bell — the hold is a hold, not a swallowed done.
    expect(await taskViewBadge(taskId)).toBe("working");
    await snap("agent-pending-held.png");
  });

  it("fires done once the agent's status line clears", async () => {
    await submitToAgent(taskId!, "#settle");
    await waitForWorkBadgeGone(taskId!, "working", {
      timeout: 25_000,
      interval: 300,
      message: "done never fired after the work landed",
    });
    await snap("agent-pending-settled.png");
  });
});

// P0: the hold above must not be able to pin a tab to "working" forever. A
// status line that never clears (a background shell that outlives the turn, or
// the words still sitting in the tail after the work landed) leaves the screen
// byte-quiet and unchanging, so every demoter either fires into the hold or
// latches itself off. The absolute ceiling is the only thing that outranks the
// hold, and it was unreachable: byte-quiet gave up the tick on a held done, so
// the ceiling below it never ran and the tab stayed "working" until the user
// clicked it. Shortened here via the workDoneCeilingMs debug knob, since the
// real one is ten minutes.
describe("a hold that never clears still ends", () => {
  let taskId!: string;
  const CEILING_MS = 8_000;

  after(async () => {
    await browser.execute(() => localStorage.removeItem("workDoneCeilingMs"));
    if (taskId) await archiveTask(taskId);
  });

  it("force-clears the working state at the absolute ceiling", async () => {
    await waitForAppShell();
    await requireTermicApi();
    await requireWorkBadges();
    await browser.execute((ms) => localStorage.setItem("workDoneCeilingMs", String(ms)), CEILING_MS);
    taskId = await openTask("e2e-pending-ceiling");
    await waitForAgentReady(taskId);

    // Same drill as the hold spec, and #settle is never sent: the pending line
    // stays on screen for the rest of the test.
    await submitToAgent(taskId, "#pending 2");
    await waitForWorkBadge(taskId, "working", {
      timeout: 10_000,
      message: "agent never showed a working badge",
    });

    await waitForWorkBadgeGone(taskId, "working", {
      timeout: CEILING_MS + 20_000,
      interval: 500,
      message: "the held done never reached the ceiling — tab pinned to working",
    });
    await snap("agent-pending-ceiling.png");
  });
});

// P0: the same backstop, for a tab whose agent reports its own state. This is
// the case that was actually UNBOUNDED, and it needs its own task because the
// ceiling override is resolved once when the sampler starts: setting it on a
// tab that is already mounted changes nothing.
//
// The reported case was a claude session that published an artifact. That opens
// an ambient websocket monitor which stays `running` for the rest of the
// session, claude reports it in every later `Stop` payload, and the done hook's
// old "is background_tasks non-empty" guard dropped every one of them. The
// guard is a whitelist now, so that particular hole is shut. The reason it was
// unbounded is here, not in the hook: the absolute ceiling sat BELOW the
// hooks-own gate and never ran, so a hook done that never arrives had nothing
// behind it at all, across new turns and finished turns.
//
// `#hookturn` is that shape exactly: 133;C, then a title going idle, and no
// 133;D ever. The suppression spec above asserts such a tab is still working at
// 8s, which is right. This asserts it does not stay that way forever.
describe("a hook-owned turn whose done never arrives still ends", () => {
  let taskId!: string;
  const CEILING_MS = 8_000;

  after(async () => {
    await browser.execute(() => localStorage.removeItem("workDoneCeilingMs"));
    await setHooksOwnState("fakeagent", false);
    if (taskId) await archiveTask(taskId);
  });

  it("force-clears the working state at the absolute ceiling", async function () {
    this.timeout(90_000);
    await waitForAppShell();
    await requireTermicApi();
    await requireWorkBadges();
    await setHooksOwnState("fakeagent", true);
    await browser.execute((ms) => localStorage.setItem("workDoneCeilingMs", String(ms)), CEILING_MS);
    taskId = await openTask("e2e-hook-ceiling");
    await waitForAgentReady(taskId);

    await submitToAgent(taskId, "#hookturn");
    await waitForWorkBadge(taskId, "working", {
      timeout: 20_000,
      message: "the hook-owned turn never reached the working badge",
    });

    // The hook said 133;C and will never say 133;D, the title has gone idle and
    // is ignored, and every heuristic demoter is stood down for this tab. The
    // ceiling is the only thing left.
    await waitForWorkBadgeGone(taskId, "working", {
      timeout: CEILING_MS + 30_000,
      interval: 500,
      message: "a hook done never came and the ceiling never fired - tab pinned to working",
    });
    await snap("agent-hook-ceiling.png");
  });
});

// P0: a done we got wrong must not outlive the evidence. Every heuristic here
// can misread a stage boundary in a long multi-stage turn as the end of it, and
// the recovery used to be a click: agent signals could never undo a "done", so
// the tab showed no spinner for the rest of the turn, and the turn's one done
// token was already spent so the real completion badged nothing.
describe("a premature done is taken back", () => {
  let a: string | undefined;
  let b: string | undefined;
  after(async () => {
    if (a) await archiveTask(a);
    if (b) await archiveTask(b);
  });

  // The ONLY case in this file that needs more than mocha's 60s default, and
  // it is raised on purpose rather than left to be killed by it. The fixture
  // burns ~19s that cannot be compressed away: a 16s idle window (it must
  // outlast STICKY_DONE_MS = 8s counted from when the done FIRES, ~6s in, or
  // stage 2's busy signal is ignored as post-answer glyph flicker), plus
  // stage 2 and the settle. Six sequential waits sit on top. Left at 60s, the
  // last waits are unreachable: a stall gets killed mid-wait with mocha's
  // generic "timeout of 60000ms exceeded" and no clue which signal never
  // arrived. Raised, each wait reaches its own bound — so a stall FAILS FASTER
  // (at the wait that broke) and names what it was waiting for.
  it("returns to working, then still fires the real done", async function () {
    this.timeout(95_000);
    await waitForAppShell();
    await requireTermicApi();
    await requireWorkBadges();

    a = await openTask("e2e-stage-a");
    await waitForAgentReady(a);

    // Task B exists BEFORE the submit. A's done only badges while nobody is
    // watching it (a focused tab's done is downgraded to idle on the spot), so
    // A has to be backgrounded before stage 1 ends — and creating a task takes
    // ~1.5s, which used to race the fixture's stage-1 spinner. The fixture
    // padded that spinner to 6s to cover the race. Creating B up front makes
    // backgrounding a sub-millisecond store call instead, so there is nothing
    // left to race and the fixture's padding could go (see `#stage` in
    // scripts/fake-agent.sh).
    b = await openTask("e2e-stage-b");
    await ensureActiveTask(a);

    await submitToAgent(a, "#stage");
    await waitForWorkBadge(a, "working", {
      timeout: 10_000,
      message: "agent never showed a working badge",
    });

    // Background A. From here on the sidebar row is the surface under test.
    await ensureActiveTask(b);

    await browser.waitUntil(async () => (await sidebarBadge(a!)) === "done", {
      timeout: 15_000,
      interval: 300,
      timeoutMsg: "the premature done never showed in the sidebar",
    });

    // Stage 2 starts. The spinner has to come back on its own, and the wrong
    // bullet has to go with it — a spinner is what the sidebar shows only once
    // the done AND its bell are gone (attention outranks both).
    await browser.waitUntil(async () => (await sidebarBadge(a!)) === "working", {
      timeout: 15_000,
      interval: 300,
      timeoutMsg: "the sidebar row never went back to a working badge",
    });

    // And the turn's real ending still has a done to spend.
    await browser.waitUntil(
      async () => {
        const badge = await sidebarBadge(a!);
        return badge === "done" || badge === "attention";
      },
      { timeout: 15_000, interval: 300, timeoutMsg: "the real completion badged nothing" },
    );
    await snap("agent-stage-recovered.png");
  });
});

// P0 (GH #276): one turn is allowed to interrupt the user ONCE.
//
// The two cases below are the same bug reached down two different code paths,
// and neither existed until a user reported "notifications for what seems like
// every action". The mechanism is a loop between three places that are each
// individually right:
//
//   1. a done fires  -> markAttention("done") -> the tab's `unread` goes from
//      null to set, and that RISING EDGE is exactly what useAttentionNotifier
//      turns into an OS banner.
//   2. the agent goes back to work -> setWorkState("working") in store/app.ts
//      CLEARS that unread, deliberately: leaving "this finished" on a visibly
//      working tab would be a lie.
//   3. the turn ends again -> another rising edge -> another banner.
//
// So every premature done in a long turn costs one notification, and a long
// agentic turn has many. The notifier's own 8s debounce is no defence at all
// against a turn measured in minutes.
//
// Counted on the rising edge rather than on `notify()` itself on purpose: the
// notifier imports `notify` as a live ESM binding, so there is nothing a spec
// can hook, and the edge is what it consumes one-for-one anyway (its only
// other gates are the focus check, which `setWindowPresence(false)` settles,
// and the debounce this is about).
describe("one turn raises one notification", () => {
  const ids: string[] = [];
  after(async () => {
    for (const id of ids) await archiveTask(id);
  });

  /** Count what the notifier consumes, and separately what the sidebar shows.
   *
   *  The OS banner itself is NOT the observable here, and that was measured
   *  rather than assumed: `ipc.notify` bails at `ensureNotifyPermission()`
   *  before it ever reaches the plugin, so an e2e binary asks the OS for
   *  nothing and a count of notification commands reads 0 both before and after
   *  the fix. The `__TAURI_INTERNALS__.invoke` wrapper below is kept anyway,
   *  because it is the only hook a page HAS (`notify` is a live ESM binding)
   *  and a run with permission granted gets the count for free. It is reported,
   *  never asserted.
   *
   *  So the assertion is on the newsworthy rising edge: the last decision
   *  before `notify()`, mapping to banners one-for-one. */
  const armCounters = (taskId: string) =>
    browser.execute((id) => {
      const w = window as unknown as {
        __notifyCount?: number;
        __notifyCmds?: string[];
        __unreadEdges?: number;
        __newsEdges?: number;
        __unreadStop?: () => void;
        __invokeRestore?: () => void;
        __TAURI_INTERNALS__?: { invoke: (...a: unknown[]) => unknown };
      };
      w.__unreadStop?.();
      w.__invokeRestore?.();
      w.__notifyCount = 0;
      w.__notifyCmds = [];
      w.__unreadEdges = 0;
      w.__newsEdges = 0;

      const internals = w.__TAURI_INTERNALS__;
      if (internals) {
        const original = internals.invoke;
        internals.invoke = (...args: unknown[]) => {
          const cmd = String(args[0] ?? "");
          if (/notif/i.test(cmd)) {
            w.__notifyCount = (w.__notifyCount ?? 0) + 1;
            w.__notifyCmds!.push(cmd);
          }
          return original.apply(internals, args as never);
        };
        w.__invokeRestore = () => { internals.invoke = original; };
      }

      // Two counts, because the fix has to move one and not the other:
      //   edges  - every null -> set transition. This is the sidebar dot, and
      //            a long turn is SUPPOSED to produce several.
      //   news   - the same transitions measured on newsworthiness rather than
      //            truthiness, which is exactly what useAttentionNotifier
      //            turns into a banner (lib/attentionNotify.ts).
      // Both maps start empty, so a first mark is itself an edge.
      const seen = new Map<string, boolean>();
      const seenNews = new Map<string, boolean>();
      w.__unreadStop = window.__termic!.useApp.subscribe((s: { tabs: Record<string, Array<{ id: string; unread?: { repeat?: boolean } | null }>> }) => {
        for (const tab of s.tabs[id] ?? []) {
          const now = !!tab.unread;
          if (now && !(seen.get(tab.id) ?? false)) w.__unreadEdges = (w.__unreadEdges ?? 0) + 1;
          seen.set(tab.id, now);
          const news = !!tab.unread && tab.unread.repeat !== true;
          if (news && !(seenNews.get(tab.id) ?? false)) w.__newsEdges = (w.__newsEdges ?? 0) + 1;
          seenNews.set(tab.id, news);
        }
      });
    }, taskId);

  const readCounters = () =>
    browser.execute(() => {
      const w = window as unknown as {
        __notifyCount?: number; __notifyCmds?: string[];
        __unreadEdges?: number; __newsEdges?: number;
      };
      return {
        banners: w.__notifyCount ?? 0,
        cmds: w.__notifyCmds ?? [],
        edges: w.__unreadEdges ?? 0,
        news: w.__newsEdges ?? 0,
      };
    }) as Promise<{ banners: number; cmds: string[]; edges: number; news: number }>;

  const disarm = () =>
    browser.execute(() => {
      const w = window as unknown as {
        __unreadStop?: () => void; __invokeRestore?: () => void;
      };
      w.__unreadStop?.();
      w.__invokeRestore?.();
      w.__unreadStop = undefined;
      w.__invokeRestore = undefined;
    });

  /** Both drills are a two-stage turn: work, look finished, work, finish. */
  const runTwoStageTurn = async (name: string, directive: string) => {
    const taskId = await openTask(name);
    ids.push(taskId);
    await waitForAgentReady(taskId);
    // Away: a done on a watched tab is downgraded to idle on the spot and
    // never badges, so the bug is only reachable with nobody looking. That is
    // also the reporter's repro ("run a task and unfocus the application").
    await setWindowPresence(false);
    await armCounters(taskId);
    await submitToAgent(taskId, directive);

    // Stage 1's premature done, then stage 2 taking it back, then the real
    // ending. Waiting through all three is what proves the counter saw the
    // whole turn rather than stopping at the first badge.
    await browser.waitUntil(async () => (await sidebarBadge(taskId)) === "done", {
      timeout: 25_000, interval: 300,
      timeoutMsg: "the premature done never badged; the drill did not run",
    });
    await browser.waitUntil(async () => (await sidebarBadge(taskId)) === "working", {
      timeout: 25_000, interval: 300,
      timeoutMsg: "the agent never went back to work; the drill did not run",
    });
    await browser.waitUntil(async () => {
      const b = await sidebarBadge(taskId);
      return b === "done" || b === "attention";
    }, {
      timeout: 25_000, interval: 300,
      timeoutMsg: "the real completion badged nothing",
    });
    const counters = await readCounters();
    await disarm();
    await setWindowPresence(true);
    return counters;
  };

  // The heuristic path: no hooks, the title and the settle timers own the
  // turn. This is codex's ONLY path (it is not in agent_hooks SUPPORTED), and
  // claude's whenever its hooks are off.
  it("does not re-notify when a heuristic done is taken back and re-fired", async function () {
    this.timeout(120_000);
    await waitForAppShell();
    await requireTermicApi();
    await requireWorkBadges();
    const { edges, news } = await runTwoStageTurn("e2e-notify-once-title", "#stage");
    // 2 before the fix: one banner per stage boundary.
    expect(news).toBe(1);
    // ...while the dot still tracks BOTH boundaries. A fix that suppressed the
    // edge would have taken the sidebar marker with it, which is why this is
    // asserted rather than left to whatever the fix happened to do.
    expect(edges).toBe(2);
    await snap("agent-notify-once-title.png");
  });

  // The hook path, which needed its own case because it does not share a
  // single guard with the one above: a 133;D calls fireDone with `fromHook`,
  // which skips the one-done-per-submit token entirely. A captured
  // termic-workstate.log from real claude use shows `CDCDCDCDCD` on ordinary
  // tasks, so this is the common shape, not an exotic one.
  it("does not re-notify when a HOOK reports two dones in one turn", async function () {
    this.timeout(120_000);
    await waitForAppShell();
    await requireTermicApi();
    await requireWorkBadges();
    const { edges, news } = await runTwoStageTurn("e2e-notify-once-hook", "#hookstage");
    expect(news).toBe(1);
    expect(edges).toBe(2);
    await snap("agent-notify-once-hook.png");
  });
});

// P0: an agent asking for the user must raise ATTENTION, not "done". Claude
// sends this ~6s after its title goes idle, i.e. always just behind our own
// done paths, so attention has to be able to land on top of a done we already
// fired. It also sends a second, non-actionable notification a minute after any
// turn you don't reply to; badging that would ring a bell for finished work.
describe("agent notifications", () => {
  let taskId!: string;
  after(async () => {
    if (taskId) await archiveTask(taskId);
  });

  it("raises attention with the agent's own wording", async () => {
    await waitForAppShell();
    await requireTermicApi();
    await requireWorkBadges();
    taskId = await openTask("e2e-agent-notify");
    await waitForAgentReady(taskId);
    // Away, which is the only state in which a badge is meant to persist.
    await setWindowPresence(false);

    await submitToAgent(taskId, "#osc9 FakeAgent needs your permission");

    // The bell is the visible half.
    await waitForWorkBadge(taskId, "attention", {
      timeout: 15_000,
      message: "OSC 9 never raised an attention badge",
    });
    // The agent's own wording is carried on the notification, which has no DOM
    // surface of its own (it goes to the OS notifier), so read it from state.
    const message = await browser.execute(
      (id) => window.__termic!.useApp.getState().tabs[id][0].unread?.message ?? null,
      taskId,
    );
    expect(message).toBe("FakeAgent needs your permission");
    await snap("agent-notify-attention.png");
  });

  it("ignores the idle nag that claude sends after every unanswered turn", async () => {
    // Clear the previous badge the way focus/typing does, so a stale one can't
    // make this pass.
    await browser.execute((id) => {
      const s = window.__termic!.useApp.getState();
      s.clearAttention(id, s.tabs[id][0].id);
    }, taskId);
    expect(await taskViewBadge(taskId!)).not.toBe("attention");

    await submitToAgent(taskId!, "#osc9 FakeAgent is waiting for your input");

    // Prove the directive was consumed (the PTY echoed past it) rather than
    // asserting on a race: bytes must have flowed after the send.
    await browser.waitUntil(async () => (await quietFor(taskId!)) > 6_000, {
      timeout: 30_000,
      interval: 500,
      timeoutMsg: "PTY never went quiet after the nag",
    });

    expect(await taskViewBadge(taskId!)).not.toBe("attention");
    expect(await sidebarBadge(taskId!)).not.toBe("attention");
  });
  // The agent-hooks feature (#269), end to end through the real detector.
  //
  // Measured against Claude Code 2.1.250: while it is blocked on a permission
  // prompt it paints its IDLE glyph, which arms termic's 5s settle and fires a
  // "done" a second before the native OSC 9 notify corrects it. The installed
  // hook returns a terminalSequence that claude writes to its own PTY, and it
  // lands ~20ms after the idle title. `#hookattn` replays that exact order.
  //
  // The assertion that matters is the ABSENCE of a done: goAttention calls
  // cancelSettle, so the false one must never reach the badge at all. Waiting
  // past SETTLE_MS (5s) is what makes that a real check rather than a race the
  // spec happens to win.
  it("an agent hook's OSC 777 raises attention and suppresses the false done", async () => {
    await ensureActiveTask(taskId!);
    await browser.execute((id) => {
      const s = window.__termic!.useApp.getState();
      s.clearAttention(id, s.tabs[id][0].id);
    }, taskId);

    // This spec waits past SETTLE_MS with the badge up, so it MUST say the
    // user is away: three seconds of dwelling in a focused window is now a
    // read receipt, and without this the assertion below would pass or fail on
    // whether the suite's window happened to have focus.
    await setWindowPresence(false);
    await submitToAgent(taskId!, "#hookattn");

    await waitForWorkBadge(taskId!, "attention", {
      timeout: 20_000,
      message: "the hook's OSC 777 never raised attention",
    });

    // Past the settle window the idle title armed. If cancelSettle regressed,
    // a done bullet stacks on top of the bell right about here.
    await browser.waitUntil(async () => (await quietFor(taskId!)) > 7_000, {
      timeout: 30_000,
      interval: 500,
      timeoutMsg: "PTY never went quiet after the hook",
    });
    expect(await taskViewBadge(taskId!)).toBe("attention");
    expect(await sidebarBadge(taskId!)).not.toBe("done");
  });

  // The usage footer (GH #277), end to end through the real OSC chain.
  //
  // ONE submit, and deliberately so. The threshold table, the driving-window
  // rule and the parse are covered exhaustively in `lib/agentUsage.test.ts`,
  // which runs in milliseconds; what only a real window can prove is the
  // CHAIN, and that costs an agent round trip per assertion. An earlier
  // version of this walked four thresholds through four submits and blew
  // mocha's 60s budget, which is a slow suite buying nothing the unit test had
  // not already said.
  //
  // The body is chosen to prove two things at once: the chip renders both
  // numbers, and the colour follows the window CLOSEST TO ITS LIMIT rather
  // than the session one. 30% of five hours in front of 95% of the week has to
  // read as critical, or the footer reports comfort until the turn that fails.
  it("shows plan usage in the footer, coloured by the window nearest its limit", async () => {
    await ensureActiveTask(taskId!);
    await submitToAgent(taskId!, "#usage usage 30 95 - -");

    const chip = await browser.$('[data-testid="usage-chip"]');
    await chip.waitForExist({ timeout: 20_000 });
    // The numbers the USER reads, not the store field behind them.
    expect(await chip.getAttribute("data-usage-session")).toBe("30");
    expect(await chip.getAttribute("data-usage-weekly")).toBe("95");
    expect(await chip.getAttribute("data-usage-source")).toBe("statusline");
    expect(await chip.getAttribute("data-usage-level")).toBe("critical");
    expect(await chip.getText()).toContain("30%");
    expect(await chip.getText()).toContain("95%");
    expect(await (await browser.$('[data-testid="usage-bar-fill"]')).isExisting()).toBe(true);
  });

  // The half that is easy to get wrong and expensive to ship wrong. A trusted
  // body skips every notification filter by design, so a usage report routed
  // one branch too late falls through to notifyAttention and badges the tab.
  // This body arrives on every turn of every task, so that bug would mean a
  // bell per turn, forever.
  //
  // The quiet wait is what makes it a real check rather than a race the spec
  // happens to win, and it is the reason this is a second `it` rather than
  // another assertion on the first: the cost buys something no unit test can.
  it("never badges the tab for a usage report", async () => {
    await ensureActiveTask(taskId!);
    await browser.execute((id) => {
      const s = window.__termic!.useApp.getState();
      s.clearAttention(id, s.tabs[id][0].id);
    }, taskId);
    await setWindowPresence(false);

    await submitToAgent(taskId!, "#usage usage 61 44 - -");
    await browser.waitUntil(async () => {
      const el = await browser.$('[data-testid="usage-chip"]');
      return (await el.isExisting()) && (await el.getAttribute("data-usage-session")) === "61";
    }, { timeout: 20_000, timeoutMsg: "the second usage report never landed" });

    await browser.waitUntil(async () => (await quietFor(taskId!)) > 6_000, {
      timeout: 30_000, interval: 500,
      timeoutMsg: "PTY never went quiet after the usage report",
    });
    expect(await taskViewBadge(taskId!)).not.toBe("attention");
    expect(await sidebarBadge(taskId!)).not.toBe("attention");
  });

  // Looking at a tab is how you read its badge. `markAttention` marks
  // unconditionally, focused tab included, and the badge then cleared only on
  // a keystroke in that terminal or on re-activating the task, so the common
  // case stuck: the tab you are already on earns a badge while you are in
  // another app, you come back, and because the tab never CHANGED nothing
  // cleared it. Clicking away and back was the only way out.
  it("clears a badge on the tab you are looking at, once you are back", async () => {
    await ensureActiveTask(taskId!);
    await setWindowPresence(false);
    await browser.execute((id) => {
      const s = window.__termic!.useApp.getState();
      s.clearAttention(id, s.tabs[id][0].id);
    }, taskId);

    await submitToAgent(taskId!, "#osc9 FakeAgent needs your permission");
    await waitForWorkBadge(taskId!, "attention", {
      timeout: 20_000,
      message: "the badge never appeared while the user was away",
    });

    // Still away, and still badged. Without this the test would also pass on a
    // bug that simply drops every attention, since the assertion below is that
    // a badge went away.
    expect(await taskViewBadge(taskId!)).toBe("attention");

    // Back at the keyboard, on that very tab.
    await setWindowPresence(true);
    await waitForWorkBadgeGone(taskId!, "attention", {
      timeout: 20_000,
      message: "returning to a focused window never cleared the badge on the visible tab",
    });
  });
  // Once an agent reports its own state, the terminal TITLE stops being
  // allowed to end a turn for it. This is the case the whole design turns on:
  // measured over an 8.5 minute run with four real subagents, claude's title
  // claimed idle for 30% of the time while the work was still outstanding,
  // and its own Stop correctly stayed silent throughout. Trusting both means
  // trusting the wrong one.
  describe("when an agent reports its own state", () => {
    const reset = async () => {
      await ensureActiveTask(taskId!);
      await browser.execute((id) => {
        const s = window.__termic!.useApp.getState();
        s.clearAttention(id, s.tabs[id][0].id);
        s.setWorkState(id, s.tabs[id][0].id, "idle");
      }, taskId);
    };
    after(async () => { await setHooksOwnState("fakeagent", false); });

    // NOT covered here: "no hook ever arrives, so the fallbacks must stay
    // armed" (the Docker case, where hooks install into the container and
    // their OSC cannot reach the host pty). It needs a pty that has never
    // seen a hook, and every tab in this file has: `#hookattn` emits an OSC
    // 777 earlier. Two attempts at it disturbed the shared task and broke the
    // case below instead, so it wants its own spec file with its own launch
    // rather than a third try wedged in here. The behaviour itself is
    // `hookSeenRef` in TerminalPane.
    it("ignores the title going idle, so a long turn is not called done early", async () => {
      await setHooksOwnState("fakeagent", true);
      await reset();
      // This tab has already seen a hook (`#hookattn`'s OSC 777, earlier in
      // this file), which is what licenses the suppression under test. The case
      // above covers the opposite: a tab where none ever arrives.

      // An ordinary turn: the fixture spins, then paints claude's idle glyph.
      // That glyph alone used to be enough to end the turn.
      // `#hookturn`: the hook says the turn started and never says it ended,
      // while the title goes idle. That IS the case, and a fixture whose only
      // signal was a title could not produce it now that hooks own both edges.
      await submitToAgent(taskId!, "#hookturn");
      await waitForWorkBadge(taskId!, "working", {
        timeout: 20_000,
        message: "the turn never reached the working badge",
      });

      // Past SETTLE_MS with the PTY quiet, which is precisely the situation
      // that used to fire a done off the title.
      await browser.waitUntil(async () => (await quietFor(taskId!)) > 8_000, {
        timeout: 30_000, interval: 500,
        timeoutMsg: "PTY never went quiet after the turn",
      });
      expect(await taskViewBadge(taskId!)).not.toBe("done");
      expect(await sidebarBadge(taskId!)).not.toBe("done");
    });

    // The interrupt half. claude fires NO hook for one (measured with 29 of
    // its 31 lifecycle events registered), so the keystroke is the only
    // evidence, and it is what licenses the title for that single turn.
    it("stops claiming work when the user interrupts, which no hook reports", async () => {
      await setHooksOwnState("fakeagent", true);
      await reset();

      await submitToAgent(taskId!, "#longwork");
      await waitForWorkBadge(taskId!, "working", {
        timeout: 20_000,
        message: "the long turn never reached the working badge",
      });

      await pressEscape(taskId!);

      // Localise a failure: if the fixture never reacted, the keystroke never
      // reached the PTY and the bug is in the spec, not the detector.
      await browser.waitUntil(
        async () => ((await browser.execute(
          (id) => (window.__termic!.useApp.getState().tabs[id] ?? [])[0]?.liveTitle ?? "",
          taskId,
        )) as string).includes("✳"),
        { timeout: 15_000, interval: 250,
          timeoutMsg: "the fixture never went idle, so Escape never reached the PTY" },
      );

      await waitForWorkBadgeGone(taskId!, "working", {
        timeout: 20_000,
        message: "Escape reached the agent, but termic still claims it is working",
      });
      // An interrupt is not a completion: it clears the in-progress state and
      // nothing else, so it must not leave a done badge behind either.
      expect(await taskViewBadge(taskId!)).not.toBe("done");
    });

    // The agy shape, and the reason the keystroke is not enough on its own.
    // Measured mid-turn: agy fires NO hook for an interrupt and publishes no
    // title state either, so the only evidence its user's key landed is the
    // terminal falling quiet. An agent that did NOT act on the key keeps
    // painting (opencode's first Escape does nothing; it takes two), so quiet
    // never arrives and nothing ends: that asymmetry is what makes
    // corroboration safe.
    it("ends an interrupted turn that reports neither a hook nor a title", async () => {
      await setHooksOwnState("fakeagent", true);
      await reset();

      await submitToAgent(taskId!, "#longwork-silent");
      await waitForWorkBadge(taskId!, "working", {
        timeout: 20_000,
        message: "the silent long turn never reached the working badge",
      });

      await pressEscape(taskId!);

      await waitForWorkBadgeGone(taskId!, "working", {
        timeout: 30_000,
        message: "the terminal went quiet after an interrupt, but termic still claims it is working",
      });
      // The fixture holds its busy title through the interrupt, so the other
      // route (title goes idle) could not have fired and the badge can only
      // have cleared on the silence. Doubles as the mis-dispatch check: the
      // fixture's default branch ends on the idle glyph.
      expect(await browser.execute(
        (id) => (window.__termic!.useApp.getState().tabs[id] ?? [])[0]?.liveTitle ?? "",
        taskId,
      )).not.toContain("✳");
      expect(await taskViewBadge(taskId!)).not.toBe("done");
    });
  });
});
