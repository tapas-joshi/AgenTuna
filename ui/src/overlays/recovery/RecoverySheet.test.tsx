import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { RecoverySheetView } from "../../generated/RecoverySheetView";
import { RecoveryGate, RecoverySheet, recoveryNeedsReview } from "./RecoverySheet";

afterEach(cleanup);

const recovery: RecoverySheetView = {
  operations: [{ id: "op-1", kind: "write", paths: ["/sandbox/skills/demo/SKILL.md"] }],
  files: [{ operation_id: "op-1", path: "/sandbox/skills/demo/SKILL.md", state: "after", can_restore: true, before_hash: "abc" }],
  manual_review: ["/sandbox/skills/other: file changed externally"],
  temp_files: ["/sandbox/.pending.agentuna-tmp-1"],
};

describe("RecoverySheet", () => {
  it("shows interrupted paths, manual review, temp files and invokes recovery actions", () => {
    const onDiscard = vi.fn();
    const onRestore = vi.fn();
    const onContinue = vi.fn();
    render(<RecoverySheet recovery={recovery} onDiscard={onDiscard} onRestore={onRestore} onContinue={onContinue} />);
    expect(screen.getByRole("dialog", { name: "Recover interrupted changes" })).toBeTruthy();
    // The path is listed under the operation and again as an affected file.
    expect(screen.getAllByText("/sandbox/skills/demo/SKILL.md").length).toBeGreaterThan(0);
    expect(screen.getByText(/file changed externally/)).toBeTruthy();
    expect(screen.getByText("/sandbox/.pending.agentuna-tmp-1")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Restore" }));
    expect(onRestore).toHaveBeenCalledWith(recovery.files[0]);
    fireEvent.click(screen.getByRole("button", { name: "Discard pending" }));
    expect(onDiscard).toHaveBeenCalledWith("op-1");
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(onContinue).toHaveBeenCalledOnce();
  });

  it("shows a clean empty state", () => {
    render(<RecoverySheet recovery={{ operations: [], files: [], manual_review: [], temp_files: [] }} onDiscard={vi.fn()} onRestore={vi.fn()} onContinue={vi.fn()} />);
    expect(screen.getByText("No pending file changes need review.")).toBeTruthy();
  });
});

const emptyView: RecoverySheetView = { operations: [], files: [], manual_review: [], temp_files: [] };

describe("recoveryNeedsReview", () => {
  it("is false for the empty view an unclean exit produces", () => {
    expect(recoveryNeedsReview(emptyView)).toBe(false);
  });

  it("is true when any section lists something", () => {
    expect(recoveryNeedsReview({ ...emptyView, operations: recovery.operations })).toBe(true);
    expect(recoveryNeedsReview({ ...emptyView, files: recovery.files })).toBe(true);
    expect(recoveryNeedsReview({ ...emptyView, manual_review: recovery.manual_review })).toBe(true);
    expect(recoveryNeedsReview({ ...emptyView, temp_files: recovery.temp_files })).toBe(true);
  });
});

describe("RecoveryGate", () => {
  const props = { onContinue: vi.fn(), onDiscard: vi.fn(), onRestore: vi.fn() };

  it("renders nothing for a clean exit", () => {
    const { container } = render(<RecoveryGate view={null} {...props} />);
    expect(container.innerHTML).toBe("");
  });

  it("does not gate the app when the sheet has nothing to review", () => {
    // Regression: an empty view used to render a full-window sheet, covering the app and
    // swallowing every pointer event until Continue was pressed.
    const { container } = render(<RecoveryGate view={emptyView} {...props} />);
    expect(container.innerHTML).toBe("");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("renders the sheet when something needs review", () => {
    render(<RecoveryGate view={recovery} {...props} />);
    expect(screen.getByRole("dialog", { name: "Recover interrupted changes" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(props.onContinue).toHaveBeenCalledOnce();
  });
});
