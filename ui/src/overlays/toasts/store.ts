/**
 * Toast queue and timers (TU-005).
 *
 * The store is framework-free: it owns the visible stack, the FIFO waiting
 * queue, collapse counting and expiry timers, and exposes a
 * `useSyncExternalStore`-compatible snapshot. `AppState` (TU-009) owns the
 * instance; `ToastProvider` exposes it to React and the viewport renders it.
 *
 * Rules that come from the story:
 * - at most three toasts are visible; the rest wait in FIFO order;
 * - success, warning and info toasts auto-dismiss after {@link AUTO_DISMISS_MS};
 * - error toasts never auto-dismiss;
 * - an identical toast arriving within {@link COLLAPSE_WITHIN_MS} of the previous
 *   one increments its count instead of adding another card.
 */

import { createSystemClock, type Clock, type TimerHandle } from "./clock";
import type { Toast, ToastAction, ToastInput, ToastSnapshot } from "./types";

/** Auto-dismiss delay for non-error toasts, in milliseconds. */
export const AUTO_DISMISS_MS = 2500;

/** Window in which an identical toast collapses into the previous one. */
export const COLLAPSE_WITHIN_MS = 1000;

/** Maximum number of toasts rendered at once. */
export const MAX_VISIBLE_TOASTS = 3;

/** Options for {@link createToastStore}; every timing and sizing knob is injectable. */
export interface ToastStoreOptions {
  /** Clock used for expiry and collapse windows; defaults to the system clock. */
  readonly clock?: Clock;
  /** Auto-dismiss delay for non-error toasts; defaults to {@link AUTO_DISMISS_MS}. */
  readonly autoDismissMs?: number;
  /** Collapse window for identical toasts; defaults to {@link COLLAPSE_WITHIN_MS}. */
  readonly collapseWithinMs?: number;
  /** Visible slots; defaults to {@link MAX_VISIBLE_TOASTS}. */
  readonly maxVisible?: number;
  /** Id factory, injectable so tests get readable ids. */
  readonly createId?: () => string;
}

/** The toast service. One instance is owned by `AppState`. */
export interface ToastStore {
  /** Current snapshot; the same object until the queue changes. */
  getSnapshot(): ToastSnapshot;
  /** Registers a listener; returns the unsubscribe function. */
  subscribe(listener: () => void): () => void;
  /** Shows `input` (or increments an identical recent toast) and returns its id. */
  show(input: ToastInput): string;
  /** Closes a toast and promotes the next queued one. Unknown ids are ignored. */
  dismiss(id: string): void;
  /** Runs the toast action (if any) and dismisses it. */
  activate(id: string): void;
  /** Closes every visible and queued toast. */
  dismissAll(): void;
}

interface Entry {
  toast: Toast;
  readonly key: string;
  lastAt: number;
  timer: TimerHandle | null;
}

/**
 * Creates a toast store.
 *
 * @param options - Clock, timings and id factory; all optional.
 * @returns The store; call it once per app (or once per test).
 */
export function createToastStore(options: ToastStoreOptions = {}): ToastStore {
  const clock = options.clock ?? createSystemClock();
  const testDismissMs = typeof window !== "undefined" && "__AT_TOAST_DISMISS_MS__" in window && typeof window.__AT_TOAST_DISMISS_MS__ === "number" ? window.__AT_TOAST_DISMISS_MS__ : undefined;
  const autoDismissMs = options.autoDismissMs ?? testDismissMs ?? AUTO_DISMISS_MS;
  const collapseWithinMs = options.collapseWithinMs ?? COLLAPSE_WITHIN_MS;
  const maxVisible = Math.max(1, options.maxVisible ?? MAX_VISIBLE_TOASTS);
  let idSequence = 0;
  const createId = options.createId ?? (() => `toast-${++idSequence}`);

  // Collapse compares the action by identity, not just by label: merging two
  // different Retry buttons would make the survivor run the wrong operation.
  const actionTokens = new WeakMap<() => void, number>();
  let actionTokenSequence = 0;

  function actionToken(action: ToastAction | undefined): string {
    if (action === undefined) {
      return "";
    }
    let token = actionTokens.get(action.run);
    if (token === undefined) {
      token = ++actionTokenSequence;
      actionTokens.set(action.run, token);
    }
    return `${action.label}#${token}`;
  }

  /** Properties that decide whether two toasts are "identical" for collapse. */
  function identityKey(input: ToastInput): string {
    return [
      input.kind,
      input.title ?? "",
      input.message,
      input.details ?? "",
      actionToken(input.action),
    ].join("\u0000");
  }

  const visible: Entry[] = [];
  const waiting: Entry[] = [];
  const listeners = new Set<() => void>();
  let snapshot: ToastSnapshot = { visible: [], queued: 0 };

  function refresh(): void {
    snapshot = { visible: visible.map((entry) => entry.toast), queued: waiting.length };
    for (const listener of listeners) {
      listener();
    }
  }

  function findEntry(id: string): Entry | undefined {
    return (
      visible.find((entry) => entry.toast.id === id) ??
      waiting.find((entry) => entry.toast.id === id)
    );
  }

  function arm(entry: Entry): void {
    if (entry.timer !== null) {
      clock.cancel(entry.timer);
      entry.timer = null;
    }
    // Errors stay until the user closes them; every other kind expires.
    if (entry.toast.kind === "error") {
      return;
    }
    entry.timer = clock.schedule(() => {
      entry.timer = null;
      dismiss(entry.toast.id);
    }, autoDismissMs);
  }

  function dismiss(id: string): void {
    const visibleIndex = visible.findIndex((entry) => entry.toast.id === id);
    if (visibleIndex === -1) {
      const waitingIndex = waiting.findIndex((entry) => entry.toast.id === id);
      if (waitingIndex !== -1) {
        waiting.splice(waitingIndex, 1);
        refresh();
      }
      return;
    }

    const removed = visible.splice(visibleIndex, 1)[0];
    if (removed !== undefined && removed.timer !== null) {
      clock.cancel(removed.timer);
      removed.timer = null;
    }

    const promoted = waiting.shift();
    if (promoted !== undefined) {
      visible.push(promoted);
      arm(promoted);
    }
    refresh();
  }

  function show(input: ToastInput): string {
    const now = clock.now();
    const key = identityKey(input);
    const existing = [...visible, ...waiting].find(
      (entry) => entry.key === key && now - entry.lastAt <= collapseWithinMs,
    );
    if (existing !== undefined) {
      existing.toast = { ...existing.toast, count: existing.toast.count + 1 };
      existing.lastAt = now;
      if (visible.includes(existing)) {
        arm(existing);
      }
      refresh();
      return existing.toast.id;
    }

    const entry: Entry = {
      key,
      lastAt: now,
      timer: null,
      toast: {
        id: createId(),
        kind: input.kind,
        message: input.message,
        title: input.title,
        details: input.details,
        action: input.action,
        count: 1,
        createdAt: now,
      },
    };

    if (visible.length < maxVisible) {
      visible.push(entry);
      arm(entry);
    } else {
      waiting.push(entry);
    }
    refresh();
    return entry.toast.id;
  }

  function activate(id: string): void {
    const entry = findEntry(id);
    if (entry === undefined) {
      return;
    }
    try {
      entry.toast.action?.run();
    } finally {
      dismiss(id);
    }
  }

  function dismissAll(): void {
    for (const entry of visible) {
      if (entry.timer !== null) {
        clock.cancel(entry.timer);
        entry.timer = null;
      }
    }
    visible.length = 0;
    waiting.length = 0;
    refresh();
  }

  return {
    getSnapshot: () => snapshot,
    subscribe: (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    show,
    dismiss,
    activate,
    dismissAll,
  };
}
