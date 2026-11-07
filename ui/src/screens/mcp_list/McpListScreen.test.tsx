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
  test("Adopt on a native-only row opens AdoptModal dialog instead of navigating to migrate", async () => {
    ipc.reply = (command) =>
      Promise.resolve(
        command === "mcp_server_list"
          ? list({ servers: [row({ id: "", native: true, name: "Native server", source_path: "/sandbox/home/.claude.json" })] })
          : undefined,
      );
    renderScreen();
    const menu = await openMenu("Native server");
    await userEvent.setup().click(within(menu).getByRole("menuitem", { name: "Adopt…" }));
    expect(await screen.findByRole("dialog", { name: /Adopt Native server/i })).toBeTruthy();
    expect(app.getSnapshot().route).not.toBe("migrate");
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

  test("a locked row shows a disabled switch naming the lock reason and writes nothing", async () => {
    ipc.reply = (command) =>
      Promise.resolve(
        command === "mcp_server_list"
          ? list({ servers: [row({ locked: true, lock_scope: "global", lock_reason: "Approved version" })] })
          : undefined,
      );
    renderScreen();
    const toggle = await screen.findByRole("switch", { name: "Claude Code: Locked" });
    expect((toggle as HTMLButtonElement).disabled).toBe(true);
    const cell = toggle.closest('[role="group"]');
    expect(cell?.getAttribute("title")).toContain("Approved version");
    expect(cell?.getAttribute("title")).toContain("Global");
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
    expect(screen.getByText("Healthy")).toBeTruthy();
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

describe("McpListScreen per-agent switches and health (design 06-mcp-servers)", () => {
  test("renders an OMP switch and enables the server through the reviewed plan", async () => {
    const server = row({
      agents: [
        { agent: "claude_code", state: "linked", config_path: "/sandbox/home/.claude.json", enabled: true },
        { agent: "omp", state: "off", config_path: null, enabled: false },
      ],
    });
    ipc.reply = (command) => {
      if (command === "mcp_server_list") return Promise.resolve(list({ servers: [server] }));
      if (command === "mcp_server_enable") return Promise.resolve(preview(1));
      return Promise.resolve(undefined);
    };
    renderScreen();

    const toggle = await screen.findByRole("switch", { name: "omp: Off" });
    await userEvent.setup().click(toggle);
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/Enable Server for omp\?/)).toBeTruthy();
    await waitFor(() =>
      expect(ipc.calls).toContainEqual({
        command: "mcp_server_enable",
        args: {
          request: {
            item_id: "itm_server",
            scope: "global",
            project_id: "prj_visual_fixture",
            agent: "omp",
            in_repo: false,
            apply: false,
            preview_id: null,
          },
        },
      }),
    );
    fireEvent.click(within(dialog).getByRole("button", { name: "Enable" }));
    await waitFor(() =>
      expect(ipc.calls).toContainEqual({
        command: "mcp_server_enable",
        args: {
          request: {
            item_id: "itm_server",
            scope: "global",
            project_id: "prj_visual_fixture",
            agent: "omp",
            in_repo: false,
            apply: true,
            preview_id: "preview_1",
          },
        },
      }),
    );
    expect(await screen.findByText("Enabled Server for omp")).toBeTruthy();
  });

  test("an uninstalled agent column renders a disabled switch with the reason", async () => {
    ipc.reply = (command) => {
      if (command === "mcp_server_list") return Promise.resolve(list());
      if (command === "agents_list") {
        return Promise.resolve([
          { id: "cursor", name: "Cursor", presence: "not_installed", installed: false },
        ]);
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    const toggle = await screen.findByRole("switch", { name: "Cursor: Off" });
    expect((toggle as HTMLButtonElement).disabled).toBe(true);
    expect(toggle.closest('[role="group"]')?.getAttribute("title")).toContain("not installed");
    expect(ipc.calls.filter((call) => call.command === "mcp_server_enable")).toHaveLength(0);
  });

  test("native rows and health states are words, not colour alone", async () => {
    const nativeServer = row({
      id: "",
      name: "context7",
      native: true,
      source_path: "/sandbox/home/.codex/config.toml",
      health: "unknown",
      agents: [
        {
          agent: "codex",
          state: "native",
          config_path: "/sandbox/home/.codex/config.toml",
          enabled: true,
        },
      ],
    });
    ipc.reply = (command) => {
      if (command === "mcp_server_list") {
        return Promise.resolve(
          list({ servers: [nativeServer, row({ id: "itm_failing", name: "github", health: "failed" })] }),
        );
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    expect(await screen.findByText("native · not in AgenTuna")).toBeTruthy();
    expect(await screen.findByText("Not tested")).toBeTruthy();
    expect((await screen.findAllByText("Failing")).length).toBeGreaterThan(0);
    const nativeCell = screen
      .getAllByRole("group", { name: "Codex" })
      .find((cell) => cell.getAttribute("data-state") === "native");
    expect(nativeCell).toBeDefined();
    expect(nativeCell?.getAttribute("title")).toContain("/sandbox/home/.codex/config.toml");
    expect(nativeCell ? within(nativeCell).queryByRole("switch") : null).toBeNull();
  });

  test("the count line and the banner count the same native rows the table renders", async () => {
    const nativeRows = [
      row({ id: "", name: "context7", native: true, source_path: "/sandbox/home/.claude.json" }),
      row({ id: "", name: "alpha", native: true, source_path: "/sandbox/home/.codex/config.toml" }),
    ];
    ipc.reply = (command) => {
      if (command === "mcp_server_list") {
        return Promise.resolve(list({ servers: [...nativeRows, row({ id: "itm_managed", name: "managed" })] }));
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    expect(
      await screen.findByText("3 servers, 1 managed by AgenTuna, 2 live only in agent config files"),
    ).toBeTruthy();
    const banner = screen.getByRole("region", { name: "Unmanaged servers banner" });
    expect(banner.textContent).toContain("2 servers in agent folders are not in AgenTuna");
    const nativeTags = screen.getAllByText("native · not in AgenTuna");
    expect(nativeTags).toHaveLength(2);
  });

  test("Adopt names the server inside a shared configuration document", async () => {
    ipc.reply = (command) => {
      if (command === "mcp_server_list") {
        return Promise.resolve(
          list({
            servers: [
              row({
                id: "",
                name: "context7",
                native: true,
                source_path: "/sandbox/home/.claude.json",
              }),
            ],
          }),
        );
      }
      if (command === "adopt_preview") {
        return Promise.resolve({
          source: "/sandbox/home/.claude.json",
          store_root: "/sandbox/user-store",
          store_destination: "/sandbox/user-store/mcp.json",
          slug: "context7",
          native_name: "context7",
          kind: "mcp_server",
          agents: ["claude_code"],
          handling: "link",
          renamed: false,
          current: "present",
        });
      }
      if (command === "adopt_apply") {
        return Promise.resolve({
          summary: "Adopted",
          operation_id: "op_1",
          store_destination: "/sandbox/user-store/mcp.json",
          slug: "context7",
          handling: "link",
        });
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    fireEvent.click(await screen.findByRole("button", { name: "Adopt" }));
    const modal = await screen.findByRole("dialog", { name: /Adopt context7/i });
    expect(modal).toBeTruthy();
    const modalAdoptBtn = within(modal).getByRole("button", { name: "Adopt" });
    fireEvent.click(modalAdoptBtn);
    await waitFor(() =>
      expect(ipc.calls.some((call) => call.command === "adopt_apply")).toBe(true),
    );
    const preview = ipc.calls.find((call) => call.command === "adopt_preview");
    expect(preview?.args).toMatchObject({
      path: "/sandbox/home/.claude.json",
      name: "context7",
    });
    const applied = ipc.calls.find((call) => call.command === "adopt_apply");
    expect(applied?.args).toMatchObject({ name: "context7" });
  });
});
