/**
 * Test doubles for the toast service (TU-005).
 *
 * Exported so every suite drives the store with the same deterministic clock
 * instead of duplicating a timer implementation.
 */

import type { Clock, TimerHandle } from "./clock";

/** Deterministic clock with an explicit `advance`. */
export interface FakeClock extends Clock {
  /** Moves time forward, firing every due timer in chronological order. */
  advance(ms: number): void;
  /** Number of timers scheduled and not yet fired or cancelled. */
  pending(): number;
}

interface ScheduledTimer {
  readonly id: TimerHandle;
  readonly at: number;
  readonly run: () => void;
}

/**
 * Creates a fake clock starting at `start`.
 *
 * Timers scheduled while time advances fire in the same `advance` call when
 * they become due, matching `setTimeout` chaining in the browser.
 */
export function createFakeClock(start = 0): FakeClock {
  let current = start;
  let sequence = 0;
  const timers = new Map<TimerHandle, ScheduledTimer>();

  return {
    now: () => current,
    schedule: (callback, delayMs) => {
      const id = ++sequence;
      timers.set(id, { id, at: current + Math.max(0, delayMs), run: callback });
      return id;
    },
    cancel: (handle) => {
      timers.delete(handle);
    },
    pending: () => timers.size,
    advance: (ms) => {
      const target = current + Math.max(0, ms);
      for (;;) {
        let next: ScheduledTimer | undefined;
        for (const timer of timers.values()) {
          if (timer.at <= target && (next === undefined || timer.at < next.at)) {
            next = timer;
          }
        }
        if (next === undefined) {
          break;
        }
        timers.delete(next.id);
        current = next.at;
        next.run();
      }
      current = target;
    },
  };
}
