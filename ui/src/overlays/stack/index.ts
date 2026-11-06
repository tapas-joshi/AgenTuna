/**
 * Overlay stack barrel (TU-004).
 */

export {
  OVERLAY_Z_BASE,
  OVERLAY_Z_MAX,
  TOAST_VIEWPORT_Z,
  createOverlayStack,
} from "./OverlayStack";
export type {
  OverlayKind,
  OverlayLayer,
  OverlayStack,
  OverlayStackOptions,
} from "./OverlayStack";
export { OverlayStackProvider, useOverlayLayer, useOverlayStack } from "./OverlayStackProvider";
export type { OverlayStackProviderProps } from "./OverlayStackProvider";
