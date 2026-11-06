/**
 * Timing port for the toast service (TU-005).
 *
 * The toast store never calls `setTimeout` directly: every delay goes through a
 * [`Clock`] so expiry is measured with an injected clock in tests and by the
 * system clock in the app.
 */

/** Opaque timer handle owned by the clock that produced it. */
export type TimerHandle = unknown;

/** Time source and scheduler used by the toast store. */
export interface Clock {
  /** Milliseconds since the epoch; only differences are meaningful to callers. */
  now(): number;
  /** Runs `callback` once after `delayMs` and returns a handle for {@link Clock.cancel}. */
  schedule(callback: () => void, delayMs: number): TimerHandle;
  /** Cancels a timer that has not fired yet. Unknown handles are ignored. */
  cancel(handle: TimerHandle): void;
}

/** The real clock: `Date.now` and `setTimeout` from the host environment. */
export function createSystemClock(): Clock {
  return {
    now: () => Date.now(),
    schedule: (callback, delayMs) => {
      const handle = globalThis.setTimeout(callback, delayMs);
      return handle;
    },
    cancel: (handle) => {
      // The same host produced the handle, so it is a `setTimeout` id for `clearTimeout`.
      globalThis.clearTimeout(handle as number);
    },
  };
}
