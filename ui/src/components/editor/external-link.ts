/** External-link rules shared by the Markdown preview and the `open_external_link` command. */

import { invoke } from "@tauri-apps/api/core";

/** Longest link the backend accepts; mirrors `MAX_EXTERNAL_URL_BYTES` in `external_link.rs`. */
export const MAX_EXTERNAL_URL_BYTES = 2048;

const CONTROL_CHARACTERS = /[\u0000-\u001f\u007f]/;

/**
 * Whether `raw` may open in the system browser.
 *
 * Mirrors the Rust validator so the preview never renders a clickable link the command would
 * reject: https only, a host, no userinfo, bounded length, no control characters. `javascript:`,
 * `file:`, `data:` and relative links fail the protocol or host check.
 */
export function isSafeExternalUrl(raw: string | null | undefined): raw is string {
  if (typeof raw !== "string") return false;
  const value = raw.trim();
  if (value.length === 0 || value.length > MAX_EXTERNAL_URL_BYTES) return false;
  if (CONTROL_CHARACTERS.test(value)) return false;
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return false;
  }
  if (parsed.protocol !== "https:") return false;
  if (parsed.username !== "" || parsed.password !== "") return false;
  if (parsed.hostname === "") return false;
  return true;
}

/** Hands a validated link to the backend, which opens it in the system browser. */
export async function openExternalLink(url: string): Promise<void> {
  await invoke("open_external_link", { url });
}
