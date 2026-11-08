/** Returns a human-readable message for errors crossing the Tauri command boundary. */
export function commandErrorMessage(error: unknown, fallback = "The command failed."): string {
  if (typeof error === "string" && error.trim()) return error;
  if (error instanceof Error && error.message.trim()) return error.message;
  if (typeof error === "object" && error !== null && "message" in error) {
    const message = error.message;
    if (typeof message === "string" && message.trim()) return message;
  }
  if (typeof error === "number" || typeof error === "boolean") return String(error);
  return fallback;
}
