import { describe, it, expect } from "vitest";
import { isNewsworthyUnread, shouldNotifyUnread, UNREAD_PHRASE } from "@/lib/attentionNotify";
import { ATTENTION_ECHO_MS } from "@/lib/agents";

// GH #276: "notifications for what seems like every action in a terminal
// session". The mechanism was a loop between three individually-correct places
// (a done badges, the agent going back to work clears that badge, the turn ends
// again and re-badges), and every lap cost one OS banner. These cases pin the
// half of the fix that lives in the notifier.

type U = { reason: "done" | "attention" | "bell" | "exit" | "idle"; repeat?: boolean };
const tab = (u: U | null) => ({ unread: u });

describe("isNewsworthyUnread", () => {
  it("counts a plain mark as news", () => {
    expect(isNewsworthyUnread(tab({ reason: "done" }))).toBe(true);
    expect(isNewsworthyUnread(tab({ reason: "attention" }))).toBe(true);
  });

  it("does not count a repeat mark", () => {
    expect(isNewsworthyUnread(tab({ reason: "done", repeat: true }))).toBe(false);
  });

  it("counts nothing when there is no mark", () => {
    expect(isNewsworthyUnread(tab(null))).toBe(false);
    expect(isNewsworthyUnread(undefined)).toBe(false);
    expect(isNewsworthyUnread({})).toBe(false);
  });

  it("treats a MISSING flag as news, not as a repeat", () => {
    // Deliberate direction of failure: every unread record written before this
    // flag existed, and any path that forgets to pass it, must still reach the
    // user. Going quiet is the regression nobody notices until they miss a
    // turn; going loud is what the producer's per-turn latch bounds.
    expect(isNewsworthyUnread(tab({ reason: "done", repeat: undefined }))).toBe(true);
    expect(isNewsworthyUnread({ unread: { reason: "done" } as U })).toBe(true);
  });
});

describe("shouldNotifyUnread", () => {
  it("fires on a rising edge", () => {
    expect(shouldNotifyUnread(tab({ reason: "done" }), tab(null))).toBe(true);
  });

  it("does not fire while the same mark is still held", () => {
    // An agent re-asserting a mark it already holds is not new information.
    expect(shouldNotifyUnread(tab({ reason: "done" }), tab({ reason: "done" }))).toBe(false);
  });

  it("never fires for a repeat, however it arrives", () => {
    const rep = tab({ reason: "done", repeat: true });
    expect(shouldNotifyUnread(rep, tab(null))).toBe(false);
    expect(shouldNotifyUnread(rep, tab({ reason: "done" }))).toBe(false);
    expect(shouldNotifyUnread(rep, rep)).toBe(false);
  });

  // THE case the whole fix exists for, spelled out as the sequence a long turn
  // actually produces. Before the fix the last step fired a second banner.
  it("gives a two-stage turn exactly one notification", () => {
    const steps: Array<[U | null, string]> = [
      [{ reason: "done" }, "stage 1 looks finished"],
      [null, "back to work: the store clears the premature done"],
      [{ reason: "done", repeat: true }, "the turn really ends"],
    ];
    let prev: U | null = null;
    let fired = 0;
    for (const [next] of steps) {
      if (shouldNotifyUnread(tab(next), tab(prev))) fired += 1;
      prev = next;
    }
    expect(fired).toBe(1);
  });

  it("still lets a real needs-you through on top of a suppressed repeat", () => {
    // The subtle half. A suppressed repeat leaves `unread` SET, so a
    // truthiness-based edge check would swallow the permission prompt that
    // lands after it: the agent asking for the user, silently. Newsworthiness
    // is what the edge is measured on, so the attention still fires.
    const suppressed = tab({ reason: "done", repeat: true });
    expect(shouldNotifyUnread(tab({ reason: "attention" }), suppressed)).toBe(true);
  });

  it("does not fire twice for one needs-you", () => {
    // A single permission prompt marks attention twice: termic's hook the
    // instant claude blocks, then claude's own OSC 9 6.0s later. Only the first
    // is a banner, and the second now says so (`repeat`) rather than relying on
    // the edge happening to be spent - if anything had cleared `unread` in
    // between, the old code fired a second banner for the same prompt.
    const a = tab({ reason: "attention" });
    expect(shouldNotifyUnread(a, tab(null))).toBe(true);
    expect(shouldNotifyUnread(tab({ reason: "attention", repeat: true }), a)).toBe(false);
    // Including across a clear, which is what the plain-truthiness version
    // could not survive.
    expect(shouldNotifyUnread(tab({ reason: "attention", repeat: true }), tab(null))).toBe(false);
  });
});

// The echo window is a constant rather than a state test, and the reason is
// worth a test of its own: the state-only version went silent on real prompts.
describe("ATTENTION_ECHO_MS", () => {
  it("outlasts the measured gap between the two marks for one prompt", () => {
    // termic's hook fires the instant claude blocks; claude's own OSC 9 lands
    // 6.0s later. A window under that would let the second one raise a banner
    // for a prompt the user has already been shown.
    expect(ATTENTION_ECHO_MS).toBeGreaterThan(6_000);
  });

  it("stays short enough that a later, genuine needs-you is still news", () => {
    // The hole this shape exists to avoid: a permission prompt is answered with
    // a BARE KEY, and only Enter clears the unread mark, so "an attention mark
    // is already held" is true long after the user dealt with it. Anything on
    // the order of a minute would swallow the next real one.
    expect(ATTENTION_ECHO_MS).toBeLessThanOrEqual(30_000);
  });
});

describe("UNREAD_PHRASE", () => {
  it("has wording for every reason", () => {
    // A missing entry used to fall through to "is idle", which reads as
    // nothing having happened - the worst possible body for a banner.
    for (const r of ["bell", "exit", "done", "attention", "idle"] as const) {
      expect(UNREAD_PHRASE[r]).toBeTruthy();
    }
  });

  it("is free of em dashes, like all user-visible copy", () => {
    for (const v of Object.values(UNREAD_PHRASE)) expect(v).not.toContain("—");
  });
});
