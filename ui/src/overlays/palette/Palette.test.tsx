import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { axe } from "jest-axe";
import { afterEach, describe, expect, test, vi } from "vitest";
import { Palette } from "./Palette";
import type { PaletteEntry } from "./index";

const entries: PaletteEntry[] = [
  { id: "action:skills", kind: "action", title: "Skills", subtitle: "Open skills", keywords: [] },
  { id: "action:servers", kind: "action", title: "MCP Servers", subtitle: "Open MCP servers", keywords: [] },
];

afterEach(cleanup);

function renderPalette() {
  const onClose = vi.fn();
  const onRun = vi.fn();
  const rendered = render(<Palette open index={entries} onClose={onClose} onRun={onRun} />);
  return { ...rendered, onClose, onRun };
}

describe("Palette keyboard and accessibility", () => {
  test("keeps keyboard focus in the combobox while announcing the active result", async () => {
    const user = userEvent.setup();
    const { onClose, onRun } = renderPalette();
    const input = screen.getByRole("combobox", { name: "Search commands" });
    expect(document.activeElement).toBe(input);
    await user.keyboard("{ArrowDown}");
    expect(input.getAttribute("aria-activedescendant")).toBe("palette-option-action%3Aservers");
    await user.keyboard("{Enter}");
    expect(onRun).toHaveBeenCalledWith(entries[1]);
    expect(onClose).toHaveBeenCalledOnce();
  });

  test("Escape closes without running a result", async () => {
    const user = userEvent.setup();
    const { onClose, onRun } = renderPalette();
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledOnce();
    expect(onRun).not.toHaveBeenCalled();
  });
  test("shows shortcut and disabled reason without running unavailable actions", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const onRun = vi.fn();
    const unavailable: PaletteEntry = { ...entries[0]!, id: "action:locked", disabledReason: "Choose a project first", shortcut: "⌘⇧S" };
    render(<Palette open index={[unavailable]} onClose={onClose} onRun={onRun} />);
    expect(screen.getByText("Choose a project first")).toBeTruthy();
    expect(screen.getByText("⌘⇧S")).toBeTruthy();
    await user.keyboard("{Enter}");
    expect(onRun).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });
  test("TU-015 AC2: bolds matched spans in search results", async () => {
    const user = userEvent.setup();
    renderPalette();
    const input = screen.getByRole("combobox", { name: "Search commands" });
    await user.type(input, "serv");
    // Look for strong tags inside the rendered results
    const strong = document.querySelectorAll("strong");
    expect(strong.length).toBeGreaterThan(0);
    const matchedTexts = Array.from(strong).map(el => el.textContent?.toLowerCase());
    expect(matchedTexts.some(t => t?.includes("serv"))).toBe(true);
  });

  test("TU-015 AC2: Home and End navigate to the start and end of results", async () => {
    const user = userEvent.setup();
    renderPalette();
    const input = screen.getByRole("combobox", { name: "Search commands" });
    await user.keyboard("{ArrowDown}");
    expect(input.getAttribute("aria-activedescendant")).toBe("palette-option-action%3Aservers");
    await user.keyboard("{Home}");
    expect(input.getAttribute("aria-activedescendant")).toBe("palette-option-action%3Askills");
    await user.keyboard("{End}");
    expect(input.getAttribute("aria-activedescendant")).toBe("palette-option-action%3Aservers");
  });

  test("TU-015 AC2: Cmd+Enter opens in the background without closing palette", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const onRun = vi.fn();
    render(<Palette open index={entries} onClose={onClose} onRun={onRun} />);
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    expect(onRun).toHaveBeenCalledWith(entries[0], { background: true });
    expect(onClose).not.toHaveBeenCalled();
  });

  test("TU-015 AC3: No matches shows 'No matches' and three nearest actions", async () => {
    const user = userEvent.setup();
    const threeActions: PaletteEntry[] = [
      { id: "action:1", kind: "action", title: "Action 1", subtitle: "First action", keywords: [] },
      { id: "action:2", kind: "action", title: "Action 2", subtitle: "Second action", keywords: [] },
      { id: "action:3", kind: "action", title: "Action 3", subtitle: "Third action", keywords: [] },
      { id: "action:4", kind: "action", title: "Action 4", subtitle: "Fourth action", keywords: [] },
    ];
    const onClose = vi.fn();
    const onRun = vi.fn();
    render(<Palette open index={threeActions} onClose={onClose} onRun={onRun} />);
    const input = screen.getByRole("combobox", { name: "Search commands" });
    await user.type(input, "zzzzzzzz");
    expect(screen.getByText("No matches")).toBeTruthy();
    expect(screen.getByText("Suggested actions")).toBeTruthy();
    expect(screen.getByText("Action 1")).toBeTruthy();
    expect(screen.getByText("Action 2")).toBeTruthy();
    expect(screen.getByText("Action 3")).toBeTruthy();
    expect(screen.queryByText("Action 4")).toBeNull(); // capped at 3

    // Pressing enter runs the suggested action
    await user.keyboard("{Enter}");
    expect(onRun).toHaveBeenCalledWith(threeActions[0]);
    expect(onClose).toHaveBeenCalledOnce();
  });

  test("TU-015 AC5: records action id on execution", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const onRun = vi.fn();
    const onRecordAction = vi.fn();
    render(<Palette open index={entries} onClose={onClose} onRun={onRun} onRecordAction={onRecordAction} />);
    await user.keyboard("{Enter}");
    expect(onRecordAction).toHaveBeenCalledWith(entries[0]);
    expect(onRecordAction.mock.calls[0]?.[0]?.id).toBe("action:skills");
  });

  test("palette has no axe accessibility violations", async () => {
    const { container } = renderPalette();
    expect((await axe(container)).violations).toEqual([]);
  });
});
