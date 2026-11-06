import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import { SkillDetailScreen } from "./SkillDetailScreen";

const ipc = vi.hoisted(() => ({ calls: [] as { command: string; args: unknown }[], reply: (_command: string): Promise<unknown> => Promise.resolve(undefined) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string, args?: unknown) => { ipc.calls.push({ command, args }); return ipc.reply(command); } }));
const appState = createAppState();
appState.openItem("itm_fake", { kind: "skill", scope: "user", project_id: null });
const detail = {
  item_id: "itm_fake", name: "Review checklist", slug: "review-checklist", scope: "user",
  path: "/sandbox/.agentuna/skills/review-checklist", markdown: "# Review checklist\n\nUse this checklist.",
  description: "Use this checklist", hash: "abc123", updated_at: "2026-11-06T10:00:00Z",
  source: null, mode: "copy", shadowed_by: "project", shadow_chain: ["project", "user"],
  deployments: [{ agent_id: "codex", agent_name: "Codex", state: "linked", mode: "symlink", target_path: ".agents/skills/review-checklist" }],
  history: [{ content_hash: "abc123", captured_at: "2026-11-06T10:00:00Z", operation_kind: "skill_edit", operation_summary: "Edited skill" }], missing: false,
};

afterEach(() => { cleanup(); ipc.calls.length = 0; });

describe("SkillDetailScreen", () => {
  test("renders markdown, deployment and newest history from the typed query", async () => {
    ipc.reply = () => Promise.resolve(detail);
    render(<AppStateProvider state={appState}><SkillDetailScreen /></AppStateProvider>);
    expect((await screen.findAllByText("Review checklist")).length).toBeGreaterThan(0);
    expect(screen.getByText("Use this checklist.")).toBeTruthy();
    expect(screen.getByText("Codex")).toBeTruthy();
    expect(screen.getByText("skill_edit")).toBeTruthy();
    expect(ipc.calls[0]?.command).toBe("skill_detail_load");
  });

  test("loads a real copy comparison into the shared diff viewer", async () => {
    ipc.reply = (command) => Promise.resolve(command === "skill_detail_load" ? detail : {
      winner: { scope: "project", markdown: "# Project copy", hash: "def456", deployments: ["codex"] },
      shadow: { scope: "user", markdown: "# User copy", hash: "abc123", deployments: [] },
      identical: false, diff: ["--- a/SKILL.md", "+++ b/SKILL.md", "@@ -1 +1 +", "-# User copy", "+# Project copy"].join("\n"),
    });
    render(<AppStateProvider state={appState}><SkillDetailScreen /></AppStateProvider>);
    await screen.findAllByText("Review checklist");
    screen.getByRole("button", { name: "Compare copies" }).click();
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "skill_compare_copies")).toBe(true));
    expect(await screen.findByText(/compared with winning project/)).toBeTruthy();
  });
});
