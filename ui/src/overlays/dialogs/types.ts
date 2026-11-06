/**
 * Typed modal outcomes (TU-004).
 *
 * A caller reads one discriminated union instead of shared booleans:
 *
 * - `confirmed` carries the value the dialog produced;
 * - `cancelled` means the user pressed the explicit Cancel/Dismiss action;
 * - `dismissed` means the dialog went away without a decision — Escape, a
 *   backdrop click, or the host tearing down.
 *
 * Keeping "cancelled" and "dismissed" apart lets a caller run undo only for an
 * explicit cancel, or treat an accidental Escape as "no answer yet".
 */

/** Width class of a modal panel. `md` is the default working width. */
export type ModalSize = "sm" | "md" | "lg";

/** A decision the dialog produced; `value` is part of the result, not a side channel. */
export interface ModalConfirmed<T> {
  readonly kind: "confirmed";
  readonly value: T;
}

/** The user pressed the explicit cancel action. */
export interface ModalCancelled {
  readonly kind: "cancelled";
}

/** The dialog closed without a decision: Escape, backdrop, or host teardown. */
export interface ModalDismissed {
  readonly kind: "dismissed";
}

/** Everything a modal caller has to handle. */
export type ModalResult<T> = ModalConfirmed<T> | ModalCancelled | ModalDismissed;

/** Builds a confirmed result. */
export function modalConfirmed<T>(value: T): ModalConfirmed<T> {
  return { kind: "confirmed", value };
}

/** A cancelled result; frozen because it carries no state. */
export const MODAL_CANCELLED: ModalCancelled = Object.freeze({ kind: "cancelled" });

/** A dismissed result; frozen because it carries no state. */
export const MODAL_DISMISSED: ModalDismissed = Object.freeze({ kind: "dismissed" });

/** Narrows a result to the confirmed variant. */
export function isModalConfirmed<T>(result: ModalResult<T>): result is ModalConfirmed<T> {
  return result.kind === "confirmed";
}

/** The confirmed value, or `undefined` when the dialog was cancelled/dismissed. */
export function modalValue<T>(result: ModalResult<T>): T | undefined {
  return isModalConfirmed(result) ? result.value : undefined;
}
