/**
 * React binding for the overlay stack (TU-004).
 *
 * `<OverlayStackProvider>` is optional: a module-level stack is used when an
 * app never mounts one, which keeps the overlay components usable from a
 * single screen. Tests inject a fresh stack so assertions about depth and
 * z-index do not depend on what other tests left open.
 */

import { createContext, useContext, useLayoutEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import type { OverlayKind, OverlayLayer, OverlayStack } from "./OverlayStack";
import { OVERLAY_Z_MAX, createOverlayStack } from "./OverlayStack";

const OverlayStackContext = createContext<OverlayStack | null>(null);

let defaultStack: OverlayStack | null = null;

/** The process-wide stack used when no provider is mounted. */
function sharedStack(): OverlayStack {
  defaultStack ??= createOverlayStack();
  return defaultStack;
}

/** Properties of {@link OverlayStackProvider}. */
export interface OverlayStackProviderProps {
  /** Stack to expose; a private one is created when omitted. */
  readonly stack?: OverlayStack;
  readonly children?: ReactNode;
}

/** Exposes an overlay stack to the overlay components below it. */
export function OverlayStackProvider({ stack, children }: OverlayStackProviderProps) {
  const provided = useMemo(() => stack ?? createOverlayStack(), [stack]);
  return <OverlayStackContext.Provider value={provided}>{children}</OverlayStackContext.Provider>;
}

/** Returns the nearest overlay stack, falling back to the shared one. */
export function useOverlayStack(): OverlayStack {
  return useContext(OverlayStackContext) ?? sharedStack();
}

/**
 * Reserves a layer while `active` is true and returns its z-index.
 *
 * Allocation happens in a layout effect, so the surface paints with its real
 * z-index on the first frame. The layer is released when the surface closes or
 * unmounts, including when a Radix layer is unmounted by its own dismiss logic.
 *
 * @param kind Surface kind, recorded on the layer.
 * @param active Whether the surface is currently open.
 * @returns The layer z-index; {@link OVERLAY_Z_MAX} until the layer is allocated.
 */
export function useOverlayLayer(kind: OverlayKind, active: boolean): number {
  const stack = useOverlayStack();
  const [layer, setLayer] = useState<OverlayLayer | null>(null);

  useLayoutEffect(() => {
    if (!active) {
      return;
    }
    const allocated = stack.push(kind);
    setLayer(allocated);
    return () => {
      stack.remove(allocated.id);
      setLayer(null);
    };
  }, [stack, kind, active]);

  return layer?.zIndex ?? OVERLAY_Z_MAX;
}
