/**
 * Clipboard payload and port for toasts (TU-005).
 *
 * Error toasts expose Copy details; the text put on the clipboard must be
 * useful in a bug report, so it carries the kind, the heading, the message and
 * the technical details in that order.
 */

import type { ClipboardPort, Toast } from "./types";
import { TOAST_KIND_LABEL } from "./types";

/**
 * Builds the multi-line text the Copy details button writes to the clipboard.
 *
 * @param toast - Toast whose content is copied.
 * @returns Plain text with no Markdown, safe to paste into an issue tracker.
 */
export function toastCopyText(toast: Toast): string {
  const lines: string[] = [`[${TOAST_KIND_LABEL[toast.kind]}]`];
  if (toast.title !== undefined && toast.title !== "") {
    lines.push(toast.title);
  }
  lines.push(toast.message);
  if (toast.details !== undefined && toast.details !== "") {
    lines.push("", toast.details);
  }
  if (toast.count > 1) {
    lines.push("", `Occurrences: ${toast.count}`);
  }
  return lines.join("\n");
}

/**
 * Clipboard backed by the webview's async clipboard API.
 *
 * The failure is surfaced rather than swallowed: the viewport flips the button
 * to "Copy failed" instead of pretending the copy worked.
 */
export function createBrowserClipboard(): ClipboardPort {
  return {
    writeText: async (text) => {
      const clipboard = globalThis.navigator.clipboard;
      if (clipboard === undefined) {
        throw new Error("the clipboard API is not available");
      }
      await clipboard.writeText(text);
    },
  };
}
