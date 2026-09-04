// The sandbox mode table is a single source of truth that had one caller
// quietly opting out of it.
//
// `SandboxIcon` used to take an `icon` override, and exactly one surface used
// it: the toolbar swapped monitoring's shield for an Eye. That is the surface a
// user reads at a glance, so the one place it mattered most was the one place
// disagreeing with the sidebar row, the footer chip and the mode picker.
// Reported as "this top-right icon is outdated, for monitoring we use the
// outline orange".
//
// The override is gone, so the type system now prevents a repeat. What it
// cannot pin is what each mode should LOOK like, which is what this does.

import { describe, it, expect } from "vitest";
import { Shield, ShieldOff } from "lucide-react";
import { SANDBOX_VISUALS, SANDBOX_PICKER_ORDER, sandboxPickerLabel } from "@/components/SandboxIcon";
import type { SandboxMode } from "@/lib/types";

describe("SANDBOX_VISUALS", () => {
  it("gives monitoring an OUTLINE shield in the warning colour", () => {
    const v = SANDBOX_VISUALS.monitor;
    expect(v.Icon).toBe(Shield);
    expect(v.filled).toBe(false);
    expect(v.color).toBe("var(--color-warn)");
  });

  it("separates the two enforce modes by FILL, not by colour or glyph", () => {
    // Both are a real cage, so both are green; fill is the only thing that says
    // whether the network is caged too. Changing either to a different glyph
    // would break the "same family, one difference" read.
    expect(SANDBOX_VISUALS.enforce.color).toBe(SANDBOX_VISUALS["enforce-fs"].color);
    expect(SANDBOX_VISUALS.enforce.Icon).toBe(SANDBOX_VISUALS["enforce-fs"].Icon);
    expect(SANDBOX_VISUALS.enforce.filled).toBe(true);
    expect(SANDBOX_VISUALS["enforce-fs"].filled).toBe(false);
  });

  it("marks OFF as the only mode without a shield", () => {
    expect(SANDBOX_VISUALS.off.Icon).toBe(ShieldOff);
    const caged: SandboxMode[] = ["monitor", "enforce-fs", "enforce"];
    for (const m of caged) expect(SANDBOX_VISUALS[m].Icon).toBe(Shield);
  });

  it("describes every mode, so no surface has to invent a label", () => {
    for (const m of SANDBOX_PICKER_ORDER) {
      const v = SANDBOX_VISUALS[m];
      expect(v.label.length).toBeGreaterThan(0);
      expect(v.shortLabel.length).toBeGreaterThan(0);
      expect(v.desc.length).toBeGreaterThan(0);
      // User-visible copy, so the repo's no-em-dash rule applies.
      for (const s of [v.label, v.shortLabel, v.desc]) expect(s).not.toContain("—");
    }
  });

  it("covers every mode in the picker order, in the documented layout", () => {
    // Row-major into a 2-col grid: the two "safe" modes on the left, the two
    // enforcing ones on the right.
    expect(SANDBOX_PICKER_ORDER).toEqual(["off", "enforce-fs", "monitor", "enforce"]);
    expect(new Set(SANDBOX_PICKER_ORDER).size).toBe(Object.keys(SANDBOX_VISUALS).length);
  });

  it("uppercases only the leading keyword in a picker label", () => {
    expect(sandboxPickerLabel("monitor")).toBe("MONITORING");
    expect(sandboxPickerLabel("enforce")).toBe("ENFORCING (filesystem + network)");
  });
});
