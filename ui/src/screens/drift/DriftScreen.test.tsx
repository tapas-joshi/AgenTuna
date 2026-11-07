import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { ToastProvider } from "../../overlays/toasts";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import { DriftScreen } from "./DriftScreen";

const ipc = vi.hoisted(() => ({
  calls: [] as { command: string; args: unknown }[],
  reply: (_command: string, _args: unknown): Promise<unknown> => Promise.resolve(undefined),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args?: unknown) => {
    ipc.calls.push({ command, args });
    return ipc.reply(command, args);
  },
}));

afterEach(() => { cleanup(); ipc.calls.length = 0; });

const driftRow = {
  event_id: 17,
  kind: "link_replaced_by_file",
  item_id: "itm_skill",
  item_name: "Review checklist",
  agent: "codex",
  scope: "project",
  path: "/sandbox/project/.agents/skills/review-checklist",
  detected_at: "2026-11-05T12:00:00Z",
  changed_file_count: 1,
  can_adopt: true,
  can_restore: true,
  link_target: "/sandbox/project/.agentuna/skills/review-checklist",
};
const unreadableRow = {
  ...driftRow,
  event_id: 18,
  kind: "unreadable",
  item_name: "Private skill",
  path: "/sandbox/project/.agents/skills/private-skill",
  can_adopt: false,
  can_restore: false,
};
const backup = {
  id: "backup-17",
  original_path: "/sandbox/project/.agents/skills/review-checklist/SKILL.md",
  created_at: "2026-11-05T11:55:00Z",
  size: 84,
  operation_id: "op-17",
  restorable: true,
  unavailable_reason: null,
};
const diff = "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1,1 +1,1 @@\n-old version\n+new version\n";
const projectState = () => {
  const state = createAppState();
  state.restore("prj_test", "project");
  return state;
};

function renderScreen() {
  return render(
    <AppStateProvider state={projectState()}>
      <ToastProvider><DriftScreen /></ToastProvider>
    </AppStateProvider>,
  );
}

describe("DriftScreen", () => {
  test("reviews every drift kind, confirms adoption with a backup, and refreshes the in-sync count", async () => {
    let resolved = false;
    ipc.reply = async (command, args) => {
      if (command === "drift_snapshot") return {
        rows: resolved ? [] : [driftRow, unreadableRow],
        in_sync_count: resolved ? 3 : 1,
        watched_count: 3,
        watcher: { status: "active", path: null, error: null },
      };
      if (command === "drift_preview") return { event_id: (args as { eventId: number }).eventId, path: driftRow.path, canonical_path: "/sandbox/project/.agentuna/skills/review-checklist", previous_version_available: true, diff, on_disk_content: null, redacted: false };
      if (command === "drift_adopt") { resolved = true; return null; }
      return null;
    };
    renderScreen();

    expect(await screen.findByText("Watching")).toBeTruthy();
    expect(await screen.findByRole("heading", { name: "Review checklist" })).toBeTruthy();
    expect(screen.getByText("Managed link replaced")).toBeTruthy();
    expect(screen.getByText("Permission or read error")).toBeTruthy();
    expect(await screen.findByText("old version")).toBeTruthy();
    expect(screen.getByText("new version")).toBeTruthy();
    expect(screen.getByText("1", { selector: "strong" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Adopt on-disk" }));
    const dialog = await screen.findByRole("dialog", { name: "Adopt on-disk changes?" });
    expect(within(dialog).getByText("A backup of this path will be created before the change is applied.")).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Adopt on-disk" }));
    expect(await screen.findByText("Everything is in sync")).toBeTruthy();
    expect(screen.getByText("3", { selector: "strong" })).toBeTruthy();
    expect(ipc.calls.some(({ command }) => command === "drift_adopt")).toBe(true);
  });

  test("shows on-disk content without fabricating a diff when only a baseline hash remains", async () => {
    ipc.reply = async (command, args) => {
      if (command === "drift_snapshot") return {
        rows: [driftRow],
        in_sync_count: 1,
        watched_count: 2,
        watcher: { status: "active", path: null, error: null },
      };
      if (command === "drift_preview") return {
        event_id: (args as { eventId: number }).eventId,
        path: driftRow.path,
        canonical_path: "/sandbox/project/.agentuna/skills/review-checklist",
        previous_version_available: false,
        diff: "",
        on_disk_content: "current on-disk content\n",
        redacted: false,
      };
      return null;
    };
    renderScreen();

    expect(await screen.findByText("Previous version unavailable. Only AgenTuna's baseline hash was retained.")).toBeTruthy();
    expect(screen.getByLabelText("Current on-disk content").textContent).toBe("current on-disk content\n");
    expect(screen.queryByText("old version")).toBeNull();
    expect((screen.getByRole("button", { name: "Adopt on-disk" }) as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Adopt on-disk" }));
    const dialog = await screen.findByRole("dialog", { name: "Adopt on-disk changes?" });
    expect(within(dialog).getByLabelText("Current on-disk content").textContent).toBe("current on-disk content\n");
    expect(within(dialog).queryByText("old version")).toBeNull();
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  });


  test("restores the AgenTuna version only after confirmation", async () => {
    let restored = false;
    ipc.reply = async (command, args) => {
      if (command === "drift_snapshot") return {
        rows: restored ? [] : [driftRow],
        in_sync_count: restored ? 2 : 1,
        watched_count: 2,
        watcher: { status: "active", path: null, error: null },
      };
      if (command === "drift_preview") return {
        event_id: (args as { eventId: number }).eventId,
        path: driftRow.path,
        canonical_path: "/sandbox/project/.agentuna/skills/review-checklist",
        previous_version_available: true,
        diff,
        on_disk_content: null,
        redacted: false,
      };
      if (command === "drift_restore") { restored = true; return null; }
      return null;
    };
    renderScreen();

    expect(await screen.findByText("new version")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Restore AgenTuna version" }));
    const dialog = await screen.findByRole("dialog", { name: "Restore AgenTuna version?" });
    expect(within(dialog).getByText("A backup of this path will be created before the change is applied.")).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Restore AgenTuna version" }));
    expect(await screen.findByText("Everything is in sync")).toBeTruthy();
    expect(ipc.calls.some(({ command }) => command === "drift_restore")).toBe(true);
  });

  test("pause and resume controls call the native watcher command", async () => {
    let paused = false;
    ipc.reply = async (command, args) => {
      if (command === "drift_snapshot") return {
        rows: [],
        in_sync_count: 2,
        watched_count: 2,
        watcher: { status: paused ? "paused" : "active", path: null, error: null },
      };
      if (command === "drift_watcher_set_paused") {
        paused = Boolean((args as { paused: boolean }).paused);
        return { status: paused ? "paused" : "active", path: null, error: null };
      }
      return null;
    };
    renderScreen();

    expect(await screen.findByText("Watching")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Pause watching" }));
    expect(await screen.findByText("Paused")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Resume watching" }));
    expect(await screen.findByText("Watching")).toBeTruthy();
    expect(ipc.calls).toContainEqual({ command: "drift_watcher_set_paused", args: { paused: true } });
    expect(ipc.calls).toContainEqual({ command: "drift_watcher_set_paused", args: { paused: false } });
  });
  test("shows watcher path failures, manually rescans, and previews a deleted-file backup before restore", async () => {
    let recovered = false;
    ipc.reply = async (command) => {
      if (command === "drift_snapshot") return {
        rows: [], in_sync_count: 0, watched_count: 1,
        watcher: recovered ? { status: "active", path: null, error: null } : { status: "error", path: "/sandbox/blocked", error: "permission denied" },
      };
      if (command === "drift_rescan") { recovered = true; return { rows: [], in_sync_count: 1, watched_count: 1, watcher: { status: "active", path: null, error: null } }; }
      if (command === "backups_list") return [backup];
      if (command === "backup_preview") return { id: backup.id, preview_id: "bkpv1_fixture", original_path: backup.original_path, created_at: backup.created_at, diff, destination_hash: null, destination_exists: false, redacted: false };
      if (command === "backup_restore") return { backup_id: backup.id, path: backup.original_path, operation_id: "op-restore" };
      return null;
    };
    renderScreen();
    expect(await screen.findByText("/sandbox/blocked")).toBeTruthy();
    const watcherError = await screen.findByText("File watching is unavailable.");
    fireEvent.click(within(watcherError.closest('[role="alert"]') as HTMLElement).getByRole("button", { name: "Rescan" }));
    expect(await screen.findByText("Watching")).toBeTruthy();
    expect(ipc.calls.some(({ command }) => command === "drift_rescan")).toBe(true);

    fireEvent.click(screen.getByRole("tab", { name: "Backups" }));
    const row = await screen.findByRole("row", { name: /op-17/ });
    expect(within(row).getByText(new Date(backup.created_at).toLocaleString())).toBeTruthy();
    fireEvent.click(within(row).getByRole("button", { name: "Restore" }));
    const dialog = await screen.findByRole("dialog", { name: "Restore from backup" });
    expect(await within(dialog).findByText("new version")).toBeTruthy();
    const restore = within(dialog).getByRole("button", { name: "Restore backup" });
    await waitFor(() => expect((restore as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(restore);
    expect(await screen.findByText(`Restored ${backup.original_path} from backup.`)).toBeTruthy();
    const restoreCall = ipc.calls.find(({ command }) => command === "backup_restore");
    expect(restoreCall?.args).toEqual({
      request: { backup_id: backup.id, preview_id: "bkpv1_fixture", destination_hash: null },
    });
  });
});
