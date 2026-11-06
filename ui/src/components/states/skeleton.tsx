import type { JSX } from "react";
import { cn } from "../cn";

/** Props accepted by {@link Skeleton}. */
export interface SkeletonProps {
  /** Number of placeholder rows. Defaults to one. */
  readonly rows?: number;
  /** Extra classes; pass a max width for table bodies. */
  readonly className?: string;
}

/**
 * Placeholder rows shown while a list or table loads.
 *
 * Decorative: each row is `aria-hidden` and the surrounding state block owns
 * the label and the live announcement.
 */
export function Skeleton({ rows = 1, className }: SkeletonProps): JSX.Element {
  return (
    <div
      data-slot="skeleton"
      aria-hidden="true"
      className={cn("flex w-full flex-col gap-8", className)}
    >
      {Array.from({ length: rows }, (_, index) => (
        <div key={index} data-slot="skeleton-row" className="h-32 w-full animate-pulse rounded-6 bg-overlay" />
      ))}
    </div>
  );
}
