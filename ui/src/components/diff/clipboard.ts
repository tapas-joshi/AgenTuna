/** Writes text to the system clipboard. Injectable so tests do not need one. */
export type CopyText = (text: string) => void | Promise<void>;

/**
 * Writes `text` through the Clipboard API, falling back to a temporary
 * textarea (`execCommand("copy")`) in webviews that do not expose it.
 *
 * The textarea only ever holds text handed in by the caller; it is created and
 * removed inside this call and never rendered inside the React tree.
 */
export async function writeClipboard(text: string): Promise<void> {
  if (typeof navigator !== "undefined" && navigator.clipboard?.writeText !== undefined) {
    await navigator.clipboard.writeText(text);
    return;
  }
  if (typeof document === "undefined") {
    throw new Error("Clipboard is unavailable");
  }
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.style.position = "fixed";
  area.style.top = "0";
  area.style.left = "0";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  try {
    const copied = document.execCommand("copy");
    if (!copied) {
      throw new Error("Clipboard is unavailable");
    }
  } finally {
    area.remove();
  }
}
