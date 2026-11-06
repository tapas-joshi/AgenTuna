/**
 * Modal host (TU-004).
 *
 * Mount it once near the app root next to `ToastProvider`:
 *
 * ```tsx
 * <ModalHost>
 *   <App />
 * </ModalHost>
 * ```
 *
 * It renders the queue's dialogs in open order — the last one is topmost — and
 * exposes the queue through {@link useModals}. Every queued request resolves to
 * a `ModalResult`; nothing in the queue reads shared UI flags. Pending
 * requests are settled as dismissed when the host unmounts, so an awaited
 * promise can never hang after teardown.
 */

import { createContext, useContext, useEffect, useMemo, useSyncExternalStore } from "react";
import type { ReactNode } from "react";
import { Button } from "../../components/button";
import type { ModalEntry, ModalQueue, ModalRenderApi } from "./modal-queue";
import { createModalQueue } from "./modal-queue";
import { Modal } from "./Modal";

const ModalsContext = createContext<ModalQueue | null>(null);

/** Properties of {@link ModalHost}. */
export interface ModalHostProps {
  /** Queue to render; a private one is created when omitted. */
  readonly queue?: ModalQueue;
  /** Rendered under the provider so descendants can call {@link useModals}. */
  readonly children?: ReactNode;
}

/** Renders queued modals and provides {@link useModals} to the tree. */
export function ModalHost({ queue, children }: ModalHostProps) {
  const manager = useMemo(() => queue ?? createModalQueue(), [queue]);
  const snapshot = useSyncExternalStore(manager.subscribe, manager.getSnapshot, manager.getSnapshot);

  useEffect(() => {
    return () => {
      manager.reset();
    };
  }, [manager]);

  return (
    <ModalsContext.Provider value={manager}>
      {children}
      {snapshot.entries.map((entry) => (
        <QueuedModal key={entry.id} entry={entry} queue={manager} />
      ))}
    </ModalsContext.Provider>
  );
}

/** One queued dialog: translates its entry into a controlled {@link Modal}. */
function QueuedModal({ entry, queue }: { readonly entry: ModalEntry; readonly queue: ModalQueue }) {
  const api = useMemo<ModalRenderApi<unknown>>(
    () => ({
      confirm: (value) => {
        queue.confirm(entry.id, value);
      },
      cancel: () => {
        queue.cancel(entry.id);
      },
    }),
    [entry.id, queue],
  );

  return (
    <Modal
      open
      onOpenChange={(open) => {
        if (!open) {
          queue.dismiss(entry.id);
        }
      }}
      title={entry.title}
      description={entry.description}
      size={entry.size}
      dismissible={entry.dismissible}
      initialFocusRef={entry.initialFocusRef}
      footer={
        <>
          <Button variant="secondary" onClick={api.cancel}>
            {entry.cancelLabel}
          </Button>
          {entry.confirmLabel === undefined ? null : (
            <Button
              variant={entry.confirmTone === "destructive" ? "destructive" : "primary"}
              onClick={() => {
                queue.confirm(entry.id, entry.confirmValue);
              }}
            >
              {entry.confirmLabel}
            </Button>
          )}
        </>
      }
    >
      {entry.render(api)}
    </Modal>
  );
}

/**
 * Returns the modal queue owned by the nearest {@link ModalHost}.
 *
 * Throws when called without a host: a screen that awaits a dialog it can never
 * show is a wiring bug, and it should fail in development rather than hang.
 */
export function useModals(): ModalQueue {
  const queue = useContext(ModalsContext);
  if (queue === null) {
    throw new Error("useModals must be called inside <ModalHost>");
  }
  return queue;
}
