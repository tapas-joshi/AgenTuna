/**
 * Modal surface (TU-004).
 *
 * A controlled Radix dialog with the product's visual contract. Radix supplies
 * the behaviours the story requires and this project does not reimplement:
 * focus moves inside on open, Tab cycles inside the panel, Escape and backdrop
 * clicks report a close, and the page behind is inert and cannot scroll. Focus
 * returns to the widget that opened the dialog because this component captures
 * it as focus moves in and restores it as focus moves out.
 *
 * `dismissible={false}` is the destructive path: Escape and outside clicks are
 * cancelled, so the user must pick an explicit action. A destructive confirm
 * should still pass `initialFocusRef` pointing at its cancel button so Enter
 * does not trigger the destructive action by accident.
 */

import * as Dialog from "@radix-ui/react-dialog";
import { useEffect, useId, useRef } from "react";
import type { ReactNode, RefObject } from "react";
import { cn } from "../../components/cn";
import { useOverlayLayer } from "../stack/OverlayStackProvider";
import type { ModalSize } from "./types";
import "./modal.css";

/**
 * Open modals in the order they opened. A closing modal must not pull focus out
 * of a modal that opened after it, so `onCloseAutoFocus` leaves focus alone when
 * a newer modal is still open.
 */
const modalOpenOrder = new Map<string, number>();
let modalOpenSeq = 0;

/** Properties of {@link Modal}. */
export interface ModalProps {
  /** Whether the dialog is open. The component renders nothing while closed. */
  readonly open: boolean;
  /** Called with `false` when Escape, the backdrop or a close control dismisses. */
  readonly onOpenChange?: (open: boolean) => void;
  /** Dialog heading; also the accessible name. Required, never empty. */
  readonly title: string;
  /** Optional supporting text, wired to `aria-describedby` when present. */
  readonly description?: string;
  /** Panel width. Defaults to `md`. */
  readonly size?: ModalSize;
  /**
   * Whether Escape and outside clicks close the dialog. Defaults to `true`;
   * destructive confirmations pass `false`.
   */
  readonly dismissible?: boolean;
  /** Element that receives focus instead of the first tabbable child. */
  readonly initialFocusRef?: RefObject<HTMLElement | null>;
  /** Fallback element to focus on close if the opener was not an interactive element (e.g. body on shortcut). */
  readonly fallbackFocusRef?: RefObject<HTMLElement | null>;
  /** Action row rendered below the body (usually Cancel plus one primary action). */
  readonly footer?: ReactNode;
  /** Panel content. */
  readonly children?: ReactNode;
  /** Extra classes for the panel, for screen-specific layout. */
  readonly className?: string;
}

/**
 * A modal dialog with a title, optional description, body and action row.
 *
 * Controlled: it renders the `open` it is given and reports dismissal through
 * `onOpenChange`. Choosing a result (`confirmed` versus `cancelled`) is the
 * caller's job; `ModalHost`/`useModals` exist for callers that want a promise
 * of `ModalResult` instead.
 */
export function Modal({
  open,
  onOpenChange,
  title,
  description,
  size = "md",
  dismissible = true,
  initialFocusRef,
  fallbackFocusRef,
  footer,
  children,
  className,
}: ModalProps) {
  const zIndex = useOverlayLayer("modal", open);
  const modalId = useId();
  useEffect(() => {
    if (typeof window === "undefined") return;
    const notify = (isOpen: boolean) => window.dispatchEvent(new CustomEvent("agentuna:modal-state", { detail: { id: modalId, open: isOpen } }));
    if (open) modalOpenOrder.set(modalId, ++modalOpenSeq);
    else modalOpenOrder.delete(modalId);
    notify(open);
    return () => {
      if (open) {
        modalOpenOrder.delete(modalId);
        notify(false);
      }
    };
  }, [modalId, open]);
  // Radix Dialog restores focus to its Trigger; `Modal` is used without one, so
  // the opener is captured as focus moves in and restored as it moves out.
  const restoreFocusRef = useRef<HTMLElement | null>(null);

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="at-modal__backdrop" style={{ zIndex }} />
        <Dialog.Content
          className={cn("at-modal", className)}
          data-size={size}
          data-dismissible={dismissible ? "true" : "false"}
          style={{ zIndex }}
          aria-modal="true"
          onEscapeKeyDown={(event) => {
            if (!dismissible) {
              event.preventDefault();
            }
          }}
          onInteractOutside={(event) => {
            if (!dismissible) {
              event.preventDefault();
            }
          }}
          onOpenAutoFocus={(event) => {
            const active = document.activeElement;
            restoreFocusRef.current = active instanceof HTMLElement ? active : null;
            const target = initialFocusRef?.current;
            if (target == null) {
              return;
            }
            event.preventDefault();
            target.focus();
          }}
          onCloseAutoFocus={(event) => {
            const mine = modalOpenOrder.get(modalId) ?? 0;
            if ([...modalOpenOrder.values()].some((order) => order > mine)) {
              // A modal that opened later owns focus now; restoring the opener here would strand it.
              event.preventDefault();
              return;
            }
            const restore = restoreFocusRef.current;
            if (restore !== null && restore !== document.body && restore.isConnected) {
              event.preventDefault();
              restore.focus();
              return;
            }
            const fallback = fallbackFocusRef?.current;
            if (fallback != null && fallback.isConnected) {
              event.preventDefault();
              fallback.focus();
            }
          }}
        >
          <div className="at-modal__header">
            <Dialog.Title className="at-modal__title">{title}</Dialog.Title>
            {description === undefined ? null : (
              <Dialog.Description className="at-modal__description">
                {description}
              </Dialog.Description>
            )}
          </div>
          <div className="at-modal__body">{children}</div>
          {footer === undefined ? null : <div className="at-modal__footer">{footer}</div>}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
