/**
 * Typed error adapters for toasts (TU-005).
 *
 * Every Tauri command rejects with a {@link CommandError} and every failed or
 * panicked background job reports a {@link JobError}; both become an error
 * toast (which never auto-dismisses) with a Copy details payload. When
 * `at-model::ErrorReport` gains its generated TypeScript type, screens can also
 * pass `user_message` and the serialised report through {@link errorToast}.
 */

import type { CommandError } from "../../generated/CommandError";
import type { JobError } from "../../generated/JobError";
import type { ToastErrorInput, ToastInput, ToastMessageOptions } from "./types";

/**
 * A non-expiring error toast.
 *
 * @param input - Friendly message plus the technical details to copy.
 * @returns Toast input of kind `error`.
 */
export function errorToast(input: ToastErrorInput): ToastInput {
  return {
    kind: "error",
    message: input.message,
    title: input.title,
    details: input.details,
    action: input.action,
  };
}

/**
 * Adapts the rejection value of a Tauri command.
 *
 * @param error - Typed, already redacted command error.
 * @param options - Optional heading override and action button.
 * @returns Toast input of kind `error`.
 */
export function toastFromCommandError(
  error: CommandError,
  options: ToastMessageOptions = {},
): ToastInput {
  return errorToast({
    message: error.message,
    title: options.title ?? error.code,
    details: `${error.code}\n${error.message}`,
    action: options.action,
  });
}

/**
 * Adapts a failed or panicked background job.
 *
 * @param error - Job failure delivered to the webview.
 * @param options - Optional heading override and action button.
 * @returns Toast input of kind `error`.
 */
export function toastFromJobError(error: JobError, options: ToastMessageOptions = {}): ToastInput {
  // Remote-backup failures carry a typed, secret-free failure instead of a bare message.
  const message = error.kind === "remote_backup" ? error.failure.message : error.message;
  return errorToast({
    message,
    title: options.title ?? (error.kind === "panic" ? "Background job crashed" : "Job failed"),
    details: `${error.kind}\n${message}`,
    action: options.action,
  });
}
