import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { RecoverySheetView } from "../../generated/RecoverySheetView";
import { RecoverySheet } from "./RecoverySheet";

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
