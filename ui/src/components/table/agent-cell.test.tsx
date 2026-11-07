import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { AgentCell } from "./agent-cell";
import type { AgentCellState, AgentDescriptor } from "./types";

afterEach(cleanup);

const CLAUDE: AgentDescriptor = { id: "claude-code", label: "Claude Code" };
const TOGGLE_AGENTS = ["codex", "omp"] as const;

const STATES: ReadonlyArray<[AgentCellState["kind"], AgentCellState]> = [
  ["linked", { kind: "linked" }],
  ["off", { kind: "off" }],
  ["native", { kind: "native", path: ".claude/skills/research", readOnly: true }],
  ["none", { kind: "none" }],
  ["locked", { kind: "locked", lockedBy: "Global" }],
  ["shadowed", { kind: "shadowed", shadowedBy: "Project" }],
];

function renderCell(state: AgentCellState, onToggle?: (next: boolean) => void) {
  return render(<AgentCell agent={CLAUDE} state={state} onToggle={onToggle} />);
}

/**
 * A stable projection of what a screen reader can perceive: role, name,
 * state and tooltip. Snapshotting this instead of raw HTML keeps the golden
 * free of generated `useId` values while still covering all six states.
 */
function accessibilityProjection(cell: Element): string {
  const lines: string[] = [];
  const group = cell.closest("[role='group']");
  lines.push(`group: ${group?.getAttribute("aria-label") ?? "(none)"}`);
  lines.push(`tooltip: ${cell.getAttribute("title") ?? "(none)"}`);

  const toggle = cell.querySelector("[role='switch']");
  if (toggle !== null) {
    lines.push(
      `switch: name=${toggle.getAttribute("aria-labelledby") === null ? "(none)" : "labelled"} checked=${toggle.getAttribute("aria-checked")} disabled=${toggle.hasAttribute("disabled")}`,
    );
  }
  for (const badge of cell.querySelectorAll("[data-slot='badge']")) {
    lines.push(`badge: ${badge.textContent ?? ""}`);
  }
  const glyph = cell.querySelector("[data-glyph]");
  if (glyph !== null) {
    lines.push(`glyph: ${glyph.getAttribute("data-glyph") ?? ""}`);
  }
  const dot = cell.querySelector("[data-slot='status-dot']");
  if (dot !== null) {
    lines.push(`status: ${dot.textContent ?? ""}`);
  }
  const label = cell.querySelector("[data-slot='agent-label']");
  if (label !== null) {
    lines.push(`label: ${label.textContent ?? ""}`);
  }
  const path = cell.querySelector("[data-slot='agent-path']");
  if (path !== null) {
    lines.push(`path: ${path.textContent ?? ""}`);
  }
  if (cell.textContent?.includes("—") === true) {
    lines.push("dash: present");
  }
  return lines.join("\n");
}

describe("AgentCell", () => {
  test("renders all six states with the widget each state needs", () => {
    const { container } = render(
      <>
        {STATES.map(([kind, state]) => (
          <div key={kind} data-fixture={kind}>
            <AgentCell agent={CLAUDE} state={state} onToggle={() => undefined} />
          </div>
        ))}
      </>,
    );

    const fixtures = new Map(
      [...container.querySelectorAll("[data-fixture]")].map((element) => [
        element.getAttribute("data-fixture"),
        element,
      ]),
    );

    const linked = fixtures.get("linked");
    expect(linked?.querySelector("[role='switch']")?.getAttribute("aria-checked")).toBe("true");
    expect(linked?.querySelector("[role='switch']")?.hasAttribute("disabled")).toBe(false);
    expect(linked?.textContent).toContain("Linked");

    const off = fixtures.get("off");
    expect(off?.querySelector("[role='switch']")?.getAttribute("aria-checked")).toBe("false");
    expect(off?.querySelector("[role='switch']")?.hasAttribute("disabled")).toBe(false);
    expect(off?.textContent).toContain("Off");

    const native = fixtures.get("native");
    expect(native?.querySelector("[data-slot='badge']")?.textContent).toBe("native");
    expect(native?.textContent).toContain(".claude/skills/research");
    expect(native?.firstElementChild?.getAttribute("title")).toBe(
      ".claude/skills/research · read-only",
    );

    const none = fixtures.get("none");
    expect(none?.textContent).toContain("—");
    expect(none?.textContent).toContain("Not present");

    const locked = fixtures.get("locked")?.querySelector("[data-slot='agent-cell']");
    const lockedSwitch = locked?.querySelector("[role='switch']");
    expect(lockedSwitch?.getAttribute("aria-checked")).toBe("true");
    expect(lockedSwitch?.hasAttribute("disabled")).toBe(true);
    expect(locked?.querySelector("[data-glyph='locked']")).not.toBeNull();
    expect(locked?.getAttribute("title")).toBe("Locked by Global");

    const shadowed = fixtures.get("shadowed")?.querySelector("[data-slot='agent-cell']");
    expect(shadowed?.querySelector("[data-slot='status-dot']")?.textContent).toBe("Shadowed");
    expect(shadowed?.getAttribute("title")).toBe("Shadowed by Project");
  });

  test("the six states match a checked-in golden of their accessible projection", () => {
    const { container } = render(
      <>
        {STATES.map(([kind, state]) => (
          <div key={kind} data-fixture={kind}>
            <AgentCell agent={CLAUDE} state={state} onToggle={() => undefined} />
          </div>
        ))}
      </>,
    );

    const projection = [...container.querySelectorAll("[data-fixture]")].map((element) => {
      const cell = element.querySelector("[data-slot='agent-cell']");
      return `${element.getAttribute("data-fixture")}\n${cell === null ? "(missing)" : accessibilityProjection(cell)}`;
    });

    expect(projection).toEqual([
      "linked\ngroup: Claude Code\ntooltip: Deployed to this agent by AgenTuna and unchanged since the last write.\nswitch: name=labelled checked=true disabled=false\nlabel: Linked",
      "off\ngroup: Claude Code\ntooltip: Not deployed to this agent.\nswitch: name=labelled checked=false disabled=false\nlabel: Off",
      "native\ngroup: Claude Code\ntooltip: .claude/skills/research · read-only\nbadge: native\npath: .claude/skills/research",
      "none\ngroup: Claude Code\ntooltip: Not present in this agent\ndash: present",
      "locked\ngroup: Claude Code\ntooltip: Locked by Global\nswitch: name=labelled checked=true disabled=true\nglyph: locked\nlabel: Locked",
      "shadowed\ngroup: Claude Code\ntooltip: Shadowed by Project\nstatus: Shadowed",
    ]);
  });

  test("a native cell names every agent that shares its folder", () => {
    renderCell({
      kind: "native",
      path: ".agents/skills/release-notes",
      sharedWith: [...TOGGLE_AGENTS],
    });

    const cell = screen.getByRole("group", { name: "Claude Code" });
    expect(cell.getAttribute("title")).toBe(".agents/skills/release-notes · shared by codex + omp");
    expect(within(cell).getByText("shared with codex + omp")).toBeTruthy();
  });

  test("clicking the switch reports the next state without flipping itself", async () => {
    const user = userEvent.setup();
    const onToggle = vi.fn();
    renderCell({ kind: "off" }, onToggle);

    const toggle = screen.getByRole("switch", { name: "Claude Code: Off" });
    await user.click(toggle);

    expect(onToggle).toHaveBeenCalledWith(true);
    expect(toggle.getAttribute("aria-checked")).toBe("false");
  });

  test("states without a handler or with pending/read-only render a disabled switch", () => {
    const { rerender } = renderCell({ kind: "linked" });
    expect(screen.getByRole("switch").hasAttribute("disabled")).toBe(true);

    rerender(<AgentCell agent={CLAUDE} state={{ kind: "linked", pending: true }} onToggle={() => undefined} />);
    expect(screen.getByRole("switch").hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("group").getAttribute("aria-busy")).toBe("true");

    rerender(<AgentCell agent={CLAUDE} state={{ kind: "off", readOnly: true }} onToggle={() => undefined} />);
    expect(screen.getByRole("switch").hasAttribute("disabled")).toBe(true);
  });

  test("locked switches stay out of the tab order and never report a change", async () => {
    const user = userEvent.setup();
    const onToggle = vi.fn();
    renderCell({ kind: "locked", lockedBy: "Global" }, onToggle);

    const toggle = screen.getByRole("switch", { name: "Claude Code: Locked" }) as HTMLButtonElement;
    await user.click(toggle);

    expect(toggle.disabled).toBe(true);
    expect(onToggle).not.toHaveBeenCalled();
  });

  test("TA-020 AC5: Unsupported kind surfaces as a disabled toggle with reason, not a panic", async () => {
    const user = userEvent.setup();
    const onToggle = vi.fn();
    renderCell(
      {
        kind: "off",
        disabledReason: "agent claude-code does not support MCP configuration",
      },
      onToggle,
    );

    const toggle = screen.getByRole("switch", { name: "Claude Code: Off" }) as HTMLButtonElement;
    expect(toggle.disabled).toBe(true);
    expect(toggle.getAttribute("aria-checked")).toBe("false");

    const group = screen.getByRole("group", { name: "Claude Code" });
    expect(group.getAttribute("title")).toBe("agent claude-code does not support MCP configuration");

    await user.click(toggle);
    expect(onToggle).not.toHaveBeenCalled();
  });
});
