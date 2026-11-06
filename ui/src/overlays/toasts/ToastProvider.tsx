/**
 * Toast provider and hook (TU-005).
 *
 * The provider owns the store unless `AppState` (TU-009) passes one, then
 * renders the viewport once alongside the app tree. Screens call `useToast()`
 * and never touch timers, the queue or the clipboard.
 */

import { createContext, useContext, useMemo, useState, type ReactNode } from "react";
import type { CommandError } from "../../generated/CommandError";
import type { JobError } from "../../generated/JobError";
import type {
  ClipboardPort,
  ToastErrorInput,
  ToastInput,
  ToastMessageOptions,
} from "./types";
import { createToastStore, type ToastStore } from "./store";
import { errorToast, toastFromCommandError, toastFromJobError } from "./errors";
import { ToastViewport } from "./ToastViewport";

/** The imperative toast API handed to screens through {@link useToast}. */
export interface ToastApi {
  /** Shows any toast and returns its id. */
  show(input: ToastInput): string;
  /** Shows a success toast. */
  success(message: string, options?: ToastMessageOptions): string;
  /** Shows a warning toast. */
  warning(message: string, options?: ToastMessageOptions): string;
  /** Shows an info toast. */
  info(message: string, options?: ToastMessageOptions): string;
  /** Shows an error toast, which stays until dismissed. */
  error(input: ToastErrorInput): string;
  /** Shows a command rejection as an error toast with Copy details. */
  commandError(error: CommandError, options?: ToastMessageOptions): string;
  /** Shows a background job failure as an error toast with Copy details. */
  jobError(error: JobError, options?: ToastMessageOptions): string;
  /** Closes one toast. */
  dismiss(id: string): void;
  /** Closes every toast. */
  dismissAll(): void;
}

/** Properties of {@link ToastProvider}. */
export interface ToastProviderProps {
  /** App tree that receives the provider. */
  readonly children?: ReactNode;
  /** Store owned by `AppState`; a private store is created when omitted. */
  readonly store?: ToastStore;
  /** Clipboard port; injectable for tests. */
  readonly clipboard?: ClipboardPort;
}

export const ToastContext = createContext<ToastApi | null>(null);

/** Provides the toast API and renders the viewport once for the app. */
export function ToastProvider({ children, store, clipboard }: ToastProviderProps) {
  const [privateStore] = useState(() => createToastStore());
  const activeStore = store ?? privateStore;

  const api = useMemo<ToastApi>(
    () => ({
      show: (input) => activeStore.show(input),
      success: (message, options) =>
        activeStore.show({ kind: "success", message, title: options?.title, action: options?.action }),
      warning: (message, options) =>
        activeStore.show({ kind: "warning", message, title: options?.title, action: options?.action }),
      info: (message, options) =>
        activeStore.show({ kind: "info", message, title: options?.title, action: options?.action }),
      error: (input) => activeStore.show(errorToast(input)),
      commandError: (error, options) => activeStore.show(toastFromCommandError(error, options)),
      jobError: (error, options) => activeStore.show(toastFromJobError(error, options)),
      dismiss: (id) => activeStore.dismiss(id),
      dismissAll: () => activeStore.dismissAll(),
    }),
    [activeStore],
  );

  return (
    <ToastContext.Provider value={api}>
      {children}
      <ToastViewport store={activeStore} clipboard={clipboard} />
    </ToastContext.Provider>
  );
}

/**
 * Returns the toast API of the nearest {@link ToastProvider}.
 *
 * @throws Error when called outside the provider, so a missing provider fails
 * loudly instead of dropping notifications.
 */
export function useToast(): ToastApi {
  const api = useContext(ToastContext);
  if (api === null) {
    throw new Error("useToast must be used inside <ToastProvider>");
  }
  return api;
}
