// Task port range (GH #271): the window Settings → Tasks lets the user pick
// for new task port blocks. The allocator itself lives in Rust
// (`next_base_port`); this is the front-end guard that keeps a range the
// backend could never allocate from ever reaching settings.json, plus the
// capacity number the field previews.
//
// The two ends are INCLUSIVE, the way the user reads "3000 to 4000".

/** What the allocator used before the setting existed, and what an absent
 *  or unusable stored range falls back to (`PortRange::default` in lib.rs). */
export const PORT_RANGE_DEFAULT_START = 18100;
export const PORT_RANGE_DEFAULT_END = 65535;

/** Below this are the privileged ports, which a task's dev server could not
 *  bind without root. Mirrors `PORT_FLOOR` in lib.rs. */
export const PORT_RANGE_MIN = 1024;
export const PORT_RANGE_MAX = 65535;

/** The smallest block one task takes: 1 ($TERMIC_PORT) + the 5-port buffer
 *  that leaves room for named ports added later. Mirrors `block_len(0, 0)`.
 *  A range narrower than this could never allocate anything. */
export const PORT_BLOCK_MIN = 6;

/** Why this range cannot be saved, or null when it can. One sentence, shown
 *  under the fields; the Save button reads the same call. */
export function portRangeError(start: number, end: number): string | null {
  for (const v of [start, end]) {
    if (!Number.isInteger(v) || v < PORT_RANGE_MIN || v > PORT_RANGE_MAX) {
      return `Ports must be whole numbers between ${PORT_RANGE_MIN} and ${PORT_RANGE_MAX}.`;
    }
  }
  if (end < start) return "The end of the range must not be below its start.";
  if (end - start + 1 < PORT_BLOCK_MIN) {
    return `A task takes ${PORT_BLOCK_MIN} consecutive ports, so the range needs at least that many.`;
  }
  return null;
}

/** How many plain tasks the range holds, for the preview line. A task with
 *  extra named ports or multi-repo members takes a wider block, so this is
 *  the ceiling, not a promise. Returns 0 for a range that fails validation. */
export function portRangeCapacity(start: number, end: number): number {
  if (portRangeError(start, end)) return 0;
  return Math.floor((end - start + 1) / PORT_BLOCK_MIN);
}
