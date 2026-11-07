/**
 * Row menu plumbing shared by every list screen.
 *
 * The table renders a `rowActions` slot per row; the screen owns the menu
 * contents. This hook owns the part every screen would otherwise repeat: only
 * one row menu is open at a time, and it closes on Escape or on a pointer press
 * outside the menu. Mark the trigger and the menu with `data-row-menu` so
 * inside clicks are not treated as outside clicks.
 */
import { useCallback, useEffect, useState } from "react";
import type { MouseEvent } from "react";
import { cn } from "../cn";

/** Open state and actions for a single-row menu. */
export interface RowMenuController {
  /** Row id whose menu is open, or `null`. */
  readonly openRowId: string | null;
  /**
   * Trigger element of the open menu, or `null` when closed. Feeds
   * {@link useRowMenuPlacement} so the menu opens on the roomier side.
   */
  readonly anchor: Element | null;
  /** True when `rowId` owns the open menu. */
  readonly isOpen: (rowId: string) => boolean;
  /** Opens the menu for `rowId`, or closes it when it is already open. */
  readonly toggle: (rowId: string, anchor?: Element | null) => void;
  /** Closes whichever menu is open. */
  readonly close: () => void;
}

const MENU_SELECTOR = "[data-row-menu]";

/** Gap between the trigger and its menu, in pixels. */
const ROW_MENU_GAP = 4;
/** Tallest a row menu grows before it scrolls its own items. */
const ROW_MENU_MAX_HEIGHT = 320;

/** Where an open row menu fits: which side of its trigger, and how tall it may be. */
export interface RowMenuPlacement {
  /** `below` when opening downward, `above` when the menu flips upward. */
  readonly side: "above" | "below";
  /** Tallest the menu may render before its items scroll, in pixels. */
  readonly maxHeight: number;
}

/** The visible box of the nearest ancestor that clips, limited to the window. */
export function rowMenuBounds(anchor: Element): { readonly top: number; readonly bottom: number } {
  for (let ancestor = anchor.parentElement; ancestor; ancestor = ancestor.parentElement) {
    const style = getComputedStyle(ancestor);
    if (
      style.overflowY !== "auto" &&
      style.overflowY !== "scroll" &&
      style.overflowY !== "hidden" &&
      style.overflowY !== "clip"
    ) {
      continue;
    }
    const box = ancestor.getBoundingClientRect();
    return { top: Math.max(box.top, 0), bottom: Math.min(box.bottom, window.innerHeight) };
  }
  return { top: 0, bottom: window.innerHeight };
}

/**
 * Picks the side of the trigger with more room and clamps the menu to it.
 *
 * A menu opened from a row near the bottom of the table would otherwise render
 * past the scroll container and have its last items clipped away, which makes
 * them unclickable. Choosing the roomier side keeps every item reachable; when
 * neither side is tall enough the menu scrolls its own items.
 */
export function rowMenuPlacement(
  anchor: DOMRect,
  bounds: { readonly top: number; readonly bottom: number },
): RowMenuPlacement {
  const below = bounds.bottom - anchor.bottom - ROW_MENU_GAP;
  const above = anchor.top - bounds.top - ROW_MENU_GAP;
  const side = below >= above ? "below" : "above";
  const room = side === "below" ? below : above;
  return { side, maxHeight: Math.max(0, Math.min(ROW_MENU_MAX_HEIGHT, Math.floor(room))) };
}

/**
 * Placement for the open menu, measured when it opens and on resize or scroll.
 *
 * The listeners exist only while a menu is open: a table renders one trigger
 * per row, so subscribing per row would add idle wake-ups for no benefit.
 */
export function useRowMenuPlacement(anchor: Element | null, open: boolean): RowMenuPlacement {
  const [placement, setPlacement] = useState<RowMenuPlacement>({
    side: "below",
    maxHeight: ROW_MENU_MAX_HEIGHT,
  });

  useEffect(() => {
    if (!open || anchor === null) return;
    const measure = () => {
      setPlacement(rowMenuPlacement(anchor.getBoundingClientRect(), rowMenuBounds(anchor)));
    };
    measure();
    window.addEventListener("resize", measure);
    document.addEventListener("scroll", measure, true);
    return () => {
      window.removeEventListener("resize", measure);
      document.removeEventListener("scroll", measure, true);
    };
  }, [anchor, open]);

  return placement;
}

/**
 * Tracks which row's menu is open.
 *
 * The document listeners exist only while a menu is open, so a screen with a
 * closed menu pays nothing.
 */
export function useRowMenu(): RowMenuController {
  const [openRow, setOpenRow] = useState<{ readonly id: string; readonly anchor: Element | null } | null>(
    null,
  );
  const openRowId = openRow?.id ?? null;

  useEffect(() => {
    if (openRow === null) {
      return;
    }
    const onPointerDown = (event: PointerEvent) => {
      if (event.target instanceof Element && event.target.closest(MENU_SELECTOR) !== null) {
        return;
      }
      setOpenRow(null);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpenRow(null);
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [openRow]);

  const toggle = useCallback((rowId: string, anchor: Element | null = null) => {
    setOpenRow((current) => (current?.id === rowId ? null : { id: rowId, anchor }));
  }, []);
  const close = useCallback(() => setOpenRow(null), []);
  const isOpen = useCallback((rowId: string) => openRowId === rowId, [openRowId]);

  return { openRowId, anchor: openRow?.anchor ?? null, isOpen, toggle, close };
}

/** Props accepted by {@link RowMenuTrigger}. */
export interface RowMenuTriggerProps {
  /** Accessible name, for example `Actions for tdd`. Never a bare icon. */
  readonly label: string;
  /** True while this row's menu is open; rendered as `aria-expanded`. */
  readonly open: boolean;
  /** Whether the row action is temporarily unavailable. */
  readonly disabled?: boolean;
  /** Id of the menu element this trigger controls, when the menu is rendered. */
  readonly controls?: string;
  /** Reports the click; the table marks the event as row-interactive already. */
  readonly onToggle: (event: MouseEvent<HTMLButtonElement>) => void;
  readonly className?: string;
}

/**
 * The `...` trigger that opens a row menu.
 *
 * It is an ordinary button with `aria-haspopup="menu"`, so Enter and Space work
 * without extra key handling, and it is marked `data-row-interactive` so the
 * click does not also change the row selection.
 */
export function RowMenuTrigger({ label, open, disabled = false, controls, onToggle, className }: RowMenuTriggerProps) {
  return (
    <button
      type="button"
      aria-haspopup="menu"
      aria-expanded={open}
      aria-disabled={disabled || undefined}
      aria-controls={controls}
      aria-label={label}
      data-row-interactive=""
      data-row-menu=""
      data-slot="row-menu-trigger"
      onClick={disabled ? undefined : onToggle}
      className={cn(
        "inline-flex size-24 items-center justify-center rounded-6 text-text-muted transition-colors",
        "hover:bg-overlay hover:text-text",
        "aria-disabled:cursor-not-allowed aria-disabled:text-text-secondary",
        "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
        className,
      )}
    >
      <svg aria-hidden="true" viewBox="0 0 12 12" className="size-12" fill="currentColor" focusable="false">
        <circle cx="2.5" cy="6" r="1" />
        <circle cx="6" cy="6" r="1" />
        <circle cx="9.5" cy="6" r="1" />
      </svg>
    </button>
  );
}

