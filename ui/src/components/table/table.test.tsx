import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { RenderResult } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterAll, afterEach, beforeAll, describe, expect, test, vi } from "vitest";
import type { ColumnDef } from "@tanstack/react-table";
import { useState } from "react";
import { agentColumns } from "./agent-columns";
import type { DataTableFeatures } from "./features";
import { RowMenuTrigger, useRowMenu } from "./row-menu";
import { DataTable, type DataTableProps } from "./table";
import type { AgentCellState, AgentDescriptor } from "./types";

/**
 * jsdom does not lay out elements, so the virtualizer would see a zero-height
 * viewport and render nothing. Pinning client sizes makes the render window
 * deterministic without faking the virtualizer itself.
 */
beforeAll(() => {
  Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 600 });
  Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => 900 });
});

const ORIGINAL_CLIENT_HEIGHT = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");
const ORIGINAL_CLIENT_WIDTH = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");

const restorePrototypeGetter = (
  name: "clientHeight" | "clientWidth",
  original: PropertyDescriptor | undefined,
) => {
  if (original === undefined) {
    Reflect.deleteProperty(HTMLElement.prototype, name);
  } else {
    Object.defineProperty(HTMLElement.prototype, name, original);
  }
};

afterAll(() => {
  restorePrototypeGetter("clientHeight", ORIGINAL_CLIENT_HEIGHT);
  restorePrototypeGetter("clientWidth", ORIGINAL_CLIENT_WIDTH);
});

afterEach(cleanup);

interface Item {
  readonly id: string;
  readonly name: string;
  readonly state: AgentCellState;
}

const AGENTS: readonly AgentDescriptor[] = [
  { id: "claude-code", label: "Claude Code" },
  { id: "codex", label: "Codex" },
];

const STATE: AgentCellState = { kind: "linked" };

const ITEMS: readonly Item[] = [
  { id: "1", name: "beta", state: STATE },
  { id: "2", name: "alpha", state: STATE },
  { id: "3", name: "beta", state: STATE },
  { id: "4", name: "alpha", state: STATE },
  { id: "5", name: "beta", state: STATE },
];

const NAME_COLUMN: ColumnDef<DataTableFeatures, Item> = {
  accessorKey: "name",
  header: "Name",
  size: 240,
  meta: { grow: true },
};

function itemTable(overrides: Partial<DataTableProps<Item>>): RenderResult {
  return render(
    <DataTable<Item>
      ariaLabel="Skills"
      data={ITEMS}
      columns={[NAME_COLUMN, ...agentColumns<Item>({ agents: AGENTS, getState: (row) => row.state })]}
      getRowId={(item) => item.id}
      getRowLabel={(item) => item.name}
      viewportHeight={600}
      {...overrides}
    />,
  );
}

function renderedIds(container: HTMLElement): string[] {
  return [...container.querySelectorAll<HTMLElement>("[data-row-id]")].map(
    (element) => element.dataset["rowId"] ?? "",
  );
}

function rowElement(container: HTMLElement, id: string): HTMLElement {
  const element = container.querySelector<HTMLElement>(`[data-row-id="${id}"]`);
  if (element === null) {
    throw new Error(`row ${id} is not rendered`);
  }
  return element;
}

function selectedIds(container: HTMLElement): string[] {
  return [...container.querySelectorAll<HTMLElement>("[data-row-id][data-selected]")].map(
    (element) => element.dataset["rowId"] ?? "",
  );
}

describe("DataTable sorting", () => {
  test("a header click sorts ascending, again descending, again back to the original order", () => {
    const { container } = itemTable({});
    const header = screen.getByRole("columnheader", { name: "Name" });
    const button = within(header).getByRole("button", { name: "Name" });

    fireEvent.click(button);
    expect(header.getAttribute("aria-sort")).toBe("ascending");
    expect(renderedIds(container)).toEqual(["2", "4", "1", "3", "5"]);

    fireEvent.click(button);
    expect(header.getAttribute("aria-sort")).toBe("descending");
    expect(renderedIds(container)).toEqual(["1", "3", "5", "2", "4"]);

    fireEvent.click(button);
    expect(header.getAttribute("aria-sort")).toBe("none");
    expect(renderedIds(container)).toEqual(["1", "2", "3", "4", "5"]);
  });

  test("sorting keeps the selection and does not report a selection change", () => {
    const onSelectionChange = vi.fn();
    const { container } = itemTable({ onSelectionChange });

    fireEvent.click(rowElement(container, "3"));
    fireEvent.click(within(screen.getByRole("columnheader", { name: "Name" })).getByRole("button"));

    expect(selectedIds(container)).toEqual(["3"]);
    expect(onSelectionChange).toHaveBeenCalledTimes(1);
    expect(renderedIds(container)).toEqual(["2", "4", "1", "3", "5"]);
  });

  test("agent columns are not sortable and render no button", () => {
    itemTable({});
    const header = screen.getByRole("columnheader", { name: "Claude Code" });

    expect(within(header).queryByRole("button")).toBeNull();
    expect(header.getAttribute("aria-sort")).toBeNull();
  });
});

describe("DataTable selection", () => {
  test("single click replaces, Cmd-click toggles and Shift-click extends the range", () => {
    const { container } = itemTable({});

    fireEvent.click(rowElement(container, "2"));
    expect(selectedIds(container)).toEqual(["2"]);

    fireEvent.click(rowElement(container, "4"), { metaKey: true });
    expect(selectedIds(container).sort()).toEqual(["2", "4"]);

    fireEvent.click(rowElement(container, "5"), { shiftKey: true });
    expect(selectedIds(container).sort()).toEqual(["4", "5"]);
  });

  test("row checkboxes report their own state without changing the rest", () => {
    const { container } = itemTable({});
    fireEvent.click(rowElement(container, "1"));

    const checkbox = within(rowElement(container, "3")).getByRole("checkbox", { name: "Select beta" });
    fireEvent.click(checkbox);

    expect(selectedIds(container).sort()).toEqual(["1", "3"]);
  });

  test("Select all targets the displayed rows and names the count", () => {
    const onSelectionChange = vi.fn();
    const { container } = itemTable({ data: ITEMS.slice(0, 2), onSelectionChange });

    const selectAll = screen.getByRole("checkbox", { name: "Select all 2 rows" });
    fireEvent.click(selectAll);

    expect(selectedIds(container).sort()).toEqual(["1", "2"]);
    expect(onSelectionChange).toHaveBeenLastCalledWith(new Set(["1", "2"]));
    expect(container.querySelector("[role='table']")?.getAttribute("data-selected-rows")).toBe("2");

    fireEvent.click(selectAll);
    expect(selectedIds(container)).toEqual([]);
  });

  test("the header checkbox is indeterminate when only some rows are selected", () => {
    const { container } = itemTable({});
    fireEvent.click(rowElement(container, "2"));

    const selectAll = screen.getByRole("checkbox", { name: "Select all 5 rows" });
    expect(selectAll.getAttribute("aria-checked")).toBe("mixed");
  });

  test("selectionMode none hides the checkbox column and ignores row clicks", () => {
    const { container } = itemTable({ selectionMode: "none" });

    expect(screen.queryByRole("checkbox")).toBeNull();
    fireEvent.click(rowElement(container, "1"));
    expect(selectedIds(container)).toEqual([]);
  });

  test("controlled selection follows the selectedIds prop", () => {
    const { container, rerender } = itemTable({ selectedIds: new Set(["2"]) });
    expect(selectedIds(container)).toEqual(["2"]);

    rerender(
      <DataTable<Item>
        ariaLabel="Skills"
        data={ITEMS}
        columns={[NAME_COLUMN]}
        getRowId={(item) => item.id}
        getRowLabel={(item) => item.name}
        viewportHeight={600}
        selectedIds={new Set(["2", "4"])}
        onSelectionChange={() => undefined}
      />,
    );

    expect(selectedIds(container).sort()).toEqual(["2", "4"]);
  });
});

describe("DataTable keyboard and semantics", () => {
  test("exposes table, rowgroup, row, columnheader and cell roles with counts", () => {
    const { container } = itemTable({});

    const table = screen.getByRole("table", { name: "Skills" });
    expect(table.getAttribute("aria-rowcount")).toBe("6");
    expect(table.getAttribute("aria-colcount")).toBe("4");
    expect(screen.getAllByRole("columnheader")).toHaveLength(4);
    expect(within(table).getAllByRole("row")).toHaveLength(6);
    expect(container.querySelectorAll("[role='cell']")).toHaveLength(5 * 4);
  });

  test("one row is in the tab order and Arrow keys move focus to the next row", () => {
    const { container } = itemTable({});
    const first = rowElement(container, "1");
    const second = rowElement(container, "2");

    expect(first.getAttribute("tabindex")).toBe("0");
    expect(second.getAttribute("tabindex")).toBe("-1");

    first.focus();
    fireEvent.keyDown(first, { key: "ArrowDown" });

    expect(document.activeElement).toBe(second);
    expect(second.getAttribute("tabindex")).toBe("0");
  });

  test("Space toggles the focused row and Enter activates it", () => {
    const onRowActivate = vi.fn();
    const { container } = itemTable({ onRowActivate });
    const first = rowElement(container, "1");

    first.focus();
    fireEvent.keyDown(first, { key: " " });
    expect(selectedIds(container)).toEqual(["1"]);

    fireEvent.keyDown(first, { key: " " });
    expect(selectedIds(container)).toEqual([]);

    fireEvent.keyDown(first, { key: "Enter" });
    expect(onRowActivate).toHaveBeenCalledWith(ITEMS[0]);
  });

  test("clicking a switch inside a row does not select the row", async () => {
    const user = userEvent.setup();
    const onToggle = vi.fn();
    const { container } = itemTable({
      columns: [
        NAME_COLUMN,
        ...agentColumns<Item>({
          agents: AGENTS,
          getState: (row) => row.state,
          onToggle,
        }),
      ],
    });

    const toggle = within(rowElement(container, "1")).getAllByRole("switch")[0];
    if (toggle === undefined) {
      throw new Error("expected an agent switch");
    }
    await user.click(toggle);

    expect(onToggle).toHaveBeenCalledWith(ITEMS[0], AGENTS[0], false);
    expect(selectedIds(container)).toEqual([]);
  });
});

describe("DataTable row actions and spans", () => {
  test("the row menu opens from the trigger and closes on an outside click or Escape", async () => {
    const user = userEvent.setup();

    function Harness() {
      const [activated, setActivated] = useState(0);
      const menu = useRowMenu();
      return (
        <>
          <DataTable<Item>
            ariaLabel="Skills"
            data={ITEMS.slice(0, 2)}
            columns={[NAME_COLUMN]}
            getRowId={(item) => item.id}
            getRowLabel={(item) => item.name}
            viewportHeight={300}
            rowActions={(item) => (
              <RowMenuTrigger
                label={`Actions for ${item.name}`}
                open={menu.isOpen(item.id)}
                onToggle={() => menu.toggle(item.id)}
              />
            )}
          />
          <button type="button" onClick={() => setActivated((value) => value + 1)}>
            Outside {activated}
          </button>
          {menu.openRowId === null ? null : (
            <div role="menu" data-row-menu={menu.openRowId}>
              Menu for {menu.openRowId}
            </div>
          )}
        </>
      );
    }

    render(<Harness />);
    const trigger = screen.getByRole("button", { name: "Actions for alpha" });

    await user.click(trigger);
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("menu").textContent).toBe("Menu for 2");

    await user.click(screen.getByRole("button", { name: /Outside/ }));
    expect(screen.queryByRole("menu")).toBeNull();
    expect(trigger.getAttribute("aria-expanded")).toBe("false");

    await user.click(trigger);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).toBeNull();

    await user.click(trigger);
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    await user.click(trigger);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("getColumnSpan merges agent columns into one cell", () => {
    const { container } = itemTable({
      getColumnSpan: (row, columnId) =>
        row.id === "1" && columnId === "agent:claude-code" ? 2 : 1,
    });

    const row = rowElement(container, "1");
    const cells = row.querySelectorAll<HTMLElement>("[role='cell']");
    expect(cells).toHaveLength(3);
    expect(cells[2]?.getAttribute("aria-colspan")).toBe("2");
    expect(row.querySelectorAll("[data-slot='agent-cell']")).toHaveLength(1);
  });
});

describe("DataTable states", () => {
  test("loading delegates to the TU-021 StateView with the same reserved height", () => {
    itemTable({ data: [], loading: true });

    const state = document.querySelector<HTMLElement>("[data-slot='state-view']");
    expect(state?.dataset["variant"]).toBe("loading");
    expect(state?.style.minHeight).toBe("600px");
    expect(screen.queryByRole("table")).toBeNull();
    expect(document.querySelectorAll("[data-slot='skeleton-row']")).toHaveLength(6);
  });

  test("empty delegates to the StateView, keeps the height and shows its action", () => {
    const onAdd = vi.fn();
    itemTable({ data: [], emptyTitle: "No skills in this project", emptyAction: { label: "Add skill", onClick: onAdd } });

    const state = document.querySelector<HTMLElement>("[data-slot='state-view']");
    expect(state?.dataset["variant"]).toBe("empty");
    expect(state?.style.minHeight).toBe("600px");
    expect(screen.getByRole("heading", { name: "No skills in this project" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Add skill" }));
    expect(onAdd).toHaveBeenCalledTimes(1);
  });
});
