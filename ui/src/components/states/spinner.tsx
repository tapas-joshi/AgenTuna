import type { JSX } from "react";
import { cn } from "../cn";

/** Props accepted by {@link Spinner}. */
export interface SpinnerProps {
  /** Visible or assistive label; the spinner is a progressbar and needs a name. */
  readonly label: string;
  /** `sm` is 12px for buttons and dense rows, `md` is 24px for state blocks. */
  readonly size?: "sm" | "md";
  /** Extra classes appended after the visual classes. */
  readonly className?: string;
}

const SIZE_CLASSES: Readonly<Record<NonNullable<SpinnerProps["size"]>, string>> = {
  sm: "size-12 border",
  md: "size-24 border-2",
};

/** Indeterminate progress ring drawn with theme tokens. */
export function Spinner({ label, size = "md", className }: SpinnerProps): JSX.Element {
  return (
    <span
      role="progressbar"
      aria-label={label}
      data-slot="spinner"
      className={cn(
        "inline-block shrink-0 animate-spin rounded-full border-border border-t-accent",
        SIZE_CLASSES[size],
        className,
      )}
    />
  );
}
