import { cleanup, render, screen } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, describe, expect, test, vi } from "vitest";
import type { ColumnDef } from "@tanstack/react-table";
import { agentColumns } from "./agent-columns";
import type { DataTableFeatures } from "./features";
import { DataTable, TABLE_HEADER_HEIGHT, TABLE_ROW_HEIGHT } from "./table";
import type { AgentCellState, AgentDescriptor } from "./types";

/**
 * Performance guard: the acceptance criterion is "only visible rows are laid
 * out". jsdom cannot measure frames, so the test asserts the same property
 * through the instrumentation counters the table publishes: a 5,000-row model
 * renders a viewport-sized window of rows and nothing else.
 */

const VIEWPORT = 600;
const OVERSCAN = 8;

beforeAll(() => {
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get: () => VIEWPORT,
  });
  Object.defineProperty(HTMLElement.prototype, "clientWidth", {
    configurable: true,
    get: () => 1200,
  });
});

afterAll(() => {
  Reflect.deleteProperty(HTMLElement.prototype, "clientHeight");
  Reflect.deleteProperty(HTMLElement.prototype, "clientWidth");
});

afterEach(cleanup);

interface SkillRow {
  readonly id: string;
  readonly name: string;
  readonly state: AgentCellState;
}

const AGENTS: readonly AgentDescriptor[] = [
  { id: "claude-code", label: "Claude Code" },
  { id: "codex", label: "Codex" },
  { id: "omp", label: "omp" },
  { id: "cursor", label: "Cursor" },
];

const ROW_COUNT = 5000;

const ROWS: readonly SkillRow[] = Array.from({ length: ROW_COUNT }, (_, index) => ({
  id: `skill-${index}`,
  name: `skill-${index}`,
  state: index % 3 === 0 ? { kind: "off" } : { kind: "linked" },
}));

const COLUMNS: ReadonlyArray<ColumnDef<DataTableFeatures, SkillRow>> = [
  { accessorKey: "name", header: "Name", size: 320, meta: { grow: true } },
  ...agentColumns<SkillRow>({
    agents: AGENTS,
    getState: (row) => row.state,
    onToggle: () => undefined,
  }),
];

describe("DataTable virtualization", () => {
  test("a 5,000-row model with four agent columns lays out only the viewport", () => {
    const { container } = render(
      <DataTable<SkillRow>
        ariaLabel="Skills"
        data={ROWS}
        columns={COLUMNS}
        getRowId={(row) => row.id}
        getRowLabel={(row) => row.name}
        viewportHeight={VIEWPORT}
        overscan={OVERSCAN}
      />,
    );

    const table = screen.getByRole("table", { name: "Skills" });
    const rendered = Number(table.getAttribute("data-rendered-rows"));
    const total = Number(table.getAttribute("data-total-rows"));

    expect(total).toBe(ROW_COUNT);
    expect(rendered).toBeGreaterThan(0);

    // Visible rows plus overscan on both edges; never a meaningful fraction.
    const visible = Math.ceil((VIEWPORT - TABLE_HEADER_HEIGHT) / TABLE_ROW_HEIGHT);
    expect(rendered).toBeLessThanOrEqual(visible + OVERSCAN * 2);
    expect(rendered).toBeLessThan(100);

    const domRows = container.querySelectorAll("[role='row'][data-row-index]");
    expect(domRows).toHaveLength(rendered);

    // 4 agent cells per rendered row; nothing is built for the other ~4,980.
    expect(container.querySelectorAll("[data-slot='agent-cell']")).toHaveLength(rendered * 4);
    expect(container.querySelectorAll("[data-slot='agent-cell']").length).toBeLessThan(200);
  });

  test("virtual rows use stable keys and contiguous absolute positions", () => {
    const { container } = render(
      <DataTable<SkillRow>
        ariaLabel="Skills"
        data={ROWS}
        columns={COLUMNS}
        getRowId={(row) => row.id}
        getRowLabel={(row) => row.name}
        viewportHeight={VIEWPORT}
        overscan={OVERSCAN}
      />,
    );

    const rows = [...container.querySelectorAll<HTMLElement>("[data-row-index]")];
    expect(new Set(rows.map((row) => row.dataset["rowId"])).size).toBe(rows.length);

    const transforms = rows.map((row) => row.style.transform);
    expect(transforms[0]).toBe(`translateY(${TABLE_ROW_HEIGHT * 0}px)`);
    expect(transforms[1]).toBe(`translateY(${TABLE_ROW_HEIGHT * 1}px)`);
    expect(new Set(transforms).size).toBe(transforms.length);
  });
});

/**
 * TF-035: the release build put 188 rows in the DOM for one 600 px viewport
 * because the scroll element measured ~12,000 px tall, and the virtualizer
 * sized its window from that measurement. These tests reproduce the bad
 * measurement and assert the window still depends on the viewport only.
 */

/** Mounted rows allowed for the 600 px viewport: visible + overscan + slack. */
const MOUNTED_ROW_LIMIT = 40;
/** What the flex parent made the scroll element measure in the release build. */
const TALL_CONTAINER = 12_000;

/** Pretends the scroll element measured `height` px; restores the shared stub. */
function stubMeasuredHeight(height: number): () => void {
  const spy = vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(height);
  return () => spy.mockRestore();
}

function tableElement(
  data: readonly SkillRow[],
  viewportHeight: number | undefined,
  loading = false,
) {
  return (
    <DataTable<SkillRow>
      ariaLabel="Skills"
      data={data}
      columns={COLUMNS}
      getRowId={(row) => row.id}
      getRowLabel={(row) => row.name}
      viewportHeight={viewportHeight}
      overscan={OVERSCAN}
      loading={loading}
    />
  );
}

describe("DataTable viewport independence", () => {
  test("a container measured 12,000 px tall still mounts one 600 px viewport", () => {
    const restore = stubMeasuredHeight(TALL_CONTAINER);
    try {
      const { container } = render(tableElement(ROWS.slice(0, 3000), VIEWPORT));
      const table = screen.getByRole("table", { name: "Skills" });
      const rendered = Number(table.getAttribute("data-rendered-rows"));

      expect(rendered).toBeGreaterThan(0);
      expect(rendered).toBeLessThanOrEqual(MOUNTED_ROW_LIMIT);
      const domRows = container.querySelectorAll("[role='row'][data-row-index]");
      expect(domRows).toHaveLength(rendered);
      expect(container.querySelectorAll("[data-slot='agent-cell']").length).toBe(rendered * 4);
    } finally {
      restore();
    }
  });

  test("an unconstrained parent mounts at most a screenful, not the spacer", () => {
    const restore = stubMeasuredHeight(TALL_CONTAINER);
    try {
      const { container } = render(tableElement(ROWS.slice(0, 3000), undefined));
      const rendered = Number(
        screen.getByRole("table", { name: "Skills" }).getAttribute("data-rendered-rows"),
      );

      // Without `viewportHeight` the window is capped by the window height
      // (jsdom reports 768 px), never by the 12,000 px measurement.
      expect(rendered).toBeGreaterThan(0);
      expect(rendered).toBeLessThanOrEqual(Math.ceil(window.innerHeight / TABLE_ROW_HEIGHT) + OVERSCAN * 2);
      expect(container.querySelectorAll("[role='row'][data-row-index]")).toHaveLength(rendered);
    } finally {
      restore();
    }
  });

  test("the mounted window and DOM size do not depend on the snapshot size", () => {
    const restore = stubMeasuredHeight(TALL_CONTAINER);
    try {
      const small = render(tableElement(ROWS.slice(0, 300), VIEWPORT));
      const smallRendered = Number(
        small.container.querySelector("[role='table']")?.getAttribute("data-rendered-rows"),
      );
      const smallNodes = small.container.querySelectorAll("*").length;
      cleanup();

      const large = render(tableElement(ROWS.slice(0, 3000), VIEWPORT));
      const largeRendered = Number(
        large.container.querySelector("[role='table']")?.getAttribute("data-rendered-rows"),
      );
      const largeNodes = large.container.querySelectorAll("*").length;

      expect(smallRendered).toBeGreaterThan(0);
      expect(largeRendered).toBe(smallRendered);
      expect(largeNodes - smallNodes).toBeLessThan(100);
    } finally {
      restore();
    }
  });

  test("keys follow row ids, so a reordered snapshot keeps its DOM nodes", () => {
    const restore = stubMeasuredHeight(TALL_CONTAINER);
    try {
      const data = ROWS.slice(0, 3000);
      const view = render(tableElement(data, VIEWPORT));
      const ids = [...view.container.querySelectorAll<HTMLElement>("[data-row-id]")].map(
        (row) => row.dataset["rowId"] ?? "",
      );
      expect(ids.length).toBeGreaterThan(0);
      expect(new Set(ids).size).toBe(ids.length);
      const nodes = new Map(
        ids.map((id) => [id, view.container.querySelector(`[data-row-id="${id}"]`)]),
      );

      const reordered = [...data];
      const first = reordered[0];
      const second = reordered[1];
      if (first === undefined || second === undefined) throw new Error("fixture is too small");
      reordered[0] = second;
      reordered[1] = first;
      view.rerender(tableElement(reordered, VIEWPORT));

      for (const id of ids) {
        expect(view.container.querySelector(`[data-row-id="${id}"]`), id).toBe(nodes.get(id));
      }
    } finally {
      restore();
    }
  });

  test("switching between the loading, empty and row branches keeps the hook order", () => {
    // The selection counters are hooks, so they must stay above the
    // loading/empty early return. When they sat below it, a reload that cleared
    // the rows crashed the whole app with React's hook-order error.
    const filler = ROWS.slice(0, 3000);
    const view = render(tableElement([], VIEWPORT, true));
    expect(view.container.querySelector("[role='table']")).toBeNull();

    view.rerender(tableElement(filler, VIEWPORT));
    expect(screen.getByRole("table", { name: "Skills" })).toBeTruthy();

    view.rerender(tableElement([], VIEWPORT));
    view.rerender(tableElement(filler, VIEWPORT, true));
    view.rerender(tableElement(filler, VIEWPORT));

    const rendered = Number(
      screen.getByRole("table", { name: "Skills" }).getAttribute("data-rendered-rows"),
    );
    expect(rendered).toBeGreaterThan(0);
    expect(rendered).toBeLessThanOrEqual(MOUNTED_ROW_LIMIT);
  });
});
