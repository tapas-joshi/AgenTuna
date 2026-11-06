import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { OverwriteRefusalDialog } from "./OverwriteRefusalDialog";
import type { RefusedOverwriteDetails } from "./OverwriteRefusalDialog";
import { refusedOverwriteDetails } from "./OverwriteRefusalDialog";

afterEach(cleanup);
const refusal: RefusedOverwriteDetails = {
  report: {
    code: "E_REFUSED_OVERWRITE",
    class: "conflict",
    message: "target exists and is not managed",
    userMessage: "AgenTuna refused to overwrite this path.",
    path: "/tmp/unmanaged-folder/" + "a".repeat(280),
    sourceChain: [],
    remedies: ["adopt", "move_aside", "cancel"],
    retryable: false,
  },
  kind: "directory",
  existingHash: "sha256:fixture",
};
function renderDialog(overrides: Partial<React.ComponentProps<typeof OverwriteRefusalDialog>> = {}) {
  const props = {
    refusal,
    onMoveAside: vi.fn().mockResolvedValue({ summary: "Moved aside", movedTo: "/tmp/support/.moved-aside/item" }),
    onUndo: vi.fn().mockResolvedValue(undefined),
    onClose: vi.fn(),
    ...overrides,
  };
  render(<OverwriteRefusalDialog {...props} />);
  return props;
}

describe("OverwriteRefusalDialog", () => {
  it("converts only the typed RefusedOverwrite command error", () => {
    expect(refusedOverwriteDetails({
      code: "refused_overwrite",
      message: "Refused an unmanaged target.",
      refusal: { path: "/sandbox/native/skill", kind: "directory", existing_hash: "sha256:abc", remedies: ["adopt", "move_aside", "cancel"] },
    })).toEqual({
      report: { code: "E_REFUSED_OVERWRITE", class: "conflict", message: "Refused an unmanaged target.", path: "/sandbox/native/skill", sourceChain: [], remedies: ["adopt", "move_aside", "cancel"], retryable: false },
      kind: "directory",
      existingHash: "sha256:abc",
    });
    expect(refusedOverwriteDetails({ code: "Internal", message: "Other failure" })).toBeNull();
  });
  it("shows the refusal, preserved path, kind, hash and only the three remedies", () => {
    renderDialog();
    expect(screen.getByRole("dialog", { name: "AgenTuna won't overwrite this item" })).toBeTruthy();
    expect(screen.getByText(refusal.report.path!)).toBeTruthy();
    expect(screen.getAllByText(/unmanaged directory/)).toHaveLength(2);
    expect(screen.getByText("sha256:fixture")).toBeTruthy();
    expect(screen.getAllByRole("button").map((button) => button.textContent)).toEqual(["Copy path", "Adopt", "Move aside", "Cancel"]);
  });

  it("keeps Adopt disabled with an honest TC-011 reason", async () => {
    const user = userEvent.setup();
    const props = renderDialog();
    const adopt = screen.getByRole("button", { name: "Adopt" });
    expect(adopt.hasAttribute("disabled")).toBe(true);
    expect(screen.getByText(/TC-011/)).toBeTruthy();
    await user.click(adopt);
    expect(props.onMoveAside).not.toHaveBeenCalled();
  });
  it("moves aside and displays the destination without claiming an undo operation", async () => {
    const user = userEvent.setup();
    const props = renderDialog();
    await user.click(screen.getByRole("button", { name: "Move aside" }));
    expect(await screen.findByText(/Moved to/)).toBeTruthy();
    expect(screen.getByText("/tmp/support/.moved-aside/item")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Undo" })).toBeNull();
    expect(props.onUndo).not.toHaveBeenCalled();
  });


  it("leaves the refusal unchanged when cancelled or escaped", async () => {
    const user = userEvent.setup();
    const props = renderDialog();
    await user.keyboard("{Escape}");
    expect(props.onClose).toHaveBeenCalledOnce();
    expect(props.onMoveAside).not.toHaveBeenCalled();
  });

  it("shows a typed command failure with Retry", async () => {
    const user = userEvent.setup();
    renderDialog({ onMoveAside: vi.fn().mockRejectedValueOnce(new Error("stale refusal")) });
    await user.click(screen.getByRole("button", { name: "Move aside" }));
    expect((await screen.findByRole("alert")).textContent).toContain("stale refusal");
    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(screen.getByRole("button", { name: "Adopt" })).toBeTruthy();
  });
});
