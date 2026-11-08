/**
 * Waiting contract for one migration apply job.
 *
 * An apply runs as a cancellable bridge job. The command layer keeps a finished job's result
 * keyed by its job id, so the result command alone answers "done or still running": the UI
 * never has to guess a deadline and never reports a failure it did not receive from the
 * backend. A job that failed, was cancelled or was interrupted publishes no result; the
 * `agentuna:job-finished` event carries its typed error, and the result command reports the
 * terminal answer for it (`not_found`).
 */

import type { JobError } from "../../generated/JobError";
import type { JobFinishedEvent } from "../../generated/JobFinishedEvent";
import type { MigrateApplyResultView } from "../../generated/MigrateApplyResultView";
import { commandErrorMessage } from "../../lib/commandError";

/** What the wait needs from the bridge, so tests can supply a fake. */
export interface ApplyJobBridge {
  /** Typed Tauri command. */
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  /** Tauri event subscription; resolves to the unsubscribe function. */
  listen<T>(event: string, handler: (payload: T) => void): Promise<() => void>;
}

/** One typed failure that ended an apply, as the backend described it. */
export interface ApplyFailure {
  /** Stable failure class, when the backend named one. */
  readonly code: string;
  /** Human-readable, secret-free explanation. */
  readonly message: string;
  /** What the user can do next, when the backend named a remedy. */
  readonly remedy: string | null;
}

/** The backend's answer for one apply job. */
export type ApplyJobOutcome =
  | { readonly kind: "applied"; readonly result: MigrateApplyResultView }
  | { readonly kind: "failed"; readonly failure: ApplyFailure };

/** Result-command code for "the job is still running". */
const CODE_RUNNING = "conflict";
/** Result-command code for "the job ended without publishing a result". */
const CODE_NO_RESULT = "not_found";
/** Pause between result reads while the job is registered as running. */
const RESULT_POLL_INTERVAL_MS = 150;
/**
 * How long to wait for the finished event after the result command turns terminal. The event
 * is emitted by the bridge before the job leaves the registry, but it crosses the IPC boundary
 * asynchronously, so a short grace keeps the message accurate without inventing a failure.
 */
const FINISHED_EVENT_GRACE_MS = 500;
/** Polling step inside the finished-event grace. */
const EVENT_POLL_STEP_MS = 25;

/** Resolves after `ms` milliseconds. */
function delay(ms: number): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  setTimeout(resolve, ms);
  return promise;
}

/** Reads a command rejection as `{code, message}` without trusting its shape. */
export function failureFromCommandError(cause: unknown): ApplyFailure {
  if (typeof cause === "object" && cause !== null) {
    const code = "code" in cause && typeof cause.code === "string" ? cause.code : "";
    const message =
      "message" in cause && typeof cause.message === "string" ? cause.message : commandErrorMessage(cause);
    return { code, message, remedy: null };
  }
  return { code: "", message: commandErrorMessage(cause), remedy: null };
}

/** One line combining the backend's explanation and its remedy. */
export function describeFailure(failure: ApplyFailure): string {
  if (failure.remedy === null || failure.remedy.trim() === "") return failure.message;
  return `${failure.message} ${failure.remedy}`;
}

/**
 * Describes any command rejection for display.
 *
 * A Tauri command rejects with a serialized error value; the shared formatter extracts its
 * human-readable message instead of coercing an object to `[object Object]`.
 */
export function describeCause(cause: unknown): string {
  return describeFailure(failureFromCommandError(cause));
}

/** Maps the typed error a finished job reported onto the screen's failure shape. */
export function failureFromJobError(error: JobError | null): ApplyFailure | null {
  if (error === null) return null;
  if (error.kind === "migration") {
    return {
      code: error.failure.code,
      message: error.failure.message,
      remedy: error.failure.remedy,
    };
  }
  if (error.kind === "panic") {
    return { code: "panic", message: error.message, remedy: "Retry the apply, then export diagnostics from the logs screen." };
  }
  if (error.kind === "failed") {
    return { code: "failed", message: error.message, remedy: null };
  }
  return {
    code: "remote_backup",
    message: error.failure.message,
    remedy: null,
  };
}

/** The failure a terminal job state without a typed error represents. */
function stateFailure(state: string): ApplyFailure {
  if (state === "cancelled") {
    return {
      code: "cancelled",
      message: "The migration was cancelled before it finished.",
      remedy: "Review the plan and apply again.",
    };
  }
  if (state === "interrupted") {
    return {
      code: "interrupted",
      message: "The migration was interrupted when AgenTuna shut down.",
      remedy: "Reopen the migration, review the plan and apply again.",
    };
  }
  return {
    code: "succeeded",
    message: "The migration job reported success but published no result.",
    remedy: "Reopen the migration screen; the operation history lists what was written.",
  };
}

/**
 * Waits for one apply job and returns the backend's outcome.
 *
 * The finished-event listener is registered before the first result read, so a job that
 * completed before the screen subscribed is still answered: its result stays in the registry
 * and the lookup returns it immediately.
 */
export async function awaitApplyJob(
  bridge: ApplyJobBridge,
  jobId: string,
): Promise<ApplyJobOutcome> {
  let finished: JobFinishedEvent | null = null;
  // Read through a function: the listener assigns from a closure, which TypeScript's
  // control-flow analysis does not track, so a bare read would stay narrowed to `null`.
  const finishedEvent = (): JobFinishedEvent | null => finished;
  const stop = await bridge
    .listen<JobFinishedEvent>("agentuna:job-finished", (payload) => {
      if (payload.id === jobId) finished = payload;
    })
    .catch(() => () => undefined);

  try {
    for (;;) {
      try {
        // Tauri 2 converts command arguments from camelCase, so the Rust `job_id`
        // parameter is `jobId` on the wire. Sending `job_id` makes the command reject with
        // "invalid args" and every poll look like a job that never finishes.
        const result = await bridge.invoke<MigrateApplyResultView>("migrate_apply_result", {
          jobId,
        });
        return { kind: "applied", result };
      } catch (cause) {
        const failure = failureFromCommandError(cause);
        if (failure.code !== CODE_RUNNING && failure.code !== CODE_NO_RESULT) {
          return { kind: "failed", failure };
        }
        if (failure.code === CODE_RUNNING) {
          await delay(RESULT_POLL_INTERVAL_MS);
          continue;
        }
        // Terminal: the job is no longer running and published no result.
        const deadline = Date.now() + FINISHED_EVENT_GRACE_MS;
        while (finishedEvent() === null && Date.now() < deadline) {
          await delay(EVENT_POLL_STEP_MS);
        }
        const event = finishedEvent();
        if (event !== null) {
          const typed = failureFromJobError(event.error);
          if (typed !== null) return { kind: "failed", failure: typed };
          return { kind: "failed", failure: stateFailure(event.state) };
        }
        return {
          kind: "failed",
          failure: {
            code: "no_result",
            message: failure.message,
            remedy: "Review the plan and apply again; if it repeats, export diagnostics from the logs screen.",
          },
        };
      }
    }
  } finally {
    stop();
  }
}
