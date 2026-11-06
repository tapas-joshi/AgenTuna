import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { AgentCardView } from "../../generated/AgentCardView";
import type { AgentPathCandidateView } from "../../generated/AgentPathCandidateView";
import { ToastProvider } from "../toasts";
import { ConnectAgentDialog } from "./ConnectAgentDialog";

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

const cursorAgent: AgentCardView = {
  id: "cursor",
  name: "Cursor",
  installed: false,
  version: null,
  managed: false,
  paths: [],
  health: "!<path> · 0 / 0 managed · 0 in sync",
  link_mode: "link",
  presence: "not_installed",
  format: "—",
  managed_skills: 0,
  managed_servers: 0,
  native_skills: 0,
  native_servers: 0,
  copy_fallback: false,
  primary_path: null,
  needs_path: true,
  in_sync: 0,
  total: 0,
  linked_deployments: 0,
};

const cursorCandidates: AgentPathCandidateView[] = [
  {
    path: "/Users/test/.cursor/skills",
    scope: "user",
    kind: "skills_dir",
    format: null,
    is_primary: true,
    exists: false,
    verified: false,
    is_override: false,
  },
  {
    path: "/Users/test/.cursor/mcp.json",
    scope: "user",
    kind: "mcp_file",
    format: "json",
    is_primary: false,
    exists: false,
    verified: false,
    is_override: false,
  },
];

function renderDialog(props: Partial<Parameters<typeof ConnectAgentDialog>[0]> = {}) {
  const onClose = props.onClose ?? vi.fn();
  const onSaved = props.onSaved ?? vi.fn();
  const rendered = render(
    <ToastProvider>
      <ConnectAgentDialog
        agent={props.agent !== undefined ? props.agent : cursorAgent}
        onClose={onClose}
        onSaved={onSaved}
      />
    </ToastProvider>,
  );
  return { ...rendered, onClose, onSaved };
}

describe("ConnectAgentDialog (TA-059)", () => {
  test("opening lists candidate paths with exists=false markers and focuses input", async () => {
    ipc.reply = (command) => {
      if (command === "agent_path_candidates") return Promise.resolve(cursorCandidates);
      if (command === "agent_path_validate") {
        return Promise.resolve({
          exists: false,
          is_directory: false,
          structure_warning: null,
          reason: "Folder does not exist",
        });
      }
      return Promise.resolve(undefined);
    };

    renderDialog();

    expect(await screen.findByText("/Users/test/.cursor/skills")).toBeTruthy();
    expect(screen.getByText("/Users/test/.cursor/mcp.json")).toBeTruthy();
    const notFoundMarkers = screen.getAllByText(/not found/i);
    expect(notFoundMarkers.length).toBeGreaterThanOrEqual(2);

    const input = screen.getByRole("textbox", { name: "Agent configuration path" });
    expect(document.activeElement).toBe(input);
  });

  test("typing a path that exists without expected structure shows soft warning", async () => {
    ipc.reply = (command) => {
      if (command === "agent_path_candidates") return Promise.resolve([]);
      if (command === "agent_path_validate") {
        return Promise.resolve({
          exists: true,
          is_directory: true,
          structure_warning: "No skills folder or config file found here",
          reason: null,
        });
      }
      return Promise.resolve(undefined);
    };

    renderDialog();

    const input = await screen.findByRole("textbox", { name: "Agent configuration path" });
    fireEvent.change(input, { target: { value: "/Users/test/empty-folder" } });

    expect(await screen.findByText("No skills folder or config file found here")).toBeTruthy();

    const saveButton = screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;
    expect(saveButton.disabled).toBe(false);
  });

  test("Browse fills input; invalid paths keep Save disabled with a reason", async () => {
    ipc.reply = (command, args) => {
      if (command === "agent_path_candidates") return Promise.resolve([]);
      if (command === "agent_path_choose") {
        return Promise.resolve("/Users/test/invalid-file.txt");
      }
      if (command === "agent_path_validate") {
        const path =
          typeof args === "object" && args !== null && "path" in args
            ? String(args.path)
            : undefined;
        if (path === "/Users/test/invalid-file.txt") {
          return Promise.resolve({
            exists: true,
            is_directory: false,
            structure_warning: null,
            reason: "Must be a directory, not a file",
          });
        }
        return Promise.resolve({
          exists: false,
          is_directory: false,
          structure_warning: null,
          reason: "Folder does not exist",
        });
      }
      return Promise.resolve(undefined);
    };

    renderDialog();

    const browseButton = await screen.findByRole("button", { name: "Choose folder…" });
    fireEvent.click(browseButton);

    expect(await screen.findByText("Must be a directory, not a file")).toBeTruthy();
    const input = screen.getByRole("textbox", { name: "Agent configuration path" }) as HTMLInputElement;
    expect(input.value).toBe("/Users/test/invalid-file.txt");

    const saveButton = screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;
    expect(saveButton.disabled).toBe(true);
  });

  test("Save persists override, rescans, closes dialog, toasts and calls onSaved", async () => {
    const onSaved = vi.fn();
    const onClose = vi.fn();
    const updatedAgent: AgentCardView = {
      ...cursorAgent,
      installed: true,
      presence: "installed",
      primary_path: "/Users/test/.cursor",
    };

    ipc.reply = (command) => {
      if (command === "agent_path_candidates") return Promise.resolve([]);
      if (command === "agent_path_validate") {
        return Promise.resolve({
          exists: true,
          is_directory: true,
          structure_warning: null,
          reason: null,
        });
      }
      if (command === "agent_path_set") return Promise.resolve(updatedAgent);
      if (command === "agents_rescan") return Promise.resolve([updatedAgent]);
      return Promise.resolve(undefined);
    };

    renderDialog({ onSaved, onClose });

    const input = await screen.findByRole("textbox", { name: "Agent configuration path" });
    fireEvent.change(input, { target: { value: "/Users/test/.cursor" } });

    const saveButton = await screen.findByRole("button", { name: "Save" });
    await waitFor(() => expect((saveButton as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(saveButton);

    await waitFor(() => expect(onSaved).toHaveBeenCalledWith(updatedAgent));
    expect(onClose).toHaveBeenCalled();
    expect(ipc.calls.some((c) => c.command === "agent_path_set")).toBe(true);
    expect(ipc.calls.some((c) => c.command === "agents_rescan")).toBe(true);
  });

  test("Cancel writes nothing and calls onClose", async () => {
    const onClose = vi.fn();
    ipc.reply = (command) => {
      if (command === "agent_path_candidates") return Promise.resolve([]);
      if (command === "agent_path_validate") {
        return Promise.resolve({
          exists: false,
          is_directory: false,
          structure_warning: null,
          reason: "Folder does not exist",
        });
      }
      return Promise.resolve(undefined);
    };

    renderDialog({ onClose });

    const cancelButton = await screen.findByRole("button", { name: "Cancel" });
    fireEvent.click(cancelButton);

    expect(onClose).toHaveBeenCalled();
    expect(ipc.calls.some((c) => c.command === "agent_path_set")).toBe(false);
  });
});
