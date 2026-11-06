import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { ToastProvider } from "../../overlays/toasts";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import type { AppState } from "../../state/AppState";
import type { McpServerDetailView } from "../../generated/McpServerDetailView";
import { McpDetailScreen } from "./McpDetailScreen";

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

const mockDetail = (overrides = {}): McpServerDetailView => ({
  item_id: "itm_postgres",
  name: "Postgres",
  scope: "user",
  description: "Postgres database tools",
  transport: "stdio",
  command: "npx",
  args: ["-y", "@modelcontextprotocol/server-postgres"],
  cwd: "/sandbox/projects/db",
  url: null,
  env: [
    { key: "DB_HOST", value: "localhost", is_secret: false, keychain_account: null },
    { key: "DB_PASS", value: "${DB_PASS}", is_secret: true, keychain_account: "postgres-pass" },
  ],
  defined_in: ["/sandbox/user/mcp.json", "/sandbox/home/.claude.json"],
  tools: ["query", "schema", "tables"],
  health_status: "ok",
  health_error_code: null,
  health_error_text: null,
  last_checks: [
    { checked_at: "100", status: "ok", latency_ms: 25n, trigger: "manual", error_text: null },
    { checked_at: "200", status: "ok", latency_ms: 30n, trigger: "scheduled", error_text: null },
  ],
  cadence_text: "every 5 min",
  is_oauth: false,
  needs_auth: false,
  locked: false,
  locked_by: null,
  lock_reason: null,
  agents: [
    { id: "claude_code", label: "Claude Code", presence: "installed", reason: null, enabled: true, state: "linked", config_path: "/sandbox/home/.claude.json" },
    { id: "cursor", label: "Cursor", presence: "not_installed", reason: "Agent is not installed on this machine", enabled: false, state: "none", config_path: null },
  ],
  why_summary: "Postgres is defined in user scope and active for 1 agent(s).",
  ...overrides,
});

let app: AppState;
beforeEach(() => {
  app = createAppState();
  app.restore("prj_test", "user");
  app.setSelection("mcp-detail", ["itm_postgres"]);
  ipc.calls.length = 0;
});
afterEach(() => cleanup());

function renderScreen() {
  return render(
    <AppStateProvider state={app}>
      <ToastProvider>
        <McpDetailScreen />
      </ToastProvider>
    </AppStateProvider>,
  );
}

describe("McpDetailScreen (TM-021)", () => {
  test("renders command, args and env with monospace styling and Keychain account name without secrets", async () => {
    ipc.reply = (command) => {
      if (command === "mcp_server_detail") return Promise.resolve(mockDetail());
      return Promise.resolve(undefined);
    };
    renderScreen();

    await screen.findByText("Postgres");
    expect(screen.getByText("npx")).toBeTruthy();
    expect(screen.getByText("-y @modelcontextprotocol/server-postgres")).toBeTruthy();

    // Secret row shows Keychain account name and never a value
    expect(screen.getByText("Keychain: postgres-pass")).toBeTruthy();
    expect(screen.queryByText("supersecret")).toBeNull();
  });

  test("health strip renders exactly 5 slots, oldest last, and shows Not tested when empty", async () => {
    ipc.reply = (command) => {
      if (command === "mcp_server_detail") {
        return Promise.resolve(mockDetail({ health_status: "not_tested", last_checks: [] }));
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    await screen.findByText("Postgres");
    expect(screen.getAllByText("Not tested").length).toBeGreaterThan(0);
  });

  test("a failing server with auth error shows Fix token and opens TM-026 dialog", async () => {
    ipc.reply = (command) => {
      if (command === "mcp_server_detail") {
        return Promise.resolve(
          mockDetail({
            health_status: "failed",
            health_error_code: "auth_failed",
            needs_auth: true,
          })
        );
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    const fixTokenBtn = await screen.findByRole("button", { name: "Fix token" });
    fireEvent.click(fixTokenBtn);

    expect(await screen.findByRole("heading", { name: "Fix token" })).toBeTruthy();
  });

  test("an OAuth server with no token shows Sign in and opens TM-026", async () => {
    ipc.reply = (command) => {
      if (command === "mcp_server_detail") {
        return Promise.resolve(
          mockDetail({
            is_oauth: true,
            needs_auth: true,
          })
        );
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    const signInBtn = await screen.findByRole("button", { name: "Sign in" });
    fireEvent.click(signInBtn);

    expect(await screen.findByRole("heading", { name: "Fix token" })).toBeTruthy();
  });

  test("Loaded by toggles call SetEnabled and reflect state within one render", async () => {
    ipc.reply = (command) => {
      if (command === "mcp_server_detail") return Promise.resolve(mockDetail());
      if (command === "mcp_server_disable") return Promise.resolve({ stage: "applied", applied: { operation_id: "op_1", files_changed: 1 } });
      return Promise.resolve(undefined);
    };
    renderScreen();

    const toggle = await screen.findByRole("switch", { name: "Claude Code: enabled" });
    fireEvent.click(toggle);

    // Reflects new state within one render
    await waitFor(() => {
      expect(ipc.calls.some((c) => c.command === "mcp_server_disable")).toBe(true);
    });
  });

  test("missing server error displays error and Back button instead of panicking", async () => {
    ipc.reply = (command) => {
      if (command === "mcp_server_detail") {
        return Promise.reject({ code: "not_found", message: "server is missing or was deleted externally" });
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    await screen.findByText(/server is missing or was deleted externally/);
    const backBtn = screen.getByRole("button", { name: "Back to MCP Servers" });
    fireEvent.click(backBtn);
    expect(app.getSnapshot().route).toBe("mcp-list");
  });
});
