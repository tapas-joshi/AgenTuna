import { useEffect, useState } from "react";
import type { JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "../../components/button";
import type { MigrateApplyResultView } from "../../generated/MigrateApplyResultView";
import type { OperationUndoCheckView } from "../../generated/OperationUndoCheckView";
import type { OperationUndoView } from "../../generated/OperationUndoView";
import { Modal } from "../../overlays/dialogs/Modal";
import { useToast } from "../../overlays/toasts";

export interface MigrateDoneScreenProps {
  readonly result: MigrateApplyResultView | null;
  readonly failedStep?: string | null;
  readonly failureCause?: string | null;
  readonly onRetry?: () => void;
  readonly onFinish: () => void;
  readonly onViewMcp?: () => void;
}

/**
 * Migration Step 4: Done, undo and failure recovery (TC-024, 12-migrate-done design).
 */
export function MigrateDoneScreen({
  result,
  failedStep,
  failureCause,
  onRetry,
  onFinish,
  onViewMcp,
}: MigrateDoneScreenProps): JSX.Element {
  const toast = useToast();
  const [readiness, setReadiness] = useState<OperationUndoCheckView | null>(null);
  const [undoModalOpen, setUndoModalOpen] = useState(false);
  const [undoBusy, setUndoBusy] = useState(false);
  const [undoError, setUndoError] = useState<string | null>(null);
  const [blockedPaths, setBlockedPaths] = useState<readonly string[]>([]);
  const [undone, setUndone] = useState(false);

  useEffect(() => {
    if (!result?.operation_id) return;
    invoke<OperationUndoCheckView>("operation_undo_check", {
      operation_id: result.operation_id,
    })
      .then((view) => {
        setReadiness(view);
      })
      .catch(() => {
        // Readiness check failed or operation not found
      });
  }, [result?.operation_id]);

  async function openUndoModal() {
    setUndoError(null);
    setBlockedPaths([]);
    if (result?.operation_id) {
      try {
        const view = await invoke<OperationUndoCheckView>("operation_undo_check", {
          operation_id: result.operation_id,
        });
        setReadiness(view);
        if (view.conflicted_paths.length > 0) {
          setBlockedPaths(view.conflicted_paths);
        }
      } catch {
        // ignore
      }
    }
    setUndoModalOpen(true);
  }
  async function handleUndo(force = false) {
    if (!result?.operation_id) return;
    setUndoBusy(true);
    setUndoError(null);
    try {
      const outcome = await invoke<OperationUndoView>("operation_undo", {
        operation_id: result.operation_id,
        force,
      });
      toast.success(outcome.summary || "Migration was undone.");
      setUndone(true);
      setUndoModalOpen(false);
      setBlockedPaths([]);
    } catch (err: unknown) {
      const msg = String(err);
      setUndoError(msg);
      if (readiness && readiness.conflicted_paths.length > 0) {
        setBlockedPaths(readiness.conflicted_paths);
      } else {
        const lines = msg.split("\n").filter((l) => l.trim().startsWith("/") || l.trim().startsWith("."));
        if (lines.length > 0) {
          setBlockedPaths(lines);
        }
      }
    } finally {
      setUndoBusy(false);
    }
  }

  // Calculate expiry date label
  const expiryLabel = (() => {
    if (readiness?.expires_at_unix_ms) {
      const date = new Date(Number(readiness.expires_at_unix_ms));
      return `available until ${date.toLocaleDateString()}`;
    }
    return "available for 30 days";
  })();

  const isExpired = readiness?.expired ?? false;
  const isRolledBack = result === null && Boolean(failedStep);

  return (
    <main
      aria-label="Migration summary"
      className="mx-auto flex min-h-full max-w-[800px] flex-col items-center justify-center gap-24 p-24"
    >
      {isRolledBack ? (
        <section
          role="alert"
          className="flex w-full flex-col items-center gap-16 rounded-12 border border-danger/40 bg-danger/10 p-24 text-center"
        >
          <div
            aria-hidden="true"
            className="grid h-48 w-48 place-items-center rounded-full bg-danger/20 text-24 text-danger font-bold"
          >
            ✕
          </div>
          <h1 className="m-0 text-22 font-semibold text-text">Migration rolled back</h1>
          <p className="m-0 text-13 text-danger">
            Failed at step: <span className="font-mono font-medium">{failedStep}</span>
          </p>
          {failureCause && (
            <p className="m-0 text-12 text-text-secondary">{failureCause}</p>
          )}
          <p className="m-0 text-12 text-text-muted">
            0 steps completed. All originals remain in place without modification.
          </p>
          {onRetry && (
            <Button variant="primary" onClick={onRetry}>
              Retry migration
            </Button>
          )}
        </section>
      ) : result !== null ? (
        <>
          <div
            aria-hidden="true"
            className="grid h-64 w-64 place-items-center rounded-full bg-success/20 text-32 text-success"
          >
            ✓
          </div>
          <div className="text-center">
            <h1 className="m-0 text-26 font-semibold text-text">
              {result.counts.items} item{result.counts.items === 1 ? "" : "s"} now live in AgenTuna
            </h1>
            <p className="m-0 mt-8 text-13 text-text-secondary">
              Agents load the same things as before. From now on, change them here.
            </p>
          </div>

          <section
            aria-label="Applied summary"
            className="flex w-full flex-col gap-12 rounded-12 border border-border bg-raised p-20"
          >
            <ul className="m-0 flex list-none flex-col gap-8 p-0 text-13 text-text">
              <li className="flex items-center gap-8">
                <span className="text-success">✓</span>
                <span>
                  {result.counts.items} items copied into store ({result.scope} scope)
                </span>
              </li>
              <li className="flex items-center gap-8">
                <span className="text-success">✓</span>
                <span>{result.counts.links_written} skill folders replaced by links</span>
              </li>
              <li className="flex items-center gap-8">
                <span className="text-success">✓</span>
                <span>{result.counts.files_created} files created or rewritten in total</span>
              </li>
              <li className="flex items-center gap-8">
                <span className="text-success">✓</span>
                <span>
                  {result.counts.files_backed_up} originals backed up in {result.backup_dir} · undo{" "}
                  {expiryLabel}
                </span>
              </li>
              {result.counts.files_left_in_place > 0 && (
                <li className="flex items-center gap-8">
                  <span className="text-text-muted">•</span>
                  <span>{result.counts.files_left_in_place} files left in place and watched</span>
                </li>
              )}
            </ul>
          </section>

          {isExpired && (
            <p role="status" className="m-0 text-12 text-text-muted">
              The 30-day undo window has closed. Backups are preserved in {result.backup_dir}.
            </p>
          )}

          {undone && (
            <p role="status" className="m-0 text-13 text-success font-medium">
              Migration was successfully undone. Original files restored.
            </p>
          )}

          <footer className="mt-8 flex items-center justify-between gap-16 w-full border-t border-border pt-16">
            <div>
              {!isExpired && !undone && (
                <Button
                  variant="ghost"
                  onClick={() => void openUndoModal()}
                >
                  Undo migration ({expiryLabel})
                </Button>
              )}
            </div>
            <div className="flex items-center gap-8">
              {onViewMcp && (
                <Button variant="secondary" onClick={onViewMcp}>
                  View MCP Servers
                </Button>
              )}
              <Button variant="primary" onClick={onFinish}>
                Back to Skills
              </Button>
            </div>
          </footer>
        </>
      ) : null}

      {undoModalOpen && (
        <Modal
          open={true}
          onOpenChange={(open) => {
            if (!open) setUndoModalOpen(false);
          }}
          title="Undo migration"
          description="Restore native files from the backup created before migration."
          footer={
            <div className="flex items-center justify-end gap-8">
              <Button variant="ghost" disabled={undoBusy} onClick={() => setUndoModalOpen(false)}>
                Cancel
              </Button>
              {blockedPaths.length > 0 ? (
                <Button
                  variant="destructive"
                  disabled={undoBusy}
                  onClick={() => void handleUndo(true)}
                >
                  {undoBusy ? "Restoring…" : "Force undo and overwrite changes"}
                </Button>
              ) : (
                <Button
                  variant="primary"
                  disabled={undoBusy}
                  onClick={() => void handleUndo(false)}
                >
                  {undoBusy ? "Undoing…" : "Confirm undo"}
                </Button>
              )}
            </div>
          }
        >
          <div className="flex flex-col gap-12 py-8">
            {blockedPaths.length > 0 ? (
              <div role="alert" className="flex flex-col gap-8 rounded-8 border border-warning/40 bg-warning/10 p-12">
                <p className="m-0 text-13 font-semibold text-warning">
                  Undo blocked: the following files changed since migration:
                </p>
                <ul className="m-0 flex max-h-[160px] list-disc flex-col gap-2 overflow-auto pl-16 font-mono text-12 text-text">
                  {blockedPaths.map((p) => (
                    <li key={p}>{p}</li>
                  ))}
                </ul>
                <p className="m-0 text-12 text-text-secondary">
                  Forcing undo will discard newer edits and restore the pre-migration backup.
                </p>
              </div>
            ) : (
              <p className="m-0 text-13 text-text">
                All files moved or linked during migration will be restored to their original locations from{" "}
                <span className="font-mono">{result?.backup_dir}</span>.
              </p>
            )}
            {undoError && blockedPaths.length === 0 && (
              <p role="alert" className="m-0 text-12 text-danger">
                {undoError}
              </p>
            )}
          </div>
        </Modal>
      )}
    </main>
  );
}
