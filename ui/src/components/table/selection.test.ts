import { describe, expect, test } from "vitest";
import {
  bulkSelectionState,
  countSelected,
  selectionForClick,
  setDisplayedSelected,
  toggleSelection,
  type SelectionClick,
} from "./selection";

/**
 * The selection model is pure, so these tests describe the product rules
 * directly: one click selects one row, Cmd toggles, Shift extends from the
 * anchor, and "select all" never reaches rows that are not displayed.
 */

const ORDER = ["alpha", "bravo", "charlie", "delta", "echo"];

function click(overrides: Partial<SelectionClick>): SelectionClick {
  return {
    order: ORDER,
    selection: new Set<string>(),
    anchor: null,
    rowId: "alpha",
    mode: "multiple",
    modifiers: {},
    ...overrides,
  };
}

describe("selectionForClick", () => {
  test("a plain click replaces the selection with the clicked row", () => {
    const result = selectionForClick(
      click({ selection: new Set(["bravo", "charlie"]), anchor: "bravo", rowId: "delta" }),
    );

    expect([...result.selection]).toEqual(["delta"]);
    expect(result.anchor).toBe("delta");
  });

  test("Cmd-click toggles one row and moves the anchor", () => {
    const added = selectionForClick(
      click({ selection: new Set(["alpha"]), anchor: "alpha", rowId: "charlie", modifiers: { metaKey: true } }),
    );
    expect([...added.selection].sort()).toEqual(["alpha", "charlie"]);
    expect(added.anchor).toBe("charlie");

    const removed = selectionForClick(
      click({
        selection: new Set(["alpha", "charlie"]),
        anchor: "charlie",
        rowId: "alpha",
        modifiers: { ctrlKey: true },
      }),
    );
    expect([...removed.selection]).toEqual(["charlie"]);
    expect(removed.anchor).toBe("alpha");
  });

  test("Shift-click selects the inclusive range from the anchor in display order", () => {
    const result = selectionForClick(
      click({
        selection: new Set(["alpha"]),
        anchor: "bravo",
        rowId: "delta",
        modifiers: { shiftKey: true },
      }),
    );

    expect([...result.selection]).toEqual(["bravo", "charlie", "delta"]);
    expect(result.anchor).toBe("bravo");
  });

  test("Shift-click works upward and Shift+Cmd adds the range to the selection", () => {
    const upward = selectionForClick(
      click({ anchor: "delta", rowId: "bravo", modifiers: { shiftKey: true } }),
    );
    expect([...upward.selection]).toEqual(["bravo", "charlie", "delta"]);

    const additive = selectionForClick(
      click({
        selection: new Set(["alpha"]),
        anchor: "delta",
        rowId: "echo",
        modifiers: { shiftKey: true, metaKey: true },
      }),
    );
    expect([...additive.selection].sort()).toEqual(["alpha", "delta", "echo"]);
  });

  test("Shift-click with an anchor hidden by a filter falls back to a plain click", () => {
    const result = selectionForClick(
      click({ selection: new Set(["zulu"]), anchor: "zulu", rowId: "charlie", modifiers: { shiftKey: true } }),
    );

    expect([...result.selection]).toEqual(["charlie"]);
    expect(result.anchor).toBe("charlie");
  });

  test("single mode ignores modifiers and keeps exactly one row", () => {
    const result = selectionForClick(
      click({ selection: new Set(["alpha", "bravo"]), anchor: "alpha", rowId: "charlie", mode: "single", modifiers: { metaKey: true } }),
    );

    expect([...result.selection]).toEqual(["charlie"]);
  });

  test("none mode never changes the selection", () => {
    const selection = new Set(["alpha"]);
    const result = selectionForClick(click({ mode: "none", selection }));

    expect(result.selection).toBe(selection);
  });
});

describe("bulk selection", () => {
  test("reports none, some and all for the displayed rows only", () => {
    expect(bulkSelectionState(ORDER, new Set())).toBe("none");
    expect(bulkSelectionState(ORDER, new Set(["bravo"]))).toBe("some");
    expect(bulkSelectionState(ORDER, new Set(ORDER))).toBe("all");
    expect(bulkSelectionState(ORDER, new Set([...ORDER, "hidden"]))).toBe("all");
    expect(bulkSelectionState([], new Set(ORDER))).toBe("none");
  });

  test("select all adds only displayed rows and leaves hidden selections alone", () => {
    const selected = setDisplayedSelected(ORDER.slice(0, 2), new Set(["zulu"]), true);

    expect([...selected].sort()).toEqual(["alpha", "bravo", "zulu"]);
    expect(bulkSelectionState(ORDER.slice(0, 2), selected)).toBe("all");
  });

  test("clearing all removes only displayed rows", () => {
    const selected = setDisplayedSelected(ORDER.slice(0, 2), new Set(["alpha", "charlie"]), false);

    expect([...selected]).toEqual(["charlie"]);
  });

  test("countSelected counts displayed rows, not the whole map", () => {
    expect(countSelected(ORDER, new Set(["alpha", "echo", "zulu"]))).toBe(2);
  });

  test("toggleSelection flips one id without touching the input set", () => {
    const before = new Set(["alpha"]);
    const after = toggleSelection(before, "bravo");

    expect([...after]).toEqual(["alpha", "bravo"]);
    expect([...before]).toEqual(["alpha"]);
    expect([...toggleSelection(after, "bravo")]).toEqual(["alpha"]);
  });
});
