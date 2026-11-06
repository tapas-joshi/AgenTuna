import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ToastProvider } from "../../overlays/toasts";
import type { MigrateApplyResultView } from "../../generated/MigrateApplyResultView";
import { MigrateDoneScreen } from "./MigrateDoneScreen";

const MOCK_INVOKE = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => MOCK_INVOKE(...args),
}));

const SAMPLE_RESULT: MigrateApplyResultView = {
  job_id: "job_1",
  operation_id: "op_mig_42",
  completed: 5n,
  touched_paths: ["/home/.agentuna/skills/demo"],
  warnings: [],
  backup_dir: "/home/.agentuna/backups/migration",
  scope: "user",
  strategy: "replace_with_links",
  plan_hash: "a".repeat(64),
  counts: {
    items: 3,
    files_created: 5,
    files_backed_up: 3,
    links_written: 2,
    files_left_in_place: 0,
  },
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

  test("renders applied summary counts and undo button with expiry", async () => {
    render(
      <ToastProvider>
        <MigrateDoneScreen
          result={SAMPLE_RESULT}
          onFinish={vi.fn()}
        />
      </ToastProvider>,
    );

    expect(screen.getByText("3 items now live in AgenTuna")).toBeTruthy();
    expect(screen.getByText(/3 items copied into store/)).toBeTruthy();
    expect(screen.getByText(/2 skill folders replaced by links/)).toBeTruthy();
    expect(screen.getByText(/5 files created or rewritten/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /Undo migration/ })).toBeTruthy();
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
    expect(screen.getByText("Confirm undo")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Confirm undo" }));
    await waitFor(() => {
      expect(MOCK_INVOKE).toHaveBeenCalledWith("operation_undo", {
        operation_id: "op_mig_42",
        force: false,
      });
    });
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

    expect(screen.getByText("Migration rolled back")).toBeTruthy();
    expect(screen.getByText(/createstoreitem/)).toBeTruthy();
    expect(screen.getByText("Permission denied")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Undo migration/ })).toBeNull();
  });
});
