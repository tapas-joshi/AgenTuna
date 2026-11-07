import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { McpLaunchApprovalView } from "../../generated/McpLaunchApprovalView";
import { McpLaunchConfirmDialog } from "./McpLaunchConfirmDialog";

afterEach(cleanup);

const approval: McpLaunchApprovalView = {
  approval_id: "apr_fixture",
  request_hash: "b".repeat(64),
  transport: "stdio",
  command: "/usr/bin/true",
  args: ["--serve", "--flag"],
  cwd: "/sandbox",
  value_names: ["MODE", "TOKEN"],
  url: null,
  expires_in_ms: 120000n,
};

describe("McpLaunchConfirmDialog", () => {
  it("shows the exact command, names-only values and expiry, and runs only on Run", () => {
    const onRun = vi.fn();
    const onCancel = vi.fn();
    render(<McpLaunchConfirmDialog approval={approval} onRun={onRun} onCancel={onCancel} />);
    const dialog = screen.getByRole("dialog", { name: "Run this connection test?" });
    expect(dialog.textContent).toContain("/usr/bin/true --serve --flag");
    expect(dialog.textContent).toContain("MODE, TOKEN");
    expect(dialog.textContent).toContain("120 seconds");
    expect(onRun).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Run" }));
    expect(onRun).toHaveBeenCalledOnce();
    expect(onCancel).not.toHaveBeenCalled();
  });
});
