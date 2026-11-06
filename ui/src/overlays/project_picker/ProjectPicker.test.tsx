import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { axe } from "jest-axe";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ProjectPicker } from "./ProjectPicker";
import { filterProjects } from ".";
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
    await userEvent.type(screen.getByRole("combobox", { name: "Search projects" }), "Agen");
    const highlighted = screen.getByRole("option", { name: /AgenTuna/ }).querySelectorAll("mark");
    expect(highlighted).toHaveLength(2);
    expect(highlighted[0]?.textContent).toBe("Agen");
    expect(highlighted[1]?.textContent).toBe("Agen");
    expect(screen.queryByRole("option", { name: /Studio/ })).toBeNull();
  });
  it("selects the active row with Enter and closes", async () => {
    const props = renderPicker();
    const input = screen.getByRole("combobox", { name: "Search projects" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onSelect).toHaveBeenCalledWith(projects[1]);
    expect(props.onClose).toHaveBeenCalledOnce();
  });

  it("reveals on Cmd+Enter and clears a no-results query", async () => {
    const onReveal = vi.fn();
    renderPicker({ onReveal });
    const input = screen.getByRole("combobox", { name: "Search projects" });
    fireEvent.keyDown(input, { key: "Enter", metaKey: true });
    expect(onReveal).toHaveBeenCalledWith(projects[0]);
    await userEvent.type(input, "missing");
    await userEvent.click(screen.getByRole("button", { name: "Clear" }));
    await waitFor(() => expect(screen.getAllByRole("option")).toHaveLength(2));
  });
  it("has no axe accessibility violations in the populated state", async () => {
    const { container } = render(<ProjectPicker open projects={projects} onSelect={vi.fn()} onClose={vi.fn()} onAddFolder={vi.fn()} onScan={vi.fn()} />);
    expect((await axe(container)).violations).toEqual([]);
  });

  it("identifies the currently active project separately from keyboard selection", () => {
    renderPicker({ currentProjectId: "p2" });
    expect(screen.getByRole("option", { name: /Studio/ }).getAttribute("aria-current")).toBe("true");
    expect(screen.getByText("Current")).toBeTruthy();
    expect(screen.getByRole("option", { name: /AgenTuna/ }).getAttribute("aria-selected")).toBe("true");
  });

  it("highlights multiple path matches and leaves selection unchanged on Escape", async () => {
    const matching = { ...projects[0]!, id: "path-match", name: "Repo", path: "/repos/agentuna/agentuna" };
    const props = renderPicker({ projects: [matching] });
    const input = screen.getByRole("combobox", { name: "Search projects" });
    await userEvent.type(input, "agentuna");
    expect(screen.getByRole("option", { name: /Repo/ }).querySelectorAll("mark")).toHaveLength(2);
    fireEvent.keyDown(input, { key: "Escape" });
    expect(props.onSelect).not.toHaveBeenCalled();
    expect(props.onClose).toHaveBeenCalledOnce();
  });

  it("filters 2,000 projects within the picker budget", () => {
    const entries = Array.from({ length: 2_000 }, (_, index) => ({
      id: `project-${index}`,
      name: `Project ${index}`,
      path: `/workspace/project-${index}`,
      skillCount: 0,
      serverCount: 0,
      lastUsed: null,
    }));
    const started = performance.now();
    const result = filterProjects(entries, "project-1999");
    expect(performance.now() - started).toBeLessThan(16);
    expect(result.map((project) => project.id)).toEqual(["project-1999"]);
  });
  it("virtualizes a ten-thousand-project inventory and announces keyboard selection", () => {
    const many = Array.from({ length: 10_000 }, (_, index) => ({
      id: `project-${index}`,
      name: `Project ${index}`,
      path: `/sandbox/projects/project-${index}`,
      skillCount: index % 7,
      serverCount: index % 3,
      lastUsed: null,
    }));
    render(<ProjectPicker open projects={many} onSelect={vi.fn()} onClose={vi.fn()} onAddFolder={vi.fn()} onScan={vi.fn()} />);
    const input = screen.getByRole("combobox", { name: "Search projects" });
    expect(screen.getAllByRole("option").length).toBeLessThan(30);
    expect(input.getAttribute("aria-activedescendant")).toBe("project-option-project-0");
    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(input.getAttribute("aria-activedescendant")).toBe("project-option-project-1");
  });
});
