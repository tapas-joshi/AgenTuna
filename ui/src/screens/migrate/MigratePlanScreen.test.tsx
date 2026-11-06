import { afterEach, describe, expect, test, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { MigratePlanView } from "../../generated/MigratePlanView";
import { MigratePlanScreen } from "./MigratePlanScreen";

const SAMPLE_PLAN: MigratePlanView = {
  plan_id: "plan_42",
  plan_hash: "a".repeat(64),
  scope: "user",
  strategy: "replace_with_links",
  counts: {
    items: 2,
    files_created: 4,
    files_backed_up: 2,
    links_written: 2,
    files_left_in_place: 0,
  },
  steps: [
    {
      seq: 1,
      action: "backuporiginal",
      item_name: "test-skill",
      source: "/home/.agents/skills/test-skill",
      destination: null,
      backup_path: "/home/.agentuna/backups/migration/test-skill",
      bytes: 120n,
      agents: ["claude_code"],
    },
    {
      seq: 2,
      action: "createstoreitem",
      item_name: "test-skill",
      source: "/home/.agents/skills/test-skill",
      destination: "/home/.agentuna/skills/test-skill",
      backup_path: null,
      bytes: 120n,
      agents: ["claude_code"],
    },
  ],
  files_checked: 2,
};

describe("MigratePlanScreen", () => {
  afterEach(() => {
    cleanup();
  });

  test("renders strategy cards, counts, and opens confirmation modal", async () => {
    const user = userEvent.setup();
    const onApply = vi.fn().mockResolvedValue(undefined);

    render(
      <MigratePlanScreen
        plan={SAMPLE_PLAN}
        strategy="replace_with_links"
        busy={false}
        error={null}
        onStrategyChange={vi.fn()}
        onApply={onApply}
        onBack={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByText("2 items copied into the store")).toBeTruthy();
    expect(screen.getByText("2 skill folders replaced by links")).toBeTruthy();

    // Toggle dry run file list
    await user.click(screen.getByRole("button", { name: "Show file list" }));
    expect(screen.getByText(/1\. Back up original/)).toBeTruthy();
    expect(screen.getByText(/2\. Copy into store/)).toBeTruthy();

    // Click Apply
    await user.click(screen.getByRole("button", { name: "Apply 2 items" }));
    expect(screen.getByText("Confirm migration")).toBeTruthy();

    // Confirm button is disabled until checkbox is checked
    const confirmBtn = screen.getByRole("button", { name: "Apply migration" });
    expect(confirmBtn.getAttribute("disabled")).not.toBeNull();

    // Check confirmation checkbox
    const checkbox = screen.getByRole("checkbox");
    await user.click(checkbox);
    expect(confirmBtn.getAttribute("disabled")).toBeNull();

    // Click confirm
    await user.click(confirmBtn);
    expect(onApply).toHaveBeenCalledTimes(1);
  });
});
