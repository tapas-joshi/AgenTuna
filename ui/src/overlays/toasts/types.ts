/**
 * Toast types for the toast service (TU-005).
 *
 * A toast is plain data. The store owns timing and ordering; the viewport owns
 * rendering and accessibility. `details` carries the technical text the Copy
 * details button puts on the clipboard, so error toasts can show a friendly
 * sentence and still give a bug report payload.
 */

/** Kind of toast; it selects the label, icon colour and announcement channel. */
export type ToastKind = "success" | "warning" | "info" | "error";

/** Every kind, in display order. */
export const TOAST_KINDS: readonly ToastKind[] = ["success", "warning", "info", "error"];

/** Human-readable label for a kind; colour never carries the meaning alone. */
export const TOAST_KIND_LABEL: Readonly<Record<ToastKind, string>> = {
  success: "Success",
  warning: "Warning",
  info: "Info",
  error: "Error",
};

/** A button rendered inside a toast. */
export interface ToastAction {
  /** Visible button text, also part of the toast identity. */
  readonly label: string;
  /** Runs when the button is pressed; the toast then dismisses. */
  readonly run: () => void;
}

/** Input accepted by the store. */
export interface ToastInput {
  /** Kind of toast. */
  readonly kind: ToastKind;
  /** One-sentence message, announced to assistive technology. */
  readonly message: string;
  /** Optional short heading shown above the message. */
  readonly title?: string;
  /** Technical text exposed through Copy details; required for error payloads. */
  readonly details?: string;
  /** Optional action button. */
  readonly action?: ToastAction;
}

/** A toast as stored and rendered. */
export interface Toast {
  /** Stable identity, unique for the lifetime of the app session. */
  readonly id: string;
  /** Kind of toast. */
  readonly kind: ToastKind;
  /** Message announced and displayed. */
  readonly message: string;
  /** Optional heading. */
  readonly title?: string;
  /** Technical text for Copy details. */
  readonly details?: string;
  /** Optional action button. */
  readonly action?: ToastAction;
  /** How many identical toasts were collapsed into this one; at least 1. */
  readonly count: number;
  /** Clock time the first occurrence was shown, used for the auto-dismiss window. */
  readonly createdAt: number;
}

/** Options shared by the typed `success`, `warning` and `info` helpers. */
export interface ToastMessageOptions {
  /** Optional short heading. */
  readonly title?: string;
  /** Optional action button. */
  readonly action?: ToastAction;
}

/** Input of the typed `error` helper. */
export interface ToastErrorInput extends ToastMessageOptions {
  /** Friendly sentence shown and announced. */
  readonly message: string;
  /** Technical details for the Copy details button. */
  readonly details?: string;
}

/** Clipboard port so the viewport never touches the platform directly. */
export interface ClipboardPort {
  /** Copies `text` to the system clipboard. */
  writeText(text: string): Promise<void>;
}

/** Read-only queue state exposed by the toast store. */
export interface ToastSnapshot {
  /** Visible toasts, oldest first; at most `maxVisible`. */
  readonly visible: readonly Toast[];
  /** Toasts waiting for a visible slot, in FIFO order. */
  readonly queued: number;
}
