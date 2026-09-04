// Minimal themed wrapper over Radix Popover. Same chrome language as
// Dialog/Dropdown (dark surface, soft border, shadow). Used for small
// anchored forms like the message queue.

import * as P from "@radix-ui/react-popover";
import { type ReactNode } from "react";
import { cn } from "@/lib/utils";

export const PopoverRoot = P.Root;
export const PopoverTrigger = P.Trigger;
export const PopoverAnchor = P.Anchor;

interface ContentProps {
  children: ReactNode;
  side?: "top" | "right" | "bottom" | "left";
  align?: "start" | "center" | "end";
  sideOffset?: number;
  className?: string;
  /** Radix focus-return hook — preventDefault() to keep focus where it is. */
  onCloseAutoFocus?: (event: Event) => void;
  onOpenAutoFocus?: (event: Event) => void;
}

export function PopoverContent({
  children, side = "top", align = "end", sideOffset = 6, className, onCloseAutoFocus, onOpenAutoFocus,
}: ContentProps) {
  return (
    <P.Portal>
      <P.Content
        side={side}
        align={align}
        sideOffset={sideOffset}
        collisionPadding={8}
        onCloseAutoFocus={onCloseAutoFocus}
        onOpenAutoFocus={onOpenAutoFocus}
        className={cn(
          // `outline-none` on the CONTAINER, not on the controls inside it.
          // Radix gives Content `tabindex="-1"` and focuses it on open, and
          // WebKit then paints its own blue focus ring around the whole panel
          // — a colour from no theme, on a box the user cannot type into.
          // index.css's `:focus-visible` rule deliberately skips
          // `[tabindex="-1"]`, so this is the container's own business.
          // Real controls inside a popover still get the themed ring.
          "z-50 rounded-xl border border-[var(--color-border)] bg-[var(--color-bg-1)] p-3 shadow-2xl outline-none",
          "data-[state=open]:animate-in data-[state=open]:fade-in-0",
          className,
        )}
      >{children}</P.Content>
    </P.Portal>
  );
}
