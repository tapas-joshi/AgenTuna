import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { ToastProvider } from "../../overlays/toasts";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import type { AppState } from "../../state/AppState";
import type { McpServerActionOutcome } from "../../generated/McpServerActionOutcome";
import type { McpServerListView } from "../../generated/McpServerListView";
import { McpListScreen } from "./McpListScreen";

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

const row = (overrides: Partial<McpServerListView["servers"][number]> = {}) => ({
  id: "itm_server",
  slug: "server",
  name: "Server",
  scope: "global",
  transport: "stdio",
  native: false,
  locked: false,
  lock_scope: null,
  lock_reason: null,
  health: "ok",
  source_path: "/sandbox/global/mcp.json",
  agents: [{ agent: "claude_code", state: "linked", config_path: "/sandbox/home/.claude.json", enabled: true }],
  ...overrides,
});

const list = (overrides: Partial<McpServerListView> = {}): McpServerListView => ({
  servers: [row()],
  project_id: "prj_visual_fixture",
  project_name: "AgenTuna",
  project_agents: ["claude_code"],
  global_writable: true,
  global_reason: null,
  ...overrides,
});

const preview = (files: number): McpServerActionOutcome => ({
  stage: "preview",
  preview: {
    change_set_id: "cs_1",
    preview_id: "preview_1",
    files: Array.from({ length: files }, (_, index) => ({
      agent: "claude_code",
      scope: "global",
      path: `/sandbox/home/.claude.json#${index}`,
      additions: 0n,
      deletions: 1n,
      diff: "",
    })),
    skipped_agents: [],
    warnings: [],
  },
});

let app: AppState;
beforeEach(() => {
  app = createAppState();
  app.restore("prj_visual_fixture", "global");
  ipc.calls.length = 0;
});
afterEach(() => cleanup());

function renderScreen() {
  render(
    <AppStateProvider state={app}>
      <ToastProvider>
        <McpListScreen />
      </ToastProvider>
    </AppStateProvider>,
  );
}

/** Preview ids passed to an applied disable call, read through runtime narrowing. */
function appliedDisablePreviewIds(): (string | undefined)[] {
  return ipc.calls
    .filter((call) => call.command === "mcp_server_disable")
    .map((call) => {
      const args = call.args;
      if (typeof args !== "object" || args === null || !("request" in args)) return undefined;
      const request = args.request;
      if (typeof request !== "object" || request === null || !("apply" in request)) return undefined;
      return request.apply === true && "preview_id" in request && typeof request.preview_id === "string"
        ? request.preview_id
        : undefined;
    });
}

async function openMenu(name = "Server") {
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: `Actions for ${name}` }));
  return screen.findByRole("menu");
}

describe("McpListScreen row actions", () => {
  test("Open on a native-only row routes to adopt instead of detail", async () => {
    ipc.reply = (command) =>
      Promise.resolve(
        command === "mcp_server_list"
          ? list({ servers: [row({ id: "", native: true, name: "Native server" })] })
          : undefined,
      );
    renderScreen();
    const menu = await openMenu("Native server");
    await userEvent.setup().click(within(menu).getByRole("menuitem", { name: "Adopt…" }));
    await waitFor(() => expect(app.getSnapshot().route).toBe("migrate"));
    expect(ipc.calls.some((call) => call.command === "mcp_disable_in_repo")).toBe(false);
  });

  test("confirming Disable in this repo previews then applies, and cancelling writes nothing", async () => {
    ipc.reply = (command) => {
      if (command === "mcp_server_list") return Promise.resolve(list());
      if (command === "mcp_server_disable") return Promise.resolve(preview(1));
      return Promise.resolve(undefined);
    };
    renderScreen();
    let menu = await openMenu();
    await userEvent.setup().click(within(menu).getByRole("menuitem", { name: "Disable in this repo" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/stops loading in AgenTuna/)).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(ipc.calls.filter((call) => call.command === "mcp_server_disable")).toHaveLength(1);

    ipc.calls.length = 0;
    menu = await openMenu();
    await userEvent.setup().click(within(menu).getByRole("menuitem", { name: "Disable in this repo" }));
    const confirm = await screen.findByRole("dialog");
    fireEvent.click(within(confirm).getByRole("button", { name: "Disable" }));
    await waitFor(() => expect(appliedDisablePreviewIds()).toContain("preview_1"));
    expect(await screen.findByText("Disabled Server in this repo")).toBeTruthy();
  });

  test("delete on a server referenced by three files needs the exact typed name", async () => {
    ipc.reply = (command) => {
      if (command === "mcp_server_list") return Promise.resolve(list());
      if (command === "mcp_server_delete") return Promise.resolve(preview(3));
      return Promise.resolve(undefined);
    };
    renderScreen();
    const menu = await openMenu();
    await userEvent.setup().click(within(menu).getByRole("menuitem", { name: "Delete server" }));
    const dialog = await screen.findByRole("dialog");
    const confirm = within(dialog).getByRole("button", { name: "Delete server" }) as HTMLButtonElement;
    await waitFor(() => expect(within(dialog).getByLabelText("Server name")).toBeTruthy());
    expect(confirm.disabled).toBe(true);
    fireEvent.change(within(dialog).getByLabelText("Server name"), { target: { value: "Server" } });
    expect(confirm.disabled).toBe(false);
  });

  test("a locked row shows the locked toast and leaves the toggle unchanged", async () => {
    ipc.reply = (command) =>
      Promise.resolve(
        command === "mcp_server_list"
          ? list({ servers: [row({ locked: true, lock_reason: "Approved version" })] })
          : undefined,
      );
    renderScreen();
    const toggle = await screen.findByRole("button", { name: "claude_code: linked" });
    expect((toggle as HTMLButtonElement).disabled).toBe(true);
    expect(toggle.getAttribute("title")).toBe("Approved version");
    expect(ipc.calls.filter((call) => call.command === "mcp_server_disable")).toHaveLength(0);
  });

  test("Lock is disabled on a read-only Global store and says why", async () => {
    ipc.reply = (command) =>
      Promise.resolve(
        command === "mcp_server_list"
          ? list({ global_writable: false, global_reason: "Global store is read-only" })
          : undefined,
      );
    renderScreen();
    const menu = await openMenu();
    const lockItem = within(menu).getByRole("menuitem", { name: /Lock/ });
    expect(lockItem.getAttribute("data-disabled")).not.toBeNull();
    expect(within(menu).getByText(/Unavailable: Global store is read-only/)).toBeTruthy();
  });

  test("a failed lock shows a warning and leaves the rendered row unchanged", async () => {
    ipc.reply = (command) => {
      if (command === "mcp_server_list") return Promise.resolve(list());
      if (command === "effective_lock") return Promise.reject({ code: "locked", message: "lock denied" });
      return Promise.resolve(undefined);
    };
    renderScreen();
    const menu = await openMenu();
    await userEvent.setup().click(within(menu).getByRole("menuitem", { name: /Lock/ }));
    const dialog = await screen.findByRole("dialog");
    const confirm = within(dialog).getByRole("button", { name: "Lock item" }) as HTMLButtonElement;
    expect(confirm.disabled).toBe(true);
    fireEvent.change(within(dialog).getByLabelText("Lock reason"), { target: { value: "Approved version" } });
    fireEvent.click(confirm);
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "effective_lock")).toBe(true));
    expect(await screen.findByText("lock denied")).toBeTruthy();
    // No refresh: the rendered model is exactly the pre-failure row.
    expect(ipc.calls.filter((call) => call.command === "mcp_server_list")).toHaveLength(1);
    expect(screen.getByText("health: ok")).toBeTruthy();
  });

  test("Escape closes the lock dialog without writing", async () => {
    ipc.reply = (command) => (command === "mcp_server_list" ? Promise.resolve(list()) : Promise.resolve(undefined));
    renderScreen();
    const menu = await openMenu();
    await userEvent.setup().click(within(menu).getByRole("menuitem", { name: /Lock/ }));
    await screen.findByRole("dialog");
    await userEvent.setup().keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(ipc.calls.some((call) => call.command === "effective_lock")).toBe(false);
  });
});
