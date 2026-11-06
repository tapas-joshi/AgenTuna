/**
 * Default clipboard for the Copy details button.
 *
 * Components never touch `navigator.clipboard` directly: tests and the app can
 * inject any {@link ClipboardPort}.
 */

import type { ClipboardPort } from "./types";

/** Clipboard backed by the webview's async clipboard API. */
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
