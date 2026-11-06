import { cleanup, render, screen } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, describe, expect, test } from "vitest";
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
