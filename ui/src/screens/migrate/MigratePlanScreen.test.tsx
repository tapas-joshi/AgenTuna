import { afterEach, describe, expect, test, vi } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { MigratePlanView } from "../../generated/MigratePlanView";
import { MigratePlanScreen } from "./MigratePlanScreen";

const SAMPLE_PLAN: MigratePlanView = {
  plan_hash: "a".repeat(64),
  backup_dir: "/sandbox/support/backups/migration",
  strategy: "replace_with_links",
  counts: {
    items: 2,
    files_created: 4,
    files_backed_up: 2,
    links_written: 2,
    files_left_in_place: 0,
  },
  scopes: [
    {
      scope: "user",
      store_path: "/sandbox/home/.agentuna",
      counts: {
        items: 1,
        files_created: 2,
        files_backed_up: 1,
        links_written: 1,
        files_left_in_place: 0,
      },
      steps: [
        {
          seq: 1,
          action: "backuporiginal",
          scope: "user",
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
          scope: "user",
          item_name: "test-skill",
          source: "/home/.agents/skills/test-skill",
          destination: "/home/.agentuna/skills/test-skill",
          backup_path: null,
          bytes: 120n,
          agents: ["claude_code"],
        },
      ],
    },
    {
      scope: "project",
      store_path: "/sandbox/project/.agentuna",
      counts: {
        items: 1,
        files_created: 2,
        files_backed_up: 1,
        links_written: 1,
        files_left_in_place: 0,
      },
      steps: [
        {
          seq: 1,
          action: "renderrule",
          scope: "project",
          item_name: "CLAUDE",
          source: "/sandbox/project/CLAUDE.md",
          destination: "/sandbox/project/.agentuna/rules/claude.md",
          backup_path: null,
          bytes: 64n,
          agents: ["claude_code"],
        },
      ],
    },
  ],
  files_checked: 4,
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
    // Each receiving store prints the root the backend resolved, never a guessed directory.
    expect(screen.getByText("/sandbox/home/.agentuna")).toBeTruthy();
    expect(screen.getByText("/sandbox/project/.agentuna")).toBeTruthy();
    expect(screen.queryByText("./.config-studio/")).toBeNull();

    // Toggle dry run file list
    await user.click(screen.getByRole("button", { name: "Show file list" }));
    expect(screen.getByText(/1\. Back up original/)).toBeTruthy();
    expect(screen.getByText(/2\. Copy into store/)).toBeTruthy();
    expect(screen.getByText(/1\. Rewrite rule document/)).toBeTruthy();

    // Click Apply
    await user.click(screen.getByRole("button", { name: "Apply 2 items" }));
    expect(screen.getByText("Confirm migration")).toBeTruthy();
    expect(screen.getAllByText("/sandbox/support/backups/migration").length).toBeGreaterThan(0);

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

  test("groups destinations by receiving scope with the store path the backend chose", async () => {
    const user = userEvent.setup();
    render(
      <MigratePlanScreen
        plan={SAMPLE_PLAN}
        strategy="replace_with_links"
        busy={false}
        error={null}
        onStrategyChange={vi.fn()}
        onApply={vi.fn().mockResolvedValue(undefined)}
        onBack={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    const userStore = screen.getByRole("listitem", { name: "User store" });
    expect(within(userStore).getByText("/sandbox/home/.agentuna")).toBeTruthy();
    expect(within(userStore).getByText(/1 item/)).toBeTruthy();
    const projectStore = screen.getByRole("listitem", { name: "Project store" });
    expect(within(projectStore).getByText("/sandbox/project/.agentuna")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Show file list" }));
    expect(screen.getByLabelText("User store files")).toBeTruthy();
    expect(screen.getByLabelText("Project store files")).toBeTruthy();
  });

  test("shows the backend refusal when a project-scope item has no project", () => {
    render(
      <MigratePlanScreen
        plan={null}
        strategy="replace_with_links"
        busy={false}
        error="The selected items include project-scope native files, which belong in the project store. Select a project and rebuild the plan."
        onStrategyChange={vi.fn()}
        onApply={vi.fn().mockResolvedValue(undefined)}
        onBack={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByRole("alert").textContent).toContain("Select a project and rebuild the plan");
  });
});
