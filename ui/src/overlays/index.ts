/**
 * Overlay framework barrel (TU-004).
 *
 * One import path for dialogs, popovers, menus and the stacking model. The
 * toast service keeps its own barrel in `overlays/toasts` because it is a
 * different lifecycle (queued, auto-dismissing feedback rather than a
 * focus-owning surface).
 *
 * ```tsx
 * <OverlayStackProvider>
 *   <ModalHost>
 *     <App />
 *   </ModalHost>
 * </OverlayStackProvider>
 * ```
 */

export * from "./dialogs";
export * from "./menus";
export * from "./popovers";
export * from "./stack";
