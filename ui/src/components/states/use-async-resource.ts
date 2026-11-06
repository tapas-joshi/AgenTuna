/**
 * `useAsyncResource` — the fetch lifecycle behind {@link AsyncBoundary} (TU-021).
 *
 * It keeps the previous data and the previous error visible while a new attempt
 * runs, normalises any rejection into an {@link ErrorReportView}, and holds the
 * loading state for a short minimum so a fast response never flashes the
 * skeleton. It never writes or navigates; the screen owns both.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { toErrorReport, type ErrorReportView } from "./error-model";

/** Lifecycle of one async read. */
export type AsyncStatus = "loading" | "success" | "error";

/** Options accepted by {@link useAsyncResource}. */
export interface AsyncResourceOptions<T> {
  /**
   * Values that re-run the load when they change, compared like a `useEffect`
   * dependency list. The array size must stay constant.
   */
  readonly deps?: readonly unknown[];
  /**
   * Treats loaded data as empty, so the screen renders the empty state instead
   * of the populated one. Defaults to "arrays and strings with length 0".
   */
  readonly isEmpty?: (data: T) => boolean;
  /**
   * Minimum time in milliseconds the loading state stays on screen, so a fast
   * local read does not flash a skeleton. Defaults to
   * {@link DEFAULT_MIN_LOADING_MS}; pass `0` to disable.
   */
  readonly minLoadingMs?: number;
}

/** The observable state of {@link useAsyncResource}. */
export interface AsyncResource<T> {
  /** Current lifecycle step. */
  readonly status: AsyncStatus;
  /** Shorthand for `status === "loading"`. */
  readonly loading: boolean;
  /** Loaded data; retained during a reload so the screen does not collapse. */
  readonly data: T | undefined;
  /** Last failure, retained while a retry runs. */
  readonly error: ErrorReportView | undefined;
  /** Whether `data` is present and counts as empty. */
  readonly isEmpty: boolean;
  /** Starts another attempt, keeping the current data and error on screen. */
  readonly retry: () => void;
}

/** Default minimum time the loading state is visible. */
export const DEFAULT_MIN_LOADING_MS = 180;

interface ResourceState<T> {
  readonly status: AsyncStatus;
  readonly data: T | undefined;
  readonly error: ErrorReportView | undefined;
}

interface Inflight<T> {
  readonly attempt: number;
  readonly minLoadingMs: number;
  readonly deps: readonly unknown[];
  readonly promise: Promise<T>;
}

function shallowEqualDeps(first: readonly unknown[], second: readonly unknown[]): boolean {
  return first.length === second.length && first.every((value, index) => Object.is(value, second[index]));
}

function defaultIsEmpty(value: unknown): boolean {
  if (Array.isArray(value) || typeof value === "string") {
    return value.length === 0;
  }
  return false;
}

/**
 * Loads `data` on mount and whenever `deps` change, with a retry that never
 * clears the previous data or error.
 *
 * Renders start in `loading`, so the first paint is a skeleton rather than a
 * misleading empty state. Results from a superseded attempt are ignored, and
 * an in-flight request is reused across React StrictMode's double effect so the
 * same read is never issued twice.
 *
 * @param load - Fetches the data. The latest closure is always used.
 * @param options - Dependency list, empty predicate and minimum loading time.
 * @returns The resource state plus `retry`.
 */
export function useAsyncResource<T>(
  load: () => Promise<T>,
  options: AsyncResourceOptions<T> = {},
): AsyncResource<T> {
  const { deps = [], isEmpty, minLoadingMs = DEFAULT_MIN_LOADING_MS } = options;
  const loadRef = useRef(load);
  // Reusing the promise keeps React StrictMode's double effect from issuing the
  // same network call twice while that call is still in flight.
  const inflightRef = useRef<Inflight<T> | undefined>(undefined);
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<ResourceState<T>>({
    status: "loading",
    data: undefined,
    error: undefined,
  });

  useEffect(() => {
    loadRef.current = load;
  });

  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;
    const startedAt = Date.now();

    setState((previous) => ({ status: "loading", data: previous.data, error: previous.error }));

    const cached = inflightRef.current;
    const reusable =
      cached !== undefined &&
      cached.attempt === attempt &&
      cached.minLoadingMs === minLoadingMs &&
      shallowEqualDeps(cached.deps, deps);
    let promise: Promise<T>;
    if (reusable && cached !== undefined) {
      promise = cached.promise;
    } else {
      promise = Promise.resolve().then(loadRef.current);
      inflightRef.current = { attempt, minLoadingMs, deps: [...deps], promise };
    }

    const settle = (next: ResourceState<T>): void => {
      if (cancelled) {
        return;
      }
      const remaining = Math.max(0, minLoadingMs - (Date.now() - startedAt));
      if (remaining === 0) {
        if (!cancelled) {
          setState(next);
        }
        return;
      }
      timer = window.setTimeout(() => {
        if (!cancelled) {
          setState(next);
        }
      }, remaining);
    };

    const run = async (): Promise<void> => {
      try {
        settle({ status: "success", data: await promise, error: undefined });
      } catch (error) {
        settle({ status: "error", data: undefined, error: toErrorReport(error) });
      }
    };

    void run();

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
    // `deps` is a caller-supplied dependency list, like `useEffect`'s.
  }, [attempt, minLoadingMs, ...deps]);

  const retry = useCallback(() => {
    setAttempt((previous) => previous + 1);
  }, []);

  const isEmptyData =
    state.data !== undefined && (isEmpty === undefined ? defaultIsEmpty(state.data) : isEmpty(state.data));

  return {
    status: state.status,
    loading: state.status === "loading",
    data: state.data,
    error: state.error,
    isEmpty: isEmptyData,
    retry,
  };
}
