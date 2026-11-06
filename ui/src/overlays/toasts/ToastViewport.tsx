/**
 * Toast viewport (TU-005, TU-022).
 *
 * Renders the visible stack bottom-right, above the status bar, in a
 * pointer-transparent container so toasts never block the screen. Two live
 * regions are mounted for the whole session (one polite, one assertive) and
 * only their text changes, so assistive technology announces new toasts
 * without focus ever leaving the control the user is using.
 */

import { useMemo, useState, useSyncExternalStore } from "react";
import type { ClipboardPort, Toast } from "./types";
import { TOAST_KIND_LABEL } from "./types";
import type { ToastStore } from "./store";
import { createBrowserClipboard, toastCopyText } from "./copy";
import "./toasts.css";

/** Feedback shown on the copy button after a copy attempt. */
type CopyState = "idle" | "copied" | "failed";

const COPY_LABEL: Readonly<Record<CopyState, string>> = {
  idle: "Copy details",
  copied: "Copied",
  failed: "Copy failed",
};

/** Properties of {@link ToastViewport}. */
export interface ToastViewportProps {
  /** Store to render; usually supplied by `ToastProvider`. */
  readonly store: ToastStore;
  /** Clipboard port; defaults to the webview clipboard API. */
  readonly clipboard?: ClipboardPort;
}

function announcement(toast: Toast): string {
  const heading = toast.title === undefined || toast.title === "" ? "" : `${toast.title}. `;
  const occurrences = toast.count > 1 ? ` (${toast.count} occurrences)` : "";
  return `${TOAST_KIND_LABEL[toast.kind]}: ${heading}${toast.message}${occurrences}`;
}

/** Renders the visible toast stack and its live regions. */
export function ToastViewport({ store, clipboard }: ToastViewportProps) {
  const snapshot = useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
  const [copyStates, setCopyStates] = useState<Readonly<Record<string, CopyState>>>({});
  const clipboardPort = useMemo(() => clipboard ?? createBrowserClipboard(), [clipboard]);

  const copy = async (toast: Toast): Promise<void> => {
    const stateKey = `${toast.id}:${toast.count}`;
    try {
      await clipboardPort.writeText(toastCopyText(toast));
      setCopyStates((previous) => ({ ...previous, [stateKey]: "copied" }));
    } catch {
      setCopyStates((previous) => ({ ...previous, [stateKey]: "failed" }));
    }
  };

  const polite = snapshot.visible.filter((toast) => toast.kind !== "error");
  const assertive = snapshot.visible.filter((toast) => toast.kind === "error");

  return (
    <div className="at-toasts" data-testid="toast-viewport">
      <div
        className="at-toasts__live"
        role="status"
        aria-live="polite"
        aria-atomic="false"
        data-testid="toast-live-polite"
      >
        {polite.map((toast) => (
          <p key={toast.id}>{announcement(toast)}</p>
        ))}
      </div>
      <div
        className="at-toasts__live"
        role="alert"
        aria-live="assertive"
        aria-atomic="false"
        data-testid="toast-live-assertive"
      >
        {assertive.map((toast) => (
          <p key={toast.id}>{announcement(toast)}</p>
        ))}
      </div>
      <ol className="at-toasts__stack" aria-label="Notifications">
        {snapshot.visible.map((toast) => {
          const copyState = copyStates[`${toast.id}:${toast.count}`] ?? "idle";
          return (
            <li
              key={toast.id}
              className="at-toasts__item"
              data-kind={toast.kind}
              data-toast-id={toast.id}
            >
              <div className="at-toasts__meta">
                <span className="at-toasts__kind">{TOAST_KIND_LABEL[toast.kind]}</span>
                {toast.count > 1 ? (
                  <span
                    className="at-toasts__count"
                    aria-label={`${toast.count} occurrences`}
                  >{`×${toast.count}`}</span>
                ) : null}
                {toast.title === undefined ? null : (
                  <span className="at-toasts__title">{toast.title}</span>
                )}
              </div>
              <p className="at-toasts__message">{toast.message}</p>
              <div className="at-toasts__actions">
                {toast.action === undefined ? null : (
                  <button
                    type="button"
                    className="at-toasts__action"
                    onClick={() => {
                      store.activate(toast.id);
                    }}
                  >
                    {toast.action.label}
                  </button>
                )}
                {toast.kind !== "error" && toast.details === undefined ? null : (
                  <button
                    type="button"
                    className="at-toasts__copy"
                    onClick={() => {
                      void copy(toast);
                    }}
                  >
                    {COPY_LABEL[copyState]}
                  </button>
                )}
                <button
                  type="button"
                  className="at-toasts__close"
                  aria-label={`Dismiss notification: ${toast.message}`}
                  onClick={() => {
                    store.dismiss(toast.id);
                  }}
                >
                  ×
                </button>
              </div>
            </li>
          );
        })}
      </ol>
    </div>
  );
}
