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

  test("palette has no axe accessibility violations", async () => {
    const { container } = renderPalette();
    expect((await axe(container)).violations).toEqual([]);
  });
});
