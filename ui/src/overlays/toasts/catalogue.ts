import type { ToastInput } from "./types";

/** Minimal toast-service port used by the event and workflow copy catalogue. */
export interface ToastSink {
  /** Adds one toast to the root service and returns its stable id. */
  show(input: ToastInput): string;
}

/** Shows the native-item warning with its direct adoption action. */
export function showAdoptFirstToast(toast: ToastSink, onAdopt: () => void): string {
  return toast.show({
    kind: "warning",
    title: "Adopt first",
    message: "Adopt this unmanaged native item into AgenTuna before enabling it.",
    action: { label: "Adopt", run: onAdopt },
  });
}

/** Shows the successful migration count and a direct Undo action. */
export function showMigrationCompleteToast(
  toast: ToastSink,
  movedCount: number,
  onUndo: () => void,
): string {
  const noun = movedCount === 1 ? "item" : "items";
  return toast.show({
    kind: "success",
    title: "Migration complete",
    message: `${movedCount} ${noun} moved to AgenTuna.`,
    action: { label: "Undo", run: onUndo },
  });
}

/** Shows a persistent error after a migration rollback. */
export function showMigrationRolledBackToast(
  toast: ToastSink,
  failedStep: string,
  details?: string,
): string {
  return toast.show({
    kind: "error",
    title: "Migration rolled back",
    message: `Migration rolled back at ${failedStep}. All original files remain in place.`,
    details,
  });
}

/** Shows the successful undo confirmation. */
export function showUndoCompleteToast(toast: ToastSink): string {
  return toast.show({
    kind: "success",
    title: "Undo complete",
    message: "Migration was undone. Original files restored.",
  });
}

/** Shows a persistent blocked-undo error with a path back to review. */
export function showUndoBlockedToast(
  toast: ToastSink,
  details: string,
  onReview: () => void,
): string {
  return toast.show({
    kind: "error",
    title: "Undo blocked",
    message: "Files changed after migration. Review the changes before undoing.",
    details,
    action: { label: "Review changes", run: onReview },
  });
}

/** Shows one detected drift item with its Review route action. */
export function showDriftDetectedToast(
  toast: ToastSink,
  itemName: string,
  onReview: () => void,
): string {
  return toast.show({
    kind: "warning",
    title: "Drift detected",
    message: `${itemName} changed outside AgenTuna.`,
    action: { label: "Review", run: onReview },
  });
}

/** Shows the successful local backup restore. */
export function showBackupRestoredToast(toast: ToastSink, path: string): string {
  return toast.show({
    kind: "success",
    title: "Restore complete",
    message: `Restored ${path} from backup.`,
  });
}

/** Shows the successful remote backup job. */
export function showRemoteBackupCompleteToast(toast: ToastSink): string {
  return toast.show({
    kind: "success",
    title: "Remote backup complete",
    message: "AgenTuna backed up the store to the remote repository.",
  });
}

/** Shows a persistent remote-conflict error with a route to the conflict list. */
export function showRemoteConflictToast(
  toast: ToastSink,
  details: string,
  onReview: () => void,
): string {
  return toast.show({
    kind: "error",
    title: "Remote conflict found",
    message: "Remote backup changes conflict with local files. Review the conflict before continuing.",
    details,
    action: { label: "Review conflict", run: onReview },
  });
}
