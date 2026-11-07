import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { AgentCardView } from "../../generated/AgentCardView";
import { CustomAgentDialog } from "./CustomAgentDialog";

const ipc = vi.hoisted(() => ({
  calls: [] as { command: string; args: unknown }[],
  reply: (_command: string, _args?: unknown): Promise<unknown> => Promise.resolve(undefined),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args?: unknown) => {
    ipc.calls.push({ command, args });
    return ipc.reply(command, args);
  },
}));

afterEach(() => {
  cleanup();
  ipc.calls.length = 0;
});

const card: AgentCardView = {
  id: "custom:wind-surf",
  name: "WindSurf",
  installed: true,
  version: null,
  managed: true,
  paths: ["/sandbox/windsurf/skills"],
  health: "",
  link_mode: "link",
  presence: "installed",
  format: "MCP not supported",
  managed_skills: 0,
  managed_servers: 0,
  native_skills: 0,
  native_servers: 0,
  copy_fallback: false,
  primary_path: "/sandbox/windsurf/skills",
  needs_path: false,
  in_sync: 0,
  total: 0,
  linked_deployments: 0,
};

function preview(overrides: Record<string, unknown> = {}) {
  return {
    skills_path: "/sandbox/windsurf/skills",
    mcp_path: null,
    rule_paths: ["/sandbox/windsurf/AGENTS.md"],
    valid: true,
    reason: null,
    existing_agent_id: null,
    ...overrides,
  };
}

describe("CustomAgentDialog (TA-060)", () => {
  test("previews resolved paths and creates a no-MCP custom agent", async () => {
    const onCreated = vi.fn();
    const onClose = vi.fn();
    ipc.reply = (command) => {
      if (command === "custom_agent_preview") return Promise.resolve(preview());
      if (command === "custom_agent_create") return Promise.resolve(card);
      return Promise.resolve(undefined);
    };
    render(<CustomAgentDialog open onClose={onClose} onCreated={onCreated} onOpenExisting={vi.fn()} />);

    fireEvent.change(screen.getByRole("textbox", { name: "Custom agent name" }), { target: { value: "WindSurf" } });
    fireEvent.change(screen.getByRole("textbox", { name: "Custom agent root" }), { target: { value: "/sandbox/windsurf" } });
    fireEvent.change(screen.getByRole("combobox", { name: "MCP format" }), { target: { value: "none" } });
    expect(screen.queryByRole("textbox", { name: "MCP configuration file" })).toBeNull();
    expect(await screen.findByText("MCP not supported")).toBeTruthy();
    expect(await screen.findByText("/sandbox/windsurf/AGENTS.md")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Add agent" }));
    await waitFor(() => expect(onCreated).toHaveBeenCalledWith(card));
    expect(onClose).toHaveBeenCalled();
    expect(ipc.calls.find((call) => call.command === "custom_agent_create")?.args).toMatchObject({
      request: { display_name: "WindSurf", root: "/sandbox/windsurf", mcp_format: "none" },
    });
  });

  test("shows the exact duplicate-name reason and opens the existing agent card", async () => {
    const onOpenExisting = vi.fn();
    ipc.reply = (command) => command === "custom_agent_preview"
      ? Promise.resolve(preview({ valid: false, reason: "An agent with this name exists", existing_agent_id: "custom:wind-surf" }))
      : Promise.resolve(undefined);
    render(<CustomAgentDialog open onClose={vi.fn()} onCreated={vi.fn()} onOpenExisting={onOpenExisting} />);

    fireEvent.change(screen.getByRole("textbox", { name: "Custom agent name" }), { target: { value: "WindSurf" } });
    fireEvent.change(screen.getByRole("textbox", { name: "Custom agent root" }), { target: { value: "/sandbox/windsurf" } });
    expect(await screen.findByText("An agent with this name exists")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Add agent" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Open existing agent" }));
    expect(onOpenExisting).toHaveBeenCalledWith("custom:wind-surf");
  });
  test("shows invalid relative roots inline and blocks saving until corrected", async () => {
    ipc.reply = (command, args) => {
      if (command === "custom_agent_preview") {
        const request = (args as { request: { root: string } }).request;
        return Promise.resolve(preview({ valid: false, reason: request.root ? "Root folder must be an absolute path" : "Enter an absolute root folder" }));
      }
      return Promise.resolve(undefined);
    };
    render(<CustomAgentDialog open onClose={vi.fn()} onCreated={vi.fn()} onOpenExisting={vi.fn()} />);
    fireEvent.change(screen.getByRole("textbox", { name: "Custom agent name" }), { target: { value: "WindSurf" } });
    fireEvent.change(screen.getByRole("textbox", { name: "Custom agent root" }), { target: { value: "relative/windsurf" } });
    expect((await screen.findByRole("alert")).textContent).toContain("Root folder must be an absolute path");
    expect((screen.getByRole("button", { name: "Add agent" }) as HTMLButtonElement).disabled).toBe(true);
    expect(ipc.calls.some(({ command }) => command === "custom_agent_create")).toBe(false);
  });
});
