/**
 * Modal queue (TU-004).
 *
 * Callers ask for a dialog and receive a promise of {@link ModalResult}:
 *
 * ```ts
 * const result = await modals.open<string>({
 *   title: "Adopt release-notes",
 *   confirm: { label: "Adopt", value: "adopt" },
 *   body: () => <p>Original is copied, then replaced by a link.</p>,
 * });
 * ```
 *
 * The queue is framework-free so it can be tested without rendering, and so an
 * app-state object can own it next to the toast store. Requests stack in open
 * order; the last entry is the topmost dialog. Every request settles exactly
 * once — confirm, cancel, dismiss, or host teardown.
 */

import type { ReactNode, RefObject } from "react";
import type { ModalSize, ModalResult } from "./types";
import { MODAL_CANCELLED, MODAL_DISMISSED, modalConfirmed } from "./types";

/** What a dialog body can do to its own request. */
export interface ModalRenderApi<T> {
  /** Settle the dialog as confirmed with `value`. */
  readonly confirm: (value: T) => void;
  /** Settle the dialog as cancelled (the explicit cancel action). */
  readonly cancel: () => void;
}

/** A footer confirm button rendered by the host from the request. */
export interface ModalConfirmAction<T> {
  /** Button label, for example "Adopt". */
  readonly label: string;
  /** Value passed to the promise on confirm. */
  readonly value: T;
  /** Visual tone. Destructive actions render as `destructive` buttons. */
  readonly tone?: "primary" | "destructive";
}

/** One queued dialog. */
export interface ModalRequest<T> {
  /** Heading and accessible name. */
  readonly title: string;
  /** Optional supporting text under the heading. */
  readonly description?: string;
  /** Panel width; defaults to `md`. */
  readonly size?: ModalSize;
  /**
   * Whether Escape and outside clicks dismiss. Defaults to `true`; destructive
   * confirmations pass `false`.
   */
  readonly dismissible?: boolean;
  /** Label of the always-present cancel button. Defaults to "Cancel". */
  readonly cancelLabel?: string;
  /** Optional footer confirm button; omit when the body renders its own actions. */
  readonly confirm?: ModalConfirmAction<T>;
  /**
   * Element that receives focus instead of the first tabbable child. Pass the
   * cancel button's ref for destructive confirmations.
   */
  readonly initialFocusRef?: RefObject<HTMLElement | null>;
  /** Body content; may call `confirm`/`cancel` from inside. */
  readonly body: (api: ModalRenderApi<T>) => ReactNode;
}

/** The part of a queued dialog the host needs, independent of its result type. */
export interface ModalEntry {
  /** Unique request id. */
  readonly id: string;
  readonly title: string;
  readonly description?: string;
  readonly size?: ModalSize;
  readonly dismissible: boolean;
  readonly cancelLabel: string;
  readonly confirmLabel?: string;
  readonly confirmTone?: "primary" | "destructive";
  /** Value resolved on confirm; only meaningful when `confirmLabel` is set. */
  readonly confirmValue?: unknown;
  /** Initial focus target, forwarded to the modal. */
  readonly initialFocusRef?: RefObject<HTMLElement | null>;
  /** Renders the body with a type-erased action API. */
  readonly render: (api: ModalRenderApi<unknown>) => ReactNode;
}

/** Immutable view of the queue for `useSyncExternalStore`. */
export interface ModalSnapshot {
  /** Open dialogs from bottom to top. */
  readonly entries: readonly ModalEntry[];
}

/** The modal service. */
export interface ModalQueue {
  /**
   * Opens a dialog and settles when it closes.
   *
   * The promise never rejects: dismissal is a result, not an error.
   */
  open<T>(request: ModalRequest<T>): Promise<ModalResult<T>>;
  /** Settles the entry as confirmed. Unknown ids are ignored. */
  confirm(id: string, value: unknown): void;
  /** Settles the entry as cancelled. Unknown ids are ignored. */
  cancel(id: string): void;
  /** Settles the entry as dismissed. Unknown ids are ignored. */
  dismiss(id: string): void;
  /** Dismisses everything still open (host unmount, app teardown). */
  reset(): void;
  /** Subscribes to queue changes; `useSyncExternalStore` compatible. */
  subscribe(listener: () => void): () => void;
  /** Current snapshot; stable until the queue changes. */
  getSnapshot(): ModalSnapshot;
}

/** Options for {@link createModalQueue}. */
export interface ModalQueueOptions {
  /** Id factory, injectable so tests get readable ids. */
  readonly createId?: () => string;
}

let nextRequestId = 1;

const EMPTY_SNAPSHOT: ModalSnapshot = Object.freeze({ entries: Object.freeze([]) });

/**
 * Creates an isolated modal queue.
 *
 * Each entry keeps its own resolver and a `settled` flag, so a double click on
 * a footer button cannot resolve the same promise twice or reopen a closed
 * dialog.
 */
export function createModalQueue(options: ModalQueueOptions = {}): ModalQueue {
  const createId = options.createId ?? (() => `modal-${nextRequestId++}`);
  let snapshot: ModalSnapshot = EMPTY_SNAPSHOT;
  const resolvers = new Map<string, (result: ModalResult<unknown>) => void>();
  const listeners = new Set<() => void>();

  const notify = (): void => {
    for (const listener of listeners) {
      listener();
    }
  };

  const settle = (id: string, result: ModalResult<unknown>): void => {
    const resolve = resolvers.get(id);
    if (resolve === undefined) {
      return;
    }
    resolvers.delete(id);
    snapshot = { entries: snapshot.entries.filter((entry) => entry.id !== id) };
    notify();
    resolve(result);
  };

  return {
    open<T>(request: ModalRequest<T>): Promise<ModalResult<T>> {
      const id = createId();
      const { promise, resolve } = Promise.withResolvers<ModalResult<T>>();
      const entry: ModalEntry = {
        id,
        title: request.title,
        description: request.description,
        size: request.size,
        dismissible: request.dismissible ?? true,
        cancelLabel: request.cancelLabel ?? "Cancel",
        confirmLabel: request.confirm?.label,
        confirmTone: request.confirm?.tone,
        confirmValue: request.confirm?.value,
        initialFocusRef: request.initialFocusRef,
        render: (api) =>
          request.body({
            confirm: (value: T) => api.confirm(value),
            cancel: api.cancel,
          }),
      };
      // The queue stores one resolver shape; the body adapter above keeps the
      // caller-facing types checked at the boundary.
      resolvers.set(id, (result) => resolve(result as ModalResult<T>));
      snapshot = { entries: [...snapshot.entries, entry] };
      notify();
      return promise;
    },
    confirm(id, value) {
      settle(id, modalConfirmed(value));
    },
    cancel(id) {
      settle(id, MODAL_CANCELLED);
    },
    dismiss(id) {
      settle(id, MODAL_DISMISSED);
    },
    reset() {
      const open = [...snapshot.entries];
      snapshot = EMPTY_SNAPSHOT;
      const pending = [...resolvers.entries()];
      resolvers.clear();
      if (pending.length === 0 && open.length === 0) {
        return;
      }
      notify();
      for (const [, resolve] of pending) {
        resolve(MODAL_DISMISSED);
      }
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    getSnapshot() {
      return snapshot;
    },
  };
}
