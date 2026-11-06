/**
 * Overlay stacking (TU-004).
 *
 * Every open overlay — modal, popover or menu — takes a layer from one stack.
 * A layer's `zIndex` is higher than everything opened before it, so a popover
 * opened from inside a modal paints above that modal's backdrop while the modal
 * still paints above the page. The band stops below the toast viewport: toasts
 * report background results and stay readable above a dialog instead of being
 * buried by a backdrop.
 *
 * The stack is framework-free so the ordering rules can be tested without
 * rendering, and so a caller can inject a private stack (tests, embedded
 * surfaces) through {@link OverlayStackProvider} instead of sharing the
 * process-wide default.
 */

/** What kind of surface opened the layer. Used for diagnostics and tests. */
export type OverlayKind = "modal" | "popover" | "menu";

/** Lowest z-index an overlay receives. */
export const OVERLAY_Z_BASE = 40;

/** Highest z-index an overlay receives. */
export const OVERLAY_Z_MAX = 59;

/** z-index of the toast viewport (`toasts.css`); overlays always stay below it. */
export const TOAST_VIEWPORT_Z = 60;

/** A reserved slot in the overlay band. */
export interface OverlayLayer {
  /** Unique per allocation, so a stale cleanup call cannot remove a live layer. */
  readonly id: string;
  /** Kind of surface that owns the layer. */
  readonly kind: OverlayKind;
  /** Position in the stack at allocation time; 0 is the first layer. */
  readonly depth: number;
  /** Value for the surface's `z-index`; never above {@link OVERLAY_Z_MAX}. */
  readonly zIndex: number;
}

/** Options for {@link createOverlayStack}. */
export interface OverlayStackOptions {
  /** Id factory, injectable so tests get readable ids. */
  readonly createId?: () => string;
}

/** Shared overlay ordering. */
export interface OverlayStack {
  /** Reserves the next layer; the caller owns it until {@link OverlayStack.remove}. */
  push(kind: OverlayKind): OverlayLayer;
  /** Releases a layer. Removing an unknown or already-removed id is a no-op. */
  remove(id: string): void;
  /** The layer currently on top, or `null` when nothing is open. */
  top(): OverlayLayer | null;
  /** Layers from bottom to top. */
  layers(): readonly OverlayLayer[];
  /** Subscribes to stack changes; returns an unsubscribe function. */
  subscribe(listener: () => void): () => void;
}

let nextLayerId = 1;

/**
 * Creates an isolated overlay stack.
 *
 * Ordering is allocation order: a layer keeps the z-index it was given until it
 * closes, so closing a middle layer never reorders the ones still open (that
 * would visibly swap a popover and the dialog it belongs to).
 */
export function createOverlayStack(options: OverlayStackOptions = {}): OverlayStack {
  const createId = options.createId ?? (() => `overlay-${nextLayerId++}`);
  let entries: OverlayLayer[] = [];
  const listeners = new Set<() => void>();

  const notify = (): void => {
    for (const listener of listeners) {
      listener();
    }
  };

  return {
    push(kind) {
      const depth = entries.length;
      const layer: OverlayLayer = {
        id: createId(),
        kind,
        depth,
        zIndex: Math.min(OVERLAY_Z_BASE + depth, OVERLAY_Z_MAX),
      };
      entries = [...entries, layer];
      notify();
      return layer;
    },
    remove(id) {
      const next = entries.filter((entry) => entry.id !== id);
      if (next.length === entries.length) {
        return;
      }
      entries = next;
      notify();
    },
    top() {
      return entries[entries.length - 1] ?? null;
    },
    layers() {
      return entries;
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}
