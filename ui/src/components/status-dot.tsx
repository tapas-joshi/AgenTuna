import { cn } from "./cn";
import { STATUS_META, STATUS_TONE_CLASSES, type ItemStatus } from "./status";

/** Props accepted by {@link StatusDot}. */
export interface StatusDotProps {
  /** One of the seven item states. */
  readonly status: ItemStatus;
  /** Visible text; defaults to the status label. Never pass an empty string. */
  readonly label?: string;
  /** Optional tooltip; defaults to the status explanation. */
  readonly title?: string;
  readonly className?: string;
}

/**
 * A dot plus a text label for an item state.
 *
 * The dot is decorative (`aria-hidden`); the label carries the meaning, so the
 * state is legible without colour perception.
 */
export function StatusDot({ status, label, title, className }: StatusDotProps) {
  const meta = STATUS_META[status];
  const classes = STATUS_TONE_CLASSES[meta.tone];

  return (
    <span
      data-slot="status-dot"
      data-status={status}
      title={title ?? meta.description}
      className={cn("inline-flex min-w-0 items-center gap-4 text-12", classes.text, className)}
    >
      <span aria-hidden="true" data-dot={status} className={cn("size-8 shrink-0 rounded-full", classes.dot)} />
      <span className="truncate">{label ?? meta.label}</span>
    </span>
  );
}
