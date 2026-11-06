import type { JSX } from "react";
import { cn } from "./cn";

/**
 * Deployment state of one item in one agent's view.
 *
 * These are the labels the product uses on the Skills, MCP Servers, Effective
 * config and Sync & drift screens. Every state ships a colour *and* a text
 * label, plus a distinct glyph where the label is abbreviated.
 */
export const ITEM_STATUSES = [
  "linked",
  "off",
  "native",
  "conflict",
  "drifted",
  "locked",
  "shadowed",
] as const;

/** One of the seven item states the UI must express. */
export type ItemStatus = (typeof ITEM_STATUSES)[number];

/** Semantic colour family behind a status. */
export type StatusTone = "success" | "muted" | "info" | "warning" | "danger" | "locked" | "neutral";

/** Label, tone and explanation for one status. */
export interface StatusMeta {
  /** Visible text label; never omit it, colour is not a label. */
  readonly label: string;
  /** Colour family used for the dot, text and badge tint. */
  readonly tone: StatusTone;
  /** One-sentence explanation shown as a tooltip or in the effective-config view. */
  readonly description: string;
}

/** Presentation for every status the UI can render. */
export const STATUS_META: Readonly<Record<ItemStatus, StatusMeta>> = {
  linked: {
    label: "Linked",
    tone: "success",
    description: "Deployed to this agent by AgenTuna and unchanged since the last write.",
  },
  off: {
    label: "Off",
    tone: "muted",
    description: "Not deployed to this agent.",
  },
  native: {
    label: "Native",
    tone: "info",
    description: "Found only in the agent's own folder, outside AgenTuna management.",
  },
  conflict: {
    label: "Conflict",
    tone: "danger",
    description: "The agent's file and the AgenTuna entry disagree and need a decision.",
  },
  drifted: {
    label: "Drifted",
    tone: "warning",
    description: "The deployed copy changed outside AgenTuna since the last write.",
  },
  locked: {
    label: "Locked",
    tone: "locked",
    description: "A Global lock prevents changes from lower scopes.",
  },
  shadowed: {
    label: "Shadowed",
    tone: "neutral",
    description: "A higher-precedence scope overrides this entry.",
  },
};

/** Class fragments for one tone: dot background, text colour and badge skin. */
export interface StatusToneClasses {
  readonly dot: string;
  readonly text: string;
  readonly badge: string;
}

/** Tailwind classes for each tone. Theme tokens only, never raw values. */
export const STATUS_TONE_CLASSES: Readonly<Record<StatusTone, StatusToneClasses>> = {
  success: {
    dot: "bg-success",
    text: "text-success",
    badge: "border-success/40 bg-success/10 text-success",
  },
  muted: {
    dot: "bg-text-muted",
    text: "text-text-muted",
    badge: "border-border bg-overlay text-text-muted",
  },
  info: {
    dot: "bg-scope-user",
    text: "text-scope-user",
    badge: "border-scope-user/40 bg-scope-user/10 text-scope-user",
  },
  warning: {
    dot: "bg-warning",
    text: "text-warning",
    badge: "border-warning/40 bg-warning/10 text-warning",
  },
  danger: {
    dot: "bg-danger",
    text: "text-danger",
    badge: "border-danger/40 bg-danger/10 text-danger",
  },
  locked: {
    dot: "bg-scope-global",
    text: "text-scope-global",
    badge: "border-scope-global/40 bg-scope-global/10 text-scope-global",
  },
  neutral: {
    dot: "bg-text-secondary",
    text: "text-text-secondary",
    badge: "border-border bg-raised text-text-secondary",
  },
};

/** Glyph drawn beside a status label. `data-glyph` identifies it for tests and styling. */
export interface StatusGlyphProps {
  readonly status: ItemStatus;
  readonly className?: string;
}

const GLYPHS: Readonly<Record<ItemStatus, JSX.Element>> = {
  linked: <path d="M2.25 6.4 4.6 8.75 9.75 3.25" />,
  off: <path d="M2.5 6h7" />,
  native: (
    <>
      <path d="M3 2.75h6v6.5H3z" />
      <path d="M4.75 5.25h2.5" />
    </>
  ),
  conflict: (
    <>
      <path d="M6 2.5v4.25" />
      <circle cx="6" cy="9.25" r="0.85" fill="currentColor" stroke="none" />
    </>
  ),
  drifted: <path d="M1.5 7.5c1.5-3 2.5 3 4 0s2.5 3 4 0" />,
  locked: (
    <>
      <rect x="2.75" y="5.75" width="6.5" height="4" rx="1" />
      <path d="M4.5 5.75V4.5a1.5 1.5 0 0 1 3 0v1.25" />
    </>
  ),
  shadowed: (
    <>
      <rect x="3.75" y="2.25" width="6" height="6" rx="1" />
      <path d="M8.25 9.75h-5a1 1 0 0 1-1-1v-5" />
    </>
  ),
};

/**
 * Decorative glyph for a status. It is `aria-hidden`: the adjacent visible
 * label carries the meaning, so screen readers hear the words and not a shape.
 */
export function StatusGlyph({ status, className }: StatusGlyphProps): JSX.Element {
  return (
    <svg
      aria-hidden="true"
      data-glyph={status}
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      focusable="false"
      className={cn("size-12 shrink-0", className)}
    >
      {GLYPHS[status]}
    </svg>
  );
}

/** Returns the presentation for a status. */
export function statusMeta(status: ItemStatus): StatusMeta {
  return STATUS_META[status];
}
