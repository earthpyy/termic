// Subscription usage: the OSC body claude's termic status line writes, and the
// parser for it (GH #277).
//
// This is the claude half of the usage feed. The codex half needs no wire
// format at all: `agent_usage.rs` asks `codex app-server` for the numbers over
// JSON-RPC and returns them typed, so there is nothing to parse out of a
// terminal. See docs/ideas/usage-footer.md for why the two providers get
// different transports.
//
// The status line is NOT a hook, but it rides the hook channel: same OSC 777
// `notify`, same trusted `termic` title, same three-target write. It is told
// apart from every other signal on that channel by this prefix alone, which is
// why the prefix must never be a prefix of, or prefixed by, HOOK_OSC_BODY or
// HOOK_OSC_READY_BODY.
//
// KEEP IN SYNC with `agent_hooks::USAGE_BODY_PREFIX` and
// `agent_hooks::statusline_body()`. Both sides pin the literal in their own
// test, because a string cannot be shared across the language boundary.

/** Prefix of the body that reports subscription usage. */
export const USAGE_BODY_PREFIX = "usage ";

/** One rolling limit window. Percentages are 0-100 as the provider reports
 *  them; `resetsAt` is a Unix epoch in SECONDS, or null when unknown. */
export interface UsageWindow {
  usedPercent: number;
  resetsAt: number | null;
}

/** What one account has spent. Either window can be absent: codex on a free
 *  plan reports a single 30-day window and no second one at all, so a UI that
 *  assumes two columns renders an empty one. */
export interface AgentUsage {
  /** The short window. 5 hours for claude; whatever codex reports as the
   *  shorter of its two. */
  session: UsageWindow | null;
  /** The long window. 7 days for claude. */
  weekly: UsageWindow | null;
}

/** One space-separated field: a number, or `-` for "the agent did not say".
 *
 *  Deliberately strict. The body is an agent-controlled string that reaches a
 *  render path, and `Number("")` is 0, which would paint a confident 0% over a
 *  field that was actually missing. */
function field(raw: string | undefined): number | null {
  if (!raw || raw === "-") return null;
  if (!/^\d+(\.\d+)?$/.test(raw)) return null;
  const n = Number(raw);
  return Number.isFinite(n) ? n : null;
}

/**
 * Parse a trusted `usage …` body, or null when it is not one.
 *
 * Wire format, four space-separated fields after the prefix:
 *
 *     usage <5h percent> <7d percent> <5h resets_at> <7d resets_at>
 *
 * with `-` standing in for any field the payload did not carry. A percentage
 * that fails to parse drops its whole window rather than defaulting, so an
 * unreadable payload shows nothing instead of showing a wrong number.
 */
export function parseUsageBody(body: string): AgentUsage | null {
  if (!body.startsWith(USAGE_BODY_PREFIX)) return null;
  const parts = body.slice(USAGE_BODY_PREFIX.length).trim().split(/\s+/);
  const [fh, sd, fhr, sdr] = parts;
  const fhPct = field(fh);
  const sdPct = field(sd);
  if (fhPct === null && sdPct === null) return null;
  return {
    session: fhPct === null ? null : { usedPercent: clamp(fhPct), resetsAt: field(fhr) },
    weekly: sdPct === null ? null : { usedPercent: clamp(sdPct), resetsAt: field(sdr) },
  };
}

/** Percentages are rendered into a fixed-width bar, so a provider reporting
 *  101 must not overflow it. */
function clamp(n: number): number {
  return Math.min(100, Math.max(0, n));
}

/** Are two usage readings the same? Used to bail before writing an unchanged
 *  value through a store setter, which the status line would otherwise do on
 *  every single turn (docs/performance.md bear trap 8). */
export function sameUsage(a: AgentUsage | undefined, b: AgentUsage | undefined): boolean {
  if (!a || !b) return a === b;
  return sameWindow(a.session, b.session) && sameWindow(a.weekly, b.weekly);
}

function sameWindow(a: UsageWindow | null, b: UsageWindow | null): boolean {
  if (!a || !b) return a === b;
  return a.usedPercent === b.usedPercent && a.resetsAt === b.resetsAt;
}

/** `58%`, or `—` when the window is absent. Rounded, because the footer is a
 *  glance and `14.000000000000002%` is what the provider actually sends. */
export function formatPercent(w: UsageWindow | null): string {
  return w ? `${Math.round(w.usedPercent)}%` : "—";
}

/** "resets 14:00" / "resets Tue 07:00", or "" when the window said nothing.
 *  Tooltip text only: the footer chip itself never reflows on a clock. */
export function formatReset(w: UsageWindow | null): string {
  if (!w || w.resetsAt == null) return "";
  const d = new Date(w.resetsAt * 1000);
  if (Number.isNaN(d.getTime())) return "";
  const today = d.toDateString() === new Date().toDateString();
  return today
    ? `resets ${d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}`
    : `resets ${d.toLocaleDateString(undefined, { weekday: "short" })} ` +
      `${d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}`;
}

/** Where a percentage stops being background information.
 *
 *  Two thresholds, not a gradient. A bar that drifts through a hue tells you
 *  nothing you were not already reading off the number; what a footer owes you
 *  is the moment the number starts to matter. 70 is "plan the rest of the day
 *  around this", 90 is "you are about to be blocked mid-turn".
 *
 *  Below WARN the bar is deliberately NEUTRAL rather than green. This number
 *  only ever goes up, so a green bar is not good news, it is early news, and a
 *  window of tasks each showing a green bar is noise that trains you to stop
 *  looking at the one that turns amber. */
export const USAGE_WARN_PERCENT = 70;
export const USAGE_CRITICAL_PERCENT = 90;

export type UsageLevel = "normal" | "warn" | "critical";

export function usageLevel(usedPercent: number): UsageLevel {
  if (usedPercent >= USAGE_CRITICAL_PERCENT) return "critical";
  if (usedPercent >= USAGE_WARN_PERCENT) return "warn";
  return "normal";
}

/** Which window the bar and its colour should be about: whichever is CLOSEST
 *  TO ITS LIMIT, not whichever is shorter.
 *
 *  The session window is the one people watch, but it is not always the one
 *  that stops them. 30% of five hours next to 95% of the week is a footer that
 *  has to say "week", or it reports a comfortable number right up until the
 *  turn that fails. Returns null only when neither window was reported. */
export function drivingWindow(u: AgentUsage): { window: UsageWindow; label: "5h" | "wk" } | null {
  const s = u.session ? { window: u.session, label: "5h" as const } : null;
  const w = u.weekly ? { window: u.weekly, label: "wk" as const } : null;
  if (!s) return w;
  if (!w) return s;
  return w.window.usedPercent > s.window.usedPercent ? w : s;
}
