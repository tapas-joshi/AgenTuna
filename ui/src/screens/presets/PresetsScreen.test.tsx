import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { ToastProvider } from "../../overlays/toasts";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import { PresetsScreen } from "./PresetsScreen";

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
appState.restore("prj_fixture", "user");
const preset = {
  id: "prst_1",
  name: "Code review",
  scope: "user",
  created_at: "2026-11-06T10:00:00Z",
  ratios: [{ target: "claude_code", enabled: 2, total: 3, complete: false }],
  members: [
    { item_id: "itm_skill", name: "Code Review", kind: "skill", enabled: true, exists: true },
    { item_id: "itm_gone", name: "Missing item", kind: "skill", enabled: true, exists: false },
  ],
  applications: [{ id: 7, target_kind: "agent", target: "claude_code", applied_at: "2026-11-06T10:00:00Z", member_count: 1 }],
  missing_members: ["Missing item"],
  applicable: true,
};
const emptyPreset = { ...preset, members: [], ratios: [{ target: "claude_code", enabled: 0, total: 0, complete: false }], applicable: false, missing_members: [] };

afterEach(() => {
  cleanup();
  ipc.calls.length = 0;
});

function renderScreen() {
  return render(
    <AppStateProvider state={appState}>
      <ToastProvider>
        <PresetsScreen />
      </ToastProvider>
    </AppStateProvider>,
  );
}

describe("PresetsScreen", () => {
  test("lists database ratios and explains missing members", async () => {
    ipc.reply = (command) =>
      Promise.resolve(command === "presets_list" ? [preset] : command === "preset_member_candidates" ? [] : undefined);
    renderScreen();
    const list = await screen.findByLabelText("Preset list");
    expect(await within(list).findByText("Code review", { exact: true })).toBeTruthy();
    expect(within(list).getByText("claude code 2/3")).toBeTruthy();
    expect(screen.getByText("1 missing")).toBeTruthy();
    expect(screen.getByText(/excluded from the applied ratios/)).toBeTruthy();
  });

  test("applying to an agent sends the reviewed target and refreshes the ratios", async () => {
    ipc.reply = (command) => {
      if (command === "presets_list") return Promise.resolve([preset]);
      if (command === "preset_member_candidates") return Promise.resolve([]);
      if (command === "preset_apply") {
        return Promise.resolve({ operation_id: "op_1", changed: true, changed_members: 1, enabled: 3, total: 3, complete: true, warnings: [] });
      }
      return Promise.resolve(undefined);
    };
    renderScreen();
    fireEvent.click(await screen.findByRole("button", { name: "Apply to agent" }));
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "preset_apply")).toBe(true));
    expect(ipc.calls.find((call) => call.command === "preset_apply")?.args).toMatchObject({
      request: { preset_id: "prst_1", target_kind: "agent", target_id: "claude_code", project_id: "prj_fixture" },
    });
    expect((await screen.findAllByText(/Code review applied/)).length).toBeGreaterThan(0);
  });

  test("unapply reverts the recorded application", async () => {
    ipc.reply = (command) => {
      if (command === "presets_list") return Promise.resolve([preset]);
      if (command === "preset_member_candidates") return Promise.resolve([]);
      if (command === "preset_unapply") {
        return Promise.resolve({ operation_id: "op_2", changed: true, changed_members: 1, enabled: 1, total: 3, complete: false, warnings: [] });
      }
      return Promise.resolve(undefined);
    };
    renderScreen();
    fireEvent.click(await screen.findByRole("button", { name: "Unapply" }));
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "preset_unapply")).toBe(true));
    expect(ipc.calls.find((call) => call.command === "preset_unapply")?.args).toMatchObject({ applicationId: 7 });
  });

  test("an empty preset cannot be applied and explains why", async () => {
    ipc.reply = (command) =>
      Promise.resolve(command === "presets_list" ? [emptyPreset] : command === "preset_member_candidates" ? [] : undefined);
    renderScreen();
    const apply = await screen.findByRole("button", { name: "Apply to agent" });
    expect((apply as HTMLButtonElement).disabled).toBe(true);
    expect(apply.getAttribute("title")).toContain("no existing enabled members");
  });

  test("a duplicate preset name is blocked inline when renaming", async () => {
    const other = { ...preset, id: "prst_2", name: "Take two", applications: [], members: [], ratios: [] };
    ipc.reply = (command) =>
      Promise.resolve(command === "presets_list" ? [preset, other] : command === "preset_member_candidates" ? [] : undefined);
    renderScreen();
    await screen.findByRole("textbox", { name: "Preset name" });
    const name = screen.getByRole("textbox", { name: "Preset name" });
    fireEvent.change(name, { target: { value: "take two" } });
    expect(
      await screen.findByText("A preset named “take two” already exists in this scope."),
    ).toBeTruthy();
    expect((screen.getByRole("button", { name: "Rename" }) as HTMLButtonElement).disabled).toBe(true);
  });

  test("deleting a preset calls preset_delete after confirmation and refreshes the list", async () => {
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
    ipc.reply = (command) =>
      Promise.resolve(command === "presets_list" ? [preset] : command === "preset_member_candidates" ? [] : undefined);
    renderScreen();
    const deleteBtn = await screen.findByRole("button", { name: "Delete" });
    fireEvent.click(deleteBtn);
    expect(confirmSpy).toHaveBeenCalledWith("Delete the preset “Code review”?");
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "preset_delete")).toBe(true));
    expect(ipc.calls.find((call) => call.command === "preset_delete")?.args).toMatchObject({
      presetId: "prst_1",
    });
    confirmSpy.mockRestore();
  });
});
