import { describe, expect, test } from "vitest";
import { shellAgentStatus } from "./agents";
import type { AgentCardView } from "../generated/AgentCardView";

const card = (overrides: Partial<AgentCardView>): AgentCardView => ({
  id: "claude_code",
  name: "Claude Code",
  installed: true,
  version: null,
  managed: true,
  paths: [],
  health: "",
  link_mode: "link",
  presence: "installed",
  format: "JSON",
  managed_skills: 0,
  managed_servers: 0,
  native_skills: 0,
  native_servers: 0,
  copy_fallback: false,
  primary_path: null,
  needs_path: false,
  in_sync: 0,
  total: 0,
  linked_deployments: 0,
  ...overrides,
});

describe("shellAgentStatus", () => {
  test("keeps the backend order and maps presence to detected / not found", () => {
    const rows = shellAgentStatus([
      card({ id: "claude_code", name: "Claude Code", presence: "installed" }),
      card({ id: "codex", name: "Codex", presence: "configured_only" }),
      card({ id: "cursor", name: "Cursor", presence: "not_installed" }),
    ]);
    expect(rows.map((row) => row.id)).toEqual(["claude_code", "codex", "cursor"]);
    expect(rows.map((row) => row.detected)).toEqual([true, true, false]);
    expect(rows[0]?.label).toBe("Claude Code");
  });

  test("falls back to the id when detection has no display name", () => {
    expect(shellAgentStatus([card({ id: "custom", name: "" })])[0]?.label).toBe("custom");
  });
});
