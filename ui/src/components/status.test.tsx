import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";
import { Badge } from "./badge";
import { StatusDot } from "./status-dot";
import { ITEM_STATUSES, STATUS_META, STATUS_TONE_CLASSES, type ItemStatus } from "./status";

afterEach(cleanup);

const EXPECTED_LABELS: Readonly<Record<ItemStatus, string>> = {
  linked: "Linked",
  off: "Off",
  native: "Native",
  conflict: "Conflict",
  drifted: "Drifted",
  locked: "Locked",
  shadowed: "Shadowed",
};

describe("item states", () => {
  test("every state has its own visible text label", () => {
    expect(ITEM_STATUSES).toHaveLength(7);
    const labels = ITEM_STATUSES.map((status) => STATUS_META[status].label);
    expect(labels).toEqual(Object.values(EXPECTED_LABELS));
    expect(new Set(labels).size).toBe(ITEM_STATUSES.length);
    expect(new Set(ITEM_STATUSES.map((status) => STATUS_META[status].tone)).size).toBe(ITEM_STATUSES.length);
  });

  test.each(ITEM_STATUSES)("%s badge carries a glyph and the label, never colour alone", (status) => {
    render(<Badge status={status} />);
    const badge = document.querySelector<HTMLElement>(`[data-slot="badge"][data-status="${status}"]`);

    expect(badge).not.toBeNull();
    expect(badge?.textContent).toBe(EXPECTED_LABELS[status]);
    expect(badge?.querySelector(`[data-glyph="${status}"]`)?.getAttribute("aria-hidden")).toBe("true");
    expect(badge?.className).toContain(STATUS_TONE_CLASSES[STATUS_META[status].tone].text);
  });

  test.each(ITEM_STATUSES)("%s status dot shows the label next to a decorative dot", (status) => {
    render(<StatusDot status={status} />);
    const dot = document.querySelector<HTMLElement>(`[data-slot="status-dot"][data-status="${status}"]`);

    expect(dot?.textContent).toBe(EXPECTED_LABELS[status]);
    expect(dot?.querySelector("[data-dot]")?.getAttribute("aria-hidden")).toBe("true");
    expect(dot?.getAttribute("title")).toBe(STATUS_META[status].description);
  });

  test("status label can be overridden with screen wording", () => {
    render(<StatusDot status="locked" label="Locked by Global" />);
    expect(screen.getByText("Locked by Global")).toBeTruthy();
  });

  test("a status badge accepts custom text but keeps the state glyph", () => {
    render(<Badge status="native">Not in AgenTuna</Badge>);
    const badge = document.querySelector<HTMLElement>("[data-slot='badge']");
    expect(badge?.textContent).toBe("Not in AgenTuna");
    expect(badge?.querySelector("[data-glyph='native']")).not.toBeNull();
  });
});

describe("Badge", () => {
  test("generic badges use tone classes and an optional dot", () => {
    render(
      <Badge tone="warning" dot>
        Drift detected
      </Badge>,
    );
    const badge = screen.getByText("Drift detected").closest("[data-slot='badge']");
    expect(badge?.className).toContain("text-warning");
    expect(badge?.querySelector("[aria-hidden='true']")).not.toBeNull();
    expect(badge?.getAttribute("data-status")).toBeNull();
  });

  test("mono badges render identifiers in the mono face", () => {
    render(<Badge mono>native</Badge>);
    expect(screen.getByText("native").className).toContain("font-mono");
  });
});
