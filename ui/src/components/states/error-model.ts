/**
 * Domain error model and human copy for the error state (TU-021).
 *
 * Two shapes reach the UI: a {@link CommandError} generated from
 * `at-app::commands`, and the serialised `at-model::ErrorReport` (code, message,
 * user message, path, operation, source chain, remedy ids, retryability) that the
 * engine and the CLI `--json` output produce. `at-model` is a core crate and does
 * not depend on `ts-rs`, so there is no generated binding for the report yet;
 * {@link toErrorReport} parses the wire shape defensively instead of trusting
 * `any`, and accepts the generated `CommandError` as-is.
 *
 * This module owns the mapping from a technical failure to the sentence a screen
 * shows. The raw path and the OS error stay in {@link errorDetailsText}, which is
 * what the collapsed Details drawer and Copy details expose.
 */

import type { CommandError } from "../../generated/CommandError";

/** Categories a screen can reason about when choosing copy. */
export const ERROR_CLASSES = [
  "permission_denied",
  "invalid_format",
  "missing_agent",
  "conflict",
  "offline",
  "network_denied",
  "not_found",
  "auth",
  "secret_missing",
  "locked",
  "unsupported",
  "unavailable",
  "invalid_input",
  "io",
  "cli_install",
  "cancelled",
  "unknown",
] as const;

/** One human-facing category of failure. */
export type ErrorClass = (typeof ERROR_CLASSES)[number];

/** Remedy ids, matching `at_model::Remedy`'s stable wire names. */
export const REMEDY_IDS = [
  "adopt",
  "move_aside",
  "retry",
  "open_folder",
  "reveal_in_finder",
  "reauthenticate",
  "install_agent",
  "set_path",
  "open_settings",
  "copy_details",
  "cancel",
] as const;

/** One action that can resolve a failure. */
export type RemedyId = (typeof REMEDY_IDS)[number];

/** Button text for a remedy; mirrors `at_model::Remedy::label`. */
export const REMEDY_LABELS: Readonly<Record<RemedyId, string>> = {
  adopt: "Adopt",
  move_aside: "Move aside",
  retry: "Retry",
  open_folder: "Open Folder",
  reveal_in_finder: "Reveal in Finder",
  reauthenticate: "Sign in again",
  install_agent: "Install agent",
  set_path: "Choose path",
  open_settings: "Open Settings",
  copy_details: "Copy details",
  cancel: "Cancel",
};

/** A failure normalised for rendering. Plain data; never holds a resolved secret. */
export interface ErrorReportView {
  /** Stable code from the report (`E_…`) or the command rejection (`internal`). */
  readonly code: string;
  /** Human-facing category chosen from the code. */
  readonly class: ErrorClass;
  /** Technical sentence, shown only inside the details drawer. */
  readonly message: string;
  /** Backend-authored user sentence when the report carries one. */
  readonly userMessage?: string;
  /** Raw path the failure is about, kept for the details drawer. */
  readonly path?: string;
  /** Journaled operation the failure belongs to. */
  readonly operationId?: string;
  /** Underlying causes, outermost first. */
  readonly sourceChain: readonly string[];
  /** Actions offered for this failure, in display order. */
  readonly remedies: readonly RemedyId[];
  /** Whether running the operation again may succeed. */
  readonly retryable: boolean;
}

/** Human copy and defaults for one error class. */
export interface ErrorClassCopy {
  /** Screen heading in plain language. */
  readonly title: string;
  /** One or two sentences that say what happened and what to do next. */
  readonly body: string;
  /** Whether a retry is meaningful for this class by default. */
  readonly retryable: boolean;
  /** Remedy buttons offered when the report names none. */
  readonly remedies: readonly RemedyId[];
  /** Label of the retry button. */
  readonly retryLabel: string;
}

const DETAILS: RemedyId = "copy_details";

/** Copy for every class. Colour is never the only signal; this text is. */
export const ERROR_COPY: Readonly<Record<ErrorClass, ErrorClassCopy>> = {
  permission_denied: {
    title: "AgenTuna can't open this file",
    body: "Your account isn't allowed to read or change it. Check the file's permissions, or choose another location.",
    retryable: false,
    remedies: ["reveal_in_finder", DETAILS],
    retryLabel: "Retry",
  },
  invalid_format: {
    title: "This file isn't valid",
    body: "The file isn't in the format this agent expects, so AgenTuna didn't change it. Fix the syntax or restore a backup.",
    retryable: false,
    remedies: ["reveal_in_finder", DETAILS],
    retryLabel: "Retry",
  },
  missing_agent: {
    title: "That agent isn't installed",
    body: "AgenTuna couldn't find this agent on this Mac, so its files can't be managed yet. Install it, then check again.",
    retryable: true,
    remedies: ["install_agent", "retry", DETAILS],
    retryLabel: "Check again",
  },
  conflict: {
    title: "Something already uses this name",
    body: "A different item or file claims this name or path. Move the existing one aside, or take it over, then try again.",
    retryable: false,
    remedies: ["move_aside", "adopt", DETAILS],
    retryLabel: "Retry",
  },
  offline: {
    title: "You're offline",
    body: "AgenTuna couldn't reach the network. Nothing was lost; reconnect and try again.",
    retryable: true,
    remedies: ["retry", "open_settings", DETAILS],
    retryLabel: "Retry",
  },
  network_denied: {
    title: "Online access is turned off",
    body: "The network policy blocked this action before any connection was made. Allow it in Settings if you want to continue.",
    retryable: false,
    remedies: ["open_settings", DETAILS],
    retryLabel: "Retry",
  },
  not_found: {
    title: "That item no longer exists",
    body: "It was moved, renamed or deleted in another app. Refresh to see the current list.",
    retryable: true,
    remedies: ["retry", DETAILS],
    retryLabel: "Refresh",
  },
  auth: {
    title: "Sign in again",
    body: "The saved credential was rejected or has expired. Sign in again to continue.",
    retryable: false,
    remedies: ["reauthenticate", DETAILS],
    retryLabel: "Retry",
  },
  secret_missing: {
    title: "A credential is missing",
    body: "This item refers to a secret that is no longer in the Keychain. Add it again or point the item at another secret.",
    retryable: false,
    remedies: ["reauthenticate", "open_settings", DETAILS],
    retryLabel: "Retry",
  },
  locked: {
    title: "This item is locked",
    body: "Another operation is using it. Nothing was changed; try again in a moment.",
    retryable: true,
    remedies: ["retry", DETAILS],
    retryLabel: "Retry",
  },
  unsupported: {
    title: "Not available on this Mac",
    body: "This action isn't supported on this platform. The path and the technical reason are in the details.",
    retryable: false,
    remedies: [DETAILS],
    retryLabel: "Retry",
  },
  unavailable: {
    title: "AgenTuna's background service stopped",
    body: "The task runner isn't responding, so the action didn't run. Restart AgenTuna, then try again.",
    retryable: true,
    remedies: ["retry", DETAILS],
    retryLabel: "Retry",
  },
  invalid_input: {
    title: "AgenTuna couldn't run that action",
    body: "The request wasn't valid, so nothing changed. Update AgenTuna and try again.",
    retryable: false,
    remedies: [DETAILS],
    retryLabel: "Retry",
  },
  io: {
    title: "AgenTuna couldn't finish that file operation",
    body: "Reading or writing the file failed partway. Nothing was left half-written; the path and the system error are in the details.",
    retryable: true,
    remedies: ["retry", DETAILS],
    retryLabel: "Retry",
  },
  cli_install: {
    title: "The agentuna command wasn't installed",
    body: "AgenTuna couldn't write the command-line tool to its install location. Check that the location is writable, then try again.",
    retryable: true,
    remedies: ["retry", DETAILS],
    retryLabel: "Retry",
  },
  cancelled: {
    title: "Cancelled",
    body: "Nothing was changed.",
    retryable: false,
    remedies: [],
    retryLabel: "Retry",
  },
  unknown: {
    title: "Something went wrong",
    body: "AgenTuna hit an unexpected condition. Nothing was written. Copy the details and include them in a bug report.",
    retryable: true,
    remedies: ["retry", DETAILS],
    retryLabel: "Retry",
  },
};

const CODE_CLASSES: Readonly<Record<string, ErrorClass>> = {
  E_IO: "io",
  E_PERMISSION_DENIED: "permission_denied",
  E_NOT_FOUND: "not_found",
  E_INVALID_FORMAT: "invalid_format",
  E_CONFLICT: "conflict",
  E_CONCURRENT_MODIFICATION: "conflict",
  E_REFUSED_OVERWRITE: "conflict",
  E_LOCKED: "locked",
  E_UNSUPPORTED_PLATFORM: "unsupported",
  E_NETWORK: "offline",
  E_NETWORK_DENIED: "network_denied",
  E_AUTH: "auth",
  E_SECRET_MISSING: "secret_missing",
  E_UPDATE_SIGNATURE_INVALID: "unknown",
  E_INTERNAL: "unknown",
  E_CANCELLED: "cancelled",
  invalid_argument: "invalid_input",
  not_found: "not_found",
  bridge: "unavailable",
  cli_install: "cli_install",
  unavailable: "unavailable",
  refused_overwrite: "conflict",
  internal: "unknown",
};

/** Maps a wire code (report or command rejection) to its class. Unknown codes are `unknown`. */
export function classifyErrorCode(code: string): ErrorClass {
  return CODE_CLASSES[code] ?? "unknown";
}

function isErrorClass(value: unknown): value is ErrorClass {
  return typeof value === "string" && (ERROR_CLASSES as readonly string[]).includes(value);
}

function isRemedyId(value: unknown): value is RemedyId {
  return typeof value === "string" && (REMEDY_IDS as readonly string[]).includes(value);
}

function asString(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

function asStringArray(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.filter((entry): entry is string => typeof entry === "string");
}

function asRemedyArray(value: unknown): RemedyId[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.filter(isRemedyId);
}

/**
 * Normalises any rejection value into an {@link ErrorReportView}.
 *
 * Accepts the generated {@link CommandError}, a serialised `at-model::ErrorReport`,
 * a JavaScript `Error`, a string or anything else a promise may reject with.
 * Unknown shapes never throw: they become the `unknown` class with the value's
 * text as the message.
 */
export function toErrorReport(value: CommandError): ErrorReportView;
export function toErrorReport(value: unknown): ErrorReportView;
export function toErrorReport(value: unknown): ErrorReportView {
  if (value !== null && typeof value === "object") {
    const record = value as Record<string, unknown>;
    const code = asString(record["code"]) ?? "unknown";
    const errorClass = isErrorClass(record["class"]) ? record["class"] : classifyErrorCode(code);
    const copy = ERROR_COPY[errorClass];
    const message =
      asString(record["message"]) ??
      (value instanceof Error ? value.message : undefined) ??
      "An unexpected failure occurred.";
    // Accept both the wire shape (snake_case, from the engine and the CLI) and
    // an already-normalised view, so normalising twice is lossless.
    const remedies = asRemedyArray(record["remedy"] ?? record["remedies"]);
    const retryable = typeof record["retryable"] === "boolean" ? record["retryable"] : copy.retryable;

    return {
      code,
      class: errorClass,
      message,
      userMessage: asString(record["user_message"]) ?? asString(record["userMessage"]),
      path: asString(record["path"]),
      operationId: asString(record["operation_id"]) ?? asString(record["operationId"]),
      sourceChain: asStringArray(record["source_chain"] ?? record["sourceChain"]),
      remedies: remedies.length > 0 ? remedies : copy.remedies,
      retryable,
    };
  }

  const message =
    typeof value === "string" && value !== ""
      ? value
      : value instanceof Error
        ? value.message
        : value === undefined || value === null
          ? "An unexpected failure occurred."
          : String(value);
  return {
    code: "unknown",
    class: "unknown",
    message,
    sourceChain: [],
    remedies: ERROR_COPY.unknown.remedies,
    retryable: ERROR_COPY.unknown.retryable,
  };
}

/**
 * Builds the multi-line text behind Copy details.
 *
 * It carries the code, the class, the raw path, the OS/parser message, the
 * operation id and the source chain so a bug report is actionable without a
 * screenshot. The backend has already redacted secrets before the report
 * reaches the webview.
 */
export function errorDetailsText(report: ErrorReportView): string {
  const lines = [`code: ${report.code}`, `class: ${report.class}`];
  if (report.path !== undefined) {
    lines.push(`path: ${report.path}`);
  }
  if (report.operationId !== undefined) {
    lines.push(`operation: ${report.operationId}`);
  }
  lines.push(`message: ${report.message}`);
  if (report.userMessage !== undefined) {
    lines.push(`user_message: ${report.userMessage}`);
  }
  if (report.sourceChain.length > 0) {
    lines.push("caused by:");
    for (const cause of report.sourceChain) {
      lines.push(`  ${cause}`);
    }
  }
  if (report.remedies.length > 0) {
    lines.push(`remedy: ${report.remedies.join(", ")}`);
  }
  lines.push(`retryable: ${report.retryable ? "yes" : "no"}`);
  return lines.join("\n");
}
