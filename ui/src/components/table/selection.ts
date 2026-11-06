/**
 * Selection model for the table.
 *
 * The model is pure: every function takes the current selection as an
 * immutable set of row ids and returns a new set, so the table component never
 * has to know how a click was produced. Row ids (not indexes) are selected, so
 * sorting or filtering never moves a selection onto a different row.
 *
 * Rules:
 * - a plain click selects exactly that row;
 * - Cmd-click (or Ctrl-click) toggles one row;
 * - Shift-click selects the inclusive range from the last clicked row;
 * - Shift-click with Cmd adds the range to the current selection;
 * - "select all" only adds or removes the rows currently on screen, which is
 *   the filtered model the screen passed in.
 */

/** How many rows a table lets the user select at once. */
export type SelectionMode = "none" | "single" | "multiple";

/** Modifier keys of the pointer event that triggered a selection change. */
export interface SelectionModifiers {
  readonly metaKey?: boolean;
  readonly ctrlKey?: boolean;
  readonly shiftKey?: boolean;
}

/** One row click, described without any DOM objects. */
export interface SelectionClick {
  /** Row ids in the order they are displayed (the sorted, filtered model). */
  readonly order: readonly string[];
  /** Currently selected row ids. */
  readonly selection: ReadonlySet<string>;
  /** Row id of the previous non-range click, or `null` before the first click. */
  readonly anchor: string | null;
  /** Row id that was clicked. */
  readonly rowId: string;
  readonly mode: SelectionMode;
  readonly modifiers: SelectionModifiers;
}

/** The next selection plus the anchor the following Shift-click extends from. */
export interface SelectionResult {
  readonly selection: ReadonlySet<string>;
  readonly anchor: string | null;
}

/** `all` and `some` describe the displayed rows only, never the whole store. */
export type BulkSelectionState = "none" | "some" | "all";

/**
 * Applies one click (or Space press) to the selection model.
 *
 * A Shift-click whose anchor is no longer in the displayed order (filtered
 * away, for example) falls back to a plain click instead of guessing a range.
 */
export function selectionForClick(click: SelectionClick): SelectionResult {
  const { order, selection, anchor, rowId, mode, modifiers } = click;

  if (mode === "none" || !order.includes(rowId)) {
    return { selection, anchor };
  }

  if (mode === "single") {
    return { selection: new Set([rowId]), anchor: rowId };
  }

  const additive = modifiers.metaKey === true || modifiers.ctrlKey === true;

  if (modifiers.shiftKey === true && anchor !== null && anchor !== rowId) {
    const from = order.indexOf(anchor);
    const to = order.indexOf(rowId);
    if (from !== -1 && to !== -1) {
      const start = Math.min(from, to);
      const end = Math.max(from, to);
      const next = additive ? new Set(selection) : new Set<string>();
      for (const id of order.slice(start, end + 1)) {
        next.add(id);
      }
      return { selection: next, anchor };
    }
  }

  if (additive) {
    const next = new Set(selection);
    if (next.has(rowId)) {
      next.delete(rowId);
    } else {
      next.add(rowId);
    }
    return { selection: next, anchor: rowId };
  }

  return { selection: new Set([rowId]), anchor: rowId };
}

/** Adds or removes exactly one row, the behaviour of Space on a focused row. */
export function toggleSelection(
  selection: ReadonlySet<string>,
  rowId: string,
): ReadonlySet<string> {
  const next = new Set(selection);
  if (next.has(rowId)) {
    next.delete(rowId);
  } else {
    next.add(rowId);
  }
  return next;
}

/** Describes how many of the displayed rows are selected. */
export function bulkSelectionState(
  displayedIds: readonly string[],
  selection: ReadonlySet<string>,
): BulkSelectionState {
  if (displayedIds.length === 0) {
    return "none";
  }
  let selected = 0;
  for (const id of displayedIds) {
    if (selection.has(id)) {
      selected += 1;
    }
  }
  if (selected === 0) {
    return "none";
  }
  return selected === displayedIds.length ? "all" : "some";
}

/**
 * Selects or clears every displayed row.
 *
 * Rows that are not displayed keep their previous state: selecting all while a
 * filter is active must not silently select invisible rows, and clearing must
 * not silently clear them either.
 */
export function setDisplayedSelected(
  displayedIds: readonly string[],
  selection: ReadonlySet<string>,
  selected: boolean,
): ReadonlySet<string> {
  const next = new Set(selection);
  for (const id of displayedIds) {
    if (selected) {
      next.add(id);
    } else {
      next.delete(id);
    }
  }
  return next;
}

/** Count of displayed rows that are selected; used for the bulk action bar. */
export function countSelected(
  displayedIds: readonly string[],
  selection: ReadonlySet<string>,
): number {
  let selected = 0;
  for (const id of displayedIds) {
    if (selection.has(id)) {
      selected += 1;
    }
  }
  return selected;
}
