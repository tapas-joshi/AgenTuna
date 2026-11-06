/**
 * Public surface of the toast service (TU-005).
 *
 * ```tsx
 * // once, near the root of the app
 * <ToastProvider store={appState.toasts}>…</ToastProvider>
 *
 * // anywhere below it
 * const toast = useToast();
 * toast.success("Skill enabled");
 * toast.commandError(error);          // stays until dismissed, Copy details
 * ```
 */

export {
  AUTO_DISMISS_MS,
  COLLAPSE_WITHIN_MS,
  MAX_VISIBLE_TOASTS,
  createToastStore,
} from "./store";
export type { ToastStore, ToastStoreOptions } from "./store";
export { ToastProvider, useToast } from "./ToastProvider";
export type { ToastApi, ToastProviderProps } from "./ToastProvider";
export { ToastViewport } from "./ToastViewport";
export type { ToastViewportProps } from "./ToastViewport";
export { createSystemClock } from "./clock";
export type { Clock, TimerHandle } from "./clock";
export { createBrowserClipboard, toastCopyText } from "./copy";
export { errorToast, toastFromCommandError, toastFromJobError } from "./errors";
export { TOAST_KINDS, TOAST_KIND_LABEL } from "./types";
export type {
  ClipboardPort,
  Toast,
  ToastAction,
  ToastErrorInput,
  ToastInput,
  ToastKind,
  ToastMessageOptions,
  ToastSnapshot,
} from "./types";
