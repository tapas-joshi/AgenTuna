/**
 * Modal surface barrel (TU-004).
 *
 * Typed results first: a caller awaits {@link ModalResult} instead of reading a
 * shared `confirmed` flag.
 */

export { Modal } from "./Modal";
export type { ModalProps } from "./Modal";
export { OverwriteRefusalDialog } from "./OverwriteRefusalDialog";
export type { OverwriteRemedyResult, RefusedOverwriteDetails } from "./OverwriteRefusalDialog";
export { AdoptModal } from "./AdoptModal";
export type { AdoptModalProps } from "./AdoptModal";
export { ModalHost, useModals } from "./ModalHost";
export type { ModalHostProps } from "./ModalHost";
export { createModalQueue } from "./modal-queue";
export type {
  ModalConfirmAction,
  ModalEntry,
  ModalQueue,
  ModalQueueOptions,
  ModalRenderApi,
  ModalRequest,
  ModalSnapshot,
} from "./modal-queue";
export {
  MODAL_CANCELLED,
  MODAL_DISMISSED,
  isModalConfirmed,
  modalConfirmed,
  modalValue,
} from "./types";
export { WhyTraceDialog } from "./WhyTraceDialog";
export type {
  ModalCancelled,
  ModalConfirmed,
  ModalDismissed,
  ModalResult,
  ModalSize,
} from "./types";
