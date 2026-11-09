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
    rows: [
      {
        item_id: "",
        name: "release-notes",
        scope: "project",
        state: "native",
        shadowed_by: null,
        description: null,
        native: true,
        native_path: sharedPath,
        native_link_target: null,
        native_state: null,
        native_problem: null,
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
      },
      {
        item_id: "",
        name: "release-notes",
        scope: "user",
        state: "native",
        shadowed_by: null,
        description: null,
        native: true,
        native_path: "/sandbox/home/.claude/skills/release-notes",
        native_link_target: null,
        native_state: null,
        native_problem: null,
        update_available: false,
        source: null,
        source_commit_sha: null,
        source_remote_sha: null,
        source_last_checked_at: null,
        updated_at: "2026-11-07T10:00:00Z",
        agents: [
          {
            agent: "claude_code",
            state: "native",
            enabled: true,
            config_path: "/sandbox/home/.claude/skills/release-notes",
          },
        ],
        locked: false,
        lock_scope: null,
        lock_reason: null,
      },
    ],
    agents: [
      { id: "claude_code", label: "Claude Code", presence: "installed" },
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
  expect(screen.getByText(/exists 2 times: an own copy for Claude Code, one shared folder that Codex and omp both read/)).toBeTruthy();
  expect(screen.getByRole("heading", { name: "Claude Code" })).toBeTruthy();
  expect(screen.getAllByText("Own copy · native, not managed")).toHaveLength(1);
  expect(screen.getByRole("heading", { name: "Codex + omp" })).toBeTruthy();
  expect(screen.getByText("One shared folder · read-only until adopted")).toBeTruthy();
  expect(screen.getByRole("region", { name: "Ownership" })).toBeTruthy();
});
