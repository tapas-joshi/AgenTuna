import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import { NativeSkillDetail } from "./NativeSkillDetail";

const ipc = vi.hoisted(() => ({ reply: (_command: string): Promise<unknown> => Promise.resolve(undefined) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string) => ipc.reply(command) }));

afterEach(() => cleanup());

test("renders shared-root readers, folder ownership and coupling from the skills list projection", async () => {
  const sharedPath = "/sandbox/project/.agents/skills/release-notes";
  ipc.reply = () => Promise.resolve({
    rows: [{
      item_id: "",
      name: "release-notes",
      scope: "project",
      state: "native",
      shadowed_by: null,
      description: null,
      native: true,
      native_path: sharedPath,
      update_available: false,
      source: null,
      source_commit_sha: null,
      source_remote_sha: null,
      source_last_checked_at: null,
      updated_at: "2026-11-07T10:00:00Z",
      agents: [
        { agent: "codex", state: "native", enabled: true, config_path: sharedPath },
        { agent: "omp", state: "native", enabled: true, config_path: sharedPath },
      ],
      locked: false,
      lock_scope: null,
      lock_reason: null,
    }],
    agents: [
      { id: "codex", label: "Codex", presence: "installed" },
      { id: "omp", label: "omp", presence: "installed" },
    ],
    project_id: "project-fixture",
    project_name: "Fixture",
  });
  const state = createAppState();
  state.restore("project-fixture", "project");

  render(<AppStateProvider state={state}><NativeSkillDetail path={sharedPath} onClose={() => undefined} /></AppStateProvider>);

  expect(await screen.findByRole("heading", { name: "One folder, two agents" })).toBeTruthy();
  expect(screen.getByRole("note", { name: "Shared discovery root" })).toBeTruthy();
  expect(screen.getByText(/lives in one shared folder read by Codex and omp/)).toBeTruthy();
  expect(screen.getByRole("heading", { name: "Codex + omp" })).toBeTruthy();
  expect(screen.getByText("One shared folder · read-only until adopted")).toBeTruthy();
  expect(screen.getByRole("region", { name: "Ownership" })).toBeTruthy();
});
