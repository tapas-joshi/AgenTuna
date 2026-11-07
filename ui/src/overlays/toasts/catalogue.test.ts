import { describe, expect, test, vi } from "vitest";
import { createToastStore } from "./store";
import type { ToastStore } from "./store";
import {
  showAdoptFirstToast,
  showBackupRestoredToast,
  showDriftDetectedToast,
  showMigrationCompleteToast,
  showMigrationRolledBackToast,
  showRemoteBackupCompleteToast,
  showRemoteConflictToast,
  showUndoBlockedToast,
  showUndoCompleteToast,
} from "./catalogue";

function onlyToast(store: ToastStore) {
  const toast = store.getSnapshot().visible[0];
  if (toast === undefined) throw new Error("expected one visible toast");
  return toast;
}

describe("workflow toast catalogue", () => {
  test("adopt-first warning calls Adopt", () => {
    const store = createToastStore();
    const adopt = vi.fn();
    showAdoptFirstToast(store, adopt);
    const toast = onlyToast(store);
    expect(toast.kind).toBe("warning");
    expect(toast.title).toBe("Adopt first");
    expect(toast.message).toBe("Adopt this unmanaged native item into AgenTuna before enabling it.");
    expect(toast.action?.label).toBe("Adopt");
    toast.action?.run();
    expect(adopt).toHaveBeenCalledOnce();
  });

  test("migration copy uses moved count and invokes Undo", () => {
    const store = createToastStore();
    const undo = vi.fn();
    showMigrationCompleteToast(store, 3, undo);
    const toast = onlyToast(store);
    expect(toast.title).toBe("Migration complete");
    expect(toast.message).toBe("3 items moved to AgenTuna.");
    expect(toast.action?.label).toBe("Undo");
    toast.action?.run();
    expect(undo).toHaveBeenCalledOnce();
  });

  test("rollback and undo failures are persistent and reviewable", () => {
    const rollbackStore = createToastStore();
    showMigrationRolledBackToast(rollbackStore, "writecopy", "Permission denied");
    expect(onlyToast(rollbackStore)).toMatchObject({
      kind: "error",
      title: "Migration rolled back",
      message: "Migration rolled back at writecopy. All original files remain in place.",
      details: "Permission denied",
    });

    const blockedStore = createToastStore();
    const review = vi.fn();
    showUndoBlockedToast(blockedStore, "/sandbox/skill/SKILL.md changed", review);
    const blocked = onlyToast(blockedStore);
    expect(blocked).toMatchObject({
      kind: "error",
      title: "Undo blocked",
      message: "Files changed after migration. Review the changes before undoing.",
    });
    expect(blocked.action?.label).toBe("Review changes");
    blocked.action?.run();
    expect(review).toHaveBeenCalledOnce();
  });

  test("undo, drift, local restore, remote success and conflict use their catalogue entries", () => {
    const undoStore = createToastStore();
    showUndoCompleteToast(undoStore);
    expect(onlyToast(undoStore)).toMatchObject({ kind: "success", message: "Migration was undone. Original files restored." });

    const driftStore = createToastStore();
    const reviewDrift = vi.fn();
    showDriftDetectedToast(driftStore, "Code review", reviewDrift);
    const drift = onlyToast(driftStore);
    expect(drift.message).toBe("Code review changed outside AgenTuna.");
    expect(drift.action?.label).toBe("Review");
    drift.action?.run();
    expect(reviewDrift).toHaveBeenCalledOnce();

    const localStore = createToastStore();
    showBackupRestoredToast(localStore, "/sandbox/skill/SKILL.md");
    expect(onlyToast(localStore)).toMatchObject({ kind: "success", message: "Restored /sandbox/skill/SKILL.md from backup." });

    const remoteStore = createToastStore();
    showRemoteBackupCompleteToast(remoteStore);
    expect(onlyToast(remoteStore)).toMatchObject({ kind: "success", title: "Remote backup complete" });

    const conflictStore = createToastStore();
    const reviewConflict = vi.fn();
    showRemoteConflictToast(conflictStore, "2 conflicting files", reviewConflict);
    const conflict = onlyToast(conflictStore);
    expect(conflict).toMatchObject({ kind: "error", title: "Remote conflict found" });
    expect(conflict.action?.label).toBe("Review conflict");
    conflict.action?.run();
    expect(reviewConflict).toHaveBeenCalledOnce();
  });
});
