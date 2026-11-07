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
  /** True when `rowId` owns the open menu. */
  readonly isOpen: (rowId: string) => boolean;
  /** Opens the menu for `rowId`, or closes it when it is already open. */
  readonly toggle: (rowId: string) => void;
  /** Closes whichever menu is open. */
  readonly close: () => void;
}

const MENU_SELECTOR = "[data-row-menu]";

/**
 * Tracks which row's menu is open.
 *
 * The document listeners exist only while a menu is open, so a screen with a
 * closed menu pays nothing.
 */
export function useRowMenu(): RowMenuController {
  const [openRowId, setOpenRowId] = useState<string | null>(null);

  useEffect(() => {
    if (openRowId === null) {
      return;
    }
    const onPointerDown = (event: PointerEvent) => {
      if (event.target instanceof Element && event.target.closest(MENU_SELECTOR) !== null) {
        return;
      }
      setOpenRowId(null);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpenRowId(null);
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [openRowId]);

  const toggle = useCallback((rowId: string) => {
    setOpenRowId((current) => (current === rowId ? null : rowId));
  }, []);
  const close = useCallback(() => setOpenRowId(null), []);
  const isOpen = useCallback((rowId: string) => openRowId === rowId, [openRowId]);

  return { openRowId, isOpen, toggle, close };
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

