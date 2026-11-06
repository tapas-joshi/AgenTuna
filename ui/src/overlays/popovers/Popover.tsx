/**
 * Popover surface (TU-004).
 *
 * A controlled panel anchored to a widget: pass the live element, or a
 * `DOMRect` captured from a click or a row. Radix Popover supplies dismissal
 * (outside click, Escape), focus moves into the panel, and floating-ui keeps
 * the panel inside the window — flipping it to the other side when the
 * preferred side has no room, shifting it along the anchor when it would cross
 * the edge, and re-measuring on anchor scroll and window resize.
 *
 * The panel never shrinks below 220px and never grows past the space the
 * positioning engine reports, so its content scrolls instead of leaving the
 * viewport.
 */

import * as PopoverPrimitive from "@radix-ui/react-popover";
import { useMemo, useRef } from "react";
import type { CSSProperties, ReactNode, RefObject } from "react";
import { cn } from "../../components/cn";
import { useOverlayLayer } from "../stack/OverlayStackProvider";
import type { AnchorRect, MeasurableAnchor } from "./anchor";
import { isAnchorRect, virtualAnchorFromRect } from "./anchor";
import "./popover.css";

/** Side of the anchor the panel prefers. */
export type PopoverSide = "top" | "right" | "bottom" | "left";

/** Alignment along the chosen side. */
export type PopoverAlign = "start" | "center" | "end";

/**
 * Space, in CSS pixels, the panel keeps between itself and the window edge.
 * Shared with the stylesheet through `--at-popover-gutter`.
 */
export const POPOVER_VIEWPORT_GUTTER = 8;

/** Properties of {@link Popover}. */
export interface PopoverProps {
  /** Whether the panel is open. */
  readonly open: boolean;
  /** Called with `false` when outside click, Escape or a close control dismisses. */
  readonly onOpenChange?: (open: boolean) => void;
  /**
   * Live element or captured rectangle the panel anchors to. The panel stays
   * closed while this is `null`, because an unanchored panel has no honest
   * position on screen.
   */
  readonly anchor: Element | AnchorRect | null;
  /** Preferred side; floats to another side when there is no room. Defaults to `bottom`. */
  readonly side?: PopoverSide;
  /** Alignment along `side`. Defaults to `start`. */
  readonly align?: PopoverAlign;
  /** Gap between the anchor and the panel, in CSS pixels. Defaults to 8. */
  readonly sideOffset?: number;
  /** Accessible name of the panel (`role="dialog"`). Required, never empty. */
  readonly ariaLabel: string;
  /** Extra classes for the panel, for screen-specific width or padding. */
  readonly className?: string;
  /** Inline style merged onto the panel, for sizes the layout cannot express. */
  readonly style?: CSSProperties;
  /** Panel content. */
  readonly children?: ReactNode;
}

/**
 * A panel anchored to a widget rect, kept inside the viewport.
 *
 * Controlled: the caller owns `open` and decides what a dismissal means.
 */
export function Popover({
  open,
  onOpenChange,
  anchor,
  side = "bottom",
  align = "start",
  sideOffset = POPOVER_VIEWPORT_GUTTER,
  ariaLabel,
  className,
  style,
  children,
}: PopoverProps) {
  const zIndex = useOverlayLayer("popover", open);
  // Radix restores focus to its Trigger; a popover anchored to a widget rect has
  // none, so the opener is captured as focus moves in and restored as it leaves.
  const restoreFocusRef = useRef<HTMLElement | null>(null);

  const anchorRef = useMemo<RefObject<MeasurableAnchor | null>>(() => {
    if (anchor === null) {
      return { current: null };
    }
    return { current: isAnchorRect(anchor) ? virtualAnchorFromRect(anchor) : anchor };
  }, [anchor]);

  return (
    <PopoverPrimitive.Root open={open && anchor !== null} onOpenChange={onOpenChange}>
      <PopoverPrimitive.Anchor virtualRef={anchorRef} />
      <PopoverPrimitive.Portal>
        <PopoverPrimitive.Content
          className={cn("at-popover", className)}
          aria-label={ariaLabel}
          side={side}
          align={align}
          sideOffset={sideOffset}
          collisionPadding={POPOVER_VIEWPORT_GUTTER}
          sticky="always"
          updatePositionStrategy="always"
          style={{ ...style, zIndex }}
          onOpenAutoFocus={() => {
            const active = document.activeElement;
            restoreFocusRef.current = active instanceof HTMLElement ? active : null;
          }}
          onCloseAutoFocus={(event) => {
            const restore = restoreFocusRef.current;
            if (restore === null || !restore.isConnected) {
              return;
            }
            event.preventDefault();
            restore.focus();
          }}
        >
          {children}
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  );
}
