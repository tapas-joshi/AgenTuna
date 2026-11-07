import type { ComponentPropsWithoutRef } from "react";
import { cn } from "./cn";
import { STATUS_META, StatusGlyph, type ItemStatus } from "./status";

/** Neutral-to-semantic skins for a plain badge. */
export type BadgeTone = "neutral" | "muted" | "accent" | "success" | "warning" | "danger" | "info" | "locked" | "project" | "local";

const TONE_CLASSES: Readonly<Record<BadgeTone, string>> = {
  neutral: "border-border bg-raised text-text-secondary",
  muted: "border-border bg-overlay text-text-muted",
  accent: "border-accent/40 bg-accent/10 text-accent",
  success: "border-success/40 bg-success/10 text-success",
  warning: "border-warning/40 bg-warning/10 text-warning",
  danger: "border-danger/40 bg-danger/10 text-danger",
  info: "border-scope-user/40 bg-scope-user/10 text-scope-user",
  locked: "border-scope-global/40 bg-scope-global/10 text-scope-global",
  project: "border-scope-project/40 bg-scope-project/10 text-scope-project",
  local: "border-scope-local/40 bg-scope-local/10 text-scope-local",
};

const DOT_CLASSES: Readonly<Record<BadgeTone, string>> = {
  neutral: "bg-text-secondary",
  muted: "bg-text-muted",
  accent: "bg-accent",
  success: "bg-success",
  warning: "bg-warning",
  danger: "bg-danger",
  info: "bg-scope-user",
  locked: "bg-scope-global",
  project: "bg-scope-project",
  local: "bg-scope-local",
};

const STATUS_TONE_TO_BADGE: Readonly<Record<ItemStatus, BadgeTone>> = {
  linked: "success",
  off: "muted",
  native: "info",
  conflict: "danger",
  drifted: "warning",
  locked: "locked",
  shadowed: "neutral",
};

/** Props accepted by {@link Badge}. */
export interface BadgeProps extends ComponentPropsWithoutRef<"span"> {
  /** Skin to use when the badge is not describing an item status. */
  readonly tone?: BadgeTone;
  /**
   * Item status. Supplies the default label, the tint and a glyph, so the
   * state is never communicated by colour alone.
   */
  readonly status?: ItemStatus;
  /** Shows a leading tone dot for generic (non-status) badges. */
  readonly dot?: boolean;
  /** Renders the label in the monospace face, for paths and identifiers. */
  readonly mono?: boolean;
}

/**
 * A compact pill for scope, ownership and item status.
 *
 * When `status` is set the badge renders a glyph plus the status label; any
 * `children` replace the label text but the glyph stays.
 */
export function Badge({
  tone,
  status,
  dot = false,
  mono = false,
  className,
  children,
  title,
  ...rest
}: BadgeProps) {
  const meta = status === undefined ? undefined : STATUS_META[status];
  const resolvedTone = tone ?? (status === undefined ? "neutral" : STATUS_TONE_TO_BADGE[status]);

  return (
    <span
      {...rest}
      data-slot="badge"
      data-status={status}
      data-tone={resolvedTone}
      title={title ?? meta?.description}
      className={cn(
        "inline-flex max-w-full items-center gap-4 rounded-full border px-8 py-2 text-11 font-medium leading-normal",
        TONE_CLASSES[resolvedTone],
        mono && "font-mono",
        className,
      )}
    >
      {status === undefined ? (
        dot ? <span aria-hidden="true" className={cn("size-8 shrink-0 rounded-full", DOT_CLASSES[resolvedTone])} /> : null
      ) : (
        <StatusGlyph status={status} />
      )}
      {children ?? meta?.label}
    </span>
  );
}
