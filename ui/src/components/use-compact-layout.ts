import { useCallback, useSyncExternalStore } from "react";

/**
 * Window-width breakpoint for the compact lists.
 *
 * The desktop window opens at 1440×900 and can shrink to 960×640. Below
 * {@link COMPACT_LAYOUT_MAX_WIDTH} the lists drop their secondary column
 * (Skills `Status`, MCP `Health`) instead of scrolling sideways: the table is
 * a CSS grid of declared column widths, so a column can only disappear by
 * leaving the model, and a model decision needs the width in JavaScript.
 *
 * The threshold is derived, not round: the lists live in `main` (24px padding
 * each side) right of the 200px sidebar and carry 8px of row padding, so the
 * scrolling viewport is `window - 250`. The widest list (MCP Servers) needs
 * 1020px for every column, which is reached at a 1270px window; the Skills
 * list needs 1002px. Everything from 1270px up keeps the full column set.
 */

/** Window width at or below which the lists use their compact column set. */
export const COMPACT_LAYOUT_MAX_WIDTH = 1269;

/** Media query matching {@link COMPACT_LAYOUT_MAX_WIDTH}. */
export const COMPACT_LAYOUT_QUERY = `(max-width: ${COMPACT_LAYOUT_MAX_WIDTH}px)`;

/** The subset of `window` this hook reads, so tests can pass a stub. */
export interface CompactLayoutWindow {
  readonly matchMedia?: (query: string) => MediaQueryListLike;
}

/** The subset of `MediaQueryList` this hook reads. */
export interface MediaQueryListLike {
  readonly matches: boolean;
  addEventListener?: (type: "change", listener: (event: { matches: boolean }) => void) => void;
  removeEventListener?: (type: "change", listener: (event: { matches: boolean }) => void) => void;
}

/** Reads the current value; browsers without `matchMedia` keep the full set. */
export function compactLayoutMatches(target: CompactLayoutWindow | undefined): boolean {
  return target?.matchMedia?.(COMPACT_LAYOUT_QUERY).matches ?? false;
}

/**
 * Tracks whether the window is at or below {@link COMPACT_LAYOUT_MAX_WIDTH}.
 *
 * Subscribe with `useSyncExternalStore` semantics by hand: the hook re-renders
 * on every crossing, and a window without `matchMedia` (jsdom in older tests)
 * reports the full, non-compact layout so nothing silently loses a column.
 */
export function useCompactLayout(target: CompactLayoutWindow | undefined = typeof window === "undefined" ? undefined : window): boolean {
  const subscribe = useCallback(
    (onChange: () => void) => {
      const query = target?.matchMedia?.(COMPACT_LAYOUT_QUERY);
      query?.addEventListener?.("change", onChange);
      return () => query?.removeEventListener?.("change", onChange);
    },
    [target],
  );
  const getSnapshot = useCallback(() => compactLayoutMatches(target), [target]);
  return useSyncExternalStore(subscribe, getSnapshot, () => false);
}
