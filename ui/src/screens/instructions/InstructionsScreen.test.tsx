import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import "../../components/editor/codemirror-test-setup";
import { ToastProvider } from "../../overlays/toasts";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import { InstructionsScreen } from "./InstructionsScreen";

const ipc = vi.hoisted(() => ({
  calls: [] as { command: string; args: unknown }[],
  reply: (_command: string): Promise<unknown> => Promise.resolve(undefined),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args?: unknown) => {
    ipc.calls.push({ command, args });
    return ipc.reply(command);
  },
}));

const appState = createAppState();
appState.restore("prj_fixture", "project");
const file = {
  item_id: "itm_rule",
  scope: "project",
  path: "/sandbox/Projects/AgenTuna/AGENTS.md",
  managed: true,
  content: "# Repository guidance\n\nReview changes carefully.\n",
  hash: "c".repeat(64),
  agents: ["codex", "omp"],
  read_error: null,
};
const unreadable = {
  item_id: "",
  scope: "user",
  path: "/sandbox/home/.claude/CLAUDE.md",
  managed: false,
  content: "",
  hash: "",
  agents: [],
  read_error: "Permission denied: the file cannot be read by AgenTuna",
};

afterEach(() => {
  cleanup();
  ipc.calls.length = 0;
});

function renderScreen() {
  return render(
    <AppStateProvider state={appState}>
      <ToastProvider>
        <InstructionsScreen />
      </ToastProvider>
    </AppStateProvider>,
  );
}

describe("InstructionsScreen", () => {
  test("loads a scope-grouped list and marks the opened file clean", async () => {
    ipc.reply = (command) =>
      Promise.resolve(command === "instructions_list" ? [file] : undefined);
    renderScreen();
    expect(await screen.findByRole("button", { name: "AGENTS.md" })).toBeTruthy();
    const list = screen.getByRole("complementary", { name: "Instruction files" });
    expect(within(list).getByText("Project")).toBeTruthy();
    expect(screen.getByText("No unsaved changes")).toBeTruthy();
    expect(screen.getByText("Repo conventions (codex, omp)")).toBeTruthy();
    expect(ipc.calls[0]).toMatchObject({
      command: "instructions_list",
      args: { projectId: "prj_fixture" },
    });
  });

  test("editing sets the dirty line and Save sends the opened snapshot hash", async () => {
    ipc.reply = (command) => {
      if (command === "instructions_list") return Promise.resolve([file]);
      if (command === "instruction_save") {
        return Promise.resolve({ status: "saved", conflict: null, backup_path: "/sandbox/backups/AGENTS.md", hash: "d".repeat(64), unreadable_path: null });
      }
      return Promise.resolve(undefined);
    };
    renderScreen();
    const editor = await screen.findByRole("textbox", { name: "Instruction content" });
    await userEvent.click(editor);
    await userEvent.type(editor, "!");
    await waitFor(() => expect(screen.getByText(/Unsaved changes/)).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(ipc.calls.some((call) => call.command === "instruction_save")).toBe(true),
    );
    const save = ipc.calls.find((call) => call.command === "instruction_save");
    expect(save?.args).toMatchObject({
      request: {
        item_id: "itm_rule",
        scope: "project",
        expected_hash: "c".repeat(64),
        force: false,
        project_id: "prj_fixture",
      },
    });
    expect((await screen.findAllByText(/AGENTS\.md saved · backup/)).length).toBeGreaterThan(0);
  });

  test("an external edit shows the diff and Overwrite repeats the save with force", async () => {
    const conflict = {
      path: "/sandbox/Projects/AgenTuna/AGENTS.md",
      diff: "--- disk\n+++ buffer\n@@ -1 +1 @@\n-old\n+new\n",
      disk_content: "# Edited elsewhere\n",
      disk_hash: "e".repeat(64),
      buffer_content: "# Repository guidance\n",
    };
    let saves = 0;
    ipc.reply = (command) => {
      if (command === "instructions_list") return Promise.resolve([file]);
      if (command === "instruction_save") {
        saves += 1;
        return Promise.resolve(
          saves === 1
            ? { status: "conflict", conflict, backup_path: null, hash: null, unreadable_path: null }
            : { status: "saved", conflict: null, backup_path: "/sandbox/backups/AGENTS.md", hash: "f".repeat(64), unreadable_path: null },
        );
      }
      return Promise.resolve(undefined);
    };
    renderScreen();
    const editor = await screen.findByRole("textbox", { name: "Instruction content" });
    await userEvent.click(editor);
    await userEvent.type(editor, "!");
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    const dialog = await screen.findByRole("dialog", { name: "File changed on disk" });
    expect(dialog.textContent).toContain("@@ -1 +1 @@");
    fireEvent.click(screen.getByRole("button", { name: "Overwrite" }));
    await waitFor(() => expect(saves).toBe(2));
    const forced = ipc.calls.filter((call) => call.command === "instruction_save").at(-1);
    expect(forced?.args).toMatchObject({ request: { force: true } });
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "File changed on disk" })).toBeNull(),
    );
  });

  test("an unreadable file renders the inline permission error with its path", async () => {
    ipc.reply = (command) =>
      Promise.resolve(command === "instructions_list" ? [file, unreadable] : undefined);
    renderScreen();
    const entry = await screen.findByRole("button", { name: "CLAUDE.md" });
    fireEvent.click(entry);
    const alerts = await screen.findAllByRole("alert");
    expect(alerts.some((alert) => alert.textContent?.includes("Permission denied"))).toBe(true);
    expect(alerts.some((alert) => alert.textContent?.includes("/sandbox/home/.claude/CLAUDE.md"))).toBe(true);
    expect(screen.getByText("Unreadable")).toBeTruthy();
  });

  test("an empty scope offers the New rule action and creates a project rule", async () => {
    ipc.reply = (command) => {
      if (command === "instructions_list") return Promise.resolve([]);
      if (command === "instruction_create_options") {
        return Promise.resolve({
          global_writable: true,
          global_reason: null,
          local_available: true,
          local_reason: null,
          has_project: true,
        });
      }
      if (command === "instruction_create_preview") {
        return Promise.resolve({
          path: "/sandbox/Projects/AgenTuna/AGENTS.md",
          exists: false,
          reason: null,
        });
      }
      if (command === "instruction_create") {
        return Promise.resolve("/sandbox/Projects/AgenTuna/AGENTS.md");
      }
      return Promise.resolve(undefined);
    };
    renderScreen();
    const buttons = await screen.findAllByRole("button", { name: "New rule file" });
    fireEvent.click(buttons.at(-1) as HTMLButtonElement);
    const createButton = await screen.findByRole("button", { name: /^Create/ });
    fireEvent.click(createButton);
    await waitFor(() =>
      expect(ipc.calls.some((call) => call.command === "instruction_create")).toBe(true),
    );
    expect((await screen.findAllByText(/created/)).length).toBeGreaterThan(0);
  });
});
  test("external watcher events protect dirty edits until Reload is confirmed or Keep mine is chosen", async () => {
    let diskFile = file;
    ipc.reply = (command) => Promise.resolve(command === "instructions_list" ? [diskFile] : undefined);
    appState.setEditorBuffer("overview", { route: "overview", value: file.content, savedValue: file.content });
    renderScreen();
    const editor = await screen.findByRole("textbox", { name: "Instruction content" });
    await userEvent.click(editor);
    await userEvent.type(editor, " Mine");
    await waitFor(() => expect(appState.getSnapshot().editors[appState.getSnapshot().route]?.value).toContain("Mine"));
    diskFile = { ...file, content: "# External version\n", hash: "d".repeat(64) };

    fireEvent(window, new CustomEvent("agentuna:external-refresh", {
      detail: { paths: [file.path], full_rescan: false },
    }));
    const bannerText = await screen.findByText("Changed outside AgenTuna.");
    const banner = bannerText.closest('[role="alert"]') as HTMLElement;
    expect(banner.textContent).toContain("Changed outside AgenTuna.");
    fireEvent.click(within(banner).getByRole("button", { name: "Keep mine" }));
    await waitFor(() => expect(appState.getSnapshot().externalChanges[appState.getSnapshot().route]?.kept).toBe(true));
    expect(editor.textContent).toContain("Mine");

    fireEvent.click(within(banner).getByRole("button", { name: "Reload" }));
    const confirmation = await screen.findByRole("dialog", { name: "Reload external changes?" });
    expect(confirmation.textContent).toContain("replaces your unsaved edits");
    fireEvent.click(within(confirmation).getByRole("button", { name: "Reload file" }));
    await waitFor(() => expect(editor.textContent).toContain("External version"));
    expect(appState.getSnapshot().externalChanges[appState.getSnapshot().route]).toBeUndefined();
  });
