import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { BulkSkillTransferDialog, SkillActionDialogs } from "./SkillActionDialogs";

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

const transferOptions = {
  item_id: "itm_code_review",
  name: "Code Review",
  source_scope: "user",
  source_path: "/sandbox/user/skills/code-review",
  targets: [
    { scope: "global", path: "/sandbox/global", available: true, reason: null },
    { scope: "user", path: "/sandbox/user", available: true, reason: null },
  ],
};

function collisionPreview(resolution: string | null, transfer: string | undefined) {
  const collision = resolution === null && transfer !== "duplicate";
  return {
    preview_id: collision ? null : "preview-transfer-ready",
    name: "Code Review",
    source_scope: "user",
    target_scope: transfer === "duplicate" ? "user" : "global",
    source_path: "/sandbox/user/skills/code-review",
    target_path: transfer === "duplicate" ? "/sandbox/user/skills/code-review-copy" : "/sandbox/global/skills/code-review",
    source_hash: "0".repeat(64),
    target_hash: collision ? "1".repeat(64) : null,
    removes_source_after_backup: transfer === "move",
    backup_location: "/sandbox/support/backups/skills",
    overwrite: collision
      ? { source_hash: "0".repeat(64), target_hash: "1".repeat(64), files: [{ path: "SKILL.md", change: "changed" }] }
      : null,
    files: collision ? [] : ["SKILL.md"],
  };
}

beforeEach(() => {
  ipc.calls.length = 0;
  ipc.reply = (command, args) => {
    if (command === "skill_transfer_options") return Promise.resolve(transferOptions);
    if (command === "skill_transfer_preview") {
      const request = (args as { request?: { resolution?: string | null; transfer?: string } } | undefined)?.request;
      return Promise.resolve(collisionPreview(request?.resolution ?? null, request?.transfer));
    }
    if (command === "skill_transfer_apply") return Promise.resolve({ operation_id: "op-transfer", backup_location: "/sandbox/support/backups/skills", target_path: "/sandbox/global/skills/code-review" });
    if (command === "skill_delete_preview") return Promise.resolve({ preview_id: "preview-delete", path: "/sandbox/user/skills/code-review", files: ["SKILL.md"], file_count: 1n, name: "Code Review", scope: "user", next_effective: null, deployments: ["/sandbox/codex/skills/code-review"] });
    if (command === "skill_delete_apply") return Promise.resolve({ operation_id: "op-delete", backup_folder: "/sandbox/support/backups/skills/code-review" });
    if (command === "skill_disable_in_repo_preview") return Promise.resolve({ preview_id: "preview-disable", name: "Code Review", scope: "user", override_path: "/sandbox/project/.config-studio/disabled-skills.json", links: ["/sandbox/project/.agents/skills/code-review"], agents: ["Codex", "omp"], backup_location: "/sandbox/support/backups/skills" });
    if (command === "skill_disable_in_repo_apply") return Promise.resolve({ operation_id: "op-disable", backup_location: "/sandbox/support/backups/skills", agents_disabled: 2n });
    if (command === "skill_transfer_bulk_preview") return Promise.resolve({
      preview_id: null,
      target_scope: "global",
      backup_location: "/sandbox/support/backups/skills",
      items: [
        { item_id: "itm_code_review", name: "Code Review", reason: null },
        { item_id: "itm_release_notes", name: "Release Notes", reason: "The destination has a native conflict." },
      ],
    });
    return Promise.resolve(undefined);
  };
});

afterEach(cleanup);

describe("Skill action dialogs", () => {
  test("compares source and target hashes and applies Replace only after a reviewed preview", async () => {
    const user = userEvent.setup();
    const onSuccess = vi.fn();
    render(
      <SkillActionDialogs
        action={{ kind: "transfer", itemId: "itm_code_review", transfer: "promote" }}
        projectId="prj_test"
        onClose={vi.fn()}
        onTransferSuccess={onSuccess}
        onDeleteSuccess={vi.fn()}
        onDisableSuccess={vi.fn()}
      />,
    );

    await user.click(await screen.findByRole("button", { name: "Review transfer" }));
    const warning = await screen.findByRole("region", { name: "Destination overwrite warning" });
    expect(warning.textContent).toContain(`Source SHA-256: ${"0".repeat(64)}`);
    expect(warning.textContent).toContain(`Target SHA-256: ${"1".repeat(64)}`);
    expect(warning.textContent).toContain("Replace backs up the current folder before writing.");

    await user.click(screen.getByRole("button", { name: "Replace with backup" }));
    const previewCall = ipc.calls.filter((call) => call.command === "skill_transfer_preview").at(-1);
    expect(previewCall?.args).toEqual({
      request: { item_id: "itm_code_review", target_scope: "global", transfer: "promote", resolution: "replace", new_slug: null, project_id: "prj_test" },
    });
    await user.click(await screen.findByRole("button", { name: "Promote skill" }));
    expect(ipc.calls.at(-1)).toEqual({ command: "skill_transfer_apply", args: { request: { preview_id: "preview-transfer-ready" } } });
    expect(onSuccess).toHaveBeenCalledWith({ operation_id: "op-transfer", backup_location: "/sandbox/support/backups/skills", target_path: "/sandbox/global/skills/code-review" });
  });

  test("duplicates to a new slug within the source scope", async () => {
    const user = userEvent.setup();
    const onSuccess = vi.fn();
    render(
      <SkillActionDialogs
        action={{ kind: "transfer", itemId: "itm_code_review", transfer: "duplicate" }}
        projectId="prj_test"
        onClose={vi.fn()}
        onTransferSuccess={onSuccess}
        onDeleteSuccess={vi.fn()}
        onDisableSuccess={vi.fn()}
      />,
    );

    await user.type(await screen.findByRole("textbox", { name: "New skill slug" }), "code-review-copy");
    await user.click(screen.getByRole("button", { name: "Review duplicate" }));
    const previewCall = ipc.calls.filter((call) => call.command === "skill_transfer_preview").at(-1);
    expect(previewCall?.args).toEqual({
      request: { item_id: "itm_code_review", target_scope: "user", transfer: "duplicate", resolution: null, new_slug: "code-review-copy", project_id: "prj_test" },
    });
    await user.click(await screen.findByRole("button", { name: "Duplicate skill" }));
    expect(ipc.calls.at(-1)).toEqual({ command: "skill_transfer_apply", args: { request: { preview_id: "preview-transfer-ready" } } });
    expect(onSuccess).toHaveBeenCalled();
  });

  test("Rename requires a destination slug and sends the reviewed collision decision", async () => {
    const user = userEvent.setup();
    render(
      <SkillActionDialogs
        action={{ kind: "transfer", itemId: "itm_code_review", transfer: "move" }}
        projectId="prj_test"
        onClose={vi.fn()}
        onTransferSuccess={vi.fn()}
        onDeleteSuccess={vi.fn()}
        onDisableSuccess={vi.fn()}
      />,
    );

    await user.click(await screen.findByRole("button", { name: "Review transfer" }));
    const rename = screen.getByRole("button", { name: "Rename destination" }) as HTMLButtonElement;
    expect(rename.disabled).toBe(true);
    await user.type(screen.getByRole("textbox", { name: "New destination slug" }), "code-review-v2");
    await user.click(screen.getByRole("button", { name: "Rename destination" }));
    const previewCall = ipc.calls.filter((call) => call.command === "skill_transfer_preview").at(-1);
    expect(previewCall?.args).toEqual({
      request: { item_id: "itm_code_review", target_scope: "global", transfer: "move", resolution: "rename", new_slug: "code-review-v2", project_id: "prj_test" },
    });
    const review = await screen.findByRole("region", { name: "Reviewed transfer" });
    expect(review.textContent).toContain("Move removes the source folder only after it has been backed up.");
  });

  test("requires the exact skill name before sending the delete apply command", async () => {
    const user = userEvent.setup();
    const onSuccess = vi.fn();
    render(
      <SkillActionDialogs
        action={{ kind: "delete", itemId: "itm_code_review" }}
        projectId="prj_test"
        onClose={vi.fn()}
        onTransferSuccess={vi.fn()}
        onDeleteSuccess={onSuccess}
        onDisableSuccess={vi.fn()}
      />,
    );

    const apply = await screen.findByRole("button", { name: "Delete skill" }) as HTMLButtonElement;
    expect(apply.disabled).toBe(true);
    const confirmation = screen.getByRole("textbox", { name: "Type skill name to confirm deletion" });
    await user.type(confirmation, "not the name");
    expect(apply.disabled).toBe(true);
    expect(ipc.calls.some((call) => call.command === "skill_delete_apply")).toBe(false);

    await user.clear(confirmation);
    await user.type(confirmation, "Code Review");
    await user.click(apply);
    expect(ipc.calls.at(-1)).toEqual({ command: "skill_delete_apply", args: { request: { preview_id: "preview-delete", confirmed_name: "Code Review" } } });
    await waitFor(() => expect(onSuccess).toHaveBeenCalledWith({ operation_id: "op-delete", backup_folder: "/sandbox/support/backups/skills/code-review" }));
  });

  test("previews and applies the project-only disable with its backup note", async () => {
    const user = userEvent.setup();
    const onSuccess = vi.fn();
    render(
      <SkillActionDialogs
        action={{ kind: "disable-in-repo", itemId: "itm_code_review" }}
        projectId="prj_test"
        onClose={vi.fn()}
        onTransferSuccess={vi.fn()}
        onDeleteSuccess={vi.fn()}
        onDisableSuccess={onSuccess}
      />,
    );

    expect(await screen.findByText(/Backup location: \/sandbox\/support\/backups\/skills/)).toBeTruthy();
    expect(screen.getByText("Codex, omp")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Disable in repo" }));
    expect(ipc.calls.at(-1)).toEqual({ command: "skill_disable_in_repo_apply", args: { request: { preview_id: "preview-disable", confirmed: true } } });
    await waitFor(() => expect(onSuccess).toHaveBeenCalledWith({ operation_id: "op-disable", backup_location: "/sandbox/support/backups/skills", agents_disabled: 2n }));
  });

  test("blocks a bulk transfer when any selected skill fails preflight", async () => {
    const user = userEvent.setup();
    render(
      <BulkSkillTransferDialog
        itemIds={["itm_code_review", "itm_release_notes"]}
        names={["Code Review", "Release Notes"]}
        blockedItems={[]}
        transfer="move"
        projectId="prj_test"
        onClose={vi.fn()}
        onSuccess={vi.fn()}
      />,
    );

    await user.click(await screen.findByRole("button", { name: "Review batch" }));
    expect(await screen.findByText("No files were changed. Resolve the blocked skills before applying.")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Apply 2 skills" }) as HTMLButtonElement).disabled).toBe(true);
    const previewCall = ipc.calls.find((call) => call.command === "skill_transfer_bulk_preview");
    expect(previewCall?.args).toEqual({
      request: { item_ids: ["itm_code_review", "itm_release_notes"], target_scope: "global", transfer: "move", project_id: "prj_test" },
    });
  });
  test("applies the atomic bulk transfer after every selected skill passes preflight", async () => {
    const user = userEvent.setup();
    const onSuccess = vi.fn();
    ipc.reply = (command, args) => {
      if (command === "skill_transfer_options") return Promise.resolve(transferOptions);
      if (command === "skill_transfer_bulk_preview") {
        const request = (args as { request?: { item_ids?: string[]; target_scope?: string } } | undefined)?.request;
        return Promise.resolve({
          preview_id: "preview-bulk-transfer",
          target_scope: request?.target_scope ?? "global",
          backup_location: "/sandbox/support/backups/skills",
          items: (request?.item_ids ?? []).map((item_id) => ({ item_id, name: "Code Review", reason: null })),
        });
      }
      if (command === "skill_transfer_bulk_apply") {
        return Promise.resolve({ operation_id: "op-bulk", backup_location: "/sandbox/support/backups/skills", applied_count: 2n });
      }
      return Promise.resolve(undefined);
    };
    render(
      <BulkSkillTransferDialog
        itemIds={["itm_code_review", "itm_release_notes"]}
        names={["Code Review", "Release Notes"]}
        blockedItems={[]}
        transfer="move"
        projectId="prj_test"
        onClose={vi.fn()}
        onSuccess={onSuccess}
      />,
    );

    await user.click(await screen.findByRole("button", { name: "Review batch" }));
    expect(await screen.findByText("All 2 skills passed preflight.")).toBeTruthy();
    expect(screen.getByText("Each source folder is removed only after its backup is created.")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Apply 2 skills" }));
    expect(ipc.calls.at(-1)).toEqual({ command: "skill_transfer_bulk_apply", args: { request: { preview_id: "preview-bulk-transfer" } } });
    await waitFor(() => expect(onSuccess).toHaveBeenCalledWith({ operation_id: "op-bulk", backup_location: "/sandbox/support/backups/skills", applied_count: 2n }));
  });
});
