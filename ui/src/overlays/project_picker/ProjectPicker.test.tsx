import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ProjectPicker } from "./ProjectPicker";
import type { ProjectPickerProject } from "./ProjectPicker";

afterEach(cleanup);
const projects: ProjectPickerProject[] = [
  { id: "p1", name: "AgenTuna", path: "/work/AgenTuna", skillCount: 3, serverCount: 2, lastUsed: "2 hours ago" },
  { id: "p2", name: "Studio", path: "/work/Studio", skillCount: 1, serverCount: 0, lastUsed: null },
];

function renderPicker(overrides: Partial<React.ComponentProps<typeof ProjectPicker>> = {}) {
  const props = { open: true, projects, onSelect: vi.fn(), onClose: vi.fn(), onAddFolder: vi.fn(), onScan: vi.fn(), ...overrides };
  render(<ProjectPicker {...props} />);
  return props;
}

describe("ProjectPicker", () => {
  it("filters and highlights matches in both names and paths", async () => {
    renderPicker();
    await userEvent.type(screen.getByRole("textbox", { name: "Search projects" }), "Agen");
    const highlighted = screen.getByRole("option", { name: /AgenTuna/ }).querySelectorAll("mark");
    expect(highlighted).toHaveLength(2);
    expect(highlighted[0]?.textContent).toBe("Agen");
    expect(highlighted[1]?.textContent).toBe("Agen");
    expect(screen.queryByRole("option", { name: /Studio/ })).toBeNull();
  });
  it("selects the active row with Enter and closes", async () => {
    const props = renderPicker();
    const input = screen.getByRole("textbox", { name: "Search projects" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onSelect).toHaveBeenCalledWith(projects[1]);
    expect(props.onClose).toHaveBeenCalledOnce();
  });

  it("reveals on Cmd+Enter and clears a no-results query", async () => {
    const onReveal = vi.fn();
    renderPicker({ onReveal });
    const input = screen.getByRole("textbox", { name: "Search projects" });
    fireEvent.keyDown(input, { key: "Enter", metaKey: true });
    expect(onReveal).toHaveBeenCalledWith(projects[0]);
    await userEvent.type(input, "missing");
    await userEvent.click(screen.getByRole("button", { name: "Clear" }));
    await waitFor(() => expect(screen.getAllByRole("option")).toHaveLength(2));
  });
});
