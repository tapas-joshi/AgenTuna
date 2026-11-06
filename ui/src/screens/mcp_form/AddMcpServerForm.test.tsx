import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { McpChangeSetPreviewView } from "../../generated/McpChangeSetPreviewView";
import { AddMcpServerForm } from "./AddMcpServerForm";

afterEach(cleanup);

const preview: McpChangeSetPreviewView = {
  change_set_id: "plan-1",
  preview_id: "preview-1",
  files: [{ agent: "Codex", scope: "user", path: "/sandbox/.codex/config.toml", additions: 2n, deletions: 0n, diff: "+++ config.toml\n+server", selected: true, cannot_write_reason: null }],
};

describe("AddMcpServerForm", () => {
  it("validates required fields without requesting a plan", async () => {
    const onPlan = vi.fn();
    render(<AddMcpServerForm onCancel={() => undefined} onPlan={onPlan} onSave={() => Promise.resolve()} />);
    fireEvent.click(screen.getByRole("button", { name: "Review changes" }));
    expect(await screen.findByText("Enter a server name.")).toBeTruthy();
    expect(await screen.findByText("Enter a command.")).toBeTruthy();
    expect(await screen.findByText("Select at least one installed agent.")).toBeTruthy();
    expect(onPlan).not.toHaveBeenCalled();
  });

  it("preserves stdio values across transports and saves only after reviewing the planner preview", async () => {
    const onPlan = vi.fn().mockResolvedValue(preview);
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<AddMcpServerForm defaultAgents={["codex"]} onCancel={() => undefined} onPlan={onPlan} onSave={onSave} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Search" } });
    fireEvent.change(screen.getByLabelText("Command"), { target: { value: "npx" } });
    fireEvent.click(screen.getByRole("button", { name: "HTTP" }));
    fireEvent.change(screen.getByLabelText("URL"), { target: { value: "https://example.invalid/mcp" } });
    fireEvent.click(screen.getByRole("button", { name: "Command (stdio)" }));
    expect((screen.getByLabelText("Command") as HTMLInputElement).value).toBe("npx");
    fireEvent.click(screen.getByRole("button", { name: "Add variable" }));
    fireEvent.change(screen.getByLabelText("Variable 1 name"), { target: { value: "API_KEY" } });
    fireEvent.change(screen.getByLabelText("Variable 1 value type"), { target: { value: "literal" } });
    fireEvent.change(screen.getByLabelText("Variable 1 literal value"), { target: { value: "fixture-value" } });
    fireEvent.click(screen.getByRole("button", { name: "Review changes" }));
    await waitFor(() => expect(onPlan).toHaveBeenCalledTimes(1));
    expect(onSave).not.toHaveBeenCalled();
    expect(screen.getByText("Files that will change")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith("preview-1", ["Codex"]));
    expect(onPlan.mock.calls[0]?.[0]).toMatchObject({ name: "Search", command: "npx", url: "https://example.invalid/mcp" });
    expect(await screen.findByText("MCP server saved.")).toBeTruthy();
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("");
  });

  it("shows duplicate ownership, scope permissions and not-installed agents", () => {
    render(<AddMcpServerForm installedAgents={["codex"]} globalAllowed={false} duplicateScope="project" onCancel={() => undefined} onPlan={() => Promise.resolve(preview)} onSave={() => Promise.resolve()} />);
    expect(screen.getByText(/already exists in project scope/)).toBeTruthy();
    expect((screen.getByRole("button", { name: "Review changes" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Claude Code" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Codex" }) as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByLabelText("Global") as HTMLInputElement).disabled).toBe(true);
    expect(screen.getAllByText("not installed").length).toBe(3);
    expect(screen.getByText("Local (only you, this repo)")).toBeTruthy();
  });
});
