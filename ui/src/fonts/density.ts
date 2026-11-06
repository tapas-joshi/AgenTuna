/**
 * Device pixel density (TU-024).
 *
 * The webview reports `devicePixelRatio`, and the same value changes when the
 * window moves to a monitor with another scale factor or the display setting
 * changes. The app mirrors it onto the root as `data-density` plus
 * `--at-pixel-ratio`, which `fonts.css` turns into `--at-hairline` (one device
 * pixel). Nothing else depends on the ratio, so a change repaints without a
 * remount: CSS pixel sizes are already correct at 1x, 1.5x and 2x.
 *
 * Detection uses `matchMedia("(resolution: <ratio>dppx)")`, the only portable
 * change event for scale factor; browsers without `matchMedia` still get the
 * startup value.
 */

/** Density bucket the story tests screenshots at. */
export type DensityBucket = "1x" | "1.5x" | "2x";

/** Applied density state. */
export interface DensityInfo {
  /** `devicePixelRatio` that produced this state, for example `1.5`. */
  readonly pixelRatio: number;
  /** Bucket used by tests and CSS hooks. */
  readonly bucket: DensityBucket;
}

/** One `MediaQueryList`, structurally typed so tests can supply a fake. */
export interface MediaQueryListLike {
  /** Registers a change listener (modern API). */
  addEventListener?(type: "change", listener: () => void): void;
  /** Removes a change listener (modern API). */
  removeEventListener?(type: "change", listener: () => void): void;
  /** Registers a change listener (legacy Safari API). */
  addListener?(listener: () => void): void;
  /** Removes a change listener (legacy Safari API). */
  removeListener?(listener: () => void): void;
}

/** The parts of `window` density handling needs. */
export interface DensityWindow {
  /** Device pixels per CSS pixel. */
  readonly devicePixelRatio?: number;
  /** Media query factory. */
  matchMedia?(query: string): MediaQueryListLike;
}

/** Live density state and its subscription. */
export interface DensityController {
  /** Current state; updated in place on monitor change. */
  readonly info: DensityInfo;
  /** Stops listening; the last state stays applied. */
  stop(): void;
}

/** Maps a pixel ratio onto the bucket used by the screenshot matrix. */
export function pixelRatioBucket(pixelRatio: number): DensityBucket {
  if (pixelRatio >= 1.75) {
    return "2x";
  }
  if (pixelRatio >= 1.25) {
    return "1.5x";
  }
  return "1x";
}

/**
 * Applies `pixelRatio` to `root`.
 *
 * @param root - Element that owns the tokens.
 * @param pixelRatio - Ratio from the window; non-finite or non-positive values fall back to 1.
 */
export function applyDensity(root: HTMLElement, pixelRatio: number): DensityInfo {
  const ratio = Number.isFinite(pixelRatio) && pixelRatio > 0 ? pixelRatio : 1;
  const bucket = pixelRatioBucket(ratio);
  root.setAttribute("data-density", bucket);
  root.style.setProperty("--at-pixel-ratio", String(ratio));
  return { pixelRatio: ratio, bucket };
}

function subscribe(query: MediaQueryListLike | undefined, listener: () => void): void {
  if (query === undefined) {
    return;
  }
  if (query.addEventListener !== undefined) {
    query.addEventListener("change", listener);
  } else {
    query.addListener?.(listener);
  }
}

function unsubscribe(query: MediaQueryListLike | undefined, listener: () => void): void {
  if (query === undefined) {
    return;
  }
  if (query.removeEventListener !== undefined) {
    query.removeEventListener("change", listener);
  } else {
    query.removeListener?.(listener);
  }
}

/**
 * Applies the current ratio and keeps it in sync with monitor changes.
 *
 * @param root - Element that owns the tokens.
 * @param target - Window-like source; defaults to the real window.
 */
export function watchDensity(
  root: HTMLElement,
  target: DensityWindow = globalThis.window,
): DensityController {
  let state = applyDensity(root, target.devicePixelRatio ?? 1);
  let query = target.matchMedia?.(`(resolution: ${state.pixelRatio}dppx)`);

  const onChange = (): void => {
    state = applyDensity(root, target.devicePixelRatio ?? 1);
    unsubscribe(query, onChange);
    query = target.matchMedia?.(`(resolution: ${state.pixelRatio}dppx)`);
    subscribe(query, onChange);
  };

  subscribe(query, onChange);

  return {
    get info() {
      return state;
    },
    stop: () => {
      unsubscribe(query, onChange);
      query = undefined;
    },
  };
}
