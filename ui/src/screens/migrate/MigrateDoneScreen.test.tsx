import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ToastProvider } from "../../overlays/toasts";
import type { MigrateApplyResultView } from "../../generated/MigrateApplyResultView";
import { MigrateDoneScreen } from "./MigrateDoneScreen";

const MOCK_INVOKE = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => MOCK_INVOKE(...args),
}));

const SAMPLE_COUNTS = {
  items: 3,
  files_created: 5,
  files_backed_up: 3,
  links_written: 2,
  files_left_in_place: 0,
};

const SAMPLE_RESULT: MigrateApplyResultView = {
  job_id: "job_1",
  operation_id: "op_mig_42",
  completed: 5n,
  touched_paths: ["/home/.agentuna/skills/demo"],
  warnings: [],
  backup_dir: "/home/.agentuna/backups/migration",
  scopes: [
    { scope: "user", store_path: "/home/.agentuna", counts: SAMPLE_COUNTS },
  ],
  strategy: "replace_with_links",
  plan_hash: "a".repeat(64),
  counts: SAMPLE_COUNTS,
  steps: [],
};

describe("MigrateDoneScreen", () => {
  beforeEach(() => {
    MOCK_INVOKE.mockReset();
    MOCK_INVOKE.mockImplementation((command: string) => {
      if (command === "operation_undo_check") {
        return Promise.resolve({
          operation_id: "op_mig_42",
          known: true,
          already_undone: false,
          expired: false,
          window_days: 30,
          expires_at_unix_ms: BigInt(Date.now() + 30 * 86400 * 1000),
          conflicted_paths: [],
        });
      }
      if (command === "operation_undo") {
        return Promise.resolve({
          undo_operation_id: "op_undo_99",
          summary: "Operation op_mig_42 was undone.",
        });
      }
      return Promise.reject(new Error("unexpected command"));
    });
  });

  afterEach(() => {
    cleanup();
  });

  test("renders migration toast copy and its Undo action with applied summary counts", async () => {
    const user = userEvent.setup();
    render(
      <ToastProvider>
        <MigrateDoneScreen
          result={SAMPLE_RESULT}
          onFinish={vi.fn()}
        />
      </ToastProvider>,
    );

    expect(screen.getByText("3 items now live in AgenTuna")).toBeTruthy();
    expect(screen.getByText(/3 items copied into the User store/)).toBeTruthy();
    expect(screen.getByText(/2 skill folders replaced by links/)).toBeTruthy();
    expect(screen.getByText(/5 files created or rewritten/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /Undo migration/ })).toBeTruthy();
    const notifications = screen.getByRole("list", { name: "Notifications" });
    expect(await within(notifications).findByText("Migration complete")).toBeTruthy();
    expect(within(notifications).getByText("3 items moved to AgenTuna.")).toBeTruthy();
    await user.click(within(notifications).getByRole("button", { name: /^Undo$/ }));
    expect(await screen.findByText("Confirm undo")).toBeTruthy();
  });


  test("runs undo flow and displays confirmation dialog", async () => {
    const user = userEvent.setup();
    render(
      <ToastProvider>
        <MigrateDoneScreen
          result={SAMPLE_RESULT}
          onFinish={vi.fn()}
        />
      </ToastProvider>,
    );

    await user.click(await screen.findByRole("button", { name: /Undo migration/ }));
    expect(await screen.findByText("Confirm undo")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Confirm undo" }));
    await waitFor(() => {
      expect(MOCK_INVOKE).toHaveBeenCalledWith("operation_undo", {
        operationId: "op_mig_42",
        force: false,
      });
    });
    const notifications = screen.getByRole("list", { name: "Notifications" });
    expect(await within(notifications).findByText("Migration was undone. Original files restored.")).toBeTruthy();
  });
  test("shows the backend's undo refusal instead of [object Object]", async () => {
    MOCK_INVOKE.mockImplementation((command: string) => {
      if (command === "operation_undo_check") {
        return Promise.resolve({
          operation_id: "op_mig_42",
          known: true,
          already_undone: false,
          expired: false,
          window_days: 30,
          expires_at_unix_ms: BigInt(Date.now() + 30 * 86400 * 1000),
          conflicted_paths: [],
        });
      }
      if (command === "operation_undo") {
        // Tauri rejects with the serialized CommandError, not an `Error`.
        return Promise.reject({
          code: "conflict",
          message: "These paths cannot be restored safely:\n/sandbox/home/.claude/skills/demo-skill",
          refusal: null,
        });
      }
      return Promise.reject(new Error("unexpected command"));
    });
    const user = userEvent.setup();
    render(
      <ToastProvider>
        <MigrateDoneScreen result={SAMPLE_RESULT} onFinish={vi.fn()} />
      </ToastProvider>,
    );

    await user.click(await screen.findByRole("button", { name: /Undo migration/ }));
    await user.click(await screen.findByRole("button", { name: "Confirm undo" }));

    const alerts = await screen.findAllByRole("alert");
    expect(alerts.map((alert) => alert.textContent ?? "").join("\n")).toContain(
      "/sandbox/home/.claude/skills/demo-skill",
    );
    // A Tauri `CommandError` carries `{code, message}`; the screen must show that message
    // rather than `String(cause)`.
    expect(document.body.textContent ?? "").toContain("These paths cannot be restored safely");
    expect(document.body.textContent ?? "").not.toContain("[object Object]");
  });

  test("renders rolled-back state when failedStep is present", () => {
    render(
      <ToastProvider>
        <MigrateDoneScreen
          result={null}
          failedStep="createstoreitem"
          failureCause="Permission denied"
          onRetry={vi.fn()}
          onFinish={vi.fn()}
        />
      </ToastProvider>,
    );

    const summary = screen.getByRole("region", { name: "Migration summary" });
    expect(within(summary).getByText("Migration rolled back")).toBeTruthy();
    expect(within(summary).getByText(/createstoreitem/)).toBeTruthy();
    expect(within(summary).getByText("Permission denied")).toBeTruthy();
    const notifications = screen.getByRole("list", { name: "Notifications" });
    expect(within(notifications).getByText("Migration rolled back")).toBeTruthy();
    expect(within(notifications).getByText("Migration rolled back at createstoreitem. All original files remain in place.")).toBeTruthy();
    expect(within(notifications).getByRole("button", { name: /Dismiss notification:/ })).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Undo migration/ })).toBeNull();
  });
});
