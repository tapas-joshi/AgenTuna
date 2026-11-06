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
  editable: true, update_available: false, source_commit_sha: null, source_remote_sha: null, source_last_checked_at: null, auto_update: "inherit",
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

  test("checks a source, shows the update badge and applies only a reviewed preview", async () => {
    const sourced = { ...detail, source: "git", update_available: true, source_commit_sha: "a".repeat(40), source_remote_sha: "b".repeat(40), source_last_checked_at: "2026-11-06T11:00:00Z" };
    ipc.reply = (command) => Promise.resolve(
      command === "skill_detail_load" ? sourced :
      command === "skill_source_check" ? { status: "update_available", remote_sha: "b".repeat(40), local_modified: false } :
      command === "skill_update_preview" ? { preview_id: "preview_update", files: [{ path: "skills/review-checklist/SKILL.md", diff: "-old\n+new", backup_required: true }] } :
      command === "skill_update_apply" ? { operation_id: "op_update", files_changed: 1 } :
      undefined,
    );
    render(<AppStateProvider state={appState}><SkillDetailScreen /></AppStateProvider>);
    expect(await screen.findByText("Update available")).toBeTruthy();
    screen.getByRole("button", { name: "Check now" }).click();
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "skill_source_check")).toBe(true));
    screen.getByRole("button", { name: "Update now" }).click();
    expect(await screen.findByRole("dialog", { name: "Review skill update" })).toBeTruthy();
    expect(screen.getByText("skills/review-checklist/SKILL.md")).toBeTruthy();
    screen.getByRole("button", { name: "Apply update" }).click();
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "skill_update_apply")).toBe(true));
    expect(ipc.calls.find((call) => call.command === "skill_update_apply")?.args).toEqual({ previewId: "preview_update", confirmed: true });
  });

  test("hides a stale update badge when the latest source check is unknown", async () => {
    const sourced = { ...detail, source: "git", update_available: true, source_commit_sha: "a".repeat(40), source_remote_sha: "b".repeat(40), source_last_checked_at: "2026-11-06T11:00:00Z" };
    ipc.reply = (command) => Promise.resolve(
      command === "skill_detail_load" ? sourced :
      command === "skill_source_check" ? { status: "unknown", remote_sha: null, local_modified: null } :
      undefined,
    );
    render(<AppStateProvider state={appState}><SkillDetailScreen /></AppStateProvider>);
    expect(await screen.findByText("Update available")).toBeTruthy();
    screen.getByRole("button", { name: "Check now" }).click();
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "skill_source_check")).toBe(true));
    await waitFor(() => expect(screen.queryByText("Update available")).toBeNull());
    expect(screen.getByTitle("Source status is unknown or offline; no update badge is shown.")).toBeTruthy();
  });

  test("mounts the reviewed per-agent enable flow from skill detail", async () => {
    ipc.reply = (command) => Promise.resolve(
      command === "skill_detail_load" ? detail :
      command === "agents_list" ? [{ id: "codex", name: "Codex", installed: true, version: "1.0.0", managed: true, paths: [], health: "healthy", link_mode: "link" }] :
      command === "skill_agent_preview" ? { preview_id: "preview_skill_agent", enabled: false, mode: "link", paths: ["/sandbox/.agents/skills/review-checklist"], coupled_agents: ["omp"], warnings: [], no_op: false } :
      command === "skill_agent_apply" ? { operation_id: "op_skill_agent", enabled: false, changed: true } :
      undefined,
    );
    render(<AppStateProvider state={appState}><SkillDetailScreen /></AppStateProvider>);

    await screen.findAllByText("Review checklist");
    screen.getByRole("button", { name: "Manage agents" }).click();
    const dialog = await screen.findByRole("dialog", { name: "Enable for agents" });
    const checkbox = dialog.querySelector('input[type="checkbox"]');
    expect(checkbox).toBeTruthy();
    (checkbox as HTMLInputElement).click();
    screen.getByRole("button", { name: "Apply" }).click();

    await waitFor(() => expect(ipc.calls.some((call) => call.command === "skill_agent_apply")).toBe(true));
    expect(ipc.calls.find((call) => call.command === "skill_agent_preview")?.args).toEqual({
      request: { item_id: "itm_fake", scope: "user", agent_id: "codex", enabled: false, project_id: null },
    });
    expect(ipc.calls.find((call) => call.command === "skill_agent_apply")?.args).toEqual({ previewId: "preview_skill_agent" });
  });
});
